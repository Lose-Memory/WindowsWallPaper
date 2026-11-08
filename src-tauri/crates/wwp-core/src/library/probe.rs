//! 读取图像头部，拿到尺寸与 EXIF 方向。
//!
//! 三条设计约束：
//!
//! 1. **只读头部，不解码整图。** 建立几万张图的索引时解码整图是不可接受的。
//! 2. **不依赖扩展名判断格式。** 参考项目实测约一成的图片扩展名与内容不符，
//!    因此这里一律按内容嗅探。
//! 3. **尺寸是"看起来的尺寸"。** 竖拍照片带 EXIF 方向 6/8，宽高在文件里是反的，
//!    若按原始宽高建立索引，长宽比筛选会把这些照片判反。因此这里就地换算，
//!    让索引里的 `w`/`h` 始终等于"用户看到的宽高"，`orientation` 另外保留原始值。

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// 读多少字节用于嗅探格式与尺寸。JPEG 的 SOF 段、PNG 的 IHDR、WebP 的头部
/// 都远在这个范围内。
const HEADER_BYTES: usize = 64 * 1024;

/// 嗅探结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// 拿到了尺寸（已按 EXIF 方向换算）。
    Sized {
        width: u32,
        height: u32,
        /// EXIF 原始方向值，1 表示未旋转；无 EXIF 时为 1。
        orientation: u16,
    },
    /// 头像是能认出来的，但格式在本程序里没有解码器（见 [`UNSUPPORTED_FORMATS`]）。
    Unsupported { format: String },
    /// 认不出，或文件损坏。
    Unreadable { reason: String },
}

/// 有解码器、可以参与播放的扩展名。
///
/// 注意它只用于**初筛**：真正的判定依据是文件内容。扩展名不在这个表里的文件
/// 直接跳过，连头部都不读，这是几万张图库扫描速度的关键。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "bmp", "webp"];

/// 图片合理可能的扩展名，但本程序没有对应解码器。
///
/// 这些文件要被**计数并报告**，而不是静默丢弃：用户看到一个目录里"少了几张图"，
/// 必须能查到原因。`.txt` 这类明显不是图片的扩展名刻意不在表里——
/// 只有"用户可能真的以为它是图片"的格式才值得报告。
pub const UNSUPPORTED_EXTENSIONS: &[&str] = &[
    "heic", "heif", "avif", "jxl", "tif", "tiff", "psd", "raw", "cr2", "cr3", "nef", "arw", "dng",
    "gif", "ico", "svg", "exr", "hdr", "dds", "ktx2", "ilbm", "ff", "aseprite",
];

/// 扩展名是否值得尝试扫描（小写比较，不含点）。
///
/// **包含视频容器**：设计需求要求"图库里混有 MP4/AVI 时按其顺序播放"，
/// 因此视频与图片一样要进候选、进索引。它们的尺寸由 [`crate::video_size`] 读。
pub fn is_candidate_extension(ext: &str) -> bool {
    let lower = ext.to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.contains(&lower.as_str())
        || UNSUPPORTED_EXTENSIONS.contains(&lower.as_str())
        || crate::video_size::is_video_extension(&lower)
}

/// 扩展名是否属于"我们明确没有解码器"的那一类。
pub fn is_known_unsupported_extension(ext: &str) -> bool {
    UNSUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// 探测一个文件。
pub fn probe_file(path: &Path) -> Probe {
    match std::fs::File::open(path) {
        Ok(file) => {
            // 只读文件开头的一段：几万张图库的扫描时间几乎全花在这里，
            // 读满整个文件会让"读头部拿尺寸"失去意义。
            let mut header = Vec::with_capacity(HEADER_BYTES);
            let mut limited = std::io::Read::take(file, HEADER_BYTES as u64);
            match limited.read_to_end(&mut header) {
                Ok(_) => probe_bytes(&header, path),
                Err(error) => Probe::Unreadable {
                    reason: format!("读取失败：{error}"),
                },
            }
        }
        Err(error) => Probe::Unreadable {
            reason: format!("打开失败：{error}"),
        },
    }
}

/// 探测一段已读入的头部数据。独立出来是为了便于单元测试。
pub fn probe_bytes(header: &[u8], path: &Path) -> Probe {
    if header.is_empty() {
        return Probe::Unreadable {
            reason: "文件为空".to_string(),
        };
    }

    // imagesize 在遇到畸形头部时可能 panic（它内部有 unwrap），
    // 而图库里出现损坏文件是常态，不能让一个坏文件带走整个扫描。
    let size_result = std::panic::catch_unwind(|| imagesize::blob_size(header));
    let size = match size_result {
        Ok(Ok(size)) => size,
        Ok(Err(error)) => {
            // 认不出来时再按扩展名区分一次：可能是我们已知不支持的格式。
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            let sniffed = sniff_format(header);
            if let Some(format) = sniffed.filter(|f| !has_decoder(f)) {
                return Probe::Unsupported { format };
            }
            if is_known_unsupported_extension(&ext) {
                return Probe::Unsupported {
                    format: ext.to_ascii_lowercase(),
                };
            }
            return Probe::Unreadable {
                reason: format!("无法解析图像头部：{error}"),
            };
        }
        Err(_) => {
            return Probe::Unreadable {
                reason: "图像头部解析时发生内部错误".to_string(),
            }
        }
    };

    // 嗅探出来的格式若属于"已知没有解码器"，即便头部能读出尺寸也要记为不支持，
    // 否则索引里会出现一批点开必然失败的条目。
    if let Some(format) = sniff_format(header) {
        if !has_decoder(&format) {
            return Probe::Unsupported { format };
        }
    }

    let orientation = read_exif_orientation(header);
    let (width, height) = apply_orientation(size.width, size.height, orientation);
    Probe::Sized {
        width: width as u32,
        height: height as u32,
        orientation,
    }
}

/// 按 EXIF 方向把"存储的宽高"换算成"显示时的宽高"。
///
/// 5/6/7/8 这四种方向会交换宽高。
fn apply_orientation(width: usize, height: usize, orientation: u16) -> (usize, usize) {
    match orientation {
        5 | 6 | 7 | 8 => (height, width),
        _ => (width, height),
    }
}

/// 从头部字节里嗅探格式名（小写）。
fn sniff_format(header: &[u8]) -> Option<String> {
    let format = imagesize::image_type(header).ok()?;
    Some(
        match format {
            imagesize::ImageType::Jpeg => "jpeg",
            imagesize::ImageType::Png => "png",
            imagesize::ImageType::Bmp => "bmp",
            imagesize::ImageType::Webp => "webp",
            imagesize::ImageType::Gif => "gif",
            imagesize::ImageType::Tiff => "tiff",
            imagesize::ImageType::Heif(_) => "heif",
            imagesize::ImageType::Jxl => "jxl",
            imagesize::ImageType::Ico => "ico",
            imagesize::ImageType::Psd => "psd",
            imagesize::ImageType::Exr => "exr",
            imagesize::ImageType::Hdr => "hdr",
            imagesize::ImageType::Dds => "dds",
            imagesize::ImageType::Ktx2 => "ktx2",
            imagesize::ImageType::Ilbm => "ilbm",
            imagesize::ImageType::Farbfeld => "farbfeld",
            imagesize::ImageType::Aseprite => "aseprite",
            imagesize::ImageType::Pnm => "pnm",
            imagesize::ImageType::Qoi => "qoi",
            imagesize::ImageType::Tga => "tga",
            imagesize::ImageType::Vtf => "vtf",
            // ImageType 是 non_exhaustive：以后新增变体时按"未知格式"处理，
            // 而不是让编译失败——那会让依赖升级变成一次必须改代码的事件。
            other => return Some(format!("{other:?}").to_ascii_lowercase()),
        }
        .to_string(),
    )
}

/// 本程序有解码器的格式。
fn has_decoder(format: &str) -> bool {
    matches!(format, "jpeg" | "png" | "bmp" | "webp")
}

/// 读 EXIF 方向。读不到就返回 1（视为正常方向）。
fn read_exif_orientation(header: &[u8]) -> u16 {
    let cursor = std::io::Cursor::new(header);
    let mut reader = std::io::BufReader::new(cursor);
    let exif = match exif::Reader::new().read_from_container(&mut reader) {
        Ok(exif) => exif,
        Err(_) => return 1,
    };
    match exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        Some(field) => match field.value.get_uint(0) {
            Some(value) if (1..=8).contains(&value) => value as u16,
            _ => 1,
        },
        None => 1,
    }
}

/// 供外部调试：判断一个扩展名是否会进入候选列表。
pub fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 打开文件用于解码。此处只是把 `File` 的构造包一层，方便将来加长路径前缀。
pub fn open_for_decode(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小 PNG 头部（IHDR 里带尺寸），不需要真的写出完整 PNG。
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        data.extend_from_slice(&13u32.to_be_bytes()); // IHDR 长度
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 2, 0, 0, 0]); // 位深/颜色类型等
        data
    }

    #[test]
    fn png_header_yields_dimensions() {
        let header = png_header(1920, 1080);
        match probe_bytes(&header, Path::new("x.png")) {
            Probe::Sized {
                width,
                height,
                orientation,
            } => {
                assert_eq!((width, height), (1920, 1080));
                assert_eq!(orientation, 1);
            }
            other => panic!("应读出尺寸，实际：{other:?}"),
        }
    }

    #[test]
    fn empty_and_text_files_are_unreadable_not_unsupported() {
        assert!(matches!(
            probe_bytes(&[], Path::new("empty.jpg")),
            Probe::Unreadable { .. }
        ));
        assert!(matches!(
            probe_bytes(b"this is not an image at all", Path::new("fake.png")),
            Probe::Unreadable { .. }
        ));
    }

    #[test]
    fn heic_header_is_reported_as_unsupported_with_a_format_name() {
        let mut header = vec![0u8; 64];
        header[3] = 0x18;
        header[4..8].copy_from_slice(b"ftyp");
        header[8..12].copy_from_slice(b"heic");
        match probe_bytes(&header, Path::new("photo.heic")) {
            Probe::Unsupported { format } => {
                assert!(
                    format.contains("heif") || format.contains("heic"),
                    "应报出可读的格式名，实际：{format}"
                );
            }
            // 若 imagesize 直接判定为无法解析，也会落到按扩展名分类的分支。
            Probe::Unreadable { .. } => panic!("HEIC 应被归为「不支持」而不是「损坏」"),
            other => panic!("预期 Unsupported，实际：{other:?}"),
        }
    }

    #[test]
    fn orientation_swaps_dimensions_for_rotated_photos() {
        assert_eq!(apply_orientation(4000, 3000, 1), (4000, 3000));
        assert_eq!(apply_orientation(4000, 3000, 6), (3000, 4000));
        assert_eq!(apply_orientation(4000, 3000, 8), (3000, 4000));
        assert_eq!(apply_orientation(4000, 3000, 3), (4000, 3000));
    }

    #[test]
    fn extension_classification_matches_the_two_tables() {
        assert!(is_candidate_extension("JPG"));
        assert!(is_candidate_extension("webp"));
        assert!(is_candidate_extension("HEIC"));
        assert!(!is_candidate_extension("txt"));
        // 视频容器**必须**进候选：设计需求要求"图库里混有 MP4/AVI 时按其顺序播放"。
        // 这条断言原先写的是 `!is_candidate_extension("mp4")`——那是接入视频之前的
        // 旧契约，接入时改成现在这样。
        assert!(
            is_candidate_extension("mp4"),
            "视频容器必须进候选，否则视频壁纸无法接入图库"
        );
        assert!(is_candidate_extension("AVI"), "大小写不敏感");
        assert!(is_candidate_extension("mkv"));
        assert!(is_known_unsupported_extension("AVIF"));
        // 两张表不能有交集，否则同一个文件会有两种解释。
        for ext in SUPPORTED_EXTENSIONS {
            assert!(
                !UNSUPPORTED_EXTENSIONS.contains(ext),
                "{ext} 同时出现在两张表里"
            );
        }
        // 视频扩展名也不该与"没有解码器"那张表冲突——它们是能播的。
        for ext in crate::video_size::VIDEO_EXTENSIONS {
            assert!(
                !UNSUPPORTED_EXTENSIONS.contains(ext),
                "{ext} 被同时归为视频与'没有解码器'，两者矛盾"
            );
        }
    }
}

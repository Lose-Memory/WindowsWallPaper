//! 从视频容器头读出画面尺寸，**不依赖 Media Foundation**。
//!
//! ## 为什么不复用 `VideoReader`
//!
//! 扫描 5 万个文件时，每个文件都建一个 Media Foundation Source Reader
//! （协商解码器、可能初始化解码器）比读图片头贵几个数量级。
//! 而扫描只需要**宽高**——它就写在容器头里，读几百字节就能拿到。
//!
//! 这也让扫描不必初始化 Media Foundation，避免了"扫一次库就把解码器全加载起来"。
//!
//! ## 支持的容器与各自要处理的坑
//!
//! - **MP4/MOV/M4V**：尺寸在 `moov > trak > tkhd` 里。
//!   - box 长度可能是 32 位或 64 位（长度字段为 1 时后面跟 8 字节真实长度）；
//!   - 长度为 0 表示"延伸到容器末尾"；
//!   - `tkhd` 的 version 0 与 1 布局不同（version 1 的 ctime/mtime/duration 各多 4 字节）；
//!   - **旋转**：手机竖拍的 MP4 用矩阵表示旋转，此时容器里的宽高是横的，
//!     显示时要对调——不处理会让竖屏视频被判成横屏，筛选条件选错。
//! - **AVI**：尺寸在 `strf` 的 `BITMAPINFOHEADER` 里（小端）。
//!   音频流的结构可能靠前，因此要靠 `biSize == 40` 与尺寸合理性区分。
//!
//! 其他容器（MKV/WebM）格式复杂得多（EBML 变长整数），这一版不解析，
//! 由调用方按"未知尺寸"处理并给出保守默认值。

use std::io::{Read, Seek, SeekFrom};

/// 视频的显示尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSize {
    pub width: u32,
    pub height: u32,
    /// 容器里标记的旋转角度（目前只识别 90/270 两种情况），宽高**已经**按它换算过。
    pub rotation: Option<i32>,
    /// 尺寸来源，便于诊断"这个数字是哪来的"。
    pub source: SizeSource,
}

/// 尺寸是从哪里读出来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeSource {
    /// MP4 的 `tkhd`。
    Mp4Tkhd,
    /// AVI 的 `strf`（BITMAPINFOHEADER）。
    AviStrh,
    /// 解析不出来，用了保守默认值。
    Fallback,
}

/// 解析不出来时用的默认尺寸。
///
/// 用 1920x1080 而不是 0x0：0x0 的条目会被筛选条件（最小宽度等）全部排除，
/// 表现为"视频文件一个都播不了"；而给一个常见分辨率只是让筛选结果不精确，
/// 用户还能看到并播放它们。两害相权取其轻。
pub const FALLBACK_SIZE: (u32, u32) = (1920, 1080);

/// 视频容器的扩展名。
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "avi", "mkv", "webm", "wmv", "mpg", "mpeg"];

/// 扩展名是否是视频容器。
pub fn is_video_extension(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// 读视频尺寸。读不出来时返回带 [`SizeSource::Fallback`] 的默认值
/// （而不是 `None`）——原因见 [`FALLBACK_SIZE`] 的说明。
pub fn read_size(path: &std::path::Path) -> VideoSize {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let parsed = match extension.as_str() {
        "mp4" | "m4v" | "mov" => read_mp4_size(path),
        "avi" => read_avi_size(path),
        // 扩展名与实际容器不符的情况很常见（改了名的文件），
        // 因此对未知扩展名两种都试一下。
        _ => read_mp4_size(path).or_else(|| read_avi_size(path)),
    };

    match parsed {
        Some(size) => size,
        None => VideoSize {
            width: FALLBACK_SIZE.0,
            height: FALLBACK_SIZE.1,
            rotation: None,
            source: SizeSource::Fallback,
        },
    }
}

/// 读一个 MP4 box 头：返回 (类型, 内容长度, 内容起始相对偏移)。
fn read_box(reader: &mut impl Read) -> Option<([u8; 4], u64, u64)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let kind = [header[4], header[5], header[6], header[7]];

    if size32 == 1 {
        // 64 位长度：后面 8 字节才是真实长度
        let mut extended = [0u8; 8];
        reader.read_exact(&mut extended).ok()?;
        let size64 = u64::from_be_bytes(extended);
        Some((kind, size64.saturating_sub(16), 16))
    } else if size32 == 0 {
        // 0 表示"一直到容器末尾"，用 u64::MAX 让调用方按边界收敛
        Some((kind, u64::MAX, 8))
    } else {
        Some((kind, (size32 as u64).saturating_sub(8), 8))
    }
}

fn read_mp4_size(path: &std::path::Path) -> Option<VideoSize> {
    let mut file = std::fs::File::open(path).ok()?;
    let total = file.metadata().ok()?.len();
    let mut found = find_moov_tkhd(&mut file, 0, total)?;
    found.source = SizeSource::Mp4Tkhd;
    Some(found)
}

/// 在 `[start, end)` 里找 `moov`，再从它里面找 `tkhd`。
fn find_moov_tkhd(
    file: &mut std::fs::File,
    start: u64,
    end: u64,
) -> Option<VideoSize> {
    let mut position = start;

    while position + 8 <= end {
        file.seek(SeekFrom::Start(position)).ok()?;
        let (kind, content_len, header_len) = read_box(file)?;
        let content_start = position + header_len;
        let content_end = if content_len == u64::MAX {
            end
        } else {
            content_start.saturating_add(content_len).min(end)
        };

        // moov 可能出现在文件开头（流式写出）或末尾，因此顶层要一直找下去。
        if &kind == b"moov" {
            if let Some(size) = find_tkhd(file, content_start, content_end) {
                return Some(size);
            }
        }

        // 防止长度异常导致死循环
        if content_end <= position {
            break;
        }
        position = content_end;
    }
    None
}

/// 在 `moov` 内容里逐层找 `trak > tkhd`。
fn find_tkhd(file: &mut std::fs::File, start: u64, end: u64) -> Option<VideoSize> {
    let mut position = start;

    while position + 8 <= end {
        file.seek(SeekFrom::Start(position)).ok()?;
        let (kind, content_len, header_len) = read_box(file)?;
        let content_start = position + header_len;
        let content_end = if content_len == u64::MAX {
            end
        } else {
            content_start.saturating_add(content_len).min(end)
        };

        if &kind == b"trak" {
            if let Some(size) = find_tkhd(file, content_start, content_end) {
                return Some(size);
            }
        } else if &kind == b"tkhd" {
            if let Some(size) = parse_tkhd(file, content_start, content_end) {
                return Some(size);
            }
        }

        if content_end <= position {
            break;
        }
        position = content_end;
    }
    None
}

/// 解析 `tkhd` 内容。
///
/// 布局（version 0）：
/// `version(1) flags(3) ctime(4) mtime(4) trackid(4) reserved(4) duration(4)
///  reserved(8) layer(2) altgroup(2) volume(2) reserved(2) matrix(36)
///  width(4, 16.16) height(4, 16.16)`
///
/// version 1 的 ctime/mtime/duration 各多 4 字节。
fn parse_tkhd(file: &mut std::fs::File, start: u64, end: u64) -> Option<VideoSize> {
    let len = end.saturating_sub(start);
    if len < 8 {
        return None;
    }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut content = vec![0u8; len as usize];
    file.read_exact(&mut content).ok()?;

    let version = content[0];
    let mut offset = 4usize; // version(1) + flags(3)
    // ctime + mtime + trackid + reserved
    offset += if version == 1 { 8 + 8 + 4 + 4 } else { 4 + 4 + 4 + 4 };
    // duration
    offset += if version == 1 { 8 } else { 4 };
    offset += 8; // reserved
    offset += 8; // layer, altgroup, volume, reserved

    if offset + 36 + 8 > content.len() {
        return None;
    }

    // 矩阵 9 个 16.16 定点数，按 a,b,u,c,d,v,x,y,w 排列。
    let a = read_fixed_16_16(&content, offset);
    let b = read_fixed_16_16(&content, offset + 4);
    let c = read_fixed_16_16(&content, offset + 12);
    let d = read_fixed_16_16(&content, offset + 16);
    offset += 36;

    let raw_width =
        u32::from_be_bytes([content[offset], content[offset + 1], content[offset + 2], content[offset + 3]])
            >> 16;
    let raw_height = u32::from_be_bytes([
        content[offset + 4],
        content[offset + 5],
        content[offset + 6],
        content[offset + 7],
    ]) >> 16;

    if raw_width == 0 || raw_height == 0 {
        return None;
    }

    // 旋转判定：矩阵的 a/d 是轴向缩放，b/c 反映旋转。
    // 90/270 度时 a 与 d 都接近 0，而 b/c 非零。
    let rotation = if a.abs() < 0.01 && d.abs() < 0.01 && (b.abs() > 0.01 || c.abs() > 0.01) {
        Some(90)
    } else if b.abs() > 0.01 && c.abs() > 0.01 {
        // 180 度时 a、d 都是 -1
        if a < -0.01 && d < -0.01 {
            Some(180)
        } else {
            Some(270)
        }
    } else {
        None
    };

    let (width, height) = match rotation {
        Some(90) | Some(270) => (raw_height, raw_width),
        _ => (raw_width, raw_height),
    };

    Some(VideoSize {
        width,
        height,
        rotation,
        source: SizeSource::Mp4Tkhd,
    })
}

/// 读 16.16 定点数。
fn read_fixed_16_16(data: &[u8], offset: usize) -> f32 {
    let raw = i32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]);
    raw as f32 / 65536.0
}

/// 从 AVI 里读尺寸。
///
/// 尺寸在 `strf` 的 `BITMAPINFOHEADER` 里（小端）：
/// `biSize(4) biWidth(4) biHeight(4, 有符号) ...`
///
/// `biSize == 40` 是区分视频流与音频流的关键：音频的 `strf` 是 `WAVEFORMATEX`，
/// 头几个字节看起来也像数字，不加这个判断会读出荒谬的"尺寸"。
fn read_avi_size(path: &std::path::Path) -> Option<VideoSize> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"AVI " {
        return None;
    }

    // 只扫前 1 MB：hdrl 一定在前面，扫全文件对大视频是浪费。
    const SCAN_LIMIT: usize = 1024 * 1024;
    let mut buffer = vec![0u8; SCAN_LIMIT];
    file.seek(SeekFrom::Start(0)).ok()?;
    let read = file.read(&mut buffer).ok()?;
    let data = &buffer[..read];

    let mut index = 0usize;
    while index + 8 + 12 <= data.len() {
        if &data[index..index + 4] == b"strf" {
            let chunk = index + 8;
            let bi_size =
                u32::from_le_bytes([data[chunk], data[chunk + 1], data[chunk + 2], data[chunk + 3]]);
            let width = u32::from_le_bytes([
                data[chunk + 4],
                data[chunk + 5],
                data[chunk + 6],
                data[chunk + 7],
            ]);
            // biHeight 有符号：负值表示自上而下（与 BMP 同源），取绝对值。
            let height = i32::from_le_bytes([
                data[chunk + 8],
                data[chunk + 9],
                data[chunk + 10],
                data[chunk + 11],
            ]);

            if bi_size == 40 && (1..100_000).contains(&width) && height != 0 {
                let height = height.unsigned_abs();
                if (1..100_000).contains(&height) {
                    return Some(VideoSize {
                        width,
                        height,
                        rotation: None,
                        source: SizeSource::AviStrh,
                    });
                }
            }
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_recognized_case_insensitively() {
        for ext in ["mp4", "MP4", "Avi", "mkv", "MOV", "webm"] {
            assert!(is_video_extension(ext), "{ext} 应当被认作视频");
        }
        for ext in ["jpg", "png", "txt", "webp"] {
            assert!(!is_video_extension(ext), "{ext} 不该被认作视频");
        }
    }

    #[test]
    fn missing_file_falls_back_to_a_usable_default() {
        // 关键行为：读不出来**不能**返回 0x0。
        // 0x0 会被最小宽度之类的筛选条件全部排除，表现成"视频一个都播不了"；
        // 给一个常见分辨率只是筛选不精确，用户还能看到并播放它们。
        let size = read_size(std::path::Path::new(r"E:\definitely\does\not\exist.mp4"));
        assert_eq!((size.width, size.height), FALLBACK_SIZE);
        assert_eq!(size.source, SizeSource::Fallback);
        assert!(
            size.width > 0 && size.height > 0,
            "兜底尺寸必须是正数，否则会被筛选条件全部排除"
        );
    }

    #[test]
    fn a_non_video_file_does_not_parse_as_video() {
        // 传一个不是视频的文件（这里用不存在的路径模拟），必须走兜底而不是崩溃。
        let size = read_size(std::path::Path::new("Cargo.toml"));
        assert_eq!(size.source, SizeSource::Fallback);
    }

    #[test]
    fn fixed_point_conversion_matches_the_16_16_layout() {
        // 1.0 的 16.16 定点是 0x00010000
        assert!((read_fixed_16_16(&[0x00, 0x01, 0x00, 0x00], 0) - 1.0).abs() < 1e-6);
        // -1.0 是 0xFFFF0000
        assert!((read_fixed_16_16(&[0xFF, 0xFF, 0x00, 0x00], 0) + 1.0).abs() < 1e-6);
        // 0.5
        assert!((read_fixed_16_16(&[0x00, 0x00, 0x80, 0x00], 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn box_length_of_one_means_a_64_bit_length() {
        // 头部：size=1, type='moov', 然后 8 字节真实长度
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&1024u64.to_be_bytes());
        let mut cursor = std::io::Cursor::new(data);
        let (kind, content_len, header_len) = read_box(&mut cursor).expect("应当解析出 box");
        assert_eq!(&kind, b"moov");
        assert_eq!(header_len, 16, "64 位长度时头部是 16 字节");
        assert_eq!(content_len, 1024 - 16, "内容长度要减掉 16 字节头部");
    }

    #[test]
    fn box_length_of_zero_means_to_the_end() {
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        let mut cursor = std::io::Cursor::new(data);
        let (kind, content_len, _) = read_box(&mut cursor).expect("应当解析出 box");
        assert_eq!(&kind, b"mdat");
        assert_eq!(content_len, u64::MAX, "长度为 0 表示延伸到末尾");
    }
}

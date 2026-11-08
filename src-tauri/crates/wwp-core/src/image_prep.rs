//! 图像准备：把图库里的一张图变成"某块屏幕上的壁纸文件"。
//!
//! ## 为什么必须落盘，以及为什么**不**做复用缓存
//!
//! 系统接口要的是**文件路径**，不是内存里的位图（`IDesktopWallpaper::SetWallpaper`），
//! 因此每换一张都必然写一个目标尺寸的文件出去——这一步去不掉。
//!
//! 去掉的是"留着旧产物当缓存"那一层。它原先按
//! 「源路径 + mtime + 目标尺寸 + 填充方式」算哈希文件名，命中就跳过解码，
//! 并保留最近 24 个文件、每次回收要遍历整个目录。代价是 `data/output`
//! 会累积到几百 MB（4K 每张约 25 MB），而收益只有"同一张图再次被播到时省一次解码"。
//!
//! 现在改成由调用方指定目标文件、**每屏两个文件轮换覆盖写**（见
//! `engine::DesktopSink`）：磁盘占用恒定，没有哈希、没有目录遍历、没有回收逻辑。
//! 代价是点「上一张」要重新解码——实测一张 30 MP 的照片约 130 ms，可以接受。
//!
//! ## 输出格式为什么还是 BMP
//!
//! 用 `--test-wallpaper-latency` 与本地基准实测（4480×6720 源图 → 3840×2160）：
//!
//! | 落盘方式 | 文件大小 | 耗时 |
//! |---|---|---|
//! | BMP | 23 MB | 52 ms |
//! | JPEG（默认质量 75） | 0.6 MB | 120~138 ms |
//!
//! JPEG 省了磁盘，但多花一倍多的时间，而且质量 75 对壁纸来说太低。
//! 因此保留 BMP：这条路径上真正的开销是解码（130 ms）与重采样（60 ms），
//! 不是落盘。

use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use serde::{Deserialize, Serialize};

use crate::config::FitMode;
use crate::{Error, Result};

/// 准备一张壁纸的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    /// 可直接交给系统接口的文件路径。
    pub path: PathBuf,
    /// 产出尺寸。
    pub width: u32,
    pub height: u32,
    /// 本次耗时（毫秒）。
    pub millis: u64,
}

/// 准备一张图作为指定尺寸的壁纸，写到 `destination`。
///
/// `destination` 由调用方决定（每屏两个文件轮换），因此重复调用会**覆盖**它。
/// 覆盖一个可能已存在的文件必须走"临时文件 + 改名"：中途失败不会留下
/// 一个半截的 BMP 被系统当成有效壁纸。
pub fn prepare(
    source: &Path,
    destination: &Path,
    target_width: u32,
    target_height: u32,
    fit: FitMode,
    orientation: u16,
) -> Result<Prepared> {
    if target_width == 0 || target_height == 0 {
        return Err(Error::config(
            "无法准备壁纸",
            format!("目标尺寸非法：{target_width}x{target_height}"),
        ));
    }

    let started = std::time::Instant::now();
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("创建输出目录失败 {}", parent.display()), e))?;
    }

    let image = load_with_orientation(source, orientation)?;
    let canvas = fit_for_screen(&image, target_width, target_height, fit);

    write_atomically(destination, &canvas)?;

    Ok(Prepared {
        path: destination.to_path_buf(),
        width: target_width,
        height: target_height,
        millis: started.elapsed().as_millis() as u64,
    })
}

/// 把一张已准备好的壁纸产物另存成 JPEG。
///
/// ## 为什么引擎里要有这个（当前产品路径并不用它）
///
/// 产品路径一律落 BMP，理由见文件头：BMP 写得更快、质量无损。但"写"这一侧的
/// 成本与"系统读取并转码"那一侧的**是两回事**：`SetWallpaper` 之后画面要等
/// 几百毫秒才出现（实测 470 ms），而那段时间花在 Windows 读这个文件并转码上。
/// 因此"换一个更小的源文件能不能缩短那几百毫秒"是一个**可测的问题**，
/// 而测它就需要能生成同样内容的 JPEG——这个函数就是那条诊断路径的编码器
/// （`--probe-nsfw-latency --compare-nsfw-formats`）。
///
/// 放在引擎而不是外壳：外壳不直接依赖 `image` crate，它只通过 `wwp-core`
/// 的公开函数用这些能力。
pub fn write_jpeg(source: &Path, destination: &Path, quality: u8) -> Result<()> {
    let image = load_with_orientation(source, 1)?;
    let file = std::fs::File::create(destination)
        .map_err(|e| Error::io(format!("创建 JPEG 失败 {}", destination.display()), e))?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        quality,
    );
    encoder
        .encode_image(&image)
        .map_err(|e| Error::config("编码 JPEG 失败", e.to_string()))
}

/// 把画布落成 BMP，先写临时文件再改名。
///
/// 改名失败时退回"复制 + 删除"：目标文件可能被别的进程按共享模式打开着
/// （它是上一次的壁纸产物），而 `fs::copy` 只是以写入方式打开并截断，
/// 在多数共享模式下都能成功。留着这个回退是因为失败一次的后果是
/// "这一屏这一轮换不了纸"，而它在日志里看起来和"功能坏了"一样。
fn write_atomically(destination: &Path, canvas: &image::RgbImage) -> Result<()> {
    let temp = destination.with_extension("bmp.tmp");
    canvas
        .save_with_format(&temp, image::ImageFormat::Bmp)
        .map_err(|e| {
            Error::config(
                format!("写入壁纸文件失败 {}", temp.display()),
                e.to_string(),
            )
        })?;

    if std::fs::rename(&temp, destination).is_ok() {
        return Ok(());
    }

    log::debug!(
        "改名到 {} 失败（目标可能被占用），退回复制",
        destination.display()
    );
    std::fs::copy(&temp, destination).map_err(|e| {
        Error::io(
            format!("替换壁纸文件失败 {}", destination.display()),
            e,
        )
    })?;
    let _ = std::fs::remove_file(&temp);
    Ok(())
}

/// 按填充方式把图放进目标尺寸的画布。
///
/// 与 [`fit_to_canvas`] 的差别只有 `Fill`：这里优先走
/// "在重采样器内部直接按目标长宽比取源图区域"（见 [`resize_fill_simd`]），
/// 不再先放大到覆盖画布再裁掉多余部分。标量回退路径与原实现一致。
fn fit_for_screen(
    source: &image::RgbImage,
    target_width: u32,
    target_height: u32,
    fit: FitMode,
) -> image::RgbImage {
    if fit == FitMode::Fill {
        if let Some(filled) = resize_fill_simd(source, target_width, target_height) {
            return filled;
        }
        log::debug!("Fill 的 SIMD 路径不可用，退回标量实现");
    }
    fit_to_canvas(source, target_width, target_height, fit)
}

/// 读取并应用 EXIF 方向。
pub fn load_with_orientation(source: &Path, orientation: u16) -> Result<image::RgbImage> {
    let reader = image::ImageReader::open(source)
        .map_err(|e| Error::io(format!("打开图片失败 {}", source.display()), e))?
        // 不按扩展名猜格式（约一成图片的扩展名与内容不符），一律嗅探内容。
        .with_guessed_format()
        .map_err(|e| Error::io(format!("判定图片格式失败 {}", source.display()), e))?;
    let decoded = reader
        .decode()
        .map_err(|e| Error::config(format!("解码图片失败 {}", source.display()), e.to_string()))?;

    // 用 `into_rgb8` 而不是 `to_rgb8`：JPEG/PNG 解出来本来就是 RGB8，
    // 前者直接拿走所有权，后者会**再拷一份**整图。
    // 一张 30 MP 的图是 90 MB，这次多余的拷贝实测值 20~30 ms。
    let rgb = decoded.into_rgb8();
    Ok(apply_orientation(rgb, orientation))
}

/// 按 EXIF 方向旋转/翻转。
///
/// 取值含义（1 为正常，5-8 会交换宽高）：
/// 1 正常、2 水平翻转、3 旋转 180°、4 垂直翻转、
/// 5 顺时针 90° 后水平翻转、6 顺时针 90°、7 逆时针 90° 后水平翻转、8 逆时针 90°。
pub fn apply_orientation(image: image::RgbImage, orientation: u16) -> image::RgbImage {
    match orientation {
        2 => image::imageops::flip_horizontal(&image),
        3 => image::imageops::rotate180(&image),
        4 => image::imageops::flip_vertical(&image),
        5 => image::imageops::rotate90(&image::imageops::flip_horizontal(&image)),
        6 => image::imageops::rotate90(&image),
        7 => image::imageops::rotate270(&image::imageops::flip_horizontal(&image)),
        8 => image::imageops::rotate270(&image),
        // 1 与非法值都按正常处理：宁可方向不对，也不要因为一个坏字段拒绝显示。
        _ => image,
    }
}

/// 把图片放进目标尺寸的画布，按填充方式处理长宽比差异。
///
/// 这条是**标量**路径：`Fill` 用"先等比放大到覆盖画布、再中心裁切"的直观做法。
/// 生产路径优先走 [`resize_fill_simd`]，它算得更少；但两者必须给出同样的画幅，
/// 有测试逐项比对。
pub fn fit_to_canvas(
    source: &image::RgbImage,
    target_width: u32,
    target_height: u32,
    fit: FitMode,
) -> image::RgbImage {
    let (source_width, source_height) = source.dimensions();
    if source_width == 0 || source_height == 0 {
        return image::RgbImage::new(target_width, target_height);
    }

    match fit {
        FitMode::Stretch => resize_exact(source, target_width, target_height),

        FitMode::Center => {
            // 原始尺寸居中，不缩放；比目标大的部分裁掉，小的部分留黑。
            let mut canvas = image::RgbImage::new(target_width, target_height);
            let offset_x = (target_width as i32 - source_width as i32) / 2;
            let offset_y = (target_height as i32 - source_height as i32) / 2;
            image::imageops::replace(
                &mut canvas,
                source,
                offset_x as i64,
                offset_y as i64,
            );
            canvas
        }

        FitMode::Fit => {
            // 完整显示：等比缩放到能放进画布，剩余补黑。
            let scale = f64::min(
                target_width as f64 / source_width as f64,
                target_height as f64 / source_height as f64,
            );
            let width = ((source_width as f64 * scale).round() as u32).max(1);
            let height = ((source_height as f64 * scale).round() as u32).max(1);
            let resized = resize_exact(source, width, height);
            let mut canvas = image::RgbImage::new(target_width, target_height);
            let offset_x = (target_width as i32 - width as i32) / 2;
            let offset_y = (target_height as i32 - height as i32) / 2;
            image::imageops::replace(&mut canvas, &resized, offset_x as i64, offset_y as i64);
            canvas
        }

        FitMode::Fill => {
            // 裁切填满：等比放大到覆盖画布，再从中心裁出目标尺寸。
            let scale = f64::max(
                target_width as f64 / source_width as f64,
                target_height as f64 / source_height as f64,
            );
            let width = ((source_width as f64 * scale).round() as u32).max(target_width);
            let height = ((source_height as f64 * scale).round() as u32).max(target_height);
            let resized = resize_exact(source, width, height);
            let offset_x = ((width - target_width) / 2) as i64;
            let offset_y = ((height - target_height) / 2) as i64;
            image::imageops::crop_imm(&resized, offset_x as u32, offset_y as u32, target_width, target_height)
                .to_image()
        }
    }
}

/// 重采样到精确尺寸。
///
/// 用 Lanczos3：壁纸是一次性放大到全屏的，缩略图那种速度优先的取舍在这里
/// 会让画面明显发糊。降采样到比目标大很多的尺寸时，Lanczos3 的振铃也最轻。
///
/// 走 `fast_image_resize`（SIMD）而不是 `image::imageops::resize`：
/// 重采样是这条路径上最热的环节，参考项目实测标量版会让程序比它替代掉的 Python 还慢。
/// 若 SIMD 路径因为任何原因失败（例如像素类型不支持），退回标量实现——
/// 慢一点也比显示不出来强。
fn resize_exact(source: &image::RgbImage, width: u32, height: u32) -> image::RgbImage {
    if source.width() == width && source.height() == height {
        return source.clone();
    }

    if let Some(resized) = resize_simd(source, width, height, None) {
        return resized;
    }

    log::debug!(
        "SIMD 重采样不可用，退回标量实现（{}x{} -> {width}x{height}）",
        source.width(),
        source.height()
    );
    image::imageops::resize(source, width, height, FilterType::Lanczos3)
}

/// `Fill` 的 SIMD 路径：让重采样器**只读源图上对应目标长宽比的那块区域**，
/// 直接产出一张正好是屏幕尺寸的图。
///
/// ## 为什么这样算得少
///
/// 直观做法（`fit_to_canvas` 里的标量路径）是"先把整图放大到覆盖画布，再裁掉多余"。
/// 以本机素材实测（4480×6720 → 3840×2160）：
///
/// | 做法 | 中间产物 | 重采样耗时 |
/// |---|---|---|
/// | 先放大到覆盖（3840×5760）再裁 | 22 MP | 108 ms |
/// | 直接在源图上取区域并缩到 3840×2160 | 无 | 58 ms |
///
/// 差距来自两处：中间产物本身就是 22 MP（多算了一倍的输出像素），
/// 而裁切那一步还要再拷一份 8 MP。让重采样器自己按区域取样，两者都省掉了。
///
/// `fit_into_destination` 的语义与"裁切填满"完全一致：按目标长宽比在源图上
/// 居中取最大区域，再缩放到目标尺寸。
fn resize_fill_simd(
    source: &image::RgbImage,
    target_width: u32,
    target_height: u32,
) -> Option<image::RgbImage> {
    use fast_image_resize::ResizeOptions;

    resize_simd(
        source,
        target_width,
        target_height,
        Some(ResizeOptions::new().fit_into_destination(Some((0.5, 0.5)))),
    )
}

/// SIMD 重采样。失败返回 `None`，由调用方决定回退策略。
///
/// `options` 为 `None` 时是"整图缩放到目标尺寸"；给了选项则按选项里的
/// 裁剪规则取样（见 [`resize_fill_simd`]）。
fn resize_simd(
    source: &image::RgbImage,
    width: u32,
    height: u32,
    options: Option<fast_image_resize::ResizeOptions>,
) -> Option<image::RgbImage> {
    use fast_image_resize::images::{Image, ImageRef};
    use fast_image_resize::{
        FilterType as FirFilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    };

    let src_view = ImageRef::new(
        source.width(),
        source.height(),
        source.as_raw(),
        PixelType::U8x3,
    )
    .ok()?;

    // 目标用拥有型 `Image`：`ImageRef` 只实现只读视图，做不了重采样目标。
    let mut destination = Image::new(width, height, PixelType::U8x3);

    let mut resizer = Resizer::new();
    resizer
        .resize(
            &src_view,
            &mut destination,
            // Lanczos3 卷积：与标量回退用的滤波器一致，两条路径的画质可比。
            // 注意这是 fast_image_resize 自己的 FilterType，与 image crate 的同名类型不是一回事。
            &options.unwrap_or_else(|| {
                ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FirFilterType::Lanczos3))
            }),
        )
        .ok()?;

    image::RgbImage::from_raw(width, height, destination.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-image-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 造一张纯色图，尺寸可控。
    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> image::RgbImage {
        image::RgbImage::from_pixel(width, height, image::Rgb(rgb))
    }

    fn write_png(path: &Path, width: u32, height: u32, rgb: [u8; 3]) {
        solid(width, height, rgb)
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    /// 带渐变的测试图：纯色图验不出重采样的差别，渐变才可以。
    fn gradient(width: u32, height: u32) -> image::RgbImage {
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        })
    }

    #[test]
    fn fill_crops_to_exact_target_size() {
        let source = solid(1000, 500, [255, 0, 0]);
        let out = fit_to_canvas(&source, 400, 400, FitMode::Fill);
        assert_eq!(out.dimensions(), (400, 400));
    }

    #[test]
    fn fit_letterboxes_and_keeps_the_whole_image() {
        // 2:1 的图放进 1:1 画布：应缩到 400x200，上下补黑。
        let source = solid(1000, 500, [255, 0, 0]);
        let out = fit_to_canvas(&source, 400, 400, FitMode::Fit);
        assert_eq!(out.dimensions(), (400, 400));
        // 中间行应是原色，顶部行应是补的黑色。
        assert_eq!(out.get_pixel(200, 200).0, [255, 0, 0]);
        assert_eq!(out.get_pixel(200, 2).0, [0, 0, 0]);
    }

    #[test]
    fn stretch_ignores_aspect_ratio() {
        let source = solid(1000, 500, [10, 20, 30]);
        let out = fit_to_canvas(&source, 300, 700, FitMode::Stretch);
        assert_eq!(out.dimensions(), (300, 700));
    }

    #[test]
    fn center_keeps_original_size_and_pads() {
        let source = solid(100, 50, [1, 2, 3]);
        let out = fit_to_canvas(&source, 300, 300, FitMode::Center);
        assert_eq!(out.dimensions(), (300, 300));
        // 正中间是原图，角落是补的黑色。
        assert_eq!(out.get_pixel(150, 150).0, [1, 2, 3]);
        assert_eq!(out.get_pixel(1, 1).0, [0, 0, 0]);
    }

    #[test]
    fn orientation_six_swaps_dimensions() {
        let source = solid(400, 200, [9, 9, 9]);
        let rotated = apply_orientation(source, 6);
        assert_eq!(rotated.dimensions(), (200, 400), "方向 6 应交换宽高");
        let normal = apply_orientation(solid(400, 200, [9, 9, 9]), 1);
        assert_eq!(normal.dimensions(), (400, 200));
        // 非法值按正常处理，不 panic。
        let bogus = apply_orientation(solid(400, 200, [9, 9, 9]), 99);
        assert_eq!(bogus.dimensions(), (400, 200));
    }

    #[test]
    fn the_fast_fill_path_agrees_with_the_scalar_one() {
        // **这条守的是"优化不能改画面"**。
        //
        // 生产路径（`resize_fill_simd`）与标量路径（`fit_to_canvas`）用了两种取样顺序：
        // 前者让重采样器只读源图的一块区域，后者先放大到覆盖画布再裁。
        // 两者的重采样网格不同，像素不可能逐一相等，但**画幅与被裁掉的内容**
        // 必须一致——差一格就会表现为"壁纸偏了"。
        let source = gradient(1200, 800);

        for (w, h) in [(400u32, 400u32), (640, 360), (360, 640), (200, 500)] {
            let fast = resize_fill_simd(&source, w, h).expect("SIMD 路径应当可用");
            let scalar = fit_to_canvas(&source, w, h, FitMode::Fill);
            assert_eq!(fast.dimensions(), (w, h));
            assert_eq!(scalar.dimensions(), (w, h));

            // 逐像素的平均差：取样网格不同会有小差异，但整体色调必须一致。
            let mut total = 0u64;
            for (a, b) in fast.pixels().zip(scalar.pixels()) {
                for c in 0..3 {
                    total += (a.0[c] as i32 - b.0[c] as i32).unsigned_abs() as u64;
                }
            }
            let mean = total as f64 / (w as f64 * h as f64 * 3.0);
            assert!(
                mean < 12.0,
                "{w}x{h} 上两条路径的平均通道差 {mean:.1} 过大，说明裁的位置对不上"
            );
        }
    }

    #[test]
    fn prepare_writes_a_readable_bmp_and_overwrites_next_time() {
        let src_dir = temp_dir("src");
        let out_dir = temp_dir("out");
        let source = src_dir.join("a.png");
        write_png(&source, 1200, 800, [200, 100, 50]);
        let destination = out_dir.join("DISPLAY1-0.bmp");

        let first = prepare(&source, &destination, 640, 480, FitMode::Fill, 1).unwrap();
        assert!(first.path.is_file());
        assert_eq!((first.width, first.height), (640, 480));

        // 产出必须是能被解回来的合法 BMP，且尺寸正确。
        let decoded = image::open(&first.path).unwrap();
        assert_eq!(decoded.width(), 640);
        assert_eq!(decoded.height(), 480);

        // 再准备一张别的图到**同一个文件**：必须被覆盖成新内容，而不是报错。
        let other = src_dir.join("b.png");
        write_png(&other, 300, 900, [1, 2, 3]);
        let second = prepare(&other, &destination, 640, 480, FitMode::Fill, 1).unwrap();
        assert_eq!(second.path, destination);
        let decoded = image::open(&second.path).unwrap();
        assert_eq!(decoded.width(), 640);
        assert_eq!(decoded.height(), 480);

        // 覆盖写不应该留下临时文件。
        let leftovers: Vec<_> = std::fs::read_dir(&out_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不应留下临时文件：{leftovers:?}");

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn changing_the_source_content_changes_the_wallpaper() {
        // 覆盖同名源文件之后必须真的换内容——这类"改了没生效"的问题最难排查。
        let src_dir = temp_dir("mtime-src");
        let out_dir = temp_dir("mtime-out");
        let source = src_dir.join("a.png");
        let destination = out_dir.join("DISPLAY1-0.bmp");

        write_png(&source, 100, 100, [255, 0, 0]);
        prepare(&source, &destination, 200, 200, FitMode::Fill, 1).unwrap();
        assert_eq!(image::open(&destination).unwrap().to_rgb8().get_pixel(100, 100).0, [255, 0, 0]);

        write_png(&source, 100, 100, [0, 0, 255]);
        prepare(&source, &destination, 200, 200, FitMode::Fill, 1).unwrap();
        assert_eq!(image::open(&destination).unwrap().to_rgb8().get_pixel(100, 100).0, [0, 0, 255]);

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn prepare_rejects_degenerate_target_size() {
        let src_dir = temp_dir("bad-src");
        let out_dir = temp_dir("bad-out");
        let source = src_dir.join("a.png");
        write_png(&source, 10, 10, [0, 0, 0]);
        let destination = out_dir.join("x.bmp");
        assert!(prepare(&source, &destination, 0, 100, FitMode::Fill, 1).is_err());
        assert!(prepare(&source, &destination, 100, 0, FitMode::Fill, 1).is_err());
        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }
}

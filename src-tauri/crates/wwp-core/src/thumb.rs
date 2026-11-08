//! 缩略图：把一张图压成小 JPEG，再编码成可以直接塞进 `<img src>` 的 data URL。
//!
//! ## 谁在用
//!
//! - **回收站相册**（`trash.rs`）：一条一张缩略图；
//! - **播放页的每屏预览**（「当前显示」与「下一张」）：显示这张壁纸长什么样。
//!
//! 两处要做的事完全一样，因此只有这一份实现。项目里已经吃过一次教训
//! （AGENTS §7.3）：同一件事写成两套，最后只有一半会跟着改。
//!
//! ## 为什么不让页面直接加载原图
//!
//! 图库里很多是 30 MP 的照片（本机最大的素材约 30 MP，解码一次要 400 MB 内存）。
//! 让 WebView 自己解码，一是慢、二是内存峰值很吓人；压成 360 px 的 JPEG 之后
//! 每张只有几十 KB。生成结果缓存在磁盘上，同一张图第二次是瞬时的。
//!
//! ## 缓存键里必须带"源文件的修改时间与大小"
//!
//! AGENTS §7.14 那条教训：只按"缓存比源文件新"判断复用，会在**换上一张更老的图**
//! 时复用上一张的产物（设置没错、代码没错，只有画面不对）。
//! 这里把"是哪一张源文件"直接编进缓存文件名（路径 + 大小 + 修改时间），
//! 换了图就一定换键，**不可能**复用错；反过来，键相同就意味着同一份内容，
//! 因此不再需要"时间戳比较"那一层判断。
//!
//! ## 缓存不会无限长大
//!
//! 播放页每次换纸都会产生一个新键。一张图几十 KB，但如果长年累月地播下去，
//! 几万个键就是几百 MB。因此写入新缓存之后会修剪一次目录（见 [`prune`]）：
//! 按修改时间留下最近用到的那些。修剪做得很保守——只在**真的生成了新图**之后
//! 才做，且目录不到上限时一个文件都不碰。

use std::fs;
use std::path::Path;

use crate::base64;

/// 缩略图长边的最大像素数。
///
/// 360 是"看得清是什么"与"够小"之间的取值：卡片上的预览框实际只有一百多
/// 逻辑像素宽，360 已经足够清晰（高 DPI 屏上也不糊）。
pub const MAX_EDGE: u32 = 360;

/// 缩略图的 JPEG 质量。80 出头在"看得清"与"体积小"之间够用了。
pub const QUALITY: u8 = 82;

/// 不生成缩略图的像素上限。
///
/// 解码一张图要按宽度×高度×4 字节算内存：一亿像素就是 400 MB。
/// 壁纸本身通常到不了这个量级，但图库是用户的，里面可能有一张他自己都不知道
/// 多大的图，因此先读头部尺寸挡一道（只读头部，不分配那几百 MB）。
pub const MAX_PIXELS: u64 = 100_000_000;

/// 缓存目录里允许多少个文件；超过 [`PRUNE_TO`] 就修剪。
pub const CACHE_LIMIT: usize = 600;

/// 修剪之后保留多少个（最近用到的）。
pub const PRUNE_TO: usize = 500;

/// 把一张图压成小 JPEG（不落盘、不编码 base64）。
///
/// 失败时说清是**这张图的问题**（读不出尺寸 / 打不开 / 太大），
/// 界面会把这句原样显示在预览框里——比一个空白框有用得多。
pub fn render(source: &Path) -> Result<Vec<u8>, String> {
    // 先只读头部拿尺寸：像素数离谱的直接拒绝，不去分配那几百 MB。
    let (width, height) = image::image_dimensions(source)
        .map_err(|e| format!("读不出图片尺寸（可能不是图片或已损坏）：{e}"))?;
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(format!(
            "这张图太大（{width}×{height}），为了不占满内存就不生成缩略图了"
        ));
    }
    let image = image::open(source).map_err(|e| format!("打不开这张图：{e}"))?;
    let thumb = image.thumbnail(MAX_EDGE, MAX_EDGE);

    let mut buffer = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buffer);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, QUALITY)
        .encode_image(&thumb)
        .map_err(|e| format!("缩略图编码失败：{e}"))?;
    Ok(buffer)
}

/// 一个源文件的缓存键（16 个十六进制字符）。
///
/// 组成：规范化后的**路径** + 文件大小 + 修改时间。
/// 三者任一变化都会得到新键——这正是 §7.14 要的东西：
/// "换了输入"不能靠时间戳去猜，要直接写进键里。
///
/// 取不到元数据（文件不在、权限不够）时只用路径：那种情况下 `render` 会报错，
/// 键是什么已经不重要了。用 FNV-1a 而不是引入哈希库：这里只需要"同一输入永远
/// 同一串、不同输入几乎不撞车"，不涉及任何安全性质。
pub fn key_for(source: &Path) -> String {
    let mut seed = crate::library::exclude::normalize(&source.to_string_lossy());
    if let Ok(meta) = fs::metadata(source) {
        seed.push('|');
        seed.push_str(&meta.len().to_string());
        if let Ok(modified) = meta.modified() {
            if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                seed.push('|');
                seed.push_str(&since_epoch.as_nanos().to_string());
            }
        }
    }

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 某个文件的缩略图（data URL）。缓存目录 + 缓存键由调用方给。
///
/// `key` 必须**唯一标识源文件的那一份内容**（[`key_for`] 就是干这个的）。
/// 键相同即内容相同，因此这里只判断"缓存文件在不在、非空"，不再比时间戳。
///
/// 写缓存失败不影响结果：图已经压好了，这一份照样返回给界面，
/// 只是下一次还得再压一遍（记一条 warn 就够，不该因此让预览显示不出来）。
pub fn data_url(source: &Path, cache_dir: &Path, key: &str) -> Result<String, String> {
    let cache = cache_dir.join(format!("{key}.jpg"));

    let jpeg = match fs::read(&cache) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        _ => {
            let rendered = render(source)?;
            if let Err(error) = fs::create_dir_all(cache_dir) {
                log::warn!("无法创建缩略图缓存目录 {}：{error}", cache_dir.display());
            } else if let Err(error) = fs::write(&cache, &rendered) {
                log::warn!("写缩略图缓存失败 {}：{error}", cache.display());
            } else {
                prune(cache_dir);
            }
            rendered
        }
    };
    Ok(format!("data:image/jpeg;base64,{}", base64::encode(&jpeg)))
}

/// 为一个源文件生成缩略图（缓存键自动算）。播放页的壁纸预览走这条。
pub fn data_url_for(source: &Path, cache_dir: &Path) -> Result<String, String> {
    data_url(source, cache_dir, &key_for(source))
}

/// 修剪缓存目录：超过 [`CACHE_LIMIT`] 时按修改时间删到只剩 [`PRUNE_TO`] 个。
///
/// 只删 `.jpg`（本目录只放这个），删不动（被占用、权限）就跳过——
/// 缓存本来就可以随时丢，不值得为它报错。
///
/// 每次生成新缩略图之后调用一次：目录没到上限时它只做一次 `read_dir` 与
/// 一次计数，代价可以忽略；到了上限时删掉的也是"最久没用过"的那些。
pub fn prune(cache_dir: &Path) {
    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jpg") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push((modified, path));
    }

    if files.len() <= CACHE_LIMIT {
        return;
    }
    // 最旧的在前，删掉多出来的那些。
    files.sort_by_key(|(modified, _)| *modified);
    let remove = files.len() - PRUNE_TO;
    let mut removed = 0usize;
    for (_, path) in files.into_iter().take(remove) {
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    log::info!("缩略图缓存已修剪：删掉 {removed} 个（保留最近 {PRUNE_TO} 个）");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-thumb-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写一张真 PNG（用 image 的编码器，免得手搓字节）。
    fn write_png(path: &Path, width: u32, height: u32) {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        image.save(path).unwrap();
    }

    #[test]
    fn a_rendered_thumbnail_is_a_real_jpeg() {
        let dir = temp_dir("render");
        let source = dir.join("shot.png");
        write_png(&source, 800, 600);

        let bytes = render(&source).unwrap();
        assert!(bytes.len() > 100, "生成的 JPEG 不该是空的");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG 必须以 SOI 开头");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_data_url_is_ready_for_an_img_tag() {
        let dir = temp_dir("dataurl");
        let source = dir.join("shot.png");
        write_png(&source, 64, 48);

        let url = data_url_for(&source, &dir.join("cache")).unwrap();
        assert!(
            url.starts_with("data:image/jpeg;base64,"),
            "要能直接塞进 img src：{}",
            &url[..40.min(url.len())]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_call_reuses_the_cache_instead_of_rendering_again() {
        // **这条守的是"缓存真的被复用了"。** 判据是缓存文件的修改时间：
        // 若第二次又压了一遍（几十兆像素的照片要几百毫秒），它必然被重写。
        let dir = temp_dir("cache");
        let source = dir.join("shot.png");
        write_png(&source, 64, 48);
        let cache = dir.join("cache");

        let first = data_url_for(&source, &cache).unwrap();
        let cached = cache.join(format!("{}.jpg", key_for(&source)));
        assert!(cached.is_file(), "第一次生成后应当留下缓存");
        let stamp = fs::metadata(&cached).unwrap().modified().unwrap();

        // 时间戳的精度在不同文件系统上不一样，等一小会儿再比，避免"本来就是同一刻"。
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = data_url_for(&source, &cache).unwrap();
        assert_eq!(first, second, "缓存命中时结果必须与第一次一致");
        assert_eq!(
            stamp,
            fs::metadata(&cached).unwrap().modified().unwrap(),
            "第二次不该重新生成缓存"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_key_follows_the_content_not_just_the_path() {
        // **这条守的是 AGENTS §7.14**：同一个路径换了内容（大小变了）必须得到
        // 不同的键，否则界面上会显示上一张图的缩略图。
        let dir = temp_dir("key");
        let source = dir.join("shot.png");
        write_png(&source, 64, 48);
        let before = key_for(&source);

        write_png(&source, 64, 96);
        let after = key_for(&source);
        assert_ne!(before, after, "换了内容就必须换键");

        // 同一个文件连着算两次必须一样（否则缓存永远不命中）。
        assert_eq!(after, key_for(&source));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_image_reports_a_reason_instead_of_a_blank_thumbnail() {
        let dir = temp_dir("broken");
        let source = dir.join("not-a-real.jpg");
        fs::write(&source, b"this is not an image at all").unwrap();

        let error = data_url_for(&source, &dir.join("cache")).unwrap_err();
        assert!(
            error.contains("尺寸") || error.contains("打不开"),
            "要说清是这张图的问题：{error}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_the_newest_entries_and_never_touches_other_files() {
        let dir = temp_dir("prune");
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();

        // 造 CACHE_LIMIT+5 个缓存文件，修改时间依次递增（每个文件往后 1 秒）。
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(10_000);
        for index in 0..CACHE_LIMIT + 5 {
            let path = cache.join(format!("{index:016x}.jpg"));
            fs::write(&path, b"jpeg").unwrap();
            let stamp = base + std::time::Duration::from_secs(index as u64);
            set_modified(&path, stamp);
        }
        // 一个不是 .jpg 的文件：修剪不该碰它。
        let keep = cache.join("readme.txt");
        fs::write(&keep, b"x").unwrap();

        prune(&cache);

        let left: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            left.iter().filter(|name| name.ends_with(".jpg")).count(),
            PRUNE_TO,
            "修剪之后应当正好留下 PRUNE_TO 个"
        );
        assert!(keep.is_file(), "非 .jpg 的文件不该被动");
        // 留下的必须是**最新的**那批：0000.jpg（最旧）应当已被删掉。
        assert!(
            !cache.join("0000000000000000.jpg").is_file(),
            "最旧的缓存应当先被删"
        );
        assert!(
            cache
                .join(format!("{:016x}.jpg", CACHE_LIMIT + 4))
                .is_file(),
            "最新的缓存必须留着"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_does_nothing_below_the_limit() {
        let dir = temp_dir("prune-small");
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        for index in 0..10 {
            fs::write(cache.join(format!("{index:016x}.jpg")), b"jpeg").unwrap();
        }
        prune(&cache);
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 10);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 把文件的修改时间设成给定时刻。
    ///
    /// `fs::File::set_modified` 是 Rust 1.75 起稳定的，用它而不是 Win32：
    /// 这里只是测试要造"谁更旧"。
    fn set_modified(path: &Path, stamp: std::time::SystemTime) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(stamp).unwrap();
    }
}

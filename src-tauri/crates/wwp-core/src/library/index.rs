//! 索引文件的读写。
//!
//! 格式是 JSONL（每行一条 JSON 记录），而不是一个大的 JSON 数组：
//!
//! - 可以流式写入，扫描过程中不必把全部记录留在内存里；
//! - 单行损坏只损失一行，不会让整个索引解析失败；
//! - 出问题时可以让人直接打开看，不需要专门的工具。
//!
//! 写入走"临时文件 + 改名"，进程中途被杀不会留下半截索引。

use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// 索引格式版本。字段含义变化时递增，旧文件会被识别并重建而不是解析出错。
pub const FORMAT_VERSION: u32 = 1;

/// 一个可播放的图库条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// 绝对路径。
    pub p: String,
    /// 显示宽（已按 EXIF 方向换算）。
    pub w: u32,
    /// 显示高（已按 EXIF 方向换算）。
    pub h: u32,
    /// 修改时间（Unix 秒）。取不到时为 0，此时每次都视为"需要重新探测"。
    pub mt: i64,
    /// 文件大小（字节）。
    pub sz: u64,
    /// EXIF 原始方向值，1 表示未旋转。
    #[serde(default = "default_orientation")]
    pub or: u16,
    /// 嗅探出的真实格式（不是扩展名）。
    pub fmt: String,
    /// 可解码为 1；内容认得出但本程序没有解码器为 0。
    ///
    /// `ok = 0` 的条目**参与统计但不参与播放**：留着它是为了让界面能回答
    /// "我那个 HEIC 去哪了"。
    pub ok: u8,
    /// 是视频还是图片。
    ///
    /// 用 `#[serde(default)]` 让旧索引（没有这个字段）能读进来，
    /// 默认当成图片——旧索引里本来也只有图片，因此这是正确的迁移语义。
    #[serde(default)]
    pub kind: MediaKind,
}

/// 条目的媒体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// 静态图片：由 `IDesktopWallpaper` 设为壁纸。
    #[default]
    Image,
    /// 视频：由桌面层的合成窗口播放。
    Video,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }
}

fn default_orientation() -> u16 {
    1
}

impl LibraryEntry {
    pub fn is_playable(&self) -> bool {
        self.ok == 1
    }

    pub fn is_video(&self) -> bool {
        self.kind == MediaKind::Video
    }

    pub fn is_image(&self) -> bool {
        self.kind == MediaKind::Image
    }

    /// 长宽比（宽/高）。高度为 0 时返回 0，避免除零。
    pub fn aspect(&self) -> f64 {
        if self.h == 0 {
            0.0
        } else {
            self.w as f64 / self.h as f64
        }
    }

    /// 内容是否与磁盘一致。不一致就需要重新探测。
    pub fn unchanged_since(&self, mtime: i64, size: u64) -> bool {
        self.mt == mtime && self.sz == size
    }
}

/// 扫描统计。这些数字会直接显示给用户，用于回答"为什么少了几张图"。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexStats {
    /// 目录里看到的所有文件数（含非图片）。
    pub files_seen: u64,
    /// 扩展名不是图片、直接跳过的文件数。
    pub files_ignored: u64,
    /// 进入候选、并成功读到尺寸的条目数。
    ///
    /// **包含视频**：视频也是"可播放"的条目，只是播放路径不同。
    /// 单独的视频数量见 [`IndexStats::videos`]。
    pub playable: u64,
    /// 其中有多少是视频。
    ///
    /// 单独计数是必要的：图片与视频的播放路径完全不同（图片交给
    /// `IDesktopWallpaper`，视频交给桌面层的合成窗口），因此统计里必须能分开
    /// 回答"这里面有多少个视频"。它同时是 [`IndexStats::playable`] 的**子集**，
    /// 统计守恒断言要按这个关系写。
    #[serde(default)]
    pub videos: u64,
    /// 内容认得出但没有解码器的条目数（HEIC 等）。
    pub unsupported: u64,
    /// 打开或解析失败的文件数（损坏文件）。
    pub unreadable: u64,
    /// 被排除规则挡掉的文件数。
    pub excluded: u64,
    /// 没有解码器的格式及其出现次数。
    pub unsupported_formats: std::collections::BTreeMap<String, u64>,
    /// 损坏或读不了的样本，最多保留 [`MAX_REPORTED_FAILURES`] 条。
    pub unreadable_samples: Vec<UnreadableSample>,
}

/// 最多记录多少条失败样本。图库里可能有上万个坏文件，全记下来只会撑爆索引。
pub const MAX_REPORTED_FAILURES: usize = 500;

impl IndexStats {
    pub fn record_unsupported(&mut self, format: &str) {
        self.unsupported += 1;
        *self
            .unsupported_formats
            .entry(format.to_string())
            .or_insert(0) += 1;
    }

    pub fn record_unreadable(&mut self, path: &str, reason: &str) {
        self.unreadable += 1;
        if self.unreadable_samples.len() < MAX_REPORTED_FAILURES {
            self.unreadable_samples.push(UnreadableSample {
                path: path.to_string(),
                reason: reason.to_string(),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnreadableSample {
    pub path: String,
    pub reason: String,
}

/// 索引的头部信息，单独一个小文件，便于不读全量就能知道概况。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexFile {
    pub format_version: u32,
    /// 图库根目录。
    pub root: String,
    /// 上次扫描完成时间（Unix 秒）。
    pub scanned_at: i64,
    /// 耗时（毫秒）。
    pub scan_millis: u64,
    /// 本次扫描是复用旧记录、还是重新探测。
    pub reused_entries: u64,
    pub probed_entries: u64,
    pub stats: IndexStats,
    /// 已从磁盘消失、本次被移除的条目数。
    pub removed: u64,
    /// 旧索引里**没有**、这次新出现的条目数。
    ///
    /// ## 为什么要有它（与 `probed_entries` 的区别）
    ///
    /// `probed_entries` 回答的是内部开销问题："这次打开了多少个文件读头部"。
    /// 它同时包含"新加进来的图"与"老图被改过"两类，而用户问的是后一个问题：
    /// **我的图库到底变了什么**。因此另外记这两个数，界面上就能直接说
    /// "新增 3、更新 1、删除 2"，而不是给一个"重探 4"让他自己猜。
    #[serde(default)]
    pub added: u64,
    /// 旧索引里有、但大小或修改时间变了的条目数（内容被替换过的图）。
    #[serde(default)]
    pub updated: u64,
}

impl IndexFile {
    pub fn new(root: &Path) -> IndexFile {
        IndexFile {
            format_version: FORMAT_VERSION,
            root: root.display().to_string(),
            scanned_at: 0,
            scan_millis: 0,
            reused_entries: 0,
            probed_entries: 0,
            stats: IndexStats::default(),
            removed: 0,
            added: 0,
            updated: 0,
        }
    }
}

/// 一个图库的全部落盘内容。
#[derive(Debug, Clone)]
pub struct IndexPaths {
    /// 索引目录：`data/index/<library-id>/`。
    pub dir: PathBuf,
    pub meta: PathBuf,
    pub rows: PathBuf,
}

impl IndexPaths {
    pub fn new(index_root: &Path, library_id: &str) -> IndexPaths {
        let dir = index_root.join(library_id);
        IndexPaths {
            meta: dir.join("index.meta.json"),
            rows: dir.join("index.jsonl"),
            dir,
        }
    }
}

/// 由图库根目录算出一个稳定的目录名。
///
/// 用自己实现的 FNV-1a 而不是引入哈希库：这里只需要"同一路径永远得到同一串、
/// 不同路径几乎不会撞车"，不涉及任何安全性质。
pub fn library_id(root: &Path) -> String {
    let normalized = crate::library::exclude::normalize(&root.to_string_lossy());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 从**别的**图库的索引里收集条目，供"新图库在已有图库下面"时复用。
///
/// ## 为什么要有这一条（用户实际遇到的情况）
///
/// 用户会把 `F:\素材` 整个加成一个图库，过一阵又把 `F:\素材\2024` 单独加一个。
/// 第二个图库里每个文件都已经在第一个的索引里有了尺寸与格式，可扫描仍然会
/// 逐个打开文件读头部——那正是扫描里最慢的一步。
///
/// ## 复用与复用的区别（这条很容易做错）
///
/// 这里**不判断文件还在不在**，也不判断"要不要用"：它只把候选记录交出去。
/// 真正的判据在 `scan.rs` 那一侧——文件大小与修改时间都与磁盘一致才算命中
/// （`LibraryEntry::unchanged_since`）。因此返回一大堆用不上的记录是安全的，
/// 代价只是内存；而少返回一条的代价是"那张图被重新探测一次"，也不影响正确性。
///
/// 调用方（外壳）自己决定把**哪些**图库的索引喂进来（见 `commands::library_scan`：
/// 只挑路径上是祖先的那些）。
pub fn collect_entries(
    index_root: &Path,
    library_roots: &[PathBuf],
) -> (Vec<LibraryEntry>, u64) {
    let mut entries = Vec::new();
    // 返回"还没有索引的图库数"而不是"读失败数"：调用方要判断的是
    // "这次到底有没有捞到可复用的数据"，而**没有索引**才是最常见的那种情况
    // （刚加的下级图库自己还没扫过，这完全是正常的）。
    let mut without_index = 0u64;
    for root in library_roots {
        let paths = IndexPaths::new(index_root, &library_id(root));
        if !paths.rows.exists() {
            without_index += 1;
            log::debug!("图库 {} 还没有索引，无可复用数据", root.display());
            continue;
        }
        match load(&paths) {
            Ok((_, rows)) => entries.extend(rows),
            Err(error) => {
                // 读不出来只是"没得复用"，不该让新图库扫不了。
                without_index += 1;
                log::warn!(
                    "读取图库 {} 的索引失败（不影响新图库扫描）：{error}",
                    root.display()
                );
            }
        }
    }
    (entries, without_index)
}

/// 在一批图库路径里挑出**是 `root` 祖先**的那些，按"最贴近 `root` 的排前面"。
///
/// ## 为什么需要它
///
/// 用户会把 `F:\素材` 整个加成一个图库，后来又单独加 `F:\素材\2024`。
/// 后者的数据其实已经在前者的索引里了，扫描时可以直接沿用
/// （见 [`collect_entries`] 与 `scan::scan_library` 的 `inherited` 参数）。
///
/// ## 两个容易做错的地方
///
/// 1. **按路径段比，不按字符串前缀比。** `F:\素材2` 的字符串开头就是 `F:\素材`，
///    但它不是它的子目录。这里先归一化（统一分隔符、去掉结尾的 `\`），
///    再要求前面的那一段后面紧跟分隔符。
/// 2. **自己不算自己的祖先。** 相等时排除，否则"把某个图库的数据当成外部数据
///    喂给自己"会绕过它自己那份索引的优先关系。
///
/// 顺序上"最贴近的排前面"：同一路径在多个祖先索引里都有记录时，
/// 越靠近自己的那份越可能还准确（调用方按顺序去重，先来后到）。
pub fn pick_ancestors(root: &Path, candidates: &[PathBuf]) -> Vec<PathBuf> {
    let target = crate::library::exclude::normalize(&root.to_string_lossy());
    let mut picked: Vec<(usize, PathBuf)> = candidates
        .iter()
        .filter_map(|candidate| {
            let text = crate::library::exclude::normalize(&candidate.to_string_lossy());
            if text.is_empty() || text == target {
                return None;
            }
            // 归一化之后分隔符一定是 `/`（**不是** `MAIN_SEPARATOR`——
            // `normalize` 把反斜杠换成了斜杠，这里用平台分隔符会一条都匹配不上）。
            let prefix = format!("{text}/");
            if target.starts_with(&prefix) {
                // 层级越深越贴近：前缀关系已经确定，因此路径越长就越靠近目标。
                Some((text.len(), candidate.clone()))
            } else {
                None
            }
        })
        .collect();
    picked.sort_by(|a, b| b.0.cmp(&a.0));
    picked.into_iter().map(|(_, path)| path).collect()
}

/// 读索引。文件不存在时返回空索引。
pub fn load(paths: &IndexPaths) -> Result<(IndexFile, Vec<LibraryEntry>)> {    if !paths.rows.exists() {
        let meta = IndexFile::new(Path::new(""));
        return Ok((meta, Vec::new()));
    }

    let meta = if paths.meta.exists() {
        let text = fs::read_to_string(&paths.meta)
            .map_err(|e| Error::io(format!("读取索引头失败 {}", paths.meta.display()), e))?;
        serde_json::from_str::<IndexFile>(&text).unwrap_or_else(|error| {
            log::warn!("索引头无法解析，将按新索引处理：{error}");
            IndexFile::new(Path::new(""))
        })
    } else {
        IndexFile::new(Path::new(""))
    };

    // 版本不一致：旧记录不可信，直接当作没有索引，让扫描重新探测。
    if meta.format_version != FORMAT_VERSION {
        log::info!(
            "索引格式版本不一致（文件 {}，当前 {}），将重新扫描",
            meta.format_version,
            FORMAT_VERSION
        );
        return Ok((IndexFile::new(Path::new(&meta.root)), Vec::new()));
    }

    let file = File::open(&paths.rows)
        .map_err(|e| Error::io(format!("打开索引失败 {}", paths.rows.display()), e))?;
    let reader = BufReader::new(file);

    let mut entries = Vec::new();
    let mut bad_lines = 0u64;
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                log::warn!("读取索引行失败，停止读取：{error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<LibraryEntry>(&line) {
            Ok(entry) => entries.push(entry),
            Err(_) => bad_lines += 1,
        }
    }
    if bad_lines > 0 {
        // 单行损坏只损失那一行，不影响其余条目。
        log::warn!("索引里有 {bad_lines} 行无法解析，已跳过");
    }

    Ok((meta, entries))
}

/// 写索引：先写临时文件，再改名覆盖。
pub fn save(paths: &IndexPaths, meta: &IndexFile, entries: &[LibraryEntry]) -> Result<()> {
    fs::create_dir_all(&paths.dir)
        .map_err(|e| Error::io(format!("创建索引目录失败 {}", paths.dir.display()), e))?;

    let tmp_rows = paths.rows.with_extension("jsonl.tmp");
    {
        let file = File::create(&tmp_rows)
            .map_err(|e| Error::io(format!("创建索引临时文件失败 {}", tmp_rows.display()), e))?;
        let mut writer = BufWriter::new(file);
        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| Error::config("序列化索引条目失败", e.to_string()))?;
            writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.write_all(b"\n"))
                .map_err(|e| Error::io("写入索引失败", e))?;
        }
        writer
            .flush()
            .map_err(|e| Error::io("刷新索引缓冲失败", e))?;
    }

    let tmp_meta = paths.meta.with_extension("json.tmp");
    let meta_text = serde_json::to_string_pretty(meta)
        .map_err(|e| Error::config("序列化索引头失败", e.to_string()))?;
    fs::write(&tmp_meta, meta_text)
        .map_err(|e| Error::io(format!("写入索引头失败 {}", tmp_meta.display()), e))?;

    // 改名是原子操作：要么看到旧索引，要么看到新索引。
    fs::rename(&tmp_rows, &paths.rows)
        .map_err(|e| Error::io(format!("替换索引失败 {}", paths.rows.display()), e))?;
    fs::rename(&tmp_meta, &paths.meta)
        .map_err(|e| Error::io(format!("替换索引头失败 {}", paths.meta.display()), e))?;

    Ok(())
}

/// 索引损坏时的处理：留档备份，然后当作空索引。
pub fn quarantine(paths: &IndexPaths) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = paths.rows.with_extension(format!("jsonl.bad-{stamp}"));
    if paths.rows.exists() {
        fs::rename(&paths.rows, &backup)
            .map_err(|e| Error::io(format!("备份损坏索引失败 {}", paths.rows.display()), e))?;
        log::warn!("索引已备份到 {}，将重新建立", backup.display());
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-index-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_entry(path: &str, w: u32, h: u32) -> LibraryEntry {
        LibraryEntry {
            p: path.to_string(),
            w,
            h,
            mt: 1_700_000_000,
            sz: 1234,
            or: 1,
            fmt: "jpeg".to_string(),
            ok: 1,
                kind: crate::library::index::MediaKind::Image,
        }
    }

    #[test]
    fn library_id_is_stable_and_case_insensitive() {
        let a = library_id(Path::new(r"D:\Pics"));
        let b = library_id(Path::new(r"d:/pics"));
        let c = library_id(Path::new(r"D:\Pics\Other"));
        assert_eq!(a, b, "同一目录的写法差异不应产生不同 id");
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("roundtrip");
        let paths = IndexPaths::new(&dir, "lib1");
        let entries = vec![
            sample_entry(r"D:\Pics\a.jpg", 1920, 1080),
            sample_entry(r"D:\Pics\b.png", 800, 600),
        ];
        let mut meta = IndexFile::new(Path::new(r"D:\Pics"));
        meta.scanned_at = 42;
        meta.stats.playable = 2;

        save(&paths, &meta, &entries).unwrap();
        let (loaded_meta, loaded) = load(&paths).unwrap();

        assert_eq!(loaded, entries);
        assert_eq!(loaded_meta.scanned_at, 42);
        assert_eq!(loaded_meta.stats.playable, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_index_loads_as_empty() {
        let dir = temp_dir("missing");
        let paths = IndexPaths::new(&dir, "lib");
        let (meta, entries) = load(&paths).unwrap();
        assert!(entries.is_empty());
        assert_eq!(meta.format_version, FORMAT_VERSION);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_only_loses_that_line() {
        let dir = temp_dir("corrupt");
        let paths = IndexPaths::new(&dir, "lib");
        let entries = vec![
            sample_entry(r"D:\Pics\a.jpg", 100, 100),
            sample_entry(r"D:\Pics\b.jpg", 200, 200),
        ];
        save(&paths, &IndexFile::new(Path::new(r"D:\Pics")), &entries).unwrap();

        // 手工插入一行垃圾。
        let text = fs::read_to_string(&paths.rows).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.insert(1, "{ 这不是合法 JSON");
        fs::write(&paths.rows, lines.join("\n")).unwrap();

        let (_, loaded) = load(&paths).unwrap();
        assert_eq!(loaded.len(), 2, "两行合法记录都应保留");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_mismatch_invalidates_the_index() {
        let dir = temp_dir("version");
        let paths = IndexPaths::new(&dir, "lib");
        let entries = vec![sample_entry(r"D:\Pics\a.jpg", 100, 100)];
        let mut meta = IndexFile::new(Path::new(r"D:\Pics"));
        meta.format_version = FORMAT_VERSION + 1;
        save(&paths, &meta, &entries).unwrap();

        let (_, loaded) = load(&paths).unwrap();
        assert!(loaded.is_empty(), "版本不符时必须重新扫描，而不是勉强解析");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_since_detects_edits_by_size_or_mtime() {
        let entry = sample_entry("x", 1, 1);
        assert!(entry.unchanged_since(entry.mt, entry.sz));
        assert!(!entry.unchanged_since(entry.mt + 1, entry.sz), "mtime 变了要重扫");
        assert!(!entry.unchanged_since(entry.mt, entry.sz + 1), "大小变了要重扫");
    }

    #[test]
    fn aspect_handles_zero_height() {
        let mut entry = sample_entry("x", 100, 0);
        assert_eq!(entry.aspect(), 0.0);
        entry.h = 200;
        assert_eq!(entry.aspect(), 0.5);
    }

    #[test]
    fn failure_samples_are_capped() {
        let mut stats = IndexStats::default();
        for i in 0..(MAX_REPORTED_FAILURES + 50) {
            stats.record_unreadable(&format!("f{i}.jpg"), "坏了");
        }
        assert_eq!(stats.unreadable, (MAX_REPORTED_FAILURES + 50) as u64);
        assert_eq!(stats.unreadable_samples.len(), MAX_REPORTED_FAILURES);
    }

    #[test]
    fn ancestors_are_matched_by_path_segment_not_by_string_prefix() {
        let candidates = vec![
            PathBuf::from(r"F:\素材"),
            // 字符串上以 `F:\素材` 开头，但它**不是**子目录。
            PathBuf::from(r"F:\素材2"),
            PathBuf::from(r"F:\素材\2024"),
            PathBuf::from(r"F:\其它"),
            // 与目标相同：自己不算自己的祖先。
            PathBuf::from(r"F:\素材\2024\旅行"),
        ];

        let picked = pick_ancestors(Path::new(r"F:\素材\2024\旅行"), &candidates);
        let texts: Vec<String> = picked.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            texts,
            vec![r"F:\素材\2024".to_string(), r"F:\素材".to_string()],
            "只挑真正的祖先，且最贴近的排前面"
        );

        // 末尾多写一个分隔符、大小写不同、斜杠方向不同，都应当认出是同一个祖先。
        assert_eq!(
            pick_ancestors(Path::new(r"f:/素材/2024/旅行/"), &candidates).len(),
            2
        );
        // 不相干的路径一条都不挑。
        assert!(pick_ancestors(Path::new(r"E:\图"), &candidates).is_empty());
    }

    #[test]
    fn collect_entries_gathers_from_several_indexes_and_survives_missing_ones() {
        let dir = temp_dir("collect");
        let index_root = dir.join("index");
        let parent = dir.join("素材");
        let child = dir.join("素材").join("2024");

        // 父图库有索引；子图库还没有（这正是"新加一个下级图库"的现场）。
        let parent_paths = IndexPaths::new(&index_root, &library_id(&parent));
        save(
            &parent_paths,
            &IndexFile::new(&parent),
            &[
                sample_entry(&format!(r"{}\a.jpg", parent.display()), 100, 200),
                sample_entry(&format!(r"{}\2024\b.jpg", parent.display()), 300, 400),
            ],
        )
        .unwrap();

        let (entries, skipped) = collect_entries(&index_root, &[parent.clone(), child.clone()]);
        assert_eq!(entries.len(), 2, "父图库的两条都该收集到");
        assert_eq!(skipped, 1, "子图库还没有索引，应当被记为跳过而不是报错");

        let _ = fs::remove_dir_all(&dir);
    }
}

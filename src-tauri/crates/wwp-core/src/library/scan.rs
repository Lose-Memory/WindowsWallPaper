//! 图库扫描。
//!
//! 扫描分两步，这是"几万张图也能流畅"的前提：
//!
//! 1. **遍历**（单线程，I/O 受限）：只做扩展名初筛与排除规则判定，不打开文件。
//! 2. **探测**（有界并行）：只对"新文件"和"内容变过的文件"读头部拿尺寸。
//!    内容没变的文件直接沿用旧索引记录，**完全不打开**。
//!
//! 进度通过回调上报而不是返回值，因为一次扫描可能持续几分钟，
//! 界面需要边扫边看；取消也走同一个回调（返回 `false` 即中止）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use walkdir::WalkDir;

use super::exclude::{normalize, ExcludeMatcher};
use super::index::{
    self, IndexFile, IndexPaths, IndexStats, LibraryEntry, UnreadableSample,
};
use super::probe::{self, Probe};
use crate::config::{ExcludeSettings, Settings};
use crate::{Error, Result};

/// 扫描参数。
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// 是否复用旧索引里未变化的记录（增量重扫）。为 false 时强制全部重新探测。
    pub incremental: bool,
    /// 探测阶段的并行度。默认按 CPU 核数，上限 12。
    ///
    /// 上限而不是"无条件等于核心数"是有意的：每个 worker 都在读文件，
    /// 并行度过高只会把磁盘队列拉长。实测（`--scan --threads N`）热缓存下
    /// 4/8/12/16/24 线程的探测耗时在 456~572 ms 之间波动、没有单调趋势，
    /// 说明缓存命中时瓶颈不在并发；只有冷缓存下加线程才有明显收益。
    pub threads: usize,
    /// 进度回调的节流：每处理这么多文件回报一次。
    pub progress_every: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        ScanOptions {
            incremental: true,
            // 上限 12 而不是 8。机器是 12 逻辑核心，原先硬性砍到 8 等于
            // 白白放弃 4 个逻辑核心。实测（`--scan --threads N` 对照实验）
            // 热缓存下并行度几乎无影响，冷缓存下才有效——因此上限给到
            // 核心数，但不再高于它：并行度过高只会把磁盘队列拉长。
            threads: cores.clamp(1, 12),
            progress_every: 200,
        }
    }
}

/// 遍历阶段每访问这么多个目录就上报一次进度。
///
/// ## 为什么按目录数，而不是复用 `progress_every`（文件数）
///
/// 实测用户报告"扫描的准备阶段还是很久、界面一直显示正在准备"。
/// 原因是当时的门槛是"看到 200 个文件"，而遍历的开销几乎全在**目录**上。
///
/// 两个实测例子（都是未扫过的目录，因此接近冷缓存）：
///
///   | 目录 | 目录数 | 遍历 | 探测 | 文件数门槛下首报 |
///   |---|---|---|---|---|
///   | `D:\Projects` | 41,734 | 4.7 s | 0.6 s | 约 1 s |
///   | `C:\Users\<用户>` | 35,799 | **29.9 s** | 4.9 s | 约 1 s |
///
/// 注意第二行：**目录树大时遍历占绝对多数**（29.9 s vs 4.9 s），
/// 与"图库文件多所以探测慢"的直觉相反。这正是用户感觉"准备阶段很久"
/// 的来源，也说明按目录数上报是对的方向。
///
/// ## 为什么是 16
///
/// 实测 32 时首批反馈约在 3,401 个目录后（不到 1 秒，已经够快）。
/// 改成 16 让反馈再提前一倍——代价只是多一点回调，
/// 而遍历本身每次 `read_dir` 就是一次系统调用，多几次进度上报可以忽略。
const WALK_DIR_STEP: u64 = 16;

/// 遍历阶段两次上报之间的**最大**间隔。
///
/// 计数触发（目录数 / 文件数）在比例极端时可能很久才满足，因此再加一条
/// 按时间的兜底：无论图库长什么样，界面每 500 ms 内必定拿到一次新数字。
/// 这直接消掉了"卡在正在准备"的观感——即使某个阶段真的很慢，
/// 用户也能看到"已访问 N 个目录"在持续增长，从而区分"在动"与"死了"。
const WALK_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// 一次进度上报。
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// 已遍历到的文件数。
    pub files_seen: u64,
    /// 已进入候选（扩展名像是图片且未被排除）的文件数。
    pub candidates: u64,
    /// 已确定可播放的条目数。仅在扫描结束后有准确值。
    pub playable: u64,
    /// 没有解码器的条目数。仅在扫描结束后有准确值。
    pub unsupported: u64,
    /// 读不了的条目数。仅在扫描结束后有准确值。
    pub unreadable: u64,
    /// 复用旧记录、未重新打开的条目数。
    pub reused: u64,
    /// 其中有多少条是从**别的图库**的索引里沿用的（见 [`scan_library`] 的 `inherited`）。
    ///
    /// 单独报这个数是因为用户会问"这次为什么这么快"：`reused` 只说明"没打开文件"，
    /// 而这一条说明"数据是从哪来的"。本图库自己的旧索引与祖先图库的索引
    /// 对用户是两件不同的事（前者是"我扫过它"，后者是"我扫过它的上级目录"）。
    pub reused_inherited: u64,
    /// 探测阶段已完成的条目数（进度条用这个）。
    pub processed: u64,
    /// 当前正在处理的路径。
    pub current: String,
}

/// 扫描结果。
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub meta: IndexFile,
    pub entries: Vec<LibraryEntry>,
    /// 是否被调用方取消。
    pub cancelled: bool,
}

/// 扫描一个图库根目录并把索引写盘。
///
/// `should_continue` 返回 `false` 即中止；中止时**不写索引**，磁盘上仍是上一次的
/// 完整索引。这一点是刻意的：半截索引会让人以为图少了。
///
/// ## `inherited`：从别的图库沿用条目（用户提的"上一级已经是图库了"）
///
/// 传进来的是一批**候选**记录（通常来自祖先图库的索引，由外壳挑选，见
/// `index::collect_entries`）。某一条只有满足下面全部条件才会真正被沿用：
///
/// - 本图库自己的索引里**没有**这条路径（有的话以自己那份为准——它更权威）；
/// - 文件大小与修改时间都与磁盘一致（`LibraryEntry::unchanged_since`）；
/// - 本次不是强制全量重扫（`incremental`）。
///
/// 因此这条捷径**不改变结果**：沿用的记录与重新探测得到的记录是同一份内容
/// （尺寸、格式、EXIF 方向都来自同一次探测）。省掉的只是"打开文件读头部"。
///
/// ## 为什么遍历阶段照常走
///
/// "直接把祖先索引过滤一下"看起来能省掉遍历，但会漏两类东西：祖先图库**排除过**
/// 的子目录（新图库并没有排除它）与祖先索引建立之后新增的文件。而且那样得到的
/// 统计数字（看过多少文件、跳过多少非图片）是编的。遍历本身在热缓存下很快
/// （实测四万目录约 5 秒），慢的是逐个打开文件——那正是这里省掉的。
pub fn scan_library(
    root: &Path,
    index_root: &Path,
    settings: &Settings,
    options: &ScanOptions,
    inherited: &[LibraryEntry],
    mut on_progress: impl FnMut(&ScanProgress) -> bool,
) -> Result<ScanOutcome> {
    let started = std::time::Instant::now();
    let paths = IndexPaths::new(index_root, &index::library_id(root));

    // **立刻报一次进度，在做任何事之前。**
    //
    // ## 为什么这一条是必要的（不是可有可无的礼貌）
    //
    // 界面在"收到第一个进度事件之前"只能显示「正在准备…」——它一个数字都没有。
    // 而这几步都可能明显耗时：
    //   - `root.is_dir()`：网络盘或刚断开的盘上可能阻塞几秒到几十秒；
    //   - `index::load`：几万条的索引要读文件 + 逐行解析 JSON；
    //   - `index::library_id`：对很长的路径做哈希（很快，但也在前面）。
    //
    // 这段时间里界面看起来就是**卡死**，而用户无法区分"在准备"与"已经死了"。
    // 用户报过两次这个问题，因此这里不再依赖"后面的上报会来救我"。
    //
    // 事件在扫描一开始就发出，界面立刻从「正在准备…」切到有内容的显示
    // （已用时秒数会开始走），即使后续阶段很慢，用户也能看出它在动。
    let mut cancelled = false;
    {
        let initial = ScanProgress {
            files_seen: 0,
            candidates: 0,
            playable: 0,
            unsupported: 0,
            unreadable: 0,
            reused: 0,
            reused_inherited: 0,
            processed: 0,
            current: "正在读取旧索引…".to_string(),
        };
        if !on_progress(&initial) {
            // 还没开始就要求取消：直接返回一个"已取消"的结果，
            // 与后面取消的语义一致（不写索引）。
            return Ok(ScanOutcome {
                meta: IndexFile::new(root),
                entries: Vec::new(),
                cancelled: true,
            });
        }
    }

    if !root.is_dir() {
        return Err(Error::not_found(format!("图库目录 {}", root.display())));
    }

    // --- 旧索引：既用于增量复用，也用于统计"消失了多少条" ---
    let (_, old_entries) = index::load(&paths).unwrap_or_else(|error| {
        log::warn!("读取旧索引失败，将做一次全量扫描：{error}");
        (IndexFile::new(root), Vec::new())
    });
    // 复用的开关。两种"有东西可复用"都算：
    //
    // - 本图库自己的旧索引非空（常规的增量重扫）；
    // - 或者有祖先图库的记录可用（**新加的下级图库**就是这一种：它自己的索引
    //   还不存在，但那批文件的数据就在上级图库的索引里）。
    //
    // 漏掉第二种会怎样：用户按"上级已经是图库了"的期待加了个下级图库，
    // 结果第一次扫描仍然把每个文件都重新打开一遍——正是他要避免的那件事。
    // 全量重扫（`options.incremental == false`）两种都不许用。
    let incremental = options.incremental && (!old_entries.is_empty() || !inherited.is_empty());
    // 旧记录的按路径索引**始终建**（不再只在增量时才建）。
    //
    // 两件事都要用它：增量时的"能不能复用"，以及**无论增量还是全量**都要算的
    // "新增 / 更新 / 删除"。原先非增量时这里给一个空表，于是全量重扫的结果里
    // 每条都算"新增"——数字是错的，而"我的图库多了什么"正是用户要看的那个数。
    //
    // 台账只是查表（不打开文件），几万条也就几毫秒，且全量重扫本来就更贵。
    let old_by_path: HashMap<String, LibraryEntry> = old_entries
        .iter()
        .map(|entry| (normalize(&entry.p), entry.clone()))
        .collect();
    let old_count = old_by_path.len();

    // --- 祖先图库的记录：只用于"不用打开文件也知道尺寸" ---
    //
    // 建表时**先来后到**：同一路径出现两次时保留先来的那条。外壳按"最近的祖先
    // 优先"传进来，因此先来的那条就是更贴近新图库的那一份。
    let mut inherited_by_path: HashMap<String, LibraryEntry> = HashMap::with_capacity(inherited.len());
    for entry in inherited {
        let key = normalize(&entry.p);
        // 本图库自己索引里有的路径以自己那份为准：它才是"这个图库"的历史。
        if old_by_path.contains_key(&key) {
            continue;
        }
        inherited_by_path.entry(key).or_insert_with(|| entry.clone());
    }
    log::info!(
        "图库 {}：可沿用的外部记录 {} 条（自己的旧索引 {} 条）",
        root.display(),
        inherited_by_path.len(),
        old_count
    );

    let matcher = ExcludeMatcher::new(&settings.exclude);

    // --- 第一步：遍历，收集候选文件 ---
    let mut stats = IndexStats::default();
    let mut candidates: Vec<CandidateFile> = Vec::new();
    let mut last_report = 0u64;
    // 被排除规则整棵剪掉的目录数。剪枝发生在遍历的过滤回调里，回调必须是 Fn，
    // 因此用原子计数把它带出来。
    let pruned_dirs = std::sync::atomic::AtomicU64::new(0);
    // 已访问的目录数。遍历开销几乎全在目录上，因此进度按它来报更贴合真实耗时。
    let walked_dirs = std::sync::atomic::AtomicU64::new(0);

    // 阶段计时。分开测是因为"扫描慢"有三个完全不同的可能原因，
    // 而它们的优化手段完全不同：
    //   - 遍历慢 → 目录树太深/太多，或排除规则的正则回溯
    //   - 探测慢 → 磁盘随机读 + 每文件解码开销
    //   - 写索引慢 → 索引太大或磁盘慢
    // 不分阶段测就只能猜，而"猜错方向"的优化是纯粹的浪费。
    let walk_started = std::time::Instant::now();
    // 遍历阶段的下次上报门槛：文件数与目录数各一个，谁先到就报。
    let mut next_walk_report = options.progress_every.max(1);
    let mut next_walk_dir_report = WALK_DIR_STEP;
    // 上次上报的时刻。用于"无论如何每 WALK_REPORT_INTERVAL 必须报一次"。
    let mut last_walk_report = std::time::Instant::now();

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        // walkdir 的错误（权限不足等）不应该让整次扫描失败。
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let path = entry.path();
            if entry.file_type().is_dir() {
                walked_dirs.fetch_add(1, Ordering::Relaxed);
                if let Some(hit) = matcher.should_skip_dir(path) {
                    log::debug!("跳过目录 {}（{}）", path.display(), hit.describe());
                    pruned_dirs.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
            true
        });

    for item in walker {
        // **遍历阶段必须能取消、也必须频繁报进度。**
        //
        // 这里原先两件事都没做好，后果是：
        //   - 用户点"取消扫描"后，如果扫描还在遍历（大目录树、网络盘、
        //     机械盘上尤其久），它会一直走完才停——看起来就是"点了取消没用"；
        //   - 界面上"正在准备"那一段正是遍历阶段，拿不到任何数字。
        //
        // ## 为什么按**目录数**而不是文件数触发上报
        //
        // 原先用 `files_seen >= progress_every`（200 个文件）作为门槛。
        // 但遍历的开销几乎全在目录上：一个图库可能在找到头 200 个文件之前
        // 已经访问了几百个目录（每个目录一次 `read_dir`，机械盘上可能几十毫秒）。
        // 实测用户报告"准备阶段还是很久"就是这个原因——门槛迟迟不满足，
        // 界面只能一直显示"正在准备"。
        //
        // 改成目录数触发后，反馈来得早得多，而且与真实的耗时来源对齐。
        // 文件数仍然参与判断（`files_seen`），两者取先满足的那个。
        //
        // **再加上"距上次上报超过 WALK_REPORT_INTERVAL"这一条。**
        //
        // 为什么光有计数触发不够：目录数与文件数的比例因图库而异。
        // 实测 `C:\Program Files` 是 11 个文件/目录，`C:\Windows` 是 2.5。
        // 在"一个目录里塞了几千个文件"的极端情况下，16 个目录的门槛可能
        // 很久才满足——那段时间界面又变成没有任何数字可显示。
        //
        // 时间触发保证**上报节奏的上界与目录结构无关**：无论图库长什么样，
        // 界面每 500 ms 内必定拿到一次新数字。
        let dirs_now = walked_dirs.load(Ordering::Relaxed);
        let due = stats.files_seen >= next_walk_report
            || dirs_now >= next_walk_dir_report
            || last_walk_report.elapsed() >= WALK_REPORT_INTERVAL;
        if due {
            next_walk_report = stats.files_seen + options.progress_every;
            last_walk_report = std::time::Instant::now();
            next_walk_dir_report = dirs_now + WALK_DIR_STEP;
            let progress = ScanProgress {
                files_seen: stats.files_seen,
                // 遍历还没结束，候选总数未知——这正是界面需要知道的：
                // 这个阶段的分母不存在，所以进度条要用"不确定"样式。
                candidates: candidates.len() as u64,
                playable: 0,
                unsupported: 0,
                unreadable: stats.unreadable,
                reused: 0,
                reused_inherited: 0,
                processed: 0,
                current: format!(
                    "正在遍历目录（已访问 {} 个目录、看到 {} 个文件）",
                    dirs_now, stats.files_seen
                ),
            };
            if !on_progress(&progress) {
                cancelled = true;
                log::info!("遍历阶段收到取消请求，已停止收集候选");
                break;
            }
        }

        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                // 记录到失败样本里：权限问题必须让用户看得见。
                let path = error
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| root.display().to_string());
                stats.record_unreadable(&path, &format!("遍历失败：{error}"));
                continue;
            }
        };

        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            // 符号链接、设备文件等一律跳过：图库扫描不该跟着链接跑出目录树。
            continue;
        }

        stats.files_seen += 1;
        let path = entry.path();

        let extension = probe::extension_of(path);
        if !probe::is_candidate_extension(&extension) {
            stats.files_ignored += 1;
            continue;
        }
        if let Some(hit) = matcher.matches_file(path) {
            stats.excluded += 1;
            // 被排除也要计入"看到过"。否则用户看到的总数会比目录里实际的文件少，
            // 而"少了多少、为什么少"正是这份统计要回答的问题。
            stats.files_seen += 1;
            log::debug!("排除 {}（{}）", path.display(), hit.describe());
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                stats.record_unreadable(&path.display().to_string(), &format!("读取属性失败：{error}"));
                continue;
            }
        };

        candidates.push(CandidateFile {
            path: path.to_path_buf(),
            size: metadata.len(),
            mtime: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });

        if stats.files_seen - last_report >= options.progress_every {
            last_report = stats.files_seen;
            let progress = ScanProgress {
                files_seen: stats.files_seen,
                candidates: candidates.len() as u64,
                playable: 0,
                unsupported: 0,
                unreadable: stats.unreadable,
                reused: 0,
                reused_inherited: 0,
                processed: 0,
                current: path.display().to_string(),
            };
            if !on_progress(&progress) {
                cancelled = true;
                break;
            }
        }
    }

    let candidate_total = candidates.len() as u64;
    let walk_millis = walk_started.elapsed().as_millis() as u64;
    let probe_started = std::time::Instant::now();

    // --- 第二步：并行探测（未取消时才做）---
    let mut entries: Vec<LibraryEntry> = Vec::with_capacity(candidates.len());
    let mut reused_count = 0u64;
    let mut probed_count = 0u64;
    // 变化的台账：这次新出现的、内容变了的、以及"重新打开过但内容没变"的。
    //
    // 第三个计数（`probed_unchanged_count`）是算 `removed` 所必需的：
    // removed 的语义是"旧索引里有、这次没出现在候选里"，而"重新探测过"不等于
    // "消失"——把它算进 removed 会得到一个自相矛盾的结果（同一条既在索引里、
    // 又被报成已消失）。原先只减 `reused_count` 就有这个毛病。
    let mut added_count = 0u64;
    let mut updated_count = 0u64;
    let mut probed_unchanged_count = 0u64;
    // 其中有多少条是从**别的图库**的索引沿用的（见 `inherited_by_path`）。
    let mut inherited_count = 0u64;

    if !cancelled {
        let stop = AtomicBool::new(false);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.threads.max(1))
            .build()
            .map_err(|e| Error::config("创建扫描线程池失败", e.to_string()))?;

        // 分批：批内在多个线程上并行探测，批与批之间回到本线程上报进度、
        // 检查取消。这样进度回调永远只在主线程被调用，既不需要回调本身是 Sync，
        // 也让"取消后最多再多做一批"这件事有个明确的粒度。
        //
        // ## 批次大小为什么按图库规模自适应
        //
        // 批次同时决定进度更新的粒度：一批只上报一次（外加批内的轮询）。
        // 固定 4096 时实测过一个问题——439 张的图库整个落在**一批**里，
        // 于是进度始终显示"已处理 0/439"，结束瞬间跳到 439/439，
        // 进度条看起来就是卡住的。
        //
        // 但也不能一味调小：小批次意味着更多次"线程池同步 + 进度回调"，
        // 对五万张的库是纯浪费（那时用户也不需要每 0.1 秒更新一次）。
        //
        // 实测参考：439 张 3000 万像素照片约 0.5 秒、948 张约 1 秒（文件缓存热时）。
        // 因此目标是"每批约 0.2~1 秒"，用规模反推批次：
        //   - 1 万张以内 → 256 张/批（约 10~40 次更新，进度条能动起来）
        //   - 更大 → 4096 张/批（大库本来就要跑几秒到几十秒，粗一点也够）
        const CHUNK_SMALL: usize = 256;
        const CHUNK_LARGE: usize = 4096;
        const SMALL_LIBRARY: usize = 10_000;

        let chunk_size = if candidate_total as usize <= SMALL_LIBRARY {
            CHUNK_SMALL
        } else {
            CHUNK_LARGE
        };
        log::debug!(
            "扫描分批：{candidate_total} 个候选按 {chunk_size} 张/批（用于进度上报）"
        );

        // 批内上报进度的轮询间隔。
        //
        // 10 ms 而不是 50 ms：这个睡眠**出现在每次扫描的关键路径上**
        // （批内轮询的最后一次必然要睡一下才发现已经做完），因此它直接加到
        // 扫描耗时里。实测过 50 ms 的版本会让一个 12 个小文件的增量扫描
        // 从 1 ms 变成 50 ms——而且这还让一条"增量扫描应当更快"的测试
        // 变成了随机失败（首次 50ms、二次 51ms）。
        //
        // 10 ms 的代价是每秒最多 100 次空转，相对一次扫描可以忽略；
        // 而它对进度的连续性也够用（进度条以动画过渡到新宽度）。
        const PROGRESS_POLL: std::time::Duration = std::time::Duration::from_millis(10);

        let mut probe_stats = IndexStats::default();
        let mut processed = 0u64;

        for chunk in candidates.chunks(chunk_size) {
            if stop.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }

            // **批内也要能上报进度**，否则小图库只会在整批结束时跳一次。
            //
            // 实测过这个 bug：439 个文件（不到一批）时，进度始终是
            // "已处理 0/439"，结束瞬间跳到 439/439——进度条看起来就是卡住的。
            // 原因是原先只在整批探测完成后才累加 `processed`。
            //
            // 做法：用一个原子计数器记录批内已完成数；把并行探测放到**工作线程**
            // 上跑，主线程在它运行期间轮询这个计数器并上报。这样既保留"回调只在
            // 主线程"的性质，又能拿到批内进度。
            let batch_done = std::sync::atomic::AtomicU64::new(0);
            let stop_ref = &stop;
            let old_ref = &old_by_path;
            let inherited_ref = &inherited_by_path;
            let done_ref = &batch_done;
            // `&pool` 而不是 `pool`：池要被每一批复用，不能被第一轮 move 走。
            // `thread::scope` 的闭包捕获引用，因此借用只持续到该批结束。
            let pool_ref = &pool;

            let probed: Vec<ProbeOutcome> = std::thread::scope(|scope| {
                let worker = scope.spawn(move || {
                    pool_ref.install(|| {
                        chunk
                            .par_iter()
                            .map(|candidate| {
                                let key = normalize(&candidate.path.to_string_lossy());
                                let from_self = old_ref.get(&key).filter(|previous| {
                                    // **只有增量扫描才复用。** 强制全量重扫必须真的重新
                                    // 打开每个文件——而台账现在无论如何都建好了，
                                    // 少了这个判断就会让"全量重扫"静默变成增量。
                                    incremental
                                        && previous.unchanged_since(
                                            candidate.mtime,
                                            candidate.size,
                                        )
                                });
                                if let Some(previous) = from_self {
                                    // 增量命中的关键路径：不打开文件。
                                    done_ref.fetch_add(1, Ordering::Relaxed);
                                    return ProbeOutcome::Reused(previous.clone(), false);
                                }

                                // 本图库自己没记录（或记录对不上）：看看祖先图库那份
                                // 能不能用。判据完全一样——大小与修改时间都要对得上。
                                let from_ancestor = inherited_ref.get(&key).filter(|previous| {
                                    incremental
                                        && previous.unchanged_since(
                                            candidate.mtime,
                                            candidate.size,
                                        )
                                });
                                let outcome = match from_ancestor {
                                    Some(previous) => ProbeOutcome::Reused(previous.clone(), true),
                                    None => ProbeOutcome::Probed(probe_one(candidate)),
                                };
                                done_ref.fetch_add(1, Ordering::Relaxed);
                                outcome
                            })
                            .collect::<Vec<ProbeOutcome>>()
                    })
                });

                // 主线程：轮询批内进度并上报，同时保持对取消的响应。
                loop {
                    if stop_ref.load(Ordering::Relaxed) {
                        cancelled = true;
                        // 让这一批做完再走：并行任务没法中途叫停，
                        // 强杀线程池会留下半完成的索引。
                        break;
                    }
                    let in_batch = batch_done.load(Ordering::Relaxed);
                    if in_batch >= chunk.len() as u64 {
                        break;
                    }
                    let progress = ScanProgress {
                        files_seen: stats.files_seen,
                        candidates: candidate_total,
                        playable: probe_stats.playable,
                        unsupported: probe_stats.unsupported,
                        unreadable: probe_stats.unreadable,
                        reused: reused_count,
                        reused_inherited: inherited_count,
                        processed: processed + in_batch,
                        current: chunk
                            .get(in_batch as usize)
                            .map(|c| c.path.display().to_string())
                            .unwrap_or_default(),
                    };
                    if !on_progress(&progress) {
                        cancelled = true;
                        stop.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(PROGRESS_POLL);
                }

                worker.join().unwrap_or_default()
            });

            for outcome in probed {
                match outcome {
                    ProbeOutcome::Reused(entry, from_ancestor) => {
                        reused_count += 1;
                        if from_ancestor {
                            inherited_count += 1;
                            // 台账口径：**沿用来的这条路对"这个图库"而言是新的**。
                            // 它已经在别处的索引里，但这个图库的索引里还没有它，
                            // 用户在这一页看到的"新增"说的就是这件事。
                            // 不算进来的话，子图库第一次扫描会报"新增 0"，
                            // 而它的索引事实上从 0 条变成了 N 条。
                            added_count += 1;
                        }
                        entries.push(entry);
                    }
                    ProbeOutcome::Probed((entry, mut local)) => {
                        // 台账：这条路径以前有没有记录、记录还对不对得上。
                        // 用条目自己的 `mt`/`sz`（它们就是这次从磁盘读到的值）。
                        match old_by_path.get(&normalize(&entry.p)) {
                            None => added_count += 1,
                            Some(previous) if previous.unchanged_since(entry.mt, entry.sz) => {
                                // 全量重扫时走到这里：重新打开过，但内容没变。
                                // 它既不是新增也不是更新，更不能算作"已消失"。
                                probed_unchanged_count += 1;
                            }
                            Some(_) => updated_count += 1,
                        }
                        entries.push(entry);
                        probe_stats.playable += local.playable;
                        probe_stats.videos += local.videos;
                        probe_stats.unsupported += local.unsupported;
                        probe_stats.unreadable += local.unreadable;
                        // 用 take 而不是 drain：BTreeMap 没有 drain。
                        for (format, count) in std::mem::take(&mut local.unsupported_formats) {
                            *probe_stats.unsupported_formats.entry(format).or_insert(0) += count;
                        }
                        probe_stats.unreadable_samples.append(&mut local.unreadable_samples);
                    }
                }
            }

            processed += chunk.len() as u64;
            let last = chunk.last().map(|c| c.path.display().to_string());
            let progress = ScanProgress {
                files_seen: stats.files_seen,
                candidates: candidate_total,
                playable: probe_stats.playable,
                unsupported: probe_stats.unsupported,
                unreadable: probe_stats.unreadable,
                reused: reused_count,
                reused_inherited: inherited_count,
                processed,
                current: last.unwrap_or_default(),
            };
            if !on_progress(&progress) {
                cancelled = true;
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }

        stats.playable += probe_stats.playable;
        stats.videos += probe_stats.videos;
        stats.unsupported += probe_stats.unsupported;
        stats.unreadable += probe_stats.unreadable;
        for (format, count) in std::mem::take(&mut probe_stats.unsupported_formats) {
            *stats.unsupported_formats.entry(format).or_insert(0) += count;
        }
        stats.unreadable_samples.append(&mut probe_stats.unreadable_samples);
        if stats.unreadable_samples.len() > index::MAX_REPORTED_FAILURES {
            stats.unreadable_samples.truncate(index::MAX_REPORTED_FAILURES);
        }
        probed_count = candidate_total.saturating_sub(reused_count);
    }

    if cancelled {
        log::info!("扫描被取消，索引保持原样（未写盘）");
        let meta = IndexFile::new(root);
        return Ok(ScanOutcome {
            meta,
            entries: Vec::new(),
            cancelled: true,
        });
    }

    // 稳定排序：路径升序。界面上顺序播放的次序因此是确定的，不随并行度变化。
    entries.sort_by(|a, b| normalize(&a.p).cmp(&normalize(&b.p)));

    // 统计与条目必须一致：统计是给人看的，不一致比不精确更糟。
    //
    // 这里**不能**简单地令 unsupported = 数一遍 ok==0：`ok == 0` 同时包含
    // "没有解码器"（HEIC/TIFF）与"文件损坏"两种情况，那样会把坏文件算进
    // "不支持的格式"里，而且 unsupported_formats 的明细会对不上
    // （实测踩过：unsupported=3 而明细只有 heif+tiff=2）。
    //
    // 判据用条目自己的 `fmt`，它已经记录了原因：
    //   - 没有解码器：`fmt` 是嗅探出的真实格式（heif/tiff/...），不在支持表里
    //   - 文件损坏：`fmt` 是扩展名（png/jpeg/...），本身是支持的格式
    // 因此 `ok == 0 且 fmt 不是受支持格式` 就是"没有解码器"。
    stats.playable = entries.iter().filter(|e| e.ok == 1).count() as u64;
    stats.unsupported = entries
        .iter()
        .filter(|e| e.ok == 0 && !probe::SUPPORTED_EXTENSIONS.contains(&e.fmt.as_str()))
        .count() as u64;
    stats.unreadable = entries
        .iter()
        .filter(|e| e.ok == 0 && probe::SUPPORTED_EXTENSIONS.contains(&e.fmt.as_str()))
        .count() as u64;
    stats.files_ignored += stats.excluded;

    // 被整棵剪掉的目录也计入"被排除"。注意这**打破**了"看到总数 = 各类之和"这条等式，
    // 因为剪枝进不去目录，里面到底有几个文件是**不知道**的——这正是剪枝的意义所在。
    // 因此这里如实只加目录数，不去伪造文件数；等式只对"真正看到的文件"成立。
    stats.excluded += pruned_dirs.load(Ordering::Relaxed);

    // 旧索引里有、这次没出现的条目数（文件被删、被排除规则挡下，或所在目录被剪掉）。
    //
    // 注意是**减掉三类**：复用 + 更新 + 重新探测过但没变。只减"复用"会把
    // 改过的图与全量重扫过的图都算成"已消失"，而它们明明就在这次的新索引里。
    let removed = (old_count as u64)
        .saturating_sub(reused_count)
        .saturating_sub(updated_count)
        .saturating_sub(probed_unchanged_count);

    let probe_millis = probe_started.elapsed().as_millis() as u64;
    let write_started = std::time::Instant::now();

    let mut meta = IndexFile::new(root);
    meta.scanned_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    meta.scan_millis = started.elapsed().as_millis() as u64;
    meta.reused_entries = reused_count;
    meta.probed_entries = if incremental { probed_count } else { candidate_total };
    meta.stats = stats;
    meta.removed = removed;
    meta.added = added_count;
    meta.updated = updated_count;

    index::save(&paths, &meta, &entries)?;

    // 分阶段耗时。这是优化的**唯一**依据：三个阶段的优化手段完全不同，
    // 不分阶段就只能是猜（而猜错方向的优化是纯浪费）。
    log::info!(
        "扫描阶段耗时：遍历 {} ms、探测 {} ms（{} 个候选、{} 线程）、写索引 {} ms",
        walk_millis,
        probe_millis,
        candidate_total,
        options.threads.max(1),
        write_started.elapsed().as_millis()
    );

    log::info!(
        "扫描完成：{}（{:.1} 秒，候选 {}，可播放 {}，不支持 {}，读不了 {}，\
         新增 {}、更新 {}、删除 {}，复用 {}，探测 {}）",
        root.display(),
        meta.scan_millis as f64 / 1000.0,
        candidate_total,
        meta.stats.playable,
        meta.stats.unsupported,
        meta.stats.unreadable,
        meta.added,
        meta.updated,
        meta.removed,
        reused_count,
        meta.probed_entries,
    );

    Ok(ScanOutcome {
        meta,
        entries,
        cancelled: false,
    })
}

struct CandidateFile {
    path: PathBuf,
    size: u64,
    mtime: i64,
}

/// 并行探测阶段的单个结果。
enum ProbeOutcome {
    /// 复用了已有记录（没有打开文件）。
    ///
    /// 第二个字段：这份记录是**从别的图库沿用的**（而不是本图库自己的旧索引）。
    /// 界面上要能说清"快是因为沿用了上级图库的数据"。
    Reused(LibraryEntry, bool),
    /// 重新探测过，附带这次探测自己的统计。
    Probed((LibraryEntry, IndexStats)),
}

/// 探测单个文件并落到索引条目，同时返回这次探测的独立统计。
///
/// 返回独立统计而不是就地改一个共享结构：并行阶段需要它，且合并动作留在主线程，
/// 汇总逻辑只有一处，不会出现"某个分支忘了累加"的错误。
fn probe_one(candidate: &CandidateFile) -> (LibraryEntry, IndexStats) {
    let path_text = candidate.path.display().to_string();
    let mut stats = IndexStats::default();

    // 视频走另一条探测路径：尺寸从**容器头**读，不用 Media Foundation
    // （见 video_size 模块的说明——扫 5 万个文件时建立解码器代价太高）。
    if crate::video_size::is_video_extension(&probe::extension_of(&candidate.path)) {
        let size = crate::video_size::read_size(&candidate.path);
        stats.playable = 1;
        stats.videos = 1;
        let entry = LibraryEntry {
            p: path_text,
            w: size.width,
            h: size.height,
            mt: candidate.mtime,
            sz: candidate.size,
            or: 1,
            fmt: probe::extension_of(&candidate.path),
            ok: 1,
            kind: index::MediaKind::Video,
        };
        return (entry, stats);
    }

    let entry = match probe::probe_file(&candidate.path) {
        Probe::Sized {
            width,
            height,
            orientation,
        } => {
            stats.playable = 1;
            LibraryEntry {
                p: path_text,
                w: width,
                h: height,
                mt: candidate.mtime,
                sz: candidate.size,
                or: orientation,
                fmt: probe::extension_of(&candidate.path),
                ok: 1,
                kind: index::MediaKind::Image,
            }
        }
        Probe::Unsupported { format } => {
            stats.record_unsupported(&format);
            LibraryEntry {
                p: path_text,
                w: 0,
                h: 0,
                mt: candidate.mtime,
                sz: candidate.size,
                or: 1,
                fmt: format,
                ok: 0,
                kind: index::MediaKind::Image,
            }
        }
        Probe::Unreadable { reason } => {
            stats.record_unreadable(&path_text, &reason);
            LibraryEntry {
                p: path_text,
                w: 0,
                h: 0,
                mt: candidate.mtime,
                sz: candidate.size,
                or: 1,
                fmt: probe::extension_of(&candidate.path),
                ok: 0,
                kind: index::MediaKind::Image,
            }
        }
    };

    (entry, stats)
}

/// 只做排除规则的即时测试，不扫描。供"这条路径为什么没出现"的查询使用。
pub fn test_exclusion(path: &Path, exclusions: &ExcludeSettings) -> Option<super::exclude::ExcludedBy> {
    super::exclude::explain(exclusions, path)
}

/// 供统计展示：把失败样本排序后返回。
pub fn sorted_unreadable(stats: &IndexStats) -> Vec<UnreadableSample> {
    let mut samples = stats.unreadable_samples.clone();
    samples.sort_by(|a, b| a.path.cmp(&b.path));
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-scan-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写一个最小但合法的 PNG（1x1），确保探测能读出尺寸。
    fn write_png(path: &Path, width: u32, height: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut data: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        // IHDR
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]);
        let ihdr_crc = crc32(&data[12..]);
        data.extend_from_slice(&ihdr_crc.to_be_bytes());
        // IDAT：一个合法的 zlib 流，内容为 width*height 个 RGBA 像素（全 0）
        let raw_len = (width as usize) * (height as usize) * 4 + height as usize;
        let raw = vec![0u8; raw_len];
        let compressed = zlib_stored(&raw);
        data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        data.extend_from_slice(b"IDAT");
        data.extend_from_slice(&compressed);
        let idat_crc = crc32(&data[data.len() - compressed.len() - 4..]);
        data.extend_from_slice(&idat_crc.to_be_bytes());
        // IEND
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"IEND");
        let iend_crc = crc32(b"IEND");
        data.extend_from_slice(&iend_crc.to_be_bytes());
        fs::write(path, data).unwrap();
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc = table[((crc ^ *byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFF_FFFF
    }

    /// zlib "stored" 块：不做压缩，但格式合法。用于生成可被解码的测试 PNG。
    fn zlib_stored(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let mut offset = 0;
        while offset < raw.len() {
            let chunk = (raw.len() - offset).min(0xFFFF);
            let last = offset + chunk >= raw.len();
            out.push(if last { 1 } else { 0 });
            out.extend_from_slice(&(chunk as u16).to_le_bytes());
            out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
            out.extend_from_slice(&raw[offset..offset + chunk]);
            offset += chunk;
        }
        // adler32
        let (mut a, mut b) = (1u32, 0u32);
        for byte in raw {
            a = (a + *byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    #[test]
    fn scan_finds_images_and_ignores_non_images() {
        let root = temp_root("basic");
        let index_root = temp_root("basic-index");
        write_png(&root.join("a.png"), 1920, 1080);
        write_png(&root.join("sub").join("b.png"), 800, 600);
        fs::write(root.join("notes.txt"), "不是图片").unwrap();
        // 这个 mp4 的内容是假的（32 个零字节），因此尺寸解析会走兜底值——
        // 但它**必须**作为视频条目进索引，这是"视频壁纸接入图库"的关键。
        fs::write(root.join("clip.mp4"), [0u8; 32]).unwrap();

        let settings = Settings::default();
        let outcome = scan_library(
            &root,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();

        assert!(!outcome.cancelled);
        assert_eq!(outcome.entries.len(), 3, "两张图 + 一个视频");
        assert_eq!(outcome.meta.stats.playable, 3, "视频也算可播放条目");
        assert_eq!(outcome.meta.stats.videos, 1, "其中一个是视频");
        assert_eq!(outcome.meta.stats.files_seen, 4, "看到了 4 个文件");
        assert_eq!(
            outcome.meta.stats.files_ignored, 1,
            "只有 .txt 是非媒体文件；视频不再是'被忽略'"
        );
        assert_eq!(outcome.meta.stats.unreadable, 0);

        // 尺寸应正确读出。
        let first = outcome.entries.iter().find(|e| e.p.ends_with("a.png")).unwrap();
        assert_eq!((first.w, first.h), (1920, 1080));

        // 视频条目要有正确的类型标记与兜底尺寸（内容损坏时不能是 0x0）。
        let video = outcome
            .entries
            .iter()
            .find(|e| e.p.ends_with("clip.mp4"))
            .expect("视频必须进索引");
        assert!(video.is_video(), "mp4 条目应当被标记为视频");
        assert!(video.is_playable(), "视频应当是可播放条目");
        assert!(
            video.w > 0 && video.h > 0,
            "损坏的视频也要给出兜底尺寸，否则会被筛选条件全部排除（得到 {}x{}）",
            video.w,
            video.h
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn unreadable_and_unsupported_are_reported_not_dropped() {
        let root = temp_root("report");
        let index_root = temp_root("report-index");
        write_png(&root.join("good.png"), 100, 100);
        // 扩展名支持、内容损坏 -> 读不了
        fs::write(root.join("broken.png"), b"not a png at all").unwrap();
        // HEIC 头：认得出格式，但没有解码器 -> 不支持
        let mut heic = vec![0u8; 64];
        heic[3] = 0x18;
        heic[4..8].copy_from_slice(b"ftyp");
        heic[8..12].copy_from_slice(b"heic");
        fs::write(root.join("photo.heic"), heic).unwrap();
        // 另一种"认得出格式但没解码器"：TIFF 头
        let tiff = vec![0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
        fs::write(root.join("scan.tiff"), tiff).unwrap();

        let outcome = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();

        assert_eq!(outcome.meta.stats.playable, 1, "只有 good.png 可播放");
        assert_eq!(outcome.meta.stats.unreadable, 1, "损坏文件要被计数");
        assert_eq!(
            outcome.meta.stats.unsupported, 2,
            "HEIC 与 TIFF 都要被算作「没有解码器」"
        );
        // 格式分布要能说明"少了的是些什么"
        let formats = &outcome.meta.stats.unsupported_formats;
        assert!(
            formats.keys().any(|k| k.contains("heif") || k.contains("heic")),
            "格式统计里应有 heif/heic，实际：{formats:?}"
        );
        assert!(
            formats.contains_key("tiff"),
            "格式统计里应有 tiff，实际：{formats:?}"
        );
        assert!(
            !outcome.meta.stats.unreadable_samples.is_empty(),
            "失败样本要能列出路径与原因"
        );
        // 统计与明细必须自洽：这条不变式正是上面那个 bug 的探测器。
        // 当时 unsupported 数成了 3（把坏文件也算了进去），而明细只有 heif+tiff=2。
        let detail_total: u64 = outcome.meta.stats.unsupported_formats.values().sum();
        assert_eq!(
            detail_total, outcome.meta.stats.unsupported,
            "unsupported 计数必须等于 unsupported_formats 的明细合计，明细：{:?}",
            outcome.meta.stats.unsupported_formats
        );
        // 三类条目都要留在索引里，界面才能回答"少的那张去哪了"。
        assert_eq!(outcome.entries.len(), 4);
        assert!(outcome
            .entries
            .iter()
            .any(|e| e.p.ends_with("photo.heic") && e.ok == 0));
        assert!(outcome
            .entries
            .iter()
            .any(|e| e.p.ends_with("broken.png") && e.ok == 0));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn second_scan_reuses_unchanged_entries() {
        let root = temp_root("incremental");
        let index_root = temp_root("incremental-index");
        for i in 0..12 {
            write_png(&root.join(format!("img{i:02}.png")), 100 + i, 100);
        }

        let first = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(first.meta.reused_entries, 0, "首次扫描没有可复用的记录");
        assert_eq!(first.meta.probed_entries, 12);

        let second = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(second.meta.reused_entries, 12, "未变化的文件应全部复用");
        assert_eq!(second.meta.probed_entries, 0, "复用意味着一次文件都没打开");
        assert_eq!(second.entries, first.entries, "复用出来的条目应与首次一致");
        // 什么都没动过的一次增量重扫，三个变化数必须都是 0。
        // （"重新扫描全部图库"这个按钮最常得到的就是这个结果，而它报出
        //  任何非零的"新增/删除"都会让用户去磁盘上找并不存在的问题。）
        assert_eq!(second.meta.added, 0, "没有新文件");
        assert_eq!(second.meta.updated, 0, "没有改过的文件");
        assert_eq!(second.meta.removed, 0, "没有删掉的文件");

        // 增量扫描必须更快，这是它存在的理由。
        //
        // 但**不能按固定毫秒数断言**：这个库只有 12 个小文件，一次扫描可能
        // 只要 1 ms，此时任何固定余量都既可能过松也可能过紧。而且批内进度轮询
        // 会在关键路径上加一次最多 `PROGRESS_POLL` 的睡眠，两次扫描都受影响。
        //
        // 更本质的一点：这条断言想问的是"复用了旧记录吗"，而那个问题
        // 上面的 `reused_entries` / `probed_entries` 已经**精确**回答了。
        // 耗时只是它的代理指标——代理指标更慢并不说明复用没发生。
        //
        // 因此这里只挡住"二次扫描慢得离谱"（超过 10 倍且绝对差超过 100 ms）：
        // 那才说明真的退化了，而不是抖动或轮询开销。
        let first_ms = first.meta.scan_millis.max(1);
        let second_ms = second.meta.scan_millis;
        assert!(
            second_ms <= first_ms * 10 + 100,
            "增量扫描不应慢这么多（首次 {first_ms}ms，二次 {second_ms}ms）"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn incremental_scan_reports_added_updated_and_removed() {
        // 这条测试钉住"一键重新扫描全部图库"最核心的那句承诺：
        // **只说真的变了什么**——新增几张、改了几张、删了几张，未动的原样复用。
        let root = temp_root("changes");
        let index_root = temp_root("changes-index");
        let settings = Settings::default();
        let keep = root.join("keep.png");
        let edited = root.join("edited.png");
        let doomed = root.join("doomed.png");
        write_png(&keep, 100, 100);
        write_png(&edited, 100, 100);
        write_png(&doomed, 100, 100);

        let first = scan_library(&root, &index_root, &settings, &ScanOptions::default(), &[], |_| true)
            .unwrap();
        assert_eq!(first.meta.added, 3, "首次扫描：三条都是新增");
        assert_eq!(first.meta.updated, 0);
        assert_eq!(first.meta.removed, 0);

        // 磁盘上做三件事：加一张、改一张、删一张。
        write_png(&root.join("brand-new.png"), 200, 200);
        // 改的那张**尺寸也要变**：mtime 只精确到秒，同一秒内重写同样大小的文件
        // 在判据上就是"没变过"（那是判据的设计，不是这条测试要考的东西）。
        write_png(&edited, 640, 480);
        fs::remove_file(&doomed).unwrap();

        let second = scan_library(&root, &index_root, &settings, &ScanOptions::default(), &[], |_| true)
            .unwrap();
        assert_eq!(second.meta.added, 1, "只加了一张");
        assert_eq!(second.meta.updated, 1, "只改了一张");
        assert_eq!(second.meta.removed, 1, "只删了一张");
        assert_eq!(second.meta.reused_entries, 1, "没动过的那张必须复用");
        assert_eq!(second.entries.len(), 3, "新索引 = 复用的 + 新增的 + 改过的");
        assert!(
            !second.entries.iter().any(|e| e.p.ends_with("doomed.png")),
            "删掉的文件不能留在索引里"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn a_forced_full_rescan_does_not_reuse_but_still_reports_changes_honestly() {        // **回归护栏**：台账（`old_by_path`）现在是无论增量与否都建的，
        // 而复用判断必须仍然只看 `incremental`。少写那个条件，"全量重扫"
        // 会静默退化成增量（文件一个都不会被重新打开），而界面照常显示"完成"。
        //
        // 同时钉住全量重扫下的台账口径：文件内容没变时，它既不是"新增"
        // 也不是"更新"，更不能被算成"已删除"。
        let root = temp_root("full-rescan");
        let index_root = temp_root("full-rescan-index");
        let settings = Settings::default();
        write_png(&root.join("a.png"), 100, 100);
        write_png(&root.join("b.png"), 120, 100);

        scan_library(&root, &index_root, &settings, &ScanOptions::default(), &[], |_| true).unwrap();

        let forced = ScanOptions {
            incremental: false,
            ..Default::default()
        };
        let second = scan_library(&root, &index_root, &settings, &forced, &[], |_| true).unwrap();
        assert_eq!(second.meta.reused_entries, 0, "强制全量时一个都不许复用");
        assert_eq!(second.meta.probed_entries, 2, "两条都要重新打开");
        assert_eq!(second.meta.added, 0, "内容没变，不是新增");
        assert_eq!(second.meta.updated, 0, "内容没变，不是更新");
        assert_eq!(second.meta.removed, 0, "重新探测过绝不等于已消失");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn a_new_library_under_an_existing_one_reuses_the_ancestor_entries() {
        // 用户提的场景：`F:\素材` 已经是图库了，后来又把 `F:\素材\2024` 单加成图库。
        // 后者一个文件都不该被重新打开——尺寸与格式都能从父图库的索引里拿到。
        let parent = temp_root("inherit-parent");
        let child = parent.join("2024");
        let index_root = temp_root("inherit-index");
        let settings = Settings::default();

        write_png(&parent.join("outside.png"), 100, 100);
        write_png(&child.join("a.png"), 120, 100);
        write_png(&child.join("b.png"), 100, 120);

        // 先扫父图库，拿到它那份索引（这就是"已有的数据"）。
        let parent_outcome = scan_library(
            &parent,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(parent_outcome.entries.len(), 3);
        let inherited = parent_outcome.entries.clone();

        // 再扫子图库：把父图库的记录作为 `inherited` 传进去。
        let mut reused_reported = 0u64;
        let mut inherited_reported = 0u64;
        let child_outcome = scan_library(
            &child,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &inherited,
            |progress| {
                reused_reported = progress.reused;
                inherited_reported = progress.reused_inherited;
                true
            },
        )
        .unwrap();

        assert_eq!(child_outcome.entries.len(), 2, "只有子目录里那两张");
        assert_eq!(
            child_outcome.meta.reused_entries, 2,
            "两张都该沿用，一个文件都不打开"
        );
        assert_eq!(child_outcome.meta.probed_entries, 0, "没有任何一条需要重新探测");
        assert_eq!(reused_reported, 2, "进度里也要报出复用数");
        assert_eq!(inherited_reported, 2, "并且要说清是从**别的图库**沿用的");
        // 沿用的记录必须带着原来探测到的尺寸，而不是 0。
        let a = child_outcome
            .entries
            .iter()
            .find(|e| e.p.ends_with("a.png"))
            .unwrap();
        assert_eq!((a.w, a.h), (120, 100), "沿用的尺寸要正确");
        assert_eq!(a.fmt, "png", "沿用的格式也要正确");

        // 台账口径：沿用的东西**不是新增**（它本来就在别处的索引里，
        // 但对**这个**图库而言确实是第一次出现）。这里钉住实际行为，
        // 免得以后有人"顺手"改掉却没人发现界面上数字变了。
        assert_eq!(child_outcome.meta.added, 2, "对这个图库而言这两条是新的");

        let _ = fs::remove_dir_all(&parent);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn ancestor_records_are_ignored_when_the_file_changed_on_disk() {
        // 沿用必须仍然以"磁盘上那份没变"为前提：否则界面上的尺寸会是旧的。
        let parent = temp_root("inherit-stale-parent");
        let child = parent.join("sub");
        let index_root = temp_root("inherit-stale-index");
        let settings = Settings::default();

        write_png(&child.join("a.png"), 120, 100);
        let parent_outcome = scan_library(
            &parent,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();

        // 把那张图改掉（尺寸与修改时间都变）。
        write_png(&child.join("a.png"), 200, 50);
        // 再加一张父索引里没有的。
        write_png(&child.join("new.png"), 60, 60);

        let child_outcome = scan_library(
            &child,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &parent_outcome.entries,
            |_| true,
        )
        .unwrap();

        assert_eq!(
            child_outcome.meta.reused_entries, 0,
            "变过的那张不能沿用；新增的那张也没得沿用"
        );
        assert_eq!(child_outcome.meta.probed_entries, 2, "两条都要重新探测");
        let a = child_outcome
            .entries
            .iter()
            .find(|e| e.p.ends_with("a.png"))
            .unwrap();
        assert_eq!((a.w, a.h), (200, 50), "必须反映磁盘上的新尺寸");

        let _ = fs::remove_dir_all(&parent);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn a_forced_full_rescan_ignores_ancestor_records_too() {
        // 与 `incremental` 的关系：强制全量重扫时，连祖先的记录也不许用。
        // 少这个判断，"全量重扫"会变成一个谁都没注意到的谎话。
        let parent = temp_root("inherit-full-parent");
        let child = parent.join("sub");
        let index_root = temp_root("inherit-full-index");
        let settings = Settings::default();
        write_png(&child.join("a.png"), 120, 100);

        let parent_outcome = scan_library(
            &parent,
            &index_root,
            &settings,
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();

        let forced = ScanOptions {
            incremental: false,
            ..Default::default()
        };
        let child_outcome = scan_library(
            &child,
            &index_root,
            &settings,
            &forced,
            &parent_outcome.entries,
            |_| true,
        )
        .unwrap();

        assert_eq!(child_outcome.meta.reused_entries, 0);
        assert_eq!(child_outcome.meta.probed_entries, 1, "强制全量必须真的打开它");

        let _ = fs::remove_dir_all(&parent);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn changed_file_is_reprobed() {
        let root = temp_root("changed");
        let index_root = temp_root("changed-index");
        let file = root.join("a.png");
        write_png(&file, 100, 100);

        scan_library(&root, &index_root, &Settings::default(), &ScanOptions::default(), &[], |_| true)
            .unwrap();

        // 换掉内容：尺寸变了，必须重新探测。
        write_png(&file, 640, 480);
        let second = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions::default(),
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(second.meta.reused_entries, 0, "内容变了不能复用");
        assert_eq!(second.entries[0].w, 640);
        assert_eq!(second.entries[0].h, 480);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn exclusion_rules_take_effect_and_are_counted() {
        let root = temp_root("excluded");
        let index_root = temp_root("excluded-index");
        write_png(&root.join("keep.png"), 10, 10);
        write_png(&root.join("screenshots").join("s1.png"), 10, 10);
        write_png(&root.join("screenshots").join("deep").join("s2.png"), 10, 10);
        write_png(&root.join("other").join("s3.png"), 10, 10);

        let mut settings = Settings::default();
        settings.exclude.dirs = vec![root.join("screenshots").display().to_string()];

        let outcome = scan_library(&root, &index_root, &settings, &ScanOptions::default(), &[], |_| true)
            .unwrap();

        assert_eq!(outcome.entries.len(), 2, "只剩 keep.png 与 other/s3.png");
        // 剪枝进不去目录，因此目录里到底有几个文件是**不知道**的：
        // 排除计数里"整个被剪掉的目录"只记 1 条，而不是它里面的文件数。
        assert_eq!(
            outcome.meta.stats.excluded, 1,
            "一条目录规则把一个目录整棵剪掉，记 1 条，实际：{:?}",
            outcome.meta.stats
        );
        // 真正的不变式：看到的文件数 = 可播放 + 不支持 + 读不了 + 被排除（文件部分）
        // + 扩展名不符。任何文件都不能被静默丢掉。
        //
        // 被剪掉的目录不在这个等式里（其内容数未知），因此把目录那一条减掉。
        let excluded_files = outcome.meta.stats.excluded - 1;
        assert_eq!(
            outcome.meta.stats.files_seen,
            outcome.meta.stats.playable
                + outcome.meta.stats.unsupported
                + outcome.meta.stats.unreadable
                + excluded_files
                + outcome.meta.stats.files_ignored,
            "每一个真正看到的文件都必须归入某一类。统计：{:?}",
            outcome.meta.stats
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn cancellation_leaves_the_previous_index_intact() {
        let root = temp_root("cancel");
        let index_root = temp_root("cancel-index");
        for i in 0..30 {
            write_png(&root.join(format!("img{i:02}.png")), 50, 50);
        }

        // 先建立一份完整索引。
        let first = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions {
                progress_every: 1,
                ..Default::default()
            },
            &[],
            |_| true,
        )
        .unwrap();
        assert_eq!(first.entries.len(), 30);

        // 再扫一次，调用方在第一个进度回调就要求取消。
        let cancelled = scan_library(
            &root,
            &index_root,
            &Settings::default(),
            &ScanOptions {
                progress_every: 1,
                incremental: false,
                ..Default::default()
            },
            &[],
            |_| false,
        )
        .unwrap();
        assert!(cancelled.cancelled);

        // 磁盘上的索引必须还是上一次的完整内容。
        let paths = IndexPaths::new(&index_root, &index::library_id(&root));
        let (meta, entries) = index::load(&paths).unwrap();
        assert_eq!(entries.len(), 30, "取消后索引不应被写成半截");
        assert_eq!(meta.stats.playable, 30);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }

    #[test]
    fn empty_file_is_unreadable() {
        let root = temp_root("empty");
        let index_root = temp_root("empty-index");
        fs::write(root.join("empty.png"), []).unwrap();

        let outcome = scan_library(&root, &index_root, &Settings::default(), &ScanOptions::default(), &[], |_| true)
            .unwrap();
        assert_eq!(outcome.meta.stats.unreadable, 1);
        assert_eq!(outcome.meta.stats.playable, 0);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&index_root);
    }
}

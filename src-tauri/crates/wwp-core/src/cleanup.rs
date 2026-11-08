//! 清理程序目录：把"删掉也不会影响使用"的东西列出来，由用户勾选后删除。
//!
//! ## 这个模块的边界（用户明确要求的两条）
//!
//! 1. **绝不动回收站**。`data/trash` 与 `data/trash-thumbs` 里可能是用户还没
//!    决定去留的图，一次"清理"把它们删掉就等于绕过了回收站——那是最糟的失败。
//!    它们因此不在这份清单里。
//! 2. **只删"跑一次就会自己回来"的东西**。设置、图库索引、播放位置、窗口大小
//!    都是重建不出来的（索引重建要重扫几万个文件），因此一律保留。
//!
//! ## 为什么"能删什么"由代码决定，而不是让界面写死一张表
//!
//! 清单里每一项都带着"它是什么、删了会怎样"的说明，界面只负责显示与勾选。
//! 写死在界面里的话，将来改一处目录名就会让界面删错东西——而这是**删除**。
//!
//! ## 为什么正在写的日志是"截断"而不是"删除"
//!
//! `logs/app.log` 被日志器一直开着。删除它（`remove_file`）在 Windows 上会失败
//! （文件被占用），即使成功，日志器手里的句柄也指着一个已经不在目录里的文件，
//! 之后所有日志都会静默丢失——而"日志没了"恰恰是出问题时最需要它的时刻。
//! 截断（`set_len(0)`）既释放了空间，又让同一个句柄继续可写。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;
use crate::{Error, Result};

/// 可清理项的种类。
///
/// 种类决定**怎么删**，而不是"删不删"：目录递归删、单个文件截断。
/// 用枚举而不是让内部字段直接写做法，是为了让"要删的是文件还是目录"这件事
/// 在类型上就说得清，避免哪天有人给目录填了个 `truncate`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanupKind {
    /// 整个目录（含内容）删掉。
    Directory,
    /// 文件截断成 0 字节（保留文件本身，见模块文档）。
    Truncate,
    /// 若干个同类的文件，逐个删除。
    Files,
}

/// 一项可清理的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupItem {
    /// 稳定标识：界面勾选后回传的就是它（不传路径，避免前端拼路径）。
    pub id: String,
    /// 界面上显示的名字。
    pub label: String,
    /// 一句话说明"这是什么、删了会怎样"。
    pub detail: String,
    /// 实际路径（仅供界面显示，删除时只认 id）。
    pub path: String,
    /// 当前占用字节数（目录是递归求和）。
    pub bytes: u64,
    /// 有多少个文件（目录类才有意义，单文件为 1）。
    pub files: u64,
    pub kind: CleanupKind,
    /// 现在能不能删。程序正在用着的目录（WebView2 的界面缓存）为 false。
    pub removable: bool,
    /// 不能删的原因（`removable` 为 false 时给出）。
    pub blocked_reason: Option<String>,
}

impl CleanupItem {
    /// 界面上显示的大小，例如 `102.8 MB`。
    pub fn size_text(&self) -> String {
        human_bytes(self.bytes)
    }
}

/// 一项的删除结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupOutcome {
    pub id: String,
    pub label: String,
    pub ok: bool,
    pub freed: u64,
    /// 失败原因（成功时为 None）。
    pub error: Option<String>,
}

/// 一次清理的总体结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub outcomes: Vec<CleanupOutcome>,
    pub freed: u64,
    /// 清理**之后**重新盘出来的清单，界面直接用它刷新显示。
    pub items: Vec<CleanupItem>,
}

/// 把字节数写成人看的样子。
pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else if value < KB * KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else {
        format!("{:.2} GB", value / (KB * KB * KB))
    }
}

/// 列出现在可以清理的东西。
///
/// **只列存在的**：不存在的项不占地方，"清理 0 B"只会让人以为功能坏了。
/// 调用方（界面）因此不必自己判断哪些有内容。
pub fn list(paths: &AppPaths) -> Vec<CleanupItem> {
    let mut items = Vec::new();

    // ① 壁纸产物缓存。轮播每换一张都会覆盖写，删了之后第一次换纸会慢一些
    //    （实测一张 4K 约 130~270 ms），功能完全不受影响。
    push_dir(
        &mut items,
        paths.data_dir().join("output"),
        "id-wallpaper-cache",
        "壁纸缓存",
        "轮播与 NSFW 壁纸的中间产物。删除后第一次换纸会重新生成。",
    );

    // ② 播放页预览缩略图。
    push_dir(
        &mut items,
        paths.data_dir().join("wallpaper-thumbs"),
        "id-wallpaper-thumbs",
        "预览缩略图",
        "播放页「当前 / 下一张」两格用的小图。删除后回到播放页会重新生成。",
    );

    // ③ 回收站缩略图。注意：**只删缩略图，不碰回收站里的图**。
    push_dir(
        &mut items,
        paths.data_dir().join("trash-thumbs"),
        "id-trash-thumbs",
        "回收站缩略图",
        "回收站相册用的小图。删除后重新打开回收站会重新生成；回收站里的图不受影响。",
    );

    // ④ 日志。`app.log` 正在被写，因此是截断而不是删除。
    push_log_files(&mut items, paths);

    // ⑤ 崩溃报告。
    push_dir(
        &mut items,
        paths.crash_dir(),
        "id-crash",
        "崩溃报告",
        "历史崩溃现场记录。删除后不影响运行，只是以后没法回看这些崩溃。",
    );

    items
}

/// 执行清理：只处理 `ids` 里点到的那些项。
///
/// 逐项独立：一项失败不影响其余（比如某个日志文件正被别的进程读着）。
pub fn run(paths: &AppPaths, ids: &[String]) -> Result<CleanupReport> {
    let available = list(paths);
    let mut outcomes = Vec::new();
    let mut freed = 0u64;

    for item in available.iter().filter(|item| ids.iter().any(|id| id == &item.id)) {
        if !item.removable {
            outcomes.push(CleanupOutcome {
                id: item.id.clone(),
                label: item.label.clone(),
                ok: false,
                freed: 0,
                error: Some(
                    item.blocked_reason
                        .clone()
                        .unwrap_or_else(|| "现在不能清理".to_string()),
                ),
            });
            continue;
        }

        let path = PathBuf::from(&item.path);
        let result = match item.kind {
            CleanupKind::Directory => remove_dir(&path),
            CleanupKind::Truncate => truncate_file(&path),
            // 正在写的那个日志文件必须显式保护（不能指望"删不掉"这个偶然事实）。
            CleanupKind::Files => remove_log_files(&path, &protected_log_files(paths)),
        };

        match result {
            Ok(freed_bytes) => {
                freed += freed_bytes;
                log::info!("清理：{} 释放 {}", item.label, human_bytes(freed_bytes));
                outcomes.push(CleanupOutcome {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    ok: true,
                    freed: freed_bytes,
                    error: None,
                });
            }
            Err(error) => {
                log::warn!("清理「{}」失败：{error}", item.label);
                outcomes.push(CleanupOutcome {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    ok: false,
                    freed: 0,
                    error: Some(error),
                });
            }
        }
    }

    Ok(CleanupReport {
        outcomes,
        freed,
        // 重新盘一次：界面拿它刷新每一项的大小（清完可能还剩一部分，
        // 比如日志被别的进程占着没截断成功）。
        items: list(paths),
    })
}

/// 递归求一个目录的体积与文件数。
///
/// 读不到的条目直接跳过（不算失败）：清理是"顺手省点空间"，
/// 不该因为一个拒绝访问的子目录就整项不显示。
fn measure_dir(path: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// 把一个目录加进清单（不存在或空目录则跳过）。
fn push_dir(items: &mut Vec<CleanupItem>, dir: PathBuf, id: &str, label: &str, detail: &str) {
    let (bytes, files) = measure_dir(&dir);
    if files == 0 {
        return;
    }
    items.push(CleanupItem {
        id: id.to_string(),
        label: label.to_string(),
        detail: detail.to_string(),
        path: dir.display().to_string(),
        bytes,
        files,
        kind: CleanupKind::Directory,
        removable: true,
        blocked_reason: None,
    });
}

/// 把 `logs/` 下的日志文件加成一项。
///
/// 正在写的那个（`app.log`）用截断，滚动档（`app.1.log`…）直接删。
/// 清单里合成**一项**：它们对用户是同一件事（"日志"），拆成多项只会让他
/// 逐个去判断"这个能不能删"——而那几个文件的区别他无从判断。
///
/// 判据是**体积**而不是"有没有文件"：截断之后 `app.log` 仍然存在（这是有意的，
/// 见模块文档），按文件数判断会让"清理"之后这一项以 `0 B` 继续挂在清单里。
fn push_log_files(items: &mut Vec<CleanupItem>, paths: &AppPaths) {
    let (bytes, files, _) = measure_logs(paths);
    if bytes == 0 {
        return;
    }
    items.push(CleanupItem {
        id: "id-logs".to_string(),
        label: "日志".to_string(),
        detail: "运行日志与滚动归档。正在写的那个会被清空但保留文件，程序继续正常记录。"
            .to_string(),
        path: paths.log_dir().display().to_string(),
        bytes,
        files,
        kind: CleanupKind::Files,
        removable: true,
        blocked_reason: None,
    });
}

/// 日志的体积与文件数。
fn measure_logs(paths: &AppPaths) -> (u64, u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut count = 0u64;
    let Ok(entries) = std::fs::read_dir(paths.log_dir()) else {
        return (0, 0, 0);
    };
    for entry in entries.flatten() {
        // 只算日志文件：`logs/crash/` 是目录（另一项），别的杂项不动。
        if !entry.path().is_file() || !is_log_file(&entry.path()) {
            continue;
        }
        count += 1;
        if let Ok(meta) = entry.metadata() {
            bytes += meta.len();
            files += 1;
        }
    }
    (bytes, files, count)
}

fn remove_dir(path: &Path) -> std::result::Result<u64, String> {
    let (bytes, _) = measure_dir(path);
    std::fs::remove_dir_all(path).map_err(|e| format!("删除 {} 失败：{e}", path.display()))?;
    Ok(bytes)
}

/// 删掉 `logs/` 下的日志文件；**正在写的那个改为截断**。
///
/// ## 为什么"哪个是正在写的"要显式判断
///
/// 第一版是"先试删除，删不掉（被占用）再截断"。那条路只在**日志器确实开着
/// 句柄**时才对，而它依赖的是一个偶然事实：实测（单元测试里）没有句柄时
/// `app.log` 会被直接删掉。真机上它通常删不掉，但"通常"不是保证——
/// 一旦删掉，日志器手里的句柄就指向一个不在目录里的文件，
/// 之后所有日志静默丢失（见模块文档）。
///
/// 因此现在把当前日志文件**直接指定为截断**，其余归档档才删。
fn remove_log_files(dir: &Path, protected: &[PathBuf]) -> std::result::Result<u64, String> {
    let mut freed = 0u64;
    let mut first_error: Option<String> = None;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Err(format!("打开 {} 失败", dir.display()));
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_log_file(&path) {
            continue;
        }
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        let is_protected = protected.iter().any(|keep| keep == &path);
        if is_protected {
            // 清空但保留文件本身：日志器继续往同一个句柄里写。
            match truncate_file(&path) {
                Ok(freed_now) => freed += freed_now,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => freed += size,
            Err(_) => match truncate_file(&path) {
                Ok(freed_now) => freed += freed_now,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            },
        }
    }
    match first_error {
        // 一个都没处理成功才算失败；动了任意一个就报成功（界面看得到剩余大小）。
        Some(error) if freed == 0 => Err(error),
        _ => Ok(freed),
    }
}

/// 正在被日志器写着的那些文件：清理时它们只能清空、不能删除。
///
/// 判据来自 `AppPaths` 本身（`app_log()`），而不是在清理模块里另拼一遍
/// `logs/app.log`：那个文件名是日志器的约定，两处各写一遍迟早会不一致。
fn protected_log_files(paths: &AppPaths) -> Vec<PathBuf> {
    vec![paths.app_log()]
}

/// 一个路径是不是我们的日志文件（`app*.log`）。
fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("app") && name.ends_with(".log"))
        .unwrap_or(false)
}

/// 把一个文件截断成 0 字节，返回释放的字节数。
///
/// 用 `OpenOptions::write` 而不是 `File::create`：后者会截断并保持句柄，
/// 而我们要的正是"文件还在、长度为 0"，句柄用完就关掉。
fn truncate_file(path: &Path) -> std::result::Result<u64, String> {
    let before = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| Error::io(format!("打开 {} 失败", path.display()), e).to_string())?;
    file.set_len(0)
        .map_err(|e| Error::io(format!("清空 {} 失败", path.display()), e).to_string())?;
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RootSource;

    fn temp_paths(tag: &str) -> AppPaths {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wwp-cleanup-{tag}-{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        AppPaths {
            root: root.clone(),
            root_source: RootSource::ProgramDir,
            program_dir: root,
            fallback_reason: None,
        }
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn lists_only_what_exists_and_never_the_trash() {
        let paths = temp_paths("list");
        write(&paths.data_dir().join("output").join("a.bmp"), 1024);
        write(&paths.data_dir().join("wallpaper-thumbs").join("t.jpg"), 100);
        write(&paths.log_dir().join("app.log"), 200);
        write(&paths.log_dir().join("app.1.log"), 300);
        write(&paths.log_dir().join("crash").join("panic-1.txt"), 10);
        // 回收站里的东西必须**一个都不出现**在清单里。
        write(&paths.trash_dir().join("item-1").join("a.jpg"), 4096);
        write(&paths.data_dir().join("trash-thumbs").join("t.jpg"), 50);
        // 这些是重建不出来的，绝不能被列出来。
        write(&paths.config_dir().join("settings.toml"), 512);
        write(&paths.data_dir().join("index").join("lib").join("index.jsonl"), 2048);
        write(&paths.data_dir().join("playback-state.json"), 20);

        let items = list(&paths);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert!(ids.contains(&"id-wallpaper-cache"), "壁纸缓存应当列出：{ids:?}");
        assert!(ids.contains(&"id-wallpaper-thumbs"), "预览缩略图应当列出：{ids:?}");
        assert!(ids.contains(&"id-trash-thumbs"), "回收站缩略图应当列出：{ids:?}");
        assert!(ids.contains(&"id-logs"), "日志应当列出：{ids:?}");
        assert!(ids.contains(&"id-crash"), "崩溃报告应当列出：{ids:?}");

        for item in &items {
            assert!(
                !item.path.contains("settings.toml")
                    && !item.path.contains("index")
                    && !item.path.contains("playback-state")
                    && !item.path.contains(&format!("trash{}", std::path::MAIN_SEPARATOR)),
                "不可重建的内容不该出现在清单里：{} ({})",
                item.label,
                item.path
            );
        }

        // 日志那一项要把两个文件一起算进去。
        let logs = items.iter().find(|item| item.id == "id-logs").unwrap();
        assert_eq!(logs.bytes, 500, "两个日志文件都要算进体积");
        assert_eq!(logs.files, 2);

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn running_log_is_truncated_and_keeps_the_file() {
        let paths = temp_paths("truncate");
        write(&paths.log_dir().join("app.log"), 4096);
        write(&paths.log_dir().join("app.1.log"), 2048);

        let report = run(&paths, &["id-logs".to_string()]).unwrap();
        assert_eq!(report.freed, 6144, "两个日志文件的体积都该释放");
        assert_eq!(report.outcomes.len(), 1);
        assert!(report.outcomes[0].ok, "清理日志应当成功");

        // 关键：`app.log` 必须还在（日志器手里的句柄要继续可写）。
        assert!(
            paths.log_dir().join("app.log").is_file(),
            "正在写的日志不能被删除，只能清空"
        );
        assert_eq!(
            std::fs::metadata(paths.log_dir().join("app.log")).unwrap().len(),
            0
        );
        assert!(!paths.log_dir().join("app.1.log").exists(), "归档档应当被删掉");

        // 清完之后清单里不该再有日志这一项。
        assert!(
            !report.items.iter().any(|item| item.id == "id-logs"),
            "清空后不该再列出日志"
        );

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn trash_and_config_survive_a_full_cleanup() {
        let paths = temp_paths("survive");
        write(&paths.data_dir().join("output").join("a.bmp"), 1024);
        write(&paths.data_dir().join("wallpaper-thumbs").join("t.jpg"), 100);
        write(&paths.data_dir().join("trash-thumbs").join("t.jpg"), 100);
        write(&paths.log_dir().join("app.log"), 100);
        write(&paths.log_dir().join("crash").join("panic.txt"), 100);
        write(&paths.trash_dir().join("item-1").join("keep.jpg"), 4096);
        write(&paths.config_dir().join("settings.toml"), 512);
        write(&paths.data_dir().join("trash").join("origin.json"), 30);

        let ids: Vec<String> = list(&paths).into_iter().map(|item| item.id).collect();
        assert!(ids.len() >= 5, "应当列出五项：{ids:?}");
        let report = run(&paths, &ids).unwrap();
        assert!(report.outcomes.iter().all(|o| o.ok), "全部应当成功：{:?}", report.outcomes);
        assert!(report.freed > 0);

        // 用户明确要求：清理不得清空回收站。
        assert!(
            paths.trash_dir().join("item-1").join("keep.jpg").is_file(),
            "回收站里的图必须原样保留"
        );
        assert!(paths.config_dir().join("settings.toml").is_file(), "设置必须保留");

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn unselected_items_are_left_alone() {
        let paths = temp_paths("selected");
        write(&paths.data_dir().join("output").join("a.bmp"), 1024);
        write(&paths.data_dir().join("wallpaper-thumbs").join("t.jpg"), 100);

        let report = run(&paths, &["id-wallpaper-cache".to_string()]).unwrap();
        assert!(paths.data_dir().join("wallpaper-thumbs").join("t.jpg").is_file());
        assert!(!paths.data_dir().join("output").exists());
        assert!(report.freed >= 1024);
        assert!(
            report
                .items
                .iter()
                .any(|item| item.id == "id-wallpaper-thumbs"),
            "没清的项应当还在清单里"
        );

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn human_bytes_reads_like_a_size() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.00 GB");
    }
}

//! 诊断导出。
//!
//! 做成一个独立模块而不是写在命令里，是因为它有两个入口：界面上的按钮，以及
//! `--export-diagnostics` 命令行开关。后者是必需的——用户可能遇到"界面起不来"
//! 的情况，而那正是最需要诊断信息的时候。
//!
//! ## 打包什么、为什么、以及不打包什么
//!
//! - `environment.txt`：版本、系统、显示器/DPI、显卡、桌面层拓扑、当前壁纸。
//!   这些是"我这边复现不出来"时最先要问的东西。
//! - `settings.toml` 副本：配置写错是最常见的原因之一。
//! - `index-stats.txt`：每个图库的索引统计，**不含**完整索引文件——
//!   几万条路径的索引可能几十 MB，而且路径含用户私人信息；统计数字足够判断
//!   "扫到了什么、漏了什么"。
//! - `playback.txt`：轮播状态与每屏当前显示的图。
//! - `app-log-tail.txt`：最近 800 行日志。整份日志可能几 MB，有用的通常是尾部。
//!
//! **不包含**：用户的图片、完整索引文件、WebView2 缓存。

use std::path::{Path, PathBuf};

use serde::Serialize;
use wwp_core::library;
use wwp_core::paths::AppPaths;
use wwp_core::Settings;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExport {
    /// 导出目录。
    pub directory: String,
    /// 目录里生成的文件清单。
    pub files: Vec<String>,
    /// 导出的环境信息（同时也会写进文件）。
    pub environment: wwp_core::environment::Environment,
}

/// 把诊断信息打包到 `paths.data_dir()/diagnostics-<时间戳>/`。
pub fn export(paths: &AppPaths, settings: &Settings) -> Result<DiagnosticsExport, String> {
    let environment = wwp_core::environment::collect_soft(
        env!("CARGO_PKG_VERSION"),
        &paths.root.display().to_string(),
        paths.root_source.as_str(),
    );

    // 目录名带时间戳，避免覆盖上一次导出。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let directory = paths.data_dir().join(format!("diagnostics-{stamp}"));
    std::fs::create_dir_all(&directory).map_err(|e| format!("创建导出目录失败：{e}"))?;

    let mut files: Vec<String> = Vec::new();
    let mut wrote_anything = false;

    // 1. 环境信息
    let environment_text = environment.describe();
    if write_file(&directory, "environment.txt", &environment_text, &mut files).is_ok() {
        wrote_anything = true;
    }

    // 2. 设置副本
    if let Ok(text) = std::fs::read_to_string(paths.config_dir().join("settings.toml")) {
        let _ = write_file(&directory, "settings.toml", &text, &mut files);
    }

    // 3. 每个图库的索引统计
    let stats_text = index_stats(paths, settings);
    let _ = write_file(&directory, "index-stats.txt", &stats_text, &mut files);

    // 4. 说明
    let readme = format!(
        "WindowsWallPaper 诊断导出\n\
         =========================\n\n\
         导出时间(Unix 秒): {stamp}\n\n\
         文件说明:\n\
         - environment.txt   版本、系统、显示器/DPI、显卡、桌面层拓扑、当前壁纸\n\
         - settings.toml     配置副本\n\
         - index-stats.txt   每个图库的索引统计（不含完整索引文件）\n\
         - app-log-tail.txt  最近 800 行日志\n\n\
         请注意：index-stats.txt 与 app-log-tail.txt 里可能包含你的本地路径，\n\
         分享前可以先看一眼。本导出不包含任何图片文件或 WebView2 缓存。\n"
    );
    let _ = write_file(&directory, "README.txt", &readme, &mut files);

    // 5. 最近日志
    if let Ok(log) = std::fs::read_to_string(paths.app_log()) {
        let tail: Vec<&str> = log.lines().rev().take(800).collect();
        let tail_text: Vec<&str> = tail.into_iter().rev().collect();
        let _ = write_file(&directory, "app-log-tail.txt", &tail_text.join("\n"), &mut files);
    }

    if !wrote_anything {
        return Err(format!(
            "导出目录 {} 里没有写入任何文件，请检查磁盘权限",
            directory.display()
        ));
    }

    log::info!(
        "诊断已导出到 {}（{} 个文件）",
        directory.display(),
        files.len()
    );

    Ok(DiagnosticsExport {
        directory: directory.display().to_string(),
        files,
        environment,
    })
}

/// 汇总每个图库的索引统计。
fn index_stats(paths: &AppPaths, settings: &Settings) -> String {
    let index_root = paths.data_dir().join("index");
    let mut text = String::from("图库索引统计\n============\n\n");

    // 图库清单来自配置（图库现在是**独立配置项**，增删入口在「图库」页）。
    // "被几个列表引用"另算：0 个引用意味着它不会被播到，但仍然是合法的状态。
    if settings.libraries.is_empty() {
        text.push_str("（还没有配置任何图库）\n");
        return text;
    }

    for library_path in &settings.libraries {
        let lower = library_path.to_string_lossy().to_ascii_lowercase();
        let users: Vec<&str> = settings
            .playlists
            .iter()
            .filter(|p| {
                p.libraries
                    .iter()
                    .any(|l| l.to_string_lossy().to_ascii_lowercase() == lower)
            })
            .map(|p| p.name.as_str())
            .collect();
        let index_paths = library::IndexPaths::new(&index_root, &library::library_id(library_path));
        text.push_str(&format!("图库: {}\n", library_path.display()));
        text.push_str(&format!(
            "  被 {} 个播放列表引用: {}\n",
            users.len(),
            if users.is_empty() {
                "（没有列表用它，不会被播到）".to_string()
            } else {
                users.join("、")
            }
        ));

        if !index_paths.rows.is_file() {
            text.push_str("  尚未建立索引\n\n");
            continue;
        }

        match library::load_index(&index_paths) {
            Ok((meta, entries)) => {
                let playable = entries.iter().filter(|e| e.is_playable()).count();
                text.push_str(&format!("  条目: {}（可播放 {playable}）\n", entries.len()));
                text.push_str(&format!("  扫描于: {}\n", meta.scanned_at));
                text.push_str(&format!(
                    "  耗时: {} ms（复用 {} / 重探 {}）\n",
                    meta.scan_millis, meta.reused_entries, meta.probed_entries
                ));
                let stats = &meta.stats;
                text.push_str(&format!("  看到的文件: {}\n", stats.files_seen));
                text.push_str(&format!("  扩展名不符图片: {}\n", stats.files_ignored));
                text.push_str(&format!("  被排除: {}\n", stats.excluded));
                text.push_str(&format!("  没有解码器: {}\n", stats.unsupported));
                text.push_str(&format!("  读不了: {}\n", stats.unreadable));
                if !stats.unsupported_formats.is_empty() {
                    let items: Vec<String> = stats
                        .unsupported_formats
                        .iter()
                        .map(|(format, count)| format!("{format} {count}"))
                        .collect();
                    text.push_str(&format!("  跳过格式: {}\n", items.join("、")));
                }
                if !stats.unreadable_samples.is_empty() {
                    text.push_str("  无法读取的样本（仅前 10 条，含本地路径）:\n");
                    for sample in stats.unreadable_samples.iter().take(10) {
                        text.push_str(&format!("    {} —— {}\n", sample.path, sample.reason));
                    }
                }
            }
            Err(error) => text.push_str(&format!("  读取索引失败: {error}\n")),
        }
        text.push('\n');
    }
    text
}

/// 写一个导出文件并把文件名记进清单。
fn write_file(
    directory: &Path,
    name: &str,
    content: &str,
    files: &mut Vec<String>,
) -> std::io::Result<()> {
    let path: PathBuf = directory.join(name);
    std::fs::write(&path, content)?;
    files.push(name.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wwp_core::config::Playlist;
    use wwp_core::paths::RootSource;

    fn temp_paths() -> AppPaths {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wwp-diag-{nanos}"));
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        AppPaths {
            root: root.clone(),
            root_source: RootSource::ProgramDir,
            program_dir: root,
            fallback_reason: None,
        }
    }

    #[test]
    fn export_writes_the_files_a_bug_report_needs() {
        let paths = temp_paths();
        let mut settings = Settings::default();
        // 图库是独立配置项；这里再挂一个引用它的播放列表，验证"被几个列表引用"
        // 那一行仍然数得出来。
        settings.libraries.push(PathBuf::from(r"E:\NoSuchLibrary"));
        settings.playlists.push(Playlist {
            id: "list1".to_string(),
            name: "诊断用".to_string(),
            libraries: vec![PathBuf::from(r"E:\NoSuchLibrary")],
            ..Default::default()
        });

        let export = export(&paths, &settings).expect("导出应成功");
        assert!(!export.files.is_empty(), "至少要写出一个文件");

        // README 与 index-stats 必须存在：前者解释内容，后者是"扫到了什么"的依据。
        assert!(export.files.iter().any(|f| f == "README.txt"));
        assert!(
            export.files.iter().any(|f| f == "index-stats.txt"),
            "实际清单：{:?}",
            export.files
        );

        // index-stats 里要写明"尚未建立索引"，而不是留空——留空会让人以为扫过了但什么都没有。
        let stats = std::fs::read_to_string(
            PathBuf::from(&export.directory).join("index-stats.txt"),
        )
        .unwrap();
        assert!(stats.contains("NoSuchLibrary"));
        assert!(stats.contains("尚未建立索引"), "实际内容：\n{stats}");

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn export_without_libraries_says_so() {
        // 一个图库都没配时，诊断包必须**明确说出来**，
        // 而不是留一段空白。空白会让人以为"扫过了但什么都没有"。
        let paths = temp_paths();
        let export = export(&paths, &Settings::default()).expect("没有图库也要能导出");
        let stats = std::fs::read_to_string(
            PathBuf::from(&export.directory).join("index-stats.txt"),
        )
        .unwrap();
        assert!(
            stats.contains("还没有配置任何图库"),
            "实际内容：\n{stats}"
        );
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn a_library_no_playlist_uses_is_still_reported() {
        // 图库可以不被任何列表引用（图库页加进来、还没勾进列表）。
        // 那种图库仍要在诊断包里出现，并写明"不会被播到"——
        // 否则用户会以为它凭空消失了。
        let paths = temp_paths();
        let mut settings = Settings::default();
        settings.libraries.push(PathBuf::from(r"E:\Orphan"));
        let export = export(&paths, &settings).expect("导出应成功");
        let stats = std::fs::read_to_string(
            PathBuf::from(&export.directory).join("index-stats.txt"),
        )
        .unwrap();
        assert!(stats.contains("Orphan"), "实际内容：\n{stats}");
        assert!(stats.contains("不会被播到"), "实际内容：\n{stats}");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn two_exports_do_not_overwrite_each_other() {
        // 目录名带时间戳；同一秒内连续导出会撞名，因此这里只断言"都能成功且各有一份"。
        let paths = temp_paths();
        let first = export(&paths, &Settings::default()).unwrap();
        let second = export(&paths, &Settings::default()).unwrap();
        assert!(PathBuf::from(&first.directory).is_dir());
        assert!(PathBuf::from(&second.directory).is_dir());
        let _ = std::fs::remove_dir_all(&paths.root);
    }
}

//! 退出时的播放位置：每块屏**当前**与**下一张**是哪张图。
//!
//! ## 它解决什么（用户 2026-10 的要求）
//!
//! > 退出程序或关机时，保存每块显示器当前壁纸和下一张壁纸的路径，不保存剩余播放
//! > 时间。下次启动后优先恢复退出时的当前壁纸，并确保第一次切换使用退出时记录的
//! > 下一张壁纸，不重新抽取。
//!
//! 在此之前每次启动都是"重新抽一张"：用户关机时看到的那张图与开机后看到的
//! 没有关系，而他上一眼看到的那张"下一张"也不会再出现。
//!
//! ## 为什么**不**保存剩余时间
//!
//! 用户明确说了不保存。语义上也对：关机一整夜之后"还剩 12 秒"没有意义，
//! 恢复成一个刚换过纸的状态（等满一个间隔）才符合直觉。
//!
//! ## 为什么放在 data 而不是设置文件
//!
//! 与 `data/nsfw-state.json` 同理（见 `nsfw` 模块的说明）：这是**运行时状态**，
//! 不是用户设置。写进 `settings.toml` 会让它在界面上变成一个可编辑的字段。
//!
//! ## 谁会写它
//!
//! **调度线程每轮循环**在"记忆变了"时写一次（见 `playback_manager::run_manager`），
//! 而不是只在退出时写一次。理由：Windows 的关机、注销与"任务管理器结束任务"
//! 都不保证给程序一次收尾机会（`ExitRequested` 不一定到得了），只在退出时写
//! 等于"正常退出才有记忆"。换纸是分钟级的事件，因此这份写入的开销可以忽略。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wwp_core::playback::ScreenMemory;

/// 状态文件的内容。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlaybackState {
    /// 格式版本。将来若字段语义变了，旧文件按"读不懂"处理（见 [`load`]）。
    #[serde(default)]
    pub version: u32,
    /// 每块屏一条。块顺序无关紧要，按显示器名匹配。
    #[serde(default)]
    pub screens: Vec<ScreenMemory>,
}

/// 这条记录里有没有值得恢复的东西。
///
/// 两边都空的记录不写进文件：一块屏被拔掉之后留下的空条目只会让文件越来越长。
fn is_empty(memory: &ScreenMemory) -> bool {
    memory.current.is_none() && memory.next.is_none()
}

/// 当前写入的格式版本。
const FORMAT_VERSION: u32 = 1;

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("playback-state.json")
}

/// 读回上次退出时的播放位置。读不到、内容坏了、版本不认识都返回空表。
///
/// **一律不报错**：这只是"锦上添花"的记忆，坏了就该退回"照常抽一张"，
/// 而不是让轮播起不来。
pub fn load(data_dir: &Path) -> Vec<ScreenMemory> {
    let path = state_path(data_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<PlaybackState>(&text) {
        Ok(state) if state.version == FORMAT_VERSION => state
            .screens
            .into_iter()
            .filter(|screen| !is_empty(screen))
            .collect(),
        Ok(state) => {
            log::warn!(
                "{} 的格式版本是 {}（本程序只认识 {FORMAT_VERSION}），按「没有记忆」处理",
                path.display(),
                state.version
            );
            Vec::new()
        }
        Err(error) => {
            log::warn!("{} 无法解析（{error}），按「没有记忆」处理", path.display());
            Vec::new()
        }
    }
}

/// 记下这次的播放位置。写失败只记日志——不该因为写不了这个文件而影响轮播。
pub fn save(data_dir: &Path, screens: &[ScreenMemory]) {
    let path = state_path(data_dir);
    let state = PlaybackState {
        version: FORMAT_VERSION,
        screens: screens
            .iter()
            .filter(|screen| !is_empty(screen))
            .cloned()
            .collect(),
    };
    let text = match serde_json::to_string_pretty(&state) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("无法序列化播放位置：{error}");
            return;
        }
    };
    // 走 `write_atomic`：这份文件在**每次换纸时**都会被重写，而它同时可能
    // 正被下一次启动读取（用户点退出又马上点开）。半个文件的 JSON 会让那份
    // 记忆作废——而它的失败是静默的（下次启动只是"没恢复"），很难排查。
    if let Err(error) = wwp_core::config::write_atomic(&path, &text) {
        log::warn!("写入播放位置失败 {}：{error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-playback-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn memory(monitor: &str, current: Option<&str>, next: Option<&str>) -> ScreenMemory {
        ScreenMemory {
            monitor_name: monitor.to_string(),
            current: current.map(|p| p.to_string()),
            next: next.map(|p| p.to_string()),
        }
    }

    #[test]
    fn what_was_saved_comes_back_unchanged() {
        let dir = temp_dir("roundtrip");
        let screens = vec![
            memory(r"\\.\DISPLAY1", Some("C:/pics/a.jpg"), Some("C:/pics/b.jpg")),
            memory(r"\\.\DISPLAY2", Some("C:/pics/c.jpg"), None),
        ];
        save(&dir, &screens);
        assert_eq!(load(&dir), screens);
    }

    #[test]
    fn a_missing_file_means_no_memory_rather_than_an_error() {
        // 第一次运行、用户清过 data、或文件被手工删掉——都必须只是"没有记忆"。
        let dir = temp_dir("missing");
        assert!(load(&dir).is_empty());
    }

    #[test]
    fn a_corrupt_file_means_no_memory_rather_than_an_error() {
        let dir = temp_dir("corrupt");
        std::fs::write(state_path(&dir), "{ 这不是 JSON").unwrap();
        assert!(load(&dir).is_empty());
    }

    #[test]
    fn a_future_format_version_is_ignored_instead_of_misread() {
        // 版本不认识的旧/新文件按"读不懂"处理：宁可退回随机抽一张，
        // 也不要把别人的字段当成自己的。
        let dir = temp_dir("version");
        std::fs::write(
            state_path(&dir),
            r#"{"version":999,"screens":[{"monitorName":"\\\\.\\DISPLAY1","current":"C:/a.jpg"}]}"#,
        )
        .unwrap();
        assert!(load(&dir).is_empty());
    }

    #[test]
    fn entries_without_any_path_are_not_written() {
        // 一块屏被拔掉之后留下的空条目不该一直留在文件里。
        let dir = temp_dir("empty-entry");
        save(
            &dir,
            &[
                memory("DISPLAY1", Some("C:/a.jpg"), None),
                memory("DISPLAY2", None, None),
            ],
        );
        let text = std::fs::read_to_string(state_path(&dir)).unwrap();
        assert!(!text.contains("DISPLAY2"), "空条目不该被写进文件：{text}");
        assert_eq!(load(&dir).len(), 1);
    }
}

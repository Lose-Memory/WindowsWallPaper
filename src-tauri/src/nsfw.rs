//! NSFW 模式：状态持久化与切换。
//!
//! ## 它是什么
//!
//! 用户预先给每块屏指定一张图，点一次按钮（或按全局快捷键）就把两屏都切过去、
//! 并**停下轮播**。再点一次回到正常轮播。
//!
//! ## 状态为什么放在 data 而不是设置文件
//!
//! "现在处于 NSFW 模式"是**运行时状态**，不是用户设置。写进 `settings.toml`
//! 会让它在界面上变成一个可编辑的字段，而它不是。因此单独一个小文件：
//! `data/nsfw-state.json`，只有 `{"active":true}` 这一个事实。
//!
//! 之所以要持久化：用户要求"下次启动时依然保持这两张壁纸"。开机后如果直接
//! 开始轮播，那两张就被换掉了——所以启动时必须知道"上次是在 NSFW 模式里"。
//!
//! ## 1.12.0 改名留下的旧文件
//!
//! 模式原先叫 SAFE，状态文件因此叫 `data/safe-state.json`。改名之后**读**的时候
//! 会退回那个旧名字（用户升级后第一次启动仍然记得"上次停在 NSFW 模式里"），
//! 而**写**的时候用新名字并把旧文件删掉——不然它会永远留在 `data/` 里，
//! 而清理程序目录那份清单只认"跑一次就会自己回来"的东西，不含它。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::AppState;

/// 状态文件的内容。
#[derive(Debug, Default, Serialize, Deserialize)]
struct NsfwState {
    /// 是否处于 NSFW 模式。
    #[serde(default)]
    active: bool,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("nsfw-state.json")
}

/// 改名之前那个文件名（1.12.0 之前叫 SAFE）。只读一次做迁移。
fn legacy_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("safe-state.json")
}

/// 当前是否处于 NSFW 模式。
///
/// 读不到或内容坏了都当作"不在 NSFW 模式"：NSFW 模式是用户主动进入的，
/// 一个坏文件不该让程序启动后就固定成两张图且不轮播——那是很难联想到的状态。
pub fn is_active(data_dir: &Path) -> bool {
    let path = state_path(data_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // 新文件不在就看改名之前那个：升级之后第一次启动走的就是这一条。
        Err(_) => match std::fs::read_to_string(legacy_state_path(data_dir)) {
            Ok(text) => text,
            Err(_) => return false,
        },
    };
    match serde_json::from_str::<NsfwState>(&text) {
        Ok(state) => state.active,
        Err(error) => {
            log::warn!("{} 无法解析（{error}），按「不在 NSFW 模式」处理", path.display());
            false
        }
    }
}

/// 记下是否处于 NSFW 模式。
pub fn set_active(data_dir: &Path, active: bool) {
    let path = state_path(data_dir);
    let text = match serde_json::to_string_pretty(&NsfwState { active }) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("无法序列化 NSFW 模式状态：{error}");
            return;
        }
    };
    if let Err(error) = std::fs::write(&path, text) {
        log::warn!("写入 NSFW 模式状态失败 {}：{error}", path.display());
    }
    // 旧文件用完就删：上面那次读已经把它迁移过来了，再留着只会让下一个人
    // 以为还有两套状态。
    let legacy = legacy_state_path(data_dir);
    if legacy.exists() {
        if let Err(error) = std::fs::remove_file(&legacy) {
            log::debug!("删不掉改名前的状态文件 {}：{error}", legacy.display());
        }
    }
}

/// 切换 NSFW 模式。界面按钮与全局快捷键都走这里，因此两条路的行为必然一致。
///
/// 返回给用户看的结果文字（成功/部分失败的原因）。
pub fn toggle<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<String, String> {
    let state = app.state::<AppState>();
    let settings = state.settings();

    if state.playback.is_nsfw() {
        state.playback.leave_nsfw(&settings)?;
        set_active(&state.paths.data_dir(), false);
        return Ok("已退出 NSFW 模式".to_string());
    }

    let results = state.playback.enter_nsfw(&settings)?;
    state.playback.record_nsfw(&results);
    set_active(&state.paths.data_dir(), true);

    let failed: Vec<&crate::playback_manager::ScreenResult> =
        results.iter().filter(|result| !result.ok).collect();
    if failed.is_empty() {
        Ok("已进入 NSFW 模式（固定壁纸，已暂停轮播）".to_string())
    } else {
        Ok(format!(
            "已进入 NSFW 模式，但有 {} 块屏没切成：{}",
            failed.len(),
            failed
                .iter()
                .map(|r| format!("{}（{}）", r.monitor_name, r.outcome))
                .collect::<Vec<_>>()
                .join("；")
        ))
    }
}

/// 启动时按"上次是不是在 NSFW 模式"决定做什么。
///
/// 三条分支，顺序不能变：
///
/// 1. **上次在 NSFW 模式、并且现在仍有 NSFW 壁纸** → 直接切回那两张，**不开始轮播**；
/// 2. 上次在 NSFW 模式、但 NSFW 壁纸已被清空 → 忽略那个状态，按正常流程走；
/// 3. 否则 → `auto_start` 打开就开始轮播，并**优先恢复上次退出时的壁纸**。
///
/// 第 3 条里"不再记住上次的暂停状态"是用户明确要求的：那个机制（存的是
/// 暂停/停止）已经删除。现在记的是**播放到哪张图**，是另一回事，文件也不同
/// （`data/playback-state.json`，见 `playback_state` 模块）。
pub fn apply_startup_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let data_dir = state.paths.data_dir();

    let has_nsfw_wallpaper = settings
        .monitors
        .values()
        .any(|monitor| !monitor.nsfw_wallpaper.trim().is_empty());

    if is_active(&data_dir) && has_nsfw_wallpaper {
        log::info!("上次退出时处于 NSFW 模式，本次直接显示固定的 NSFW 壁纸（不开始轮播）");
        // 顺手把它**写一遍**：内容与读到的完全一样，但 1.12.0 之前留下的
        // `safe-state.json` 会被这一步收掉（`set_active` 写完新文件就删旧文件）。
        // 不这样的话，只在用户点一次 NSFW 时才会迁移，而"一直停在 NSFW 模式、
        // 从不切换"的用户永远留着一个旧文件——它不在任何清理清单里。
        set_active(&data_dir, true);
        match state.playback.enter_nsfw(&settings) {
            Ok(results) => {
                let failed = results.iter().filter(|r| !r.ok).count();
                if failed > 0 {
                    log::warn!("NSFW 模式恢复时 {failed} 块屏没能切过去");
                }
            }
            Err(error) => log::error!("恢复 NSFW 模式失败：{error}"),
        }
        return;
    }
    if is_active(&data_dir) && !has_nsfw_wallpaper {
        log::warn!("NSFW 模式状态还在，但没有任何 NSFW 壁纸，已清除该状态并回到正常流程");
        set_active(&data_dir, false);
    }

    if !settings.general.auto_start {
        log::info!("启动自动轮播已关闭，等待手动开始");
        return;
    }

    log::info!("启动自动开始轮播（优先恢复上次退出时的壁纸，不再参考上次的暂停状态）");
    if let Err(error) = state.playback.start_restored(&settings) {
        log::error!("启动轮播失败（可以在界面上手动点「开始轮播」）：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个只属于这次测试的临时目录（与 `cleanup` 那边的做法一致）。
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-nsfw-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_state_round_trips_and_a_missing_file_means_not_active() {
        let dir = temp_dir("roundtrip");
        // 没有文件 = 不在 NSFW 模式（程序从没进过，或用户把 data 清了）。
        assert!(!is_active(&dir));

        set_active(&dir, true);
        assert!(
            dir.join("nsfw-state.json").is_file(),
            "写的时候用的是新文件名"
        );
        assert!(is_active(&dir));
        set_active(&dir, false);
        assert!(!is_active(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_from_before_the_rename_is_read_once_and_then_removed() {
        // 1.12.0 之前这个文件叫 `safe-state.json`。升级之后第一次启动必须仍然
        // 记得"上次停在 NSFW 模式里"——否则用户会发现"开机后它自己开始轮播了"，
        // 而他上次明明停在固定壁纸上。旧文件用完就删：留着只会让人以为还有两套状态。
        let dir = temp_dir("legacy");
        std::fs::write(legacy_state_path(&dir), r#"{"active":true}"#).unwrap();

        assert!(is_active(&dir), "改名前的状态必须被读到");
        assert!(
            !state_path(&dir).exists(),
            "只读不写：读一次不该顺手创建新文件"
        );

        set_active(&dir, true);
        assert!(state_path(&dir).is_file(), "写的时候用新名字");
        assert!(
            !legacy_state_path(&dir).exists(),
            "旧文件应当被删掉（它不在「清理程序目录」的清单里）"
        );
        assert!(is_active(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! 系统广播监听：显示器插拔、分辨率变化、系统设置变化。
//!
//! ## 为什么需要一个隐藏的顶层窗口
//!
//! `WM_DISPLAYCHANGE`（显示器插拔/分辨率变化）是**广播**消息，只发给顶层窗口。
//! 这一点在探测阶段就确认过：被 `SetParent` 成子窗口的窗口收不到广播。
//! 因此这里留一个不显示、不抢焦点的顶层窗口专门听广播，
//! 收到后转成对应用层有意义的动作（目前是"重新适配显示器"）。
//!
//! ## 为什么收到变化要重启轮播
//!
//! 每块屏的播放计划是在启动时按当时的显示器列表算出来的（候选集合、间隔、顺序）。
//! 拔掉一块屏之后旧计划里仍然有它，留着它只会让计时线程对着不存在的屏
//! 反复调 `SetWallpaper` 然后失败刷日志；插上一块新屏则完全不会被播放。
//! 因此正确的反应是**按新的显示器列表重建计划**。
//!
//! 重启时沿用已有的壁纸快照，不会因此丢掉"用户原来的壁纸"（见
//! [`crate::playback_manager::PlaybackManager::start`] 的说明）。

use std::sync::OnceLock;

use tauri::{AppHandle, Manager};
use wwp_core::hidden_window;
use wwp_core::hotkey::{self, HotkeySpecs};

use crate::playback_manager::PlaybackCommand;

/// 应用句柄。窗口过程是 C 函数指针，拿不到闭包捕获，因此只能走全局。
/// 在 `setup` 里设置一次，之后不再变化。
static APP: OnceLock<AppHandle<tauri::Wry>> = OnceLock::new();

/// 启动广播监听线程，并注册全局快捷键。
///
/// 两件事共用一个隐藏窗口线程：`WM_DISPLAYCHANGE` 这类广播与 `WM_HOTKEY`
/// 都只投递给"注册它们的那个窗口"，而注册必须在拥有消息队列的线程上做。
/// 分成两个窗口/两个线程只会多一份状态。
pub fn start(app: &AppHandle<tauri::Wry>) {
    let _ = APP.set(app.clone());

    hidden_window::set_display_change_handler(on_display_change);
    hidden_window::set_setting_change_handler(on_setting_change);
    hidden_window::set_hotkey_handler(on_hotkey);
    // 另一个实例启动时会请我们把主界面显示出来（单实例：第二次启动不再开新进程）。
    hidden_window::set_show_window_handler(on_show_main_window);
    // 告诉窗口线程要注册哪几个快捷键；窗口还没建好时它会记下，建完自己注册。
    hidden_window::set_hotkey_specs(&HotkeySpecs::from(
        &app.state::<crate::AppState>().settings().general,
    ));

    std::thread::Builder::new()
        .name("wwp-broadcast-listener".to_string())
        .spawn(|| {
            if let Err(error) = hidden_window::run_message_loop() {
                log::error!("系统广播监听线程退出：{error}");
            }
        })
        .map(|_| log::info!("系统广播与全局快捷键监听已启动"))
        .unwrap_or_else(|error| log::error!("无法启动广播监听线程：{error}"));
}

/// 全局快捷键被按下。按 id 分发到这五个动作上。
///
/// 前四个动作复用界面按钮走的同一段逻辑（`nsfw::toggle` / `PlaybackCommand`），
/// 第五个（显示主界面）复用托盘的 `tray::show_main_window`，
/// 因此快捷键与界面/托盘的行为不可能不一致。
fn on_hotkey(id: i32) {
    let Some(app) = APP.get() else {
        return;
    };
    let state = app.state::<crate::AppState>();

    match id {
        hotkey::NSFW_HOTKEY_ID => match crate::nsfw::toggle(app) {
            Ok(message) => log::info!("快捷键：{message}"),
            Err(error) => log::error!("快捷键：切换 NSFW 模式失败：{error}"),
        },
        hotkey::PAUSE_HOTKEY_ID => {
            // 未在轮播时"暂停"没有意义，明确记一条而不是静默无反应。
            if !state.playback.is_running() {
                log::info!("快捷键：轮播未在运行，暂停/继续无效果（先点「开始轮播」）");
                return;
            }
            let paused = state.playback.status().paused;
            let (command, what) = if paused {
                (PlaybackCommand::Resume, "继续")
            } else {
                (PlaybackCommand::Pause, "暂停")
            };
            match state.playback.send(command) {
                Ok(()) => log::info!("快捷键：已{what}所有屏的轮播"),
                Err(error) => log::error!("快捷键：{what}失败：{error}"),
            }
        }
        hotkey::NEXT_HOTKEY_ID => send_control(&state, PlaybackCommand::Next, "下一张"),
        hotkey::PREVIOUS_HOTKEY_ID => send_control(&state, PlaybackCommand::Previous, "上一张"),
        // 「显示主界面」不改任何状态，因此不需要 `state`：与托盘的「打开主界面」、
        // 第二次启动程序走的是同一段逻辑（显示 / 取消最小化 / 拿焦点）。
        hotkey::SHOW_WINDOW_HOTKEY_ID => {
            crate::tray::show_main_window(app);
            log::info!("快捷键：已显示主界面");
        }
        other => log::warn!("收到未知的快捷键 id={other}"),
    }
}

/// 发一条"作用于所有屏"的轮播指令。
///
/// 未在轮播时**明确说出来**：用户按了键、屏幕上什么都没发生，
/// 若日志里也没有一行，那他就只能怀疑快捷键没注册成功。
fn send_control(state: &crate::AppState, command: PlaybackCommand, what: &str) {
    if !state.playback.is_running() {
        log::info!("快捷键：轮播未在运行，「{what}」无效果（先点「开始轮播」）");
        return;
    }
    match state.playback.send(command) {
        Ok(()) => log::info!("快捷键：已请求「{what}」（所有屏）"),
        Err(error) => log::error!("快捷键：「{what}」失败：{error}"),
    }
}

/// 显示器配置变化。
fn on_display_change() {
    let Some(app) = APP.get() else {
        return;
    };
    let state = app.state::<crate::AppState>();

    // 每块屏的计划里都带着屏的尺寸与候选集合，显示器变了就必须重建。
    let status = state.playback.status();
    if !status.running {
        // 没在轮播时什么都不用做：下次启动会按新的显示器列表算计划。
        log::info!("显示器配置已变化（当前未轮播，无需适配）");
        return;
    }

    log::info!("检测到显示器变化，正在按新的显示器列表重建每屏计划");
    let settings = state.settings();
    let was_paused = status.paused;
    match state.playback.start(&settings, !was_paused) {
        Ok(()) => {
            if was_paused {
                // start(apply_immediately=false) 已经把它置于暂停，这里不必再动。
                log::info!("已按新的显示器列表重建轮播（保持暂停）");
            } else {
                log::info!("已按新的显示器列表重建轮播");
            }
        }
        Err(error) => log::error!("显示器变化后重建轮播失败：{error}"),
    }
}

/// 系统设置变化（主题、语言等）。
///
/// 目前只记日志：界面是 WebView2，主题由它自己跟随系统；
/// 将来若在外壳里画自绘控件，这里就是刷新它们的入口。
fn on_setting_change() {
    log::debug!("收到 WM_SETTINGCHANGE 广播");
}

/// 另一个实例启动时发来的"请显示主界面"。
///
/// 复用托盘那一份 [`crate::tray::show_main_window`]：用户点托盘图标、
/// 双击 exe 两次，最终都走同一个显示逻辑，因此不可能出现"托盘能打开、
/// 二次启动打不开"这种只有一条路坏掉的情况。
fn on_show_main_window() {
    let Some(app) = APP.get() else {
        return;
    };
    crate::tray::show_main_window(app);
    log::info!("已按另一个实例的请求显示主界面");
}

//! 系统托盘。
//!
//! 五项，顺序固定：显示主界面 / 切换到下一张 / 轮播中 / NSFW / 退出。
//!
//! ## 为什么「轮播中」「NSFW」做成带勾的菜单项
//!
//! 用户 2026-10-05 报："托盘的按钮无法显示是否选中当前功能"——点开菜单看不出
//! 现在到底是在轮播、已暂停，还是处在 NSFW 模式里，只能点一下试试。
//! 现在这两项前面带勾：**勾 = 此刻正处于那个状态**。
//!
//! 文字保持固定（「轮播中」不会改成「已暂停」）：与界面上的 NSFW 按钮同一个道理
//! （见 §10.21）——文字长短一变，整张菜单会跟着左右跳；状态交给勾来表达更直观。
//!
//! 点「轮播中」做的事与界面上那个「暂停/继续」按钮**完全一样**：
//! 没在跑就开，暂停中就是继续，正在跑就是暂停。
//!
//! ## 勾的状态为什么由一个 200 ms 的小线程轮询
//!
//! 状态改变点有四处（界面命令、全局快捷键、托盘自己、以及调度线程内部处理
//! `Pause` / `Resume`），而且 `send()` 是**异步**的：它返回时 `paused` 还没置位，
//! 在调用处刷新会得到慢一拍、甚至相反的结果。四处挂钩子必然会漏掉一处——
//! 本项目已经因为"两套机制只有一半能工作"踩过一次坑（见 §7.2 / §7.3）。
//!
//! 因此这里只读三个原子量（有没有调度线程 / 是否暂停 / 是否 NSFW 模式），
//! 值变了才动一次菜单。代价是每 200 ms 三次原子读（可以忽略），
//! 好处是**不可能漏掉任何一次状态变化**，也不需要别处配合。
//!
//! 注意：菜单项的勾在点击时由菜单库**自动翻转**（muda 的行为），
//! 而翻转结果与我们的动作方向一致（点「轮播中」本来就是要把轮播状态翻过来），
//! 所以这里不自己再翻一次；轮询会在 200 ms 内把**真实**状态对回来——
//! 例如"点了开始但一块屏都没有、启动失败"时，勾会自己退回去。
//!
//! ## 为什么"退出"要单独一个菜单项
//!
//! 关闭窗口只是隐藏到托盘（这是壁纸软件的常态：用户关掉设置界面，
//! 但壁纸还要继续换）。因此必须有一个明确的退出入口，否则用户只能去任务管理器。
//! 退出前先停掉调度线程，避免它正写到一半时进程被拆掉（留下一个半截文件）。
//! **不还原壁纸**：只换壁纸、不负责换回来（见 §10.10）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::AppState;

/// 托盘菜单项 ID。用常量而不是字面量：菜单事件的匹配与菜单的构造必须一致，
/// 而这两处相隔很远，写错字面量不会有编译错误。
pub const MENU_SHOW: &str = "wwp-show";
pub const MENU_NEXT: &str = "wwp-next";
/// 「轮播中」——可勾选项，勾表示"正在轮播（未被全局暂停）"。
pub const MENU_ROTATION: &str = "wwp-rotation";
/// 「NSFW」——可勾选项，勾表示"正处于 NSFW 模式"。
pub const MENU_NSFW: &str = "wwp-nsfw";
pub const MENU_EXIT: &str = "wwp-exit";

/// 托盘图标的 id。
const TRAY_ID: &str = "main-tray";

/// 勾选状态的对账间隔。
///
/// 200 ms 是"用户察觉不到"与"开销可忽略"之间的取值：每轮只读三个原子量，
/// 值没变就什么都不做。用轮询而不是事件的原因见模块文档。
const CHECK_INTERVAL: Duration = Duration::from_millis(200);

/// 单击与双击的分界窗口（见 [`on_left_click`]）。
///
/// Windows 的双击判定默认是 500 ms（`GetDoubleClickTime`），这里取 300 ms：
/// 用户选了"单击换一张"之后，单击要有"按下去就换"的手感，等半秒太钝；
/// 而 300 ms 足够让正常速度的双击的第二下赶在动作之前到达。
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(300);

/// 托盘图标单击的序号。每来一次单击 +1，判定为双击时也 +1
/// （用来**作废**已经排队的那次单击动作）。
static CLICK_SEQ: AtomicU64 = AtomicU64::new(0);

/// 进程启动时刻，用来算"距离上次点击过了多久"。
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// 最近一次**被判定为双击**的时刻（毫秒，相对 [`START`]）。`u64::MAX` = 还没有过。
static LAST_DOUBLE_CLICK: AtomicU64 = AtomicU64::new(u64::MAX);

/// 最近一次单击的时刻。用它**自己判双击**，见 [`on_left_click`]。
static LAST_CLICK: AtomicU64 = AtomicU64::new(u64::MAX);

fn now_millis() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 一次左键单击事件该怎么解读。
///
/// 抽成纯函数是为了能测：这段判定的三种结果**很难靠手点验证**
/// （"双击的第一下不该换纸"这种事只有日志看得出来），而它一旦错了，
/// 用户看到的是"点一下换两张"或者"双击没反应"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// 就是单击。
    Single,
    /// 与上一次单击挨得太近 → 用户其实是在双击。
    Double,
    /// 刚刚已经判定过一次双击，这一下是它的尾巴，忽略。
    Ignore,
}

/// 判定规则见 [`ClickKind`]。`u64::MAX` 表示"还没有过"。
pub fn classify_click(now: u64, previous_click: u64, last_double: u64) -> ClickKind {
    let window = DOUBLE_CLICK_WINDOW.as_millis() as u64;
    if last_double != u64::MAX && now.saturating_sub(last_double) < window {
        // 双击之后紧跟着的单击（Windows 会给三次消息）不再当成单击。
        ClickKind::Ignore
    } else if previous_click != u64::MAX && now.saturating_sub(previous_click) < window {
        ClickKind::Double
    } else {
        ClickKind::Single
    }
}

/// 托盘图标用的 PNG。用 `include_bytes!` 把图标编进可执行文件：
/// 托盘图标必须随时可用，而"去磁盘找图标文件"会在文件缺失时静默失败。
const TRAY_ICON: &[u8] = include_bytes!("../icons/32x32.png");

/// 建立托盘图标与菜单。
///
/// 这里**不再泛型化**（只接受 `AppHandle<Wry>`）：勾选状态的刷新线程要把菜单项
/// 句柄 move 进一个 `'static` 线程，泛型 `R` 会把这个约束一路带下去，
/// 而这个程序只跑 Wry 一种运行时。
pub fn setup(app: &AppHandle<tauri::Wry>) -> tauri::Result<()> {
    // 顺序是用户指定的：显示主界面、切换到下一张、轮播中、NSFW、退出。
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主界面", true, None::<&str>)?;
    // 文字要说准它做的事：这一项**不是**"随机再抽一张"，而是把播放页上
    // 已经预告的那张换上去（见 `next_wallpaper`）。用户 2026-10 指出原来的
    // "随机换一张"与实际行为不符。
    let next = MenuItem::with_id(app, MENU_NEXT, "切换到下一张", true, None::<&str>)?;
    let rotation = CheckMenuItem::with_id(app, MENU_ROTATION, "轮播中", true, false, None::<&str>)?;
    let nsfw = CheckMenuItem::with_id(app, MENU_NSFW, "NSFW", true, false, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let exit = MenuItem::with_id(app, MENU_EXIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &next, &rotation, &nsfw, &separator, &exit])?;

    let icon = tauri::image::Image::from_bytes(TRAY_ICON)?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("WindowsWallPaper")
        .menu(&menu)
        // 左键单击按设置分派（打开主界面 / 换一张），因此**不让**左键弹菜单。
        // 菜单仍然用右键打开——那是 Windows 上的另一套习惯，两边互不干扰。
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } => on_left_click(tray.app_handle()),
            TrayIconEvent::DoubleClick {
                button: tauri::tray::MouseButton::Left,
                ..
            } => on_left_double_click(tray.app_handle()),
            _ => {}
        })
        .on_menu_event(|app, event| {
            handle_menu_event(app, event.id().as_ref());
        })
        .build(app)?;

    spawn_check_refresher(app.clone(), rotation, nsfw);
    log::info!("托盘已就绪");
    Ok(())
}

/// 左键**单击**托盘图标。
///
/// 动作由设置决定（`general.tray_click_action`）：
/// `open-window` → 显示主界面；`next-wallpaper` → 换一张壁纸。
///
/// ## 双击是自己判的，不依赖 `WM_LBUTTONDBLCLK`
///
/// 选了"单击换一张"之后，双击用来打开主界面。而双击有两层麻烦：
///
/// 1. 双击的**两下都会各自先来一次单击**（`Click`，可能还有 `DoubleClick`）；
/// 2. `WM_LBUTTONDBLCLK` 那个消息由资源管理器**合成**后作为回调消息的参数发过来，
///    而托盘窗口类并没有注册 `CS_DBLCLKS`——因此不能假设它一定会到。
///
/// 所以这里不靠那个消息：**两次单击挨得比 [`DOUBLE_CLICK_WINDOW`] 还近 = 双击**，
/// 取消排队中的换纸、直接打开主界面。`DoubleClick` 事件如果来了，走的是同一段逻辑
/// （[`on_left_double_click`]），两条路的结果一样，不会重复换纸。
fn on_left_click(app: &AppHandle<tauri::Wry>) {
    let state = app.state::<AppState>();
    let action = state.settings().general.tray_click_action;

    if action == wwp_core::config::TrayClickAction::OpenWindow {
        show_main_window(app);
        return;
    }

    let now = now_millis();
    let previous = LAST_CLICK.swap(now, Ordering::SeqCst);
    let kind = classify_click(now, previous, LAST_DOUBLE_CLICK.load(Ordering::SeqCst));
    match kind {
        // 刚判定过双击：这一下是双击的尾巴。
        ClickKind::Ignore => {
            log::debug!("托盘：刚双击过，忽略这一次单击");
            return;
        }
        // 上一次单击就在刚才 → 用户其实是在双击。
        ClickKind::Double => {
            on_left_double_click(app);
            return;
        }
        ClickKind::Single => {}
    }

    // 普通单击：等一个双击窗口再换，免得双击的第一下就把壁纸换了。
    let seq = CLICK_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let for_thread = app.clone();
    let spawned = std::thread::Builder::new()
        .name("wwp-tray-click".to_string())
        .spawn(move || {
            std::thread::sleep(DOUBLE_CLICK_WINDOW);
            // 这期间序号被推过 → 用户其实是在双击，换纸取消。
            if CLICK_SEQ.load(Ordering::SeqCst) != seq {
                log::debug!("托盘：单击被后来的双击取消（不换纸）");
                return;
            }
            next_wallpaper(&for_thread);
        });
    if let Err(error) = spawned {
        log::warn!("无法启动托盘单击的处理线程，改为立刻换一张：{error}");
        next_wallpaper(app);
    }
}

/// 判定为**双击**：打开主界面，并作废已经排队的那次"换一张"。
///
/// 两种设置下都这么做：选 `open-window` 时单击已经打开过，再打开一次是幂等的；
/// 选 `next-wallpaper` 时这正是打开界面的唯一方式（用户的要求）。
fn on_left_double_click(app: &AppHandle<tauri::Wry>) {
    // 记下时刻并推序号：前者让紧随其后的"单击尾巴"被忽略，后者作废排队的换纸。
    LAST_DOUBLE_CLICK.store(now_millis(), Ordering::SeqCst);
    CLICK_SEQ.fetch_add(1, Ordering::SeqCst);
    log::info!("托盘：单击被判定为双击 → 打开主界面");
    show_main_window(app);
}

/// 起一个后台线程，按**实际状态**给「轮播中」「NSFW」打勾。
///
/// 线程活到进程结束。取状态用 `try_state` 而不是 `state`：退出过程中状态可能
/// 已经被拆掉，那时 `state()` 会 panic，而这个线程只是让菜单好看一点，
/// 不该在退出路径上制造一条 panic 记录。
fn spawn_check_refresher(
    app: AppHandle<tauri::Wry>,
    rotation: CheckMenuItem<tauri::Wry>,
    nsfw: CheckMenuItem<tauri::Wry>,
) {
    let spawned = std::thread::Builder::new()
        .name("wwp-tray-checks".to_string())
        .spawn(move || {
            // None = 还没对过账，因此第一轮一定会写一次（把启动时的真实状态摆正）。
            let mut last: Option<(bool, bool)> = None;
            loop {
                std::thread::sleep(CHECK_INTERVAL);
                let Some(state) = app.try_state::<AppState>() else {
                    return; // 正在退出
                };
                // 「轮播中」= 有调度线程 且 没有被全局暂停。
                // NSFW 模式里没有调度线程，因此它天然是未勾选——那正是
                // "NSFW 模式把轮播停下来了"的如实反映，与 NSFW 那个勾互补。
                let now = (
                    rotation_checked(state.playback.is_running(), state.playback.is_paused()),
                    state.playback.is_nsfw(),
                );
                if last == Some(now) {
                    continue;
                }
                last = Some(now);

                // 每次变化记一行：托盘上的勾是"用户看不见代码"的地方，
                // 排查时最需要知道的就是"它到底有没有在对账、算出来的是什么"。
                log::debug!("托盘勾选状态：轮播中={}，NSFW={}", now.0, now.1);

                let (rotation_item, safe_item) = (rotation.clone(), nsfw.clone());
                // `set_checked` 必须在**主线程**上做：muda 内部的状态是 `Rc<RefCell<..>>`
                // （本项目没有开它的 snapshot 特性），从别的线程直接改会与主线程
                // （正在显示/操作菜单）撞上——轻则这个线程 panic，重则整个程序崩。
                if let Err(error) = app.run_on_main_thread(move || {
                    if let Err(error) = rotation_item.set_checked(now.0) {
                        log::warn!("设置托盘菜单「轮播中」的勾选状态失败：{error}");
                    }
                    if let Err(error) = safe_item.set_checked(now.1) {
                        log::warn!("设置托盘菜单「NSFW」的勾选状态失败：{error}");
                    }
                }) {
                    log::debug!("无法把托盘勾选状态投递到主线程（多半是正在退出）：{error}");
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("无法启动托盘勾选状态的刷新线程（菜单上的勾会停在初始值）：{error}");
    }
}

/// 「轮播中」这一项的勾应该是什么。
///
/// 抽成纯函数是为了能测——它有两个容易写错的地方：
///
/// 1. **暂停时调度线程还在**（`is_running()` 为真），只看 `is_running` 会把
///    "已暂停"显示成"正在轮播"；
/// 2. NSFW 模式里没有调度线程（`is_running()` 为假），因此这一项必须是未勾选。
pub fn rotation_checked(running: bool, paused: bool) -> bool {
    running && !paused
}

/// 处理托盘菜单点击。
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        MENU_SHOW => show_main_window(app),
        MENU_NEXT => next_wallpaper(app),
        MENU_ROTATION => toggle_rotation(app),
        MENU_NSFW => toggle_nsfw(app),
        MENU_EXIT => exit_app(app),
        other => log::warn!("收到未知的托盘菜单项：{other}"),
    }
}

/// 切换 NSFW 模式（与界面按钮、全局快捷键走同一段逻辑）。
fn toggle_nsfw<R: Runtime>(app: &AppHandle<R>) {
    match crate::nsfw::toggle(app) {
        Ok(message) => log::info!("托盘：{message}"),
        Err(error) => log::error!("托盘：切换 NSFW 模式失败：{error}"),
    }
}

/// 显示并聚焦主窗口。
///
/// 用户 2026-10 的要求："当我使用快捷键唤起主页面时，默认把页面调到播放页，
/// 而不是其他我上次打开过的页面。"窗口只是隐藏、并没有卸载，因此前端会一直
/// 停在用户上次打开的那一页；这里在**把它叫到前台之前**发一个事件，前端收到
/// 就切回播放页。
///
/// 只在该窗口**本来不在屏幕上**（隐藏或最小化）时才发：用户正开着设置页在改
/// 东西，此时点一下托盘图标（托盘动作就是"显示主界面"）不该把页面抢走。
/// 判断放在 `show`/`unminimize` **之前**，否则"最小化"就会被 show 抹掉。
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    match app.get_webview_window("main") {
        Some(window) => {
            let was_on_screen = window.is_visible().unwrap_or(true)
                && !window.is_minimized().unwrap_or(false);
            if !was_on_screen {
                if let Err(error) = app.emit(crate::commands::EVENT_MAIN_WINDOW_SHOWN, ()) {
                    // 发不出去最多是"还停在上次那一页"，不该拦住窗口显示。
                    log::warn!("无法通知界面切回播放页：{error}");
                }
            }
            // 先 show 再 unminimize：窗口被最小化过时，只 show 不会把它带到前台。
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
        None => log::error!("找不到主窗口，无法显示"),
    }
}

/// 暂停/继续轮播。未在轮播时则开始。
///
/// 菜单上的勾由 [`spawn_check_refresher`] 按真实状态维护，因此这里只管做事。
fn toggle_rotation<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    if !state.playback.is_running() {
        let settings = state.settings();
        match state.playback.start(&settings, true) {
            Ok(()) => log::info!("托盘：已开始轮播"),
            Err(error) => log::error!("托盘：开始轮播失败：{error}"),
        }
    } else if state.playback.is_paused() {
        match state
            .playback
            .send(crate::playback_manager::PlaybackCommand::Resume)
        {
            Ok(()) => log::info!("托盘：已继续轮播"),
            Err(error) => log::error!("托盘：继续轮播失败：{error}"),
        }
    } else {
        match state
            .playback
            .send(crate::playback_manager::PlaybackCommand::Pause)
        {
            Ok(()) => log::info!("托盘：已暂停轮播"),
            Err(error) => log::error!("托盘：暂停轮播失败：{error}"),
        }
    }
}

/// 换一张（作用于所有屏）。
fn next_wallpaper<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    match state
        .playback
        .send(crate::playback_manager::PlaybackCommand::Next)
    {
        Ok(()) => log::info!("托盘：已请求换一张"),
        Err(error) => log::warn!("托盘：换一张失败：{error}"),
    }
}

/// 退出程序：先停掉轮播，再退出。
///
/// **不还原壁纸**（用户明确要求删掉那个行为），因此退出后桌面上留着的就是
/// 程序最后换上的那张。
fn exit_app<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<AppState>();
    log::info!("托盘：请求退出");
    // 退出前把窗口大小记下来：用户可能刚调整过，而去抖线程还没到点写盘。
    if let Some(window) = app.get_webview_window("main") {
        crate::window_state::save_now(&window, &state.paths.data_dir());
    }
    // 先停调度线程，避免它正在换纸时进程被拆掉（那会留下一个半截文件）。
    state.playback.stop();
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::{classify_click, rotation_checked, ClickKind};

    #[test]
    fn the_rotation_check_follows_running_and_paused() {
        // 正在轮播 → 打勾。
        assert!(rotation_checked(true, false));
        // **暂停时调度线程还活着**：只看"有没有线程"会把已暂停显示成正在轮播。
        assert!(!rotation_checked(true, true), "暂停了就不该打勾");
        // 没在跑（含 NSFW 模式、退出 NSFW 模式前的空档）→ 不打勾。
        assert!(!rotation_checked(false, false));
        // "没在跑 + 暂停标志还在"这种组合也该是不打勾（先前那次暂停留下的尾巴）。
        assert!(!rotation_checked(false, true));
    }

    #[test]
    fn a_lone_click_is_a_single_click() {
        // 第一次点击（`u64::MAX` = 还没有过上一次）。
        assert_eq!(classify_click(10_000, u64::MAX, u64::MAX), ClickKind::Single);
        // 两次点击隔得很远（远超 300 ms）→ 还是两次单击。
        assert_eq!(classify_click(10_000, 9_000, u64::MAX), ClickKind::Single);
        // 边界：正好一个窗口（300 ms）**不算**双击（用 `<` 而不是 `<=`）。
        assert_eq!(classify_click(10_300, 10_000, u64::MAX), ClickKind::Single);
    }

    #[test]
    fn two_close_clicks_are_a_double_click() {
        // 相距 120 ms → 用户是在双击，双击要打开主界面、并且**不能**换纸。
        assert_eq!(classify_click(10_120, 10_000, u64::MAX), ClickKind::Double);
        // 边界内侧。
        assert_eq!(classify_click(10_299, 10_000, u64::MAX), ClickKind::Double);
    }

    #[test]
    fn a_click_right_after_a_double_click_is_ignored() {
        // Windows 对一次双击会给不止两条消息，尾巴那一下必须丢掉，
        // 否则"双击打开界面"之后会顺带换一张壁纸。
        assert_eq!(classify_click(10_150, 10_140, 10_100), ClickKind::Ignore);
        // 双击窗口过去之后，下一次点击仍按单击处理（用户下一轮操作）。
        assert_eq!(classify_click(10_500, u64::MAX, 10_100), ClickKind::Single);
        // 注意这一条：那次"下一轮点击"如果又紧跟在上一次单击之后（相隔 < 300 ms），
        // 它仍然算双击——因为从事件流看，那就是用户连点了两下。
        assert_eq!(classify_click(10_500, 10_400, 10_100), ClickKind::Double);
    }
}

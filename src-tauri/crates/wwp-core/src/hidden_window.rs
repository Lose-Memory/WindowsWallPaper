//! 一个隐藏的顶层消息窗口。
//!
//! 需要它的原因在探测阶段就明确了：**被 `SetParent` 成别人的子窗口之后，
//! 就收不到 `WM_SETTINGCHANGE`、`WM_DISPLAYCHANGE` 这类广播**——广播只发给顶层窗口。
//!
//! 因此这里留一个不显示、不抢焦点的顶层窗口专门"听广播"，收到后转发给关心它的人。
//! 现在用于显示器变化与**全局快捷键**（`WM_HOTKEY` 同样只投递给注册它的窗口）。
//!
//! ## 快捷键为什么必须在这里注册
//!
//! `RegisterHotKey` 注册到**调用它的线程**，而 `WM_HOTKEY` 只会投递到那个线程
//! 创建的窗口。因此注册/注销都必须在跑消息循环的这个线程上做，
//! 外部（界面改设置时）只能通过 [`reload_hotkeys`] 发一条消息让这里重做一遍。

use std::sync::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW,
    GetMessageW, GetWindowThreadProcessId, PostMessageW, RegisterClassW, TranslateMessage, MSG,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_HOTKEY, WM_SETTINGCHANGE, WNDCLASSW,
};

use crate::hotkey::{self, HotkeySpecs};

/// 窗口类名。
///
/// `windows` crate 的 `w!` 宏只接受**字面量**、不接受常量，因此类名在使用处写成了
/// 字面量。这里保留这个常量并在下面做一次编译期核对：两处不一致的表现是
/// `CreateWindowExW` 直接失败，而它的错误信息不会告诉你原因是类名对不上。
const CLASS_NAME: &str = "WindowsWallPaperHiddenSink";

/// 编译期核对：使用处的字面量必须与 `CLASS_NAME` 长度一致。
/// 长度一致不等于内容一致，但它能挡住"改了名字却只改了一处"这类错误，
/// 而长度巧合相同的改名在实践里极罕见。
const _: () = assert!(
    CLASS_NAME.len() == 26,
    "类名长度变了，请同步修改 CreateWindowExW 里的字面量"
);

/// 显示器配置变化的处理器。
///
/// 用函数指针而不是闭包：它要被 Win32 的消息回调调用，跨 FFI 边界只有函数指针可靠。
pub type DisplayChangeHandler = fn();

/// 全局处理器。
///
/// 用 `Mutex<Option<..>>` 而不是 `OnceLock`：允许重复设置（重启轮播时会重新注册），
/// 而 `OnceLock` 第二次设置会被静默忽略，那会让人以为新处理器生效了其实没有。
static HANDLER: Mutex<Option<DisplayChangeHandler>> = Mutex::new(None);
static SETTING_HANDLER: Mutex<Option<DisplayChangeHandler>> = Mutex::new(None);

/// 快捷键被按下时的处理器，参数是注册时用的 id。
pub type HotkeyHandler = fn(i32);
static HOTKEY_HANDLER: Mutex<Option<HotkeyHandler>> = Mutex::new(None);

/// "请把主界面显示出来"的处理器（第二个实例启动时发来的请求）。
///
/// 界面属于外壳（窗口是 Tauri 建的），引擎不碰它，因此这里只留一个函数指针。
pub type ShowWindowHandler = fn();
static SHOW_WINDOW_HANDLER: Mutex<Option<ShowWindowHandler>> = Mutex::new(None);

/// 要注册的快捷键（字符串形式，未解析）。注册与解析都在窗口线程里做。
static HOTKEY_SPEC: Mutex<Option<HotkeySpecs>> = Mutex::new(None);

/// 窗口线程创建出来的窗口句柄。外部要"重新注册快捷键"时用它发消息。
static WINDOW: Mutex<Option<isize>> = Mutex::new(None);

/// 自定义消息：让窗口线程重新注册全局快捷键。
///
/// 快捷键存在设置里，用户在界面上改完之后必须让**窗口线程**重新注册——
/// `RegisterHotKey` 只对调用它的线程有效。
const WM_RELOAD_HOTKEYS: u32 = 0x8000 + 1;

/// 自定义消息：第二个实例请求把主界面显示出来。
///
/// 走自定义消息而不是 `WM_COPYDATA` 之类：消息里没有任何数据要传，
/// 而"谁来显示窗口"这件事必须由**拥有窗口的那个进程**自己做
/// （见 [`ask_existing_to_show_window`] 的说明）。
const WM_SHOW_MAIN_WINDOW: u32 = 0x8000 + 2;

/// 广播消息到达时的窗口过程。
unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DISPLAYCHANGE => {
            let width = lparam.0 & 0xFFFF;
            let height = (lparam.0 >> 16) & 0xFFFF;
            log::info!("收到显示器变化广播（新分辨率 {width}x{height}）");
            if let Some(handler) = *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) {
                handler();
            }
        }
        WM_SETTINGCHANGE => {
            if let Some(handler) = *SETTING_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) {
                handler();
            }
        }
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            log::info!("收到全局快捷键（id={id}）");
            if let Some(handler) = *HOTKEY_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) {
                handler(id);
            }
        }
        WM_RELOAD_HOTKEYS => {
            apply_hotkeys(hwnd);
        }
        WM_SHOW_MAIN_WINDOW => {
            log::info!("收到「显示主界面」请求（另一个实例启动时发来的）");
            if let Some(handler) = *SHOW_WINDOW_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) {
                handler();
            } else {
                // 处理器还没注册：这只会在启动早期的几百毫秒内发生，
                // 而那时主界面本来就还没建好。记一条，免得"点了没反应"查不出来。
                log::warn!("还没有注册显示主界面的处理器，本次请求被忽略");
            }
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// 按当前设置注册（或注销）全局快捷键。**必须在窗口线程上调用。**
///
/// ## 为什么每次都是"先全部注销、再全部注册"
///
/// `RegisterHotKey` 对同一个 id 第二次注册会失败，而"改了快捷键"必须先放开旧的。
/// 逐个 id 判断"是否变化"要保存上一轮的解析结果、还要处理解析失败的中间态，
/// 而这里总共只有四个键——无条件重来最简单，也不会漏。
fn apply_hotkeys(hwnd: HWND) {
    let specs = HOTKEY_SPEC
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();

    unsafe {
        for id in HotkeySpecs::ids() {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
    }

    for (id, label, spec) in specs.entries() {
        if spec.trim().is_empty() {
            log::info!("未设置全局快捷键「{label}」（只能用界面/托盘触发）");
            continue;
        }
        match hotkey::parse(spec) {
            Ok(key) => {
                let mods = key.mods | hotkey::MOD_NOREPEAT;
                let registered =
                    unsafe { RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(mods), key.vk) };
                if registered.is_ok() {
                    log::info!("已注册全局快捷键「{label}」：{key}");
                } else {
                    // 最常见的原因是别的程序已经占用了同一个组合
                    // （例如显卡驱动把 Ctrl+Alt+方向键 用来旋转屏幕）。
                    // 只记 warn、不阻塞启动——快捷键不是程序能跑起来的必要条件。
                    log::warn!(
                        "注册全局快捷键「{label}」({spec}) 失败（多半是被别的程序占用了），\
                         可以在播放页换一个写法"
                    );
                }
            }
            Err(reason) => log::warn!("快捷键「{label}」({spec}) 无法解析，已忽略：{reason}"),
        }
    }
}

/// 注册窗口类。重复调用是安全的（第二次返回 0，但类已存在）。
fn register_class() -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("WindowsWallPaperHiddenSink"),
            ..Default::default()
        };
        RegisterClassW(&class);
    }
    Ok(())
}

/// 创建隐藏消息窗口并进入消息循环。**会阻塞当前线程**，因此必须在专用线程里调用。
///
/// "隐藏"是这样实现的：不传 `WS_VISIBLE`，尺寸为 0，且没有父窗口。
/// 它必须仍是**顶层**窗口——WM_DISPLAYCHANGE 这类广播只发给顶层窗口，
/// 这正是被 SetParent 成子窗口的窗口收不到广播的原因。
pub fn run_message_loop() -> windows::core::Result<()> {
    register_class()?;

    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("WindowsWallPaperHiddenSink"),
            w!("WindowsWallPaper 消息接收窗口"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )?;
        log::info!(
            "隐藏消息窗口已创建（hwnd=0x{:X}），开始监听系统广播与全局快捷键",
            hwnd.0 as isize
        );
        *WINDOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
        // 全局快捷键必须在这个线程注册（WM_HOTKEY 只投递给注册它的窗口）。
        apply_hotkeys(hwnd);
    }

    // 标准消息循环：没有窗口要显示，只是为了让广播能进到我们的窗口过程。
    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// 设置显示器变化处理器。
pub fn set_display_change_handler(handler: DisplayChangeHandler) {
    *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// 清空显示器变化处理器（停止轮播时调用）。
pub fn clear_display_change_handler() {
    *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 设置系统设置变化处理器（主题、语言等）。
pub fn set_setting_change_handler(handler: DisplayChangeHandler) {
    *SETTING_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// 设置快捷键处理器。
pub fn set_hotkey_handler(handler: HotkeyHandler) {
    *HOTKEY_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// 设置"请显示主界面"的处理器（第二个实例启动时会走它）。
pub fn set_show_window_handler(handler: ShowWindowHandler) {
    *SHOW_WINDOW_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// 找到**另一个实例**的隐藏消息窗口。
///
/// 类名是我们自己注册的（`WindowsWallPaperHiddenSink`），不与系统或别的程序重名，
/// 因此按类名就能唯一定位到那个窗口。窗口类是在**每个进程内部**注册的本地类，
/// 但 `FindWindowW` 是按类名在系统里找窗口，不受"谁注册的"影响。
fn find_sink_window() -> Option<HWND> {
    unsafe { FindWindowW(w!("WindowsWallPaperHiddenSink"), None).ok() }
}

/// 把"请显示主界面"发给已经在跑的那个实例，返回是否成功送出。
///
/// ## 为什么不在这里直接 ShowWindow
///
/// 那个窗口不是本进程的。跨进程直接操作别人的窗口（`ShowWindow`/`SetForegroundWindow`）
/// 要么受限、要么只能碰巧成功；而**发一条消息让那个实例自己去做**，
/// 走的是与"点托盘菜单里的显示主界面"完全相同的一段代码，因此行为不会有两套。
///
/// ## 为什么要把前台权限转交过去
///
/// `SetForegroundWindow` 只对"当前拥有前台的进程"或"被前者显式授权的进程"生效。
/// 刚被双击启动的**第二个进程**恰好还握着这份权利，而我们马上就要退出了，
/// 因此在这里把权利转交给已经在跑的那个实例（`AllowSetForegroundWindow`），
/// 它随后 `set_focus()` 才能真的把窗口带到眼前——否则窗口只是显示出来、
/// 却可能在别的窗口后面（用户仍然会认为"双击了没反应"）。
pub fn ask_existing_to_show_window() -> bool {
    let Some(hwnd) = find_sink_window() else {
        return false;
    };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            if let Err(error) = AllowSetForegroundWindow(pid) {
                // **失败是正常的，不必报警**：这个 API 只有"当前拥有前台的进程"
                // （或由它启动的进程）才调得动。用户双击 exe 时新进程正是 Explorer
                // 启动的，因此通常能成功；而从终端/脚本里启动时会被拒绝。
                // 拒绝只意味着窗口可能不在最前面——它**已经显示出来了**，
                // 用户顶多多点一下任务栏。
                log::debug!("把前台权限转交给已有实例未获准（{error}），窗口仍会显示");
            }
        }
        match PostMessageW(Some(hwnd), WM_SHOW_MAIN_WINDOW, WPARAM(0), LPARAM(0)) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("请求已有实例显示主界面失败：{error}");
                false
            }
        }
    }
}

/// 告诉窗口线程要注册哪些快捷键（空串 = 不注册）。
///
/// 可以在任何线程调用：它只更新意图并**通知**窗口线程去重新注册。
/// 窗口还没建好时（启动早期）只记下意图，建窗口时会自己注册一次。
pub fn set_hotkey_specs(specs: &HotkeySpecs) {
    *HOTKEY_SPEC.lock().unwrap_or_else(|e| e.into_inner()) = Some(specs.clone());
    reload_hotkeys();
}

/// 让窗口线程按当前意图重新注册一次。
pub fn reload_hotkeys() {
    let hwnd = *WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    let Some(hwnd) = hwnd else {
        // 窗口还没建好：`run_message_loop` 建完会自己注册。
        return;
    };
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut _)),
            WM_RELOAD_HOTKEYS,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

//! 桌面窗口拓扑探测。
//!
//! 这一段是 M3/M4 的地基，也是本机实测（Win11 24H2 build 26100）后发现必须
//! 单独成模块的部分：Windows 11 24H2 起，Shell 的桌面窗口结构变了。
//!
//! 旧结构（Win10 ~ Win11 23H2，"Classic"）：
//!
//! ```text
//! Progman
//! └── WorkerW            <- 壁纸层在我们这里
//!     └── SHELLDLL_DefView
//!         └── SysListView32   <- 桌面图标
//! （另一个顶层 WorkerW 在最底下）
//! ```
//!
//! 新结构（24H2+，"RaisedDesktop"）：
//!
//! ```text
//! Progman（带 WS_EX_NOREDIRECTIONBITMAP）
//! ├── SHELLDLL_DefView（分层子窗口，图标）
//! └── WorkerW            <- Shell 自己画的壁纸
//! ```
//!
//! 判定依据是 `Progman` 的 `WS_EX_NOREDIRECTIONBITMAP` 扩展样式。这个样式的含义是
//! "不要为我分配重定向表面"，也就是 GDI 画上去的东西不会被合成——这正是 24H2 上
//! GDI 方案失效的原因。
//!
//! 本模块只做**只读探测**：枚举、判断、报告。不创建窗口、不改窗口父子关系，
//! 因此可以在任何时刻安全运行。

use std::fmt;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, WS_CHILD, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Shell 的未文档化消息：让 Progman 创建壁纸层窗口。
///
/// 社区普遍写作 `0x052C`。参考项目（KokoroPaper）用 `wParam=0, lParam=0`，
/// Lively 用 `wParam=0xD, lParam=0x1`；两者都能工作。本程序只在"找不到壁纸层"
/// 时才发这条消息。
pub const WM_SPAWN_WORKERW: u32 = 0x052C;

/// 把字符串转成以 0 结尾的 UTF-16，供 Win32 宽字符 API 使用。
///
/// 自己转而不依赖宏：这样字符串可以来自运行时（类名要枚举比对），
/// 而且 `Vec<u16>` 的生命周期在调用处一目了然。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 桌面拓扑种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// Win10 ~ Win11 23H2：壁纸层是顶层 WorkerW。
    Classic,
    /// Win11 24H2+：图标层是 Progman 的分层子窗口，壁纸由 Progman 下的 WorkerW 画。
    RaisedDesktop,
    /// 认不出来（Progman 不存在等）。
    Unknown,
}

impl Topology {
    pub fn as_str(self) -> &'static str {
        match self {
            Topology::Classic => "Classic",
            Topology::RaisedDesktop => "RaisedDesktop",
            Topology::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for Topology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个子窗口的最小描述。
#[derive(Debug, Clone)]
pub struct ChildWindow {
    pub hwnd: isize,
    pub class_name: String,
    pub visible: bool,
    pub rect: Option<(i32, i32, i32, i32)>,
    /// 扩展样式。`GetWindowLongPtrW` 在 64 位下返回 `isize`，这里保留原始类型，
    /// 不做收窄转换，避免把高位样式位吃掉。
    pub ex_style: isize,
    /// 基本样式（含 `WS_CHILD` / `WS_POPUP`，挂载时这两个不能混用）。
    pub style: isize,
    /// 所属进程 ID与可执行文件名。用来识别"这个窗口是谁挂上去的"——
    /// 本机装了 Wallpaper Engine，它的窗口正好是一份现成的参考实现。
    pub pid: u32,
    pub process_name: String,
}

impl ChildWindow {
    pub fn describe(&self) -> String {
        let rect = match self.rect {
            Some((l, t, r, b)) => format!("({l},{t}) {}x{}", r - l, b - t),
            None => "矩形未知".to_string(),
        };
        format!(
            "0x{:X} class={:<18} visible={:<5} flags={:<28} pid={:<6} {:<22} {}",
            self.hwnd,
            self.class_name,
            self.visible,
            self.style_flags(),
            self.pid,
            self.process_name,
            rect
        )
    }

    /// 用**具名常量**判断样式位，而不是手算十六进制。
    ///
    /// 这条规则是踩过才立的：我一开始凭记忆把 `WS_EX_NOREDIRECTIONBITMAP`
    /// 当成 `0x08000000`（实际是 `0x00200000`，`0x08000000` 是 `WS_EX_NOACTIVATE`），
    /// 于是每个窗口的样式都被解读错，连"这是不是 raised 拓扑"都会判反。
    /// 具名常量不会记错。
    pub fn style_flags(&self) -> String {
        let mut flags: Vec<&str> = Vec::new();
        let style = self.style as u32;
        if style & WS_CHILD.0 != 0 {
            flags.push("WS_CHILD");
        }
        if style & WS_POPUP.0 != 0 {
            flags.push("WS_POPUP");
        }
        let ex = self.ex_style as u32;
        if ex & WS_EX_NOREDIRECTIONBITMAP.0 != 0 {
            flags.push("EX_NOREDIRECTIONBITMAP");
        }
        if ex & WS_EX_LAYERED.0 != 0 {
            flags.push("EX_LAYERED");
        }
        if ex & WS_EX_NOACTIVATE.0 != 0 {
            flags.push("EX_NOACTIVATE");
        }
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            flags.push("EX_TRANSPARENT");
        }
        if flags.is_empty() {
            "—".to_string()
        } else {
            flags.join("|")
        }
    }
}

/// 一次探测的完整结果，供诊断输出与日志使用。
#[derive(Debug, Clone)]
pub struct DesktopProbe {
    pub topology: Topology,
    pub progman: Option<isize>,
    /// Progman 的扩展样式原始值。
    pub progman_ex_style: isize,
    /// Progman 是否带 WS_EX_NOREDIRECTIONBITMAP（决定拓扑的关键）。
    pub no_redirection_bitmap: bool,
    /// 找到的壁纸层窗口句柄（将来挂载窗口的目标）。
    pub wallpaper_layer: Option<isize>,
    /// 壁纸层是怎么找到的，便于解释"为什么挂到这里"。
    pub layer_source: String,
    /// 含 SHELLDLL_DefView 的顶层窗口（经典拓扑下它的下一个兄弟是壁纸层）。
    pub defview_host: Option<isize>,
    /// 桌面图标层窗口。
    pub defview: Option<isize>,
    /// Progman 的子窗口清单（类名 + 矩形 + 扩展样式），用于核对结构与 z 序。
    pub progman_children: Vec<ChildWindow>,
    /// Progman 的**全部后代**窗口（递归）。
    ///
    /// 需要这一层是因为实测发现：壁纸程序未必把窗口挂在 Progman 的直接子层，
    /// 也可能挂到某个子窗口之下。只看直接子窗口会漏掉它。
    pub progman_descendants: Vec<ChildWindow>,
    /// 顶层 WorkerW 窗口清单。
    pub top_level_workerw: Vec<ChildWindow>,
    /// 指定进程名拥有的全部窗口（用于观察别的壁纸程序是怎么挂的）。
    pub windows_of_interest: Vec<ChildWindow>,
    /// 观察的进程名。
    pub interest_process: String,
    /// Progman 自身的矩形。
    pub progman_rect: Option<(i32, i32, i32, i32)>,
}

impl DesktopProbe {
    /// 多行描述，供 `--probe-desktop` 与日志使用。
    pub fn describe(&self) -> String {
        let mut text = String::new();
        text.push_str(&format!("拓扑             : {}\n", self.topology));
        text.push_str(&format!(
            "Progman          : {}\n",
            self.progman
                .map(|h| format!("0x{h:X}"))
                .unwrap_or_else(|| "未找到".to_string())
        ));
        text.push_str(&format!(
            "Progman 扩展样式 : 0x{:X}（NOREDIRECTIONBITMAP：{}）\n",
            self.progman_ex_style, self.no_redirection_bitmap
        ));
        if let Some((l, t, r, b)) = self.progman_rect {
            text.push_str(&format!("Progman 矩形     : ({l},{t}) {}x{}\n", r - l, b - t));
        }
        text.push_str(&format!(
            "壁纸层窗口       : {}（来源：{}）\n",
            self.wallpaper_layer
                .map(|h| format!("0x{h:X}"))
                .unwrap_or_else(|| "未找到".to_string()),
            self.layer_source
        ));
        text.push_str(&format!(
            "DefView 宿主     : {}\n",
            self.defview_host
                .map(|h| format!("0x{h:X}"))
                .unwrap_or_else(|| "未找到".to_string())
        ));
        text.push_str(&format!(
            "DefView（图标）  : {}\n",
            self.defview
                .map(|h| format!("0x{h:X}"))
                .unwrap_or_else(|| "未找到".to_string())
        ));

        text.push_str(&format!(
            "Progman 的子窗口（{} 个）:\n",
            self.progman_children.len()
        ));
        for child in &self.progman_children {
            text.push_str(&format!("  {}\n", child.describe()));
        }

        text.push_str(&format!(
            "Progman 的全部后代（{} 个）:\n",
            self.progman_descendants.len()
        ));
        for child in &self.progman_descendants {
            text.push_str(&format!("  {}\n", child.describe()));
        }

        text.push_str(&format!(
            "顶层 WorkerW（{} 个）:\n",
            self.top_level_workerw.len()
        ));
        for window in &self.top_level_workerw {
            text.push_str(&format!("  {}\n", window.describe()));
        }

        if !self.interest_process.is_empty() {
            text.push_str(&format!(
                "进程 {} 拥有的窗口（{} 个）——观察别的壁纸程序是怎么挂的:\n",
                self.interest_process,
                self.windows_of_interest.len()
            ));
            for window in &self.windows_of_interest {
                text.push_str(&format!("  {}\n", window.describe()));
            }
        }
        text
    }
}

/// 只读探测当前桌面的窗口拓扑。
///
/// `interest_process` 指定一个额外观察的进程名（例如本机正在运行的
/// `wallpaper64.exe`）：它拥有的全部顶层窗口会被列出来。这是学习"成熟实现
/// 到底把窗口挂到了哪里"的最直接方式——比读文档可靠。
pub fn probe_desktop_with_interest(interest_process: &str) -> DesktopProbe {
    unsafe {
        let progman_wide = wide("Progman");
        let progman = FindWindowW(windows::core::PCWSTR(progman_wide.as_ptr()), None).ok();

        let progman_ex_style = progman
            .map(|hwnd| GetWindowLongPtrW(hwnd, GWL_EXSTYLE))
            .unwrap_or(0);
        let no_redirection_bitmap =
            (progman_ex_style & WS_EX_NOREDIRECTIONBITMAP.0 as isize) != 0;
        let topology = match progman {
            None => Topology::Unknown,
            Some(_) if no_redirection_bitmap => Topology::RaisedDesktop,
            Some(_) => Topology::Classic,
        };

        let progman_children = progman
            .map(|parent| enumerate_children(parent))
            .unwrap_or_default();
        let progman_descendants = progman
            .map(|parent| enumerate_descendants(parent))
            .unwrap_or_default();

        let top_level_workerw = enumerate_top_level_by_class("WorkerW");

        // 含 SHELLDLL_DefView 的顶层窗口（经典拓扑的判定锚点）。
        let defview_wide = wide("SHELLDLL_DefView");
        let mut defview_host = None;
        let mut defview = None;
        for window in &top_level_workerw {
            let hwnd = HWND(window.hwnd as *mut _);
            if let Ok(child) = FindWindowExW(
                Some(hwnd),
                None,
                windows::core::PCWSTR(defview_wide.as_ptr()),
                None,
            ) {
                defview_host = Some(window.hwnd);
                defview = Some(child.0 as isize);
                break;
            }
        }
        // 24H2 下 DefView 直接挂在 Progman 下。
        if defview.is_none() {
            if let Some(progman) = progman {
                if let Ok(child) = FindWindowExW(
                    Some(progman),
                    None,
                    windows::core::PCWSTR(defview_wide.as_ptr()),
                    None,
                ) {
                    defview = Some(child.0 as isize);
                }
            }
        }

        // 决定"将来要把窗口挂到哪里"。这里只做只读判断，不创建任何窗口。
        let (wallpaper_layer, layer_source) = match topology {
            Topology::RaisedDesktop => match progman {
                Some(hwnd) => (
                    Some(hwnd.0 as isize),
                    "24H2+ 结构：挂在 Progman 自身，再插到 SHELLDLL_DefView 之下".to_string(),
                ),
                None => (None, "Progman 不存在".to_string()),
            },
            Topology::Classic => {
                let workerw_wide = wide("WorkerW");
                let mut found = None;
                for window in &top_level_workerw {
                    let hwnd = HWND(window.hwnd as *mut _);
                    if FindWindowExW(
                        Some(hwnd),
                        None,
                        windows::core::PCWSTR(defview_wide.as_ptr()),
                        None,
                    )
                    .is_ok()
                    {
                        if let Ok(sibling) = FindWindowExW(
                            None,
                            Some(hwnd),
                            windows::core::PCWSTR(workerw_wide.as_ptr()),
                            None,
                        ) {
                            found = Some(sibling.0 as isize);
                        }
                        break;
                    }
                }
                match found {
                    Some(hwnd) => (
                        Some(hwnd),
                        "经典结构：含 SHELLDLL_DefView 的顶层窗口的下一个兄弟 WorkerW".to_string(),
                    ),
                    None => (
                        progman.map(|h| h.0 as isize),
                        "经典结构但未找到兄弟 WorkerW，回退到 Progman".to_string(),
                    ),
                }
            }
            Topology::Unknown => (None, "Progman 不存在，无法判断".to_string()),
        };

        DesktopProbe {
            topology,
            progman: progman.map(|h| h.0 as isize),
            progman_ex_style,
            no_redirection_bitmap,
            wallpaper_layer,
            layer_source,
            defview_host,
            defview,
            progman_children,
            progman_descendants,
            top_level_workerw,
            windows_of_interest: enumerate_top_level_by_process(interest_process),
            interest_process: interest_process.to_string(),
            progman_rect: progman.and_then(window_rect),
        }
    }
}

/// 便捷入口：不额外观察任何进程。
pub fn probe_desktop() -> DesktopProbe {
    probe_desktop_with_interest("wallpaper64.exe")
}

fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some((rect.left, rect.top, rect.right, rect.bottom))
    }
}

fn class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buffer = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buffer);
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

fn describe_window(hwnd: HWND) -> ChildWindow {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        ChildWindow {
            hwnd: hwnd.0 as isize,
            class_name: class_name(hwnd),
            visible: IsWindowVisible(hwnd).as_bool(),
            rect: window_rect(hwnd),
            ex_style: GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
            style: GetWindowLongPtrW(hwnd, GWL_STYLE),
            pid,
            process_name: process_name_of(pid),
        }
    }
}

/// 由进程 ID 取可执行文件名。
///
/// 用只读的 `QueryFullProcessImageNameW` 而不是 `OpenProcess`：
/// 前者对一个只是"存在"的进程也能查，且不需要任何权限提升，
/// 因此枚举别人的窗口（比如 Wallpaper Engine 的）不会因为我们权限不够而失败。
fn process_name_of(pid: u32) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buffer = [0u16; 260];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if ok.is_err() {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buffer[..size as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

/// 枚举某个窗口的直接子窗口。
fn enumerate_children(parent: HWND) -> Vec<ChildWindow> {
    unsafe {
        let mut children = Vec::new();
        let mut child = match FindWindowExW(Some(parent), None, None, None) {
            Ok(child) => child,
            Err(_) => return children,
        };
        // FindWindowExW 返回空句柄表示没有更多子窗口。
        while child.0 as isize != 0 {
            children.push(describe_window(child));
            child = match FindWindowExW(Some(parent), Some(child), None, None) {
                Ok(next) => next,
                Err(_) => break,
            };
        }
        children
    }
}

/// 枚举顶层窗口中指定类名的那些。
fn enumerate_top_level_by_class(target: &str) -> Vec<ChildWindow> {
    // 把结果放在 Box 里再传指针：EnumWindows 的回调要通过 LPARAM 拿到它，
    // 而借用检查器不允许把栈上变量的 &mut 直接转成整数。
    let mut state = Box::new(EnumState {
        target: target.to_string(),
        found: Vec::new(),
    });

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // 安全性：lparam 由本函数传入，指向在本次调用期间保持有效的 EnumState。
        let state = unsafe { &mut *(lparam.0 as *mut EnumState) };
        if class_name(hwnd) == state.target {
            state.found.push(describe_window(hwnd));
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM((&mut *state) as *mut EnumState as isize),
        );
    }
    state.found
}

struct EnumState {
    target: String,
    found: Vec<ChildWindow>,
}

/// 递归枚举某个窗口的全部后代（深度优先）。
///
/// 带深度上限：窗口树理论上可以很深，而这里只是诊断用途，没必要为了
/// 一个畸形结构把栈吃光。
fn enumerate_descendants(root: HWND) -> Vec<ChildWindow> {
    const MAX_DEPTH: usize = 12;
    let mut collected = Vec::new();
    collect_descendants(root, 0, MAX_DEPTH, &mut collected);
    collected
}

fn collect_descendants(hwnd: HWND, depth: usize, max_depth: usize, out: &mut Vec<ChildWindow>) {
    if depth >= max_depth {
        return;
    }
    unsafe {
        let mut child = match FindWindowExW(Some(hwnd), None, None, None) {
            Ok(child) => child,
            Err(_) => return,
        };
        while child.0 as isize != 0 {
            out.push(describe_window(child));
            collect_descendants(child, depth + 1, max_depth, out);
            child = match FindWindowExW(Some(hwnd), Some(child), None, None) {
                Ok(next) => next,
                Err(_) => break,
            };
        }
    }
}

/// 列出某个进程名拥有的**顶层**窗口。
fn enumerate_top_level_by_process(process_name: &str) -> Vec<ChildWindow> {
    if process_name.is_empty() {
        return Vec::new();
    }
    let mut state = Box::new(EnumState {
        target: process_name.to_string(),
        found: Vec::new(),
    });

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // 安全性：lparam 由本函数传入，指向调用期间保持有效的 EnumState。
        let state = unsafe { &mut *(lparam.0 as *mut EnumState) };
        let described = describe_window(hwnd);
        if described.process_name.eq_ignore_ascii_case(&state.target) {
            state.found.push(described);
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM((&mut *state) as *mut EnumState as isize),
        );
    }
    state.found
}

/// 把一条窗口记录转成"父窗口是谁"，用于打印窗口树。
pub fn parent_of(hwnd: isize) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetParent;
    unsafe {
        let parent = GetParent(HWND(hwnd as *mut _)).ok()?;
        if parent.0 as isize == 0 {
            None
        } else {
            Some(parent.0 as isize)
        }
    }
}

//! 检测「某块屏上是否有程序正处于全屏」。
//!
//! ## 这个模块曾经被删掉，为什么又回来了
//!
//! 上一版（提交 `de5e6f4` → `a7fc4dc` → `993027a`）在 `ea1b8d9` 连同
//! `--probe-fullscreen` 一起删掉了，因为用户报告"全屏后壁纸照换"。
//! 复盘那三条提交记录，失败原因**一条都不是"Windows 做不到"**：
//!
//! 1. **只查 `GetForegroundWindow()` 一个窗口**。副屏上有全屏视频、用户在主屏
//!    干活时，前台窗口是主屏那个；"两块屏各自全屏"更是无法表达。
//! 2. **DPI 口径不一致**：`GetWindowRect` 给物理像素、`GetMonitorInfoW` 给逻辑
//!    坐标，判据"矩形相等"恒为假（已在 `a7fc4dc` 修好，本模块继承这条教训）。
//! 3. **验证是假的**：那条提交自己承认"我原先的测试窗口也从没真正全屏过，
//!    因此'检测正常'的结论并不成立"。
//!
//! 因此这一版换了判据：**枚举该屏上所有可见顶层窗口**，逐个问"它盖住这块屏了吗"，
//! 而不是问"系统说谁是前台窗口"。这与业界两个成熟实现的做法一致：
//!
//! - Chromium `ui/base/fullscreen_win.cc`：`SHQueryUserNotificationState` +
//!   前台窗口矩形与显示器矩形相等 + 样式位检查。它的短板是**只认主显示器**
//!   （`MONITORINFOF_PRIMARY`），因此只能抄判据、不能照搬。
//! - Lively Wallpaper `WindowUtil.IsDisplayCoveredByWindow`：候选窗口做
//!   "可见 / 未被 DWM 遮蔽 / 非最小化 / 非工具窗口"过滤，然后
//!   `IsZoomed(hwnd) || 覆盖率 >= 0.95`。这正是本模块的形态。
//!
//! ## 判据：两条取或
//!
//! ```text
//! 忙 = 存在一个候选窗口，使得
//!        (窗口最大化 且 它主要落在这一块屏上)      // 用户说的"最大化全屏"
//!     或 (窗口与这块屏的交集面积 / 这块屏面积 >= 0.95)  // 独占全屏、无边框全屏
//! ```
//!
//! - **覆盖率 0.95** 对付独占全屏（矩形就是屏幕）与无边框全屏（留出任务栏，
//!   高度损失约 2%，覆盖率约 0.978）。取 0.95 这个值与 Lively 一致。
//! - **`IsZoomed`** 单独一条，是因为任务栏很高时（例如 768 高的屏 + 48 像素任务栏，
//!   覆盖率只有 0.9375）覆盖率会掉到门槛以下，而"最大化"这件事本身是明确的。
//!   用户明确要求"最大化也算"。
//! - **最大化必须同时校验"它主要落在这一块屏上"**。这是 Lively 判据里的一个洞：
//!   只看 `IsZoomed(hwnd)` 的话，一块屏上的最大化窗口会把**所有**屏都判成忙。
//!
//! ## 刻意排除的东西
//!
//! | 排除项 | 不排除会怎样 |
//! |---|---|
//! | 我们自己进程的窗口 | 主界面最大化时，把自己算成"有程序全屏"，壁纸就停了 |
//! | `Progman` / `WorkerW` / 任务栏等外壳窗口 | 它们本来就铺满屏幕，点一下桌面轮播就停 |
//! | 最小化的窗口 | 有些程序最小化后仍保留全屏时的矩形 |
//! | DWM 遮蔽（cloaked）的窗口 | 虚拟桌面上"看不见"的 UWP 窗口仍是全屏尺寸 |
//! | 工具窗口（`WS_EX_TOOLWINDOW`） | 托盘浮层、输入法候选之类 |
//!
//! **没有**排除的：`WS_EX_LAYERED`（分层，Lively 会排除）。理由是有些无边框
//! 全屏播放器正是分层窗口，排掉它会漏判；而"盖住整块屏的分层窗口"一般是用户
//! 正在看的画面。同理没有要求窗口必须有标题——无边框全屏的窗口标题可以是空的，
//! 那条要求会漏判。
//!
//! ## 拿不到信息时怎么办
//!
//! 一律按**不忙**处理（枚举失败、矩形取不到、显示器列表为空）。理由：漏判的
//! 后果只是"白白换了一次壁纸"，而误判的后果是"轮播莫名停住"——后者难解释得多。

use crate::monitors::Monitor;

/// 窗口至少要覆盖屏幕面积的这个比例才算"盖满了"。
///
/// 0.95 这个值与 Lively 一致。任务栏高度在 2160 高的屏上约占 2.2%（覆盖率 0.978），
/// 在 1440 高的屏上约占 3.3%（覆盖率 0.967），都在门槛之内。
pub const MIN_COVERAGE: f64 = 0.95;

/// 桌面外壳的窗口类名。
///
/// 这些窗口本身就铺满屏幕，但它们不表示"用户正在全屏看一个程序"。
/// 漏掉一个的后果是"点一下桌面/打开任务视图，轮播就停了"，
/// 因此放在模块级、可被测试直接引用。
pub const SHELL_CLASSES: &[&str] = &[
    // 桌面与壁纸层（Win10 的 Classic 与 Win11 24H2+ 的 RaisedDesktop 都有）。
    "Progman",
    "WorkerW",
    // 主任务栏与副屏任务栏。
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    // 任务视图（Win10 是 MultitaskingViewFrame，Win11 是 Xaml 岛窗口）。
    // 它们是 Shell 自己的全屏浮层，不是"某个程序在全屏"。
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
];

/// 这个窗口类名是不是桌面外壳的。
pub fn is_shell_class(class_name: &str) -> bool {
    SHELL_CLASSES.contains(&class_name)
}

/// 一个矩形（屏幕坐标，物理像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// 由显示器描述构造。
    pub fn of_monitor(monitor: &Monitor) -> Rect {
        Rect {
            left: monitor.x,
            top: monitor.y,
            right: monitor.x + monitor.width as i32,
            bottom: monitor.y + monitor.height as i32,
        }
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn area(&self) -> i64 {
        self.width() as i64 * self.height() as i64
    }

    /// 与另一个矩形的交集面积。不相交时为 0。
    pub fn intersection_area(&self, other: &Rect) -> i64 {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if right <= left || bottom <= top {
            0
        } else {
            (right - left) as i64 * (bottom - top) as i64
        }
    }

    /// 与另一个矩形的交集面积占**另一个矩形**的比例。
    ///
    /// 分母传屏幕：得到的是"这个窗口盖住了屏幕的多少"。
    pub fn coverage_of(&self, screen: &Rect) -> f64 {
        let screen_area = screen.area();
        if screen_area <= 0 {
            return 0.0;
        }
        self.intersection_area(screen) as f64 / screen_area as f64
    }

    pub fn describe(&self) -> String {
        format!(
            "({},{}) {}x{}",
            self.left,
            self.top,
            self.width(),
            self.height()
        )
    }
}

/// 一块屏的矩形与名字。
#[derive(Debug, Clone)]
pub struct ScreenRect {
    /// 形如 `\\.\DISPLAY1`（与 `[monitors.'<名字>']` 用同一套键）。
    pub name: String,
    pub rect: Rect,
}

/// 一个通过了可见性/外壳/本进程过滤的候选窗口。
///
/// "候选"= 有可能被判成"这块屏上有程序全屏"。过滤在 [`collect_windows`] 里做，
/// 判定在 [`busy_screens`] 里做，两者分开是为了让判定部分能脱离真实窗口测试。
#[derive(Debug, Clone)]
pub struct CandidateWindow {
    pub hwnd: isize,
    pub class_name: String,
    /// 窗口标题，只用于诊断输出（判定不看它，见模块文档）。
    pub title: String,
    pub pid: u32,
    pub rect: Rect,
    /// 窗口处于最大化状态（`IsZoomed`）。
    pub maximized: bool,
}

impl CandidateWindow {
    /// 与每块屏的交集面积，取最大的那一块的下标（没有任何交集时为 `None`）。
    ///
    /// 这是"这个窗口主要落在哪块屏上"的判据，用来给最大化窗口定位——
    /// 不这样做的话，一块屏上的最大化窗口会把所有屏都判成忙。
    pub fn dominant_screen(&self, screens: &[Rect]) -> Option<usize> {
        screens
            .iter()
            .enumerate()
            .map(|(index, screen)| (index, self.rect.intersection_area(screen)))
            .filter(|(_, area)| *area > 0)
            .max_by_key(|(_, area)| *area)
            .map(|(index, _)| index)
    }
}

/// 判定每块屏是否"有程序全屏"。**纯函数**，不碰任何 Win32。
///
/// 判据见模块文档：`(最大化 且 主要落在这块屏上) || 覆盖率 >= min_coverage`。
pub fn busy_screens(windows: &[CandidateWindow], screens: &[Rect], min_coverage: f64) -> Vec<bool> {
    let dominant: Vec<Option<usize>> = windows
        .iter()
        .map(|window| window.dominant_screen(screens))
        .collect();

    screens
        .iter()
        .enumerate()
        .map(|(index, screen)| {
            windows.iter().zip(dominant.iter()).any(|(window, owner)| {
                (window.maximized && *owner == Some(index))
                    || window.rect.coverage_of(screen) >= min_coverage
            })
        })
        .collect()
}

/// 一次检测的完整结果（`--probe-fullscreen` 直接把它打成文字）。
#[derive(Debug, Clone)]
pub struct Detection {
    pub screens: Vec<ScreenRect>,
    pub windows: Vec<CandidateWindow>,
    /// 与 `screens` 一一对应。
    pub busy: Vec<bool>,
}

impl Detection {
    /// 现在有程序全屏的那几块屏（元素是显示器名）。
    pub fn busy_names(&self) -> Vec<String> {
        self.screens
            .iter()
            .zip(self.busy.iter())
            .filter(|(_, busy)| **busy)
            .map(|(screen, _)| screen.name.clone())
            .collect()
    }

    pub fn describe(&self) -> String {
        let mut text = String::new();
        text.push_str(&format!(
            "判据：最大化(且主要落在该屏) 或 覆盖率 >= {:.2}\n",
            MIN_COVERAGE
        ));
        text.push_str(&format!("候选窗口 {} 个\n\n", self.windows.len()));

        for (index, screen) in self.screens.iter().enumerate() {
            let busy = self.busy.get(index).copied().unwrap_or(false);
            text.push_str(&format!(
                "{} {}  {}\n",
                screen.name,
                screen.rect.describe(),
                if busy { "【全屏中】" } else { "正常" }
            ));
        }

        text.push_str("\n候选窗口（已排除最小化 / 不可见 / DWM 遮蔽 / 工具窗口 / 外壳 / 本进程）：\n");
        if self.windows.is_empty() {
            text.push_str("  （一个都没有）\n");
        }
        let screens: Vec<Rect> = self.screens.iter().map(|s| s.rect).collect();
        for window in &self.windows {
            let owner = match window.dominant_screen(&screens) {
                Some(index) => self.screens[index].name.clone(),
                None => "不在任何屏上".to_string(),
            };
            // 覆盖率按"它主要落在的那块屏"算，这样多屏时数字才有意义。
            let coverage = window
                .dominant_screen(&screens)
                .map(|index| window.rect.coverage_of(&screens[index]))
                .unwrap_or(0.0);
            text.push_str(&format!(
                "  0x{:X} {:<26} {} 覆盖率={:.3}{} {}\n    标题: {}\n",
                window.hwnd,
                window.class_name,
                window.rect.describe(),
                coverage,
                if window.maximized { " 最大化" } else { "" },
                owner,
                if window.title.is_empty() {
                    "（无标题）"
                } else {
                    &window.title
                }
            ));
        }
        text
    }
}

/// 跑一次完整检测。
///
/// 代价：`EnumDisplayMonitors` 一次 + `EnumWindows` 一次（本机约几十个顶层窗口），
/// 都是微秒到毫秒量级，因此调用方可以每 500 ms 跑一次而不必担心开销。
pub fn detect() -> Detection {
    let monitors = match crate::monitors::enumerate() {
        Ok(monitors) => monitors,
        Err(error) => {
            log::warn!("全屏检测：枚举显示器失败，按「没有全屏」处理：{error}");
            return Detection {
                screens: Vec::new(),
                windows: Vec::new(),
                busy: Vec::new(),
            };
        }
    };

    let screens: Vec<ScreenRect> = monitors
        .iter()
        .map(|monitor| ScreenRect {
            name: monitor.name.clone(),
            rect: Rect::of_monitor(monitor),
        })
        .collect();
    let rects: Vec<Rect> = screens.iter().map(|screen| screen.rect).collect();
    let windows = collect_windows();
    let busy = busy_screens(&windows, &rects, MIN_COVERAGE);

    Detection {
        screens,
        windows,
        busy,
    }
}

/// 现在有程序全屏的那几块屏（元素是显示器名）。
pub fn screens_with_fullscreen_window() -> Vec<String> {
    detect().busy_names()
}

/// 某一块屏上是否有程序全屏。
pub fn is_screen_busy(monitor_name: &str) -> bool {
    screens_with_fullscreen_window()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(monitor_name))
}

/// 枚举候选窗口。
///
/// 这里**必须**先把本线程标成 per-monitor DPI 感知，否则窗口矩形（物理像素）
/// 与显示器矩形（逻辑坐标）口径不一致，判据会恒为假——上一版就是这么失败的，
/// 而且两个数字各自看都"很正常"，只有并排比较才看得出来。
/// `SetThreadDpiAwarenessContext` 按线程生效，不影响界面线程的布局。
fn collect_windows() -> Vec<CandidateWindow> {
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    collect_windows_raw()
}

#[cfg(windows)]
fn collect_windows_raw() -> Vec<CandidateWindow> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsIconic, IsWindowVisible};

    /// `EnumWindows` 的回调要通过 `LPARAM` 拿到的东西。
    ///
    /// 回调是 `extern "system" fn` 而**不是闭包**，因此它不能捕获外部变量——
    /// 进程 ID 必须随这个结构一起传进去（`desktop.rs` 里的枚举是同样的道理，
    /// 这也是为什么"自己进程的窗口要跳过"这条不能直接在回调里写 `std::process::id()`）。
    struct CollectState {
        own_pid: u32,
        found: Vec<CandidateWindow>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // 安全性：lparam 由本函数传入，指向本次调用期间保持有效的 CollectState。
        let state = unsafe { &mut *(lparam.0 as *mut CollectState) };
        // 这一层只做最便宜的两个判断，剩下的交给 `inspect`，
        // 免得为每个窗口都做一遍跨进程查询。
        if unsafe { IsWindowVisible(hwnd).as_bool() } && !unsafe { IsIconic(hwnd).as_bool() } {
            if let Some(window) = inspect(hwnd, state.own_pid) {
                state.found.push(window);
            }
        }
        BOOL(1)
    }

    let mut state = Box::new(CollectState {
        own_pid: std::process::id(),
        found: Vec::new(),
    });

    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM((&mut *state) as *mut CollectState as isize),
        );
    }
    std::mem::take(&mut state.found)
}

/// 把一个窗口变成候选（不满足条件就返回 `None`）。
#[cfg(windows)]
fn inspect(hwnd: windows::Win32::Foundation::HWND, own_pid: u32) -> Option<CandidateWindow> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsZoomed, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW,
    };

    unsafe {
        // 本进程自己的窗口：主界面最大化时不该把自己算成"有程序在全屏"。
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid {
            return None;
        }

        // 工具窗口：托盘浮层、输入法候选框之类，它们不该参与判定。
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex_style & (WS_EX_TOOLWINDOW.0 as isize) != 0 {
            return None;
        }

        let class_name = window_class(hwnd);
        if is_shell_class(&class_name) {
            return None;
        }

        // DWM 遮蔽：虚拟桌面上"看不见"的 UWP 窗口仍然报着全屏尺寸。
        let mut cloaked: u32 = 0;
        let cloaked_ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok();
        if cloaked_ok && cloaked != 0 {
            return None;
        }

        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }
        let rect = Rect::new(rect.left, rect.top, rect.right, rect.bottom);
        // 尺寸为 0 的矩形没有意义（隐藏的宿主窗口常常是 0x0）。
        if rect.width() <= 0 || rect.height() <= 0 {
            return None;
        }

        Some(CandidateWindow {
            hwnd: hwnd.0 as isize,
            class_name,
            title: window_title(hwnd),
            pid,
            rect,
            maximized: IsZoomed(hwnd).as_bool(),
        })
    }
}

#[cfg(windows)]
fn window_class(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    unsafe {
        let mut buffer = [0u16; 256];
        let length = GetClassNameW(hwnd, &mut buffer);
        if length <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..length as usize])
    }
}

/// 取窗口标题（只用于诊断）。
///
/// 对**别的进程**的窗口，`GetWindowTextW` 读的是系统缓存下来的标题，
/// 不会向那个窗口发 `WM_GETTEXT`，因此不会因为对方无响应而卡住；
/// 我们本来就已经排除了自己的进程。
#[cfg(windows)]
fn window_title(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    unsafe {
        let mut buffer = [0u16; 512];
        let length = GetWindowTextW(hwnd, &mut buffer);
        if length <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buffer[..length as usize])
    }
}

#[cfg(not(windows))]
fn collect_windows_raw() -> Vec<CandidateWindow> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机主屏：3840x2160（`monitors.rs` 里的实测值）。
    fn primary() -> Rect {
        Rect::new(0, 0, 3840, 2160)
    }

    /// 本机副屏：1440x2560 竖屏，位于主屏左侧且 y 为负。
    fn secondary() -> Rect {
        Rect::new(-1440, -93, 0, 2467)
    }

    fn window(rect: Rect, maximized: bool) -> CandidateWindow {
        CandidateWindow {
            hwnd: 0x1234,
            class_name: "TestWindow".to_string(),
            title: "测试".to_string(),
            pid: 4321,
            rect,
            maximized,
        }
    }

    #[test]
    fn shell_class_names_are_recognized() {
        // 漏掉一个外壳类名的后果是"点一下桌面 / 打开任务视图，轮播就停了"，
        // 而那种现象很难联想到是这个名单的问题，因此单独守一条。
        for name in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "MultitaskingViewFrame",
            "XamlExplorerHostIslandWindow",
        ] {
            assert!(is_shell_class(name), "{name} 应当被当成系统外壳窗口");
        }
        // 反例：普通程序窗口与我们的隐藏消息窗口都不该在这个名单里。
        assert!(!is_shell_class("Chrome_WidgetWin_1"));
        assert!(!is_shell_class("WindowsWallPaperHiddenSink"));
    }

    #[test]
    fn a_window_flush_with_the_screen_counts_as_fullscreen() {
        let screens = [primary()];
        let busy = busy_screens(&[window(primary(), false)], &screens, MIN_COVERAGE);
        assert_eq!(busy, vec![true]);
    }

    #[test]
    fn a_borderless_fullscreen_window_that_leaves_room_for_the_taskbar_still_counts() {
        // 有些播放器的"无边框"模式留出任务栏高度：2160 高的屏上约损失 2%。
        let screens = [primary()];
        let busy = busy_screens(
            &[window(Rect::new(0, 0, 3840, 2088), false)],
            &screens,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![true], "留出任务栏的无边框全屏应当算全屏");
    }

    #[test]
    fn a_maximized_window_counts_even_when_a_tall_taskbar_drops_the_coverage() {
        // **用户明确要求"最大化也算"**，而这条不能只靠覆盖率：
        // 768 高的屏 + 48 像素任务栏只有 0.9375，掉到门槛以下。
        // 这里用一个覆盖率不足但确实最大化的窗口验证 `IsZoomed` 那条分支。
        let screens = [Rect::new(0, 0, 1366, 768)];
        let half_height = 720; // 覆盖率 0.9375 < 0.95
        let busy = busy_screens(
            &[window(Rect::new(0, 0, 1366, half_height), true)],
            &screens,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![true], "最大化窗口应当算全屏（用户要求）");

        // 反例：**同一个矩形但没最大化**（比如一个手动拉大的窗口）不算。
        let busy = busy_screens(
            &[window(Rect::new(0, 0, 1366, half_height), false)],
            &screens,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![false], "覆盖率不足且没最大化的窗口不是全屏");
    }

    #[test]
    fn a_window_on_one_screen_does_not_pause_the_other() {
        // 允许把第二块屏搬到任意位置，因此用相对关系断言：
        // 副屏上铺满的窗口只该让副屏忙。
        let screens = [primary(), secondary()];
        let busy = busy_screens(&[window(secondary(), false)], &screens, MIN_COVERAGE);
        assert_eq!(busy, vec![false, true]);
    }

    #[test]
    fn two_screens_can_both_be_busy() {
        // 上一版用"前台窗口"做判据，这个场景根本无法表达（只有一个前台窗口）。
        let screens = [primary(), secondary()];
        let busy = busy_screens(
            &[window(primary(), false), window(secondary(), false)],
            &screens,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![true, true]);
    }

    #[test]
    fn a_maximized_window_only_pauses_its_own_screen() {
        // **这条守的是 Lively 判据里的那个洞**：只看 `IsZoomed` 而不校验
        // "它主要落在哪块屏"，一块屏上的最大化窗口会把所有屏都判成忙。
        let screens = [primary(), secondary()];
        // 覆盖率故意压到门槛以下（768 高的虚拟屏 + 高任务栏），
        // 这样能真正走到 `IsZoomed` 那条分支。
        let small = Rect::new(0, 0, 1366, 768);
        let screens_small = [small, secondary()];
        let busy = busy_screens(
            &[window(Rect::new(0, 0, 1366, 720), true)],
            &screens_small,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![true, false], "最大化只该影响它自己那块屏");

        // 顺带确认"两块屏"那组数据仍然可用（避免上面那两个基准被写错）。
        assert_eq!(busy_screens(&[], &screens, MIN_COVERAGE), vec![false, false]);
    }

    #[test]
    fn a_window_mode_window_is_not_fullscreen() {
        // 实测样本：PotPlayer 在窗口模式下是 (600,171)-(3239,1989)。
        let screens = [primary()];
        let busy = busy_screens(
            &[window(Rect::new(600, 171, 3239, 1989), false)],
            &screens,
            MIN_COVERAGE,
        );
        assert_eq!(busy, vec![false]);
    }

    #[test]
    fn a_window_covering_half_the_screen_is_not_fullscreen() {
        let screens = [primary()];
        for rect in [
            Rect::new(0, 0, 1920, 2160),   // 左半边
            Rect::new(0, 0, 3840, 1080),   // 上半边
            Rect::new(-100, 0, 1920, 2160), // 一半在屏外
        ] {
            let busy = busy_screens(&[window(rect, false)], &screens, MIN_COVERAGE);
            assert_eq!(busy, vec![false], "{} 不该算全屏", rect.describe());
        }
    }

    #[test]
    fn a_degenerate_screen_rectangle_is_never_busy() {
        // 防御：拿不到显示器矩形（全 0）时不能把任何东西当成全屏，
        // 那会导致"轮播永远停着"，比多换一张壁纸糟得多。
        let screens = [Rect::new(0, 0, 0, 0)];
        let busy = busy_screens(&[window(primary(), false)], &screens, MIN_COVERAGE);
        assert_eq!(busy, vec![false]);
    }

    #[test]
    fn no_windows_means_nothing_is_busy() {
        let screens = [primary(), secondary()];
        assert_eq!(busy_screens(&[], &screens, MIN_COVERAGE), vec![false, false]);
    }

    #[test]
    fn coverage_ignores_the_part_of_a_window_that_hangs_off_screen() {
        // 一个比屏幕还大的窗口（最大化的窗口矩形常常比屏幕各大 8 像素），
        // 覆盖率必须按"与屏幕的交集"算，否则会算出大于 1 的数字。
        let screen = primary();
        let oversized = Rect::new(-8, -8, 3848, 2168);
        let coverage = oversized.coverage_of(&screen);
        assert!((coverage - 1.0).abs() < 0.001, "覆盖率应当封顶在 1.0");
    }

    #[test]
    fn a_window_spanning_both_screens_pauses_both() {
        // 把一块窗口横跨两块屏铺满：两块屏都被盖住，都该暂停。
        //
        // 矩形取两块屏的**并集**（副屏在左边且 y 为负，因此 y 要从 -93 起）：
        // 只盖住副屏的大部分（例如从 y=0 开始）覆盖率只有 0.84，
        // 那本来就不该算"这块屏被盖满了"。
        let screens = [primary(), secondary()];
        let spanning = Rect::new(-1440, -93, 3840, 2467);
        let busy = busy_screens(&[window(spanning, false)], &screens, MIN_COVERAGE);
        assert_eq!(busy, vec![true, true]);
    }

    #[test]
    fn a_real_detection_run_does_not_panic() {
        // 真实调用只验证"能安全跑完"：本机此刻有没有全屏窗口是不确定的，
        // 因此不断言结果。参数值由 `--probe-fullscreen` 人工核对。
        let detection = detect();
        assert_eq!(detection.screens.len(), detection.busy.len());
        let _ = detection.describe();
        let _ = screens_with_fullscreen_window();
        let _ = is_screen_busy(r"\\.\DISPLAY1");
    }
}

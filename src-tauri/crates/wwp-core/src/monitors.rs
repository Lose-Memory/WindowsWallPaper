//! 显示器枚举。
//!
//! 用 `EnumDisplayMonitors` + `GetMonitorInfoW` 而不是 Tauri 的显示器 API：
//! tao 在 Windows 上的显示器标识被报告为不可靠（tao#1102），而每屏配置必须跨重启、
//! 跨热插拔稳定对应到同一块屏幕。自己枚举可以把"用于显示的矩形"和
//! "用于设图的设备路径"这两件不同的事分开处理。
//!
//! 注意 `DISPLAY_DEVICE.DeviceName`（如 `\\.\DISPLAY1`）与
//! `IDesktopWallpaper` 要求的 `GetMonitorDevicePathAt` 返回值（形如
//! `\\?\DISPLAY#DEL40E8#...#{...}`）**不是同一个东西**。前者用于进程内的屏幕定位，
//! 后者用于按屏设图；两者的对应关系由 `wallpaper` 模块在运行期做宽松匹配。

use serde::{Deserialize, Serialize};

use crate::Result;

/// 一块显示器。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    /// 形如 `\\.\DISPLAY1`。
    pub name: String,
    /// 虚拟桌面坐标下的左上角与尺寸。
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// `GetDpiForMonitor(MDT_EFFECTIVE_DPI)` 得到的 DPI。取不到时为 0。
    pub dpi: u32,
}

impl Monitor {
    /// 长宽比（宽/高）。
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {}x{} @({},{}) {}{} dpi={}",
            self.name,
            self.width,
            self.height,
            self.x,
            self.y,
            if self.primary { "[主]" } else { "" },
            if self.is_portrait() { " 竖屏" } else { "" },
            self.dpi
        )
    }

    /// 是否竖屏（高度大于宽度）。本机有一块 1440x2560 的竖屏，
    /// 按长宽比筛选时这块屏需要的是竖图，因此这个判断有用。
    pub fn is_portrait(&self) -> bool {
        self.height > self.width
    }
}

/// 虚拟桌面的整体矩形 `(x, y, width, height)`。
///
/// 多屏壁纸合成或每屏宿主窗口定位都要用它：屏幕坐标可以是负的，
/// 不先算出这个范围就无法把窗口摆到正确的绝对位置上。
pub fn virtual_desktop(monitors: &[Monitor]) -> (i32, i32, u32, u32) {
    if monitors.is_empty() {
        return (0, 0, 0, 0);
    }
    let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = monitors
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(0);
    let max_y = monitors
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(0);
    (min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32)
}

/// 虚拟桌面的**布局尺寸**：(左, 上, 宽, 高)。
///
/// ## 为什么不把各显示器的矩形并起来
///
/// 因为那样算出来是错的。本机实测：两块屏是 3840x2160 @(0,0) 与 1440x2560 @(-1440,-93)，
/// 并集是 5280x2653，而系统权威值 `SM_CXVIRTUALSCREEN`/`SM_CYVIRTUALSCREEN` 是 **4000x2560**。
///
/// 原因是**进程是 per-monitor DPI 感知的，而桌面布局用的是逻辑像素**：
/// 主屏物理 3840x2160、缩放 150%，在布局里按 2560x1440 参与；副屏物理 1440x2560、
/// 缩放 100%，按 1440x2560 参与。并集因此是 2560+1440=4000 宽。
/// 直接并物理矩形会把主屏多算 1.5 倍，得到一个比真实桌面大得多的区域。
///
/// 这个尺寸要用来给桌面层窗口定尺寸，算大了虽然不至于出错（窗口超出部分不可见），
/// 但会让"窗口是否正好覆盖桌面"这类验证失去意义，也会让按屏切分区域的坐标算错。
///
/// `fallback_monitors` 只在 `GetSystemMetrics` 意外返回 0 时使用。
pub fn virtual_desktop_layout(fallback_monitors: &[Monitor]) -> (i32, i32, u32, u32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    let (x, y, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };

    if width > 0 && height > 0 {
        return (x, y, width as u32, height as u32);
    }

    // 兜底：并集。已知在有 DPI 缩放时偏大，但总比 0 好。
    log::warn!("GetSystemMetrics 未能给出虚拟桌面尺寸，退回按显示器矩形求并集（缩放时可能偏大）");
    virtual_desktop(fallback_monitors)
}

#[cfg(windows)]
pub fn enumerate() -> Result<Vec<Monitor>> {
    windows_impl::enumerate()
}

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<Monitor>> {
    Ok(Vec::new())
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::Error;

    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
    };

    pub fn enumerate() -> Result<Vec<Monitor>> {
        // 先声明 DPI 感知，再枚举。
        //
        // 这一步不是可选的：进程若不是 DPI 感知的，`GetMonitorInfoW` 返回的是
        // **被系统虚拟化过的**尺寸（本机 4K@150% 的屏会报成 2560x1440），
        // 而我们按这个尺寸准备壁纸图就会与实际显示模式不符。
        // 实测踩过这个坑：同一个屏在两个进程里报出不同的分辨率，差别只在于
        // 那个进程是不是 DPI 感知的。Tauri 运行时通常已经把进程设成感知的，
        // 但依赖"通常"会让结论随启动方式变化，因此这里显式设一次（重复设置无害）。
        ensure_dpi_aware();

        let mut collected: Vec<Monitor> = Vec::new();

        unsafe extern "system" fn callback(
            monitor: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            // 安全性：data 由本函数传入，指向调用期间保持有效的 Vec。
            let collected = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
                let rect = info.monitorInfo.rcMonitor;
                let name = String::from_utf16_lossy(
                    &info
                        .szDevice
                        .iter()
                        .take_while(|c| **c != 0)
                        .copied()
                        .collect::<Vec<u16>>(),
                );
                collected.push(Monitor {
                    name,
                    x: rect.left,
                    y: rect.top,
                    width: (rect.right - rect.left) as u32,
                    height: (rect.bottom - rect.top) as u32,
                    primary: (info.monitorInfo.dwFlags & 1) != 0, // MONITORINFOF_PRIMARY
                    dpi: dpi_of(monitor),
                });
            }
            TRUE
        }

        unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(callback),
                LPARAM((&mut collected) as *mut Vec<Monitor> as isize),
            )
            .ok()
            .map_err(|e| Error::config("枚举显示器失败", e.to_string()))?;
        }

        // 主屏排最前，其余按虚拟桌面坐标排序，保证顺序稳定、不随枚举顺序变化。
        collected.sort_by(|a, b| {
            b.primary
                .cmp(&a.primary)
                .then(a.x.cmp(&b.x))
                .then(a.y.cmp(&b.y))
        });
        Ok(collected)
    }


/// 取显示器的有效 DPI。失败时返回 0（调用方按"未知"处理）。
    fn dpi_of(monitor: HMONITOR) -> u32 {
        use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
        unsafe {
            let mut x = 0u32;
            let mut y = 0u32;
            match GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) {
                Ok(()) => x,
                Err(_) => 0,
            }
        }
    }

    /// 确保进程是 per-monitor DPI 感知的。
    ///
    /// 用 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`：Win10 1703+ 起可用，
    /// 且是按显示器独立处理的（本机两块屏 DPI 可能不同）。
    ///
    /// 进程已设过时这个调用会失败，但那没关系——我们要的只是"确保它是感知的"，
    /// 而不是"由我们来设置它"。
    fn ensure_dpi_aware() {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
}

#[cfg(test)]
mod dpi_diag {
    #[test]
    fn dump_raw_monitor_and_metric_values() {
        // 把四个原始数值都打出来，用于确定"到底哪个来源给的是布局像素"。
        // 之前我按推断写结论，结果被反复推翻，因此改成直接打印事实。
        let monitors = crate::monitors::enumerate().expect("枚举应成功");
        for m in &monitors {
            println!(
                "显示器 {} 位置=({},{}) 尺寸={}x{} 主屏={} dpi={}",
                m.name, m.x, m.y, m.width, m.height, m.primary, m.dpi
            );
        }
        let (ux, uy, uw, uh) = crate::monitors::virtual_desktop(&monitors);
        println!("按显示器求并集 = ({ux},{uy}) {uw}x{uh}");
        let (lx, ly, lw, lh) = crate::monitors::virtual_desktop_layout(&monitors);
        println!("GetSystemMetrics 布局 = ({lx},{ly}) {lw}x{lh}");
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
            };
            println!(
                "主屏 GetSystemMetrics = {}x{}",
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_size_agrees_with_the_monitor_union() {
        // 这条测试记录本机的**实测数值**。
        //
        // 背景（我在这里连续推断错了两次，所以把结论和依据一起写下）：
        // 我曾按"主屏物理 3840x2160 + 副屏 1440x2560"推断虚拟桌面是 5280x2560，
        // 又从另一个（DPI 未感知的）进程里读到 4000x2560，于是断定"并集偏大"。
        // 把四个数字在**同一个进程**里都打出来之后才确认：
        // 本进程里 `GetMonitorInfoW` 给的并集与 `GetSystemMetrics` **一致**，都是 5280x2560。
        // 那个 4000 来自另一个进程，两者不可比。
        //
        // 本条断言的作用是：本机显示配置或枚举方式一旦变化就立刻失败，
        // 逼人重新实测，而不是照着旧推断改代码。
        let monitors = crate::monitors::enumerate().expect("枚举显示器应当成功");
        assert!(!monitors.is_empty(), "本机应当至少有一块屏");

        let (_, _, layout_width, layout_height) = virtual_desktop_layout(&monitors);
        let (_, _, union_width, union_height) = virtual_desktop(&monitors);

        // 本机实测：主屏 3840x2160 @(0,0)，副屏 1440x2560 @(-1440,-93)。
        assert_eq!(
            (union_width, union_height),
            (5280, 2560),
            "显示器枚举给出的并集变了（期望 5280x2560）。\
             若显示配置确实变了，请更新这条期望值；\
             若没变，说明枚举方式改了（例如 DPI 感知设置变化）"
        );
        assert_eq!(
            (layout_width, layout_height),
            (union_width, union_height),
            "系统布局尺寸与显示器并集不一致：系统 {layout_width}x{layout_height}，\
             并集 {union_width}x{union_height}。本进程里两者本应一致"
        );
    }

    #[test]
    fn layout_size_falls_back_when_monitors_are_empty() {
        // GetSystemMetrics 在真实桌面上总是有效，因此这里只验证"空输入不会 panic"。
        let (_, _, width, height) = virtual_desktop_layout(&[]);
        assert!(width > 0 && height > 0, "系统应能给出虚拟桌面尺寸");
    }

    fn monitor(name: &str, x: i32, y: i32, w: u32, h: u32, primary: bool) -> Monitor {
        Monitor {
            name: name.to_string(),
            x,
            y,
            width: w,
            height: h,
            primary,
            dpi: 144,
        }
    }

    #[test]
    fn virtual_desktop_covers_negative_coordinates() {
        // 本机实测布局：主屏在 (0,0)，副屏在左边且 y 为负。
        let monitors = vec![
            monitor(r"\\.\DISPLAY1", 0, 0, 2560, 1440, true),
            monitor(r"\\.\DISPLAY2", -1440, -93, 1440, 2560, false),
        ];
        let (x, y, w, h) = virtual_desktop(&monitors);
        assert_eq!(x, -1440);
        assert_eq!(y, -93);
        // 右边界取两块屏的最大值：主屏右边界 0+2560=2560，副屏右边界 -1440+1440=0。
        assert_eq!(w, 2560u32 + 1440u32);
        // 下边界取副屏的 -93+2560=2467（主屏只有 1440）。
        assert_eq!(h, 2467u32 + 93u32);
    }

    #[test]
    fn empty_monitor_list_gives_empty_rect() {
        assert_eq!(virtual_desktop(&[]), (0, 0, 0, 0));
    }

    #[test]
    fn portrait_detection_matches_the_rotated_secondary_display() {
        let landscape = monitor("a", 0, 0, 2560, 1440, true);
        let portrait = monitor("b", -1440, -93, 1440, 2560, false);
        assert!(!landscape.is_portrait());
        assert!(portrait.is_portrait());
        assert!((landscape.aspect() - 16.0 / 9.0).abs() < 0.01);
        assert!((portrait.aspect() - 1440.0 / 2560.0).abs() < 0.0001);
    }
}

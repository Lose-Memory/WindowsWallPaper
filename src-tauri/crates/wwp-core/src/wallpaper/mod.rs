//! 静态壁纸：按屏设置、读写用户原有设置、可回滚。
//!
//! 使用 COM 接口 `IDesktopWallpaper`（Windows 8 起提供，逐屏设图是文档化能力，
//! 不是未公开行为）。与 `SystemParametersInfoW` 的区别很关键：
//!
//! - `SystemParametersInfoW(SPI_SETDESKWALLPAPER)` 只接受**一整张**位图，
//!   要"每屏不同图"就得自己合成虚拟桌面大小的位图（双 4K 约 63 MB/帧），
//!   而且任何一屏换图都要整体重发，两屏的切换时机被强制绑定。
//! - `IDesktopWallpaper::SetWallpaper(monitor_id, path)` **按屏独立设图**，
//!   互不重合成，这正是需求 4/5 要的。
//!
//! ## 两个容易踩的点
//!
//! **1. 位置模式的编号有两套，不能混用。** COM 的 `DESKTOP_WALLPAPER_POSITION`
//! 取值是 0=居中 1=平铺 2=拉伸 3=适应 4=填充 5=跨屏；而注册表里的 `WallpaperStyle`
//! 是 0=居中 2=拉伸 6=适应 10=填充 22=跨屏。也就是说 `DWPOS_FILL` 是 **4**，
//! 对应的 `WallpaperStyle` 却是 **10**。把两套数字互相赋值会静默设成错误的模式。
//! 本模块对外一律用 [`WallpaperPosition`] 枚举，两套编号只在边界上出现一次。
//!
//! **2. 逐屏设图必须配合非跨屏模式。** 本机原状态正是 `WallpaperStyle = 22`（跨屏），
//! 那种模式下系统把图按整块虚拟桌面拉伸处理，逐屏设置就失去意义。
//!
//! ## 安全网
//!
//! 每次改动前把用户原有的壁纸设置（各屏壁纸 + 位置模式 + 全局壁纸文件）存成快照，
//! 并提供回滚。壁纸是用户的东西，程序不能只在"正常退出"时才想起来还回去。

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// 壁纸位置模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperPosition {
    Center,
    Tile,
    Stretch,
    Fit,
    Fill,
    Span,
}

impl WallpaperPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            WallpaperPosition::Center => "center",
            WallpaperPosition::Tile => "tile",
            WallpaperPosition::Stretch => "stretch",
            WallpaperPosition::Fit => "fit",
            WallpaperPosition::Fill => "fill",
            WallpaperPosition::Span => "span",
        }
    }

    pub fn parse(text: &str) -> WallpaperPosition {
        match text {
            "center" => WallpaperPosition::Center,
            "tile" => WallpaperPosition::Tile,
            "stretch" => WallpaperPosition::Stretch,
            "fit" => WallpaperPosition::Fit,
            "span" => WallpaperPosition::Span,
            _ => WallpaperPosition::Fill,
        }
    }

    /// 注册表 `WallpaperStyle` 的取值。
    ///
    /// 只用于与注册表对照显示。**不要**拿它去调用 COM 接口。
    pub fn registry_wallpaper_style(self) -> i32 {
        match self {
            WallpaperPosition::Center => 0,
            WallpaperPosition::Tile => 0,
            WallpaperPosition::Stretch => 2,
            WallpaperPosition::Fit => 6,
            WallpaperPosition::Fill => 10,
            WallpaperPosition::Span => 22,
        }
    }

    /// 逐屏设图要求位置模式不能是跨屏。
    pub fn is_per_monitor_compatible(self) -> bool {
        self != WallpaperPosition::Span
    }
}

/// 一块显示器以及它当前的壁纸。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorWallpaper {
    /// `IDesktopWallpaper::GetMonitorDevicePathAt` 返回的设备路径。
    pub device_path: String,
    /// 该屏当前壁纸的文件路径。为空表示这一屏没有单独设置。
    pub current: String,
    /// 该屏的矩形（由 `GetMonitorRECT` 取得，便于与显示器枚举结果对照）。
    pub rect: Option<(i32, i32, i32, i32)>,
}

/// 用户原有壁纸设置的一份快照，用于回滚。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperSnapshot {
    pub position: String,
    pub monitors: Vec<MonitorWallpaper>,
    /// 系统全局壁纸文件（注册表 `HKCU\Control Panel\Desktop\WallPaper`）。
    pub desktop_file: String,
    pub taken_at: i64,
}

impl WallpaperSnapshot {
    pub fn describe(&self) -> String {
        let mut text = format!(
            "位置模式      : {}\n全局壁纸文件  : {}\n",
            self.position,
            if self.desktop_file.is_empty() {
                "（未设置）"
            } else {
                &self.desktop_file
            }
        );
        text.push_str(&format!("显示器（{} 块）:\n", self.monitors.len()));
        for monitor in &self.monitors {
            let rect = match monitor.rect {
                Some((l, t, r, b)) => format!("({l},{t}) {}x{}", r - l, b - t),
                None => "矩形未知".to_string(),
            };
            text.push_str(&format!(
                "  {}\n    设备路径: {}\n    当前壁纸: {}\n",
                rect,
                monitor.device_path,
                if monitor.current.is_empty() {
                    "（未设置）"
                } else {
                    &monitor.current
                }
            ));
        }
        text
    }
}

/// `IDesktopWallpaper` 的可用性与能力。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub available: bool,
    pub error: Option<String>,
    pub monitor_count: usize,
    pub position: String,
    /// 位置模式是否与"逐屏不同图"相容。不相容时逐屏设图的语义不成立。
    pub per_monitor_compatible: bool,
    /// 注册表里的 WallpaperStyle，用于对照（与 COM 的编号不同）。
    pub registry_wallpaper_style: Option<i32>,
}

/// 一次设置的逐屏结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOutcome {
    pub device_path: String,
    pub path: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// `IDesktopWallpaper` 的封装。
///
/// 每次操作都新建 COM 实例、用完即释放：COM 有套间（apartment）语义，
/// 把实例长期持有并跨线程使用会引入难以复现的问题，而这里的操作都是低频的。
/// 每个方法内部自行完成 `CoInitializeEx`/`CoUninitialize` 配对。
pub struct DesktopWallpaper;

impl DesktopWallpaper {
    /// 枚举所有显示器及它们当前的壁纸。
    pub fn enumerate() -> Result<Vec<MonitorWallpaper>> {
        windows_impl::enumerate()
    }

    pub fn position() -> Result<WallpaperPosition> {
        windows_impl::position()
    }

    pub fn set_position(position: WallpaperPosition) -> Result<()> {
        windows_impl::set_position(position)
    }

    /// 给指定屏幕设置壁纸。`device_path` 传 `None` 表示所有屏幕。
    pub fn set_wallpaper(device_path: Option<&str>, image: &str) -> Result<()> {
        windows_impl::set_wallpaper(device_path, image)
    }

    /// 系统全局壁纸文件路径（注册表）。
    ///
    /// 这是"用户原来的壁纸"最可靠的来源：`GetWallpaper` 在某些配置下会返回空，
    /// 而注册表这一项总是被 Windows 写入过。
    pub fn desktop_file() -> String {
        windows_impl::desktop_file()
    }

    /// 拍一份快照：位置模式 + 各屏当前壁纸 + 全局壁纸文件。
    pub fn snapshot() -> Result<WallpaperSnapshot> {
        let position = Self::position()?;
        let monitors = Self::enumerate()?;
        Ok(WallpaperSnapshot {
            position: position.as_str().to_string(),
            monitors,
            desktop_file: Self::desktop_file(),
            taken_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        })
    }

    /// 按快照回滚。
    ///
    /// 顺序有意为之：**先逐屏恢复壁纸，最后恢复位置模式**。反过来的话，恢复过程中
    /// 会短暂经过"跨屏模式 + 逐屏图"的组合，那正是会导致拉伸错乱的状态。
    ///
    /// ## 这个函数只知道"照着快照设"
    ///
    /// 它拿快照里的设备路径直接调 `SetWallpaper`。给一个**不存在**的设备路径调用
    /// 也会返回成功（实测），于是"那块屏其实已经不在了"会被静默吞掉。
    /// 需要在显示器可能变化过的场合（崩溃恢复）请用 [`DesktopWallpaper::restore_matched`]。
    pub fn restore(snapshot: &WallpaperSnapshot) -> Result<Vec<SetOutcome>> {
        let mut outcomes = Vec::new();
        for monitor in &snapshot.monitors {
            if monitor.current.is_empty() {
                continue;
            }
            let result = Self::set_wallpaper(Some(&monitor.device_path), &monitor.current);
            outcomes.push(SetOutcome {
                device_path: monitor.device_path.clone(),
                path: monitor.current.clone(),
                ok: result.is_ok(),
                error: result.err().map(|e| e.to_string()),
            });
        }
        // 位置模式放最后；这一步失败不影响壁纸已经恢复这件事。
        if let Err(error) = Self::set_position(WallpaperPosition::parse(&snapshot.position)) {
            log::warn!("恢复壁纸位置模式失败：{error}");
        }
        Ok(outcomes)
    }

    /// 按快照回滚，但**先把快照里的屏与当前实际屏对起来**。
    ///
    /// ## 为什么不直接用 [`DesktopWallpaper::restore`]
    ///
    /// 见上面对 `restore` 的说明：给不存在的设备路径调用也会"成功"。
    /// 这个函数多做两件事：
    ///
    /// 1. **匹配**：把快照里的每条记录与当前枚举出的屏对上。
    ///    优先用设备路径，路径对不上时退回用**屏幕矩形**——
    ///    屏幕位置比设备路径稳定（路径里含连接序号与 UID，
    ///    换个接口插就可能变，而分辨率与坐标通常不变）。
    /// 2. **如实报告**：对不上的记录标成失败并说明"这块屏现在不在"，
    ///    而不是报一个假的成功。
    ///
    /// 副屏拔掉、显示器换口、笔记本切换投影模式都会走到这里。
    pub fn restore_matched(snapshot: &WallpaperSnapshot) -> Result<Vec<SetOutcome>> {
        let current = Self::enumerate()?;
        let mut outcomes = Vec::new();

        for saved in &snapshot.monitors {
            let Some(target) = match_saved_monitor(saved, &current) else {
                // 如实报告"这块屏现在不在"。**不能**报成功——
                // 那会让用户以为壁纸已还原，而实际上那块屏并不在这里。
                outcomes.push(SetOutcome {
                    device_path: saved.device_path.clone(),
                    path: saved.current.clone(),
                    ok: false,
                    error: Some(
                        "这块屏现在不在（设备路径与屏幕位置都匹配不到）".to_string(),
                    ),
                });
                continue;
            };

            if saved.current.is_empty() {
                continue;
            }
            let result = Self::set_wallpaper(Some(&target.device_path), &saved.current);
            outcomes.push(SetOutcome {
                // 报告**实际用到的**设备路径，而不是快照里那个旧的：
                // 用户看到"还原到了哪块屏"才是有用的信息。
                device_path: target.device_path.clone(),
                path: saved.current.clone(),
                ok: result.is_ok(),
                error: result.err().map(|e| e.to_string()),
            });
        }

        if let Err(error) = Self::set_position(WallpaperPosition::parse(&snapshot.position)) {
            log::warn!("恢复壁纸位置模式失败：{error}");
        }
        Ok(outcomes)
    }
}

/// 把快照里的一条显示器记录与当前实际存在的屏对上。
///
/// ## 匹配顺序为什么是这样
///
/// 1. **先按设备路径**（大小写不敏感）：路径是唯一标识，能对上就一定是对的。
/// 2. **再按屏幕矩形**：设备路径里含"连接序号"与 UID，换个接口插就可能变，
///    而"这块屏在虚拟桌面里的位置与大小"通常不变。因此路径对不上时
///    用矩形是可以接受的退路。
/// 3. **都对不上就返回 `None`**，由调用方如实报告"这块屏现在不在"。
///    **绝不能**退化成"随便找一块屏设上去"——那把别人的壁纸覆盖了，
///    比不还原更糟。
///
/// ## 为什么抽成独立函数
///
/// 这段逻辑原本内联在 `restore_matched` 里，只能靠真机实测（拔插显示器）
/// 验证。抽出来之后它是**纯函数**，可以在无头环境把所有分支覆盖住——
/// 而"匹配错了会把壁纸设到错误的屏上"这种问题，真机测试很难穷举。
fn match_saved_monitor<'a>(
    saved: &MonitorWallpaper,
    current: &'a [MonitorWallpaper],
) -> Option<&'a MonitorWallpaper> {
    // 1. 设备路径优先。
    if let Some(found) = current
        .iter()
        .find(|m| m.device_path.eq_ignore_ascii_case(&saved.device_path))
    {
        return Some(found);
    }

    // 2. 退回矩形匹配。要求两边都**确实有**矩形，避免
    //    "两边都没有矩形信息"被当成匹配（那会把第一块屏当成目标）。
    if let Some(rect) = saved.rect {
        if let Some(found) = current.iter().find(|m| m.rect == Some(rect)) {
            return Some(found);
        }
    }

    None
}

/// 探测本机 `IDesktopWallpaper` 是否可用。
pub fn probe_capability() -> Capability {
    match DesktopWallpaper::enumerate() {
        Ok(monitors) => {
            let position = DesktopWallpaper::position().unwrap_or(WallpaperPosition::Fill);
            Capability {
                available: true,
                error: None,
                monitor_count: monitors.len(),
                position: position.as_str().to_string(),
                per_monitor_compatible: position.is_per_monitor_compatible(),
                registry_wallpaper_style: windows_impl::registry_wallpaper_style(),
            }
        }
        Err(error) => Capability {
            available: false,
            error: Some(error.to_string()),
            monitor_count: 0,
            position: String::new(),
            per_monitor_compatible: false,
            registry_wallpaper_style: windows_impl::registry_wallpaper_style(),
        },
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;

    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        DesktopWallpaper as DesktopWallpaperClsid, IDesktopWallpaper, DESKTOP_WALLPAPER_POSITION,
        DWPOS_CENTER, DWPOS_FILL, DWPOS_FIT, DWPOS_SPAN, DWPOS_STRETCH, DWPOS_TILE,
    };

    /// 保证线程在使用 COM 期间是已初始化的。
    ///
    /// 用 RAII 是因为 COM 初始化是**按线程计数**的：配对多一次少一次都会出问题，
    /// 而中途 `?` 提前返回在 Rust 里很常见。
    struct ComScope {
        should_uninitialize: bool,
    }

    impl ComScope {
        fn enter() -> Result<ComScope> {
            unsafe {
                let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let code = hr.0;
                // S_OK(0) 表示是我们初始化的，需要配对释放；
                // S_FALSE(1) 表示本线程已初始化过，不要释放。
                if code == 0 {
                    Ok(ComScope {
                        should_uninitialize: true,
                    })
                } else if code == 1 {
                    Ok(ComScope {
                        should_uninitialize: false,
                    })
                } else {
                    Err(Error::config(
                        "COM 初始化失败",
                        format!("CoInitializeEx 返回 0x{code:08X}"),
                    ))
                }
            }
        }
    }

    impl Drop for ComScope {
        fn drop(&mut self) {
            if self.should_uninitialize {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn interface() -> Result<IDesktopWallpaper> {
        unsafe {
            CoCreateInstance(&DesktopWallpaperClsid, None, CLSCTX_ALL).map_err(|e| {
                Error::config(
                    "无法创建 IDesktopWallpaper（DesktopWallpaper COM 对象）",
                    e.to_string(),
                )
            })
        }
    }

    /// `PWSTR` → `String`。
    ///
    /// `GetMonitorDevicePathAt` 与 `GetWallpaper` 都返回 `PWSTR`（由 COM 分配、
    /// 调用方负责释放），这里立刻转成 `String`，不把裸指针带出这一层。
    fn pwstr_to_string(value: windows::core::PWSTR) -> String {
        if value.is_null() {
            return String::new();
        }
        // SAFETY：接口保证返回以 0 结尾的字符串。
        unsafe { value.to_string().unwrap_or_default() }
    }

    fn position_to_com(position: WallpaperPosition) -> DESKTOP_WALLPAPER_POSITION {
        match position {
            WallpaperPosition::Center => DWPOS_CENTER,
            WallpaperPosition::Tile => DWPOS_TILE,
            WallpaperPosition::Stretch => DWPOS_STRETCH,
            WallpaperPosition::Fit => DWPOS_FIT,
            WallpaperPosition::Fill => DWPOS_FILL,
            WallpaperPosition::Span => DWPOS_SPAN,
        }
    }

    fn position_from_com(raw: DESKTOP_WALLPAPER_POSITION) -> WallpaperPosition {
        if raw == DWPOS_CENTER {
            WallpaperPosition::Center
        } else if raw == DWPOS_TILE {
            WallpaperPosition::Tile
        } else if raw == DWPOS_STRETCH {
            WallpaperPosition::Stretch
        } else if raw == DWPOS_FIT {
            WallpaperPosition::Fit
        } else if raw == DWPOS_SPAN {
            WallpaperPosition::Span
        } else {
            WallpaperPosition::Fill
        }
    }

    pub fn enumerate() -> Result<Vec<MonitorWallpaper>> {
        let _scope = ComScope::enter()?;
        let wallpaper = interface()?;

        let count = unsafe { wallpaper.GetMonitorDevicePathCount() }
            .map_err(|e| Error::config("枚举显示器数量失败", e.to_string()))?;

        let mut monitors = Vec::new();
        for index in 0..count {
            let device_path = unsafe { wallpaper.GetMonitorDevicePathAt(index) }
                .map_err(|e| {
                    Error::config(
                        format!("读取第 {index} 块显示器的设备路径失败"),
                        e.to_string(),
                    )
                })?;
            let device_path = pwstr_to_string(device_path);

            let current = unsafe { wallpaper.GetWallpaper(&HSTRING::from(&device_path)) }
                .map(pwstr_to_string)
                .unwrap_or_default();

            // GetMonitorRECT 是另一个独立接口，拿不到就留 None，不影响设图。
            let rect = unsafe { wallpaper.GetMonitorRECT(&HSTRING::from(&device_path)) }
                .ok()
                .map(|r| (r.left, r.top, r.right, r.bottom));

            monitors.push(MonitorWallpaper {
                device_path,
                current,
                rect,
            });
        }
        Ok(monitors)
    }

    pub fn position() -> Result<WallpaperPosition> {
        let _scope = ComScope::enter()?;
        let wallpaper = interface()?;
        let raw = unsafe { wallpaper.GetPosition() }
            .map_err(|e| Error::config("读取壁纸位置模式失败", e.to_string()))?;
        Ok(position_from_com(raw))
    }

    pub fn set_position(position: WallpaperPosition) -> Result<()> {
        let _scope = ComScope::enter()?;
        let wallpaper = interface()?;
        unsafe { wallpaper.SetPosition(position_to_com(position)) }.map_err(|e| {
            Error::config(
                format!("设置壁纸位置模式为 {} 失败", position.as_str()),
                e.to_string(),
            )
        })
    }

    pub fn set_wallpaper(device_path: Option<&str>, image: &str) -> Result<()> {
        let _scope = ComScope::enter()?;
        let wallpaper = interface()?;
        let image_wide = HSTRING::from(image);

        let result = unsafe {
            match device_path {
                Some(device) => {
                    let device_wide = HSTRING::from(device);
                    wallpaper.SetWallpaper(&device_wide, &image_wide)
                }
                // 传 null 表示所有显示器。
                None => wallpaper.SetWallpaper(PCWSTR::null(), &image_wide),
            }
        };
        result.map_err(|e| {
            Error::config(
                format!(
                    "为 {} 设置壁纸失败（{}）",
                    device_path.unwrap_or("全部显示器"),
                    image
                ),
                e.to_string(),
            )
        })
    }

    /// 读注册表 `HKCU\Control Panel\Desktop` 下的三个值。
    fn read_desktop_registry() -> (String, Option<i32>) {
        use windows::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ,
        };

        let mut wallpaper_file = String::new();
        let mut style = None;

        unsafe {
            let mut key = HKEY::default();
            let subkey = wide("Control Panel\\Desktop");
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_READ,
                &mut key,
            )
            .is_err()
            {
                return (wallpaper_file, style);
            }

            // WallPaper 是字符串
            let name = wide("WallPaper");
            let mut buffer = [0u16; 1024];
            let mut size = (buffer.len() * 2) as u32;
            let mut kind = REG_SZ;
            if RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut size),
            )
            .is_ok()
            {
                let len = (size as usize / 2).saturating_sub(1);
                wallpaper_file = String::from_utf16_lossy(&buffer[..len.min(buffer.len())]);
            }

            // WallpaperStyle 是字符串形式的数字
            let name = wide("WallpaperStyle");
            let mut buffer = [0u16; 64];
            let mut size = (buffer.len() * 2) as u32;
            let mut kind = REG_SZ;
            if RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr() as *mut u8),
                Some(&mut size),
            )
            .is_ok()
            {
                let len = (size as usize / 2).saturating_sub(1);
                let text = String::from_utf16_lossy(&buffer[..len.min(buffer.len())]);
                style = text.trim().parse::<i32>().ok();
            }

            let _ = RegCloseKey(key);
        }

        (wallpaper_file, style)
    }

    pub fn desktop_file() -> String {
        read_desktop_registry().0
    }

    pub fn registry_wallpaper_style() -> Option<i32> {
        read_desktop_registry().1
    }
}

#[cfg(not(windows))]
mod windows_impl {
    use super::*;

    pub fn enumerate() -> Result<Vec<MonitorWallpaper>> {
        Err(Error::not_found("非 Windows 平台没有 IDesktopWallpaper"))
    }
    pub fn position() -> Result<WallpaperPosition> {
        Err(Error::not_found("非 Windows 平台"))
    }
    pub fn set_position(_: WallpaperPosition) -> Result<()> {
        Err(Error::not_found("非 Windows 平台"))
    }
    pub fn set_wallpaper(_: Option<&str>, _: &str) -> Result<()> {
        Err(Error::not_found("非 Windows 平台"))
    }
    pub fn desktop_file() -> String {
        String::new()
    }
    pub fn registry_wallpaper_style() -> Option<i32> {
        None
    }
}

/// 注册表与 Win32 用的宽字符串。
///
/// 放在外层是因为两个平台模块都要用，而 `#[cfg]` 只包住实现体。
#[cfg(windows)]
pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条显示器记录。
    fn monitor(device: &str, rect: Option<(i32, i32, i32, i32)>) -> MonitorWallpaper {
        MonitorWallpaper {
            device_path: device.to_string(),
            current: format!("C:/wallpapers/{device}.jpg"),
            rect,
        }
    }

    /// 本机实测的两块屏，用于构造贴近真实的用例。
    fn display1() -> MonitorWallpaper {
        monitor(
            r"\\?\DISPLAY#MON0001#5&25556a85&5&UID4353#{e6f07b5f}",
            Some((0, 0, 3840, 2160)),
        )
    }
    fn display2() -> MonitorWallpaper {
        monitor(
            r"\\?\DISPLAY#MON0002#5&25556a85&5&UID4357#{e6f07b5f}",
            Some((-1440, -93, 0, 2560)),
        )
    }

    #[test]
    fn a_monitor_that_is_still_there_matches_by_device_path() {
        // 最常见的情况：屏没动过。必须按路径对上。
        let current = vec![display1(), display2()];
        let found = match_saved_monitor(&display2(), &current).expect("应当匹配到");
        assert_eq!(found.device_path, display2().device_path);
    }

    #[test]
    fn device_path_matching_is_case_insensitive() {
        // Windows 的设备路径大小写不保证一致，而它确实是同一个设备。
        // 不做大小写无关比较的话，用户会看到毫无道理的"这块屏现在不在"。
        let current = vec![display1()];
        let saved = monitor(
            r"\\?\display#mon0001#5&25556a85&5&uid4353#{E6F07B5F}",
            Some((0, 0, 3840, 2160)),
        );
        assert!(
            match_saved_monitor(&saved, &current).is_some(),
            "大小写不同不该影响匹配"
        );
    }

    #[test]
    fn a_replugged_monitor_matches_by_screen_rectangle() {
        // 换了个接口插：设备路径变了（连接序号与 UID 都不同），
        // 但这块屏还在原来的位置。按矩形匹配上，壁纸就能正确还原。
        let current = vec![display1(), display2()];
        let saved = monitor(
            r"\\?\DISPLAY#NEWPORT#1&11111111&1&UID1111#{e6f07b5f}",
            Some((-1440, -93, 0, 2560)),
        );
        let found = match_saved_monitor(&saved, &current).expect("应当按矩形匹配到");
        assert_eq!(
            found.device_path,
            display2().device_path,
            "应当匹配到位置相同的那块屏，而不是第一块"
        );
    }

    #[test]
    fn a_monitor_that_is_gone_does_not_match_anything() {
        // 拔掉了：路径与位置都对不上。
        //
        // **这条是本模块最重要的一条**：匹配不到时必须返回 None。
        // 如果实现退化成"随便找一块屏"，就会把另一块屏的壁纸覆盖掉——
        // 而那比"不还原"更糟（用户什么都没做，壁纸却被改了）。
        let current = vec![display1(), display2()];
        let saved = monitor(
            r"\\?\DISPLAY#GONE999#9&99999999&9&UID9999#{e6f07b5f}",
            Some((-9999, -9999, -9000, -9000)),
        );
        assert!(
            match_saved_monitor(&saved, &current).is_none(),
            "对不上的屏必须返回 None，绝不能随便选一块"
        );
    }

    #[test]
    fn a_monitor_without_rect_matches_only_by_path() {
        // 快照来自一个没能读到矩形的环境（`rect` 为空）。
        // 此时**只能**靠路径匹配——不能因为"两边都没有矩形"就把第一块屏当成目标。
        let current = vec![display1(), display2()];
        let saved_without_rect = monitor(r"\\?\DISPLAY#NEWPORT#1&11111111", None);
        assert!(
            match_saved_monitor(&saved_without_rect, &current).is_none(),
            "没有矩形信息时不该靠矩形蒙一个"
        );

        // 但如果路径对得上，没有矩形也应当匹配成功。
        let mut same_path = display1();
        same_path.rect = None;
        let found = match_saved_monitor(&same_path, &current).expect("路径对上就该匹配");
        assert_eq!(found.device_path, display1().device_path);
    }

    #[test]
    fn a_monitor_whose_rect_is_unknown_does_not_match_a_known_rect() {
        // 一边有矩形、一边没有：不该被当成匹配。
        let mut current_without_rect = display2();
        current_without_rect.device_path = r"\\?\DISPLAY#OTHER#1".to_string();
        current_without_rect.rect = None;
        let current = vec![current_without_rect];

        let saved = monitor(r"\\?\DISPLAY#MON0002#5&25556a85", Some((-1440, -93, 0, 2560)));
        assert!(
            match_saved_monitor(&saved, &current).is_none(),
            "`Some(rect)` 不该等于 `None`"
        );
    }

    #[test]
    fn an_empty_current_list_matches_nothing() {
        // 所有屏都被拔光的极端情况：不能 panic，也不能凭空匹配。
        assert!(match_saved_monitor(&display1(), &[]).is_none());
    }

    #[test]
    fn the_two_position_numberings_are_not_the_same() {
        // 这是本模块最容易出错的地方：COM 的枚举与注册表的 WallpaperStyle
        // 用两套完全不同的数字。把这条差异固定成测试，防止日后有人"统一"它们。
        assert_eq!(WallpaperPosition::Fill.registry_wallpaper_style(), 10);
        assert_eq!(WallpaperPosition::Span.registry_wallpaper_style(), 22);
        assert_eq!(WallpaperPosition::Fit.registry_wallpaper_style(), 6);
        assert_eq!(WallpaperPosition::Stretch.registry_wallpaper_style(), 2);
        // COM 侧的取值由 windows crate 定义：FILL=4、SPAN=5、FIT=3。
        // 两者必须不同，否则说明有人把它们合并了。
        assert_ne!(
            WallpaperPosition::Fill.registry_wallpaper_style(),
            4,
            "COM 的 DWPOS_FILL 是 4，注册表的是 10；相等说明编号被混用了"
        );
    }

    #[test]
    fn only_non_span_allows_per_monitor_images() {
        for position in [
            WallpaperPosition::Center,
            WallpaperPosition::Tile,
            WallpaperPosition::Stretch,
            WallpaperPosition::Fit,
            WallpaperPosition::Fill,
        ] {
            assert!(position.is_per_monitor_compatible(), "{position:?} 应相容");
        }
        assert!(!WallpaperPosition::Span.is_per_monitor_compatible());
    }

    #[test]
    fn position_strings_round_trip() {
        for position in [
            WallpaperPosition::Center,
            WallpaperPosition::Tile,
            WallpaperPosition::Stretch,
            WallpaperPosition::Fit,
            WallpaperPosition::Fill,
            WallpaperPosition::Span,
        ] {
            assert_eq!(WallpaperPosition::parse(position.as_str()), position);
        }
        // 未知值退回填充，而不是 panic。
        assert_eq!(
            WallpaperPosition::parse("nonsense"),
            WallpaperPosition::Fill
        );
    }
}

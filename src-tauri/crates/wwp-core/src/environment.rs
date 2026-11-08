//! 运行环境信息采集。
//!
//! 用途是**诊断导出**：用户报问题时，把版本、系统、显示器、DPI、显卡、配置概况
//! 一次性打包出来，比来回问"你是什么系统、几块屏、什么缩放"快得多，
//! 也避免我根据不完整的信息做出错误判断。
//!
//! ## 系统版本为什么要从 ntdll 取
//!
//! `GetVersionEx` / `RtlGetVersion` 之外的路子（包括 `GetVersion`）会被应用清单
//! 里的"兼容版本"声明篡改，报出的是**声明支持的版本**而不是真实版本。
//! 本机是 Win11 24H2 build 26100，如果按常见做法去读，很容易报成 Windows 10。
//! `RtlGetVersion` 不做这层伪装。

use serde::{Deserialize, Serialize};

use crate::Result;

/// 系统版本。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    /// 修订号（注册表里的 UBR）。它才是"同一 build 的第几次累积更新"。
    pub revision: u32,
    /// 形如 `Windows 11 24H2 build 26100.1000`。
    pub display: String,
}

/// 图形适配器。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterInfo {
    pub name: String,
    pub driver_version: String,
    /// 显存（字节）。取不到时为 0。
    pub memory_bytes: u64,
}

/// 显示器的一条记录，含桌面层探测结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// `GetDpiForMonitor(MDT_EFFECTIVE_DPI)`。
    pub dpi: u32,
    /// 缩放百分比（`dpi / 96 * 100`）。
    pub scale_percent: u32,
    /// `IDesktopWallpaper` 的设备路径（用于按屏设图）。
    pub device_path: String,
}

/// 一份完整的运行环境报告。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub os: OsVersion,
    pub adapters: Vec<AdapterInfo>,
    pub monitors: Vec<MonitorInfo>,
    /// WebView2 运行时版本（从注册表读）。
    pub webview2_version: String,
    /// 桌面拓扑（Classic / RaisedDesktop）与挂载目标。
    pub desktop_topology: String,
    pub desktop_layer: String,
    /// 每块屏的当前壁纸。
    pub current_wallpapers: Vec<(String, String)>,
    /// 采集时间与本次运行的数据位置。
    pub app_version: String,
    pub data_root: String,
    pub root_source: String,
}

impl Environment {
    /// 渲染成给人看的文本（诊断导出里作为主文件）。
    pub fn describe(&self) -> String {
        let mut out = String::new();
        out.push_str("WindowsWallPaper 运行环境\n");
        out.push_str("========================\n\n");

        out.push_str(&format!("程序版本      : {}\n", self.app_version));
        out.push_str(&format!("系统          : {}\n", self.os.display));
        out.push_str(&format!(
            "版本号        : {}.{}.{} 修订 {}\n",
            self.os.major, self.os.minor, self.os.build, self.os.revision
        ));
        out.push_str(&format!("WebView2      : {}\n", self.webview2_version));
        out.push_str(&format!("数据根目录    : {}\n", self.data_root));
        out.push_str(&format!("数据位置来源  : {}\n", self.root_source));

        out.push_str("\n图形适配器\n----------\n");
        if self.adapters.is_empty() {
            out.push_str("  （未读到）\n");
        }
        for adapter in &self.adapters {
            let memory = if adapter.memory_bytes > 0 {
                format!("{:.1} GB", adapter.memory_bytes as f64 / 1_073_741_824.0)
            } else {
                "未知".to_string()
            };
            out.push_str(&format!(
                "  {}  驱动 {}  显存 {}\n",
                adapter.name, adapter.driver_version, memory
            ));
        }

        out.push_str("\n显示器\n------\n");
        if self.monitors.is_empty() {
            out.push_str("  （未读到）\n");
        }
        for monitor in &self.monitors {
            out.push_str(&format!(
                "  {} {}x{} @({},{}) {} {} dpi（{}%）\n",
                monitor.name,
                monitor.width,
                monitor.height,
                monitor.x,
                monitor.y,
                if monitor.primary { "[主]" } else { "" },
                monitor.dpi,
                monitor.scale_percent
            ));
            out.push_str(&format!("      设备路径: {}\n", monitor.device_path));
        }

        out.push_str("\n桌面层\n------\n");
        out.push_str(&format!("  拓扑: {}\n", self.desktop_topology));
        out.push_str(&format!("  挂载目标: {}\n", self.desktop_layer));

        out.push_str("\n当前壁纸\n--------\n");
        if self.current_wallpapers.is_empty() {
            out.push_str("  （未读到）\n");
        }
        for (device, wallpaper) in &self.current_wallpapers {
            out.push_str(&format!("  {device}\n      {wallpaper}\n"));
        }

        out
    }
}

/// 采集运行环境。
pub fn collect(app_version: &str, data_root: &str, root_source: &str) -> Result<Environment> {
    let monitors_wallpaper = crate::wallpaper::DesktopWallpaper::enumerate().unwrap_or_default();
    let monitors = crate::monitors::enumerate()
        .unwrap_or_default()
        .into_iter()
        .map(|monitor| {
            // 设备路径按顺序与 IDesktopWallpaper 对应（两边的枚举顺序一致，
            // 这一点在 engine::bind_screens 里已经校验过数量）。
            let device_path = monitors_wallpaper
                .iter()
                .find(|entry| entry.rect.map(|(l, _, _, _)| l) == Some(monitor.x))
                .map(|entry| entry.device_path.clone())
                .unwrap_or_default();
            MonitorInfo {
                name: monitor.name,
                x: monitor.x,
                y: monitor.y,
                width: monitor.width,
                height: monitor.height,
                primary: monitor.primary,
                dpi: monitor.dpi,
                scale_percent: if monitor.dpi > 0 {
                    (monitor.dpi as f64 / 96.0 * 100.0).round() as u32
                } else {
                    0
                },
                device_path,
            }
        })
        .collect();

    let (desktop_topology, desktop_layer) = {
        let probe = crate::desktop::probe_desktop();
        (
            probe.topology.as_str().to_string(),
            probe.layer_source.clone(),
        )
    };

    Ok(Environment {
        os: os_version(),
        adapters: adapters(),
        monitors,
        webview2_version: webview2_version(),
        desktop_topology,
        desktop_layer,
        current_wallpapers: monitors_wallpaper
            .into_iter()
            .map(|entry| (entry.device_path, entry.current))
            .collect(),
        app_version: app_version.to_string(),
        data_root: data_root.to_string(),
        root_source: root_source.to_string(),
    })
}

/// 读真实的系统版本。
fn os_version() -> OsVersion {
    let (mut major, mut minor, mut build) = (0u32, 0u32, 0u32);

    // 优先 RtlGetVersion：它不做版本谎言。
    #[cfg(windows)]
    unsafe {
        #[repr(C)]
        struct OsVersionInfoW {
            os_version_info_size: u32,
            major_version: u32,
            minor_version: u32,
            build_number: u32,
            platform_id: u32,
            csd_version: [u16; 128],
        }
        // RtlGetVersion 的签名与 RTL_OSVERSIONINFOW 对应。
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

        if let Ok(ntdll) = GetModuleHandleW(windows::core::w!("ntdll.dll")) {
            if let Some(proc) = GetProcAddress(ntdll, windows::core::s!("RtlGetVersion")) {
                let function: unsafe extern "system" fn(*mut OsVersionInfoW) -> i32 =
                    std::mem::transmute(proc);
                let mut info = OsVersionInfoW {
                    os_version_info_size: std::mem::size_of::<OsVersionInfoW>() as u32,
                    major_version: 0,
                    minor_version: 0,
                    build_number: 0,
                    platform_id: 0,
                    csd_version: [0; 128],
                };
                if function(&mut info) == 0 {
                    major = info.major_version;
                    minor = info.minor_version;
                    build = info.build_number;
                }
            }
        }
    }

    // 修订号（UBR）只有注册表里有。它与 build 一起才构成完整版本号。
    let revision = registry_u32("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "UBR").unwrap_or(0);
    let display_version =
        registry_string("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "DisplayVersion")
            .unwrap_or_default();
    let product = registry_string(
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
        "ProductName",
    )
    .unwrap_or_else(|| "Windows".to_string());

    // ProductName 在 Win11 上仍可能写着 "Windows 10"，因此用 build 号判断代次：
    // build >= 22000 就是 Windows 11。这是判断代次唯一可靠的办法。
    let generation = if build >= 22000 { "Windows 11" } else { &product };
    let display = format!(
        "{} {} build {}.{}",
        generation,
        display_version,
        build,
        revision
    )
    .replace("  ", " ");

    OsVersion {
        major,
        minor,
        build,
        revision,
        display,
    }
}

/// 读显卡信息。
fn adapters() -> Vec<AdapterInfo> {
    // 用 WMI 需要额外依赖，因此这里从注册表读显示适配器的类键：
    // 每一项的 DriverDesc 是适配器名，DriverVersion 是驱动版本。
    // 显存大小注册表里没有可靠来源，因此留 0 并在显示时标"未知"——
    // 编造一个数字比留空更糟。
    let mut found = Vec::new();
    for index in 0..8 {
        let key = format!(
            "SYSTEM\\CurrentControlSet\\Control\\Class\\{{4d36e968-e325-11ce-bfc1-08002be10318}}\\{index:04}"
        );
        let Some(name) = registry_string(&key, "DriverDesc") else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        found.push(AdapterInfo {
            name,
            driver_version: registry_string(&key, "DriverVersion").unwrap_or_default(),
            memory_bytes: 0,
        });
    }
    found
}

/// 读 WebView2 运行时版本。
fn webview2_version() -> String {
    // 三个位置都可能存在：机器级（64/32 位视图）与用户级。
    const CLIENT: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    for root in [
        "SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients",
        "SOFTWARE\\Microsoft\\EdgeUpdate\\Clients",
    ] {
        let key = format!("{root}\\{CLIENT}");
        if let Some(version) = registry_string(&key, "pv") {
            if !version.is_empty() {
                return version;
            }
        }
    }
    "未安装（Win11 通常随系统自带）".to_string()
}

/// 从 `HKCU`/`HKLM` 读一个字符串值。先试 HKCU，再试 HKLM。
fn registry_string(subkey: &str, name: &str) -> Option<String> {
    read_registry(subkey, name).and_then(|value| match value {
        RegistryValue::Text(text) => Some(text),
        _ => None,
    })
}

fn registry_u32(subkey: &str, name: &str) -> Option<u32> {
    read_registry(subkey, name).and_then(|value| match value {
        RegistryValue::Number(number) => Some(number),
        RegistryValue::Text(text) => text.trim().parse().ok(),
    })
}

enum RegistryValue {
    Text(String),
    Number(u32),
}

#[cfg(windows)]
fn read_registry(subkey: &str, name: &str) -> Option<RegistryValue> {
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ, REG_DWORD, REG_SZ,
    };

    let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let mut key = HKEY::default();
            if RegOpenKeyExW(
                root,
                windows::core::PCWSTR(subkey_wide.as_ptr()),
                None,
                KEY_READ,
                &mut key,
            )
            .is_err()
            {
                continue;
            }

            let mut buffer = [0u8; 1024];
            let mut size = buffer.len() as u32;
            let mut kind = REG_SZ;
            let ok = RegQueryValueExW(
                key,
                windows::core::PCWSTR(name_wide.as_ptr()),
                None,
                Some(&mut kind),
                Some(buffer.as_mut_ptr()),
                Some(&mut size),
            );
            let _ = RegCloseKey(key);
            if ok.is_err() {
                continue;
            }

            if kind == REG_DWORD {
                let value = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
                return Some(RegistryValue::Number(value));
            }

            let wide: Vec<u16> = buffer[..size as usize]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|c| *c != 0)
                .collect();
            return Some(RegistryValue::Text(String::from_utf16_lossy(&wide)));
        }
    }
    None
}

#[cfg(not(windows))]
fn read_registry(_subkey: &str, _name: &str) -> Option<RegistryValue> {
    None
}

/// 采集失败也不该让诊断导出整体失败，因此提供这个宽松版本。
pub fn collect_soft(app_version: &str, data_root: &str, root_source: &str) -> Environment {
    collect(app_version, data_root, root_source).unwrap_or_else(|error| {
        log::warn!("采集运行环境时出错，导出内容将不完整：{error}");
        let mut environment = Environment::default();
        environment.app_version = app_version.to_string();
        environment.data_root = data_root.to_string();
        environment.root_source = root_source.to_string();
        environment
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_includes_the_sections_a_bug_report_needs() {
        let environment = Environment {
            os: OsVersion {
                major: 10,
                minor: 0,
                build: 26100,
                revision: 1000,
                display: "Windows 11 24H2 build 26100.1000".to_string(),
            },
            adapters: vec![AdapterInfo {
                name: "NVIDIA GeForce RTX 4060".to_string(),
                driver_version: "32.0.15.7270".to_string(),
                memory_bytes: 0,
            }],
            monitors: vec![MonitorInfo {
                name: "\\\\.\\DISPLAY1".to_string(),
                x: 0,
                y: 0,
                width: 3840,
                height: 2160,
                primary: true,
                dpi: 144,
                scale_percent: 150,
                device_path: "\\\\?\\DISPLAY#MON0001#...".to_string(),
            }],
            webview2_version: "154.0.4258.37".to_string(),
            desktop_topology: "RaisedDesktop".to_string(),
            desktop_layer: "挂在 Progman 自身".to_string(),
            current_wallpapers: vec![("dev".to_string(), "C:/x.jpg".to_string())],
            app_version: "0.1.0 (release)".to_string(),
            data_root: "C:/wwp".to_string(),
            root_source: "program-dir".to_string(),
        };

        let text = environment.describe();
        for needle in [
            "程序版本",
            "系统",
            "WebView2",
            "显示器",
            "3840x2160",
            "150%",
            "桌面层",
            "RaisedDesktop",
            "当前壁纸",
        ] {
            assert!(text.contains(needle), "诊断文本缺少 {needle}：\n{text}");
        }
    }

    #[test]
    fn empty_environment_still_renders() {
        // 采集失败时也要能导出（留空比导出失败好），因此空值不能 panic。
        let text = Environment::default().describe();
        assert!(text.contains("WindowsWallPaper 运行环境"));
        assert!(text.contains("（未读到）"));
    }
}

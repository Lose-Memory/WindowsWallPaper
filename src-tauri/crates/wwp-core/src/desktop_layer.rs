//! 桌面层合成器：每屏一个宿主窗口，各自用 DirectComposition 显示内容。
//!
//! 这是 M4 视频壁纸的运行时基础。静态壁纸不需要它——那是 `IDesktopWallpaper` 的活。
//!
//! ## 线程模型（本模块最关键的设计决定）
//!
//! **所有窗口与所有 DirectComposition 对象都在同一个专用线程上创建和使用。**
//!
//! 我实测过"窗口在 A 线程、合成在 B 线程"是能工作的（`CreateTargetForHwnd` 与
//! `Commit` 都成功）。但微软文档并没有承诺 `IDCompositionDevice` 是线程安全的，
//! 而文档里所有示例都是同线程用法；实测能跑不等于有保证。因此这里选择
//! "一条线程、一个消息循环、一套设备"：
//!
//! - 外部（命令线程）通过通道送 [`LayerCommand`] 进来；
//! - 渲染在合成线程上执行，线程内串行，不需要锁保护设备；
//! - 关窗口也在合成线程上做（`DestroyWindow` **只能由创建窗口的线程调用**，
//!   否则会静默失败——窗口留在那里而调用方以为已经销毁了）。
//!
//! 代价是换图要过一次通道。但换图本来就不是高频操作（视频帧率由解码器驱动，
//! 不走这条通道），因此这个代价可以接受，换来的是不需要推理 COM 的线程亲和性。
//!
//! ## 尺寸与坐标（这里踩过两次坑，见下）
//!
//! - **窗口尺寸**取自桌面层自身的客户区，不用虚拟桌面像素并集。
//!   本机前者是 5280x2560（逻辑）、后者会因 DPI 缩放偏大 1.5 倍。
//! - **窗口位置**要先把屏幕坐标换算成**父窗口客户区坐标**。
//!   `ClientToScreen(progman, (0,0))` 给出父窗口客户区原点在屏幕上的位置，
//!   相减即可。之所以不用 `MapWindowPoints`：本 crate 里它只有返回打包 `i32`
//!   的原始版本，要手工符号扩展才能表示负坐标，而本机桌面原点确实在负坐标。
//! - **每屏窗口的尺寸不能取显示器矩形**，理由同上：那是物理像素，
//!   而子窗口的坐标空间是父窗口的（含 DPI 缩放关系）。
//!   本机实测：直接按显示器矩形建窗口，物理结果是对的（Progman 自动缩放），
//!   但这是巧合式的正确。稳妥做法是按"桌面层客户区 / 布局"等比切分。

use std::sync::mpsc::{self, Receiver, Sender};

use windows::core::{Interface, w};
use windows::Win32::Foundation::{GetLastError, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionSurface, IDCompositionTarget,
    IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, HWND_BOTTOM,
    RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, MSG, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_CLOSE, WM_QUIT, WM_USER, WNDCLASSW, WS_CHILD,
    WS_EX_NOACTIVATE,
};

/// 窗口类名。`w!` 宏只接受字面量，因此这里和使用处各写一次，下面做长度核对。
const CLASS_NAME: &str = "WindowsWallPaperDesktopLayer";
const _: () = assert!(
    CLASS_NAME.len() == 28,
    "类名长度变了，请同步修改 CreateWindowExW 里的字面量"
);

/// 保留给"阻塞式消息循环 + PostMessage 唤醒"这一替代方案。
/// 当前用的是 PeekMessage 轮询，因此它暂时没有被使用。
#[allow(dead_code)]
const WM_WWP_WAKE: u32 = WM_USER + 1;

/// 一块屏的宿主窗口与它的合成对象。
struct ScreenHost {
    monitor_name: String,
    /// 该屏的屏幕坐标矩形。保留它是因为"窗口到底铺在哪块屏上"这个问题出过真 bug
    /// （曾因坐标系混淆差 1.5 倍），诊断时要能对着日志核对。
    #[allow(dead_code)]
    screen_rect: (i32, i32, u32, u32),
    /// 合成表面的尺寸。上传像素时要用它判断是否需要缩放。
    surface_size: (u32, u32),
    hwnd: HWND,
    target: IDCompositionTarget,
    visual: IDCompositionVisual,
    surface: IDCompositionSurface,
}

/// 合成线程持有的全部状态。
struct Compositor {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    composition: IDCompositionDevice,
    hosts: Vec<ScreenHost>,
    /// 设备是硬件还是软件（软件设备意味着没有 GPU 加速，帧率会差）。
    /// 只用于构造 [`AttachReport`]，构造完就不再读。
    #[allow(dead_code)]
    hardware: bool,
}

/// 送给合成线程的指令。
enum LayerCommand {
    /// 把某块屏填成指定颜色（`0x00BBGGRR`，即 Windows 的 COLORREF 顺序）。
    ///
    /// 注意这个顺序容易看错：`0x00D14700` 是**红**（B=00 G=47 R=D1），不是蓝。
    /// 我第一版就把调色板的标注写反了，所以在这里把规则写清楚。
    Fill { monitor: String, color: u32 },
    /// 所有屏填同一个颜色。
    FillAll { color: u32 },
    /// 在某块屏上显示**自检图案**（四角不同颜色 + 居中编号条）。
    ///
    /// 存在的理由：我需要一个"人眼一秒就能判定"的验证手段。
    /// 我试过用 DXGI 桌面复制自动取色来验证内容是否上屏，但结果**不可重复**
    /// （同一个自检，一次采到 `#FF00FF`、下一次采到 `#000000`），因此不能作为判据。
    /// 纯色也不好判定——用户无法区分"屏幕是蓝色的"和"屏幕显示了一张蓝色的图"。
    /// 四角四色的图案能同时回答三件事：合成路径是否生效、窗口位置是否正确、
    /// 以及这块屏是哪一块（编号）。
    TestPattern {
        monitor: String,
        /// 该屏编号（1 起）。
        index: u32,
    },
    /// 把一帧紧凑 BGRA 像素显示在某块屏上。**图片与视频共用这条上传路径。**
    Pixels {
        monitor: String,
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// 一次挂载的结果，供调用方核对。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachReport {
    pub topology: String,
    pub parent: isize,
    pub screen_count: usize,
    pub screens: Vec<ScreenReport>,
    pub hardware: bool,
    /// 每块屏的窗口是否正好落在它自己的显示器矩形上（按屏幕坐标核对）。
    pub placement_ok: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenReport {
    pub monitor_name: String,
    pub hwnd: isize,
    /// 期望的屏幕坐标矩形（来自 `EnumDisplayMonitors`）。
    pub expected: (i32, i32, u32, u32),
    /// 实际测到的窗口屏幕坐标矩形。
    pub actual: (i32, i32, i32, i32),
    /// 客户区尺寸（逻辑）。
    pub client_size: (i32, i32),
    pub surface_size: (u32, u32),
    /// DWM 是否认为这个窗口被"遮蔽"（cloaked）。被遮蔽的窗口不会显示任何内容。
    ///
    /// **这是排除项，不是判据**：实测我们的窗口遮蔽值恒为 0，内容却看不见，
    /// 所以"没被遮蔽"不等于"会被显示"。
    pub cloaked: Option<u32>,
    /// 进程级 DWM 合成是否开启。
    ///
    /// **也不是判据**：Win8 之后 DWM 合成始终开启，这个查询几乎总是 true。
    /// 保留它只为让诊断信息完整、排除"系统层面关了合成"这种极端情况。
    pub dwm_composition_enabled: Option<bool>,
    /// 该屏的窗口扩展样式原始值，便于核对 `WS_EX_NOREDIRECTIONBITMAP` 等标志。
    pub ex_style: isize,
}

impl ScreenReport {
    /// 是否被 DWM 遮蔽。
    pub fn is_cloaked(&self) -> bool {
        self.cloaked.map(|c| c != 0).unwrap_or(false)
    }

    /// 是否带 `WS_EX_NOREDIRECTIONBITMAP`。
    ///
    /// 这个标志的含义是"我没有重定向位图，DWM 要单独合成我"。`Progman` 自己带它。
    /// 实测给我们的子窗口加上它之后，可用的自动检测信号**没有变化**，
    /// 因此它是否影响显示仍未确认（见设计规格"未解疑点"）。
    pub fn has_no_redirection_bitmap(&self) -> bool {
        const WS_EX_NOREDIRECTIONBITMAP: isize = 0x0020_0000;
        self.ex_style & WS_EX_NOREDIRECTIONBITMAP != 0
    }
}

/// 宿主窗口的形态。
///
/// **这是一个机制假设，需要实测确认。**
///
/// `Child`（Progman 的子窗口）实现起来最自然：`Progman` 就在这个位置上，
/// 而实测确认了它的子窗口能创建、位置与 z 序都正确（在 `SHELLDLL_DefView` 之下）。
/// 但**它的 DirectComposition 内容没有被任何自动手段看到**。
///
/// 我推断的机制是：**Progman 的子窗口由 Progman 自己绘制，不经过 DWM 的窗口重定向**，
/// 因此 `CreateTargetForHwnd` 对这些子窗口没有合成路径。支持这个推断的证据是
/// 对照实验：**普通顶层窗口的 DComp 内容能被看到**。
///
/// `TopLevel` 就是绕开这个疑点的办法：自己建顶层窗口并压到 z 序最底。
/// 传统 WorkerW 做法本质上是同一思路，只是它借用 Shell 已有的 WorkerW 窗口——
/// 而本机那些 WorkerW 都是 173x34 的不可见遗留窗口，借不到，所以这里自己建一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerMode {
    /// 挂在 Progman 之下。
    Child,
    /// 自己建顶层窗口并压到 z 序底部。
    TopLevel,
}

impl LayerMode {
    /// 供命令行解析。无法识别时返回 `None`，由调用方决定报错还是回退默认值。
    pub fn parse(text: &str) -> Option<LayerMode> {
        match text.to_ascii_lowercase().as_str() {
            "child" => Some(LayerMode::Child),
            "top" | "toplevel" | "top-level" => Some(LayerMode::TopLevel),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LayerMode::Child => "child",
            LayerMode::TopLevel => "top",
        }
    }
}

/// 挂载选项。
#[derive(Debug, Clone, Copy)]
pub struct AttachOptions {
    /// 宿主窗口的形态。见 [`LayerMode`]。
    pub mode: LayerMode,
    /// 是否给宿主窗口加 `WS_EX_NOREDIRECTIONBITMAP`。
    ///
    /// 待验证的假设：`Progman` 自己带这个标志（含义是"我没有重定向位图，
    /// DWM 要单独合成我"）。实测给子窗口加上它之后可用的信号没有变化，
    /// 但"信号没变化"不等于"没有效果"——因为那个信号本身已被证明不可靠。
    pub no_redirection_bitmap: bool,
}

impl Default for AttachOptions {
    fn default() -> AttachOptions {
        AttachOptions {
            // 默认顶层窗口：它符合"内容要经过 DWM 合成"这个已知条件
            // （对照实验证明顶层窗口的 DComp 内容能被看到，子窗口不能）。
            // 见 LayerMode 的说明。
            mode: LayerMode::TopLevel,
            no_redirection_bitmap: false,
        }
    }
}

impl AttachOptions {
    /// 从设置构造。让配置里的 `layer_mode` 真正生效。
    pub fn from_settings(settings: &crate::Settings) -> AttachOptions {
        AttachOptions {
            mode: match settings.general.layer_mode {
                crate::config::LayerModeSetting::Child => LayerMode::Child,
                crate::config::LayerModeSetting::Toplevel => LayerMode::TopLevel,
            },
            no_redirection_bitmap: false,
        }
    }
}

/// 桌面层合成器的句柄。
pub struct DesktopLayer {
    commands: Sender<LayerCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
    report: AttachReport,
}

impl DesktopLayer {
    /// 挂载所有屏的宿主窗口并建立合成。
    ///
    /// 返回前会等合成线程把窗口全部建好，因此拿到它就意味着窗口已存在。
    pub fn attach() -> Result<DesktopLayer, String> {
        DesktopLayer::attach_with(AttachOptions::default())
    }

    /// 带选项挂载。见 [`AttachOptions`]。
    pub fn attach_with(options: AttachOptions) -> Result<DesktopLayer, String> {
        let (report_tx, report_rx) = mpsc::channel::<Result<AttachReport, String>>();
        let (command_tx, command_rx) = mpsc::channel::<LayerCommand>();

        let thread = std::thread::Builder::new()
            .name("wwp-desktop-layer".to_string())
            .spawn(move || {
                if let Err(error) = run_compositor_thread(report_tx, command_rx, options) {
                    log::error!("桌面层合成线程出错：{error}");
                }
            })
            .map_err(|e| format!("创建桌面层线程失败：{e}"))?;

        let report = report_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|e| format!("等待桌面层窗口创建超时：{e}"))??;

        Ok(DesktopLayer {
            commands: command_tx,
            thread: Some(thread),
            report,
        })
    }

    pub fn report(&self) -> &AttachReport {
        &self.report
    }

    /// 把某块屏填成指定颜色（`0x00BBGGRR`）。
    pub fn fill_screen(&self, monitor: &str, color: u32) -> Result<(), String> {
        self.send(LayerCommand::Fill {
            monitor: monitor.to_string(),
            color,
        })
    }

    /// 所有屏填同一个颜色。
    pub fn fill_all(&self, color: u32) -> Result<(), String> {
        self.send(LayerCommand::FillAll { color })
    }

    /// 在某块屏上显示自检图案。见 [`LayerCommand::TestPattern`] 的用途说明。
    pub fn show_test_pattern(&self, monitor: &str, index: u32) -> Result<(), String> {
        self.send(LayerCommand::TestPattern {
            monitor: monitor.to_string(),
            index,
        })
    }

    /// 把一帧紧凑 BGRA 像素显示在某块屏上。
    ///
    /// 这是**图片与视频共用的上传路径**。像素必须紧凑排列（每行 `width * 4` 字节）。
    ///
    /// **尺寸必须与这块屏的合成表面一致**。尺寸不一致时直接报错而不是在这里缩放——
    /// 理由是性能：CPU 最近邻缩放 1920x1080 → 3840x2160 实测要 100 ms 以上，
    /// 而本函数跑在**合成线程**上，把它拖住会让窗口消息与后续帧全部排队。
    /// 缩放应当在调用方做好（视频路径在解码线程上缩放，图片路径在准备阶段缩放）。
    pub fn present_pixels(
        &self,
        monitor: &str,
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        self.send(LayerCommand::Pixels {
            monitor: monitor.to_string(),
            pixels,
            width,
            height,
        })
    }

    fn send(&self, command: LayerCommand) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "桌面层合成线程已退出".to_string())
    }

    /// 关闭所有窗口并等待线程退出。
    pub fn close(mut self) -> Result<(), String> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), String> {
        // 丢掉命令发送端会让合成线程的 recv 返回 Err，从而走收尾流程。
        let (dead, _) = mpsc::channel();
        let _ = std::mem::replace(&mut self.commands, dead);

        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(()) => {
                    log::info!("桌面层已关闭，所有宿主窗口已销毁");
                    Ok(())
                }
                Err(_) => Err("桌面层线程 panic".to_string()),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for DesktopLayer {
    fn drop(&mut self) {
        // 忘了 close 也要保证窗口被销毁——留下挂在桌面层的窗口，
        // 用户看到的就是一块永远擦不掉的色块。
        if self.thread.is_some() {
            let _ = self.close_inner();
        }
    }
}

/// 合成线程主体：建窗口、建设备、然后串行处理指令。
fn run_compositor_thread(
    ready: Sender<Result<AttachReport, String>>,
    commands: Receiver<LayerCommand>,
    options: AttachOptions,
) -> Result<(), String> {
    // 1. 桌面层拓扑。
    //
    // Child 模式需要一个父窗口（Progman 本身）；TopLevel 模式不需要父窗口，
    // 但仍要读拓扑以便诊断信息里说明"我们绕开了什么"。
    let probe = crate::desktop::probe_desktop();
    let parent_raw = match options.mode {
        LayerMode::Child => match probe.wallpaper_layer {
            Some(handle) => handle,
            None => {
                let message = format!("找不到桌面层窗口（拓扑 {:?}）", probe.topology);
                let _ = ready.send(Err(message.clone()));
                return Err(message);
            }
        },
        // TopLevel 模式没有父窗口，用 0 表示。
        LayerMode::TopLevel => 0,
    };

    // 2. 注册窗口类
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("WindowsWallPaperDesktopLayer"),
            ..Default::default()
        };
        RegisterClassW(&class);
    }

    // 3. 硬件 D3D 设备。失败时退回软件设备——没有 GPU 加速也要能用，
    //    只是帧率会差；直接失败会让"虚拟机/远程桌面"这类环境完全不可用。
    let (device, context, hardware) = create_d3d_device()?;
    let composition: IDCompositionDevice = {
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("ID3D11Device 转 IDXGIDevice 失败：{e}"))?;
        unsafe { DCompositionCreateDevice(&dxgi) }
            .map_err(|e| format!("创建 DirectComposition 设备失败：{e}"))?
    };
    log::info!(
        "图形设备已就绪（{}），拓扑 {:?}",
        if hardware { "硬件" } else { "软件" },
        probe.topology
    );

    // 4. 每屏建窗口与合成对象
    let monitors = crate::monitors::enumerate().map_err(|e| format!("枚举显示器失败：{e}"))?;
    if monitors.is_empty() {
        let message = "没有检测到显示器".to_string();
        let _ = ready.send(Err(message.clone()));
        return Err(message);
    }

    let mut hosts: Vec<ScreenHost> = Vec::new();
    let mut reports: Vec<ScreenReport> = Vec::new();
    let mut placement_ok = true;

    // 父窗口客户区原点与尺寸。只有 Child 模式需要它们：
    // 子窗口的坐标空间是父窗口客户区，而顶层窗口直接用屏幕坐标。
    // 只保留原点：子窗口需要它来换算坐标，尺寸不再需要（尺寸统一取显示器矩形）。
    let parent_origin = match options.mode {
        LayerMode::Child => {
            let parent = HWND(parent_raw as *mut _);
            let mut origin = POINT { x: 0, y: 0 };
            if !unsafe { ClientToScreen(parent, &mut origin) }.as_bool() {
                let message = "取不到桌面层客户区原点，无法定位窗口".to_string();
                let _ = ready.send(Err(message.clone()));
                return Err(message);
            }
            let mut client = RECT::default();
            unsafe { GetClientRect(parent, &mut client) }
                .map_err(|e| format!("取不到桌面层客户区：{e}"))?;
            log::info!(
                "桌面层客户区原点在屏幕 ({},{})，客户区 {}x{}",
                origin.x,
                origin.y,
                client.right - client.left,
                client.bottom - client.top
            );
            origin
        }
        LayerMode::TopLevel => {
            // 顶层窗口直接用屏幕坐标，不需要父窗口原点。
            log::info!("使用顶层窗口模式：每屏一个顶层窗口，压到 z 序底部");
            POINT { x: 0, y: 0 }
        }
    };

    for monitor in &monitors {
        // 位置：Child 模式要换算成父窗口客户区坐标；TopLevel 模式直接用屏幕坐标。
        let (window_x, window_y) = match options.mode {
            LayerMode::Child => (monitor.x - parent_origin.x, monitor.y - parent_origin.y),
            LayerMode::TopLevel => (monitor.x, monitor.y),
        };
        // 尺寸：直接用显示器矩形，缩放交给系统（见 screen_client_size 的实测说明）。
        let (client_width, client_height) = screen_client_size(monitor);

        // 扩展样式：EX_NOACTIVATE 是必需的（否则点桌面会把焦点丢给壁纸）；
        // NOREDIRECTIONBITMAP 按选项加（见 AttachOptions 的说明）。
        let mut ex_style = WS_EX_NOACTIVATE;
        if options.no_redirection_bitmap {
            ex_style |= windows::Win32::UI::WindowsAndMessaging::WS_EX_NOREDIRECTIONBITMAP;
        }

        // 普通样式：子窗口用 WS_CHILD；顶层窗口用 WS_POPUP（无边框无标题的顶层窗口）。
        //
        // 用 WS_POPUP 而不是 WS_OVERLAPPEDWINDOW：后者会带标题栏与边框，
        // 铺在桌面上会露出这些装饰。
        let (style, parent_for_create) = match options.mode {
            LayerMode::Child => (WS_CHILD, Some(HWND(parent_raw as *mut _))),
            LayerMode::TopLevel => (
                windows::Win32::UI::WindowsAndMessaging::WS_POPUP,
                None,
            ),
        };

        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                w!("WindowsWallPaperDesktopLayer"),
                // 标题里带显示器名，便于用工具认窗口。
                windows::core::PCWSTR(
                    format!("WindowsWallPaper 桌面层 {}", monitor.name)
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect::<Vec<u16>>()
                        .as_ptr(),
                ),
                style,
                window_x,
                window_y,
                client_width,
                client_height,
                parent_for_create,
                None,
                // hInstance 必须不传：显式传本模块 handle 会报 ERROR_INVALID_HANDLE(6)。
                None,
                None,
            )
        };

        let hwnd = match hwnd {
            Ok(h) => h,
            Err(error) => {
                let last = unsafe { GetLastError() };
                log::error!(
                    "为 {} 建窗口失败：{}（GetLastError={}）；继续处理其余屏",
                    monitor.name,
                    error.message(),
                    last.0
                );
                continue;
            }
        };

        // 每屏一套 target/visual/surface。topmost=true 让合成内容盖在窗口自身绘制之上。
        let target = match unsafe { composition.CreateTargetForHwnd(hwnd, true) } {
            Ok(t) => t,
            Err(error) => {
                log::error!("{} 建合成目标失败：{}", monitor.name, error.message());
                unsafe {
                    let _ = DestroyWindow(hwnd);
                };
                continue;
            }
        };
        let visual = match unsafe { composition.CreateVisual() } {
            Ok(v) => v,
            Err(error) => {
                log::error!("{} 建视觉对象失败：{}", monitor.name, error.message());
                unsafe {
                    let _ = DestroyWindow(hwnd);
                };
                continue;
            }
        };
        let surface = match unsafe {
            composition.CreateSurface(
                client_width as u32,
                client_height as u32,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_IGNORE,
            )
        } {
            Ok(s) => s,
            Err(error) => {
                log::error!("{} 建合成表面失败：{}", monitor.name, error.message());
                unsafe {
                    let _ = DestroyWindow(hwnd);
                };
                continue;
            }
        };

        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }

        // z 序。
        //
        // Child 模式：插到 `SHELLDLL_DefView` **之下**，即"图标层下面"。
        // 用 `SetWindowPos` 的 `hWndInsertAfter` 参数表达"排在这个窗口之后
        // （也就是显示在它下面）"。同时把 Shell 自己的壁纸窗口压到最底，
        // 避免它盖住我们。
        //
        // TopLevel 模式：先压到 z 序最底（`HWND_BOTTOM`），再把自己插到
        // `Progman` **之下**——这样 Progman（桌面图标）仍在我们上面，
        // 而我们在所有普通窗口之下。顺序很重要：只压到底会让 Progman 也跑到
        // 我们上面或下面，结果不确定。
        match options.mode {
            LayerMode::Child => {
                if let Some(workerw) = probe
                    .progman_children
                    .iter()
                    .find(|child| child.class_name == "WorkerW")
                    .map(|child| child.hwnd)
                {
                    let _ = unsafe {
                        SetWindowPos(
                            HWND(workerw as *mut _),
                            Some(HWND_BOTTOM),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    };
                }
                if let Some(defview) = probe.defview {
                    if let Err(error) = unsafe {
                        SetWindowPos(
                            hwnd,
                            Some(HWND(defview as *mut _)),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        )
                    } {
                        log::warn!("调整 z 序失败（窗口已创建，可能被其他层盖住）：{error}");
                    }
                }
            }
            LayerMode::TopLevel => {
                // 先压到底，再插到 Progman 之下。
                let mut insert_after = HWND_BOTTOM;
                if let Ok(progman) = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::FindWindowW(w!("Progman"), None)
                } {
                    insert_after = progman;
                }
                if let Err(error) = unsafe {
                    SetWindowPos(
                        hwnd,
                        Some(insert_after),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    )
                } {
                    log::warn!("调整顶层窗口 z 序失败：{error}");
                }
            }
        }

        // 核对：窗口实际落在哪里（物理/屏幕坐标）。
        let mut rect = RECT::default();
        let actual = match unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) } {
            Ok(()) => (
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ),
            Err(_) => (0, 0, 0, 0),
        };
        let expected = (monitor.x, monitor.y, monitor.width, monitor.height);
        // 尺寸允许多屏拼接造成的取整误差（这里放宽到 2 像素）。
        let ok = actual.0 == expected.0
            && actual.1 == expected.1
            && (actual.2 - expected.2 as i32).abs() <= 2
            && (actual.3 - expected.3 as i32).abs() <= 2;
        if !ok {
            placement_ok = false;
            log::warn!(
                "{} 的窗口位置与显示器矩形不符：期望 ({},{}) {}x{}，实际 ({},{}) {}x{}",
                monitor.name,
                expected.0,
                expected.1,
                expected.2,
                expected.3,
                actual.0,
                actual.1,
                actual.2,
                actual.3
            );
        }

        log::info!(
            "{} 宿主窗口 0x{:X} 客户区 {}x{}，屏幕矩形 ({},{}) {}x{}，表面 {}x{}",
            monitor.name,
            hwnd.0 as isize,
            client_width,
            client_height,
            actual.0,
            actual.1,
            actual.2,
            actual.3,
            client_width,
            client_height
        );

        reports.push(ScreenReport {
            monitor_name: monitor.name.clone(),
            hwnd: hwnd.0 as isize,
            expected,
            actual,
            client_size: (client_width, client_height),
            surface_size: (client_width as u32, client_height as u32),
            cloaked: query_cloaked(hwnd),
            dwm_composition_enabled: query_dwm_composition(hwnd),
            ex_style: unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE,
                )
            },
        });

        hosts.push(ScreenHost {
            monitor_name: monitor.name.clone(),
            screen_rect: (monitor.x, monitor.y, monitor.width, monitor.height),
            surface_size: (client_width as u32, client_height as u32),
            hwnd,
            target,
            visual,
            surface,
        });
    }

    if hosts.is_empty() {
        let message = "所有显示器都未能建立宿主窗口".to_string();
        let _ = ready.send(Err(message.clone()));
        return Err(message);
    }

    let mut compositor = Compositor {
        device,
        context,
        composition,
        hosts,
        hardware,
    };

    // 初始状态：全部填成深色（桌面层不该在启动瞬间露出上一帧的残留或纯黑，
    // 深灰比纯黑更接近常见壁纸的暗部，视觉上不那么突兀）。
    compositor.fill_all(0x0020_2020)?;

    let report = AttachReport {
        topology: format!("{:?}", probe.topology),
        parent: parent_raw,
        screen_count: compositor.hosts.len(),
        screens: reports,
        hardware,
        placement_ok,
    };
    let _ = ready.send(Ok(report));

    // 5. 消息循环 + 指令处理。
    //
    // 用非阻塞的 PeekMessage 轮询而不是阻塞式 GetMessage + 通道回调：
    // 这样同一条线程既能处理窗口消息又能处理我们的指令，不需要在
    // wndproc 里再做一层分发（wndproc 是 C 函数指针，拿不到闭包状态）。
    loop {
        // 先处理窗口消息（PeekMessage 不留消息给别的线程）。
        unsafe {
            let mut message = MSG::default();
            while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                &mut message,
                None,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
            )
            .as_bool()
            {
                if message.message == WM_QUIT {
                    shutdown(&mut compositor);
                    return Ok(());
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        // 再处理指令；超时 50 ms 让消息循环保持响应（约 20 Hz，换图足够快）。
        match commands.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(LayerCommand::Fill { monitor, color }) => {
                if let Err(error) = compositor.fill_screen(&monitor, color) {
                    log::error!("填充 {monitor} 失败：{error}");
                }
            }
            Ok(LayerCommand::FillAll { color }) => {
                if let Err(error) = compositor.fill_all(color) {
                    log::error!("填充所有屏失败：{error}");
                }
            }
            Ok(LayerCommand::TestPattern { monitor, index }) => {
                if let Err(error) = compositor.show_test_pattern(&monitor, index) {
                    log::error!("在 {monitor} 上显示自检图案失败：{error}");
                }
            }
            Ok(LayerCommand::Pixels {
                monitor,
                pixels,
                width,
                height,
            }) => {
                if let Err(error) = compositor.present_pixels_on(&monitor, &pixels, width, height) {
                    log::error!("在 {monitor} 上显示像素失败：{error}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::info!("桌面层收到关闭请求");
                shutdown(&mut compositor);
                return Ok(());
            }
        }
    }
}

/// 算出一块屏的宿主窗口应当使用的尺寸。
///
/// ## 结论：直接用显示器矩形，让系统去缩放
///
/// 这里我原本写了个"按显示器在布局里的占比去乘父窗口客户区尺寸"的版本，
/// 理由是"显示器给的是物理像素，而父窗口客户区是逻辑尺寸，两者差一个 DPI 缩放"。
/// 那个版本实测也对，但它的正确是**巧合**：本机上两个算法给出相同结果。
///
/// 实测确认的真实机制是：**系统按父窗口（Progman）的 DPI 上下文缩放子窗口**。
/// 本机两块屏的证据：
///
/// | 屏 | 建窗口用的尺寸（逻辑） | 系统给出的物理矩形 | 缩放 |
/// |----|----------------------|-------------------|------|
/// | DISPLAY1 | 3840x2160 | (0,0) 2560x1440 | 1/1.5 |
/// | DISPLAY2 | 1440x2560 | (-960,-62) 960x1707 | 1/1.5 |
///
/// 两者都落在各自显示器上，`placement_ok` 为 true。因此我们只负责报**逻辑尺寸**，
/// 缩放是系统的事——这正是"子窗口活在父窗口坐标空间里"的直接后果。
///
/// 之所以不再"按占比算"：那个算法要推断缩放关系，而推断出来的关系在本机恰好与
/// 真实情况一致，换个屏的排列或缩放就可能不一致。少推断一件事就少一个出错点。
///
/// 兜底仍然保留：算不出正尺寸时用 1x1，总比 0 好（0 会让窗口创建失败或不可见，
/// 那比"窗口偏小"更难排查）。
fn screen_client_size(monitor: &crate::monitors::Monitor) -> (i32, i32) {
    let width = monitor.width as i32;
    let height = monitor.height as i32;
    if width > 0 && height > 0 {
        (width, height)
    } else {
        log::warn!("{} 的显示器矩形为 0，退回 1x1", monitor.name);
        (1, 1)
    }
}

/// 建 D3D11 设备。优先硬件，失败退回软件。
fn create_d3d_device() -> Result<(ID3D11Device, ID3D11DeviceContext, bool), String> {
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;

    for (driver, hardware) in [
        (D3D_DRIVER_TYPE_HARDWARE, true),
        (D3D_DRIVER_TYPE_WARP, false),
    ] {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let result = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match result {
            Ok(()) => {
                if let (Some(device), Some(context)) = (device, context) {
                    return Ok((device, context, hardware));
                }
            }
            Err(error) => {
                log::warn!(
                    "创建{} D3D11 设备失败：{}",
                    if hardware { "硬件" } else { "软件(WARP)" },
                    error.message()
                );
            }
        }
    }
    Err("硬件与软件 D3D11 设备都创建失败".to_string())
}

impl Compositor {
    /// 在某块屏上显示自检图案。
    fn show_test_pattern(&self, monitor: &str, index: u32) -> Result<(), String> {
        let host = self
            .hosts
            .iter()
            .find(|h| h.monitor_name == monitor)
            .ok_or_else(|| format!("没有名为 {monitor} 的宿主窗口"))?;
        let (width, height) = host.surface_size;
        let pixels = Self::test_pattern(index, width, height);
        self.present_pixels(host, &pixels, width, height)
    }

    /// 按显示器名找宿主窗口并显示像素。
    fn present_pixels_on(
        &self,
        monitor: &str,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let host = self
            .hosts
            .iter()
            .find(|h| h.monitor_name == monitor)
            .ok_or_else(|| format!("没有名为 {monitor} 的宿主窗口"))?;
        self.present_pixels(host, pixels, width, height)
    }

    /// 生成自检图案的像素（紧凑 BGRA）。
    ///
    /// 图案设计的目标是"人眼一秒判定 + 无法与壁纸混淆"：
    /// - 中间一条深灰底、亮色块组成的横带，横带里画该屏编号的方块；
    /// - 四角各一个纯色大方块（青/黄/绿/洋红）——如果只看到其中一部分，
    ///   就能判断窗口没铺满或位置偏了；
    /// - 其余区域用接近黑的深灰，避免被误认为"这就是我的壁纸"。
    fn test_pattern(index: u32, width: u32, height: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        // 先整体填深灰 (B=20 G=20 R=20)
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = 20;
            chunk[1] = 20;
            chunk[2] = 20;
            chunk[3] = 255;
        }

        let mut fill_rect = |x0: i32, y0: i32, w: i32, h: i32, b: u8, g: u8, r: u8| {
            let x1 = (x0 + w).min(width as i32);
            let y1 = (y0 + h).min(height as i32);
            for y in y0.max(0)..y1 {
                for x in x0.max(0)..x1 {
                    let offset = (y as usize * width as usize + x as usize) * 4;
                    pixels[offset] = b;
                    pixels[offset + 1] = g;
                    pixels[offset + 2] = r;
                    pixels[offset + 3] = 255;
                }
            }
        };

        // 四角的纯色方块。边长取屏短边的 12%，小屏上也看得见。
        let corner = ((width.min(height) as f32) * 0.12) as i32;
        let corner = corner.max(40);
        // 左上：青 (B=255 G=255 R=0)
        fill_rect(0, 0, corner, corner, 255, 255, 0);
        // 右上：黄 (B=0 G=255 R=255)
        fill_rect(width as i32 - corner, 0, corner, corner, 0, 255, 255);
        // 左下：绿 (B=0 G=255 R=0)
        fill_rect(0, height as i32 - corner, corner, corner, 0, 255, 0);
        // 右下：洋红 (B=255 G=0 R=255)
        fill_rect(width as i32 - corner, height as i32 - corner, corner, corner, 255, 0, 255);

        // 中间横带：便于确认窗口的垂直位置正确。
        let band_height = (height as f32 * 0.10) as i32;
        let band_height = band_height.max(30);
        fill_rect(
            0,
            height as i32 / 2 - band_height / 2,
            width as i32,
            band_height,
            40,
            40,
            40,
        );

        // 横带里画编号方块（1 起）。
        //
        // 编号用"几个白方块"表示，而不是画字形：画字形要带字体渲染，
        // 而这一步只是想确认"哪块屏是哪一块"，方块数量足够了。
        let block = band_height / 2;
        let gap = block / 2;
        let total = index as i32 * block + (index as i32 - 1) * gap;
        let mut x = width as i32 / 2 - total / 2;
        for _ in 0..index {
            fill_rect(
                x,
                height as i32 / 2 - block / 2,
                block,
                block,
                255,
                255,
                255,
            );
            x += block + gap;
        }

        pixels
    }

    /// 把一帧像素拷进某块屏的表面。
    ///
    /// 实现要点：
    /// - 先建一张 `D3D11_USAGE_DEFAULT` 的纹理，把像素放进去（`pSysMem` + `SysMemPitch`）；
    /// - 再用 `CopyResource` 拷到 `BeginDraw` 拿到的表面上。
    ///
    /// 之所以不直接往表面上写：`BeginDraw` 给的 `IDXGISurface` 只保证"可以用来画"，
    /// 而 `CopyResource` 是 D3D11 里最直接的"整块搬运"方式，语义明确、不需要逐行处理跨距。
    ///
    /// **不做缩放**：尺寸不匹配时报错（原因见 [`DesktopLayer::present_pixels`] 的说明）。
    /// 这一层只负责搬运，任何 CPU 密集的工作都不该放在合成线程上。
    fn present_pixels(
        &self,
        host: &ScreenHost,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("像素尺寸为 0".to_string());
        }
        let expected = width as usize * height as usize * 4;
        if pixels.len() < expected {
            return Err(format!(
                "像素数据不足：需要 {expected} 字节，实际 {} 字节",
                pixels.len()
            ));
        }

        let (surface_width, surface_height) = host.surface_size;
        if width != surface_width || height != surface_height {
            // 明确报出来，而不是默默缩放在合成线程上——那样会拖住消息循环，
            // 表现为"播放一会儿就开始卡、拖动窗口没反应"。
            return Err(format!(
                "像素尺寸 {width}x{height} 与 {} 的合成表面 {surface_width}x{surface_height} 不一致；\
                 请在调用方先缩放好",
                host.monitor_name
            ));
        }

        let mut offset = POINT::default();
        let dxgi_surface: IDXGISurface = unsafe {
            host.surface
                .BeginDraw::<IDXGISurface>(None, &mut offset)
                .map_err(|e| format!("BeginDraw 失败：{}", e.message()))?
        };
        let target_texture: ID3D11Texture2D = dxgi_surface
            .cast()
            .map_err(|e| format!("取 D3D11 纹理失败：{e}"))?;

        // 建源纹理并上传像素
        let desc = D3D11_TEXTURE2D_DESC {
            Width: surface_width,
            Height: surface_height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr() as *const _,
            SysMemPitch: surface_width * 4,
            SysMemSlicePitch: 0,
        };
        let mut source: Option<ID3D11Texture2D> = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&initial), Some(&mut source))
                .map_err(|e| format!("建上传纹理失败：{}", e.message()))?;
        }
        let source = source.ok_or_else(|| "建上传纹理返回空".to_string())?;
        unsafe {
            self.context.CopyResource(&target_texture, &source);
            self.context.Flush();
        }
        unsafe {
            host.surface
                .EndDraw()
                .map_err(|e| format!("EndDraw 失败：{}", e.message()))?;
            host.visual
                .SetContent(&host.surface)
                .map_err(|e| format!("SetContent 失败：{}", e.message()))?;
            host.target
                .SetRoot(&host.visual)
                .map_err(|e| format!("SetRoot 失败：{}", e.message()))?;
            self.composition
                .Commit()
                .map_err(|e| format!("Commit 失败：{}", e.message()))?;
        }
        Ok(())
    }

    /// 把某块屏填成指定颜色。
    fn fill_screen(&self, monitor: &str, color: u32) -> Result<(), String> {
        let host = self
            .hosts
            .iter()
            .find(|h| h.monitor_name == monitor)
            .ok_or_else(|| format!("没有名为 {monitor} 的宿主窗口"))?;
        self.fill_host(host, color)
    }

    /// 所有屏填同一个颜色。
    fn fill_all(&self, color: u32) -> Result<(), String> {
        let mut first_error = None;
        for host in &self.hosts {
            if let Err(error) = self.fill_host(host, color) {
                log::error!("填充 {} 失败：{error}", host.monitor_name);
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 往一块屏的表面填色。
    ///
    /// `color` 是 `0x00BBGGRR`（Windows 的 COLORREF 约定），
    /// 而 `ClearRenderTargetView` 要的是归一化 RGBA，因此这里要拆字节并翻转顺序。
    fn fill_host(&self, host: &ScreenHost, color: u32) -> Result<(), String> {
        let r = (color & 0xFF) as f32 / 255.0;
        let g = ((color >> 8) & 0xFF) as f32 / 255.0;
        let b = ((color >> 16) & 0xFF) as f32 / 255.0;

        let mut offset = POINT::default();
        let dxgi_surface: IDXGISurface = unsafe {
            host.surface
                .BeginDraw::<IDXGISurface>(None, &mut offset)
                .map_err(|e| format!("BeginDraw 失败：{}", e.message()))?
        };
        let texture: ID3D11Texture2D = dxgi_surface
            .cast()
            .map_err(|e| format!("取 D3D11 纹理失败：{e}"))?;
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        unsafe {
            self.device
                .CreateRenderTargetView(&texture, None, Some(&mut rtv))
                .map_err(|e| format!("建渲染目标失败：{}", e.message()))?;
        }
        let rtv = rtv.ok_or_else(|| "建渲染目标返回了空指针".to_string())?;
        unsafe {
            self.context.ClearRenderTargetView(&rtv, &[r, g, b, 1.0]);
            // 把命令交给 GPU 之后再 EndDraw，否则 EndDraw 可能拿到还没画完的表面。
            self.context.Flush();
        }
        unsafe {
            host.surface
                .EndDraw()
                .map_err(|e| format!("EndDraw 失败：{}", e.message()))?;
        }

        unsafe {
            host.visual
                .SetContent(&host.surface)
                .map_err(|e| format!("SetContent 失败：{}", e.message()))?;
            host.target
                .SetRoot(&host.visual)
                .map_err(|e| format!("SetRoot 失败：{}", e.message()))?;
        }
        unsafe {
            self.composition
                .Commit()
                .map_err(|e| format!("Commit 失败：{}", e.message()))?;
        }
        Ok(())
    }
}

/// 最近邻缩放（紧凑 BGRA）。
///
/// **公开**是为了让调用方能在自己的线程上缩放，而不是让合成线程去做
/// （合成线程被拖住会连带窗口消息一起排队，表现为播放卡顿、拖动无响应）。
///
/// 用最近邻而不是双线性：壁纸场景下真正该做的是在 `image_prep` 阶段按屏幕尺寸
/// 裁切；走到这里缩放的多数是"视频分辨率与屏幕不一致"这种临时情况。
/// 将来接 GPU 缩放时替换这一个函数即可。
pub fn scale_nearest(source: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 4];
    let src_row = src_w as usize * 4;
    for y in 0..dst_h as usize {
        // 目标行映射回源行（用乘法再除，避免累加误差随行数放大）
        let sy = (y as u64 * src_h as u64 / dst_h as u64) as usize;
        let sy = sy.min(src_h as usize - 1);
        let src_offset = sy * src_row;
        let dst_offset = y * dst_w as usize * 4;
        for x in 0..dst_w as usize {
            let sx = (x as u64 * src_w as u64 / dst_w as u64) as usize;
            let sx = sx.min(src_w as usize - 1);
            let s = src_offset + sx * 4;
            let d = dst_offset + x * 4;
            if s + 3 < source.len() {
                out[d..d + 4].copy_from_slice(&source[s..s + 4]);
            }
        }
    }
    out
}

/// 查 DWM 是否认为这个窗口被遮蔽（cloaked）。
///
/// 被遮蔽的窗口 DWM 不会合成，因此 DirectComposition 的内容也不会出现。
/// 这是排查"我们画了但看不见"时要先排除的一项。
/// 查询失败时返回 `None`（例如窗口已经被销毁）。
fn query_cloaked(hwnd: HWND) -> Option<u32> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut value: u32 = 0;
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut value as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    };
    match result {
        Ok(()) => Some(value),
        Err(error) => {
            log::debug!("查询 DWMWA_CLOAKED 失败：{}", error.message());
            None
        }
    }
}

/// 查进程级 DWM 合成是否开启。
///
/// Win8 之后 DWM 合成始终开启，因此这个查询几乎总是返回 true。
/// 保留它是为了让诊断信息完整（排除"系统层面关了合成"这种极端情况），
/// 但它**不能**用来判断"某个窗口的内容会不会显示"。
///
/// 我一度用 `DWMWA_EXTENDED_FRAME_BOUNDS` 是否可查询来判断"DWM 认不认得这个窗口"，
/// 并据此推断"DWM 没合成我们的窗口"。**那个推断是错的**：加了
/// `WS_EX_NOREDIRECTIONBITMAP` 之后该查询依然失败，说明它失败的原因是
/// **对象是子窗口**，而不是 DWM 不合成它。教训是：一个信号能区分两种状态，
/// 不等于它能证明我们想知道的那件事。
fn query_dwm_composition(_hwnd: HWND) -> Option<bool> {
    // 这个重载直接返回 BOOL（另一个重载才是输出参数形式）。
    use windows::Win32::Graphics::Dwm::DwmIsCompositionEnabled;
    match unsafe { DwmIsCompositionEnabled() } {
        Ok(enabled) => Some(enabled.as_bool()),
        Err(error) => {
            log::debug!("查询 DwmIsCompositionEnabled 失败：{}", error.message());
            None
        }
    }
}

/// 收尾：销毁所有窗口。
///
/// 必须在创建窗口的线程上调用（本函数只从合成线程调用）。
fn shutdown(compositor: &mut Compositor) {
    for host in &compositor.hosts {
        unsafe {
            let _ = DestroyWindow(host.hwnd);
        }
    }
    log::info!("已销毁 {} 个桌面层宿主窗口", compositor.hosts.len());
    compositor.hosts.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitors::Monitor;

    fn monitor(name: &str, x: i32, y: i32, w: u32, h: u32, primary: bool, dpi: u32) -> Monitor {
        Monitor {
            name: name.to_string(),
            x,
            y,
            width: w,
            height: h,
            primary,
            dpi,
        }
    }

    #[test]
    fn test_pattern_marks_all_four_corners_with_distinct_colors() {
        // 图案的用途是让人眼一秒判定"内容上屏了没有"，因此四角必须是
        // **四种互相不同**的纯色，而且四角位置都要被画到。
        let (w, h) = (400u32, 300u32);
        let pixels = Compositor::test_pattern(1, w, h);
        assert_eq!(pixels.len(), (w * h * 4) as usize);

        let at = |x: u32, y: u32| -> (u8, u8, u8) {
            let offset = (y as usize * w as usize + x as usize) * 4;
            (pixels[offset], pixels[offset + 1], pixels[offset + 2])
        };

        // 四角颜色（BGRA 里的 B,G,R）
        let top_left = at(5, 5);
        let top_right = at(w - 6, 5);
        let bottom_left = at(5, h - 6);
        let bottom_right = at(w - 6, h - 6);

        for (name, color) in [
            ("左上", top_left),
            ("右上", top_right),
            ("左下", bottom_left),
            ("右下", bottom_right),
        ] {
            assert_ne!(
                color,
                (20, 20, 20),
                "{name} 角没有被画上颜色（还是底色），图案无法用于判定"
            );
        }

        let corners = [top_left, top_right, bottom_left, bottom_right];
        for i in 0..corners.len() {
            for j in (i + 1)..corners.len() {
                assert_ne!(
                    corners[i], corners[j],
                    "第 {i} 与第 {j} 个角的颜色相同，无法判断窗口是否铺满"
                );
            }
        }
    }

    #[test]
    fn test_pattern_shows_the_screen_number_as_more_blocks() {
        // 编号方块数量应当随 index 增加，这样多屏时能分辨哪块是哪块。
        let (w, h) = (600u32, 400u32);
        let count_white_in_band = |index: u32| -> usize {
            let pixels = Compositor::test_pattern(index, w, h);
            // 只统计中间横带那一行
            let y = h / 2;
            let mut count = 0;
            for x in 0..w {
                let offset = (y as usize * w as usize + x as usize) * 4;
                let (b, g, r) = (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
                if b == 255 && g == 255 && r == 255 {
                    count += 1;
                }
            }
            count
        };
        let one = count_white_in_band(1);
        let three = count_white_in_band(3);
        assert!(one > 0, "第 1 块屏应当有编号方块");
        assert!(
            three > one,
            "第 3 块屏的白色像素（{three}）应当多于第 1 块屏（{one}）"
        );
    }

    #[test]
    fn scale_nearest_keeps_size_and_sampling_within_bounds() {
        // 缩放要给出正确尺寸，且不能越界读取（源比目标小的时候最容易越界）。
        let src = vec![128u8; 4 * 2 * 4]; // 4x2 BGRA
        let out = scale_nearest(&src, 4, 2, 8, 4);
        assert_eq!(out.len(), 8 * 4 * 4);

        // 放大到很大也不能 panic 或越界
        let out = scale_nearest(&src, 4, 2, 100, 100);
        assert_eq!(out.len(), 100 * 100 * 4);

        // 缩小
        let out = scale_nearest(&src, 4, 2, 2, 1);
        assert_eq!(out.len(), 2 * 1 * 4);

        // 尺寸为 0 时返回空而不是 panic
        assert!(scale_nearest(&src, 0, 2, 4, 4).is_empty());
        assert!(scale_nearest(&src, 4, 2, 0, 0).is_empty());
    }

    #[test]
    fn screen_size_uses_the_monitor_rect_and_lets_the_system_scale() {
        // 这条测试固定住一个反直觉但正确的做法。
        //
        // 我原本打算"按显示器在布局里的占比去乘父窗口客户区尺寸"，理由是
        // 显示器给的是物理像素而父窗口客户区是逻辑尺寸，两者差一个 DPI 缩放。
        // 实测证明：**直接用显示器尺寸建窗口是对的**——
        // 系统会按父窗口（Progman）的 DPI 上下文把逻辑尺寸缩放到该屏的物理像素。
        //
        // 本机实测（主屏 150% 缩放）：
        //   DISPLAY1 建窗口用 3840x2160（逻辑）→ 物理矩形 (0,0) 2560x1440    ✓
        //   DISPLAY2 建窗口用 1440x2560（逻辑）→ 物理矩形 (-960,-62) 960x1707  ✓
        // Progman 客户区是 5280x2560 逻辑，被系统缩放到 3520x1707 物理像素。
        // 两者都落在各自显示器上，`placement_ok` 为 true。
        let all = vec![
            monitor(r"\\.\DISPLAY1", 0, 0, 3840, 2160, true, 144),
            monitor(r"\\.\DISPLAY2", -1440, -93, 1440, 2560, false, 96),
        ];
        // parent_origin 不再参与计算（缩放由系统负责），保留注释说明历史。

        let (w1, h1) = screen_client_size(&all[0]);
        assert_eq!(
            (w1, h1),
            (3840, 2160),
            "DISPLAY1 的窗口尺寸应当就是它的显示器矩形（缩放交给系统）"
        );

        let (w2, h2) = screen_client_size(&all[1]);
        assert_eq!(
            (w2, h2),
            (1440, 2560),
            "DISPLAY2 的窗口尺寸应当就是它的显示器矩形（1:1，无缩放）"
        );
    }

    #[test]
    fn screen_size_never_returns_zero() {
        // 0 尺寸的窗口创建会失败、或者建出一个看不见的窗口，
        // 两种都比"窗口偏大"更难排查，因此必须有兜底。
        let broken = monitor(r"\\.\DISPLAY1", 0, 0, 0, 0, true, 96);
        let (w, h) = screen_client_size(&broken);
        assert!(w > 0 && h > 0, "显示器矩形为 0 时也要给出正尺寸：{w}x{h}");
    }
}

/// 窗口过程。
unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    match message {
        WM_CLOSE => {
            // 我们不希望 Shell 或别的什么东西关掉宿主窗口；
            // 真正的退出走指令通道（见 run_compositor_thread 的收尾）。
            log::debug!("桌面层窗口收到 WM_CLOSE，忽略（退出请用 close()）");
            windows::Win32::Foundation::LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// 供 `--probe-desktop-layer` 之类的只读用途：报告"如果要挂，会挂到哪里"。
pub fn describe_attach_target() -> String {
    let probe = crate::desktop::probe_desktop();
    let monitors = crate::monitors::enumerate().unwrap_or_default();
    let (x, y, width, height) = crate::monitors::virtual_desktop_layout(&monitors);
    format!(
        "拓扑        : {:?}\n\
         父窗口      : {}\n\
         插到其之下  : {}\n\
         虚拟桌面    : ({x},{y}) {width}x{height}\n\
         显示器      : {} 块\n",
        probe.topology,
        probe
            .wallpaper_layer
            .map(|h| format!("0x{h:X}"))
            .unwrap_or_else(|| "（找不到，无法挂载）".to_string()),
        probe
            .defview
            .map(|h| format!("0x{h:X}（SHELLDLL_DefView）"))
            .unwrap_or_else(|| "（无）".to_string()),
        monitors.len()
    )
}



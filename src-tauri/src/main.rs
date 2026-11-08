//! Tauri 外壳：窗口、命令白名单、日志、诊断与扫描入口。
//!
//! 业务逻辑一律在 `wwp-core` 里；本文件只做三件事：
//! 确定数据位置、把引擎接上前端、把该记的记进日志。
//!
//! 除了窗口模式，本程序还支持两种无界面模式（见 `main`）：
//!
//! - `--diagnose`：打印数据位置，用于确认便携模式是否生效；
//! - `--scan --dir <目录>`：扫一个图库并打印统计，用于在大图库上实测扫描耗时。
//!
//! 这两种模式是这个项目"能自己证明自己"的方式：它们输出的是可核对的数字，
//! 而不是"应该没问题"。

// 交付产物是控制台程序时，双击会弹出一个黑窗口；这里禁掉它。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod broadcast;
mod commands;
mod diagnostics;
mod playback_manager;
mod playback_state;
mod nsfw;
mod tray;
mod window_state;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use tauri::Manager;
use wwp_core::logfile::FileLogger;
use wwp_core::paths::{self, AppPaths};
use wwp_core::{Settings, StartupInfo};

/// 启动时解析一次的数据位置。窗口与命令都从这里读。
static APP: OnceLock<AppPaths> = OnceLock::new();
/// 日志实现。`log::set_logger` 需要 `&'static dyn Log`，因此放在静态槽里。
static LOGGER: OnceLock<FileLogAdapter> = OnceLock::new();

/// 扫描的实时状态。
///
/// ## 为什么要有它（而不只是靠事件推送）
///
/// 扫描进度原先只通过事件（`EVENT_SCAN_PROGRESS`）推给界面，而前端只在
/// **图库页**订阅了那个事件。于是用户切到别的页，进度条就消失了——
/// 而扫描其实还在跑。用户会以为扫描停了，或者以为自己弄坏了什么。
///
/// 有了这份快照，任何页面都能随时**查询**当前扫描状态，因此可以在右上角
/// 常驻显示一条进度（见界面里的 ScanToast）。
///
/// 它同时也是"卡死检测"的依据：`started` 让我们能显示已经跑了多久，
/// 而"卡住不动"正是用户最容易误判成程序死了的情况。
#[derive(Debug, Default)]
pub struct ScanStatus {
    pub library: Option<String>,
    pub started: Option<std::time::Instant>,
    /// 最近一次收到的进度。遍历阶段还没到，因此初始为 None。
    pub latest: Option<wwp_core::library::ScanProgress>,
    /// 是否已经完成（成功、失败或被取消）。用于让界面把提示收起来。
    pub finished: bool,
    /// 完成后的收尾说明（错误信息，或"已取消"）。
    pub outcome: Option<String>,
    /// "一键重扫全部图库"时的进度：(当前第几个, 一共几个)，都是 1 基。
    ///
    /// 单库扫描时为 `None`——那时"第几个"没有意义，界面也不该显示"第 1/1 个"。
    pub batch: Option<(usize, usize)>,
}

/// 命令层共享的状态。
pub struct AppState {
    pub paths: AppPaths,
    /// 当前设置。界面改设置后回写这里，轮播启动时从这里读。
    pub settings: Mutex<Settings>,
    /// 扫描的取消标志。扫描在后台线程里跑，用户点"取消"时置位。
    pub scan_cancel: Arc<AtomicBool>,
    /// 是否有扫描正在进行，避免同时发起两次。
    pub scanning: Arc<AtomicBool>,
    /// 当前（或最近一次）扫描的细节，供界面随时查询。
    pub scan_status: Arc<Mutex<ScanStatus>>,
    /// 轮播管理器。界面通过它启停与操作每屏轮播。
    pub playback: playback_manager::PlaybackManager,
}

impl AppState {
    pub fn settings_path(&self) -> PathBuf {
        self.paths.config_dir().join("settings.toml")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.paths.data_dir().join("index")
    }

    pub fn output_dir(&self) -> PathBuf {
        self.paths.data_dir().join("output")
    }

    /// 软件自己的回收站（「删除当前壁纸」把文件移到这里）。
    ///
    /// 放在 `data/` 下面而不是 `config/` 旁边：它是**运行时数据**，
    /// 与索引、壁纸缓存一样可以由用户自己清掉，也在 `-CleanData` 的清理范围内。
    ///
    /// 具体路径由 `AppPaths::trash_dir` 定（界面显示的也是那一个，
    /// 见 `StartupInfo::trash_dir`）——同一件事不留两处写法。
    pub fn trash_dir(&self) -> PathBuf {
        self.paths.trash_dir()
    }

    /// 读一份设置副本。持锁时间尽可能短，避免与界面写设置互相等待。
    pub fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// 把 `FileLogger` 接到 `log` 门面上。
///
/// `FileLogger` 自己刻意不实现 `log::Log`：它属于引擎，引擎不该知道是谁在驱动它。
/// 这一层适配留在外壳里。
struct FileLogAdapter(&'static FileLogger);

impl log::Log for FileLogAdapter {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        self.0
            .write_line(record.level(), record.target(), &format!("{}", record.args()));
    }

    fn flush(&self) {
        // FileLogger 每行都 flush，这里无需额外动作。
    }
}

fn main() {
    let paths = startup_paths();
    apply_webview_env(&paths);
    install_logger(&paths);
    install_panic_hook(&paths);

    // 引擎需要知道索引放在哪里。外壳决定数据位置，因此由外壳告诉它。
    wwp_core::set_index_root(paths.data_dir().join("index"));

    log::info!(
        "启动 WindowsWallPaper：数据根目录 {}（来源 {}）",
        paths.root.display(),
        paths.root_source.as_str()
    );
    if let Some(reason) = &paths.fallback_reason {
        log::warn!("数据目录已回退到系统目录：{reason}");
    }

    // --- 无界面模式 ---
    if has_flag("--diagnose") {
        let info = StartupInfo::from_paths(&paths);
        let text = format!(
            "WindowsWallPaper 诊断\n\
             版本          : {} ({})\n\
             {}\
             标记文件      : {}\n",
            info.version,
            info.build,
            paths.describe(),
            paths.root.join("wwp-data-location.txt").display(),
        );
        log::info!("诊断输出：\n{text}");
        log::info!("诊断模式：数据位置已确认（详见日志与下方输出）");
        emit(&text);
        return;
    }

    if has_flag("--scan") {
        run_scan_mode(&paths);
        return;
    }

    // 桌面拓扑探测：只读，不改任何窗口关系，也不碰壁纸。
    if has_flag("--probe-desktop") {
        run_probe_desktop();
        return;
    }

    // 静态壁纸能力探测（只读）。
    if has_flag("--probe-wallpaper") {
        run_probe_wallpaper(&paths);
        return;
    }

    // 全屏检测（只读）：逐屏打印"有没有程序全屏"的原始数据与判定。
    // 本功能的验收要靠它 + scripts\probe-fullscreen.ps1 造出的**真全屏窗口**，
    // 上一版就是因为"测试窗口从没真正全屏过"而误判成可用。
    if has_flag("--probe-fullscreen") {
        run_probe_fullscreen();
        return;
    }

    // 逐屏设图实测：会改动壁纸，但**先拍快照、验证后回滚**。
    if has_flag("--spike-wallpaper") {
        run_spike_wallpaper(&paths);
        return;
    }

    // 逐屏轮播实测：把每屏的独立轮播真正跑起来。会改动壁纸，退出时按快照还原。
    if has_flag("--rotate") {
        run_rotate(&paths, &settings_for(&paths));
        return;
    }

    // 诊断导出：不打开窗口也能导出。
    // 这条路径是必需的——用户可能遇到"界面起不来"，而那正是最需要诊断信息的时候。
    if has_flag("--export-diagnostics") {
        match diagnostics::export(&paths, &settings_for(&paths)) {
            Ok(export) => {
                let text = format!(
                    "诊断已导出到: {}\n包含 {} 个文件：{}\n",
                    export.directory,
                    export.files.len(),
                    export.files.join("、")
                );
                log::info!("{text}");
                emit(&text);
            }
            Err(error) => {
                eprintln!("导出诊断失败：{error}");
                log::error!("导出诊断失败：{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    // 走界面路径的启停验证（与点"开始轮播"同一条路）。
    if has_flag("--test-ui-playback-path") {
        run_test_ui_playback_path(&paths);
        return;
    }

    // 排除规则的保存→重载往返（与界面点"保存"走同一条路）。
    // —— 已随「排除规则」页删除：现在只剩目录前缀一类规则，配置往返由单元测试守着。

    // 图库只读保证：跑完整条链（扫描/读索引/图像预处理），
    // 前后各拍一次目录快照并逐项比对，确认没有任何写入。
    if has_flag("--test-library-readonly") {
        run_test_library_readonly(&paths);
        return;
    }

    // 扫描状态：跑一次真实扫描并轮询共享状态。
    if has_flag("--probe-scan-status") {
        run_probe_scan_status(&paths);
        return;
    }

    // 目录树（界面"选择目录…"用的那个接口）。
    if has_flag("--probe-folder-tree") {
        run_probe_folder_tree(&paths);
        return;
    }

    // "在资源管理器里打开它所在的文件夹并选中它"：与播放页点路径、
    // 回收站点文件名走的是同一个函数，命令行里能看见它到底去哪了。
    if has_flag("--reveal") {
        run_probe_reveal();
        return;
    }

    // 读写壁纸的对照实验：设置后**等一会儿再回读**，看是否被别的程序改写。
    if has_flag("--test-wallpaper-readback") {
        run_test_wallpaper_readback();
        return;
    }

    // 测"换纸后画面真的变了"要多久（回读与截屏两种判据）。
    if has_flag("--test-wallpaper-latency") {
        run_test_wallpaper_latency(&paths);
        return;
    }

    // 把"点 NSFW"拆成准备 / 设上去 / 画面上出现三步分别计时（用户报有几百毫秒延时）。
    if has_flag("--probe-nsfw-latency") {
        run_probe_nsfw_latency(&paths);
        return;
    }

    // 同一件事的完整路径版：先把轮播跑起来并立刻换纸，再点 NSFW，
    // 用来量"停轮播那一步在等什么"（见函数注释）。
    if has_flag("--probe-nsfw-timing") {
        run_probe_nsfw_timing(&paths);
        return;
    }

    // 真机验收"进入 NSFW 再退出，屏幕上回到进入之前那张"（1.13.0 的那条要求）。
    // 它会真的设壁纸，因此只在用户那份程序没在跑的时候用（见函数注释）。
    if has_flag("--probe-nsfw-cycle") {
        run_probe_nsfw_cycle(&paths);
        return;
    }

    // 清理程序目录：默认**只列不删**，加 `--run` 才真的清（见 cli.rs 的说明）。
    if has_flag("--cleanup") {
        run_cleanup(&paths);
        return;
    }

    // --- 单实例门闩（用户要求：只允许一个进程）---
    //
    // 位置很讲究：**在所有无界面模式之后**。`--diagnose` / `--scan` / `--export-diagnostics`
    // 这些是排查工具，而最常见的使用场景恰恰是"程序正跑着但界面/壁纸不对，我要看一下"——
    // 拦住它们等于把排查手段本身删掉。因此单实例只约束**窗口模式**。
    //
    // `_instance` 必须活到进程结束（它在 main 的栈帧里，直到 `run()` 返回），
    // 一旦被 drop，门闩就放开了。
    let _instance = match wwp_core::single_instance::claim() {
        Some(guard) => guard,
        None => {
            // 已经有一个在跑：本进程不做任何事，只请它把主界面显示出来，然后退出。
            // 用户要求"再次启动程序则默认打开主界面"，而不是静默地什么都没有发生。
            log::info!("已经有一个 WindowsWallPaper 在运行；本次启动只请求它显示主界面");
            if wwp_core::single_instance::ask_existing_to_show_window() {
                log::info!("已请求正在运行的实例显示主界面，本进程退出");
            } else {
                // 找不到那个实例的消息窗口（可能它正在启动，或刚退出）。
                // 什么都不做地退出是安全的：绝不能让第一个实例的文件被两个进程同时写。
                log::warn!("没能通知到正在运行的实例（它可能正在启动或刚退出），本进程退出");
            }
            return;
        }
    };

    // --- 窗口模式 ---
    let settings = match Settings::load_or_create(&paths.config_dir().join("settings.toml")) {
        Ok(settings) => settings,
        Err(error) => {
            log::error!("读取设置失败，将使用默认设置：{error}");
            Settings::default()
        }
    };

    let state = AppState {
        paths: paths.clone(),
        settings: Mutex::new(settings),
        scan_cancel: Arc::new(AtomicBool::new(false)),
        scanning: Arc::new(AtomicBool::new(false)),
        scan_status: Arc::new(Mutex::new(ScanStatus::default())),
        playback: playback_manager::PlaybackManager::new(),
    };
    // 告诉轮播管理器数据目录与产物目录在哪：NSFW 壁纸的预解码产物要写到 output 下。
    state
        .playback
        .set_paths(paths.data_dir(), paths.data_dir().join("output"));

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::startup_info,
            commands::log_error,
            commands::log_debug,
            commands::copy_text,
            commands::open_folder,
            commands::reveal_file,
            commands::open_file,
            commands::pick_folder,
            commands::pick_image,
            commands::library_folder_tree,
            commands::library_scan_status,
            commands::log_path,
            commands::settings_info,
            commands::library_list,
            commands::library_scan,
            commands::library_scan_all,
            commands::library_cancel_scan,
            commands::library_stats,
            commands::library_entries,
            commands::window_metrics,
            commands::wallpaper_thumbnail,
            commands::settings_update,
            commands::settings_reload,
            commands::playback_status,
            commands::playback_start,
            commands::playback_control,
            commands::playback_screen_pause,
            commands::playback_next_screen,
            commands::playback_nsfw,
            commands::playback_delete_current,
            commands::trash_info,
            commands::trash_open,
            commands::trash_open_item,
            commands::trash_entries,
            commands::trash_thumbnail,
            commands::trash_restore,
            commands::trash_delete,
            commands::trash_empty,
            commands::diagnostics_export,
            commands::cleanup_items,
            commands::cleanup_run,
        ])
        .setup(|app| {
            log::info!("主窗口已创建");
            if let Some(window) = app.get_webview_window("main") {
                // 窗口在 `tauri.conf.json` 里是 `visible: false` 创建的：
                // 默认不显示主界面（只留托盘图标），需要时按设置显示。
                //
                // ## 为什么必须"以隐藏状态创建"而不是"创建后再隐藏"
                //
                // 后者会让窗口先出现一次再消失——开机启动时那一下闪烁很显眼，
                // 而且用户会以为程序界面出问题了。
                //
                // ## 顺序：先恢复上次的窗口大小，再决定显不显示
                //
                // 反过来的话，窗口会先以配置里的 1400×950 出现一下再跳到用户
                // 上次的尺寸——"界面闪一下"与上面那个理由是同一条。
                let data_dir = app.state::<AppState>().paths.data_dir();
                window_state::apply_saved(&window, &data_dir);
                window_state::install(&window, data_dir);

                let start_minimized = app
                    .state::<AppState>()
                    .settings()
                    .general
                    .start_minimized;
                if start_minimized {
                    log::info!("按设置启动时不显示主界面（点托盘图标可打开）");
                } else {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                install_close_to_tray(&window);
            }
            // 托盘失败不该让程序起不来：没有托盘仍然可以从窗口里操作，
            // 而缺了主窗口就什么都没法用了。
            if let Err(error) = tray::setup(app.handle()) {
                log::error!("托盘初始化失败（程序仍可使用，只是没有托盘图标）：{error}");
            }
            // 显示器插拔/分辨率变化的广播监听。它让"拔掉一块屏再插回"能被自动适配。
            // 全局快捷键也在同一个消息窗口上注册（WM_HOTKEY 只投递给注册它的窗口）。
            broadcast::start(app.handle());
            // NSFW 壁纸预热：先把两张固定图解好，点 NSFW 时才能"瞬间"。
            // 放在后台线程里做，不挡启动。
            {
                let state = app.state::<AppState>();
                state.playback.prewarm_nsfw(&state.settings());
            }
            // 启动决策：上次在 NSFW 模式就回到那两张固定图，否则按 auto_start 开始轮播。
            nsfw::apply_startup_state(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!());

    // 构建失败（拿不到应用）与运行结束是两件事，因此分两步匹配：
    // `build` 给 `Result<App>`，`App::run_return` 给退出码。
    let result = match app {
        Ok(app) => {
            app.run_return(|_handle, event| {
                // **退出原因必须在这里记**，而且只有这里记得住。
                //
                // ## 为什么原来那行 `log::info!("正常退出")` 从来没出现在日志里
                //
                // 用户报"程序会自己退出"，而日志里**每一段都以一行 DEBUG 轮询戛然而止**
                // ——那说明进程是被结束的，不是走完收尾流程退出的。查下去发现原因
                // 就在这一行：原先的写法是 `Builder::run()`（它 `std::process::exit`
                // 直接结束进程），而托盘「退出」调的是 `AppHandle::exit(0)`，
                // 内部同样是 `std::process::exit`（tauri 的 `App::exit` 实现）。
                // 于是 `run()` 之后那行 `log::info!("正常退出")` **永远执行不到**，
                // 它是一条死代码——真机上"托盘退出"与"被任务管理器结束"的日志
                // 完全一样，没有任何办法区分。这与 §7.2「两套机制只有一半能工作」
                // 是同一类毛病：**看着有日志，其实那行不可能被打印**。
                //
                // ## 为什么不改成"托盘退出前先记一行"就够了
                //
                // 那只覆盖托盘那一条路。关闭窗口直接退出（设置里关掉了
                // `minimize_to_tray` 时）、以及将来任何一处新的退出入口都会漏掉。
                // `RunEvent::Exit` 是**所有**退出路径的汇合点：事件循环结束时
                // tauri 先跑 `cleanup_before_exit`（清资源、隐藏窗口），再回调这里，
                // 然后才 `std::process::exit`。因此在这一点记，覆盖全部路径。
                //
                // 它同时是排查"程序是不是被强杀的"的判据：
                // 日志里有这一行 = 走的是正常退出；没有 = 进程被外力结束
                // （关机、注销、任务管理器、崩溃），两者从此可区分。
                if let tauri::RunEvent::Exit = event {
                    log::info!("事件循环结束，进程正常退出（本行缺失即代表进程被外力结束）");
                }
            })
        }
        // 构建失败：没有事件循环可跑，直接报出来。
        Err(error) => {
            log::error!("Tauri 运行失败：{error}");
            std::process::exit(1);
        }
    };

    // 走到这里说明事件循环是**正常结束**的（不是 `process::exit` 走的）。
    // 托盘「退出」那条路会在这里之前就把进程结束掉，因此这一行只在
    // "所有窗口都被销毁"这类收尾路径上出现——它和 `RunEvent::Exit` 那一行
    // 一起，把"谁结束的进程"这件事在日志里钉死。
    log::info!("事件循环已返回，进程退出（退出码 {result}）");
}

/// 输出到 stdout，但**仅在有控制台时**。
///
/// Windows 子系统的可执行文件（release 构建）没有控制台，`print!` 会以一个
/// "管道正在被关闭"的错误 panic——而那时工作其实已经做完了（实测踩到：
/// 整个实测流程成功、结果已写进日志，最后却因为打印失败而 panic）。
///
/// 用标准库的 `IsTerminal` 判断，因此外壳不需要直接依赖 `windows` crate
/// （Win32 调用一律留在 `wwp-core` 里）。重定向到文件时它返回 false，
/// 于是不打印——但结果本来就已经写进日志了。
fn emit(text: &str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        print!("{text}");
    }
}

/// 读取设置。日志已经装好，因此失败可以正常记录。
fn settings_for(paths: &AppPaths) -> Settings {
    match Settings::load_or_create(&paths.config_dir().join("settings.toml")) {
        Ok(settings) => settings,
        Err(error) => {
            log::error!("读取设置失败，将使用默认设置：{error}");
            Settings::default()
        }
    }
}

/// 关闭窗口 = 最小化到托盘，而不是退出。
///
/// 这是壁纸软件的常态：用户关掉设置界面，但壁纸还要继续换。
/// 因此真正的退出入口在托盘菜单里，不是窗口的关闭按钮。
///
/// 是否最小化到托盘由设置里的 `minimize_to_tray` 决定：关掉它时，关闭窗口就等于退出。
/// **退出不会还原壁纸**（用户明确要求删掉这个行为），因此这里只是停掉轮播。
fn install_close_to_tray<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    let window = window.clone();
    window.clone().on_window_event(move |event| {
        let tauri::WindowEvent::CloseRequested { api, .. } = event else {
            return;
        };
        let app = window.app_handle().clone();
        let state = app.state::<AppState>();
        let minimize_to_tray = state.settings().general.minimize_to_tray;

        // 关闭窗口这个动作会带走窗口——两次都先把大小记下来（见 window_state）。
        // 放在这里而不是只靠 Resized 的去抖线程：用户可能刚拖完窗口就点了关闭，
        // 那 700 ms 的去抖还没到点。
        window_state::save_now(&window, &state.paths.data_dir());

        if minimize_to_tray {
            // 拦住关闭，只隐藏窗口。
            api.prevent_close();
            let _ = window.hide();
            log::info!("窗口已隐藏到托盘（退出请用托盘菜单）");
        } else {
            // 用户明确不要托盘常驻：这一下就是退出。
            log::info!("设置里关闭了“最小化到托盘”，本次关闭将退出程序");
            state.playback.stop();
        }
    });
}

/// 解析数据位置。日志在此之后才可用，因此失败只能先报告到 stderr。
fn startup_paths() -> AppPaths {
    let program_dir = match paths::program_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("[wwp] 无法确定程序目录：{error}；将退回当前工作目录");
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    };

    let paths = AppPaths::resolve(program_dir);
    if let Err(error) = paths.ensure_dirs() {
        eprintln!("[wwp] 创建数据目录失败：{error}");
    }

    // 顺手保证配置文件存在，即使这次运行是 `--diagnose` 这类不读设置的模式。
    // 配置文件是自解释的（头部写明了每个可配置项），因此"第一次运行后文件夹里
    // 就有它"本身就是最好的说明——而不是等用户去猜有哪些设置。
    let settings_path = paths.config_dir().join("settings.toml");
    if !settings_path.exists() {
        if let Err(error) = Settings::default().save(&settings_path) {
            eprintln!("[wwp] 写入默认设置失败：{error}");
        }
    }

    let _ = APP.set(paths.clone());
    paths
}

/// 把 WebView2 的用户数据目录钉在数据根目录里。
///
/// 不设置的话 WebView2 会写 `%LOCALAPPDATA%\<bundle-id>\EBWebView`，
/// "拷走整个文件夹就能用"就破了。必须在创建窗口之前设置。
fn apply_webview_env(paths: &AppPaths) {
    std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", paths.webview_data_dir());
}

/// 安装文件日志。写入失败不影响程序运行，只是失去日志。
fn install_logger(paths: &AppPaths) {
    let log_path = paths.app_log();
    match FileLogger::open(&log_path) {
        Ok(logger) => {
            // 泄漏一个实例换取 `&'static`：进程生命周期内只有一个日志器，这是它的归宿。
            let logger: &'static FileLogger = Box::leak(Box::new(logger));
            if LOGGER.set(FileLogAdapter(logger)).is_err() {
                eprintln!("[wwp] 日志实现已初始化，忽略重复初始化");
                return;
            }
            let adapter = LOGGER.get().expect("刚刚设置成功");
            match log::set_logger(adapter) {
                Ok(()) => {
                    // Debug 级别也打开：排查"界面显示的值不对"这类问题时，
                    // 关键信息往往就在 debug 日志里，而默认只记 Info 会让现场消失。
                    log::set_max_level(log::LevelFilter::Debug);
                    log::info!("日志已就绪：{}", log_path.display());
                }
                Err(error) => eprintln!("[wwp] 无法安装日志实现：{error}"),
            }
        }
        Err(error) => {
            eprintln!("[wwp] 无法打开日志文件 {}：{error}", log_path.display());
        }
    }
}

/// panic 同时写进日志与 `logs/crash/`，否则崩溃现场只剩一个消失的窗口。
fn install_panic_hook(paths: &AppPaths) {
    let crash_dir = paths.crash_dir();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "未知位置".to_string());
        let payload = payload_text(info.payload());
        log::error!("发生 panic（{location}）：{payload}");

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Err(error) = std::fs::create_dir_all(&crash_dir) {
            eprintln!("[wwp] 无法创建崩溃目录 {}：{error}", crash_dir.display());
        } else {
            let file = crash_dir.join(format!("panic-{stamp}.txt"));
            let text = format!(
                "WindowsWallPaper panic\r\n\
                 位置: {location}\r\n\
                 时间(Unix 秒): {stamp}\r\n\
                 详情: {payload}\r\n\r\n\
                 请把本文件与 logs/app.log 一起反馈。\r\n"
            );
            if let Err(error) = std::fs::write(&file, text) {
                eprintln!("[wwp] 无法写入崩溃报告 {}：{error}", file.display());
            }
        }

        default_hook(info);
    }));
}

fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "无法读取 panic 详情".to_string()
    }
}

/// 我们的命令行参数。
///
/// 只看 `--` 之后的参数：Tauri/WebView2 会把自己的参数接在 `--` 之前，
/// 不过滤的话它们的取值可能被误判成我们的开关。没有 `--` 时按完整参数列表判断，
/// 便于 `cargo run -- --diagnose` 之外的调用方式。
fn our_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().position(|a| a == "--") {
        Some(index) => args[index + 1..].to_vec(),
        None => args,
    }
}

fn has_flag(flag: &str) -> bool {
    our_args().iter().any(|a| a.eq_ignore_ascii_case(flag))
}

/// 取 `--flag value` 形式的取值。
fn flag_value(flag: &str) -> Option<String> {
    let args = our_args();
    args.iter()
        .position(|a| a.eq_ignore_ascii_case(flag))
        .and_then(|index| args.get(index + 1).cloned())
}

// --- 诊断与实测入口 ---
//
// 这些是开发与排查用的命令行模式，单独放在 cli.rs 里。
// 用 `include!` 而不是 `mod`：它们要用本文件里的 emit / has_flag /
// flag_value / settings_for，文本级包含让互访不需要任何可见性修饰。
// 详见 cli.rs 顶部的说明。
include!("cli.rs");


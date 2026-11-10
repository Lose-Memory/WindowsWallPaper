//! 命令白名单：前端能调用的全部方法都在这里。
//!
//! `main.rs` 的 `generate_handler!` 就是白名单本身——引擎里的方法再多，
//! 没在这里注册的前端也够不到。
//!
//! 扫描是唯一耗时可能到分钟级的操作，因此它单独处理：在后台线程里跑，
//! 通过事件把进度推给界面，并支持取消。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use wwp_core::library::{
    self, IndexPaths, IndexStats, LibraryEntry, ScanOutcome, ScanProgress, UnreadableSample,
};
use wwp_core::cleanup::{CleanupItem, CleanupReport};
use wwp_core::{Settings, StartupInfo};

use crate::AppState;

/// 扫描进度事件名。前端 `listen` 这个字符串。
pub const EVENT_SCAN_PROGRESS: &str = "wwp://scan-progress";
/// 扫描结束事件名（含被取消的情况）。**单个图库**的扫描用它。
pub const EVENT_SCAN_DONE: &str = "wwp://scan-done";
/// "一键重扫全部图库"结束事件名。
///
/// 单独一个事件而不是复用 `EVENT_SCAN_DONE`：那一个是**每库一张结果卡**的语义
/// （"这个图库扫完了、可播放多少张"），而用户点"全部重新扫描"要看的是
/// **一批的合计**——新增了什么、删了什么。复用同一个事件会让界面在几秒内
/// 被 N 张卡片轮流覆盖，最后只剩最后一个图库的数字。
pub const EVENT_SCAN_ALL_DONE: &str = "wwp://scan-all-done";
/// 主窗口被**从隐藏/最小化状态叫到前台**时发一次。
///
/// 用户 2026-10 的要求："当我使用快捷键唤起主页面时，默认把页面调到播放页，
/// 而不是其他我上次打开过的页面。"
///
/// 为什么不用"窗口获得焦点"这个前端事件：隐藏窗口再显示时焦点事件的行为
/// 随平台与"是不是同一个前台窗口"而变，而且点托盘、按快捷键、二次启动
/// **三条路径**都要有同样的效果。现在这三条路径本来就走同一个
/// `crate::tray::show_main_window`，因此在那里发事件最准。
pub const EVENT_MAIN_WINDOW_SHOWN: &str = "wwp://main-window-shown";
/// 「全部删除」的进度事件。
///
/// 单独一个事件而不是复用 `EVENT_SCAN_PROGRESS`：两者说的是**完全不同**的
/// 两件事（一个是扫描文件、一个是把文件交给系统回收站），进度条的位置、
/// 能否取消、结束后跳到哪里都不一样。共用会让"正在扫描"的界面在删除时
/// 显示扫描的文案。
pub const EVENT_TRASH_EMPTY_PROGRESS: &str = "wwp://trash-empty-progress";
/// 按了一次「切换播放列表」快捷键。
///
/// 前端据此重新拉一次设置——快捷键改的是 `current_playlist`，而设置页上
/// 那两个槽位与播放页的列表下拉框都读设置。不通知的话界面会一直显示旧值，
/// 直到用户切页或做别的操作，看起来像"按了没反应"。
pub const EVENT_PLAYLISTS_SWITCHED: &str = "wwp://playlists-switched";

#[tauri::command]
pub fn startup_info(state: State<'_, AppState>) -> StartupInfo {
    StartupInfo::from_paths(&state.paths)
}

/// 供界面错误边界上报渲染期异常。
#[tauri::command]
pub fn log_error(message: String) {
    log::error!("[界面] {message}");
}

/// 记录一条**诊断**信息（debug 级别），用于排查"界面显示的值不对"这类问题。
///
/// ## 为什么需要它，而不是直接用 `log_error`
///
/// 界面上的几何/环境诊断信息原先走的是 `log_error`，于是它们被记成 `[ERROR]`。
/// 后果有两层：
///
///   1. **真正的错误被淹没**。按 `[ERROR]` 过滤日志时会先跳出一堆
///      "页面看到的视口 1120x761"这种完全正常的信息；
///   2. **"错误条数"这个指标失去意义**。用它判断"这次运行有没有问题"
///      会一直得到非零结果，于是没人再看它。
///
/// 诊断信息与错误是两件事，因此分成两个命令。日志级别仍然开着 Debug
/// （见 `main.rs` 里的说明），所以这些信息照样会落盘，只是不再冒充错误。
#[tauri::command]
pub fn log_debug(message: String) {
    log::debug!("[界面] {message}");
}

/// 在资源管理器里打开一个目录（图库列表上的「打开目录」按钮用）。
///
/// ## 为什么用 `explorer.exe` 而不是 ShellExecuteW
///
/// 两者都能打开文件夹，但这里只要"打开一个目录"这一件事，不需要引入 shell 的
/// COM 接口；`explorer` 是 Windows 自带的，参数就是一个路径。
///
/// **路径必须整体作为一个参数传**（不经过 shell）：含空格的路径
/// （用户的图库就在 `D:\Pictures\示例图库A` 这类名字下）才不会
/// 被拆成两段。
///
/// 不做存在性校验之外的任何检查：这是一个只读的浏览动作，
/// 打不开就是打不开，日志里留一条即可。
#[tauri::command]
pub fn open_folder(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".to_string());
    }
    let target = std::path::PathBuf::from(trimmed);
    if !target.is_dir() {
        // 盘没插、目录被改名都会走到这里。给用户一句人话，而不是让资源管理器
        // 弹一个"找不到"的框——后者看起来像程序出错了。
        return Err(format!("目录不存在或暂时不可用：{trimmed}"));
    }

    std::process::Command::new("explorer")
        .arg(&target)
        .spawn()
        .map_err(|error| {
            log::warn!("打开目录失败 {trimmed}：{error}");
            format!("无法打开目录：{error}")
        })?;
    log::info!("已在资源管理器中打开：{trimmed}");
    Ok(())
}

/// 在资源管理器里打开某个**文件**所在的目录，并选中这个文件。
///
/// ## 与 [`open_folder`] 的区别（两件事，不能互相顶替）
///
/// - `open_folder` 打开的是**目录本身**（图库页的"打开文件夹"）；
/// - 这里打开的是**文件所在的目录**，而且要把这个文件**选中**——用户点的
///   是"这张图在哪儿"，落点必须是那张图本身，而不是一个需要自己再翻的目录。
///
/// ## 实现放在 `wwp-core` 里，这里只做记录
///
/// 真正干活的是 [`wwp_core::shell::reveal_file`]，它用 shell 的
/// `SHOpenFolderAndSelectItems`。**1.11.0 那版不是这样**：那时这里拼一条
/// `/select,<路径>` 交给新起的 `explorer.exe`，而 `explorer.exe` 只是把请求
/// 转交给已经在跑的那个 shell——转交一旦失败，它会自己开一个默认位置
/// （Home / 文档），**参数被静默丢掉、`spawn` 却照样成功**。于是日志里全是
/// "已在资源管理器中定位"，而用户看到的是"有时候打开的是我的文档"。
/// 详情与实测见 `wwp_core::shell::reveal_file` 的注释。
///
/// 文件不在了不报错、也不静默失败：退回"打开它所在的那个目录"。这与用户的
/// 意图一致（"我想找到这张图"）——文件被删掉之后，那里通常还留着同一批图。
/// 目录也不在了才报错，此时给一句人话而不是弹一个"找不到"的系统框。
///
/// 它是**同步命令**：`SHOpenFolderAndSelectItems` 是一次进程内调用，
/// 毫秒级返回；而且它要求调用线程已初始化 COM，同步命令跑的正是 WebView2 的
/// STA 主线程（与 [`open_file`] 同一条理由）。
#[tauri::command]
pub fn reveal_file(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".to_string());
    }

    match wwp_core::shell::reveal_file(std::path::Path::new(trimmed)) {
        Ok(()) => {
            log::info!("已在资源管理器中定位：{trimmed}");
            Ok(())
        }
        Err(error) => {
            log::warn!("定位文件失败 {trimmed}：{error}");
            Err(error)
        }
    }
}

#[tauri::command]
pub fn log_path(state: State<'_, AppState>) -> String {
    state.paths.app_log().display().to_string()
}

/// 用系统默认关联的程序打开一个文件（播放页点"当前壁纸"缩略图）。
///
/// 与 [`reveal_file`] 是两件事：那个是"在资源管理器里找到它"，这个是"拿默认的
/// 看图程序打开它"。用户点击当前壁纸的缩略图要的是后者——看这张图本身。
///
/// ## 为什么是同步命令
///
/// 与 `trash_open_item` 同一条理由：只做一次 `ShellExecuteW`，把"打开它"交给
/// shell 就返回，不等那个程序启动完（毫秒级，不占住主线程）；而且 shell 这个
/// 入口要在 **STA** 线程上调用，主线程（WebView2 初始化的那个 STA）正是最合适
/// 的调用点。详见 `wwp_core::shell::open_file`。
///
/// 文件不在时由 `wwp_core::shell::open_file` 给一句人话（"它已经不在了：…"），
/// 不会崩溃，也不会静默什么都不做。
#[tauri::command]
pub fn open_file(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".to_string());
    }
    wwp_core::shell::open_file(std::path::Path::new(trimmed)).map_err(|error| {
        log::warn!("打开文件失败 {trimmed}：{error}");
        error
    })
}

/// 把一段文本写进系统剪贴板。
///
/// ## 为什么在界面里做"复制路径"而不是让用户手动选中
///
/// 用户要的是"点一下『当前显示』就把那张图的完整路径复制走"——卡片上显示的
/// 是截断过的短路径，手动选中既难选也拿不到完整路径。
///
/// 实现放在引擎里（`wwp_core::clipboard`）：前端那套 `navigator.clipboard`
/// 在 WebView2 里依赖焦点与权限，失败时既不抛错也不写进任何东西，
/// 表现成"点了没反应"；而 Win32 的剪贴板 API 成败都是明确的。
#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("没有可复制的内容".to_string());
    }
    wwp_core::clipboard::set_text(&text).map_err(|error| {
        log::warn!("写入剪贴板失败：{error}");
        error
    })?;
    log::info!("已复制到剪贴板（{} 个字符）：{text}", text.chars().count());
    Ok(())
}

/// 窗口几何信息。
///
/// 这个命令存在的理由是排查一类"看起来像布局 bug"的问题：当 WebView2 的缩放因子
/// 与窗口的实际尺寸不一致时，页面会以错误的逻辑尺寸排版，表现为界面被裁掉一截。
/// 只看截图无法区分"页面横向溢出"和"渲染缩放不对"，因此这里把两边的数字都取出来：
/// 窗口自身的物理尺寸、Tauri 报告的缩放因子，以及页面看到的 CSS 视口。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowMetrics {
    /// 窗口客户区的物理像素尺寸（Tauri 报告值）。
    pub inner_size: (u32, u32),
    /// Tauri 报告的缩放因子。
    pub scale_factor: f64,
    /// 显示器缩放与 Tauri 缩放因子是否一致——不一致就是界面被裁掉的原因。
    pub consistent: bool,
}

#[tauri::command]
pub fn window_metrics(window: tauri::Window) -> Result<WindowMetrics, String> {
    let inner = window.inner_size().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    Ok(WindowMetrics {
        inner_size: (inner.width, inner.height),
        scale_factor: scale,
        consistent: true,
    })
}

/// 一张**壁纸**的缩略图（data URL，直接给 `<img src>` 用）。
///
/// 播放页每块屏的「当前显示 / 下一张」两格预览用它（用户 2026-10 的要求：
/// 要能看见这两张壁纸长什么样，而不是只有一行路径）。
///
/// ## 与 `trash_thumbnail` 的关系
///
/// 压图、编码、缓存都走同一份实现（`wwp_core::thumb`），只是缓存目录不同：
/// 回收站那份跟着**条目**走（条目删了缓存也跟着删），这一份跟着**图片内容**走，
/// 因此缓存键里带了源文件的修改时间与大小——换了图就一定换键，不会串味
/// （AGENTS §7.14 那条教训）。缓存目录是 `data/wallpaper-thumbs/`，
/// 超过上限时由 `thumb::prune` 按"最久没用过"清理。
///
/// ## 为什么是 `async` 命令
///
/// 与 `trash_thumbnail` 同一个理由：同步的 `#[tauri::command]` 跑在**主线程**上，
/// 而解码一张几十兆像素的照片要几百毫秒。每块屏两个预览、两块屏就是四张，
/// 同步做就是"一打开播放页卡住一两秒"。这里用 `spawn_blocking` 把解码挪到
/// 阻塞线程池：既不占主线程，也不占异步运行时的工作线程。
///
/// ## 只处理"确实存在的文件"
///
/// 传进来的是后端自己算出来的当前/下一张路径，这里再挡一道：文件不在就直接
/// 返回原因（图被移走、盘没插上都会走到这里），界面把这句话显示在预览框里，
/// 比一个空白框有用。
#[tauri::command]
pub async fn wallpaper_thumbnail(app: AppHandle, path: String) -> Result<String, String> {
    let cache = {
        let state = app.state::<AppState>();
        state.paths.data_dir().join("wallpaper-thumbs")
    };
    tauri::async_runtime::spawn_blocking(move || {
        let source = PathBuf::from(&path);
        if !source.is_file() {
            return Err(format!("文件不存在或不可用：{path}"));
        }
        wwp_core::thumb::data_url_for(&source, &cache)
    })
    .await
    .map_err(|error| format!("缩略图任务没能完成：{error}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInfo {
    /// 设置文件位置。
    pub settings_path: String,
    /// 数据根目录（界面用来显示"数据存在哪"）。
    pub data_root: String,
    pub root_source: String,
    pub fell_back: bool,
    pub fallback_reason: Option<String>,
    /// 播放列表（界面在这里增删改，以及给每块屏选当前列表）。
    pub playlists: Vec<PlaylistView>,
    /// 图库清单（独立配置项，顺序 = 用户添加的顺序）。
    ///
    /// 增删的入口是「图库」页的「新增图库 / 移除」；播放列表只是勾选其中的几个。
    pub libraries: Vec<LibraryView>,
    /// 全局排除规则（界面展示与编辑用）。
    pub exclude: ExcludeView,
    /// 全局默认值。
    pub general: GeneralView,
    /// 每屏的原始设置。界面需要它来显示"已保存的筛选条件"——
    /// 只给"生效值"是不够的，用户改过的东西必须能被他看到，否则会以为没保存。
    pub monitors: std::collections::BTreeMap<String, MonitorView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralView {
    pub interval_seconds: u64,
    pub fit_mode: String,
    pub minimize_to_tray: bool,
    /// 启动后是否自动接管壁纸。用户要求给成开关，默认开启。
    pub auto_start: bool,
    /// 启动时是否不显示主界面。默认开启，见 `wwp_core::config::General`。
    pub start_minimized: bool,
    /// 某块屏上有程序全屏（含最大化）时，只暂停这块屏的轮播。默认关闭。
    pub pause_on_fullscreen: bool,
    /// NSFW 模式的全局快捷键（`Ctrl+Alt+S` 这种写法）。
    pub nsfw_hotkey: String,
    /// 全局"暂停/继续"的快捷键（作用于所有屏）。
    pub pause_hotkey: String,
    /// 全局"下一张"的快捷键（所有屏各换一张）。
    pub next_hotkey: String,
    /// 全局"上一张"的快捷键（所有屏各退回一张）。
    pub previous_hotkey: String,
    /// 全局"显示主界面"的快捷键（把设置窗口翻出来）。
    pub show_window_hotkey: String,
    /// 每块屏在两个播放列表之间往返切换的全局快捷键。
    pub switch_playlists_hotkey: String,
    /// 设置页「清理程序目录」那张卡片是否展开（用户折叠过就记下来，默认展开）。
    pub cleanup_expanded: bool,
    /// 单击托盘图标做什么：`open-window` 或 `next-wallpaper`。
    pub tray_click_action: String,
}

/// 每屏设置的界面视图。
///
/// 注意这里**没有** interval / fit_mode：它们在播放列表上
/// （它们是内容相关的属性）。长宽比等筛选留在这里，因为那是屏幕的物理属性。
///
/// 也**没有**"是否参与轮播"：那块屏要不要换纸，只看它有没有选播放列表。
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MonitorView {
    /// 当前选中的播放列表 id。空串表示"还没选"。
    pub current_playlist: String,
    /// 「切换播放列表」快捷键在这块屏上往返的两个列表 id（空串 = 这一槽没配）。
    pub switch_playlist_a: String,
    pub switch_playlist_b: String,
    /// NSFW 模式时这块屏显示哪张图（空串 = 没设置）。
    pub nsfw_wallpaper: String,
    /// 这块屏的固定单图（空串 = 没设置）。非空时它优先于播放列表、不轮播。
    pub single_image: String,
    /// 这块屏在界面上的显示名（空串 = 用真实显示器名）。
    ///
    /// 只用于显示：配置键、日志与产物名仍然用真实显示器名。
    pub label: String,
    pub aspect_min: Option<f64>,
    pub aspect_max: Option<f64>,
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    /// 该屏是否在配置里出现过（界面据此显示"已单独设置"）。
    pub present: bool,
}

/// 一个播放列表的界面视图。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistView {
    pub id: String,
    pub name: String,
    /// 关联的图库路径。
    pub libraries: Vec<String>,
    pub interval_seconds: u64,
    pub fit_mode: String,
}

/// 一个图库。
///
/// ## 它是独立的配置项（这一条被用户改过）
///
/// 原先图库不是独立配置项：这份清单是从所有播放列表的 `libraries` 汇总出来的，
/// 唯一的"添加图库"入口是给某个播放列表关联目录。用户要求改掉——
/// **「图库」页上要有「新增图库」按钮，只能从那里添加；播放列表只负责勾选**。
/// 因此现在清单来自 `settings.libraries`（顺序 = 用户添加的顺序）。
///
/// `used_by` 仍然是从播放列表数出来的：它回答的是"这个图库会被播到吗"。
/// 0 不是错误——刚加进来的图库还没被任何列表关联，界面会标成「未被使用」。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryView {
    pub path: String,
    /// 该图库的索引是否已经建立过。
    pub scanned: bool,
    /// 有几个播放列表在用这个图库。0 表示它还没被任何列表用到（不会被播到）。
    pub used_by: usize,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExcludeView {
    /// 目录前缀规则。图库页的目录树复选框改的就是它。
    ///
    /// 原先还有 globs / regexes，随「排除规则」页一起删除了
    /// （见 `wwp_core::config::ExcludeSettings` 的说明）。
    pub dirs: Vec<String>,
}

#[tauri::command]
pub fn settings_info(state: State<'_, AppState>) -> SettingsInfo {
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let index_root = state.index_dir();

    // 播放列表原样交给界面（间隔、填充都在列表上）。
    // **播放顺序固定为随机**，因此这里不再有 order 字段（见 `playback::select`）。
    let playlists: Vec<PlaylistView> = settings
        .playlists
        .iter()
        .map(|p| PlaylistView {
            id: p.id.clone(),
            name: p.name.clone(),
            libraries: p
                .libraries
                .iter()
                .map(|l| l.display().to_string())
                .collect(),
            interval_seconds: p.interval_seconds,
            fit_mode: fit_str(p.fit_mode).to_string(),
        })
        .collect();

    // 图库清单：直接来自配置（顺序 = 用户添加的顺序），不再从播放列表汇总。
    //
    // `usedBy` 还是要数一遍——它回答的是"这个图库会被播到吗"。数的时候按
    // 大小写不敏感匹配（Windows 路径大小写不敏感），否则同一个目录在
    // 播放列表里写成另一种大小写时会被算成"未被使用"。
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for playlist in &settings.playlists {
        for library in &playlist.libraries {
            *counts
                .entry(library.to_string_lossy().to_ascii_lowercase())
                .or_insert(0) += 1;
        }
    }
    let mut libraries: Vec<LibraryView> = settings
        .libraries
        .iter()
        .map(|library| {
            let path = library.display().to_string();
            let id = library::library_id(library);
            let paths = IndexPaths::new(&index_root, &id);
            LibraryView {
                used_by: counts
                    .get(&path.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(0),
                path,
                scanned: paths.rows.is_file(),
            }
        })
        .collect();
    // **按路径排序**（用户要求"按左侧那一列的名称排序"）。
    //
    // 在**这里**排而不是在界面上各排各的：图库页的清单、播放列表里的勾选清单、
    // 诊断里的顺序都读这一份，排一次就全都一致了（§7.3 的老教训：同一件事
    // 有两个实现，就只有一半能工作）。
    //
    // 大小写不敏感（Windows 路径不区分大小写）；`sort_by` 是**稳定**排序，
    // 因此只有大小写不同的两条路径会保持它们在配置里的先后，不会每次刷新都跳。
    libraries.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));

    let monitors = settings
        .monitors
        .iter()
        .map(|(name, monitor)| {
            (
                name.clone(),
                MonitorView {
                    current_playlist: monitor.current_playlist.clone(),
                    switch_playlist_a: monitor.switch_playlist_a.clone(),
                    switch_playlist_b: monitor.switch_playlist_b.clone(),
                    nsfw_wallpaper: monitor.nsfw_wallpaper.clone(),
                    single_image: monitor.single_image.clone(),
                    label: monitor.label.clone(),
                    aspect_min: monitor.filter.aspect_min,
                    aspect_max: monitor.filter.aspect_max,
                    min_width: monitor.filter.min_width,
                    min_height: monitor.filter.min_height,
                    present: true,
                },
            )
        })
        .collect();

    SettingsInfo {
        settings_path: state.settings_path().display().to_string(),
        data_root: state.paths.root.display().to_string(),
        root_source: state.paths.root_source.as_str().to_string(),
        fell_back: state.paths.fallback_reason.is_some(),
        fallback_reason: state.paths.fallback_reason.clone(),
        playlists,
        libraries,
        exclude: ExcludeView {
            dirs: settings.exclude.dirs.clone(),
        },
        general: GeneralView {
            interval_seconds: settings.general.interval_seconds,
            fit_mode: fit_str(settings.general.fit_mode).to_string(),
            minimize_to_tray: settings.general.minimize_to_tray,
            auto_start: settings.general.auto_start,
            start_minimized: settings.general.start_minimized,
            pause_on_fullscreen: settings.general.pause_on_fullscreen,
            nsfw_hotkey: settings.general.nsfw_hotkey.clone(),
            pause_hotkey: settings.general.pause_hotkey.clone(),
            next_hotkey: settings.general.next_hotkey.clone(),
            previous_hotkey: settings.general.previous_hotkey.clone(),
            show_window_hotkey: settings.general.show_window_hotkey.clone(),
            switch_playlists_hotkey: settings.general.switch_playlists_hotkey.clone(),
            cleanup_expanded: settings.general.cleanup_expanded,
            tray_click_action: settings.general.tray_click_action.as_str().to_string(),
        },
        monitors,
    }
}

/// 填充方式 → 界面用的稳定字符串。两者必须一致，否则界面选完再读回来会变样。
fn fit_str(fit: wwp_core::config::FitMode) -> String {
    match fit {
        wwp_core::config::FitMode::Fill => "fill",
        wwp_core::config::FitMode::Fit => "fit",
        wwp_core::config::FitMode::Stretch => "stretch",
        wwp_core::config::FitMode::Center => "center",
    }
    .to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryList {
    pub libraries: Vec<LibraryView>,
}

/// 列出配置里的图库（`settings_info` 已含此信息，这里保留一个语义更窄的入口）。
#[tauri::command]
pub fn library_list(state: State<'_, AppState>) -> LibraryList {
    let info = settings_info(state);
    LibraryList {
        libraries: info.libraries,
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressPayload {
    pub library: String,
    pub files_seen: u64,
    pub candidates: u64,
    pub playable: u64,
    pub unsupported: u64,
    pub unreadable: u64,
    pub reused: u64,
    /// 其中有多少条是从**别的图库**索引里沿用的（见 `ScanProgress::reused_inherited`）。
    pub reused_inherited: u64,
    pub processed: u64,
    pub current: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanDonePayload {
    pub library: String,
    pub cancelled: bool,
    /// 失败时的人类可读原因；成功时为 null。
    pub error: Option<String>,
    /// 成功时的统计。
    pub stats: Option<IndexStats>,
    pub entries: u64,
    pub scanned_at: i64,
    pub scan_millis: u64,
    pub reused_entries: u64,
    /// 其中有多少条是从**别的图库**索引里沿用的（"上一级已经是图库了"那种情况）。
    pub reused_inherited: u64,
    pub probed_entries: u64,
    pub removed: u64,
    /// 旧索引里没有、这次新出现的条目数。
    pub added: u64,
    /// 旧索引里有、但内容变了的条目数。
    pub updated: u64,
    /// 前端直接可用的"少了什么"摘要。
    pub unsupported_formats: Vec<(String, u64)>,
    pub unreadable_samples: Vec<UnreadableSample>,
}

/// "一键重扫全部图库"的结果：每个图库一条，外加合计。
///
/// 失败与"目录不在了"都落在各自的 `libraries[i].error` 里，**不会**中止整批：
/// 一个图库所在的盘没插上时，其余图库照常扫完。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanAllDonePayload {
    /// 每个图库的结果，顺序与设置里的图库清单一致。
    pub libraries: Vec<ScanDonePayload>,
    /// 整批被取消（用户点了「取消扫描」）。
    pub cancelled: bool,
    /// 这一批里没能扫的图库数（目录不存在、权限不够等）。
    pub failed: usize,
    /// 合计：新增 / 更新 / 删除 / 复用 / 重新探测 / 总耗时。
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub reused: u64,
    /// 其中有多少条是从别的图库索引沿用的。
    pub reused_inherited: u64,
    pub probed: u64,
    pub total_millis: u64,
}

/// 发起一次图库扫描。立即返回，进度通过事件推送。
///
/// 同一个时刻只允许一次扫描：两次并行扫描会争抢同一份索引文件，
/// 后写的那次会把前一次的结果覆盖掉，而用户看到的是两个进度条。
#[tauri::command]
pub fn library_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    rescan: Option<bool>,
) -> Result<(), String> {
    // ## 这几条日志的时间戳是刻意的排查手段
    //
    // 用户报过两次"扫描一直停在正在准备，像卡死了"。排查时缺的正是
    // **"命令走到哪一步了"**：界面上只显示「正在准备」，而它对应三种
    // 完全不同的情况——
    //   1. 这个命令还没返回（卡在下面的 `is_dir` 或取设置上），
    //      于是界面连"正在扫描"都还没开始显示；
    //   2. 命令返回了、线程起来了，但第一个进度事件还没到；
    //   3. 扫描已经在跑，只是某个阶段真的很慢。
    //
    // 有了带时间戳的这几行，拿到日志就能一眼分辨是哪一种。
    // 之前没有它们，我只能靠猜（而且猜错过一次）。
    log::info!("收到扫描请求：{path}（强制全量：{}）", rescan.unwrap_or(false));

    let root = PathBuf::from(&path);
    // `is_dir()` 在网络盘或刚断开的移动盘上可能阻塞很久。因此单独记一条，
    // 这样"卡在这一步"与"卡在后面"在日志里能分开。
    let is_dir = root.is_dir();
    log::info!("目录检查完成（is_dir={is_dir}），用时见上一行时间戳之差");
    if !is_dir {
        return Err(format!("目录不存在：{path}"));
    }

    if state.scanning.swap(true, Ordering::SeqCst) {
        log::warn!("拒绝扫描：已有扫描正在进行");
        return Err("已有扫描正在进行，请先等待它结束或取消。".to_string());
    }
    state.scan_cancel.store(false, Ordering::SeqCst);

    let settings: Settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let index_root = state.index_dir();
    let cancel = Arc::clone(&state.scan_cancel);
    let scanning = Arc::clone(&state.scanning);
    let scan_status = Arc::clone(&state.scan_status);

    // 标记"开始扫描"。界面可以随时查这份状态，因此切到别的页也能看到进度。
    {
        let mut status = scan_status.lock().unwrap_or_else(|e| e.into_inner());
        *status = crate::ScanStatus {
            library: Some(path.clone()),
            started: Some(std::time::Instant::now()),
            latest: None,
            finished: false,
            outcome: None,
            // 单库扫描不属于任何"一批"。
            batch: None,
        };
    }

    log::info!("开始扫描图库：{path}（强制全量：{}）", rescan.unwrap_or(false));

    // 用 `Builder` 而不是 `spawn`：这样线程**创建失败**也能被看见。
    //
    // `std::thread::spawn` 在创建失败时会 panic（而不是返回错误）。那种情况下
    // `scanning` 会永远留在 `true`——界面永远显示"正在扫描"，而之后每次点扫描
    // 都被"已有扫描正在进行"拒绝。系统资源紧张时这是真实可能的。
    match std::thread::Builder::new()
        .name("wwp-library-scan".to_string())
        .spawn(move || {
        // "扫一个图库"要做的事全在 `run_one_library_scan` 里（进度上报、panic
        // 兜底、组装结果），"一键重扫全部图库"复用的就是它——因此两条路的
        // 行为不可能不一致（这正是 §7.3 那条教训：同一件事不要有两套实现）。
        let payload = run_one_library_scan(
            &app,
            &settings,
            &index_root,
            &root,
            rescan.unwrap_or(false),
            &cancel,
            &scan_status,
            // 不是"一批"里的一个，因此不报"第 i/N 个"。
            None,
        );

        if let Err(error) = app.emit(EVENT_SCAN_DONE, &payload) {
            log::debug!("推送扫描结果失败：{error}");
        }
        // 收尾：释放"正在扫描"标志，并给共享状态留下结论。
        scanning.store(false, Ordering::SeqCst);
        if let Ok(mut status) = scan_status.lock() {
            status.finished = true;
            status.outcome = Some(describe_scan(&payload));
        }
        })
    {
        Ok(_handle) => {}
        Err(error) => {
            // 线程开不出来：**必须把 `scanning` 放回去**。
            //
            // 否则界面会永远停在"正在扫描"，而且之后每次点扫描都被
            // "已有扫描正在进行"拒绝——用户除了重启程序没有出路。
            // （用 `spawn` 而不是 `Builder` 时，这种情况是 panic，
            //  `scanning` 就留在 true 上，正是上面描述的死局。）
            //
            // 这里用 `state.scanning` / `state.scan_status` 而不是上面那两个
            // 局部 clone——它们已经被 move 进闭包了。`state` 本身一直可用。
            state.scanning.store(false, Ordering::SeqCst);
            if let Ok(mut status) = state.scan_status.lock() {
                status.finished = true;
                status.outcome = Some(format!("无法启动扫描线程：{error}"));
            }
            log::error!("无法启动扫描线程：{error}");
            return Err(format!("无法启动扫描线程：{error}"));
        }
    }

    Ok(())
}

/// 扫描**一个**图库：跑引擎、把进度推到界面与共享状态、组装成一份结果报告。
///
/// 只在扫描线程里调用。单库扫描与"一键重扫全部图库"都走它——那两件事除了
/// "扫几个"以外完全一样，分成两份实现迟早会出现"有一边忘了更新状态"这类
/// 只有一半能工作的问题。
///
/// 它**不**发结束事件：发 `EVENT_SCAN_DONE` 还是 `EVENT_SCAN_ALL_DONE`
/// 由调用方决定，因为两种结束态的语义不同（见那两个常量的说明）。
fn run_one_library_scan(
    app: &AppHandle,
    settings: &Settings,
    index_root: &std::path::Path,
    root: &std::path::Path,
    rescan: bool,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    scan_status: &Arc<std::sync::Mutex<crate::ScanStatus>>,
    batch: Option<(usize, usize)>,
) -> ScanDonePayload {
    let library_label = root.display().to_string();

    // 每换一个图库就把"最近一次进度"清掉。不清的话，界面上那条进度条会先显示
    // 上一个图库的候选数（比如"已处理 5000/5000"），而新图库其实刚开始——
    // 看起来就像"这个库不用扫就已经好了"。
    if let Ok(mut status) = scan_status.lock() {
        status.library = Some(library_label.clone());
        status.batch = batch;
        status.latest = None;
    }

    let options = library::ScanOptions {
        incremental: !rescan,
        ..Default::default()
    };
    // 祖先图库的索引：新加的**下级图库**可以直接沿用里面每条记录的尺寸与格式，
    // 不必把每个文件都重新打开读一遍头部（用户提的"上一级已经是图库了"）。
    //
    // 挑法与取舍见 `index::pick_ancestors` / `scan_library` 的 `inherited` 参数：
    // 自己不扫（`rescan`）时不传，因为"强制全量重扫"的意思就是逐个重新打开。
    let inherited = if rescan {
        Vec::new()
    } else {
        inherited_entries_for(app, root)
    };
    let inherited_count = inherited.len();
    if inherited_count > 0 {
        log::info!(
            "扫描 {library_label} 时可以沿用其他图库的 {inherited_count} 条记录（不再逐个打开文件）"
        );
    }
    let status_for_progress = Arc::clone(scan_status);
    let progress_app = app.clone();
    let progress_library = library_label.clone();
    // 实际沿用了多少条（进度里报的最后一个值就是最终值）。
    let mut reused_inherited = 0u64;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        library::scan_library(
            root,
            index_root,
            settings,
            &options,
            &inherited,
            |progress: &ScanProgress| {
                let payload = ScanProgressPayload {
                    library: progress_library.clone(),
                    files_seen: progress.files_seen,
                    candidates: progress.candidates,
                    playable: progress.playable,
                    unsupported: progress.unsupported,
                    unreadable: progress.unreadable,
                    reused: progress.reused,
                    reused_inherited: progress.reused_inherited,
                    processed: progress.processed,
                    current: progress.current.clone(),
                };
                if let Err(error) = progress_app.emit(EVENT_SCAN_PROGRESS, payload) {
                    // 界面可能已经关掉；扫描本身继续跑完，索引照样写盘。
                    log::debug!("推送扫描进度失败：{error}");
                }
                // 把最新进度存进共享状态：这样**切到别的页也能查到**，
                // 而不只是图库页那个订阅了事件的进度条能显示。
                if let Ok(mut status) = status_for_progress.lock() {
                    status.latest = Some(progress.clone());
                }
                reused_inherited = progress.reused_inherited;
                // 返回 false 即取消。取消后引擎不写索引，磁盘上仍是上一次的完整索引。
                !cancel.load(Ordering::SeqCst)
            },
        )
    }));

    // 扫描线程崩了（panic）也要给界面一个明确结果。
    //
    // 原先这里是 `match outcome { Ok(..) => .., Err(..) => .. }`，直接对
    // `scan_library` 的结果做匹配。加了 `catch_unwind` 之后 `outcome` 是
    // `Result<Result<ScanOutcome>, Box<dyn Any>>`，要展平。
    let outcome = match outcome {
        Ok(inner) => inner,
        Err(panic) => {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "未知原因".to_string());
            log::error!("扫描线程崩溃：{library_label} —— {detail}");
            Err(wwp_core::error::Error::config(
                "扫描过程中发生内部错误",
                detail,
            ))
        }
    };

    match outcome {
        Ok(ScanOutcome {
            meta,
            entries,
            cancelled,
        }) => {
            let mut formats: Vec<(String, u64)> = meta
                .stats
                .unsupported_formats
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            formats.sort_by(|a, b| b.1.cmp(&a.1));

            if cancelled {
                log::info!("扫描被取消：{library_label}");
            } else {
                log::info!(
                    "扫描完成：{library_label}，可播放 {}，新增 {}、更新 {}、删除 {}，\
                     耗时 {} ms（复用 {}，其中沿用其他图库 {}）",
                    meta.stats.playable,
                    meta.added,
                    meta.updated,
                    meta.removed,
                    meta.scan_millis,
                    meta.reused_entries,
                    reused_inherited
                );
            }

            ScanDonePayload {
                library: library_label,
                cancelled,
                error: None,
                stats: Some(meta.stats.clone()),
                entries: entries.len() as u64,
                scanned_at: meta.scanned_at,
                scan_millis: meta.scan_millis,
                reused_entries: meta.reused_entries,
                reused_inherited,
                probed_entries: meta.probed_entries,
                removed: meta.removed,
                added: meta.added,
                updated: meta.updated,
                unsupported_formats: formats,
                unreadable_samples: meta.stats.unreadable_samples.clone(),
            }
        }
        Err(error) => {
            log::error!("扫描失败：{library_label} —— {error}");
            ScanDonePayload {
                library: library_label,
                cancelled: false,
                error: Some(error.to_string()),
                stats: None,
                entries: 0,
                scanned_at: 0,
                scan_millis: 0,
                reused_entries: 0,
                reused_inherited: 0,
                probed_entries: 0,
                removed: 0,
                added: 0,
                updated: 0,
                unsupported_formats: Vec::new(),
                unreadable_samples: Vec::new(),
            }
        }
    }
}

/// 扫描某个图库前，从**祖先图库**的索引里收集可沿用的记录。
///
/// "祖先"= 路径上是它上级的那些图库（`F:\素材` 之于 `F:\素材\2024`）。
/// 用户可以先把上级目录加成图库，之后又单独加下级目录——这时下级图库里
/// 每个文件的尺寸与格式其实已经在上级的索引里了，重新打开纯属白费。
///
/// ## 只挑祖先，不是把所有图库的索引都读一遍
///
/// 读一份索引要解析整个 JSONL（几万条）。把所有图库都读进来会变成
/// "扫一个小图库先把几十万条记录读进内存"，比重新探测还慢，而且毫无命中。
/// 路径上不相干的图库因此直接跳过（`pick_ancestors` 负责这一步）。
fn inherited_entries_for(app: &AppHandle, root: &std::path::Path) -> Vec<LibraryEntry> {
    let state = app.state::<AppState>();
    let settings = state.settings();

    let others: Vec<std::path::PathBuf> = settings
        .libraries
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    let ancestors = library::index::pick_ancestors(root, &others);
    if ancestors.is_empty() {
        return Vec::new();
    }

    let (entries, without_index) = library::index::collect_entries(&state.index_dir(), &ancestors);    log::info!(
        "图库 {} 的祖先图库 {} 个（其中 {} 个还没有索引），共读到 {} 条可复用记录",
        root.display(),
        ancestors.len(),
        without_index,
        entries.len()
    );
    entries
}

/// 一次单库扫描的结论写成一句话，给共享状态（界面上那条浮窗的收尾提示）用。
fn describe_scan(payload: &ScanDonePayload) -> String {
    if payload.cancelled {
        "已取消".to_string()
    } else if let Some(error) = &payload.error {
        format!("失败：{error}")
    } else {
        format!(
            "完成，可播放 {} 张",
            payload.stats.as_ref().map(|s| s.playable).unwrap_or(0)
        )
    }
}

/// 一键把**全部**图库各重扫一次（增量）。
///
/// ## 为什么是后端串行扫、而不是前端一个个点
///
/// 前端循环调用 `library_scan` 也能得到同样的结果，但"扫到第几个了"、
/// "取消要取消整批还是这一个"、"某一库所在盘没插上要不要继续"这三件事
/// 都会散在前端的一个循环里——而后端本来就有一份全局扫描状态
/// （`scanning` / `scan_status`），同一件事放两个地方就是两套机制。
///
/// ## 语义
///
/// - **增量**：每个图库都用 `incremental`，未变化的文件一个都不重新打开
///   （用户明确要求"已有的数据不做修改，只做增量"）；
/// - **逐个串行**：并行扫多个图库只会让磁盘队列更长，而进度也没法讲清楚；
/// - **一个图库失败不影响其余**：目录被删/盘没插上都会记进它自己那条结果里，
///   然后继续扫下一个（"某个盘没插上"不该让整次操作白做）；
/// - **取消**：界面上那个「取消扫描」停的是整批（当前这个库跑完它的检查点就停）。
///
/// 返回的是这一批要扫几个图库，界面用它做提示。立即返回，结果走事件。
#[tauri::command]
pub fn library_scan_all(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let settings: Settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let roots: Vec<PathBuf> = settings.libraries.clone();

    // 一个图库都没有时明确拒绝：界面能把这句话原样显示给用户
    // （"扫了 0 个，完成"这种结论等于什么都没说）。
    if roots.is_empty() {
        return Err("还没有图库。先在「图库」页用「新增图库」加一个目录。".to_string());
    }

    if state.scanning.swap(true, Ordering::SeqCst) {
        log::warn!("拒绝一键重扫：已有扫描正在进行");
        return Err("已有扫描正在进行，请先等待它结束或取消。".to_string());
    }
    state.scan_cancel.store(false, Ordering::SeqCst);

    let total = roots.len();
    let index_root = state.index_dir();
    let cancel = Arc::clone(&state.scan_cancel);
    let scanning = Arc::clone(&state.scanning);
    let scan_status = Arc::clone(&state.scan_status);

    // 与单库扫描同样的理由先标记一次：界面不必等线程起来才显示"正在扫描"。
    {
        let mut status = scan_status.lock().unwrap_or_else(|e| e.into_inner());
        *status = crate::ScanStatus {
            library: roots.first().map(|p| p.display().to_string()),
            started: Some(std::time::Instant::now()),
            latest: None,
            finished: false,
            outcome: None,
            batch: Some((1, total)),
        };
    }

    log::info!("开始一键重扫全部图库：共 {total} 个（增量）");

    match std::thread::Builder::new()
        .name("wwp-library-scan-all".to_string())
        .spawn(move || {
            let mut results: Vec<ScanDonePayload> = Vec::with_capacity(total);
            // "提前停下来了"——由库与库之间那次检查发现的取消。
            //
            // ## 为什么不能只看 `results` 里有没有 cancelled
            //
            // 取消可能正好落在**两个库之间**：上一个库已经扫完（它的进度回调
            // 不会再被调用，因此它不知道用户点了取消），下一次循环的检查才发现。
            // 那种情况下没有任何一条结果带 `cancelled`，只按结果去推就会
            // **把"只扫了 3 个就停了"报成"完整的 3 个"**——用户看不出来
            // 后面那些还没扫。
            let mut stopped_early = false;
            for (index, root) in roots.iter().enumerate() {
                // 取消是在**库与库之间**生效的：当前这个库由它自己的检查点停下。
                if cancel.load(Ordering::SeqCst) {
                    log::info!("一键重扫：收到取消请求，剩余图库不再处理");
                    stopped_early = true;
                    break;
                }
                let payload = run_one_library_scan(
                    &app,
                    &settings,
                    &index_root,
                    root,
                    // **增量**：这是这个按钮的全部意义所在。
                    false,
                    &cancel,
                    &scan_status,
                    Some((index + 1, total)),
                );
                let cancelled = payload.cancelled;
                results.push(payload);
                if cancelled {
                    // 当前这个库在它的检查点上被取消了。
                    stopped_early = true;
                    break;
                }
            }

            let payload = ScanAllDonePayload {
                cancelled: stopped_early,
                failed: results.iter().filter(|r| r.error.is_some()).count(),
                added: results.iter().map(|r| r.added).sum(),
                updated: results.iter().map(|r| r.updated).sum(),
                removed: results.iter().map(|r| r.removed).sum(),
                reused: results.iter().map(|r| r.reused_entries).sum(),
                reused_inherited: results.iter().map(|r| r.reused_inherited).sum(),
                probed: results.iter().map(|r| r.probed_entries).sum(),
                total_millis: results.iter().map(|r| r.scan_millis).sum(),
                libraries: results,
            };

            if let Err(error) = app.emit(EVENT_SCAN_ALL_DONE, &payload) {
                log::debug!("推送重扫结果失败：{error}");
            }

            log::info!(
                "一键重扫结束：扫了 {} 个图库（失败 {}），新增 {}、更新 {}、删除 {}，\
                 复用 {}，合计 {} ms{}",
                payload.libraries.len(),
                payload.failed,
                payload.added,
                payload.updated,
                payload.removed,
                payload.reused,
                payload.total_millis,
                if payload.cancelled { "（已取消）" } else { "" }
            );

            // 收尾：释放"正在扫描"标志，并给共享状态留下结论。
            scanning.store(false, Ordering::SeqCst);
            if let Ok(mut status) = scan_status.lock() {
                status.finished = true;
                status.outcome = Some(describe_scan_all(&payload));
            }
        })
    {
        Ok(_handle) => {}
        Err(error) => {
            // 与单库扫描同样的死局防线：线程开不出来必须把标志放回去，
            // 否则界面永远"正在扫描"，之后每次都报"已有扫描正在进行"。
            state.scanning.store(false, Ordering::SeqCst);
            if let Ok(mut status) = state.scan_status.lock() {
                status.finished = true;
                status.outcome = Some(format!("无法启动扫描线程：{error}"));
            }
            log::error!("无法启动扫全部图库的线程：{error}");
            return Err(format!("无法启动扫描线程：{error}"));
        }
    }

    Ok(total)
}

/// 一键重扫的结论写成一句话（共享状态里那份收尾提示用）。
fn describe_scan_all(payload: &ScanAllDonePayload) -> String {
    if payload.cancelled {
        return format!("已取消（已扫完 {} 个图库）", payload.libraries.len());
    }
    let mut text = format!(
        "完成：{} 个图库，新增 {}、更新 {}、删除 {}",
        payload.libraries.len(),
        payload.added,
        payload.updated,
        payload.removed
    );
    if payload.failed > 0 {
        text.push_str(&format!("（{} 个没能扫）", payload.failed));
    }
    text
}

/// 查询当前（或最近一次）扫描的状态。
///
/// 界面用它做两件事：
///   1. 在**任何页面**的右上角显示一条常驻进度——切页后也看得到（原先只有
///      图库页订阅了进度事件，切走就什么都不显示了，用户会以为扫描停了）；
///   2. 显示"已经跑了多久"。卡住时这是用户唯一能判断"还在动 vs 死了"的依据。
#[tauri::command]
pub fn library_scan_status(state: State<'_, AppState>) -> ScanStatusView {
    let scanning = state.scanning.load(Ordering::SeqCst);
    let status = state.scan_status.lock().unwrap_or_else(|e| e.into_inner());

    // 命令被调用过就记一条 debug。
    //
    // ## 为什么值得为"被调用"这件事专门记日志
    //
    // 界面上那条扫描浮窗依赖这个命令。实测遇到过"浮窗完全不出现"，
    // 而它能由三种完全不同的原因造成：命令没被前端调用、命令调不通、
    // 或者前端拿到了数据但没渲染。**这三种从界面上看是同一个现象**，
    // 光靠读代码分不出来。
    //
    // 有了这条日志，一次实测就能定位到是哪一种：日志里没有这一行，
    // 就说明前端根本没发起调用（问题在前端）；有这一行却还是没浮窗，
    // 就说明问题在渲染。
    //
    // 用 debug 级别：它每秒会被调用两次左右，记成 info 就是新的噪音。
    log::debug!(
        "查询扫描状态：running={scanning}，已收到进度={}",
        status.latest.is_some()
    );

    let elapsed = status
        .started
        .map(|s| s.elapsed().as_secs())
        .unwrap_or(0);
    // 取一份进度的拷贝（ScanProgress 里全是可复制的字段）。
    let progress = status.latest.as_ref().map(|p| ScanProgressPayload {
        library: status.library.clone().unwrap_or_default(),
        files_seen: p.files_seen,
        candidates: p.candidates,
        playable: p.playable,
        unsupported: p.unsupported,
        unreadable: p.unreadable,
        reused: p.reused,
        reused_inherited: p.reused_inherited,
        processed: p.processed,
        current: p.current.clone(),
    });

    ScanStatusView {
        // 以 `scanning` 标志为准：它是"线程是否活着"的唯一真相。
        // 共享状态里的 `finished` 只是给界面收起提示用的。
        running: scanning,
        library: status.library.clone(),
        elapsed_seconds: if scanning { elapsed } else { 0 },
        progress,
        finished: status.finished,
        outcome: status.outcome.clone(),
        batch_index: status.batch.map(|(index, _)| index),
        batch_total: status.batch.map(|(_, total)| total),
    }
}

/// 扫描状态的界面视图。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatusView {
    /// 是否正在扫描。
    pub running: bool,
    /// 正在扫描哪个图库。
    pub library: Option<String>,
    /// 已经跑了多少秒。**卡死检测的关键**：用户能看出"数字不动了"。
    pub elapsed_seconds: u64,
    /// 最近一次进度。
    pub progress: Option<ScanProgressPayload>,
    /// 是否已经结束（成功、失败或取消）。
    pub finished: bool,
    /// 结束时的结论文字。
    pub outcome: Option<String>,
    /// "一键重扫全部图库"时：当前是第几个（1 基）。单库扫描时为 null。
    ///
    /// 单独给两个数字而不是让界面从进度里推：进度里的候选数是**这一个图库**的，
    /// 推不出"一共几个库、现在第几个"。
    pub batch_index: Option<usize>,
    /// "一键重扫全部图库"时：这一批一共几个图库。
    pub batch_total: Option<usize>,
}

/// 请求取消正在进行的扫描。
///
/// ## 为什么"取消"要在 5 秒后**强制**释放状态
///
/// 正常的取消流程是：这里把 `scan_cancel` 置真，扫描线程在下一个检查点
/// 看到它、收尾、把自己的 `scanning` 置假。
///
/// 但那条路径有前提：线程还活着、并且很快会到达检查点。实测遇到过不满足的
/// 情况——扫描卡在某个系统调用里（僵住的磁盘、断开的网络盘），
/// 于是用户点了"取消"，界面却一直显示"正在扫描"，而且**再点扫描会被
/// "已有扫描正在进行"直接拒绝**。用户面对的是一个永久卡死的界面，
/// 除了重启程序没有任何出路。
///
/// 因此这里加一个逃生口：请求取消后如果 5 秒内没停下来，就**强制**把
/// `scanning` 置假，让界面恢复可用。
///
/// ## 强制释放的代价（诚实说明）
///
/// 那个卡住的线程仍然在跑。如果它之后醒过来并走到 `index::save`，
/// 会写出一份"这次扫描的结果"——而用户以为已经取消了。
/// 这个风险是刻意接受的，理由是：
///   - 留在那里卡死是**确定**的坏结果（界面不可用、无法扫描任何图库）；
///   - 写出索引是**可能**的坏结果，而且写入是原子的（临时文件 + 改名），
///     不会留下半截文件；
///   - 真正的卡死（磁盘/网络永久失效）下，那个线程通常再也回不来。
///
/// 另一个选择是等更久（比如 30 秒），但那会让"取消"这个操作在用户眼里
/// 依然是坏的——他按了取消，界面却在半分钟里毫无反应。
#[tauri::command]
pub fn library_cancel_scan(
    state: State<'_, AppState>,
    force_after_seconds: Option<u64>,
) -> bool {
    if !state.scanning.load(Ordering::SeqCst) {
        return false;
    }
    state.scan_cancel.store(true, Ordering::SeqCst);
    log::info!("已请求取消扫描");

    // 起一个守护线程：等一会儿，如果还没停就强制释放。
    let scanning = Arc::clone(&state.scanning);
    let cancel = Arc::clone(&state.scan_cancel);
    let status = Arc::clone(&state.scan_status);
    let timeout = std::time::Duration::from_secs(force_after_seconds.unwrap_or(5).max(1));
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if !scanning.load(Ordering::SeqCst) {
                // 正常收尾了，什么都不用做。
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if scanning.load(Ordering::SeqCst) {
            log::warn!(
                "取消请求发出 {} 秒后扫描仍未停止，强制释放状态以便界面继续使用\
                 （那个线程可能仍在后台运行，直到它自己走到检查点）",
                timeout.as_secs()
            );
            scanning.store(false, Ordering::SeqCst);
            if let Ok(mut s) = status.lock() {
                s.finished = true;
                s.outcome = Some("已取消（强制停止）".to_string());
            }
            // 保持 scan_cancel 为真：那个线程万一醒了，应当立刻自行停止。
            let _ = cancel;
        }
    });

    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    /// 索引是否已建立。
    pub scanned: bool,
    pub stats: Option<IndexStats>,
    pub entries: u64,
    pub playable: u64,
    pub unsupported_formats: Vec<(String, u64)>,
    pub unreadable_samples: Vec<UnreadableSample>,
    pub scanned_at: i64,
    pub scan_millis: u64,
    pub reused_entries: u64,
    /// 沿用其他图库的记录数。**这一页永远是 0**：它是某一次扫描的经过，
    /// 没有落盘（见 `library_stats` 里的说明），要看它请用扫描结果卡片。
    pub reused_inherited: u64,
    pub probed_entries: u64,
    /// 已建立索引里的可播放条目数（与 stats.playable 应当一致）。
    pub index_path: String,
}

/// 读取某个图库已建立的索引概况，不重新扫描。
#[tauri::command]
pub fn library_stats(state: State<'_, AppState>, path: String) -> Result<LibraryStats, String> {
    let root = PathBuf::from(&path);
    let paths = IndexPaths::new(&state.index_dir(), &library::library_id(&root));

    if !paths.rows.is_file() {
        return Ok(LibraryStats {
            scanned: false,
            stats: None,
            entries: 0,
            playable: 0,
            unsupported_formats: Vec::new(),
            unreadable_samples: Vec::new(),
            scanned_at: 0,
            scan_millis: 0,
            reused_entries: 0,
            reused_inherited: 0,
            probed_entries: 0,
            index_path: paths.rows.display().to_string(),
        });
    }

    let (meta, entries) = library::load_index(&paths).map_err(|e| e.to_string())?;
    let mut formats: Vec<(String, u64)> = meta
        .stats
        .unsupported_formats
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    formats.sort_by(|a, b| b.1.cmp(&a.1));

    Ok(LibraryStats {
        scanned: true,
        playable: entries.iter().filter(|e| e.is_playable()).count() as u64,
        entries: entries.len() as u64,
        unsupported_formats: formats,
        unreadable_samples: meta.stats.unreadable_samples.clone(),
        scanned_at: meta.scanned_at,
        scan_millis: meta.scan_millis,
        reused_entries: meta.reused_entries,
        // 这一页读的是**已经写在盘上的索引**，而"其中有多少条是沿用别的图库的"
        // 没有落盘（它是那一次扫描的经过，不是索引的内容）。
        // 因此这里如实报 0，界面不显示这一项；要看这个数请用扫描结果卡片。
        reused_inherited: 0,
        probed_entries: meta.probed_entries,
        stats: Some(meta.stats),
        index_path: paths.rows.display().to_string(),
    })
}

/// 读取索引里的条目（分页，避免把五万条一次性推给界面）。
#[tauri::command]
pub fn library_entries(
    state: State<'_, AppState>,
    path: String,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<LibraryEntry>, String> {
    let root = PathBuf::from(&path);
    let paths = IndexPaths::new(&state.index_dir(), &library::library_id(&root));
    let (_, entries) = library::load_index(&paths).map_err(|e| e.to_string())?;

    let offset = offset.unwrap_or(0) as usize;
    let limit = limit.unwrap_or(200).min(2000) as usize;
    Ok(entries.into_iter().skip(offset).take(limit).collect())
}

// ---------------------------------------------------------------------------
// 设置读写
// ---------------------------------------------------------------------------

/// 界面提交的设置修改。
///
/// 只暴露界面真正会改的字段，而不是整个 `Settings`：这样界面不可能意外清掉
/// 它不认识的部分（例如用户手写的图库列表），也避免把内部结构当成协议。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub interval_seconds: Option<u64>,
    pub fit_mode: Option<String>,
    pub minimize_to_tray: Option<bool>,
    /// 启动后是否自动接管壁纸。
    pub auto_start: Option<bool>,
    /// 启动时是否不显示主界面。
    pub start_minimized: Option<bool>,
    /// 某块屏上有程序全屏（含最大化）时，只暂停这块屏的轮播。
    pub pause_on_fullscreen: Option<bool>,
    /// NSFW 模式的全局快捷键（`Ctrl+Alt+S`）。
    pub nsfw_hotkey: Option<String>,
    /// 全局"暂停/继续"的快捷键。
    pub pause_hotkey: Option<String>,
    /// 全局"下一张"的快捷键。
    pub next_hotkey: Option<String>,
    /// 全局"上一张"的快捷键。
    pub previous_hotkey: Option<String>,
    /// 全局"显示主界面"的快捷键。
    pub show_window_hotkey: Option<String>,
    /// 每块屏在两个播放列表之间往返切换的全局快捷键。
    pub switch_playlists_hotkey: Option<String>,
    /// 设置页「清理程序目录」那张卡片展开还是折叠（用户在界面上点一次就存一次）。
    pub cleanup_expanded: Option<bool>,
    /// 单击托盘图标做什么：`open-window` | `next-wallpaper`。
    pub tray_click_action: Option<String>,
    /// 每屏修改：键是显示器名，值是只包含"要改的字段"的对象。
    #[serde(default)]
    pub monitors: std::collections::BTreeMap<String, MonitorPatch>,
    /// 全局排除规则。给 `Some` 就整体替换。
    pub exclude: Option<ExcludePatch>,
    /// 图库清单。给 `Some` 就**整体替换**（「图库」页新增/移除走这里）。
    ///
    /// 与播放列表同样的理由用整体替换：界面一次显示全部、用户直接编辑，
    /// 整体替换让后端状态与"界面上现在显示什么"完全一致。
    ///
    /// 移除一个图库时，后端会**顺手把播放列表里对它的引用一起清掉**：
    /// 不清的话那条引用会在下次读盘时被 `adopt_missing_libraries` 补回来，
    /// 于是"移除了但重启又出现"。
    pub libraries: Option<Vec<String>>,
    /// 播放列表。给 `Some` 就**整体替换**。
    ///
    /// ## 为什么是"整体替换"而不是"加一个/删一个"
    ///
    /// 界面上的列表本来就是一次显示全部、用户直接编辑的。用整体替换可以让
    /// 后端逻辑与"界面现在显示什么"完全一致——不会出现"界面删了第 2 条、
    /// 后端理解成删第 3 条"这类顺序依赖的 bug。
    ///
    /// 界面负责生成 id（新建时）与保持 id 稳定（改名/改内容时）。
    /// 后端会校验 id 唯一、名称非空、填充方式认得出；**图库目录是否存在不校验**
    /// （盘可能没插上、目录可能被改名），见 `apply_playlist_list`。
    pub playlists: Option<Vec<PlaylistPatch>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistPatch {
    /// 已存在的列表带上原 id；新建的传空串，后端会分配一个。
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub libraries: Vec<String>,
    pub interval_seconds: u64,
    pub fit_mode: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorPatch {
    /// 当前选中的播放列表 id。空串表示清空（该屏无内容可播）。
    pub current_playlist: Option<String>,
    /// 「切换播放列表」两个槽位。空串表示清空这一槽（这块屏就不参与切换）。
    pub switch_playlist_a: Option<String>,
    pub switch_playlist_b: Option<String>,
    /// NSFW 模式时这块屏显示哪张图。空串表示清空。
    pub nsfw_wallpaper: Option<String>,
    /// 这块屏的固定单图。空串表示清空（回到按播放列表轮播）。
    pub single_image: Option<String>,
    /// 这块屏在界面上的显示名。空串表示恢复成真实显示器名。
    ///
    /// 只有 ASCII 字母/数字/空格/`-`/`_` 会被保留（见
    /// `wwp_core::config::sanitize_monitor_label`）——用户要求"支持英文和数字就行"。
    pub label: Option<String>,
    pub aspect_min: Option<f64>,
    pub aspect_max: Option<f64>,
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    /// 用 `null` 表示清除该字段（例如取消某个筛选条件）。
    /// 之所以要区分"没提到"与"清成 None"：界面上"清空一个输入框"与
    /// "没碰这个字段"是两件不同的事。
    #[serde(default)]
    pub filter_present: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludePatch {
    pub dirs: Vec<String>,
}

fn parse_fit(text: &str) -> Option<wwp_core::config::FitMode> {
    match text {
        "fill" => Some(wwp_core::config::FitMode::Fill),
        "fit" => Some(wwp_core::config::FitMode::Fit),
        "stretch" => Some(wwp_core::config::FitMode::Stretch),
        "center" => Some(wwp_core::config::FitMode::Center),
        _ => None,
    }
}

/// 校验并规范化一个全局快捷键的写法。
///
/// ## 写错时不拒绝保存
///
/// 拒绝会让用户连"把写错的那个清空"都做不到（保存被拒 → 磁盘上还是错的）。
/// 因此**原样存下**并记一条 warn：界面上看到的就是他打的那串字，
/// 下次他能改对；而我们的日志里留下了"它没生效"的事实。
///
/// 空串是"不注册"，不是错误——用户清空一个快捷键时不该收到一条警告。
fn normalize_hotkey(field: &str, raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    match wwp_core::hotkey::parse(raw) {
        // 回写成规范写法（`ctrl+alt+left` → `Ctrl+Alt+Left`），
        // 这样界面上显示的与实际注册的一致。
        Ok(parsed) => parsed.to_string(),
        Err(reason) => {
            log::warn!("{field}快捷键「{raw}」无法解析，已原样保存但不生效：{reason}");
            raw.trim().to_string()
        }
    }
}

/// 用界面提交的清单替换 `settings.libraries`，并返回被移除的那些路径。
///
/// ## 为什么抽成独立函数
///
/// 它是**改了配置但看不出效果**那一类 bug 的高发区（本项目就踩过"设置了但完全
/// 没生效"），而 `settings_update` 需要 tauri 的 `State`、进不了测试。
/// 抽出来之后下面两件事都能直接测：
///
/// 1. **大小写不敏感去重**：Windows 路径大小写不敏感，`E:\Pics` 与 `e:\pics`
///    是同一个目录。留着两份会让界面上出现两行看起来一模一样的图库。
/// 2. **移除时要顺手解绑**：播放列表里对已移除图库的引用必须一起清掉，
///    否则下次读盘时 `Settings::adopt_missing_libraries` 会把它补回清单——
///    用户看到的是"我移除了它，重启又回来了"。
///
/// ## 为什么不校验目录是否存在
///
/// 图库所在的盘可能暂时没插上，而前端每次提交的都是**整份清单**——
/// 拒绝保存会让用户连"移除另一个图库"都做不到。这与 `single_image` /
/// `nsfw_wallpaper` 的处理是一致的：路径原样存下，真要扫的时候再报错。
fn apply_library_list(
    settings: &mut wwp_core::config::Settings,
    raw: Vec<String>,
) -> Vec<String> {
    let mut cleaned: Vec<std::path::PathBuf> = Vec::new();
    let mut keep: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in raw {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = std::path::PathBuf::from(trimmed);
        if keep.insert(path.to_string_lossy().to_ascii_lowercase()) {
            cleaned.push(path);
        }
    }

    let removed: Vec<String> = settings
        .libraries
        .iter()
        .map(|l| l.display().to_string())
        .filter(|p| !keep.contains(&p.to_ascii_lowercase()))
        .collect();

    settings.libraries = cleaned;
    if !removed.is_empty() {
        for playlist in settings.playlists.iter_mut() {
            playlist
                .libraries
                .retain(|l| keep.contains(&l.to_string_lossy().to_ascii_lowercase()));
        }
        // 索引文件**不删**：再用同一个路径加回来时可以直接复用，不必重扫几万个文件。
        log::info!("已移除图库 {} 个：{}", removed.len(), removed.join("、"));
    }
    removed
}

/// 套用一份播放列表清单之后的结果（只用于日志）。
#[derive(Debug)]
struct PlaylistApply {
    /// 收下的列表数量。
    count: usize,
    /// 引用了、但目录当前不存在的图库路径（去重后，按首次出现的顺序）。
    missing: Vec<String>,
}

/// 把界面提交的播放列表整体套用到设置上（后端语义就是整体替换）。
///
/// ## 为什么**不**校验图库目录是否存在（2026-10-06 踩的坑）
///
/// 原先这里对每个图库做 `path.is_dir()`，不成立就 `return Err` —— 整份保存被拒。
/// 而界面每次提交的都是**整份列表**，于是用户在资源管理器里把某个图库目录
/// **改了名**（磁盘上一切正常、别的页面也都能用）之后，播放列表页的**每一个**
/// 操作都失败：新建、改名、改间隔、改填充、勾图库、**连删除**都做不到。
/// 他看到的现象就是"这一页卡死了，别的页面正常"。
///
/// 这与 `apply_library_list` 的理由是一致的（那里写着"盘可能暂时没插上，
/// 拒绝保存会让用户连移除另一个图库都做不到"）：路径**原样存下**，
/// 目录不在只记一条 warn，真正要播的时候由引擎报"这一屏筛选后没有可用的图"。
///
/// 仍然拒绝的只有"说不清要什么"的那几种：名字为空、填充方式认不出、id 重复。
fn apply_playlist_list(
    settings: &mut wwp_core::config::Settings,
    playlists: Vec<PlaylistPatch>,
) -> Result<PlaylistApply, String> {
    let mut cleaned: Vec<wwp_core::config::Playlist> = Vec::new();
    let mut seen_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();

    for entry in playlists {
        let name = entry.name.trim().to_string();
        if name.is_empty() {
            return Err("播放列表的名字不能为空".to_string());
        }
        let fit = parse_fit(&entry.fit_mode)
            .ok_or_else(|| format!("未知的填充方式：{}", entry.fit_mode))?;

        // id：界面给了就用，空串则分配一个。
        // 在"已经收下的"里面找不重复的编号，避免同一个请求里新建两个列表
        // 都拿到 list1。
        let id = if entry.id.trim().is_empty() {
            let mut n = cleaned.len() + 1;
            loop {
                let candidate = format!("list{n}");
                if !seen_ids.contains(&candidate)
                    && !settings.playlists.iter().any(|p| p.id == candidate)
                {
                    break candidate;
                }
                n += 1;
            }
        } else {
            entry.id.trim().to_string()
        };
        if !seen_ids.insert(id.clone()) {
            return Err(format!("播放列表 id 重复：{id}"));
        }

        // 图库路径：去重（大小写不敏感：Windows 路径不区分大小写）。
        //
        // 重复不会崩，但会让同一批图在候选里出现两次——随机播放就会
        // 偏向那些图，而用户看不出原因。
        let mut libs: Vec<std::path::PathBuf> = Vec::new();
        let mut seen_libs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for raw in entry.libraries {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let path = std::path::PathBuf::from(trimmed);
            if !seen_libs.insert(path.to_string_lossy().to_ascii_lowercase()) {
                continue;
            }
            // 目录不在**不**拒绝保存，只记下来给日志用（理由见函数说明）。
            if !path.is_dir() && !missing.iter().any(|m| m.eq_ignore_ascii_case(trimmed)) {
                missing.push(trimmed.to_string());
            }
            libs.push(path);
        }

        cleaned.push(wwp_core::config::Playlist {
            id,
            name,
            libraries: libs,
            interval_seconds: entry.interval_seconds.clamp(10, 86_400),
            fit_mode: fit,
        });
    }

    let count = cleaned.len();
    // 记录一份 id 集合，用来把每屏指向已删除列表的引用清掉。
    let valid_ids: std::collections::BTreeSet<String> =
        cleaned.iter().map(|p| p.id.clone()).collect();
    settings.playlists = cleaned;

    // 清掉指向已删除列表的绑定。不清的话那块屏会一直"选了但找不到"，
    // 界面显示成"已选 list3"而实际没有任何内容——很难查。
    for (name, monitor) in settings.monitors.iter_mut() {
        if !monitor.current_playlist.is_empty() && !valid_ids.contains(&monitor.current_playlist) {
            log::info!(
                "屏幕 {} 原本指向的播放列表「{}」已不存在，已清空该绑定",
                name,
                monitor.current_playlist
            );
            monitor.current_playlist.clear();
        }
        // 两个切换槽位同理：列表被删掉后槽位留着，那块屏会"看着配好了、
        // 按快捷键却没动静"。清掉它，界面上那两条下拉框会回到「不切换」。
        if !monitor.switch_playlist_a.is_empty() && !valid_ids.contains(&monitor.switch_playlist_a)
        {
            log::info!(
                "屏幕 {} 的切换槽位 A 指向的播放列表「{}」已不存在，已清空",
                name,
                monitor.switch_playlist_a
            );
            monitor.switch_playlist_a.clear();
        }
        if !monitor.switch_playlist_b.is_empty() && !valid_ids.contains(&monitor.switch_playlist_b)
        {
            log::info!(
                "屏幕 {} 的切换槽位 B 指向的播放列表「{}」已不存在，已清空",
                name,
                monitor.switch_playlist_b
            );
            monitor.switch_playlist_b.clear();
        }
    }

    Ok(PlaylistApply { count, missing })
}

/// 应用设置修改并写盘。
///
/// 校验失败必须**整体不生效**：半个修改落盘会让用户面对一个他不认识的配置，
/// 而配置又是他排查问题的唯一依据。
#[tauri::command]
pub fn settings_update(state: State<'_, AppState>, patch: SettingsPatch) -> Result<SettingsInfo, String> {
    let mut settings = state.settings();

    // 记下改动前的关键值：有些改动需要在写盘之后触发别的动作
    // （重新注册快捷键、预热 NSFW 壁纸、热重建轮播）。
    let hotkeys_before = wwp_core::hotkey::HotkeySpecs::from(&settings.general);
    let safe_before: Vec<String> = settings
        .monitors
        .iter()
        .map(|(name, monitor)| format!("{name}={}", monitor.nsfw_wallpaper))
        .collect();

    if let Some(interval) = patch.interval_seconds {
        // 与设计规格 §6 的范围一致：10 秒 ~ 24 小时。
        settings.general.interval_seconds = interval.clamp(10, 86_400);
    }
    if let Some(fit) = &patch.fit_mode {
        settings.general.fit_mode = parse_fit(fit).ok_or_else(|| format!("未知的填充方式：{fit}"))?;
    }
    if let Some(minimize) = patch.minimize_to_tray {
        settings.general.minimize_to_tray = minimize;
    }
    if let Some(auto_start) = patch.auto_start {
        settings.general.auto_start = auto_start;
    }
    if let Some(start_minimized) = patch.start_minimized {
        settings.general.start_minimized = start_minimized;
    }
    if let Some(pause_on_fullscreen) = patch.pause_on_fullscreen {
        settings.general.pause_on_fullscreen = pause_on_fullscreen;
    }
    if let Some(hotkey) = &patch.nsfw_hotkey {
        // 只做一次"能不能解析"的校验并记日志：写错的快捷键不该拒绝整个保存，
        // 但也必须让用户立刻知道它没生效（日志 + 界面上的提示）。
        settings.general.nsfw_hotkey = normalize_hotkey("NSFW 模式", hotkey);
    }
    if let Some(hotkey) = &patch.pause_hotkey {
        settings.general.pause_hotkey = normalize_hotkey("暂停/继续", hotkey);
    }
    if let Some(hotkey) = &patch.next_hotkey {
        settings.general.next_hotkey = normalize_hotkey("下一张", hotkey);
    }
    if let Some(hotkey) = &patch.previous_hotkey {
        settings.general.previous_hotkey = normalize_hotkey("上一张", hotkey);
    }
    if let Some(hotkey) = &patch.show_window_hotkey {
        settings.general.show_window_hotkey = normalize_hotkey("显示主界面", hotkey);
    }
    if let Some(hotkey) = &patch.switch_playlists_hotkey {
        settings.general.switch_playlists_hotkey = normalize_hotkey("切换播放列表", hotkey);
    }
    if let Some(expanded) = patch.cleanup_expanded {
        // 纯界面偏好：存下来只为"下次进设置页照旧收着"，不触发任何别的动作
        // （不重建轮播、不碰壁纸——下面的 `reload` 会因为计划没变而直接返回）。
        settings.general.cleanup_expanded = expanded;
    }
    if let Some(action) = &patch.tray_click_action {
        // 认不出的写法**拒绝保存**而不是退回默认值：这是一个"选一个"的设置项，
        // 静默改成别的值会让用户以为界面上选的那个生效了。
        settings.general.tray_click_action = match action.as_str() {
            "open-window" => wwp_core::config::TrayClickAction::OpenWindow,
            "next-wallpaper" => wwp_core::config::TrayClickAction::NextWallpaper,
            other => return Err(format!("未知的托盘单击行为：{other}")),
        };
    }
    if let Some(exclude) = patch.exclude {
        settings.exclude = wwp_core::config::ExcludeSettings {
            dirs: exclude.dirs,
        };
    }

    // 图库清单：整体替换（「图库」页的新增/移除）。细节见 `apply_library_list`。
    if let Some(libraries) = patch.libraries {
        apply_library_list(&mut settings, libraries);
    }

    // 播放列表：整体替换。细节（尤其"目录不在也不拒绝保存"的理由）见
    // `apply_playlist_list`。
    if let Some(playlists) = patch.playlists {
        let outcome = apply_playlist_list(&mut settings, playlists)?;
        log::info!("播放列表已更新为 {} 项", outcome.count);
        if !outcome.missing.is_empty() {
            log::warn!(
                "有 {} 个图库目录当前不存在，路径已按原样保存（盘没插上、或目录被改名/移走；改回来即可）：{}",
                outcome.missing.len(),
                outcome.missing.join("、")
            );
        }
    }

    for (monitor_name, monitor_patch) in patch.monitors {
        if let Some(current) = &monitor_patch.current_playlist {
            let current = current.trim();
            // 校验：指向的列表必须存在（空串=清空，允许）。
            if !current.is_empty() && settings.playlist(current).is_none() {
                return Err(format!("找不到播放列表：{current}"));
            }
            settings
                .monitors
                .entry(monitor_name.clone())
                .or_default()
                .current_playlist = current.to_string();
        }
        // 「切换播放列表」的两个槽位：空串 = 清空这一槽（该屏就不参与切换）。
        // 与 current_playlist 一样校验"指向的列表必须存在"，避免配置里留下
        // 一个按了快捷键也切不到的死引用。
        if let Some(slot) = &monitor_patch.switch_playlist_a {
            let id = slot.trim();
            if !id.is_empty() && settings.playlist(id).is_none() {
                return Err(format!("找不到播放列表：{id}"));
            }
            settings
                .monitors
                .entry(monitor_name.clone())
                .or_default()
                .switch_playlist_a = id.to_string();
        }
        if let Some(slot) = &monitor_patch.switch_playlist_b {
            let id = slot.trim();
            if !id.is_empty() && settings.playlist(id).is_none() {
                return Err(format!("找不到播放列表：{id}"));
            }
            settings
                .monitors
                .entry(monitor_name.clone())
                .or_default()
                .switch_playlist_b = id.to_string();
        }
        if let Some(nsfw_wallpaper) = &monitor_patch.nsfw_wallpaper {
            let trimmed = nsfw_wallpaper.trim();
            let entry = settings.monitors.entry(monitor_name.clone()).or_default();
            // 只记路径本身，不在这里校验文件是否存在：
            // 图库所在的盘可能暂时没插上，那时拒绝保存会让用户什么都改不了。
            // 真正要设置壁纸时（enter_nsfw）会报出"读不到这张图"。
            entry.nsfw_wallpaper = trimmed.to_string();
        }
        if let Some(single) = &monitor_patch.single_image {
            // 固定单图。同样不校验文件是否存在，理由与 NSFW 壁纸相同：
            // 它所在的盘可能暂时没插上，而拒绝保存会让用户连"改成别的图"都做不到。
            // 真正设图时（apply_single）会把"读不到这张图"报出来。
            let trimmed = single.trim();
            settings
                .monitors
                .entry(monitor_name.clone())
                .or_default()
                .single_image = trimmed.to_string();
        }
        if let Some(label) = &monitor_patch.label {
            // 显示名只保留英文/数字/空格/-/_（用户要求不支持中文）。
            // 清洗而不是拒绝：他双击改名时敲进去一个中文，期望的是"名字改了"，
            // 不是弹一个"保存失败"——而那个字本来也显示不出来。
            let cleaned = wwp_core::config::sanitize_monitor_label(label);
            settings
                .monitors
                .entry(monitor_name.clone())
                .or_default()
                .label = cleaned;
        }
        let entry = settings.monitors.entry(monitor_name).or_default();
        // filter_present 为 true 时整体替换该屏的筛选条件，这样界面可以清空某一项。
        if monitor_patch.filter_present
            || monitor_patch.aspect_min.is_some()
            || monitor_patch.aspect_max.is_some()
            || monitor_patch.min_width.is_some()
            || monitor_patch.min_height.is_some()
        {
            entry.filter = wwp_core::config::FilterSettings {
                aspect_min: monitor_patch.aspect_min,
                aspect_max: monitor_patch.aspect_max,
                min_width: monitor_patch.min_width,
                min_height: monitor_patch.min_height,
            };
        }
    }

    settings
        .save(&state.settings_path())
        .map_err(|e| format!("保存设置失败：{e}"))?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    log::info!("设置已更新并保存");

    // --- 让改动立刻生效（用户明确要求"改了就生效"） ---
    //
    // 顺序有讲究：先把快捷键与 NSFW 壁纸预热处理好，再做轮播的热重建。
    // 重建是阻塞的（要重新解码两张图），放在最后就不必再等它。
    let hotkeys_after = wwp_core::hotkey::HotkeySpecs::from(&settings.general);
    if hotkeys_after != hotkeys_before {
        wwp_core::hidden_window::set_hotkey_specs(&hotkeys_after);
    }

    let safe_after: Vec<String> = settings
        .monitors
        .iter()
        .map(|(name, monitor)| format!("{name}={}", monitor.nsfw_wallpaper))
        .collect();
    if safe_after != safe_before {
        if state.playback.is_nsfw() {
            // **正在 NSFW 模式里改 NSFW 壁纸**：直接把新的那几张切上去。
            //
            // 不这样做的话后端只把新图解好缓存，屏幕上仍然是旧的那张——
            // 用户看到的就是"我改了但它没变"（他报的正是这个）。
            //
            // 这里**不能**再调 `prewarm_nsfw`：那条路会另起一个线程写同一个
            // 产物文件，与这里的解码互相踩。
            match state.playback.reapply_nsfw(&settings) {
                Ok(results) => state.playback.record_nsfw(&results),
                Err(error) => log::warn!("NSFW 模式：按新的 NSFW 壁纸重新切换失败：{error}"),
            }
        } else {
            // 不在 NSFW 模式：提前把它解好，点 NSFW 时才够快。
            state.playback.prewarm_nsfw(&settings);
        }
    }

    match state.playback.reload(&settings) {
        Ok(true) => log::info!("设置改动已立即生效（轮播已按新设置重建）"),
        Ok(false) => {}
        Err(error) => log::warn!("设置已保存，但重建轮播失败：{error}"),
    }

    Ok(settings_info(state))
}

/// 按一次「切换播放列表」：把每块配好两个列表的屏切到另一个列表。
///
/// 由 `broadcast::on_hotkey` 调用。返回一句给人看的结果（进日志、也进提示），
/// 不是给界面解析的协议。
///
/// ## 为什么复用"改 `current_playlist` + `reload`"这条路
///
/// 这正是用户手动在播放页改列表时走的那条路：保存 → `reload` 只重建列表
/// 真的变了的屏、并立刻换一张。于是快捷键的行为与手动改列表**完全一致**，
/// 不需要在播放引擎里再开一条"切列表"的通道（那会多一套需要单独验证的状态）。
///
/// 未配好两个列表、或列表已不存在的屏由 `playlist_switch_targets` 直接跳过；
/// 固定单图的屏照旧只改配置（单图优先于列表，屏幕上不会变）。
pub fn switch_playlists(state: &AppState) -> Result<String, String> {
    let mut settings = state.settings();
    let targets = settings.playlist_switch_targets();
    if targets.is_empty() {
        return Ok("没有屏幕配好两个播放列表，未切换".to_string());
    }

    let mut switched = Vec::new();
    for target in &targets {
        if let Some(monitor) = settings.monitors.get_mut(&target.monitor) {
            monitor.current_playlist = target.to.clone();
        }
        switched.push(format!(
            "{} {} → {}",
            target.monitor,
            if target.from.is_empty() {
                "（未选）"
            } else {
                target.from.as_str()
            },
            target.to
        ));
    }

    settings
        .save(&state.settings_path())
        .map_err(|e| format!("保存设置失败：{e}"))?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    log::info!("切换播放列表：{}", switched.join("；"));

    match state.playback.reload(&settings) {
        Ok(true) => log::info!("播放列表已切换并立即生效"),
        Ok(false) => {}
        Err(error) => log::warn!("播放列表已切换，但重建轮播失败：{error}"),
    }

    Ok(format!("已切换 {} 块屏的播放列表", targets.len()))
}

/// 丢弃内存里的设置，从磁盘重新读取。
#[tauri::command]
pub fn settings_reload(state: State<'_, AppState>) -> Result<SettingsInfo, String> {
    let settings =
        wwp_core::Settings::load_or_create(&state.settings_path()).map_err(|e| e.to_string())?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    log::info!("设置已从磁盘重新读取");
    Ok(settings_info(state))
}

// ---------------------------------------------------------------------------
// 诊断导出
// ---------------------------------------------------------------------------

/// 把诊断信息打包到一个目录里。实现见 `crate::diagnostics`——
/// 那个模块同时服务界面按钮与 `--export-diagnostics` 开关。
#[tauri::command]
pub fn diagnostics_export(
    state: State<'_, AppState>,
) -> Result<crate::diagnostics::DiagnosticsExport, String> {
    crate::diagnostics::export(&state.paths, &state.settings())
}

/// 轮播状态。界面轮询它来刷新各屏显示与倒计时。
///
/// 顺带把显示器的**当前显示模式**也带上：`IDesktopWallpaper::GetMonitorRECT` 返回的
/// 是壁纸槽位的矩形（本机 3840x2160），与实际显示模式（2560x1440）不一致；
/// 界面要显示"这块屏是什么分辨率"就该用后者，否则用户会以为认错了屏。
#[tauri::command]
pub fn playback_status(state: State<'_, AppState>) -> crate::playback_manager::PlaybackStatus {
    let mut status = state.playback.status();

    // 用 EnumDisplayMonitors 的当前结果校正绑定里的尺寸。
    //
    // ## 为什么这里要写得这么小心
    //
    // 这个命令处在**界面每秒轮询十几次**的热路径上。原先的写法有两处浪费：
    //
    //   1. 每次都调 `monitors::enumerate()`。那是 COM 调用
    //      （EnumDisplayMonitors + IDesktopWallpaper 枚举），每秒十几次纯属浪费。
    //   2. 每次都无条件为每块屏打一条 `log::debug!("分辨率修正：…")`。
    //      实测日志里这条在**无限重复**——"由 3840x2160 改为 3840x2160"，
    //      数值根本没变。它会迅速把真正有用的记录淹没，而这个项目的日志
    //      是事后排查的主要依据（release 构建没有控制台）。
    //
    // 修法：先看有没有"尺寸未知"的绑定，**只有那时才去枚举**。
    // 正常情况下这次判断直接通过，零 COM 调用、零日志。
    //
    // 那"显示器真的变了"靠谁发现？靠系统广播：`broadcast::on_display_change`
    // 收到 `WM_DISPLAYCHANGE` 后会用新的显示器列表重建每屏计划
    // （见那个函数的说明）。轮询不需要承担发现变化的职责。
    let needs_refresh = status
        .bindings
        .iter()
        .any(|view| view.width == 0 || view.height == 0);
    if !needs_refresh {
        return status;
    }

    match wwp_core::monitors::enumerate() {
        Ok(monitors) => {
            for view in &mut status.bindings {
                match monitors
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(&view.monitor_name))
                {
                    Some(monitor) => {
                        view.width = monitor.width;
                        view.height = monitor.height;
                        view.primary = monitor.primary;
                    }
                    // 找不到不是错误：显示器刚被拔掉时这里会短暂对不上，
                    // 而广播处理随后会重建计划。
                    None => log::debug!(
                        "显示器 {} 暂时不在枚举结果里（可用：{:?}）",
                        view.monitor_name,
                        monitors.iter().map(|m| m.name.clone()).collect::<Vec<_>>()
                    ),
                }
            }
        }
        Err(error) => log::warn!("枚举显示器失败：{error}"),
    }

    status
}

/// 启动轮播。`applyImmediately` 为 false 时先装载但不换纸（界面上的"继续"再触发）。
#[tauri::command]
pub fn playback_start(
    state: State<'_, AppState>,
    apply_immediately: Option<bool>,
) -> Result<crate::playback_manager::PlaybackStatus, String> {
    let settings = state.settings();
    state
        .playback
        .start(&settings, apply_immediately.unwrap_or(true))?;
    Ok(state.playback.status())
}

/// 单独暂停/继续某一块屏。返回该屏是否真的被切换（找不到这块屏时为 false）。
#[tauri::command]
pub fn playback_screen_pause(
    state: State<'_, AppState>,
    monitor_name: String,
    paused: bool,
) -> Result<bool, String> {
    state.playback.pause_screen(&monitor_name, paused)
}

/// 单独把某一块屏切到下一张，**另一块屏不动**。
///
/// 用户 2026-10："当我点击展示的下一张的壁纸时，能够直接将对应屏幕的壁纸切换到
/// 下一张，另一个屏幕不要动。"界面上点的是那张预览格，因此这里换过去的
/// 正是预览里显示的那张（预览与抽取共用同一套算式，见 §10.36）。
#[tauri::command]
pub fn playback_next_screen(
    state: State<'_, AppState>,
    monitor_name: String,
) -> Result<bool, String> {
    state.playback.next_screen(&monitor_name)
}

// ---------------------------------------------------------------------------
// 软件回收站（用户 2026-10 的要求）
// ---------------------------------------------------------------------------
//
// 用户的要求（原话）："删除当前壁纸……不要放到系统回收站，也不要彻底删除，
// 而是放到软件的数据文件夹里面，可以新建一个软件的回收站。然后再设置页加一个
// 功能，使我可以打开回收站。清空回收站的话，把所有的东西都放到系统回收站。"
//
// 因此这里是两级：删除 → `data/trash/`（软件回收站）；清空 → 系统回收站。
// 任何一条路径都不会让图**不可恢复地**消失——这正是他要的语义。
// 具体实现（移动、重名、送系统回收站）在 `wwp_core::trash` 里。

/// 回收站的规模，设置页用来显示"里面有 N 个文件 / X MB"。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashInfo {
    /// 回收站目录。界面显示它，并给一个「打开回收站」按钮。
    pub path: String,
    pub count: u64,
    pub bytes: u64,
}

fn trash_of(state: &AppState) -> wwp_core::trash::Trash {
    wwp_core::trash::Trash::new(state.trash_dir())
}

#[tauri::command]
pub fn trash_info(state: State<'_, AppState>) -> TrashInfo {
    let trash = trash_of(&state);
    let stats = trash.stats();
    TrashInfo {
        path: trash.root().display().to_string(),
        count: stats.count,
        bytes: stats.bytes,
    }
}

/// 在资源管理器里打开软件回收站。
///
/// 先 `ensure()` 再打开：目录还没被创建过（一次都没删过东西）时，
/// 资源管理器会弹一个"找不到"的框，看起来像程序出错了。
#[tauri::command]
pub fn trash_open(state: State<'_, AppState>) -> Result<(), String> {
    let trash = trash_of(&state);
    trash.ensure()?;
    open_folder(trash.root().display().to_string())
}

/// 用系统默认关联的程序打开回收站里的某一张图（相册页上点缩略图）。
///
/// 传的是**条目 id**而不是路径：路径由后端从 id 查出来（与 `trash_restore` 同一套
/// 做法），因此前端拿不到"用这个命令打开任意文件"的能力。
///
/// ## 为什么是同步命令（不像 `trash_thumbnail` 那样 `async`）
///
/// 这里只做一次 `ShellExecuteW`：把"打开它"交给 shell 就返回，不等那个程序
/// 启动完，因此是毫秒级的，不会占住主线程。而且 shell 这个入口要在 **STA**
/// 线程上调用——主线程（WebView2 初始化的那个 STA）正是最合适的调用点，
/// 挪到工作线程反而要自己再初始化一次 COM 单元（见 `trash::StaApartment`）。
///
/// 打开**文件**与 `trash_open` 打开**目录**不是一件事，两者的取舍见
/// `wwp_core::shell::open_file` 的注释。
#[tauri::command]
pub fn trash_open_item(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let item = trash_of(&state)
        .find(&id)
        .ok_or_else(|| format!("回收站里没有这一项：{id}"))?;
    wwp_core::shell::open_file(&item.file)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntryView {
    /// 条目标识。界面把它传回 `trash_restore` / `trash_delete` / `trash_thumbnail`。
    pub id: String,
    /// 原始文件名。
    pub name: String,
    /// 删除前的完整路径。**旧版（1.3.0）删掉的条目没有记录，为 null。**
    pub original: Option<String>,
    /// 原位置现在是不是已经有一个同名文件（恢复时会另存一个名字）。
    pub original_exists: bool,
    /// 能不能恢复（旧版条目不能）。
    pub can_restore: bool,
    pub bytes: u64,
    /// 删除时间（Unix 秒）。界面自己格式化成可读时间。
    pub deleted_at: i64,
}

/// 列出回收站里的条目（相册页用），新的在前。
#[tauri::command]
pub fn trash_entries(state: State<'_, AppState>) -> Vec<TrashEntryView> {
    trash_of(&state)
        .items()
        .into_iter()
        .map(|item| TrashEntryView {
            can_restore: item.can_restore(),
            id: item.id,
            name: item.name,
            original: item.original,
            original_exists: item.original_exists,
            bytes: item.bytes,
            deleted_at: item.deleted_at,
        })
        .collect()
}

/// 恢复一个条目：把它**放回原来的位置**。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashRestoreReport {
    /// 给用户看的一句话结论。
    pub message: String,
    /// 最终落到哪里（原位置被占用时是另存的那个名字）。
    pub target: String,
    /// 原本要放回去的位置。
    pub original: String,
    /// 是不是因为原位置已被占用而改了名字。
    pub renamed: bool,
    /// 这张图属于哪个图库（界面据此顺手做一次增量重扫，让恢复的图回到候选里）。
    /// 不属于任何已配置的图库时为 null。
    pub library: Option<String>,
}

/// 从回收站恢复一张图。
///
/// ## 恢复只做"把文件放回去"这一件事，索引交给扫描
///
/// 删除时我们把它从 `index.jsonl` 里删掉了（否则「重新加载」会去打开一个
/// 不存在的文件）。恢复之后那张图**不在索引里**，也就不会参与轮播——
/// 用户会以为"恢复了但没生效"。
///
/// 这里**不自己往索引里塞记录**：索引条目的字段（宽高、EXIF 方向、格式、mtime）
/// 都要真的读一遍文件才能得到，那正是扫描做的事；自己拼一份就等于把
/// "怎么判定一张图"这套规则复制了第二份（§7.3 的老教训）。
/// 因此这里只把**它属于哪个图库**告诉界面，由界面调一次
/// `library_scan`（增量：只打开新增的那一个文件）。
#[tauri::command]
pub fn trash_restore(
    state: State<'_, AppState>,
    id: String,
) -> Result<TrashRestoreReport, String> {
    let outcome = trash_of(&state).restore(&id)?;
    let target = outcome.target.display().to_string();
    let library = library_of_path(&state, &target);
    let message = if outcome.renamed {
        format!("已恢复：{target}（原位置已有同名文件，因此另存了一个名字）")
    } else {
        format!("已恢复到原位置：{target}")
    };
    Ok(TrashRestoreReport {
        message,
        target,
        original: outcome.original,
        renamed: outcome.renamed,
        library,
    })
}

/// 把某一项直接送进**系统回收站**（相册页上的「删除」按钮）。
#[tauri::command]
pub fn trash_delete(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let trash = trash_of(&state);
    let name = trash
        .find(&id)
        .map(|item| item.name)
        .unwrap_or_else(|| id.clone());
    trash.discard(&id)?;
    Ok(format!(
        "已把「{name}」送进系统回收站（在桌面的「回收站」里还能还原）"
    ))
}

/// 某一条的缩略图（data URL，直接给 `<img src>` 用）。
///
/// ## 为什么是 `async` 命令
///
/// 同步的 `#[tauri::command]` 跑在**主线程**上，而生成缩略图要解码一张
/// 几十兆像素的照片（几百毫秒）。相册页一打开要十几张，同步做就是"界面卡死几秒"。
/// `async` 命令跑在异步运行时上，这里再用 `spawn_blocking` 把解码挪到阻塞线程池：
/// 既不占主线程，也不占异步运行时的工作线程。
///
/// 取状态放在一个**块**里：`app.state()` 借的是 `app`，块一结束借用就还回去了，
/// 于是这个 future 里没有跨 `await` 的借用，Send 约束自然满足。
#[tauri::command]
pub async fn trash_thumbnail(app: AppHandle, id: String) -> Result<String, String> {
    let (root, cache) = {
        let state = app.state::<AppState>();
        (
            state.trash_dir(),
            state.paths.data_dir().join("trash-thumbs"),
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        wwp_core::trash::Trash::new(root).thumbnail_data_url(&cache, &id)
    })
    .await
    .map_err(|error| format!("缩略图任务没能完成：{error}"))?
}

/// 某个路径属于哪个已配置的图库（前缀匹配，大小写与分隔符都归一化过）。
///
/// 与 `remove_from_index` 用的是同一套判据，因此"删的时候认为它属于哪个库"
/// 与"恢复的时候认为它属于哪个库"必然一致。
fn library_of_path(state: &AppState, path: &str) -> Option<String> {
    let target = wwp_core::library::exclude::normalize(path);
    state
        .settings()
        .libraries
        .iter()
        .find(|root| {
            let prefix = wwp_core::library::exclude::normalize(&root.to_string_lossy());
            !prefix.is_empty() && (target == prefix || target.starts_with(&format!("{prefix}/")))
        })
        .map(|root| root.display().to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEmptyReport {
    /// 成功交给系统回收站的条目数。
    pub moved: u64,
    /// 没能交出去的条目数。
    pub failed: u64,
    /// 给用户看的一句话结论。
    pub message: String,
}

/// 「全部删除」的进度（前端据此画进度条）。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEmptyProgress {
    /// 已经处理完的条目数。**成功与失败都算**——进度条回答的是"还剩多少没弄完"，
    /// 成功了多少由 `TrashEmptyReport` 回答。
    pub done: u64,
    /// 这一次要处理的总条目数。
    pub total: u64,
}

/// 全部删除：回收站里每一样东西都送进**系统回收站**（仍然可以还原）。
///
/// ## 为什么必须是 `async` 命令（这就是"点了没反应"的根因）
///
/// 同步的 `#[tauri::command]` 跑在**主线程**上。删除几千张图时，每一项都要走一次
/// `SHFileOperationW`，几秒到几十秒都有可能；这几秒里主线程既画不了界面、
/// 也不处理消息，用户看到的就是"程序卡死了"（他会去点第二次、第三次）。
/// 改成 `async` + `spawn_blocking` 之后：删除在阻塞线程池上跑，主线程继续
/// 负责界面，每处理完一项就推一次进度。
///
/// ## 进度为什么在后端限流
///
/// 单项只要几毫秒，几千项就是几千次事件；而每一次事件都要序列化、过 IPC、
/// 再让前端 setState，密到那个程度纯属浪费（界面一帧也画不了两次）。
/// 因此这里按 50 ms 限流，但**最后一项一定发**——否则进度条会停在 99% 不动。
#[tauri::command]
pub async fn trash_empty(app: AppHandle) -> Result<TrashEmptyReport, String> {
    let trash = {
        let state = app.state::<AppState>();
        trash_of(&state)
    };

    let progress_app = app.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        // 第一次一定发（界面马上能看到 "1 / N"），之后按 50 ms 限流。
        let mut last_emit: Option<std::time::Instant> = None;
        let mut on_progress = |done: u64, total: u64| {
            let now = std::time::Instant::now();
            let due = match last_emit {
                None => true,
                Some(last) => now.duration_since(last) >= std::time::Duration::from_millis(50),
            };
            if !due && done < total {
                return;
            }
            last_emit = Some(now);
            // 推不动进度不该让删除失败：这只影响界面好不好看。
            if let Err(error) =
                progress_app.emit(EVENT_TRASH_EMPTY_PROGRESS, TrashEmptyProgress { done, total })
            {
                log::debug!("推送回收站删除进度失败：{error}");
            }
        };
        trash.discard_all_with(&mut on_progress)
    })
    .await
    .map_err(|error| format!("删除任务没能完成：{error}"))??;

    let message = if report.moved == 0 && report.failed == 0 {
        "回收站本来就是空的".to_string()
    } else if report.failed == 0 {
        format!(
            "已把 {} 项送进系统回收站（在桌面的「回收站」里可以还原；确认不要了再在那里清空）",
            report.moved
        )
    } else {
        format!(
            "已送进系统回收站 {} 项，{} 项失败（详见日志）",
            report.moved, report.failed
        )
    };
    Ok(TrashEmptyReport {
        moved: report.moved,
        failed: report.failed,
        message,
    })
}

/// 「删除当前壁纸」的结果。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    /// 给用户看的一句话结论。
    pub message: String,
    /// 被移走的那张图的**原路径**（界面显示在用）。
    pub removed: String,
    /// 它在软件回收站里的新路径。
    pub stored: String,
    /// 是否同步更新了图库索引（false = 只移了文件，索引留给下次扫描）。
    pub index_updated: bool,
    /// 操作之后的最新轮播状态（界面不必等下一次轮询）。
    pub status: crate::playback_manager::PlaybackStatus,
}

/// 删除当前壁纸：把这块屏正在显示的那张图移进软件回收站，并换下一张。
///
/// ## 为什么要改索引
///
/// 只把文件移走是不够的：候选列表是从 `index.jsonl` 建出来的，用户下一次
/// 「重新加载」就会把那张已经不存在的图重新算进候选——抽到它时解码失败，
/// 屏幕上停在旧图上，看起来就是"删了但没用"。因此顺手把它从索引里删掉。
///
/// ## 为什么不在这里等换纸的结果
///
/// 换纸要解码（几百毫秒），而这个命令是同步命令，等在原地会挡住界面线程。
/// 因此分两截：慢不了的两步（移动文件、改索引）在这里同步做完并如实报错，
/// 换纸交给调度线程（`ForgetAndNext`），界面上的「当前显示」下一秒就更新。
#[tauri::command]
pub fn playback_delete_current(
    state: State<'_, AppState>,
    monitor_name: String,
) -> Result<DeleteReport, String> {
    let name = monitor_name.trim().to_string();
    if name.is_empty() {
        return Err("没指定要删除哪块屏的壁纸".to_string());
    }

    let Some(current) = state.playback.shown_for(&name) else {
        return Err("这块屏还没有显示过任何图片，没有可删除的壁纸".to_string());
    };
    log::info!("收到「删除当前壁纸」请求：{name} -> {current}");

    // 1) 移进软件回收站。失败必须原样报出去——绝不假装成功。
    //    条目里会记下**原来的完整路径**，这样以后能从相册页恢复回原处。
    let trash = trash_of(&state);
    let stored = trash.store(std::path::Path::new(&current))?;

    // 2) 从图库索引里删掉它（尽力而为：扫描正在写同一份索引时就不跟它抢）。
    let index_note = match remove_from_index(&state, &current) {
        Ok(()) => String::new(),
        Err(note) => {
            log::warn!("删除壁纸：索引未同步（{note}）");
            format!("；索引未同步（{note}）")
        }
    };

    // 3) 让调度线程把这张移出内存候选并换下一张。
    let switched = match state.playback.forget_and_next(&name, &current) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("删除壁纸：轮播没在运行，无法切换（{error}）");
            false
        }
    };

    let file_name = std::path::Path::new(&current)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| current.clone());
    let message = if switched {
        format!("已把「{file_name}」移进软件回收站，正在换下一张{index_note}")
    } else {
        format!("已把「{file_name}」移进软件回收站；轮播没在运行，因此没有换下一张{index_note}")
    };

    Ok(DeleteReport {
        message,
        removed: current,
        stored: stored.file.display().to_string(),
        index_updated: index_note.is_empty(),
        status: state.playback.status(),
    })
}

/// 把一张图从它所属图库的索引里移除。
///
/// 返回 `Err(说明)` 表示**索引没改**（调用方只记日志、不影响删除本身）：
/// 索引是"可重建的缓存"，为它把一次成功的删除判成失败是不划算的。
fn remove_from_index(state: &AppState, image_path: &str) -> Result<(), String> {
    if state.scanning.load(Ordering::SeqCst) {
        // 扫描正在重写同一份索引文件。**不跟它抢**：下一次扫描会把
        // 磁盘上已经不存在的条目自然收走。
        return Err("扫描进行中".to_string());
    }

    let target = wwp_core::library::exclude::normalize(image_path);
    let settings = state.settings();
    let root = settings
        .libraries
        .iter()
        .find(|root| {
            let prefix = wwp_core::library::exclude::normalize(&root.to_string_lossy());
            !prefix.is_empty() && (target == prefix || target.starts_with(&format!("{prefix}/")))
        })
        .cloned()
        .ok_or_else(|| "这张图不属于任何已配置的图库".to_string())?;

    let paths = IndexPaths::new(&state.index_dir(), &library::library_id(&root));
    let (mut meta, entries) = library::index::load(&paths).map_err(|e| e.to_string())?;
    let before = entries.len();
    let mut removed_playable = false;
    let kept: Vec<LibraryEntry> = entries
        .into_iter()
        .filter(|entry| {
            if wwp_core::library::exclude::normalize(&entry.p) == target {
                removed_playable = entry.is_playable();
                false
            } else {
                true
            }
        })
        .collect();
    if kept.len() == before {
        return Err("索引里没有它".to_string());
    }
    // 统计里那个"可播放"的数字要跟着减，否则图库页会显示一个偏大的值。
    if removed_playable {
        meta.stats.playable = meta.stats.playable.saturating_sub(1);
    }
    library::index::save(&paths, &meta, &kept).map_err(|e| e.to_string())?;
    log::info!(
        "已从图库索引里移除被删掉的壁纸：{}（索引剩 {} 条）",
        image_path,
        kept.len()
    );
    Ok(())
}

/// 列出程序目录里可以清理的东西（设置页那张「清理程序目录」卡片用）。
///
/// **回收站与设置、图库索引都不在清单里**（用户明确要求"不要清空回收站"），
/// 具体范围与理由见 `wwp_core::cleanup` 的模块文档。
#[tauri::command]
pub fn cleanup_items(state: State<'_, AppState>) -> Vec<CleanupItem> {
    wwp_core::cleanup::list(&state.paths)
}

/// 执行清理：只处理 `ids` 里点到的那些项。
///
/// 逐项独立：一项失败不影响其余（某个日志文件正被别的进程读着是常见情况）。
/// 返回里带着**清理之后重新盘出来的清单**，界面直接拿它刷新显示。
#[tauri::command]
pub fn cleanup_run(state: State<'_, AppState>, ids: Vec<String>) -> Result<CleanupReport, String> {
    log::info!("清理程序目录：{} 项被选中（{:?}）", ids.len(), ids);
    let report = wwp_core::cleanup::run(&state.paths, &ids).map_err(|error| error.to_string())?;
    let failed = report.outcomes.iter().filter(|outcome| !outcome.ok).count();
    log::info!(
        "清理完成：释放 {}，失败 {} 项",
        wwp_core::cleanup::human_bytes(report.freed),
        failed
    );
    Ok(report)
}

/// NSFW 模式按钮：进入/退出。全局快捷键走的是同一段逻辑（见 `crate::nsfw::toggle`），
/// 因此两条路的行为不可能不一致。
#[tauri::command]
pub fn playback_nsfw(app: tauri::AppHandle) -> Result<NsfwReport, String> {
    let message = crate::nsfw::toggle(&app)?;
    let state = app.state::<AppState>();
    Ok(NsfwReport {
        message,
        status: state.playback.status(),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NsfwReport {
    /// 给用户看的结果文字（成功或部分失败的原因）。
    pub message: String,
    pub status: crate::playback_manager::PlaybackStatus,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlReport {
    pub action: String,
    pub status: crate::playback_manager::PlaybackStatus,
}


/// 目录树里的一个节点。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    /// 完整路径。界面把它写进排除规则的 `dirs` 里。
    pub path: String,
    /// 只显示名字（最后一段）。
    pub name: String,
    /// 相对图库根的第几层，0 是根的直接子目录。界面靠它缩进。
    pub depth: usize,
    /// 这个目录当前是否参与轮播。
    ///
    /// 它已经把"父目录被排除"这件事算进去了——也就是说它表示**实际生效**的
    /// 状态，而不是"这个路径本身是否在排除列表里"。
    /// 界面上的复选框直接反映这个值，用户看到的才是真的。
    pub included: bool,
    /// 它自己（**精确匹配**，不含父级）就出现在排除列表里。
    ///
    /// 界面用这个区分两种情况：用户显式取消了它，还是它因为父目录被取消
    /// 而顺带被取消。后者要让用户知道"想恢复它得先恢复上级"。
    ///
    /// 注意这里**不能**用 `matcher.should_skip_dir`：那个函数会向上查找父规则，
    /// 因此父目录被排除时子目录也会返回命中，两者就分不开了。
    /// 实测踩过这个坑：嵌套测试里只排除「甲」，子目录「甲一」也被标成
    /// "自己就被排除"，于是界面无法提示"随上级"。
    pub excluded_here: bool,
    /// 该目录下的图片数量（含子目录）。
    pub image_count: usize,
}

/// 列出某个图库的**目录结构**（不含具体文件）。
///
/// ## 为什么只列目录
///
/// 一个图库可能有上万个文件，把文件列出来既没法看也没必要——用户想回答的
/// 问题是"哪些目录要参与轮播"，那是个目录级的问题。
///
/// ## 勾选的语义
///
/// 界面上的复选框反映 `included`（实际生效的状态）。用户改变勾选时，
/// 界面通过 `settings_update` 把路径加进/移出排除规则的 `dirs` 列表。
///
/// 因为排除规则是**前缀匹配**（`should_skip_dir` 会向上查找父规则），
/// 所以：
/// - 取消勾选一个父目录 → 它全部子目录自动都不参与，**不需要逐个写入**；
/// - 子目录无法单独"重新勾选"（父级规则仍在）——这是刻意的：
///   若允许这样，用户会得到一个"父目录不参与、但里面某个子目录参与"的
///   状态，而他在目录树上看不出这个矛盾。
///
/// `max_depth` 限制递归深度，避免用户选到 `C:\` 时枚举整块盘。
#[tauri::command]
pub fn library_folder_tree(
    state: State<'_, AppState>,
    path: String,
    max_depth: Option<usize>,
) -> Result<Vec<FolderNode>, String> {
    let root = std::path::PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("目录不存在：{path}"));
    }
    // 默认 6 层。图库一般不会更深，而更深的目录用户也不容易看清。
    let max_depth = max_depth.unwrap_or(6).min(16);

    let settings = state.settings();
    // 用完整规则（全局 + 各屏）判断——与扫描时用的是同一套，
    // 因此界面上看到的"参与/不参与"与实际扫描结果一致。
    let matcher = wwp_core::library::exclude::ExcludeMatcher::new(&settings.exclude);

    let mut nodes: Vec<FolderNode> = Vec::new();
    collect_folders(&root, &root, 0, max_depth, &matcher, &settings.exclude, &mut nodes)?;
    Ok(nodes)
}

/// 递归收集目录。只进目录，不算文件数之外的东西。
fn collect_folders(
    root: &std::path::Path,
    dir: &std::path::Path,
    depth: usize,
    max_depth: usize,
    matcher: &wwp_core::library::exclude::ExcludeMatcher,
    exclude: &wwp_core::config::ExcludeSettings,
    out: &mut Vec<FolderNode>,
) -> Result<(), String> {
    if depth > max_depth {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // 单个目录读不了（权限等）不该让整棵树失败——跳过它继续。
        Err(error) => {
            log::debug!("读目录失败，跳过：{}（{error}）", dir.display());
            return Ok(());
        }
    };

    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // 只处理目录。符号链接不跟进：图库里放一个指向上级的链接会导致
        // 无限递归（虽然 max_depth 会兜住，但那样得到的是无意义的树）。
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && !kind.is_symlink() => subdirs.push(path),
            _ => {}
        }
    }
    // 按名字排序，让界面上的顺序稳定（文件系统返回的顺序不保证）。
    subdirs.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .cmp(&b.file_name().unwrap_or_default().to_string_lossy().to_lowercase())
    });

    for sub in subdirs {
        let name = sub
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // `excluded_here` 用**精确**比较；`included` 用会向上查找的匹配器。
        // 两个判断的差别正是"用户显式取消"与"随上级被取消"的差别。
        let excluded_here = matched_exactly(exclude, &sub);
        let rule_hits = matcher.should_skip_dir(&sub).is_some();
        // 实际是否参与：自己没被任何规则命中，且没有祖先被排除。
        let included = !rule_hits && !ancestors_excluded(&sub, root, matcher);
        out.push(FolderNode {
            path: sub.to_string_lossy().to_string(),
            name,
            depth,
            included,
            excluded_here,
            image_count: count_images(&sub),
        });
        collect_folders(root, &sub, depth + 1, max_depth, matcher, exclude, out)?;
    }
    Ok(())
}

/// 这个目录是否**精确地**出现在排除列表的目录规则里。
///
/// 为什么不用 `should_skip_dir`：那个函数是给"判断一个文件要不要跳过"用的，
/// 它会向上查找父规则——因此父目录被排除时，子目录也会返回命中。
/// 界面需要区分"用户显式取消了这个目录"与"它随上级被取消"，
/// 所以这里必须做精确比较。
///
/// 比较前统一小写并把分隔符转成 `/`：Windows 路径大小写不敏感，
/// 而用户可能粘贴的是带反斜杠的路径。
fn matched_exactly(settings: &wwp_core::config::ExcludeSettings, dir: &std::path::Path) -> bool {
    // 只有 `dirs` 规则参与"按目录勾选"。glob/regex 是另一类规则，
    // 它们可能命中任何路径，界面上不该据此去勾选复选框。
    let normalize = |text: &str| -> String {
        text.replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    let target = normalize(&dir.to_string_lossy());
    settings
        .dirs
        .iter()
        .any(|rule| normalize(rule) == target)
}

/// 从 `dir` 往上找到 `root`（不含 root 本身），看是否有祖先被排除。
///
/// 需要它是因为 `should_skip_dir(dir)` 只回答"这条路径本身命中规则吗"，
/// 而界面上要显示的是"实际生效"的状态。
fn ancestors_excluded(
    dir: &std::path::Path,
    root: &std::path::Path,
    matcher: &wwp_core::library::exclude::ExcludeMatcher,
) -> bool {
    let mut current = dir.parent();
    while let Some(parent) = current {
        // 到根就停：根目录不参与勾选（整库都由列表里的启用状态控制）。
        if parent == root {
            break;
        }
        if matcher.should_skip_dir(parent).is_some() {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// 数一个目录下（含子目录）有多少个候选扩展名的文件。
///
/// 用扩展名而不是读图片头：这里只是给用户一个"这个目录大概有多少图"的量级，
/// 而逐个读头会让打开目录树变得很慢（几万个文件要读几万次）。
fn count_images(dir: &std::path::Path) -> usize {
    let mut count = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && !kind.is_symlink() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => {
                    let ext = entry
                        .path()
                        .extension()
                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default();
                    if wwp_core::library::probe::is_candidate_extension(&ext) {
                        count += 1;
                    }
                }
                _ => {}
            }
        }
    }
    count
}

/// 弹出"选择文件夹"对话框，返回用户选的路径（取消则返回 `null`）。
///
/// 用系统自带的 `SHBrowseForFolderW` 而不是 Tauri 的 dialog 插件：
/// 后者要下载新依赖，而本项目的约定是"不在项目之外产生下载行为"。
/// 详见 `wwp_core::folder_picker` 的说明。
///
/// `initial` 是打开时的起始目录（一般是用户已经在列表里的某个图库，
/// 或者程序数据目录）。传它能让用户少点几层。
#[tauri::command]
pub fn pick_folder(initial: Option<String>) -> Result<Option<String>, String> {
    let initial = initial.map(std::path::PathBuf::from);
    wwp_core::folder_picker::pick_folder(initial.as_deref())
}

/// 选一张图片文件（NSFW 模式用）。用户取消时返回 null。
///
/// 与 `pick_folder` 分开是因为用的是系统里另一个对话框
/// （`SHBrowseForFolderW` 只能选目录），两者的返回值含义相同。
#[tauri::command]
pub fn pick_image(initial: Option<String>) -> Result<Option<String>, String> {
    let initial = initial.map(std::path::PathBuf::from);
    wwp_core::folder_picker::pick_image(initial.as_deref())
}

/// 操作**所有屏**的轮播：`next` / `previous` / `pause` / `resume`。///
/// 统一的入口而不是各自一个命令：界面上的按钮是同一排，
/// 加一个动作时只改这里与界面，不必再注册一条命令。
///
/// 换纸是异步的（真正的换纸由管理器线程执行），因此这里返回的 `results` 为空；
/// 界面稍后从 `status.lastResults` 读到逐屏结果与"哪块屏没做成、为什么"。
#[tauri::command]
pub fn playback_control(
    state: State<'_, AppState>,
    action: String,
) -> Result<ControlReport, String> {
    use crate::playback_manager::PlaybackCommand;

    match action.as_str() {
        "next" => state.playback.send(PlaybackCommand::Next)?,
        "previous" => state.playback.send(PlaybackCommand::Previous)?,
        "pause" => state.playback.send(PlaybackCommand::Pause)?,
        "resume" => state.playback.send(PlaybackCommand::Resume)?,
        other => return Err(format!("未知的操作：{other}")),
    }

    Ok(ControlReport {
        action,
        status: state.playback.status(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wwp_core::config::ExcludeSettings;

    fn exclude(dirs: &[&str]) -> ExcludeSettings {
        ExcludeSettings {
            dirs: dirs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn matched_exactly_only_matches_the_directory_itself() {
        // 这条守的是界面上的一个具体区分：**用户显式取消的目录**
        // 与**因为上级被取消而顺带取消的目录**要能分开显示。
        //
        // 若这里用 `should_skip_dir`（它会向上查找父规则），父目录被排除时
        // 子目录也会返回命中，两者就分不开了——实测踩过这个坑：
        // 嵌套测试里只排除「甲」，子目录「甲一」也被标成"自己就被排除"。
        let settings = exclude(&[r"D:\Pics\甲"]);
        assert!(
            matched_exactly(&settings, std::path::Path::new(r"D:\Pics\甲")),
            "目录本身应当精确命中"
        );
        assert!(
            !matched_exactly(&settings, std::path::Path::new(r"D:\Pics\甲\甲一")),
            "子目录不该被当成「自己就被排除」——它是随上级"
        );
    }

    #[test]
    fn matched_exactly_is_case_insensitive_and_separator_agnostic() {
        // Windows 路径大小写不敏感；而用户可能粘贴带反斜杠或正斜杠的路径。
        // 不统一的话，界面上会出现"我已经取消了这个目录，但它还显示参与"。
        let settings = exclude(&[r"D:\Pics\甲"]);
        for candidate in [
            r"d:\pics\甲",
            r"D:/Pics/甲",
            r"D:\Pics\甲\",
            r"d:/pics/甲/",
        ] {
            assert!(
                matched_exactly(&settings, std::path::Path::new(candidate)),
                "{candidate} 应当被认为与 D:\\Pics\\甲 是同一个目录"
            );
        }
    }

    #[test]
    fn matched_exactly_ignores_paths_that_are_not_in_the_list() {
        // 只有 `dirs` 里的路径参与"按目录勾选"。别的目录（包括名字很像的兄弟目录）
        // 都不该被当成"已取消"——那会让复选框显示成空的，而实际上它参与轮播。
        let settings = exclude(&[r"D:\Pics\甲"]);
        assert!(
            !matched_exactly(&settings, std::path::Path::new(r"D:\Pics\thumbs")),
            "不在排除列表里的目录不该被标成已取消"
        );
        assert!(
            !matched_exactly(&settings, std::path::Path::new(r"D:\Pics\tmp")),
            "不在排除列表里的目录不该被标成已取消"
        );
    }

    #[test]
    fn matched_exactly_rejects_unrelated_directories() {
        // 基准行为：排除列表里的路径不该命中别的目录。
        // 没有这条，"任何路径都返回 true" 这种实现也能通过上面那些测试。
        let settings = exclude(&[r"D:\Pics\甲"]);
        assert!(!matched_exactly(&settings, std::path::Path::new(r"D:\Pics\乙")));
        assert!(!matched_exactly(&settings, std::path::Path::new(r"D:\Pics\甲一")));
    }

    /// 造一份"两个图库 + 两个列表"的设置，用来测图库清单的整体替换。
    fn settings_with_libraries() -> wwp_core::config::Settings {
        use wwp_core::config::{Playlist, Settings};
        let mut settings = Settings::default();
        settings.libraries = vec![
            std::path::PathBuf::from(r"D:\Pics\甲"),
            std::path::PathBuf::from(r"E:\Pics\乙"),
        ];
        settings.playlists.push(Playlist {
            id: "list1".to_string(),
            name: "工作".to_string(),
            libraries: vec![
                std::path::PathBuf::from(r"D:\Pics\甲"),
                std::path::PathBuf::from(r"E:\Pics\乙"),
            ],
            ..Default::default()
        });
        settings.playlists.push(Playlist {
            id: "list2".to_string(),
            name: "晚上".to_string(),
            libraries: vec![std::path::PathBuf::from(r"E:\Pics\乙")],
            ..Default::default()
        });
        settings
    }

    #[test]
    fn applying_a_library_list_keeps_order_and_dedupes_case_insensitively() {
        // 顺序必须保住：界面按这个顺序列图库，顺序一跳用户就找不到刚加的那个。
        // 大小写不同的同一个目录要去重（Windows 路径大小写不敏感）。
        let mut settings = settings_with_libraries();
        let removed = apply_library_list(
            &mut settings,
            vec![
                r"F:\新加的".to_string(),
                r"d:\pics\甲".to_string(),
                r"  ".to_string(),
            ],
        );

        assert_eq!(
            settings.libraries,
            vec![
                std::path::PathBuf::from(r"F:\新加的"),
                std::path::PathBuf::from(r"d:\pics\甲"),
            ],
            "空串要丢掉，路径原样保留（大小写不改写），顺序按提交的顺序"
        );
        assert_eq!(
            removed,
            vec![r"E:\Pics\乙".to_string()],
            "不在新清单里的那个才算被移除；D:\\Pics\\甲 换个大小写仍然算保留"
        );
    }

    #[test]
    fn removing_a_library_unbinds_it_from_every_playlist() {
        // 这一条是**真的有用户可见后果**的：不解绑的话，下次读盘
        // `adopt_missing_libraries` 会把被移除的目录补回清单，
        // 用户看到的是"我移除了它，重启又回来了"。
        let mut settings = settings_with_libraries();
        apply_library_list(&mut settings, vec![r"F:\新加的".to_string()]);

        assert!(
            settings.playlists.iter().all(|p| p.libraries.is_empty()),
            "两个列表都引用过被移除的图库，都要被解绑：{:?}",
            settings.playlists
        );
        // 列表本身不能被删掉——用户只是换了内容来源。
        assert_eq!(settings.playlists.len(), 2);

        // 再走一遍"读盘时的补齐"：被移除的目录不该复活。
        settings.adopt_missing_libraries();
        assert_eq!(
            settings.libraries,
            vec![std::path::PathBuf::from(r"F:\新加的")],
            "解绑之后，读盘时不该把已移除的图库补回来"
        );
    }

    #[test]
    fn adding_a_library_does_not_touch_the_playlists() {
        // 反向的护栏：新增一个图库**不该**动任何播放列表的关联
        // （新加进来的图库默认不被任何列表使用，界面上会标「未被使用」）。
        let mut settings = settings_with_libraries();
        apply_library_list(
            &mut settings,
            vec![
                r"D:\Pics\甲".to_string(),
                r"E:\Pics\乙".to_string(),
                r"F:\新加的".to_string(),
            ],
        );
        assert_eq!(settings.playlists[0].libraries.len(), 2);
        assert_eq!(settings.playlists[1].libraries.len(), 1);
    }

    /// 造一条提交用的播放列表补丁。
    fn playlist_patch(id: &str, name: &str, libraries: Vec<String>) -> PlaylistPatch {
        PlaylistPatch {
            id: id.to_string(),
            name: name.to_string(),
            libraries,
            interval_seconds: 100,
            fit_mode: "fill".to_string(),
        }
    }

    #[test]
    fn a_missing_library_directory_does_not_block_the_playlist_save() {
        // 用户实际踩到的那个"播放列表页卡死"（2026-10-06）：
        // 他在资源管理器里把一个图库目录改了名，而播放列表页每次提交的都是
        // **整份列表**——原先只要有一处 `is_dir()` 不成立就整体拒绝，
        // 于是新建/改名/改间隔/改填充/勾图库/删除**全部**失败，
        // 而界面上什么提示都没有（前端那半边的修复见 PlaylistManager.submit）。
        //
        // 现在：目录不在只记进 `missing` 供日志使用，保存必须成功。
        let mut settings = settings_with_libraries();
        // **前提要用真实存在的目录**：这台机器上没有 `D:\Pics\甲`，
        // 拿它当"存在的那一个"会让断言多出一条缺失（第一版就这么错了）。
        let present = std::env::temp_dir();
        assert!(present.is_dir(), "测试前提：临时目录存在");
        let gone = present.join("wwp-测试-不存在的图库目录");
        assert!(!gone.is_dir(), "测试前提：这个目录确实不存在");

        let outcome = apply_playlist_list(
            &mut settings,
            vec![playlist_patch(
                "list1",
                "工作",
                vec![
                    present.display().to_string(),
                    gone.display().to_string(),
                    // 同一个目录写两遍：去重那一步在小写比较上先命中，
                    // 因此只该报一次缺失。
                    gone.display().to_string(),
                ],
            )],
        )
        .expect("图库目录不存在时也必须保存成功——否则整页都改不动");

        assert_eq!(outcome.count, 1);
        assert_eq!(
            outcome.missing,
            vec![gone.display().to_string()],
            "缺失目录要报出来（给日志用），但同一个目录只报一次；存在的那个不该出现"
        );
        assert_eq!(settings.playlists.len(), 1);
        assert_eq!(
            settings.playlists[0].libraries.len(),
            2,
            "路径原样存下，不因为目录暂时不在就把它丢掉"
        );
    }

    #[test]
    fn a_playlist_change_that_makes_no_sense_is_still_rejected() {
        // 放宽的只是"目录在不在"。说不清要什么的那几种仍然整体拒绝，
        // 而且被拒绝时设置必须**原封不动**（半个修改落盘比不改更糟）。
        let mut settings = settings_with_libraries();

        let err = apply_playlist_list(&mut settings, vec![playlist_patch("list1", "   ", vec![])])
            .expect_err("名字为空必须拒绝");
        assert!(err.contains("名字"), "错误要说清是哪一项：{err}");
        assert_eq!(settings.playlists.len(), 2, "被拒绝时设置必须原封不动");

        let err = apply_playlist_list(
            &mut settings,
            vec![
                playlist_patch("list1", "工作", vec![]),
                playlist_patch("list1", "晚上", vec![]),
            ],
        )
        .expect_err("同一个请求里 id 重复必须拒绝");
        assert!(err.contains("重复"), "错误要说清是哪个 id：{err}");
        assert_eq!(settings.playlists.len(), 2, "被拒绝时设置必须原封不动");

        let mut bad_fit = playlist_patch("list1", "工作", vec![]);
        bad_fit.fit_mode = "不认识的填充方式".to_string();
        let err = apply_playlist_list(&mut settings, vec![bad_fit])
            .expect_err("填充方式认不出必须拒绝");
        assert!(err.contains("填充"), "错误要说清是哪一项：{err}");
        assert_eq!(settings.playlists.len(), 2, "被拒绝时设置必须原封不动");
    }

    #[test]
    fn deleting_a_playlist_unbinds_the_screens_pointing_at_it() {
        // 这条守的是"删了列表但屏还指着它"：界面会显示成"已选 list2"，
        // 而实际上那块屏没有任何内容——很难查，因此在后端就清掉。
        let mut settings = settings_with_libraries();
        {
            let monitor = settings
                .monitors
                .entry(r"\\.\DISPLAY1".to_string())
                .or_default();
            monitor.current_playlist = "list2".to_string();
        }
        let monitor = settings
            .monitors
            .entry(r"\\.\DISPLAY2".to_string())
            .or_default();
        monitor.current_playlist = "list1".to_string();

        apply_playlist_list(
            &mut settings,
            vec![playlist_patch("list1", "工作", vec![])],
        )
        .expect("删除 list2 是合法提交");

        assert_eq!(
            settings.monitors[r"\\.\DISPLAY1"].current_playlist, "",
            "指着被删列表的那块屏要清空"
        );
        assert_eq!(
            settings.monitors[r"\\.\DISPLAY2"].current_playlist, "list1",
            "指着还在的列表的屏不能被连带清掉"
        );
    }
}

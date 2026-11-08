//! 轮播管理器：让界面能启停、暂停、热重建与操作每屏的轮播。
//!
//! ## 为什么需要一个专用线程
//!
//! 换纸会阻塞（解码一张几千万像素的照片要几百毫秒），而 Tauri 命令不能阻塞
//! （那会卡住界面）。因此这里的结构是：一个**管理器线程**独占 [`Playback`]，
//! 界面通过命令通道向它发指令，查询状态走读写锁。
//!
//! ## 这个线程同时负责"到点换纸"
//!
//! 计时不在引擎里各屏一个线程，而是由本线程按 [`Playback::time_until_next`]
//! 决定下一次醒来：到点就调用 [`Playback::tick`]（那里会**并行换纸并等齐**，
//! 于是两块屏看起来是同时切换的）。命令与计时因此共用一个循环，
//! 不存在"两块屏各自为政"的时钟。
//!
//! ## 不做任何壁纸还原
//!
//! 停止轮播只是"不再换纸"。用户的明确要求是程序**永远不要**把他原来的壁纸设回去，
//! 因此这里没有快照、没有崩溃恢复、没有"停止并还原"。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use wwp_core::engine::{bind_screens, build_plans, DesktopSink, ScreenBinding};
use wwp_core::playback::{ControlOutcome, Playback, ScreenState};
use wwp_core::Settings;

use crate::playback_state;

/// 界面能发起的操作。
///
/// **不再派生 `PartialEq` / `Eq`**：`ReplacePlans` 带着整批 [`ScreenPlan`]，
/// 而那里面是候选条目（带浮点与可选路径），逐字段比较既贵又没人要。
/// 原先那两个派生本来也没有任何地方用。
#[derive(Debug, Clone)]
pub enum PlaybackCommand {
    Next,
    Previous,
    Pause,
    Resume,
    /// 单独暂停/继续某一块屏（另一块不受影响）。
    PauseScreen { monitor_name: String, paused: bool },
    /// 单独把某一块屏切到下一张（另一块完全不动）。
    ///
    /// 用户 2026-10 的要求："当我点击展示的下一张的壁纸时，能够直接将对应屏幕的
    /// 壁纸切换到下一张，另一个屏幕不要动。"界面上点的是那张预览格，
    /// 因此这里切过去的必须**正是**预览里那张（`Selector` 已经把它算准了，
    /// 见 §10.36；这条指令走的就是同一个抽签序列）。
    NextScreen { monitor_name: String },
    /// 「删除当前壁纸」的后半截：把已经移进回收站的那张图**移出候选**，
    /// 并立刻换下一张。
    ///
    /// 前半截（把文件移进 `data/trash/`、把条目从索引里删掉）在命令层同步做完，
    /// 因为那两步很快且失败必须立刻告诉用户；这一步要动 [`Playback`] 与解码，
    /// 只能在调度线程里做。
    ForgetAndNext { monitor_name: String, path: String },
    /// 按新计划**局部**重建：计划没变的屏原地留用，只有真的变了的屏换纸。
    ///
    /// ## 为什么必须由调度线程来做（用户 2026-10 报的 bug）
    ///
    /// 用户：「当我更改某一个显示器的播放列表时，两个显示器的播放列表都会同时
    /// 刷新。」原因是外壳把 `Playback` 整个换掉后对**所有**屏换纸。
    /// 而 [`Playback`] 由本线程独占，只有它能按 [`Playback::rebuild_with`]
    /// 做"搬运没变的屏"的重建。
    ReplacePlans { plans: Vec<wwp_core::playback::ScreenPlan> },
    /// 只是刷新一次状态，不做任何换纸。
    Refresh,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenResult {
    pub monitor_name: String,
    pub outcome: String,
    /// 是否算作"做到了"。`false` 不等于"程序出错"——例如"没有可回退的上一张"
    /// 是正常状态，界面应按提示而不是报错来显示。
    pub ok: bool,
    /// 这次实际设上去的**源图**路径（成功时才有）。
    ///
    /// NSFW 模式那条路不经过 `Playback`，因此状态里拿不到它；
    /// 而界面上的「当前显示」需要它（用户要求"只要是我们设的就显示那张"）。
    pub source: Option<String>,
}

impl ScreenResult {
    fn from_outcome(monitor_name: &str, outcome: ControlOutcome) -> ScreenResult {
        let (text, ok) = match outcome {
            ControlOutcome::Applied => ("已切换".to_string(), true),
            ControlOutcome::NoCandidates => ("该屏筛选后没有可用的图".to_string(), false),
            ControlOutcome::UnknownScreen => ("找不到这块屏".to_string(), false),
            ControlOutcome::NoHistory => ("没有可回退的上一张".to_string(), false),
            ControlOutcome::FixedImage => ("这块屏是固定单图，不参与轮播".to_string(), false),
            ControlOutcome::Stopped => ("轮播已停止".to_string(), false),
            ControlOutcome::Failed(reason) => (reason, false),
        };
        ScreenResult {
            monitor_name: monitor_name.to_string(),
            outcome: text,
            ok,
            source: None,
        }
    }

    fn message(monitor_name: &str, text: impl Into<String>, ok: bool) -> ScreenResult {
        ScreenResult {
            monitor_name: monitor_name.to_string(),
            outcome: text.into(),
            ok,
            source: None,
        }
    }

    /// 成功的一条结果，并带上"实际设上去的是哪张源图"。
    fn applied(monitor_name: &str, text: impl Into<String>, source: &str) -> ScreenResult {
        ScreenResult {
            monitor_name: monitor_name.to_string(),
            outcome: text.into(),
            ok: true,
            source: Some(source.to_string()),
        }
    }
}

/// 把一批换纸结果里**没做成**的那些记进日志。
///
/// ## 为什么界面上那块"逐屏结果"删掉了，这里却还要记
///
/// 用户明确说播放页最下面那块"上次操作的逐屏结果"没有意义，已经删掉。
/// 但这些结果里有两类**不是错误、却会让人觉得"点了没反应"**的情况：
/// 「筛选后没有可用的图」与「没有可回退的上一张」。它们必须能在日志里查到，
/// 否则用户反馈"按了下一张没反应"时，我们手上什么都没有。
///
/// 成功的那条不重复记：`playback::advance` 已经逐张记过"换纸 xxx -> yyy（nn ms）"。
fn log_outcomes(outcomes: &[(String, ControlOutcome)]) {
    for (monitor, outcome) in outcomes {
        let result = ScreenResult::from_outcome(monitor, outcome.clone());
        if result.ok {
            continue;
        }
        log::warn!("{}：{}", result.monitor_name, result.outcome);
    }
}

/// 轮播的运行状态，供界面展示。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStatus {
    /// 是否正在轮播（调度线程活着）。
    pub running: bool,
    /// 全局暂停。
    pub paused: bool,
    /// 是否处于 NSFW 模式（固定壁纸、不轮播）。
    pub nsfw: bool,
    /// 各屏状态。
    pub screens: Vec<ScreenState>,
    /// 显示器绑定（界面用它画出屏幕示意与设置入口）。
    pub bindings: Vec<BindingView>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingView {
    pub monitor_name: String,
    pub device_path: String,
    /// 显示模式的宽高（来自 `EnumDisplayMonitors`，与准备图像时用的尺寸一致）。
    pub width: u32,
    pub height: u32,
    /// 这块屏在虚拟桌面里的左上角坐标。
    ///
    /// 界面靠它把两块屏的卡片按**真实摆位**从左到右排开；
    /// 绑定的顺序本身已经是排好的（见 `engine::bind_screens`）。
    pub x: i32,
    pub y: i32,
    pub primary: bool,
    /// 该屏的候选数量（轮播未启动时为 None）。
    pub candidates: Option<usize>,
    /// 该屏生效的间隔（秒）。
    pub interval_seconds: Option<u64>,
    /// 这块屏是不是"固定单图"（不轮播）。界面据此换一套显示。
    pub single_image: bool,
}

/// 起一个调度线程时，第一张壁纸怎么办。
///
/// 三种可能以前是一个 `apply_immediately: bool`，加了"恢复上次的壁纸"之后
/// 它表达不了——**恢复**与**立刻换一张**是互斥的两条路：恢复要先认上次记下的
/// 那张（且只在文件还在时认），换一张则是照常抽签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// 立刻给每屏换一张（"开始轮播"按钮、改了计划的整批重建）。
    ApplyNow,
    /// 优先恢复上次退出时记下的壁纸；没记忆/记的那张没了就照常换一张。
    ///
    /// 用在哪：程序启动、且上次没停在 NSFW 模式（见 `nsfw::apply_startup_state`）。
    Remember,
    /// 用一份记忆恢复，并把恢复的那几张**当场**设上去（不等调度线程那一轮）。
    ///
    /// 用在哪：退出 NSFW 模式（用户 2026-10 的要求："退出 NSFW 模式后，继续播放
    /// NSFW 模式之前的壁纸，而不是重新加载壁纸"）。它与 [`Startup::Remember`]
    /// 只差"等不等"这一件事：
    ///
    /// - `Remember` 走启动路径——桌面上本来就是上次那张，重新设一遍没有意义，
    ///   而那是启动路径，不该等解码；
    /// - `RememberNow` 退出 NSFW 时用——那一刻屏幕上盖着 NSFW 壁纸，**必须马上
    ///   换回来**，否则用户会先盯着那张 NSFW 壁纸看半秒。
    RememberNow,
    /// 什么都不做，等倒计时到点（重建轮播时"不该换纸"的那条路）。
    Wait,
}

/// 轮播管理器：界面通过它间接操作 [`Playback`]。
pub struct PlaybackManager {
    /// 指令通道。有它就意味着调度线程活着。
    commands: Mutex<Option<Sender<PlaybackCommand>>>,
    /// 各屏绑定，即使没在轮播也能列出来（界面要显示屏幕列表）。
    bindings: Mutex<Vec<ScreenBinding>>,
    /// 各屏状态快照，由调度线程更新。
    screens: Arc<Mutex<Vec<ScreenState>>>,
    paused: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    /// 是否处于 NSFW 模式。
    nsfw: Arc<AtomicBool>,
    /// `[general].pause_on_fullscreen`：某块屏上有程序全屏时自动暂停那块屏。
    ///
    /// 存放在这里而不是只存在配置里：调度线程每轮都要读它，而用户开关这个
    /// 选项时不该重建轮播（见 [`ManagerShared::pause_on_fullscreen`]）。
    pause_on_fullscreen: Arc<AtomicBool>,
    /// 每块屏**最后一次由我们设上去**的源图路径（显示器名 → 图片路径）。
    ///
    /// ## 为什么需要它（用户报的"明明是我们设的，界面却说尚未换过"）
    ///
    /// 界面上"当前显示"原先只认调度线程里的状态，而那份状态在几种情况下是空的：
    ///
    /// - 停在 NSFW 模式时根本没有调度线程（每屏显示的是那张 NSFW 壁纸）；
    /// - 点「重新加载」会先停掉调度线程，于是运行时状态被清空——
    ///   可桌面上留着的明明还是我们上一张设的图。
    ///
    /// 用户的要求是："只要当前壁纸是这软件设的，就该显示当前那张"。
    /// 因此这里另记一份**不随调度线程生死**的记忆：谁被设上去过、设的是哪张源图。
    /// 它是内存里的（不落盘）：重启程序后如果还没开始轮播，"尚未换过"是诚实的。
    shown: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// 计划信息（候选数、间隔），供界面展示与"要不要热重建"的比对。
    plans: Mutex<Vec<wwp_core::playback::ScreenPlan>>,
    /// 换纸产物写到哪里（NSFW 壁纸的预解码也要用）。
    output_dir: Mutex<std::path::PathBuf>,
    /// "上面那份显示器列表是什么时候枚举的"，见 [`PlaybackManager::bindings_for_apply`]。
    bindings_at: Mutex<Option<std::time::Instant>>,
    /// 数据目录。目前只用于排查日志；NSFW 模式的状态文件由外壳读写。
    data_dir: Mutex<std::path::PathBuf>,
    /// 进入 NSFW 模式**之前**每块屏在显示什么。
    ///
    /// ## 为什么需要它（用户 2026-10 的要求）
    ///
    /// 用户原话："退出 NSFW 模式后，可以继续播放 NSFW 模式之前的壁纸，
    /// 而不是重新加载壁纸。"原先退出时走的是 [`Startup::ApplyNow`]——每块屏各抽
    /// 一张新的，用户看到的就是"退出 NSFW 之后壁纸被换掉了"。
    ///
    /// 因此进入的那一刻先把"现在屏上是哪张（以及预告的下一张）"记在这里，
    /// 退出时把这份记忆交给调度线程恢复。
    ///
    /// **只在内存里**：它说的是"这几分钟里屏幕上是什么"，跨重启没有意义。
    /// 跨重启那条路由 `data/playback-state.json` 兜住（见
    /// [`PlaybackManager::leave_nsfw`]）。
    pre_nsfw: Mutex<Vec<wwp_core::playback::ScreenMemory>>,
    /// 下一次起调度线程时用哪份记忆（`None` = 读 `data/playback-state.json`）。
    ///
    /// 一次性：[`PlaybackManager::load_memory`] 会把它取走。这样
    /// `spawn_scheduler` 不必多一个参数，也不存在"上一次的临时记忆被下一次启动
    /// 误用"的窗口。
    memory_override: Mutex<Option<Vec<wwp_core::playback::ScreenMemory>>>,
    /// 现在有几个"换纸线程"正在跑（见 [`SwitchGuard`]）。
    ///
    /// ## 为什么需要它（用户报的"切 NSFW 有几百毫秒延时"）
    ///
    /// 到点换纸时调度线程**不等**换纸做完（一块屏解码慢不该拖住另一块）。
    /// 于是"停轮播"这一步就有了两种语义完全不同的做法：
    ///
    /// - 等换纸也做完（原来的做法）：那一刻正好有一块屏在解码，
    ///   用户点 NSFW 就得先陪它把这一张图解码、重采样、落盘（实测 200~250 ms，
    ///   而"等"还是按 100 ms 一档轮询的，于是实际等待还要再往上凑）；
    /// - 只管停轮播：调度线程立刻退出，NSFW 壁纸紧接着设上去——**先后无关紧要**，
    ///   因为两件事都是"把某张图设成壁纸"，后设的那张赢。
    ///
    /// 用户的要求是"点一下立刻切过去"，因此走第二种。代价只有一个：
    /// 清理轮播产物（`forget_slots`）必须等换纸线程把文件写完再删，
    /// 否则删掉的是它刚写了一半的目标文件。这个计数就是为那一次等待准备的。
    switches: Arc<AtomicUsize>,
}

impl PlaybackManager {
    pub fn new() -> PlaybackManager {
        PlaybackManager {
            commands: Mutex::new(None),
            bindings: Mutex::new(Vec::new()),
            screens: Arc::new(Mutex::new(Vec::new())),
            paused: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            nsfw: Arc::new(AtomicBool::new(false)),
            pause_on_fullscreen: Arc::new(AtomicBool::new(false)),
            shown: Arc::new(Mutex::new(std::collections::HashMap::new())),
            plans: Mutex::new(Vec::new()),
            output_dir: Mutex::new(std::path::PathBuf::new()),
            bindings_at: Mutex::new(None),
            data_dir: Mutex::new(std::path::PathBuf::new()),
            pre_nsfw: Mutex::new(Vec::new()),
            memory_override: Mutex::new(None),
            switches: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 记下"这块屏现在显示的是哪张源图"。
    pub fn remember_shown(&self, monitor_name: &str, source: &str) {
        if source.trim().is_empty() {
            return;
        }
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(monitor_name.to_string(), source.to_string());
    }

    /// 把调度线程当前的各屏状态并进这份记忆。
    ///
    /// 每轮循环之后都调一次，因此"换了哪张图"不会漏记。
    fn remember_from(&self, states: &[ScreenState]) {
        let mut shown = self.shown.lock().unwrap_or_else(|e| e.into_inner());
        for state in states {
            if let Some(current) = &state.current {
                shown.insert(state.monitor_name.clone(), current.clone());
            }
        }
    }

    fn shown_snapshot(&self) -> std::collections::HashMap<String, String> {
        self.shown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 收一份"每块屏现在显示什么"的记忆。
    ///
    /// 两个来源，缺一不可：
    ///
    /// - **调度线程发布的实时状态**（`self.screens`）：它同时带着「下一张」
    ///   （只有选择器算得出来），退出 NSFW 之后"第一次切换用哪张"靠的就是它；
    /// - **[`PlaybackManager::shown`]**：调度线程不在跑时实时状态是空的
    ///   （刚点过「重新加载」、上次就停在 NSFW 模式里），而那份记忆是
    ///   "最后被我们设上去的是哪张"，正是要放回去的那一张。
    ///
    /// 名字按**大小写不敏感**比对：两边都来自同一份绑定列表，但中间隔着一次
    /// JSON 往返（与 `shown_for` 同一条理由）。
    fn memory_snapshot(&self) -> Vec<wwp_core::playback::ScreenMemory> {
        let shown = self.shown_snapshot();
        let lookup = |name: &str| -> Option<String> {
            shown
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        };

        let mut out: Vec<wwp_core::playback::ScreenMemory> = self
            .screens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|state| wwp_core::playback::ScreenMemory {
                monitor_name: state.monitor_name.clone(),
                current: state
                    .current
                    .clone()
                    .or_else(|| lookup(&state.monitor_name)),
                next: state.next.clone(),
            })
            .collect();

        for (name, path) in shown {
            if !out
                .iter()
                .any(|record| record.monitor_name.eq_ignore_ascii_case(&name))
            {
                out.push(wwp_core::playback::ScreenMemory {
                    monitor_name: name,
                    current: Some(path),
                    next: None,
                });
            }
        }

        // 一条内容都没有的屏没有恢复价值（`restore_memory` 也认不出它），
        // 留着只会让"恢复了几块屏"这个数字虚高。
        out.retain(|record| record.current.is_some() || record.next.is_some());
        out
    }

    /// 进入 NSFW 模式的那一刻调一次：把"现在是哪张"记下来（见 `pre_nsfw`）。
    fn capture_pre_nsfw(&self) {
        let snapshot = self.memory_snapshot();
        log::debug!("进入 NSFW 模式前记下 {} 块屏的壁纸", snapshot.len());
        *self.pre_nsfw.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }

    /// 取走"进入 NSFW 模式之前"那份记忆（取一次就空了）。
    fn take_pre_nsfw(&self) -> Vec<wwp_core::playback::ScreenMemory> {
        std::mem::take(&mut *self.pre_nsfw.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// 某块屏**最后被我们设上去**的那张源图（没有时返回 `None`）。
    ///
    /// 「删除当前壁纸」在命令层同步用它：先知道"当前是哪一张"，才能把它
    /// 移进回收站。按显示器名做**大小写不敏感**匹配——界面上传来的键与
    /// 调度线程写进去的键都来自同一份绑定列表，但两者之间隔着一次 JSON 往返，
    /// 不给自己留这种"大小写不同就查不到"的隐患。
    pub fn shown_for(&self, monitor_name: &str) -> Option<String> {
        self.shown_snapshot()
            .into_iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(monitor_name))
            .map(|(_, path)| path)
    }

    /// 让调度线程把某张图移出候选并立刻换下一张（「删除当前壁纸」的后半截）。
    pub fn forget_and_next(&self, monitor_name: &str, path: &str) -> Result<(), String> {
        self.send(PlaybackCommand::ForgetAndNext {
            monitor_name: monitor_name.to_string(),
            path: path.to_string(),
        })
    }

    /// 告诉管理器数据与产物目录。启动时调用一次。
    pub fn set_paths(&self, data_dir: std::path::PathBuf, output_dir: std::path::PathBuf) {
        *self.data_dir.lock().unwrap_or_else(|e| e.into_inner()) = data_dir;
        *self.output_dir.lock().unwrap_or_else(|e| e.into_inner()) = output_dir;
    }

    fn output_dir(&self) -> std::path::PathBuf {
        self.output_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 刷新显示器列表（不做轮播）。
    pub fn refresh_bindings(&self) -> Result<Vec<ScreenBinding>, String> {
        let bindings = bind_screens().map_err(|e| e.to_string())?;
        *self.bindings.lock().unwrap_or_else(|e| e.into_inner()) = bindings.clone();
        *self.bindings_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        Ok(bindings)
    }

    /// 拿一份显示器列表用于**马上要设壁纸**的路径。
    ///
    /// ## 为什么热路径上不每次都现枚举
    ///
    /// 枚举一次要问两个来源（`EnumDisplayMonitors` 与 `IDesktopWallpaper`），
    /// 实测 46~129 ms。用户点 NSFW 要求"立刻切过去"，而**这一屏的设备路径**
    /// 在刚枚举过之后不会立刻变（会变的情况是插拔显示器，那有系统广播兜着，
    /// 见 `broadcast`）。因此只要手里的那份足够新就直接用。
    ///
    /// 新鲜度取 [`BINDINGS_FRESH`]：它比"用户点一次按钮"的间隔长得多
    /// （没人会在一秒内连点 NSFW），又比"拔掉一块屏"的手速短。
    fn bindings_for_apply(&self) -> Result<Vec<ScreenBinding>, String> {
        let age = self
            .bindings_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|at| at.elapsed());
        let fresh = age.map(|age| age < BINDINGS_FRESH).unwrap_or(false);
        if fresh {
            let cached = self.bindings();
            if !cached.is_empty() {
                log::debug!(
                    "显示器列表还新鲜（{:.0} ms 前枚举的），直接用它设壁纸",
                    age.map(|a| a.as_millis() as f64).unwrap_or(0.0)
                );
                return Ok(cached);
            }
        }
        log::debug!(
            "显示器列表不可用（{}），现枚举一次",
            match age {
                Some(age) => format!("上次枚举在 {} ms 前", age.as_millis()),
                None => "还没枚举过".to_string(),
            }
        );
        self.refresh_bindings()
    }

    pub fn bindings(&self) -> Vec<ScreenBinding> {
        self.bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn status(&self) -> PlaybackStatus {
        // 若还没列过显示器，现在就列一次：界面一打开就要能列出屏幕并让用户设置，
        // 而不是"必须先启动轮播才看得到屏幕"。
        if self.bindings.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            if let Err(error) = self.refresh_bindings() {
                log::warn!("枚举显示器失败：{error}");
            }
        }

        let bindings = self.bindings();
        let plans = self.plans.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut screens = self.screens.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let shown = self.shown_snapshot();

        // 为每块屏补上"我们最后设上去的那张图"，并**为每块屏都给出状态**。
        //
        // 调度线程不在跑时 `screens` 是空的（NSFW 模式、刚点过「重新加载」），
        // 而那时屏上明明还挂着我们设的图。用户报过"明明设了却显示尚未换过"，
        // 因此这里以"绑定列表"为准逐屏补齐，而不是只交调度线程那份快照。
        for binding in &bindings {
            let remembered = shown.get(&binding.monitor_name).cloned();
            match screens
                .iter_mut()
                .find(|state| state.monitor_name.eq_ignore_ascii_case(&binding.monitor_name))
            {
                Some(state) => {
                    if state.current.is_none() {
                        state.current = remembered;
                    }
                }
                None => {
                    let plan = plans
                        .iter()
                        .find(|p| p.monitor_name.eq_ignore_ascii_case(&binding.monitor_name));
                    screens.push(ScreenState {
                        monitor_name: binding.monitor_name.clone(),
                        current: remembered,
                        // 「下一张」只有调度线程算得出来（它手里有选择器）。
                        // 调度线程不在跑（NSFW 模式、还没开始轮播）时如实报空，
                        // 界面显示"轮播未运行"而不是编一张出来。
                        next: None,
                        changes: 0,
                        last_change_millis: 0,
                        last_error: None,
                        candidate_count: plan.map(|p| p.candidates.len()).unwrap_or(0),
                        next_in_seconds: 0,
                        paused: false,
                        paused_manual: false,
                        paused_by_fullscreen: false,
                    });
                }
            }
        }

        let views = bindings
            .iter()
            .map(|binding| {
                let plan = plans
                    .iter()
                    .find(|p| p.monitor_name.eq_ignore_ascii_case(&binding.monitor_name));
                BindingView {
                    monitor_name: binding.monitor_name.clone(),
                    device_path: binding.device_path.clone(),
                    width: binding.width,
                    height: binding.height,
                    x: binding.x,
                    y: binding.y,
                    primary: binding.primary,
                    candidates: plan.map(|p| p.candidates.len()),
                    interval_seconds: plan.map(|p| p.interval.as_secs()),
                    single_image: plan.map(|p| p.is_static_image()).unwrap_or(false),
                }
            })
            .collect();

        PlaybackStatus {
            running: self.running.load(Ordering::SeqCst),
            paused: self.paused.load(Ordering::SeqCst),
            nsfw: self.nsfw.load(Ordering::SeqCst),
            screens,
            bindings: views,
        }
    }

    /// 开始轮播。已经在跑的话先停掉（改设置后重启是最常见的用法）。
    ///
    /// `apply_immediately` 为真时立刻给每屏换一张。
    pub fn start(&self, settings: &Settings, apply_immediately: bool) -> Result<(), String> {
        let startup = if apply_immediately {
            Startup::ApplyNow
        } else {
            Startup::Wait
        };
        self.start_with(settings, startup)
    }

    /// 启动时开始轮播，并**优先恢复上次退出时的壁纸**。
    ///
    /// 与 [`PlaybackManager::start`] 的唯一区别是第一张怎么来：这里先看
    /// `data/playback-state.json`，认得出的屏就把上次那张放回去、并保证第一次
    /// 自动切换换成上次预告的那张；认不出的屏（没记忆、图已被移走、文件已不在）
    /// 照常立刻换一张——用户要求"若记录的文件已不存在，则按现有候选规则重新选择"。
    pub fn start_restored(&self, settings: &Settings) -> Result<(), String> {
        self.start_with(settings, Startup::Remember)
    }

    fn start_with(&self, settings: &Settings, startup: Startup) -> Result<(), String> {
        // 停掉上一次运行——**不还原壁纸**（程序不会再碰用户原来的壁纸）。
        self.stop_core();
        self.nsfw.store(false, Ordering::SeqCst);
        // 每次启动都按当前设置同步一次这个开关，避免"改了设置但还没保存过"
        // 之类的路径留下旧值。
        self.pause_on_fullscreen
            .store(settings.general.pause_on_fullscreen, Ordering::SeqCst);

        let bindings = self.refresh_bindings()?;
        if bindings.is_empty() {
            return Err("没有检测到显示器".to_string());
        }

        let plans = build_plans(settings, &bindings);
        *self.plans.lock().unwrap_or_else(|e| e.into_inner()) = plans.clone();
        self.spawn_scheduler(plans, startup)
    }

    /// 热重建：设置改了之后**不还原壁纸**地按新设置重建轮播。
    ///
    /// ## 只在播放计划真的变了时才重建
    ///
    /// 界面上的每一次保存都会调到这里（用户要求"改了立刻生效"），但很多改动
    /// 其实与播放无关（托盘行为、快捷键、启动选项）。无条件重建意味着重新解码
    /// 并重设两张壁纸——用户会看到一次无意义的闪动。
    ///
    /// 因此先比对计划的指纹：候选集合（含内容）、间隔、顺序、填充方式，
    /// 任何一项变了才算"真的变了"。
    ///
    /// 返回是否发生了重建。
    pub fn reload(&self, settings: &Settings) -> Result<bool, String> {
        // **先同步这个开关，再走后面的提前返回。**
        //
        // 它不影响播放计划，因此不该让轮播重建；但"用户在设置页打开它"这件事
        // 恰恰经常发生在计划没变的情况下（`reload` 会在下面两处直接返回），
        // 放到后面就会变成"勾了但没生效，重启才好"。
        self.pause_on_fullscreen
            .store(settings.general.pause_on_fullscreen, Ordering::SeqCst);

        let bindings = self.refresh_bindings()?;
        let plans = build_plans(settings, &bindings);

        if !self.running.load(Ordering::SeqCst) {
            // 没在轮播：只更新计划（界面上的候选数要跟着变），不启动。
            *self.plans.lock().unwrap_or_else(|e| e.into_inner()) = plans;
            return Ok(false);
        }

        let before = plan_fingerprint(
            &self.plans.lock().unwrap_or_else(|e| e.into_inner()),
        );
        let after = plan_fingerprint(&plans);
        if before == after {
            log::info!("设置已改，但播放计划没有变化，不重建轮播");
            return Ok(false);
        }

        log::info!("播放计划有变化，正在按新计划重建轮播（不还原壁纸）");
        *self.plans.lock().unwrap_or_else(|e| e.into_inner()) = plans.clone();

        // **重建交给正在跑的那个调度线程**：只有它独占着 `Playback`，因此也只有
        // 它能"把没变的屏原样搬过去"（见 `Playback::rebuild_with`）。
        //
        // 以前这里是自己 `stop_core()` + `spawn_scheduler(plans, true)`：那是整批
        // 重建 + 对所有屏换纸，于是"改了 A 屏的播放列表"会让 B 屏也立刻换一张、
        // 倒计时还重新起算——用户 2026-10 报的正是这个。
        let sender = self
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let sent = match &sender {
            Some(sender) => sender
                .send(PlaybackCommand::ReplacePlans { plans })
                .is_ok(),
            None => false,
        };
        if sent {
            return Ok(true);
        }

        // 通道断了（调度线程已经退出）：退回"停掉重起"。这时没有别的选择，
        // 也就谈不上"连累别的屏"——本来就没有屏在轮播。
        drop(sender);
        self.stop_core();
        let plans = self
            .plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.spawn_scheduler(plans, Startup::ApplyNow)?;
        Ok(true)
    }

    /// 起一个调度线程独占这批计划。
    fn spawn_scheduler(
        &self,
        plans: Vec<wwp_core::playback::ScreenPlan>,
        startup: Startup,
    ) -> Result<(), String> {
        let output_dir = self.output_dir();
        let sink = Arc::new(DesktopSink::new(self.bindings(), output_dir));
        let sink: Arc<dyn wwp_core::playback::WallpaperSink> = sink;
        let playback = Playback::new(plans);

        // "上次退出时记下的播放位置"——三种起点都要用到它。
        let memory = self.load_memory();
        let restored = match startup {
            Startup::ApplyNow => {
                // 第一张在这里换掉：它属于"启动动作"，不该等一个间隔。
                let outcomes = playback.apply_all(&sink);
                log_outcomes(&outcomes);
                Vec::new()
            }
            Startup::Remember | Startup::RememberNow => {
                let restored = playback.restore_memory(&memory);
                if startup == Startup::RememberNow {
                    // 退出 NSFW 模式：屏幕上还盖着 NSFW 壁纸，**当场**换回来
                    // （见 [`Playback::apply_remembered`]，它与下面那条"等
                    // `claim_due`"的支路是同一件事的两种时机）。
                    log_outcomes(&playback.apply_remembered(&sink));
                }
                // 记过的屏恢复；**没记过的屏照旧立刻换一张**——这正是"记录的文件
                // 已不存在时按现有候选规则重新选择"（`restore_memory` 认不出那张
                // 图时也会返回 false，于是这里同样落到"换一张"上）。
                //
                // 认出来的屏不动：它等 `claim_due` 把记下的那张放回去（`RememberNow`
                // 那条路刚刚已经设过一次了）。换纸是慢操作，不在启动路径上等它——
                // 与"到点自动换纸谁也不等谁"同一个理由。
                let fresh: Vec<String> = playback
                    .states()
                    .into_iter()
                    .map(|state| state.monitor_name)
                    .filter(|name| {
                        !restored
                            .iter()
                            .any(|done| done.eq_ignore_ascii_case(name))
                    })
                    .collect();
                if !fresh.is_empty() {
                    log_outcomes(&playback.apply_screens(&fresh, &sink));
                }
                restored
            }
            Startup::Wait => Vec::new(),
        };

        // 界面上的「当前显示」要跟着变，见 `PlaybackManager::shown`。
        //
        // 恢复之后各屏的「下一张」也已经定了（`pin` 之后 `peek_next` 报的就是
        // 记下的那张），不需要在这里另记一份——调度线程的 `persist_memory`
        // 会从实时状态里收出同一份内容。
        self.remember_from(&playback.states());

        let (sender, receiver) = mpsc::channel::<PlaybackCommand>();
        let screens = Arc::clone(&self.screens);
        let paused = Arc::clone(&self.paused);
        let running = Arc::clone(&self.running);
        let shown = Arc::clone(&self.shown);

        running.store(true, Ordering::SeqCst);
        // 新的调度线程一律从**未暂停**开始（`Playback::new` 就是这么建的），
        // 因此这个对外可见的标志必须跟着复位。
        //
        // 不复位会怎样：暂停 → 点「重新加载」之后，屏上明明在轮播，而
        // `status().paused` 仍是 true——界面上的按钮会显示成「继续」，
        // 托盘的「轮播中」也不会打勾（见 `tray::spawn_check_refresher`）。
        // 那个状态位只由调度线程在收 Pause/Resume 时改，reset 只能在这里做。
        paused.store(false, Ordering::SeqCst);
        let thread_sink = Arc::clone(&sink);
        let manager = ManagerShared {
            screens,
            paused,
            running,
            shown,
            pause_on_fullscreen: Arc::clone(&self.pause_on_fullscreen),
            state_dir: self.data_dir(),
            // 初值 `None`：第一轮一定写一次（见 `persist_memory`）。
            remembered: None,
            switches: Arc::clone(&self.switches),
        };
        std::thread::Builder::new()
            .name("wwp-playback-manager".to_string())
            .spawn(move || run_manager(playback, thread_sink, receiver, manager))
            .map_err(|e| format!("创建轮播线程失败：{e}"))?;

        *self.commands.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
        log::info!(
            "轮播已启动（{} 块屏，起点：{startup:?}，恢复 {} 块屏）",
            self.bindings().len(),
            restored.len()
        );
        Ok(())
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 读这次要恢复的播放位置。
    ///
    /// 两个来源，优先级明确：
    ///
    /// 1. **内存里那份一次性覆盖**（`memory_override`）——退出 NSFW 模式时把
    ///    "进入之前那两张"交给它，取走即失效；
    /// 2. `data/playback-state.json`——启动时的常规来源（目录取自管理器自己那份
    ///    `data_dir`，与 `nsfw` 模块读 `nsfw-state.json` 的是同一个目录）。
    fn load_memory(&self) -> Vec<wwp_core::playback::ScreenMemory> {
        if let Some(memory) = self
            .memory_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            log::debug!("本次恢复用内存里那份记忆（{} 块屏）", memory.len());
            return memory;
        }
        let dir = self.data_dir();
        if dir.as_os_str().is_empty() {
            return Vec::new();
        }
        playback_state::load(&dir)
    }

    /// 停掉轮播（**不还原壁纸**），并等调度线程收尾。
    pub fn stop(&self) {
        self.stop_core();
    }

    /// 请求停掉轮播，但**不等**任何东西。
    ///
    /// 返回后调度线程立刻开始收尾（它每轮最多睡 500 ms，而通道断开会让
    /// `recv_timeout` 立即返回，因此实际上是一瞬间）。**正在换纸的那一块屏
    /// 不等**——理由见 [`PlaybackManager::switches`]：用户点 NSFW 时要求"立刻切"，
    /// 而那张图还在解码的屏，它马上就会被 NSFW 壁纸覆盖掉。
    fn request_stop_core(&self) {
        let sender = self.commands.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(sender) = sender {
            drop(sender); // 断开通道 = 让调度线程收尾退出
        }
    }

    /// 等"正在换纸"的线程全部结束，最多等 `timeout`。
    ///
    /// 只有"要删掉轮播产物"的场合需要它（见 [`PlaybackManager::switches`]）。
    /// 返回是否在超时前等到。
    fn wait_for_switches(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while self.switches.load(Ordering::SeqCst) > 0 {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }

    /// 停掉轮播，并等调度线程与在跑的换纸线程都收尾。
    fn stop_core(&self) {
        let had_sender = self
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        self.request_stop_core();
        if !had_sender {
            return;
        }

        // 轮询步进 10 ms 而不是 100 ms：调度线程真的退出只要毫秒级
        // （通道断开会让 `recv_timeout` 立即返回），而 100 ms 的步进意味着
        // "最坏情况白等 100 ms"——那是 NSFW 延时里我们自己贡献的那一段。
        for _ in 0..500 {
            if !self.running.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // 换纸线程也要收尾：紧接着的 `forget_slots` 会删掉它们的产物文件。
        let waited = self.wait_for_switches(std::time::Duration::from_secs(5));
        if !waited {
            log::warn!("还有换纸线程没结束，产物缓存这次不清理（下一轮会覆盖）");
        }
        self.screens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        log::info!("轮播已停止（未改动你的壁纸）");
    }

    /// 单独暂停/继续某一块屏。
    pub fn pause_screen(&self, monitor_name: &str, paused: bool) -> Result<bool, String> {
        // 先看这块屏在不在：不在就直接拒绝，不必绕一圈命令通道。
        if !self
            .bindings()
            .iter()
            .any(|b| b.monitor_name.eq_ignore_ascii_case(monitor_name))
        {
            return Ok(false);
        }
        self.send(PlaybackCommand::PauseScreen {
            monitor_name: monitor_name.to_string(),
            paused,
        })?;
        Ok(true)
    }

    /// 单独把某一块屏切到下一张（另一块不动）。界面点「下一张」那张预览格走这里。
    pub fn next_screen(&self, monitor_name: &str) -> Result<bool, String> {
        // 先看这块屏在不在：不在就直接拒绝，不必绕一圈命令通道（与 `pause_screen` 一致）。
        if !self
            .bindings()
            .iter()
            .any(|b| b.monitor_name.eq_ignore_ascii_case(monitor_name))
        {
            return Ok(false);
        }
        self.send(PlaybackCommand::NextScreen {
            monitor_name: monitor_name.to_string(),
        })?;
        Ok(true)
    }

    /// 发一条指令。
    pub fn send(&self, command: PlaybackCommand) -> Result<(), String> {
        let guard = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(sender) => sender.send(command).map_err(|e| e.to_string()),
            None => Err("轮播未在运行".to_string()),
        }
    }

    pub fn is_running(&self) -> bool {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// 是否被**全局**暂停（每屏单独暂停不算，见 [`PlaybackManager::pause_screen`]）。
    ///
    /// 单独给一个取值口而不是让调用方读 `status().paused`：`status()` 会顺带
    /// 枚举显示器、锁三份状态、拼一整份界面用的视图；而托盘菜单每 200 ms 就要问一次
    /// "现在到底在不在轮播"（见 `tray::spawn_check_refresher`），那份开销没有必要。
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn is_nsfw(&self) -> bool {
        self.nsfw.load(Ordering::SeqCst)
    }

    // --- NSFW 模式 -------------------------------------------------

    /// 预先解码每屏的 NSFW 壁纸，让点击/快捷键那一刻只剩一次系统调用。
    ///
    /// 在后台线程里做：一张 30 MP 的图要几百毫秒，不该卡住启动或界面。
    pub fn prewarm_nsfw(&self, settings: &Settings) {
        let monitors: Vec<(String, String)> = settings
            .monitors
            .iter()
            .filter(|(_, m)| !m.nsfw_wallpaper.trim().is_empty())
            .map(|(name, m)| (name.clone(), m.nsfw_wallpaper.clone()))
            .collect();
        if monitors.is_empty() {
            return;
        }
        let bindings = match self.refresh_bindings() {
            Ok(bindings) => bindings,
            Err(error) => {
                log::warn!("预热 NSFW 壁纸失败（枚举显示器）：{error}");
                return;
            }
        };
        let output_dir = self.output_dir();
        let spawned = std::thread::Builder::new()
            .name("wwp-nsfw-prewarm".to_string())
            .spawn(move || {
                let sink = DesktopSink::new(bindings, output_dir);
                for (monitor, source) in monitors {
                    match sink.prepare_nsfw(&monitor, std::path::Path::new(&source)) {
                        Ok(true) => log::info!("NSFW 壁纸已预热：{monitor}"),
                        Ok(false) => log::debug!("NSFW 壁纸已是最新：{monitor}"),
                        Err(error) => log::warn!("预热 NSFW 壁纸失败：{monitor}：{error}"),
                    }
                }
            });
        if let Err(error) = spawned {
            log::warn!("无法启动 NSFW 壁纸预热线程：{error}");
        }
    }

    /// 进入 NSFW 模式：停掉轮播，把每屏切成设置好的那张固定图。
    ///
    /// 这个过程**不还原任何壁纸**，只是把当前显示换成 NSFW 壁纸。
    pub fn enter_nsfw(&self, settings: &Settings) -> Result<Vec<ScreenResult>, String> {
        self.apply_nsfw_wallpapers(settings, true)
    }

    /// NSFW 模式**已经在开着**时，按新的 NSFW 壁纸重新切换一次。
    ///
    /// ## 为什么需要单独一个入口
    ///
    /// 用户报过"有时候无法设置 NSFW 壁纸"。原因是：在 NSFW 模式里改了那一张图，
    /// 后端只把新图**解好缓存**，屏幕上仍然显示旧的那张——从用户的角度看，
    /// 就是"我改了但它没变"。他的要求是"无论是否处于 NSFW 模式，都能改这两张"。
    ///
    /// 因此改完设置后：状态还是 NSFW 模式，就把新的那几张立刻切上去。
    pub fn reapply_nsfw(&self, settings: &Settings) -> Result<Vec<ScreenResult>, String> {
        self.apply_nsfw_wallpapers(settings, false)
    }

    /// 把每屏的 NSFW 壁纸准备并设上去。`entering` 只影响日志措辞。
    ///
    /// ## 顺序为什么是"停 → 设 → 再收尾"（用户报的"切 NSFW 有几百毫秒延时"）
    ///
    /// 第一版是"停轮播（等它彻底停干净）→ 设 NSFW 壁纸"。而"停干净"包含了
    /// **等在跑的那一块屏把当前这张图解码完**（实测 200~250 ms，且按 100 ms
    /// 一档轮询，凑上去更多），于是用户点一下 NSFW，先白等几百毫秒才看到图。
    ///
    /// 关键在于这两步**没有先后依赖**：都是"把某张图设成壁纸"，后设的那张赢。
    /// 因此现在只发一个"请停"的信号（毫秒级返回）就立刻设 NSFW 壁纸，
    /// 要删产物缓存时再等在跑的那次换纸写完（见 [`PlaybackManager::stop_core`]）。
    /// 用户感知的时间因此只剩"设上去 + 系统画出新图"这两段。
    fn apply_nsfw_wallpapers(
        &self,
        settings: &Settings,
        entering: bool,
    ) -> Result<Vec<ScreenResult>, String> {
        let started = std::time::Instant::now();
        if entering {
            // **先把"屏幕上现在是哪张"记下来**，退出时要把它放回去
            // （用户 2026-10 的要求，见 `pre_nsfw`）。必须赶在 `request_stop_core()`
            // 之前：那一步之后调度线程就再也不会发布新状态了。
            //
            // 已知的一个小缺口：如果按下 NSFW 的那一刻正好有一块屏在解码（换纸线程
            // 已经起来、但还没把新状态发布出去），记下的是**上一张**。窗口只有
            // 100~250 ms，而且后果只是"退出时回到再上一张"，不值得为它把
            // "点一下就切过去"这条要求赔进去（那正是上一轮花力气去掉的等待）。
            self.capture_pre_nsfw();
        }
        self.request_stop_core();
        let stop_millis = started.elapsed().as_millis();

        let bindings = self.bindings_for_apply()?;
        let listed_millis = started.elapsed().as_millis();
        let sink = DesktopSink::new(bindings.clone(), self.output_dir());
        let mut results = Vec::new();

        for binding in &bindings {
            let monitor = binding.monitor_name.clone();
            let source = settings
                .monitors
                .get(&monitor)
                .map(|m| m.nsfw_wallpaper.trim().to_string())
                .unwrap_or_default();
            if source.is_empty() {
                results.push(ScreenResult::message(
                    &monitor,
                    "没有设置 NSFW 壁纸（在播放页这块屏的卡片里选一张）",
                    false,
                ));
                continue;
            }

            // 命中缓存时这一步几乎不花时间；没命中才现场解码。
            match sink.prepare_nsfw(&monitor, std::path::Path::new(&source)) {
                Ok(prepared) => {
                    let path = sink.nsfw_path(&monitor);
                    match sink.apply_prepared(&monitor, &path) {
                        Ok(millis) => results.push(ScreenResult::applied(
                            &monitor,
                            if prepared {
                                format!("已切换 NSFW 壁纸（现场解码，{millis} ms）")
                            } else {
                                format!("已切换 NSFW 壁纸（{millis} ms）")
                            },
                            &source,
                        )),
                        Err(error) => results.push(ScreenResult::message(&monitor, error, false)),
                    }
                }
                Err(error) => results.push(ScreenResult::message(&monitor, error, false)),
            }
        }

        self.nsfw.store(true, Ordering::SeqCst);
        // NSFW 模式下没有播放线程，清掉每屏状态，界面显示"NSFW 模式"而不是"未运行"。
        self.screens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        // **壁纸已经设上去了，下面才是收尾。**
        // 停在跑的换纸线程：它的产物文件马上要被删掉（写完再删，顺序不能反）。
        // 这一步在用户看来是"已经切过去了"之后的事，因此不占感知时间。
        if !self.wait_for_switches(std::time::Duration::from_secs(5)) {
            log::warn!("还有换纸线程没结束，本次不清理轮播产物缓存");
        } else {
            // 只保留 NSFW 壁纸的缓存：把轮播用的两个产物删掉（用户明确要求）。
            let freed: u64 = bindings
                .iter()
                .map(|binding| sink.forget_slots(&binding.monitor_name))
                .sum();
            if freed > 0 {
                log::info!(
                    "已清掉轮播产物缓存（释放 {:.1} MB），只保留 NSFW 壁纸",
                    freed as f64 / 1_048_576.0
                );
            }
        }

        log::info!(
            "{}（{} 块屏）；用户可感知的那一段：请停轮播 {stop_millis} ms → 显示器列表到 {listed_millis} ms → 壁纸设完 {} ms",
            if entering {
                "已进入 NSFW 模式"
            } else {
                "NSFW 模式：已按新的 NSFW 壁纸重新切换"
            },
            results.len(),
            started.elapsed().as_millis(),
        );
        Ok(results)
    }

    /// 退出 NSFW 模式：恢复轮播（若开了自动接管）。
    ///
    /// ## 回去的是"进入 NSFW 之前那两张"，不是重新抽一张
    ///
    /// 用户 2026-10 的要求："退出 NSFW 模式后，可以继续播放 NSFW 模式之前的壁纸，
    /// 而不是重新加载壁纸。"原先这里走的是 `start(settings, true)`
    /// （[`Startup::ApplyNow`]）——每块屏各抽一张新的，用户看到的就是"一退出
    /// 壁纸就被换掉了"。
    ///
    /// 现在：把进入时记下的那份记忆交给 [`Startup::RememberNow`]，
    /// 屏幕**当场**回到原图，第一次自动切换用原来看见的那张「下一张」，
    /// 倒计时按配置间隔重新起算（不去猜"暂停了多久"——与"不保存剩余播放时间"
    /// 是同一条要求）。
    ///
    /// ## 记忆是空的怎么办
    ///
    /// 重启后直接停在 NSFW 模式里时，内存里那份是空的（进程刚起来、还没有
    /// 调度线程）。这时**不覆盖**文件那份：`data/playback-state.json` 记的正是
    /// 上次退出时屏幕上的两张，于是"跨重启退出 NSFW"同样回到原来看的那张；
    /// 连文件也没有（第一次用、或文件被删了）才落到"照常抽一张"。
    pub fn leave_nsfw(&self, settings: &Settings) -> Result<(), String> {
        self.nsfw.store(false, Ordering::SeqCst);

        // 取走"进入之前那两张"。**放在自动接管那个判断之前**：它回答的是
        // "下一次恢复用哪份记忆"，与"这一次要不要立刻开始轮播"是两件事——
        // 关了自动接管、之后手动点「开始轮播」时也不该把这份记忆弄丢。
        let captured = self.take_pre_nsfw();
        if !captured.is_empty() {
            log::info!(
                "退出 NSFW 模式：回到进入之前那 {} 块屏的壁纸（不重新抽签）",
                captured.len()
            );
            *self.memory_override.lock().unwrap_or_else(|e| e.into_inner()) = Some(captured);
        } else {
            log::info!("退出 NSFW 模式：内存里没有记过屏幕上是哪张，改用上次退出的记录");
        }

        if settings.general.auto_start {
            self.start_with(settings, Startup::RememberNow)?;
        } else {
            log::info!("已退出 NSFW 模式（自动接管关闭，等待手动开始轮播）");
        }
        Ok(())
    }

    /// 把"进入 NSFW 模式"的逐屏结果记进状态，供界面展示。
    ///
    /// 现在只用于**记下每块屏显示的是哪张源图**（界面上的「当前显示」要用），
    /// 不再维护"上次操作的逐屏结果"——用户明确说那块内容没有意义，已删掉。
    pub fn record_nsfw(&self, results: &[ScreenResult]) {
        for result in results {
            if result.ok {
                if let Some(source) = &result.source {
                    self.remember_shown(&result.monitor_name, source);
                }
            }
        }
    }
}
/// 调度线程与外部共享的几个槽位（打包传递，省得参数列表太长）。
struct ManagerShared {
    screens: Arc<Mutex<Vec<ScreenState>>>,
    paused: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    /// "每块屏最后设上去的是哪张源图"。调度线程每轮都往里写，
    /// 界面据此显示「当前显示」（见 `PlaybackManager::shown`）。
    shown: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// `[general].pause_on_fullscreen` 的当前值。
    ///
    /// 用共享原子量而不是"建线程时传一个 bool"：用户在设置页开关这个选项时
    /// **不该重建轮播**。重建会让每块屏白闪一次（项目里为此专门做过
    /// `plan_fingerprint` 那套"计划没变就不重建"的判断），而这个开关只是
    /// 让调度线程多看一眼某个值而已。
    pause_on_fullscreen: Arc<AtomicBool>,
    /// 数据目录：退出时的播放位置写在这里（`data/playback-state.json`）。
    ///
    /// 由调度线程自己拿着，而不是每轮去 `state.data_dir` 里取：写盘发生在
    /// **每次换纸之后**，那条路上不该再有锁与克隆。
    state_dir: std::path::PathBuf,
    /// "上一轮写进文件的那份记忆"，用来判断**要不要再写一次**。
    ///
    /// 初值故意是 `None`（而不是启动时算出的那份）：那样第一轮一定会写一次，
    /// 于是"文件被用户删了/坏了"也会被立刻修好，不必等到下一次换纸。
    remembered: Option<Vec<String>>,
    /// 正在换纸的线程数（见 [`SwitchGuard`]）。
    switches: Arc<AtomicUsize>,
}

/// 把各屏的实时状态收成"要落盘的那份记忆"。
///
/// 只留**有内容**的屏：一块屏被拔掉之后，它那条空记录不该一直留在文件里
/// （与 `playback_state::save` 的过滤是同一条，这里做一遍是为了让
/// "要不要写盘"的比较与真正写下去的内容一致）。
fn memory_of(states: &[ScreenState]) -> Vec<wwp_core::playback::ScreenMemory> {
    states
        .iter()
        .filter(|state| state.current.is_some() || state.next.is_some())
        .map(|state| wwp_core::playback::ScreenMemory {
            monitor_name: state.monitor_name.clone(),
            current: state.current.clone(),
            next: state.next.clone(),
        })
        .collect()
}

/// 这份记忆的"内容指纹"，只用来比对变没变。
fn memory_fingerprint(memory: &[wwp_core::playback::ScreenMemory]) -> Vec<String> {
    let mut lines: Vec<String> = memory
        .iter()
        .map(|screen| {
            format!(
                "{}|{}|{}",
                screen.monitor_name,
                screen.current.as_deref().unwrap_or(""),
                screen.next.as_deref().unwrap_or("")
            )
        })
        .collect();
    lines.sort();
    lines
}

impl ManagerShared {
    /// 把当前的播放位置落盘——**只在内容真的变了时**写。
    ///
    /// ## 为什么每轮都试、而不是退出时写一次
    ///
    /// Windows 的关机、注销与"任务管理器结束任务"都不保证给程序一次收尾机会，
    /// 只在退出时写等于"正常退出才有记忆"。换纸是分钟级事件，因此这里几乎
    /// 总是"比一比就返回"，开销可以忽略（见 `playback_state` 模块的说明）。
    fn persist_memory(&mut self, states: &[ScreenState]) {
        let memory = memory_of(states);
        let fingerprint = memory_fingerprint(&memory);
        if self.remembered.as_ref() == Some(&fingerprint) {
            return;
        }
        if self.state_dir.as_os_str().is_empty() {
            // 还没告诉我们数据目录（理论上不会发生）：不写，但也别每轮重试。
            self.remembered = Some(fingerprint);
            return;
        }
        playback_state::save(&self.state_dir, &memory);
        self.remembered = Some(fingerprint);
    }
}

/// 全屏暂停的防抖状态。
///
/// ## 为什么需要防抖
///
/// 游戏切出去看一眼浏览器、再切回来是极常见的动作。暂停**立刻**生效没问题
/// （用户看不见的时候不该继续解码换纸），但恢复不能那么急：
/// 每恢复一次都会把这块屏的倒计时重置（见 `ScreenRuntime::settle_pause_change`），
/// 若 500 ms 后跟着又暂停，那块屏的倒计时就被反复重置，永远走不到换纸那一刻。
///
/// 因此"没有全屏窗口"必须**连续成立若干轮**才真的恢复轮播。
#[derive(Default)]
struct FullscreenDebounce {
    /// 每块屏："没有全屏窗口"已经连续成立了几轮。
    clear_streak: std::collections::HashMap<String, u32>,
}

impl FullscreenDebounce {
    /// 一轮判定：把结果同步到 [`Playback`] 的"自动暂停"位上。
    ///
    /// 每轮循环（最多 500 ms 一次）调用一次。
    fn sync(&mut self, playback: &Playback, pause_on_fullscreen: bool) {
        if !pause_on_fullscreen {
            // 开关被关掉：把之前因为全屏而停下的屏放开。
            // `clear_auto_pauses` 只碰"全屏"那一位，用户手动暂停的不受影响。
            playback.clear_auto_pauses();
            self.clear_streak.clear();
            return;
        }

        let busy = wwp_core::fullscreen::screens_with_fullscreen_window();
        for state in playback.states() {
            let name = state.monitor_name;
            let is_busy = busy
                .iter()
                .any(|busy_name| busy_name.eq_ignore_ascii_case(&name));
            if is_busy {
                self.clear_streak.insert(name.clone(), 0);
                // 固定单图的屏会在这里被 `set_auto_paused` 忽略（返回 None）。
                let _ = playback.set_auto_paused(&name, true);
            } else {
                let streak = self.clear_streak.entry(name.clone()).or_insert(0);
                *streak += 1;
                if *streak >= RESUME_STREAK {
                    let _ = playback.set_auto_paused(&name, false);
                }
            }
        }
    }
}

/// "没有全屏窗口"要连续成立这么多轮才恢复轮播。
///
/// 调度线程每轮最多睡 500 ms，因此 3 轮约等于 1.5 秒。
const RESUME_STREAK: u32 = 3;

/// 手里的显示器列表多久算"还新鲜"，可以直接用它设壁纸。
///
/// 比"用户点一次按钮"的间隔长得多（没人一秒内连点 NSFW），
/// 又比"拔掉一块屏再点 NSFW"短。超时或列表为空就现枚举一次
/// （见 [`PlaybackManager::bindings_for_apply`]）。
const BINDINGS_FRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// 换纸线程的在场证明：建它时计数 +1，它被 drop 时 -1。
///
/// 用一个 RAII 计数而不是 `JoinHandle` 列表：换纸线程是在循环里现起现丢的
/// （见 `run_manager` 的"到点了"分支），要收住它们的 `JoinHandle` 就得在
/// 调度线程里维护一个只增不减的容器。计数器只回答一个问题：
/// **现在还有几个换纸线程没写完文件**——清理产物缓存前需要知道的就是这个。
struct SwitchGuard(Arc<AtomicUsize>);

impl SwitchGuard {
    fn new(counter: Arc<AtomicUsize>) -> SwitchGuard {
        counter.fetch_add(1, Ordering::SeqCst);
        SwitchGuard(counter)
    }
}

impl Drop for SwitchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 调度线程主体：独占 [`Playback`]，串行处理指令，到点**各自**换纸。
fn run_manager(
    mut playback: Playback,
    sink: Arc<dyn wwp_core::playback::WallpaperSink>,
    receiver: Receiver<PlaybackCommand>,
    mut shared: ManagerShared,
) {
    // 状态先发布一次：界面立刻能看到各屏的候选数。
    *shared.screens.lock().unwrap_or_else(|e| e.into_inner()) = playback.states();

    // 全屏暂停的防抖状态（见 `FullscreenDebounce`）。
    let mut fullscreen = FullscreenDebounce::default();

    loop {
        // 等到"下一次该换纸"，但最多等 500 ms——这样手动操作（下一张/暂停）
        // 不会被一个很长的间隔压住，而换纸本身仍然精确发生在到点那一刻。
        let wait = playback
            .time_until_next()
            .min(std::time::Duration::from_millis(500));

        let command = match receiver.recv_timeout(wait) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            // 通道断开（发送端被丢弃）就是要退出。
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match command {
            Some(PlaybackCommand::Next) => {
                log_outcomes(&playback.next_all(&sink));
            }
            Some(PlaybackCommand::Previous) => {
                log_outcomes(&playback.previous_all(&sink));
            }
            Some(PlaybackCommand::Pause) => {
                playback.pause();
                shared.paused.store(true, Ordering::SeqCst);
                log::info!("已暂停（所有屏）");
            }
            Some(PlaybackCommand::Resume) => {
                playback.resume();
                shared.paused.store(false, Ordering::SeqCst);
                log::info!("已继续（所有屏）");
            }
            Some(PlaybackCommand::PauseScreen {
                monitor_name,
                paused,
            }) => {
                playback.pause_screen(&monitor_name, paused);
            }
            Some(PlaybackCommand::NextScreen { monitor_name }) => {
                // 只动这一块屏：另一块既不该换纸，也不该被碰一下倒计时。
                let outcome = playback.next_on(&monitor_name, &sink);
                log_outcomes(&[(monitor_name.clone(), outcome)]);
                *shared.screens.lock().unwrap_or_else(|e| e.into_inner()) = playback.states();
            }
            Some(PlaybackCommand::ForgetAndNext { monitor_name, path }) => {
                // 先把"我们最后设上去的那张"从记忆里抹掉：它已经进了回收站，
                // 界面上再显示它就会变成"当前显示：一个不存在的文件"。
                // 紧接着的换纸会把新的一张写进这份记忆。
                {
                    let mut shown = shared.shown.lock().unwrap_or_else(|e| e.into_inner());
                    shown.retain(|name, value| {
                        !(name.eq_ignore_ascii_case(&monitor_name)
                            && value.eq_ignore_ascii_case(&path))
                    });
                }
                playback.forget_candidate(&monitor_name, &path);
                // 即使它本来不在候选里（例如计划刚被重建过）也要换一张：
                // 用户点这个按钮的意思是"这张不要了，换下一张"。
                let outcome = playback.next_on(&monitor_name, &sink);
                log_outcomes(&[(monitor_name.clone(), outcome)]);
                // 换了纸之后立刻把新状态发布出去，界面不必等下一轮。
                *shared.screens.lock().unwrap_or_else(|e| e.into_inner()) = playback.states();
            }
            Some(PlaybackCommand::ReplacePlans { plans }) => {
                // **只重建、只换纸那些计划真的变了的屏。**
                //
                // 搬运没变的屏是关键：新建一个 `Playback` 会让每块屏的当前图、
                // 倒计时与随机历史全部清零，于是"改了 A 屏"会让 B 屏也立刻换一张
                // （用户 2026-10 报的 bug）。`rebuild_with` 会把指纹相同的屏的
                // 运行时原样搬过去，并告诉我们哪些屏是新的。
                let (next, changed) = playback.rebuild_with(plans);
                playback = next;
                if changed.is_empty() {
                    log::info!("已按新设置更新播放计划（没有屏的计划变化，未换任何纸）");
                } else {
                    log::info!(
                        "已按新设置重建 {} 块屏的计划，正在为变了的屏换纸：{}",
                        playback.states().len(),
                        changed.join("、")
                    );
                    log_outcomes(&playback.apply_screens(&changed, &sink));
                }
            }
            Some(PlaybackCommand::Refresh) => {}
            None => {
                // 到点了：把每块该换的屏**各自**派到一个短命线程里换。
                //
                // 这里**不等它们做完**：一块屏解码慢（几百毫秒）不该拖住另一块
                // 跟着晚换。用户已明确取消"两屏同步、快的等慢的"，而且两块屏
                // 现在可以各自暂停/继续——"等齐"与"独立"是互相牵制的。
                //
                // 换纸结果由每个线程各自记日志（`playback::advance`），
                // 界面需要的那份"现在显示的是哪张"由下面的 `shown` 承担。
                for task in playback.claim_due() {
                    let sink = Arc::clone(&sink);
                    let name = task.monitor_name().to_string();
                    // 在场证明：`stop` 之后要删产物缓存，得先确认这个线程写完了。
                    let guard = SwitchGuard::new(Arc::clone(&shared.switches));
                    let spawned = std::thread::Builder::new()
                        .name(format!("wwp-switch-{name}"))
                        .spawn(move || {
                            let _guard = guard;
                            log_outcomes(&[task.run(&sink)]);
                        });
                    if let Err(error) = spawned {
                        log::warn!("无法为 {name} 启动换纸线程：{error}");
                    }
                }
            }
        }

        // --- 全屏检测 ---
        //
        // 放在这里而不是另起一条线程：本循环最多睡 500 ms，因此它天然就是
        // "每 500 ms 看一次"的节奏，而 `EnumWindows` 枚举几十个窗口是微秒级开销。
        // 检测前会先把本线程标成 per-monitor DPI 感知（见 `fullscreen` 模块）。
        //
        // **必须在下面发布 `states` 之前做**：界面读的就是那一份快照，
        // 放在后面会让"全屏中已暂停"这个标记晚一轮才显示出来。
        fullscreen.sync(&playback, shared.pause_on_fullscreen.load(Ordering::SeqCst));

        // 把最新状态写给界面，并记下"每块屏现在显示的是哪张源图"。
        let states = playback.states();
        {
            let mut shown = shared.shown.lock().unwrap_or_else(|e| e.into_inner());
            for state in &states {
                if let Some(current) = &state.current {
                    shown.insert(state.monitor_name.clone(), current.clone());
                }
            }
        }
        // 顺手把"退出时的播放位置"记下来（内容没变就什么都不做）。
        // 放在这里是因为 `states` 恰好就是最新的一份，而它已经算好了。
        shared.persist_memory(&states);
        *shared.screens.lock().unwrap_or_else(|e| e.into_inner()) = states;
    }

    // 收尾：只标记停止，**不还原任何壁纸**。
    shared.running.store(false, Ordering::SeqCst);
    log::info!("调度线程已退出");
}

/// 播放计划的指纹（每屏一个）。
///
/// 用来判断"设置改了，但播放行为其实没变"——那种情况下重建轮播只会让壁纸
/// 白闪一次。候选集合的**内容**也要参与：加了几张图、改了筛选之后数量可能不变，
/// 但候选不同了，必须重建。
///
/// **固定单图的路径也必须参与**：那种屏的候选本来就是空的，若只看候选，
/// "换了一张单图"会得出"什么都没变"的结论，于是换了图屏幕上却还是旧的。
///
/// ## 实现已经搬到引擎里（`ScreenPlan::fingerprint`）
///
/// 因为"哪几块屏的计划变了"不再只是外壳内部的一个判断，而是
/// [`Playback::rebuild_with`] 的输入——引擎必须自己认得"这两份计划是不是同一份"。
/// 两处各写一套必然走样（AGENTS §7.3），因此这里只做转发。
fn plan_fingerprint(plans: &[wwp_core::playback::ScreenPlan]) -> Vec<String> {
    plans.iter().map(|plan| plan.fingerprint()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wwp_core::config::FitMode;
    use wwp_core::library::LibraryEntry;
    use wwp_core::playback::ScreenPlan;

    fn entry(path: &str) -> LibraryEntry {
        LibraryEntry {
            p: path.to_string(),
            w: 100,
            h: 100,
            mt: 1,
            sz: 1,
            or: 1,
            fmt: "png".to_string(),
            ok: 1,
            kind: wwp_core::library::index::MediaKind::Image,
        }
    }

    fn plan(name: &str, interval: u64, paths: &[&str]) -> ScreenPlan {
        ScreenPlan {
            monitor_name: name.to_string(),
            width: 800,
            height: 600,
            interval: std::time::Duration::from_secs(interval),
            fit: FitMode::Fill,
            candidates: paths.iter().map(|p| entry(p)).collect(),
            single_image: None,
        }
    }

    /// 一块固定单图的屏。
    fn static_plan(name: &str, image: &str) -> ScreenPlan {
        ScreenPlan {
            monitor_name: name.to_string(),
            width: 800,
            height: 600,
            interval: std::time::Duration::from_secs(100),
            fit: FitMode::Fill,
            candidates: Vec::new(),
            single_image: Some(std::path::PathBuf::from(image)),
        }
    }

    #[test]
    fn outcomes_are_translated_and_failure_is_distinguished_from_no_op() {
        // "没有可回退的上一张"是正常状态，不该被界面显示成错误；
        // 而 Failed 必须带出原因。这条区分是界面上"提示"与"报错"的分界线。
        let applied = ScreenResult::from_outcome("a", ControlOutcome::Applied);
        assert!(applied.ok);
        assert_eq!(applied.outcome, "已切换");

        let no_history = ScreenResult::from_outcome("a", ControlOutcome::NoHistory);
        assert!(!no_history.ok);
        assert!(no_history.outcome.contains("上一张"));

        let fixed = ScreenResult::from_outcome("a", ControlOutcome::FixedImage);
        assert!(!fixed.ok);
        assert!(
            fixed.outcome.contains("固定单图"),
            "固定单图的屏要明确说清原因：{}",
            fixed.outcome
        );

        let failed = ScreenResult::from_outcome("a", ControlOutcome::Failed("磁盘满了".into()));
        assert!(!failed.ok);
        assert_eq!(failed.outcome, "磁盘满了", "失败原因要原样带出来");
    }

    #[test]
    fn fingerprint_ignores_nothing_that_changes_playback() {
        let base = vec![plan(r"\\.\DISPLAY1", 100, &["a.jpg", "b.jpg"])];

        // 间隔变了 → 指纹必须变（否则"改了间隔"不会生效）。
        assert_ne!(
            plan_fingerprint(&base),
            plan_fingerprint(&[plan(r"\\.\DISPLAY1", 60, &["a.jpg", "b.jpg"])])
        );
        // 候选内容变了（数量不变）→ 指纹必须变。
        assert_ne!(
            plan_fingerprint(&base),
            plan_fingerprint(&[plan(r"\\.\DISPLAY1", 100, &["a.jpg", "c.jpg"])])
        );
        // 候选数量变了 → 指纹必须变。
        assert_ne!(
            plan_fingerprint(&base),
            plan_fingerprint(&[plan(r"\\.\DISPLAY1", 100, &["a.jpg"])])
        );
        // 一样的计划 → 指纹相同（这是"不必重建"的依据）。
        assert_eq!(
            plan_fingerprint(&base),
            plan_fingerprint(&[plan(r"\\.\DISPLAY1", 100, &["a.jpg", "b.jpg"])])
        );
    }

    #[test]
    fn fingerprint_changes_when_the_single_image_changes() {
        // **这条守的是"换了单图要生效"。** 固定单图的屏**没有候选**，
        // 若指纹只看候选，"把 a.png 换成 b.png"会被判成"什么都没变"，
        // 于是设置保存了、屏幕上却还是旧的那张。
        let before = vec![static_plan(r"\\.\DISPLAY1", "C:/pics/a.png")];
        let after = vec![static_plan(r"\\.\DISPLAY1", "C:/pics/b.png")];
        assert_ne!(
            plan_fingerprint(&before),
            plan_fingerprint(&after),
            "单图路径变了，指纹必须变，否则不会重建、屏幕上还是旧图"
        );
        // 没变的时候必须相同，否则每次保存都会白闪一次。
        assert_eq!(
            plan_fingerprint(&before),
            plan_fingerprint(&[static_plan(r"\\.\DISPLAY1", "C:/pics/a.png")])
        );
        // "轮播"与"固定单图"之间切换也必须被认出来。
        assert_ne!(
            plan_fingerprint(&before),
            plan_fingerprint(&[plan(r"\\.\DISPLAY1", 100, &[])])
        );
    }

    #[test]
    fn fingerprint_is_stable_across_monitor_order() {
        // 显示器枚举顺序变了（插拔之后很常见），但每块屏自己的设置没变时，
        // 指纹的**内容**应当一致。这里用排序后的比较来固定这一点。
        let a = vec![
            plan(r"\\.\DISPLAY1", 100, &["a.jpg"]),
            plan(r"\\.\DISPLAY2", 100, &["b.jpg"]),
        ];
        let b = vec![
            plan(r"\\.\DISPLAY2", 100, &["b.jpg"]),
            plan(r"\\.\DISPLAY1", 100, &["a.jpg"]),
        ];
        let mut fa = plan_fingerprint(&a);
        let mut fb = plan_fingerprint(&b);
        fa.sort();
        fb.sort();
        assert_eq!(fa, fb);
    }

    #[test]
    fn restarting_the_scheduler_clears_the_paused_flag() {
        // **真实踩到的 bug**（把状态摆到托盘菜单上时才暴露出来）：
        // 暂停 → 点「重新加载」之后，新建的 `Playback` 是**未暂停**的，
        // 而对外那个 `paused` 标志没人复位。于是屏上明明在轮播，
        // `status().paused` 却是 true：界面上的按钮显示成「继续」，
        // 托盘菜单里的「轮播中」也不打勾。
        //
        // 空计划 + 不立刻换纸：只起一个会空转的调度线程，**不碰任何壁纸**。
        let manager = PlaybackManager::new();
        manager.paused.store(true, Ordering::SeqCst);

        manager
            .spawn_scheduler(Vec::new(), Startup::Wait)
            .expect("起调度线程不该失败");
        assert!(
            !manager.is_paused(),
            "新起的调度线程是未暂停的，对外的暂停标志必须跟着复位"
        );
        assert!(manager.is_running(), "有调度线程就是运行中");

        manager.stop();
        assert!(!manager.is_running(), "停掉之后不再是运行中");
    }

    #[test]
    fn shown_for_finds_the_monitor_regardless_of_name_case() {
        // 「删除当前壁纸」先要问"这块屏现在显示的是哪张"。名字从界面绕了一圈
        // JSON 回来，大小写不敏感匹配是这一层的责任（Windows 的显示器名
        // `\\.\DISPLAY1` 本身不区分大小写）。
        let manager = PlaybackManager::new();
        assert_eq!(manager.shown_for(r"\\.\DISPLAY1"), None, "还没设过就是没有");
        manager.remember_shown(r"\\.\DISPLAY1", "C:/pics/a.png");
        assert_eq!(
            manager.shown_for(r"\\.\display1").as_deref(),
            Some("C:/pics/a.png")
        );
        assert_eq!(manager.shown_for(r"\\.\DISPLAY2"), None);
        // 空路径不该被记下来（否则界面上会显示"当前显示：空"）。
        manager.remember_shown(r"\\.\DISPLAY2", "   ");
        assert_eq!(manager.shown_for(r"\\.\DISPLAY2"), None);
    }

    /// 一块屏的运行时状态快照（只填测试关心的那几个字段）。
    fn screen_state(name: &str, current: Option<&str>, next: Option<&str>) -> ScreenState {
        ScreenState {
            monitor_name: name.to_string(),
            current: current.map(str::to_string),
            next: next.map(str::to_string),
            changes: 3,
            last_change_millis: 12,
            last_error: None,
            candidate_count: 100,
            next_in_seconds: 30,
            paused: false,
            paused_manual: false,
            paused_by_fullscreen: false,
        }
    }

    #[test]
    fn the_memory_taken_before_the_mode_covers_both_sources() {
        // 进入 NSFW 模式之前那一份记忆要同时用到两个来源（见 `memory_snapshot`）：
        //  - 调度线程发布的实时状态：它带着「下一张」，退出后第一次切换靠它；
        //  - `shown`：调度线程不在跑时它就是"最后被我们设上去的是哪张"。
        let manager = PlaybackManager::new();
        *manager.screens.lock().unwrap() = vec![
            screen_state(
                r"\\.\DISPLAY1",
                Some("C:/pics/live.png"),
                Some("C:/pics/predicted.png"),
            ),
            // 实时状态里没有内容（刚点过「重新加载」、或还没换过纸）。
            screen_state(r"\\.\DISPLAY2", None, None),
            // 两边都没有内容的屏：不该混进记忆里（否则"恢复了几块屏"会虚高）。
            screen_state(r"\\.\DISPLAY3", None, None),
        ];
        manager.remember_shown(r"\\.\DISPLAY2", "C:/pics/from-shown.png");
        // 只在 `shown` 里的屏（实时状态里连它都没有）。
        manager.remember_shown(r"\\.\DISPLAY4", "C:/pics/only-shown.png");

        let memory = manager.memory_snapshot();
        assert_eq!(memory.len(), 3, "只有真有内容的屏才该进记忆：{memory:?}");

        let find = |suffix: &str| {
            memory
                .iter()
                .find(|record| record.monitor_name.ends_with(suffix))
                .unwrap_or_else(|| panic!("记忆里应当有 {suffix}：{memory:?}"))
        };
        let first = find("DISPLAY1");
        assert_eq!(first.current.as_deref(), Some("C:/pics/live.png"));
        assert_eq!(
            first.next.as_deref(),
            Some("C:/pics/predicted.png"),
            "「下一张」只有实时状态里有，必须带上（退出后第一次切换用它）"
        );
        let second = find("DISPLAY2");
        assert_eq!(second.current.as_deref(), Some("C:/pics/from-shown.png"));
        assert_eq!(second.next, None);
        let fourth = find("DISPLAY4");
        assert_eq!(fourth.current.as_deref(), Some("C:/pics/only-shown.png"));
        assert_eq!(fourth.next, None);
        assert!(
            !memory
                .iter()
                .any(|record| record.monitor_name.ends_with("DISPLAY3")),
            "什么都没有的屏不该进记忆"
        );
    }

    #[test]
    fn leaving_the_mode_hands_the_pre_mode_memory_to_the_next_restore() {
        // **用户 2026-10 的要求**："退出 NSFW 模式后，继续播放 NSFW 模式之前的
        // 壁纸，而不是重新加载壁纸。"
        //
        // 这里把自动接管关掉，于是 `leave_nsfw` 不会去动真壁纸（测试环境里也不该动），
        // 但"进入之前那两张"必须完整地交到下一次恢复手里——而这正是问题的全部：
        // 原先退出走的是 `ApplyNow`（每屏各抽一张新的）。
        let manager = PlaybackManager::new();
        *manager.screens.lock().unwrap() = vec![screen_state(
            r"\\.\DISPLAY1",
            Some("C:/pics/before.png"),
            Some("C:/pics/before-next.png"),
        )];
        manager.capture_pre_nsfw();

        let mut settings = Settings::default();
        settings.general.auto_start = false;
        manager
            .leave_nsfw(&settings)
            .expect("退出 NSFW 模式不该失败");

        assert!(!manager.is_nsfw(), "退出之后状态位必须落下来");
        assert!(
            manager.pre_nsfw.lock().unwrap().is_empty(),
            "记的那份已经交出去，不该留着"
        );
        let handed = manager
            .memory_override
            .lock()
            .unwrap()
            .clone()
            .expect("应当交出一份记忆给下一次恢复");
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].current.as_deref(), Some("C:/pics/before.png"));
        assert_eq!(
            handed[0].next.as_deref(),
            Some("C:/pics/before-next.png")
        );

        // 只交一次：`load_memory` 取走之后就不再覆盖文件那份了
        // （否则下一次启动会莫名其妙地恢复一份过期记忆）。
        assert_eq!(manager.load_memory().len(), 1);
        assert!(manager.memory_override.lock().unwrap().is_none());
        assert!(
            manager.load_memory().is_empty(),
            "数据目录是空的，第二次应当什么都拿不到"
        );
    }
}

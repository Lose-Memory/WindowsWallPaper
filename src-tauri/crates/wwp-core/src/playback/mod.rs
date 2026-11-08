//! 播放：每块屏**各自**的换纸时刻与候选。
//!
//! ## 为什么不是"所有屏共用一个节拍"
//!
//! 原先这里是"所有屏共用同一个基准时刻 + 一次换齐、等全部完成"，
//! 目的是"两屏看起来同时切换，快的等一下慢的"。
//!
//! **这条要求已被用户取消**：两块屏现在可以各自暂停/继续，而"等齐"与
//! "独立暂停"是互相牵制的——一块屏被暂停后，另一个还在按节拍等它，
//! 用户看到的就是"我明明没停那块，它却跟着停了"。
//!
//! 现在：每块屏按自己的间隔决定下一次换纸的时刻，换纸时各自进行
//! （外壳为每块屏各起一个短命线程），**谁也不等谁**。
//! 间隔相同的两块屏仍然会落在相近的时刻——那是"它们间隔一样"的自然结果，
//! 不是被强行对齐的。
//!
//! ## 暂停分三层
//!
//! - **全局暂停**（[`Playback::pause`]）：所有屏都不换纸；
//! - **每屏手动暂停**（[`Playback::pause_screen`]）：只停那一块，另一块照常；
//! - **每屏自动暂停**（[`Playback::set_auto_paused`]）：这块屏上有程序全屏时
//!   由外壳自动按下，程序退出全屏后自动放开。
//!
//! 三者是"或"的关系：任一层暂停，这块屏这一轮就不换纸。
//!
//! ### 为什么"手动"与"自动"必须分开存
//!
//! 这是本模块里最容易被写错的一处。两个原因如果合成一个布尔标志：
//!
//! - 用户在玩游戏**之前**手动暂停了某块屏，游戏退出时自动恢复会把
//!   他的手动暂停一起解掉——他会看到"我没动它，它自己开始换了"；
//! - 反过来，手动"继续"也会把"正在全屏"这件事误当成已解除。
//!
//! 因此 [`ScreenRuntime`] 里存两个独立的原因位，对外报的 `paused`
//! 是两者的**或**，而界面上的按钮只反映**手动**那一位
//! （否则在全屏自动暂停时按钮会显示成"继续"，点它又什么都不会变）。
//!
//! ## 固定单图
//!
//! 某块屏设了固定单图（[`ScreenPlan::single_image`]）时它**不参与轮播**：
//! 不会到点、不会被"下一张/上一张"带上，只在启动/重建时应用一次。
//!
//! ## 这里不做任何壁纸还原
//!
//! 停止轮播只是"不再换纸"，不会把用户原来的壁纸设回去——那是用户明确
//! 要求删掉的行为。因此本模块里没有任何快照/还原的逻辑。

pub mod select;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::FitMode;
use crate::library::LibraryEntry;
use select::Selector;

/// 一块屏的播放计划：由设置解析出来的最终结果。
#[derive(Debug, Clone)]
pub struct ScreenPlan {
    /// 显示器标识（`\\.\DISPLAY1` 之类），用于与配置对应。
    pub monitor_name: String,
    /// 该屏的尺寸（换纸时要按这个尺寸准备图像）。
    pub width: u32,
    pub height: u32,
    pub interval: Duration,
    pub fit: FitMode,
    /// 该屏的候选条目（已经过筛选与排除）。
    ///
    /// **设了固定单图时这里是空的**：单图不轮播，不需要候选。
    pub candidates: Vec<LibraryEntry>,
    /// 固定单图（不轮播）。`None` 表示这块屏按候选轮播。
    ///
    /// 它优先于播放列表：设了它，这块屏就只显示这一张图。
    pub single_image: Option<std::path::PathBuf>,
}

impl ScreenPlan {
    /// 这块屏是不是"固定单图"（不轮播）。
    pub fn is_static_image(&self) -> bool {
        self.single_image.is_some()
    }

    /// 这块屏播放计划的指纹：只覆盖**会改变播放行为**的东西。
    ///
    /// 它有两个用处，而且是同一件事：
    ///
    /// - 外壳用它判断"设置改了，但播放行为其实没变"——那种情况下重建轮播
    ///   只会让壁纸白闪一次；
    /// - [`Playback::rebuild_with`] 用它判断"这块屏的播放列表到底变没变"，
    ///   没变的屏原地留用，因此不会被别的屏的改动连累着换一张。
    ///
    /// 候选集合的**内容**必须参与：加了几张图、改了筛选之后数量可能不变，
    /// 但候选不同了，必须算"变了"。
    ///
    /// **固定单图的路径也必须参与**：那种屏的候选本来就是空的，若只看候选，
    /// "换了一张单图"会被判成"什么都没变"，于是换了图屏幕上却还是旧的。
    ///
    /// `width` / `height` 故意**不**参与：显示器分辨率变了会走
    /// `PlaybackManager::start` 那条整批重建的路（见 `broadcast::on_display_change`），
    /// 而把它算进来只会让"两块屏分辨率不同"这种常驻差异扩大指纹的差异面。
    pub fn fingerprint(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for entry in &self.candidates {
            for byte in entry.p.as_bytes() {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!(
            "{}|{}|{:?}|{}|{}|{:016x}",
            self.monitor_name,
            self.interval.as_secs(),
            self.fit,
            self.candidates.len(),
            self.single_image
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            hash
        )
    }
}

/// 一块屏的当前状态，供界面展示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenState {
    pub monitor_name: String,
    /// 正在显示的图片路径。还没换过纸时为空。
    pub current: Option<String>,
    /// **下一张**要换上的是哪张源图（界面上的"即将切换"）。
    ///
    /// ## 为什么不只是"随便猜一张"
    ///
    /// 它来自 `Selector::peek_next`——与真正抽取用同一套算式算出的**同一张**
    /// （见 `select.rs` 的说明）。用户看到的"下一张"和实际换上的必须是同一张，
    /// 否则那个预览就是在骗人。
    ///
    /// 固定单图的屏、以及还没轮到任何候选的屏（轮播没启动、候选被筛空）是 `None`。
    pub next: Option<String>,
    /// 已经换过多少次。
    pub changes: u64,
    /// 最近一次换纸的耗时（毫秒）。
    ///
    /// ## 这个数字的含义（容易被误解，因此写清楚）
    ///
    /// 它是**准备位图 + 调用系统接口返回**的时间，**不是**"用户已经看到新图"
    /// 的时间。实测（`--test-wallpaper-latency`）：
    ///
    /// - API 调用本身只要 7~9 ms，加上准备位图后是 48~71 ms；
    /// - 但 `GetWallpaper` 要到约 **230 ms** 后才返回新路径——
    ///   Windows 需要先转码图片、再让桌面重绘。
    ///
    /// 也就是说"画面真的换过去"比这个数字晚一个数量级。
    pub last_change_millis: u64,
    /// 最近一次失败原因（成功为 None）。
    pub last_error: Option<String>,
    /// 候选数量。为 0 表示这块屏筛选后没有可用的图。
    pub candidate_count: usize,
    /// 距离下次换纸还有多少秒（估算）。暂停时显示整个间隔。
    pub next_in_seconds: u64,
    /// 这块屏是否被暂停（不影响别的屏）。
    ///
    /// 它是下面两位的**或**：用户手动暂停，或这块屏上有程序全屏而自动暂停。
    /// 界面要判断"该显示继续还是暂停"时**不要**用它，用手动那一位。
    pub paused: bool,
    /// 用户手动暂停（界面上的「暂停」按钮、每屏暂停命令）。
    pub paused_manual: bool,
    /// 因为这块屏上有程序全屏而**自动**暂停（见 [`Playback::set_auto_paused`]）。
    pub paused_by_fullscreen: bool,
}

/// 一块屏"上次退出时正在显示什么"的记忆。
///
/// ## 为什么要记这个
///
/// 用户 2026-10 的要求：退出程序或关机时把每块屏**当前**与**下一张**的路径记下来，
/// 下次启动先把当前那张放回去，并且**第一次切换必须换成上次预告的那张**
/// （不重新抽签）——否则用户看到的是"重启后壁纸变了，预告的那张也没兑现"。
///
/// 只管**路径**，不记剩余时间：重启后倒计时按配置的间隔从头算，这是用户
/// 明确要的行为（"不保存剩余播放时间"）。
///
/// 两个字段都可能为 `None`：还没换过纸（`current`）、候选为空（`next`）。
/// 文件还在不在**不在这里判断**——引擎看不见真实文件系统（测试用的候选路径
/// 本来就是假的），存在性检查由外壳负责，见 [`Playback::restore_memory`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenMemory {
    pub monitor_name: String,
    pub current: Option<String>,
    pub next: Option<String>,
}

/// 换纸通知：外壳收到后负责真正调用系统接口。
///
/// 引擎不直接碰壁纸接口，是为了让"每屏独立轮播"这件事可以脱离显示器与 COM 单测。
pub trait WallpaperSink: Send + Sync {
    /// 把某块屏换成给定文件。返回实际耗时（毫秒）或错误信息。
    fn apply(&self, monitor_name: &str, file: &std::path::Path) -> Result<u64, String>;

    /// 把某块屏设成"固定单图"（不轮播的那张）。
    ///
    /// 与 [`WallpaperSink::apply`] 的**唯一**区别是产物落哪：轮播每屏用两个
    /// 文件名轮换覆盖，而单图只需要**一份**固定缓存，换了源图就覆盖它
    /// （用户明确要求"只存放一份"）。因此这不是一个可以随便复用 `apply` 的地方。
    ///
    /// 默认实现退化成普通 `apply`（测试用的假 sink 用得上）。
    /// 真实实现（`engine::DesktopSink`）会覆盖它，写到 `-single.bmp`。
    fn apply_single(
        &self,
        monitor_name: &str,
        file: &std::path::Path,
        _fit: FitMode,
    ) -> Result<u64, String> {
        self.apply(monitor_name, file)
    }
}

/// 一次请求的接受/拒绝结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlOutcome {
    Applied,
    /// 该屏没有可用的图（筛选后为空）。
    NoCandidates,
    /// 找不到这块屏。
    UnknownScreen,
    /// 没有可回退的历史（"上一张"在随机模式下刚开头时会遇到）。
    /// 这是正常状态而不是错误，因此与 [`ControlOutcome::Failed`] 分开。
    NoHistory,
    /// 这块屏设了固定单图，不参与轮播——"下一张/上一张"对它没有意义。
    /// 同样不是错误（它显示的就是用户指定的那张图）。
    FixedImage,
    /// 播放器已停止。
    Stopped,
    Failed(String),
}

/// 换纸方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Backward,
    /// 固定单图：不走候选，直接把计划里指定的那张设上去。
    Static,
    /// 恢复上次退出时记下的那张（见 [`Playback::restore_memory`]）。
    ///
    /// 它不抽签：只是一次"把这张放回去"的动作。
    Remember,
}

/// 所有屏的播放状态。
///
/// **它自己不跑线程**：计时由外壳的调度线程驱动——到点调用
/// [`Playback::claim_due`] 取出"该换纸的屏"，再由外壳决定怎么执行它们
/// （现在是每块屏各一个线程，互不等待，见模块文档）。
pub struct Playback {
    screens: Vec<Arc<ScreenRuntime>>,
    /// 全局暂停。暂停时所有屏都不换纸，但各自的倒计时继续走。
    paused: Arc<AtomicBool>,
}

struct ScreenRuntime {
    plan: ScreenPlan,
    state: Mutex<ScreenState>,
    /// 选择器与它需要的历史。放在 Mutex 里因为调度线程与"手动下一张"都会用到。
    selector: Mutex<Selector>,
    /// 用户手动暂停。
    paused: AtomicBool,
    /// 因为这块屏上有程序全屏而自动暂停。
    ///
    /// 与 `paused` **必须分开存**：合成一个标志的话，"退出全屏"会把用户的
    /// 手动暂停一起解掉（详见模块文档里"为什么手动与自动必须分开存"）。
    paused_auto: AtomicBool,
    /// 下一次该换纸的时刻。
    next_due: Mutex<Instant>,
    /// 暂停期间**冻结的剩余时间**（`None` = 没在暂停）。
    ///
    /// 用户 2026-10 报的 bug：「每次当我某个屏幕的壁纸被全屏暂停播放后，退出全屏
    /// 以后，它会重新计时 100 秒开始下一张。」根因就在这里：
    ///
    /// - 暂停中，`claim_due` 每到点就把 `next_due` 往后推一个间隔（为了不换纸），
    ///   于是"剩余时间"其实一直在被重置；
    /// - 恢复时又调 `reset_due`（整段间隔）——**两处都在重置**，所以退出全屏后
    ///   必然要再等满一个间隔才换。
    ///
    /// 现在的语义是**暂停 = 计时停住**：进入暂停时把"当时还剩多久"记在这里，
    /// 暂停期间界面报告的就是这个数（不动），恢复时把 `next_due` 设成
    /// `now + 记下的剩余`，从原地接着往下算。
    hold: Mutex<Option<Duration>>,
    /// 重启后要**原样放回去**的那张（见 [`Playback::restore_memory`]）。
    ///
    /// 与 `selector.pin` 是两件事，缺一不可：
    ///
    /// - `pin` 管"**下一次**该换哪张"（抽签时优先取它）；
    /// - 这里管"**现在**屏幕上该是哪张"——它必须真的被应用一次，而不是绕过
    ///   抽签直接改状态。启动时若只把 `current` 写进状态而不碰壁纸，
    ///   屏幕上还是系统原来的壁纸，界面却报告"已经是这张了"。
    ///
    /// 取用一次就清掉（见 [`Playback::take_remembered`]），因此它不影响之后的抽取。
    remember: Mutex<Option<String>>,
}

impl ScreenRuntime {
    /// 按计划建一块全新的屏运行时：没有当前图、倒计时从 `now` 起算一整段间隔。
    fn new(plan: ScreenPlan, now: Instant) -> ScreenRuntime {
        let candidate_count = plan.candidates.len();
        let interval = plan.interval;
        ScreenRuntime {
            state: Mutex::new(ScreenState {
                monitor_name: plan.monitor_name.clone(),
                current: None,
                next: None,
                changes: 0,
                last_change_millis: 0,
                last_error: None,
                candidate_count,
                next_in_seconds: interval.as_secs(),
                paused: false,
                paused_manual: false,
                paused_by_fullscreen: false,
            }),
            selector: Mutex::new(Selector::new(plan.candidates.clone())),
            paused: AtomicBool::new(false),
            paused_auto: AtomicBool::new(false),
            next_due: Mutex::new(now + interval),
            // 一开始没有暂停，因此也没有"冻结的剩余时间"。
            hold: Mutex::new(None),
            remember: Mutex::new(None),
            plan,
        }
    }

    /// 现在是不是该换纸了。
    fn is_due(&self, now: Instant) -> bool {
        *self.next_due.lock().unwrap_or_else(|e| e.into_inner()) <= now
    }

    /// 把下次换纸时刻往后推一个间隔。
    ///
    /// 用 `due + interval` 而不是 `now + interval`：后者会让每一轮的换纸耗时
    /// 累积到节奏里，跑久了越来越偏。但若已经落后太多（比如机器睡了一觉），
    /// 就对齐到当前时刻，避免连续追赶地换好几次。
    fn advance_due(&self, now: Instant) {
        let mut due = self.next_due.lock().unwrap_or_else(|e| e.into_inner());
        let next = *due + self.plan.interval;
        *due = if next < now { now + self.plan.interval } else { next };
    }

    /// 手动操作之后重置倒计时：不该刚点完"下一张"就又被自动换一张。
    fn reset_due(&self, now: Instant) {
        *self.next_due.lock().unwrap_or_else(|e| e.into_inner()) = now + self.plan.interval;
        // 暂停中手动换了一张：恢复时该从**新的**整段间隔接着算，
        // 而不是回到暂停前那个剩余时间（用户刚换过，等满一个间隔才是他要的）。
        let mut hold = self.hold.lock().unwrap_or_else(|e| e.into_inner());
        if hold.is_some() {
            *hold = Some(self.plan.interval);
        }
    }

    /// 把"暂停时钟"与当前状态对齐。
    ///
    /// - 从"没暂停"变成"暂停"：记下此刻还剩多久（`hold`）；
    /// - 从"暂停"变成"没暂停"：把记下的剩余时间接回去（`next_due = now + 剩余`）；
    /// - 状态没变（例如暂停中又叠加了另一个暂停原因）：什么都不做——
    ///   否则游戏每切进切出都在动倒计时。
    fn sync_hold(&self, now: Instant, paused: bool) {
        let mut hold = self.hold.lock().unwrap_or_else(|e| e.into_inner());
        match (*hold, paused) {
            (None, true) => {
                let due = *self.next_due.lock().unwrap_or_else(|e| e.into_inner());
                *hold = Some(due.saturating_duration_since(now));
            }
            (Some(held), false) => {
                *self.next_due.lock().unwrap_or_else(|e| e.into_inner()) = now + held;
                *hold = None;
            }
            _ => {}
        }
    }

    /// 界面上"还有几秒换下一张"。
    ///
    /// 暂停中报告 `hold`（冻结的那个数）：这样"暂停时计时停住"在界面上也看得见，
    /// 而且它与恢复后接着算的是同一个数。
    fn next_in_seconds(&self, now: Instant) -> u64 {
        if let Some(held) = *self.hold.lock().unwrap_or_else(|e| e.into_inner()) {
            return held.as_secs();
        }
        self.next_due
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .saturating_duration_since(now)
            .as_secs()
    }

    /// 这块屏现在是否**有效暂停**（手动 / 全屏 / 全局三者之一）。
    fn is_effectively_paused(&self, global_paused: bool) -> bool {
        global_paused || self.is_screen_paused()
    }

    /// 改"手动暂停"那一位。`global_paused` 是全局暂停那一位的当前值。
    fn set_paused(&self, paused: bool, global_paused: bool) -> bool {
        let before = self.is_screen_paused();
        self.paused.store(paused, Ordering::SeqCst);
        self.sync_hold(Instant::now(), global_paused || self.is_screen_paused());
        self.is_screen_paused() != before
    }

    /// 改"全屏自动暂停"那一位。
    fn set_paused_by_fullscreen(&self, paused: bool, global_paused: bool) -> bool {
        let before = self.is_screen_paused();
        self.paused_auto.store(paused, Ordering::SeqCst);
        self.sync_hold(Instant::now(), global_paused || self.is_screen_paused());
        self.is_screen_paused() != before
    }

    fn is_screen_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst) || self.paused_auto.load(Ordering::SeqCst)
    }

    fn is_paused_by_fullscreen(&self) -> bool {
        self.paused_auto.load(Ordering::SeqCst)
    }

    fn is_manually_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// 候选数量。
    ///
    /// **不能读 `plan.candidates`**：用户删掉一张之后（「删除当前壁纸」）
    /// 那张图会从选择器里被移出去，而 `plan` 是只读的初始快照。
    /// 界面上显示的"候选 N 张"必须跟着变，否则删了几张之后数字还是旧的。
    fn candidate_count(&self) -> usize {
        self.selector
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// 把一张图移出候选（用户删掉了那张壁纸）。返回是否真的移掉了。
    fn forget_candidate(&self, path: &str) -> bool {
        let mut selector = self.selector.lock().unwrap_or_else(|e| e.into_inner());
        selector.forget(path)
    }

    /// **下一张**会是哪张源图（不消耗抽签，见 `Selector::peek_next`）。
    ///
    /// 固定单图的屏返回 `None`：它不轮播，"下一张"对它没有意义。
    fn peek_next(&self) -> Option<String> {
        if self.plan.is_static_image() {
            return None;
        }
        self.selector
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek_next()
            .map(|entry| entry.p.clone())
    }

    fn current(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .clone()
    }

    /// 用上次退出时的记忆恢复这块屏。返回"是否恢复成功"。
    ///
    /// ## 判据
    ///
    /// - **固定单图**的屏不参与：它显示的就是计划里那张，与记忆无关（用户要求
    ///   "固定单图按现有行为处理"）；
    /// - `current` **必须还在候选里**才认：用户可能已经把那张图挪出图库、
    ///   改了筛选条件，那样恢复它只会让屏幕停在一张不该再出现的图上，
    ///   此时如实返回 `false`，让外壳按现有候选规则重新选；
    /// - `next` 单独判断：它不在候选里就不钉，但**不因此否定整条记忆**——
    ///   当前那张仍然值得恢复。
    ///
    /// 因此 `can_restore` 说的是"**当前那张**能不能放回去"，而不是"记忆完不完整"。
    /// 调用方（外壳）已经先查过文件还在不在，这里只管候选成员资格。
    ///
    /// 恢复成功时倒计时按配置间隔**从头算**（用户明确要求不保存剩余时间）。
    fn restore_memory(&self, current: Option<&str>, next: Option<&str>) -> bool {
        if self.plan.is_static_image() {
            return false;
        }
        let Some(current) = current else {
            return false;
        };

        let mut selector = self.selector.lock().unwrap_or_else(|e| e.into_inner());
        if !selector.contains(current) {
            return false;
        }
        // 预告的那张也必须在候选里才钉得住；不在就让它照常随机（此处不报错）。
        if let Some(next) = next.filter(|path| selector.contains(path)) {
            selector.pin(next);
        }        drop(selector);

        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current = Some(current.to_string());
            // `changes` 从 0 起算：本次运行确实一次都没换过（用户要求"未启动轮播
            // 等场景按现有行为处理"，界面上的"已切换几次"说的是这一轮运行）。
            state.last_error = None;
        }
        *self.remember.lock().unwrap_or_else(|e| e.into_inner()) = Some(current.to_string());
        self.reset_due(Instant::now());
        log::info!(
            "恢复上次的壁纸 {} -> {}",
            self.plan.monitor_name,
            current
        );
        true
    }

    /// 取出"要原样放回去的那张"，取一次就没了。
    fn take_remembered(&self) -> Option<String> {
        self.remember
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

impl Playback {
    /// 按计划建立播放状态。此时**不换任何纸**，第一次由调用方决定
    /// （见 [`Playback::apply_all`] 与 [`Playback::claim_due`]）。
    pub fn new(plans: Vec<ScreenPlan>) -> Playback {
        // 每块屏的倒计时从"建立这一刻"起算，间隔不同的屏自然落在不同时刻上。
        // 间隔相同的两块屏会同时到点——那是设置的必然结果，不是被强行对齐的；
        // 而且两块屏各自在自己的线程里换纸，谁也不必等谁。
        let base = Instant::now();
        let screens = plans
            .into_iter()
            .map(|plan| Arc::new(ScreenRuntime::new(plan, base)))
            .collect();
        Playback {
            screens,
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 按新计划重建，但**播放计划没变的屏原地留用**；返回新 `Playback` 与
    /// **真的变了的**显示器名。
    ///
    /// ## 为什么要这样做（用户报的 bug）
    ///
    /// 用户 2026-10 的报告：「当我更改某一个显示器的播放列表时，两个显示器的
    /// 播放列表都会同时刷新。」根因在外壳：它把整个 `Playback` 换成新建的，
    /// 再对**所有**屏 `apply_all`，于是"改了 A 屏"必然连累 B 屏也换一张、
    /// 倒计时也重新起算。
    ///
    /// 现在按 [`ScreenPlan::fingerprint`] 逐屏比对：
    ///
    /// - 指纹相同的屏**搬走原来的运行时**（当前图、倒计时、随机历史、两个暂停位
    ///   全都保持）；
    /// - 指纹变了的屏换成全新的运行时，并把名字放进返回值，调用方只给这些屏
    ///   换纸（见 [`Playback::apply_screens`]）。
    ///
    /// 屏幕列表中消失的屏自然被丢掉（拔掉的显示器），新出现的屏算"变了"。
    pub fn rebuild_with(&self, plans: Vec<ScreenPlan>) -> (Playback, Vec<String>) {
        let now = Instant::now();
        let mut screens: Vec<Arc<ScreenRuntime>> = Vec::with_capacity(plans.len());
        let mut changed: Vec<String> = Vec::new();

        for plan in plans {
            let kept = self
                .find(&plan.monitor_name)
                .filter(|old| old.plan.fingerprint() == plan.fingerprint())
                .map(Arc::clone);
            match kept {
                Some(screen) => screens.push(screen),
                None => {
                    changed.push(plan.monitor_name.clone());
                    screens.push(Arc::new(ScreenRuntime::new(plan, now)));
                }
            }
        }

        (
            Playback {
                screens,
                // 全局暂停那一位跟人走：重建不该悄悄"继续"。
                paused: Arc::clone(&self.paused),
            },
            changed,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.screens.is_empty()
    }

    /// 当前各屏状态。
    pub fn states(&self) -> Vec<ScreenState> {
        let now = Instant::now();
        self.screens
            .iter()
            .map(|screen| {
                let mut state = screen
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                state.paused = screen.is_screen_paused();
                state.paused_manual = screen.is_manually_paused();
                state.paused_by_fullscreen = screen.is_paused_by_fullscreen();
                // 候选数按**当前**的选择器算（删过图之后要变小，见方法说明）。
                state.candidate_count = screen.candidate_count();
                // 「下一张」每轮重新算一次：删过图、回退过之后它都会跟着变，
                // 而它与真正换上的那张始终是同一张（见 `Selector::peek_next`）。
                state.next = screen.peek_next();
                // 暂停中这里是"冻结的剩余时间"（`hold`），与恢复后接着算的同一个数。
                state.next_in_seconds = screen.next_in_seconds(now);
                state
            })
            .collect()
    }

    /// 暂停全部轮播（当前壁纸保持不动）。
    ///
    /// 同时让每块屏**停表**（把剩余时间记进 `hold`）——用户 2026-10 的要求：
    /// 暂停期间倒计时停住，继续之后接着算，而不是重新计时一整段。
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        let now = Instant::now();
        for screen in &self.screens {
            // 全局暂停 → 每块屏都是"有效暂停"，无论它自己那两位是什么。
            screen.sync_hold(now, true);
        }
    }

    /// 继续轮播。每块屏的倒计时都**从暂停那一刻的剩余时间接着算**。
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        let now = Instant::now();
        for screen in &self.screens {
            // 单独暂停着的那块屏仍然是暂停状态，不该跟着全局继续一起走表。
            screen.sync_hold(now, screen.is_screen_paused());
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// 单独暂停/继续某一块屏（**用户手动**那一位）。返回是否找到了这块屏。
    ///
    /// 找到但状态没变（例如在"全屏自动暂停"期间点「继续」）也返回 `true`：
    /// 调用方问的是"这块屏存在吗"，不是"状态变了吗"。
    pub fn pause_screen(&self, monitor_name: &str, paused: bool) -> bool {
        match self.find(monitor_name) {
            Some(screen) => {
                if screen.set_paused(paused, self.is_paused()) {
                    log::info!(
                        "{} 已{}（只影响这块屏）",
                        screen.plan.monitor_name,
                        if paused { "暂停" } else { "继续" }
                    );
                }
                true
            }
            None => false,
        }
    }

    /// 由外壳调用：这块屏上有程序全屏（`paused = true`）或已经退出全屏。
    ///
    /// 返回 `Some(状态是否真的变了)`，找不到这块屏（或它是固定单图）时返回 `None`。
    /// **只在状态变化时记日志**：外壳每 500 ms 调用一次，无条件记日志会把
    /// 日志刷满，而这个项目的日志是事后排查的主要依据（release 没有控制台）。
    ///
    /// 固定单图的屏直接忽略：它本来就不轮播，"全屏时暂停"对它没有意义。
    pub fn set_auto_paused(&self, monitor_name: &str, paused: bool) -> Option<bool> {
        let screen = self.find(monitor_name)?;
        if screen.plan.is_static_image() {
            return None;
        }
        let changed = screen.set_paused_by_fullscreen(paused, self.is_paused());
        if changed {
            log::info!(
                "{} {}",
                screen.plan.monitor_name,
                if paused {
                    "检测到程序全屏，已自动暂停这块屏的轮播"
                } else {
                    "已退出全屏，恢复这块屏的轮播"
                }
            );
        }
        Some(changed)
    }

    /// 放开所有"全屏自动暂停"。开关被关掉时由外壳调用。
    ///
    /// 与 [`Playback::resume`] 的区别：那只管全局暂停，不碰每屏的手动/自动位。
    pub fn clear_auto_pauses(&self) {
        for screen in &self.screens {
            if screen.is_paused_by_fullscreen() {
                log::info!("{}：不再启用「全屏暂停」，恢复轮播", screen.plan.monitor_name);
                screen.set_paused_by_fullscreen(false, self.is_paused());
            }
        }
    }

    /// 距离下一次换纸还有多久（用于决定调度线程睡多久）。
    ///
    /// 固定单图的屏不参与——它们永远不会有"下一次换纸"。
    ///
    /// **暂停中的屏也不参与**：它们的 `next_due` 停在暂停那一刻（见 `hold`），
    /// 报出来会是 0，于是调度线程会以 0 超时反复空转。恢复走的是命令通道，
    /// 醒来时自然会重算。
    pub fn time_until_next(&self) -> Duration {
        let now = Instant::now();
        let global_paused = self.is_paused();
        self.screens
            .iter()
            .filter(|screen| {
                !screen.plan.is_static_image() && !screen.is_effectively_paused(global_paused)
            })
            .map(|screen| {
                screen
                    .next_due
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .saturating_duration_since(now)
            })
            .min()
            .unwrap_or(Duration::from_secs(3600))
    }

    /// 用上次退出时的记忆恢复各屏。返回**真的恢复了的**显示器名。
    ///
    /// ## 调用时机与语义
    ///
    /// 启动时（用户要求"下次启动后优先恢复退出时的当前壁纸"）先调这个：
    ///
    /// - 恢复成功、且记了 `next` 的屏：下一次自动切换**必定**换成记下的那张
    ///   （走 `selector.pin`，不重新抽签）；
    /// - 恢复失败的屏（那张图已经被移出候选、或本来就没记过）：**什么都不做**，
    ///   交给调用方按现有规则处理——通常是紧接着
    ///   [`Playback::apply_screens`] 给它换一张，与以前的行为一致；
    /// - 固定单图的屏不参与（用户要求"固定单图按现有行为处理"）。
    ///
    /// 恢复本身**不碰壁纸**：这里只把"该放回哪张"记进运行时，真正的
    /// `sink.apply` 由调用方随后的 [`Playback::claim_due`] 执行——换纸是慢操作，
    /// 启动路径上不能在这里等它（与"到点自动换纸谁也不等谁"同一个理由）。
    ///
    /// 倒计时在恢复的那一刻按配置间隔从头算：用户明确要求不保存剩余时间。
    pub fn restore_memory(&self, memory: &[ScreenMemory]) -> Vec<String> {
        let mut restored: Vec<String> = Vec::new();
        for recorded in memory {
            let Some(screen) = self.find(&recorded.monitor_name) else {
                continue;
            };
            if screen.restore_memory(recorded.current.as_deref(), recorded.next.as_deref()) {
                restored.push(screen.plan.monitor_name.clone());
            }
        }
        if !restored.is_empty() {
            log::info!("已恢复上次的壁纸：{}", restored.join("、"));
        }
        restored
    }

    /// 把"要原样放回去的那张"**立刻**设上去，并等齐。
    ///
    /// ## 与 `claim_due` 里那条支路的分工
    ///
    /// `claim_due` 也处理待放回的那张，但它只把任务**取出来**、由外壳异步执行，
    /// 而且要等调度线程下一次醒来（最多 500 ms）。这在启动时没问题——
    /// 桌面上本来就是上次那张。**退出 NSFW 模式时不行**：那一刻屏幕上盖着的是
    /// NSFW 壁纸，用户按下快捷键就该看到原来那张回来，不能先白等半秒。
    ///
    /// 因此这条路由启动路径（`PlaybackManager::spawn_scheduler`）在**建好调度线程
    /// 之前**自己调用：取出所有待放回的屏、并行设上去并等齐。取用一次就清
    /// （[`ScreenRuntime::take_remembered`]），因此随后 `claim_due` 不会重复设。
    ///
    /// 用户 2026-10 的要求："退出 NSFW 模式后，继续播放 NSFW 模式之前的壁纸，
    /// 而不是重新加载壁纸"——重新抽一张是 `Startup::ApplyNow` 干的事，这里不抽签。
    pub fn apply_remembered(&self, sink: &Arc<dyn WallpaperSink>) -> Vec<(String, ControlOutcome)> {
        let pending: Vec<(Arc<ScreenRuntime>, String)> = self
            .screens
            .iter()
            .filter(|screen| !screen.plan.is_static_image())
            .filter_map(|screen| {
                screen
                    .take_remembered()
                    .map(|path| (Arc::clone(screen), path))
            })
            .collect();
        if pending.is_empty() {
            return Vec::new();
        }
        let started = Instant::now();
        let results: Vec<(String, ControlOutcome)> = std::thread::scope(|scope| {
            let handles: Vec<_> = pending
                .into_iter()
                .map(|(screen, path)| {
                    let sink = Arc::clone(sink);
                    scope.spawn(move || {
                        let name = screen.plan.monitor_name.clone();
                        let outcome = advance(&screen, &sink, Direction::Remember, Some(&path));
                        (name, outcome)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        (
                            "?".to_string(),
                            ControlOutcome::Failed("换纸线程 panic".to_string()),
                        )
                    })
                })
                .collect()
        });
        log::info!(
            "已把 {} 块屏恢复到原来看的那张（共 {} ms）",
            results.len(),
            started.elapsed().as_millis()
        );
        results
    }

    /// 把所有屏的倒计时清零——下一轮 [`Playback::claim_due`] 会立刻换纸。
    /// 仅供测试使用。
    pub fn mark_all_due(&self) {
        let now = Instant::now();
        for screen in &self.screens {
            *screen.next_due.lock().unwrap_or_else(|e| e.into_inner()) = now;
        }
    }

    /// 立即给所有屏各换一张（启动时的第一张、以及"开始轮播"按钮）。
    ///
    /// 固定单图的屏在这里应用它那张图；其余屏各换一张。
    /// 并行执行并等齐：这是**手动/启动**动作，用户要的是"点完就都上屏"，
    /// 与"到点自动换纸时谁也不等谁"是两件事。
    pub fn apply_all(&self, sink: &Arc<dyn WallpaperSink>) -> Vec<(String, ControlOutcome)> {
        let mut results = self.run_parallel(
            self.with_candidates(),
            sink,
            Direction::Forward,
            |screen| screen.reset_due(Instant::now()),
        );
        results.extend(self.run_parallel(self.static_images(), sink, Direction::Static, |_| {}));
        results
    }

    /// 只给**列出的**这些屏各换一张，其余屏一动不动。
    ///
    /// 与 [`Playback::apply_all`] 的唯一区别就是这个筛选：用户改了 A 屏的播放列表时，
    /// B 屏不该跟着闪一下（那是用户 2026-10 报的 bug，根因见
    /// [`Playback::rebuild_with`]）。名字按大小写不敏感比对，与 [`Playback::find`] 一致。
    ///
    /// 并行等齐的语义同 `apply_all`：这是"用户刚改完，马上要看到效果"的手动动作。
    pub fn apply_screens(
        &self,
        monitor_names: &[String],
        sink: &Arc<dyn WallpaperSink>,
    ) -> Vec<(String, ControlOutcome)> {
        let wanted = |screen: &Arc<ScreenRuntime>| {
            monitor_names
                .iter()
                .any(|name| screen.plan.monitor_name.eq_ignore_ascii_case(name))
        };

        let rotating: Vec<Arc<ScreenRuntime>> = self
            .with_candidates()
            .into_iter()
            .filter(|screen| wanted(screen))
            .collect();
        let mut results = self.run_parallel(rotating, sink, Direction::Forward, |screen| {
            screen.reset_due(Instant::now())
        });

        // 变了的屏也可能从"轮播"变成"固定单图"：那张图同样要立刻上屏。
        let fixed: Vec<Arc<ScreenRuntime>> = self
            .static_images()
            .into_iter()
            .filter(|screen| wanted(screen))
            .collect();
        results.extend(self.run_parallel(fixed, sink, Direction::Static, |_| {}));
        results
    }

    /// 到点了：取出所有"该换纸的屏"。
    ///
    /// ## 为什么只"取出"，不在这里执行
    ///
    /// 换纸要解码，一块慢的屏能花几百毫秒。原先这里并行换、然后**等全部完成**，
    /// 于是慢的屏会把快的屏（以及调度线程）一起拖住——那条"同步"要求
    /// 已被用户取消。现在调用方拿到任务后自己安排执行（外壳是每块屏一个线程），
    /// 谁也等不到谁。
    ///
    /// 取出的同时就把倒计时推到下一个间隔：任务由调用方异步执行，
    /// 若等它做完再推，同一个间隔里可能被取出两次。
    pub fn claim_due(&self) -> Vec<ScreenTask> {
        let now = Instant::now();
        let global_paused = self.is_paused();

        let mut tasks: Vec<ScreenTask> = Vec::new();
        for screen in &self.screens {
            // 固定单图没有"到点"这回事，永不参与。
            if screen.plan.is_static_image() {
                continue;
            }
            if screen.is_effectively_paused(global_paused) {
                // 暂停中：**既不换纸，也不动倒计时**。
                //
                // 这里以前是 `advance_due(now)`（把到点时刻往后推一个间隔）——
                // 那正是"退出全屏还要再等 100 秒"的一半根因：暂停每到一个间隔
                // 就把剩余时间重置一次。现在剩余时间由 `hold` 记住，
                // `next_due` 原地不动，恢复时再接回去（见 `sync_hold`）。
                continue;
            }
            // "恢复上次的壁纸"要**立刻**做，不等倒计时到点：否则启动后屏幕上
            // 会先留着系统原来的壁纸，界面上却报告已经是记下的那张了。
            // 这种恢复不算一次用户可见的轮播，因此也不走 `advance_due`。
            if let Some(remembered) = screen.take_remembered() {
                tasks.push(ScreenTask {
                    runtime: Arc::clone(screen),
                    remember: Some(remembered),
                });
                continue;
            }
            if !screen.is_due(now) {
                continue;
            }
            screen.advance_due(now);
            tasks.push(ScreenTask {
                runtime: Arc::clone(screen),
                remember: None,
            });
        }
        tasks
    }

    /// 参与轮播的屏（有候选、且不是固定单图）。
    fn with_candidates(&self) -> Vec<Arc<ScreenRuntime>> {
        self.screens
            .iter()
            .filter(|screen| !screen.plan.is_static_image())
            .cloned()
            .collect()
    }

    /// 固定单图的屏。
    fn static_images(&self) -> Vec<Arc<ScreenRuntime>> {
        self.screens
            .iter()
            .filter(|screen| screen.plan.is_static_image())
            .cloned()
            .collect()
    }

    /// 并行执行一批换纸，等**全部**完成后再返回。
    ///
    /// 只用于手动/启动动作（"所有屏各换一张"、"启动时各上一张"）——
    /// 到点自动换纸走 [`Playback::claim_due`]，那条路上不等待。
    ///
    /// `after` 在每个屏换完之后调用（推进倒计时）。
    fn run_parallel<F>(
        &self,
        screens: Vec<Arc<ScreenRuntime>>,
        sink: &Arc<dyn WallpaperSink>,
        direction: Direction,
        after: F,
    ) -> Vec<(String, ControlOutcome)>
    where
        F: Fn(&ScreenRuntime) + Sync,
    {
        if screens.is_empty() {
            return Vec::new();
        }
        let started = Instant::now();
        let results: Vec<(String, ControlOutcome)> = std::thread::scope(|scope| {
            let handles: Vec<_> = screens
                .into_iter()
                .map(|screen| {
                    let sink = Arc::clone(sink);
                    let after = &after;
                    scope.spawn(move || {
                        let name = screen.plan.monitor_name.clone();
                        // 这条路上没有"放回记下的那张"：`Direction::Remember` 走的是
                        // `claim_due`（用户要求恢复发生在启动那一刻，不等倒计时）。
                        let outcome = advance(&screen, &sink, direction, None);
                        after(&screen);
                        (name, outcome)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        (
                            "?".to_string(),
                            ControlOutcome::Failed("换纸线程 panic".to_string()),
                        )
                    })
                })
                .collect()
        });
        if results.len() > 1 {
            log::debug!(
                "本次批量换纸 {} 块屏全部完成，共 {} ms",
                results.len(),
                started.elapsed().as_millis()
            );
        }
        results
    }

    /// 这块屏**现在显示的是哪张源图**（没显示过时返回 `None`）。
    ///
    /// 「删除当前壁纸」要先知道"当前是哪一张"才能把它移走，因此这里给一个
    /// 取值口。它以运行时状态为准，而外壳那份"最后设上去的记忆"
    /// （`PlaybackManager::shown`）在调度线程重建后才会为空——两者不冲突：
    /// 外壳只在真正要动手之前问一次。
    pub fn current_of(&self, monitor_name: &str) -> Option<String> {
        self.find(monitor_name)?.current()
    }

    /// 把一张图移出某块屏的候选（用户把它删掉了）。
    ///
    /// 返回是否真的移掉了（那张图本来就不在这块屏的候选里时为 `false`）。
    /// 界面上的候选数会在下一次 [`Playback::states`] 里自动变小。
    pub fn forget_candidate(&self, monitor_name: &str, path: &str) -> bool {
        match self.find(monitor_name) {
            Some(screen) => {
                let removed = screen.forget_candidate(path);
                if removed {
                    log::info!("{}：已把 {path} 移出候选（它已被删掉）", screen.plan.monitor_name);
                }
                removed
            }
            None => false,
        }
    }

    /// 立即在某块屏上换一张。
    pub fn next_on(&self, monitor_name: &str, sink: &Arc<dyn WallpaperSink>) -> ControlOutcome {
        let Some(screen) = self.find(monitor_name) else {
            return ControlOutcome::UnknownScreen;
        };
        if screen.plan.is_static_image() {
            return ControlOutcome::FixedImage;
        }
        let outcome = advance(screen, sink, Direction::Forward, None);
        screen.reset_due(Instant::now());
        outcome
    }

    /// 立即在某块屏上退回上一张。
    pub fn previous_on(&self, monitor_name: &str, sink: &Arc<dyn WallpaperSink>) -> ControlOutcome {
        let Some(screen) = self.find(monitor_name) else {
            return ControlOutcome::UnknownScreen;
        };
        if screen.plan.is_static_image() {
            return ControlOutcome::FixedImage;
        }
        let outcome = advance(screen, sink, Direction::Backward, None);
        screen.reset_due(Instant::now());
        outcome
    }

    /// 立即在所有屏上各换一张（并行等齐）。
    ///
    /// 固定单图的屏被跳过：它们显示的那张图只由设置决定，
    /// "下一张"对它们没有意义（返回它们只会让界面显示一条让人困惑的结果）。
    pub fn next_all(&self, sink: &Arc<dyn WallpaperSink>) -> Vec<(String, ControlOutcome)> {
        self.run_parallel(
            self.with_candidates(),
            sink,
            Direction::Forward,
            |screen| screen.reset_due(Instant::now()),
        )
    }

    /// 立即在所有屏上各退回一张（并行等齐）。固定单图的屏同样跳过。
    pub fn previous_all(&self, sink: &Arc<dyn WallpaperSink>) -> Vec<(String, ControlOutcome)> {
        // 只有真的有历史的屏才会换；其余返回 NoHistory 由界面提示。
        self.run_parallel(
            self.with_candidates(),
            sink,
            Direction::Backward,
            |screen| screen.reset_due(Instant::now()),
        )
    }

    fn find(&self, monitor_name: &str) -> Option<&Arc<ScreenRuntime>> {
        self.screens
            .iter()
            .find(|s| s.plan.monitor_name.eq_ignore_ascii_case(monitor_name))
    }
}

/// 一块"到点了、该换纸"的屏。
///
/// 由 [`Playback::claim_due`] 取出，调用方负责执行（外壳为每块屏各起一个线程）。
pub struct ScreenTask {
    runtime: Arc<ScreenRuntime>,
    /// 这次不是抽签，而是"把上次记下的这张放回去"（见 [`Playback::restore_memory`]）。
    remember: Option<String>,
}

impl ScreenTask {
    /// 这块屏的名字（给线程命名、给结果打标签）。
    pub fn monitor_name(&self) -> &str {
        &self.runtime.plan.monitor_name
    }

    /// 执行这次换纸，返回 `(显示器名, 结果)`。
    pub fn run(&self, sink: &Arc<dyn WallpaperSink>) -> (String, ControlOutcome) {
        let name = self.runtime.plan.monitor_name.clone();
        let direction = match &self.remember {
            Some(_) => Direction::Remember,
            None => Direction::Forward,
        };
        let outcome = advance(&self.runtime, sink, direction, self.remember.as_deref());
        (name, outcome)
    }
}

/// 换一张并更新状态。返回结果供调用方判断。
///
/// `remember` 只有 [`Direction::Remember`] 时才有值：那是"不抽签、直接放回这张"。
fn advance(
    runtime: &Arc<ScreenRuntime>,
    sink: &Arc<dyn WallpaperSink>,
    direction: Direction,
    remember: Option<&str>,
) -> ControlOutcome {
    if direction == Direction::Static {
        return apply_fixed_image(runtime, sink);
    }

    // 判据用**选择器**而不是 `plan.candidates`：用户删掉几张之后（「删除当前壁纸」）
    // 候选会变少，而 `plan` 是建立时的只读快照。删空的屏必须报"没有可用的图"，
    // 而不是继续去抽一张已经被移走的图。
    //
    // 恢复记忆这条路上它退化成"候选还在不在"：候选已经被筛空的屏，
    // 记下的那张自然也没得可放（外壳在恢复前还会再查一次文件在不在）。
    if runtime.candidate_count() == 0 {
        // 筛选后为空是用户会真实遇到的情况（比如"只看横图"但目录里全是竖图），
        // 必须明确报告，而不是静默保持旧图。
        return ControlOutcome::NoCandidates;
    }

    let entry = {
        // 恢复记忆时不碰抽签状态：`tick` 与"最近看过"名单都保持原样，
        // 因此紧接其后的那次自动切换仍然落在记下的那张上（`selector.pin` 定的）。
        if let Some(remembered) = remember {
            runtime
                .selector
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(remembered)
                .cloned()
        } else {
            let mut selector = runtime
                .selector
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match direction {
                Direction::Forward => selector.next().cloned(),
                Direction::Backward => selector.previous().cloned(),
                Direction::Static | Direction::Remember => unreachable!("上面已经分流"),
            }
        }
    };
    let Some(entry) = entry else {
        // 随机模式在历史不足时会走到这里（"上一张"没有可回退的目标）。
        // 这是一种正常状态，不是错误，因此单独报告而不是算作失败。
        return match direction {
            Direction::Backward => ControlOutcome::NoHistory,
            // 恢复的那张已经被移出候选（用户删掉了它），或这块屏根本没有候选：
            // 两种都按现有规则重新选，而不是让这块屏空着——与"文件已不存在时
            // 重新选择"是同一条要求。
            Direction::Forward | Direction::Remember => ControlOutcome::NoCandidates,
            Direction::Static => unreachable!("固定单图在上面已经分流"),
        };
    };

    // 文件可能在扫描之后被删掉或被移动。交给 sink 去报错，并记为 last_error。
    let source = std::path::PathBuf::from(&entry.p);
    match sink.apply(&runtime.plan.monitor_name, &source) {
        Ok(millis) => {
            let mut state = runtime.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current = Some(entry.p.clone());
            // 恢复不算"换过一张"：界面上的"已切换几次"说的是本次运行的轮播次数。
            if direction != Direction::Remember {
                state.changes += 1;
            }
            state.last_change_millis = millis;
            state.last_error = None;
            log::info!(
                "{} {} -> {}（{} ms）",
                match direction {
                    Direction::Forward => "换纸",
                    Direction::Backward => "退回",
                    Direction::Remember => "恢复",
                    Direction::Static => unreachable!("上面已经分流"),
                },
                runtime.plan.monitor_name,
                entry.p,
                millis
            );
            ControlOutcome::Applied
        }
        Err(error) => {
            let mut state = runtime.state.lock().unwrap_or_else(|e| e.into_inner());
            state.last_error = Some(error.clone());
            log::warn!(
                "{}失败 {} -> {}：{error}",
                match direction {
                    Direction::Remember => "恢复壁纸",
                    _ => "换纸",
                },
                runtime.plan.monitor_name,
                entry.p
            );
            ControlOutcome::Failed(error)
        }
    }
}

/// 应用"固定单图"：准备到固定产物（命中缓存则跳过解码）→ 设上去 → 更新状态。
///
/// 这块屏不轮播，因此它**永远只在"启动/重建"时被应用一次**，
/// 不会进 `claim_due` 的到点集合。
fn apply_fixed_image(
    runtime: &Arc<ScreenRuntime>,
    sink: &Arc<dyn WallpaperSink>,
) -> ControlOutcome {
    let Some(source) = runtime.plan.single_image.clone() else {
        return ControlOutcome::NoCandidates;
    };
    match sink.apply_single(
        &runtime.plan.monitor_name,
        &source,
        runtime.plan.fit,
    ) {
        Ok(millis) => {
            let mut state = runtime.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current = Some(source.display().to_string());
            state.changes += 1;
            state.last_change_millis = millis;
            state.last_error = None;
            log::info!(
                "固定单图 {} -> {}（{} ms）",
                runtime.plan.monitor_name,
                source.display(),
                millis
            );
            ControlOutcome::Applied
        }
        Err(error) => {
            let mut state = runtime.state.lock().unwrap_or_else(|e| e.into_inner());
            state.last_error = Some(error.clone());
            log::warn!(
                "固定单图应用失败 {} -> {}：{error}",
                runtime.plan.monitor_name,
                source.display()
            );
            ControlOutcome::Failed(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryEntry;
    use std::sync::atomic::AtomicU64;

    fn entry(path: &str, w: u32, h: u32) -> LibraryEntry {
        LibraryEntry {
            p: path.to_string(),
            w,
            h,
            mt: 1,
            sz: 1,
            or: 1,
            fmt: "png".to_string(),
            ok: 1,
            kind: crate::library::index::MediaKind::Image,
        }
    }

    /// 记录被调用过哪些文件的假 sink。
    struct RecordingSink {
        calls: Mutex<Vec<(String, std::path::PathBuf)>>,
        applied: AtomicU64,
        /// 让**某一块屏**的 apply 睡这么久，用来模拟"这块屏解码慢"。
        ///
        /// 按屏设而不是"所有屏一起慢"：这条链路上要验证的正是
        /// "一块慢的屏不该拖住另一块快的屏"。
        delays: Mutex<std::collections::HashMap<String, Duration>>,
    }

    impl RecordingSink {
        fn new() -> (Arc<RecordingSink>, Arc<dyn WallpaperSink>) {
            let concrete = Arc::new(RecordingSink {
                calls: Mutex::new(Vec::new()),
                applied: AtomicU64::new(0),
                delays: Mutex::new(std::collections::HashMap::new()),
            });
            let as_trait: Arc<dyn WallpaperSink> = concrete.clone();
            (concrete, as_trait)
        }

        fn applied_files(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(_, path)| path.display().to_string())
                .collect()
        }

        fn set_delay(&self, monitor: &str, delay: Duration) {
            self.delays
                .lock()
                .unwrap()
                .insert(monitor.to_string(), delay);
        }
    }

    impl WallpaperSink for RecordingSink {
        fn apply(&self, monitor: &str, file: &std::path::Path) -> Result<u64, String> {
            let delay = self
                .delays
                .lock()
                .unwrap()
                .get(monitor)
                .copied()
                .unwrap_or(Duration::ZERO);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            self.calls
                .lock()
                .unwrap()
                .push((monitor.to_string(), file.to_path_buf()));
            self.applied.fetch_add(1, Ordering::SeqCst);
            Ok(3)
        }
    }

    /// 走一遍"到点换纸"：取出到点的屏，每块屏各一个线程执行。
    ///
    /// 与外壳的做法一致（`playback_manager` 就是这么干的）。注意**等待发生在
    /// 这个测试助手和外层调用方里**，引擎的 `claim_due` 自己不等待。
    fn run_due(playback: &Playback, sink: &Arc<dyn WallpaperSink>) -> Vec<(String, ControlOutcome)> {
        let tasks = playback.claim_due();
        std::thread::scope(|scope| {
            let handles: Vec<_> = tasks
                .iter()
                .map(|task| scope.spawn(move || task.run(sink)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        })
    }

    fn plan(name: &str, interval_secs: u64, count: usize) -> ScreenPlan {
        ScreenPlan {
            monitor_name: name.to_string(),
            width: 800,
            height: 600,
            interval: Duration::from_secs(interval_secs),
            fit: FitMode::Fill,
            candidates: (0..count)
                .map(|i| entry(&format!("C:/pics/{name}_{i}.png"), 800, 600))
                .collect(),
            single_image: None,
        }
    }

    /// 一块"固定单图"的屏：没有候选，只有那一张图。
    fn static_plan(name: &str, image: &str) -> ScreenPlan {
        ScreenPlan {
            monitor_name: name.to_string(),
            width: 800,
            height: 600,
            interval: Duration::from_secs(100),
            fit: FitMode::Fill,
            candidates: Vec::new(),
            single_image: Some(std::path::PathBuf::from(image)),
        }
    }

    fn memory(monitor: &str, current: Option<&str>, next: Option<&str>) -> ScreenMemory {
        ScreenMemory {
            monitor_name: monitor.to_string(),
            current: current.map(str::to_string),
            next: next.map(str::to_string),
        }
    }

    #[test]
    fn restoring_puts_the_recorded_current_wallpaper_on_screen() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);
        let wanted = "C:/pics/screen-a_2.png";

        let restored = playback.restore_memory(&[memory("screen-a", Some(wanted), None)]);
        assert_eq!(restored, vec!["screen-a".to_string()]);

        // 登记之后 state 上就已经是它了：界面不必等到真正换完才显示对。
        let restored_state = state_of(&playback, "screen-a");
        assert_eq!(restored_state.current.as_deref(), Some(wanted));

        // 真正的换纸发生在紧接着的那一次 `claim_due`：**立刻**，不等倒计时。
        // （`claim_due` 的 `now` 是它自己取的时刻，而 `restore_memory` 刚刚把倒计时
        // 重置成"一整个间隔之后"，因此这里要让它先过去一瞬，才是在测"立刻"。
        // 外壳里两者天然隔着一轮循环——管理器线程每轮至少等 500 ms。）
        std::thread::sleep(Duration::from_millis(1));
        let outcomes = run_due(&playback, &sink_trait);
        assert_eq!(outcomes, vec![("screen-a".to_string(), ControlOutcome::Applied)]);
        assert_eq!(sink.applied_files(), vec![wanted.to_string()]);

        // 恢复不算"切换"：次数必须还是 0，否则界面上的计数会凭空多一次。
        let after = state_of(&playback, "screen-a");
        assert_eq!(after.changes, 0, "恢复不是切换，计数器不该动");
        assert_eq!(after.current.as_deref(), Some(wanted));
    }

    #[test]
    fn restoring_pins_the_recorded_next_wallpaper_instead_of_drawing_a_new_one() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 1, 4)]);
        let current = "C:/pics/screen-a_0.png";
        let recorded_next = "C:/pics/screen-a_3.png";

        playback.restore_memory(&[memory("screen-a", Some(current), Some(recorded_next))]);

        // 预告的下一张就是退出时记下的那张——不重新抽取。
        let state = state_of(&playback, "screen-a");
        assert_eq!(state.next.as_deref(), Some(recorded_next));

        // 第一批是"把当前那张原样放回去"，不算切换。
        std::thread::sleep(Duration::from_millis(1));
        let first = run_due(&playback, &sink_trait);
        assert_eq!(first, vec![("screen-a".to_string(), ControlOutcome::Applied)]);
        assert_eq!(sink.applied_files(), vec![current.to_string()]);
        assert_eq!(state_of(&playback, "screen-a").changes, 0);

        // 第二次才是"第一次自动切换"：必须落在退出时记下的那张上，而不是
        // 照现在的抽签状态重新抽一张。这块屏的间隔只有 1 s，等它到点。
        std::thread::sleep(Duration::from_millis(1100));
        let second = run_due(&playback, &sink_trait);
        assert_eq!(second, vec![("screen-a".to_string(), ControlOutcome::Applied)]);
        assert_eq!(
            sink.applied_files(),
            vec![current.to_string(), recorded_next.to_string()],
            "第一次自动切换用的必须正是退出时记录的下一张"
        );
        assert_eq!(
            state_of(&playback, "screen-a").current.as_deref(),
            Some(recorded_next)
        );
    }

    #[test]
    fn a_recalled_wallpaper_that_left_the_candidates_is_not_restored() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);

        // 这张图已经不在候选里了（被移出图库、或换了播放列表）。
        let restored = playback.restore_memory(&[memory(
            "screen-a",
            Some("C:/gone/not-a-candidate.png"),
            None,
        )]);
        assert!(restored.is_empty(), "不在候选里的记录不能被当成恢复成功");
        assert!(state_of(&playback, "screen-a").current.is_none());

        // 也没留下"待补一次"的痕迹：下一次 `claim_due` 什么都没干。
        assert!(run_due(&playback, &sink_trait).is_empty());
        assert!(sink.applied_files().is_empty());
    }

    #[test]
    fn a_next_that_left_the_candidates_is_dropped_but_the_current_is_kept() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);
        let current = "C:/pics/screen-a_1.png";

        let restored = playback.restore_memory(&[memory(
            "screen-a",
            Some(current),
            Some("C:/gone/not-a-candidate.png"),
        )]);
        assert_eq!(restored, vec!["screen-a".to_string()]);
        assert_eq!(
            state_of(&playback, "screen-a").current.as_deref(),
            Some(current)
        );

        // 恢复这一批先把当前那张放回去。
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(
            run_due(&playback, &sink_trait),
            vec![("screen-a".to_string(), ControlOutcome::Applied)]
        );
        assert_eq!(sink.applied_files(), vec![current.to_string()]);

        // 记下的"下一张"不在候选里，于是**没有钉住任何东西**：这块屏回到
        // 正常抽取，第一次自动切换照常发生，落点是当时预告的那张。
        let predicted = state_of(&playback, "screen-a")
            .next
            .expect("候选还在，就该照常预告下一张");
        assert_ne!(predicted, "C:/gone/not-a-candidate.png");

        playback.mark_all_due();
        run_due(&playback, &sink_trait);
        assert_eq!(
            state_of(&playback, "screen-a").current.as_deref(),
            Some(predicted.as_str()),
            "下一次切换必须落在预告的那张上"
        );
    }

    #[test]
    fn a_screen_that_is_not_in_the_memory_keeps_its_normal_behaviour() {
        let playback = Playback::new(vec![plan("screen-a", 3600, 3), plan("screen-b", 3600, 3)]);

        // 只有 A 有记录：B 必须原样，不能因为"没记录"就被当成空记录。
        let restored = playback.restore_memory(&[
            memory("screen-a", Some("C:/pics/screen-a_0.png"), None),
            memory("screen-c", Some("C:/pics/screen-c_0.png"), None),
        ]);
        assert_eq!(restored, vec!["screen-a".to_string()]);
        assert!(state_of(&playback, "screen-b").current.is_none());
    }

    #[test]
    fn a_fixed_image_screen_is_never_restored() {
        let playback = Playback::new(vec![static_plan("screen-a", "C:/fixed/one.png")]);

        // 固定单图没有候选，"恢复"对它没有意义：它本来就一直是那一张。
        let restored = playback.restore_memory(&[memory(
            "screen-a",
            Some("C:/fixed/one.png"),
            None,
        )]);
        assert!(restored.is_empty());
        assert!(state_of(&playback, "screen-a").current.is_none());

        // 连"待补一次"的痕迹都没留下：claim_due 什么都不做。
        assert!(playback.claim_due().is_empty());
    }

    #[test]
    fn restoring_matches_the_monitor_name_regardless_of_case() {
        let playback = Playback::new(vec![plan("Screen-A", 3600, 2)]);

        let restored = playback.restore_memory(&[memory(
            "screen-a",
            Some("C:/pics/Screen-A_0.png"),
            None,
        )]);
        assert_eq!(restored, vec!["Screen-A".to_string()]);
    }

    #[test]
    fn a_restored_screen_still_switches_on_schedule() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);

        playback.restore_memory(&[memory("screen-a", Some("C:/pics/screen-a_0.png"), None)]);
        run_due(&playback, &sink_trait);

        // 恢复把倒计时重新起算：间隔 3600 s，刚恢复完不可能到点。
        let state = state_of(&playback, "screen-a");
        assert!(
            state.next_in_seconds > 3500,
            "恢复之后倒计时应从整段间隔重新起算，实际 {}",
            state.next_in_seconds
        );
    }

    #[test]
    fn apply_remembered_puts_the_screen_back_without_waiting_for_the_timer() {
        // **退出 NSFW 模式走的就是这条路**（用户 2026-10 的要求："退出 NSFW 模式后，
        // 继续播放 NSFW 模式之前的壁纸，而不是重新加载壁纸"）。与启动那条路
        // （`restore_memory` + `claim_due`）的区别只有一处：这里**当场**设上去，
        // 不等调度线程那一轮（最多 500 ms）——那时屏幕上还盖着 NSFW 壁纸。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 4)]);
        let current = "C:/pics/screen-a_1.png";
        let next = "C:/pics/screen-a_3.png";

        playback.restore_memory(&[memory("screen-a", Some(current), Some(next))]);
        assert_eq!(
            playback.apply_remembered(&sink_trait),
            vec![("screen-a".to_string(), ControlOutcome::Applied)]
        );
        assert_eq!(sink.applied_files(), vec![current.to_string()]);

        // 恢复不是"切换"：界面上的次数一个都不该动。
        assert_eq!(state_of(&playback, "screen-a").changes, 0);

        // 已经设过了：紧接着的 `claim_due` 不该再来一次（否则这块屏被设两遍）。
        assert!(run_due(&playback, &sink_trait).is_empty());
        assert_eq!(sink.applied_files().len(), 1);

        // 第一次自动切换仍然落在记下的那张上（`pin` 没有被这里消费掉）。
        playback.mark_all_due();
        assert_eq!(
            run_due(&playback, &sink_trait),
            vec![("screen-a".to_string(), ControlOutcome::Applied)]
        );
        assert_eq!(state_of(&playback, "screen-a").current.as_deref(), Some(next));
    }

    #[test]
    fn apply_remembered_does_nothing_when_there_is_nothing_to_put_back() {
        // 没有记忆（或记的那张已经不在候选里）时**什么都不做**：退出 NSFW 模式那条路
        // 紧接着会用 `apply_screens` 给这些屏换一张，这里抢着抽签只会白解码一次。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);

        assert!(playback.apply_remembered(&sink_trait).is_empty());
        assert!(sink.applied_files().is_empty());

        playback.restore_memory(&[memory("screen-a", Some("C:/gone/x.png"), None)]);
        assert!(playback.apply_remembered(&sink_trait).is_empty());
        assert!(sink.applied_files().is_empty());
    }

    #[test]
    fn next_on_applies_once_and_updates_state() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);

        let outcome = playback.next_on("screen-a", &sink_trait);
        assert_eq!(outcome, ControlOutcome::Applied);
        assert_eq!(sink.applied_files().len(), 1);

        let states = playback.states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].changes, 1);
        assert_eq!(states[0].candidate_count, 3);
        assert!(states[0].current.is_some());
        assert!(states[0].last_error.is_none());
    }

    #[test]
    fn the_state_predicts_exactly_the_next_wallpaper_to_be_applied() {
        // **这条守的是播放页上那格"下一张"预览。** 状态里报的 `next` 必须就是
        // 随后真正换上的那一张——用户照着预览等，等来的就得是它。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 10)]);

        for round in 0..6 {
            let predicted = state_of(&playback, "screen-a")
                .next
                .unwrap_or_else(|| panic!("第 {} 轮：状态里应当有下一张", round + 1));
            assert_eq!(
                playback.next_on("screen-a", &sink_trait),
                ControlOutcome::Applied
            );
            let applied = sink.applied_files().pop().expect("刚换过就该有记录");
            assert_eq!(
                applied, predicted,
                "第 {} 轮：换上的不是状态里预告的那张",
                round + 1
            );
        }
    }

    #[test]
    fn a_fixed_image_screen_has_no_next_wallpaper() {
        // 固定单图的屏不轮播，"下一张"对它没有意义，界面据此显示"固定单图"。
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![static_plan("screen-a", "C:/pics/only.png")]);
        playback.apply_all(&sink_trait);
        let state = state_of(&playback, "screen-a");
        assert!(state.current.is_some(), "单图应当已经设上去了");
        assert!(state.next.is_none(), "固定单图不该有下一张");
    }

    #[test]
    fn deleting_the_current_wallpaper_switches_to_the_previewed_next_one() {
        // **用户 2026-10 报的场景，走完整条链**（界面上的「删除当前壁纸」就是
        // 先 `forget_candidate` 再 `next_on`）：他点删除之后，换上来的不是页面上
        // 显示的那张「下一张」，而是重新生成了另一张。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 12)]);

        // 先换一张，让"当前显示"有值。
        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::Applied
        );
        let current = state_of(&playback, "screen-a").current.expect("刚换过");
        let previewed = state_of(&playback, "screen-a").next.expect("应当有下一张");
        assert_ne!(current, previewed, "前提：预告的不是当前这张");

        // 删除当前这张：移出候选，然后立刻换下一张。
        assert!(playback.forget_candidate("screen-a", &current));
        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::Applied
        );

        let applied = sink.applied_files().pop().expect("刚换过就该有记录");
        assert_eq!(
            applied, previewed,
            "删掉当前这张之后，换上的必须是页面上预告的那一张"
        );
        // 删掉的那张也不能再被预告（否则下一次换纸会去打开一个不存在的文件）。
        let after = state_of(&playback, "screen-a").next.expect("应当还有下一张");
        assert_ne!(after, current, "被删掉的那张不该又出现在预告里");
        assert_eq!(
            state_of(&playback, "screen-a").candidate_count,
            11,
            "候选数要跟着少一张"
        );
    }

    #[test]
    fn unknown_screen_is_reported_not_silently_ignored() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 2)]);
        assert_eq!(
            playback.next_on("screen-does-not-exist", &sink_trait),
            ControlOutcome::UnknownScreen
        );
        assert!(sink.applied_files().is_empty());
    }

    #[test]
    fn empty_candidates_are_reported_rather_than_keeping_a_stale_wallpaper() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 0)]);
        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::NoCandidates
        );
        let states = playback.states();
        assert_eq!(states[0].candidate_count, 0);
        assert!(states[0].current.is_none());
    }

    #[test]
    fn apply_all_gives_every_screen_a_wallpaper_without_waiting() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 3600, 2),
            plan("screen-b", 3600, 2),
        ]);
        let results = playback.apply_all(&sink_trait);
        assert_eq!(results.len(), 2, "两块屏都要换");
        assert_eq!(sink.applied_files().len(), 2);
        let states = playback.states();
        assert!(states.iter().all(|s| s.current.is_some()));
    }

    #[test]
    fn both_screens_are_claimed_in_one_pass() {
        // 到点时两块屏都会被取出来（间隔一样就落在相近的时刻）。
        // 注意这里只断言"取出来了"，不管它们怎么执行——执行方式是调用方的事。
        let (_sink, _sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);
        playback.mark_all_due();
        let tasks = playback.claim_due();
        assert_eq!(tasks.len(), 2, "两块到点的屏都该被取出来");

        let mut names: Vec<String> = tasks.iter().map(|t| t.monitor_name().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["screen-a", "screen-b"]);
    }

    #[test]
    fn claiming_due_screens_does_not_wait_for_any_of_them() {
        // **这条守的是"取消同步"**：`claim_due` 只是把任务取出来，
        // 自己不做任何换纸工作。若它内部又"并行换、等齐"，那块慢的屏
        // 就会把快的屏和调度线程一起拖住——那正是被取消的行为。
        let (sink, _sink_trait) = RecordingSink::new();
        sink.set_delay("screen-a", Duration::from_millis(150));
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);
        playback.mark_all_due();

        let started = Instant::now();
        let tasks = playback.claim_due();
        let elapsed = started.elapsed();
        assert_eq!(tasks.len(), 2);
        assert!(
            elapsed < Duration::from_millis(50),
            "取出任务不该阻塞（哪怕有慢屏），实际花了 {elapsed:?}"
        );
        assert!(
            sink.applied_files().is_empty(),
            "claim_due 阶段不该已经换过纸"
        );
    }

    #[test]
    fn a_slow_screen_does_not_hold_up_the_fast_one() {
        // **这条守的是"谁也不等谁"**：一块屏解码慢（150 ms）时，
        // 另一块不该跟着等——它做完就该结束。
        let (sink, sink_trait) = RecordingSink::new();
        sink.set_delay("screen-b", Duration::from_millis(150));

        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);
        playback.mark_all_due();
        let tasks = playback.claim_due();
        assert_eq!(tasks.len(), 2);

        // 每块屏各一个线程，与外壳的做法一致；分别记录各自的耗时。
        let elapsed: Vec<(String, Duration, ControlOutcome)> = std::thread::scope(|scope| {
            let handles: Vec<_> = tasks
                .iter()
                .map(|task| {
                    let sink = Arc::clone(&sink_trait);
                    let name = task.monitor_name().to_string();
                    (name, scope.spawn(move || {
                        let started = Instant::now();
                        // 名字由上面单独记了，这里只要结果。
                        let (_, outcome) = task.run(&sink);
                        (started.elapsed(), outcome)
                    }))
                })
                .collect();
            handles
                .into_iter()
                .map(|(name, handle)| {
                    let (spent, outcome) = handle.join().unwrap();
                    (name, spent, outcome)
                })
                .collect()
        });

        let fast = elapsed.iter().find(|(name, _, _)| name == "screen-a").unwrap();
        let slow = elapsed.iter().find(|(name, _, _)| name == "screen-b").unwrap();
        assert_eq!(fast.2, ControlOutcome::Applied);
        assert_eq!(slow.2, ControlOutcome::Applied);
        assert!(
            fast.1 < slow.1,
            "快的屏不该等慢的屏：快 {}，慢 {}",
            fast.1.as_millis(),
            slow.1.as_millis()
        );
        assert!(
            fast.1 < Duration::from_millis(100),
            "快的那块应当在慢屏睡着的时候就结束了，实际 {:?}",
            fast.1
        );
        // 两块屏各换一次（并行，不是串行两次）。
        assert_eq!(sink.applied_files().len(), 2);
    }

    #[test]
    fn a_paused_screen_is_skipped_while_the_other_keeps_rotating() {
        // **每屏独立暂停**：只停一块，另一块照常。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);

        assert!(playback.pause_screen("screen-a", true));
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 1, "只有未暂停的那块该换：{results:?}");
        assert_eq!(results[0].0, "screen-b");

        // 状态里也要如实反映"这块屏被单独暂停了"。
        let states = playback.states();
        let a = states.iter().find(|s| s.monitor_name == "screen-a").unwrap();
        let b = states.iter().find(|s| s.monitor_name == "screen-b").unwrap();
        assert!(a.paused && !b.paused);

        // 继续之后又能换。
        playback.pause_screen("screen-a", false);
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 2, "继续之后两块都该换：{results:?}");
        assert_eq!(sink.applied_files().len(), 3);
    }

    /// 取某块屏的状态（测试里到处要用，抽出来免得每处都写一遍 find）。
    fn state_of(playback: &Playback, name: &str) -> ScreenState {
        playback
            .states()
            .into_iter()
            .find(|state| state.monitor_name == name)
            .unwrap_or_else(|| panic!("没有这块屏：{name}"))
    }

    #[test]
    fn fullscreen_pause_stops_only_the_busy_screen() {
        // **用户要的正是这条**：一块屏上有程序全屏就停那一块，另一块照常。
        // 上一版用"前台窗口"做判据，这个场景根本无法表达。
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);

        assert_eq!(playback.set_auto_paused("screen-a", true), Some(true));
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 1, "只有没全屏的那块该换：{results:?}");
        assert_eq!(results[0].0, "screen-b");

        let a = state_of(&playback, "screen-a");
        let b = state_of(&playback, "screen-b");
        assert!(a.paused && a.paused_by_fullscreen && !a.paused_manual);
        assert!(!b.paused && !b.paused_by_fullscreen);
    }

    #[test]
    fn exiting_fullscreen_resumes_without_an_immediate_change() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);

        playback.set_auto_paused("screen-a", true);
        playback.mark_all_due();
        assert_eq!(run_due(&playback, &sink_trait).len(), 1);

        assert_eq!(playback.set_auto_paused("screen-a", false), Some(true));
        assert!(
            playback.claim_due().is_empty(),
            "刚退出全屏不该立刻换一张——倒计时要重新开始"
        );
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 2, "恢复之后两块都该换：{results:?}");
    }

    #[test]
    fn the_two_pause_reasons_do_not_cancel_each_other() {
        // **本模块里最容易写错的一处。**两个暂停原因合成一个布尔标志的话：
        // 退出全屏会解掉用户的手动暂停，手动"继续"也会解掉全屏自动暂停。
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);

        // 一、用户先手动暂停，之后出现又退出全屏：手动暂停必须留着。
        assert!(playback.pause_screen("screen-a", true));
        playback.set_auto_paused("screen-a", true);
        playback.set_auto_paused("screen-a", false);
        let a = state_of(&playback, "screen-a");
        assert!(a.paused, "退出全屏不该解掉用户的手动暂停");
        assert!(a.paused_manual && !a.paused_by_fullscreen);

        // 二、全屏自动暂停期间用户点「继续」：自动暂停必须留着。
        playback.set_auto_paused("screen-a", true);
        playback.pause_screen("screen-a", false);
        let a = state_of(&playback, "screen-a");
        assert!(a.paused, "手动「继续」不该解掉全屏自动暂停");
        assert!(!a.paused_manual && a.paused_by_fullscreen);

        // 三、两个原因都放开，才真的恢复轮播。
        playback.pause_screen("screen-a", false);
        playback.set_auto_paused("screen-a", false);
        assert!(!state_of(&playback, "screen-a").paused);
        let (_sink, sink_trait) = RecordingSink::new();
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 2, "两个原因都放开后两块都该换：{results:?}");
    }

    #[test]
    fn auto_pause_reports_a_change_only_when_the_effective_state_changes() {
        // 外壳每 500 ms 调一次，靠这个返回值决定要不要记日志。
        let playback = Playback::new(vec![plan("screen-a", 100, 3)]);
        assert_eq!(playback.set_auto_paused("screen-a", true), Some(true));
        assert_eq!(
            playback.set_auto_paused("screen-a", true),
            Some(false),
            "状态没变时不该再报变化（否则日志会被刷满）"
        );
        assert_eq!(playback.set_auto_paused("screen-a", false), Some(true));
        assert_eq!(
            playback.set_auto_paused("不存在的屏", true),
            None,
            "找不到这块屏"
        );
    }

    #[test]
    fn a_fixed_image_screen_ignores_the_fullscreen_pause() {
        // 固定单图本来就不轮播，"全屏时暂停"对它没有意义，
        // 也不该在界面上显示成"被暂停了"。
        let playback = Playback::new(vec![static_plan("screen-a", "C:/pics/only.png")]);
        assert_eq!(playback.set_auto_paused("screen-a", true), None);
        let state = state_of(&playback, "screen-a");
        assert!(!state.paused && !state.paused_by_fullscreen);
    }

    #[test]
    fn clearing_auto_pauses_leaves_manual_ones_alone() {
        // 用户在设置页把开关关掉时走这条：只放开"因为全屏"而停的那几块。
        let playback = Playback::new(vec![
            plan("screen-a", 100, 3),
            plan("screen-b", 100, 3),
        ]);
        playback.pause_screen("screen-a", true);
        playback.set_auto_paused("screen-a", true);
        playback.set_auto_paused("screen-b", true);

        playback.clear_auto_pauses();

        let a = state_of(&playback, "screen-a");
        let b = state_of(&playback, "screen-b");
        assert!(
            a.paused && a.paused_manual && !a.paused_by_fullscreen,
            "手动暂停必须留着"
        );
        assert!(!b.paused, "只有自动暂停的那块屏应当被放开");
    }

    /// 把某块屏"下一次该换纸"的时刻直接摆到指定位置。
    ///
    /// 单元测试推进不了时钟，因此时间相关的判据都靠这个助手把 `due` 摆好
    /// （`mark_all_due` 只能摆到"现在"）。
    fn set_due(playback: &Playback, name: &str, due: Instant) {
        let screen = playback
            .screens
            .iter()
            .find(|s| s.plan.monitor_name == name)
            .unwrap_or_else(|| panic!("应当有 {name} 这块屏"));
        *screen.next_due.lock().unwrap() = due;
    }

    /// 某块屏"暂停期间冻住的剩余时间"（没在暂停时为 `None`）。
    fn held(playback: &Playback, name: &str) -> Option<Duration> {
        let screen = playback
            .screens
            .iter()
            .find(|s| s.plan.monitor_name == name)
            .unwrap_or_else(|| panic!("应当有 {name} 这块屏"));
        *screen.hold.lock().unwrap()
    }

    /// 冻住的剩余秒数（**四舍五入**：`40 秒` 记下来是 `39.999…`，
    /// 直接 `as_secs()` 会被截成 39，判据就假失败了）。
    fn held_secs(playback: &Playback, name: &str) -> Option<u64> {
        held(playback, name).map(|d| d.as_secs_f64().round() as u64)
    }

    /// 用户 2026-10 报的 bug：全屏暂停之后退出全屏，倒计时**重新计时一整段**。
    ///
    /// 这条测试钉的是新的语义：暂停 = 计时停住（界面报的数冻结、`due` 不动），
    /// 恢复 = 从冻住的那个剩余时间接着算。
    #[test]
    fn pausing_freezes_the_countdown_instead_of_restarting_it() {
        let (_sink, _sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 100, 3)]);
        set_due(&playback, "screen-a", Instant::now() + Duration::from_secs(60));

        playback.pause();
        let frozen = state_of(&playback, "screen-a").next_in_seconds;
        assert!(
            (59..=60).contains(&frozen),
            "暂停时应当报告冻结的剩余时间，实际 {frozen}"
        );

        // 暂停期间时间往前走（这里直接把 due 甩到 5 分钟前）：界面报的仍是那个数，
        // 也不该换纸。
        set_due(&playback, "screen-a", Instant::now() - Duration::from_secs(300));
        assert_eq!(
            state_of(&playback, "screen-a").next_in_seconds,
            frozen,
            "暂停中倒计时不该跟着时间走"
        );
        assert!(playback.claim_due().is_empty(), "暂停中不该换纸");
        // 暂停中的屏必须从 `time_until_next` 里排除掉：它的 due 停在过去，
        // 报 0 会让调度线程以 0 超时反复空转（`next_due` 不再被推进了）。
        assert!(
            playback.time_until_next() > Duration::from_secs(600),
            "暂停中的屏不该让调度线程空转"
        );

        playback.resume();
        let wait = playback.time_until_next();
        assert!(
            wait > Duration::from_secs(55) && wait <= Duration::from_secs(61),
            "恢复后应当接着算剩下的时间，而不是重新计时一整段 100 秒：{wait:?}"
        );
        assert!(
            playback.claim_due().is_empty(),
            "剩下的 60 秒还没到，不该立刻换纸"
        );
    }

    /// 用户实际遇到的那条路：全屏自动暂停 → 退出全屏。
    #[test]
    fn leaving_fullscreen_does_not_restart_the_whole_interval() {
        let playback = Playback::new(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        set_due(&playback, "screen-a", Instant::now() + Duration::from_secs(30));

        assert_eq!(
            playback.set_auto_paused("screen-a", true),
            Some(true),
            "全屏应当自动暂停这块屏"
        );
        // 游戏开了 10 分钟：这块屏的 due 早就过去了，但一直没换纸。
        set_due(&playback, "screen-a", Instant::now() - Duration::from_secs(600));
        assert!(playback.claim_due().is_empty(), "全屏期间不该换纸");

        assert_eq!(
            playback.set_auto_paused("screen-a", false),
            Some(true),
            "退出全屏应当恢复这块屏"
        );
        let wait = playback.time_until_next();
        assert!(
            wait > Duration::from_secs(25) && wait <= Duration::from_secs(31),
            "退出全屏后应当接着那 30 秒算（而不是重新 100 秒、也不该立刻跳）：{wait:?}"
        );
    }

    /// 暂停中手动换一张：恢复后该等满一个间隔，而不是回到暂停前那个剩余时间。
    #[test]
    fn a_manual_next_while_paused_restarts_the_held_interval() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 100, 3)]);
        set_due(&playback, "screen-a", Instant::now() + Duration::from_secs(20));
        assert!(playback.pause_screen("screen-a", true));
        assert!(held(&playback, "screen-a").is_some(), "暂停时应当记下剩余时间");

        playback.next_on("screen-a", &sink_trait);
        assert_eq!(
            held(&playback, "screen-a"),
            Some(Duration::from_secs(100)),
            "手动换过一张之后，暂停中记的剩余时间应当改成整段间隔"
        );
    }

    /// 暂停中叠加第二个暂停原因（先手动暂停、再进游戏）：倒计时不能被重新取一次。
    #[test]
    fn a_second_pause_reason_does_not_recapture_the_countdown() {
        let playback = Playback::new(vec![plan("screen-a", 100, 3)]);
        set_due(&playback, "screen-a", Instant::now() + Duration::from_secs(40));
        assert!(playback.pause_screen("screen-a", true));
        assert_eq!(held_secs(&playback, "screen-a"), Some(40));

        // 暂停期间把 due 挪到别处（模拟时间流逝），再叠加"全屏暂停"。
        set_due(&playback, "screen-a", Instant::now() + Duration::from_secs(5));
        assert_eq!(playback.set_auto_paused("screen-a", true), Some(false));
        assert_eq!(
            held_secs(&playback, "screen-a"),
            Some(40),
            "叠加暂停原因不该动倒计时"
        );

        // 放开"全屏"那一位：手动暂停还在，因此仍然停表、剩余时间不变。
        assert_eq!(playback.set_auto_paused("screen-a", false), Some(false));
        assert_eq!(held_secs(&playback, "screen-a"), Some(40));
        assert!(playback.claim_due().is_empty(), "手动暂停还在，不该换纸");
    }

    #[test]
    fn global_pause_stops_every_screen_then_resumes() {
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 1, 3),
            plan("screen-b", 1, 3),
        ]);
        playback.pause();
        playback.mark_all_due();
        assert!(
            run_due(&playback, &sink_trait).is_empty(),
            "暂停期间不该换纸"
        );
        assert!(sink.applied_files().is_empty());

        playback.resume();
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        assert_eq!(results.len(), 2, "恢复后两块都该换");
    }

    #[test]
    fn previous_on_returns_to_the_previously_shown_image() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 5)]);

        playback.next_on("screen-a", &sink_trait);
        let first = playback.states()[0].current.clone().unwrap();
        playback.next_on("screen-a", &sink_trait);
        let second = playback.states()[0].current.clone().unwrap();
        assert_ne!(first, second, "两次 next 应是不同的图");

        let outcome = playback.previous_on("screen-a", &sink_trait);
        assert_eq!(outcome, ControlOutcome::Applied);
        let back = playback.states()[0].current.clone().unwrap();
        assert_eq!(back, first, "退回后应显示第一次那张");
    }

    #[test]
    fn previous_on_random_without_history_reports_no_history_not_failure() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 10)]);
        assert_eq!(
            playback.previous_on("screen-a", &sink_trait),
            ControlOutcome::NoHistory,
            "没有可回退的历史是正常状态，不应报成失败"
        );
    }

    #[test]
    fn failure_is_recorded_in_state_without_incrementing_changes() {
        struct FailingSink;
        impl WallpaperSink for FailingSink {
            fn apply(&self, _monitor: &str, _file: &std::path::Path) -> Result<u64, String> {
                Err("磁盘满了".to_string())
            }
        }
        let sink: Arc<dyn WallpaperSink> = Arc::new(FailingSink);
        let playback = Playback::new(vec![plan("screen-a", 3600, 1)]);

        let outcome = playback.next_on("screen-a", &sink);
        assert!(matches!(outcome, ControlOutcome::Failed(_)));
        let states = playback.states();
        assert_eq!(states[0].changes, 0, "失败不应计入换纸次数");
        assert_eq!(states[0].last_error.as_deref(), Some("磁盘满了"));
        assert!(states[0].current.is_none());
    }

    #[test]
    fn a_screen_without_candidates_is_reported_on_the_tick() {
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("screen-a", 100, 0),
            plan("screen-b", 100, 2),
        ]);
        playback.mark_all_due();
        let results = run_due(&playback, &sink_trait);
        // 没有候选的屏也会被"处理"（结果里带 NoCandidates），
        // 但不会真的设壁纸——界面据此提示用户去改筛选条件。
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|(name, outcome)| name == "screen-a" && *outcome == ControlOutcome::NoCandidates));
    }

    #[test]
    fn pending_screens_are_not_touched_before_their_deadline() {
        // 间隔很长的屏不该因为"刚启动"就被换纸；到点集合只收该换的屏。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);
        assert!(run_due(&playback, &sink_trait).is_empty());
        assert!(sink.applied_files().is_empty());
        assert!(playback.time_until_next() > Duration::from_secs(3000));
    }

    #[test]
    fn a_static_image_screen_is_never_due_and_only_applied_on_demand() {
        // **固定单图不参与轮播**：它不会到点、不会被"下一张"带上，
        // 只在启动/重建（apply_all）时应用一次。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![static_plan("screen-a", "C:/pics/fixed.png")]);
        playback.mark_all_due();

        assert!(
            playback.claim_due().is_empty(),
            "固定单图的屏永远不该到点（否则每个间隔都会重复设一次同一张图）"
        );
        assert!(playback.next_all(&sink_trait).is_empty(), "「下一张」不该带上它");
        assert!(playback.previous_all(&sink_trait).is_empty());
        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::FixedImage,
            "单独对它按「下一张」要明确说清是固定单图，而不是报成没有候选"
        );
        assert!(sink.applied_files().is_empty(), "上面这些操作都不该动壁纸");

        // 启动/重建时应用一次。
        let results = playback.apply_all(&sink_trait);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, ControlOutcome::Applied);
        assert_eq!(sink.applied_files(), vec!["C:/pics/fixed.png"]);

        let state = &playback.states()[0];
        assert_eq!(state.current.as_deref(), Some("C:/pics/fixed.png"));
        assert_eq!(state.changes, 1);
        assert!(state.last_error.is_none());
    }

    #[test]
    fn apply_all_mixes_rotating_and_static_screens() {
        // 一台机器上"一块屏轮播、另一块屏固定单图"是最常见的组合，
        // 两边的行为都要对：轮播的那块拿到候选里的一张，单图的那块拿到那一张。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![
            plan("rotating", 100, 2),
            static_plan("fixed", "C:/pics/fixed.png"),
        ]);
        let results = playback.apply_all(&sink_trait);
        assert_eq!(results.len(), 2, "两块屏都要被应用：{results:?}");

        let files = sink.applied_files();
        assert!(
            files.iter().any(|f| f.ends_with("fixed.png")),
            "固定单图那张要设上去：{files:?}"
        );
        assert!(
            files.iter().any(|f| f.contains("rotating")),
            "轮播的那块要换一张：{files:?}"
        );

        // 之后再"到点"时，只有轮播的那块参与。
        playback.mark_all_due();
        let tasks = playback.claim_due();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].monitor_name(), "rotating");
    }

    #[test]
    fn current_of_reports_what_is_on_screen() {
        // 「删除当前壁纸」要靠它知道"当前是哪一张"。
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 3)]);
        assert_eq!(playback.current_of("screen-a"), None, "还没换过纸时没有当前图");

        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::Applied
        );
        let shown = playback.current_of("screen-a").expect("换过之后要有当前图");
        assert!(shown.starts_with("C:/pics/screen-a_"));
        assert_eq!(playback.current_of("不存在的屏"), None);
    }

    #[test]
    fn a_forgotten_candidate_is_never_drawn_again_and_the_count_drops() {
        // **这条守的是「删除当前壁纸」的可见效果**：删掉的那张不能再换上来，
        // 而且界面上的"候选 N 张"要跟着变小（否则用户会以为没删掉）。
        let (sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 4)]);

        playback.next_on("screen-a", &sink_trait);
        let victim = playback.current_of("screen-a").unwrap();
        assert_eq!(playback.states()[0].candidate_count, 4);

        assert!(
            playback.forget_candidate("screen-a", &victim),
            "它就在候选里，应当移得掉"
        );
        assert_eq!(
            playback.states()[0].candidate_count,
            3,
            "候选数要跟着变小"
        );
        assert!(
            !playback.forget_candidate("screen-a", &victim),
            "第二次移同一张应当报 false"
        );

        // 之后无论换多少次，都不该再出现那一张。
        // （前一次是**删除之前**设上去的，因此只能从这一刻之后开始比。）
        let applied_before = sink.applied_files().len();
        for _ in 0..40 {
            playback.next_on("screen-a", &sink_trait);
            assert_ne!(playback.current_of("screen-a").as_deref(), Some(victim.as_str()));
        }
        assert!(
            !sink.applied_files()[applied_before..].iter().any(|f| f == &victim),
            "被删掉的那张一次都不该再被设上去"
        );
    }

    #[test]
    fn deleting_the_last_candidate_reports_no_candidates_instead_of_failing() {
        // 只剩一张时把它删掉：要明确报"没有可用的图"，而不是继续去抽一张
        // 已经被移走的图（那会变成"点了没反应"）。
        let (_sink, sink_trait) = RecordingSink::new();
        let playback = Playback::new(vec![plan("screen-a", 3600, 1)]);
        playback.next_on("screen-a", &sink_trait);
        let only = playback.current_of("screen-a").unwrap();

        assert!(playback.forget_candidate("screen-a", &only));
        assert_eq!(playback.states()[0].candidate_count, 0);
        assert_eq!(
            playback.next_on("screen-a", &sink_trait),
            ControlOutcome::NoCandidates
        );
    }

    /// 按**指定的候选路径**造一块屏的计划（`plan` 是按数量自动生成的，
    /// 改不出"数量一样但内容不同"这种情况）。
    fn plan_of(name: &str, interval_secs: u64, paths: &[&str]) -> ScreenPlan {
        ScreenPlan {
            monitor_name: name.to_string(),
            width: 800,
            height: 600,
            interval: Duration::from_secs(interval_secs),
            fit: FitMode::Fill,
            candidates: paths.iter().map(|p| entry(p, 800, 600)).collect(),
            single_image: None,
        }
    }

    /// 某块屏"下一次该换纸"的时刻。
    fn due_of(playback: &Playback, name: &str) -> Instant {
        let screen = playback
            .screens
            .iter()
            .find(|s| s.plan.monitor_name == name)
            .unwrap_or_else(|| panic!("应当有 {name} 这块屏"));
        *screen.next_due.lock().unwrap()
    }

    #[test]
    fn changing_one_screen_only_rebuilds_that_screen() {
        // **用户 2026-10 报的 bug**：「当我更改某一个显示器的播放列表时，
        // 两个显示器的播放列表都会同时刷新。」根因是外壳把整个 `Playback`
        // 换新的、再对**所有**屏换纸。这条钉住新的语义：只有计划真的变了的屏
        // 才被重建、才被换纸；另一块屏的当前图、倒计时、换纸次数都不动。
        let (sink, sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        before.apply_all(&sink_trait);

        let applied_before = sink.applied_files().len();
        let a_current = state_of(&before, "screen-a").current;
        let a_changes = state_of(&before, "screen-a").changes;
        let a_due = due_of(&before, "screen-a");

        // 只有 screen-b 变了：候选数量一样（都是 2 张），但**内容**不同。
        let (after, changed) = before.rebuild_with(vec![
            plan("screen-a", 100, 3),
            plan_of("screen-b", 100, &["C:/other/x.png", "C:/other/y.png"]),
        ]);

        assert_eq!(changed, vec!["screen-b".to_string()], "只有 b 变了");

        // b 是全新的：没有当前图、倒计时重新起算。
        assert_eq!(state_of(&after, "screen-b").current, None);
        assert_eq!(state_of(&after, "screen-b").candidate_count, 2);

        // a 原地留用：当前图与倒计时**一模一样**（不是"重新起算一整段"）。
        assert_eq!(state_of(&after, "screen-a").current, a_current);
        assert_eq!(state_of(&after, "screen-a").changes, a_changes);
        assert_eq!(due_of(&after, "screen-a"), a_due, "a 的倒计时不该被重置");

        // 只给变了的屏换纸：a 一次都不该再被设上去。
        after.apply_screens(&changed, &sink_trait);
        let newly_applied = &sink.applied_files()[applied_before..];
        assert_eq!(newly_applied.len(), 1, "只该换一块屏：{newly_applied:?}");
        assert!(
            newly_applied[0].starts_with("C:/other/"),
            "换的应当是 b 的新候选：{newly_applied:?}"
        );
        assert!(
            !newly_applied.iter().any(|f| Some(f) == a_current.as_ref()),
            "a 屏那块图不该被重新设上去"
        );
    }

    #[test]
    fn nothing_is_reapplied_when_no_plan_changed() {
        // 与上一条配套：所有计划都没变时，`changed` 为空，一块屏都不该被碰。
        let (sink, sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        before.apply_all(&sink_trait);
        let applied_before = sink.applied_files().len();

        let (after, changed) = before.rebuild_with(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        assert!(changed.is_empty(), "没有计划变化时 changed 必须为空");
        after.apply_screens(&changed, &sink_trait);
        assert_eq!(sink.applied_files().len(), applied_before, "一块屏都不该被换纸");
    }

    #[test]
    fn a_newly_connected_screen_counts_as_changed() {
        // 插上一块新屏：它必须被算成"变了"，否则那块屏会一直空着等下个间隔。
        let (_sink, _sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3)]);
        let (after, changed) = before.rebuild_with(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        assert_eq!(changed, vec!["screen-b".to_string()]);
        assert_eq!(after.states().len(), 2);
        assert_eq!(state_of(&after, "screen-a").changes, 0, "a 不该被牵连");
    }

    #[test]
    fn a_switched_off_screen_disappears_without_touching_the_rest() {
        // 拔掉一块屏之后重建：它从状态里消失，留下的那块不受影响。
        let (_sink, _sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        let (after, changed) = before.rebuild_with(vec![plan("screen-a", 100, 3)]);
        assert!(changed.is_empty(), "留下的那块没变，不该被算成变了");
        assert_eq!(after.states().len(), 1);
        assert_eq!(after.states()[0].monitor_name, "screen-a");
    }

    #[test]
    fn rebuild_keeps_the_global_pause() {
        // 全局暂停中保存设置重建：不该悄悄开始换纸。
        let (sink, sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3), plan("screen-b", 100, 3)]);
        before.pause();

        let (after, changed) = before.rebuild_with(vec![
            plan("screen-a", 100, 3),
            plan_of("screen-b", 100, &["C:/other/x.png"]),
        ]);
        assert!(after.is_paused(), "重建不该把全局暂停弄丢");
        assert_eq!(changed, vec!["screen-b".to_string()]);

        // 全局暂停中，新屏的倒计时也不走（`claim_due` 一条都不取）。
        after.apply_screens(&changed, &sink_trait);
        assert!(after.claim_due().is_empty(), "暂停中不该有屏到点");
        assert!(!sink.applied_files().is_empty(), "但手动换的那一张是设上去了的");
    }

    #[test]
    fn a_screen_turned_into_a_fixed_image_is_applied_immediately() {
        // "轮播"改成"固定单图"也是一种变化：那张图要立刻上屏。
        let (sink, sink_trait) = RecordingSink::new();
        let before = Playback::new(vec![plan("screen-a", 100, 3)]);
        let (after, changed) = before.rebuild_with(vec![static_plan("screen-a", "C:/pics/only.png")]);
        assert_eq!(changed, vec!["screen-a".to_string()]);
        after.apply_screens(&changed, &sink_trait);
        assert_eq!(sink.applied_files(), vec!["C:/pics/only.png".to_string()]);
    }

    #[test]
    fn fingerprint_sees_everything_that_changes_playback() {
        // 引擎自己这一层也要守住"什么算变了"（外壳的指纹判断已改为调它）。
        let base = plan("screen-a", 100, 3);
        assert_eq!(base.fingerprint(), plan("screen-a", 100, 3).fingerprint());
        assert_ne!(base.fingerprint(), plan("screen-a", 60, 3).fingerprint());
        assert_ne!(base.fingerprint(), plan("screen-a", 100, 4).fingerprint());
        // 数量一样但内容不同：必须算是变了。
        assert_ne!(
            plan_of("screen-a", 100, &["C:/pics/a.png"]).fingerprint(),
            plan_of("screen-a", 100, &["C:/pics/b.png"]).fingerprint()
        );
        // 固定单图的路径必须参与。
        assert_ne!(
            static_plan("screen-a", "C:/pics/a.png").fingerprint(),
            static_plan("screen-a", "C:/pics/b.png").fingerprint()
        );
        // 单调图与"空的轮播计划"必须能区分开。
        assert_ne!(
            static_plan("screen-a", "C:/pics/a.png").fingerprint(),
            plan_of("screen-a", 100, &[]).fingerprint()
        );
    }
}

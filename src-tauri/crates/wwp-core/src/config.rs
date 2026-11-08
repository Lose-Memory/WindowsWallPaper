//! 用户设置。
//!
//! 文件放在数据根目录的 `config/settings.toml`。缺失的字段一律用默认值补齐，
//! 因此新增设置项不会让旧配置文件失效。
//!
//! ## 为什么要自己生成 TOML 文本
//!
//! 曾经用 `toml_edit::ser` 自动序列化 + "把新值合并进旧文档以保留注释"，那条路
//! 出了两个真实的错误，都由实测（每屏设置完全不生效）暴露出来：
//!
//! 1. **合并会把嵌套表写成数组表**：`[monitors.'...']` 变成 `[[monitors.'...']]`。
//!    而 `monitors` 是 `BTreeMap`，反序列化时期望的是表而不是数组，于是整个
//!    `monitors` 段落解析失败。
//! 2. **解析失败会被 `#[serde(default)]` 吞掉**：表现为"每屏设置静默失效"，
//!    现场看起来像功能没做，实际是配置读不进来。
//!
//! 现在的做法是：**自己拼文本、整篇重写**，并且解析失败必须报错。
//! 代价是不再保留用户手写的注释——用固定的文件头说明代替。这个取舍是有意的：
//! 宁可不要注释保留，也不能接受"保存一次就丢掉每屏设置"。
//!
//! ## 键必须用单引号字面量
//!
//! 显示器名形如 `\\.\DISPLAY1`。在 TOML 的双引号字符串里，反斜杠序列不是合法
//! 转义（合法的只有 `\b \f \n \r \t \u \U \\ \"`），会让整个段落解析失败。
//! 字面量字符串（单引号）不做转义处理，是唯一不会踩这个坑的写法。
//! [`toml_string`] 负责这一层，并在值里确实含单引号时退回双引号 + 手工转义。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// 图片填充方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// 裁切填满（默认）。
    Fill,
    /// 完整显示，留黑边。
    Fit,
    /// 拉伸到满屏。
    Stretch,
    /// 居中原始尺寸。
    Center,
}

impl Default for FitMode {
    fn default() -> Self {
        FitMode::Fill
    }
}

/// 硬件渲染层（视频壁纸）所用的宿主窗口形态。
///
/// 这不是一个"风格偏好"，而是一个**兼容性开关**：两种形态在系统里的合成路径不同。
///
/// - `Child`：把窗口挂成 `Progman` 的子窗口。实现最自然，位置与 z 序都正确，
///   但**它的 DirectComposition 内容在本机看不到**（疑点是 Progman 的子窗口
///   由 Progman 自己绘制、不经过 DWM 的窗口重定向）。
/// - `TopLevel`：自己建顶层窗口并压到 z 序底部（`Progman` 正上方）。
///   实测 z 序与位置都正确，而且是真正的顶层窗口，会经过 DWM 合成——
///   上轮的对照实验证明顶层窗口的 DComp 内容能被看到。
///
/// 默认用 `TopLevel`：它符合"内容要经过 DWM 合成"这个已知条件。
/// 保留 `Child` 是为了对比与回退，而不是因为它在功能上等价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerModeSetting {
    Child,
    #[serde(alias = "top", alias = "top-level")]
    Toplevel,
}

impl Default for LayerModeSetting {
    fn default() -> Self {
        LayerModeSetting::Toplevel
    }
}

/// 单击托盘图标要做什么。
///
/// ## 为什么做成设置项
///
/// 用户 2026-10-05 的要求：托盘的单击可以配成"打开主界面"或"换一张壁纸"，
/// 选了后者时用**双击**打开主界面。理由是这台机器上壁纸一直在换，
/// "换一张"是最高频的动作，而打开设置界面一天也用不到一次。
///
/// 两个值都保留（而不是只留"换一张"）：换到别的用法时改回来即可，
/// 不必改代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayClickAction {
    /// 打开主界面（默认，也是这个程序一直以来的行为）。
    OpenWindow,
    /// 换一张壁纸；此时**双击**托盘图标打开主界面。
    NextWallpaper,
}

impl Default for TrayClickAction {
    fn default() -> Self {
        TrayClickAction::OpenWindow
    }
}

impl TrayClickAction {
    /// 渲染进 TOML 的字符串（与 [`TrayClickAction`] 的 serde 写法必须一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            TrayClickAction::OpenWindow => "open-window",
            TrayClickAction::NextWallpaper => "next-wallpaper",
        }
    }
}

/// 全局默认设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub interval_seconds: u64,
    pub fit_mode: FitMode,
    pub minimize_to_tray: bool,
    /// 硬件渲染层的宿主窗口形态。见 [`LayerModeSetting`]。
    pub layer_mode: LayerModeSetting,
    /// 视频壁纸的音量，0 ~ 100 的整数百分比。
    ///
    /// 用整数百分比而不是 0.0~1.0 的浮点：界面上的滑块本来就是整数档，
    /// 用整数能避免浮点往返造成的"显示 42% 实际 41.999%"这类不一致；
    /// 而且 TOML 里的整数比浮点更不容易被手改坏。
    ///
    /// `serde(default)` 让旧设置文件（没有这个字段）能读进来并取默认值——
    /// 不加的话升级后用户的设置会**整个解析失败**，那比丢一个音量值严重得多。
    #[serde(default = "default_volume_percent")]
    pub volume_percent: u32,
    /// 是否静音。
    #[serde(default)]
    pub muted: bool,
    /// 是否启用视频壁纸。
    ///
    /// ## 为什么首版默认关闭
    ///
    /// 视频壁纸需要在每块屏上创建一层铺满的**不透明**宿主窗口来画帧。
    /// 图片壁纸由系统渲染，完全不需要它。因此"只播图片"时那层窗口
    /// 不仅没用，还会**盖住系统壁纸**——用户看到的是一片灰色。
    ///
    /// 做成开关而不是删掉代码有两个好处：
    /// 1. 首版只走图片路径，没有那层窗口，行为简单可靠；
    /// 2. 视频相关的代码与实测结论都保留着，将来打开即可，
    ///    不必重新踩一遍窗口、合成、解码、音画同步那些坑。
    ///
    /// `serde(default)` 让没有这个字段的旧设置文件也能读进来（取 `false`）。
    /// 这个默认值必须与 `Default::default()` 保持一致，否则"新建配置"
    /// 与"读旧配置"会得到不同的行为。
    #[serde(default = "default_video_enabled")]
    pub video_enabled: bool,
    /// 启动后是否自动接管壁纸并开始轮播。
    ///
    /// ## 为什么默认开启，但**不是**无条件自动开始
    ///
    /// 默认开启是因为"打开程序就是要它换壁纸"——让用户每次再点一下
    /// "开始轮播"是多余的步骤。
    ///
    /// 但有一件事不能自动：**如果上次退出时是「暂停」状态，这次要保持暂停**。
    /// 否则用户"临时不想它换壁纸 → 暂停 → 退出"之后，下次开机它又开始换，
    /// 而用户很难想到是这里的问题。
    ///
    /// 那个"上次是不是暂停"不存在这个文件里：它是**运行时状态**，
    /// 不是用户设置——写进配置会让它在界面上变成一个可编辑的字段，
    /// 而它不是。见 `data/playback-state.json`。
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
    /// 启动时是否**不显示主界面**（只留托盘图标）。
    ///
    /// ## 为什么默认开启
    ///
    /// 这是一个常驻的壁纸软件：用户开机后并不需要看到设置界面，
    /// 需要时点托盘图标即可。默认弹出窗口反而让人以为"得先关掉它"。
    ///
    /// 实现上窗口是**以隐藏状态创建**的（`tauri.conf.json` 里的 `visible: false`），
    /// 再按这个设置决定要不要显示——而不是"先显示再重隐藏"，
    /// 那样会闪一下。
    #[serde(default = "default_start_minimized")]
    pub start_minimized: bool,
    /// 某块屏上有程序全屏时，**只暂停这块屏**的轮播；退出全屏后自动恢复。
    ///
    /// ## 判据
    ///
    /// "全屏"包含两种：独占全屏 / 无边框全屏（窗口盖住整块屏 ≥ 95%），
    /// 以及**最大化窗口**——用户明确要求两者都算。
    /// 判定与排除规则见 [`crate::fullscreen`]。
    ///
    /// ## 为什么默认关闭
    ///
    /// 它改变的是"壁纸什么时候不动"，而"最大化窗口也算全屏"意味着
    /// 只要有一块屏上挂着最大化的浏览器，那块屏的壁纸就停着——那是用户
    /// 可能不想要的行为。默认关闭，让用户自己明确打开。
    ///
    /// ## 与手动暂停的关系
    ///
    /// 这个开关带来的暂停与用户手动按下的暂停**是两个独立的原因**，
    /// 互不解除（详见 [`crate::playback`] 的模块文档）。因此打开它不会
    /// 影响用户自己的暂停，关掉它也不会动用户手动暂停的那几块屏。
    #[serde(default)]
    pub pause_on_fullscreen: bool,
    /// 切换 NSFW 模式的**全局快捷键**，例如 `Ctrl+Alt+S`。
    ///
    /// 空串表示不注册。写法与界面上的输入框一致：`Ctrl` / `Alt` / `Shift` / `Win`
    /// 用 `+` 连接，最后一个是单个字母、单个数字、F1~F24，或方向键一类
    /// 有名字的键（`Left` / `Right` / `Space` 等，清单见 [`crate::hotkey::parse`]）。
    /// 解析失败只记一条 warn、不注册——一个写错的快捷键不该让程序起不来。
    ///
    /// ## 旧名字（`safe_hotkey`）
    ///
    /// 这个模式原先叫 SAFE，字段名因此是 `safe_hotkey`。1.12.0 改名时保留了
    /// serde 别名：升级后**读**旧配置照样认得，下一次保存才写成新名字。
    /// 不留别名的话，用户升级后会发现自己的 NSFW 快捷键悄悄变回了默认值。
    #[serde(default = "default_nsfw_hotkey", alias = "safe_hotkey")]
    pub nsfw_hotkey: String,
    /// 全局暂停/继续的快捷键。
    ///
    /// 与界面上的"暂停"按钮是同一件事（作用于**所有**屏），只是不用把窗口翻出来。
    #[serde(default = "default_pause_hotkey")]
    pub pause_hotkey: String,
    /// 全局"下一张"的快捷键（所有屏各换一张）。
    #[serde(default = "default_next_hotkey")]
    pub next_hotkey: String,
    /// 全局"上一张"的快捷键（所有屏各退回上一张）。
    #[serde(default = "default_previous_hotkey")]
    pub previous_hotkey: String,
    /// 全局"显示主界面"的快捷键。
    ///
    /// ## 为什么需要它
    ///
    /// 程序默认**不显示主界面**（[`General::start_minimized`]），平时只留托盘图标。
    /// 而"想改点什么"时得先把窗口翻出来——托盘图标会被折叠进溢出区、
    /// 任务栏上又没有它，于是用户只能去双击 exe（那会走单实例门闩把窗口打开）。
    /// 给一个快捷键，这条路就不必再找图标。
    ///
    /// 与托盘菜单的「打开主界面」、第二次启动程序走的是**同一段逻辑**
    /// （外壳的 `tray::show_main_window`）：显示、取消最小化、拿焦点。
    #[serde(default = "default_show_window_hotkey")]
    pub show_window_hotkey: String,
    /// 设置页「清理程序目录」那张卡片是否展开。
    ///
    /// ## 为什么它是一个设置（而不是界面自己记在别处）
    ///
    /// 用户要求"默认展开，用户可以手动选择折叠，**记忆这个选项**"。它因此必须
    /// 落在一个"下次启动还在"的地方，而 `settings.toml` 正是这个程序记这类事
    /// 的地方（`tray_click_action` 也是同一类：纯界面偏好，不是播放行为）。
    ///
    /// 放这里还有一个实际好处：清理功能会删 `data/` 下的各种缓存，而设置文件
    /// 本身**永远不在清理清单里**——这个"记住折叠"的小状态不会把自己清掉。
    #[serde(default = "default_cleanup_expanded")]
    pub cleanup_expanded: bool,
    /// 单击托盘图标做什么。见 [`TrayClickAction`]。
    ///
    /// 缺失时取默认值（[`General`] 的容器级 `#[serde(default)]` 会从
    /// `Default::default()` 里取），因此旧配置照常能读进来。
    pub tray_click_action: TrayClickAction,
}

/// 默认**开启**启动自动接管。理由见 [`General::auto_start`]。
fn default_auto_start() -> bool {
    true
}

/// 默认**开启**启动时不显示主界面。理由见 [`General::start_minimized`]。
fn default_start_minimized() -> bool {
    true
}

/// 默认**展开**设置页那张「清理程序目录」卡片。
///
/// 用户 2026-10 的要求就是"默认展开"：那张卡片里的东西是"看一眼就知道要不要清"，
/// 默认收起来等于把这个功能藏起来。
fn default_cleanup_expanded() -> bool {
    true
}

/// 默认的 NSFW 模式快捷键。
///
/// 选 `Ctrl+Alt+S`：`S` 对应 NSFW，且加上两个修饰键之后几乎不会和别的软件撞
/// （`Ctrl+Alt+` 系列被大量软件占用，但加上具体字母后冲突概率很低）。
fn default_nsfw_hotkey() -> String {
    "Ctrl+Alt+S".to_string()
}

/// 默认的"暂停/继续"快捷键。`P` 对应 Pause。
fn default_pause_hotkey() -> String {
    "Ctrl+Alt+P".to_string()
}

/// 默认的"下一张"快捷键。
///
/// ## 为什么默认给方向键而不是留空
///
/// 留空意味着"装了但按了没反应"，用户得先去设置里找。给一个能用的默认值，
/// 他要改也随时能改。
///
/// 已知的冲突可能：部分显卡驱动（尤其 Intel 核显）把 `Ctrl+Alt+方向键`
/// 注册成"旋转屏幕"。那属于驱动劫持，我们的注册会失败并记一条 warn，
/// 用户在界面上把写法改成别的即可——因此提示里写明了这件事。
fn default_next_hotkey() -> String {
    "Ctrl+Alt+Right".to_string()
}

/// 默认的"上一张"快捷键。理由见 [`default_next_hotkey`]。
fn default_previous_hotkey() -> String {
    "Ctrl+Alt+Left".to_string()
}

/// 默认的"显示主界面"快捷键。
///
/// `W` 对应 Window。要避开的组合已经很清楚了：`S` / `P` / `Left` / `Right`
/// 已被上面四个占用，而 `Ctrl+Alt+Esc` 一类是系统保留的。
/// 与其他四个一样，撞车（注册失败）只会记一条 warn，界面上换一个写法即可。
fn default_show_window_hotkey() -> String {
    "Ctrl+Alt+W".to_string()
}

/// 首版默认**不启用**视频壁纸。理由见 [`General::video_enabled`]。
fn default_video_enabled() -> bool {
    false
}

/// 默认音量：100%。
///
/// 选 100 而不是"某个温和的值"：用户没调过音量时应当听到原声。
/// 替用户把音量调小会让人以为视频本身没声音。
fn default_volume_percent() -> u32 {
    100
}

impl Default for General {
    fn default() -> Self {
        General {
            interval_seconds: default_interval_seconds(),
            fit_mode: FitMode::Fill,
            minimize_to_tray: true,
            layer_mode: LayerModeSetting::default(),
            volume_percent: default_volume_percent(),
            muted: false,
            video_enabled: default_video_enabled(),
            auto_start: default_auto_start(),
            start_minimized: default_start_minimized(),
            pause_on_fullscreen: false,
            nsfw_hotkey: default_nsfw_hotkey(),
            pause_hotkey: default_pause_hotkey(),
            next_hotkey: default_next_hotkey(),
            previous_hotkey: default_previous_hotkey(),
            show_window_hotkey: default_show_window_hotkey(),
            cleanup_expanded: default_cleanup_expanded(),
            tray_click_action: TrayClickAction::default(),
        }
    }
}

/// 默认换图间隔：100 秒。
///
/// 用户明确要求新建播放列表的默认间隔是 100 秒。它同时是这里"全局默认"的值，
/// 而界面新建列表时取的就是这个值，因此两处不会不一致。
///
/// 为什么不是原先的 300 秒：壁纸软件的默认值应当让人"打开就能看到它在工作"，
/// 5 分钟一次的节奏对第一次使用的人来说太慢了，会以为没生效。
pub fn default_interval_seconds() -> u64 {
    100
}

/// 一个播放列表。
///
/// ## 为什么要有它（而不是让每屏直接挂一个图库）
///
/// 原先每屏只能挂**一个**图库，图库本身还有一个全局的"启用"开关。
/// 那套模型回答不了"同一块屏，工作时播 A 目录、晚上播 B 目录"这种需求——
/// 它只能靠手工改设置或挪文件，而后者违背"从不动用户文件"的约定。
///
/// 播放列表把"内容集"独立出来：一个列表可以关联**多个**图库，
/// 每块屏各自记住自己当前用哪个列表。切换列表就是切换内容集，
/// 不需要碰任何文件。
///
/// ## 字段放在这里的理由
///
/// 间隔与填充方式是**内容相关的**（一套精选图想慢慢看、
/// 一套随手拍的可以快翻），因此属于列表；而长宽比筛选放在显示器上
/// ——它是**屏幕的物理属性**（竖屏就该只留竖图），换列表不该改变它。
///
/// **这里没有"播放顺序"**：用户取消顺序/倒序两种播放方式，只保留随机
/// （见 `playback::select`）。旧配置里的 `order` 会被安静忽略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    /// 稳定标识。界面改名字不该让"哪块屏在用哪个列表"失效，
    /// 因此绑定关系引用的是 id 而不是名字。
    pub id: String,
    /// 显示名。用户可以随便改。
    pub name: String,
    /// 这个列表使用 [`Settings::libraries`] 里的哪几个图库根目录。
    ///
    /// **它只是一份引用**：添加图库（往清单里加一个目录）的入口是「图库」页，
    /// 这里只负责"勾选"。至少关联一个图库，这个列表才有内容可播。
    pub libraries: Vec<PathBuf>,
    pub interval_seconds: u64,
    pub fit_mode: FitMode,
}

impl Default for Playlist {
    fn default() -> Self {
        Playlist {
            id: String::new(),
            name: String::new(),
            libraries: Vec::new(),
            interval_seconds: General::default().interval_seconds,
            fit_mode: FitMode::default(),
        }
    }
}

/// 单个显示器的设置。键是显示器名（形如 `\\.\DISPLAY1`）。
///
/// **这里没有"是否参与轮播"的开关。** 原先有一个 `enabled` 字段与界面上的
/// 「参与轮播」勾选框，而它与"这块屏播什么"是两套说法：真正的开关是
/// [`MonitorSettings::current_playlist`]——**不选播放列表，这块屏就没有内容可播**，
/// 也就不会换纸。两个开关各自能关掉一块屏，用户很难说清是哪个在起作用，
/// 因此按用户要求把 `enabled` 整个删掉了。
///
/// **也没有每屏的排除规则**：排除规则只保留"目录前缀"一类，而它的编辑入口
/// 是图库页的目录树复选框（写全局 `[exclude].dirs`）。原先那套每屏
/// `[monitors.*.exclude]` 没有任何界面入口，留着只会让人以为它能配。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorSettings {
    /// 这块屏当前使用哪个播放列表（存的是 `Playlist::id`）。
    ///
    /// 空字符串表示"还没选"。那种状态下该屏没有内容可播——这是刻意的：
    /// 与其猜一个默认列表，不如让界面明确提示"这块屏还没选播放列表"。
    pub current_playlist: String,
    /// 长宽比等筛选条件。**属于显示器**，所有列表共用
    /// （理由见 [`Playlist`] 的说明）。
    pub filter: FilterSettings,
    /// NSFW 模式时这块屏显示哪张图。空串表示没设置。
    ///
    /// 它的产物固定落在 `data/output/<屏>-nsfw.bmp`，因此点 NSFW 时只是
    /// 调一次 `SetWallpaper`（几毫秒），不需要现场解码。
    ///
    /// 字段原先叫 `safe_wallpaper`（模式当时叫 SAFE）：1.12.0 改名时留了 serde
    /// 别名，旧配置照样读得进来——不留的话用户升级后每块屏的 NSFW 壁纸都会丢。
    #[serde(alias = "safe_wallpaper")]
    pub nsfw_wallpaper: String,
    /// 这块屏的**固定单图**。非空时这块屏只显示这一张图，不参与轮播。
    ///
    /// ## 为什么它优先于播放列表
    ///
    /// 用户要的是"这块屏就显示这一张"。若两者同时存在还要按优先级挑一个，
    /// 那用户必须自己记住哪个赢——不如果断地让"单图"赢，并在界面上写明
    /// "固定单图生效中，播放列表被忽略"。这样就没有两套说法。
    ///
    /// 产物固定落在 `data/output/<屏>-single.bmp`：**一份**，换了源图就覆盖它
    /// （用 mtime 判断是否需要重新解码，见 `engine::DesktopSink::prepare_single`）。
    pub single_image: String,
    /// 这块屏在界面上的**显示名**（用户双击显示器名改的就是它）。空串 = 用真实显示器名。
    ///
    /// ## 为什么要有它，以及为什么只允许英文和数字
    ///
    /// 真实显示器名形如 `\\.\DISPLAY1`，两块屏排在一起时**很难一眼分辨谁是谁**，
    /// 而"哪块是竖屏、哪块在左边"这件事正是这一页要对比的。用户要求双击改名。
    ///
    /// 用户明确说"支持英文和数字就行，不需要支持中文"，因此这里存的是
    /// [`sanitize_monitor_label`] 过滤后的结果：中文、标点等字符直接丢掉。
    /// 这个字段**只用于显示**——所有配置键、日志、`data/output/` 里的产物名
    /// 仍然用真实显示器名，因此改个显示名不会让任何绑定失效。
    pub label: String,
}

/// 显示名允许的最大字符数。
///
/// 上限存在的理由不是"存不下"，而是**不能让一个名字把卡片挤爆**：
/// 名字那一行还要放「主 / 竖屏 / 左」几个标签。
pub const MONITOR_LABEL_MAX: usize = 24;

/// 清洗显示器显示名：只保留 ASCII 字母、数字、空格、`-`、`_`。
///
/// 用户要求"支持英文和数字就行，不需要支持中文"，因此其它字符（中文、标点）
/// **直接丢弃**而不是原样保存。原样保存的话界面上会出现一段半中文半英文的名字，
/// 用户根本认不出自己写了什么——而这个字段只用于显示，丢掉不损失任何功能。
///
/// 连续空白折成一个空格，首尾空白去掉；空串表示"用真实显示器名"。
pub fn sanitize_monitor_label(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in raw.trim().chars() {
        if out.chars().count() >= MONITOR_LABEL_MAX {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        } else if ch == ' ' || ch == '\t' {
            pending_space = true;
        }
    }
    out
}

/// 每屏可独立设置的筛选条件（需求 5）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterSettings {
    /// 长宽比下限（宽/高）。例如 1.5 表示只保留横向明显的图。
    ///
    /// **包含**这个值：比值等于下限的图会通过。
    pub aspect_min: Option<f64>,
    /// 长宽比上限。给竖屏用（例如 0.9）。
    ///
    /// **不包含**这个值：比值等于上限的图会被排除。
    ///
    /// ## 两个边界的语义为什么不一样
    ///
    /// 合起来是半开区间 `[下限, 上限)`。这样相邻区间能**无重叠地拼接**：
    /// 一块屏要 `[1.3, ∞)`、另一块要 `[0, 1.3)`，正好把所有图分完、不重不漏。
    /// 若两端都包含，比值恰好 1.3 的图会同时落进两块屏。
    ///
    /// 界面上的提示文字必须与这里一致（见 `frontend/components/PlaybackPage.tsx`），
    /// 否则用户会按错误的假设去设值。
    pub aspect_max: Option<f64>,
    /// 最小宽度（像素）。**包含**这个值。
    pub min_width: Option<u32>,
    /// 最小高度（像素）。**包含**这个值。
    pub min_height: Option<u32>,
}

/// 排除规则。
///
/// ## 只剩下"目录前缀"一类
///
/// 原先还有 `globs`（通配符）与 `regexes`（正则），配套的界面是「排除规则」页。
/// 用户明确说不会去那一页编辑规则，图库页的目录树复选框才是他排除目录的方式，
/// 因此**那两类规则连同页面一起删掉了**：留着它们意味着维护三套匹配语义、
/// 三套测试、一个没人用的界面，而复选框用的始终只有 `dirs`。
///
/// 代价要说清楚：以前靠 glob/正则排除的文件，现在只能通过"取消勾选所在目录"
/// 来排除。旧配置里的 `globs` / `regexes` 会被忽略（并在下次保存时消失）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExcludeSettings {
    /// 目录路径前缀，命中则整棵子树跳过。
    ///
    /// 图库页的目录树复选框改的就是它：取消勾选一个目录 = 往这里加一条路径，
    /// 它的全部子目录自动跟着不参与。
    pub dirs: Vec<String>,
}

impl ExcludeSettings {
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// 完整设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    /// **图库清单**：存在哪几个"放图片的根目录"。顺序就是用户添加的顺序。
    ///
    /// ## 它是独立的配置项（这一条被用户改过）
    ///
    /// 原先图库**不是**独立配置项：那份清单是从所有播放列表的 `libraries`
    /// 汇总出来的，唯一的"添加图库"入口是给某个播放列表关联目录。
    /// 用户 2026-10-05 要求改掉：**"图库"页上要有「新增图库」按钮，
    /// 只能从那里添加；播放列表不再负责添加图库**（它只勾选已有的图库）。
    ///
    /// 理由与他之前的偏好一致（见 §10.22 与"不做多余步骤"那一类要求）：
    /// "加一个图库"是配置这条链的第一步，不该先绕到一个播放列表里去做，
    /// 更不该为了加第二个图库而先打开某个列表。
    ///
    /// 因此现在有两层：图库清单（这里）说"有哪些目录"，
    /// 播放列表的 [`Playlist::libraries`] 说"哪个列表用其中哪几个"。
    /// 一个图库可以不被任何列表引用（那时不会被播到，界面上会标出来）。
    pub libraries: Vec<PathBuf>,
    /// 播放列表。列表归"全局"所有（不挂在某块屏下），
    /// 因为同一份内容集可能被两块屏共用；绑定关系在
    /// [`MonitorSettings::current_playlist`] 里。
    pub playlists: Vec<Playlist>,
    /// 以显示器名为键。
    pub monitors: std::collections::BTreeMap<String, MonitorSettings>,
    pub exclude: ExcludeSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            general: General::default(),
            // **默认为空**：不替用户编内容集，也不替他挑目录。
            // 每块屏的 `current_playlist` 也是空的，界面会提示"还没选播放列表"。
            libraries: Vec::new(),
            playlists: Vec::new(),
            monitors: std::collections::BTreeMap::new(),
            exclude: ExcludeSettings::default(),
        }
    }
}

impl Settings {
    /// 按 id 找一个播放列表。
    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        if id.is_empty() {
            return None;
        }
        self.playlists.iter().find(|p| p.id == id)
    }

    /// 生成一个在现有列表里不重复的 id。
    ///
    /// 用递增计数而不是随机串：用户手改配置时要能看懂 id 是什么，
    /// 而且递增不会有"撞了同一个随机数"的隐患。
    pub fn next_playlist_id(&self) -> String {
        let mut n = self.playlists.len() + 1;
        loop {
            let candidate = format!("list{n}");
            if !self.playlists.iter().any(|p| p.id == candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// 找出引用了某个图库路径的播放列表。用于在移除图库前提示影响面。
    pub fn playlists_using(&self, library: &Path) -> Vec<&Playlist> {
        self.playlists
            .iter()
            .filter(|p| p.libraries.iter().any(|l| l == library))
            .collect()
    }

    /// 把"播放列表引用了、但图库清单里没有"的目录补进图库清单，并去掉重复项。
    ///
    /// ## 为什么必须有这一步（升级路径）
    ///
    /// 图库原先不是独立配置项，那份清单是从播放列表汇总出来的。因此用户手上
    /// 那份旧 `settings.toml` **没有** `libraries = [...]` 这一行——不补的话，
    /// 升级后界面上的图库页会是空的，而播放列表还在引用那些目录
    /// （图照播，但用户看不到它们、也点不了扫描）。
    ///
    /// 它同时兜住"手改配置只在播放列表里加了目录"这种情况，因此是一条
    /// **幂等**的合并：任何时候读盘都保证"被播放列表引用的一定在图库清单里"。
    /// 反过来的那一半（图库不在任何列表里）是允许的——那正是这次改动新增的用法。
    ///
    /// 顺序：已有的图库在前（保持用户添加的顺序），只能从播放列表里发现的追加在后面。
    /// 判重按大小写不敏感（Windows 路径大小写不敏感）。
    pub fn adopt_missing_libraries(&mut self) {
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut merged: Vec<PathBuf> = Vec::new();
        let from_playlists = self
            .playlists
            .iter()
            .flat_map(|playlist| playlist.libraries.iter());
        for path in self.libraries.iter().chain(from_playlists) {
            if seen.insert(path.to_string_lossy().to_ascii_lowercase()) {
                merged.push(path.clone());
            }
        }
        if merged.len() != self.libraries.len() {
            log::info!(
                "图库清单已补齐：{} 个（其中来自播放列表引用的已并入）",
                merged.len()
            );
        }
        self.libraries = merged;
    }

    /// 读取设置；文件不存在时写出默认文件并返回默认值。
    ///
    /// **解析失败必须报错**，不能悄悄退回默认值：那会让"配置写错"表现成
    /// "功能没实现"，而后者会让人去读一整天代码。
    pub fn load_or_create(path: &Path) -> Result<Settings> {
        if !path.exists() {
            let settings = Settings::default();
            settings.save(path)?;
            log::info!("已写入默认设置：{}", path.display());
            return Ok(settings);
        }

        let text = fs::read_to_string(path)
            .map_err(|e| Error::io(format!("读取设置失败 {}", path.display()), e))?;
        let mut settings = toml_edit::de::from_str::<Settings>(&text).map_err(|error| {
            log::error!("解析设置失败，未应用任何设置：{error}");
            Error::config(
                format!("解析设置失败 {}", path.display()),
                format!("{error}"),
            )
        })?;
        // 旧配置里没有独立的图库清单（那时它是从播放列表汇总出来的），
        // 因此这里补一次，否则升级后"图库页是空的、播放列表却在引用目录"。
        settings.adopt_missing_libraries();
        Ok(settings)
    }

    /// 保存设置（整篇重写，见模块文档）。
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("创建设置目录失败 {}", parent.display()), e))?;
        }
        write_atomic(path, &render_toml(self))
    }
}

/// 把字符串渲染成 TOML 字符串。
///
/// 优先用单引号**字面量**：它不做转义处理，因此任何 Windows 路径都能原样表达，
/// 尤其是形如 `\\.\DISPLAY1` 的显示器名。字面量里不能出现单引号，
/// 因此含单引号时退回双引号并手工转义——路径里带单引号是可能的，
/// 这条分支不能省。
fn toml_string(value: &str) -> String {
    if !value.contains('\'') && !value.contains('\n') && !value.contains('\r') {
        format!("'{value}'")
    } else {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        format!("\"{escaped}\"")
    }
}

/// 字符串数组。
fn toml_string_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    let items: Vec<String> = values.iter().map(|value| toml_string(value)).collect();
    format!("[{}]", items.join(", "))
}

/// 路径数组：多行写，一行一个。图库清单与"播放列表关联了哪些图库"都用它。
///
/// 多行是刻意的：图库多的时候一行会很长，而且用户可能想手改这个文件
/// （去掉或加一个目录），一行一个有 diff 可读。
fn toml_path_array(key: &str, values: &[PathBuf]) -> String {
    if values.is_empty() {
        return format!("{key} = []\n");
    }
    let mut out = format!("{key} = [\n");
    for value in values {
        out.push_str(&format!(
            "    {},\n",
            toml_string(&value.display().to_string())
        ));
    }
    out.push_str("]\n");
    out
}

fn fit_str(fit: FitMode) -> &'static str {
    match fit {
        FitMode::Fill => "fill",
        FitMode::Fit => "fit",
        FitMode::Stretch => "stretch",
        FitMode::Center => "center",
    }
}

/// 文件头的固定说明。
///
/// 注释不保留，因此所有可配置项都必须在这里说清楚——这是"不保留注释"这个
/// 取舍必须付出的代价，不能省。
const HEADER: &str = r#"# WindowsWallPaper 设置
# 本文件位于程序目录的 config/ 下。
#
# 注意：程序保存设置时会整篇重写这个文件，因此在这里手写的注释不会保留。
#
# libraries = [...]   图库清单：放图片的根目录（可以写多个）
#                     增删的入口是界面上「图库」页的「新增图库 / 移除」按钮。
#                     一个图库可以不被任何播放列表引用——那时它不会被播到，
#                     界面上会标成「未被使用」。
#
# [general]
#   interval_seconds  换图间隔（秒），10 ~ 86400。新建播放列表时取这个值做默认值
#   fit_mode          fill | fit | stretch | center
#   （播放顺序固定为随机，界面上不再有顺序/倒序；旧配置里的 order 会被忽略）
#   minimize_to_tray  关闭窗口时是否只最小化到托盘
#   start_minimized   启动时是否不显示主界面（只留托盘图标），默认 true
#   auto_start        启动后是否自动开始轮播，默认 true（**不会再记住上次的暂停状态**）
#   pause_on_fullscreen  某块屏上有程序全屏（含最大化）时只暂停这块屏的轮播，
#                     退出全屏后自动恢复，默认 false。判据见 fullscreen.rs。
#                     它与界面上的手动暂停是两个独立原因，互不解除。
#   nsfw_hotkey       切换 NSFW 模式的全局快捷键，默认 'Ctrl+Alt+S'；空串 = 不注册
#   pause_hotkey      全局"暂停/继续"（所有屏一起），默认 'Ctrl+Alt+P'
#   next_hotkey       全局"下一张"（所有屏各换一张），默认 'Ctrl+Alt+Right'
#   previous_hotkey   全局"上一张"（所有屏各退回一张），默认 'Ctrl+Alt+Left'
#   show_window_hotkey  全局"显示主界面"（把设置窗口翻出来），默认 'Ctrl+Alt+W'
#                     快捷键写法：Ctrl / Alt / Shift / Win 用 + 连接，最后一个是
#                     单个字母、单个数字、F1~F24，或 Left / Right / Space 这类
#                     有名字的键。至少要带一个修饰键。写错只记 warn、不注册。
#                     若被别的程序（例如显卡驱动的"旋转屏幕"）占用，注册会失败，
#                     在界面上换一个写法即可。
#   tray_click_action 单击托盘图标做什么：open-window（打开主界面，默认）|
#                     next-wallpaper（换一张壁纸；此时**双击**打开主界面）
#   cleanup_expanded  设置页「清理程序目录」那张卡片是否展开，默认 true
#                     （用户在界面上折叠一次就记下来，下次进设置页照旧收着）
#   layer_mode        硬件渲染层的宿主窗口形态：toplevel | child（视频壁纸用，当前不启用）
#   volume_percent    视频壁纸的音量，0 ~ 100（当前不启用）
#   muted             是否静音（当前不启用）
#   video_enabled     是否启用视频壁纸（当前固定 false）
#
# [[playlists]]       播放列表：一个列表可关联多个图库
#   id                稳定标识，界面改名字不会让"哪块屏在用哪个列表"失效
#   name              显示名
#   libraries         这个列表用上面 libraries 清单里的哪几个（可以写多个）。
#                     播放列表**只能勾选已有的图库**，添加图库请到「图库」页。
#   interval_seconds  同 [general].interval_seconds
#   fit_mode          同 [general].fit_mode
#
# [monitors.'<显示器名>']   每屏独立设置；显示器名用 --probe-wallpaper 查看
#   键必须用单引号。显示器名形如 \\.\DISPLAY1，其中的反斜杠序列在双引号里
#   不是合法转义，会让整个 monitors 段落解析失败、每屏设置全部失效。
#   current_playlist  这块屏当前用哪个播放列表的 id；空串表示"还没选"，
#                     此时这块屏没有内容可播（**这就是"不轮播"的唯一开关**）
#   nsfw_wallpaper    NSFW 模式时这块屏显示哪张图（空串 = 没设置）
#   single_image      这块屏的固定单图（空串 = 没设置）。**非空时它优先于
#                     播放列表**：这块屏只显示这一张图、不轮播。产物只留一份
#                     （data/output/<屏>-single.bmp），换了源图就覆盖它。
#   label             这块屏在界面上的显示名（空串 = 显示真实显示器名）。
#                     只保留英文/数字/空格/-/_（用户要求不支持中文）。
#                     改它只影响显示：配置键、日志与产物名仍用真实显示器名。
#                     界面入口是「播放」页上双击显示器名。
#   [monitors.'...'.filter]   aspect_min / aspect_max / min_width / min_height
#     长宽比区间是 [aspect_min, aspect_max)：含下限、不含上限。
#     这样相邻区间可以无重叠拼接（例如一屏 [1.3, 无穷)、另一屏 [0, 1.3)）。
#     min_width / min_height 也都是"至少这么大"，含边界值。
#
# [exclude]           排除规则：**只有目录前缀一类**
#   dirs              目录路径前缀（命中即整棵子树跳过）。
#                     界面上的入口是「图库」页的目录树复选框，不必手改这里。
#                     原先的 globs / regexes 已随「排除规则」页一起删除。

"#;

/// 把设置渲染成 TOML 文本。
fn render_toml(settings: &Settings) -> String {
    let mut out = String::from(HEADER);

    // 图库清单是一个**裸键**，因此必须写在所有 `[表]` 之前——TOML 规定裸键
    // 只能出现在第一个表头之前，写到 `[general]` 后面会被算进 general 表里。
    out.push_str(&toml_path_array("libraries", &settings.libraries));

    out.push_str("\n[general]\n");
    out.push_str(&format!(
        "interval_seconds = {}\n",
        settings.general.interval_seconds
    ));
    out.push_str(&format!(
        "fit_mode = {}\n",
        toml_string(fit_str(settings.general.fit_mode))
    ));
    out.push_str(&format!(
        "minimize_to_tray = {}\n",
        settings.general.minimize_to_tray
    ));
    out.push_str(&format!(
        "layer_mode = {}\n",
        toml_string(match settings.general.layer_mode {
            LayerModeSetting::Child => "child",
            LayerModeSetting::Toplevel => "toplevel",
        })
    ));
    // 音量夹在 0..=100：手改过的设置文件可能写着 500，
    // 而 500% 会让音频严重削顶失真。读进来时夹住比拒绝整个文件更合适。
    out.push_str(&format!(
        "volume_percent = {}\n",
        settings.general.volume_percent.min(100)
    ));
    out.push_str(&format!("muted = {}\n", settings.general.muted));
    // 视频开关也写出来：用户在文件里能直接看到它、也方便手工打开。
    out.push_str(&format!(
        "video_enabled = {}\n",
        settings.general.video_enabled
    ));
    // 启动自动接管。写出来让用户能一眼看到它的当前值。
    out.push_str(&format!(
        "auto_start = {}\n",
        settings.general.auto_start
    ));
    // 启动时是否显示主界面。
    out.push_str(&format!(
        "start_minimized = {}\n",
        settings.general.start_minimized
    ));
    // 全屏时按屏暂停。
    out.push_str(&format!(
        "pause_on_fullscreen = {}\n",
        settings.general.pause_on_fullscreen
    ));
    // NSFW 模式快捷键。
    out.push_str(&format!(
        "nsfw_hotkey = {}\n",
        toml_string(&settings.general.nsfw_hotkey)
    ));
    // 另外四个全局快捷键。空串照样写出来——用户要能从文件里看出"它被清空了"。
    out.push_str(&format!(
        "pause_hotkey = {}\n",
        toml_string(&settings.general.pause_hotkey)
    ));
    out.push_str(&format!(
        "next_hotkey = {}\n",
        toml_string(&settings.general.next_hotkey)
    ));
    out.push_str(&format!(
        "previous_hotkey = {}\n",
        toml_string(&settings.general.previous_hotkey)
    ));
    out.push_str(&format!(
        "show_window_hotkey = {}\n",
        toml_string(&settings.general.show_window_hotkey)
    ));
    // 单击托盘图标的行为。
    out.push_str(&format!(
        "tray_click_action = {}\n",
        toml_string(settings.general.tray_click_action.as_str())
    ));
    // 设置页「清理程序目录」那张卡片是否展开。
    out.push_str(&format!(
        "cleanup_expanded = {}\n",
        settings.general.cleanup_expanded
    ));

    for playlist in &settings.playlists {
        out.push_str("\n[[playlists]]\n");
        out.push_str(&format!("id = {}\n", toml_string(&playlist.id)));
        out.push_str(&format!("name = {}\n", toml_string(&playlist.name)));
        // 路径数组。多行写比一行更容易读，尤其是图库多的时候。
        out.push_str(&toml_path_array("libraries", &playlist.libraries));
        out.push_str(&format!("interval_seconds = {}\n", playlist.interval_seconds));
        out.push_str(&format!(
            "fit_mode = {}\n",
            toml_string(fit_str(playlist.fit_mode))
        ));
    }

    for (name, monitor) in &settings.monitors {
        let quoted = toml_string(name);
        out.push_str(&format!("\n[monitors.{quoted}]\n"));
        // 当前播放列表也写出来：它必须持久化，否则重启后每块屏都要重选一次。
        out.push_str(&format!(
            "current_playlist = {}\n",
            toml_string(&monitor.current_playlist)
        ));
        // NSFW 壁纸也写出来：没设置（空串）时不写，让文件干净一点。
        if !monitor.nsfw_wallpaper.is_empty() {
            out.push_str(&format!(
                "nsfw_wallpaper = {}\n",
                toml_string(&monitor.nsfw_wallpaper)
            ));
        }
        // 固定单图同理：非空时这块屏只显示它、不轮播。
        if !monitor.single_image.is_empty() {
            out.push_str(&format!(
                "single_image = {}\n",
                toml_string(&monitor.single_image)
            ));
        }
        // 界面上的显示名。空串（= 用真实显示器名）时不写，让文件干净一点。
        if !monitor.label.is_empty() {
            out.push_str(&format!("label = {}\n", toml_string(&monitor.label)));
        }
        out.push_str(&render_filter(&monitor.filter, &quoted));
    }

    out.push_str(&render_exclude(&settings.exclude));
    out
}

fn render_filter(filter: &FilterSettings, monitor_key: &str) -> String {
    if filter.aspect_min.is_none()
        && filter.aspect_max.is_none()
        && filter.min_width.is_none()
        && filter.min_height.is_none()
    {
        return String::new();
    }
    let mut out = format!("\n[monitors.{monitor_key}.filter]\n");
    if let Some(value) = filter.aspect_min {
        out.push_str(&format!("aspect_min = {value}\n"));
    }
    if let Some(value) = filter.aspect_max {
        out.push_str(&format!("aspect_max = {value}\n"));
    }
    if let Some(value) = filter.min_width {
        out.push_str(&format!("min_width = {value}\n"));
    }
    if let Some(value) = filter.min_height {
        out.push_str(&format!("min_height = {value}\n"));
    }
    out
}

/// 渲染排除规则。只剩下目录前缀一类（见 [`ExcludeSettings`] 的说明）。
fn render_exclude(exclude: &ExcludeSettings) -> String {
    let mut out = "\n[exclude]\n".to_string();
    out.push_str(&format!("dirs = {}\n", toml_string_array(&exclude.dirs)));
    out
}

/// 先写临时文件再改名：中途失败不会留下半截配置。
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| Error::io(format!("写入临时文件失败 {}", tmp.display()), e))?;
    fs::rename(&tmp, path)
        .map_err(|e| Error::io(format!("替换文件失败 {} -> {}", tmp.display(), path.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wwp-config-{tag}-{nanos}.toml"))
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let path = temp_path("create");
        let settings = Settings::load_or_create(&path).unwrap();
        assert!(path.is_file(), "首次加载必须留下一个配置文件");
        assert_eq!(
            settings.general.interval_seconds,
            default_interval_seconds(),
            "默认间隔应当与 default_interval_seconds 一致（新建列表取的就是它）"
        );
        assert!(
            settings.general.start_minimized,
            "默认不显示主界面：壁纸软件不该每次启动都弹一个窗口"
        );

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("interval_seconds"), "默认文件应包含可读的设置项");
        assert!(text.starts_with("# WindowsWallPaper"), "默认文件应带说明头");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn older_settings_without_audio_fields_still_load() {
        // **升级路径的回归测试**：音量与静音是新增字段。
        // 如果它们没有 `#[serde(default)]`，用户手上那份旧 settings.toml
        // 会**整个解析失败**——那比丢一个音量值严重得多：
        // 图库、每屏设置、排除规则会全部退回默认值。
        //
        // 这里写一份"旧版本会写出来的"设置（只有 general 里旧有的键）验证能读进来。
        let path = temp_path("older-no-audio");
        // 故意带上已删除的 order：旧配置文件里有它，它必须被安静忽略，
        // 而不是让整份设置解析失败（那会连图库与每屏设置一起丢）。
        fs::write(
            &path,
            "[general]\ninterval_seconds = 42\norder = 'random'\n\
             fit_mode = 'fill'\nminimize_to_tray = true\nlayer_mode = 'toplevel'\n",
        )
        .unwrap();

        let settings = Settings::load_or_create(&path).expect("旧设置文件必须能读进来");
        assert_eq!(settings.general.interval_seconds, 42, "已有字段要保留");
        assert_eq!(
            settings.general.volume_percent, 100,
            "缺失的音量字段应当取默认值 100（用户没调过就听原声）"
        );
        assert!(!settings.general.muted, "缺失的静音字段默认不静音");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn audio_settings_survive_a_save_and_load_round_trip() {
        // 音量是整数百分比、静音是布尔。渲染再解析必须一模一样，
        // 否则"调了音量、重启变回 100%"这种问题会很难查。
        let path = temp_path("audio-roundtrip");
        let mut settings = Settings::default();
        settings.general.volume_percent = 37;
        settings.general.muted = true;
        settings.save(&path).unwrap();

        let reloaded = Settings::load_or_create(&path).unwrap();
        assert_eq!(reloaded.general.volume_percent, 37);
        assert!(reloaded.general.muted);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn pause_on_fullscreen_survives_a_save_and_load_round_trip() {
        // 这个字段是**手写**进 `render_toml` 的（不是靠 serde 自动生成），
        // 忘了写那一行的后果是"开关打开了、保存也成功、重启却变回关闭"——
        // 与 `libraries` 裸键写错位置那次是同一类问题，因此单独守一条。
        let path = temp_path("pause-on-fullscreen-roundtrip");
        let mut settings = Settings::default();
        assert!(
            !settings.general.pause_on_fullscreen,
            "默认必须是关闭：打开它会让最大化窗口也停住壁纸"
        );
        settings.general.pause_on_fullscreen = true;
        settings.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("pause_on_fullscreen = true"),
            "渲染出来的文件里必须有这一行：\n{text}"
        );

        let reloaded = Settings::load_or_create(&path).unwrap();
        assert!(reloaded.general.pause_on_fullscreen);

        // 反方向也要能往返：关掉之后写出来是 false，读回来也是 false。
        settings.general.pause_on_fullscreen = false;
        settings.save(&path).unwrap();
        assert!(!Settings::load_or_create(&path)
            .unwrap()
            .general
            .pause_on_fullscreen);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn old_settings_without_pause_on_fullscreen_still_load() {
        // 用户手上那份 settings.toml 里没有这个新字段，必须照常读进来（取默认 false）。
        // 缺了 `#[serde(default)]` 会让**整份设置解析失败**——图库、每屏筛选、
        // 排除规则会一起丢，代价远大于多一个字段。
        let path = temp_path("old-without-fullscreen");
        fs::write(&path, "[general]\ninterval_seconds = 42\n").unwrap();
        let settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.general.interval_seconds, 42);
        assert!(!settings.general.pause_on_fullscreen);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn partial_file_gets_defaults_for_missing_fields() {
        let path = temp_path("partial");
        fs::write(&path, "[general]\ninterval_seconds = 42\n").unwrap();
        let settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.general.interval_seconds, 42);
        assert!(settings.monitors.is_empty());
        assert_eq!(
            settings.general.tray_click_action,
            TrayClickAction::OpenWindow,
            "旧配置没有这一项时，托盘单击应当还是「打开主界面」（原来的行为）"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn saving_rewrites_the_file_and_keeps_working_settings() {
        // 旧实现试图"保留用户手写注释"，做法是把新值合并进旧文档；那条路径会把
        // 嵌套表写成数组表，于是保存一次就丢掉全部每屏设置。现在整篇重写：
        // 注释不再保留（统一写在文件头），但**设置一定不丢**。
        let path = temp_path("rewrite");
        let _ = Settings::load_or_create(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let edited = text.replace(
            "interval_seconds = 100",
            "# 我自己的备注：改成 10 分钟\ninterval_seconds = 600",
        );
        fs::write(&path, edited).unwrap();

        let mut settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.general.interval_seconds, 600, "应读到用户改过的值");
        settings.general.minimize_to_tray = false;
        settings.save(&path).unwrap();

        let after = Settings::load_or_create(&path).unwrap();
        assert!(!after.general.minimize_to_tray, "改过的值应被保存");
        assert_eq!(after.general.interval_seconds, 600);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn written_file_reads_back_identically() {
        // 配置模块的护栏测试：写出来的文件必须能被自己读回，且每个字段逐一相等。
        // 上一版的 bug 正是"能写、读回来却静默少一半"。
        let path = temp_path("identical");
        let mut settings = Settings::default();
        settings.general.interval_seconds = 137;
        settings.general.fit_mode = FitMode::Fit;
        // 播放列表：一个带两个图库（含一个带单引号的路径，验证 TOML 转义），
        // 另一个只带一个图库再加一段"没有图库"的边界情形。
        settings.playlists.push(Playlist {
            id: "list1".to_string(),
            name: "工作".to_string(),
            libraries: vec![
                PathBuf::from(r"E:\Pictures\相册"),
                PathBuf::from(r"D:\带有'单引号'\目录"),
            ],
            interval_seconds: 4,
            fit_mode: FitMode::Fill,
        });
        settings.playlists.push(Playlist {
            id: "list2".to_string(),
            name: "晚上（还没有关联图库）".to_string(),
            libraries: Vec::new(),
            ..Default::default()
        });
        settings.monitors.insert(
            r"\\.\DISPLAY1".to_string(),
            MonitorSettings {
                current_playlist: "list1".to_string(),
                filter: FilterSettings {
                    aspect_min: Some(1.5),
                    min_width: Some(1000),
                    ..Default::default()
                },
                nsfw_wallpaper: r"E:\Pictures\nsfw 一号.jpg".to_string(),
                single_image: r"E:\Pictures\单图 一号.png".to_string(),
                label: "LEFT-4K".to_string(),
            },
        );
        settings.monitors.insert(
            r"\\.\DISPLAY2".to_string(),
            MonitorSettings {
                // 这块屏还没选列表——必须能原样往返成空串，
                // 否则重启后它会被"继承"成别的列表，播出一堆用户没指定的图。
                current_playlist: String::new(),
                filter: FilterSettings {
                    aspect_max: Some(0.9),
                    min_height: Some(1000),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        settings.exclude.dirs = vec![r"E:\Pictures\排除我".to_string()];
        settings.general.nsfw_hotkey = "Ctrl+Shift+F12".to_string();
        // 另外四个快捷键也要往返：它们与 nsfw_hotkey 是同一类字段，
        // 但漏写渲染或解析时**不会报错**，只会表现为"设了没生效"。
        settings.general.pause_hotkey = "Ctrl+Alt+P".to_string();
        settings.general.next_hotkey = "Ctrl+Alt+Right".to_string();
        settings.general.previous_hotkey = String::new();
        settings.general.show_window_hotkey = "Ctrl+Shift+F1".to_string();
        // 托盘单击行为：非默认值也要能原样往返（否则"设了换一张、重启又变回打开界面"）。
        settings.general.tray_click_action = TrayClickAction::NextWallpaper;
        // 「清理程序目录」那张卡片折叠过：这个偏好必须记住（用户明确要求），
        // 否则每次进设置页它都重新展开，而那一次展开要盘一遍目录。
        settings.general.cleanup_expanded = false;

        settings.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();

        // 键必须是单引号字面量，否则反斜杠会变成非法转义。
        let expected_key = format!("[monitors.{}]", toml_string(r"\\.\DISPLAY1"));
        assert!(
            text.contains(&expected_key),
            "显示器键应写成 {expected_key}。文件内容：\n{text}"
        );
        assert!(
            !text.contains("[[monitors]]"),
            "不应把 monitors 写成数组表。文件内容：\n{text}"
        );

        let back = Settings::load_or_create(&path).unwrap();
        assert_eq!(back.general.interval_seconds, 137);
        assert_eq!(back.general.fit_mode, FitMode::Fit);
        assert_eq!(back.playlists.len(), 2, "两个播放列表都要往返");
        assert_eq!(back.playlists[0].id, "list1");
        assert_eq!(back.playlists[0].name, "工作");
        assert_eq!(back.playlists[0].libraries.len(), 2, "一个列表可关联多个图库");
        assert_eq!(
            back.playlists[0].libraries[1],
            PathBuf::from(r"D:\带有'单引号'\目录"),
            "带单引号的路径必须原样往返"
        );
        assert_eq!(back.playlists[0].interval_seconds, 4);
        assert!(back.playlists[1].libraries.is_empty(), "空列表也要往返");
        assert_eq!(back.monitors.len(), 2, "两块屏的设置都要往返");

        let one = back.monitors.get(r"\\.\DISPLAY1").expect("键应能往返");
        assert_eq!(one.current_playlist, "list1");
        assert_eq!(one.filter.aspect_min, Some(1.5));
        assert_eq!(one.filter.min_width, Some(1000));
        assert_eq!(
            one.nsfw_wallpaper, r"E:\Pictures\nsfw 一号.jpg",
            "每屏的 NSFW 壁纸必须原样往返（含空格与中文）"
        );
        assert_eq!(
            one.single_image, r"E:\Pictures\单图 一号.png",
            "每屏的固定单图必须原样往返（含空格与中文）"
        );
        assert_eq!(
            one.label, "LEFT-4K",
            "显示名必须原样往返（否则双击改的名字重启就没了）"
        );

        let two = back.monitors.get(r"\\.\DISPLAY2").expect("第二个键也要往返");
        assert_eq!(two.current_playlist, "", "「还没选列表」也要如实往返");
        assert_eq!(two.filter.aspect_max, Some(0.9));
        assert_eq!(two.filter.min_height, Some(1000));
        assert_eq!(two.nsfw_wallpaper, "", "没设 NSFW 壁纸时是空串");
        assert_eq!(two.single_image, "", "没设固定单图时是空串");
        assert_eq!(two.label, "", "没改显示名时是空串（界面据此显示真实显示器名）");

        assert_eq!(back.exclude.dirs, vec![r"E:\Pictures\排除我".to_string()]);
        assert_eq!(back.general.nsfw_hotkey, "Ctrl+Shift+F12");
        assert_eq!(back.general.pause_hotkey, "Ctrl+Alt+P");
        assert_eq!(back.general.next_hotkey, "Ctrl+Alt+Right");
        assert_eq!(
            back.general.previous_hotkey, "",
            "清空一个快捷键也要如实往返（否则它会自己恢复默认值）"
        );
        assert_eq!(
            back.general.show_window_hotkey, "Ctrl+Shift+F1",
            "「显示主界面」快捷键必须原样往返"
        );
        assert_eq!(
            back.general.tray_click_action,
            TrayClickAction::NextWallpaper,
            "托盘单击行为必须原样往返"
        );
        assert!(
            !back.general.cleanup_expanded,
            "「清理程序目录」展开过还是折叠过必须原样往返"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn settings_from_before_the_nsfw_rename_still_load() {
        // **升级路径（1.12.0 → 1.13.0）**：模式从 SAFE 改名成 NSFW 时，
        // 配置字段也跟着改（`safe_hotkey` → `nsfw_hotkey`、`safe_wallpaper` →
        // `nsfw_wallpaper`）。旧文件必须照样读得进来——否则用户升级后
        // "自己设的 NSFW 快捷键变回默认值、每块屏的 NSFW 壁纸全丢"，
        // 而这两样他都得重新配一遍。
        let path = temp_path("legacy-safe-names");
        fs::write(
            &path,
            "[general]\nsafe_hotkey = 'Ctrl+Shift+F9'\n\n\
             [monitors.'\\\\.\\DISPLAY1']\ncurrent_playlist = 'list1'\n\
             safe_wallpaper = 'E:/Pictures/旧名字.jpg'\n",
        )
        .unwrap();

        let settings = Settings::load_or_create(&path).expect("改名前的配置必须能读进来");
        assert_eq!(
            settings.general.nsfw_hotkey, "Ctrl+Shift+F9",
            "旧字段名 safe_hotkey 必须被当成 nsfw_hotkey 读进来"
        );
        assert_eq!(
            settings
                .monitors
                .get(r"\\.\DISPLAY1")
                .expect("这块屏的设置应当还在")
                .nsfw_wallpaper,
            "E:/Pictures/旧名字.jpg",
            "旧字段名 safe_wallpaper 必须被当成 nsfw_wallpaper 读进来"
        );

        // 下一次保存就写成新名字（旧名字不再出现在文件里）。
        settings.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("nsfw_hotkey"), "保存后应当写新字段名：\n{text}");
        assert!(text.contains("nsfw_wallpaper"));
        assert!(
            !text.contains("safe_hotkey") && !text.contains("safe_wallpaper"),
            "旧字段名不该再被写出来：\n{text}"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn old_settings_with_globs_and_per_monitor_exclude_still_load() {
        // **升级路径**：globs / regexes 与每屏 exclude 已被删除。
        // 旧配置里那些字段必须被安静忽略，而不是让整份设置解析失败——
        // 后者会让图库、每屏筛选、排除规则一起丢。
        let path = temp_path("old-exclude");
        fs::write(
            &path,
            "[general]\ninterval_seconds = 42\n\n\
             [monitors.'\\\\.\\DISPLAY1']\ncurrent_playlist = 'list1'\n\n\
             [monitors.'\\\\.\\DISPLAY1'.exclude]\ndirs = ['D:/x']\nglobs = ['**/s.png']\nregexes = ['a\\d']\n\n\
             [exclude]\ndirs = ['E:/keep']\nglobs = ['**/skip/**']\nregexes = ['x']\n",
        )
        .unwrap();

        let settings = Settings::load_or_create(&path).expect("旧配置必须能读进来");
        assert_eq!(
            settings.exclude.dirs,
            vec!["E:/keep".to_string()],
            "保留下来的目录规则要照常读进来"
        );
        assert_eq!(settings.general.interval_seconds, 42);
        assert_eq!(
            settings.monitors.get(r"\\.\DISPLAY1").unwrap().current_playlist,
            "list1"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn double_quoted_monitor_key_with_invalid_escape_is_rejected_loudly() {
        // 记录一个真实踩过的坑：把显示器名放进双引号里在 TOML 里是**非法**的，
        // 因为其中的反斜杠序列不是合法转义。这种写法曾让整个 monitors 表静默
        // 变成空 map，表现为"每屏设置完全不生效"。现在解析失败必须报错。
        let path = temp_path("bad-escape");
        let bad = format!(
            "[general]\ninterval_seconds = 300\n\n[monitors.\"{}\"]\ncurrent_playlist = ''\n",
            r"\\.\DISPLAY1"
        );
        fs::write(&path, bad).unwrap();
        let result = Settings::load_or_create(&path);
        assert!(
            result.is_err(),
            "非法转义必须报错，而不是静默用默认值（那会表现为每屏设置不生效）"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn literal_string_fallback_handles_single_quotes() {
        // 值里含单引号时无法用字面量，必须退回双引号并正确转义。
        assert_eq!(toml_string(r"E:\Pictures"), r"'E:\Pictures'");

        let with_quote = toml_string(r"D:\it's\here");
        assert!(
            with_quote.starts_with('"'),
            "含单引号时应退回双引号：{with_quote}"
        );
        assert_eq!(with_quote, r#""D:\\it's\\here""#);
    }

    #[test]
    fn string_arrays_render_and_read_back() {
        let values = vec![
            r"E:\a".to_string(),
            r"D:\b c".to_string(),
            r"E:\含'引号'".to_string(),
        ];
        let rendered = toml_string_array(&values);
        let text = format!("[exclude]\ndirs = {rendered}\n");
        let parsed: Settings = toml_edit::de::from_str(&text).unwrap();
        assert_eq!(parsed.exclude.dirs, values);
    }

    #[test]
    fn empty_arrays_render_as_empty() {
        assert_eq!(toml_string_array(&[]), "[]");
    }

    #[test]
    fn monitor_label_keeps_english_and_digits_only() {
        // 用户要求"支持英文和数字就行，不需要支持中文"。
        // 中文被丢掉是**有意的**：宁可他看到名字变短，也不要存下一段
        // 界面上显示成乱码式、他自己也认不出的名字。
        assert_eq!(sanitize_monitor_label("LeftMain"), "LeftMain");
        assert_eq!(sanitize_monitor_label("4K-竖屏"), "4K-");
        assert_eq!(
            sanitize_monitor_label("左 屏 2"),
            "2",
            "中文与中文之间的空格不该留下（否则会得到「  2」这种名字）"
        );
        assert_eq!(sanitize_monitor_label("主屏"), "", "整串都是中文时得到空串");
    }

    #[test]
    fn monitor_label_trims_and_collapses_spaces() {
        assert_eq!(sanitize_monitor_label("  Left   4K  "), "Left 4K");
        assert_eq!(sanitize_monitor_label("DISP_1 x"), "DISP_1 x");
    }

    #[test]
    fn monitor_label_is_capped() {
        // 上限是为了不让一个名字把卡片那一行挤爆（那一行还有「主 / 竖屏 / 左」标签）。
        let long = "a".repeat(MONITOR_LABEL_MAX + 10);
        assert_eq!(sanitize_monitor_label(&long).chars().count(), MONITOR_LABEL_MAX);
    }

    #[test]
    fn monitor_label_round_trips_through_the_file() {
        // 显示名与其它每屏字段一样必须能原样往返——它是用户手打的，
        // 丢了会让"重启后名字自己变回去"这种问题非常难查。
        let path = temp_path("label");
        let mut settings = Settings::default();
        settings
            .monitors
            .entry(r"\\.\DISPLAY1".to_string())
            .or_default()
            .label = "LEFT-4K".to_string();
        settings.save(&path).unwrap();

        let back = Settings::load_or_create(&path).unwrap();
        assert_eq!(back.monitors[r"\\.\DISPLAY1"].label, "LEFT-4K");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn library_list_round_trips_in_order() {
        // 图库清单是**独立配置项**（用户要求"只能从图库页新增图库"）。
        // 它必须原样往返，而且顺序要保住——界面按这个顺序列出来，
        // 顺序一跳用户就找不到刚加的那个了。
        let path = temp_path("libraries");
        let mut settings = Settings::default();
        settings.libraries = vec![
            PathBuf::from(r"D:\Pictures\示例图库A"),
            PathBuf::from(r"D:\带有'单引号'\目录"),
            PathBuf::from(r"E:\Pictures"),
        ];
        settings.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        // 裸键必须写在第一个表头之前，否则它会被解析成 `[general]` 里的键，
        // 于是"保存成功、读回来图库却全没了"。
        let libraries_at = text.find("libraries = [").expect("应写出图库清单");
        let general_at = text.find("\n[general]").expect("应写出 general");
        assert!(
            libraries_at < general_at,
            "图库清单必须写在 [general] 之前（TOML 的裸键规则）。文件内容：\n{text}"
        );

        let back = Settings::load_or_create(&path).unwrap();
        assert_eq!(
            back.libraries,
            vec![
                PathBuf::from(r"D:\Pictures\示例图库A"),
                PathBuf::from(r"D:\带有'单引号'\目录"),
                PathBuf::from(r"E:\Pictures"),
            ],
            "图库清单必须原样往返（含中文与单引号），顺序也要保留"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_library_used_by_no_playlist_survives_the_round_trip() {
        // 这次改动新增的用法：图库可以不被任何播放列表引用。
        // 若把清单当成"从播放列表算出来的"，这种图库会在下一次保存时消失。
        let path = temp_path("orphan-library");
        let mut settings = Settings::default();
        settings.libraries = vec![PathBuf::from(r"E:\还没关联到任何列表")];
        settings.playlists.push(Playlist {
            id: "list1".to_string(),
            name: "工作".to_string(),
            libraries: Vec::new(),
            ..Default::default()
        });
        settings.save(&path).unwrap();

        let back = Settings::load_or_create(&path).unwrap();
        assert_eq!(
            back.libraries,
            vec![PathBuf::from(r"E:\还没关联到任何列表")],
            "没被任何列表引用的图库也要留在清单里"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn old_settings_without_library_list_adopt_the_playlist_ones() {
        // **升级路径（一定要留住）**：旧配置里图库不是独立配置项，
        // 那份清单是从播放列表汇总出来的。升级后若不补一次，用户会发现
        // "图库页空了，而播放列表还在引用那些目录"——图照播，但他看不到、
        // 也点不了扫描。
        let path = temp_path("adopt-libraries");
        fs::write(
            &path,
            "[general]\ninterval_seconds = 42\n\n\
             [[playlists]]\nid = 'list1'\nname = '工作'\n\
             libraries = ['E:/Pictures/一号', 'E:/Pictures/二号']\n\
             interval_seconds = 30\nfit_mode = 'fill'\n\n\
             [[playlists]]\nid = 'list2'\nname = '晚上'\n\
             libraries = ['E:/Pictures/二号']\n\
             interval_seconds = 60\nfit_mode = 'fit'\n",
        )
        .unwrap();

        let settings = Settings::load_or_create(&path).expect("旧配置必须能读进来");
        assert_eq!(
            settings.libraries,
            vec![
                PathBuf::from("E:/Pictures/一号"),
                PathBuf::from("E:/Pictures/二号"),
            ],
            "被播放列表引用的目录要并进图库清单，并按首次出现的顺序去重"
        );
        assert_eq!(settings.general.interval_seconds, 42, "其余字段照常读进来");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn duplicate_libraries_are_collapsed() {
        // 大小写不同但指向同一个目录的情况也要去重：Windows 路径大小写不敏感，
        // 留着两份会让界面里出现两行看起来一模一样的图库。
        let path = temp_path("dup-libraries");
        fs::write(
            &path,
            "libraries = ['E:/Pictures', 'e:/pictures']\n\
             [general]\ninterval_seconds = 42\n",
        )
        .unwrap();

        let settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.libraries.len(), 1, "重复项只留第一次出现的那个");

        let _ = fs::remove_file(&path);
    }
}

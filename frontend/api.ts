import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 与后端 wwp_core::StartupInfo 对应。 */
export interface StartupInfo {
  version: string;
  build: string;
  root: string;
  rootSource: string;
  fellBack: boolean;
  fallbackReason: string | null;
  programDir: string;
  configDir: string;
  dataDir: string;
  /** 软件回收站目录（`data/trash`）。回收站页与诊断页都显示它。 */
  trashDir: string;
  logDir: string;
  webviewDataDir: string;
  os: string;
}

/**
 * 一个图库。
 *
 * **它是独立的配置项**：增删的入口是「图库」页的「新增图库 / 移除」，
 * 播放列表只负责勾选其中几个。因此这份清单的顺序就是用户添加的顺序。
 */
export interface LibraryView {
  path: string;
  /** 索引是否已经建立过。 */
  scanned: boolean;
  /** 有几个播放列表在用这个图库。0 表示它还没被任何列表用到（不会被播到）。 */
  usedBy: number;
}

/**
 * 一个播放列表。
 *
 * ## 字段为什么这样分
 *
 * 顺序 / 间隔 / 填充是**内容相关**的属性，因此属于列表；
 * 长宽比筛选属于显示器（见 `MonitorView`）。
 */
export interface PlaylistView {
  id: string;
  name: string;
  /** 关联的图库路径。 */
  libraries: string[];
  intervalSeconds: number;
  fitMode: string;
}

export interface ExcludeView {
  /**
   * 目录前缀规则。图库页的目录树复选框改的就是它。
   *
   * 原先还有 globs / regexes（配套「排除规则」页），已按用户要求删除：
   * 他只通过目录复选框排除目录。
   */
  dirs: string[];
}

export interface SettingsInfo {
  settingsPath: string;
  dataRoot: string;
  rootSource: string;
  fellBack: boolean;
  fallbackReason: string | null;
  /** 播放列表。界面在这里增删改，并给每块屏选当前列表。 */
  playlists: PlaylistView[];
  /**
   * 图库清单（独立配置项，顺序 = 添加顺序）。
   *
   * 增删入口在「图库」页；播放列表只是勾选其中几个。
   */
  libraries: LibraryView[];
  exclude: ExcludeView;
  general: GeneralView;
  /** 每屏的原始设置（已保存的值），键是显示器名。 */
  monitors: Record<string, MonitorView>;
}

export interface GeneralView {
  minimizeToTray: boolean;
  /** 启动后是否自动接管壁纸。 */
  autoStart: boolean;
  /** 启动时是否不显示主界面（只留托盘图标）。 */
  startMinimized: boolean;
  /**
   * 某块屏上有程序全屏（独占全屏 / 无边框全屏 / 最大化）时，
   * **只暂停这块屏**的轮播；退出全屏后自动恢复。默认关闭。
   *
   * 它与界面上的「暂停」是**两个独立原因**，互不解除：
   * 打开它不会影响用户手动暂停的那几块屏，关掉它也不会。
   */
  pauseOnFullscreen: boolean;
  /** NSFW 模式的全局快捷键，例如 Ctrl+Alt+S；空串 = 不注册。 */
  nsfwHotkey: string;
  /** 全局"暂停/继续"（作用于所有屏）；空串 = 不注册。 */
  pauseHotkey: string;
  /** 全局"下一张"（所有屏各换一张）；空串 = 不注册。 */
  nextHotkey: string;
  /** 全局"上一张"（所有屏各退回一张）；空串 = 不注册。 */
  previousHotkey: string;
  /** 全局"显示主界面"（把设置窗口翻出来，不改壁纸）；空串 = 不注册。 */
  showWindowHotkey: string;
  /**
   * 全局"切换播放列表"：每块配好两个列表的屏在两者之间往返一次。
   * 空串 = 不注册。
   */
  switchPlaylistsHotkey: string;
  /**
   * 设置页「清理程序目录」那张卡片是否展开。默认展开（用户要求），
   * 用户在界面上折叠一次就记下来，下次进设置页照旧收着。
   */
  cleanupExpanded: boolean;
  /** 单击托盘图标做什么：`open-window` 或 `next-wallpaper`（后者双击打开主界面）。 */
  trayClickAction: string;
  // 后端的 [general] 里还有 interval_seconds / fit_mode / volume_percent /
  // muted：前两个只在"这块屏没有选播放列表"时兜底（那时它没有候选，值用不到），
  // 后两个只有视频壁纸才用。界面都不需要它们，因此不声明。
  // 播放顺序固定为随机，`order` 这个字段已经从配置里删掉了。
}

/**
 * 新建播放列表时的初值。
 *
 * **必须与 `wwp_core::config::Playlist::default()` 保持一致**：
 * 后端在没收到某个字段时用的就是那份默认值，两处不一致会出现
 * "界面显示 100 秒、存下来却是别的值"这种只在前端看不出的偏差。
 *
 * 100 秒是用户明确要求的默认值。
 */
export const NEW_PLAYLIST_DEFAULTS = {
  intervalSeconds: 100,
  fitMode: "fill",
} as const;

/**
 * 每屏设置的界面视图。
 *
 * 这里**没有** interval / fitMode：它们在播放列表上
 * （内容相关的属性）。长宽比等筛选留在这里，因为那是屏幕的物理属性。
 *
 * 也**没有**"是否参与轮播"：不想让某块屏轮播，就不给它选播放列表。
 */
export interface MonitorView {
  /** 当前选中的播放列表 id。空串表示"还没选"。 */
  currentPlaylist: string;
  /**
   * 「切换播放列表」快捷键在这块屏上往返的两个列表 id。
   *
   * 两个都非空、互不相同、且都还存在，这块屏才参与切换；
   * 当前是 A 就切到 B，否则切到 A。空串 = 这一槽没配。
   */
  switchPlaylistA: string;
  switchPlaylistB: string;
  /** NSFW 模式时这块屏显示哪张图。空串表示没设置。 */
  nsfwWallpaper: string;
  /**
   * 这块屏的**固定单图**。空串表示没设置。
   *
   * 非空时它**优先于播放列表**：这块屏只显示这一张图、不轮播。
   * 产物只留一份（`data/output/<屏>-single.bmp`），换了源图就覆盖它。
   */
  singleImage: string;
  /**
   * 这块屏在界面上的**显示名**（双击显示器名改的就是它）。空串 = 用真实显示器名。
   *
   * 只保留英文/数字/空格/`-`/`_`（用户要求不支持中文），并且**只影响显示**：
   * 配置键、日志与 `data/output/` 里的产物名仍然用真实显示器名。
   */
  label: string;
  aspectMin?: number | null;
  aspectMax?: number | null;
  minWidth?: number | null;
  minHeight?: number | null;
  present?: boolean;
}

/** 与 wwp_core::library::index::IndexStats 对应。 */
export interface IndexStats {
  files_seen: number;
  files_ignored: number;
  playable: number;
  unsupported: number;
  unreadable: number;
  excluded: number;
  unsupported_formats: Record<string, number>;
  unreadable_samples: { path: string; reason: string }[];
}

export interface ScanProgress {
  library: string;
  filesSeen: number;
  candidates: number;
  playable: number;
  unsupported: number;
  unreadable: number;
  reused: number;
  /** 其中有多少条是从**别的图库**索引里沿用的（见 `ScanDone.reusedInherited`）。 */
  reusedInherited: number;
  processed: number;
  current: string;
}

/** 与 commands::ScanStatusView 对应（后端用了 camelCase）。 */
export interface ScanStatusView {
  /** 是否正在扫描。以"线程是否活着"为准。 */
  running: boolean;
  library: string | null;
  /**
   * 已经跑了多少秒。
   *
   * **卡死检测的关键**：如果这个数字长时间不变、而 `progress.processed`
   * 也不动，说明扫描卡住了——用户能据此判断"还在动 vs 死了"。
   */
  elapsedSeconds: number;
  progress: ScanProgress | null;
  /** 是否已经结束（成功、失败或取消）。 */
  finished: boolean;
  /** 结束时的结论文字。 */
  outcome: string | null;
  /**
   * 「全部重新扫描」时：当前是第几个（1 基）。单库扫描时为 null。
   *
   * 单独给两个数字，而不是让界面从进度里推：进度里的候选数是**这一个图库**的，
   * 推不出"一共几个库、现在第几个"。
   */
  batchIndex: number | null;
  /** 「全部重新扫描」时：这一批一共几个图库。 */
  batchTotal: number | null;
}

export interface ScanDone {
  library: string;
  cancelled: boolean;
  error: string | null;
  stats: IndexStats | null;
  entries: number;
  scannedAt: number;
  scanMillis: number;
  reusedEntries: number;
  /** 其中有多少条是从**别的图库**索引里沿用的（"上一级已经是图库了"那种情况）。 */
  reusedInherited: number;
  probedEntries: number;
  removed: number;
  /** 旧索引里没有、这次新出现的条目数。 */
  added: number;
  /** 旧索引里有、但内容变了的条目数。 */
  updated: number;
  unsupportedFormats: [string, number][];
  unreadableSamples: { path: string; reason: string }[];
}

/**
 * 「全部重新扫描」的结果：每个图库一条，外加合计。
 *
 * 与 `ScanDone` 是两个事件（`wwp://scan-all-done` / `wwp://scan-done`）：
 * 单库那个的语义是"这个库扫完了、可播放多少张"，而一次扫全部要回答的是
 * **一批的合计**——新增了什么、删了什么。
 */
export interface ScanAllDone {
  /** 每个图库的结果，顺序与图库清单一致。 */
  libraries: ScanDone[];
  /** 整批被取消（用户点了「取消扫描」）。 */
  cancelled: boolean;
  /** 这一批里没能扫的图库数（目录不存在、权限不够等）。 */
  failed: number;
  added: number;
  updated: number;
  removed: number;
  reused: number;
  /** 其中有多少条是从别的图库索引沿用的。 */
  reusedInherited: number;
  probed: number;
  totalMillis: number;
}

export interface LibraryStats {
  scanned: boolean;
  stats: IndexStats | null;
  entries: number;
  playable: number;
  unsupportedFormats: [string, number][];
  unreadableSamples: { path: string; reason: string }[];
  scannedAt: number;
  scanMillis: number;
  reusedEntries: number;
  /**
   * 沿用其他图库的记录数。**这一项永远是 0**：它是某一次扫描的经过，没有落盘，
   * 因此从已建立的索引里读不出来。扫描结果卡片用的是 `ScanDone.reusedInherited`。
   */
  reusedInherited: number;
  probedEntries: number;
  indexPath: string;
}

export interface LibraryEntry {
  p: string;
  w: number;
  h: number;
  mt: number;
  sz: number;
  or: number;
  fmt: string;
  ok: number;
}

/**
 * `index.jsonl` 里一行的字段含义。
 *
 * **已经搬到 `tooltip-texts.ts`**（`INDEX_FIELDS` / `indexFileHelp`）：
 * 它是一段悬浮提示，而用户要求"所有悬浮提示集中在一个文件里，方便自己改"。
 * 这份说明同时出现在界面上（图库页的悬浮提示）与 `docs/index-jsonl.md`，
 * 两处必须一致——界面上是给"想看一眼索引里到底存了什么"的用户看的，
 * 文档是给想深挖的人看的。
 */

/** 窗口几何信息，用于区分"页面溢出"与"渲染缩放不对"。 */
export interface WindowMetrics {
  /** 窗口客户区的物理像素尺寸（Tauri 报告值）。 */
  innerSize: [number, number];
  /** Tauri 报告的缩放因子。 */
  scaleFactor: number;
  /** 缩放因子是否自洽。 */
  consistent: boolean;
}

/** 与 wwp_core::playback::ScreenState 对应。 */
/**
 * 一块屏的运行状态（对应 Rust 的 `playback::ScreenState`）。
 *
 * **字段名必须是驼峰**：Rust 侧那个结构体带 `#[serde(rename_all = "camelCase")]`，
 * 因此 JSON 里是 `monitorName` / `candidateCount` / `nextInSeconds`。
 *
 * 这里曾经错写成蛇形（`monitor_name` 等）。后果不是编译错误，而是界面
 * 永远显示「尚未换过 / 0 次」——因为按 `s.monitor_name === binding.monitorName`
 * 去查永远查不到，`screenState` 恒为 `undefined`，于是所有取值都走了 `?? 0`
 * 兜底。**类型声明与协议不一致时，TypeScript 不会报错，只会静默给错值。**
 */
export interface ScreenState {
  monitorName: string;
  current: string | null;
  /**
   * **下一张**要换上的是哪张源图（界面上的「下一张」预览）。
   *
   * 它不是随机猜的：后端用与真正抽取完全相同的算式算出来，因此这里显示的
   * 就是随后会换上的那一张（见 `wwp_core::playback::select::Selector::peek_next`）。
   * 固定单图的屏、轮播没在运行时为 `null`。
   */
  next: string | null;
  changes: number;
  /** 最近一次换纸耗时（毫秒）。含义见 Rust 侧同名字段的说明。 */
  lastChangeMillis: number;
  lastError: string | null;
  candidateCount: number;
  nextInSeconds: number;
  /** 这块屏是否被暂停（手动暂停或全屏自动暂停，任一个成立即为真）。 */
  paused: boolean;
  /**
   * 用户**手动**暂停（界面上那个「暂停」按钮）。
   *
   * 按钮该显示"暂停"还是"继续"要看这一位，**不要**看 `paused`：
   * 全屏自动暂停期间用 `paused` 会让按钮显示成"继续"，
   * 而点它什么都不会变（自动那一位还在），看起来就像按钮坏了。
   */
  pausedManual: boolean;
  /** 因为这块屏上有程序全屏而**自动**暂停（设置页里的开关）。 */
  pausedByFullscreen: boolean;
}

/** 与 playback_manager::BindingView 对应。 */
export interface BindingView {
  monitorName: string;
  devicePath: string;
  width: number;
  height: number;
  /**
   * 这块屏在虚拟桌面里的左上角坐标。
   *
   * 卡片的先后顺序由后端按 `(x, y)` 排好——也就是**桌上的左右顺序**，
   * 而不是"主屏排最前"。因此这里只需要按 `bindings` 的顺序渲染。
   */
  x: number;
  y: number;
  primary: boolean;
  candidates: number | null;
  intervalSeconds: number | null;
  /** 这块屏是不是"固定单图"（不轮播）。 */
  singleImage: boolean;
}

export interface PlaybackStatus {
  running: boolean;
  paused: boolean;
  /** 是否处于 NSFW 模式（固定壁纸、不轮播）。 */
  nsfw: boolean;
  /**
   * 各屏状态。
   *
   * **每块屏都一定有一条**（后端按绑定列表补齐），因此界面上可以直接按
   * `monitorName` 查。`current` 为空才是真的"这块屏还没被我们设过壁纸"。
   */
  screens: ScreenState[];
  bindings: BindingView[];
}

export interface ScreenResult {
  monitorName: string;
  outcome: string;
  ok: boolean;
}

export interface ControlReport {
  action: string;
  results: ScreenResult[];
  status: PlaybackStatus;
}

export interface MonitorPatch {
  /** 当前选中的播放列表 id。传空串表示清空（该屏将没有内容可播）。 */
  currentPlaylist?: string;
  /** 「切换播放列表」两个槽位。传空串表示清空这一槽（该屏就不参与切换）。 */
  switchPlaylistA?: string;
  switchPlaylistB?: string;
  /** NSFW 模式时这块屏显示哪张图。传空串表示清空。 */
  nsfwWallpaper?: string;
  /** 这块屏的固定单图。传空串表示清空（回到按播放列表轮播）。 */
  singleImage?: string;
  /** 这块屏的界面显示名。只保留英文/数字/空格/-/_；传空串 = 恢复真实显示器名。 */
  label?: string;
  /** 传 null 表示清除该条件。 */
  aspectMin?: number | null;
  aspectMax?: number | null;
  minWidth?: number | null;
  minHeight?: number | null;
  /** 为 true 时整体替换该屏的筛选条件（这样才能清空某一项）。 */
  filterPresent?: boolean;
}

/** 播放列表的修改。界面负责生成 id（新建时）与保持 id 稳定。 */
export interface PlaylistPatch {
  /** 已存在的列表带上原 id；新建的传空串，后端会分配一个。 */
  id: string;
  name: string;
  libraries: string[];
  intervalSeconds: number;
  fitMode: string;
}

export interface SettingsPatch {
  minimizeToTray?: boolean;
  autoStart?: boolean;
  /** 启动时是否不显示主界面。 */
  startMinimized?: boolean;
  /** 某块屏上有程序全屏（含最大化）时只暂停这块屏的轮播。 */
  pauseOnFullscreen?: boolean;
  /** NSFW 模式的全局快捷键。写错只会记一条日志，不会拒绝保存。 */
  nsfwHotkey?: string;
  /** 全局"暂停/继续"的快捷键。 */
  pauseHotkey?: string;
  /** 全局"下一张"的快捷键。 */
  nextHotkey?: string;
  /** 全局"上一张"的快捷键。 */
  previousHotkey?: string;
  /** 全局"显示主界面"的快捷键。 */
  showWindowHotkey?: string;
  /** 每块屏在两个播放列表之间往返切换的全局快捷键。 */
  switchPlaylistsHotkey?: string;
  /** 「清理程序目录」那张卡片展开还是折叠（纯界面偏好，记在设置里）。 */
  cleanupExpanded?: boolean;
  /** 单击托盘图标做什么：`open-window` | `next-wallpaper`。 */
  trayClickAction?: string;
  monitors?: Record<string, MonitorPatch>;
  /** 排除规则（只剩目录前缀）。给值就整体替换。 */
  exclude?: { dirs: string[] };
  /**
   * 图库清单。给值就**整体替换**（「图库」页的新增/移除走这里）。
   *
   * 规则与 `playlists` 相同：界面一次显示全部、用户直接编辑，整体替换让
   * 后端状态与"界面上现在显示什么"完全一致。移除一个图库时后端会顺手把
   * 播放列表里对它的引用一起清掉。
   */
  libraries?: string[];
  /**
   * 播放列表。给值就**整体替换**。
   *
   * 之所以是整体替换而不是"加一条/删一条"：界面上的列表本来就是
   * 一次显示全部、用户直接编辑的。整体替换让后端逻辑与"界面现在显示什么"
   * 完全一致，不会出现"界面删了第 2 条、后端理解成删第 3 条"这类
   * 顺序依赖的问题。
   */
  playlists?: PlaylistPatch[];
}

/** 图库目录树里的一个目录（不含具体文件）。 */
export interface FolderNode {
  path: string;
  name: string;
  /** 相对图库根的第几层，0 是根的直接子目录。界面靠它缩进。 */
  depth: number;
  /**
   * 这个目录当前是否参与轮播。
   *
   * 已经把"父目录被排除"算进去了，也就是**实际生效**的状态——
   * 复选框直接反映它，用户看到的才是真的。
   */
  included: boolean;
  /** 它自己（不含父级）就直接在排除列表里。 */
  excludedHere: boolean;
  /** 该目录下的图片数量（含子目录）。 */
  imageCount: number;
}

/**
 * 播放顺序固定为随机，因此这里不再有"顺序"选项（用户取消了顺序/倒序）。
 *
 * ## 为什么标签是这四个英文单词
 *
 * 中文全称（"裁切填满" / "完整显示（留黑边）"）太长，会把播放列表那一行挤到折行——
 * 用户明确要求"一个播放列表的信息只占一行"，并把默认那项改名为 `Fill`。
 * 中文含义挪到「填充」标签的悬浮提示里（见 `FIT_MODE_HELP`），一眼需要时悬停即可。
 */
export const FIT_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "fill", label: "Fill" },
  { value: "fit", label: "Fit" },
  { value: "stretch", label: "Stretch" },
  { value: "center", label: "Center" },
];

// 四个填充方式的中文含义（「填充」标签的悬浮提示）已经搬到 `tooltip-texts.ts`
// 的 `TIPS.fit`：它是一段悬浮提示，而用户要求所有悬浮提示集中在一处，方便自己改。

export const EVENT_SCAN_PROGRESS = "wwp://scan-progress";
export const EVENT_SCAN_DONE = "wwp://scan-done";
/** 「全部重新扫描」整批结束。单库扫描发的是 `EVENT_SCAN_DONE`。 */
export const EVENT_SCAN_ALL_DONE = "wwp://scan-all-done";
/**
 * 主窗口被**从隐藏/最小化状态叫到前台**（托盘、全局快捷键、二次启动都算）。
 *
 * 用户 2026-10 的要求："当我使用快捷键唤起主页面时，默认把页面调到播放页，
 * 而不是其他我上次打开过的页面。"窗口只是隐藏、没有卸载，因此前端会一直停在
 * 上次那一页；收到这个事件就切回播放页（见 `App.tsx`）。
 */
export const EVENT_MAIN_WINDOW_SHOWN = "wwp://main-window-shown";
/**
 * 「全部删除」的进度（回收站页的进度条）。
 *
 * 与 `EVENT_SCAN_PROGRESS` 分开：那一个是"正在扫描图库"，这一个说的是
 * "正在把项交给系统回收站"，两者的文案、能否取消都不一样。
 */
export const EVENT_TRASH_EMPTY_PROGRESS = "wwp://trash-empty-progress";
/**
 * 按了一次「切换播放列表」快捷键（事件不带数据）。
 *
 * 快捷键改的是设置里的 `current_playlist`，界面手上那份 `SettingsInfo`
 * 不会自己更新；收到就重新拉一次设置，否则播放页的列表下拉框会一直显示旧值。
 */
export const EVENT_PLAYLISTS_SWITCHED = "wwp://playlists-switched";

export const api = {
  startupInfo: () => invoke<StartupInfo>("startup_info"),
  /**
   * 记录一条由界面发起的**错误**（会被记成 `[ERROR]`）。
   *
   * 只用于真正的失败。诊断信息请用 `logDebug`——混用会让 `[ERROR]`
   * 过滤失去意义：真正的错误会被一堆正常信息淹没，而"错误条数"
   * 这个指标也会一直非零，于是没人再看它。
   */
  logError: (message: string) => invoke<void>("log_error", { message }),
  /**
   * 记录一条诊断信息（debug 级别）。
   *
   * 日志级别开着 Debug（见 main.rs 的说明），所以这些信息照样会落盘，
   * 只是不再冒充错误。
   */
  logDebug: (message: string) => invoke<void>("log_debug", { message }),
  logPath: () => invoke<string>("log_path"),
  settingsInfo: () => invoke<SettingsInfo>("settings_info"),
  libraryStats: (path: string) => invoke<LibraryStats>("library_stats", { path }),
  libraryEntries: (path: string, offset = 0, limit = 200) =>
    invoke<LibraryEntry[]>("library_entries", { path, offset, limit }),
  libraryScan: (path: string, rescan = false) =>
    invoke<void>("library_scan", { path, rescan }),
  /**
   * 一键把**所有**图库各重扫一次（增量）。
   *
   * 返回的是这一批要扫几个图库；结果走 `EVENT_SCAN_ALL_DONE` 事件。
   * 增量是后端定死的（这个按钮的语义就是"只改有变化的部分"），
   * 因此这里没有 `rescan` 参数——想强制全量请用每一行的「全量重扫」。
   */
  libraryScanAll: () => invoke<number>("library_scan_all"),
  libraryCancelScan: (forceAfterSeconds?: number) =>
    invoke<boolean>("library_cancel_scan", { forceAfterSeconds: forceAfterSeconds ?? null }),
  /**
   * 查询当前（或最近一次）扫描状态。
   *
   * 用它而不是只靠事件：进度事件只有图库页在订阅，切到别的页就断了，
   * 用户会以为扫描停了。这份查询在任何页面都能调到。
   */
  libraryScanStatus: () => invoke<ScanStatusView>("library_scan_status"),
  /**
   * 列出某个图库的目录结构（只列目录，不列文件）。
   *
   * `maxDepth` 限制递归深度，避免用户选到 `C:\` 时枚举整块盘。
   */
  libraryFolderTree: (path: string, maxDepth?: number) =>
    invoke<FolderNode[]>("library_folder_tree", { path, maxDepth }),
  /**
   * 弹出系统"选择文件夹"对话框。用户取消返回 `null`。
   *
   * 用系统自带的对话框而不是 Tauri 的 dialog 插件：后者要下载新依赖，
   * 而本项目的约定是不在项目之外产生下载行为。
   */
  pickFolder: (initial?: string) =>
    invoke<string | null>("pick_folder", { initial: initial ?? null }),
  /**
   * 弹出系统"选择一张图片"对话框（NSFW 模式用）。用户取消返回 `null`。
   *
   * 与 `pickFolder` 是两个不同的系统对话框：`SHBrowseForFolderW` 只能选目录。
   */
  pickImage: (initial?: string) =>
    invoke<string | null>("pick_image", { initial: initial ?? null }),
  windowMetrics: () => invoke<WindowMetrics>("window_metrics"),
  /**
   * 一张壁纸的缩略图（data URL，直接给 `<img src>` 用）。
   *
   * 由后端把原图压成 360 px 的 JPEG 并按内容缓存（`data/wallpaper-thumbs/`）：
   * 图库里很多是 30 MP 的照片，让 WebView 自己解码会吃掉大量内存与时间。
   *
   * **同一张图只该请求一次**：界面按路径缓存结果（见 `components/WallpaperShot.tsx`），
   * 轮播状态每秒轮询一次，但路径不变时不会重复调用这个命令。
   */
  wallpaperThumbnail: (path: string) => invoke<string>("wallpaper_thumbnail", { path }),
  /**
   * 把一段文本写进系统剪贴板。
   *
   * 用后端的 Win32 剪贴板而不是前端的 `navigator.clipboard`：后者在 WebView2 里
   * 依赖焦点与权限，失败时既不抛错也不写进任何东西，表现成"点了没反应"。
   */
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  /**
   * 在资源管理器里打开一个目录（图库行上的「打开目录」按钮）。
   *
   * 路径整体作为一个参数传给 `explorer`，因此含空格的中文路径不会被拆开。
   * 目录不存在（盘没插）时会返回一句人话，而不是让资源管理器弹一个错误框。
   */
  openFolder: (path: string) => invoke<void>("open_folder", { path }),
  /**
   * 在资源管理器里打开某个文件**所在的目录**，并选中该文件本身。
   *
   * 路径同样整体作为一个参数传（`/select,` 与路径之间不加空格），含空格的中文
   * 路径不会被拆开。文件已经不在了（被移走或删掉）时后端退回"打开它所在的
   * 目录"，只有连目录都没了才报错——用户点这一下想找的是那张图，不是一句报错。
   */
  revealFile: (path: string) => invoke<void>("reveal_file", { path }),

  /**
   * 用系统默认看图程序打开这张图（播放页点"当前壁纸"缩略图）。
   *
   * 与 `revealFile` 是两件事：那个是"在资源管理器里找到它"，这个是"打开看"。
   * 文件已经不在时后端回一句人话，界面照常运行。
   */
  openFile: (path: string) => invoke<void>("open_file", { path }),

  settingsUpdate: (patch: SettingsPatch) => invoke<SettingsInfo>("settings_update", { patch }),
  settingsReload: () => invoke<SettingsInfo>("settings_reload"),

  playbackStatus: () => invoke<PlaybackStatus>("playback_status"),
  /**
   * 启动/重建轮播。
   *
   * `applyImmediately` 为真时**立刻给每屏换一张**（"开始轮播"按钮）；
   * 为假时只重建计划、不动当前壁纸，下一次换纸等一个间隔——
   * "重新加载"按钮走的就是这一条（用户报过"点重新加载会很快地切换一次"，
   * 那正是旧实现无条件 `applyImmediately=true` 的结果）。
   */
  playbackStart: (applyImmediately = true) =>
    invoke<PlaybackStatus>("playback_start", { applyImmediately }),
  playbackControl: (action: "next" | "previous" | "pause" | "resume") =>
    invoke<ControlReport>("playback_control", { action }),
  /** 单独暂停/继续某一块屏。返回 false 表示找不到这块屏。 */
  playbackScreenPause: (monitorName: string, paused: boolean) =>
    invoke<boolean>("playback_screen_pause", { monitorName, paused }),
  /**
   * 单独把某一块屏切到下一张，**另一块屏不动**。
   *
   * 播放页点「下一张」那张预览格走这条。切过去的就是预览里那张：
   * 后端的预览与实际抽取共用同一套算式（见 `Selector::peek_next`）。
   * 返回 false 表示找不到这块屏。
   */
  playbackNextScreen: (monitorName: string) =>
    invoke<boolean>("playback_next_screen", { monitorName }),
  /**
   * NSFW 模式按钮：进入/退出。与全局快捷键走同一段后端逻辑。
   *
   * ⚠️ 这个字符串必须与 `main.rs` 里 `generate_handler!` 的注册名**逐字一致**。
   * 1.13.0 的第一次交付就栽在这里：模式改名时后端命令改成了 `playback_nsfw`，
   * 而这一行漏改，界面上点按钮得到的是 `Command playback_safe not found`——
   * `tsc` 查不出字符串，排版自检也没点到这个按钮。
   * 现在两头都有闸：`scripts/build-package.ps1` 会逐个比对前端调用的命令名与
   * `generate_handler!` 的注册名（构建期），排版自检判据 10 会真的点一次这个按钮
   * 并核对调用的命令名（界面接线）。
   */
  playbackNsfw: () => invoke<NsfwReport>("playback_nsfw"),
  /**
   * 删除这块屏**当前显示**的那张壁纸。
   *
   * 后端做的三件事（顺序就是下面这个）：
   *   1. 把文件**移动**到软件回收站 `data/trash/`（不彻底删除、也不直接丢系统回收站）；
   *   2. 把它从图库索引里删掉（否则下次「重新加载」它又会回到候选里）；
   *   3. 让调度线程把它移出内存候选并换下一张。
   *
   * 返回的那句话（`message`）直接浮给用户看——失败原因只有后端说得准。
   */
  playbackDeleteCurrent: (monitorName: string) =>
    invoke<DeleteReport>("playback_delete_current", { monitorName }),

  /** 软件回收站的规模（设置页显示"里面有 N 项 / X MB"）。 */
  trashInfo: () => invoke<TrashInfo>("trash_info"),
  /** 在资源管理器里打开软件回收站（目录不存在时后端会先建出来）。 */
  trashOpen: () => invoke<void>("trash_open"),
  /**
   * 用系统默认关联的程序打开某一条**里面的那张图**（相册页点缩略图）。
   *
   * 传的是条目 id：路径由后端查出来，因此前端不能借这个命令去开别的文件。
   */
  trashOpenItem: (id: string) => invoke<void>("trash_open_item", { id }),
  /**
   * 回收站里的条目（相册页用），新的在前。
   *
   * 每一条都带 `original`——**删除时记下的原位置**。这正是"以后能恢复回去"
   * 的依据（旧版 1.3.0 删掉的条目没有这个字段，界面上「恢复」会禁用）。
   */
  trashEntries: () => invoke<TrashEntryView[]>("trash_entries"),
  /**
   * 某一条的缩略图，返回可以直接塞进 `<img src>` 的 data URL。
   *
   * 后端把它压成 360 px 的 JPEG 并缓存（`data/trash-thumbs/`）：
   * 图库里很多是 30 MP 的照片，让页面直接加载原图会吃掉大量内存与时间。
   */
  trashThumbnail: (id: string) => invoke<string>("trash_thumbnail", { id }),
  /**
   * 从回收站恢复一张图：放回**原来的位置**。
   *
   * 返回里的 `library` 是它属于哪个图库；恢复之后那张图**不在索引里**
   * （删除时被移掉了），因此界面要顺手调一次 `libraryScan` 让它回到候选里。
   */
  trashRestore: (id: string) => invoke<TrashRestoreReport>("trash_restore", { id }),
  /** 把某一项直接送进**系统回收站**（仍然可以在桌面的回收站里还原）。 */
  trashDelete: (id: string) => invoke<string>("trash_delete", { id }),
  /** 全部删除：回收站里每一样东西都送进**系统回收站**（仍然可以还原）。 */
  trashEmpty: () => invoke<TrashEmptyReport>("trash_empty"),

  diagnosticsExport: () => invoke<DiagnosticsExport>("diagnostics_export"),

  /**
   * 列出程序目录里可以清理的东西（设置页那张卡片用）。
   *
   * **回收站不在清单里**（用户明确要求"不要清空回收站"），设置、图库索引
   * 也不在——它们重建不出来。清单由后端决定，界面只负责显示与勾选。
   */
  cleanupItems: () => invoke<CleanupItem[]>("cleanup_items"),
  /**
   * 清理选中的那几项。传的是 `CleanupItem.id`（不传路径）。
   *
   * 返回里带着"清理之后重新盘出来的清单"，界面直接用 `report.items` 刷新显示，
   * 不必再调一次 `cleanupItems`。
   */
  cleanupRun: (ids: string[]) => invoke<CleanupReport>("cleanup_run", { ids }),
};

/** 与 commands::NsfwReport 对应。 */
export interface NsfwReport {
  /** 给用户看的结果文字（成功，或"哪几块屏没切成、为什么"）。 */
  message: string;
  status: PlaybackStatus;
}

/** 与 commands::DeleteReport 对应（「删除当前壁纸」的结果）。 */
export interface DeleteReport {
  /** 给用户看的一句话结论（已移进回收站 / 轮播没在运行 / 索引未同步）。 */
  message: string;
  /** 被移走的那张图的**原路径**。 */
  removed: string;
  /** 它在软件回收站里的新路径。 */
  stored: string;
  /** 是否同步更新了图库索引（false = 只移了文件，索引留给下次扫描）。 */
  indexUpdated: boolean;
  /** 操作之后的最新轮播状态（界面不必等下一次轮询）。 */
  status: PlaybackStatus;
}

/** 与 commands::TrashInfo 对应。 */
export interface TrashInfo {
  /** 回收站目录（`data/trash`）。设置页显示它并给一个「打开」按钮。 */
  path: string;
  /** 里面的条目数。 */
  count: number;
  /** 合计字节数。 */
  bytes: number;
}

/** 与 commands::TrashEmptyReport 对应。 */
export interface TrashEmptyReport {
  /** 成功交给系统回收站的条目数。 */
  moved: number;
  /** 没能交出去的条目数。 */
  failed: number;
  /** 给用户看的一句话结论。 */
  message: string;
}

/** 与 commands::TrashEmptyProgress 对应（「全部删除」的进度）。 */
export interface TrashEmptyProgress {
  /** 已经处理完的条目数（含失败的）。 */
  done: number;
  /** 总共要处理多少条。后端每推一次都带上，界面不必自己算。 */
  total: number;
}

/** 与 commands::TrashEntryView 对应（回收站相册里的一张图）。 */
export interface TrashEntryView {
  /** 条目标识。恢复/删除/取缩略图都把它传回后端。 */
  id: string;
  /** 原始文件名。 */
  name: string;
  /** 删除前的完整路径。旧版（1.3.0）删掉的条目为 null。 */
  original: string | null;
  /** 原位置现在是不是已经有一个同名文件（恢复时会另存一个名字）。 */
  originalExists: boolean;
  /** 能不能恢复（旧版条目不知道原位置，因此不能）。 */
  canRestore: boolean;
  bytes: number;
  /** 删除时间（Unix 秒）。界面自己格式化。 */
  deletedAt: number;
}

/** 与 commands::TrashRestoreReport 对应。 */
export interface TrashRestoreReport {
  /** 给用户看的一句话结论（原位置被占用时会说明另存了名字）。 */
  message: string;
  /** 最终落到哪里。 */
  target: string;
  /** 原本要放回去的位置。 */
  original: string;
  renamed: boolean;
  /** 这张图属于哪个图库（据此做一次增量重扫）；不属于任何图库时为 null。 */
  library: string | null;
}

/** 与 `wwp_core::cleanup::CleanupItem` 对应：一项可以清理的东西。 */
export interface CleanupItem {
  /** 稳定标识。勾选后回传的就是它（**不传路径**：前端不该有拼路径的能力）。 */
  id: string;
  label: string;
  /** 一句话说明"这是什么、删了会怎样"。 */
  detail: string;
  /** 实际路径（只用于显示）。 */
  path: string;
  bytes: number;
  files: number;
  /** `directory`（整个目录删掉）| `truncate`（清空文件、保留文件本身）| `files`（逐个删）。 */
  kind: string;
  /** 现在能不能删。程序正在用着的目录为 false。 */
  removable: boolean;
  /** 不能删的原因（`removable` 为 false 时才有）。 */
  blockedReason: string | null;
}

/** 与 `wwp_core::cleanup::CleanupOutcome` 对应：一项的删除结果。 */
export interface CleanupOutcome {
  id: string;
  label: string;
  ok: boolean;
  freed: number;
  error: string | null;
}

/** 与 `wwp_core::cleanup::CleanupReport` 对应。 */
export interface CleanupReport {
  outcomes: CleanupOutcome[];
  freed: number;
  /** 清理**之后**重新盘出来的清单。界面拿它刷新每一项的大小。 */
  items: CleanupItem[];
}

export interface DiagnosticsExport {  directory: string;
  files: string[];
  environment: {
    os: { display: string; build: number; revision: number };
    adapters: Array<{ name: string; driverVersion: string; memoryBytes: number }>;
    monitors: Array<{
      name: string;
      width: number;
      height: number;
      x: number;
      y: number;
      primary: boolean;
      dpi: number;
      scalePercent: number;
      devicePath: string;
    }>;
    webview2Version: string;
    desktopTopology: string;
    desktopLayer: string;
    appVersion: string;
    dataRoot: string;
    rootSource: string;
  };
}

export function onScanProgress(handler: (payload: ScanProgress) => void): Promise<UnlistenFn> {
  return listen<ScanProgress>(EVENT_SCAN_PROGRESS, (event) => handler(event.payload));
}

export function onScanDone(handler: (payload: ScanDone) => void): Promise<UnlistenFn> {
  return listen<ScanDone>(EVENT_SCAN_DONE, (event) => handler(event.payload));
}

/** 「全部重新扫描」整批结束时的结果。 */
export function onScanAllDone(handler: (payload: ScanAllDone) => void): Promise<UnlistenFn> {
  return listen<ScanAllDone>(EVENT_SCAN_ALL_DONE, (event) => handler(event.payload));
}

/**
 * 主窗口被叫到前台时回调一次（事件不带数据）。
 *
 * 用途只有一个：把页面切回播放页（见 `App.tsx`）。
 */
export function onMainWindowShown(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_MAIN_WINDOW_SHOWN, () => handler());
}

/**
 * 按下「切换播放列表」快捷键后回调一次（事件不带数据）。
 *
 * 用途：重新拉一次设置，让播放页的列表下拉框与设置页的槽位跟上。
 */
export function onPlaylistsSwitched(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_PLAYLISTS_SWITCHED, () => handler());
}

/**
 * 「全部删除」的进度：后端每处理完一项推一次。
 *
 * 空回收站时后端直接返回，一个事件都不会发——界面靠"点击时先摆一个 0/total 的
 * 进度条"来保证点下去立刻有反应，不指望这个事件来开个头。
 */
export function onTrashEmptyProgress(
  handler: (payload: TrashEmptyProgress) => void,
): Promise<UnlistenFn> {
  return listen<TrashEmptyProgress>(EVENT_TRASH_EMPTY_PROGRESS, (event) => handler(event.payload));
}

/** 把字节数变成人看得懂的大小。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

/** Unix 秒转本地可读时间。 */
export function formatTime(unixSeconds: number): string {
  if (!unixSeconds) return "从未";
  const date = new Date(unixSeconds * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 后端约定的数据位置来源文案。 */
export function sourceLabel(source: string): string {
  switch (source) {
    case "program-dir":
      return "程序目录（便携模式）";
    case "env-override":
      return "环境变量 WWP_HOME 指定";
    case "dev-project-dir":
      return "工程目录（开发态）";
    case "fallback-localappdata":
      return "系统目录（程序目录不可写，已回退）";
    default:
      return source;
  }
}

/**
 * 排版自检工具（开发用，**不参与交付构建**）。
 *
 * ## 它解决什么问题
 *
 * 这个程序是 Tauri + WebView2 的。想核对界面排版（有没有折行、有没有溢出、
 * 列宽对不对）时，正常的办法是把程序跑起来截图——但有两个硬障碍：
 *
 *   1. **单实例门闩**：用户自己那一份正跑着的时候，我们这份起不来；
 *   2. **WebView2 不接受合成点击**（见 AGENTS §9）：想切到"图库/列表/设置"页
 *      只能临时改 `App.tsx` 的默认页再重新构建，一轮几分钟。
 *
 * 这个入口绕开两件事：它**不启动 Rust 后端**，而是在普通浏览器里把前端的
 * `App` 渲染出来，并给 `window.__TAURI_INTERNALS__.invoke` 装一个返回固定数据的
 * 假后端。页面切换用 DOM 的 `click()`（在真浏览器里是有效的，与 WebView2 不同）。
 *
 * ## 怎么跑
 *
 * ```powershell
 * bunx vite build --config vite.layout.config.ts
 * # 然后对每一页各截一张图：
 * & $chrome --headless=new --disable-gpu --allow-file-access-from-files `
 *     --screenshot=out.png --window-size=1400,950 --virtual-time-budget=4000 `
 *     "file:///<绝对路径>/layout-check-dist/layout-check/index.html?page=library"
 * ```
 *
 * `?page=` 取 `playback` / `library` / `playlists` / `settings` / `diagnostics`。
 *
 * ## 它证明不了什么
 *
 * 这是**排版**自检，不是功能验收：假后端返回的是固定数据，按钮点了也没有真行为。
 * 功能仍然只能靠"真机跑一遍 + 看日志"（见 AGENTS §6）。
 */
import React from "react";
import { createRoot } from "react-dom/client";
import "../frontend/index.css";
import { App } from "../frontend/App";
import type { PlaybackStatus } from "../frontend/api";
// 事件名从 `api.ts` 拿：探针要推的那条事件必须与界面订阅的是**同一个字符串**，
// 抄一份写在探针里迟早会两边对不上。
import { EVENT_TRASH_EMPTY_PROGRESS } from "../frontend/api";

// ---------------------------------------------------------------------------
// 假后端：命令名 → 固定数据
// ---------------------------------------------------------------------------

const LIB_A = "D:\\Pictures\\示例图库A";
const LIB_B = "D:\\Pictures\\示例图库B";
const LIB_C = "D:\\Pictures\\示例图库C";

/** 与前端 `SettingsInfo` 同形。数据刻意做得"难看"：长路径、4 个列表、
 *  一个未扫描、一个未被使用的图库——排版问题都是这些边界暴露出来的。 */
const settingsInfo = {
  settingsPath: "D:\\Apps\\WindowsWallPaper\\config\\settings.toml",
  dataRoot: "D:\\Apps\\WindowsWallPaper",
  rootSource: "program-dir",
  fellBack: false,
  fallbackReason: null,
  playlists: [
    {
      id: "list1",
      name: "工作时的精选",
      libraries: [LIB_A, LIB_B],
      intervalSeconds: 100,
      fitMode: "fill",
    },
    {
      id: "list2",
      name: "随手拍的慢慢看",
      libraries: [LIB_A],
      intervalSeconds: 600,
      fitMode: "fit",
    },
    { id: "list3", name: "竖屏专用", libraries: [], intervalSeconds: 30, fitMode: "center" },
    {
      id: "list4",
      name: "很长很长的一个列表名字用来挤别的控件",
      libraries: [LIB_B],
      intervalSeconds: 100,
      fitMode: "stretch",
    },
  ],
  libraries: [
    { path: LIB_A, scanned: true, usedBy: 2 },
    { path: LIB_B, scanned: true, usedBy: 2 },
    { path: LIB_C, scanned: false, usedBy: 0 },
  ],
  exclude: { dirs: [LIB_A + "\\一些不参与的目录"] },
  general: {
    intervalSeconds: 100,
    fitMode: "fill",
    minimizeToTray: true,
    autoStart: true,
    startMinimized: true,
    pauseOnFullscreen: false,
    nsfwHotkey: "Ctrl+Alt+S",
    pauseHotkey: "Ctrl+Alt+P",
    nextHotkey: "Ctrl+Alt+Right",
    previousHotkey: "Ctrl+Alt+Left",
    showWindowHotkey: "Ctrl+Alt+W",
    switchPlaylistsHotkey: "Ctrl+Alt+Shift+L",
    trayClickAction: "open-window",
    // 「清理程序目录」那张卡片展开着（用户要求默认展开，见 `CleanupCard`）。
    cleanupExpanded: true,
  },
  monitors: {
    "\\\\.\\DISPLAY1": {
      currentPlaylist: "list1",
      switchPlaylistA: "list1",
      switchPlaylistB: "list2",
      nsfwWallpaper: "F:\\BT\\Private\\精选横屏素材\\nsfw.jpg",
      singleImage: "",
      label: "Main",
      aspectMin: 1.3,
      aspectMax: null,
      minWidth: null,
      minHeight: null,
      present: true,
    },
    "\\\\.\\DISPLAY2": {
      currentPlaylist: "list3",
      switchPlaylistA: "list3",
      switchPlaylistB: "list2",
      nsfwWallpaper: "",
      singleImage: "",
      label: "Left",
      aspectMin: null,
      aspectMax: 1,
      minWidth: null,
      minHeight: null,
      present: true,
    },
  },
};

/**
 * 与前端 `PlaybackStatus` **同形**（这里显式标注类型：后面 `?nocand=1` 那个
 * 对照组要改其中的字段，标注之后改错字段名会被 tsc 拦下来）。
 */
const playbackStatus: PlaybackStatus = {
  running: true,
  paused: false,
  nsfw: false,
  bindings: [
    {
      monitorName: "\\\\.\\DISPLAY2",
      devicePath: "\\\\?\\DISPLAY#LEFT#1",
      width: 1440,
      height: 2560,
      x: -1440,
      y: 0,
      primary: false,
      candidates: 1284,
      intervalSeconds: 100,
      singleImage: false,
    },
    {
      monitorName: "\\\\.\\DISPLAY1",
      devicePath: "\\\\?\\DISPLAY#MAIN#1",
      width: 3840,
      height: 2160,
      x: 0,
      y: 0,
      primary: true,
      candidates: 9137,
      intervalSeconds: 100,
      singleImage: false,
    },
  ],
  screens: [
    {
      monitorName: "\\\\.\\DISPLAY2",
      current: LIB_B + "\\2023\\竖构图\\IMG_2291.jpg",
      next: LIB_A + "\\套图 042\\cover_0007.jpg",
      changes: 37,
      lastChangeMillis: 143,
      lastError: null,
      candidateCount: 1284,
      nextInSeconds: 42,
      paused: false,
      pausedManual: false,
      pausedByFullscreen: false,
    },
    {
      monitorName: "\\\\.\\DISPLAY1",
      current: LIB_A + "\\套图 042\\cover_0007.jpg",
      next: LIB_B + "\\2023\\一个很长的目录名用来挤预览格\\IMG_0001_final_version.jpg",
      changes: 118,
      lastChangeMillis: 71,
      lastError: null,
      candidateCount: 9137,
      nextInSeconds: 12,
      paused: false,
      pausedManual: true,
      pausedByFullscreen: false,
    },
  ],
};

/**
 * 回收站相册的假数据：四种情况都要有，排版与按钮状态才算都测到——
 * 普通条目、原位置已被占用的、旧版（没有原位置）的、以及一个超长中文路径。
 */
const trashEntries = [
  {
    id: "1759751415-008.jpg",
    name: "008.jpg",
    original: "D:\\Pictures\\示例图库A\\套图 001\\008.jpg",
    originalExists: false,
    canRestore: true,
    bytes: 8_412_672,
    deletedAt: 1759751415,
  },
  {
    id: "1759751480-IMG_2291.jpg",
    name: "IMG_2291.jpg",
    original: "D:\\Pictures\\示例图库B\\竖构图\\IMG_2291.jpg",
    originalExists: true,
    canRestore: true,
    bytes: 30_115_840,
    deletedAt: 1759751480,
  },
  {
    id: "1759751522-很长的名字很长的名字很长的名字很长的名字很长的名字很长的名字.jpg",
    name: "很长的名字很长的名字很长的名字很长的名字很长的名字很长的名字.jpg",
    original:
      "D:\\Pictures\\示例图库A\\套图 042 某个很长的目录名\\cover_0007_final_version.jpg",
    originalExists: false,
    canRestore: true,
    bytes: 1_204_224,
    deletedAt: 1759751522,
  },
  {
    id: "1759751600-old.jpg",
    name: "old.jpg",
    original: null,
    originalExists: false,
    canRestore: false,
    bytes: 512_000,
    deletedAt: 1759751600,
  },
];

/**
 * 「清理程序目录」那张卡片的假清单（用户 2026-10 的要求：增设清理功能）。
 *
 * 三项刻意各代表一种情况：大目录、单项文件（日志）、以及一项**现在不能清**的
 * （WebView2 的界面缓存正被程序用着）——界面要用禁用复选框把第三种表达出来。
 */
const cleanupItems = [
  {
    id: "id-wallpaper-cache",
    label: "壁纸缓存",
    detail: "轮播与 NSFW 壁纸的中间产物。删除后第一次换纸会重新生成。",
    path: settingsInfo.dataRoot + "\\data\\output",
    bytes: 137_363_456,
    files: 6,
    kind: "directory",
    removable: true,
    blockedReason: null,
  },
  {
    id: "id-logs",
    label: "日志",
    detail: "运行日志与滚动归档。正在写的那个会被清空但保留文件，程序继续正常记录。",
    path: settingsInfo.dataRoot + "\\logs",
    bytes: 2_411_724,
    files: 3,
    kind: "files",
    removable: true,
    blockedReason: null,
  },
  {
    id: "id-webview2",
    label: "界面缓存",
    detail: "WebView2 的渲染缓存。程序运行期间不能清理，退出后再清。",
    path: settingsInfo.dataRoot + "\\webview2",
    bytes: 31_457_280,
    files: 214,
    kind: "directory",
    removable: false,
    blockedReason: "程序正在使用这个目录",
  },
];

/**
 * 缩略图的替身：一张 SVG（真机上是后端生成的 JPEG data URL）。
 *
 * 尺寸可以指定——判据 4 要验"整张图都看得见"（没被 cover 裁掉），
 * 因此回收站那几张刻意一半横图、一半竖图，两种方向都要能完整放进格子里。
 */
function fakeThumb(name: string, width = 360, height = 270): string {
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}">` +
    `<rect width="100%" height="100%" fill="#2b3a4a"/>` +
    `<text x="50%" y="50%" fill="#9fc4e8" font-size="20" font-family="Microsoft YaHei UI" ` +
    `text-anchor="middle" dominant-baseline="middle">${name.slice(0, 14)}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

/**
 * 壁纸过大时后端给的那句原因（`wwp_core::thumb` 里的原话）。
 *
 * 自检要证明的正是"这一整句都看得见"——它原来是 `nowrap` + 省略号，
 * 屏幕上只剩前半句（"这张图太大（12000×8000），为了不占满内…"）。
 */
const OVERSIZED_MESSAGE = "这张图太大（12000×8000），为了不占满内存就不生成缩略图了";

/**
 * `playback_next_screen` 被调用过几次、每次是哪块屏（判据 7 读它）。
 *
 * 放在这里而不是 `fake` 表里：探针要读的是"调用记录"，不是一次返回值。
 */
const nextScreenCalls: string[] = [];

/**
 * 点 NSFW 按钮时前端到底调用了**哪个命令名**（判据 10 读它）。
 *
 * ## 为什么要专门记这个（1.13.0 的第一次交付就栽在这里）
 *
 * `invoke("命令名")` 那个字符串两端没有任何编译期联系：模式改名时后端命令
 * 改成了 `playback_nsfw`，而 `frontend/api.ts` 里那一行漏改，界面上点按钮得到
 * 的是 `Command playback_safe not found`——`tsc` 查不出字符串、排版自检当时也
 * 没点过这个按钮，于是带着这个 bug 交付了。
 *
 * 因此这里把**真实收到的命令名**记下来：探针点一次按钮，就能证明前端送出的名字
 * 与后端注册名一致（后端注册名由 `scripts/build-package.ps1` 的构建期闸核对）。
 */
const nsfwClickCalls: string[] = [];

/**
 * `copy_text` 被调用过几次、每次复制了什么。
 *
 * 「点预览图即复制路径」的探针（`?copyprobe=1`）读它：用户 2026-10 的追加要求
 * 是"点击展示当前壁纸的**图像**本身就能复制路径"，因此要证明调用的确实是
 * `copy_text`、带的确实是那块屏当前显示的那条完整路径。
 */
const copyTextCalls: string[] = [];

/**
 * `reveal_file` 被调用过几次、每次是哪条路径（`?copyprobe=1` / `?trashprobe=1` 读它）。
 *
 * 用户 2026-10 的要求：点播放页的路径行或回收站的文件名，都要"打开该图像原来
 * 所在的文件夹，并选中该文件"，不再复制路径/文件名。探针要证明走的确实是这个
 * 命令、带的确实是那一张图的完整路径。
 */
const revealFileCalls: string[] = [];

/**
 * `open_file` 被调用过几次、每次是哪条路径（`?copyprobe=1` 读它）。
 *
 * 用户 2026-10 的要求：点播放页「当前壁纸」的缩略图，要用系统默认看图程序
 * 打开这张图（不再是复制路径）。
 */
const openFileCalls: string[] = [];

/**
 * `trash_open_item` 被调用过几次、每次是哪一条（`?trashprobe=1` 读它）。
 *
 * 用户 2026-10 的要求："点击回收站的预览图像能够直接打开该图像"。
 * 探针要证明点缩略图走的确实是这个命令、带的确实是这一条的 id。
 */
const trashOpenCalls: string[] = [];

/**
 * 事件名 → 界面注册的回调。
 *
 * 浏览器里没有 Rust 那一侧，事件永远发不出来；但 `listen()` 会带着回调 id 调
 * `plugin:event|listen`，因此在这里把它截下来，探针就能**手动推一次事件**。
 * 「全部删除」的进度条只有这样才验得了：光看进度条出现还不够，得看见它跟着
 * 后端推来的数字走。
 */
const eventHandlers = new Map<string, (event: { payload: unknown }) => void>();

/** 推一个事件给当前页面（等价于 Rust 侧 `app.emit(事件名, 负载)`）。 */
function emitEvent(event: string, payload: unknown) {
  eventHandlers.get(event)?.({ payload });
}

const fake: Record<string, unknown> = {
  startup_info: {
    // 假后端里唯一一处版本号：跟着 package.json 走，否则截图左下角
    // 会写着一个早就过去的版本，看图的人会以为自检跑的是旧构建。
    version: "1.15.0",
    build: "2026-10-16",
    root: settingsInfo.dataRoot,
    rootSource: "program-dir",
    fellBack: false,
    fallbackReason: null,
    programDir: settingsInfo.dataRoot,
    configDir: settingsInfo.dataRoot + "\\config",
    dataDir: settingsInfo.dataRoot + "\\data",
    trashDir: settingsInfo.dataRoot + "\\data\\trash",
    logDir: settingsInfo.dataRoot + "\\logs",
    webviewDataDir: settingsInfo.dataRoot + "\\webview2",
    os: "Windows 11 (26100.1742)",
  },
  settings_info: settingsInfo,
  settings_update: settingsInfo,
  settings_reload: settingsInfo,
  playback_status: playbackStatus,
  playback_start: playbackStatus,
  library_scan_status: {
    running: false,
    library: null,
    elapsedSeconds: 0,
    progress: null,
    finished: true,
    outcome: null,
    batchIndex: null,
    batchTotal: null,
  },
  window_metrics: { innerSize: [1400, 950], scaleFactor: 1, consistent: true },
  /**
   * 壁纸缩略图：假后端回一张 SVG 替身（真机上是后端压出来的 JPEG data URL）。
   * 真机上这个命令按内容缓存，这里只要有内容能让 `<img>` 解码出来即可。
   */
  wallpaper_thumbnail: fakeThumb("壁纸预览"),
  trash_info: {
    path: settingsInfo.dataRoot + "\\data\\trash",
    count: trashEntries.length,
    bytes: trashEntries.reduce((sum, item) => sum + item.bytes, 0),
  },
  trash_entries: trashEntries,
  trash_empty: { moved: trashEntries.length, failed: 0, message: "已把 4 项送进系统回收站（在桌面的「回收站」里可以还原；确认不要了再在那里清空）" },
  trash_delete: "已把「008.jpg」送进系统回收站（在桌面的「回收站」里还能还原）",
  trash_restore: {
    message: "已恢复到原位置：D:\\Pictures\\示例图库A\\套图 001\\008.jpg",
    target: "D:\\Pictures\\示例图库A\\套图 001\\008.jpg",
    original: "D:\\Pictures\\示例图库A\\套图 001\\008.jpg",
    renamed: false,
    library: "D:\\Pictures\\示例图库A",
  },
  trash_open: null,
  // 点缩略图 = 用系统默认程序打开这一张。返回值是 `null`（真机上失败会走异常）。
  trash_open_item: null,
  // 设置页「清理程序目录」那张卡片：展开时立刻去盘清单（用户要求默认展开）。
  cleanup_items: cleanupItems,
  log_path: settingsInfo.dataRoot + "\\logs\\app.log",
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

window.__TAURI_INTERNALS__ = {
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    // `settings_update` 做一次真的合并：这样"改设置"在界面上是可观察的
    // （拖拽排序、新建、删除都能被测出来），而不是永远回同一份假数据。
    if (cmd === "settings_update") {
      const patch = (args?.patch ?? {}) as Record<string, unknown>;
      if (Array.isArray(patch.playlists)) {
        settingsInfo.playlists = patch.playlists as typeof settingsInfo.playlists;
      }
      for (const key of [
        "nsfwHotkey",
        "pauseHotkey",
        "nextHotkey",
        "previousHotkey",
        "showWindowHotkey",
        "switchPlaylistsHotkey",
        "trayClickAction",
      ] as const) {
        if (typeof patch[key] === "string") {
          (settingsInfo.general as unknown as Record<string, unknown>)[key] = patch[key];
        }
      }
      // 每屏的两个切换槽位也做一次真的合并：设置页那两条下拉框改完必须能读回来。
      const monitorPatches = patch.monitors as
        | Record<string, Record<string, unknown>>
        | undefined;
      if (monitorPatches) {
        for (const [name, values] of Object.entries(monitorPatches)) {
          const target = settingsInfo.monitors[name];
          if (!target) continue;
          for (const field of ["switchPlaylistA", "switchPlaylistB"] as const) {
            const value = values[field];
            if (typeof value === "string") target[field] = value;
          }
        }
      }
      // 「清理程序目录」展开/折叠也是"存在设置里"的（用户要求记忆这个选项）：
      // 探针要能证明**界面上那一次切换真的写回去了**。
      if (typeof patch.cleanupExpanded === "boolean") {
        settingsInfo.general.cleanupExpanded = patch.cleanupExpanded;
      }
      // ⚠️ **必须回一个新对象**（真后端每次都是从内存里的设置现拼一份）。
      // 回同一个引用的话，`setSettings(同一个对象)` 会让 React 认为状态没变、
      // 直接跳过重渲染——界面上那次切换就"看着没生效"，而其实已经写回设置了。
      // （判据 9 第一次跑就是这么红的：`CLEAN_SETTING_AFTER=false` 但复选框还在。）
      return { ...settingsInfo, general: { ...settingsInfo.general } };
    }
    if (cmd === "playback_delete_current") {
      return {
        message: "已把「样例.jpg」移进软件回收站，正在换下一张",
        removed: LIB_A + "\\样例.jpg",
        stored: settingsInfo.dataRoot + "\\data\\trash\\样例.jpg",
        indexUpdated: true,
        status: playbackStatus,
      };
    }
    // 「点预览里的下一张 = 只切这块屏」的接线：把收到的显示器名记下来，
    // 由 `?nextprobe=1` 的探针读出来（判据 7）。
    if (cmd === "playback_next_screen") {
      const monitorName = String(args?.monitorName ?? "");
      nextScreenCalls.push(monitorName);
      return true;
    }
    // 「点预览图 = 复制路径」的接线：把收到的文本记下来，
    // 由 `?copyprobe=1` 的探针读出来（判据 7）。回收站页的文件名也走同一条接线。
    if (cmd === "copy_text") {
      copyTextCalls.push(String(args?.text ?? ""));
      return null;
    }
    // 「点路径行 = 打开所在文件夹并选中它」的接线：记下路径。
    // 由 `?copyprobe=1`（播放页）与 `?trashprobe=1`（回收站）的探针读出来。
    if (cmd === "reveal_file") {
      revealFileCalls.push(String(args?.path ?? ""));
      return null;
    }
    // 「点缩略图 = 用系统默认看图程序打开」的接线：记下路径。
    // 由 `?copyprobe=1` 的探针读出来。
    if (cmd === "open_file") {
      openFileCalls.push(String(args?.path ?? ""));
      return null;
    }
    // 「点回收站缩略图 = 打开原图」的接线：把收到的 id 记下来，
    // 由 `?trashprobe=1` 读出（判定点的是**这一条**，不是随便哪一条）。
    if (cmd === "trash_open_item") {
      trashOpenCalls.push(String(args?.id ?? ""));
      return null;
    }
    // 事件订阅：`listen()` 发过来的就是这个命令，`handler` 是 `transformCallback`
    // 给的 id，真正的回调躺在 `window["_" + id]` 上。存进 `eventHandlers` 之后，
    // 探针就能用 `emitEvent` 手动推一次（浏览器里没有 Rust 侧，事件不会自己来）。
    if (cmd === "plugin:event|listen") {
      const event = String(args?.event ?? "");
      const id = Number(args?.handler ?? -1);
      const handler = (window as unknown as Record<string, unknown>)[`_${id}`];
      if (typeof handler === "function") {
        eventHandlers.set(event, handler as (event: { payload: unknown }) => void);
      }
      return 1;
    }
    // `?trashprobe=1` 下的「全部删除」**故意不返回**：真实后端要一项一项送进系统
    // 回收站、跑好几秒，假后端一毫秒就返回，进度条根本来不及出现在屏幕上。
    // 挂住这个 Promise，删除中的状态才能被探针量到。
    if (cmd === "trash_empty" && params.get("trashprobe") === "1") {
      return new Promise(() => {});
    }
    // 缩略图：按 id 现造一张 SVG 替身（真机上是后端生成的 JPEG data URL）。
    // 偶数条横图、奇数条竖图——判据 4 要验两种方向都被完整放进格子里
    // （`object-fit: contain`），只要有一种被裁掉就会非零退出。
    if (cmd === "trash_thumbnail") {
      const id = String(args?.id ?? "");
      const index = trashEntries.findIndex((item) => item.id === id);
      const name = trashEntries.find((item) => item.id === id)?.name ?? id;
      return index % 2 === 1 ? fakeThumb(name, 270, 360) : fakeThumb(name, 360, 270);
    }
    // 点 NSFW 按钮走的是 `playback_nsfw`（模式从 SAFE 改名时一起改的）。
    // 这里把**收到的命令名**记下来：假后端不认识 `playback_safe`，
    // 因此探针能一眼看出"前端送的名字还是旧的"——真机上那正是
    // `Command playback_safe not found` 的来源。
    //
    // ⚠️ 这一支必须在下面那个 `cmd in fake` 的早退**之前**：`playback_nsfw`
    // 不在 `fake` 表里，放到后面就永远走不到（第一版就是这么写的，
    // 判据 10 当场测出"点了按钮却调用 0 次"）。
    if (cmd === "playback_nsfw") {
      nsfwClickCalls.push(cmd);
      return {
        message: "已进入 NSFW 模式（固定壁纸，已暂停轮播）",
        status: playbackStatus,
      };
    }
    if (!(cmd in fake)) {
      // 没被打桩的命令：回一个空对象，界面不会因此崩。
      return null;
    }
    // `?shoterror=1`：把缩略图那一步做成"这张图太大"的**真实失败**，用来验
    // 预览框里那句说明有没有被截断（用户 2026-10 报的"本来是展示预览图的地方
    // 现在是半句话"）。这句话与 `wwp_core::thumb` 里那句逐字一致。
    if (cmd === "wallpaper_thumbnail" && params.get("shoterror") === "1") {
      return Promise.reject(OVERSIZED_MESSAGE);
    }
    return fake[cmd];
  },
  transformCallback: (callback: unknown) => {
    const id = Math.floor(Math.random() * 1e9);
    (window as unknown as Record<string, unknown>)[`_${id}`] = callback;
    return id;
  },
  unregisterCallback: () => {},
  convertFileSrc: (path: string) => path,
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  plugins: {},
};

// ---------------------------------------------------------------------------
// 渲染 + 切页
// ---------------------------------------------------------------------------

const params = new URLSearchParams(window.location.search);
const PAGES = ["playback", "library", "playlists", "trash", "settings", "diagnostics"];
const wanted = params.get("page") ?? "playback";
const index = Math.max(0, PAGES.indexOf(wanted));

// `?rows=N`：把播放列表补到 N 条，用来制造"页面刚好放不下"的情形
// （见下面 measure 模式的说明）。
const extraRows = Number.parseInt(params.get("rows") ?? "0", 10);
if (Number.isFinite(extraRows) && extraRows > settingsInfo.playlists.length) {
  while (settingsInfo.playlists.length < extraRows) {
    const n = settingsInfo.playlists.length + 1;
    settingsInfo.playlists.push({
      id: `list${n}`,
      name: `第 ${n} 个列表`,
      libraries: [LIB_B],
      intervalSeconds: 100,
      fitMode: "fill",
    });
  }
}

// `?nocand=1`：把第二块屏伪造成"筛完一张不剩"的状态——那时**不该**再显示倒计时
// （写着"21 秒后"却永远不换纸，比不写更糟）。判据 5 拿它当对照组：
// 正常情况下两个倒计时，这个情况下只能有一个。必须在**渲染之前**改假数据。
if (params.get("nocand") === "1") {
  playbackStatus.screens[1].candidateCount = 0;
  playbackStatus.screens[1].next = null;
  playbackStatus.bindings[1].candidates = 0;
}

// `?cleanupfolded=1`：让假设置一开始就是**折叠**的——相当于"用户上次折叠过，
// 这次重新进设置页"。那一页打开时就不该有复选框（这就是"记忆这个选项"）。
// 必须在**渲染之前**改假数据：设置是首帧就去取的。
if (params.get("cleanupfolded") === "1") {
  settingsInfo.general.cleanupExpanded = false;
}

// `?toastprobe=1`：让两个"右上角提示"**同时**出现，而且都取最坏情况
// （一条很长的复制提示 + 一张正在扫描的进度卡），用来量它们和「重新加载」的
// 位置关系（判据 8）。必须在**渲染之前**改假数据：扫描状态是首帧就去取的。
if (params.get("toastprobe") === "1") {
  // 最坏情况的提示：路径越长，提示越高、越宽。
  playbackStatus.screens[0].current =
    LIB_A + "\\套图 042 - 一个很长很长的目录名，专门用来把提示气泡撑到最宽\\cover_0007 - 同样很长的文件名.jpg";
  const scanStatus = fake.library_scan_status as unknown as Record<string, unknown>;
  scanStatus.running = true;
  scanStatus.library = LIB_B;
  scanStatus.elapsedSeconds = 92;
  scanStatus.progress = { candidates: 8000, processed: 3400, filesSeen: 21000 };
  scanStatus.finished = false;
  scanStatus.batchIndex = 2;
  scanStatus.batchTotal = 5;
  scanStatus.outcome = null;
}

const root = createRoot(document.getElementById("root")!);
root.render(React.createElement(App));

// 等 App 拿到假数据、渲染出导航之后再点一下对应那一项。
// 用 retry 而不是固定延时：React 的首次渲染与数据到达之间隔了一两个微任务。
let attempts = 0;
const clickNav = () => {
  const navs = document.querySelectorAll<HTMLButtonElement>(".wwp-nav");
  if (navs.length === 0) {
    if (attempts++ < 100) window.setTimeout(clickNav, 30);
    return;
  }
  navs[index]?.click();
  if (params.get("measure") === "1") window.setTimeout(measure, 200);
  if (params.get("dragtest")) window.setTimeout(() => dragTest(params.get("dragtest")), 250);
  if (params.get("deletetest") === "1") window.setTimeout(deleteTest, 250);
  if (params.get("nextprobe") === "1") window.setTimeout(nextTest, 900);
  if (params.get("copyprobe") === "1") window.setTimeout(copyTest, 900);
  if (params.get("toastprobe") === "1") window.setTimeout(toastPopTest, 1200);
  if (params.get("trashprobe") === "1") window.setTimeout(trashProbe, 900);
  if (params.get("diagprobe") === "1") window.setTimeout(diagProbe, 700);
  if (params.get("shotprobe") === "1") window.setTimeout(shotProbe, 900);
  if (params.get("shoterror") === "1") window.setTimeout(shotErrorProbe, 900);
  if (params.get("nameprobe") === "1") window.setTimeout(nameTest, 900);
  if (params.get("cleanupprobe") === "1") window.setTimeout(cleanupProbe, 900);
  if (params.get("nsfwprobe") === "1") window.setTimeout(nsfwClickProbe, 900);
  if (params.get("overflowprobe") === "1") window.setTimeout(overflowProbe, 700);
  if (params.get("pathtest") === "1") window.setTimeout(pathTest, 400);
};
clickNav();

/**
 * `?shotprobe=1`：数一数播放页每块屏的壁纸预览（当前 / 下一张）。
 *
 * 验证的是**接线与排版**：每块屏都有两格、缩略图真的解码出来了、
 * 「下一张」那格带倒计时、路径是省略过的短路径（不是空的），
 * 而且格子内部没有任何横向溢出。
 *
 * 数字判据由 `scripts/check-layout.ps1` 的判据 5 读取。
 */
function shotProbe() {
  // 对照组：把预览整块藏起来，量"没有预览时这一页要滚多少"——
  // 用来判断预览这一块**额外**占掉了多少高度（见 scripts/check-layout.ps1 判据 5）。
  const previewOff = params.get("nopreview") === "1";
  if (previewOff) {
    document
      .querySelectorAll<HTMLElement>(".wwp-shot-grid")
      .forEach((grid) => {
        grid.style.display = "none";
      });
  }

  const shots = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot"));
  const images = Array.from(document.querySelectorAll<HTMLImageElement>(".wwp-shot-img"));
  // `naturalWidth > 0` 才算真的解码出来了（src 设上了但图坏了时它是 0）。
  const loaded = images.filter((image) => image.naturalWidth > 0).length;
  const widths = shots.map((shot) => shot.getBoundingClientRect().width);
  const minWidth = widths.length > 0 ? Math.min(...widths) : 0;
  const maxWidth = widths.length > 0 ? Math.max(...widths) : 0;
  const overflowing = shots.filter((shot) => shot.scrollWidth > shot.clientWidth + 1).length;
  const frames = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot-frame"));
  // 预览框太小就说明被挤扁了（正常应当有一百多像素高）。
  const badFrames = frames.filter(
    (frame) => frame.clientHeight < 60 || frame.clientWidth < 80,
  ).length;
  const badges = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot-badge")).map(
    (badge) => badge.textContent ?? "",
  );
  // 倒计时只剩"秒后"这一种写法了：暂停时**数字照样显示**（后端语义是"计时停住"），
  // 状态字样另占一格（见下面的 `pausedTags`）。
  const countdowns = badges.filter((text) => /秒后/.test(text)).length;
  // 「已暂停」那个状态字样（用户 2026-10 要求它与倒计时**并排**，
  // 而不是把数值替换掉）。假数据里第二块屏是"已单独暂停"，因此应当恰好一个，
  // 而且它那一格的倒计时也必须还在。
  const pausedTags = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot")).filter(
    (shot) => shot.querySelector(".wwp-shot-paused") !== null,
  );
  const pausedWithCountdown = pausedTags.filter((shot) =>
    /秒后/.test(shot.querySelector(".wwp-shot-badge")?.textContent ?? ""),
  ).length;
  const paths = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot-path")).map(
    (node) => node.textContent ?? "",
  );
  // 有图的格子应当显示路径（含分隔符），没图的才显示"尚未换过"这类短说明。
  const pathTexts = paths.filter((text) => /[\\/]/.test(text)).length;

  // 整页高度：`.wwp-content` 是滚动容器，滚动高度超过可视高度就要滚动。
  // 播放页曾经靠"设置另开一页"做到不用滚动看全（设计规格 §10.18），
  // 加上预览之后这里要能看出"到底多出去多少"。
  const content = document.querySelector<HTMLElement>(".wwp-content");
  const scrollHeight = content?.scrollHeight ?? -1;
  const viewHeight = content?.clientHeight ?? -1;

  // --- 用户 2026-10 提的这几条排版要求，都要有数字判据 ---
  //
  // 一、每屏那三个方框（播放列表 / 固定单图 / NSFW 壁纸）必须**同宽**。
  const fieldWidths = Array.from(
    document.querySelectorAll<HTMLElement>(".wwp-monitor-field"),
  ).map((field) => field.getBoundingClientRect().width);
  const fieldSpread =
    fieldWidths.length > 0 ? Math.max(...fieldWidths) - Math.min(...fieldWidths) : -1;

  // 二、数字块（`66 次 · 12 ms` / `520 张` / 固定单图 / 最近错误）：一行装下，
  //     而且整块不被裁掉。
  //
  //     用户 2026-10 把"已切换 / 候选"这两个标签去掉之后，多数小块里**已经没有
  //     `.wwp-stat-key` 了**——标签没了不等于这一条判据可以空转：没有标签可比的
  //     块改成量它自己的高度（一行装得下就是三十来像素，折成两行必然超过 40），
  //     标签还在的块（固定单图 / 最近错误）照旧比标签与数值的基线。
  const stats = Array.from(document.querySelectorAll<HTMLElement>(".wwp-stat"));
  let statsStacked = 0;
  let statsCut = 0;
  for (const stat of stats) {
    const key = stat.querySelector<HTMLElement>(".wwp-stat-key");
    const value = stat.querySelector<HTMLElement>(".wwp-stat-value");
    if (key && value) {
      const gap = Math.abs(value.getBoundingClientRect().top - key.getBoundingClientRect().top);
      if (gap > 6) statsStacked++;
    } else if (stat.getBoundingClientRect().height > 40) {
      statsStacked++;
    }
    if (stat.scrollWidth > stat.clientWidth + 1) statsCut++;
  }

  // 三、「66 次 · 12 ms」「520 张」和末尾那两个比例方框必须在**同一行**上
  //     （用户 2026-10："使两行的信息能够集中到一行，精简页面"），并且
  //     "已切换 / 候选 / 图像比例筛选"这三个词要真的从这一行里消失。
  //
  //     四个数字都是客观的：这一行一共有几块（`MERGED_BLOCKS`）、其中跟第一条
  //     基线同高的有几块（`MERGED_SAME_LINE`）、块与块之间的 top 极差
  //     （`MERGED_SPREAD`），以及这一行里还剩几个标签（`STAT_KEYS`）。
  //     默认假数据下三块都没有标签，所以 **`STAT_KEYS` 应当是 0**——
  //     "固定单图"和"最近错误"才带标签，而这两个状态在默认假数据里都不出现。
  //
  //     页面里有两块屏，就有两行：两行都要量（`_FOLD` 那行数完再数这行）。
  const statsRows = Array.from(document.querySelectorAll<HTMLElement>(".wwp-stats"));
  let mergedBlocks = 0;
  let mergedSameLine = 0;
  let mergedSpread = -1;
  for (const row of statsRows) {
    const items = Array.from(row.children) as HTMLElement[];
    const tops = items.map((item) => Math.round(item.getBoundingClientRect().top));
    if (tops.length === 0) continue;
    const firstTop = Math.min(...tops);
    mergedBlocks += tops.length;
    mergedSameLine += tops.filter((top) => Math.abs(top - firstTop) <= 2).length;
    mergedSpread = Math.max(mergedSpread, Math.max(...tops) - firstTop);
  }
  const statKeys = document.querySelectorAll(".wwp-stats .wwp-stat-key").length;
  // 这一行"有多宽"与"需要多宽"：折行时这两个数字直接说明差多少
  // （内容宽度是各块的**自然**宽度之和——块不会被压缩，`.wwp-stat` 是
  // `flex: 0 0 auto`，比例方框也只在自己内部缩）。
  const firstRow = statsRows[0] ?? null;
  const firstRowItems = firstRow ? (Array.from(firstRow.children) as HTMLElement[]) : [];
  const rowGap = firstRow ? Number.parseFloat(getComputedStyle(firstRow).columnGap) || 0 : 0;
  const rowWidth = firstRow?.clientWidth ?? -1;
  const needWidth =
    firstRowItems.length > 0
      ? Math.round(
          firstRowItems.reduce((sum, item) => sum + item.getBoundingClientRect().width, 0) +
            rowGap * (firstRowItems.length - 1),
        )
      : -1;
  // 比例区间那两个方框合并之后搬进了数字行：两块屏各两个，一共 4 个；
  // 宽度按 `.wwp-aspect-range > .wwp-input` 的 88px 判（`ASPECT_SPREAD` 为 0
  // 表示几个都一样宽）。
  const aspectInputs = Array.from(
    document.querySelectorAll<HTMLElement>(".wwp-stats .wwp-aspect-range input"),
  );
  const aspectWidths = aspectInputs.map((input) => input.getBoundingClientRect().width);
  const aspectSpread =
    aspectWidths.length > 0 ? Math.max(...aspectWidths) - Math.min(...aspectWidths) : -1;

  // 四、量一下每屏那一行里两个按钮各有多宽——网格的按钮列就是按它们定的，
  //     改文案或字号后看这两个数就知道方框还能不能放得下。
  const pickButton = document.querySelector<HTMLElement>(".wwp-monitor-fields .wwp-cell-btn-a");
  const clearButton = document.querySelector<HTMLElement>(".wwp-monitor-fields .wwp-cell-btn-b");
  const actionsWidth =
    pickButton && clearButton
      ? Math.round(pickButton.getBoundingClientRect().width + clearButton.getBoundingClientRect().width)
      : -1;

  // 五、标签与它右边那个方框之间**隔着多少像素**。
  //
  // 用户 2026-10 的两条要求都在这一条上："保持文字和右侧的方框合理紧密接触，
  // 不要让 UI 出现大面积的空白"、"播放列表后面的方框太短了"。
  // 旧实现里标签是一个固定 116px 的列，而"播放列表"四个字只有 52px 宽，
  // 于是文字与方框之间白空着 60 多像素（这个数字就是那 60 多像素）。
  const monitorFields = document.querySelector<HTMLElement>(".wwp-monitor-fields");
  const labelCell = monitorFields?.querySelector<HTMLElement>(".wwp-cell-label");
  const valueCell = monitorFields?.querySelector<HTMLElement>(".wwp-cell-value");
  const labelRect = labelCell?.getBoundingClientRect();
  const valueRect = valueCell?.getBoundingClientRect();
  const labelGap = labelRect && valueRect ? Math.round(valueRect.left - labelRect.right) : -1;

  // 六、模式从 SAFE 更名为 NSFW（用户 2026-10 的要求："程序里所有相关的表述都要
  //     改掉"）。两个数字：那个按钮应当写着 NSFW；整页文字里不该再剩一个 SAFE。
  //     这是"改名有没有漏"的客观判据——漏掉的那一处不会报错，只会在界面上留着旧名字。
  const nsfwButton = Array.from(document.querySelectorAll<HTMLElement>(".wwp-btn")).filter(
    (button) => (button.textContent ?? "").trim() === "NSFW",
  ).length;
  const safeTextLeft = ((document.body.textContent ?? "").match(/SAFE/g) ?? []).length;

  const line = document.createElement("pre");
  line.id = "shotprobe";
  line.textContent =
    `SHOTS=${shots.length} IMAGES=${images.length} LOADED=${loaded} ` +
    `PREVIEW=${previewOff ? "off" : "on"} ` +
    `WIDTH_MIN=${minWidth.toFixed(1)} WIDTH_SPREAD=${(maxWidth - minWidth).toFixed(1)} ` +
    `OVERFLOWING=${overflowing} BAD_FRAMES=${badFrames} ` +
    `COUNTDOWNS=${countdowns} PAUSED_TAGS=${pausedTags.length} ` +
    `PAUSED_WITH_COUNTDOWN=${pausedWithCountdown} PATHS=${pathTexts} ` +
    `SCROLL_H=${scrollHeight} VIEW_H=${viewHeight} SCROLL_BY=${scrollHeight - viewHeight} ` +
    `FIELDS=${fieldWidths.length} FIELD_W=${fieldWidths[0]?.toFixed(1) ?? "-"} ` +
    `FIELD_SPREAD=${fieldSpread.toFixed(1)} LABEL_GAP=${labelGap} ACTIONS=${actionsWidth} ` +
    `STATS=${stats.length} STATS_STACKED=${statsStacked} STATS_CUT=${statsCut} ` +
    `MERGED_BLOCKS=${mergedBlocks} MERGED_SAME_LINE=${mergedSameLine} ` +
    `MERGED_SPREAD=${mergedSpread} STAT_KEYS=${statKeys} ` +
    `MERGED_ROW_W=${rowWidth} MERGED_NEED_W=${needWidth} ` +
    `ASPECT_INPUTS=${aspectInputs.length} ASPECT_W=${aspectWidths[0]?.toFixed(1) ?? "-"} ` +
    `ASPECT_SPREAD=${aspectSpread.toFixed(1)} ` +
    `NSFW_BUTTON=${nsfwButton} SAFE_TEXT_LEFT=${safeTextLeft}`;
  document.body.appendChild(line);
}

/**
 * `?shoterror=1`：预览**生成不出来**时那句说明有没有被截断。
 *
 * 用户 2026-10 的原话："现在当壁纸过大时，不会展示预览，这一点我接受，但是此时的
 * 文字说明看不全，本来应该是展示预览图的地方现在是半句话，这里的文字可以折行，
 * 完整显示相关信息。"
 *
 * 三个数字把这个要求变成客观判据：
 *
 * - `ERR_BOXES`：几个格子在显示这句说明（假后端让四格全都失败）；
 * - `ERR_LINES`：其中折成了**多行**的有几个——一句话根本放不进一行，
 *   所以"折行"是"完整显示"的前提；
 * - `ERR_CLIPPED`：`scrollHeight / scrollWidth` 超出可视框的有几个。
 *   **这才是"看不全"的客观证据**：旧实现是 `nowrap` + 省略号，
 *   它会让 `scrollWidth > clientWidth`（判据 5 的对照组专门验这件事）。
 */
function shotErrorProbe() {
  // 对照组：把那一格按**改之前**的写法（nowrap + 省略号）再排一次，
  // 探针必须能测出"被截断"。只报"一切正常"的判据是自证不了什么的。
  if (params.get("shotnowrap") === "1") {
    const style = document.createElement("style");
    style.textContent =
      ".wwp-shot-empty{white-space:nowrap !important;overflow:hidden !important;" +
      "text-overflow:ellipsis !important;}";
    document.head.appendChild(style);
  }

  const boxes = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot-empty")).map(
    (node) => {
      const style = getComputedStyle(node);
      const lineHeight = Number.parseFloat(style.lineHeight) || 0;
      return {
        text: (node.textContent ?? "").trim(),
        lines: lineHeight > 0 ? Math.round(node.clientHeight / lineHeight) : 0,
        clipped:
          node.scrollHeight > node.clientHeight + 1 || node.scrollWidth > node.clientWidth + 1,
        nowrap: style.whiteSpace === "nowrap",
      };
    },
  );

  const line = document.createElement("pre");
  line.id = "shoterrorprobe";
  line.textContent =
    `ERR_BOXES=${boxes.length} ` +
    `ERR_LINES=${boxes.filter((box) => box.lines > 1).length} ` +
    `ERR_CLIPPED=${boxes.filter((box) => box.clipped).length} ` +
    `ERR_NOWRAP=${boxes.filter((box) => box.nowrap).length} ` +
    `ERR_COMPLETE=${boxes.filter((box) => box.text === OVERSIZED_MESSAGE).length} ` +
    `ERR_FIRST_LINES=${boxes[0]?.lines ?? 0} ` +
    `ERR_TEXT=${boxes[0]?.text ?? "（没有说明文字）"}`;
  document.body.appendChild(line);
}

/**
 * `?nameprobe=1`：**右键**点那行路径 = 复制文件名（不含扩展名）。
 *
 * 用户 2026-10 的要求："我希望使用右键点击这个位置时，能够复制对应图像的文件名，
 * 只复制文件名即可，不需要后缀。"左键那件事（打开所在文件夹并选中）由
 * `?copyprobe=1` / `?nextprobe=1` 守着，这里只验右键这一条：
 *
 * - 右键真的接到了（`NAME_CALLS=1`，走的是 `copy_text`——WebView2 里的
 *   `navigator.clipboard` 失败时既不报错也不写入，见 `commands.rs`）；
 * - 复制的是**文件名**且**没有扩展名**（`NAME_STEM_OK`）；
 * - 提示语说清了复制的是什么（`NAME_TOAST`）；
 * - 那一下**不打开资源管理器**（`NAME_NO_REVEAL`：右键不该顺带触发左键那件事）。
 *
 * 期望值由探针**自己**从界面上那行路径算（用另一套写法：先切扩展名再判断），
 * 与界面里 `fileStem` 的实现互为对照，而不是抄一遍。
 */
function nameTest() {
  const shots = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot"));
  const currents = shots.filter((shot) => shotLabel(shot) === "当前显示");
  const target = currents[0];
  const pathText = target ? shotPath(target) : "";
  const lastSegment = pathText.split(/[\\/]/).pop() ?? "";
  const dot = lastSegment.lastIndexOf(".");
  const expectedStem = dot > 0 ? lastSegment.slice(0, dot) : lastSegment;
  const revealBefore = revealFileCalls.length;

  target
    ?.querySelector<HTMLElement>(".wwp-shot-path")
    ?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));

  window.setTimeout(() => {
    const toast = document.querySelector(".wwp-toast")?.textContent ?? "（没有提示）";
    const line = document.createElement("pre");
    line.id = "nameprobe";
    line.textContent =
      `NAME_CELLS=${currents.length} NAME_CALLS=${copyTextCalls.length} ` +
      `NAME_PATH=${pathText} NAME_EXPECT=${expectedStem} ` +
      `NAME_GOT=${copyTextCalls[0] ?? "-"} ` +
      `NAME_STEM_OK=${copyTextCalls.length === 1 && copyTextCalls[0] === expectedStem} ` +
      `NAME_NO_EXT=${copyTextCalls.length === 1 && !/\.[^.]*$/.test(copyTextCalls[0] ?? "")} ` +
      `NAME_NO_REVEAL=${revealFileCalls.length === revealBefore} ` +
      `NAME_TOAST=${toast}`;
    document.body.appendChild(line);
  }, 500);
}

/**
 * `?nsfwprobe=1`：**真的点一次 NSFW 按钮**，看前端送出的是哪个命令名。
 *
 * ## 这条判据是补出来的（1.13.0 第一次交付漏了它）
 *
 * 模式从 SAFE 改名成 NSFW 时，后端命令改成了 `playback_nsfw`，而
 * `frontend/api.ts` 里 `invoke("playback_safe")` 那一行漏改——`tsc` 把命令名当
 * 普通字符串、查不出来，当时的八条判据里也没有一条点过这个按钮。
 * 结果交付出去的版本里**点 NSFW 按钮只会得到一句 `Command playback_safe not found`**，
 * 是用户点下去才发现的。
 *
 * 因此现在这里真的点一次，并把**假后端收到的命令名**报出来：
 *   - `NSFW_CLICK_CALLS=1`：点一次只调用一次；
 *   - `NSFW_CLICK_CMD=playback_nsfw`：送出的名字就是后端注册的那个
 *     （后端注册名由 `scripts/build-package.ps1` 的构建期闸核对，两头合起来才是完整证据）；
 *   - `NSFW_CLICK_TOAST=` 后端返回的那句话确实浮出来了。
 */
function nsfwClickProbe() {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
    (candidate) => (candidate.textContent ?? "").trim() === "NSFW",
  );
  button?.click();
  window.setTimeout(() => {
    const toast = document.querySelector(".wwp-toast")?.textContent ?? "（没有提示）";
    const line = document.createElement("pre");
    line.id = "nsfwprobe";
    line.textContent =
      `NSFW_BUTTON_FOUND=${Boolean(button)} NSFW_BUTTON_DISABLED=${button?.disabled ?? "-"} ` +
      `NSFW_CLICK_CALLS=${nsfwClickCalls.length} ` +
      `NSFW_CLICK_CMD=${nsfwClickCalls[0] ?? "-"} ` +
      `NSFW_CLICK_TOAST=${toast}`;
    document.body.appendChild(line);
  }, 500);
}

/**
 * `?cleanupprobe=1`：设置页「清理程序目录」那张卡片——**默认展开、折叠要记住**。
 *
 * 用户 2026-10 的要求："设置页清理程序目录默认展开，用户可以手动选择折叠，
 * 记忆这个选项。"三个数字把它拆开：
 *
 * - 进页面时（设置里 `cleanupExpanded = true`）复选框就该在：`CLEAN_OPEN_AT_START`；
 * - 点一下「收起」→ 复选框消失，而且那一次切换**写回了设置**
 *   （`CLEAN_SAVED=false` 假后端里读得到）；
 * - 再点一下「查看可清理项」→ 又展开（`CLEAN_REOPENED`），
 *   证明它是"跟着设置走的开关"而不是一次性的本地状态；
 * - 那一项"现在不能清理"（假数据里的界面缓存）的复选框必须是**禁用**的，
 *   而且旁边写出原因（`CLEAN_BLOCKED=1`）。
 *
 * `?cleanupfolded=1` 是另一半：让假设置一开始就是折叠的（相当于用户上次折叠过、
 * 这次重新进设置页），那时**页面一打开就不该有复选框**——这就是"记忆"。
 */
function cleanupProbe() {
  const checkboxes = () => document.querySelectorAll(".wwp-cleanup-item .wwp-check").length;
  const toggle = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
    (button) => button.textContent === "收起" || button.textContent === "查看可清理项",
  );
  const atStart = checkboxes();
  const startLabel = toggle?.textContent ?? "-";

  // 折叠被记住的那一半（`?cleanupfolded=1`）：页面一打开就不该有复选框，
  // 因此**不点任何东西**，只报初始状态。
  if (params.get("cleanupfolded") === "1") {
    const line = document.createElement("pre");
    line.id = "cleanupprobe";
    line.textContent =
      `CLEAN_FOLDED_AT_START=${atStart} CLEAN_FOLDED_LABEL=${startLabel} ` +
      `CLEAN_FOLDED_SETTING=${settingsInfo.general.cleanupExpanded}`;
    document.body.appendChild(line);
    return;
  }

  // 折叠（此时按钮写着「收起」）。
  toggle?.click();
  window.setTimeout(() => {
    const collapsed = checkboxes();
    const saved = settingsInfo.general.cleanupExpanded;
    // 再展开一次。
    const reopen = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
      (button) => button.textContent === "查看可清理项",
    );
    reopen?.click();
    window.setTimeout(() => {
      const reopened = checkboxes();
      const blocked = Array.from(
        document.querySelectorAll<HTMLInputElement>(".wwp-cleanup-item .wwp-check"),
      ).filter((box) => box.disabled).length;
      const line = document.createElement("pre");
      line.id = "cleanupprobe";
      line.textContent =
        `CLEAN_OPEN_AT_START=${atStart} CLEAN_START_LABEL=${startLabel} ` +
        `CLEAN_COLLAPSED=${collapsed} CLEAN_SETTING_AFTER=${saved} ` +
        `CLEAN_REOPENED=${reopened} CLEAN_BLOCKED=${blocked}`;
      document.body.appendChild(line);
    }, 300);
  }, 300);
}

/**
 * `?overflowprobe=1`：整页找"被挤扁的不可折行文字"，以及页面级的横向溢出。
 *
 * 为什么这么查：用户报过多次"某一行字折行了 / 挤在一起"。那些元素几乎都写着
 * `white-space: nowrap`（`wwp-label` / `wwp-tag` / `wwp-btn` / 字段标签），
 * 因此**挤不下时它们不会折行，而是溢出**——`scrollWidth > clientWidth`
 * 就是"被挤扁了"的客观判据，比人眼看截图可靠。
 *
 * 刻意排除的几类：`.wwp-path-ellipsis` / `.wwp-stat-value` / `.wwp-shot-path` 等
 * **本来就要截断**的显示（它们溢出是设计的一部分）。文字内容再长也不算问题。
 */
function overflowProbe() {
  const page = params.get("page") ?? "playback";
  const root = document.documentElement;

  // 对照组：把那些"不许折行"的文字放大到 34px，它们必然挤不下。
  // 判据 6 拿它证明这条探针**真的能测出被挤扁的文字**——只报"一切都是 0"
  // 是自证不了什么的（本来就没问题也满足）。与判据 1 的对照组同一个道理。
  if (params.get("squeezecontrol") === "1") {
    const style = document.createElement("style");
    style.textContent =
      ".wwp-nav, .wwp-label, .wwp-field-label, .wwp-btn, .wwp-tag, .wwp-stat-key," +
      ".wwp-shot-label { font-size: 34px !important; }";
    document.head.appendChild(style);
  }

  // 页面横向溢出 = 内容比视口宽（右侧会被裁掉，用户看不全）。
  const docOverflow =
    Math.max(root.scrollWidth, document.body.scrollWidth) - root.clientWidth;

  const selectors = [
    // 一、装内容的盒子：nowrap 的文字挤不下时**不会折行**，而是把盒子撑出内容
    // （flex 子项的 `min-width: auto` 保证它自己不被压缩）。因此"盒子溢出"
    // 才是"这一行放不下"的直接证据。
    ".wwp-card",
    ".wwp-row",
    ".wwp-field",
    ".wwp-shot",
    ".wwp-shot-cap",
    ".wwp-shot-frame",
    ".wwp-stats",
    ".wwp-inline",
    ".wwp-value",
    ".wwp-lib-row",
    ".wwp-trash-item",
    // 二、会自己截断的叶子（`overflow: hidden`）：文字比它宽就说明被挤扁了
    //    （截断在这里不是设计的一部分——这些元素的设计是"完整显示"）。
    ".wwp-label",
    ".wwp-field-label",
    ".wwp-tag",
    ".wwp-stat-key",
    ".wwp-shot-label",
    ".wwp-nav",
  ];
  const squeezed: string[] = [];
  for (const selector of selectors) {
    for (const element of Array.from(document.querySelectorAll<HTMLElement>(selector))) {
      if (element.clientWidth <= 0) continue;
      if (element.scrollWidth > element.clientWidth + 1) {
        const text = (element.textContent ?? "").trim().slice(0, 12);
        squeezed.push(`${selector}「${text}」${element.clientWidth}>${element.scrollWidth}`);
      }
    }
  }

  const line = document.createElement("pre");
  line.id = "overflowprobe";
  line.textContent =
    `PAGE=${page} SQUEEZE_CONTROL=${params.get("squeezecontrol") === "1"} ` +
    `DOC_OVERFLOW=${docOverflow} SQUEEZED=${squeezed.length}` +
    (squeezed.length > 0 ? ` DETAIL=${squeezed.slice(0, 4).join(" ; ")}` : "");
  document.body.appendChild(line);
}

/**
 * `?pathtest=1`：长路径"从开头截断"这件事到底哪种写法有效。
 *
 * 项目里 `index.css` 的 `.wwp-path-ellipsis` 用的是
 * `direction: rtl` + `unicode-bidi: plaintext`（AGENTS §7.11 记的做法）。
 * 回收站相册里实测**看到的是开头**（盘符那一段），也就是截断发生在右边——
 * 而能分辨一张图的是**结尾那段目录名**。这里把四种写法并排画出来，
 * 用截图直接看哪一种能保留结尾。
 */
function pathTest() {
  const paths = [
    "D:\\Pictures\\示例图库A\\套图 042\\cover_0007_final_version.jpg",
    "D:\\Apps\\WindowsWallPaper\\data\\trash\\1759751415-008.jpg",
  ];
  const variants: Array<[string, React.CSSProperties, boolean]> = [
    ["A 现用类 rtl+plaintext", {}, true],
    ["B rtl 无 plaintext", { direction: "rtl" }, false],
    ["C rtl + text-align:left", { direction: "rtl", textAlign: "left" }, false],
  ];
  const box = document.createElement("div");
  box.style.cssText =
    "position:fixed;left:240px;top:540px;width:420px;background:#111;color:#ddd;" +
    "font:12px Consolas,monospace;padding:8px;z-index:9999";
  for (const [label, style, useClass] of variants) {
    for (const path of paths) {
      const title = document.createElement("div");
      title.textContent = label;
      title.style.cssText = "color:#7cc;font-size:11px;margin-top:6px";
      const span = document.createElement("span");
      if (useClass) span.className = "wwp-path-ellipsis";
      span.textContent = path;
      Object.assign(span.style, style);
      if (!useClass) {
        span.style.display = "block";
        span.style.overflow = "hidden";
        span.style.textOverflow = "ellipsis";
        span.style.whiteSpace = "nowrap";
      }
      box.appendChild(title);
      box.appendChild(span);
    }
  }
  document.body.appendChild(box);
}

/**
 * `?trashprobe=1`：回收站页上"点一下会发生什么"。
 *
 * 三条要求各有一组数字：
 *
 * - 点缩略图 → 用系统默认程序打开这张图（`TRASH_OPEN_CALLS=1`，
 *   而且带的 id 就是**第一条**那条：`TRASH_OPEN_ID_OK=true`）；
 * - 点文件名 → 打开它原来所在的文件夹并选中它（`TRASH_REVEAL_CALLS=1`、
 *   `TRASH_REVEAL_ORIGINAL_OK=true`，比的是这一条的 `original` 完整路径），
 *   **不再复制文件名**（`TRASH_COPY_CALLS=0`）；
 * - 点「全部删除」→ 进度条立刻出现，并**跟着后端推来的数字走**
 *   （`EMPTY_FILL` 0% → 50% → 100%）。真实后端要一项一项送进系统回收站、
 *   跑好几秒；假后端一毫秒就返回，因此 `invoke` 里对 `trash_empty`
 *   在 `?trashprobe=1` 下故意不返回，把"删除中"这个状态留在屏幕上。
 *
 * 另外两张卡片上的固定事实：每张卡片都有「恢复」与「删除」、顶部有「全部删除」、
 * 旧版条目（没有原位置）的「恢复」是禁用的、卡片上**不再显示原位置**。
 * 缩略图是后端生成的，这里只证明"每个缩略图位置都被填上了"、整张图没被裁掉。
 */
function trashProbe() {
  const cards = document.querySelectorAll(".wwp-trash-item");
  const restore = document.querySelectorAll(".wwp-trash-item .wwp-btn-restore");
  const discard = document.querySelectorAll(".wwp-trash-item .wwp-btn-discard");
  const images = document.querySelectorAll(".wwp-trash-item img");
  const allButton = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
    (button) => button.textContent?.startsWith("全部删除"),
  );
  // 旧版条目（没有原位置）的「恢复」必须禁用。
  const disabledRestore = Array.from(restore).filter((button) =>
    (button as HTMLButtonElement).disabled
  ).length;

  // 缩略图必须能看见**整张图**（用户 2026-10 的要求）：显示出来的宽高比要与原图
  // 一致（说明没有被 cover 裁掉），而且不超出格子。原来 `object-fit: cover` 会把
  // 竖图裁掉上下、横图裁掉左右，恰好把"这张图要不要留"的判断依据切掉。
  let measured = 0;
  let cropped = 0;
  let oversized = 0;
  for (const image of Array.from(document.querySelectorAll<HTMLImageElement>(".wwp-trash-thumb img"))) {
    const cell = image.parentElement;
    const shown = image.getBoundingClientRect();
    const box = cell?.getBoundingClientRect();
    if (!box || shown.width === 0 || shown.height === 0 || image.naturalWidth === 0) continue;
    measured++;
    const shownAspect = shown.width / shown.height;
    const realAspect = image.naturalWidth / image.naturalHeight;
    if (Math.abs(shownAspect - realAspect) > 0.02) cropped++;
    if (shown.width > box.width + 1 || shown.height > box.height + 1) oversized++;
  }

  // 卡片第三行（原位置）已经整行删掉：用户 2026-10 说缩略图下面只留文件名。
  // 原位置仍然记在后端的 `origin.json` 里，只是不再显示——这里数出来应当是 0。
  const originRows = document.querySelectorAll(".wwp-trash-origin").length;
  // 两个"点一下"的目标各有多少个：缩略图（打开原图）与文件名（打开原位置）。
  const openable = document.querySelectorAll(".wwp-trash-openable");
  const copyable = document.querySelectorAll(".wwp-trash-reveal");
  const firstCard = cards[0] as HTMLElement | undefined;
  const firstThumb = firstCard?.querySelector<HTMLElement>(".wwp-trash-openable") ?? null;
  const firstName = firstCard?.querySelector<HTMLElement>(".wwp-trash-reveal") ?? null;
  const expectedId = trashEntries[0]?.id ?? "-";
  // 点文件名要打开的是**这一条原来所在的完整路径**（`original`），不是文件名，
  // 也不是回收站里那份拷贝的路径——用户想找的是它原来在哪儿。
  const expectedOriginal = trashEntries[0]?.original ?? "-";

  // 真的点下去。不直接调 `api.trashOpenItem` / `api.revealFile`：那样证明不了
  // "这一格是可点的"——接线的两头都要验。
  firstThumb?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  firstName?.dispatchEvent(new MouseEvent("click", { bubbles: true }));

  window.setTimeout(() => {
    const toast = document.querySelector(".wwp-toast");
    // 回收站目录的绝对路径（用户 2026-10 要求在这一页显示）。这是**页面级**的
    // 回收站目录，与卡片上的原位置无关——后者已经删掉了。
    const pathRow = Array.from(document.querySelectorAll<HTMLElement>(".wwp-row")).find(
      (row) => row.querySelector(".wwp-label")?.textContent === "位置",
    );
    const shownPath = pathRow?.querySelector(".wwp-value")?.textContent ?? "";

    const readBar = () => {
      const bar = document.querySelector<HTMLElement>(".wwp-progress");
      const fill = document.querySelector<HTMLElement>(".wwp-progress-fill");
      const caption = document.querySelector<HTMLElement>(".wwp-progress-caption");
      return {
        exists: bar !== null,
        path: bar?.getAttribute("role") ?? "-",
        aria: bar?.getAttribute("aria-valuenow") ?? "-",
        caption: caption?.textContent ?? "",
        width: fill?.style.width ?? "-",
      };
    };

    allButton?.click();
    // 等 React 把"删除中"那一帧画出来再量。
    window.setTimeout(() => {
      const start = readBar();
      // 推一次真实后端会推的进度：一半。
      emitEvent(EVENT_TRASH_EMPTY_PROGRESS, { done: 2, total: trashEntries.length });
      window.setTimeout(() => {
        const half = readBar();
        emitEvent(EVENT_TRASH_EMPTY_PROGRESS, {
          done: trashEntries.length,
          total: trashEntries.length,
        });
        window.setTimeout(() => {
          const full = readBar();
          const line = document.createElement("pre");
          line.id = "trashprobe";
          line.textContent =
            `TRASH_CARDS=${cards.length} RESTORE_BUTTONS=${restore.length} ` +
            `DISCARD_BUTTONS=${discard.length} THUMBNAILS=${images.length} ` +
            `ALL_DELETE_BUTTON=${allButton !== undefined} DISABLED_RESTORE=${disabledRestore} ` +
            `THUMBS_MEASURED=${measured} THUMBS_CROPPED=${cropped} THUMBS_OVERSIZED=${oversized} ` +
            `TRASH_PATH_SHOWN=${shownPath !== ""} TRASH_PATH=${shownPath} ` +
            `TRASH_ORIGIN_ROWS=${originRows} OPENABLE=${openable.length} COPYABLE=${copyable.length} ` +
            `TRASH_OPEN_CALLS=${trashOpenCalls.length} ` +
            `TRASH_OPEN_ID_OK=${trashOpenCalls[0] === expectedId} ` +
            `TRASH_REVEAL_CALLS=${revealFileCalls.length} ` +
            `TRASH_REVEAL_ORIGINAL_OK=${revealFileCalls[0] === expectedOriginal} ` +
            `TRASH_COPY_CALLS=${copyTextCalls.length} ` +
            `TRASH_COPY_TOAST=${toast?.textContent ?? "（没有提示）"} ` +
            `EMPTY_ROLE=${start.path} EMPTY_CAPTION=${start.caption} ` +
            `EMPTY_START=${start.exists ? start.width : "（没有进度条）"}/${start.aria} ` +
            `EMPTY_HALF=${half.width}/${half.aria} EMPTY_FULL=${full.width}/${full.aria}`;
          document.body.appendChild(line);
        }, 80);
      }, 80);
    }, 250);
  }, 300);
}

/**
 * `?diagprobe=1`：诊断页那两列"数据存在哪里"。
 *
 * 用户 2026-10 要求诊断页也显示回收站路径（与回收站页同源，都是后端的
 * `trash_info.path` / `StartupInfo::trash_dir`）。
 */
function diagProbe() {
  const rows = Array.from(document.querySelectorAll<HTMLElement>(".wwp-card .wwp-row"));
  const find = (label: string) =>
    rows
      .find((row) => row.querySelector(".wwp-label")?.textContent === label)
      ?.querySelector(".wwp-value")?.textContent ?? "";
  const line = document.createElement("pre");
  line.id = "diagprobe";
  line.textContent =
    `DIAG_ROWS=${rows.length} TRASH_ROW=${find("回收站目录")} ` +
    `CONFIG_ROW=${find("配置目录")} DATA_ROW=${find("数据根目录")}`;
  document.body.appendChild(line);
}

/**
 * `?deletetest=1`：点一下播放页上的「删除当前壁纸」，把弹出的提示读出来。
 *
 * 验证的是**接线上**：按钮 → `playback_delete_current` → 右上角浮层。
 * 后端那三件事（移动文件、改索引、换下一张）由 `cargo test` 与真机验收，
 * 这里证明不了。
 */
function deleteTest() {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
    (b) => b.textContent === "删除当前壁纸",
  );
  button?.click();
  window.setTimeout(() => {
    const toast = document.querySelector(".wwp-toast");
    const line = document.createElement("pre");
    line.id = "deletetest";
    line.textContent =
      `DELETE_BUTTON_FOUND=${button !== undefined} ` +
      `DELETE_ENABLED=${button ? !button.disabled : false} ` +
      `TOAST=${toast?.textContent ?? "（没有提示）"}`;
    document.body.appendChild(line);
  }, 500);
}

/**
 * `?nextprobe=1`：点第一块屏「下一张」那张预览格，读接线结果。
 *
 * 验的是**这一条**（用户 2026-10 的要求："当我点击展示的下一张的壁纸时，
 * 能够直接将对应屏幕的壁纸切换到下一张，另一个屏幕不要动"；1.11.0 又加了
 * 一句"点下面的路径只打开所在文件夹、不切换壁纸"）：
 *
 * - 那一格真的可点（`NEXT_OPENABLE`）；
 * - 点缩略图调用的是 `playback_next_screen`，并且**只带这一块屏的显示器名**
 *   （`NEXT_CALLS` 只有一条、`NEXT_ONLY_FIRST=true`）；
 * - 点下面那行路径**不切换壁纸**，只调 `reveal_file`（`NEXT_REVEAL_OK=true`
 *   且 `NEXT_CALLS` 仍只有一条）；
 * - 提示语说清了"另一块屏不动"。
 *
 * 后端真的换了哪张纸由 `cargo test`（`next_on` 只碰一块屏）与真机验收，
 * 这里证明不了。
 */
/**
 * 一格的标签（`当前显示` / `下一张`）。
 *
 * 探针用它来挑格子，而不是按下标（`shots[0]`、`shots[1]`…）：下标把"后端返回的
 * 屏幕顺序"和"界面渲染的顺序"绑在一起，任何一侧调整都会让探针静默指向错的格子。
 * 标签是用户真的看得见的那行字，按它找最稳。
 */
function shotLabel(shot: HTMLElement): string {
  return shot.querySelector(".wwp-shot-label")?.textContent?.trim() ?? "";
}

/** 一格下面那行路径的文字（真路径被截断显示时也是它）。 */
function shotPath(shot: HTMLElement): string {
  return shot.querySelector(".wwp-shot-path")?.textContent?.trim() ?? "";
}

function nextTest() {
  const shots = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot"));
  // 「下一张」是每块屏的**第二**格。按**标签文字**找，不按下标——下标依赖
  // 后端与界面两侧的排列顺序，标签是用户真的看得见的东西。
  const nextShots = shots.filter((shot) => shotLabel(shot) === "下一张");
  const openable = nextShots.filter((shot) => shot.querySelector(".wwp-shot-openable"));
  const first = nextShots[0];
  const firstCard = first?.closest<HTMLElement>(".wwp-shot-grid")?.parentElement ?? null;
  const monitors = playbackStatus.screens.map((screen) => screen.monitorName);
  // 第一张卡的显示器名：从卡上那句 `data-monitor` 读出来（探针不猜顺序）。
  // 不能读卡上显示的名字：那是用户能改的显示名（假数据里叫"Left"），
  // 与后端认的显示器名（`\\.\DISPLAY2`）对不上，`screens.find` 会落空。
  const firstMonitor =
    firstCard?.querySelector<HTMLElement>(".wwp-shot-grid")?.dataset.monitor ?? "-";
  const firstState = playbackStatus.screens.find((s) => s.monitorName === firstMonitor);
  const expectedNext = firstState?.next ?? "-";
  // 顺序有讲究：先点那行路径（打开文件夹），再点缩略图（切纸）。
  // 提示条只有一个，后浮出来的那句会把前一句顶掉——把切纸放在最后，
  // 读到的才是"点了下一张给了什么反馈"这句。
  //
  // 基线必须取在**两次点击之前**：`nextScreenCalls` 是整场累加的，
  // 取在两次点击之间就成了一句恒假的话（一边是 1、一边是 0）。
  const callsAtStart = nextScreenCalls.length;
  first?.querySelector<HTMLElement>(".wwp-shot-path")
    ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  window.setTimeout(() => {
    const revealToast = document.querySelector(".wwp-toast")?.textContent ?? "（没有提示）";
    first?.querySelector<HTMLElement>(".wwp-shot-openable")
      ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    window.setTimeout(() => {
      const toast = document.querySelector(".wwp-toast")?.textContent ?? "（没有提示）";
      const line = document.createElement("pre");
      line.id = "nextprobe";
      line.textContent =
        `NEXT_SHOTS=${shots.length} NEXT_CELLS=${nextShots.length} ` +
        `NEXT_OPENABLE=${openable.length} ` +
        `NEXT_CALLS=${nextScreenCalls.length} NEXT_FIRST=${nextScreenCalls[0] ?? "-"} ` +
        `NEXT_MONITOR=${firstMonitor} ` +
        `NEXT_ONLY_FIRST=${nextScreenCalls.length === 1 && nextScreenCalls[0] === firstMonitor} ` +
        `NEXT_REVEAL_OK=${revealFileCalls.length === 1 && revealFileCalls[0] === expectedNext} ` +
        `NEXT_REVEAL_NO_SWITCH=${nextScreenCalls.length === callsAtStart + 1} ` +
        `NEXT_REVEAL_TOAST=${revealToast} NEXT_TOAST=${toast}`;
      document.body.appendChild(line);
    }, 400);
  }, 400);
}

/**
 * `?copyprobe=1`：点「当前显示」那格里的**图像本身**与**路径行**，读接线结果。
 *
 * 验的是**这一条**（用户 2026-10 的要求，1.11.0 改过）：
 *
 * - 点缩略图 → 用系统默认看图程序打开这张图（`COPY_OPEN_OK=true`，
 *   调用的是 `open_file`，带的就是那块屏当前显示的那条完整路径）；
 * - 点下面那行路径 → 打开它所在的文件夹并选中它（`COPY_REVEAL_OK=true`，
 *   调用的是 `reveal_file`）；
 * - 两处都在缩略图那格上（`COPY_OPENABLE=2`）——"图像本身就是点击目标"；
 * - **不再复制路径**（`COPY_TEXT_CALLS=0`）：这正是 1.11.0 改掉的行为。
 *
 * 真去打开哪个程序、资源管理器里选中了谁由真机验收——这里证明的是前端接线。
 */
function copyTest() {
  const shots = Array.from(document.querySelectorAll<HTMLElement>(".wwp-shot"));
  // 「当前显示」是每块屏的第一格，按**标签文字**找（理由同 `nextTest`）。
  const currents = shots.filter((shot) => shotLabel(shot) === "当前显示");
  const openable = currents.filter((shot) => shot.querySelector(".wwp-shot-openable"));
  const target = currents[0];
  const expected = target ? shotPath(target) : "-";
  const image = target?.querySelector<HTMLElement>(".wwp-shot-img") ?? null;
  // 点缩略图（图像本身）。路径行是它下面那个 `span.wwp-shot-path`。
  image?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  target?.querySelector<HTMLElement>(".wwp-shot-path")
    ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  window.setTimeout(() => {
    const toast = document.querySelector(".wwp-toast");
    const line = document.createElement("pre");
    line.id = "copyprobe";
    line.textContent =
      `COPY_SHOTS=${shots.length} COPY_CELLS=${currents.length} ` +
      `COPY_OPENABLE=${openable.length} ` +
      `COPY_VIA=${image ? "img" : "（没有图）"} ` +
      `COPY_TEXT_CALLS=${copyTextCalls.length} ` +
      `COPY_OPEN_OK=${openFileCalls.length === 1 && openFileCalls[0] === expected} ` +
      `COPY_REVEAL_OK=${revealFileCalls.length === 1 && revealFileCalls[0] === expected} ` +
      `COPY_TOAST=${toast?.textContent ?? "（没有提示）"}`;
    document.body.appendChild(line);
  }, 500);
}

/**
 * `?toastprobe=1`：两个"右上角提示"同时在最坏情况下出现，量它们与「重新加载」
 * 的位置关系。
 *
 * 验的是**这一条**（用户 2026-10 的要求："检查所有右上角的悬浮提示，使其一定出现
 * 在程序 UI 界面的最右上角，不要覆盖掉重新加载几个字"）：
 *
 * - `POP_CORNER_OK`：两张卡的上边缘都在 y=8、右边缘都在 `innerWidth-8`
 *   （容差 1 px）——"一定在最右上角"；
 * - `POP_OVERLAP=0`：两张卡的矩形与「重新加载」按钮的矩形**一个都不相交**——
 *   "不覆盖重新加载"；
 * - `POP_HIT_RELOAD=true`：按钮中心点用 `elementFromPoint` 探到的还是按钮本身，
 *   证明这一层浮层没有把右上角的鼠标事件拦下来（整层是 `pointer-events: none`）。
 *
 * 为什么必须量矩形而不是"看着没挡"：提示的高度随文字长度变、扫描卡的高度随内容变，
 * 而「重新加载」就在右上角正下方——两格一旦**纵向堆叠**（而不是并排）就会压住它。
 * 这条判据把这个回归钉死。
 */
function toastPopTest() {
  // 点「当前显示」那一格**下面那行路径**：这条路上才会浮出一句提示
  // （1.11.0 起，点路径 = 打开所在文件夹并选中，会 notify）。
  // 以前点的是缩略图 `.wwp-shot-img`，因为那时点图片是复制路径；
  // 现在点图片改成用系统默认程序打开、**成功不提示**，再点它就造不出这条提示了。
  // 这里不去伪造一条提示：探针量的是真实行为浮出来的东西。
  document
    .querySelector<HTMLElement>(".wwp-shot-path")
    ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  window.setTimeout(() => {
    const cells = [".wwp-toast", ".wwp-scan-toast"].map((sel) =>
      document.querySelector(sel)?.getBoundingClientRect() ?? null,
    );
    const reload =
      Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-control-row button")).find((b) =>
        (b.textContent ?? "").includes("重新加载"),
      ) ?? null;
    const reloadRect = reload?.getBoundingClientRect() ?? null;
    const fmt = (r: DOMRect | null) =>
      r
        ? `[${Math.round(r.left)},${Math.round(r.top)},${Math.round(r.width)},${Math.round(r.height)}]`
        : "（没有）";
    // 相交 = 两个矩形在 x 与 y 上都真的重叠。判据要的是"一个都没有"。
    const hits = (a: DOMRect | null, b: DOMRect | null) =>
      Boolean(a && b && a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom);
    const overlap = cells.filter((r) => hits(r, reloadRect)).length;
    // "在最右上角" = 两张卡的**上边缘都在 y=8**，且**最右边那张**的右边缘在
    // 布局视口的右边缘往里 8px（容差 1px）。注意两点：
    //   - 不能要求两张卡都在那一列：它们是并排的，扫描卡本来就排在提示的左边；
    //   - 比的是 `documentElement.clientWidth`（**去掉竖向滚动条**的布局视口），
    //     不是 `window.innerWidth`（含滚动条）。浮层的 `position: fixed` 是相对
    //     布局视口定位的，用 innerWidth 会平白差出一个滚动条的宽度
    //     （这一版第一次跑就是这么判成"没贴角"的：1120 - 1090 = 30）。
    const viewportRight = document.documentElement.clientWidth;
    const tops = cells.every((r) => r && Math.abs(r.top - 8) <= 1);
    const rightEdge = cells.reduce((max, r) => (r && r.right > max ? r.right : max), -Infinity);
    const corner = tops && Math.abs(viewportRight - rightEdge - 8) <= 1;
    const gap = cells[0] && cells[1] ? Math.round(cells[0].left - cells[1].right) : -1;
    const at = reloadRect
      ? document.elementFromPoint(reloadRect.left + reloadRect.width / 2, reloadRect.top + reloadRect.height / 2)
      : null;
    const line = document.createElement("pre");
    line.id = "toastprobe";
    // 标题行的内容宽度：库名是长路径时会不会把这行撑破、会不会被省略号截掉。
    // 这里只报数，不判——判据 8 只守"位置"和"不覆盖"两件事。
    const head = document.querySelector<HTMLElement>(".wwp-scan-toast-head");
    line.textContent =
      `POP_TOAST=${fmt(cells[0])} POP_SCAN=${fmt(cells[1])} POP_RELOAD=${fmt(reloadRect)} ` +
      `POP_TOAST_H=${cells[0] ? Math.round(cells[0].height) : 0} ` +
      `POP_SCAN_H=${cells[1] ? Math.round(cells[1].height) : 0} ` +
      `POP_CARDS=${cells.filter(Boolean).length} POP_GAP=${gap} ` +
      `POP_VIEWPORT=[${viewportRight},${document.documentElement.clientHeight}] ` +
      `POP_INNER=${window.innerWidth} ` +
      `POP_SCAN_HEAD=${head ? Math.round(head.clientWidth) : 0}/${head ? Math.round(head.scrollWidth) : 0} ` +
      `POP_CORNER_OK=${corner} POP_OVERLAP=${overlap} ` +
      `POP_HIT_RELOAD=${Boolean(at && reload && (at === reload || reload.contains(at)))}`;
    document.body.appendChild(line);
  }, 500);
}

/** 把界面上现在的列表顺序读出来（用于验证拖拽排序真的改了顺序）。 */
function currentOrder(): string[] {
  return Array.from(document.querySelectorAll<HTMLInputElement>(".wwp-playlist-name")).map(
    (input) => input.value,
  );
}

/**
 * `?dragtest=below|above`：**用真的 DOM 拖拽事件**走一遍"把第 1 行往下拖"。
 *
 * 在真浏览器里合成 `DragEvent` 是有效的（WebView2 里 `PostMessage` 的
 * 合成点击无效，那是另一回事），因此这条能证明：
 * React 的 onDragStart/onDragOver/onDrop 接上了、下标换算没错、
 * 提交之后界面的顺序真的变了。
 */
function dragTest(mode: string) {
  const before = currentOrder();
  const handles = document.querySelectorAll<HTMLElement>(".wwp-drag-handle");
  const rows = document.querySelectorAll<HTMLElement>(".wwp-playlist");
  const transfer = new DataTransfer();
  handles[0].dispatchEvent(
    new DragEvent("dragstart", { bubbles: true, dataTransfer: transfer }),
  );

  // **必须先让 React 把这一批状态更新渲染出来**再发 dragover/drop：
  // `dragIndex` 是通过 `setState` 传下去的，同一个任务里连着发三个事件时
  // 行上的 onDragOver 闭包里的 `dragIndex` 还是 null（真实的拖动之间隔着
  // 任务边界，因此不会有这个问题）。
  window.setTimeout(() => {
    const target = rows[2];
    const rect = target.getBoundingClientRect();
    // below：落点在第三行的下半部分 = 插到它后面；above：上半部分 = 插到它前面。
    const clientY = mode === "above" ? rect.top + 2 : rect.bottom - 2;
    const point = { clientX: rect.left + 40, clientY };
    target.dispatchEvent(
      new DragEvent("dragover", { bubbles: true, dataTransfer: transfer, ...point }),
    );
    target.dispatchEvent(
      new DragEvent("drop", { bubbles: true, dataTransfer: transfer, ...point }),
    );

    window.setTimeout(() => {
      const after = currentOrder();
      const line = document.createElement("pre");
      line.id = "dragtest";
      line.textContent = `DRAG_${mode.toUpperCase()}_BEFORE=${before.join(" | ")} DRAG_AFTER=${after.join(" | ")}`;
      document.body.appendChild(line);
    }, 400);
  }, 150);
}


/**
 * `?measure=1`：量一下"点开关联图库之后，左边那个名字输入框的宽度有没有变"。
 *
 * 这是用户报的那个跳动（"点击关联图库时左侧的文字栏会发生宽度的变化"）的
 * **客观判据**：面板一展开页面变高，竖直滚动条出现，内容区被挤窄十几个像素，
 * 于是名字输入框跟着变窄。修法是 `.wwp-content { scrollbar-gutter: stable }`。
 *
 * 数字直接写进 DOM，配合 `--dump-dom` 读出来——比人眼看截图可靠。
 *
 * ⚠️ 调用它的行数要挑对：页面必须**本来不滚动、展开面板之后才滚动**，
 * 否则滚动条本来就在，怎么量都是 0。`scripts/check-layout.ps1` 用的是
 * `rows=4`（在真机那个 1120×761 的 CSS 视口下正是这个临界点）。
 */
function measure() {
  const content = document.querySelector<HTMLElement>(".wwp-content");
  // 对照组：`?nogutter=1` 把 `scrollbar-gutter` 关掉，看没有这条规则时会差多少。
  // 两组数字放在一起才说明这条 CSS 是**在起作用**，而不是"本来就不跳"。
  if (params.get("nogutter") === "1" && content) {
    content.style.scrollbarGutter = "auto";
  }
  const name = document.querySelector<HTMLInputElement>(".wwp-playlist-name");
  const openButton = Array.from(document.querySelectorAll<HTMLButtonElement>(".wwp-btn")).find(
    (b) => b.textContent === "关联图库…",
  );
  const before = name?.getBoundingClientRect().width ?? -1;
  const contentBefore = content?.clientWidth ?? -1;
  openButton?.click();
  window.setTimeout(() => {
    const after =
      document.querySelector<HTMLInputElement>(".wwp-playlist-name")?.getBoundingClientRect().width ??
      -1;
    const contentAfter = content?.clientWidth ?? -1;
    const scrolls = content ? content.scrollHeight > content.clientHeight : false;
    const line = document.createElement("pre");
    line.id = "measure";
    line.textContent =
      `GUTTER=${params.get("nogutter") === "1" ? "off" : "stable"} ` +
      `NAME_WIDTH_BEFORE=${before.toFixed(2)} NAME_WIDTH_AFTER=${after.toFixed(2)} ` +
      `DELTA=${(after - before).toFixed(2)} ` +
      `CONTENT_WIDTH_BEFORE=${contentBefore} CONTENT_WIDTH_AFTER=${contentAfter} ` +
      `SCROLLS=${scrolls}`;
    document.body.appendChild(line);
  }, 300);
}


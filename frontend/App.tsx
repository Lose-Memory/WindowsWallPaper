import React from "react";
import {
  api as apiClient,
  onMainWindowShown,
  onPlaylistsSwitched,
  sourceLabel,
  type DiagnosticsExport,
  type SettingsInfo,
  type StartupInfo,
} from "./api";
import { LibraryPage } from "./components/LibraryPage";
import { PlaybackPage } from "./components/PlaybackPage";
import { PlaylistPage } from "./components/PlaylistPage";
import { SettingsPage } from "./components/SettingsPage";
import { TrashPage } from "./components/TrashPage";
import { ToastView, useToast } from "./components/Toast";
import { ScanToast } from "./components/ScanToast";
// 品牌标记用软件自己的图标。它由 `scripts/make-icon.ps1` 从根目录的
// app-icon.png 生成后拷到这里，Vite 会把它作为资源打包进 exe。
import appIcon from "./assets/app-icon.png";

type Page = "playback" | "library" | "playlists" | "trash" | "settings" | "diagnostics";

const PAGES: Array<{ id: Page; label: string }> = [
  { id: "playback", label: "播放" },
  { id: "library", label: "图库" },
  // 图库与列表分成两页（用户要求）：一页回答"有哪些放图的目录"，
  // 另一页回答"每块屏放哪几套图"。挤在一页时两边都要滚。
  // 页面名用户指定为「列表」（2026-10）。
  { id: "playlists", label: "列表" },
  // 回收站单独一页（用户要求）：以相册形式看被删掉的图，逐张恢复或删除。
  // 放在列表之后，是因为它管的正是"列表里那些图"的亡羊补牢。
  { id: "trash", label: "回收站" },
  // 设置单独一页：原先快捷键与启动开关挤在播放页，那一页因此要滚动，
  // 而"两块屏并排对比"正是它存在的理由。见 SettingsPage 的说明。
  { id: "settings", label: "设置" },
  { id: "diagnostics", label: "诊断" },
];

export function App() {
  const [info, setInfo] = React.useState<StartupInfo | null>(null);
  const [settings, setSettings] = React.useState<SettingsInfo | null>(null);
  const [page, setPage] = React.useState<Page>("playback");
  // 右上角浮动提示。放在 App 这一层而不是各页面里：
  // 它是浮层，切页时不该被页面卸载顺带清掉。
  const { toast, show: showToast } = useToast();
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [startup, settingsInfo] = await Promise.all([apiClient.startupInfo(), apiClient.settingsInfo()]);
        if (cancelled) return;
        setInfo(startup);
        setSettings(settingsInfo);
        // 把窗口与页面的几何信息都记进日志：当界面的排版尺寸与窗口对不上时，
        // 这组数字是唯一能区分"页面溢出"与"渲染缩放不对"的依据。
        const metrics = await apiClient.windowMetrics();

        void apiClient.logDebug(
          `[几何] 窗口客户区（Tauri）${metrics.innerSize[0]}x${metrics.innerSize[1]} 物理 px，` +
            `scaleFactor=${metrics.scaleFactor}，页面视口 ${window.innerWidth}x${window.innerHeight} CSS px，` +
            `devicePixelRatio=${window.devicePixelRatio}`,
        );
      } catch (cause) {
        if (!cancelled) setError(String(cause));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // 窗口被从隐藏/最小化叫到前台时，**切回播放页**。
  //
  // 用户 2026-10 的要求："当我使用快捷键唤起主页面时，默认把页面调到播放页，
  // 而不是其他我上次打开过的页面。"隐藏窗口只是 `hide()`，React 树并没有卸载，
  // 所以 `page` 会一直是上次那一页；`useState("playback")` 那个初值只在**首次
  // 加载**时生效，管不到这里。
  //
  // 后端**只在窗口本来不在屏幕上时**才发这个事件（见 `tray::show_main_window`），
  // 因此用户开着设置页改东西、顺手点一下托盘图标，页面不会被抢走。
  React.useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void onMainWindowShown(() => {
      if (!disposed) setPage("playback");
    })
      // 成败都记日志：这类"事件没接上也不报错"的问题只有靠日志才查得动
      // （capabilities 缺授权就是这个症状，见 AGENTS §7.2）。
      .then((un) => {
        void apiClient.logDebug("[界面] 主窗口唤起事件订阅成功");
        if (disposed) un();
        else unlisten = un;
      })
      .catch((cause) => {
        void apiClient.logError(`[界面] 主窗口唤起事件订阅失败：${String(cause)}`);
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // 按下「切换播放列表」快捷键后，后端改的是设置里的 `current_playlist`。
  //
  // 必须重新拉一次设置：播放页的列表下拉框与设置页那两个槽位都读它，
  // 不重拉就会一直显示旧值——用户看到的是"按了快捷键，壁纸换了，
  // 但界面还写着原来的列表"，会以为程序记错了。
  React.useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void onPlaylistsSwitched(() => {
      if (disposed) return;
      void apiClient
        .settingsInfo()
        .then((next) => {
          if (!disposed) setSettings(next);
        })
        .catch((cause) => {
          void apiClient.logError(`[界面] 切换播放列表后刷新设置失败：${String(cause)}`);
        });
    })
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      })
      .catch((cause) => {
        void apiClient.logError(`[界面] 切换播放列表事件订阅失败：${String(cause)}`);
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  if (error) {
    return React.createElement(
      "div",
      { className: "wwp-page" },
      React.createElement("h1", { className: "wwp-title" }, "无法获取程序信息"),
      React.createElement("p", { className: "wwp-sub" }, "后端命令调用失败。"),
      React.createElement("pre", { className: "wwp-pre" }, error),
    );
  }

  if (!info || !settings) {
    return React.createElement("div", { className: "wwp-page" }, "正在加载…");
  }

  return React.createElement(
    "div",
    { className: "wwp-shell" },
    React.createElement(
      "aside",
      { className: "wwp-sidebar" },
      React.createElement(
        "div",
        { className: "wwp-brand" },
        React.createElement("img", {
          className: "wwp-brand-icon",
          src: appIcon,
          // 纯装饰：旁边就是软件名，读屏软件不需要重复念一遍。
          alt: "",
          "aria-hidden": true,
        }),
        "WindowsWallPaper",
      ),
      PAGES.map((entry) =>
        React.createElement(
          "button",
          {
            key: entry.id,
            className: `wwp-nav${page === entry.id ? " wwp-nav-active" : ""}`,
            onClick: () => setPage(entry.id),
          },
          entry.label,
        ),
      ),
      React.createElement(
        "div",
        { className: "wwp-sidebar-foot" },
        `v${info.version} · ${info.build}`,
      ),
    ),
    React.createElement(
      "main",
      { className: "wwp-content" },
      settings.fellBack
        ? React.createElement(
            "div",
            { className: "wwp-warn" },
            `程序目录不可写，已回退到系统目录存放配置与索引。原因：${
              settings.fallbackReason ?? "未知"
            }`,
          )
        : null,
      page === "playback"
        ? React.createElement(PlaybackPage, {
            settings,
            onSettingsChanged: setSettings,
            // 提示由 App 统一持有：它是浮层，不属于任何页面，
            // 而且切页时不该被页面卸载顺带清掉。
            notify: showToast,
          })
        : null,
      page === "library"
        ? React.createElement(LibraryPage, {
            settings,
            onSettingsChanged: setSettings,
            notify: showToast,
          })
        : null,
      page === "playlists"
        ? React.createElement(PlaylistPage, {
            settings,
            onSettingsChanged: setSettings,
            notify: showToast,
          })
        : null,
      page === "trash"
        ? React.createElement(TrashPage, { notify: showToast })
        : null,
      page === "settings"
        ? React.createElement(SettingsPage, {
            settings,
            onSettingsChanged: setSettings,
            notify: showToast,
          })
        : null,
      page === "diagnostics" ? React.createElement(DiagnosticsPage, { info, settings }) : null,
    ),
    // 浮层放在 shell 之内、内容之外：它固定定位，因此不参与布局，
    // 只是需要一个挂载点。
    //
    // 两块**共用同一层**（`.wwp-pop-layer`）：那一层负责"贴在窗口最右上角"
    // 与"横向并排"，因此两个卡片自己都不再写 `top`/`right`。
    // 用户 2026-10 的要求是"所有右上角的悬浮提示一定出现在最右上角，
    // 且不要覆盖掉重新加载几个字"——由 `check-layout.ps1` 的判据 8 量。
    React.createElement(
      "div",
      { className: "wwp-pop-layer" },
      // 第一个子元素贴最右边（这一层是 `row-reverse`），
      // 因此**一次性提示**（保存/复制反馈）在最右，扫描状态在它左边。
      React.createElement(ToastView, { toast }),
      React.createElement(ScanToast, null),
    ),
  );
}

function DiagnosticsPage({ info, settings }: { info: StartupInfo; settings: SettingsInfo }) {
  const [exporting, setExporting] = React.useState(false);
  const [exported, setExported] = React.useState<DiagnosticsExport | null>(null);
  const [error, setError] = React.useState<string | null>(null);

  const rows: Array<[string, string]> = [
    ["版本", `${info.version}（${info.build}）`],
    ["系统", info.os],
    ["程序目录", info.programDir],
    ["数据位置来源", sourceLabel(info.rootSource)],
    ["数据根目录", settings.dataRoot],
    ["配置目录", info.configDir],
    ["索引目录", info.dataDir],
    ["回收站目录", info.trashDir],
    ["日志目录", info.logDir],
    ["WebView2 数据", info.webviewDataDir],
    ["设置文件", settings.settingsPath],
  ];

  const doExport = async () => {
    setExporting(true);
    setError(null);
    try {
      setExported(await apiClient.diagnosticsExport());
    } catch (cause) {
      setError(String(cause));
    } finally {
      setExporting(false);
    }
  };

  return React.createElement(
    "div",
    null,
    React.createElement("h1", { className: "wwp-title" }, "诊断"),
    React.createElement(
      "p",
      { className: "wwp-sub" },
      "整个程序文件夹拷到别处运行时，「数据位置来源」应仍为程序目录（便携模式）。",
    ),
    React.createElement(
      "div",
      { className: "wwp-card" },
      rows.map(([label, value]) =>
        React.createElement(
          "div",
          { className: "wwp-row", key: label },
          React.createElement("span", { className: "wwp-label" }, label),
          React.createElement("span", { className: "wwp-value" }, value),
        ),
      ),
    ),

    React.createElement(
      "div",
      { className: "wwp-card" },
      React.createElement("h2", { className: "wwp-h2" }, "导出诊断信息"),
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "环境、显示器、索引统计与最近日志打包成一个目录，便于反馈问题。",
      ),
      React.createElement(
        "div",
        { className: "wwp-control-row" },
        React.createElement(
          "button",
          { className: "wwp-btn", disabled: exporting, onClick: () => void doExport() },
          exporting ? "正在导出…" : "导出到 data 目录",
        ),
      ),
      error ? React.createElement("div", { className: "wwp-error", style: { marginTop: 10 } }, error) : null,
      exported
        ? React.createElement(
            "div",
            { className: "wwp-ok-box", style: { marginTop: 10 } },
            React.createElement("div", null, `已导出到：${exported.directory}`),
            React.createElement(
              "div",
              { className: "wwp-dim", style: { marginTop: 6 } },
              `包含 ${exported.files.length} 个文件：${exported.files.join("、")}`,
            ),
            React.createElement(
              "div",
              { className: "wwp-dim", style: { marginTop: 6 } },
              "其中索引统计与日志可能含本地路径，分享前可以先看一眼。不包含图片文件。",
            ),
          )
        : null,
      exported
        ? React.createElement(
            "div",
            { style: { marginTop: 12 } },
            React.createElement("h2", { className: "wwp-h2" }, "本次采集到的环境"),
            React.createElement(
              "div",
              { className: "wwp-row" },
              React.createElement("span", { className: "wwp-label" }, "系统"),
              React.createElement(
                "span",
                { className: "wwp-value" },
                exported.environment.os.display,
              ),
            ),
            React.createElement(
              "div",
              { className: "wwp-row" },
              React.createElement("span", { className: "wwp-label" }, "WebView2"),
              React.createElement(
                "span",
                { className: "wwp-value" },
                exported.environment.webview2Version,
              ),
            ),
            React.createElement(
              "div",
              { className: "wwp-row" },
              React.createElement("span", { className: "wwp-label" }, "桌面层"),
              React.createElement(
                "span",
                { className: "wwp-value" },
                `${exported.environment.desktopTopology} —— ${exported.environment.desktopLayer}`,
              ),
            ),
            exported.environment.monitors.map((monitor) =>
              React.createElement(
                "div",
                { className: "wwp-row", key: monitor.name },
                React.createElement("span", { className: "wwp-label" }, monitor.name),
                React.createElement(
                  "span",
                  { className: "wwp-value" },
                  `${monitor.width}×${monitor.height} @(${monitor.x},${monitor.y}) ${
                    monitor.primary ? "[主] " : ""
                  }${monitor.dpi} dpi（${monitor.scalePercent}%）`,
                ),
              ),
            ),
          )
        : null,
    ),
  );
}

import React from "react";
import {
  api,
  FIT_OPTIONS,
  type BindingView,
  type MonitorView,
  type PlaybackStatus,
  type PlaylistView,
  type ScreenState,
  type SettingsInfo,
  type SettingsPatch,
} from "../api";
import { Tooltip } from "./Tooltip";
import { WallpaperShot } from "./WallpaperShot";
// 全部悬浮提示的文字都在这里（用户要求能把它们集中改一处，见那个文件的说明）。
import { TIPS } from "../tooltip-texts";

interface Props {
  settings: SettingsInfo;
  onSettingsChanged: (next: SettingsInfo) => void;
  /** 右上角浮动提示。由 App 提供，因为它不属于任何单个页面。 */
  notify: (text: string, kind?: "ok" | "error") => void;
}

/**
 * 播放页：轮播的启停、NSFW 模式、每屏的独立设置与逐屏结果。
 *
 * 这一页把"每屏是一个独立播放单元"直接呈现出来：每块屏一张卡片，卡里是该屏自己的
 * 当前播放列表、筛选项、NSFW 壁纸、正在显示的图与换纸次数。
 *
 * ## 这一页没有"停止并还原壁纸"
 *
 * 按用户要求，程序**不再把壁纸还原成他原来的那张**：关闭、退出、停止轮播都不还原。
 * 因此这里只有"暂停/继续"（不再换纸，当前壁纸保持不动），没有"停止并还原"。
 */
export function PlaybackPage({ settings, onSettingsChanged, notify }: Props) {
  const [status, setStatus] = React.useState<PlaybackStatus | null>(null);
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  // 轮播状态靠轮询刷新：后端按节拍换纸，随时可能变化。
  // 1 秒一次足够让界面看起来是实时的，而事件推送在这里反而更复杂（要按屏去重）。
  React.useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const next = await api.playbackStatus();
        if (!cancelled) setStatus(next);
      } catch (cause) {
        if (!cancelled) setError(String(cause));
      }
    };
    void tick();
    const timer = window.setInterval(tick, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const applyPatch = React.useCallback(
    async (patch: SettingsPatch) => {
      setError(null);
      try {
        const next = await api.settingsUpdate(patch);
        onSettingsChanged(next);
        // 用右上角浮层而不是页面里的提示条：浮层不影响布局。
        notify("设置已保存");
      } catch (cause) {
        setError(String(cause));
        // 保存失败也要浮出来。只在页面里显示的话，用户滚到别处就看不到，
        // 会以为已经保存成功了。
        notify(`保存失败：${String(cause)}`, "error");
      }
    },
    [onSettingsChanged, notify],
  );

  /** 执行一次"点按钮 -> 拿新状态"的操作。 */
  const run = async (label: string, action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      setStatus(await api.playbackStatus());
    } catch (cause) {
      setError(`${label}失败：${String(cause)}`);
      notify(`${label}失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  /**
   * 重新加载 = 按当前设置重建轮播。
   *
   * **不还原壁纸，也不立刻换一张**：只是停掉调度线程、按新设置重建，
   * 然后等这一轮间隔到点再换。
   *
   * ## 为什么必须传 false
   *
   * 原先这里走的是 `playbackStart(true)`（"重建并立刻换一张"）。用户看到的现象
   * 就是"点重新加载会很快地切换一次"——那不是 bug 的表现，而是这条命令本身的
   * 语义。他要的是"直接重新加载"，因此这里不再请求立即换纸。
   *
   * 大多数设置改动**保存时就自动生效了**（后端会比对播放计划的指纹，
   * 只有真的变了才重建）；这个按钮留给"想手动强制刷一次"和
   * "在程序外面改了东西（例如替换了图库里的图）"的情况。
   */
  const reload = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.playbackStart(false);
      setStatus(await api.playbackStatus());
      notify("已重新加载：按当前设置重建，当前壁纸保持不动，到下一个间隔才会换");
    } catch (cause) {
      setError(`重新加载失败：${String(cause)}`);
      notify(`重新加载失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  /** NSFW 模式：进入/退出。与全局快捷键走同一段后端逻辑。 */
  const toggleNsfw = async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await api.playbackNsfw();
      setStatus(report.status);
      notify(report.message, "ok");
    } catch (cause) {
      setError(`NSFW 模式切换失败：${String(cause)}`);
      notify(`NSFW 模式切换失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const running = status?.running ?? false;
  const paused = status?.paused ?? false;
  const nsfw = status?.nsfw ?? false;
  const bindings = status?.bindings ?? [];

  // 有几块屏设置了 NSFW 壁纸。NSFW 按钮的悬浮提示要用它（界面上不再常驻这行说明）。
  const nsfwConfigured = Object.values(settings.monitors).filter(
    (monitor) => (monitor.nsfwWallpaper ?? "").trim() !== "",
  ).length;

  return React.createElement(
    "div",
    null,
    React.createElement(
      "div",
      { className: "wwp-page-head" },
      React.createElement("h1", { className: "wwp-title" }, "播放"),
      // 只要一句定位语。更细的说明（"不想让某块屏换纸就不给它选列表"）
      // 已经分别挂在对应控件的悬浮提示上——用户要求界面上不堆说明文字。
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "每块屏各自独立播放：自己的列表、自己的筛选。",
      ),
    ),

    error ? React.createElement("div", { className: "wwp-error" }, error) : null,

    nsfw
      ? React.createElement(
          "div",
          { className: "wwp-ok-box" },
          "当前处于 NSFW 模式：屏幕上是你为每块屏指定的固定壁纸，轮播已暂停。",
        )
      : null,

    React.createElement(RunControls, {
      running,
      paused,
      nsfw,
      busy,
      bindings,
      nsfwConfigured,
      run,
      reload,
      onToggleNsfw: toggleNsfw,
    }),

    React.createElement("h2", { className: "wwp-h2", style: { marginTop: 18 } }, "每块屏幕"),

    bindings.length === 0
      ? React.createElement(
          "div",
          { className: "wwp-warn" },
          "没有检测到显示器。若是刚改过显示配置，切到别的页面再回来会重新读取。",
        )
      : null,

    // 每块屏的设置在**左右两列**里并排，而不是上下堆叠。
    //
    // 用户的要求是"支持同时完整查看两台显示器的所有配置项，方便对比与调整"。
    //
    // 顺序由后端决定：按显示器在桌面上的**物理位置**从左到右
    // （用户的实际摆位是"竖屏在横屏左边"，而枚举顺序是"主屏排最前"）。
    React.createElement(
      "div",
      { className: "wwp-screen-grid" },
      bindings.map((binding, index) =>
        React.createElement(ScreenCard, {
          key: binding.monitorName,
          binding,
          saved: settings.monitors[binding.monitorName],
          screenState: status?.screens.find((s) => s.monitorName === binding.monitorName),
          running,
          nsfw,
          busy,
          pausedGlobal: paused,
          // 位置说明：两块屏时直接说"左/右"，更多屏时按序号（顺序就是桌面左右）。
          position: bindings.length === 2 ? (index === 0 ? "左" : "右") : `第 ${index + 1} 块`,
          playlists: settings.playlists,
          onPatch: applyPatch,
          notify,
          onScreenPause: (paused: boolean) =>
            run(paused ? "暂停该屏" : "继续该屏", () =>
              api.playbackScreenPause(binding.monitorName, paused),
            ),
          onNextScreen: () =>
            run("切换这块屏", async () => {
              const found = await api.playbackNextScreen(binding.monitorName);
              notify(
                found
                  ? "已把这块屏切到下一张（另一块屏不动）"
                  : "找不到这块屏，可能刚刚拔掉或改了显示配置",
                found ? "ok" : "error",
              );
            }),
          onDeleteCurrent: () =>
            run("删除当前壁纸", async () => {
              // 后端会把"移进回收站 + 换下一张"的结果拼成一句话，原样浮出来。
              const report = await api.playbackDeleteCurrent(binding.monitorName);
              notify(report.message);
            }),
        }),
      ),
    ),
  );
}

/**
 * 启停、NSFW 模式与全局操作：**全在同一行**。
 *
 * ## 为什么 NSFW 从单独一张卡片搬到这里（用户要求）
 *
 * 它和「暂停 / 上一张 / 下一张」是同一类东西——都是"我现在想让它怎么样"的即时操作，
 * 分开放会让人先在一处找播放控制、再到另一处找 NSFW。并排之后，
 * 这一行就是完整的控制面板。
 *
 * ## 为什么 NSFW 是绿色、开启后变红
 *
 * 绿 = "可以按，按了会切到固定壁纸"；红 = "已经在 NSFW 模式的固定壁纸上了"。
 * 用底色而不是文字表达状态，是为了让按钮文字能保持一个固定的短词（`NSFW`）——
 * 原先它靠文字在「NSFW（切到固定壁纸）」与「退出 NSFW 模式」之间变化，
 * 一长一短会让整行按钮跟着左右跳动。
 */
function RunControls({
  running,
  paused,
  nsfw,
  busy,
  bindings,
  nsfwConfigured,
  run,
  reload,
  onToggleNsfw,
}: {
  running: boolean;
  paused: boolean;
  nsfw: boolean;
  busy: boolean;
  bindings: BindingView[];
  /** 已经设置了 NSFW 壁纸的屏幕数（写进 NSFW 的悬浮提示）。 */
  nsfwConfigured: number;
  run: (label: string, action: () => Promise<unknown>) => Promise<void>;
  reload: () => Promise<void>;
  onToggleNsfw: () => Promise<void>;
}) {
  const disabled = busy || bindings.length === 0;

  // NSFW 模式下**也把这一行摆成"在轮播"的样子**，只是全部禁用（用户要求）。
  //
  // NSFW 模式里没有调度线程（`running` 是 false），原先于是只剩一个「开始轮播」，
  // 这一行因此从 5 个控件缩成 3 个——按钮左右跳动，看起来像界面坏了。
  // 现在保留「暂停 / 上一张 / 下一张」三个位置，置灰即可：**布局一个像素都不动**，
  // 而"现在这些都没用"由灰色 + 红色 NSFW + 状态胶囊「NSFW 模式」三处一起说清。
  const buttons = running || nsfw
    ? [
        {
          label: paused ? "继续" : "暂停",
          primary: true,
          title: nsfw
            ? TIPS.buttonDisabledInNsfw
            : paused
              ? TIPS.pauseButtonPaused
              : TIPS.pauseButtonRunning,
          action: () => api.playbackControl(paused ? "resume" : "pause"),
        },
        {
          label: "上一张",
          primary: false,
          title: nsfw ? TIPS.previousButtonNsfw : TIPS.previousButton,
          action: () => api.playbackControl("previous"),
        },
        {
          label: "下一张",
          primary: false,
          // 这句原来常驻在卡片底部（用户要求界面不堆说明），挪进悬浮提示。
          title: nsfw ? TIPS.previousButtonNsfw : TIPS.nextButton,
          action: () => api.playbackControl("next"),
        },
      ]
    : [
        {
          label: "开始轮播",
          primary: true,
          title: TIPS.startButton,
          action: () => api.playbackStart(true),
        },
      ];

  // NSFW 模式里这三个都不可点（含"开始轮播"那一支：那时 NSFW 负责进出，
  // 不该在同一个位置出现两个互相矛盾的入口）。
  const controlsDisabled = disabled || nsfw;

  // NSFW 模式下**没有播放线程**（`running` 是 false），但那时说"未运行"是误导：
  // 屏幕上是我们要的固定壁纸，只是没在轮播。因此单独给一个状态词。
  const stateLabel = nsfw ? "NSFW 模式" : running ? (paused ? "已暂停" : "轮播中") : "未运行";
  const stateClass = !nsfw && running
    ? paused
      ? "wwp-pill wwp-pill-warn"
      : "wwp-pill wwp-pill-on"
    : "wwp-pill";

  return React.createElement(
    "div",
    { className: "wwp-card wwp-card-hero" },
    React.createElement(
      "div",
      { className: "wwp-control-row" },
      React.createElement(
        "span",
        {
          className: stateClass,
          // "谁也不等谁"原来是一整段常驻说明，现在只在需要时悬停可见。
          title: nsfw ? TIPS.statePillNsfw : TIPS.statePill,
        },
        stateLabel,
      ),
      buttons.map((button) =>
        React.createElement(
          "button",
          {
            key: button.label,
            className: button.primary ? "wwp-btn wwp-btn-primary" : "wwp-btn",
            disabled: controlsDisabled,
            title: button.title,
            onClick: () => void run(button.label, button.action),
          },
          button.label,
        ),
      ),
      // NSFW 与播放控制并排（用户要求），绿 = 未开启、红 = 已开启。
      React.createElement(
        "button",
        {
          className: nsfw ? "wwp-btn wwp-btn-nsfw-on" : "wwp-btn wwp-btn-nsfw",
          disabled: busy,
          "aria-pressed": nsfw,
          title: nsfw
            ? TIPS.nsfwButtonOn
            : nsfwConfigured > 0
              ? TIPS.nsfwButtonReady(nsfwConfigured)
              : TIPS.nsfwButtonNotConfigured,
          onClick: () => void onToggleNsfw(),
        },
        "NSFW",
      ),
      React.createElement("span", { className: "wwp-spacer" }),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy,
          // 原来这里是一整段常驻说明，压缩成一句悬浮提示。
          title: TIPS.reloadButton,
          onClick: () => void reload(),
        },
        busy ? "正在重新加载…" : "重新加载",
      ),
    ),
  );
}

/**
 * 全局快捷键的写法说明在 `tooltip-texts.ts` 的 `TIPS.hotkeySyntax` 里——
 * 五个快捷键拼的是同一段文字（用户要求集中在一处可改），因此这里不留副本。
 */
/**
 * 一个全局快捷键输入框（设置页用）。
 *
 * ## 为什么需要本地状态
 *
 * 直接用设置里的值做受控输入框会有两个问题：用户每敲一个字符都会保存一次；
 * 而后端保存时会把写法**规范化**（`ctrl+alt+left` → `Ctrl+Alt+Left`），
 * 于是刚敲到一半的输入会被改写成别的内容，光标也跟着跳。
 *
 * 因此本地状态接管输入，只在**失焦**时提交一次；外部值变了再同步回来。
 *
 * ## 说明文字全都进了悬浮提示（用户要求）
 *
 * 原先"这个键干什么用"是常驻在输入框下方的一行小字。它在布局上其实一直
 * 显示不出来（`.wwp-field` 是横向 flex，那行小字被挤到输入框右边、被卡片裁掉），
 * 现在统一并进悬浮提示：第一行是这个键的含义，后面是写法说明。
 */
export function HotkeyField({
  label,
  help,
  value,
  onSave,
}: {
  label: string;
  /** 这个键是干什么的（悬浮提示的第一行）。 */
  help: string;
  value: string;
  onSave: (value: string) => Promise<void>;
}) {
  const [draft, setDraft] = React.useState(value);

  React.useEffect(() => {
    setDraft(value);
  }, [value]);

  return React.createElement(Field, {
    label,
    tip: `${help}\n\n${TIPS.hotkeySyntax}`,
    children: React.createElement("input", {
      className: "wwp-input wwp-input-sm",
      placeholder: "例如 Ctrl+Alt+P（留空 = 不注册）",
      value: draft,
      onChange: (e: React.ChangeEvent<HTMLInputElement>) => setDraft(e.target.value),
      onBlur: () => {
        // 只在真的改了的时候提交：失焦一次就存一次会把日志刷满。
        if (draft !== value) void onSave(draft);
      },
    }),
  });
}

/** 单块屏幕的卡片。 */
function ScreenCard({
  binding,
  saved,
  screenState,
  running,
  nsfw,
  busy,
  pausedGlobal,
  position,
  playlists,
  onPatch,
  notify,
  onScreenPause,
  onNextScreen,
  onDeleteCurrent,
}: {
  binding: BindingView;
  saved: MonitorView | undefined;
  screenState: ScreenState | undefined;
  running: boolean;
  nsfw: boolean;
  busy: boolean;
  /**
   * **全局**暂停那一位（顶部那个「暂停」按钮）。
   *
   * 「下一张」那格的徽标要看它：暂停期间倒计时是停住的，写着"34 秒后"
   * 却一直不动会让人以为程序卡了（用户 2026-10 报的就是这个：直接按暂停时
   * 计时还在走，而全屏暂停时又显示"已暂停"——两处口径不一致）。
   */
  pausedGlobal: boolean;
  /** 这块屏在桌面上的位置说明（"左"/"右"/"第 N 块"），顺序来自后端。 */
  position: string;
  /** 所有播放列表。这块屏的选择器要从里面挑一个。 */
  playlists: PlaylistView[];
  onPatch: (patch: SettingsPatch) => Promise<void>;
  /** 右上角浮层，用于"已复制路径"这类即时反馈。 */
  notify: (text: string, kind?: "ok" | "error") => void;
  /** 单独暂停/继续这块屏。 */
  onScreenPause: (paused: boolean) => Promise<void>;
  /** 单独把这块屏切到下一张（另一块屏不动）。 */
  onNextScreen: () => Promise<void>;
  /** 把这块屏正在显示的那张图移进软件回收站，并换下一张。 */
  onDeleteCurrent: () => Promise<void>;
}) {
  // 输入框初值取自**已保存的设置**，这样用户能看到自己上次填了什么。
  // 只显示"生效值"是不够的：改过的东西看不见，用户会以为没保存成功。
  const [aspectMin, setAspectMin] = React.useState(saved?.aspectMin?.toString() ?? "");
  const [aspectMax, setAspectMax] = React.useState(saved?.aspectMax?.toString() ?? "");
  const [nsfwPath, setSafePath] = React.useState(saved?.nsfwWallpaper ?? "");
  const [singlePath, setSinglePath] = React.useState(saved?.singleImage ?? "");

  // 后端返回新设置后同步输入框，避免显示与磁盘上的值不一致。
  React.useEffect(() => {
    setAspectMin(saved?.aspectMin?.toString() ?? "");
    setAspectMax(saved?.aspectMax?.toString() ?? "");
  }, [saved?.aspectMin, saved?.aspectMax]);

  React.useEffect(() => {
    setSafePath(saved?.nsfwWallpaper ?? "");
  }, [saved?.nsfwWallpaper]);

  React.useEffect(() => {
    setSinglePath(saved?.singleImage ?? "");
  }, [saved?.singleImage]);

  const sendFilter = () =>
    void onPatch({
      monitors: {
        [binding.monitorName]: {
          // filterPresent 让后端整体替换筛选条件——否则清空输入框无法真的清掉条件。
          filterPresent: true,
          aspectMin: parseOptional(aspectMin),
          aspectMax: parseOptional(aspectMax),
        },
      },
    });

  const sendNsfwPath = (value: string) =>
    void onPatch({ monitors: { [binding.monitorName]: { nsfwWallpaper: value } } });

  const sendSinglePath = (value: string) =>
    void onPatch({ monitors: { [binding.monitorName]: { singleImage: value } } });

  const pickNsfwImage = async () => {
    try {
      const picked = await api.pickImage(nsfwPath.trim() || undefined);
      if (picked) {
        setSafePath(picked);
        sendNsfwPath(picked);
      }
    } catch (cause) {
      void api.logError(`选择 NSFW 壁纸失败：${String(cause)}`);
    }
  };

  const pickSingleImage = async () => {
    try {
      const picked = await api.pickImage(singlePath.trim() || undefined);
      if (picked) {
        setSinglePath(picked);
        sendSinglePath(picked);
      }
    } catch (cause) {
      void api.logError(`选择固定单图失败：${String(cause)}`);
    }
  };

  /**
   * 用系统默认看图程序打开"当前显示"那一张（点缩略图本身）。
   *
   * 1.11.0 起这里是"打开看"，不再是复制路径：用户点一张缩略图想看的是**图**，
   * 点路径那行才是"这张图在哪儿"（见 `revealImage`）。
   * 成功不提示：看图程序就是反馈，再浮一句是多余的。
   */
  const openCurrent = async () => {
    const path = screenState?.current ?? "";
    if (!path) {
      notify("这块屏还没有显示任何图片，没有图可打开", "error");
      return;
    }
    try {
      await api.openFile(path);
    } catch (cause) {
      notify(`打开图片失败：${String(cause)}`, "error");
    }
  };

  /**
   * 在资源管理器里打开这张图**所在的目录**并选中它（点下面那行路径）。
   *
   * 与 `openCurrent` 是两件事：那个是"打开看这张图"，这个是"这张图在哪儿"。
   * 卡片上那行路径是被截断过的，而这一下点击直接把资源管理器开到它跟前。
   *
   * 成功要浮一句：资源管理器是**另一个窗口**，不给回话的话用户不知道点没点上
   * （窗口被挡住时尤其明显）。这一句也顺手回答了"到底开在哪一级目录"。
   */
  const revealImage = async (path: string) => {
    if (!path) {
      notify("没有可打开的路径", "error");
      return;
    }
    try {
      await api.revealFile(path);
      const folder = path.replace(/[\\/][^\\/]*$/, "") || path;
      notify(`已在资源管理器中打开并选中：${folder}`);
    } catch (cause) {
      notify(`打开所在文件夹失败：${String(cause)}`, "error");
    }
  };

  /**
   * 复制这张图的**文件名（不含扩展名）**——右键点那行路径时走这里。
   *
   * 用户 2026-10 的要求："我希望使用右键点击这个位置时，能够复制对应图像的文件名，
   * 只复制文件名即可，不需要后缀。"左键仍然是"打开所在文件夹并选中它"
   * （上面那个 `revealImage`）：两件事分别挂在左键与右键上，互不干扰。
   *
   * ## 文件名为什么在前端算
   *
   * 路径本来就显示在这一行上，`fileStem` 只做"取最后一段、去掉扩展名"这一步。
   * 为它加一条后端命令会让"界面上显示的那一行"与"复制走的内容"多一条可能不一致的
   * 路（§7.3：同一件事不留两个实现）。**剪贴板仍然走后端**（`copy_text`）：
   * WebView2 里的 `navigator.clipboard` 失败时既不报错也不写入，实测表现成
   * "点了没反应"（见 `commands.rs` 那条命令的说明）。
   */
  const copyImageName = async (path: string) => {
    if (!path) {
      notify("没有可复制的文件名", "error");
      return;
    }
    const name = fileStem(path);
    try {
      await api.copyText(name);
      notify(`已复制文件名：${name}`);
    } catch (cause) {
      notify(`复制文件名失败：${String(cause)}`, "error");
    }
  };

  const portrait = binding.height > binding.width;
  // 这块屏当前用的播放列表。选中之后顺序/间隔/填充都来自它。
  const currentId = saved?.currentPlaylist ?? "";
  const playlist = playlists.find((p) => p.id === currentId);
  // 按钮的"暂停/继续"看**手动**那一位。用 `paused`（手动或全屏自动的或）会怎样：
  // 全屏自动暂停期间按钮显示成"继续"，而点它什么都不会变（自动那一位还在），
  // 看起来就像按钮坏了。
  const screenPaused = screenState?.pausedManual ?? false;
  // 这块屏现在是不是"因为上面有程序全屏"而停着（设置页里的那个开关）。
  const pausedByFullscreen = screenState?.pausedByFullscreen ?? false;
  // 这块屏是不是"固定单图"（不轮播）。以**计划**（后端算出来的生效值）为准，
  // 而不是以输入框里的字为准——用户可能刚清空还没失焦。
  const fixed = binding.singleImage;
  // 这块屏当前显示的源图路径（空 = 还没显示过）。「删除当前壁纸」靠它判断能不能点。
  const currentPath = screenState?.current ?? "";
  // 卡片右上角那两个按钮（删除当前壁纸 / 暂停）的显示条件：正在轮播、
  // 不在 NSFW 模式、也不是固定单图。三个条件缺一不可，理由见下面渲染处的注释。
  const showScreenButtons = running && !nsfw && !fixed;

  // --- 「下一张」预览那一格的内容（用户要求能看见即将切换的壁纸）---
  //
  // 后端只在**真的轮播着**的时候才算得出下一张（`ScreenState::next`：
  // 它需要调度线程里那个选择器）。因此每一种"没有下一张"都要说清原因，
  // 而不是留一个空框让用户猜。
  const nextPath = screenState?.next ?? "";
  const nextPlaceholder = fixed
    ? "固定单图"
    : nsfw
      ? "NSFW 模式"
      : !running
        ? "轮播未运行"
        : "暂无下一张";
  const nextTip = fixed
    ? TIPS.nextWallpaperMissing.fixed
    : nsfw
      ? TIPS.nextWallpaperMissing.nsfw
      : !running
        ? TIPS.nextWallpaperMissing.stopped
        : (screenState?.candidateCount ?? 0) === 0
          ? TIPS.nextWallpaperMissing.empty
          : TIPS.nextWallpaper;
  // 倒计时用这块屏自己的间隔（各屏互不影响）。三种情况不显示：
  // 固定单图 / NSFW 模式 / 轮播没在运行——那时"还有几秒"没有意义。
  //
  // **候选为 0 时也不显示**：那种屏根本换不了纸（没有可播的图），
  // 写着"21 秒后"却永远不换，比不写更糟——这一格下面已经说了「暂无下一张」，
  // 悬浮提示里也写清了原因（筛选后没有可用的图片）。
  //
  // 暂停时**数字照样显示**（后端的语义是"计时停住"，见 §10.38，因此这个数就是
  // 冻结的剩余时间），状态字样另占一格并排在它左边——用户 2026-10 的要求：
  // "暂停计时时，能显示当前的倒计时具体数值，也就是暂停的字样不要覆盖数值，
  // 显示在旁边"。三种暂停都算（全局的、这块屏手动的、全屏自动的）；此前只看了
  // 每屏那两位，于是"直接按暂停时倒计时还在走"，两个口径被用户一眼看出来。
  const noCandidates = (screenState?.candidateCount ?? 0) === 0;
  const nextBadge =
    fixed || nsfw || !running || noCandidates
      ? undefined
      : `${screenState?.nextInSeconds ?? 0} 秒后`;
  const pausedTag =
    nextBadge !== undefined && (pausedGlobal || screenPaused || pausedByFullscreen)
      ? "已暂停"
      : undefined;

  // 显示名（用户双击改的那个）。空 = 用真实显示器名。
  const label = (saved?.label ?? "").trim();
  const displayName = label || binding.monitorName;
  const [editingName, setEditingName] = React.useState(false);
  const [nameDraft, setNameDraft] = React.useState("");

  const startRename = () => {
    setNameDraft(label);
    setEditingName(true);
  };

  /**
   * 提交显示名。
   *
   * 只提交**变了**的值（失焦一次就存一次会把日志刷满），并且原样交给后端清洗：
   * 用户要求"支持英文和数字就行"，因此这里与后端一样把中文等字符挡在输入框外
   * （见 `sanitizeMonitorLabel`），两边规则一致才不会出现"打进去了但存下来没了"。
   */
  const commitName = () => {
    setEditingName(false);
    const next = nameDraft.trim();
    if (next === label) return;
    void onPatch({ monitors: { [binding.monitorName]: { label: next } } });
  };

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement(
      "div",
      { className: "wwp-screen-head" },
      React.createElement(
        "div",
        null,
        React.createElement(
          "span",
          { className: "wwp-screen-name" },
          editingName
            ? React.createElement("input", {
                className: "wwp-input wwp-input-name",
                autoFocus: true,
                placeholder: "英文 / 数字",
                value: nameDraft,
                "aria-label": "这块屏的显示名",
                onChange: (e: React.ChangeEvent<HTMLInputElement>) =>
                  // 字符过滤与后端一致：只留英文、数字、空格、- 和 _。
                  setNameDraft(e.target.value.replace(/[^A-Za-z0-9 _-]/g, "")),
                onBlur: commitName,
                onKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    commitName();
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    setEditingName(false);
                  }
                },
              })
            : React.createElement(
                Tooltip,
                {
                  text: label
                    ? TIPS.screenNameRenamed(binding.monitorName)
                    : TIPS.screenNameRenamedHint(binding.monitorName),
                  children: React.createElement(
                    "span",
                    { className: "wwp-screen-name-text", onDoubleClick: startRename },
                    displayName,
                  ),
                },
              ),
          binding.primary ? React.createElement("span", { className: "wwp-tag" }, "主") : null,
          portrait ? React.createElement("span", { className: "wwp-tag" }, "竖屏") : null,
          React.createElement("span", { className: "wwp-tag" }, position),
          fixed ? React.createElement("span", { className: "wwp-tag" }, "固定单图") : null,
          // 两个暂停原因分开显示，因此"谁在停着这块屏"一眼能看出来。
          // 自动那一条放在前面：它是用户没主动做过的那个状态，最需要被解释。
          !fixed && pausedByFullscreen
            ? React.createElement(
                "span",
                { className: "wwp-tag wwp-tag-warn" },
                "全屏中·已暂停",
              )
            : null,
          !fixed && screenPaused
            ? React.createElement("span", { className: "wwp-tag wwp-tag-warn" }, "已单独暂停")
            : null,
        ),
        React.createElement(
          "div",
          { className: "wwp-dim" },
          `${binding.width} × ${binding.height}`,
        ),
      ),
      // 卡片右上角：**删除当前壁纸**在左，「暂停 / 继续」在右（用户要求的位置）。
      //
      // 这两个按钮的显示条件完全相同（`running && !nsfw && !fixed`）——
      // 固定单图的屏没有"轮到它换纸"这回事，NSFW 模式里也没有轮播，
      // 因此那两种情况下"删掉当前这张并换下一张"本身就说不通，
      // 按了也只会让人以为按钮坏了。
      React.createElement(
        "div",
        { className: "wwp-screen-actions" },
        showScreenButtons
          ? React.createElement(
              "button",
              {
                className: "wwp-btn wwp-btn-quiet wwp-btn-danger",
                // 还没有显示过任何图片时没有可删的东西，用禁用表达（而不是隐藏：
                // 位置跳一下比灰一下更让人困惑）。
                disabled: busy || !currentPath,
                title: currentPath ? TIPS.deleteWallpaper : TIPS.deleteWallpaperDisabled,
                onClick: () => void onDeleteCurrent(),
              },
              "删除当前壁纸",
            )
          : null,
        showScreenButtons
          ? React.createElement(
              "button",
              {
                className: screenPaused ? "wwp-btn wwp-btn-on" : "wwp-btn wwp-btn-quiet",
                disabled: busy,
                title: screenPaused ? TIPS.screenPauseResume : TIPS.screenPausePause,
                onClick: () => void onScreenPause(!screenPaused),
              },
              screenPaused ? "继续" : "暂停",
            )
          : null,
      ),
    ),

    // 壁纸预览：这块屏**正在显示**的那张，以及**即将切换**的那张（用户要求
    // 在每屏的设置里直接看见壁纸本身，而不只是一行文件名）。
    //
    // 两格**并排**：把这两张放在一起看才有用（"现在这张 → 下一张那张"），
    // 上下堆叠会把卡片拉得很长，两块屏并排时就要滚很久。
    // 宽度由 grid 平分成两半，缩略图 object-fit: contain——竖图也不会被裁掉一截。
    React.createElement(
      "div",
      {
        className: "wwp-shot-grid",
        // 这一格属于哪块屏。卡上显示的名字用户改过（可能是"Left"这种），
        // 与后端认的显示器名对不上；排版自检要拿后端状态来算"预期路径"，
        // 因此把真实显示器名挂在这里，别让探针去猜排列顺序。
        "data-monitor": binding.monitorName,
      },
      React.createElement(WallpaperShot, {
        label: "当前显示",
        path: currentPath || null,
        placeholder: "尚未换过",
        tip: currentPath ? TIPS.currentDisplay : TIPS.currentDisplayEmpty,
        // 点缩略图 = 用系统默认看图程序打开这张图（用户 2026-10 时这里是复制
        // 路径，1.11.0 改成"打开看"）。
        onOpen: currentPath ? () => void openCurrent() : undefined,
        // 点下面那行路径 = 打开它所在的文件夹并选中它（不再复制路径）。
        onReveal: currentPath ? () => void revealImage(currentPath) : undefined,
        // 右键那行路径 = 复制文件名（不含扩展名）。
        onCopyName: currentPath ? () => void copyImageName(currentPath) : undefined,
      }),
      React.createElement(WallpaperShot, {
        label: "下一张",
        badge: nextBadge,
        pausedTag,
        path: nextPath || null,
        placeholder: nextPlaceholder,
        tip: nextTip,
        // 点缩略图 = 立刻把**这块屏**切到这一张（用户 2026-10 的要求）。
        // 只有真有下一张时才给回调；固定单图/NSFW 模式/轮播没跑/筛完没图时
        // 这一格显示的是占位说明，给了回调也切不动。
        onOpen:
          nextPath && !fixed && !nsfw && running && !noCandidates
            ? () => void onNextScreen()
            : undefined,
        // 点下面那行路径 = 只打开它所在的文件夹并选中它，**不切换壁纸**
        // （1.11.0 的要求：这一行以前是切换壁纸，与缩略图重复了）。
        onReveal: nextPath ? () => void revealImage(nextPath) : undefined,
        // 右键那行路径 = 复制文件名（不含扩展名），同样不切换壁纸。
        onCopyName: nextPath ? () => void copyImageName(nextPath) : undefined,
      }),
    ),

    // 每屏的四个信息行（播放列表 / 列表设置 / 固定单图 / NSFW 壁纸）放在
    // **同一个 grid** 里。
    //
    // ## 为什么必须是一个网格（用户 2026-10 的两条要求）
    //
    // 用户要求：①"保持文字和右侧的方框合理紧密接触，不要让 UI 出现大面积的空白"；
    // ②"播放列表后面的方框太短了……一定要和下面两个一样长"。
    //
    // - 标签列用 `max-content`：这四个标签**都是四个汉字**，因此宽度一致、
    //   方框左边缘仍然对齐，而它们与方框之间不再隔着那 116px 固定列留下的空档；
    // - 方框列是整个网格**共享的一条 `1fr`**，因此三行的方框必然同宽
    //   （各自成网格时按钮数不同会算出不同的宽度），而且吃掉标签列缩掉之后
    //   腾出来的全部宽度——比上一版按 `calc(100% - 160px)` 预留按钮位宽得多；
    // - 每个格子用 `grid-column` 显式落位，没有按钮的行就让那两列空着
    //   （播放列表那行的按钮列是空的，因此它的方框与下面两行**一样长**）。
    React.createElement(
      "div",
      { className: "wwp-monitor-fields" },
      // 播放列表选择器。这是"这块屏播什么"的唯一开关——顺序/间隔/填充都跟着它走，
      // 因此放在最上面。
      //
      // 悬浮提示挂在**标签**上（原来挂在 `<select>` 上）：Tooltip 会在孩子外面
      // 套一层 inline-block 锚点，而 `<select>` 的宽度靠 `100%` 撑着——
      // 锚点让那个百分比落到一个"由内容决定的"宽度上，实测真机里它会把方框
      // 算成 75px（Chrome 里却是正常的），正是用户看到的"太短、看不到有用信息"。
      React.createElement(
        "span",
        { className: "wwp-label wwp-cell-label" },
        React.createElement(Tooltip, {
          text: fixed ? TIPS.playlistSelectFixed : TIPS.playlistSelect,
          children: React.createElement("span", null, "播放列表"),
        }),
      ),
      React.createElement(
        "span",
        { className: "wwp-value wwp-cell-value" },
        React.createElement(
          "select",
          {
            className: "wwp-input wwp-input-sm wwp-monitor-field",
            value: currentId,
            // 固定单图时选择器仍在，但会被后端忽略——用淡显表达"它现在不算数"，
            // 而不是禁用（禁用会让人以为界面坏了）。
            style: fixed ? { opacity: 0.55 } : undefined,
            onChange: (e: React.ChangeEvent<HTMLSelectElement>) =>
              void onPatch({
                monitors: { [binding.monitorName]: { currentPlaylist: e.target.value } },
              }),
          },
          [
            React.createElement("option", { key: "", value: "" }, "（未选择 —— 这块屏不换纸）"),
            ...playlists.map((p) =>
              React.createElement(
                "option",
                { key: p.id, value: p.id },
                `${p.name}（${p.libraries.length} 个图库）`,
              ),
            ),
          ],
        ),
      ),
      // 「列表设置」= 选中列表的间隔与填充（顺序固定为随机）。
      // 值那一格横跨后面的按钮列，因此这句话仍然占满整行宽度、不会折行。
      ...(fixed
        ? [
            React.createElement(
              "span",
              { className: "wwp-label wwp-cell-label", key: "list-label" },
              React.createElement(Tooltip, {
                text: TIPS.listSettingsFixed,
                children: React.createElement("span", null, "列表设置"),
              }),
            ),
            React.createElement(
              "span",
              { className: "wwp-value wwp-dim wwp-cell-wide", key: "list-value" },
              "固定单图生效中（播放列表被忽略）",
            ),
          ]
        : playlist
          ? [
              React.createElement(
                "span",
                { className: "wwp-label wwp-cell-label", key: "list-label" },
                React.createElement(Tooltip, {
                  text: TIPS.listSettings,
                  children: React.createElement("span", null, "列表设置"),
                }),
              ),
              React.createElement(
                "span",
                { className: "wwp-value wwp-dim wwp-cell-wide", key: "list-value" },
                `每 ${playlist.intervalSeconds} 秒 · ${
                  FIT_OPTIONS.find((o) => o.value === playlist.fitMode)?.label ?? playlist.fitMode
                }`,
              ),
            ]
          : [
              React.createElement(
                "span",
                { className: "wwp-label wwp-cell-label", key: "list-label" },
                "列表设置",
              ),
              React.createElement(
                "span",
                { className: "wwp-value wwp-bad wwp-cell-wide", key: "list-value" },
                playlists.length === 0
                  ? "还没有列表，先到「列表」页新建一个"
                  : "还没选播放列表 —— 这块屏不会换纸",
              ),
            ]),

      // 固定单图：设了它就只显示这一张，不轮播。**优先于播放列表**。
      React.createElement(
        "span",
        { className: "wwp-label wwp-cell-label" },
        React.createElement(Tooltip, {
          text: TIPS.singleImage,
          children: React.createElement("span", null, "固定单图"),
        }),
      ),
      React.createElement(
        "span",
        { className: "wwp-value wwp-cell-value" },
        React.createElement("input", {
          className: "wwp-input wwp-monitor-field",
          // 占位文字刻意短：这一格只有一百多像素宽，长句会被截成半截
          // （"留空 = 按播放列"这种），看起来像坏了。完整含义在标签的悬浮提示里。
          placeholder: "留空 = 轮播",
          value: singlePath,
          onChange: (e: React.ChangeEvent<HTMLInputElement>) => setSinglePath(e.target.value),
          onBlur: () => sendSinglePath(singlePath),
        }),
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet wwp-cell-btn-a",
          disabled: busy,
          title: TIPS.pickImage,
          onClick: () => void pickSingleImage(),
        },
        "选择图片…",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet wwp-cell-btn-b",
          disabled: busy || singlePath.trim() === "",
          title: TIPS.clearSingle,
          onClick: () => {
            setSinglePath("");
            sendSinglePath("");
          },
        },
        "清除",
      ),

      // NSFW 壁纸：NSFW 模式时这块屏显示哪张图。
      React.createElement(
        "span",
        { className: "wwp-label wwp-cell-label" },
        React.createElement(Tooltip, {
          text: TIPS.nsfwWallpaper,
          children: React.createElement("span", null, "NSFW 壁纸"),
        }),
      ),
      React.createElement(
        "span",
        { className: "wwp-value wwp-cell-value" },
        React.createElement("input", {
          className: "wwp-input wwp-monitor-field",
          placeholder: "还没设置",
          value: nsfwPath,
          onChange: (e: React.ChangeEvent<HTMLInputElement>) => setSafePath(e.target.value),
          onBlur: () => sendNsfwPath(nsfwPath),
        }),
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet wwp-cell-btn-a",
          disabled: busy,
          title: TIPS.pickImage,
          onClick: () => void pickNsfwImage(),
        },
        "选择图片…",
      ),
    ),

    // 数字与比例筛选**合成一行**（用户 2026-10 的要求）。
    //
    // 原来这是两行：上面一行三个小块（已切换 / 候选 / 最近错误），下面一行是
    // 一个右对齐的字段行（图像比例筛选）。用户给的目标形式是
    //
    //     66 次 · 12 ms　520 张　[1.0] ～ [2.0]
    //
    // 并明确要求把「已切换 / 候选 / 图像比例筛选」这三个词去掉——"使两行的信息
    // 能够集中到一行，精简页面"。因此：
    //
    //   1. 三个小块的**标签全去掉**，只留数字。数字的含义挪进悬浮提示
    //      （否则"66 次"是什么就没人知道了），提示挂在**数字**上；
    //   2. 比例筛选的两个输入框直接作为这一行的最后两项，`～` 上挂原来那句
    //      说明——字段名没了之后，那句提示就是唯一的解释；
    //   3. 原来分隔"设置"与"数字"的那条细线挪到这一行上方
    //      （`.wwp-stats-fields`），卡片的分段感保持不变。
    //
    // 「当前显示」原先也是这一行的一个小块（一行省略路径），后来升级成了上面的
    // 预览格（看得见图、还带「下一张」）——同一件事不留两个显示位置。
    React.createElement(
      "div",
      {
        className: running && !nsfw
          ? "wwp-stats wwp-stats-fields"
          : "wwp-stats wwp-stats-quiet wwp-stats-fields",
      },
      // 固定单图的屏不轮播，"切换次数/候选"对它没有意义——显示它是什么模式。
      // 这一块**保留**它自己的标签：它只在固定单图时出现，没有别的线索能说明
      // "固定单图（不轮播）"这一串字是什么。
      fixed
        ? React.createElement(
            "div",
            { className: "wwp-stat" },
            React.createElement("span", { className: "wwp-stat-key" }, "模式"),
            React.createElement(
              "span",
              { className: "wwp-stat-value" },
              "固定单图（不轮播）",
            ),
          )
        : null,
      running && !nsfw && !fixed
        ? React.createElement(
            "div",
            { className: "wwp-stat" },
            React.createElement(Tooltip, {
              text: TIPS.switchCount,
              children: React.createElement(
                "span",
                { className: "wwp-stat-value" },
                `${screenState?.changes ?? 0} 次 · ${screenState?.lastChangeMillis ?? 0} ms`,
              ),
            }),
          )
        : null,
      running && !nsfw && !fixed
        ? React.createElement(
            "div",
            { className: "wwp-stat" },
            React.createElement(Tooltip, {
              text: TIPS.candidateCount,
              children: React.createElement(
                "span",
                { className: "wwp-stat-value" },
                `${screenState?.candidateCount ?? binding.candidates ?? 0} 张`,
              ),
            }),
          )
        : null,
      screenState?.lastError
        ? React.createElement(
            "div",
            { className: "wwp-stat" },
            React.createElement("span", { className: "wwp-stat-key" }, "最近错误"),
            React.createElement("span", { className: "wwp-stat-value wwp-bad" }, screenState.lastError),
          )
        : null,
      // 比例筛选的两个输入框：同一个区间（[下限, 上限)）的两端。
      //
      // 格子刻意做小（88px，见 index.css 的 `.wwp-aspect-range`）：要填的是 1.3
      // 这种两三个字符的比值，拉满宽度只会把这一行挤到折行。
      React.createElement(
        "span",
        { className: "wwp-inline wwp-aspect-range" },
        React.createElement("input", {
          className: "wwp-input wwp-input-sm",
          placeholder: "下限",
          title: TIPS.aspectMinInput,
          value: aspectMin,
          onChange: (e: React.ChangeEvent<HTMLInputElement>) => setAspectMin(e.target.value),
          onBlur: sendFilter,
        }),
        React.createElement(Tooltip, {
          text: TIPS.aspectRange,
          children: React.createElement("span", { className: "wwp-dim" }, "～"),
        }),
        React.createElement("input", {
          className: "wwp-input wwp-input-sm",
          placeholder: "上限",
          title: TIPS.aspectMaxInput,
          value: aspectMax,
          onChange: (e: React.ChangeEvent<HTMLInputElement>) => setAspectMax(e.target.value),
          onBlur: sendFilter,
        }),
      ),
    ),
  );
}

/**
 * 一行字段：左边固定宽度的标签（悬浮提示挂在这里），右边是输入控件。
 *
 * `tip` 是鼠标悬停时显示的说明。**所有补充说明都走它**，界面上不再有常驻小字
 * （用户要求"UI 不需要显示这么详细的说明，对应的说明用悬浮提示"）。
 *
 * 导出给设置页用：托盘单击行为那一项与这里的快捷键是同一类字段，
 * 共用同一个布局才能保证"各个方框对齐"（见 §7.15 那个锚点坑）。
 *
 * ## Tooltip 必须套在 `<label>` 里面
 *
 * `Tooltip` 组件会先渲染一层 inline-block 的锚点再挂浮层。把锚点套在 `<label>`
 * **外面**时，`.wwp-field` 的网格项就变成了那个锚点，而标签自己的固定列宽
 * 落在一个普通块级元素上、不起作用——标签于是被挤窄并折成两行
 * （用户截图里的"长宽比区\n间"就是这么来的）。套在里面则标签仍是网格项。
 *
 * 2026-10：原来的 `end` 开关（整行右对齐）随「图像比例筛选」那一行并入数字行
 * 而删掉——没有使用者了。
 */
export function Field({
  label,
  tip,
  children,
}: {
  label: string;
  /** 悬浮提示的正文（鼠标悬停时跟随鼠标显示）。 */
  tip?: string;
  children: React.ReactNode;
}) {
  return React.createElement(
    "div",
    { className: "wwp-field" },
    // 提示挂在**标签**上而不是输入框上：输入框上悬停会与"点击进入编辑"
    // 冲突，而且输入框里还要放 placeholder。标签是更自然的悬停目标。
    React.createElement(
      "label",
      { className: "wwp-field-label" },
      tip
        ? React.createElement(Tooltip, {
            text: tip,
            children: React.createElement("span", null, label),
          })
        : label,
    ),
    React.createElement("div", { className: "wwp-field-body" }, children),
  );
}

/** 空串 → null（清除条件），非法输入 → null。 */
function parseOptional(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

/**
 * 一个路径的**文件名，不含扩展名**（右键复制走的就是它）。
 *
 * 规则只有两条，但两条都得想清楚：
 *
 * - 分隔符按 `\` 与 `/` 都切：后端给的是 Windows 路径（`\`），而日志、索引里
 *   混合着 `/`（`index.jsonl` 的 `p` 字段就是 `/`）；
 * - **点在最开头时不算扩展名**：`.gitignore` 这种名字整段都是文件名，
 *   按"最后一个点"切会得到空串——复制一个空文件名比不复制更让人困惑。
 *   没有点的情况下同理，整段保留。
 */
function fileStem(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? path;
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(0, dot) : name;
}

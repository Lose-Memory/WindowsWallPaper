import React from "react";
import {
  api,
  formatBytes,
  type CleanupItem,
  type CleanupReport,
  type SettingsInfo,
  type SettingsPatch,
} from "../api";
import { Field, HotkeyField } from "./PlaybackPage";
import { Tooltip } from "./Tooltip";
import { TIPS } from "../tooltip-texts";

/** 单击托盘图标可以选的两件事。值与后端 `TrayClickAction` 的 serde 写法一致。 */
const TRAY_CLICK_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "open-window", label: "打开主界面" },
  { value: "next-wallpaper", label: "换一张壁纸" },
];

interface Props {
  settings: SettingsInfo;
  onSettingsChanged: (next: SettingsInfo) => void;
  notify: (text: string, kind?: "ok" | "error") => void;
}

/**
 * 设置页：把与"播放这一屏在放什么"无关的东西都收在这里。
 *
 * ## 为什么要单独一页（用户的要求）
 *
 * 原先这些都挤在播放页：全局快捷键、启动与窗口开关。播放页因此变得很长，
 * 用户**必须滚动**才能看到下面两块屏的设置——而"两块屏并排对比"正是那一页存在
 * 的唯一理由，被滚动条破坏掉就白做了。
 *
 * 拆开之后分工很清楚：
 *
 * - **播放页**：现在在放什么、怎么放（启停、每屏的内容来源与筛选）；
 * - **设置页**：程序怎么运行（快捷键、启动行为、窗口行为）。
 */
export function SettingsPage({ settings, onSettingsChanged, notify }: Props) {
  const general = settings.general;
  const [busy, setBusy] = React.useState(false);

  const apply = React.useCallback(
    async (patch: SettingsPatch, message = "设置已保存") => {
      setBusy(true);
      try {
        onSettingsChanged(await api.settingsUpdate(patch));
        notify(message);
      } catch (cause) {
        // 保存失败必须浮出来：只在页面里显示的话，用户滚到别处就看不到，
        // 会以为已经保存成功了。
        notify(`保存失败：${String(cause)}`, "error");
      } finally {
        setBusy(false);
      }
    },
    [notify, onSettingsChanged],
  );

  /**
   * 记住「清理程序目录」那张卡片是展开还是折叠（用户要求"记忆这个选项"）。
   *
   * 与上面 `apply` 的区别只有一个：**成功时不浮提示**。这是每次进出都要点一下的
   * 开关，每点一次浮一句"设置已保存"会把右上角刷满，而"卡片合上了"本身就是反馈。
   * 失败仍然要浮——不然用户以为记住了，其实没记住。
   */
  const setCleanupExpanded = React.useCallback(
    async (expanded: boolean) => {
      try {
        onSettingsChanged(await api.settingsUpdate({ cleanupExpanded: expanded }));
      } catch (cause) {
        notify(`保存失败：${String(cause)}`, "error");
      }
    },
    [notify, onSettingsChanged],
  );

  return React.createElement(
    "div",
    null,
    React.createElement(
      "div",
      { className: "wwp-page-head" },
      React.createElement("h1", { className: "wwp-title" }, "设置"),
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "全局快捷键、启动与窗口行为。",
      ),
    ),

    // 全局快捷键
    //
    // 那行"在任何程序里都有效。留空 = 不注册"原先常驻在这里，现在并进了每个
    // 输入框的悬浮提示（用户要求界面不堆说明文字）。
    React.createElement(
      "div",
      { className: "wwp-card" },
      React.createElement("h2", { className: "wwp-h2" }, "全局快捷键"),
      React.createElement(HotkeyField, {
        label: "NSFW 模式",
        help: TIPS.nsfwHotkey,
        value: general.nsfwHotkey,
        onSave: (value: string) => apply({ nsfwHotkey: value }, "NSFW 模式快捷键已保存"),
      }),
      React.createElement(HotkeyField, {
        label: "暂停 / 继续",
        help: TIPS.pauseHotkey,
        value: general.pauseHotkey,
        onSave: (value: string) => apply({ pauseHotkey: value }, "暂停快捷键已保存"),
      }),
      React.createElement(HotkeyField, {
        label: "下一张",
        help: TIPS.nextHotkey,
        value: general.nextHotkey,
        onSave: (value: string) => apply({ nextHotkey: value }, "「下一张」快捷键已保存"),
      }),
      React.createElement(HotkeyField, {
        label: "上一张",
        help: TIPS.previousHotkey,
        value: general.previousHotkey,
        onSave: (value: string) => apply({ previousHotkey: value }, "「上一张」快捷键已保存"),
      }),
      // 这个键不碰壁纸，只把窗口翻出来。放在最后：它是"想改设置"时才按的，
      // 与上面四个"正在看壁纸时按"的动作不是一类。
      React.createElement(HotkeyField, {
        label: "显示主界面",
        help: TIPS.showWindowHotkey,
        value: general.showWindowHotkey,
        onSave: (value: string) =>
          apply({ showWindowHotkey: value }, "「显示主界面」快捷键已保存"),
      }),
    ),

    // 启动与窗口
    React.createElement(
      "div",
      { className: "wwp-card" },
      React.createElement("h2", { className: "wwp-h2" }, "启动与窗口"),
      React.createElement(
        "div",
        { className: "wwp-switch-list" },
        React.createElement(Switch, {
          label: "启动时不显示主界面",
          help: TIPS.switchStartMinimized,
          checked: general.startMinimized,
          disabled: busy,
          onChange: (value: boolean) => void apply({ startMinimized: value }),
        }),
        React.createElement(Switch, {
          label: "启动时自动开始轮播",
          help: TIPS.switchAutoStart,
          checked: general.autoStart,
          disabled: busy,
          onChange: (value: boolean) => void apply({ autoStart: value }),
        }),
        React.createElement(Switch, {
          label: "全屏时暂停这块屏的轮播",
          help: TIPS.switchPauseOnFullscreen,
          checked: general.pauseOnFullscreen,
          disabled: busy,
          onChange: (value: boolean) =>
            void apply(
              { pauseOnFullscreen: value },
              value
                ? "已开启：某块屏有程序全屏时只暂停那块屏"
                : "已关闭：全屏不再暂停轮播",
            ),
        }),
        React.createElement(Switch, {
          label: "关闭窗口时最小化到托盘",
          help: TIPS.switchMinimizeToTray,
          checked: general.minimizeToTray,
          disabled: busy,
          onChange: (value: boolean) => void apply({ minimizeToTray: value }),
        }),
      ),
      // 单击托盘图标做什么（用户要求可配）：换一张是高频动作，
      // 打开设置界面一天也用不到一次，因此允许把单击让给"换一张"。
      React.createElement(Field, {
        label: "单击托盘图标",
        tip: TIPS.trayClick,
        children: React.createElement(
          "select",
          {
            className: "wwp-input wwp-input-sm",
            value: general.trayClickAction,
            disabled: busy,
            "aria-label": "单击托盘图标的行为",
            onChange: (e: React.ChangeEvent<HTMLSelectElement>) =>
              void apply(
                { trayClickAction: e.target.value },
                e.target.value === "next-wallpaper"
                  ? "已改为：单击换一张壁纸，双击打开主界面"
                  : "已改为：单击打开主界面",
              ),
          },
          TRAY_CLICK_OPTIONS.map((o) =>
            React.createElement("option", { key: o.value, value: o.value }, o.label),
          ),
        ),
      }),
    ),

    // ## 这里**没有**回收站（用户 2026-10 的要求）
    //
    // 原先设置页底部还有一张「回收站」卡片：里面有几项、在哪、以及一个
    // 「打开回收站文件夹」。用户要求"设置页面不要出现回收站的相关设置，
    // 这部分相关信息只出现在回收站的页面即可"——回收站页顶部本来就有
    // 「打开文件夹 / 刷新 / 全部删除」，同一件事不留两个入口（§10.8）。
    //
    // 更早还删过一张「数据与设置文件」卡片，理由同一条：诊断页已经把路径列全了。
    React.createElement(CleanupCard, {
      expanded: general.cleanupExpanded,
      onExpandedChange: setCleanupExpanded,
      notify,
    }),
  );
}

/**
 * 「清理程序目录」卡片。
 *
 * ## 交互为什么是"先列清单、再确认"
 *
 * 用户的要求是"删除的东西可以列出来，默认全选，用户可以选择勾选或取消一部分，
 * 待用户确认后再删除"。因此这里**不**做"点一下直接清"：删除是不可逆的，
 * 而清单本身就是他要看的东西（每一项多大、是什么）。
 *
 * ## 什么能删由后端决定
 *
 * 界面不写死目录名——那是**删除**。清单来自 `cleanup_items`，每项带自己的
 * 说明与大小。回收站不在清单里（用户明确要求"不要清空回收站"）。
 *
 * ## 展开还是折叠由设置决定（用户 2026-10 的要求）
 *
 * 用户原话："设置页清理程序目录默认展开，用户可以手动选择折叠，记忆这个选项。"
 * 因此 `open` 不是这个组件自己的 state，而是 `[general].cleanup_expanded`：
 *
 * - **默认展开**（`General::default()` 给 true）——那张卡片里的东西是"看一眼就知道
 *   要不要清"，默认收起来等于把功能藏起来；
 * - 用户点一次就写回设置，下次进这一页照旧收着。
 */
function CleanupCard({
  expanded,
  onExpandedChange,
  notify,
}: {
  /** 当前是否展开（来自设置，默认展开）。 */
  expanded: boolean;
  /** 用户切换展开/折叠：写回设置，让下次进来时保持一致。 */
  onExpandedChange: (expanded: boolean) => Promise<void>;
  notify: (text: string, kind?: "ok" | "error") => void;
}) {
  const [items, setItems] = React.useState<CleanupItem[] | null>(null);
  const [selected, setSelected] = React.useState<Record<string, boolean>>({});
  const [busy, setBusy] = React.useState(false);
  const [report, setReport] = React.useState<CleanupReport | null>(null);

  /**
   * 去盘一遍清单。要遍历几个目录（还有递归求体积），因此只在**展开的那一刻**做。
   *
   * 失败时不重试：自动那一条路（下面的 effect）只走一次，用户想再试就收起再展开，
   * 或者关掉页面重进——不该在后台反复盘目录。
   */
  const load = React.useCallback(async () => {
    setBusy(true);
    try {
      const list = await api.cleanupItems();
      setItems(list);
      // 默认全选（用户要求），且只默认勾"能删的"。
      setSelected(Object.fromEntries(list.map((item) => [item.id, item.removable])));
      setReport(null);
    } catch (cause) {
      notify(`读取可清理项失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  }, [notify]);

  /**
   * 展开着就必须有清单——**包括"一进设置页它就展开着"这一种**（默认展开）。
   *
   * 用一个 ref 记住"这次展开已经盘过了"，而不是靠 `items === null` 判断：
   * 盘失败时 `items` 仍然是 null，靠它判断会变成"失败 → 再盘 → 再失败"的循环。
   * 折叠回去时把标记清掉，于是下次展开会重新盘一遍（用户可能在资源管理器里
   * 刚删过东西，数字得跟着变）。
   */
  const autoLoaded = React.useRef(false);
  React.useEffect(() => {
    if (expanded && !autoLoaded.current) {
      autoLoaded.current = true;
      void load();
    }
    if (!expanded) {
      autoLoaded.current = false;
    }
  }, [expanded, load]);

  const run = React.useCallback(async () => {
    const ids = Object.entries(selected)
      .filter(([, on]) => on)
      .map(([id]) => id);
    if (ids.length === 0) {
      notify("没有勾选任何一项", "error");
      return;
    }
    setBusy(true);
    try {
      const result = await api.cleanupRun(ids);
      setReport(result);
      // 用后端**重新盘出来**的清单刷新：清完可能还剩一部分
      // （比如某个日志文件正被别的进程读着），直接显示旧数字会骗人。
      setItems(result.items);
      setSelected(Object.fromEntries(result.items.map((item) => [item.id, item.removable])));
      const failed = result.outcomes.filter((outcome) => !outcome.ok);
      notify(
        failed.length === 0
          ? `已清理 ${result.outcomes.length} 项，释放 ${formatBytes(result.freed)}`
          : `已清理 ${result.outcomes.length - failed.length} 项，释放 ${formatBytes(result.freed)}；` +
              `${failed.length} 项失败：${failed.map((o) => `${o.label}（${o.error}）`).join("；")}`,
        failed.length === 0 ? "ok" : "error",
      );
    } catch (cause) {
      notify(`清理失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  }, [notify, selected]);

  const total = (items ?? [])
    .filter((item) => selected[item.id])
    .reduce((sum, item) => sum + item.bytes, 0);

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement(
      "div",
      { className: "wwp-control-row" },
      React.createElement("h2", { className: "wwp-h2" }, "清理程序目录"),
      React.createElement("span", { className: "wwp-spacer" }),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy,
          // 只写设置：界面跟着 `expanded` 走（它是设置的投影，不另存一份 state，
          // 否则"记忆下来的折叠"与"眼前这一次"会打架）。
          onClick: () => void onExpandedChange(!expanded),
        },
        expanded ? "收起" : "查看可清理项",
      ),
    ),

    // 收起时这一页只有标题行 + 一个按钮，因此不会因为清理功能变长。
    expanded
      ? React.createElement(
          React.Fragment,
          null,
          React.createElement(
            "div",
            { className: "wwp-dim", style: { marginBottom: 6 } },
            "以下内容删除后不影响程序运行（回收站与设置、图库索引不在其中）。",
          ),
          items === null
            ? React.createElement("div", { className: "wwp-dim" }, "正在读取可清理项…")
            : items.length === 0
              ? React.createElement("div", { className: "wwp-dim" }, "没有可清理的内容。")
              : React.createElement(
                  "div",
                  { className: "wwp-cleanup-list" },
                  items.map((item) =>
                    React.createElement(
                      "label",
                      { key: item.id, className: "wwp-cleanup-item" },
                      React.createElement("input", {
                        type: "checkbox",
                        className: "wwp-check",
                        checked: selected[item.id] ?? false,
                        disabled: busy || !item.removable,
                        onChange: (e: React.ChangeEvent<HTMLInputElement>) =>
                          setSelected((prev) => ({ ...prev, [item.id]: e.target.checked })),
                      }),
                      React.createElement(
                        "span",
                        { className: "wwp-cleanup-text" },
                        React.createElement(
                          "span",
                          { className: "wwp-cleanup-head" },
                          React.createElement(
                            "span",
                            { className: "wwp-switch-label" },
                            item.label,
                          ),
                          React.createElement(
                            "span",
                            { className: "wwp-value" },
                            item.bytes > 0 ? formatBytes(item.bytes) : "空",
                          ),
                        ),
                        React.createElement("span", { className: "wwp-dim" }, item.detail),
                        item.removable
                          ? null
                          : React.createElement(
                              "span",
                              { className: "wwp-dim" },
                              item.blockedReason ?? "现在不能清理",
                            ),
                      ),
                    ),
                  ),
                ),
          React.createElement(
            "div",
            { className: "wwp-control-row", style: { marginTop: 10 } },
            React.createElement(
              "span",
              { className: "wwp-dim" },
              `已勾选 ${formatBytes(total)}`,
            ),
            React.createElement("span", { className: "wwp-spacer" }),
            React.createElement(
              "button",
              {
                className: "wwp-btn wwp-btn-danger",
                disabled: busy || total === 0,
                onClick: () => void run(),
              },
              busy ? "正在清理…" : "删除勾选的内容",
            ),
          ),
          report
            ? React.createElement(
                "div",
                { className: "wwp-note" },
                report.outcomes.length === 0
                  ? "没有执行任何清理。"
                  : `本次释放 ${formatBytes(report.freed)}：` +
                    report.outcomes
                      .map((outcome) =>
                        outcome.ok
                          ? `${outcome.label} ${formatBytes(outcome.freed)}`
                          : `${outcome.label} 失败（${outcome.error}）`,
                      )
                      .join("；"),
              )
            : null,
        )
      : null,
  );
}

/**
 * 一行开关：左边是说明，右边是复选框。
 *
 * 说明改成**悬浮提示**（用户要求界面上不常驻说明文字）：原先每行下面挂一句小字，
 * 三行开关因此高了一倍，而这三句话都是"看一眼就够"的内容。
 */
function Switch({
  label,
  help,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  /** 悬浮提示的正文（鼠标悬停时跟随鼠标显示）。 */
  help?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  return React.createElement(
    "label",
    { className: "wwp-switch" },
    React.createElement(
      "span",
      { className: "wwp-switch-text" },
      help
        ? React.createElement(Tooltip, {
            text: help,
            children: React.createElement("span", { className: "wwp-switch-label" }, label),
          })
        : React.createElement("span", { className: "wwp-switch-label" }, label),
    ),
    React.createElement("input", {
      type: "checkbox",
      className: "wwp-check",
      checked,
      disabled,
      onChange: (e: React.ChangeEvent<HTMLInputElement>) => onChange(e.target.checked),
    }),
  );
}

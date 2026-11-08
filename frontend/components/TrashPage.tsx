import React from "react";
import {
  api,
  formatBytes,
  formatTime,
  onTrashEmptyProgress,
  type TrashEntryView,
} from "../api";
import { TIPS } from "../tooltip-texts";

interface Props {
  /** 右上角浮动提示。由 App 提供，因为它不属于任何单个页面。 */
  notify: (text: string, kind?: "ok" | "error") => void;
}

/**
 * 回收站页：以**相册**形式列出被删掉的图片。
 *
 * ## 用户的原话（2026-10）
 *
 * > 可以把回收站单独列一个子页，该页面以相册形式展示被程序删除的图片，
 * > 在图片下面有两个按钮，一个是恢复，一个是删除。恢复加绿色底色，
 * > 删除加红色底色。**恢复的话就把这个图像放到原来的位置**，删除则直接放到
 * > 系统回收站。同时在该页顶部也保留一个全部删除的按钮。
 *
 * 这一页存在的**根本理由**是用户提出的一个担忧：
 *
 * > 如果我以后想恢复的话，我可能找不到该图像原来的位置。
 *
 * 所以"删除"这一步就把原位置记下来了（`data/trash/<条目>/origin.json`），
 * 恢复时按记录放回去——**不需要**用户记得它原来在哪。
 *
 * ## 缩略图是逐张按需生成的
 *
 * 后端把每张压成 360 px 的 JPEG 并缓存，这里用**三个并发**去取：一次全发出去
 * 会把后端的解码线程排满（图库里的图动辄几十兆像素），串行又要等很久。
 * 取过的 id 记在 ref 里，重新渲染（恢复/删除后刷新列表）不会重复请求。
 *
 * ## 卡片上的两个"点一下"（用户 2026-10 的追加要求）
 *
 * - 点**缩略图** → 用系统默认关联的程序打开这张图（`trash_open_item`）：
 *   相册那一格是"决定留不留"的依据，光看缩略图有时不够，要打开原图看；
 * - 点**文件名** → 复制文件名（含扩展名）。
 *
 * 卡片上**不再显示原位置**（同一轮的要求）：路径又长又占地方，而"会恢复到哪里"
 * 由「恢复」按钮的悬浮提示回答就够了。原位置仍然记在 `origin.json` 里，
 * 恢复照样按它放回去——**不显示不等于没记**。
 *
 * ## 「全部删除」有进度条（用户 2026-10 的要求）
 *
 * 删除几千张图要好几秒。旧实现里这个命令跑在主线程上，于是这几秒界面完全不动，
 * 用户会以为程序卡死（原话："会导致一段时间内程序没有反应"）。现在删除在工作
 * 线程上跑，每处理完一项推一次进度（`wwp://trash-empty-progress`），这一页画一条
 * 进度条——**用户至少要看得见它在动**。
 *
 * ## 三个动作，没有一个会**不可恢复**地**删掉东西**
 *
 * - 恢复 → 放回原位置（原位置有同名文件时另存 `<名字> (恢复).jpg`，绝不覆盖）；
 * - 删除 → 送进**系统回收站**（还能从桌面回收站还原）；
 * - 全部删除 → 同上，逐项送。
 *
 * 想彻底删掉，得用户自己在系统回收站里再清一次——那一步不归我们。
 */
export function TrashPage({ notify }: Props) {
  const [items, setItems] = React.useState<TrashEntryView[] | null>(null);
  const [thumbs, setThumbs] = React.useState<Record<string, string>>({});
  const [errors, setErrors] = React.useState<Record<string, string>>({});
  const [busy, setBusy] = React.useState(false);
  /**
   * 「全部删除」的进度；null = 当前没有在删。
   *
   * 点下去就先摆出 `{done: 0, total: count}`：从点击到第一项真正删完之间
   * 还有一段时间（后端要枚举目录、把任务丢给工作线程），那一段时间里
   * 界面也必须有反应——这正是用户抱怨的那个"没反应"。
   */
  const [emptying, setEmptying] = React.useState<{ done: number; total: number } | null>(null);
  /**
   * 回收站目录的**绝对路径**（用户 2026-10 要求在这一页显示）。
   *
   * 取的是后端的 `trash_info.path`，不是自己拼 `dataRoot + "\data\trash"`：
   * 目录名只有后端一处定义（`AppPaths::trash_dir`），前端拼一遍就成了两套写法。
   */
  const [path, setPath] = React.useState<string>("");
  /** 已经取过的 id（含失败的）。用 ref 是为了不让缩略图到达触发重新取一遍。 */
  const fetched = React.useRef<Set<string>>(new Set());

  const refresh = React.useCallback(async () => {
    try {
      const next = await api.trashEntries();
      // 已经不在回收站里的 id 要把缓存忘掉：同一个 id 不会再回来，
      // 但列表刷新时留着它们只会白占内存。
      const live = new Set(next.map((item) => item.id));
      for (const id of Array.from(fetched.current)) {
        if (!live.has(id)) fetched.current.delete(id);
      }
      setItems(next);
      // 顺带取一次路径（很便宜：只读目录本身）。
      try {
        setPath((await api.trashInfo()).path);
      } catch {
        // 路径取不到不影响相册：下面那一行留空即可，不打扰用户。
      }
    } catch (cause) {
      notify(`读取回收站失败：${String(cause)}`, "error");
    }
  }, [notify]);

  React.useEffect(() => {
    void refresh();
  }, [refresh]);

  // 「全部删除」的进度事件。
  //
  // 订阅成败**都要写日志**：事件属于 Tauri 的核心插件，必须在 capabilities 里
  // 授权，没授权时 `listen` 会被静默拒绝——表现是"进度条永远不动"，而其余
  // 功能一切正常。只有日志能把这件事说出来（§7 的老教训，见 useScanWatcher.ts）。
  React.useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void onTrashEmptyProgress((payload) => {
      if (!disposed) setEmptying(payload);
    })
      .then((un) => {
        void api.logDebug("[回收站] 删除进度事件订阅成功");
        if (disposed) un();
        else unlisten = un;
      })
      .catch((cause: unknown) => {
        void api.logError(`[回收站] 删除进度事件订阅失败：${String(cause)}`);
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // 缩略图：三个 worker 从同一个队列里取 id，避免一次把后端排满。
  React.useEffect(() => {
    if (!items) return;
    let cancelled = false;
    const queue = items.map((item) => item.id).filter((id) => !fetched.current.has(id));

    const worker = async () => {
      for (;;) {
        const id = queue.shift();
        if (!id || cancelled) return;
        fetched.current.add(id);
        try {
          const url = await api.trashThumbnail(id);
          if (!cancelled) setThumbs((prev) => ({ ...prev, [id]: url }));
        } catch (cause) {
          if (!cancelled) setErrors((prev) => ({ ...prev, [id]: String(cause) }));
        }
      }
    };
    void Promise.all([worker(), worker(), worker()]);
    return () => {
      cancelled = true;
    };
  }, [items]);

  const restore = async (item: TrashEntryView) => {
    setBusy(true);
    try {
      const report = await api.trashRestore(item.id);
      notify(report.message, "ok");
      // 恢复只是把文件放回去；它**不在索引里**（删除时移掉了），
      // 因此顺手做一次那个图库的增量重扫，否则用户会发现"恢复了但没参与轮播"。
      if (report.library) {
        try {
          await api.libraryScan(report.library, false);
          notify(`正在把这个图库的索引补上（增量扫描，只读这一张）`);
        } catch (cause) {
          // 扫描失败不该让"恢复成功"这件事变成失败：文件已经在原位置了。
          notify(
            `图已恢复到原位置，但没能自动扫描图库（到「图库」页点一次「扫描」即可）：${String(
              cause,
            )}`,
            "error",
          );
        }
      } else {
        notify("这张图不属于任何已配置的图库，因此不会参与轮播", "error");
      }
      await refresh();
    } catch (cause) {
      notify(`恢复失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const discard = async (item: TrashEntryView) => {
    setBusy(true);
    try {
      notify(await api.trashDelete(item.id), "ok");
      await refresh();
    } catch (cause) {
      notify(`删除失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const discardAll = async () => {
    setBusy(true);
    // 先把进度条摆出来（总数就是列表里看到的条数），别让用户对着一个
    // 点下去毫无变化的按钮猜。
    setEmptying({ done: 0, total: count });
    try {
      const report = await api.trashEmpty();
      notify(report.message, report.failed > 0 ? "error" : "ok");
      await refresh();
    } catch (cause) {
      notify(`全部删除失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
      setEmptying(null);
    }
  };

  /** 用系统默认关联的程序打开这一条里的图。 */
  const openItem = async (item: TrashEntryView) => {
    try {
      await api.trashOpenItem(item.id);
    } catch (cause) {
      notify(`打开图片失败：${String(cause)}`, "error");
    }
  };

  /**
   * 打开这一条**原来所在的文件夹**，并选中那个文件（点卡片上的文件名）。
   *
   * 用它的是 `item.original`（删除前的完整路径），不是回收站里那份拷贝的位置：
   * 用户点文件名想知道的是"这张图原来在哪儿"。
   *
   * 旧版（1.3.0）删掉的条目没有记录原位置（`original` 为 null），此时没有可打开的
   * 地方，因此如实说明而不是静默什么都不做。
   */
  const revealName = async (item: TrashEntryView) => {
    if (!item.original) {
      notify(`这一条没有记录原位置（旧版本删除的）：${item.name}`, "error");
      return;
    }
    try {
      await api.revealFile(item.original);
      // 与播放页那句保持一致：资源管理器是另一个窗口，得给个回话。
      const folder = item.original.replace(/[\\/][^\\/]*$/, "") || item.original;
      notify(`已在资源管理器中打开并选中：${folder}`);
    } catch (cause) {
      notify(`打开原位置失败：${String(cause)}`, "error");
    }
  };

  const openFolder = async () => {
    try {
      await api.trashOpen();
    } catch (cause) {
      notify(`打开回收站文件夹失败：${String(cause)}`, "error");
    }
  };

  const count = items?.length ?? 0;
  const bytes = (items ?? []).reduce((sum, item) => sum + item.bytes, 0);
  const has = count > 0;

  return React.createElement(
    "div",
    null,
    React.createElement(
      "div",
      { className: "wwp-page-head" },
      React.createElement("h1", { className: "wwp-title" }, "回收站"),
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "被删掉的图存放在此，不会彻底删除；「恢复」按删除时记录的原位置放回。",
      ),
    ),

    React.createElement(
      "div",
      { className: "wwp-card" },
      React.createElement(
        "div",
        { className: "wwp-control-row" },
        React.createElement(
          "span",
          { className: "wwp-dim" },
          items === null
            ? "正在读取…"
            : has
              ? `共 ${count} 项（${formatBytes(bytes)}）`
              : "回收站是空的",
        ),
        React.createElement("span", { className: "wwp-spacer" }),
        React.createElement(
          "button",
          {
            className: "wwp-btn wwp-btn-quiet",
            disabled: busy,
            title: TIPS.trashOpenFolder,
            onClick: () => void openFolder(),
          },
          "打开文件夹",
        ),
        React.createElement(
          "button",
          {
            className: "wwp-btn wwp-btn-quiet",
            disabled: busy,
            title: TIPS.trashRefresh,
            onClick: () => void refresh(),
          },
          "刷新",
        ),
        React.createElement(
          "button",
          {
            className: "wwp-btn wwp-btn-discard",
            disabled: busy || !has,
            title: TIPS.trashDiscardAll,
            onClick: () => void discardAll(),
          },
          "全部删除",
        ),
      ),
      // 「全部删除」的进度条。位置紧挨着按钮那一行：用户点的是上面那个按钮，
      // 眼睛就在那里，把进度放到别处等于没给。
      emptying
        ? React.createElement(
            "div",
            null,
            React.createElement(
              "div",
              {
                className: "wwp-progress",
                role: "progressbar",
                "aria-valuenow": emptying.total > 0 ? Math.round((emptying.done / emptying.total) * 100) : 0,
                "aria-valuemin": 0,
                "aria-valuemax": 100,
              },
              React.createElement("div", {
                // 总数还不知道时用"不确定"样式（条纹流动），而不是停在 0%：
                // 停在 0% 看起来像卡住了，而这正是要消除的错觉。
                className:
                  emptying.total > 0
                    ? "wwp-progress-fill"
                    : "wwp-progress-fill wwp-progress-unknown",
                style:
                  emptying.total > 0
                    ? { width: `${Math.round((emptying.done / emptying.total) * 100)}%` }
                    : undefined,
              }),
            ),
            React.createElement(
              "div",
              { className: "wwp-progress-caption" },
              emptying.total > 0
                ? `正在送入系统回收站：${emptying.done} / ${emptying.total} 项`
                : "正在送入系统回收站…",
            ),
          )
        : null,
      // 回收站目录的绝对路径（用户 2026-10 要求显示在这里）。
      // 一行常驻文字，与诊断页那一行同源（都来自后端的 `trash_info.path`）。
      React.createElement(
        "div",
        { className: "wwp-row", style: { marginTop: 8 } },
        React.createElement("span", { className: "wwp-label" }, "位置"),
        React.createElement(
          "span",
          { className: "wwp-value wwp-dim" },
          path || "正在读取…",
        ),
      ),
      React.createElement(
        "div",
        { className: "wwp-dim", style: { marginTop: 8 } },
        "点击缩略图打开原图，点击文件名打开它原来所在的文件夹并选中它；「恢复」放回原位置（已有同名文件时另存，不覆盖），「删除」送入系统回收站、仍可还原。",
      ),
    ),

    items === null
      ? null
      : items.length === 0
        ? React.createElement(
            "div",
            { className: "wwp-note" },
            "回收站是空的。播放页每屏卡片右上角的「删除当前壁纸」会把图放到这里，而不是真的删掉。",
          )
        : React.createElement(
            "div",
            { className: "wwp-trash-grid" },
            items.map((item) =>
              React.createElement(TrashCard, {
                key: item.id,
                item,
                thumb: thumbs[item.id],
                thumbError: errors[item.id],
                busy,
                onOpen: () => void openItem(item),
                onRevealName: () => void revealName(item),
                onRestore: () => void restore(item),
                onDiscard: () => void discard(item),
              }),
            ),
          ),
  );
}

/** 相册里的一张：缩略图 + 名字/时间 + 两个按钮。 */
function TrashCard({
  item,
  thumb,
  thumbError,
  busy,
  onOpen,
  onRevealName,
  onRestore,
  onDiscard,
}: {
  item: TrashEntryView;
  /** 缩略图的 data URL；还没生成好时为 undefined。 */
  thumb: string | undefined;
  /** 这张图没法生成缩略图时的原因。 */
  thumbError: string | undefined;
  busy: boolean;
  onOpen: () => void;
  /** 打开它原来所在的文件夹并选中它（点文件名）。 */
  onRevealName: () => void;
  onRestore: () => void;
  onDiscard: () => void;
}) {
  return React.createElement(
    "div",
    { className: "wwp-trash-item" },
    React.createElement(
      "div",
      {
        // 缩略图整格可点：打开原图。缩略图还没生成好时这一格同样可点——
        // "打不开"和"还没画出来"是两件事，不该互相牵连。
        className: "wwp-trash-thumb wwp-trash-openable",
        title: TIPS.trashOpenImage,
        onClick: onOpen,
      },
      thumb
        ? React.createElement("img", {
            src: thumb,
            // 纯装饰：文件名就在下面，读屏软件不必念两遍。
            alt: "",
            "aria-hidden": true,
            // 让浏览器只在滚到可见时才解码：一页几十张时这一条很实在。
            loading: "lazy",
          })
        : React.createElement(
            "span",
            { className: "wwp-trash-thumb-note" },
            thumbError ?? "正在生成缩略图…",
          ),
    ),
    React.createElement(
      "div",
      { className: "wwp-trash-meta" },
      // 只显示**文件名**，而且点它打开它原来所在的文件夹并选中它（1.11.0 起；
      // 以前是复制文件名）。原位置那一行仍然撤掉着：它是整张卡片里最长的一行，
      // 而"它在哪儿"现在由这一下点击直接回答——比在卡片上写一行被截断的路径更直接。
      React.createElement(
        "span",
        {
          className: "wwp-trash-name wwp-trash-reveal",
          title: TIPS.trashRevealName,
          onClick: onRevealName,
        },
        item.name,
      ),
      React.createElement(
        "span",
        { className: "wwp-trash-sub" },
        `${shortTime(item.deletedAt)} 删除 · ${formatBytes(item.bytes)}`,
      ),
    ),
    React.createElement(
      "div",
      { className: "wwp-trash-actions" },
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-restore",
          disabled: busy || !item.canRestore,
          title: !item.canRestore
            ? TIPS.trashRestoreImpossible
            : item.originalExists
              ? TIPS.trashRestoreOccupied
              : TIPS.trashRestore,
          onClick: onRestore,
        },
        "恢复",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-discard",
          disabled: busy,
          title: TIPS.trashDiscard,
          onClick: onDiscard,
        },
        "删除",
      ),
    ),
  );
}

/**
 * `YYYY-MM-DD HH:MM`——比 `formatTime` 少一段秒。
 *
 * 卡片只有 190px 宽，带上秒之后那一行会折成两行（实测："8.0 MB" 被拆成
 * "8.0" 与 "MB"）。`formatTime` 的输出格式是固定的（`api.ts` 里就这么拼的），
 * 因此这里按位置截到分钟是安全的；完整时间仍在「删除时间」这个语义里够用。
 */
function shortTime(unixSeconds: number): string {
  const text = formatTime(unixSeconds);
  return text.length >= 16 ? text.slice(0, 16) : text;
}

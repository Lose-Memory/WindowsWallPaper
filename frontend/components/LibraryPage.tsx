import React from "react";
import {
  api,
  type LibraryView,
  type ScanAllDone,
  type ScanDone,
  type ScanProgress,
  type SettingsInfo,
  type SettingsPatch,
} from "../api";
import { LibraryManager } from "./LibraryManager";
import { useScanWatcher } from "../useScanWatcher";
import { TIPS } from "../tooltip-texts";

interface Props {
  settings: SettingsInfo;
  /** 提交设置改动（图库清单、排除规则）。 */
  onSettingsChanged: (next: SettingsInfo) => void;
  notify: (text: string, kind?: "ok" | "error") => void;
}

/**
 * 图库页：图库目录的增删、扫描、进度、结果。
 *
 * 这一页的重点是"回答少了什么"。可播放数只是一个数字，用户真正会问的是
 * "我那个目录里明明有 HEIC，怎么没出现"——所以跳过格式的明细、读不了的文件
 * 样本、以及被排除规则挡下的数量都直接铺在界面上，而不是藏在日志里。
 *
 * **播放列表已经不在这里了**（用户要求拆成两页）：这一页只回答"有哪些放图的
 * 目录"，"每块屏放哪几套图"在「播放列表」页。
 *
 * 顶部有「全部重新扫描」：一次把所有图库都增量重扫一遍（见下面的说明）。
 */
export function LibraryPage({ settings, onSettingsChanged, notify }: Props) {
  const [error, setError] = React.useState<string | null>(null);

  // 扫描状态用**与右上角浮窗完全相同的那一个来源**。
  //
  // ## 为什么改成这样（这里踩过一个很难查的坑）
  //
  // 原先这个页面自己订阅 `wwp://scan-progress` 事件，而浮窗轮询
  // `library_scan_status` 命令。两套机制的要求不同：Tauri 2 里事件属于核心
  // 插件，**必须在 capability 里授权**，而命令不需要。项目当时根本没有
  // capabilities 文件，于是事件订阅被静默拒绝（而且原来的写法是
  // `void listen(...).then(...)`，**没有 catch**）。
  //
  // 现象是：**浮窗正常显示进度，这个页面的卡片永远停在「正在准备…」**，
  // 而后端日志显示扫描完全正常、索引也建好了（重启程序后标签会变）。
  //
  // 教训：同一件事有两个实现，就只有一半能工作。现在合并成一个 hook，
  // 两处都用它，因此不可能再出现互相不一致的状态。
  const scan = useScanWatcher();

  // 最近一次**单库**扫描的结果、与最近一次**全部重新扫描**的结果。
  //
  // 它们不再"4 秒后自己消失"：用户扫完一个几万张的图库，正要读"新增 3、
  // 删除 2"，卡片不该在这时候不见了。现在留着，直到下一次扫描开始
  // （由 hook 清掉）或用户点「关闭」。
  const done = scan.justFinished;
  const batchDone = scan.batchFinished;

  // 扫描结束后刷新一遍图库列表，让「未建索引」标签立刻变成「已建索引」。
  //
  // 原先那个标签来自 `settings.libraries[].scanned`，而它只在启动时读一次。
  // 于是刚扫完的图库仍显示"未扫描"，必须重启程序才更新——用户会以为扫描没成功
  // （即使索引其实已经写好了）。
  const finishedSeq = done ? `${done.library}|${done.scannedAt}` : null;
  // 整批的标识：图库数 + 耗时 + 变化数。它只需要"换了一批结果就重来一次"，
  // 不像单库那份有权威的时间戳可用。
  const batchSeq = batchDone
    ? `batch|${batchDone.libraries.length}|${batchDone.totalMillis}|${batchDone.added}|${batchDone.updated}|${batchDone.removed}`
    : null;
  React.useEffect(() => {
    if (!finishedSeq && !batchSeq) return;
    void (async () => {
      try {
        // 用 settingsReload 而不是自己拼一份：后端会重新读配置与索引状态，
        // 因此 `scanned` 字段是权威的。
        onSettingsChanged(await api.settingsReload());
      } catch (cause) {
        void api.logDebug(`扫描后刷新图库列表失败：${String(cause)}`);
      }
    })();
    // 只在"某次扫描刚结束"时触发；两个 seq 是它们的稳定标识。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finishedSeq, batchSeq]);

  const progress = scan.status?.progress ?? null;
  const scanning: string | null = scan.running ? (scan.status?.library ?? "") : null;

  const startScan = async (library: LibraryView, rescan: boolean) => {
    setError(null);
    try {
      await api.libraryScan(library.path, rescan);
    } catch (cause) {
      setError(String(cause));
      // 浮窗也要给：页面上的错误盒可能被滚出视野，
      // 而"我点了扫描但什么都没发生"是最让人困惑的情况。
      notify(`扫描没能启动：${String(cause)}`, "error");
    }
  };

  /**
   * 「全部重新扫描」：一次把所有图库都**增量**重扫一遍。
   *
   * 增量是后端定死的（这个按钮的语义就是"只改有变化的部分"），因此这里
   * 没有"要不要全量"的选项——想强制全量重扫某一个是每一行的「全量重扫」。
   */
  const startScanAll = async () => {
    setError(null);
    try {
      await api.libraryScanAll();
    } catch (cause) {
      setError(String(cause));
      notify(`重扫没能启动：${String(cause)}`, "error");
    }
  };

  /**
   * 提交设置改动。图库管理里的增删目录与勾选都走这里。
   *
   * 单独包一层是为了让错误处理只有一处：`settingsUpdate` 会把整份设置返回，
   * 直接交给上层即可（不必再读一次，也就不会出现"保存成功但界面还是旧值"）。
   */
  const applyPatch = React.useCallback(
    async (patch: SettingsPatch) => {
      const next = await api.settingsUpdate(patch);
      onSettingsChanged(next);
    },
    [onSettingsChanged],
  );

  const cancelScan = async () => {
    await api.libraryCancelScan();
  };

  const totalLibraries = settings.libraries.length;

  return React.createElement(
    "div",
    null,
    React.createElement("h1", { className: "wwp-title" }, "图库"),
    React.createElement(
      "p",
      { className: "wwp-sub" },
      totalLibraries === 0
        ? "还没有配置图库。用下面的「新增图库」按钮选一个放图片的文件夹。"
        : "扫描只读图像头部取尺寸，未变化的文件会被复用。",
    ),

    // 「全部重新扫描」：一键把所有图库都增量重扫一遍。
    //
    // 为什么放在这一页最上面：这是"图库变了之后最常要做的一件事"——
    // 往目录里丢了几张、删了几张，都要它来把索引对齐。而逐个图库点一遍
    // 在十几个图库时是不可接受的。
    React.createElement(
      "div",
      { className: "wwp-control-row", style: { marginBottom: 12 } },
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-primary",
          disabled: scanning !== null || totalLibraries === 0,
          onClick: () => void startScanAll(),
          title: TIPS.scanAll,
        },
        "全部重新扫描",
      ),
      React.createElement(
        "span",
        { className: "wwp-dim" },
        totalLibraries === 0
          ? "还没有图库可扫"
          : "逐个图库检查：只打开新增或有改动的文件，删掉的图从索引里移除",
      ),
    ),

    error
      ? React.createElement(
          "div",
          { className: "wwp-error" },
          error,
          React.createElement(
            "button",
            { className: "wwp-btn wwp-btn-quiet", onClick: () => setError(null) },
            "关闭",
          ),
        )
      : null,

    scanning
      ? React.createElement(ScanProgressCard, {
          library: scanning,
          progress,
          batchIndex: scan.status?.batchIndex ?? null,
          batchTotal: scan.status?.batchTotal ?? null,
          onCancel: cancelScan,
        })
      : null,

    // 图库管理放在最前面：它是配置这条链的起点——先有图库（目录），
    // 才能扫描，也才能在播放列表里勾选。用户明确要求"添加图库"只能在这里做。
    React.createElement(LibraryManager, {
      settings,
      onPatch: applyPatch,
      notify,
      onScan: async (path: string, rescan: boolean) => {
        const found = settings.libraries.find((l) => l.path === path);
        if (found) await startScan(found, rescan);
      },
      scanning: scanning !== null,
    }),

    done && !done.cancelled && !done.error && done.stats
      ? React.createElement(ScanResultCard, { done, onClose: scan.dismiss })
      : null,

    // 单个图库扫失败：必须说出来。原先这条路径在页面上什么都不显示
    // （结果卡要求 `stats` 非空，而失败时它是 null），于是"点了扫描没反应"
    // 与"扫了但失败了"在用户眼里是同一件事。
    done?.error
      ? React.createElement(
          "div",
          { className: "wwp-error" },
          `扫描失败：${done.error}`,
          React.createElement(
            "button",
            { className: "wwp-btn wwp-btn-quiet", onClick: scan.dismiss },
            "关闭",
          ),
        )
      : null,

    done && done.cancelled
      ? React.createElement(
          "div",
          { className: "wwp-warn" },
          "扫描已取消。磁盘上的索引保持为上一次的完整内容，没有被写成半截。",
        )
      : null,

    batchDone
      ? React.createElement(ScanAllResultCard, { done: batchDone, onClose: scan.dismiss })
      : null,
  );
}

function ScanProgressCard({
  library,
  progress,
  batchIndex,
  batchTotal,
  onCancel,
}: {
  library: string;
  progress: ScanProgress | null;
  /** 「全部重新扫描」时是第几个图库（1 基）；单库扫描时为 null。 */
  batchIndex: number | null;
  /** 「全部重新扫描」时一共几个图库；单库扫描时为 null。 */
  batchTotal: number | null;
  onCancel: () => void;
}) {
  // 进度条的分母是**候选数**，不是"看到的文件数"。
  //
  // 扫描分两阶段：先单线程遍历目录（只按扩展名筛选，很快），
  // 再多线程逐个探测文件尺寸（慢，是真正的耗时所在）。
  // 候选数要到遍历结束才知道，因此遍历阶段只能显示"不确定进度"。
  // 若拿"看到的文件数"当分母，会得到一条先冲到底、再退回来重走的假进度条。
  const total = progress?.candidates ?? 0;
  const done = progress?.processed ?? 0;
  const known = total > 0;
  // 夹住范围：进度事件与界面渲染是异步的，偶尔会出现
  // processed 暂时超过上一帧的 candidates（candidates 随后才更新）。
  const percent = known ? Math.max(0, Math.min(100, Math.round((done / total) * 100))) : 0;

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement(
      "h2",
      { className: "wwp-h2" },
      // 「第 2/5 个图库」：一次扫全部时，用户需要知道还剩几个
      // （这类操作可能跑好几分钟）。单库扫描时不显示，否则是"第 1/1 个"。
      batchTotal ? `正在扫描（第 ${batchIndex ?? 1}/${batchTotal} 个图库）` : "正在扫描",
    ),
    React.createElement("div", { className: "wwp-row" }, React.createElement("span", { className: "wwp-label" }, "图库"), React.createElement("span", { className: "wwp-value" }, library)),

    // 进度条本身。
    React.createElement(
      "div",
      {
        className: "wwp-progress",
        role: "progressbar",
        "aria-valuenow": percent,
        "aria-valuemin": 0,
        "aria-valuemax": 100,
      },
      React.createElement("div", {
        // 分母未知时用"不确定"样式（条纹流动），而不是停在 0%。
        // 停在 0% 会让人以为卡死了。
        className: known ? "wwp-progress-fill" : "wwp-progress-fill wwp-progress-unknown",
        style: known ? { width: `${percent}%` } : undefined,
      }),
    ),
    React.createElement(
      "div",
      { className: "wwp-progress-caption" },
      known
        ? `${done} / ${total} 张（${percent}%）`
        : progress
          ? `正在遍历目录…已看到 ${progress.filesSeen} 个文件`
          : "正在准备…",
    ),

    progress
      ? React.createElement(
          React.Fragment,
          null,
          React.createElement(
            "div",
            { className: "wwp-row" },
            React.createElement("span", { className: "wwp-label" }, "正在处理"),
            React.createElement("span", { className: "wwp-value" }, progress.current),
          ),
          React.createElement(
            "div",
            { className: "wwp-row" },
            React.createElement("span", { className: "wwp-label" }, "已看到文件"),
            React.createElement(
              "span",
              { className: "wwp-value" },
              `${progress.filesSeen}（其中候选 ${progress.candidates}）`,
            ),
          ),
          React.createElement(
            "div",
            { className: "wwp-row" },
            React.createElement("span", { className: "wwp-label" }, "复用（未重新打开）"),
            React.createElement(
              "span",
              { className: "wwp-value" },
              // 沿用上级图库的那一部分单独括出来：用户看到"一个文件都没打开"
              // 时，最自然的疑问就是"那它的尺寸是哪来的"。
              progress.reusedInherited > 0
                ? `${progress.reused}（其中沿用上级图库 ${progress.reusedInherited}）`
                : `${progress.reused}`,
            ),
          ),
        )
      : null,
    React.createElement(
      "div",
      { style: { marginTop: 12 } },
      React.createElement(
        "button",
        { className: "wwp-btn wwp-btn-quiet", onClick: onCancel },
        batchTotal ? "取消（停在这一批之后）" : "取消扫描",
      ),
    ),
  );
}

/** 单个图库扫完的结果卡。 */
function ScanResultCard({ done, onClose }: { done: ScanDone; onClose: () => void }) {
  const stats = done.stats;
  if (!stats) return null;

  const formats = done.unsupportedFormats;
  const samples = done.unreadableSamples;
  const changed = done.added + done.updated + done.removed;

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement(
      "div",
      { className: "wwp-control-row" },
      React.createElement("h2", { className: "wwp-h2" }, "扫描结果"),
      React.createElement("span", { className: "wwp-spacer" }),
      React.createElement(
        "button",
        { className: "wwp-btn wwp-btn-quiet", onClick: onClose },
        "关闭",
      ),
    ),

    // 一句话结论放最上面：用户要的就是"变了什么"，而不是一堆统计。
    React.createElement(
      "div",
      { className: changed > 0 ? "wwp-note" : "wwp-dim", style: changed > 0 ? { marginTop: 0, borderTop: "none", paddingTop: 0 } : { marginBottom: 8 } },
      changed > 0
        ? `新增 ${done.added} 张、更新 ${done.updated} 张、删除 ${done.removed} 张；其余 ${done.reusedEntries} 张未变化。`
        : `没有变化：${done.reusedEntries} 张图都与索引一致。`,
    ),

    rows([
      ["索引条目", `${done.entries}`],
      ["可播放", `${stats.playable}`],
      ["耗时", `${(done.scanMillis / 1000).toFixed(2)} 秒`],
      ["新增 / 更新 / 删除", `${done.added} / ${done.updated} / ${done.removed}`],
      ["复用 / 重探", `${done.reusedEntries} / ${done.probedEntries}`],
      // 只有真的沿用了别的图库的数据时才多这一行：为 0 时它是一句废话，
      // 而"这一行什么时候会出现"本身就是对用户的解释——为什么这次这么快。
      ...(done.reusedInherited > 0
        ? ([
            [
              "其中沿用上级图库",
              `${done.reusedInherited} 张（直接用了上级图库已探测到的尺寸，未重新打开文件）`,
            ],
          ] as Array<[string, string]>)
        : []),
      ["看到的文件", `${stats.files_seen}`],
      ["扩展名不是图片", `${stats.files_ignored}`],
      ["被排除规则挡下", `${stats.excluded}`],
      ["没有解码器", `${stats.unsupported}`],
      ["读不了 / 损坏", `${stats.unreadable}`],
      ["磁盘上已消失", `${done.removed}`],
    ]),

    formats.length > 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          React.createElement("strong", null, "没有解码器的格式："),
          formats.map(([format, count]) => `${format} ${count} 个`).join("、"),
          React.createElement(
            "div",
            { className: "wwp-dim" },
            "这些文件留在索引里但不会参与播放，以便你能查到它们去哪了。",
          ),
        )
      : null,

    samples.length > 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          React.createElement(
            "strong",
            null,
            `无法读取的文件（共 ${stats.unreadable} 个，列出 ${samples.length} 个）：`,
          ),
          React.createElement(
            "div",
            { className: "wwp-samples" },
            samples.slice(0, 50).map((sample) =>
              React.createElement(
                "div",
                { key: sample.path, className: "wwp-sample" },
                React.createElement("span", { className: "wwp-value" }, sample.path),
                React.createElement("span", { className: "wwp-dim" }, sample.reason),
              ),
            ),
          ),
        )
      : null,
  );
}

/**
 * 「全部重新扫描」的结果卡：每个图库一行，外加合计。
 *
 * 逐库列出而不是只给一个总数，是因为"哪个图库变了"直接决定下一步做什么
 * （某个盘上的库整个消失了，与另一个库多了两张图，是两件不同的事）。
 */
function ScanAllResultCard({ done, onClose }: { done: ScanAllDone; onClose: () => void }) {
  const changed = done.added + done.updated + done.removed;

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement(
      "div",
      { className: "wwp-control-row" },
      React.createElement("h2", { className: "wwp-h2" }, "重新扫描结果"),
      React.createElement("span", { className: "wwp-spacer" }),
      React.createElement(
        "button",
        { className: "wwp-btn wwp-btn-quiet", onClick: onClose },
        "关闭",
      ),
    ),

    React.createElement(
      "div",
      { className: done.cancelled ? "wwp-warn" : "wwp-note", style: done.cancelled ? { marginTop: 0 } : { marginTop: 0, borderTop: "none", paddingTop: 0 } },
      done.cancelled
        ? `已取消：只扫完了前 ${done.libraries.length} 个图库，其余的索引保持原样。`
        : changed > 0
          ? `新增 ${done.added} 张、更新 ${done.updated} 张、删除 ${done.removed} 张；其余 ${done.reused} 张未变化。`
          : `没有变化：${done.libraries.length} 个图库里的图都与索引一致（${done.reused} 张未变）。`,
    ),

    React.createElement(
      "div",
      { className: "wwp-lib-list" },
      done.libraries.map((library) =>
        React.createElement(
          "div",
          { key: library.library, className: "wwp-scan-lib" },
          React.createElement(
            "span",
            { className: "wwp-path-ellipsis", title: library.library },
            library.library,
          ),
          React.createElement(
            "span",
            { className: "wwp-scan-lib-changes" },
            library.error
              ? "没能扫"
              : `新增 ${library.added}・更新 ${library.updated}・删除 ${library.removed}・未变 ${library.reusedEntries}`,
          ),
        ),
      ),
    ),

    done.failed > 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          React.createElement("strong", null, `${done.failed} 个图库没能扫：`),
          done.libraries
            .filter((library) => library.error)
            .map((library) => `${library.library}（${library.error}）`)
            .join("；"),
        )
      : null,

    React.createElement(
      "div",
      { className: "wwp-note" },
      `共 ${done.libraries.length} 个图库，耗时 ${(done.totalMillis / 1000).toFixed(2)} 秒` +
        `（其中重新打开并读取的文件 ${done.probed} 个` +
        // "沿用上级图库"只在真的发生时才说，否则是一句会让人困惑的废话
        // （他会去找"上级图库"是哪一个）。
        (done.reusedInherited > 0
          ? `，另有 ${done.reusedInherited} 张直接沿用了上级图库已探测到的数据`
          : "") +
        `）。`,
    ),
  );
}

function rows(items: Array<[string, string]>) {
  return items.map(([label, value]) =>
    React.createElement(
      "div",
      { className: "wwp-row", key: label },
      React.createElement("span", { className: "wwp-label" }, label),
      React.createElement("span", { className: "wwp-value" }, value),
    ),
  );
}

import React from "react";
import { useScanWatcher } from "../useScanWatcher";

/**
 * 右上角常驻的扫描状态提示。
 *
 * ## 数据来源
 *
 * 用 `useScanWatcher`——与图库页的进度条**完全同一套机制**。
 *
 * 这里原先自己轮询 `library_scan_status`，而图库页订阅事件。两套机制的要求
 * 不同（Tauri 2 里事件需要 capability 授权，命令不需要），于是出现过
 * "浮窗正常、图库页卡在正在准备"这种一半好一半坏的状态。
 * 合并成一个来源之后，这种事不会再发生。
 *
 * ## 为什么要显示"已经跑了多久"
 *
 * 扫描卡住时（磁盘僵住、网络盘断开），进度数字会停住。用户唯一能区分
 * "还在动"与"已经死了"的依据就是：**要么数字在变，要么秒数在走**。
 * 因此两个都显示。
 */
export function ScanToast() {
  const { status, justFinished, running, cancel } = useScanWatcher();
  const [cancelling, setCancelling] = React.useState(false);
  // 这条浮窗只管**一次性提示**：结束态显示 4 秒就收起来。
  //
  // ## 为什么计时在这儿，而不是在 hook 里
  //
  // hook 里的结果（`justFinished` / `batchFinished`）现在会一直留着，
  // 因为图库页的结果卡**要给人读**（新增了什么、删了什么）。而这条浮窗
  // 是"扫完了"的通知，挂久了只是遮挡。同一份数据、两种展示寿命，
  // 因此由各自的展示方计时。
  const [flash, setFlash] = React.useState<typeof justFinished>(null);
  React.useEffect(() => {
    if (!justFinished) {
      setFlash(null);
      return;
    }
    setFlash(justFinished);
    const timer = window.setTimeout(() => setFlash(null), 4000);
    return () => window.clearTimeout(timer);
  }, [justFinished]);

  // ## 显示规则
  //
  //   - **扫描中**：由轮询到的状态驱动（能拿到实时进度、已用时、取消按钮）。
  //   - **已结束**：只由 `flash` 驱动，4 秒后清除。
  //
  // 结束态**不能**用轮询的状态来判断：那个 `finished` 一旦置真就一直是真
  // （它表示"最近一次扫描已结束"，不是"刚刚结束"），用它会让浮窗永久挂着。
  if (!status) return null;
  if (!running && !flash) return null;

  const doCancel = async () => {
    setCancelling(true);
    try {
      await cancel();
    } finally {
      setCancelling(false);
    }
  };

  const progress = status.progress;
  // 遍历阶段还没有候选总数，因此这里也区分"已知分母"与"未知"。
  const total = progress?.candidates ?? 0;
  const done = progress?.processed ?? 0;
  const known = total > 0;
  const percent = known ? Math.max(0, Math.min(100, Math.round((done / total) * 100))) : 0;

  // 图库名：扫描中用轮询那份，刚结束时用事件那份
  // （轮询的状态可能已被下一次扫描覆盖）。
  const rawLibrary = running ? status.library : (flash?.library ?? status.library);
  const libraryName = rawLibrary ? rawLibrary.split(/[\\/]/).filter(Boolean).pop() : null;

  // 结束文案只用事件那份。见上面的显示规则。
  const finishedText = flash
    ? flash.cancelled
      ? "扫描已取消"
      : flash.error
        ? `扫描失败：${flash.error}`
        : `扫描完成（新增 ${flash.added}、更新 ${flash.updated}、删除 ${flash.removed}）`
    : null;

  // 正在扫描/已结束时，这一行里都有"还能撑住"的信息；进度行与计数行各自只有一行。
  //
  // ## 与「取消扫描」同行
  //
  // 2026-10 起「取消扫描」排在标题这一行。原因是这一格的高度有硬约束：
  // 右上角浮层"不许盖住「重新加载」"（用户要求，见 `.wwp-pop-layer` 与
  // `check-layout.ps1` 判据 8），而按钮独占一行要花掉约 32px——那是整格高度里
  // 最值得省的一行。
  //
  // ## 「第 2/5 个」为什么挪到计数那一行
  //
  // 标题行腾出位置之后仍然紧张：标题 + 库名 + 已用时 + 取消按钮已经占满
  // 330px 的宽度，库名会被挤到几乎看不见。而库名恰恰是"正在扫哪个图库"的
  // 唯一线索，不能牺牲；「第 2/5 个」跟计数放在一起反而更顺（都是"进度"）。
  return React.createElement(
    "div",
    { className: "wwp-scan-toast" },
    React.createElement(
      "div",
      { className: "wwp-scan-toast-head" },
      React.createElement("span", { className: "wwp-scan-toast-title" }, running ? "正在扫描" : "扫描结束"),
      // 库名只取最后一段。它是这一行里唯一可以牺牲的东西：`min-width: 0` +
      // 省略号保证后面两件（已用时、取消按钮）永远留在卡片里，也保证这一行
      // 永远是**一行**（高度稳定）。
      libraryName
        ? React.createElement("span", { className: "wwp-tag wwp-scan-toast-library" }, libraryName)
        : null,
      // 「已用时 12 秒」。
      //
      // 它一直可见：秒数在走而进度数字不动 = "还活着但卡住了"，这是用户唯一
      // 能看出的区别（见文件头的说明）。`margin-left: auto` 把它和后面的按钮
      // 一起推到右边。
      running
        ? React.createElement(
            "span",
            { className: "wwp-scan-toast-elapsed" },
            `已用时 ${formatDuration(status.elapsedSeconds)}`,
          )
        : null,
      running
        ? React.createElement(
            "button",
            {
              className: "wwp-btn wwp-btn-quiet wwp-scan-toast-cancel",
              disabled: cancelling,
              onClick: () => void doCancel(),
            },
            cancelling ? "正在取消…" : "取消扫描",
          )
        : null,
    ),

    running
      ? React.createElement(
          React.Fragment,
          null,
          React.createElement(
            "div",
            { className: "wwp-progress" },
            React.createElement("div", {
              className: known ? "wwp-progress-fill" : "wwp-progress-fill wwp-progress-unknown",
              style: known ? { width: `${percent}%` } : undefined,
            }),
          ),
          React.createElement(
            "div",
            { className: "wwp-scan-toast-line" },
            (known
              ? `${done} / ${total} 张（${percent}%）`
              : progress
                ? `正在遍历目录…已看到 ${progress.filesSeen} 个文件`
                : "正在准备…") +
              // 「第 2/5 个」：一次扫全部图库时，光看"正在扫描"与目录名无法知道
              // 还剩几个，而这类操作可能跑好几分钟。
              (status.batchTotal ? ` · 第 ${status.batchIndex ?? 1}/${status.batchTotal} 个` : ""),
          ),
        )
      : React.createElement("div", { className: "wwp-scan-toast-line" }, finishedText),
  );
}

/** 把秒数写成"1 分 23 秒"。超过一小时就显示小时。 */
function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes < 60) return `${minutes} 分 ${rest} 秒`;
  const hours = Math.floor(minutes / 60);
  return `${hours} 小时 ${minutes % 60} 分`;
}

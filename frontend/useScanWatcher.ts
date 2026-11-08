import React from "react";
import {
  api,
  onScanAllDone,
  onScanDone,
  type ScanAllDone,
  type ScanDone,
  type ScanStatusView,
} from "./api";

/**
 * 扫描状态的**唯一**来源。
 *
 * ## 为什么必须只有一个来源（这里踩过一个很难查的坑）
 *
 * 原先有两套并行机制：
 *   - 图库页的进度条订阅 `wwp://scan-progress` **事件**；
 *   - 右上角的浮窗轮询 `library_scan_status` **命令**。
 *
 * 两套机制的要求不同：Tauri 2 里事件属于核心插件，**必须在 capability 里授权**，
 * 而命令不需要。当时项目里根本没有 capabilities 文件，于是事件订阅被静默拒绝
 * （而且原来的写法是 `void listen(...).then(...)`，**没有 catch**）。
 *
 * 结果是：浮窗正常显示进度，图库页的卡片永远停在「正在准备…」，
 * 而后端日志显示扫描完全正常。**同一件事有两个实现，就只有一半能工作。**
 *
 * 现在合并成一个 hook，两个地方都用它，因此不可能再出现互相矛盾的状态。
 *
 * ## 为什么以轮询为主、事件为辅
 *
 *   - **轮询**保证"任何耗时都能被看见"：小图库可能只扫 20 ms，事件在两帧之间
 *     就结束了，界面什么都不会显示；
 *   - **事件**保证"结束态的文案准确"，而且它自带 `cancelled` / `error`，
 *     不必从状态里反推。
 *
 * 两者互补而不是冗余：事件负责"结束"，轮询负责"过程中"。
 */
export interface ScanWatcher {
  /** 当前状态（后端那份）。还没查到过时为 null。 */
  status: ScanStatusView | null;
  /** 刚结束的一次**单库**扫描（来自事件）。 */
  justFinished: ScanDone | null;
  /**
   * 刚结束的一次「全部重新扫描」（来自事件）。
   *
   * 与 `justFinished` 分开：单库结果是"这个库可播放多少张"，
   * 而一次扫全部要回答的是**一批的合计**——新增了什么、删了什么。
   */
  batchFinished: ScanAllDone | null;
  /** 是否正在扫描。以轮询到的状态为准。 */
  running: boolean;
  /** 请求取消（5 秒后强制释放，见后端 `library_cancel_scan`）。 */
  cancel: () => Promise<void>;
  /** 收起结果（用户点了「关闭」，或切走这一页时不再需要它）。 */
  dismiss: () => void;
}

export function useScanWatcher(): ScanWatcher {
  const [status, setStatus] = React.useState<ScanStatusView | null>(null);
  const [justFinished, setJustFinished] = React.useState<ScanDone | null>(null);
  const [batchFinished, setBatchFinished] = React.useState<ScanAllDone | null>(null);

  // 事件：负责"结束态"。
  //
  // ## 结果为什么不自动消失（与"右上角浮窗"的分工）
  //
  // 原先 `justFinished` 在 4 秒后被清掉。对右上角那条一次性提示是对的，
  // 但对**图库页的结果卡**是错的：用户扫完几万个文件，正要读"新增 3、删除 2"，
  // 4 秒后它自己没了——等于没给。现在结果一直留着，直到**下一次扫描开始**、
  // 或用户点「关闭」。浮窗那条 4 秒的提示改由它自己计时（见 ScanToast）。
  React.useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    // 订阅成败都要记日志：这类"机制没生效但不报错"的问题，只有把成败
    // 都写进日志才查得动。实测正是靠这一条定位到 capabilities 缺失的。
    const track = (what: string, un: () => void) => {
      void api.logDebug(`[扫描] ${what}事件订阅成功`);
      if (disposed) un();
      else unlisteners.push(un);
    };
    // 两个事件的失败信息要能分辨是哪一个（只有一个坏掉时，日志里必须看得出）。
    const failed = (what: string) => (cause: unknown) => {
      void api.logError(`[扫描] ${what}事件订阅失败：${String(cause)}`);
    };

    void onScanDone((payload) => {
      if (!disposed) setJustFinished(payload);
    })
      .then((un) => track("单库完成", un))
      .catch(failed("单库完成"));

    void onScanAllDone((payload) => {
      if (!disposed) setBatchFinished(payload);
    })
      .then((un) => track("整批完成", un))
      .catch(failed("整批完成"));

    return () => {
      disposed = true;
      unlisteners.forEach((un) => un());
    };
  }, []);

  // 轮询：负责"过程中"，并保证任何耗时都能被看见。
  React.useEffect(() => {
    let disposed = false;
    let timer: number | undefined;
    /** 上一帧是不是在扫。用于只在"开始扫"的那一次跳变上清结果。 */
    let wasRunning = false;

    const poll = async () => {
      try {
        const next = await api.libraryScanStatus();
        if (disposed) return;
        setStatus(next);
        // **新的一次扫描一开始就清掉上一次的结论。**
        // 留着它，界面上会同时出现"正在扫描 2/5"和上一次"新增 3、删除 2"，
        // 用户分不清那是这次的结果还是上次的。
        //
        // 只在**上升沿**清，不是"每帧只要在扫就清"：结束事件是在
        // `running` 变回 false **之前**发出来的，若每帧都清，就会出现
        // "结果刚到手、下一帧就被抹掉"的竞态（窗口很小但真实存在）。
        if (next.running && !wasRunning) {
          setJustFinished(null);
          setBatchFinished(null);
        }
        wasRunning = next.running;
        // 扫描中 250 ms——用户点了扫描希望马上看到反馈。
        // 这个命令很轻（不再每次枚举显示器），密一点也不成问题。
        timer = window.setTimeout(poll, next.running ? 250 : 2000);
      } catch {
        // 查询失败不打扰用户（后端可能正在忙）。稍后再试。
        if (!disposed) timer = window.setTimeout(poll, 2000);
      }
    };

    void poll();
    return () => {
      disposed = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, []);

  const cancel = React.useCallback(async () => {
    // 5 秒后强制释放。见后端 library_cancel_scan 的说明：
    // 卡死的线程可能永远到不了检查点，而界面不能因此永久卡住。
    await api.libraryCancelScan(5);
  }, []);

  const dismiss = React.useCallback(() => {
    setJustFinished(null);
    setBatchFinished(null);
  }, []);

  return {
    status,
    justFinished,
    batchFinished,
    running: status?.running ?? false,
    cancel,
    dismiss,
  };
}

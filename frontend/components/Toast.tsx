import React from "react";

/**
 * 右上角的浮动提示。
 *
 * ## 为什么不用文档流里的提示条
 *
 * 原先"设置已保存"是渲染在页面内容里的一个盒子，出现时会把下面所有内容
 * **往下顶一行**，消失时又弹回来。保存是个很轻的动作，界面却因此抖一下，
 * 看起来像布局出了问题。
 *
 * 浮层（`position: fixed`）完全脱离文档流，因此它的出现与消失**不影响任何
 * 其它元素的排版**。
 *
 * ## 位置由 `.wwp-pop-layer` 决定，本组件不管
 *
 * 这一格**自己不写** `top`/`right`：它和右上角的扫描状态卡一起排在
 * `.wwp-pop-layer` 里（那层钉在窗口右上角、两格横向并排）。
 *
 * 用户 2026-10 的要求是"所有右上角的悬浮提示一定出现在程序 UI 界面的最右上角，
 * 不要覆盖掉重新加载几个字"，所以"右上角提示在哪"只允许有一处定义——
 * 分散在各组件里的 `top`/`right` 正是以前盖住按钮的原因（扫描卡当时写的是
 * `top: 84px`）。位置与不覆盖由 `check-layout.ps1` 的判据 8 量。
 */
export interface ToastState {
  /// 提示文字。
  text: string;
  /// 是错误还是成功。错误用红色，且**停留更久**——
  /// 出问题时用户往往没立刻看到，2 秒就消失会让人以为是误报。
  kind: "ok" | "error";
  /// 每次调用都换一个值，用来触发"重新计时"。
  ///
  /// 没有它的话，连续两次相同的提示（例如连点两次保存）不会重新显示，
  /// 因为 React 认为状态没变。
  seq: number;
}

export function useToast() {
  const [toast, setToast] = React.useState<ToastState | null>(null);
  const timer = React.useRef<number | null>(null);

  const clear = React.useCallback(() => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    setToast(null);
  }, []);

  const show = React.useCallback((text: string, kind: "ok" | "error" = "ok") => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    setToast((previous) => ({ text, kind, seq: (previous?.seq ?? 0) + 1 }));
    timer.current = window.setTimeout(
      () => {
        timer.current = null;
        setToast(null);
      },
      kind === "error" ? 6000 : 2500,
    );
  }, []);

  // 卸载时清掉计时器：不然组件没了计时器还在跑，
  // 触发 setState 会警告（而且那条提示本来也没机会显示）。
  React.useEffect(() => clear, [clear]);

  return { toast, show, clear };
}

/** 渲染浮层。没有提示时返回 `null`，不占任何位置。 */
export function ToastView({ toast }: { toast: ToastState | null }) {
  if (!toast) return null;
  return React.createElement(
    "div",
    {
      className: toast.kind === "ok" ? "wwp-toast wwp-toast-ok" : "wwp-toast wwp-toast-error",
      // 用 key 让每次新提示都重新触发入场动画：
      // 同一个元素连续改文字的话动画不会重播，用户会以为没反应。
      key: toast.seq,
      role: "status",
      "aria-live": "polite",
    },
    toast.text,
  );
}

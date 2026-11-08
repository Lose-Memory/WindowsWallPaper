import React from "react";

/**
 * 跟随鼠标的悬浮提示。
 *
 * ## 为什么不用原生 `title` 属性
 *
 * 原生 `title` 有两个问题：
 *
 *   1. **位置固定**在元素旁边，鼠标移开后不会跟着走。字段多的时候，
 *      提示可能盖住下面的字段；
 *   2. **延迟约 1 秒**才出现，而且样式由系统决定（深色/浅色跟随系统），
 *      在深色界面里可能糊成一片看不清。
 *
 * 用户明确要求"跟随鼠标的浮窗"，因此自己实现。
 *
 * ## 定位策略
 *
 * 贴在鼠标右下 14px 处。若会超出窗口右边或底边，则翻到鼠标左上——
 * 否则靠近边缘时提示会被裁掉一半，而那种提示等于没有。
 */
export function Tooltip({
  text,
  children,
}: {
  /** 提示正文。支持换行（`\n`）。 */
  text: string;
  children: React.ReactNode;
}) {
  const [pos, setPos] = React.useState<{ x: number; y: number } | null>(null);
  const [flip, setFlip] = React.useState({ x: false, y: false });
  // 提示框自身的尺寸，用于判断会不会超出窗口。拿到了才能正确翻转。
  const boxRef = React.useRef<HTMLDivElement | null>(null);
  const [box, setBox] = React.useState({ w: 0, h: 0 });

  const OFFSET = 14;

  const onMove = (e: React.MouseEvent) => {
    setPos({ x: e.clientX, y: e.clientY });
    // 右下放不下就翻到左上。留 8px 边距，免得贴着窗口边。
    setFlip({
      x: e.clientX + OFFSET + box.w > window.innerWidth - 8,
      y: e.clientY + OFFSET + box.h > window.innerHeight - 8,
    });
  };

  // 首次渲染后量一次尺寸。量到之后上面的翻转判断才准确。
  React.useEffect(() => {
    if (pos && boxRef.current && (box.w === 0 || box.h === 0)) {
      const r = boxRef.current.getBoundingClientRect();
      setBox({ w: r.width, h: r.height });
    }
  }, [pos, box.w, box.h]);

  return React.createElement(
    React.Fragment,
    null,
    React.createElement(
      "span",
      {
        className: "wwp-tip-anchor",
        onMouseMove: onMove,
        onMouseEnter: onMove,
        onMouseLeave: () => setPos(null),
      },
      children,
    ),
    pos
      ? React.createElement(
          "div",
          {
            ref: boxRef,
            className: "wwp-tip",
            style: {
              // 翻转时用"鼠标坐标减去提示自身尺寸"来定位。
              // 不用 `right`/`bottom`：那两个是相对**容器**算的，
              // 在嵌套布局里会跑到别处，而且容器出现滚动条时结果会变。
              // 用 fixed + left/top 才是相对视口，与鼠标坐标同一坐标系。
              left: flip.x ? pos.x - OFFSET - box.w : pos.x + OFFSET,
              top: flip.y ? pos.y - OFFSET - box.h : pos.y + OFFSET,
            },
          },
          text,
        )
      : null,
  );
}

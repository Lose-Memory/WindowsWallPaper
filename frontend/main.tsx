import React from "react";
import { createRoot } from "react-dom/client";
import { ErrorBoundary } from "./ErrorBoundary";
import { App } from "./App";
import "./index.css";

// 原生窗口不会给"白屏 + 控制台"：渲染期抛错的结果是一个黑窗口，
// 看起来像壁纸出了问题而不是界面崩了。所以错误边界是必需品，不是装饰。
const root = document.getElementById("root");
if (!root) {
  throw new Error("找不到 #root 容器");
}

createRoot(root).render(
  React.createElement(
    React.StrictMode,
    null,
    React.createElement(ErrorBoundary, null, React.createElement(App, null)),
  ),
);

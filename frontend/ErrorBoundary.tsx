import React from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  children: React.ReactNode;
}

interface State {
  error: Error | null;
}

/** 把渲染期异常变成可读的信息，并写进日志文件（通过后端命令）。 */
export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    const detail = `${error.stack ?? error.message}\n${info.componentStack ?? ""}`;
    // 后端命令本身也可能失败，因此这里不能再抛错。
    invoke("log_error", { message: detail }).catch(() => undefined);
  }

  render() {
    const { error } = this.state;
    if (!error) {
      return this.props.children;
    }
    return React.createElement(
      "div",
      { style: { padding: 24, fontFamily: "Consolas, monospace", fontSize: 13 } },
      React.createElement("h1", { className: "wwp-title" }, "界面发生错误"),
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "错误详情已写入程序目录 logs/app.log，可以复制下面的内容反馈。",
      ),
      React.createElement(
        "pre",
        {
          style: {
            whiteSpace: "pre-wrap",
            userSelect: "text",
            background: "rgb(127 127 127 / 0.15)",
            padding: 12,
            borderRadius: 6,
            maxHeight: "60vh",
            overflow: "auto",
          },
        },
        `${error.name}: ${error.message}\n\n${error.stack ?? ""}`,
      ),
    );
  }
}

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/**
 * 排版自检专用的构建配置（开发用，**不参与交付构建**）。
 *
 * 与 `vite.config.ts` 的区别只有一个：入口是 `layout-check/index.html`，
 * 产物落到 `layout-check-dist/`，因此**不会碰到交付用的 `dist/`**。
 */
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // 相对路径：截图时是用 `file:///…/index.html` 打开的，绝对路径 `/assets/…`
  // 会被解析到盘根（`file:///assets/…`）而 404。
  base: "./",
  build: {
    outDir: "layout-check-dist",
    emptyOutDir: true,
    target: "chrome110",
    rollupOptions: { input: "layout-check/index.html" },
  },
});

import React from "react";
import { api, type SettingsInfo, type SettingsPatch } from "../api";
import { PlaylistManager } from "./PlaylistManager";
import { useScanWatcher } from "../useScanWatcher";

interface Props {
  settings: SettingsInfo;
  /** 提交设置改动（整体替换播放列表）。 */
  onSettingsChanged: (next: SettingsInfo) => void;
  notify: (text: string, kind?: "ok" | "error") => void;
}

/**
 * 播放列表页（用户要求这一页的名字叫「列表」）。
 *
 * ## 为什么要从图库页里拆出来（用户要求）
 *
 * 原先两件事挤在「图库」页：上面是播放列表、下面是图库目录。它们其实回答
 * **两个不同的问题**——"有哪些目录可以放图"（图库）与"每块屏放哪几套图"
 * （播放列表）。挤在一页时，两边都要滚，而且点开某个列表的关联图库会把它
 * 下面的图库清单整体推下去。
 *
 * 现在一页一件事：**图库页只管道录**（新增/移除/扫描/目录树），
 * **这一页只管内容集**（新建/改名/排序/间隔/填充/勾选图库）。
 *
 * 扫描进行中时这一页也**禁止改配置**（`scanning`）：扫描读的是当前设置，
 * 中途改会得到一份不一致的结果。判据仍来自 `useScanWatcher` —— 扫描状态的
 * 唯一来源（见 §7.3），不在这一页另建一套。
 */
export function PlaylistPage({ settings, onSettingsChanged, notify }: Props) {
  const scan = useScanWatcher();

  const applyPatch = React.useCallback(
    async (patch: SettingsPatch) => {
      // `settingsUpdate` 会把整份设置返回，直接交给上层即可，
      // 因此不会出现"保存成功但界面还是旧值"。
      onSettingsChanged(await api.settingsUpdate(patch));
    },
    [onSettingsChanged],
  );

  return React.createElement(
    "div",
    null,
    React.createElement(
      "div",
      { className: "wwp-page-head" },
      React.createElement("h1", { className: "wwp-title" }, "列表"),
      React.createElement(
        "p",
        { className: "wwp-sub" },
        "一个列表关联若干图库；每块屏在「播放」页选它用哪个列表。",
      ),
    ),

    React.createElement(PlaylistManager, {
      settings,
      onPatch: applyPatch,
      notify,
      scanning: scan.running,
    }),
  );
}

import React from "react";
import { api } from "../api";
import { Tooltip } from "./Tooltip";

/**
 * 一张壁纸的预览格：缩略图 + 一行路径。
 *
 * ## 为什么要有它（用户 2026-10 的要求）
 *
 * > 在播放页面的每块屏幕设置里，我希望可以展示当前各个屏幕播放的壁纸，
 * > 以及下一张即将切换的壁纸。
 *
 * 在此之前卡片上只有一行"当前显示：…\cover_0007.jpg"的路径文字——能看出文件名，
 * 但看不出那张图是什么样。现在"当前"与"下一张"各占一格，图片直接看得见，
 * 「下一张」那格还带倒计时。
 *
 * ## 缩略图为什么要按路径自己缓存
 *
 * 轮播状态是**每秒轮询**一次的（见 `PlaybackPage`），而拿到新状态就会重渲染。
 * 若每次渲染都调一次 `wallpaper_thumbnail`，后端每秒要为同一张图做四次查询
 * （两屏 × 当前/下一张），而且每换一次纸都会重新发一遍 base64（几十 KB）。
 *
 * 因此这里把"路径 → data URL"记在组件的 state 里，并且**只在路径变化时**才请求。
 * 图片按内容缓存在后端（`data/wallpaper-thumbs/`），因此同一张图即使跨越
 * "下一张 → 当前显示"也只压一次。
 *
 * ## 状态为什么要连路径一起记
 *
 * 直接把 data URL 存成一个字符串时，路径一变（换了下一张）会出现"新的还没回来，
 * 旧的还挂在屏幕上"——用户看到的是**上一张的预览配这一张的路径**，比空白更糟。
 * 因此这里存的是 `{ path, url }`，渲染时先确认它属于当前这个路径。
 */
export function WallpaperShot({
  label,
  badge,
  pausedTag,
  path,
  placeholder,
  tip,
  onOpen,
  onReveal,
  onCopyName,
}: {
  /** 左上角那一行小字：`当前显示` / `下一张`。 */
  label: string;
  /** 右上角的小字（「下一张」那格的倒计时）。 */
  badge?: string;
  /**
   * 倒计时旁边那个状态字样（「已暂停」）。
   *
   * 用户 2026-10 的要求："暂停计时时，能显示当前的倒计时具体数值，也就是暂停的
   * 字样不要覆盖数值，显示在旁边。"因此它是**独立一格**，与数值并排，
   * 而不是把数值替换掉（原来暂停时只显示"已暂停"，看不出还剩多久）。
   */
  pausedTag?: string;
  /** 这张图的完整路径。没有（尚未换过、不轮播、轮播没跑）时传 null。 */
  path: string | null;
  /** 没有图时显示在预览框里的短说明（一两个词，必须放得下一行）。 */
  placeholder: string;
  /** 悬浮在这格上时的说明（整段文字，见 `tooltip-texts.ts`）。 */
  tip?: string;
  /**
   * 单击**缩略图那张图**时做什么（用户 2026-10 的追加要求，见下面的说明）：
   *
   * - 「当前显示」那格 → 用系统默认看图程序打开这张图（看清它本身）；
   * - 「下一张」那格 → 立刻把这块屏切到这一张。
   */
  onOpen?: () => void;
  /**
   * 单击缩略图**下面那行路径**时做什么：在资源管理器里打开它所在的目录并选中它。
   *
   * 两格都是这一件事——路径那一行的含义到处都一样（"这张图在哪儿"），
   * 不再像以前那样随格子变化（"当前显示"复制路径、"下一张"切换壁纸）。
   */
  onReveal?: () => void;
  /**
   * **右键**单击下面那行路径时做什么：把这张图的**文件名（不含扩展名）**复制走。
   *
   * 用户 2026-10 的要求："当我点击预览图下方的文件名或者路经时，现在是打开要求的
   * 指定文件夹，我希望使用右键点击这个位置时，能够复制对应图像的文件名，
   * 只复制文件名即可，不需要后缀。"单击仍然是"打开所在文件夹并选中它"——
   * 两件事分别挂在左键与右键上，谁也不抢谁。
   */
  onCopyName?: () => void;
}) {
  const shot = useThumbnail(path);

  // 图片本身可点：「当前显示」那格是"打开看"，「下一张」那格是"切过去"。
  const openable = Boolean(path && onOpen);
  // 路径那一行可点：打开所在目录并选中它。**必须真有这张图**才点得动。
  const revealable = Boolean(path && onReveal);
  // 右键复制文件名：同样只在真有这张图时成立（占位文字没有文件名可复制）。
  const copyable = Boolean(path && onCopyName);
  // 「下一张」那格整格可点，但**必须真有下一张**才点得动：固定单图 / NSFW 模式 /
  // 轮播没跑 / 筛完一张不剩时，这格显示的是占位说明，点了也没有下一张可切。

  // 预览框里显示什么：拿不到图时——先说清为什么（后端的原话，或"正在取"），
  // 只有**确实没有这张图**时才显示调用方给的短说明。
  const emptyText = shot.error ?? (path ? "预览生成中…" : placeholder);

  const body = React.createElement(
    "div",
    { className: "wwp-shot" },
    React.createElement(
      "div",
      { className: "wwp-shot-cap" },
      React.createElement("span", { className: "wwp-shot-label" }, label),
      // 状态字样在**左**，倒计时在**右**：读起来是"已暂停 · 34 秒后"，
      // 而倒计时仍然贴着卡片的右边缘（与未暂停时同一个位置，不会跳）。
      pausedTag
        ? React.createElement("span", { className: "wwp-shot-paused" }, pausedTag)
        : null,
      badge ? React.createElement("span", { className: "wwp-shot-badge" }, badge) : null,
    ),
    // ## 缩略图与路径行是两个**各自独立**的点击目标
    //
    // 以前整格只有一个点击含义（图片本身只是顺带跟着走）。用户 2026-10 把这
    // 两处拆成了不同的事：点图是"看这张图"（当前格）、点路径是"这张图在哪儿"。
    // 因此这里不再把 onClick 挂在整格上——挂在整格上，点路径就会连带触发
    // 整格那件事，两件事一起做（或按冒泡顺序只做一件，取决于写法），
    // 都不是用户要的。两处各挂各的，可点范围与看到的完全一致。
    React.createElement(
      "div",
      {
        className: openable ? "wwp-shot-frame wwp-shot-openable" : "wwp-shot-frame",
        onClick: openable ? () => void onOpen?.() : undefined,
      },
      shot.url
        ? React.createElement("img", {
            className: "wwp-shot-img",
            src: shot.url,
            alt: `${label}的壁纸预览`,
            draggable: false,
          })
        : React.createElement(
            "span",
            { className: shot.error ? "wwp-shot-empty wwp-bad" : "wwp-shot-empty" },
            emptyText,
          ),
    ),
    React.createElement(
      "span",
      {
        className: revealable
          ? "wwp-shot-path wwp-path-ellipsis wwp-shot-copy"
          : "wwp-shot-path wwp-path-ellipsis wwp-dim",
        // 完整路径挂在悬浮提示里：格子里那一行会被截断。
        title: path ?? "",
        onClick: revealable ? () => void onReveal?.() : undefined,
        // 右键 = 复制文件名（不含扩展名）。**必须调 preventDefault**：
        // WebView2 的默认右键菜单里有"重新加载""另存为"这类项，它盖在这行字上
        // 会让人以为右键点错了地方（这里要的是一次复制，不是菜单）。
        onContextMenu: copyable
          ? (event: React.MouseEvent) => {
              event.preventDefault();
              void onCopyName?.();
            }
          : undefined,
      },
      // ## 这里给的是**完整路径**，不是"最后两级"的缩写
      //
      // `.wwp-path-ellipsis` 的 `direction: rtl` 让省略号出现在**开头**
      // （保留能分辨图片的结尾目录名，见 §7.21）。而自己先在字符串前面拼一个
      // `…\` 会踩到 bidi：开头的省略号是"中性字符"，在 rtl 段落里会被排到
      // **行尾**——实测出来的效果是"竖构图\IMG_2291.jpg…"，正好把省略号放错了边。
      // 交给 CSS 截断就没有这个问题，而且与图库页、回收站页的路径写法一致。
      path ?? placeholder,
    ),
  );

  // 悬浮提示挂在**整格**上：缩略图与路径都想看说明，两处各挂一个会显得零碎。
  return tip
    ? React.createElement(Tooltip, { text: tip, children: body })
    : body;
}

/** 缩略图的结果：成功是 data URL，失败是后端给的那句原因。 */
interface Shot {
  url: string | null;
  error: string | null;
  /** 还在请求中（既没有 url 也没有 error）。 */
  pending: boolean;
}

/**
 * 最多记住几张缩略图（每张 base64 几十 KB）。
 *
 * 播放页一直开着时，每换一张纸就会遇到一个新路径。不设上限的话这个表会随着
 * 时间一直长（几千张就是几十 MB），而上限之内足够覆盖"当前 + 下一张"、
 * 以及来回切页、切列表时的复用。超出时按插入顺序丢掉最早的那张。
 */
const CACHE_LIMIT = 12;

/**
 * 按路径取缩略图。**路径不变就不重复请求**（见组件文档）。
 *
 * 请求回来后写进缓存；若那时组件已经切到别的路径（或卸载了），就只更新缓存、
 * 不再触发重渲染——同一个路径以后再遇到时直接用缓存里的结果。
 */
function useThumbnail(path: string | null): { url: string | null; error: string | null } {
  const cache = React.useRef(new Map<string, Shot>());
  const [, bump] = React.useState(0);

  React.useEffect(() => {
    if (!path || cache.current.has(path)) {
      return;
    }
    let cancelled = false;
    const remember = (shot: Shot) => {
      cache.current.set(path, shot);
      // 超上限就丢最早的那张（Map 按插入顺序迭代）。
      while (cache.current.size > CACHE_LIMIT) {
        const oldest = cache.current.keys().next().value;
        if (oldest === undefined || oldest === path) break;
        cache.current.delete(oldest);
      }
      if (!cancelled) bump((value) => value + 1);
    };

    cache.current.set(path, { url: null, error: null, pending: true });
    bump((value) => value + 1);

    api
      .wallpaperThumbnail(path)
      .then((url) => remember({ url, error: null, pending: false }))
      .catch((cause) => {
        // 后端给的是人话（"文件不存在或不可用"、"这张图太大"…），原样显示。
        const message = String(cause);
        remember({ url: null, error: message, pending: false });
        void api.logDebug(`壁纸缩略图失败（${path}）：${message}`);
      });

    return () => {
      cancelled = true;
      // 还没拿到结果就走了：把"正在取"的占位删掉，免得下次看到它一直转。
      // 已经有结果的不动——那是可复用的缓存。
      if (cache.current.get(path)?.pending) {
        cache.current.delete(path);
      }
    };
  }, [path]);

  if (!path) return { url: null, error: null };
  const hit = cache.current.get(path);
  if (!hit || hit.pending) return { url: null, error: null };
  return { url: hit.url, error: hit.error };
}


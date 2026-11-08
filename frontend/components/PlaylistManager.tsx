import React from "react";
import {
  api,
  FIT_OPTIONS,
  NEW_PLAYLIST_DEFAULTS,
  type PlaylistView,
  type SettingsInfo,
  type SettingsPatch,
} from "../api";
import { Tooltip } from "./Tooltip";
import { TIPS } from "../tooltip-texts";

/**
 * 拖动时放进 `dataTransfer` 的自定义类型。
 *
 * ## 为什么不用 `text/plain` 来判断"这是不是我们自己的拖动"
 *
 * `dragover` 阶段**读不到** `getData()` 的内容（规范禁止，只有 `drop` 才能读），
 * 但 `types` 列表是能读的。外部拖进来的文件（从资源管理器拖一张图到窗口上）
 * 会带 `Files`，因此我们用这个自定义类型把自己和其它拖动区分开：
 * 不是我们的拖动就**不** `preventDefault`，浏览器自然显示"不可放置"，
 * 我们也不会莫名其妙接管一次文件拖放。
 */
const DRAG_MIME = "application/x-wwp-playlist";

/** 这一次拖动是不是我们自己发起的（只能靠 `types` 判断，见上）。 */
function isOurDrag(event: React.DragEvent): boolean {
  const types = event.dataTransfer?.types;
  return types ? Array.from(types).includes(DRAG_MIME) : false;
}

interface Props {
  settings: SettingsInfo;
  /** 提交设置改动（整体替换播放列表）。 */
  onPatch: (patch: SettingsPatch) => Promise<void>;
  notify: (text: string, kind?: "ok" | "error") => void;
  /** 扫描进行中时禁止改配置。 */
  scanning: boolean;
}

/**
 * 播放列表管理：新建 / 改名 / **拖动排序** / 勾选图库 / 设间隔填充 / 删除。
 *
 * ## 为什么要有这一层
 *
 * 原先每块屏只能挂**一个**图库，图库自己还有个全局的"启用"开关。
 * 那套模型回答不了"同一块屏，工作时播 A、晚上播 B"——只能靠改配置
 * 或挪文件，而后者违背"从不动用户文件"的约定。
 *
 * 播放列表把"内容集"独立出来：一个列表关联多个图库，每块屏各自记住
 * 自己用哪个列表（选择器在「播放」页）。
 *
 * ## 这一层不负责"添加图库"
 *
 * 图库是独立的配置项，增删都在「图库」页（用户 2026-10-05 的明确要求：
 * 只能从图库页的按钮添加）。这里只在**已有的图库**里勾选。
 *
 * ## 为什么间隔/填充在列表上，而长宽比不在
 *
 * 两者是**内容相关**的：一套精选图想慢慢看，一套随手拍的可以快翻。
 * 长宽比是**屏幕的物理属性**（竖屏就该只留竖图），因此留在「播放」页的
 * 每屏设置里，所有列表共用。这个分工是用户明确选的。
 *
 * ## 顺序（用户 2026-10 的新要求）
 *
 * > 新增一个播放列表排序功能，使我可以在播放列表页面通过拖拽左侧的某个位置，
 * > 进行上下的排序，然后播放页显示器的设置中，选择播放列表时，使用播放列表页
 * > 中的顺序。
 *
 * 顺序就是 `settings.playlists` 这个数组的顺序：拖拽之后整体提交一次，
 * 后端原样存进 `settings.toml`，播放页那个下拉菜单直接按同一份数组渲染，
 * 因此两边的顺序**必然一致**（不需要第二处去同步）。
 *
 * 注意：这个顺序**只影响界面上列表的排列**（以及播放页下拉菜单里的排列），
 * 与"播放时先看哪张后看哪张"无关——播放顺序固定是随机（见 `playback::select`）。
 */
export function PlaylistManager({ settings, onPatch, notify, scanning }: Props) {
  const playlists = settings.playlists;
  const [busy, setBusy] = React.useState(false);
  /** 展开"关联图库"编辑的列表 id。同时只开一个，避免页面拉得很长。 */
  const [editing, setEditing] = React.useState<string | null>(null);
  /** 正在被拖动的行（下标）。null = 没有在拖。**只用于视觉**。 */
  const [dragIndex, setDragIndex] = React.useState<number | null>(null);
  /**
   * 同一个下标的 **ref 版本**。
   *
   * ## 为什么必须有一个 ref（这里真踩过）
   *
   * `dragover` / `drop` 必须在**同一个事件里同步**判断"我在拖第几行"，而
   * `setDragIndex` 是异步的（React 会把同一批事件里的更新合并、下一次渲染才生效）。
   * 早先的写法在 `onDragOver` 里先 `if (dragIndex === null) return;` 再
   * `preventDefault()`——WebView2 里第一个 `dragover` 读到的还是 `null`，
   * 于是**没有** preventDefault，浏览器立刻判定"这里不能放"，鼠标变成
   * 一个红色的禁止符号，后面 `drop` 也永远不会触发。
   * 用户在真机上看到的就是"手柄能拖起来，但一拖就是禁止符号"。
   */
  const dragIndexRef = React.useRef<number | null>(null);
  /** 拖动经过的行与落点（上半 = 插到它前面，下半 = 插到它后面）。 */
  const [dropHint, setDropHint] = React.useState<{ index: number; after: boolean } | null>(null);

  /** 把列表整体提交（后端语义就是整体替换）。 */
  const submit = React.useCallback(
    async (next: PlaylistView[], why: string) => {
      setBusy(true);
      try {
        await onPatch({
          playlists: next.map((p) => ({
            id: p.id,
            name: p.name,
            libraries: p.libraries,
            intervalSeconds: p.intervalSeconds,
            fitMode: p.fitMode,
          })),
        });
        notify(why);
      } catch (cause) {
        // **保存失败必须说出来。**
        //
        // 这里原先没有 catch（别的页面都有），于是后端拒绝时错误变成
        // 未处理的 Promise 拒绝：不弹提示、不写日志、下拉框悄悄弹回旧值。
        // 用户看到的是"这一页点了没反应、卡死了"，而同一时刻别的页面都正常，
        // 完全看不出是"某一项校验没过导致整份提交被拒"。
        //
        // 实测那次的原因是：他在资源管理器里把一个图库目录改了名，
        // 而后端当时会因为"目录不存在"拒绝整份播放列表（该处已放宽，
        // 见 `commands::apply_playlist_list`）。两处缺一不可：
        // 一处让失败可见，一处让无关的改动不被牵连。
        void api.logError(`[播放列表] 保存失败：${String(cause)}`);
        notify(`保存失败：${String(cause)}`, "error");
      } finally {
        setBusy(false);
      }
    },
    [onPatch, notify],
  );

  const addPlaylist = async () => {
    // 名字给一个能直接用的默认值，用户可以马上改。
    const used = new Set(playlists.map((p) => p.name));
    let n = playlists.length + 1;
    while (used.has(`列表 ${n}`)) n += 1;
    await submit(
      [
        ...playlists,
        {
          // 空 id：后端会分配一个稳定标识。
          id: "",
          name: `列表 ${n}`,
          libraries: [],
          // 间隔/填充的初值来自 NEW_PLAYLIST_DEFAULTS（100 秒/填满），
          // 而不是"当前哪个列表的设置"——新建时还没有可参照的列表。
          ...NEW_PLAYLIST_DEFAULTS,
        },
      ],
      `已新建「列表 ${n}」，请给它关联图库`,
    );
  };

  const removePlaylist = async (playlist: PlaylistView) => {
    await submit(
      playlists.filter((p) => p.id !== playlist.id),
      `已删除「${playlist.name}」`,
    );
    if (editing === playlist.id) setEditing(null);
  };

  const rename = async (playlist: PlaylistView, name: string) => {
    await submit(
      playlists.map((p) => (p.id === playlist.id ? { ...p, name } : p)),
      "已改名",
    );
  };

  const setField = async (
    playlist: PlaylistView,
    field: "fitMode" | "intervalSeconds",
    value: string | number,
  ) => {
    await submit(
      playlists.map((p) => (p.id === playlist.id ? { ...p, [field]: value } : p)),
      "已保存",
    );
  };

  /**
   * 勾选/取消勾选一个图库。
   *
   * ## 这里**只有勾选**，没有"添加图库"
   *
   * 用户 2026-10-05 要求："图库"页要有直接的「新增图库」按钮，
   * 只能从那里添加；**取消通过播放列表新增图库**。因此这一层只负责
   * "这个列表用已有的哪几个图库"，添加入口在 `LibraryManager`。
   */
  const toggleLibrary = async (playlist: PlaylistView, path: string) => {
    const has = playlist.libraries.includes(path);
    const next = has
      ? playlist.libraries.filter((p) => p !== path)
      : [...playlist.libraries, path];
    await submit(
      playlists.map((p) => (p.id === playlist.id ? { ...p, libraries: next } : p)),
      has ? `已从「${playlist.name}」移除该图库` : `已把该图库加入「${playlist.name}」`,
    );
  };

  /**
   * 把第 `from` 个列表挪到"落点"位置。
   *
   * `insertAt` 是**还没有拿走它时**的目标下标；拿走之后若目标在它后面，
   * 下标要减一（经典的那种 off-by-one）。不处理这一步的话，
   * 往下拖一格会变成"没动"。
   */
  const movePlaylist = async (from: number, insertAt: number) => {
    let to = insertAt;
    if (from < to) to -= 1;
    if (to === from) return;
    const next = [...playlists];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    await submit(next, `已把「${moved.name}」挪到第 ${to + 1} 位`);
  };

  const canDrag = !busy && !scanning && playlists.length > 1;

  const allLibraries = settings.libraries.map((l) => l.path);

  return React.createElement(
    "div",
    { className: "wwp-card" },
    // 页面标题已经是「列表」（用户要求把这一页的名字改成"列表"），
    // 因此这里不再重复一次同名标题，只说"全部列表"。
    React.createElement("h2", { className: "wwp-h2" }, "全部列表"),
    React.createElement(
      "p",
      { className: "wwp-sub" },
      playlists.length > 1
        ? "一个列表可关联多个图库；拖动左侧手柄调整顺序，播放页下拉菜单同序。"
        : "一个列表可关联多个图库；每块屏用哪个列表在「播放」页选。",
    ),

    React.createElement(
      "div",
      { className: "wwp-control-row", style: { marginBottom: 10 } },
      React.createElement(
        "button",
        { className: "wwp-btn wwp-btn-primary", disabled: busy || scanning, onClick: () => void addPlaylist() },
        "新建列表",
      ),
      React.createElement(
        "span",
        { className: "wwp-dim" },
        playlists.length === 0
          ? "还没有列表。新建一个并关联图库后才能开始轮播。"
          : `共 ${playlists.length} 个列表`,
      ),
    ),

    playlists.length === 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          "列表为空时，任何显示器都没有内容可播——这是刻意的，程序不会替你猜该播什么。",
        )
      : React.createElement(
          "div",
          { className: "wwp-list" },
          playlists.map((playlist, index) =>
            React.createElement(PlaylistRow, {
              key: playlist.id || playlist.name,
              playlist,
              allLibraries,
              busy: busy || scanning,
              open: editing === playlist.id,
              // --- 拖动排序的状态与回调 ---
              canDrag,
              dragging: dragIndex === index,
              dropBefore: dropHint?.index === index && !dropHint.after,
              dropAfter: dropHint?.index === index && dropHint.after,
              onDragStart: (event: React.DragEvent) => {
                // 先写 ref 再 setState：ref 是同步的，后面的 dragover/drop 立刻能读到。
                dragIndexRef.current = index;
                setDragIndex(index);
                event.dataTransfer.effectAllowed = "move";
                // 自定义类型用来在 dragover 里认出自己（那里读不到 getData）；
                // `text/plain` 是给"拖到别的程序"留的兜底，顺便满足
                // "不 setData 就不启动拖动"的浏览器怪癖。
                event.dataTransfer.setData(DRAG_MIME, String(index));
                event.dataTransfer.setData("text/plain", String(index));
                // 把整行当拖动影像（默认只有那个小方块手柄，看不出在拖哪一行）。
                // 失败也无所谓：拖动本身照常进行，只是影像退回默认的样子。
                try {
                  const row = (event.currentTarget as HTMLElement).closest(".wwp-playlist");
                  if (row) event.dataTransfer.setDragImage(row, 24, 18);
                } catch {
                  // 某些环境下（例如合成事件、或元素尚未有渲染框）不允许设影像。
                }
              },
              onDragOver: (event: React.DragEvent) => {
                if (!isOurDrag(event)) return;
                // **必须** preventDefault，否则这个元素不算合法的放置目标：
                // 鼠标会变成禁止符号，而且 drop 永远不会触发。
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
                const rect = event.currentTarget.getBoundingClientRect();
                const after = event.clientY > rect.top + rect.height / 2;
                setDropHint((prev) =>
                  prev && prev.index === index && prev.after === after
                    ? prev
                    : { index, after },
                );
              },
              onDrop: (event: React.DragEvent) => {
                const from = dragIndexRef.current;
                if (from === null || !isOurDrag(event)) return;
                event.preventDefault();
                const rect = event.currentTarget.getBoundingClientRect();
                const after = event.clientY > rect.top + rect.height / 2;
                dragIndexRef.current = null;
                setDragIndex(null);
                setDropHint(null);
                void movePlaylist(from, index + (after ? 1 : 0));
              },
              onDragEnd: () => {
                dragIndexRef.current = null;
                setDragIndex(null);
                setDropHint(null);
              },
              onToggleOpen: () =>
                setEditing(editing === playlist.id ? null : playlist.id),
              onRename: (name: string) => void rename(playlist, name),
              onRemove: () => void removePlaylist(playlist),
              onFit: (v: string) => void setField(playlist, "fitMode", v),
              onInterval: (v: number) => void setField(playlist, "intervalSeconds", v),
              onToggleLibrary: (path: string) => void toggleLibrary(playlist, path),
              notify,
            }),
          ),
        ),
  );
}

/**
 * 一行播放列表：**所有信息都在同一行**（用户要求"一个播放列表的信息只占一行，
 * 不要折行"）。
 *
 * 因此这里做了三件事：
 *   1. 间隔与填充从第二行并回这一行（原先它们单独占一行）；
 *   2. 行的 CSS 是 `flex-wrap: nowrap`，长名字只会把名字输入框压窄，不会换行；
 *   3. 填充方式用短标签（`Fill` / `Fit` / `Stretch` / `Center`）——中文全称
 *      （"完整显示（留黑边）"）在下拉框里就有一百多像素，正是折行的主因；
 *      中文含义挂在「填充」标签的悬浮提示里（`TIPS.fit`）。
 *
 * 最左边是**拖动手柄**（用户要求"通过拖拽左侧的某个位置"进行上下排序）：
 * 只有它是 `draggable`，因此名字输入框里的文字照样能选中、编辑。
 */
function PlaylistRow({
  playlist,
  allLibraries,
  busy,
  open,
  canDrag,
  dragging,
  dropBefore,
  dropAfter,
  onDragStart,
  onDragOver,
  onDrop,
  onDragEnd,
  onToggleOpen,
  onRename,
  onRemove,
  onFit,
  onInterval,
  onToggleLibrary,
  notify,
}: {
  playlist: PlaylistView;
  allLibraries: string[];
  busy: boolean;
  open: boolean;
  canDrag: boolean;
  dragging: boolean;
  dropBefore: boolean;
  dropAfter: boolean;
  onDragStart: (event: React.DragEvent) => void;
  onDragOver: (event: React.DragEvent) => void;
  onDrop: (event: React.DragEvent) => void;
  onDragEnd: () => void;
  onToggleOpen: () => void;
  onRename: (name: string) => void;
  onRemove: () => void;
  onFit: (value: string) => void;
  onInterval: (value: number) => void;
  onToggleLibrary: (path: string) => void;
  notify: (text: string, kind?: "ok" | "error") => void;
}) {
  // 名字与间隔都是"输入中"的状态，失焦时才提交。
  // 每敲一个字就提交一次会让后端反复写盘，而且会打断输入法的组合。
  const [name, setName] = React.useState(playlist.name);
  const [interval, setIntervalText] = React.useState(String(playlist.intervalSeconds));
  React.useEffect(() => setName(playlist.name), [playlist.name]);
  React.useEffect(
    () => setIntervalText(String(playlist.intervalSeconds)),
    [playlist.intervalSeconds],
  );

  const commitInterval = () => {
    const parsed = Number.parseInt(interval, 10);
    if (!Number.isFinite(parsed)) {
      setIntervalText(String(playlist.intervalSeconds));
      notify("间隔必须是数字", "error");
      return;
    }
    // 后端会夹到 10..86400；这里先夹一次，免得用户看到"输入 5、显示 10"时困惑。
    const clamped = Math.min(86_400, Math.max(10, parsed));
    if (clamped !== parsed) notify(`间隔已调整到允许范围（${clamped} 秒）`);
    setIntervalText(String(clamped));
    if (clamped !== playlist.intervalSeconds) onInterval(clamped);
  };

  const className = [
    "wwp-playlist",
    dragging ? "wwp-playlist-dragging" : "",
    dropBefore ? "wwp-playlist-drop-before" : "",
    dropAfter ? "wwp-playlist-drop-after" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return React.createElement(
    "div",
    {
      className,
      // 拖动过程中的"落点"要靠整行来接（手柄太小，鼠标一晃就出了它的范围）。
      onDragOver,
      onDrop,
    },
    React.createElement(
      "div",
      { className: "wwp-playlist-head" },
      // 拖动手柄。只有它 draggable：
      // 整行 draggable 的话输入框里的文字就没法用鼠标选中了。
      React.createElement(
        "span",
        {
          className: canDrag ? "wwp-drag-handle" : "wwp-drag-handle wwp-drag-handle-off",
          draggable: canDrag,
          title: canDrag
            ? TIPS.dragHandle
            : TIPS.dragHandleDisabled,
          "aria-label": "拖动调整顺序",
          onDragStart: canDrag ? onDragStart : undefined,
          onDragEnd: canDrag ? onDragEnd : undefined,
        },
        "⋮⋮",
      ),
      React.createElement("input", {
        className: "wwp-input wwp-playlist-name",
        value: name,
        disabled: busy,
        "aria-label": "播放列表名称",
        onChange: (e: React.ChangeEvent<HTMLInputElement>) => setName(e.target.value),
        onBlur: () => {
          const trimmed = name.trim();
          if (!trimmed) {
            setName(playlist.name);
            notify("名字不能为空", "error");
            return;
          }
          if (trimmed !== playlist.name) onRename(trimmed);
        },
      }),
      React.createElement(
        "label",
        { className: "wwp-playlist-field" },
        React.createElement(Tooltip, {
          text: TIPS.interval,
          children: React.createElement("span", { className: "wwp-dim" }, "间隔"),
        }),
        React.createElement("input", {
          className: "wwp-input wwp-input-xs",
          value: interval,
          disabled: busy,
          "aria-label": "换图间隔（秒）",
          onChange: (e: React.ChangeEvent<HTMLInputElement>) => setIntervalText(e.target.value),
          onBlur: commitInterval,
        }),
        React.createElement("span", { className: "wwp-dim" }, "秒"),
      ),
      React.createElement(
        "label",
        { className: "wwp-playlist-field" },
        React.createElement(Tooltip, {
          text: TIPS.fit,
          children: React.createElement("span", { className: "wwp-dim" }, "填充"),
        }),
        React.createElement(
          "select",
          {
            className: "wwp-input wwp-input-xs",
            value: playlist.fitMode,
            disabled: busy,
            "aria-label": "填充方式",
            onChange: (e: React.ChangeEvent<HTMLSelectElement>) => onFit(e.target.value),
          },
          FIT_OPTIONS.map((o) =>
            React.createElement("option", { key: o.value, value: o.value }, o.label),
          ),
        ),
      ),
      React.createElement(
        "span",
        { className: "wwp-tag" },
        playlist.libraries.length === 0
          ? "未关联图库"
          : `${playlist.libraries.length} 个图库`,
      ),
      React.createElement(
        "button",
        {
          // 定宽：这个按钮的文字在「关联图库…」与「收起」之间变，不定宽的话
          // 它一变宽，名字输入框就跟着伸缩，整行的控件左右挪（用户报过这个跳动）。
          className: "wwp-btn wwp-btn-quiet wwp-btn-stable",
          disabled: busy,
          onClick: onToggleOpen,
        },
        open ? "收起" : "关联图库…",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy,
          onClick: onRemove,
          title: TIPS.deletePlaylist,
        },
        "删除",
      ),
    ),
    open
      ? React.createElement(PlaylistLibraries, {
          playlist,
          allLibraries,
          busy,
          onToggle: onToggleLibrary,
        })
      : null,
  );
}

/** 勾选这个列表包含哪些图库（**只能勾已有的**，添加图库在「图库」页）。 */
function PlaylistLibraries({
  playlist,
  allLibraries,
  busy,
  onToggle,
}: {
  playlist: PlaylistView;
  allLibraries: string[];
  busy: boolean;
  onToggle: (path: string) => void;
}) {
  // **不把已关联的置顶**（用户要求）：顺序与「图库」页完全一致。
  //
  // 置顶看着"贴心"，实际很烦人：勾一个，那一项就跳到最上面，剩下的一起往下滑，
  // 于是想连勾几个时鼠标底下的东西一直在换。而且两页的顺序不一致，也没法对照。
  // 顺序的唯一来源是后端（`settings_info` 按路径排好），这里原样用。
  const ordered = allLibraries;
  const linked = playlist.libraries;

  return React.createElement(
    "div",
    { className: "wwp-playlist-libs" },
    React.createElement(
      "div",
      { className: "wwp-dim", style: { marginBottom: 6 } },
      `勾选这个列表要包含的图库（已选 ${linked.length} 个）；同一个图库可被多个列表共用。`,
    ),
    ordered.length === 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          "图库清单是空的。先到「图库」页点「新增图库」添加一个放图片的文件夹。",
        )
      : ordered.map((path) => {
          const on = linked.includes(path);
          return React.createElement(
            "label",
            { key: path, className: on ? "wwp-tree-row" : "wwp-tree-row wwp-tree-row-off" },
            React.createElement("input", {
              type: "checkbox",
              checked: on,
              disabled: busy,
              onChange: () => onToggle(path),
            }),
            React.createElement("span", { className: "wwp-lib-path", title: path }, path),
          );
        }),
  );
}

import React from "react";
import {
  api,
  type FolderNode,
  type LibraryView,
  type SettingsInfo,
  type SettingsPatch,
} from "../api";
import { Tooltip } from "./Tooltip";
import { indexFileHelp, TIPS } from "../tooltip-texts";

interface Props {
  settings: SettingsInfo;
  /** 把改动提交给后端（整体替换图库列表或排除规则）。 */
  onPatch: (patch: SettingsPatch) => Promise<void>;
  /** 右上角浮动提示。 */
  notify: (text: string, kind?: "ok" | "error") => void;
  /** 交给外层去做扫描（沿用图库页已有的扫描入口）。 */
  onScan: (path: string, rescan: boolean) => Promise<void>;
  /** 正在扫描时禁止改配置：扫描读的是当前设置，中途改会得到不一致的结果。 */
  scanning: boolean;
}

/**
 * 图库管理：新增/移除图库路径，并用目录树勾选哪些目录参与轮播。
 *
 * ## 「新增图库」为什么在这里，而不在播放列表里
 *
 * 原先图库不是独立的配置项：那份清单是从所有播放列表汇总出来的，因此
 * 唯一的"添加图库"入口是给某个播放列表关联目录。用户 2026-10-05 要求改掉：
 * **图库页要有直接的「新增图库」按钮，只能从那里添加；播放列表只负责勾选**。
 *
 * 现在两层是分开的：图库清单说"有哪几个放图片的根目录"（这一页），
 * 播放列表说"我用其中哪几个"（`PlaylistManager`）。因此**允许**一个图库
 * 暂时不被任何列表引用——那种图库不会被播到，行上会标「未被使用」。
 *
 * ## 为什么用目录树而不是只给一个路径列表
 *
 * 一个图库目录下往往有几十个子目录（本机那套素材就是 26 个），
 * 用户经常只想用其中一部分。只给"整个目录参与/不参与"的话，
 * 他要么全收、要么自己在文件系统里挪文件——而本程序的约定是
 * **从不动用户的文件**。
 *
 * ## 勾选状态的存储方式（重要）
 *
 * 界面上的复选框反映的是"实际是否参与轮播"；用户改变勾选时，
 * 我们**把路径加进/移出排除规则的 `dirs` 列表**，而不是另建一份
 * "包含列表"。
 *
 * 这样做的好处是复用了已经实测过的排除机制，而且它本来就是
 * **前缀匹配**：排除一个父目录，它全部子目录自动都不参与，
 * 不需要逐个写入。这正是"取消勾选父级则子级全不参与"的语义。
 */
export function LibraryManager({ settings, onPatch, notify, onScan, scanning }: Props) {
  const [treeFor, setTreeFor] = React.useState<string | null>(null);

  /**
   * 在资源管理器里打开一个图库目录。
   *
   * 后端做了"目录是否存在"的校验，因此盘没插、目录被改名都会走到这里，
   * 用右上角浮层说清楚——而不是让资源管理器弹一个看不懂的错误框。
   */
  const openInExplorer = async (path: string) => {
    try {
      await api.openFolder(path);
    } catch (cause) {
      notify(`打开目录失败：${String(cause)}`, "error");
    }
  };
  const [tree, setTree] = React.useState<FolderNode[] | null>(null);
  const [treeError, setTreeError] = React.useState<string | null>(null);
  const [loadingTree, setLoadingTree] = React.useState(false);
  const [busy, setBusy] = React.useState(false);

  /** 当前清单里的路径（提交时用的就是它）。 */
  const paths = settings.libraries.map((l) => l.path);

  /**
   * 新增一个图库：弹系统选目录对话框，把它追加进清单。
   *
   * 用 `pickFolder`（Windows 自带的 `SHBrowseForFolderW`）而不是让用户手打路径：
   * 手打路径很难写对，而写错的代价是"这个图库永远扫不出东西"。
   */
  const addLibrary = async () => {
    setBusy(true);
    try {
      // 上次选过的目录当起点：用户的图库往往在同一块盘、相邻的目录下。
      const picked = await api.pickFolder(settings.libraries.at(-1)?.path);
      if (!picked) return; // 用户取消
      if (paths.some((p) => p.toLowerCase() === picked.toLowerCase())) {
        notify("这个目录已经在图库清单里了", "error");
        return;
      }
      await onPatch({ libraries: [...paths, picked] });
      notify(`已添加图库：${picked}`);
    } catch (cause) {
      notify(`新增图库失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  /**
   * 移除一个图库。
   *
   * 只把它从清单里去掉（并顺手清掉播放列表里对它的引用），
   * **不动磁盘上的任何文件**——这是本项目的硬约定。
   * `data/index/` 下那份索引也不删：再用同一个路径加回来时可以直接复用。
   */
  const removeLibrary = async (library: LibraryView) => {
    setBusy(true);
    try {
      await onPatch({
        libraries: paths.filter((p) => p.toLowerCase() !== library.path.toLowerCase()),
      });
      notify(
        library.usedBy > 0
          ? `已移除图库，并已从 ${library.usedBy} 个播放列表解绑`
          : "已移除该图库",
      );
      // 被移除的那个图库目录树还开着的话，收起来——它已经不在清单里了。
      if (treeFor === library.path) {
        setTreeFor(null);
        setTree(null);
      }
    } catch (cause) {
      notify(`移除图库失败：${String(cause)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  /**
   * 追加一批排除规则（去重，大小写不敏感）。
   *
   * 用它而不是直接拼数组，是为了让"去重"只有一处实现——全选/全不选要
   * 批量加很多条，重复写入会让排除列表迅速膨胀，而 `dirs` 是**前缀匹配**：
   * 列表越长，遍历时每个目录的匹配成本越高。
   */
  const withDirs = (extra: string[]) => {
    const existing = new Set(settings.exclude.dirs.map((d) => d.toLowerCase()));
    const added = extra.filter((p) => !existing.has(p.toLowerCase()));
    return [...settings.exclude.dirs, ...added];
  };

  /**
   * 全选：清空所有**目录**规则。
   *
   * 只清 `dirs`——glob/regex 是用户在排除页手写的另一类规则，这里"全选"的
   * 含义是"目录树上的目录全都参与"，不该顺手删掉那些规则。
   *
   * 若那类规则恰好排除了某些目录，界面上的复选框会如实显示成未勾选
   * （`included` 用的是完整匹配器，不只是 dirs），因此不会出现
   * "显示已勾选但实际被排除"这种自相矛盾的状态。
   */
  const selectAll = async () => {
    if (settings.exclude.dirs.length === 0) {
      notify("所有目录都已经参与轮播");
      return;
    }
    await onPatch({ exclude: { ...settings.exclude, dirs: [] } });
    notify("已让所有目录参与轮播");
    void refreshTree();
  };

  /**
   * 全不选：把当前树里**所有**目录写进排除规则。
   *
   * 只加最外层其实就够（前缀匹配，子目录自动跟着不参与），但这里仍逐条
   * 写入，原因是：用户"全不选之后只想留几个目录"是常见用法，而逐条写入
   * 让那种操作可以直接进行——否则子目录会因为父级仍在列表里而"勾不动"
   * （见 toggleFolder 的说明）。
   */
  const selectNone = async () => {
    if (!tree || tree.length === 0) return;
    await onPatch({
      exclude: { ...settings.exclude, dirs: withDirs(tree.map((n) => n.path)) },
    });
    notify(`已取消全部 ${tree.length} 个目录`);
    void refreshTree();
  };

  const openTree = async (path: string) => {
    if (treeFor === path) {
      setTreeFor(null);
      setTree(null);
      return;
    }
    setTreeFor(path);
    setTree(null);
    setTreeError(null);
    setLoadingTree(true);
    try {
      setTree(await api.libraryFolderTree(path));
    } catch (cause) {
      setTreeError(String(cause));
    } finally {
      setLoadingTree(false);
    }
  };

  /**
   * 切换一个目录的参与状态。
   *
   * 语义（必须与后端的目录前缀匹配一致）：
   *  - 取消勾选 → 把该路径加进排除列表。它的**全部子目录自动都不参与**。
   *  - 勾选 → 从排除列表里移除该路径。
   *
   * 但有个情况要处理：如果**某个祖先**被排除了，那么单独"勾选"这个子目录
   * 是无效的（祖先规则仍然命中它）。这时我们给出明确提示，而不是让用户
   * 反复点一个看起来没反应的复选框。
   */
  const toggleFolder = async (node: FolderNode) => {
    const dirs = settings.exclude.dirs;
    if (node.included) {
      // 取消勾选：加进排除列表（去重）。
      if (dirs.some((d) => d.toLowerCase() === node.path.toLowerCase())) return;
      await onPatch({ exclude: { ...settings.exclude, dirs: [...dirs, node.path] } });
      notify(`已取消：${node.name}（其子目录一并取消）`);
      void refreshTree();
      return;
    }

    // 想勾选。先看是不是被祖先挡着。
    if (!node.excludedHere) {
      notify(
        `无法单独勾选「${node.name}」：它的上级目录已被取消。请先勾选上级。`,
        "error",
      );
      return;
    }
    await onPatch({
      exclude: {
        ...settings.exclude,
        dirs: dirs.filter((d) => d.toLowerCase() !== node.path.toLowerCase()),
      },
    });
    notify(`已启用：${node.name}`);
    void refreshTree();
  };

  /** 重新读一次目录树，让 included 状态反映刚提交的改动。 */
  const refreshTree = async () => {
    if (!treeFor) return;
    try {
      setTree(await api.libraryFolderTree(treeFor));
    } catch {
      // 刷新失败不该清掉已有内容——用户可能只是切了一下勾选。
    }
  };

  return React.createElement(
    "div",
    { className: "wwp-card" },
    React.createElement("h2", { className: "wwp-h2" }, "图库目录"),

    // 「新增图库」是**唯一**的添加入口（用户要求：不要再绕到播放列表里去做）。
    React.createElement(
      "div",
      { className: "wwp-control-row", style: { marginBottom: 10 } },
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-primary",
          disabled: busy || scanning,
          onClick: () => void addLibrary(),
          title: TIPS.addLibrary,
        },
        "新增图库",
      ),
      React.createElement(
        "span",
        { className: "wwp-dim" },
        settings.libraries.length === 0
          ? "还没有图库。选一个放图片的文件夹，扫描后就能在播放列表里勾选它。"
          : `共 ${settings.libraries.length} 个图库`,
      ),
    ),

    settings.libraries.length === 0
      ? React.createElement(
          "div",
          { className: "wwp-note" },
          "还没有图库。点上面的「新增图库」选一个放图片的文件夹。",
        )
      : React.createElement(
          "div",
          { className: "wwp-lib-list" },
          settings.libraries.map((library) =>
            // 目录树是**这一行的子面板**（用户要求：点在哪个图库上，目录树就出现在
            // **那一行的下方**）。原先它渲染在整个列表之后——点开一个图库要滚到
            // 页面底部才看得到，与"我点的是这一行"完全对不上。
            React.createElement(
              "div",
              { className: "wwp-lib-group", key: library.path },
              React.createElement(LibraryRow, {
                library,
                busy: scanning || busy,
                treeOpen: treeFor === library.path,
                onOpenTree: () => void openTree(library.path),
                onScan: (rescan: boolean) => void onScan(library.path, rescan),
                onOpenFolder: () => void openInExplorer(library.path),
                onRemove: () => void removeLibrary(library),
              }),
              treeFor === library.path
                ? React.createElement(FolderTree, {
                    root: treeFor,
                    nodes: tree,
                    error: treeError,
                    loading: loadingTree,
                    busy: scanning || busy,
                    onToggle: (node: FolderNode) => void toggleFolder(node),
                    onSelectAll: () => void selectAll(),
                    onSelectNone: () => void selectNone(),
                  })
                : null,
            ),
          ),
        ),
  );
}


/**
 * 一行图库：路径、被哪些列表使用、目录树入口、扫描。
 *
 * ## 为什么用"表格"的排布（而不是每行自由伸缩）
 *
 * 之前每行是 flex：路径能有多长就占多长，于是长路径会把后面的标签挤成两行，
 * 或者把整行推开、各行的标签位置都不一样——看起来就是"乱"。
 * 现在用固定的列宽（`grid-template-columns`）：标签与按钮各占固定的一列，
 * 只有路径那一列伸缩，因此**所有行的标签与按钮都对齐**。
 *
 * ## 路径为什么"从开头截断"
 *
 * 图库路径的共同前缀往往很长（`D:\Pictures\...`），而真正能区分两条路径的是
 * **结尾那一段目录名**。省略号放在开头才能让人一眼分辨；放在结尾的话每行看到的
 * 都是同一个前缀。完整路径在跟随鼠标的悬浮提示里。
 */
function LibraryRow({
  library,
  busy,
  treeOpen,
  onOpenTree,
  onScan,
  onOpenFolder,
  onRemove,
}: {
  library: LibraryView;
  busy: boolean;
  treeOpen: boolean;
  onOpenTree: () => void;
  onScan: (rescan: boolean) => void;
  /** 在资源管理器里打开这个图库。 */
  onOpenFolder: () => void;
  /** 从图库清单里移除它（不动磁盘上的文件）。 */
  onRemove: () => void;
}) {
  // 没有被任何播放列表引用 = 不会被播到。这不是错误（刚加进来的图库就是这样），
  // 但值得标出来——用户添加了图库却发现"没有它的图"，最可能的原因就是这个。
  const orphan = library.usedBy === 0;

  // 路径单元：锁死一行，配合 CSS 的 `direction: rtl` 实现"从开头截断"。
  // 用 Tooltip（跟随鼠标）而不是原生 title：原生 title 要等一秒、
  // 样式也不可控（深色界面下常糊成一片）。
  const pathCell = React.createElement(
    "span",
    { className: orphan ? "wwp-cell-path wwp-dim" : "wwp-cell-path" },
    React.createElement("span", { className: "wwp-path-ellipsis" }, library.path),
  );

  return React.createElement(
    "div",
    {
      // 行底色表达"这个图库建好索引了没有"：
      // 淡绿 = 已建索引（点开就能播）、淡红 = 还没有索引（播不出东西）。
      // 原先这一列是一个「已建索引 / 未扫描」标签，用户要求换成整行底色，
      // 并把那一格让给「打开目录」——他能一眼看出哪些图库还没扫。
      className: library.scanned
        ? "wwp-lib-row wwp-lib-row-scanned"
        : "wwp-lib-row wwp-lib-row-unscanned",
    },
    React.createElement(Tooltip, { text: library.path, children: pathCell }),

    orphan
      ? React.createElement(
          "span",
          {
            className: "wwp-tag wwp-tag-warn wwp-cell-tag",
            title: TIPS.orphanTag,
          },
          "未被使用",
        )
      : React.createElement(
          "span",
          { className: "wwp-tag wwp-cell-tag", title: TIPS.usedByTag },
          `${library.usedBy} 个列表在用`,
        ),

    // 操作列：**「打开目录」与其余四个按钮在同一格里**（用户要求把这几个按钮的
    // 间距拉近）。原先「打开目录」独占一列，与右边那四个按钮隔了 12px 的列间距，
    // 看起来像两组不相干的操作；合并之后它们是一组，间距由 `.wwp-cell-actions`
    // 统一控制（已调小）。
    //
    // 少了一列之后，剩下的宽度全给了**路径那一列**（`minmax(0, 1fr)`），
    // 也就是用户要的"增加左侧显示具体路径的宽度"。
    React.createElement(
      "span",
      { className: "wwp-cell-actions" },
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy,
          title: TIPS.openFolder(library.path),
          onClick: onOpenFolder,
        },
        "打开目录",
      ),

      // 索引文件里存了什么、字段什么意思，正是看着这个按钮时会问的问题，
      // 因此把那份说明挂成**跟随鼠标的悬浮提示**（原生 title 要等一秒、
      // 而且这种多行说明用系统样式在深色界面下很难读）。
      React.createElement(
        Tooltip,
        {
          text: `${library.scanned ? TIPS.scanButtonAgain : TIPS.scanButtonIdle}\n\n${indexFileHelp()}`,
          children: React.createElement(
            "button",
            {
              // `wwp-btn-stable`：文字会变的按钮要给一个固定宽度，
              // 否则"扫描 → 重新扫描"会让这一格变宽、把左边那列挤窄，
              // 于是整页的路径列跟着变（用户报过这个跳动）。
              className: "wwp-btn wwp-btn-stable",
              disabled: busy,
              onClick: () => onScan(false),
            },
            library.scanned ? "重新扫描" : "扫描",
          ),
        },
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet wwp-btn-stable",
          disabled: busy,
          onClick: () => onScan(true),
          title: TIPS.rescanFull,
        },
        "全量重扫",
      ),
      // 展开目录树时这个按钮要**看得出来是开着的**：之前只有文字从"选择目录…"
      // 变成"收起目录"，在一排灰色按钮里几乎注意不到，用户会以为点了没反应。
      React.createElement(
        "button",
        {
          // 定宽同样是为了不跳动：这两个标签宽度不一样，而它一变，
          // 整个操作列（以及左边那列路径）都会跟着挪。
          className: `wwp-btn-stable ${
            treeOpen ? "wwp-btn wwp-btn-on" : "wwp-btn wwp-btn-quiet"
          }`,
          disabled: busy,
          onClick: onOpenTree,
          title: treeOpen ? TIPS.chooseFolderOpen : TIPS.chooseFolderClosed,
          "aria-pressed": treeOpen,
        },
        treeOpen ? "收起目录" : "选择目录…",
      ),
      // 移除放在最后一个按钮位：它不动磁盘上的任何文件，但用户可能只想
      // "先不看了"——把它的后果写在悬浮提示里，而不是弹一个确认框。
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet wwp-btn-danger",
          disabled: busy,
          onClick: onRemove,
          title: TIPS.removeLibrary,
        },
        "移除",
      ),
    ),
  );
}

/**
 * 目录树。只显示目录，不显示文件。
 *
 * 用缩进表示层级而不是可折叠的树：图库通常只有 5~30 个子目录，
 * 全部展开反而更容易一眼看清"哪些被取消了"。
 * 目录很多时（超过 maxDepth 的层级）由后端截断。
 */
function FolderTree({
  root,
  nodes,
  error,
  loading,
  busy,
  onToggle,
  onSelectAll,
  onSelectNone,
}: {
  root: string;
  nodes: FolderNode[] | null;
  error: string | null;
  loading: boolean;
  busy: boolean;
  onToggle: (node: FolderNode) => void;
  onSelectAll: () => void;
  onSelectNone: () => void;
}) {
  const [filter, setFilter] = React.useState("");
  /**
   * 已展开的目录路径集合。
   *
   * **初值空集 = 全部折叠**，只有第一层可见。这是用户选的行为：
   * 子目录多时一屏铺开根本看不清，而"先看有几个子目录、再决定展开哪个"
   * 是更自然的浏览方式。
   */
  const [expanded, setExpanded] = React.useState<Set<string>>(new Set());
  /** 折叠 / 展开全部。用一个递增的序号触发，避免与用户逐个展开的状态打架。 */
  const [bulk, setBulk] = React.useState<{ seq: number; open: boolean } | null>(null);

  // `nodes` 是一个**按前序遍历排好的扁平列表**（后端就是这么生成的），
  // 因此第 i 个节点的子节点紧跟在它后面、且 depth 恰好多 1。
  // 利用这个性质可以 O(n) 建出父子关系，不需要额外的 id 字段。
  const parentOf = React.useMemo(() => {
    const map = new Map<string, string | null>();
    const stack: FolderNode[] = [];
    for (const node of nodes ?? []) {
      while (stack.length > 0 && stack[stack.length - 1].depth >= node.depth) {
        stack.pop();
      }
      map.set(node.path, stack.length > 0 ? stack[stack.length - 1].path : null);
      stack.push(node);
    }
    return map;
  }, [nodes]);

  const hasChildren = React.useMemo(() => {
    const withKids = new Set<string>();
    for (const parent of parentOf.values()) {
      if (parent) withKids.add(parent);
    }
    return withKids;
  }, [parentOf]);

  // 批量展开/折叠：在状态更新之后跑，确保用的是最新的 expanded。
  React.useEffect(() => {
    if (!bulk || !nodes) return;
    if (bulk.open) {
      // 展开全部——但只展开**有子目录的**，避免往集合里塞一堆没用的路径。
      setExpanded(new Set(Array.from(hasChildren)));
    } else {
      setExpanded(new Set());
    }
    setBulk(null);
    // 只在 seq 变化时跑（open 只是当次的目标值）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bulk?.seq]);

  if (loading) {
    return React.createElement("div", { className: "wwp-note" }, `正在读取目录结构：${root}`);
  }
  if (error) {
    return React.createElement("div", { className: "wwp-error" }, `读取目录结构失败：${error}`);
  }
  if (!nodes) return null;
  if (nodes.length === 0) {
    return React.createElement("div", { className: "wwp-note" }, "这个目录下没有子目录（图片都在根目录里）。");
  }

  const keyword = filter.trim().toLowerCase();
  // 筛选命中时改成**扁平列出**（并显示完整相对路径）。
  //
  // 理由是可用性：在折叠状态下筛选，命中项可能藏在没展开的父目录下，
  // 用户会以为"筛不到"。扁平列出虽然丢了层级感，但至少能看到全部命中项，
  // 而完整路径足以让他判断那是什么。
  const matches = keyword
    ? nodes.filter((n) => n.name.toLowerCase().includes(keyword))
    : null;

  // 折叠模式的可见性：某个节点的父链上每一级都展开着才可见。
  const isVisible = (node: FolderNode): boolean => {
    let parent = parentOf.get(node.path) ?? null;
    while (parent) {
      if (!expanded.has(parent)) return false;
      parent = parentOf.get(parent) ?? null;
    }
    return true;
  };

  const shown = matches ?? nodes.filter(isVisible);
  const excluded = nodes.filter((n) => !n.included).length;
  const anyExpanded = expanded.size > 0;

  return React.createElement(
    "div",
    { className: "wwp-tree" },
    React.createElement(
      "div",
      { className: "wwp-tree-head" },
      React.createElement("span", { className: "wwp-dim" }, `根目录：${root}`),
      React.createElement("input", {
        className: "wwp-input wwp-input-sm",
        placeholder: "筛选目录名",
        value: filter,
        onChange: (e: React.ChangeEvent<HTMLInputElement>) => setFilter(e.target.value),
      }),
    ),
    React.createElement(
      "div",
      { className: "wwp-tree-actions" },
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy || anyExpanded,
          onClick: () => setBulk({ seq: Date.now(), open: true }),
          title: TIPS.treeExpandAll,
        },
        "展开全部",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy || !anyExpanded,
          onClick: () => setBulk({ seq: Date.now(), open: false }),
          title: TIPS.treeCollapseAll,
        },
        "折叠全部",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy || excluded === 0,
          onClick: onSelectAll,
          title: TIPS.treeSelectAll,
        },
        "全选",
      ),
      React.createElement(
        "button",
        {
          className: "wwp-btn wwp-btn-quiet",
          disabled: busy || excluded === nodes.length,
          onClick: onSelectNone,
          title: TIPS.treeSelectNone,
        },
        "全不选",
      ),
    ),
    React.createElement(
      "div",
      { className: "wwp-dim", style: { marginBottom: 6 } },
      matches
        ? `筛选「${filter.trim()}」命中 ${matches.length} 个目录（显示完整路径）`
        : `${nodes.length} 个目录，其中 ${excluded} 个不参与；取消勾选一个目录会连它的子目录一起取消。`,
    ),
    React.createElement(
      "div",
      { className: "wwp-tree-body" },
      shown.map((node) =>
        React.createElement(FolderRow, {
          key: node.path,
          node,
          busy,
          // 筛选时是扁平列表，折叠按钮没有意义（层级已经打平）。
          collapsible: matches ? false : hasChildren.has(node.path),
          open: expanded.has(node.path),
          flat: !!matches,
          root,
          onToggleExpand: () =>
            setExpanded((prev) => {
              const next = new Set(prev);
              if (next.has(node.path)) next.delete(node.path);
              else next.add(node.path);
              return next;
            }),
          onToggleIncluded: () => onToggle(node),
        }),
      ),
    ),
  );
}

function FolderRow({
  node,
  busy,
  collapsible,
  open,
  flat,
  root,
  onToggleExpand,
  onToggleIncluded,
}: {
  node: FolderNode;
  busy: boolean;
  /** 它有子目录（才有箭头可点）。 */
  collapsible: boolean;
  /** 子目录当前是否展开。 */
  open: boolean;
  /** 当前是"筛选后的扁平列表"，此时不缩进、显示完整路径。 */
  flat: boolean;
  root: string;
  onToggleExpand: () => void;
  onToggleIncluded: () => void;
}) {
  // 缩进用 depth，每层 16px。筛选的扁平列表里不缩进——那时层级已经被打平，
  // 再缩进只会让行看起来歪歪扭扭。
  const indent = flat ? 4 : 4 + node.depth * 16;

  // 筛选时显示相对根的路径，让用户知道这一项到底在哪一层。
  const label = flat ? node.path.replace(root, "").replace(/^[\\/]/, "") : node.name;

  return React.createElement(
    "div",
    {
      className: node.included ? "wwp-tree-row" : "wwp-tree-row wwp-tree-row-off",
      style: { paddingLeft: indent },
    },
    // 折叠箭头。**必须是独立的按钮而不是整行的一部分**：整行已经被
    // `<label>` + 复选框占用了（点那里是切换"参与轮播"），
    // 两种操作不能共用一个点击目标。
    collapsible
      ? React.createElement(
          "button",
          {
            className: "wwp-tree-arrow",
            disabled: busy,
            onClick: (e: React.MouseEvent) => {
              // 阻止冒泡，避免顺带触发外层 label 的切换。
              e.preventDefault();
              e.stopPropagation();
              onToggleExpand();
            },
            title: open ? TIPS.treeArrowOpen : TIPS.treeArrowClosed,
            "aria-expanded": open,
          },
          open ? "▾" : "▸",
        )
      : React.createElement("span", { className: "wwp-tree-arrow wwp-tree-arrow-empty" }),

    React.createElement("input", {
      type: "checkbox",
      checked: node.included,
      disabled: busy,
      onChange: onToggleIncluded,
      title: node.excludedHere
        ? TIPS.treeCheckboxExcluded
        : node.included
          ? TIPS.treeCheckboxIncluded
          : TIPS.treeCheckboxByParent,
    }),
    React.createElement("span", { className: "wwp-tree-name", title: node.path }, label),
    React.createElement("span", { className: "wwp-dim wwp-tree-count" }, `${node.imageCount} 张`),
    !node.included && !node.excludedHere
      ? React.createElement("span", { className: "wwp-tag" }, "随上级")
      : null,
  );
}

//! 软件自己的回收站。
//!
//! ## 为什么要有它（用户 2026-10 的要求）
//!
//! 播放页上有一个「删除当前壁纸」按钮：看到不想要的那张图时一键删掉，
//! 并立刻切到下一张。但用户**不想让它真的消失**，原话是：
//!
//! > 不要放到系统回收站，也不要彻底删除，而是放到软件的数据文件夹里面，
//! > 可以新建一个软件的回收站。
//!
//! 他随后提出了一个更具体的要求（也是这个模块现在的样子）：
//!
//! > 可以把回收站单独列一个子页，该页面以相册形式展示被程序删除的图片，
//! > 在图片下面有两个按钮，一个是恢复，一个是删除。恢复加绿色底色，
//! > 删除加红色底色。**恢复的话就把这个图像放到原来的位置**，删除则直接放到
//! > 系统回收站。同时在该页顶部也保留一个全部删除的按钮。
//!
//! 「放回原来的位置」意味着**必须记住它原来在哪**——这就是下面这个目录结构
//! 存在的原因。
//!
//! ## 磁盘结构（每个条目一个目录）
//!
//! ```text
//! data/trash/
//!   1759751415-008.jpg/        ← 条目标识（id）＝ 目录名：删除时间 + 原文件名
//!     origin.json              ← 原位置、原名、删除时间、字节数
//!     008.jpg                  ← 图片本体（保持原文件名）
//! ```
//!
//! 一个条目一个目录，而不是"一堆文件平铺在回收站根目录"，有三个好处：
//!
//! 1. **原位置有地方记**（`origin.json` 跟着图片走）；
//! 2. **重名不再冲突**：图库里 `01.jpg` 满地都是，旧版得存成 `01 (2).jpg`
//!    并让用户自己去猜哪个是哪个；现在目录名带删除时间，天然唯一；
//! 3. 用户在资源管理器里搬动/删掉一个条目时，是**整个目录**一起动，
//!    不会留下一个孤零零的来历文件。
//!
//! 兼容：1.3.0 删掉的条目是**平铺在回收站根目录下的文件**（那时候还没有
//! 相册页）。它们照样会被列出来，只是 `original` 是 `None`：界面上的「恢复」
//! 会禁用它并说明原因，而「删除」照常可用。
//!
//! ## 两级缓冲，任何一步都不会让图**不可恢复地**消失
//!
//! | 动作 | 落到哪里 | 还能还原吗 |
//! |---|---|---|
//! | 「删除当前壁纸」/ 相册里的「删除」 | `data/trash/`（软件回收站，移动） | 能，连原位置都记着 |
//! | 相册里的「恢复」 | 回到**原位置** | —— |
//! | 相册里的「全部删除」 | **系统回收站**（`SHFileOperationW` + `FOF_ALLOWUNDO`） | 能，从桌面回收站还原 |
//! | 用户在系统回收站里再清一次 | 真的没了 | 不能 —— **那一步不归我们** |

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 条目目录名里"原文件名"部分最多保留多少个字符。
///
/// 存在的理由不是磁盘，而是 **Windows 的路径长度**：数据目录本身可能已经很长
/// （`D:\Apps\WindowsWallPaper\data\trash\`），再叠一个几百字符的
/// 文件名就可能撞上 MAX_PATH。完整名字记在 `origin.json` 里，一点都没丢。
const ITEM_NAME_MAX_CHARS: usize = 48;

/// 回收站里现在的规模（界面上显示"回收站里有 N 项 / X MB"）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrashStats {
    /// 条目数（一项 = 一张被删掉的图）。
    pub count: u64,
    /// 合计字节数。
    pub bytes: u64,
}

/// 「全部删除」的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscardReport {
    /// 成功交给系统回收站的条目数。
    pub moved: u64,
    /// 没能交出去的条目数。
    pub failed: u64,
}

/// 一个回收站条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashItem {
    /// 条目标识：目录名（旧版平铺条目就是文件名）。界面用它指代这一项。
    pub id: String,
    /// 图片本体。
    pub file: PathBuf,
    /// 条目目录。旧版平铺条目的"目录"就是回收站根目录（`packaged` 为 false）。
    pub dir: PathBuf,
    /// 是不是新格式（一个目录一个条目）。旧版平铺条目为 false。
    pub packaged: bool,
    /// 原始文件名。
    pub name: String,
    /// 删除前的完整路径。**旧版条目没有记录，为 `None`**。
    pub original: Option<String>,
    /// 删除时间（Unix 秒）。旧版条目用文件修改时间兜底。
    pub deleted_at: i64,
    /// 文件大小（字节）。
    pub bytes: u64,
    /// 原位置现在是不是已经有一个同名文件（恢复时会另存一个名字）。
    pub original_exists: bool,
}

impl TrashItem {
    /// 能不能恢复（旧版条目不知道原位置，因此不能）。
    pub fn can_restore(&self) -> bool {
        self.original.is_some()
    }
}

/// `origin.json` 的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Origin {
    /// 删除前的完整路径。
    original: String,
    /// 原始文件名。
    name: String,
    /// 删除时间（Unix 秒）。
    deleted_at: i64,
    /// 文件大小。
    #[serde(default)]
    bytes: u64,
}

/// 恢复的结果。
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// 最终落到哪里（原位置被占用时会是另存的那个名字）。
    pub target: PathBuf,
    /// 是不是因为原位置已被占用而改了名字。
    pub renamed: bool,
    /// 原本要放回去的位置。
    pub original: String,
}

/// 软件的回收站：数据目录下的一个文件夹。
#[derive(Debug, Clone)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    /// `root` 通常是 `data/trash`。
    pub fn new(root: impl Into<PathBuf>) -> Trash {
        Trash { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 保证目录存在。界面要"打开回收站"时也走它（否则资源管理器会报找不到）。
    pub fn ensure(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("创建回收站目录失败 {}：{e}", self.root.display()))
    }

    /// 把一个文件移进回收站，返回这一条目。
    ///
    /// ## 顺序是刻意的
    ///
    /// 先建目录、再写 `origin.json`、最后才移动文件。这样一来：
    /// 中途失败时留下的是一个"看得出来是什么"的半成品目录（并且会被清理掉），
    /// 而不是"一张图片在回收站里、但没人知道它原来在哪"。
    pub fn store(&self, source: &Path) -> Result<TrashItem, String> {
        if !source.is_file() {
            return Err(format!("文件不存在或不是文件：{}", source.display()));
        }
        self.ensure()?;

        let name = file_name_of(source);
        let bytes = fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        let deleted_at = unix_now();
        let dir = self.unique_item_dir(&name, deleted_at);
        let origin = Origin {
            original: source.display().to_string(),
            name: name.clone(),
            deleted_at,
            bytes,
        };

        if let Err(error) = self.write_origin(&dir, &origin) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }

        let destination = dir.join(&name);
        if let Err(error) = move_file(source, &destination) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }

        log::info!(
            "已把壁纸移进软件回收站：{} -> {}（原位置已记在 origin.json）",
            source.display(),
            destination.display()
        );
        Ok(TrashItem {
            id: dir_name(&dir),
            file: destination,
            packaged: true,
            name,
            original: Some(origin.original),
            deleted_at,
            bytes,
            original_exists: false,
            dir,
        })
    }

    /// 列出回收站里的全部条目，**新的在前**。
    pub fn items(&self) -> Vec<TrashItem> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if let Some(item) = self.read_packaged_item(&path) {
                    items.push(item);
                }
            } else if meta.is_file() {
                // 旧版（1.3.0）平铺在回收站根目录下的文件：没有原位置记录。
                let name = file_name_of(&path);
                if name == ORIGIN_FILE {
                    continue;
                }
                items.push(TrashItem {
                    id: name.clone(),
                    file: path,
                    dir: self.root.clone(),
                    packaged: false,
                    name,
                    original: None,
                    deleted_at: meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                    bytes: meta.len(),
                    original_exists: false,
                });
            }
        }
        // 新的在前；同一秒删的两张用 id 兜底排序，保证顺序稳定。
        items.sort_by(|a, b| {
            b.deleted_at
                .cmp(&a.deleted_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        items
    }

    /// 按 id 找一个条目。**找不到就是找不到**——界面传来的字符串不会被当成路径用，
    /// 因此不存在"传一个 `..\..` 就能删别处文件"这种事。
    pub fn find(&self, id: &str) -> Option<TrashItem> {
        self.items().into_iter().find(|item| item.id == id)
    }

    /// 把一个条目**放回原位置**。
    ///
    /// - 原来的目录不在了（用户把图库改名/移走了）→ **建出来**。用户说的是
    ///   "恢复到原来的位置"，那就照做；建不出来（盘没插上）会明确报错。
    /// - 原位置已经有一个同名文件 → **绝不覆盖**，改成 `名字 (恢复).jpg`
    ///   放在旁边，并在结果里说明。
    pub fn restore(&self, id: &str) -> Result<RestoreOutcome, String> {
        let item = self
            .find(id)
            .ok_or_else(|| format!("回收站里没有这一项：{id}"))?;
        let Some(original) = item.original.clone() else {
            return Err(
                "这一项是旧版删掉的，没有记录它原来在哪——只能「删除」（送系统回收站），\
                 或者到 data\\trash 里手动取回"
                    .to_string(),
            );
        };

        let wanted = PathBuf::from(&original);
        let (target, renamed) = if wanted.exists() {
            (available_sibling(&wanted), true)
        } else {
            (wanted.clone(), false)
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!("原目录建不出来（盘没插上？）：{}：{e}", parent.display())
            })?;
        }

        move_file(&item.file, &target)?;
        // 条目目录连同 origin.json 一起收掉（旧版平铺条目没有目录可收）。
        if item.packaged {
            if let Err(error) = fs::remove_dir_all(&item.dir) {
                log::warn!("恢复后没能清掉回收站条目目录 {}：{error}", item.dir.display());
            }
        }
        self.forget_thumbnail(id);

        log::info!(
            "已从回收站恢复到原位置：{} -> {}{}",
            item.file.display(),
            target.display(),
            if renamed { "（原位置已被占用，改用了新名字）" } else { "" }
        );
        Ok(RestoreOutcome {
            target,
            renamed,
            original,
        })
    }

    /// 把一个条目交给**系统回收站**（仍然可以还原）。
    pub fn discard(&self, id: &str) -> Result<(), String> {
        let item = self
            .find(id)
            .ok_or_else(|| format!("回收站里没有这一项：{id}"))?;
        // 新格式送**整个条目目录**（连带 origin.json 一起走）；旧格式只有那个文件。
        let subject = if item.packaged { &item.dir } else { &item.file };
        send_to_system_recycle_bin(subject)?;
        self.forget_thumbnail(id);
        Ok(())
    }

    /// 全部删除：每一样东西都送进系统回收站。
    ///
    /// 逐条调用而不是一次传一整批：`SHFileOperationW` 整批失败时说不清是哪一条
    /// 出的问题，而用户需要知道"有几项没能交出去"。
    pub fn discard_all(&self) -> Result<DiscardReport, String> {
        self.discard_all_with(&mut |_done, _total| {})
    }

    /// 全部删除，并且**每处理完一项回调一次** `on_progress(已完成, 总数)`。
    ///
    /// ## 为什么进度要由这里回调出去
    ///
    /// 删除几千张图要好几秒。原先这件事跑在界面的主线程上，于是"点了没反应"，
    /// 用户以为程序卡死（这正是加进度条的原因）。要把循环挪到工作线程上，
    /// 就得有人把"删到第几张了"从循环内部带出来——**只有循环内部知道这个数**，
    /// 调用方在另一个线程上，既看不到 items，也没法自己数。
    ///
    /// 回调按"已处理"计数（成功的、失败的都算），因为进度条要回答的是
    /// "还剩多少没弄完"，而不是"成功了多少"——后者由返回值 `DiscardReport` 说。
    pub fn discard_all_with(
        &self,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<DiscardReport, String> {
        let items = self.items();
        let total = items.len() as u64;
        let mut report = DiscardReport::default();
        for (index, item) in items.into_iter().enumerate() {
            let subject = if item.packaged { &item.dir } else { &item.file };
            match send_to_system_recycle_bin(subject) {
                Ok(()) => {
                    report.moved += 1;
                    self.forget_thumbnail(&item.id);
                }
                Err(error) => {
                    report.failed += 1;
                    log::warn!("送进系统回收站失败：{}：{error}", subject.display());
                }
            }
            on_progress(index as u64 + 1, total);
        }
        log::info!(
            "回收站已全部删除：{} 项已送进系统回收站，{} 项失败",
            report.moved,
            report.failed
        );
        Ok(report)
    }

    /// 回收站里现在有多少东西。
    pub fn stats(&self) -> TrashStats {
        let items = self.items();
        TrashStats {
            count: items.len() as u64,
            bytes: items.iter().map(|item| item.bytes).sum(),
        }
    }

    /// 某一条的缩略图，编码成可以直接塞进 `<img src>` 的 data URL。
    ///
    /// ## 为什么是后端做缩略图，而不是让页面直接加载原图
    ///
    /// 图库里很多是 30 MP 的照片。相册一页要显示十几张，让 WebView 自己去解码
    /// 原图会吃掉大量内存与时间；而它们已经被压缩成 360 px 的 JPEG 之后，
    /// 每张只有几十 KB。生成结果缓存在 `data/trash-thumbs/`，第二次打开是瞬时的。
    ///
    /// 压图与缓存那部分在 [`crate::thumb`] 里（播放页的壁纸预览用的是同一份）。
    /// 缓存键用 id 的哈希（`library_id` 那套 FNV-1a）：id 里可能有中文、
    /// 空格、点号这类字符，直接当文件名不安全，而哈希之后是 16 个十六进制字符。
    /// 回收站的 id 本身带删除时间戳，**同一个 id 永远是同一份内容**，
    /// 因此这里不需要再比时间戳（那是给"路径复用"的壁纸缓存用的，见 [`crate::thumb`]）。
    pub fn thumbnail_data_url(&self, cache_dir: &Path, id: &str) -> Result<String, String> {
        let item = self
            .find(id)
            .ok_or_else(|| format!("回收站里没有这一项：{id}"))?;
        crate::thumb::data_url(&item.file, cache_dir, &thumbnail_key(id))
    }

    /// 条目没了就把它的缩略图缓存删掉（几 KB，但没必要留着）。
    fn forget_thumbnail(&self, id: &str) {
        // 缓存目录是数据目录下的 `trash-thumbs`，而回收站根目录是 `trash`。
        let Some(data_dir) = self.root.parent() else {
            return;
        };
        let cache = data_dir
            .join("trash-thumbs")
            .join(format!("{}.jpg", thumbnail_key(id)));
        if cache.is_file() {
            if let Err(error) = fs::remove_file(&cache) {
                log::debug!("删缩略图缓存失败 {}：{error}", cache.display());
            }
        }
    }

    /// 在回收站根目录下为这一项找一个不冲突的目录名。
    fn unique_item_dir(&self, name: &str, stamp: i64) -> PathBuf {
        let short = shorten(name, ITEM_NAME_MAX_CHARS);
        let base = format!("{stamp}-{short}");
        let mut candidate = self.root.join(&base);
        let mut n = 2u32;
        while candidate.exists() {
            candidate = self.root.join(format!("{base}-{n}"));
            n += 1;
        }
        candidate
    }

    fn write_origin(&self, dir: &Path, origin: &Origin) -> Result<(), String> {
        fs::create_dir_all(dir)
            .map_err(|e| format!("创建回收站条目目录失败 {}：{e}", dir.display()))?;
        let text = serde_json::to_string_pretty(origin)
            .map_err(|e| format!("序列化 origin.json 失败：{e}"))?;
        let path = dir.join(ORIGIN_FILE);
        fs::write(&path, text).map_err(|e| format!("写 origin.json 失败 {}：{e}", path.display()))
    }

    /// 读一个"新格式"条目目录；不像条目（没有图片本体）就返回 `None`。
    fn read_packaged_item(&self, dir: &Path) -> Option<TrashItem> {
        let origin = fs::read_to_string(dir.join(ORIGIN_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Origin>(&text).ok());

        // 本体优先按 origin.json 里记的名字找；找不到（用户手工改过名）就退化成
        // "目录里第一个不是 origin.json 的文件"。
        let file = origin
            .as_ref()
            .map(|o| dir.join(&o.name))
            .filter(|p| p.is_file())
            .or_else(|| {
                fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
                    p.is_file()
                        && p.file_name()
                            .map(|n| n.to_string_lossy() != ORIGIN_FILE)
                            .unwrap_or(false)
                })
            })?;

        let meta = fs::metadata(&file).ok()?;
        let name = origin
            .as_ref()
            .map(|o| o.name.clone())
            .unwrap_or_else(|| file_name_of(&file));
        let original = origin.as_ref().map(|o| o.original.clone());
        let deleted_at = origin
            .as_ref()
            .map(|o| o.deleted_at)
            .filter(|stamp| *stamp > 0)
            .or_else(|| {
                meta.modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
            })
            .unwrap_or(0);

        Some(TrashItem {
            id: dir_name(dir),
            file,
            dir: dir.to_path_buf(),
            packaged: true,
            name,
            original_exists: original
                .as_ref()
                .map(|path| Path::new(path).exists())
                .unwrap_or(false),
            original,
            deleted_at,
            bytes: meta.len(),
        })
    }
}

/// 条目目录里的来历文件。
const ORIGIN_FILE: &str = "origin.json";

/// 取路径最后一段作为名字（拿不到时给一个兜底值，不让调用方去处理 `Option`）。
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "未命名".to_string())
}

/// 取目录最后一段作为条目 id。
fn dir_name(dir: &Path) -> String {
    file_name_of(dir)
}

/// 按**字符**截断（不是字节）：中文文件名按字节截会把一个字切成两半。
fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 移动一个文件：同一个盘上是改名（瞬间完成）；跨盘退化成复制 + 删除。
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            // 跨盘时 `rename` 报 ERROR_NOT_SAME_DEVICE（os error 17）：
            // 那种情况下会真的多占一份磁盘，因此单独记一条 warn。
            log::warn!(
                "移动文件失败（{rename_error}），改用复制 + 删除：{} -> {}",
                from.display(),
                to.display()
            );
            fs::copy(from, to).map_err(|e| format!("复制到 {} 失败：{e}", to.display()))?;
            if let Err(error) = fs::remove_file(from) {
                log::warn!(
                    "已复制到 {}，但原文件删不掉（就地保留一份）：{}：{error}",
                    to.display(),
                    from.display()
                );
            }
            Ok(())
        }
    }
}

/// 原位置被占用时，找一个"名字 (恢复).jpg"式的可用邻居路径。
fn available_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "恢复".to_string());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1..10_000u32 {
        let name = if n == 1 {
            format!("{stem} (恢复){extension}")
        } else {
            format!("{stem} (恢复 {n}){extension}")
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} (恢复 {}){extension}", unix_now()))
}

/// 缩略图缓存的文件名（id 的哈希，见 `thumbnail_data_url` 的说明）。
fn thumbnail_key(id: &str) -> String {
    crate::library::index::library_id(Path::new(id))
}

/// 把一个文件/目录交给 Windows 回收站。
///
/// `FOF_ALLOWUNDO` 就是"可还原"的开关：带上它，东西进回收站而不是被删掉。
/// 另外三个标志都是为了**不弹窗**：这是后台动作，弹一个"确认删除"的框
/// 会让界面看起来卡住（而用户已经在界面上点过了）。
#[cfg(windows)]
fn send_to_system_recycle_bin(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, SHFILEOPSTRUCTW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI,
        FOF_SILENT, FO_DELETE,
    };

    // shell 的文件操作要跑在 STA 线程上。**调用方可能在任何线程**：
    // 单条删除仍在主线程（WebView2 早就把它初始化成 STA 了），
    // 而"全部删除"为了不冻住界面挪到了工作线程——那个线程没有任何 COM 单元。
    let _apartment = StaApartment::initialize();

    if !path.exists() {
        return Err(format!("它已经不在了：{}", path.display()));
    }
    // 绝对路径：`SHFileOperationW` 不接受相对路径。
    let absolute = std::path::absolute(path).map_err(|e| format!("无法取绝对路径：{e}"))?;
    let mut from: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    // 路径列表必须以**两个** 0 结尾。
    from.push(0);
    from.push(0);

    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(from.as_ptr()),
        // `FILEOPERATION_FLAGS` 是 newtype，结构体字段要的是裸 `u16`。
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };

    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 {
        return Err(format!("系统回收站返回错误码 {code}"));
    }
    if operation.fAnyOperationsAborted.as_bool() {
        return Err("操作被中止（可能被系统策略或磁盘配额拦下）".to_string());
    }
    Ok(())
}

/// 线程的 COM 单元（**STA**），只在需要时配对释放。
///
/// ## 为什么单独写一个，而不复用 `com::ComApartment`
///
/// `com::ComApartment` 初始化的是 **MTA**（媒体解码那类接口要的），
/// 而 `SHFileOperationW` 属于 shell，文档要求它在 **STA** 线程上调用。
/// 两者不能互相替代，也不能对同一个线程先后换模式（第二次调用会返回
/// `RPC_E_CHANGED_MODE`）。
///
/// 单元是按线程的，`CoInitializeEx` 是幂等调用：
///   - `S_OK(0)`：这次是**我们**初始化的，退出时必须配对 `CoUninitialize`；
///   - `S_FALSE(1)`：本线程已经是同模式的单元，引用计数加一，**同样要配对释放**；
///   - 其余（如 `RPC_E_CHANGED_MODE`）：本线程已经是别的单元，我们什么都没有改，
///     也就什么都不该释放——这时沿用改造前的行为（主线程正是这种情形）。
#[cfg(windows)]
struct StaApartment {
    needs_uninitialize: bool,
}

#[cfg(windows)]
impl StaApartment {
    fn initialize() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: 对已初始化/未初始化的线程都是安全的幂等调用；
        // 返回值决定要不要配对 CoUninitialize（见上面的三条）。
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        match result.0 {
            0 | 1 => Self {
                needs_uninitialize: true,
            },
            _ => Self {
                needs_uninitialize: false,
            },
        }
    }
}

#[cfg(windows)]
impl Drop for StaApartment {
    fn drop(&mut self) {
        if self.needs_uninitialize {
            // SAFETY: 只在本线程、且只在本结构体持有引用计数时调用一次。
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// 非 Windows 平台上没有系统回收站接口。
///
/// 本项目只面向 Windows，这个分支存在的唯一目的是让 `cargo check` 在别的平台
/// 上不至于因为缺函数而失败（而不是暗示这里支持别的平台）。
#[cfg(not(windows))]
fn send_to_system_recycle_bin(_path: &Path) -> Result<(), String> {
    Err("当前平台没有系统回收站接口".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-trash-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 造一张真的小 PNG（缩略图那条路要它）。
    fn write_png(path: &Path, width: u32, height: u32) {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        image.save(path).unwrap();
    }

    #[test]
    fn store_moves_the_file_and_remembers_where_it_came_from() {
        let base = temp_dir("move");
        let trash = Trash::new(base.join("trash"));
        let source = base.join("photo.jpg");
        fs::write(&source, b"hello").unwrap();

        let item = trash.store(&source).unwrap();

        assert!(item.packaged, "新格式应当是「一个条目一个目录」");
        assert_eq!(item.name, "photo.jpg");
        assert_eq!(item.original.as_deref(), Some(source.display().to_string().as_str()));
        assert_eq!(item.bytes, 5);
        // 本体在条目目录里，原名不变。
        assert_eq!(item.file, item.dir.join("photo.jpg"));
        assert!(item.file.is_file());
        assert!(!source.exists(), "原位置不该再有它（移动而不是复制）");
        // 来历文件里能读到原位置——**这就是"以后找得回来"的依据**。
        let origin: Origin =
            serde_json::from_str(&fs::read_to_string(item.dir.join(ORIGIN_FILE)).unwrap()).unwrap();
        assert_eq!(origin.original, source.display().to_string());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn two_files_with_the_same_name_get_their_own_directories() {
        // 旧版靠 `01 (2).jpg` 这种改名来去重，用户根本分不清哪个是哪个；
        // 现在每个条目一个目录，名字还各自是原来的名字。
        let base = temp_dir("dupe");
        let trash = Trash::new(base.join("trash"));
        fs::create_dir_all(base.join("sub1")).unwrap();
        fs::create_dir_all(base.join("sub2")).unwrap();
        fs::write(base.join("sub1/01.jpg"), b"first").unwrap();
        fs::write(base.join("sub2/01.jpg"), b"second").unwrap();

        let a = trash.store(&base.join("sub1/01.jpg")).unwrap();
        let b = trash.store(&base.join("sub2/01.jpg")).unwrap();

        assert_ne!(a.id, b.id, "同一秒删的同名文件也要是两个条目");
        assert_eq!(a.name, "01.jpg");
        assert_eq!(b.name, "01.jpg");
        assert_eq!(fs::read(&a.file).unwrap(), b"first");
        assert_eq!(fs::read(&b.file).unwrap(), b"second");
        assert_eq!(trash.items().len(), 2);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_puts_the_file_back_where_it_was() {
        let base = temp_dir("restore");
        let trash = Trash::new(base.join("trash"));
        let source = base.join("pics/photo.jpg");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"hello").unwrap();

        let item = trash.store(&source).unwrap();
        assert!(!trash.items()[0].original_exists);

        let outcome = trash.restore(&item.id).unwrap();
        assert_eq!(outcome.target, source);
        assert!(!outcome.renamed);
        assert_eq!(fs::read(&source).unwrap(), b"hello", "内容要一字不差");
        assert!(trash.items().is_empty(), "恢复之后回收站里不该还有它");
        assert!(!item.dir.exists(), "条目目录连同 origin.json 一起收掉");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restoring_recreates_the_original_folder_if_it_is_gone() {
        // 用户把图库整个改名/搬走了，恢复时应当把目录树建出来，
        // 而不是报一句"目录不存在"让他自己想办法。
        let base = temp_dir("recreate");
        let trash = Trash::new(base.join("trash"));
        let source = base.join("gone/photos/a.jpg");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"x").unwrap();
        let item = trash.store(&source).unwrap();
        fs::remove_dir_all(base.join("gone")).unwrap();

        let outcome = trash.restore(&item.id).unwrap();
        assert!(outcome.target.is_file());
        assert_eq!(outcome.target, source);
        assert_eq!(fs::read(&source).unwrap(), b"x");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restoring_never_overwrites_a_file_that_is_already_there() {
        // **这条守的是"绝不覆盖"**：原位置又出现了一个同名文件时，
        // 恢复要另存一个名字，绝不能把人家现在那张顶掉。
        let base = temp_dir("occupied");
        let trash = Trash::new(base.join("trash"));
        let source = base.join("a.jpg");
        fs::write(&source, b"deleted").unwrap();
        let item = trash.store(&source).unwrap();
        fs::write(&source, b"current").unwrap();

        assert!(trash.find(&item.id).unwrap().original_exists, "界面要知道原位置被占了");
        let outcome = trash.restore(&item.id).unwrap();

        assert!(outcome.renamed);
        assert_eq!(outcome.target, base.join("a (恢复).jpg"));
        assert_eq!(fs::read(&source).unwrap(), b"current", "原来那张不能被碰");
        assert_eq!(fs::read(&outcome.target).unwrap(), b"deleted");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_flat_files_are_listed_but_cannot_be_restored() {
        // 1.3.0 删掉的条目是**平铺**在回收站根目录下的文件，那时候还没有
        // 相册页、也没有原位置记录。它们必须照样列出来（不能凭空消失），
        // 但「恢复」要禁用并说清原因。
        let base = temp_dir("legacy");
        let trash = Trash::new(base.join("trash"));
        trash.ensure().unwrap();
        fs::write(trash.root().join("old.jpg"), b"legacy").unwrap();

        let items = trash.items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "old.jpg");
        assert!(!items[0].packaged);
        assert_eq!(items[0].original, None);
        assert!(!items[0].can_restore());
        assert!(trash.restore("old.jpg").is_err(), "没有原位置就恢复不了");

        let stats = trash.stats();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.bytes, 6);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn items_are_listed_newest_first() {
        let base = temp_dir("order");
        let trash = Trash::new(base.join("trash"));
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            fs::write(base.join(name), b"x").unwrap();
            trash.store(&base.join(name)).unwrap();
        }
        let items = trash.items();
        assert_eq!(items.len(), 3);
        // 同一秒删的：id 以同一个时间戳开头，因此按 id 的字典序稳定排序。
        assert!(items.windows(2).all(|pair| pair[0].deleted_at >= pair[1].deleted_at));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unknown_ids_are_rejected_instead_of_touching_other_paths() {
        // 界面传来的 id 只会在**回收站自己扫出来的**条目里找。传一个路径
        // （哪怕是 `..\..\config\settings.toml`）也只会得到"找不到"。
        let base = temp_dir("ids");
        let trash = Trash::new(base.join("trash"));
        trash.ensure().unwrap();
        assert!(trash.find("..\\..\\config\\settings.toml").is_none());
        assert!(trash.restore("..\\..\\config\\settings.toml").is_err());
        assert!(trash.discard("不存在的条目").is_err());
        assert!(trash.thumbnail_data_url(&base.join("thumbs"), "../x").is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn storing_a_missing_file_is_an_error_not_a_silent_success() {
        let base = temp_dir("missing");
        let trash = Trash::new(base.join("trash"));
        let error = trash.store(&base.join("nope.jpg")).unwrap_err();
        assert!(error.contains("不存在"), "要说清为什么失败：{error}");
        assert!(trash.items().is_empty(), "失败不该留下半个条目");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_very_long_file_name_is_shortened_for_the_directory() {
        // 目录名里塞一个几百字符的文件名会撞上 Windows 的路径长度限制；
        // 完整名字在 origin.json 里，一点没丢。
        let base = temp_dir("longname");
        let trash = Trash::new(base.join("trash"));
        let long = format!("{}.jpg", "很长的名字".repeat(40));
        let source = base.join(&long);
        fs::write(&source, b"x").unwrap();

        let item = trash.store(&source).unwrap();
        // id 是"时间戳-截断后的名字"，因此名字那一段必须被截到上限附近。
        let name_part = item.id.split_once('-').map(|(_, rest)| rest).unwrap_or("");
        assert!(
            name_part.chars().count() <= ITEM_NAME_MAX_CHARS,
            "名字那一段要短下来：{name_part}"
        );
        assert!(
            item.id.chars().count() < long.chars().count(),
            "整个 id 也要比原名短得多"
        );
        assert_eq!(item.name, long, "但显示名仍然是完整原名");
        assert!(item.dir.is_dir());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn thumbnails_are_generated_cached_and_reused() {
        let base = temp_dir("thumb");
        let trash = Trash::new(base.join("trash"));
        let cache = base.join("thumbs");
        let source = base.join("shot.png");
        write_png(&source, 64, 48);
        let item = trash.store(&source).unwrap();

        let url = trash.thumbnail_data_url(&cache, &item.id).unwrap();
        assert!(
            url.starts_with("data:image/jpeg;base64,"),
            "要能直接塞进 img src：{}",
            &url[..40.min(url.len())]
        );
        assert!(url.len() > 100, "生成的 JPEG 不该是空的");
        let cached = cache.join(format!("{}.jpg", thumbnail_key(&item.id)));
        assert!(cached.is_file(), "第一次生成后应当留下缓存");

        // 第二次走缓存：内容一致（比较 data URL 的长度即可，不必解码）。
        let again = trash.thumbnail_data_url(&cache, &item.id).unwrap();
        assert_eq!(url.len(), again.len());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_broken_image_reports_a_reason_instead_of_a_blank_thumbnail() {
        let base = temp_dir("broken");
        let trash = Trash::new(base.join("trash"));
        let source = base.join("not-a-real.jpg");
        fs::write(&source, b"this is not an image at all").unwrap();
        let item = trash.store(&source).unwrap();

        let error = trash
            .thumbnail_data_url(&base.join("thumbs"), &item.id)
            .unwrap_err();
        assert!(
            error.contains("尺寸") || error.contains("打不开"),
            "要说清是这张图的问题：{error}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn stats_count_items_and_bytes() {
        let base = temp_dir("stats");
        let trash = Trash::new(base.join("trash"));
        assert_eq!(trash.stats(), TrashStats::default(), "还没有目录时算空的");
        fs::write(base.join("a.jpg"), b"12345").unwrap();
        fs::write(base.join("b.jpg"), b"123").unwrap();
        trash.store(&base.join("a.jpg")).unwrap();
        trash.store(&base.join("b.jpg")).unwrap();

        let stats = trash.stats();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.bytes, 8);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_trash_reports_empty() {
        let base = temp_dir("empty");
        let trash = Trash::new(base.join("trash"));
        assert!(trash.items().is_empty());
        assert!(trash.ensure().is_ok(), "打开回收站之前要保证目录存在");
        assert!(trash.root().is_dir());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn shortening_is_by_characters_not_bytes() {
        assert_eq!(shorten("示例素材合集2024", 5), "示例素材合");
        assert_eq!(shorten("abc", 5), "abc");
    }
}

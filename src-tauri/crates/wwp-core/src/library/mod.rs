//! 图库扫描与索引。
//!
//! 需求 9 要求"扫描当前目录及其子目录，构建播放列表并存储到本地，播放时按需加载"，
//! 需求 2 要求"不要把图都读进内存"。因此这里的产出是一个**索引文件**：
//! 每张图一行，含路径、尺寸、修改时间、文件大小与 EXIF 方向。
//!
//! 关键设计（见设计规格 §5）：
//!
//! - **增量重扫**：`mtime + 文件大小` 都没变的文件直接沿用旧记录，不再打开文件。
//!   这是"第二次扫描应该在十秒内完成"的全部秘密——首次扫描要读头部拿尺寸，
//!   之后就只是遍历目录。
//! - **跳过与失败都必须被报告**：读不出内容的文件与没有解码器的格式都会被计数并
//!   列出原因。图库里"少了几张图"如果没有解释，用户只会以为是程序坏了。
//! - **可取消**：图库可能是几万张图、几十分钟的扫描，不能没有中断手段。

pub mod exclude;
pub mod index;
pub mod probe;
pub mod scan;

pub use exclude::{ExcludeMatcher, ExcludedBy};
pub use index::{
    library_id, load as load_index, IndexFile, IndexPaths, IndexStats, LibraryEntry,
    UnreadableSample,
};
pub use probe::{Probe, SUPPORTED_EXTENSIONS, UNSUPPORTED_EXTENSIONS};
pub use scan::{
    scan_library, test_exclusion, ScanOptions, ScanOutcome, ScanProgress,
};

use std::path::Path;

/// 读取某个图库已建立的索引条目。
///
/// 找不到索引目录时需要调用方知道"该扫还没扫"，因此这里返回空列表而不是错误：
/// 播放器对"没有候选"的处理（报告 `NoCandidates`）比"读取失败"更贴近用户能理解的现象。
pub fn load_index_for(library_root: &Path) -> crate::Result<Vec<LibraryEntry>> {
    let index_root = crate::index_root();
    let paths = IndexPaths::new(&index_root, &library_id(library_root));
    if !paths.rows.is_file() {
        return Ok(Vec::new());
    }
    let (_, entries) = load_index(&paths)?;
    Ok(entries)
}

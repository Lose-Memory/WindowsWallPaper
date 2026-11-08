//! WindowsWallPaper 引擎。
//!
//! 这里放所有不依赖窗口的逻辑：便携路径解析、配置读写、图库索引、壁纸呈现。
//! 外壳（Tauri）只通过命令白名单调用本 crate，因此本 crate 永远不能依赖 `tauri`。

#[cfg(windows)]
pub mod audio;
/// 最小 base64 编码（只为把缩略图塞进 `data:` URL，不引入新依赖）。
pub mod base64;
#[cfg(windows)]
pub mod clipboard;
/// 清理程序目录：列出"删掉也不影响使用"的东西，由用户勾选后删除。
/// **绝不动回收站，也不动设置与图库索引**（见模块文档）。
pub mod cleanup;
#[cfg(windows)]
pub mod com;
#[cfg(windows)]
pub mod folder_picker;
pub mod config;
#[cfg(windows)]
pub mod desktop;
#[cfg(windows)]
pub mod desktop_layer;
#[cfg(windows)]
pub mod engine;
#[cfg(windows)]
pub mod environment;
pub mod error;
#[cfg(windows)]
pub mod fullscreen;
#[cfg(windows)]
pub mod hotkey;
#[cfg(windows)]
pub mod hidden_window;
pub mod image_prep;
pub mod info;
pub mod library;
pub mod logfile;
pub mod monitors;
pub mod paths;
/// 交给系统去"打开这个文件"（按扩展名的默认关联程序）。
/// 回收站里点缩略图就用它，与打开**目录**的 `commands::open_folder` 不是一件事。
pub mod shell;
#[cfg(windows)]
pub mod single_instance;
/// 缩略图：把一张图压成小 JPEG 并编码成 `data:` URL。
/// 回收站相册与播放页的壁纸预览（当前 / 下一张）共用这一份实现。
pub mod thumb;
/// 软件自己的回收站：删除壁纸时先把文件移到这里，清空时才交给系统回收站。
pub mod trash;
#[cfg(windows)]
pub mod video;
pub mod video_size;
pub mod playback;
pub mod wallpaper;

pub use config::Settings;
pub use error::{Error, Result};
pub use info::StartupInfo;
pub use paths::{AppPaths, RootSource};

use std::path::PathBuf;
use std::sync::OnceLock;

/// 引擎运行期需要知道"数据放哪里"，但它是外壳决定的（便携模式）。
///
/// 用一个进程内的一次性槽位传递，而不是让每个 API 都多一个路径参数：
/// 引擎内部有十几处需要拼索引路径，把它们全部改成带参会让签名淹没在噪音里。
static INDEX_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 由外壳在启动时调用一次，告诉引擎索引目录在哪。
pub fn set_index_root(path: PathBuf) {
    let _ = INDEX_ROOT.set(path);
}

/// 索引目录。
///
/// 未初始化时退回进程当前目录下的 `data/index`：这只会发生在单元测试里，
/// 而测试本来就该用临时目录，不该依赖这个兜底值做出任何判断。
pub fn index_root() -> PathBuf {
    INDEX_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("data").join("index"))
}

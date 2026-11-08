//! 启动信息：供界面展示"数据到底存在哪里"。
//!
//! 这是 M1 的可见验收面：便携模式是否生效、有没有回退到系统目录，都靠这份信息判断。

use serde::Serialize;

use crate::paths::AppPaths;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupInfo {
    pub version: String,
    /// 编译期标识，便于确认跑的是哪一次构建。
    pub build: String,
    pub root: String,
    pub root_source: String,
    pub fell_back: bool,
    pub fallback_reason: Option<String>,
    pub program_dir: String,
    pub config_dir: String,
    pub data_dir: String,
    /// 软件回收站目录（`data/trash`）。回收站页与诊断页都显示它。
    pub trash_dir: String,
    pub log_dir: String,
    pub webview_data_dir: String,
    pub os: String,
}

impl StartupInfo {
    pub fn from_paths(paths: &AppPaths) -> Self {
        let fell_back = paths.fallback_reason.is_some();
        StartupInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            build: build_label(),
            root: paths.root.display().to_string(),
            root_source: paths.root_source.as_str().to_string(),
            fell_back,
            fallback_reason: paths.fallback_reason.clone(),
            program_dir: paths.program_dir.display().to_string(),
            config_dir: paths.config_dir().display().to_string(),
            data_dir: paths.data_dir().display().to_string(),
            trash_dir: paths.trash_dir().display().to_string(),
            log_dir: paths.log_dir().display().to_string(),
            webview_data_dir: paths.webview_data_dir().display().to_string(),
            os: os_description(),
        }
    }
}

fn build_label() -> String {
    if cfg!(debug_assertions) {
        "debug".to_string()
    } else {
        "release".to_string()
    }
}

/// 系统版本描述。M1 只要求能看清是不是 Win11，详细版本在诊断导出里再补。
fn os_description() -> String {
    let arch = std::env::consts::ARCH;
    format!("Windows ({arch})")
}

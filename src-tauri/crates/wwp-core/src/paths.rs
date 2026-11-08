//! 程序目录与数据目录解析。
//!
//! 交付形态是一个"拷到哪都能跑"的文件夹（设计规格 §3.1），因此所有路径都必须
//! 相对可执行文件解析，并且要在程序目录不可写时能回退到系统目录而不是直接失败。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Error, Result};

/// 覆盖程序根目录的环境变量，便于开发与自动化测试。
pub const HOME_ENV: &str = "WWP_HOME";

/// 数据根目录的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// 程序所在目录（便携模式的正常情况）。
    ProgramDir,
    /// 由 `WWP_HOME` 指定。
    EnvOverride,
    /// 开发态：可执行文件位于 Cargo 的 `target/` 下，数据改放工程目录。
    DevProjectDir,
    /// 程序目录不可写，回退到 `%LOCALAPPDATA%`。
    FallbackLocalAppData,
}

impl RootSource {
    /// 与前端约定的稳定标识串。
    pub fn as_str(self) -> &'static str {
        match self {
            RootSource::ProgramDir => "program-dir",
            RootSource::EnvOverride => "env-override",
            RootSource::DevProjectDir => "dev-project-dir",
            RootSource::FallbackLocalAppData => "fallback-localappdata",
        }
    }
}

/// 程序运行时用到的全部目录，一次解析完成后不再变化。
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// 数据根目录：config / data / logs / webview2 都在它下面。
    pub root: PathBuf,
    pub root_source: RootSource,
    /// 可执行文件所在目录（无论是否回退，都保留真实值便于界面展示）。
    pub program_dir: PathBuf,
    /// 回退原因（未回退时为 `None`）。
    pub fallback_reason: Option<String>,
}

impl AppPaths {
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// WebView2 用户数据目录。
    ///
    /// 必须显式指向数据根目录，否则 WebView2 默认写 `%LOCALAPPDATA%\<bundle-id>\EBWebView`，
    /// 便携性会被破坏。
    pub fn webview_data_dir(&self) -> PathBuf {
        self.root.join("webview2")
    }

    pub fn app_log(&self) -> PathBuf {
        self.log_dir().join("app.log")
    }

    /// 软件自己的回收站（「删除当前壁纸」把文件移到这里）。
    ///
    /// **放在这里而不是外壳里**：界面要在回收站页与诊断页显示这个路径，
    /// 而 `StartupInfo` 是这两个页面共用的那份"数据存在哪里"的来源。
    /// 外壳的 `AppState::trash_dir` 也走这一份，避免同一个目录名有两处写法。
    pub fn trash_dir(&self) -> PathBuf {
        self.data_dir().join("trash")
    }

    pub fn crash_dir(&self) -> PathBuf {
        self.log_dir().join("crash")
    }

    /// 解析路径：优先 `WWP_HOME`，其次程序目录；程序目录不可写则回退系统目录。
    pub fn resolve(program_dir: PathBuf) -> AppPaths {
        AppPaths::resolve_with_override(program_dir, std::env::var_os(HOME_ENV))
    }

    /// 与 [`AppPaths::resolve`] 相同，但把环境变量的值作为参数传入。
    ///
    /// 这样拆开是为了可测试：改进程环境变量是全局副作用，并行跑的测试会互相干扰
    /// （实测出现过"某个测试把 WWP_HOME 指到一个文件上，另一个测试因此读到了
    /// 别人的设置"）。把取值与决策分开，决策部分就能用纯函数测。
    pub fn resolve_with_override(program_dir: PathBuf, home_override: Option<OsString>) -> AppPaths {
        if let Some(value) = home_override {
            if !value.is_empty() {
                let root = PathBuf::from(value);
                if dir_writable(&root) {
                    return AppPaths {
                        root,
                        root_source: RootSource::EnvOverride,
                        program_dir,
                        fallback_reason: None,
                    };
                }
                log::warn!(
                    "{HOME_ENV} 指定的目录不可写，将忽略它并继续判定程序目录：{}",
                    root.display()
                );
            }
        }

        if let Some(project_dir) = cargo_project_dir(&program_dir) {
            if dir_writable(&project_dir) {
                log::info!(
                    "开发态运行（可执行文件在 target 下），数据改放工程目录 {}",
                    project_dir.display()
                );
                return AppPaths {
                    root: project_dir,
                    root_source: RootSource::DevProjectDir,
                    program_dir,
                    fallback_reason: None,
                };
            }
        }

        if dir_writable(&program_dir) {
            return AppPaths {
                root: program_dir.clone(),
                root_source: RootSource::ProgramDir,
                program_dir,
                fallback_reason: None,
            };
        }

        let reason = format!("程序目录不可写：{}", program_dir.display());
        let fallback = local_app_data_dir().join("WindowsWallPaper");
        log::warn!("{reason}；数据将改存 {}", fallback.display());
        AppPaths {
            root: fallback,
            root_source: RootSource::FallbackLocalAppData,
            program_dir,
            fallback_reason: Some(reason),
        }
    }

    /// 创建运行期需要的全部目录。
    ///
    /// 会在数据根目录写一个标记文件：便携模式下，这个文件随文件夹一起被拷走，
    /// 是"数据确实在程序目录"最直接的证据。
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            self.config_dir(),
            self.data_dir(),
            self.log_dir(),
            self.crash_dir(),
            self.webview_data_dir(),
        ] {
            fs::create_dir_all(&dir)
                .map_err(|e| Error::io(format!("创建目录失败 {}", dir.display()), e))?;
        }
        self.write_root_marker()?;
        Ok(())
    }

    fn write_root_marker(&self) -> Result<()> {
        let marker = self.root.join("wwp-data-location.txt");
        let text = format!(
            "WindowsWallPaper 数据位置标记\r\n\
             生成时间(Unix 秒): {}\r\n\
             数据根目录: {}\r\n\
             来源: {}\r\n\
             程序目录: {}\r\n\
             说明: 此文件位于数据根目录。把整个程序文件夹拷到别处后，\
             若本文件随之出现，即表示数据确实存放在程序目录（便携模式）。\r\n",
            unix_now(),
            self.root.display(),
            self.root_source.as_str(),
            self.program_dir.display(),
        );
        fs::write(&marker, text)
            .map_err(|e| Error::io(format!("写入标记文件失败 {}", marker.display()), e))
    }

    /// 多行描述，供日志与 `--diagnose` 使用。
    pub fn describe(&self) -> String {
        let mut text = format!(
            "程序目录      : {}\n\
             数据根目录    : {}\n\
             数据位置来源  : {}\n",
            self.program_dir.display(),
            self.root.display(),
            self.root_source.as_str(),
        );
        match &self.fallback_reason {
            Some(reason) => text.push_str(&format!("回退原因      : {reason}\n")),
            None => text.push_str("回退原因      : 无（未回退，便携模式生效）\n"),
        }
        text.push_str(&format!(
            "配置目录      : {}\n\
             索引目录      : {}\n\
             日志目录      : {}\n\
             WebView2 数据 : {}\n\
             日志文件      : {}\n",
            self.config_dir().display(),
            self.data_dir().display(),
            self.log_dir().display(),
            self.webview_data_dir().display(),
            self.app_log().display(),
        ));
        text
    }
}

/// 程序所在目录。
///
/// 取可执行文件所在目录，不做上溯——上溯会让数据目录跟着源码走，
/// 而"数据跟着程序走"是便携模式的前提。开发态另由 [`cargo_project_dir`] 处理。
pub fn program_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::io("无法确定可执行文件路径", e))?;
    let exe = strip_extended_prefix(&exe);

    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::not_found("可执行文件所在目录"))
}

/// 开发态识别：从可执行文件所在目录上溯，找到含 `tauri.conf.json` 的工程目录。
///
/// 必要性来自一个实测到的问题：开发时数据会落在 `target/debug/config`，
/// 于是配置、索引、日志散落在构建产物里，而且 `--scan` 写的索引与界面读的索引
/// 不是同一份。识别出工程目录后，开发态的数据统一放在 `src-tauri/` 下。
///
/// 判据用 `tauri.conf.json`（src-tauri 的直接标志），而不是 `Cargo.toml`：
/// 后者在 crates 子目录里也有，会误判到 `crates/wwp-core` 上。
fn cargo_project_dir(program_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(program_dir);
    for _ in 0..6 {
        let dir = current?;
        if dir.join("tauri.conf.json").is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// `%LOCALAPPDATA%`，取不到时退回进程当前目录（仅作为最后兜底）。
fn local_app_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 目录是否可写：真的写一个文件再删掉，不用只读属性之类的间接判断。
///
/// 传进来的目录可能还不存在，因此先按需创建。
pub fn dir_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(probe_file_name());
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn probe_file_name() -> OsString {
    let mut name = OsString::from(".wwp-write-probe-");
    name.push(std::process::id().to_string());
    name.push("-");
    name.push(unix_now().to_string());
    name.push(".tmp");
    name
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 去掉 Windows 的 `\\?\` 前缀（若存在）。
///
/// 统一走这一层，界面与日志里显示的路径才不会莫名其妙地以 `\\?\` 开头。
fn strip_extended_prefix(path: &Path) -> PathBuf {
    match fs::canonicalize(path) {
        Ok(canonical) => {
            let text = canonical.to_string_lossy();
            if let Some(rest) = text.strip_prefix(r"\\?\") {
                PathBuf::from(rest)
            } else {
                canonical
            }
        }
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-paths-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writable_directory_is_detected_and_probe_is_removed() {
        let dir = temp_dir("writable");
        assert!(dir_writable(&dir));
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            leftovers.is_empty(),
            "探测文件必须被清理，实际残留：{leftovers:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_override_wins_when_writable() {
        let dir = temp_dir("env");
        // 走参数而不是 std::env::set_var：后者是全局副作用，并行测试会互相干扰。
        let paths = AppPaths::resolve_with_override(
            PathBuf::from(r"C:\nonexistent-program-dir"),
            Some(dir.clone().into_os_string()),
        );

        assert_eq!(paths.root_source, RootSource::EnvOverride);
        assert_eq!(paths.root, dir);
        assert!(paths.fallback_reason.is_none());
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn empty_env_override_is_ignored() {
        let dir = temp_dir("env-empty");
        let paths = AppPaths::resolve_with_override(dir.clone(), Some(OsString::new()));
        assert_eq!(paths.root_source, RootSource::ProgramDir);
        assert_eq!(paths.root, dir);
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn ensure_dirs_creates_every_location_and_the_marker() {
        let root = temp_dir("ensure");
        let paths = AppPaths {
            root: root.clone(),
            root_source: RootSource::ProgramDir,
            program_dir: root.clone(),
            fallback_reason: None,
        };
        paths.ensure_dirs().unwrap();

        for dir in [
            paths.config_dir(),
            paths.data_dir(),
            paths.log_dir(),
            paths.crash_dir(),
            paths.webview_data_dir(),
        ] {
            assert!(dir.is_dir(), "目录未创建：{}", dir.display());
        }
        assert!(root.join("wwp-data-location.txt").is_file(), "缺少数据位置标记文件");
        // describe() 是诊断输出与日志的内容，必须包含关键字段。
        let text = paths.describe();
        for needle in ["数据位置来源", "program-dir", "日志文件"] {
            assert!(text.contains(needle), "describe() 缺少 {needle}：\n{text}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn env_override_is_ignored_when_it_cannot_be_a_directory() {
        // 用一个已存在的**文件**作为 WWP_HOME：既不可创建也不可写，必须被忽略。
        let dir = temp_dir("blocked");
        let file = dir.join("not-a-dir");
        fs::write(&file, b"x").unwrap();

        let fallback_root = temp_dir("blocked-fallback");
        let paths = AppPaths::resolve_with_override(
            fallback_root.clone(),
            Some(file.clone().into_os_string()),
        );

        assert_eq!(paths.root_source, RootSource::ProgramDir);
        assert_eq!(paths.root, fallback_root);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn project_dir_is_found_by_tauri_conf_and_not_by_cargo_toml() {
        let root = temp_dir("devroot");
        fs::write(root.join("tauri.conf.json"), "{}").unwrap();
        let deep = root.join("target").join("debug");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(cargo_project_dir(&deep), Some(root.clone()));

        // 只有 Cargo.toml 的目录不算工程根：crates 子目录里也有它，会误判。
        let other = temp_dir("cargoonly");
        fs::write(other.join("Cargo.toml"), "[package]").unwrap();
        let other_deep = other.join("target").join("release");
        fs::create_dir_all(&other_deep).unwrap();
        assert_eq!(cargo_project_dir(&other_deep), None);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }

    #[test]
    fn dev_run_puts_data_in_the_project_dir() {
        // 复现"开发时数据落进 target/debug"的问题，确认现在会改放工程目录。
        let project = temp_dir("devproject");
        fs::write(project.join("tauri.conf.json"), "{}").unwrap();
        let exe_dir = project.join("target").join("debug");
        fs::create_dir_all(&exe_dir).unwrap();

        let paths = AppPaths::resolve_with_override(exe_dir.clone(), None);
        assert_eq!(paths.root_source, RootSource::DevProjectDir);
        assert_eq!(paths.root, project);
        // 程序目录仍然如实记录，界面要能同时看到两者。
        assert_eq!(paths.program_dir, exe_dir);

        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn packaged_layout_still_uses_the_program_dir() {
        // 交付文件夹里没有 tauri.conf.json，因此必须走 program-dir 这条正常路径。
        let app = temp_dir("packaged");
        let paths = AppPaths::resolve_with_override(app.clone(), None);
        assert_eq!(paths.root_source, RootSource::ProgramDir);
        assert_eq!(paths.root, app);
        let _ = fs::remove_dir_all(&paths.root);
    }
}

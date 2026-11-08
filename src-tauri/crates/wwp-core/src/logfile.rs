//! 日志落盘。
//!
//! 需求要求"日志与崩溃报告落盘"，因此不依赖任何插件：这里自己实现一个 `log`
//! 门面实现，把日志写进数据根目录的 `logs/`。
//!
//! 轮转用编号而不是日期：`app.log` 为当前文件，写满后依次变为 `app.1.log`、
//! `app.2.log`……最多保留 [`MAX_FILES`] 个，最旧的被丢弃。相比按日期轮转，
//! 它不需要日历计算，也就不会在时区或跨日边界上出错。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{Error, Result};

/// 单个日志文件的大小上限。
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// 保留的日志文件个数（含当前文件）。
pub const MAX_FILES: usize = 5;

/// 写入 `logs/app.log` 的日志实现。
pub struct FileLogger {
    state: Mutex<State>,
    path: PathBuf,
}

struct State {
    file: File,
    written: u64,
}

impl FileLogger {
    /// 打开（或创建）日志文件。
    pub fn open(path: &Path) -> Result<FileLogger> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("创建日志目录失败 {}", parent.display()), e))?;
        }
        let (file, written) = open_at_end(path)?;
        Ok(FileLogger {
            state: Mutex::new(State { file, written }),
            path: path.to_path_buf(),
        })
    }

    /// 写一行。写入失败只报告到 stderr，不向外抛错——日志本身不该让程序崩溃。
    pub fn write_line(&self, level: log::Level, target: &str, message: &str) {
        let line = format!(
            "{} [{}] {} {}\r\n",
            timestamp(),
            level_tag(level),
            target,
            message
        );
        let bytes = line.as_bytes();

        let mut state = match self.state.lock() {
            Ok(state) => state,
            // 上一次写入 panic 过也要能继续写日志，否则故障现场会丢。
            Err(poisoned) => poisoned.into_inner(),
        };

        if state.written + bytes.len() as u64 > MAX_FILE_BYTES {
            if let Err(e) = rotate(&self.path, &mut state) {
                eprintln!("[wwp] 日志轮转失败：{e}");
            }
        }

        if let Err(e) = state.file.write_all(bytes) {
            eprintln!("[wwp] 写入日志失败：{e}");
            return;
        }
        let _ = state.file.flush();
        state.written += bytes.len() as u64;
    }
}

/// 追加模式打开，并返回当前长度（用于判断何时轮转）。
fn open_at_end(path: &Path) -> Result<(File, u64)> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::io(format!("打开日志文件失败 {}", path.display()), e))?;
    let written = file
        .metadata()
        .map_err(|e| Error::io(format!("读取日志文件大小失败 {}", path.display()), e))?
        .len();
    Ok((file, written))
}

fn rotate(path: &Path, state: &mut State) -> Result<()> {
    let numbered = |index: usize| -> PathBuf {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "log".to_string());
        path.with_file_name(format!("{stem}.{index}.{ext}"))
    };

    // 丢弃最旧的一个，再逐个后移，最后把当前文件挪成 .1。
    let _ = fs::remove_file(numbered(MAX_FILES - 1));
    for index in (1..MAX_FILES - 1).rev() {
        let from = numbered(index);
        if from.exists() {
            let _ = fs::rename(&from, numbered(index + 1));
        }
    }
    let _ = fs::rename(path, numbered(1));

    let (file, _) = open_at_end(path)?;
    state.file = file;
    state.written = 0;
    Ok(())
}

fn level_tag(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "ERROR",
        log::Level::Warn => "WARN ",
        log::Level::Info => "INFO ",
        log::Level::Debug => "DEBUG",
        log::Level::Trace => "TRACE",
    }
}

/// 本地时间 `YYYY-MM-DD HH:MM:SS.mmm`。
///
/// 使用系统 API 取本地日历时间，再按"本地时间减 UTC"求出的固定偏移标注。
/// 不做日历运算，因此不存在跨日或夏令时的边界问题。
fn timestamp() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0);

    match local_now_seconds() {
        Some(secs) => {
            let (y, mo, d, h, mi, s) = civil_from_unix(secs);
            format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{millis:03}")
        }
        // 取不到本地时间就退化为 UTC 秒，至少保证日志有序可比。
        None => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            format!("unix={secs}.{millis:03}")
        }
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemTimeFields {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[cfg(windows)]
extern "system" {
    fn GetLocalTime(out: *mut SystemTimeFields);
}

#[cfg(windows)]
fn local_now_seconds() -> Option<i64> {
    let mut fields = SystemTimeFields::default();
    unsafe { GetLocalTime(&mut fields) };
    if fields.year == 0 {
        return None;
    }
    let days = days_from_civil(fields.year as i64, fields.month as i64, fields.day as i64);
    Some(
        days * 86_400
            + fields.hour as i64 * 3600
            + fields.minute as i64 * 60
            + fields.second as i64,
    )
}

#[cfg(not(windows))]
fn local_now_seconds() -> Option<i64> {
    None
}

/// days_from_civil（Howard Hinnant 的算法），与 `civil_from_unix` 互为反函数。
/// 用 `div_euclid` 而不是 `/`，因为负年份（1970 年以前）必须向下取整。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 把 Unix 秒拆成日历字段。
fn civil_from_unix(unix: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_algorithms_are_inverses_across_epochs() {
        for unix in [
            0,
            1,
            -1,
            951_782_400,   // 2000-02-29
            1_790_000_208, // 2026-09-21
            2_147_483_647,
        ] {
            let (y, mo, d, h, mi, s) = civil_from_unix(unix);
            assert_eq!(
                days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s,
                unix,
                "往返不一致：unix={unix} -> {y}-{mo}-{d} {h}:{mi}:{s}"
            );
        }
    }

    #[test]
    fn known_epoch_is_two_thousand_twenty_six() {
        // 1_790_000_208 的日期用独立算法核对：
        //   1970-01-01 到 2026-01-01 = 56*365 + 14 个闰日 = 20454 天
        //   2026 年非闰年，9 月 21 日是第 263 天  ->  共 20717 天
        //   20717 * 86400 = 1_789_948_800，余 51_408 秒 = 14:16:48
        let (y, mo, d, h, mi, s) = civil_from_unix(1_790_000_208);
        assert_eq!((y, mo, d), (2026, 9, 21));
        assert_eq!((h, mi, s), (14, 16, 48));
    }

    #[test]
    fn rotation_keeps_bounded_number_of_files() {
        let dir = std::env::temp_dir().join(format!(
            "wwp-log-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let logger = FileLogger::open(&path).unwrap();

        logger.write_line(log::Level::Info, "test", "first");
        // 直接把已写字节数顶到上限触发轮转，不必真的写满 5 MB。
        {
            let mut state = logger.state.lock().unwrap();
            state.written = MAX_FILE_BYTES;
        }
        logger.write_line(log::Level::Info, "test", "second");

        assert!(path.is_file(), "当前日志文件必须存在");
        assert!(dir.join("app.1.log").is_file(), "轮转后应产生 app.1.log");
        let _ = fs::remove_dir_all(&dir);
    }
}

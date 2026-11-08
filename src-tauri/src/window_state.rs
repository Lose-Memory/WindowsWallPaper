//! 窗口大小记忆：用户把窗口拉成多大，下次启动就还是多大。
//!
//! ## 用户的要求（2026-10）
//!
//! > 让程序记忆我调整后的界面大小，下一次打开后保持我调整过的大小，
//! > 而不是恢复初始大小。
//!
//! 在那之前，窗口大小写死在 `tauri.conf.json`（1400×950）：每次启动都回到那个
//! 尺寸，用户每开一次程序就要重新拉一次。
//!
//! ## 存哪里、存什么单位
//!
//! - **文件**：`data/window-state.json`。它与 `nsfw-state.json` 一样属于
//!   **运行时状态**，不是用户设置：界面上没有一个"窗口宽度"的输入框，用户也
//!   不该去手改它。因此不写进 `config/settings.toml`——那会让它变成一个
//!   可编辑字段，而且每拖一下窗口都要整篇重写一次设置文件。
//! - **单位**：**逻辑像素**（物理像素 ÷ 缩放因子）。
//!   实测过：这台机器主屏 3840×2160、缩放 150%，`inner_size()` 报的是物理像素
//!   （1800×1200 对应界面的 1200×800）。存物理值的话，换一块缩放不同的显示器
//!   或者改一下系统缩放，恢复出来的窗口就会大一圈或小一圈。逻辑尺寸在
//!   "用户看到的有多大"这个意义上才是稳定的。
//!
//! ## 三条不那么显然的规矩
//!
//! 1. **只记大小，不记位置**（用户明确选的）。位置由系统/taui 决定（配置里是
//!    `center: true`）；记了位置反而会在换了显示器之后把窗口丢到看不见的地方。
//! 2. **夹进当前显示器放得下的范围**：用户上一次可能是在一块 4K 屏上拉到
//!    2560×1800，这次只在 1080p 屏上——照搬会得到一个大半在屏幕外的窗口。
//! 3. **最大化/最小化时的尺寸不记**：最大化会把尺寸报成整块屏的大小，
//!    记下来的话，下次启动会得到一个"跟屏幕一样大的普通窗口"（标题栏被顶到
//!    屏幕外、任务栏压住底边）。因此那时跳过，保留上一次的普通尺寸。
//!
//! ## 什么时候写盘
//!
//! 拖动窗口会连续产生几十上百个 `Resized` 事件，每个都写一次文件太浪费。
//! 因此这里做了**去抖**：事件来了先记下尺寸，等 700 ms 内不再有新事件才真的写。
//! 另外两个"马上写"的时机是**关闭窗口**与**退出程序**——那时最后一次调整
//! 可能还没落到盘上，而这两个时机本来就是有意义的收尾点。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, PhysicalSize, Runtime, WebviewWindow};

/// 记忆文件的名字（放在数据目录下）。
const FILE_NAME: &str = "window-state.json";

/// 与 `tauri.conf.json` 里的 `minWidth` / `minHeight` 保持一致。
///
/// Tauri 没有"读取窗口最小尺寸"的接口，因此这里只能各写一份。
/// **改一处就要改两处**：这里更小的话，恢复出来的窗口会比用户能拖到的还小。
const MIN_WIDTH: f64 = 1100.0;
const MIN_HEIGHT: f64 = 720.0;

/// 恢复时给屏幕留的余量（逻辑像素）。
///
/// 不留余量的话，一块刚好等于屏幕高度的窗口会把标题栏顶到屏幕外、底边压到
/// 任务栏下面——用户得先想办法把它拖回来。
const MARGIN: f64 = 40.0;

/// 尺寸稳定多久之后才写盘（见模块文档）。
const SETTLE: Duration = Duration::from_millis(700);

/// 记忆下来的窗口大小（逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: f64,
    pub height: f64,
}

impl WindowSize {
    /// 这个尺寸看起来是不是一个"真的窗口"。
    ///
    /// 挡的是手改坏的文件与半截写入：0、负数、NaN 都要当没有。
    fn is_plausible(self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 200.0
            && self.height >= 200.0
    }
}

/// 记忆文件的位置。
pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// 读上次的窗口大小。文件不在、读不动、内容不认识都只当"没记过"。
///
/// 不报错：窗口大小是**锦上添花**的东西，为它挡住启动流程不值得。
pub fn load(data_dir: &Path) -> Option<WindowSize> {
    let text = std::fs::read_to_string(state_path(data_dir)).ok()?;
    match serde_json::from_str::<WindowSize>(&text) {
        Ok(size) if size.is_plausible() => Some(size),
        Ok(size) => {
            log::warn!("窗口大小记忆里的数值不可用（{}×{}），按没记过处理", size.width, size.height);
            None
        }
        Err(error) => {
            log::warn!("窗口大小记忆无法解析（{error}），按没记过处理");
            None
        }
    }
}

/// 写下窗口大小。失败只记日志——不该因为写不了这个文件而影响退出流程。
pub fn save(data_dir: &Path, size: WindowSize) -> Result<(), String> {
    if !size.is_plausible() {
        return Err(format!("拒绝记录不可用的尺寸 {}×{}", size.width, size.height));
    }
    std::fs::create_dir_all(data_dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
    let text = serde_json::to_string_pretty(&size).map_err(|e| format!("序列化窗口大小失败：{e}"))?;
    std::fs::write(state_path(data_dir), text).map_err(|e| format!("写窗口大小失败：{e}"))
}

/// 把尺寸夹进"这块屏放得下"的范围里（纯函数，有测试）。
///
/// 上界是屏幕大小减 [`MARGIN`]；下界是 [`MIN_WIDTH`] / [`MIN_HEIGHT`]
/// （与窗口自己的最小尺寸一致）。屏幕比最小尺寸还小时以屏幕为准——
/// 那种情况下宁可让窗口比最小值小，也不要让它比屏幕还大。
pub fn clamp(size: WindowSize, available: WindowSize) -> WindowSize {
    let max_width = (available.width - MARGIN).max(MIN_WIDTH);
    let max_height = (available.height - MARGIN).max(MIN_HEIGHT);
    WindowSize {
        width: size.width.clamp(MIN_WIDTH, max_width),
        height: size.height.clamp(MIN_HEIGHT, max_height),
    }
}

/// 窗口现在的大小（逻辑像素）。最大化/最小化时返回 `None`（见模块文档第 3 条）。
///
/// **必须在主线程上调用**：Tauri 的窗口方法要与窗口所在的线程通信，
/// 从别的线程调有卡死的风险。因此事件处理里读尺寸、后台线程只负责写文件。
pub fn current_size<R: Runtime>(window: &WebviewWindow<R>) -> Option<WindowSize> {
    if window.is_maximized().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return None;
    }
    let physical: PhysicalSize<u32> = window.inner_size().ok()?;
    if physical.width == 0 || physical.height == 0 {
        return None;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical = physical.to_logical::<f64>(scale);
    let size = WindowSize {
        width: logical.width,
        height: logical.height,
    };
    if size.is_plausible() {
        Some(size)
    } else {
        None
    }
}

/// 按上次记下的大小设置窗口，并把窗口重新居中。
///
/// ## 为什么还要 center()
///
/// 窗口是按 `tauri.conf.json` 里的 1400×950 居中创建的，我们随后改了尺寸：
/// 尺寸变大时右下两边会探出屏幕外（左上角没动，居中位置是按旧尺寸算的）。
/// 重新居中一次就回到屏幕里了。用户没有要求记住位置，因此居中是可接受的行为。
pub fn apply_saved<R: Runtime>(window: &WebviewWindow<R>, data_dir: &Path) {
    let Some(saved) = load(data_dir) else {
        return;
    };

    // 当前显示器的可用逻辑尺寸。窗口是隐藏创建的，取不到显示器时就不夹了
    // （尺寸来自我们自己上一次写下的值，仍然是可信的）。
    let target = match window.current_monitor() {
        Ok(Some(monitor)) => {
            let scale = monitor.scale_factor();
            let screen = monitor.size().to_logical::<f64>(scale);
            clamp(
                saved,
                WindowSize {
                    width: screen.width,
                    height: screen.height,
                },
            )
        }
        _ => saved,
    };

    match window.set_size(LogicalSize::new(target.width, target.height)) {
        Ok(()) => {
            log::info!(
                "已按上次的窗口大小恢复：{}×{}（逻辑像素；记下的是 {}×{}）",
                target.width as u32,
                target.height as u32,
                saved.width as u32,
                saved.height as u32
            );
            // 尺寸变了，居中的基准也跟着变；重来一次免得窗口探到屏幕外。
            if let Err(error) = window.center() {
                log::debug!("窗口重新居中失败（不影响使用）：{error}");
            }
        }
        Err(error) => log::warn!("恢复窗口大小失败：{error}"),
    }
}

/// 立刻把窗口现在的大小写盘（关闭窗口、退出程序时用）。
pub fn save_now<R: Runtime>(window: &WebviewWindow<R>, data_dir: &Path) {
    let Some(size) = current_size(window) else {
        // 最大化时窗口报的是整块屏的尺寸，记下来下次会得到一个"跟屏幕一样大"
        // 的普通窗口，因此跳过。
        log::debug!("当前窗口处于最大化/最小化，本次不记录窗口大小");
        return;
    };
    match save(data_dir, size) {
        Ok(()) => log::info!(
            "已记住窗口大小：{}×{}（逻辑像素）",
            size.width as u32,
            size.height as u32
        ),
        Err(error) => log::warn!("记录窗口大小失败：{error}"),
    }
}

/// 装上"窗口尺寸变了就记下来"的监听（带去抖，见模块文档）。
///
/// 尺寸在**事件处理里**读出来（那时在主线程上），后台线程只负责睡一会儿再写盘；
/// 若这期间又来了新的事件，旧的那个线程会发现自己的序号过期而直接退出，
/// 于是最终写下的永远是**最后一次**的尺寸。
pub fn install<R: Runtime>(window: &WebviewWindow<R>, data_dir: PathBuf) {
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    let window = window.clone();
    window.clone().on_window_event(move |event| {
        let tauri::WindowEvent::Resized(size) = event else {
            return;
        };
        // 最小化时 Windows 报的是 0×0，那不是用户调整出来的尺寸。
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some(current) = current_size(&window) else {
            return;
        };

        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let data_dir = data_dir.clone();
        let spawned = std::thread::Builder::new()
            .name("wwp-window-size".to_string())
            .spawn(move || {
                std::thread::sleep(SETTLE);
                if GENERATION.load(Ordering::SeqCst) != generation {
                    // 这期间窗口又变了：那一次自己会写，这里直接退出。
                    return;
                }
                match save(&data_dir, current) {
                    Ok(()) => log::debug!(
                        "窗口大小已记下：{}×{}（逻辑像素）",
                        current.width as u32,
                        current.height as u32
                    ),
                    Err(error) => log::warn!("记录窗口大小失败：{error}"),
                }
            });
        if let Err(error) = spawned {
            log::warn!("无法启动记录窗口大小的线程：{error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("wwp-window-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_size_round_trips_through_the_file() {
        let dir = temp_dir("roundtrip");
        let size = WindowSize {
            width: 1732.0,
            height: 1105.0,
        };
        save(&dir, size).unwrap();
        assert_eq!(load(&dir), Some(size));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_file_just_means_no_memory() {
        let dir = temp_dir("broken");
        assert_eq!(load(&dir), None, "没记过就是 None");

        std::fs::write(state_path(&dir), "{ this is not json").unwrap();
        assert_eq!(load(&dir), None, "解析不了也只能当没记过，不能挡住启动");

        // 手改成 0 或负数：不能拿它去设窗口（那会得到一个看不见的窗口）。
        std::fs::write(state_path(&dir), r#"{"width": 0, "height": -5}"#).unwrap();
        assert_eq!(load(&dir), None);

        // 反过来：算出来的值不可信时也不该写盘。
        assert!(save(
            &dir,
            WindowSize {
                width: 0.0,
                height: 900.0
            }
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clamping_keeps_a_reasonable_size_untouched() {
        let screen = WindowSize {
            width: 2560.0,
            height: 1440.0,
        };
        let wanted = WindowSize {
            width: 1600.0,
            height: 1000.0,
        };
        assert_eq!(clamp(wanted, screen), wanted);
    }

    #[test]
    fn clamping_shrinks_a_window_that_would_not_fit() {
        // 在上一次那块屏上拉到 2560×1800，这次只有 1920×1080：
        // 照搬会得到一个右下角都在屏幕外的窗口。
        let screen = WindowSize {
            width: 1920.0,
            height: 1080.0,
        };
        let clamped = clamp(
            WindowSize {
                width: 2560.0,
                height: 1800.0,
            },
            screen,
        );
        assert_eq!(clamped.width, 1920.0 - MARGIN);
        assert_eq!(clamped.height, 1080.0 - MARGIN);
        assert!(
            clamped.height < screen.height,
            "必须留出余量，否则底边会压到任务栏下面"
        );
    }

    #[test]
    fn clamping_never_goes_below_the_windows_own_minimum() {
        // 记忆文件被手改成一个很小的值（或者窗口本来就不该那么小）：
        // 恢复出来必须仍然不小于窗口自己允许的最小尺寸。
        let screen = WindowSize {
            width: 3840.0,
            height: 2160.0,
        };
        let clamped = clamp(
            WindowSize {
                width: 300.0,
                height: 300.0,
            },
            screen,
        );
        assert_eq!(clamped.width, MIN_WIDTH);
        assert_eq!(clamped.height, MIN_HEIGHT);
    }

    #[test]
    fn when_the_screen_is_smaller_than_the_minimum_the_minimum_wins() {
        // 极端情况：屏幕比窗口的最小尺寸还小（这台机器上不会发生——最小尺寸
        // 本来就是按它的显示器定的）。此时以**最小尺寸**为准：
        // 窗口自己的最小值由系统强制，做成比它还小的窗口是自相矛盾的配置。
        let screen = WindowSize {
            width: 800.0,
            height: 600.0,
        };
        let clamped = clamp(
            WindowSize {
                width: 1600.0,
                height: 1200.0,
            },
            screen,
        );
        assert_eq!(clamped.width, MIN_WIDTH);
        assert_eq!(clamped.height, MIN_HEIGHT);
    }
}

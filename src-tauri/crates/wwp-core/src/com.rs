//! 每个线程的 COM 初始化。
//!
//! ## 为什么这是一个公共设施，而不是各模块各写一份
//!
//! 这个坑在本项目里**出现过两次**：
//!
//! 1. 视频解码：两块屏各有自己的解码线程，第一个能打开、第二个报
//!    "尚未调用 CoInitialize"——因为只有主线程初始化过。表象是"只有一块屏能播"，
//!    很容易被误判成显示器或窗口的问题。
//! 2. 音频：`--probe-audio` 走主线程，同样报"尚未调用 CoInitialize"。
//!
//! 两次原因相同：**Media Foundation 与 WASAPI 都依赖 COM，而 COM 是按线程的**。
//! 只要某段代码可能跑在没初始化过 COM 的线程上，它就必须自己初始化。
//! 既然这个需求横跨多个模块，就把它放在这里统一提供，而不是让每处各写一遍
//! （写两遍就会有两遍不一致，第三处还会忘记写）。

use crate::{Error, Result};

/// 一个线程的 COM 初始化句柄。
///
/// 持有它表示"本线程的 COM 已可用"。**它必须活到本线程不再使用 COM 为止**：
/// drop 时会配对调用 `CoUninitialize`（仅在确实由我们初始化的情况下）。
pub struct ComApartment {
    /// 本次调用是否真的完成了初始化（决定要不要配对 `CoUninitialize`）。
    needs_uninitialize: bool,
}

impl ComApartment {
    /// 在当前线程初始化 COM。
    ///
    /// 用 `COINIT_MULTITHREADED`（MTA）：MF 的解码器与 WASAPI 在 MTA 下工作得最好，
    /// 而且 MTA 不需要消息泵。`CoInitialize` 默认是 STA，STA 要求线程泵消息，
    /// 而我们的解码/播放线程都不泵消息——那会让跨套间调用挂住。
    pub fn initialize() -> Result<ComApartment> {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

        // 这个函数返回**裸 HRESULT**（不是 Result）：
        // 成功是 S_OK(0)，而"此前已初始化"是 S_FALSE(1)——后者同样算成功。
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        match hr.0 {
            0 => Ok(ComApartment {
                needs_uninitialize: true,
            }),
            code => {
                // S_FALSE = 0x00000001：已经初始化过，不需要我们再配对释放。
                // RPC_E_CHANGED_MODE = 0x80010106：本线程已是别的套间模型，也能用 COM。
                const S_FALSE: i32 = 0x0000_0001;
                const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
                if code == S_FALSE || code == RPC_E_CHANGED_MODE {
                    log::debug!("本线程的 COM 已初始化过（0x{code:08X}），沿用现有设置");
                    Ok(ComApartment {
                        needs_uninitialize: false,
                    })
                } else {
                    Err(Error::config(
                        "COM 初始化失败（视频解码与音频播放都依赖它）",
                        format!("HRESULT 0x{code:08X}"),
                    ))
                }
            }
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.needs_uninitialize {
            unsafe {
                windows::Win32::System::Com::CoUninitialize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_initialization_on_the_same_thread_is_safe() {
        // 同一个线程反复初始化必须都能成功：调用方可能嵌套使用
        // （例如音频路径里再打开视频），不能因为"已经初始化过"就失败。
        let first = ComApartment::initialize().expect("第一次初始化应当成功");
        let second = ComApartment::initialize().expect("重复初始化也应当成功");
        drop(second);
        drop(first);
        // 前一个释放之后仍然要有可用的 COM——这检查的是"不平衡的 CoUninitialize
        // 会不会破坏后续调用"这个风险的另一面。
        let third = ComApartment::initialize().expect("释放后再次初始化应当成功");
        drop(third);
    }
}

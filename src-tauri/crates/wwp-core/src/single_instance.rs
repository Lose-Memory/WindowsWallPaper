//! 单实例门闩：一个登录会话里只允许跑一个 WindowsWallPaper。
//!
//! ## 为什么必须有（用户 2026-10-05 报的问题）
//!
//! 程序原先可以启动任意多个进程，而两个进程会同时干这些事：
//!
//! - 各自按自己的计时器 `SetWallpaper`——两块屏上的图会互相打架，看起来像"随机跳图"；
//! - 各自往同一个 `config/settings.toml` 整篇重写——后写的赢，先改的设置静默丢失；
//! - 各自注册同一批全局快捷键，后注册的那个必然失败（并刷一条 warn）；
//! - 托盘里出现**两个一模一样的图标**，用户根本分不清哪个是哪个。
//!
//! 因此要求"只能启动一个进程"：已经有一个在跑时，**第二次启动不再开新进程**，
//! 而是让那个已经在跑的实例把主界面显示出来（用户要求"再次启动则默认打开主界面"）。
//!
//! ## 怎么实现的
//!
//! 两半，各自都很小：
//!
//! 1. **命名互斥体**（[`claim`]）：`CreateMutexW` 一个带名字的互斥体。
//!    进程退出（包括崩溃、被任务管理器结束）时内核会自动关掉句柄、销毁对象，
//!    因此**不存在"上次崩溃留下的锁文件"**这种要用户手工清理的状态——
//!    这正是选它而不是"写个 pid 文件"的原因。
//! 2. **给已有实例发一条消息**（[`ask_existing_to_show_window`]）：
//!    找到那个实例的隐藏消息窗口（[`crate::hidden_window`] 里那个），
//!    用 `PostMessage` 请它显示主界面。**不能由第二个进程自己去 ShowWindow**：
//!    窗口是别人线程的，跨进程操作它只能靠系统代劳，而发消息让owner自己处理
//!    与"点托盘里的显示主界面"走的是同一段代码。

use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE,
};
use windows::Win32::System::Threading::CreateMutexW;

/// 互斥体名字。
///
/// 用 `Local\` 而不是 `Global\`：`Local\` 的作用域是**当前登录会话**。
/// 这一点是刻意的——换一个用户登录（或远程桌面另开一个会话）时，
/// 桌面的壁纸、托盘、快捷键都是那个会话自己的，共用同一个实例反而会
/// "A 的界面控制 B 的壁纸"。多用户同时用时各跑一个才是对的。
const MUTEX_NAME: &str = "Local\\WindowsWallPaper-SingleInstance";

/// 已经在跑时，最长等多久去找它的消息窗口。
///
/// 需要等待是因为：**互斥体是在 Tauri 起来之前就占住的**，而隐藏消息窗口
/// 要等 `setup` 里 `broadcast::start` 才创建。第二次启动如果恰好插在这几百毫秒
/// 里，第一轮 `FindWindowW` 会找不到窗口——重试几次就够了，不必要求用户再点一次。
const LOOKUP_ATTEMPTS: u32 = 15;
const LOOKUP_INTERVAL: Duration = Duration::from_millis(200);

/// 单实例门闩。**只要它活着，别人就启动不起来**，因此不能提前丢掉。
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 占住门闩。返回 `None` 表示**已经有实例在跑**。
pub fn claim() -> Option<InstanceGuard> {
    claim_named(MUTEX_NAME)
}

/// 与 [`claim`] 相同，但可以指定名字（单元测试要用一个不冲突的名字）。
pub fn claim_named(name: &str) -> Option<InstanceGuard> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // 先把 last error 清成 0：`CreateMutexW` 只在"对象已存在"时才把它设成
        // ERROR_ALREADY_EXISTS，而**本线程之前任何一次失败的 Win32 调用**都可能
        // 恰好留下 183。不清的话会偶发地"第一次启动就以为有别人在跑"。
        SetLastError(ERROR_SUCCESS);
        // binitialowner = false：这里只关心"对象在不在"，不需要持有它。
        // 句柄本身就让对象活着，而进程退出时内核会关掉全部句柄。
        let handle = CreateMutexW(None, false, PCWSTR(wide.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(InstanceGuard(handle))
    }
}

/// 请**已经在跑的那个实例**把主界面显示出来。返回是否成功把请求发出去。
///
/// 找不到窗口时会重试几秒（见 [`LOOKUP_ATTEMPTS`] 的说明）。
pub fn ask_existing_to_show_window() -> bool {
    for attempt in 1..=LOOKUP_ATTEMPTS {
        if crate::hidden_window::ask_existing_to_show_window() {
            return true;
        }
        if attempt < LOOKUP_ATTEMPTS {
            std::thread::sleep(LOOKUP_INTERVAL);
        }
    }
    false
}

/// 互斥体名字的作用域必须是当前登录会话。
///
/// 少了 `Local\` 前缀就是一个 `Global\` 对象：多用户/远程桌面下第二个会话会
/// "启动不起来"，而它的托盘与壁纸本来该是这个会话自己的。
/// （用测试而不是 `const` 断言：`starts_with` 不是 const 函数。）
#[cfg(test)]
#[test]
fn the_mutex_is_scoped_to_the_login_session() {
    assert!(
        MUTEX_NAME.starts_with("Local\\"),
        "互斥体名字必须限定在当前登录会话内"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用独立名字：**测试机上可能正跑着真的程序**，
    /// 用生产名字会让测试与它互相干扰（那样的失败会非常费解）。
    fn unique_name(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("Local\\WindowsWallPaper-test-{tag}-{nanos}")
    }

    #[test]
    fn a_second_claim_is_refused_while_the_first_is_alive() {
        let name = unique_name("exclusive");
        let first = claim_named(&name);
        assert!(first.is_some(), "第一个实例应当拿到门闩");
        assert!(
            claim_named(&name).is_none(),
            "第一个还活着时，第二个必须被拒绝——否则两个进程会互相抢壁纸与设置"
        );
    }

    #[test]
    fn the_gate_opens_again_after_the_guard_is_dropped() {
        // 进程退出（含崩溃）时内核会关掉句柄，因此"上一次崩溃"不会留下
        // 需要人工清理的锁——这条测试守的就是这个性质。
        let name = unique_name("reopen");
        let guard = claim_named(&name).expect("第一次应当成功");
        drop(guard);
        assert!(
            claim_named(&name).is_some(),
            "门闩随句柄释放，不应该留下永久占用"
        );
    }

    #[test]
    fn different_names_do_not_block_each_other() {
        let a = unique_name("a");
        let b = unique_name("b");
        let _first = claim_named(&a).expect("a 应当成功");
        assert!(
            claim_named(&b).is_some(),
            "不同名字互不影响（否则测试之间会互相干扰）"
        );
    }

    #[test]
    fn asking_a_missing_instance_to_show_the_window_is_a_clean_failure() {
        // 没有实例在跑时（测试环境里一般不跑），这个函数必须老老实实返回 false，
        // 而不是 panic 或一直卡住——它的调用者是"第二次启动"那条路，
        // 那条路必须能干净地退出。
        //
        // 注意：并行跑测试时若有别的测试恰好创建了同名窗口，这条断言可能变红，
        // 因此只断言"能返回"（不 panic、不永久阻塞）。
        let started = std::time::Instant::now();
        let _ = crate::hidden_window::ask_existing_to_show_window();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "单次查找必须是立即返回的（重试由上层负责）"
        );
    }
}

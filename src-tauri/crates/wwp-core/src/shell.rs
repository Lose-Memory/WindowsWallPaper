//! 把"打开这个文件"这件事交给 Windows 自己去做。
//!
//! ## 与 `commands::open_folder` 的关系（这两件事看起来一样，其实不是）
//!
//! `open_folder` 打开的是**目录**，用 `explorer.exe <目录>` 就够了，因此那里
//! 刻意不引入 shell 的 COM 接口（见该函数的注释）。这里要打开的是**文件**：
//! 用户点了回收站里的缩略图，期望的是"用系统默认关联的那个程序把它打开"
//! （.jpg 用照片查看器、.mp4 用播放器、什么关联都没有时弹"打开方式"）。
//! 这件事只有 shell 知道答案，`explorer.exe <文件>` 不保证——它更可能变成
//! "在资源管理器里定位/预览"，而不是打开。
//!
//! 因此这里用 `ShellExecuteW`，它是 shell 提供的标准入口，"按关联打开"
//! 正是它的本职。代价是多一次 COM 调用。
//!
//! ## 线程要求
//!
//! shell 的这几个入口都要求调用线程有一个已初始化的 COM 单元。界面上调用时
//! 本来就有（同步命令跑在 WebView2 的 STA 主线程上），但命令行入口没有——
//! 因此本模块用 [`ComGuard`] **自己保证**，不指望调用方记得（1.11.1 的教训：
//! `--reveal` 第一版就是在这里拿到"尚未调用 CoInitialize"的）。

use std::path::Path;

/// 确保当前线程有 COM 单元，返回一个"离开作用域时收尾"的守卫。
///
/// ## 为什么要在这里自己初始化（1.11.1 的教训）
///
/// 1.11.0 的写法默认"调用方所在线程已经是 STA"——在界面上成立（Tauri 的同步
/// 命令跑在 WebView2 的 STA 主线程上），但同一个函数被**命令行入口**调用时
/// 就不成立了：`--reveal` 跑在 `main` 线程上，那里没有任何人初始化过 COM，
/// 于是 `SHOpenFolderAndSelectItems` 直接返回
/// `尚未调用 CoInitialize。(0x800401F0)`。
///
/// 与其要求每个调用点自己记得初始化，不如让这几个函数**自己保证**：
/// 已经初始化过就是无害的（`CoInitializeEx` 会返回 `S_FALSE`，引用计数 +1，
/// 与这里的 `CoUninitialize` 正好抵消），MTA 线程则返回
/// `RPC_E_CHANGED_MODE`——那种情况下 COM 是可用的，我们只是不该去配对收尾。
#[cfg(windows)]
struct ComGuard {
    /// 真的由本次调用建立单元时才需要配对 `CoUninitialize`。
    should_uninitialize: bool,
}

#[cfg(windows)]
impl ComGuard {
    fn new() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

        // SAFETY: 单参数 COM 调用，无指针参数。
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let hr = result.0;
        // S_OK 表示这次调用建立了单元，S_FALSE 表示本线程已经有单元（两者都要配对
        // 收尾）；RPC_E_CHANGED_MODE 表示本线程已是 MTA——COM 能用，但不归我们收尾。
        const S_OK: i32 = 0;
        const S_FALSE: i32 = 1;
        Self {
            should_uninitialize: hr == S_OK || hr == S_FALSE,
        }
    }
}

#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninitialize {
            // SAFETY: 与 `CoInitializeEx` 配对，且只在本次调用确实建立单元时执行。
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// 用系统默认关联的程序打开一个文件。
///
/// 失败信息要能直接给用户看：文件已经不在了、没有关联程序，都是常见情况。
#[cfg(windows)]
pub fn open_file(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if !path.exists() {
        return Err(format!("它已经不在了：{}", path.display()));
    }

    // `ShellExecuteW` 要求本线程有 COM 单元（命令行入口没有，见 ComGuard）。
    let _com = ComGuard::new();

    // 动词 "open" 与路径都要以 0 结尾的宽字符串。
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let mut file: Vec<u16> = path.as_os_str().encode_wide().collect();
    file.push(0);

    // SAFETY: 两个指针都指向本函数栈上、以 0 结尾且活得比这次调用长的缓冲区；
    // 其余参数为空指针（合法）。
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // 返回值名义上是 HINSTANCE，实际上是错误码：**大于 32 才算成功**。
    // 32 以下每一位都有含义（0 = 内存不足、2 = 找不到文件、31 = 没有关联程序…），
    // 详情见 shellapi.h 里 `ShellExecute` 的说明。
    let code = result.0 as isize;
    if code <= 32 {
        return Err(format!(
            "系统没能打开它（ShellExecuteW 返回 {code}，31 通常表示没有关联程序）"
        ));
    }
    Ok(())
}

/// 非 Windows 平台上没有 shell 关联程序这回事。
#[cfg(not(windows))]
pub fn open_file(_path: &Path) -> Result<(), String> {
    Err("当前平台没有系统默认程序接口".to_string())
}

/// 在资源管理器里打开某个**文件**所在的文件夹，并把这个文件**选中**。
///
/// 用户点播放页那一行路径、或回收站里那一条文件名时要的就是这件事：
/// "这张图在哪儿"。它**不等同于**"复制路径"，也不等同于"打开这个文件"
/// （后者是 [`open_file`]）。
///
/// ## 为什么用 `SHOpenFolderAndSelectItems` 而不是 `explorer /select,`
///
/// 1.11.0 用的写法是 `Command::new("explorer").arg("/select,<路径>")`。
/// 它在真机上"有时候"会打开 **Home（资源管理器首页）** 而不是目标目录，
/// 1.11.1 换成 shell 的官方接口。两个原因：
///
/// - `explorer.exe` 只是把请求**转交**给已经在跑的那个 shell。转交失败时它会
///   自己启一个新进程去开默认位置——参数被静默丢掉，命令行本身没有任何反馈
///   （`spawn` 是成功的，所以我们原来的日志全是"已在资源管理器中定位"，看不出
///   失败）。这台机器上正好有 20 多个只多不少的 `explorer.exe /factory,…`
///   常驻进程，还有注入 explorer 的第三方美化程序，
///   转交这一环最容易出问题。
/// - `SHOpenFolderAndSelectItems` 是**让现有的 shell 去导航**，不新起进程、
///   不经过命令行参数，因此没有"参数被丢掉"这一级。
///
/// 实测（UI Automation 读窗口标题 + 选中项）：文件不存在时
/// `explorer /select,<路径>` 会落到 Home；而"文件不在了"恰恰是最常见的一种点击
/// （预览来不及更新，或用户刚删掉那张）。
///
/// ## 参数怎么传（这里踩过一次，别再改回去）
///
/// `pidlFolder` 必须是那个文件**所在目录的完整 PIDL**，做法是
/// `ILClone(完整 PIDL)` 之后 `ILRemoveLastID` 就地砍掉最后一段
/// （`ILClone` 会给副本另分配一块内存，因此砍掉副本不影响原件）。
///
/// 两个**看上去更合理、实测都不行**的写法：
///
/// - 直接传那个文件的**完整 PIDL** 当 `pidlFolder`：函数返回
///   `E_ABORT (0x80004004)`，什么都不做。实测在英文路径
///   （`C:\Windows\notepad.exe`）与中文路径上一样失败，与路径无关。
/// - 用 `ILCloneFirst` 取"盘符那一段"当 `pidlFolder`：调用**返回成功**，
///   但资源管理器会落到**"此电脑"**上，一个文件也没选中——比报错更糟，
///   因为日志里看不出问题。
///
/// ## 文件不在了怎么办
///
/// **落到它所在的那个目录**（`apidl` 传 `None` 就是"只打开目录"），
/// 这与用户的意图一致——文件被删掉之后，那里通常还留着同一批图。
/// 目录也不在时才报错。
///
/// 调用方已经判过"路径非空"，这里不重复。
#[cfg(windows)]
pub fn reveal_file(path: &Path) -> Result<(), String> {
    use windows::Win32::UI::Shell::{ILClone, ILFindLastID, ILFree, ILRemoveLastID};
    use windows::Win32::UI::Shell::SHOpenFolderAndSelectItems;

    // shell 的两个入口都要求本线程有 COM 单元。命令行入口（`--reveal`）没有，
    // 因此这里自己保证；界面上的同步命令本来就有，重复初始化是无害的。
    let _com = ComGuard::new();

    if path.exists() {
        // 0 表示"不要求任何属性"，我们只想拿到 PIDL。
        if let Some(item) = parse_display_name(path) {
            // SAFETY: `item` 是 shell 刚给出的完整 PIDL（含文件那一段）。
            let folder = unsafe { ILClone(item) };
            if !folder.is_null() {
                // 把副本砍成"所在目录"；`item` 本身不动，后面还要从它取最后一段。
                let trimmed: bool = unsafe { ILRemoveLastID(Some(folder)) }.into();
                if trimmed {
                    let child = unsafe { ILFindLastID(item) };
                    // SAFETY: `folder` 与 `child` 都在本次调用期间有效；
                    // `child` 是 `item` 内部的一段，不是独立分配，因此不单独释放。
                    let result =
                        unsafe { SHOpenFolderAndSelectItems(folder, Some(&[child]), 0) };
                    // SAFETY: `folder` 由 `ILClone` 分配，交给 `ILFree`；`item` 由
                    // `SHParseDisplayName` 分配，交给 `CoTaskMemFree`。
                    unsafe { ILFree(Some(folder as *const _)) };
                    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(item as *const _)) };
                    return result.map_err(|error| {
                        format!("系统没能打开它所在的文件夹（SHOpenFolderAndSelectItems：{error}）")
                    });
                }
                // SAFETY: 砍不动（理论上只有"盘符根"才会这样）也要还回去。
                unsafe { ILFree(Some(folder as *const _)) };
            }
            unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(item as *const _)) };
        }
    }

    // 文件不在了（或 shell 不认这条路径）：打开它所在的目录。
    match path.parent() {
        Some(parent) if parent.is_dir() => {
            let Some(folder) = parse_display_name(parent) else {
                return Err(format!(
                    "这些文件都打不开了（系统不认识这条路径：{}）",
                    parent.display()
                ));
            };
            // SAFETY: `folder` 是刚解析出来的目录 PIDL，用完归还。
            // 传 `None` 就是"只打开这个目录、不选中里面任何东西"——
            // 这个目录本身就是我们要展示给用户的落点。
            let result = unsafe { SHOpenFolderAndSelectItems(folder, None, 0) };
            unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(folder as *const _)) };
            result.map_err(|error| {
                format!("系统没能打开那个文件夹（SHOpenFolderAndSelectItems：{error}）")
            })
        }
        // 连目录都没有（整个盘被拔掉、目录被改名）：如实报错。
        _ => Err(format!("文件与其所在目录都已不存在：{}", path.display())),
    }
}

/// 把一条路径交给 shell 解析成 PIDL（"shell 眼里的这个对象"的标识）。
///
/// 返回的指针**必须**由调用方用 `CoTaskMemFree` 释放（shell 用 COM 任务分配器
/// 分配它）。解析不到（路径不存在、不是 shell 认识的东西）时返回 `None`——
/// 这是正常情况，不是错误。
///
/// `pbc` 传 `None`：不需要绑定上下文，这是最常见的用法。
#[cfg(windows)]
fn parse_display_name(path: &Path) -> Option<*mut windows::Win32::UI::Shell::Common::ITEMIDLIST> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::IBindCtx;
    use windows::Win32::UI::Shell::{SHParseDisplayName, Common::ITEMIDLIST};

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `wide` 活得比这次调用长；`pidl` 是本函数栈上的出参，成功时被写上
    // 一块由 shell 分配、需要调用方释放的内存。
    let parsed = unsafe {
        SHParseDisplayName(
            PCWSTR(wide.as_ptr()),
            None::<&IBindCtx>,
            &mut pidl,
            0,
            None,
        )
    };
    if parsed.is_err() || pidl.is_null() {
        None
    } else {
        Some(pidl)
    }
}

/// 非 Windows 平台上没有资源管理器。
#[cfg(not(windows))]
pub fn reveal_file(_path: &Path) -> Result<(), String> {
    Err("当前平台没有资源管理器".to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn an_existing_directory_parses_into_a_pidl_and_a_missing_one_does_not() {
        // `SHParseDisplayName` 是纯解析、没有副作用，因此可以在这里测。
        // 存在的那条用系统目录（每台机器都有），不存在的那条用一条永远不存在的
        // 路径——测试**不依赖真机上的任何业务文件**。
        let here = Path::new(r"C:\Windows");
        if here.is_dir() {
            let pidl = parse_display_name(here);
            assert!(pidl.is_some(), "系统目录应当能被 shell 解析出来");
            if let Some(pidl) = pidl {
                // 解析出来的内存归我们，测完要还回去，否则这条测试自己漏内存。
                unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _)) };
            }
        }

        let missing = Path::new(r"Z:\wwp-core-test\this\path\does\not\exist\a.jpg");
        assert!(!missing.exists(), "这条测试要求 Z: 上真的没有这个目录");
        assert!(parse_display_name(missing).is_none());
    }

    #[test]
    fn a_path_without_any_directory_is_an_error_rather_than_a_panic() {
        // 连父目录都没有（真机上=整个盘被拔掉）时必须返回一句能给用户看的话。
        // 这条路径不存在父目录，因此**不会真的去打开资源管理器**，没有副作用。
        let missing = Path::new(r"Z:\wwp-core-test\this\path\does\not\exist\a.jpg");
        let message = reveal_file(missing).unwrap_err();
        assert!(
            message.contains("都已不存在"),
            "错误信息应当说明文件和目录都没了，实际是：{message}"
        );
    }
}

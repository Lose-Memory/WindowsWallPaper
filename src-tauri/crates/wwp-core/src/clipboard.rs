//! 往系统剪贴板写一段文本。
//!
//! ## 为什么放在引擎里，而不是用前端的剪贴板 API
//!
//! 前端可以调 `navigator.clipboard.writeText`，但它在 WebView2 里需要页面有焦点、
//! 依赖权限与用户手势，失败时既不抛错也不写进任何东西——表现成"点了没反应"。
//! 而 `OpenClipboard` / `SetClipboardData` 是 Windows 自带的能力，用项目已有的
//! `windows` 依赖就能调，成败都是明确的返回值。
//!
//! ## 剪贴板是全局独占资源
//!
//! 别的程序（输入法、剪贴板管理器、Office）可能正短暂占着它，这时 `OpenClipboard`
//! 会失败。因此这里**等一小会儿并重试几次**，而不是一次失败就报错：
//! "点第一次失败、点第二次成功"是最让人困惑的行为，而重试几乎总能成功。
//!
//! ## 为什么把 `CF_UNICODETEXT` 写成字面量 13
//!
//! `windows` crate 把这个常量放在 `Win32::System::Ole` 模块下，而本 crate 为了
//! 一个常量去启用整个 Ole 模块并不划算（多一份编译面）。它的值由 Win32 头文件
//! 固定（`CF_UNICODETEXT = 13`），写在这里并在测试里核对它的用法即可。

#[cfg(windows)]
pub fn set_text(text: &str) -> Result<(), String> {
    use std::time::Duration;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    /// `CF_UNICODETEXT`。见模块文档。
    const CF_UNICODETEXT: u32 = 13;

    // 打开剪贴板。被别的程序占着时等一会儿再试。
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened {
        return Err("剪贴板正被别的程序占用（等了一会儿还是打不开）".to_string());
    }

    // 从这里开始，无论成败都必须 CloseClipboard——不关掉的话**整个系统**的
    // 剪贴板会一直锁着，别的程序复制粘贴都会失败。
    let result = (|| -> Result<(), String> {
        unsafe { EmptyClipboard() }
            .map_err(|e| format!("清空剪贴板失败：{e}"))?;

        // Win32 的剪贴板文本是**以 NUL 结尾的 UTF-16**，长度含结尾的那个 0。
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes = wide.len() * std::mem::size_of::<u16>();

        // 必须用 GMEM_MOVEABLE：剪贴板要求内存块可以被系统搬动，
        // GMEM_FIXED 在这里是**无效的**（SetClipboardData 会失败）。
        let memory: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
            .map_err(|e| format!("为剪贴板分配内存失败：{e}"))?;

        let target = unsafe { GlobalLock(memory) } as *mut u16;
        if target.is_null() {
            unsafe {
                let _ = GlobalFree(Some(memory));
            }
            return Err("锁定剪贴板内存失败".to_string());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
            // 解锁之后系统才能接管这块内存。
            let _ = GlobalUnlock(memory);
        }

        // 成功后这块内存的所有权归系统，**不能再 GlobalFree**（那会让剪贴板
        // 指向一块已被释放的内存）。失败时才是我们的，必须自己释放。
        match unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) } {
            Ok(_) => Ok(()),
            Err(error) => {
                unsafe {
                    let _ = GlobalFree(Some(memory));
                }
                Err(format!("写入剪贴板失败：{error}"))
            }
        }
    })();

    unsafe {
        let _ = CloseClipboard();
    }
    result
}

#[cfg(not(windows))]
pub fn set_text(_text: &str) -> Result<(), String> {
    Err("当前平台没有剪贴板实现".to_string())
}

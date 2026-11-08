//! 让用户选一个文件夹。
//!
//! ## 为什么用系统自带的 `SHBrowseForFolderW`
//!
//! Tauri 官方做法是装 `tauri-plugin-dialog` 插件。但那会**下载新的依赖**，
//! 而本项目的约定是"需要的资源都放到项目本地、不在项目之外产生下载行为"。
//!
//! `SHBrowseForFolderW` 是 Windows 自带的文件夹选择对话框
//! （shell32.dll 里的老 API），用已有的 `windows` 依赖就能调，
//! 不需要任何新东西。它的外观比新的 `IFileDialog` 旧一些，
//! 但功能上完全够用，而且**没有任何额外依赖**。
//!
//! ## 一个容易踩的点：初始目录
//!
//! `BROWSEINFOW` 没有"初始目录"这个字段，只能通过回调
//! （`BFFM_INITIALIZED` 时发 `BFFM_SETSELECTIONW`）去设。
//! 不加这一步的话，每次打开都从"桌面"起步——用户上次选过的位置不会被记住，
//! 而图库路径往往在很深的目录里，每次都要点很多层。

use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::HWND;

/// 取一个可以当对话框属主的窗口句柄（拿不到就是"无属主"）。
///
/// ## 为什么需要它
///
/// `SHBrowseForFolderW` / `GetOpenFileNameW` 不设属主时，对话框是一个**无主**
/// 的独立窗口：它可能出现在别的窗口后面，而我们的界面此时看起来像卡住了。
/// 用户报过"有时候无法设置 NSFW 壁纸"，现象之一正是"点了「选择图片…」，
/// 什么都没发生"。
///
/// ## 为什么只认自己进程的窗口
///
/// 拿**别的进程**的窗口当属主是不安全的（属主关系假设两者在同一 UI 线程上，
/// 跨进程会出现奇怪的焦点与置顶行为）。因此只在前台窗口确实属于本进程时才用它，
/// 否则退回"无属主"——那至少不比原来差。
fn owner_window() -> HWND {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.0.is_null() {
            return HWND::default();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            foreground
        } else {
            HWND::default()
        }
    }
}

/// 弹出"选择文件夹"对话框。用户取消时返回 `None`。
///
/// `initial` 是打开时的起始目录；不存在或不可读时会被忽略
/// （对话框从默认位置开始，而不是报错——用户只是想选个目录，
/// 不该因为上次那个被删了就打不开对话框）。
pub fn pick_folder(initial: Option<&std::path::Path>) -> Result<Option<String>, String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BFFM_INITIALIZED, BFFM_SETSELECTIONW,
        BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    // 对话框标题与"起始目录"都要是**以 NUL 结尾的宽字符串**，
    // 而且必须活到对话框关闭为止——放进调用者的栈帧里即可。
    let title: Vec<u16> = "选择图库文件夹\0".encode_utf16().collect();

    // 起始目录：只有确实是个存在的目录才传给回调。
    // 传一个不存在的路径给 BFFM_SETSELECTIONW 会让对话框停在奇怪的位置。
    let initial_wide: Option<Vec<u16>> = initial
        .filter(|p| p.is_dir())
        .map(|p| {
            let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
            wide.push(0);
            wide
        });

    // 回调：对话框初始化完成时把起始目录设进去。
    //
    // 这个回调会被 shell 在自己的线程上调用，`lparam` 是我们传进去的
    // 宽字符串指针。它指向调用者栈帧里的数据，而 `SHBrowseForFolderW`
    // 是**同步**的（返回时对话框已经关了），因此指针在整个期间都有效。
    unsafe extern "system" fn on_message(
        hwnd: HWND,
        message: u32,
        _lparam: LPARAM,
        data: LPARAM,
    ) -> i32 {
        if message == BFFM_INITIALIZED && data.0 != 0 {
            // BFFM_SETSELECTIONW 的 wParam 为真表示 lParam 是字符串路径
            // （为假则是一个 PIDL）。
            SendMessageW(
                hwnd,
                BFFM_SETSELECTIONW,
                Some(windows::Win32::Foundation::WPARAM(1)),
                Some(LPARAM(data.0)),
            );
        }
        0
    }

    let mut display_name = [0u16; 260];
    let browse = BROWSEINFOW {
        // 属主用前台窗口（且必须是本进程的）：否则对话框可能跑到别的窗口后面，
        // 用户看起来像"点了没反应"。见 `owner_window`。
        hwndOwner: owner_window(),
        pidlRoot: std::ptr::null_mut(),
        pszDisplayName: PWSTR(display_name.as_mut_ptr()),
        lpszTitle: PCWSTR(title.as_ptr()),
        // BIF_NEWDIALOGSTYLE：用新的对话框样式（可以拖动、可以新建文件夹）。
        // 不给它的话是那种很老的小框，而且不能改大小。
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        lpfn: Some(on_message),
        lParam: LPARAM(
            initial_wide
                .as_ref()
                .map(|w| w.as_ptr() as isize)
                .unwrap_or(0),
        ),
        iImage: 0,
    };

    unsafe {
        let pidl = SHBrowseForFolderW(&browse);
        if pidl.is_null() {
            // 用户取消：不是错误。
            return Ok(None);
        }
        let mut path_buffer = [0u16; 260];
        let ok = SHGetPathFromIDListW(pidl, &mut path_buffer);
        // 释放 shell 分配的内存。不释放就是一次泄漏——虽然只泄漏几十字节，
        // 但"每次开对话框都漏"没有理由留着。
        windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
        if !ok.as_bool() {
            return Err("无法把所选位置转换成路径".to_string());
        }
        let end = path_buffer
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(path_buffer.len());
        let path = String::from_utf16_lossy(&path_buffer[..end]);
        if path.is_empty() {
            return Ok(None);
        }
        Ok(Some(path))
    }
}

/// 图片文件对话框的过滤器：`显示名\0模式\0...\0\0`。
///
/// `OPENFILENAMEW` 要的就是这种"双 NUL 结尾"的宽字符串数组，
/// 而且**不能**是 Rust 的 `Vec<u16>` 拼接后再借用——它必须活到调用结束，
/// 因此下面把它放在调用者的栈帧里。
fn image_filter() -> Vec<u16> {
    let text = "图片 (*.jpg;*.jpeg;*.png;*.bmp;*.webp)\0*.jpg;*.jpeg;*.png;*.bmp;*.webp\0所有文件 (*.*)\0*.*\0\0";
    text.encode_utf16().collect()
}

/// 弹出"选择一张图片"对话框。用户取消时返回 `None`。
///
/// 用于 NSFW 壁纸与固定单图：`SHBrowseForFolderW` 只能选目录，因此这里用
/// `GetOpenFileNameW`（comdlg32 里的老 API，同样是系统自带、不需要新依赖）。
pub fn pick_image(initial: Option<&std::path::Path>) -> Result<Option<String>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OPENFILENAMEW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST,
    };

    let mut filter = image_filter();
    // 文件名缓冲区：256 个宽字符足够，与对话框的经典上限一致。
    let mut file_buffer = [0u16; 1024];

    // 起始目录：把"文件初始名"设成初始路径，对话框就会停在那里。
    let initial_wide: Option<Vec<u16>> = initial
        .filter(|p| p.exists())
        .map(|p| {
            let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
            wide.push(0);
            wide
        });
    if let Some(wide) = &initial_wide {
        let copy = wide.len().min(file_buffer.len());
        file_buffer[..copy].copy_from_slice(&wide[..copy]);
    }

    let title: Vec<u16> = "选择一张图片\0".encode_utf16().collect();

    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        // 属主：与 `pick_folder` 同一个理由（对话框跑到后面 = 看起来没反应）。
        hwndOwner: owner_window(),
        lpstrFilter: PCWSTR(filter.as_mut_ptr()),
        nFilterIndex: 1,
        lpstrFile: windows::core::PWSTR(file_buffer.as_mut_ptr()),
        nMaxFile: file_buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        // OFN_FILEMUSTEXIST：用户手打的路径必须真的存在，否则对话框自己会拦下来；
        // 这比我们事后报"文件读不到"友好得多。
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };

    let opened = unsafe { GetOpenFileNameW(&mut options) };
    if !opened.as_bool() {
        // 用户取消：不是错误。`CommDlgExtendedError` 返回 0 就是取消，
        // 但把"取消"和"出错"都当取消更简单，代价只是错误信息少一条。
        return Ok(None);
    }

    let end = file_buffer
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(file_buffer.len());
    let path = String::from_utf16_lossy(&file_buffer[..end]);
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(path))
}

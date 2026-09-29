//! Windows 平台调用

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr;

#[link(name = "wininet")]
unsafe extern "system" {
    fn InternetSetOptionW(
        h_internet: *mut core::ffi::c_void,
        option: u32,
        buffer: *mut core::ffi::c_void,
        length: u32,
    ) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn IsUserAnAdmin() -> i32;
    fn ShellExecuteW(
        hwnd: *mut core::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut core::ffi::c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    /// 列出挂在当前控制台上的进程；只返回本进程 = 这个控制台是专为本进程创建的
    fn GetConsoleProcessList(process_list: *mut u32, count: u32) -> u32;
}

#[link(name = "msvcrt")]
unsafe extern "system" {
    /// 读一个键：不需要回车、不回显（回显的那个是 `_getche`）
    fn _getch() -> i32;
}

const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
const INTERNET_OPTION_REFRESH: u32 = 37;

/// 这个控制台会不会随进程一起消失？
///
/// 双击 exe、或者 `ShellExecuteW(runas)` 提权后新开的窗口，控制台里只有本进程，
/// 进程一退窗口就没了 —— 用户看不到任何输出。而在 cmd / pwsh / `cargo run` 里启动时，
/// shell 自己也挂在这个控制台上，窗口会留着，就不该多停。
pub fn console_would_vanish() -> bool {
    let mut list = [0u32; 2];
    unsafe { GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) <= 1 }
}

/// 阻塞等一个按键。
///
/// 注意：stdin 被重定向成管道/文件时（比如 `echo x | fetch_token.exe`），
/// `_getch` 会退化成从 stdin 读一个字节，不再是"等按键"。
pub fn wait_any_key() {
    unsafe {
        _getch();
    }
}

/// 改完注册表要通知 WinINet，否则有些程序不会立刻用新代理
pub fn notify_proxy_changed() {
    unsafe {
        InternetSetOptionW(ptr::null_mut(), INTERNET_OPTION_SETTINGS_CHANGED, ptr::null_mut(), 0);
        InternetSetOptionW(ptr::null_mut(), INTERNET_OPTION_REFRESH, ptr::null_mut(), 0);
    }
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

/// 用 runas 重新启动自己(会弹 UAC)
pub fn elevate() -> bool {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let params = args
        .iter()
        .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ");

    let wide = |s: &OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let operation = wide(OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let parameters = wide(OsStr::new(&params));

    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    result as isize > 32
}

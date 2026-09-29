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

const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
const INTERNET_OPTION_REFRESH: u32 = 37;

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

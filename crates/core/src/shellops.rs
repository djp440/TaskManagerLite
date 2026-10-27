//! 通过外壳打开文件位置、属性对话框与在线搜索。
//!
//! 参数的整理与校验是纯函数，可以脱离系统窗口测试；真正弹出窗口的只有最后的外壳调用。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, PCWSTR, w};


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// 路径不是绝对路径，或文件已不存在。
    NotFound,
    /// 外壳调用失败（含错误描述）。
    Failed(String),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellError::NotFound => f.write_str("文件已不存在"),
            ShellError::Failed(msg) if msg.trim().is_empty() => f.write_str("系统调用失败"),
            ShellError::Failed(msg) => write!(f, "系统调用失败：{}", msg.trim()),
        }
    }
}

/// 该线程第一次调用时初始化 COM（外壳 API 需要）；线程退出时由系统回收。
fn ensure_com() {
    thread_local! {
        static INIT: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        };
    }
    INIT.with(|_| {});
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// 校验要交给外壳的路径：必须是存在的绝对路径。
pub(crate) fn shell_target(path: &Path) -> Result<PathBuf, ShellError> {
    if path.is_absolute() && path.exists() { Ok(path.to_path_buf()) } else { Err(ShellError::NotFound) }
}

/// 在资源管理器中打开文件所在文件夹并选中该文件。
pub fn reveal_in_explorer(path: &Path) -> Result<(), ShellError> {
    let path = shell_target(path)?;
    ensure_com();
    let wide = wide(path.as_os_str());
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe {
        SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).map_err(|e| ShellError::Failed(e.message()))?;
        // 不给子项时，pidl 本身就是要在其父文件夹中选中的项。
        let r = SHOpenFolderAndSelectItems(pidl, None, 0);
        CoTaskMemFree(Some(pidl.cast()));
        r.map_err(|e| ShellError::Failed(e.message()))
    }
}

/// “属性”动词的外壳调用参数。`file` 须以 0 结尾，且在调用期间保持有效。
pub(crate) fn properties_info(file: &[u16]) -> SHELLEXECUTEINFOW {
    SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        lpVerb: w!("properties"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    }
}

/// 打开系统的文件属性对话框。
pub fn show_properties(path: &Path) -> Result<(), ShellError> {
    let path = shell_target(path)?;
    ensure_com();
    let file = wide(path.as_os_str());
    let mut info = properties_info(&file);
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        // 用户取消不算失败。
        Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(()),
        Err(e) => Err(ShellError::Failed(e.message())),
    }
}

/// 在线搜索进程名的网址。只包含进程名，不带任何其他数据。
pub fn search_url(process_name: &str) -> String {
    let mut q = String::new();
    for b in process_name.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => q.push(b as char),
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://www.bing.com/search?q={q}")
}

/// 用默认浏览器搜索进程名。
pub fn search_online(process_name: &str) -> Result<(), ShellError> {
    let url = wide(OsStr::new(&search_url(process_name)));
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // 返回值大于 32 表示成功。
    let code = r.0 as isize;
    if code > 32 { Ok(()) } else { Err(ShellError::Failed(format!("ShellExecute 返回 {code}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_as_chinese_text_not_debug() {
        assert_eq!(ShellError::NotFound.to_string(), "文件已不存在");
        assert_eq!(ShellError::Failed("拒绝访问".into()).to_string(), "系统调用失败：拒绝访问");
        assert_eq!(ShellError::Failed(String::new()).to_string(), "系统调用失败");
    }

    #[test]
    fn target_must_be_existing_absolute_path() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(shell_target(&exe), Ok(exe));
        assert_eq!(shell_target(Path::new(r"C:\no\such\file.exe")), Err(ShellError::NotFound));
        assert_eq!(shell_target(Path::new("relative.exe")), Err(ShellError::NotFound));
    }

    #[test]
    fn missing_files_are_rejected_before_any_window_opens() {
        let p = Path::new(r"C:\no\such\file.exe");
        assert_eq!(reveal_in_explorer(p), Err(ShellError::NotFound));
        assert_eq!(show_properties(p), Err(ShellError::NotFound));
    }

    #[test]
    fn properties_call_uses_properties_verb_and_invokeidlist() {
        let file = wide(OsStr::new(r"C:\Windows\notepad.exe"));
        let info = properties_info(&file);
        assert_eq!(unsafe { info.lpVerb.to_string() }.unwrap(), "properties");
        assert_eq!(unsafe { info.lpFile.to_string() }.unwrap(), r"C:\Windows\notepad.exe");
        assert_eq!(info.fMask & SEE_MASK_INVOKEIDLIST, SEE_MASK_INVOKEIDLIST);
        assert_eq!(info.cbSize as usize, size_of::<SHELLEXECUTEINFOW>());
    }

    #[test]
    fn search_url_only_contains_the_encoded_name() {
        assert_eq!(search_url("svchost.exe"), "https://www.bing.com/search?q=svchost.exe");
        assert_eq!(search_url(" My App&x=1.exe "), "https://www.bing.com/search?q=My+App%26x%3D1.exe");
        assert_eq!(search_url("微信.exe"), "https://www.bing.com/search?q=%E5%BE%AE%E4%BF%A1.exe");
    }
}

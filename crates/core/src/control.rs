//! 结束进程、关键进程判定与提权。

use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBreakOnTermination};
use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use crate::instance::WAIT_FOR_ARG;
use crate::nt::RawProcess;
use crate::snapshot::ProcessKey;
use crate::system::filetime;

pub(crate) struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<OwnedHandle> {
    unsafe { OpenProcess(access, false, pid) }.map(OwnedHandle)
}

pub(crate) fn create_time(h: &OwnedHandle) -> Option<u64> {
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    unsafe { GetProcessTimes(h.0, c, e, k, u) }.ok()?;
    Some(filetime(t[0]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndError {
    /// 进程已退出，或 PID 已被别的进程复用。
    Gone,
    /// 权限不足，通常需要提权。
    AccessDenied,
    Other(String),
}

/// 句柄指向的是否就是 `key` 这个进程，且仍在运行。
pub(crate) fn is_alive(h: &OwnedHandle, key: ProcessKey) -> bool {
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    create_time(h) == Some(key.create_time)
        && unsafe { GetExitCodeProcess(h.0, &mut code) }.is_ok()
        && code == STILL_ACTIVE
}

/// 拒绝访问时区分“确实没权限”和“进程其实已经退出”：
/// 已退出但仍被引用的进程对象同样会报拒绝访问。
fn denied_or_gone(key: ProcessKey) -> EndError {
    match open(key.pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) if !is_alive(&h, key) => EndError::Gone,
        Err(e) if e.code() != ERROR_ACCESS_DENIED.to_hresult() => EndError::Gone,
        _ => EndError::AccessDenied,
    }
}

/// 结束进程。先核对创建时间，防止误杀复用了同一 PID 的新进程。
pub fn end_process(key: ProcessKey) -> Result<(), EndError> {
    let h = match open(key.pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) => h,
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => return Err(denied_or_gone(key)),
        Err(_) => return Err(EndError::Gone),
    };
    if !is_alive(&h, key) {
        return Err(EndError::Gone);
    }
    unsafe { TerminateProcess(h.0, 1) }.map_err(|e| {
        if e.code() == ERROR_ACCESS_DENIED.to_hresult() { denied_or_gone(key) } else { EndError::Other(e.message()) }
    })
}

/// 读不到系统标记时（普通权限下打不开受保护进程）兜底使用的名单，只在会话 0 生效。
const PROTECTED_FALLBACK: &[&str] =
    &["system", "registry", "smss.exe", "csrss.exe", "wininit.exe", "winlogon.exe", "services.exe", "lsass.exe"];

/// 关键进程：系统标记了“终止即崩溃”的进程，以及本程序自身和 explorer.exe。
pub fn is_critical(raw: &RawProcess) -> bool {
    let name = raw.name.to_ascii_lowercase();
    if raw.pid == std::process::id() || name == "explorer.exe" || raw.pid == 4 {
        return true;
    }
    match open(raw.pid, PROCESS_QUERY_INFORMATION) {
        Ok(h) => {
            let mut flag = 0u32;
            let status = unsafe {
                NtQueryInformationProcess(
                    h.0,
                    ProcessBreakOnTermination,
                    (&mut flag as *mut u32).cast(),
                    4,
                    std::ptr::null_mut(),
                )
            };
            if status.is_ok() { flag != 0 } else { raw.session_id == 0 && PROTECTED_FALLBACK.contains(&name.as_str()) }
        }
        Err(_) => raw.session_id == 0 && PROTECTED_FALLBACK.contains(&name.as_str()),
    }
}

pub fn is_elevated() -> bool {
    unsafe { IsUserAnAdmin() }.as_bool()
}

/// 以管理员身份重新启动本程序。用户在 UAC 中拒绝时返回 false。
/// 新实例带 `--wait-for <本进程 PID>`：本进程此刻还没退出、仍占着单实例互斥量，新实例要等本进程退出后再取锁。
pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = HSTRING::from(exe.as_os_str());
    let args = HSTRING::from(format!("{WAIT_FOR_ARG} {}", std::process::id()));
    let r = unsafe { ShellExecuteW(None, w!("runas"), &exe, &args, PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as isize > 32
}

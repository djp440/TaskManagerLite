//! 按需读取单个进程的静态信息（可执行文件路径等）。
//!
//! 这些读取都以 `PROCESS_QUERY_LIMITED_INFORMATION` 打开进程，普通权限即可读取绝大多数进程；
//! 受保护进程（System 等）失败时返回 `None`。它们不在采样热路径上，只在需要时调用。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

use crate::snapshot::ProcessKey;
use crate::system::filetime;

/// 以受限查询权限打开的进程句柄，离开作用域时关闭。
pub(crate) struct ProcessHandle(pub(crate) HANDLE);

impl ProcessHandle {
    pub(crate) fn open(pid: u32) -> Option<Self> {
        // pid 0（空闲进程）与 4（System）没有可读的映像文件，System 通常也会被拒绝。
        if pid == 0 {
            return None;
        }
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(Self)
    }

    /// 句柄对应进程的创建时间（FILETIME，100ns）。
    pub(crate) fn create_time(&self) -> Option<u64> {
        let mut t = [FILETIME::default(); 4];
        let [c, e, k, u] = &mut t;
        unsafe { GetProcessTimes(self.0, c, e, k, u) }.ok()?;
        Some(filetime(t[0]))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn query_image_path(h: &ProcessHandle) -> Option<PathBuf> {
    // 32768 个 UTF-16 单元是 Windows 路径的上限，一次读够。
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let path = PathBuf::from(OsString::from_wide(&buf[..len as usize]));
    // 只接受绝对路径：个别内核进程会返回不是文件的名称。
    path.is_absolute().then_some(path)
}

/// 进程的可执行文件完整路径（Win32 格式）。进程已退出、权限不足或没有映像文件时返回 `None`。
///
/// 只凭 PID 查询，PID 被复用时可能读到别的进程；需要精确身份时用 [`image_path_of`]。
pub fn image_path(pid: u32) -> Option<PathBuf> {
    query_image_path(&ProcessHandle::open(pid)?)
}

/// 同 [`image_path`]，但校验创建时间，PID 已被别的进程复用时返回 `None`。
pub fn image_path_of(key: ProcessKey) -> Option<PathBuf> {
    let h = ProcessHandle::open(key.pid)?;
    if h.create_time()? != key.create_time {
        return None;
    }
    query_image_path(&h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_path_matches_current_exe() {
        let want = std::env::current_exe().unwrap();
        let got = image_path(std::process::id()).expect("应能读到本进程路径");
        assert_eq!(got.file_name(), want.file_name());
        assert!(got.is_absolute());
    }

    #[test]
    fn pid_zero_has_no_path() {
        assert_eq!(image_path(0), None);
    }

    #[test]
    fn mismatched_create_time_is_rejected() {
        let key = ProcessKey { pid: std::process::id(), create_time: 1 };
        assert_eq!(image_path_of(key), None);
    }
}

//! 按需读取单个进程的静态信息（可执行文件路径等）。
//!
//! 这些读取都以 `PROCESS_QUERY_LIMITED_INFORMATION` 打开进程，普通权限即可读取绝大多数进程；
//! 受保护进程（System 等）失败时返回 `None`。它们不在采样热路径上，只在需要时调用。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
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

/// `NtQueryInformationProcess` 返回的 UNICODE_STRING 头；字符串内容紧随其后，位于同一块缓冲区内。
#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005_u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023_u32 as i32;

fn query_command_line(h: &ProcessHandle) -> Option<String> {
    let mut needed = 0u32;
    let first = unsafe { NtQueryInformationProcess(h.0, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut needed) };
    if ![STATUS_INFO_LENGTH_MISMATCH, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL].contains(&first.0) {
        // 权限不足（受保护进程）或不支持。
        return None;
    }
    // 命令行在两次调用之间不会变长，但留出余量并最多重试几次。
    let mut size = (needed as usize).max(size_of::<UnicodeString>());
    for _ in 0..3 {
        // u64 缓冲保证对齐满足 UnicodeString。
        let mut buf = vec![0u64; size.div_ceil(8)];
        let mut got = 0u32;
        let status = unsafe {
            NtQueryInformationProcess(h.0, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut got)
        };
        if status.0 >= 0 {
            // SAFETY: 成功时缓冲区开头是 UnicodeString，buffer 指向同一块缓冲区内 length 字节的 UTF-16。
            let us = unsafe { &*buf.as_ptr().cast::<UnicodeString>() };
            if us.buffer.is_null() || us.length == 0 {
                return Some(String::new());
            }
            let base = buf.as_ptr() as usize;
            let at = us.buffer as usize;
            if at < base || at + us.length as usize > base + buf.len() * 8 {
                return None;
            }
            let units = unsafe { std::slice::from_raw_parts(us.buffer, us.length as usize / 2) };
            return Some(String::from_utf16_lossy(units));
        }
        if ![STATUS_INFO_LENGTH_MISMATCH, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL].contains(&status.0) {
            return None;
        }
        size = (got as usize).max(size * 2);
    }
    None
}

/// 进程的完整命令行。权限不足（受保护进程）、进程已退出或 PID 已被复用时返回 `None`。
/// 在右键菜单等需要时才调用，不进入采样路径。
pub fn command_line(key: ProcessKey) -> Option<String> {
    let h = ProcessHandle::open(key.pid)?;
    if h.create_time()? != key.create_time {
        return None;
    }
    query_command_line(&h)
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
    fn own_command_line_contains_own_exe_name() {
        let me = ProcessKey { pid: std::process::id(), create_time: own_create_time() };
        let line = command_line(me).expect("应能读到本进程命令行");
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_lowercase();
        assert!(line.to_lowercase().contains(&stem), "{line}");
    }

    fn own_create_time() -> u64 {
        ProcessHandle::open(std::process::id()).unwrap().create_time().unwrap()
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

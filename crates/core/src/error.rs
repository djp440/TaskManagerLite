//! 系统调用失败的结构化表示。核心库不返回面向界面的显示文案（见 issue #88）：
//! 错误只带原因与系统错误码，界面层（`tml-app`）按枚举映射成文案。

use std::{fmt, io};

/// 系统调用失败：保留系统错误码。
///
/// 描述文字不是本程序撰写的，而是操作系统按系统语言给出的（`io::Error::from_raw_os_error` 的 `Display`，
/// 或 COM 返回的 HRESULT 描述），界面层原样显示即可。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemError {
    /// Win32 错误码；描述文字在需要显示时才向系统查询。
    Win32(i32),
    /// 没有 Win32 错误码的系统错误（如 COM 的 HRESULT），带系统给出的描述。`code` 为 0 表示未知。
    Other { code: i32, message: String },
}

impl SystemError {
    /// Win32 错误码；没有时为 `None`。
    pub fn win32_code(&self) -> Option<i32> {
        match self {
            Self::Win32(code) => Some(*code),
            Self::Other { .. } => None,
        }
    }

    /// 是否为“拒绝访问”（`ERROR_ACCESS_DENIED`）。
    pub fn is_access_denied(&self) -> bool {
        self.win32_code() == Some(5)
    }

    /// 命令行子进程用作退出码：Win32 错误码，否则 1。
    pub fn exit_code(&self) -> i32 {
        self.win32_code().unwrap_or(1)
    }

    /// 来自 `windows` crate 的错误，描述取其 `Display`（含 HRESULT）。
    pub(crate) fn from_windows(e: &windows::core::Error) -> Self {
        Self::Other { code: e.code().0, message: e.to_string() }
    }
}

impl From<io::Error> for SystemError {
    fn from(e: io::Error) -> Self {
        match e.raw_os_error() {
            Some(code) => Self::Win32(code),
            None => Self::Other { code: 0, message: e.to_string() },
        }
    }
}

impl fmt::Display for SystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32(code) => write!(f, "{}", io::Error::from_raw_os_error(*code)),
            Self::Other { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for SystemError {}

/// 通过 UAC 启动提权子进程并等待结果时的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevationError {
    /// 启动、等待提权子进程的系统调用失败（含用户在 UAC 中取消），或子进程以非零退出码结束
    /// （约定退出码即 Win32 错误码）。
    System(SystemError),
    /// 提权子进程没有返回句柄。
    NoHandle,
    /// 等待提权子进程超时，结果未知。
    Timeout,
}

impl fmt::Display for ElevationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System(e) => write!(f, "elevated child failed: {e}"),
            Self::NoHandle => f.write_str("elevated child returned no handle"),
            Self::Timeout => f.write_str("timed out waiting for elevated child"),
        }
    }
}

impl std::error::Error for ElevationError {}

impl From<SystemError> for ElevationError {
    fn from(e: SystemError) -> Self {
        Self::System(e)
    }
}

impl From<io::Error> for ElevationError {
    fn from(e: io::Error) -> Self {
        Self::System(e.into())
    }
}

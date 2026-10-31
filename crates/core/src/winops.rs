//! 应用窗口操作（#70）：切换到、最大化、最小化、还原。
//!
//! 与其他核心模块一样不返回显示文案：失败用 [`WindowError`]（带系统错误码），文案由界面层按枚举映射。
//! 窗口句柄以 `isize` 传递（`HWND` 的裸值，由 [`crate::app_windows`] 在判定“应用”时顺带记录）；句柄会失效，
//! 每次探测与操作前都先 `IsWindow` 校验。
//!
//! - **UIPI**：普通权限进程无法向更高完整性级别（如提权应用）的窗口发窗口命令。探测时比较窗口所属进程与本进程的
//!   令牌完整性级别（[`needs_elevation`]）。实测提权应用的进程可以用 `PROCESS_QUERY_LIMITED_INFORMATION` 打开，
//!   但普通权限读它的令牌会被拒绝访问：这种“被拒绝访问”本身就说明目标在本程序之上，按需要提权处理
//!   （[`Integrity::Denied`]；代价是以另一个用户身份运行的同级窗口会被误判，这类窗口很少见）。
//!   其他原因读不出来（进程刚退出等，[`Integrity::Unknown`]）无法判断，保守地允许尝试，
//!   操作后若窗口状态没有变化则报 [`WindowError::NotApplied`]。
//! - **切换到**：先把最小化的窗口还原，再 `SetForegroundWindow`；被前台锁定拒绝时退化为任务栏闪烁
//!   （[`WindowOutcome::Flashed`]），不使用 `AttachThreadInput`、模拟按键等有副作用的绕过手段，
//!   依靠“用户刚点了本程序的菜单、本程序持有前台权限”这一正常途径。
//! - 操作结果不看 `ShowWindow` 的返回值（它返回的是“之前是否可见”），而是回读窗口状态核对（[`succeeded`]）。

use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, HWND};
use windows::core::HRESULT;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::WindowsAndMessaging::{
    FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsZoomed, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

use crate::control::open;
use crate::error::SystemError;

/// 完整性级别 RID（`SECURITY_MANDATORY_*_RID`）。
pub const INTEGRITY_MEDIUM: u32 = 0x2000;
pub const INTEGRITY_HIGH: u32 = 0x3000;

/// 读取进程完整性级别的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// 令牌里的完整性级别 RID。
    Level(u32),
    /// 打开进程或其令牌被拒绝访问：目标权限高于本程序。
    Denied,
    /// 因其他原因读不到（进程已退出等）。
    Unknown,
}

/// 窗口当前的显示状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Maximized,
    Minimized,
}

impl WindowState {
    /// 由 `IsIconic` / `IsZoomed` 得出；最小化优先（最小化前最大化的窗口 `IsZoomed` 也可能为真）。
    pub fn from_flags(iconic: bool, zoomed: bool) -> Self {
        if iconic {
            Self::Minimized
        } else if zoomed {
            Self::Maximized
        } else {
            Self::Normal
        }
    }
}

/// 对窗口的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOp {
    /// 切换到：最小化则先还原，再置于前台。
    Activate,
    Maximize,
    Minimize,
    Restore,
}

/// 操作的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowOutcome {
    Done,
    /// 仅 [`WindowOp::Activate`]：前台锁定拒绝了 `SetForegroundWindow`，已改为闪烁任务栏按钮。
    Flashed,
}

/// 窗口操作失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    /// 窗口句柄已失效（窗口已关闭）。
    Gone,
    /// 窗口属于更高完整性级别的进程（如提权应用），本程序受 UIPI 限制发不出命令。
    NeedsElevation,
    /// 系统接受了调用，但窗口状态没有变化：窗口不支持该操作（如固定大小的对话框），或被更高权限拒绝而无法预先判断。
    NotApplied,
    /// 其余系统调用失败，保留系统错误码。
    System(SystemError),
}

impl WindowError {
    /// 写入日志用的错误码；与系统错误码无关的原因为 0。
    pub fn code(&self) -> i32 {
        match self {
            Self::Gone | Self::NeedsElevation | Self::NotApplied => 0,
            Self::System(SystemError::Win32(code)) => *code,
            Self::System(SystemError::Other { code, .. }) => *code,
        }
    }
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => f.write_str("window is gone"),
            Self::NeedsElevation => f.write_str("target window has a higher integrity level"),
            Self::NotApplied => f.write_str("window state did not change"),
            Self::System(e) => write!(f, "window operation failed: {e}"),
        }
    }
}

impl std::error::Error for WindowError {}

/// 四个菜单项各自是否可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowOps {
    pub activate: bool,
    pub maximize: bool,
    pub minimize: bool,
    pub restore: bool,
}

/// 窗口状态到菜单可用性的映射（纯函数）：已最大化→“最大化”不可用；已最小化→“最小化”不可用；
/// 正常窗口→“还原”不可用；“切换到”任何状态都可用。
pub fn ops_enabled(state: WindowState) -> WindowOps {
    WindowOps {
        activate: true,
        maximize: state != WindowState::Maximized,
        minimize: state != WindowState::Minimized,
        restore: state != WindowState::Normal,
    }
}

/// 目标进程的完整性级别是否高于本进程（纯函数）。目标令牌被拒绝访问按“更高”处理；
/// 任何一边因其他原因读不到则无法判断，按“不需要”处理，即允许尝试；
/// 本进程已提权（High）时只有 System 级别（或同样读不到令牌）的目标才需要。
pub fn needs_elevation(target: Integrity, own: Integrity) -> bool {
    match (target, own) {
        (Integrity::Level(t), Integrity::Level(o)) => t > o,
        (Integrity::Denied, Integrity::Level(_)) => true,
        _ => false,
    }
}

/// 操作后回读到的状态是否算成功（纯函数）。`before` 为操作前的状态。
pub fn succeeded(op: WindowOp, before: WindowState, after: WindowState) -> bool {
    match op {
        WindowOp::Activate => after != WindowState::Minimized,
        WindowOp::Maximize => after == WindowState::Maximized,
        WindowOp::Minimize => after == WindowState::Minimized,
        // 最小化前最大化的窗口还原后会回到最大化，所以只要求离开了原来的状态。
        WindowOp::Restore => after != before,
    }
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut _)
}

fn state_of(h: HWND) -> WindowState {
    WindowState::from_flags(unsafe { IsIconic(h) }.as_bool(), unsafe { IsZoomed(h) }.as_bool())
}

/// 进程令牌的完整性级别 RID。
fn integrity_of_handle(process: HANDLE) -> Integrity {
    let mut token = HANDLE::default();
    if let Err(e) = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } {
        return denied_or_unknown(&e);
    }
    let rid = (|| {
        let mut len = 0u32;
        // 第一次调用只取所需长度，必然以 ERROR_INSUFFICIENT_BUFFER 失败。
        let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len) };
        if len == 0 {
            return None;
        }
        // u64 对齐，满足 TOKEN_MANDATORY_LABEL 的指针对齐要求。
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        unsafe { GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
        // SAFETY: 缓冲区刚被系统按 TOKEN_MANDATORY_LABEL 填充，其中的 SID 指针指向同一缓冲区内。
        let label = unsafe { &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
        let sid = label.Label.Sid;
        let count = unsafe { *GetSidSubAuthorityCount(sid) };
        if count == 0 {
            return None;
        }
        Some(unsafe { *GetSidSubAuthority(sid, u32::from(count) - 1) })
    })();
    let _ = unsafe { CloseHandle(token) };
    rid.map_or(Integrity::Unknown, Integrity::Level)
}

fn denied_or_unknown(e: &windows::core::Error) -> Integrity {
    if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) { Integrity::Denied } else { Integrity::Unknown }
}

/// 进程 `pid` 的完整性级别。
pub fn integrity_of_pid(pid: u32) -> Integrity {
    match open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(process) => integrity_of_handle(process.raw()),
        Err(e) => denied_or_unknown(&e),
    }
}

/// 本进程的完整性级别（进程生命周期内不变，只读一次）。
fn own_integrity() -> Integrity {
    static OWN: OnceLock<Integrity> = OnceLock::new();
    *OWN.get_or_init(|| integrity_of_handle(unsafe { GetCurrentProcess() }))
}

/// 窗口所属进程的 PID；窗口已失效为 0。
fn window_pid(h: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// 校验句柄并检查 UIPI：窗口已失效 → [`WindowError::Gone`]，属于更高完整性级别的进程 →
/// [`WindowError::NeedsElevation`]。
fn check(h: HWND) -> Result<(), WindowError> {
    if !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Err(WindowError::Gone);
    }
    if needs_elevation(integrity_of_pid(window_pid(h)), own_integrity()) {
        return Err(WindowError::NeedsElevation);
    }
    Ok(())
}

/// 菜单弹出前的探测：读一次窗口状态（`IsIconic` / `IsZoomed`），同时校验句柄与 UIPI。
pub fn probe_window(raw: isize) -> Result<WindowState, WindowError> {
    let h = hwnd(raw);
    check(h)?;
    Ok(state_of(h))
}

/// 等窗口状态变化：`ShowWindow` 对其他进程的窗口基本同步生效，个别应用（如 UWP 框架窗口）会晚一点。
fn wait_for(h: HWND, mut done: impl FnMut(WindowState) -> bool) -> bool {
    for i in 0..6 {
        if done(state_of(h)) {
            return true;
        }
        if i < 5 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    false
}

/// 对窗口执行操作。窗口失效、属于更高完整性级别的进程、或操作后状态没有变化时返回结构化错误。
pub fn apply(raw: isize, op: WindowOp) -> Result<WindowOutcome, WindowError> {
    let h = hwnd(raw);
    check(h)?;
    let before = state_of(h);
    match op {
        WindowOp::Maximize | WindowOp::Minimize | WindowOp::Restore => {
            let cmd = match op {
                WindowOp::Maximize => SW_MAXIMIZE,
                WindowOp::Minimize => SW_MINIMIZE,
                _ => SW_RESTORE,
            };
            // 返回值是“之前是否可见”，不代表成败。
            let _ = unsafe { ShowWindow(h, cmd) };
            if wait_for(h, |after| succeeded(op, before, after)) {
                Ok(WindowOutcome::Done)
            } else if !unsafe { IsWindow(Some(h)) }.as_bool() {
                Err(WindowError::Gone)
            } else {
                Err(WindowError::NotApplied)
            }
        }
        WindowOp::Activate => {
            if before == WindowState::Minimized {
                let _ = unsafe { ShowWindow(h, SW_RESTORE) };
                if !wait_for(h, |after| succeeded(op, before, after)) {
                    return Err(if unsafe { IsWindow(Some(h)) }.as_bool() { WindowError::NotApplied } else { WindowError::Gone });
                }
            }
            if unsafe { SetForegroundWindow(h) }.as_bool() {
                return Ok(WindowOutcome::Done);
            }
            // 前台锁定：退化为闪烁任务栏按钮直到用户切换过去。
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: h,
                dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                uCount: 3,
                dwTimeout: 0,
            };
            let _ = unsafe { FlashWindowEx(&info) };
            Ok(WindowOutcome::Flashed)
        }
    }
}

/// 窗口 `raw` 是否是当前前台窗口（测试与诊断用）。
pub fn is_foreground(raw: isize) -> bool {
    (unsafe { GetForegroundWindow() }) == hwnd(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_from_flags_prefers_minimized() {
        assert_eq!(WindowState::from_flags(false, false), WindowState::Normal);
        assert_eq!(WindowState::from_flags(false, true), WindowState::Maximized);
        assert_eq!(WindowState::from_flags(true, false), WindowState::Minimized);
        assert_eq!(WindowState::from_flags(true, true), WindowState::Minimized);
    }

    #[test]
    fn menu_availability_follows_window_state() {
        assert_eq!(
            ops_enabled(WindowState::Normal),
            WindowOps { activate: true, maximize: true, minimize: true, restore: false }
        );
        assert_eq!(
            ops_enabled(WindowState::Maximized),
            WindowOps { activate: true, maximize: false, minimize: true, restore: true }
        );
        assert_eq!(
            ops_enabled(WindowState::Minimized),
            WindowOps { activate: true, maximize: true, minimize: false, restore: true }
        );
    }

    #[test]
    fn higher_integrity_target_needs_elevation() {
        use Integrity::*;
        let (medium, high, system) = (Level(INTEGRITY_MEDIUM), Level(INTEGRITY_HIGH), Level(0x4000));
        // 普通权限本程序 vs 提权应用 / System。
        assert!(needs_elevation(high, medium));
        assert!(needs_elevation(system, medium));
        // 同级或更低：不需要。
        assert!(!needs_elevation(medium, medium));
        assert!(!needs_elevation(Level(0x1000), medium));
        // 本程序已提权：High 目标不需要，System 目标仍需要。
        assert!(!needs_elevation(high, high));
        assert!(!needs_elevation(medium, high));
        assert!(needs_elevation(system, high));
    }

    #[test]
    fn denied_token_means_higher_and_unknown_is_allowed_to_try() {
        use Integrity::*;
        let medium = Level(INTEGRITY_MEDIUM);
        // 目标令牌被拒绝访问（提权应用的典型表现）：需要提权。
        assert!(needs_elevation(Denied, medium));
        assert!(needs_elevation(Denied, Level(INTEGRITY_HIGH)));
        // 其他原因读不到：无法判断，允许尝试。
        assert!(!needs_elevation(Unknown, medium));
        assert!(!needs_elevation(Level(INTEGRITY_HIGH), Unknown));
        assert!(!needs_elevation(Unknown, Unknown));
        // 本程序自己的令牌读不到时没有比较基准，同样允许尝试。
        assert!(!needs_elevation(Denied, Unknown));
    }

    #[test]
    fn success_is_judged_by_readback_state() {
        use WindowState::*;
        assert!(succeeded(WindowOp::Maximize, Normal, Maximized));
        assert!(succeeded(WindowOp::Maximize, Minimized, Maximized));
        assert!(!succeeded(WindowOp::Maximize, Normal, Normal));
        assert!(succeeded(WindowOp::Minimize, Normal, Minimized));
        assert!(!succeeded(WindowOp::Minimize, Maximized, Maximized));
        assert!(succeeded(WindowOp::Restore, Maximized, Normal));
        assert!(succeeded(WindowOp::Restore, Minimized, Normal));
        assert!(succeeded(WindowOp::Restore, Minimized, Maximized), "最小化前最大化的窗口还原后回到最大化");
        assert!(!succeeded(WindowOp::Restore, Normal, Normal));
        assert!(succeeded(WindowOp::Activate, Minimized, Normal));
        assert!(!succeeded(WindowOp::Activate, Minimized, Minimized));
        assert!(succeeded(WindowOp::Activate, Normal, Normal));
    }

    #[test]
    fn error_codes_for_logging() {
        assert_eq!(WindowError::Gone.code(), 0);
        assert_eq!(WindowError::NeedsElevation.code(), 0);
        assert_eq!(WindowError::NotApplied.code(), 0);
        assert_eq!(WindowError::System(SystemError::Win32(5)).code(), 5);
        assert_eq!(WindowError::System(SystemError::Other { code: -2147024891, message: String::new() }).code(), -2147024891);
        assert_ne!(WindowError::Gone.to_string(), WindowError::NeedsElevation.to_string());
    }

    #[test]
    fn invalid_handle_is_gone() {
        assert_eq!(probe_window(0x7777_7777), Err(WindowError::Gone));
        assert_eq!(apply(0x7777_7777, WindowOp::Maximize), Err(WindowError::Gone));
        assert_eq!(apply(0, WindowOp::Activate), Err(WindowError::Gone));
    }

    #[test]
    fn own_process_integrity_is_readable() {
        let Integrity::Level(own) = integrity_of_pid(std::process::id()) else { panic!("本进程的令牌应能读到") };
        assert!(own >= 0x1000, "完整性级别至少为 Low，实际 {own:#x}");
        assert_eq!(own_integrity(), Integrity::Level(own));
    }
}

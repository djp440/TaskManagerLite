//! 判定“应用”：有可见顶层窗口的进程，即出现在任务栏 / Alt+Tab 的程序。
//!
//! 每次调用用 `EnumWindows` 遍历全部顶层窗口（约 1 ms，放在采样线程），窗口同时满足下列条件时，
//! 其所属进程记为应用：可见、没有所有者窗口（带 `WS_EX_APPWINDOW` 的除外：它会被强制显示在任务栏上）、
//! 不是工具窗口、有标题、没有被 DWM 隐藏。
//! UWP 应用的顶层窗口属于 `ApplicationFrameHost.exe`，真实进程要从它的
//! `Windows.UI.Core.CoreWindow` 子窗口取得。

use std::collections::HashSet;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GWL_EXSTYLE, GW_OWNER, GetClassNameW, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

const UWP_FRAME_CLASS: &str = "ApplicationFrameWindow";
const UWP_CORE_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// 判定一个顶层窗口用到的事实。
#[derive(Debug, Clone, Copy, Default)]
struct WindowFacts {
    visible: bool,
    has_owner: bool,
    tool_window: bool,
    /// `WS_EX_APPWINDOW`：强制显示在任务栏上，即使有所有者窗口或是工具窗口。
    app_window: bool,
    has_title: bool,
    /// `DWMWA_CLOAKED` 非 0：在其他虚拟桌面上，或是被挂起的 UWP 窗口。
    cloaked: bool,
}

/// 窗口是否算作“应用窗口”。
fn is_app_window(f: WindowFacts) -> bool {
    f.visible && (f.app_window || (!f.has_owner && !f.tool_window)) && f.has_title && !f.cloaked
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut value as *mut u32 as *mut _, size_of::<u32>() as u32) }
        .is_ok_and(|()| value != 0)
}

fn facts(hwnd: HWND) -> WindowFacts {
    // 先做最便宜、过滤力最强的检查：绝大多数顶层窗口不可见。
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    if !visible {
        return WindowFacts::default();
    }
    let has_owner = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|h| !h.is_invalid());
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let tool_window = ex_style & WS_EX_TOOLWINDOW.0 != 0;
    let app_window = ex_style & WS_EX_APPWINDOW.0 != 0;
    if (has_owner || tool_window) && !app_window {
        return WindowFacts { visible, has_owner, tool_window, ..Default::default() };
    }
    WindowFacts {
        visible,
        has_owner,
        tool_window,
        app_window,
        has_title: unsafe { GetWindowTextLengthW(hwnd) } > 0,
        cloaked: cloaked(hwnd),
    }
}

/// UWP 框架窗口里承载应用内容的 `CoreWindow` 所属进程。
fn uwp_pid(frame: HWND) -> Option<u32> {
    struct Search {
        frame_pid: u32,
        found: Option<u32>,
    }
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 是下面 `uwp_pid` 传入的 `&mut Search`，枚举期间一直有效。
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        if class_name(child) == UWP_CORE_CLASS {
            let pid = window_pid(child);
            if pid != search.frame_pid {
                search.found = Some(pid);
                return false.into();
            }
        }
        true.into()
    }
    let mut search = Search { frame_pid: window_pid(frame), found: None };
    // 找到即中止枚举时 EnumChildWindows 返回 FALSE，属正常。
    let _ = unsafe { EnumChildWindows(Some(frame), Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

/// 当前所有应用进程的 PID。
pub fn app_pids() -> HashSet<u32> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 是 `app_pids` 传入的 `&mut HashSet<u32>`，枚举期间一直有效。
        let pids = unsafe { &mut *(lparam.0 as *mut HashSet<u32>) };
        if is_app_window(facts(hwnd)) {
            if class_name(hwnd) == UWP_FRAME_CLASS {
                // 找不到 CoreWindow（正在启动或退出）时不记录：框架宿主本身不是应用。
                if let Some(pid) = uwp_pid(hwnd) {
                    pids.insert(pid);
                }
            } else {
                pids.insert(window_pid(hwnd));
            }
        }
        true.into()
    }
    let mut pids = HashSet::new();
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut pids as *mut HashSet<u32> as isize)) };
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown() -> WindowFacts {
        WindowFacts { visible: true, has_owner: false, tool_window: false, app_window: false, has_title: true, cloaked: false }
    }

    #[test]
    fn plain_window_is_app_window() {
        assert!(is_app_window(shown()));
    }

    #[test]
    fn each_condition_excludes() {
        assert!(!is_app_window(WindowFacts { visible: false, ..shown() }));
        assert!(!is_app_window(WindowFacts { has_owner: true, ..shown() }));
        assert!(!is_app_window(WindowFacts { tool_window: true, ..shown() }));
        assert!(!is_app_window(WindowFacts { has_title: false, ..shown() }));
        assert!(!is_app_window(WindowFacts { cloaked: true, ..shown() }));
    }

    #[test]
    fn appwindow_style_forces_taskbar_presence() {
        // 有所有者、但带 WS_EX_APPWINDOW 的窗口会出现在任务栏：算应用；其他条件照常适用。
        let owned = WindowFacts { has_owner: true, ..shown() };
        assert!(!is_app_window(owned));
        assert!(is_app_window(WindowFacts { app_window: true, ..owned }));
        assert!(is_app_window(WindowFacts { app_window: true, tool_window: true, ..owned }));
        assert!(!is_app_window(WindowFacts { app_window: true, visible: false, ..owned }));
        assert!(!is_app_window(WindowFacts { app_window: true, has_title: false, ..owned }));
        assert!(!is_app_window(WindowFacts { app_window: true, cloaked: true, ..owned }));
    }
}

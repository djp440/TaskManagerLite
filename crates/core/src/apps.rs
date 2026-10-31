//! 判定“应用”：有可见顶层窗口的进程，即出现在任务栏 / Alt+Tab 的程序。
//!
//! 每次调用用 `EnumWindows` 遍历全部顶层窗口（约 1 ms，放在采样线程），窗口同时满足下列条件时，
//! 其所属进程记为应用：可见、没有所有者窗口（带 `WS_EX_APPWINDOW` 的除外：它会被强制显示在任务栏上）、
//! 不是工具窗口、有标题、没有被 DWM 隐藏。其他虚拟桌面上的窗口也被 DWM 隐藏（`DWM_CLOAKED_SHELL`），
//! 与挂起的 UWP 窗口无法靠 cloaked 值区分，所以对“被外壳隐藏”的候选窗口再问一次
//! `IVirtualDesktopManager::IsWindowOnCurrentVirtualDesktop`：明确不在当前桌面的仍算应用。
//! 第一次调用该接口会把 `twinapi.dll` 及其依赖拉进进程（常驻工作集约 +4 MB），所以只有注册表显示
//! 用户确实有至少两个虚拟桌面时才去问：只有一个桌面的机器（绝大多数）完全不会触碰 COM。
//! UWP 应用的顶层窗口属于 `ApplicationFrameHost.exe`，真实进程要从它的
//! `Windows.UI.Core.CoreWindow` 子窗口取得。
//!
//! 判定时顺带记下每个应用的“主窗口”句柄（#70 窗口操作用）：`EnumWindows` 按 Z 序从上到下枚举，
//! 一个进程的第一个应用窗口即最上层的那个；UWP 应用记的是 `ApplicationFrameHost` 的外层
//! `ApplicationFrameWindow`（`ShowWindow` 等要作用于外层窗口，`CoreWindow` 只是里面的内容）。
//! 只多一次 `HashMap` 插入，不增加任何系统调用。句柄会失效，使用前由 [`crate::winops`] 校验。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND, LPARAM, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GWL_EXSTYLE, GW_OWNER, GetClassNameW, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, w};

const UWP_FRAME_CLASS: &str = "ApplicationFrameWindow";
const UWP_CORE_CLASS: &str = "Windows.UI.Core.CoreWindow";
/// `DWM_CLOAKED_SHELL`：被外壳隐藏（其他虚拟桌面、挂起的 UWP）。
const DWM_CLOAKED_SHELL: u32 = 2;
/// 创建 / 查询失败后的退避：首次 1 秒，之后每次连续失败翻倍，最长 60 秒。
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

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
    /// 被外壳隐藏、并且虚拟桌面管理器明确说不在当前桌面：只是在别的桌面上，窗口本身是正常的。
    other_desktop: bool,
}

/// 窗口是否算作“应用窗口”。
fn is_app_window(f: WindowFacts) -> bool {
    f.visible && (f.app_window || (!f.has_owner && !f.tool_window)) && f.has_title && (!f.cloaked || f.other_desktop)
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

/// `DWMWA_CLOAKED` 的取值，读不到时为 0。
fn cloaked_value(hwnd: HWND) -> u32 {
    let mut value = 0u32;
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut value as *mut u32 as *mut _, size_of::<u32>() as u32) }
        .map_or(0, |()| value)
}

/// 采样线程上的虚拟桌面查询：COM 与 `IVirtualDesktopManager` 都在第一次需要时才创建，
/// 且每个线程只创建一次（接口跟着线程的套间走）。没有 cloaked 的候选窗口时永远不会触碰 COM，
/// 所以不用分组视图、或没有被隐藏窗口的机器上不会多加载任何 DLL。
#[derive(Default)]
struct DesktopProbe {
    /// 先于 `com_initialized` 的清理释放（`Drop` 里显式按顺序处理）。
    manager: Option<IVirtualDesktopManager>,
    com_initialized: bool,
    com_usable: bool,
    /// COM 初始化只做一次（失败重试创建管理器时不再重复初始化，否则引用计数对不上）。
    com_tried: bool,
    backoff: Backoff,
    /// 本轮枚举里“是否存在多个虚拟桌面”的缓存；每轮 `app_pids` 开头清空，第一次需要时才读注册表。
    multiple_desktops: Option<bool>,
}

impl DesktopProbe {
    fn manager(&mut self) -> Option<&IVirtualDesktopManager> {
        let now = Instant::now();
        if self.manager.is_none() && self.backoff.allowed(now) {
            if !self.com_tried {
                self.com_tried = true;
                // 多线程套间：没有消息泵也能用。线程已用别的模型初始化过（RPC_E_CHANGED_MODE）时沿用，不由我们反初始化。
                let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                self.com_initialized = init.is_ok();
                self.com_usable = init.is_ok() || init == RPC_E_CHANGED_MODE;
            }
            self.manager = if self.com_usable {
                unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER) }.ok()
            } else {
                None
            };
            // 创建失败（如开机自启时外壳尚未就绪）只是暂时退避，之后会再试。创建成功不清零：
            // 否则某个窗口的查询持续失败时会形成“创建成功→查询失败→退避 1 秒→再创建”的循环，退避爬不上去；
            // 只有查询成功（`on_other_desktop`）才算恢复。
            if self.manager.is_none() {
                self.backoff.failed(now);
            }
        }
        self.manager.as_ref()
    }

    /// 窗口是否明确不在当前虚拟桌面。查询失败（窗口已销毁、外壳重启、没有管理器）一律当作“不是”。
    fn on_other_desktop(&mut self, hwnd: HWND) -> bool {
        if !*self.multiple_desktops.get_or_insert_with(multiple_desktops) {
            return false;
        }
        let Some(manager) = self.manager() else { return false };
        match unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) } {
            Ok(on_current) => {
                self.backoff.succeeded();
                !on_current.as_bool()
            }
            Err(_) => {
                // 外壳重启后旧接口会一直失败：丢弃它，按退避时间重新创建。
                self.manager = None;
                self.backoff.failed(Instant::now());
                false
            }
        }
    }
}

/// 创建 / 查询失败后的退避：只计“连续”失败，成功一次就清零；没有永久放弃的上限，只是两次尝试之间隔得越来越久。
#[derive(Debug, Default)]
struct Backoff {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Backoff {
    fn allowed(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|t| now >= t)
    }

    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let delay = BACKOFF_BASE.saturating_mul(1u32 << (self.failures - 1).min(6)).min(BACKOFF_MAX);
        self.retry_at = Some(now + delay);
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

impl Drop for DesktopProbe {
    fn drop(&mut self) {
        self.manager = None;
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static DESKTOP: RefCell<DesktopProbe> = RefCell::new(DesktopProbe::default());
}

/// 用户是否有至少两个虚拟桌面。资源管理器把桌面 ID 按 16 字节 GUID 依次写在
/// `HKCU\...\Explorer\VirtualDesktops\VirtualDesktopIDs`；没有这个值说明从未创建过额外的桌面。
/// 读取出错（不是“不存在”）时无从判断，按“有”处理，交给虚拟桌面管理器回答。
fn multiple_desktops() -> bool {
    let mut bytes = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops"),
            w!("VirtualDesktopIDs"),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&mut bytes),
        )
    };
    desktops_from_registry(status == ERROR_SUCCESS, status == ERROR_FILE_NOT_FOUND, bytes)
}

/// `multiple_desktops` 的判断部分：值存在时看 GUID 个数，不存在为单桌面，其他错误按多桌面处理。
fn desktops_from_registry(found: bool, missing: bool, bytes: u32) -> bool {
    if found {
        bytes / 16 >= 2
    } else {
        !missing
    }
}

/// 被外壳隐藏的窗口是否只是在别的虚拟桌面上。
fn on_other_desktop(hwnd: HWND) -> bool {
    DESKTOP.with(|d| d.borrow_mut().on_other_desktop(hwnd))
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
    let has_title = unsafe { GetWindowTextLengthW(hwnd) } > 0;
    let cloaked = if has_title { cloaked_value(hwnd) } else { 0 };
    // 只对“被外壳隐藏”的候选窗口问虚拟桌面管理器：其余窗口完全不涉及 COM。
    let other_desktop = cloaked == DWM_CLOAKED_SHELL && on_other_desktop(hwnd);
    WindowFacts { visible, has_owner, tool_window, app_window, has_title, cloaked: cloaked != 0, other_desktop }
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
    app_windows().into_keys().collect()
}

/// 当前所有应用进程的 PID 与其主窗口句柄（`HWND` 裸值；多个窗口时取 Z 序最上层的，UWP 取外层框架窗口）。
pub fn app_windows() -> HashMap<u32, isize> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 是 `app_windows` 传入的 `&mut HashMap<u32, isize>`，枚举期间一直有效。
        let apps = unsafe { &mut *(lparam.0 as *mut HashMap<u32, isize>) };
        if is_app_window(facts(hwnd)) {
            if class_name(hwnd) == UWP_FRAME_CLASS {
                // 找不到 CoreWindow（正在启动或退出）时不记录：框架宿主本身不是应用。
                if let Some(pid) = uwp_pid(hwnd) {
                    apps.entry(pid).or_insert(hwnd.0 as isize);
                }
            } else {
                apps.entry(window_pid(hwnd)).or_insert(hwnd.0 as isize);
            }
        }
        true.into()
    }
    let mut apps = HashMap::new();
    DESKTOP.with(|d| d.borrow_mut().multiple_desktops = None);
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut apps as *mut HashMap<u32, isize> as isize)) };
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown() -> WindowFacts {
        WindowFacts { visible: true, has_owner: false, tool_window: false, app_window: false, has_title: true, cloaked: false, other_desktop: false }
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
    fn desktop_probe_creates_manager_and_reports_current_desktop() {
        // 绕过“只有一个桌面就不问”的门槛，直接验证 COM 路径：创建管理器、正常查询、无效窗口。
        let mut probe = DesktopProbe::default();
        probe.multiple_desktops = Some(true);
        let shell = unsafe { windows::Win32::UI::WindowsAndMessaging::GetShellWindow() };
        let on_current = shell.is_invalid() || !probe.on_other_desktop(shell);
        assert!(on_current, "外壳窗口在当前桌面上，不应判为在其他桌面");
        assert!(probe.manager.is_some(), "应已创建 IVirtualDesktopManager");
        // 无效句柄：系统回答“在当前桌面”（Ok(TRUE)），同样不会被当成其他桌面上的应用。
        assert!(!probe.on_other_desktop(HWND(0x7777_7777 as _)));
        assert_eq!(probe.backoff.failures, 0, "查询成功后失败计数为 0");
        // 只有一个桌面时完全不触碰 COM。
        let mut single = DesktopProbe::default();
        single.multiple_desktops = Some(false);
        assert!(!single.on_other_desktop(HWND(0x7777_7777 as _)));
        assert!(single.manager.is_none() && single.backoff.failures == 0 && !single.com_tried && !single.com_initialized);
    }

    #[test]
    fn desktop_probe_respects_backoff_without_touching_com() {
        // 退避期内 manager() 直接返回 None，且不会去初始化 COM、不会再创建管理器。
        let mut probe = DesktopProbe::default();
        probe.backoff.failed(Instant::now());
        assert!(probe.manager().is_none());
        assert!(!probe.com_tried && !probe.com_initialized && probe.manager.is_none());
        assert_eq!(probe.backoff.failures, 1, "退避期内不应改动失败计数");
    }

    #[test]
    fn backoff_counts_only_consecutive_failures_and_never_gives_up() {
        let t0 = Instant::now();
        let mut b = Backoff::default();
        assert!(b.allowed(t0));
        b.failed(t0);
        assert!(!b.allowed(t0), "失败后先退避");
        assert!(b.allowed(t0 + BACKOFF_BASE));
        b.failed(t0 + BACKOFF_BASE);
        assert!(!b.allowed(t0 + BACKOFF_BASE + BACKOFF_BASE), "连续失败后退避翻倍");
        assert!(b.allowed(t0 + BACKOFF_BASE + BACKOFF_BASE * 2));
        // 成功一次：计数清零，下次失败又从最短退避开始。
        b.succeeded();
        assert_eq!(b.failures, 0);
        assert!(b.allowed(t0));
        b.failed(t0);
        assert!(b.allowed(t0 + BACKOFF_BASE));
        // 很多次连续失败：退避封顶，不会永久放弃，也不会溢出。
        let mut b = Backoff::default();
        for _ in 0..1000 {
            b.failed(t0);
        }
        assert!(b.allowed(t0 + BACKOFF_MAX));
    }

    #[test]
    fn registry_value_decides_whether_other_desktops_exist() {
        // 值存在：按 16 字节一个桌面计数。
        assert!(!desktops_from_registry(true, false, 0));
        assert!(!desktops_from_registry(true, false, 16));
        assert!(desktops_from_registry(true, false, 32));
        assert!(desktops_from_registry(true, false, 48));
        // 值不存在：从未创建过额外桌面。
        assert!(!desktops_from_registry(false, true, 0));
        // 其他读取错误（拒绝访问等）：无法判断，交给虚拟桌面管理器。
        assert!(desktops_from_registry(false, false, 0));
    }

    #[test]
    fn cloaked_window_on_another_desktop_is_still_app() {
        let elsewhere = WindowFacts { cloaked: true, other_desktop: true, ..shown() };
        assert!(is_app_window(elsewhere));
        // 其他条件照常适用：别的桌面上不可见 / 无标题 / 工具窗口 / 有所有者的窗口仍不是应用。
        assert!(!is_app_window(WindowFacts { visible: false, ..elsewhere }));
        assert!(!is_app_window(WindowFacts { has_title: false, ..elsewhere }));
        assert!(!is_app_window(WindowFacts { tool_window: true, ..elsewhere }));
        assert!(!is_app_window(WindowFacts { has_owner: true, ..elsewhere }));
        // 挂起的 UWP：cloaked 但在当前桌面上，仍不是应用。
        assert!(!is_app_window(WindowFacts { cloaked: true, other_desktop: false, ..shown() }));
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

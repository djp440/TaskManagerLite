//! 单实例：命名互斥量判断是否已有实例，命名事件通知已有实例把窗口带到前台。
//!
//! 对象名限定在 `Local\`（当前登录会话）。互斥量与事件都带显式安全描述符：提权实例创建的对象
//! 默认 DACL 不允许普通权限进程打开，会让普通实例误判成“没有实例”或打不开事件；这里授予 Everyone
//! 全部访问，并把完整性级别标为 Low（不拦截来自中、低完整性进程的写入），使两种权限的实例互相可见。
//! 对象只存在于本会话命名空间内，不跨用户、不跨会话。
//!
//! 通知走内核事件而不是窗口消息，是因为 UIPI 不允许普通权限进程向提权窗口发消息，而事件不受此限。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, HWND, LPARAM, LocalFree,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Threading::{
    AttachThreadInput, CreateEventW, CreateMutexW, GetCurrentProcessId, GetCurrentThreadId, INFINITE, OpenProcess,
    PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::{BOOL, PCWSTR, w};

const MUTEX_NAME: PCWSTR = w!("Local\\TaskManagerLite.SingleInstance");
const EVENT_NAME: PCWSTR = w!("Local\\TaskManagerLite.Activate");
/// Everyone 全部访问；完整性级别 Low，不设“禁止低完整性写入”。
const SDDL: PCWSTR = w!("D:(A;;GA;;;WD)S:(ML;;;;;LW)");

/// 命令行参数：`--wait-for <pid>`，提权重启时由旧实例传给新实例。
pub const WAIT_FOR_ARG: &str = "--wait-for";

/// 从命令行参数中取 `--wait-for` 后面的 PID。
pub fn wait_for_arg(args: impl IntoIterator<Item = String>) -> Option<u32> {
    let mut it = args.into_iter().skip_while(|a| a != WAIT_FOR_ARG);
    it.next()?;
    it.next()?.parse().ok()
}

/// 提权重启时新实例等待旧实例退出的上限。
const WAIT_FOR_TIMEOUT: Duration = Duration::from_secs(10);

pub enum Acquired {
    /// 本进程是唯一实例；持有期间其他实例会被挡掉。
    Primary(SingleInstance),
    /// 已有实例，已通知它唤起窗口（通知失败时静默），本进程应直接退出。
    Existing,
}

/// 单实例锁。互斥量句柄随进程退出自动释放，因此崩溃也不会留下“僵尸锁”。
pub struct SingleInstance {
    _mutex: HANDLE,
    event: Option<HANDLE>,
}

// 句柄只是内核对象标识，可以在线程间传递。
unsafe impl Send for SingleInstance {}

impl SingleInstance {
    /// 起一个后台线程等待其他实例的唤起请求，每次请求调用一次 `on_activate`（在该线程上）。
    /// 只能调用一次。
    pub fn listen(&mut self, on_activate: impl Fn() + Send + 'static) {
        let Some(event) = self.event.take() else { return };
        let event = event.0 as usize;
        let _ = std::thread::Builder::new().name("tml-activate".into()).stack_size(64 * 1024).spawn(move || {
            let event = HANDLE(event as *mut _);
            // 事件是自动复位的：创建之前就发出的请求也会保留到这里被取走。
            while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                on_activate();
            }
        });
    }
}

/// 尝试成为唯一实例。`wait_for` 是提权重启时旧实例的 PID：先等它退出（有超时）再取锁，
/// 否则新的提权实例会被尚未退出的旧实例挡掉。
pub fn acquire(wait_for: Option<u32>) -> Acquired {
    if let Some(pid) = wait_for {
        wait_for_exit(pid, WAIT_FOR_TIMEOUT);
    }
    // 旧实例退出到内核释放互斥量之间可能有短暂间隙，等待重启时多试几次。
    let deadline = Instant::now() + if wait_for.is_some() { Duration::from_secs(2) } else { Duration::ZERO };
    loop {
        match try_acquire() {
            Some(inst) => return Acquired::Primary(inst),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => break,
        }
    }
    notify_existing();
    Acquired::Existing
}

/// 取得互斥量则返回锁；已被占用（含因权限被拒绝打开）返回 `None`。
fn try_acquire() -> Option<SingleInstance> {
    with_security_attributes(|sa| {
        let mutex = unsafe { CreateMutexW(Some(sa), false, MUTEX_NAME) };
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        match mutex {
            Ok(h) if already => {
                unsafe {
                    let _ = CloseHandle(h);
                }
                None
            }
            Ok(h) => {
                let event = unsafe { CreateEventW(Some(sa), false, false, EVENT_NAME) }.ok();
                Some(SingleInstance { _mutex: h, event })
            }
            // 无权限打开别人创建的互斥量，同样说明已有实例；其他失败（极少见）放行，宁可多开也不能打不开。
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => None,
            Err(_) => Some(SingleInstance { _mutex: HANDLE::default(), event: None }),
        }
    })
    .flatten()
}

/// 授权新进程抢前台，再触发已有实例的唤起事件。
fn notify_existing() {
    unsafe {
        // 新实例是用户刚启动的前台进程，持有前台权限；放开后已有实例才能 SetForegroundWindow 成功。
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    // 创建或打开同名事件：即使对方尚未建好事件，信号也会保留到它建好后被取走。
    let event = with_security_attributes(|sa| unsafe { CreateEventW(Some(sa), false, false, EVENT_NAME) });
    if let Some(Ok(h)) = event {
        unsafe {
            let _ = SetEvent(h);
            let _ = CloseHandle(h);
        }
    }
}

fn with_security_attributes<T>(f: impl FnOnce(*const SECURITY_ATTRIBUTES) -> T) -> Option<T> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(SDDL, SDDL_REVISION_1, &mut sd, None) }.ok()?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let r = f(&sa);
    unsafe {
        LocalFree(Some(HLOCAL(sd.0)));
    }
    Some(r)
}

/// 等待进程退出；进程已不存在或打不开则立即返回。
fn wait_for_exit(pid: u32, timeout: Duration) {
    if pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return };
    unsafe {
        WaitForSingleObject(h, timeout.as_millis() as u32);
        let _ = CloseHandle(h);
    }
}

/// 把本进程的主窗口还原（若已最小化）并带到前台。须在界面线程调用。
pub fn activate_own_window() {
    let Some(hwnd) = find_own_window() else { return };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            // 系统没有放行：临时挂接到当前前台窗口的输入队列，是公认的绕过前台锁的办法。
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            if fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool() {
                let _ = SetForegroundWindow(hwnd);
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        }
    }
}

/// 本进程可见的无属主顶层窗口（即应用主窗口，最小化时仍可见）。
fn find_own_window() -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.0.is_null());
        if pid == search.pid && !owned && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            search.found = Some(hwnd);
            return false.into();
        }
        true.into()
    }
    let mut search = Search { pid: unsafe { GetCurrentProcessId() }, found: None };
    // 找到即中止枚举时 EnumWindows 返回错误，属正常。
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_wait_for_pid() {
        assert_eq!(wait_for_arg(args(&["exe", "--wait-for", "1234"])), Some(1234));
        assert_eq!(wait_for_arg(args(&["exe", "--measure-startup", "--wait-for", "7"])), Some(7));
    }

    #[test]
    fn ignores_missing_or_bad_pid() {
        assert_eq!(wait_for_arg(args(&["exe"])), None);
        assert_eq!(wait_for_arg(args(&["exe", "--wait-for"])), None);
        assert_eq!(wait_for_arg(args(&["exe", "--wait-for", "abc"])), None);
    }
}

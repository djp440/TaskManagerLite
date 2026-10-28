//! 单实例：命名互斥量判断是否已有实例，命名事件通知已有实例把窗口带到前台。
//!
//! 对象名限定在 `Local\`（当前登录会话），不跨用户、不跨会话。
//!
//! 通知走内核事件而不是窗口消息，是因为 UIPI 不允许普通权限进程向提权窗口发消息，而事件不受此限。
//!
//! # 安全描述符（最小权限）
//! 提权实例创建的对象，默认 DACL 不允许普通权限进程打开，且默认完整性标签为 High（普通进程无写权限），
//! 会让普通实例误判成“没有实例”或打不开事件。因此两个对象都带显式描述符：
//! - 互斥量：Everyone 只有 `SYNCHRONIZE`（足以打开并确认存在），标签 Medium + NO_WRITE_UP。
//! - 事件：Everyone 只有 `SYNCHRONIZE | EVENT_MODIFY_STATE`，标签同上。
//!
//! 访问推演（打开时只请求 `MUTEX_ACCESS`（互斥量）、`EVENT_MODIFY_STATE`（通知端打开事件），不能用默认的 ALL_ACCESS，否则会被拒绝）：
//! - 普通(Medium)创建、提权(High)打开：High 不低于 Medium，标签检查通过；DACL 授予所需权限。
//! - 提权创建、普通打开：对象标签被显式设为 Medium，普通进程的写操作（`SetEvent`）不被 NO_WRITE_UP 拦截；
//!   DACL 同样授予。若没有这个标签，提权实例创建的对象会是 High，普通实例无法通知。
//! - 低完整性(Low)进程：能打开互斥量（只需 SYNCHRONIZE，视为已有实例），但 `SetEvent` 被 NO_WRITE_UP 挡住，
//!   不能唤起窗口，新进程静默退出。
//! - 描述符转换失败时退回默认安全属性（宁可多开、降级为同权限互通，也不能打不开）。
//!
//! # 退出与竞态
//! - 主实例先创建激活事件，再创建互斥量：互斥量存在时事件一定已存在，败者 `OpenEventW` 不会落空。
//! - 事件由主实例持有句柄，自动复位：败者发出的信号会保留到监听线程取走（主实例尚未 `listen` 时也不丢）。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, HWND, LPARAM, LocalFree,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Threading::{
    AttachThreadInput, CREATE_EVENT, CreateEventExW, CreateMutexExW, EVENT_MODIFY_STATE, GetCurrentProcessId,
    GetCurrentThreadId, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::{BOOL, PCWSTR, w};

/// 测试隔离：环境变量 `TML_INSTANCE_ID` 非空时追加到对象名后，自动化测试可以在已有真实实例的机器上
/// 验证单实例行为而不互相干扰（只影响本进程及其子进程）。
fn object_names() -> &'static [Vec<u16>; 2] {
    static NAMES: std::sync::OnceLock<[Vec<u16>; 2]> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let suffix = std::env::var("TML_INSTANCE_ID").map(|s| format!(".{s}")).unwrap_or_default();
        ["Local\\TaskManagerLite.SingleInstance", "Local\\TaskManagerLite.Activate"]
            .map(|n| format!("{n}{suffix}").encode_utf16().chain(Some(0)).collect())
    })
}
fn mutex_name() -> PCWSTR {
    PCWSTR(object_names()[0].as_ptr())
}
fn event_name() -> PCWSTR {
    PCWSTR(object_names()[1].as_ptr())
}
/// 互斥量：Everyone 仅 SYNCHRONIZE(0x100000)；标签 Medium + NO_WRITE_UP。
const MUTEX_SDDL: PCWSTR = w!("D:(A;;0x100000;;;WD)S:(ML;;NW;;;ME)");
/// 事件：Everyone 仅 SYNCHRONIZE|EVENT_MODIFY_STATE(0x100002)；标签同上。
const EVENT_SDDL: PCWSTR = w!("D:(A;;0x100002;;;WD)S:(ML;;NW;;;ME)");
/// 打开互斥量请求的权限：只要 SYNCHRONIZE。
const MUTEX_ACCESS: u32 = 0x0010_0000;
/// 主实例创建事件请求的权限：等待 + 触发。
const EVENT_OWNER_ACCESS: u32 = 0x0010_0000 | 0x2;

/// 命令行参数：`--wait-for <pid>`，提权重启时由旧实例传给新实例。
pub const WAIT_FOR_ARG: &str = "--wait-for";

/// 命令行是否带 `--wait-for`（提权重启场景，旧实例此刻可能还占着锁）。
/// 不按 PID 等待进程：PID 可能被复用，导致白等；PID 仅供排查，实际靠轮询互斥量。
pub fn is_relaunch(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|a| a == WAIT_FOR_ARG)
}

/// 提权重启时轮询互斥量的总时长上限。超时后按“已有实例”处理：通知它并退出
/// （旧实例卡死不退出时，不能让两个实例并存）。
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(12);

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

/// 尝试成为唯一实例。`relaunch` 为真（提权重启）时，旧实例还没退出、仍占着锁：
/// 轮询互斥量直到旧实例退出，最长 [`RELAUNCH_TIMEOUT`]，超时按已有实例处理。
pub fn acquire(relaunch: bool) -> Acquired {
    let deadline = Instant::now() + if relaunch { RELAUNCH_TIMEOUT } else { Duration::ZERO };
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
    // 先建事件再建互斥量（见模块说明）。败者得到的是同一事件的另一个句柄，随后关闭即可。
    let event = with_security_attributes(EVENT_SDDL, |sa| {
        unsafe { CreateEventExW(sa, event_name(), CREATE_EVENT(0), EVENT_OWNER_ACCESS) }.ok()
    });
    let inst = try_mutex(event);
    if let (None, Some(e)) = (&inst, event) {
        unsafe {
            let _ = CloseHandle(e);
        }
    }
    inst
}

/// 创建互斥量；已存在（含被拒绝打开）返回 `None`，其他失败放行，宁可多开也不能打不开。
fn try_mutex(event: Option<HANDLE>) -> Option<SingleInstance> {
    let mutex = with_security_attributes(MUTEX_SDDL, |sa| {
        let h = unsafe { CreateMutexExW(sa, mutex_name(), 0, MUTEX_ACCESS) };
        (h, unsafe { GetLastError() } == ERROR_ALREADY_EXISTS)
    });
    match mutex {
        (Ok(h), true) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            None
        }
        (Ok(h), false) => Some(SingleInstance { _mutex: h, event }),
        // 无权限打开别人创建的互斥量（例如旧版本或默认 DACL 的提权实例），同样说明已有实例。
        (Err(e), _) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => None,
        (Err(_), _) => Some(SingleInstance { _mutex: HANDLE::default(), event }),
    }
}

/// 授权新进程抢前台，再触发已有实例的唤起事件。事件打不开（权限不足，如低完整性进程）时静默放弃。
fn notify_existing() {
    unsafe {
        // 新实例是用户刚启动的前台进程，持有前台权限；放开后已有实例才能 SetForegroundWindow 成功。
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    // 互斥量存在意味着主实例已建好事件，所以只打开、不创建。
    if let Ok(h) = unsafe { OpenEventW(EVENT_MODIFY_STATE, false, event_name()) } {
        unsafe {
            let _ = SetEvent(h);
            let _ = CloseHandle(h);
        }
    }
}

/// 用 `sddl` 构造安全属性并调用 `f`。描述符转换失败时传 `None`（默认安全属性）。
fn with_security_attributes<T>(sddl: PCWSTR, f: impl FnOnce(Option<*const SECURITY_ATTRIBUTES>) -> T) -> T {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut sd, None) }.is_err() {
        return f(None);
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let r = f(Some(&sa));
    unsafe {
        LocalFree(Some(HLOCAL(sd.0)));
    }
    r
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
    fn detects_relaunch_flag() {
        assert!(is_relaunch(args(&["exe", "--wait-for", "1234"])));
        assert!(is_relaunch(args(&["exe", "--measure-startup", "--wait-for", "7"])));
    }

    #[test]
    fn plain_launch_is_not_relaunch() {
        assert!(!is_relaunch(args(&["exe"])));
        assert!(!is_relaunch(args(&["exe", "--allow-multiple"])));
    }

    #[test]
    fn security_descriptors_are_valid() {
        // SDDL 写错会在运行时静默退回默认属性，这里提前暴露。
        for sddl in [MUTEX_SDDL, EVENT_SDDL] {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut sd, None) }
                .expect("SDDL 应当可解析");
            unsafe {
                LocalFree(Some(HLOCAL(sd.0)));
            }
        }
    }
}

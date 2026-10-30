//! 进程操作（#66）：设置优先级类与 CPU 亲和性；（#67）挂起与恢复。
//!
//! 与其他核心模块一样不返回显示文案：失败用 [`ProcessControlError`]（带系统错误码），
//! 文案由界面层按枚举映射。优先级与掩码的换算是纯逻辑，可以脱离系统单测；
//! 真正调用系统的只有 `OpenProcess` + `GetPriorityClass` / `SetPriorityClass` /
//! `GetProcessAffinityMask` / `SetProcessAffinityMask`。
//!
//! 亲和性掩码只覆盖进程当前所在的处理器组（最多 64 个逻辑处理器）。系统逻辑处理器多于一个组
//! 能容纳的数量时，[`AffinityInfo::truncated`] 为真，界面应明确提示“仅当前处理器组”。

use std::sync::OnceLock;

use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Wdk::System::Threading::{NtQueryInformationProcess, PROCESSINFOCLASS};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, NTSTATUS};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetActiveProcessorCount, GetExitCodeProcess,
    GetPriorityClass, GetProcessAffinityMask, HIGH_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, ProcessPowerThrottling, REALTIME_PRIORITY_CLASS, SetPriorityClass, SetProcessAffinityMask,
    SetProcessInformation,
};

use crate::control::{OwnedHandle, create_time, open};
use crate::error::SystemError;
use crate::snapshot::ProcessKey;

/// `GetActiveProcessorCount` 的“所有处理器组”参数（`ALL_PROCESSOR_GROUPS`）。
const ALL_PROCESSOR_GROUPS: u16 = 0xFFFF;

/// 进程优先级类（与原生任务管理器的“设置优先级”一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriorityClass {
    Realtime,
    High,
    AboveNormal,
    Normal,
    BelowNormal,
    /// Win32 的 `IDLE_PRIORITY_CLASS`，界面上叫“低”。
    Low,
}

impl PriorityClass {
    /// 菜单里的顺序：从高到低。
    pub const ALL: [PriorityClass; 6] = [
        PriorityClass::Realtime,
        PriorityClass::High,
        PriorityClass::AboveNormal,
        PriorityClass::Normal,
        PriorityClass::BelowNormal,
        PriorityClass::Low,
    ];

    /// 对应的 Win32 优先级类常量。
    pub fn win32_class(self) -> u32 {
        match self {
            Self::Realtime => REALTIME_PRIORITY_CLASS.0,
            Self::High => HIGH_PRIORITY_CLASS.0,
            Self::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS.0,
            Self::Normal => NORMAL_PRIORITY_CLASS.0,
            Self::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS.0,
            Self::Low => IDLE_PRIORITY_CLASS.0,
        }
    }

    /// 由 `GetPriorityClass` 的返回值得到优先级；未知值（含读取失败的 0）为 `None`。
    pub fn from_win32_class(class: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.win32_class() == class)
    }

    /// 在 [`Self::ALL`] 中的下标（菜单项序号）。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    /// 稳定的英文标识，用于测试钩子和日志（不是显示文案）。
    pub fn key(self) -> &'static str {
        match self {
            Self::Realtime => "realtime",
            Self::High => "high",
            Self::AboveNormal => "above-normal",
            Self::Normal => "normal",
            Self::BelowNormal => "below-normal",
            Self::Low => "low",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }
}

/// 设置优先级或亲和性失败的原因。界面层按枚举映射成显示文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessControlError {
    /// 进程已退出，或 PID 已被别的进程复用。
    Gone,
    /// 拒绝访问：受保护进程、他人的进程或需要提权。
    AccessDenied,
    /// 亲和性掩码为空：至少要选一个逻辑处理器。
    EmptyAffinity,
    /// 掩码含有当前处理器组之外的位。
    OutsideProcessorGroup,
    /// 效率模式（#68）、挂起（#67）：系统关键进程（PID 0、4）或本程序自身，不允许操作。
    Protected,
    /// 效率模式（#68）：系统版本低于 Windows 10 1709（build 16299），不支持进程电源节流。
    Unsupported,
    /// 其余系统调用失败，保留系统错误码。
    System(SystemError),
}

impl ProcessControlError {
    /// 写入日志用的错误码：Win32 错误码或 HRESULT；与系统错误码无关的原因为 0。
    pub fn code(&self) -> i32 {
        match self {
            Self::Gone => ERROR_INVALID_PARAMETER.0 as i32,
            Self::AccessDenied => ERROR_ACCESS_DENIED.0 as i32,
            Self::EmptyAffinity | Self::OutsideProcessorGroup | Self::Protected | Self::Unsupported => 0,
            Self::System(SystemError::Win32(code)) => *code,
            Self::System(SystemError::Other { code, .. }) => *code,
        }
    }
}

impl std::fmt::Display for ProcessControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => f.write_str("process is gone"),
            Self::AccessDenied => f.write_str("access denied"),
            Self::EmptyAffinity => f.write_str("empty affinity mask"),
            Self::OutsideProcessorGroup => f.write_str("mask has bits outside the processor group"),
            Self::Protected => f.write_str("protected process"),
            Self::Unsupported => f.write_str("efficiency mode is not supported on this Windows version"),
            Self::System(e) => write!(f, "process control failed: {e}"),
        }
    }
}

impl std::error::Error for ProcessControlError {}

/// 把 `windows` crate 的错误还原成系统错误码（`HRESULT_FROM_WIN32` 形式取回 Win32 码）。
fn classify(e: &windows::core::Error) -> ProcessControlError {
    let raw = e.code().0 as u32;
    if raw >> 16 == 0x8007 {
        let code = (raw & 0xFFFF) as i32;
        if code == ERROR_ACCESS_DENIED.0 as i32 {
            return ProcessControlError::AccessDenied;
        }
        return ProcessControlError::System(SystemError::Win32(code));
    }
    ProcessControlError::System(SystemError::from_windows(e))
}

/// 一个进程的亲和性读数：掩码只覆盖当前处理器组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityInfo {
    /// 进程当前的亲和性掩码（第 i 位 = 当前组的第 i 个逻辑处理器）。
    pub process_mask: u64,
    /// 当前组可用的逻辑处理器掩码。
    pub system_mask: u64,
    /// 系统全部处理器组的逻辑处理器总数。
    pub total_processors: u32,
}

impl AffinityInfo {
    /// 当前组里的逻辑处理器编号（升序）。
    pub fn processors(&self) -> Vec<u32> {
        processors_in_mask(self.system_mask)
    }

    /// 编号为 `cpu` 的逻辑处理器是否被进程选中。
    pub fn is_selected(&self, cpu: u32) -> bool {
        cpu < 64 && self.process_mask & (1u64 << cpu) != 0
    }

    /// 系统逻辑处理器多于当前处理器组：首版只能设置当前组，界面应提示。
    pub fn truncated(&self) -> bool {
        self.total_processors > self.system_mask.count_ones()
    }
}

/// 掩码里置位的逻辑处理器编号（升序）。
pub fn processors_in_mask(mask: u64) -> Vec<u32> {
    (0..64).filter(|&i| mask & (1u64 << i) != 0).collect()
}

/// 由选中的逻辑处理器编号得到掩码；超过 63 的编号被忽略。
pub fn mask_from_processors(selected: impl IntoIterator<Item = u32>) -> u64 {
    selected
        .into_iter()
        .filter(|&i| i < 64)
        .fold(0, |m, i| m | (1u64 << i))
}

/// 校验掩码：不能为空，也不能超出当前处理器组的可用处理器。
pub fn validate_affinity(mask: u64, system_mask: u64) -> Result<(), ProcessControlError> {
    if mask == 0 {
        Err(ProcessControlError::EmptyAffinity)
    } else if mask & !system_mask != 0 {
        Err(ProcessControlError::OutsideProcessorGroup)
    } else {
        Ok(())
    }
}

/// 右键菜单弹出时读到的进程状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlState {
    /// 当前优先级类；读不到时为 `None`（菜单不打勾）。
    pub priority: Option<PriorityClass>,
    /// 当前亲和性；读不到时为 `None`。
    pub affinity: Option<AffinityInfo>,
    /// 是否处于效率模式（#68）；系统不支持或读不到时为 `None`。
    pub eco: Option<bool>,
    /// 能否修改：`Ok` 表示可以打开进程设置，`Err` 是预知的原因（用于置灰与提示）。
    pub modify: Result<(), ProcessControlError>,
    /// 能否挂起/恢复（#67，需要 `PROCESS_SUSPEND_RESUME`）：`Err` 是预知的原因（用于置灰与提示）。
    pub suspend: Result<(), ProcessControlError>,
}

/// 打开进程并核对创建时间，防止改到复用了同一 PID 的新进程。
fn open_checked(
    key: ProcessKey,
    access: PROCESS_ACCESS_RIGHTS,
) -> Result<OwnedHandle, ProcessControlError> {
    // Idle 伪进程（pid 0）打不开，也没有可设置的东西：按拒绝访问处理。
    if key.pid == 0 {
        return Err(ProcessControlError::AccessDenied);
    }
    let h = match open(key.pid, access) {
        Ok(h) => h,
        Err(e) if e.code() == ERROR_INVALID_PARAMETER.to_hresult() => {
            return Err(ProcessControlError::Gone);
        }
        Err(e) => return Err(classify(&e)),
    };
    // 已退出但仍被别处引用的进程对象照样能打开、能“设置”成功，所以还要确认进程仍在运行。
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    let running = unsafe { GetExitCodeProcess(h.raw(), &mut code) }.is_ok() && code == STILL_ACTIVE;
    if create_time(&h) != Some(key.create_time) || !running {
        return Err(ProcessControlError::Gone);
    }
    Ok(h)
}

fn read_priority(h: &OwnedHandle) -> Option<PriorityClass> {
    PriorityClass::from_win32_class(unsafe { GetPriorityClass(h.raw()) })
}

fn read_affinity(h: &OwnedHandle) -> Result<AffinityInfo, ProcessControlError> {
    let (mut process, mut system) = (0usize, 0usize);
    // 进程跨多个处理器组时该调用会失败（ERROR_INVALID_PARAMETER），原样作为系统错误返回。
    unsafe { GetProcessAffinityMask(h.raw(), &mut process, &mut system) }
        .map_err(|e| classify(&e))?;
    let system_mask = system as u64;
    let total =
        unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(system_mask.count_ones());
    Ok(AffinityInfo {
        process_mask: process as u64,
        system_mask,
        total_processors: total,
    })
}

/// 读取菜单需要的状态：当前优先级、亲和性，以及能否修改。
///
/// 先以“设置 + 查询”权限打开；被拒绝时退回只读权限，仍可读到优先级用于打勾，
/// `modify` 记下拒绝原因。进程已退出等无法读取时返回错误。
pub fn control_state(key: ProcessKey) -> Result<ControlState, ProcessControlError> {
    let (h, modify) = match open_checked(
        key,
        PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
    ) {
        Ok(h) => (h, Ok(())),
        Err(ProcessControlError::AccessDenied) => {
            match open_checked(key, PROCESS_QUERY_LIMITED_INFORMATION) {
                Ok(h) => (h, Err(ProcessControlError::AccessDenied)),
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    };
    // 挂起/恢复要另一种访问权限，单独探测一次（只在菜单弹出时发生，不在采样路径上）。
    let suspend = open_checked(key, PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION).map(|_| ());
    Ok(ControlState {
        priority: read_priority(&h),
        affinity: read_affinity(&h).ok(),
        eco: if eco_supported() { read_eco(&h).ok() } else { None },
        modify,
        suspend,
    })
}

/// 读取亲和性（打开“设置亲和性”对话框时调用）。
pub fn affinity(key: ProcessKey) -> Result<AffinityInfo, ProcessControlError> {
    let h = open_checked(key, PROCESS_QUERY_LIMITED_INFORMATION)?;
    read_affinity(&h)
}

/// 设置优先级类，返回设置后回读到的实际优先级。
///
/// 没有管理员权限设置“实时”时系统不报错，而是降为“高”，所以界面应以返回值为准。
pub fn set_priority(
    key: ProcessKey,
    class: PriorityClass,
) -> Result<PriorityClass, ProcessControlError> {
    let h = open_checked(
        key,
        PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
    )?;
    unsafe { SetPriorityClass(h.raw(), PROCESS_CREATION_FLAGS(class.win32_class())) }
        .map_err(|e| classify(&e))?;
    Ok(read_priority(&h).unwrap_or(class))
}

/// 设置亲和性掩码（当前处理器组），返回设置后回读到的亲和性。
pub fn set_affinity(key: ProcessKey, mask: u64) -> Result<AffinityInfo, ProcessControlError> {
    let h = open_checked(
        key,
        PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
    )?;
    let before = read_affinity(&h)?;
    validate_affinity(mask, before.system_mask)?;
    unsafe { SetProcessAffinityMask(h.raw(), mask as usize) }.map_err(|e| classify(&e))?;
    read_affinity(&h)
}


// ---------------------------------------------------------------------------------------------
// 效率模式（#68）
//
// 效率模式 = 进程电源节流 `ProcessPowerThrottling` 的 `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`（EcoQoS：
// 调度器把进程放到节能核/低频，省电降温）+ 优先级类“低”（IDLE_PRIORITY_CLASS），与原生任务管理器一致。
//
// `PROCESS_POWER_THROTTLING_STATE` 的语义：`ControlMask` 的位 = “由调用方接管该项”，
// `StateMask` 的位 = 该项开启。因此：
//   开启：Control = SPEED，State = SPEED（强制节流）；
//   关闭：Control = SPEED，State = 0（显式禁止节流，与系统任务管理器关闭效率模式一致）。
//         实测 Control = 0 的调用是空操作（State 被忽略，不会关掉已开启的节流），所以不能用它“交还系统管理”。
// 读回：用 `NtQueryInformationProcess(77)`（见 `read_eco`），Control 与 State 的 SPEED 位同时置位才算“开启”；
//       写入后读回约有 1 毫秒延迟，`set_eco_mode` 会等待至多 50 毫秒。
// ---------------------------------------------------------------------------------------------

/// 支持进程电源节流（EcoQoS）的最低 Windows 版本：Windows 10 1709，build 16299。
const ECO_MIN_BUILD: u32 = 16299;

/// 由系统 build 号判断是否支持效率模式（纯函数，便于单测）。
pub fn eco_supported_build(build: u32) -> bool {
    build >= ECO_MIN_BUILD
}

/// 当前系统的 build 号。用 `RtlGetVersion`，不会像 `GetVersionEx` 那样被应用清单欺骗。
fn windows_build() -> Option<u32> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    unsafe { RtlGetVersion(&mut info) }.is_ok().then_some(info.dwBuildNumber)
}

/// 当前系统是否支持效率模式（只查一次）。读不到版本时按不支持处理。
pub fn eco_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| windows_build().is_some_and(eco_supported_build))
}

/// 这个 PID 是否不允许开启效率模式：Idle(0)、System(4) 与本程序自身（`own` 传本进程 PID）。
/// 其余关键进程由调用方按 [`crate::Process::critical`] 判断。
pub fn eco_protected_pid(pid: u32, own: u32) -> bool {
    pid == 0 || pid == 4 || pid == own
}

/// 挂起（#67）是否不允许操作这个 PID：与效率模式相同，Idle(0)、System(4) 与本程序自身（`own` 传本进程 PID）。
/// 其余关键进程允许挂起，但界面需先二次确认（[`crate::Process::critical`]）。
pub fn suspend_protected_pid(pid: u32, own: u32) -> bool {
    eco_protected_pid(pid, own)
}

/// `NtSuspendProcess` / `NtResumeProcess` 的函数签名：参数是进程句柄，返回 NTSTATUS。
type NtProcessFn = unsafe extern "system" fn(HANDLE) -> NTSTATUS;

/// 从 ntdll 取导出函数（它们没有公开的导入库声明，所以动态绑定；只查一次）。
fn ntdll_process_fn(name: windows::core::PCSTR) -> Option<NtProcessFn> {
    unsafe {
        let ntdll = GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()?;
        let f = GetProcAddress(ntdll, name)?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, NtProcessFn>(f))
    }
}

fn nt_suspend_fn() -> Option<NtProcessFn> {
    static F: OnceLock<Option<NtProcessFn>> = OnceLock::new();
    *F.get_or_init(|| ntdll_process_fn(windows::core::s!("NtSuspendProcess")))
}

fn nt_resume_fn() -> Option<NtProcessFn> {
    static F: OnceLock<Option<NtProcessFn>> = OnceLock::new();
    *F.get_or_init(|| ntdll_process_fn(windows::core::s!("NtResumeProcess")))
}

/// NTSTATUS 失败码映射成 [`ProcessControlError`]（0xC0000022 = STATUS_ACCESS_DENIED）。
fn nt_error(status: NTSTATUS) -> ProcessControlError {
    if status.0 as u32 == 0xC000_0022 {
        ProcessControlError::AccessDenied
    } else {
        ProcessControlError::System(SystemError::Other {
            code: status.0,
            message: format!("NTSTATUS 0x{:08X}", status.0 as u32),
        })
    }
}

fn call_nt_process(
    key: ProcessKey,
    f: Option<NtProcessFn>,
) -> Result<(), ProcessControlError> {
    let f = f.ok_or(ProcessControlError::Unsupported)?;
    let h = open_checked(key, PROCESS_SUSPEND_RESUME | PROCESS_QUERY_LIMITED_INFORMATION)?;
    let status = unsafe { f(h.raw()) };
    if status.is_err() { Err(nt_error(status)) } else { Ok(()) }
}

/// 挂起整个进程（#67，`NtSuspendProcess`）。
///
/// 系统的挂起是计数式的：每挂起一次，之后要恢复同样的次数才会真正运行。
/// 调用方应先依据采样里的“已挂起”状态（`Counters::suspended`）避免对已挂起的进程重复挂起。
/// PID 0、4 与本程序自身返回 [`ProcessControlError::Protected`]。
pub fn suspend_process(key: ProcessKey) -> Result<(), ProcessControlError> {
    if suspend_protected_pid(key.pid, std::process::id()) {
        return Err(ProcessControlError::Protected);
    }
    call_nt_process(key, nt_suspend_fn())
}

/// 恢复整个进程（#67，`NtResumeProcess`）：撤销一次挂起；多次挂起过的进程需要恢复同样的次数。
pub fn resume_process(key: ProcessKey) -> Result<(), ProcessControlError> {
    call_nt_process(key, nt_resume_fn())
}

/// 在 `keys` 里找出此刻仍存在且处于“已挂起”的进程（PID 与创建时间都要对上）。
///
/// 做一次进程枚举（`NtQuerySystemInformation`），不打开任何进程；用于退出前核对“本程序挂起的进程是否还挂着”，
/// 不依赖界面线程手里可能已过期的快照。枚举失败时按“没有”处理。
pub fn still_suspended(keys: &[ProcessKey]) -> Vec<ProcessKey> {
    if keys.is_empty() {
        return Vec::new();
    }
    let Ok(raw) = crate::nt::ProcessQuery::default().query() else {
        return Vec::new();
    };
    keys.iter()
        .copied()
        .filter(|k| raw.iter().any(|r| r.pid == k.pid && r.create_time == k.create_time && r.suspended))
        .collect()
}

/// 由控制/状态掩码得到“是否处于效率模式”。
pub fn eco_enabled(control_mask: u32, state_mask: u32) -> bool {
    control_mask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
        && state_mask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
}

/// `ProcessPowerThrottlingState` 的 NT 信息类编号（`PROCESSINFOCLASS` 77）。
const PROCESS_POWER_THROTTLING_STATE_CLASS: i32 = 77;

/// 读回 Control/State 掩码。
///
/// 不用 `GetProcessInformation(ProcessPowerThrottling)`：实测在 Windows 10 上无论权限如何都返回
/// `ERROR_INVALID_PARAMETER`（87），读不出来。可靠的读回方式是 `NtQueryInformationProcess` 的
/// `ProcessPowerThrottlingState`（77），缓冲区必须正好是 12 字节的 `PROCESS_POWER_THROTTLING_STATE`
/// （其余长度返回 STATUS_INFO_LENGTH_MISMATCH）。
fn read_eco(h: &OwnedHandle) -> Result<bool, ProcessControlError> {
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    let status = unsafe {
        NtQueryInformationProcess(
            h.raw(),
            PROCESSINFOCLASS(PROCESS_POWER_THROTTLING_STATE_CLASS),
            (&mut state as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status.is_err() {
        // 0xC0000022 = STATUS_ACCESS_DENIED
        return Err(if status.0 as u32 == 0xC000_0022 {
            ProcessControlError::AccessDenied
        } else {
            ProcessControlError::System(SystemError::Other {
                code: status.0,
                message: format!("NTSTATUS 0x{:08X}", status.0 as u32),
            })
        });
    }
    Ok(eco_enabled(state.ControlMask, state.StateMask))
}

fn write_eco(h: &OwnedHandle, control_mask: u32, state_mask: u32) -> Result<(), ProcessControlError> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: control_mask,
        StateMask: state_mask,
    };
    unsafe {
        SetProcessInformation(
            h.raw(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(|e| classify(&e))
}

/// 关闭效率模式：`ControlMask` 置位但 `StateMask` 清零，即“显式禁止节流”（与系统任务管理器关闭效率模式一致）。
/// 不用 `ControlMask = 0` 回到“系统管理”：系统可能立刻又按后台状态自动节流，用户点了关闭却仍被降速。
fn clear_eco(h: &OwnedHandle) -> Result<(), ProcessControlError> {
    write_eco(h, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, 0)
}

/// `SetProcessInformation` 写入后，读回的状态最多滞后约 1 毫秒才生效；这里最多等 50 毫秒，
/// 让紧随其后的采样读到新状态。超时不算错误（调用方按期望值返回）。
fn wait_eco(h: &OwnedHandle, want: bool) {
    let start = std::time::Instant::now();
    while read_eco(h) != Ok(want) && start.elapsed() < std::time::Duration::from_millis(50) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// 读取一个进程当前是否处于效率模式。
pub fn eco_mode(key: ProcessKey) -> Result<bool, ProcessControlError> {
    if !eco_supported() {
        return Err(ProcessControlError::Unsupported);
    }
    let h = open_checked(key, PROCESS_QUERY_LIMITED_INFORMATION)?;
    read_eco(&h)
}

/// 切换效率模式的结果（均为回读到的实际状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcoOutcome {
    /// 切换后是否处于效率模式。
    pub enabled: bool,
    /// 切换后的优先级类；读不到时为 `None`。
    pub priority: Option<PriorityClass>,
    /// 切换之前的优先级类；读不到时为 `None`。界面层据此在关闭时恢复。
    pub previous_priority: Option<PriorityClass>,
}

/// 开启或关闭效率模式。
///
/// 开启：节流 + 优先级降为“低”；任何一步失败都会把节流恢复到调用前的状态，不留半截。
/// 关闭：关闭节流；`restore` 为 `Some` 时再把优先级设回该值，为 `None` 则不动优先级
/// （用户在开启期间改过优先级时由界面层传 `None`，保留用户的选择）。
pub fn set_eco_mode(
    key: ProcessKey,
    on: bool,
    restore: Option<PriorityClass>,
) -> Result<EcoOutcome, ProcessControlError> {
    if !eco_supported() {
        return Err(ProcessControlError::Unsupported);
    }
    if eco_protected_pid(key.pid, std::process::id()) {
        return Err(ProcessControlError::Protected);
    }
    let h = open_checked(
        key,
        PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
    )?;
    let previous_priority = read_priority(&h);
    if on {
        let was_on = read_eco(&h).unwrap_or(false);
        write_eco(
            &h,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        )?;
        let low = PROCESS_CREATION_FLAGS(PriorityClass::Low.win32_class());
        if let Err(e) = unsafe { SetPriorityClass(h.raw(), low) } {
            if !was_on {
                let _ = clear_eco(&h);
            }
            return Err(classify(&e));
        }
    } else {
        clear_eco(&h)?;
        if let Some(class) = restore {
            unsafe { SetPriorityClass(h.raw(), PROCESS_CREATION_FLAGS(class.win32_class())) }
                .map_err(|e| classify(&e))?;
        }
    }
    wait_eco(&h, on);
    Ok(EcoOutcome {
        enabled: read_eco(&h).unwrap_or(on),
        priority: read_priority(&h),
        previous_priority,
    })
}

/// 给进程快照填入效率模式状态（`Process::eco_mode`）。只在显示“效率模式”列时由采样器调用；
/// 系统不支持或进程打不开时为 `None`。
pub(crate) fn apply_eco_modes(processes: &mut [crate::snapshot::Process]) {
    let supported = eco_supported();
    for p in processes {
        p.eco_mode = if supported && p.key.pid > 4 {
            open(p.key.pid, PROCESS_QUERY_LIMITED_INFORMATION)
                .ok()
                .and_then(|h| read_eco(&h).ok())
        } else {
            None
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nt::ProcessQuery;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use windows::core::HRESULT;

    /// 测试用子进程（`ping -n 30`），离开作用域时结束。
    struct Victim(Child);

    impl Victim {
        fn spawn() -> Self {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let child = Command::new("ping.exe")
                .args(["-n", "30", "127.0.0.1"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("应能启动 ping");
            Victim(child)
        }

        fn key(&self) -> ProcessKey {
            let h = open(self.0.id(), PROCESS_QUERY_LIMITED_INFORMATION).expect("应能打开子进程");
            ProcessKey {
                pid: self.0.id(),
                create_time: create_time(&h).expect("应能读到创建时间"),
            }
        }
    }

    impl Drop for Victim {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn priority_maps_to_win32_constants() {
        assert_eq!(PriorityClass::Realtime.win32_class(), 0x100);
        assert_eq!(PriorityClass::High.win32_class(), 0x80);
        assert_eq!(PriorityClass::AboveNormal.win32_class(), 0x8000);
        assert_eq!(PriorityClass::Normal.win32_class(), 0x20);
        assert_eq!(PriorityClass::BelowNormal.win32_class(), 0x4000);
        assert_eq!(PriorityClass::Low.win32_class(), 0x40);
        for p in PriorityClass::ALL {
            assert_eq!(PriorityClass::from_win32_class(p.win32_class()), Some(p));
            assert_eq!(PriorityClass::from_key(p.key()), Some(p));
            assert_eq!(PriorityClass::ALL[p.index()], p);
        }
        assert_eq!(
            PriorityClass::from_win32_class(0),
            None,
            "读取失败返回 0，不应打勾"
        );
        assert_eq!(PriorityClass::from_win32_class(0x1234), None);
        assert_eq!(PriorityClass::from_key("nonsense"), None);
    }

    #[test]
    fn mask_and_processor_list_round_trip() {
        assert_eq!(processors_in_mask(0b1011), vec![0, 1, 3]);
        assert_eq!(mask_from_processors([0, 1, 3]), 0b1011);
        assert_eq!(mask_from_processors(processors_in_mask(u64::MAX)), u64::MAX);
        assert_eq!(processors_in_mask(0), Vec::<u32>::new());
        assert_eq!(mask_from_processors([63]), 1 << 63);
        assert_eq!(
            mask_from_processors([64, 100]),
            0,
            "超出一个处理器组的编号被忽略"
        );
    }

    #[test]
    fn validate_rejects_empty_and_out_of_group_masks() {
        assert_eq!(
            validate_affinity(0, 0xFF),
            Err(ProcessControlError::EmptyAffinity)
        );
        assert_eq!(
            validate_affinity(0x100, 0xFF),
            Err(ProcessControlError::OutsideProcessorGroup)
        );
        assert_eq!(
            validate_affinity(0x1FF, 0xFF),
            Err(ProcessControlError::OutsideProcessorGroup)
        );
        assert_eq!(validate_affinity(0x81, 0xFF), Ok(()));
        assert_eq!(validate_affinity(u64::MAX, u64::MAX), Ok(()));
    }

    #[test]
    fn affinity_info_reports_selection_and_truncation() {
        let info = AffinityInfo {
            process_mask: 0b0101,
            system_mask: 0b1111,
            total_processors: 4,
        };
        assert_eq!(info.processors(), vec![0, 1, 2, 3]);
        assert!(info.is_selected(0) && !info.is_selected(1) && info.is_selected(2));
        assert!(!info.truncated());
        // 128 个逻辑处理器、当前组 64 个：只能设置当前组。
        let big = AffinityInfo {
            process_mask: u64::MAX,
            system_mask: u64::MAX,
            total_processors: 128,
        };
        assert!(big.truncated());
        assert_eq!(big.processors().len(), 64);
        assert!(!big.is_selected(64), "超出一个组的编号不会被误判为选中");
    }

    #[test]
    fn error_codes_for_logging() {
        assert_eq!(ProcessControlError::AccessDenied.code(), 5);
        assert_eq!(ProcessControlError::Gone.code(), 87);
        assert_eq!(ProcessControlError::EmptyAffinity.code(), 0);
        assert_eq!(
            ProcessControlError::System(SystemError::Win32(1450)).code(),
            1450
        );
        let denied = windows::core::Error::from_hresult(HRESULT::from_win32(5));
        assert_eq!(classify(&denied), ProcessControlError::AccessDenied);
        let other = windows::core::Error::from_hresult(HRESULT::from_win32(1450));
        assert_eq!(
            classify(&other),
            ProcessControlError::System(SystemError::Win32(1450))
        );
    }

    #[test]
    fn changes_priority_of_own_child_and_snapshot_follows() {
        let victim = Victim::spawn();
        let key = victim.key();
        let base_priority_in_snapshot = || {
            ProcessQuery::default()
                .query()
                .unwrap()
                .into_iter()
                .find(|p| p.pid == key.pid)
                .map(|p| p.base_priority)
        };
        // 非实时的五级逐一设置并回读；快照里的基本优先级（“优先级”列的来源）也随之变化。
        let expected_base = [
            (PriorityClass::High, 13),
            (PriorityClass::AboveNormal, 10),
            (PriorityClass::Normal, 8),
            (PriorityClass::BelowNormal, 6),
            (PriorityClass::Low, 4),
        ];
        for (class, base) in expected_base {
            assert_eq!(set_priority(key, class), Ok(class));
            assert_eq!(control_state(key).unwrap().priority, Some(class));
            assert_eq!(base_priority_in_snapshot(), Some(base), "{class:?}");
        }
        // 实时：未提权时系统静默降为“高”，提权时才是“实时”；返回值反映实际结果。
        let actual = set_priority(key, PriorityClass::Realtime).unwrap();
        assert!(
            matches!(actual, PriorityClass::Realtime | PriorityClass::High),
            "{actual:?}"
        );
        assert_eq!(control_state(key).unwrap().priority, Some(actual));
    }

    #[test]
    fn changes_affinity_of_own_child_and_reads_it_back() {
        let victim = Victim::spawn();
        let key = victim.key();
        let original = affinity(key).unwrap();
        assert!(original.system_mask != 0 && original.process_mask & !original.system_mask == 0);
        let first = 1u64 << original.processors()[0];
        let after = set_affinity(key, first).unwrap();
        assert_eq!(after.process_mask, first);
        assert_eq!(affinity(key).unwrap().process_mask, first);
        assert_eq!(
            control_state(key).unwrap().affinity.unwrap().process_mask,
            first
        );
        // 空掩码与超出当前组的掩码在调用系统前就被拒绝，进程亲和性不变。
        assert_eq!(
            set_affinity(key, 0),
            Err(ProcessControlError::EmptyAffinity)
        );
        if original.system_mask != u64::MAX {
            let outside = !original.system_mask & (1u64 << 63 | 1u64 << 62 | 1);
            assert_eq!(
                set_affinity(key, outside),
                Err(ProcessControlError::OutsideProcessorGroup)
            );
        }
        assert_eq!(affinity(key).unwrap().process_mask, first);
        // 恢复原来的掩码。
        assert_eq!(
            set_affinity(key, original.process_mask)
                .unwrap()
                .process_mask,
            original.process_mask
        );
    }

    #[test]
    fn gone_process_and_reused_pid_are_reported() {
        let victim = Victim::spawn();
        let mut key = victim.key();
        // 创建时间对不上：等同于 PID 被复用。
        let real = key.create_time;
        key.create_time += 1;
        assert_eq!(
            set_priority(key, PriorityClass::Normal),
            Err(ProcessControlError::Gone)
        );
        assert_eq!(control_state(key), Err(ProcessControlError::Gone));
        key.create_time = real;
        drop(victim);
        assert_eq!(
            set_priority(key, PriorityClass::Normal),
            Err(ProcessControlError::Gone)
        );
        assert!(matches!(
            control_state(key),
            Err(ProcessControlError::Gone | ProcessControlError::AccessDenied)
        ));
    }

    #[test]
    fn protected_process_is_denied_without_panicking() {
        // System（pid 4）：普通权限打不开“设置”权限，应预知为拒绝访问；提权时可能打开，两种结果都不应 panic。
        let h = open(4, PROCESS_QUERY_LIMITED_INFORMATION);
        let Ok(h) = h else { return };
        let key = ProcessKey {
            pid: 4,
            create_time: create_time(&h).unwrap(),
        };
        match control_state(key) {
            Ok(state) => {
                if !crate::is_elevated() {
                    assert_eq!(state.modify, Err(ProcessControlError::AccessDenied));
                }
            }
            Err(e) => assert!(
                matches!(
                    e,
                    ProcessControlError::AccessDenied | ProcessControlError::Gone
                ),
                "{e:?}"
            ),
        }
        if !crate::is_elevated() {
            assert_eq!(
                set_priority(key, PriorityClass::Normal),
                Err(ProcessControlError::AccessDenied)
            );
        }
    }

    #[test]
    fn idle_pseudo_process_is_denied() {
        let key = ProcessKey {
            pid: 0,
            create_time: 0,
        };
        assert_eq!(control_state(key), Err(ProcessControlError::AccessDenied));
    }

    #[test]
    fn eco_support_threshold_is_windows_10_1709() {
        assert!(!eco_supported_build(10240));
        assert!(!eco_supported_build(16298));
        assert!(eco_supported_build(16299));
        assert!(eco_supported_build(19045));
        assert!(eco_supported_build(26100));
    }

    #[test]
    fn eco_mask_semantics() {
        const SPEED: u32 = PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        assert!(eco_enabled(SPEED, SPEED));
        assert!(!eco_enabled(SPEED, 0), "接管但状态为关：显式禁止节流");
        assert!(!eco_enabled(0, SPEED), "未接管：状态位无意义");
        assert!(!eco_enabled(0, 0));
    }

    #[test]
    fn eco_protects_idle_system_and_self() {
        assert!(eco_protected_pid(0, 100));
        assert!(eco_protected_pid(4, 100));
        assert!(eco_protected_pid(100, 100));
        assert!(!eco_protected_pid(101, 100));
        let own = ProcessKey {
            pid: std::process::id(),
            create_time: 0,
        };
        if eco_supported() {
            assert_eq!(set_eco_mode(own, true, None), Err(ProcessControlError::Protected));
            let system = ProcessKey { pid: 4, create_time: 0 };
            assert_eq!(set_eco_mode(system, true, None), Err(ProcessControlError::Protected));
        }
    }

    #[test]
    fn eco_mode_toggles_and_links_priority() {
        if !eco_supported() {
            return;
        }
        let v = Victim::spawn();
        let key = v.key();
        assert_eq!(eco_mode(key), Ok(false), "新进程默认不在效率模式");
        let normal = set_priority(key, PriorityClass::AboveNormal).unwrap();
        assert_eq!(normal, PriorityClass::AboveNormal);

        let on = set_eco_mode(key, true, None).unwrap();
        assert!(on.enabled);
        assert_eq!(on.priority, Some(PriorityClass::Low), "开启时优先级降为低");
        assert_eq!(on.previous_priority, Some(PriorityClass::AboveNormal));
        assert_eq!(eco_mode(key), Ok(true), "状态能读回");
        assert_eq!(control_state(key).unwrap().eco, Some(true));

        // 重复开启是幂等的。
        assert!(set_eco_mode(key, true, None).unwrap().enabled);

        let off = set_eco_mode(key, false, Some(PriorityClass::AboveNormal)).unwrap();
        assert!(!off.enabled);
        assert_eq!(off.priority, Some(PriorityClass::AboveNormal), "关闭后恢复开启前的优先级");
        assert_eq!(eco_mode(key), Ok(false));

        // 传 None：只关节流，保留用户当前的优先级。
        set_eco_mode(key, true, None).unwrap();
        set_priority(key, PriorityClass::High).unwrap();
        let off = set_eco_mode(key, false, None).unwrap();
        assert!(!off.enabled);
        assert_eq!(off.priority, Some(PriorityClass::High));
    }

    #[test]
    fn eco_mode_on_gone_process_is_gone() {
        if !eco_supported() {
            return;
        }
        let key = {
            let v = Victim::spawn();
            v.key()
        };
        assert_eq!(set_eco_mode(key, true, None), Err(ProcessControlError::Gone));
        assert_eq!(eco_mode(key), Err(ProcessControlError::Gone));
    }


    #[test]
    fn sampler_fills_eco_mode_only_when_tracked() {
        if !eco_supported() {
            return;
        }
        let v = Victim::spawn();
        let key = v.key();
        set_eco_mode(key, true, None).unwrap();
        let find = |s: &crate::sampler::Sample| s.snapshot.processes.iter().find(|p| p.key == key).map(|p| p.eco_mode);

        let mut sampler = crate::Sampler::default();
        let _ = sampler.sample();
        assert_eq!(find(&sampler.sample()), Some(None), "未开启跟踪：不读取");
        sampler.track_eco_mode(true);
        assert_eq!(find(&sampler.sample()), Some(Some(true)));
        set_eco_mode(key, false, None).unwrap();
        assert_eq!(find(&sampler.sample()), Some(Some(false)));
    }

    /// 采样器回读目标进程是否“已挂起”（`Counters::suspended`）。
    fn sampled_suspended(sampler: &mut crate::Sampler, key: ProcessKey) -> Option<bool> {
        sampler
            .sample()
            .snapshot
            .processes
            .iter()
            .find(|p| p.key == key)
            .map(|p| p.counters.suspended)
    }

    #[test]
    fn suspend_and_resume_are_visible_to_the_sampler() {
        let v = Victim::spawn();
        let key = v.key();
        let mut sampler = crate::Sampler::default();
        let _ = sampler.sample();
        assert_eq!(sampled_suspended(&mut sampler, key), Some(false));

        suspend_process(key).unwrap();
        assert_eq!(sampled_suspended(&mut sampler, key), Some(true), "挂起后下一次采样应读到已挂起");
        resume_process(key).unwrap();
        assert_eq!(sampled_suspended(&mut sampler, key), Some(false), "恢复后应回到未挂起");
    }

    #[test]
    fn suspend_is_counted_so_resume_must_match() {
        let v = Victim::spawn();
        let key = v.key();
        let mut sampler = crate::Sampler::default();
        let _ = sampler.sample();
        suspend_process(key).unwrap();
        suspend_process(key).unwrap();
        resume_process(key).unwrap();
        assert_eq!(
            sampled_suspended(&mut sampler, key),
            Some(true),
            "挂起两次后恢复一次仍是挂起（计数式），所以界面要避免重复挂起"
        );
        resume_process(key).unwrap();
        assert_eq!(sampled_suspended(&mut sampler, key), Some(false));
    }

    #[test]
    fn suspend_rejects_idle_system_and_self() {
        let own = ProcessKey { pid: std::process::id(), create_time: 1 };
        for pid in [0, 4] {
            assert_eq!(
                suspend_process(ProcessKey { pid, create_time: 1 }),
                Err(ProcessControlError::Protected)
            );
        }
        assert_eq!(suspend_process(own), Err(ProcessControlError::Protected));
        assert!(suspend_protected_pid(0, 100) && suspend_protected_pid(4, 100) && suspend_protected_pid(100, 100));
        assert!(!suspend_protected_pid(8, 100));
    }

    #[test]
    fn suspend_and_resume_gone_process_is_gone() {
        let key = {
            let v = Victim::spawn();
            v.key()
        };
        assert_eq!(suspend_process(key), Err(ProcessControlError::Gone));
        assert_eq!(resume_process(key), Err(ProcessControlError::Gone));
    }

    #[test]
    fn suspend_wrong_create_time_is_gone() {
        let v = Victim::spawn();
        let mut key = v.key();
        key.create_time += 1;
        assert_eq!(suspend_process(key), Err(ProcessControlError::Gone), "PID 被复用时不能挂起新进程");
    }

    #[test]
    fn control_state_reports_suspend_permission() {
        let v = Victim::spawn();
        let state = control_state(v.key()).unwrap();
        assert_eq!(state.suspend, Ok(()), "自己启动的子进程应可挂起");
    }

    #[test]
    fn still_suspended_follows_the_process_state() {
        let v = Victim::spawn();
        let key = v.key();
        assert_eq!(still_suspended(&[]), Vec::<ProcessKey>::new());
        assert_eq!(still_suspended(&[key]), Vec::<ProcessKey>::new(), "未挂起");
        suspend_process(key).unwrap();
        assert_eq!(still_suspended(&[key]), vec![key]);
        let reused = ProcessKey { create_time: key.create_time + 1, ..key };
        assert_eq!(still_suspended(&[reused]), Vec::<ProcessKey>::new(), "创建时间不同视为另一个进程");
        resume_process(key).unwrap();
        assert_eq!(still_suspended(&[key]), Vec::<ProcessKey>::new());
    }
}

//! TaskManagerLite 的采集与领域逻辑，不依赖界面。

mod apps;
mod autostart;
pub mod startup;
mod clipboard;
mod control;
pub mod csv;
mod cpu;
mod details;
mod diskio;
mod disks;
mod error;
mod etw;
mod icon;
mod instance;
mod memory;
mod nt;
mod network;
pub use network::{ConnectionKind, NetworkAdapter};
mod procctl;
mod procinfo;
mod sampler;
pub mod services;
mod runtask;
mod shellops;
mod snapshot;
mod system;
mod taskmgr;
mod view;
mod winops;

pub use apps::{app_pids, app_windows};
pub use autostart::{AUTOSTART_ARG, AUTOSTART_TEST_KEY_ENV, Autostart, AutostartError, AutostartKey};
pub use clipboard::{clipboard_text, set_clipboard_text};
pub use control::{EndError, end_process, is_elevated, relaunch_elevated};
pub use cpu::{CpuInfo, CpuSample, read_cpu_info};
pub use details::{Arch, Counters, DetailColumn, StaticInfo, StaticReader, local_time_text};
mod gpu;
pub use gpu::Gpu;
pub use disks::{DiskVolume, MediaType, PhysicalDisk};
pub use error::{ElevationError, SystemError};
pub use etw::{DiskTrace, StartError, TraceGuard, TraceState, stop_all as stop_disk_trace};
pub use icon::{IconBitmap, default_icon, file_icon};
pub use instance::{Acquired, SingleInstance, acquire, activate_own_window, is_relaunch};
pub use memory::{MemoryHardware, MemoryStatus};
pub use procctl::{
    AffinityInfo, ControlState, EcoOutcome, PriorityClass, ProcessControlError, affinity, control_state, eco_enabled,
    eco_mode, eco_protected_pid, eco_supported, eco_supported_build, mask_from_processors, processors_in_mask, set_affinity,
    set_eco_mode, set_priority, validate_affinity, resume_process, still_suspended, suspend_process, suspend_protected_pid,
};
pub use procinfo::{command_line, image_path, image_path_of};
pub use sampler::{DiskSource, Sample, Sampler, Scope};
pub use runtask::{Command as RunCommand, RunError, expand_env, parse_command, parse_command_with, run_task};
pub use shellops::{ShellError, reveal_in_explorer, search_online, search_url, show_properties};
pub use snapshot::{Peaks, Process, ProcessKey, ProcessSnapshot};
pub use system::SystemMetrics;
pub use taskmgr::{
    DebuggerValue, Ifeo, IfeoError, Registration, TEST_KEY_ENV, TEST_KEY_PREFIX, UnsafeReason,
    check_install_location, debugger_value, launched_as_taskmgr_debugger, run_cli,
    set_replace_taskmgr,
};
pub use winops::{
    INTEGRITY_HIGH, INTEGRITY_MEDIUM, Integrity, WindowError, WindowOp, WindowOps, WindowOutcome, WindowState, apply as apply_window_op,
    integrity_of_pid, is_foreground, needs_elevation, ops_enabled as window_ops_enabled, probe_window,
    succeeded as window_op_succeeded,
};
pub use view::{
    Group, GroupHeader, HEADER_INDEX, SortKey, Totals, ViewMode, ViewOptions, ViewRow,
    adopts_descendants, build_group_view, build_view, tree_from,
};

/// 本地时间，格式 `yyyy-MM-dd HH-mm-ss`，用作日志文件名。
pub fn local_timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// 本进程的创建时间与当前时间之差（毫秒），用于测量从进程创建到首帧的冷启动耗时。
pub fn ms_since_process_start() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    if unsafe { GetProcessTimes(GetCurrentProcess(), c, e, k, u) }.is_err() {
        return f64::NAN;
    }
    let now = system::filetime(unsafe { GetSystemTimePreciseAsFileTime() });
    now.saturating_sub(system::filetime(t[0])) as f64 / 10_000.0
}

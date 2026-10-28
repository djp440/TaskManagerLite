//! TaskManagerLite 的采集与领域逻辑，不依赖界面。

mod apps;
mod clipboard;
mod control;
mod cpu;
mod details;
mod diskio;
mod etw;
mod icon;
mod instance;
mod memory;
mod nt;
mod procinfo;
mod sampler;
mod shellops;
mod snapshot;
mod system;
mod taskmgr;
mod view;

pub use apps::app_pids;
pub use clipboard::{clipboard_text, set_clipboard_text};
pub use control::{EndError, end_process, is_elevated, relaunch_elevated};
pub use cpu::{CpuInfo, CpuSample, read_cpu_info};
pub use details::{Arch, Counters, DetailColumn, StaticInfo, StaticReader, local_time_text};
pub use etw::{DiskTrace, StartError, TraceGuard, TraceState, stop_all as stop_disk_trace};
pub use icon::{IconBitmap, default_icon, file_icon};
pub use instance::{Acquired, SingleInstance, acquire, activate_own_window, is_relaunch};
pub use memory::{MemoryHardware, MemoryStatus};
pub use procinfo::{command_line, image_path, image_path_of};
pub use sampler::{DiskSource, Sample, Sampler, Scope};
pub use shellops::{ShellError, reveal_in_explorer, search_online, search_url, show_properties};
pub use snapshot::{Process, ProcessKey, ProcessSnapshot};
pub use system::SystemMetrics;
pub use taskmgr::{
    Ifeo, IfeoError, Registration, TEST_KEY_ENV, TEST_KEY_PREFIX, check_install_location, debugger_value,
    launched_as_taskmgr_debugger, run_cli, set_replace_taskmgr,
};
pub use view::{
    Group, GroupHeader, HEADER_INDEX, SortKey, Totals, ViewMode, ViewOptions, ViewRow, build_group_view, build_view, tree_from,
};

/// 本地时间，格式 `yyyy-MM-dd HH-mm-ss`，用作日志文件名。
pub fn local_timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
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

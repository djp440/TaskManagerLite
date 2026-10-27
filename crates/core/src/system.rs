//! 系统级指标：CPU、内存、磁盘、网络。

use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{PCWSTR, w};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SystemMetrics {
    /// 0–100。
    pub cpu_usage: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    /// 磁盘活动时间，0–100。
    pub disk_active: f32,
    /// 字节/秒。
    pub disk_read: f64,
    pub disk_write: f64,
    pub net_recv: f64,
    pub net_send: f64,
}

pub(crate) fn filetime(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// 整机 CPU 时间累计值（100ns）。`total` 是所有逻辑处理器的可用时间之和。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CpuTimes {
    pub idle: u64,
    pub total: u64,
}

pub(crate) fn cpu_times() -> CpuTimes {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_err() {
        return CpuTimes::default();
    }
    // 内核时间已包含空闲时间。
    CpuTimes { idle: filetime(idle), total: filetime(kernel) + filetime(user) }
}

pub(crate) fn memory() -> (u64, u64) {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_err() {
        return (0, 0);
    }
    (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys)
}

/// 网卡累计收发字节数。只统计物理网卡，跳过过滤层与回环，避免重复计数。
pub(crate) fn network_octets() -> Option<(u64, u64)> {
    const HARDWARE_INTERFACE: u8 = 0b01;
    const FILTER_INTERFACE: u8 = 0b10;
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
        return None;
    }
    let (mut recv, mut send) = (0u64, 0u64);
    unsafe {
        let n = (*table).NumEntries as usize;
        let rows: &[MIB_IF_ROW2] = std::slice::from_raw_parts(std::ptr::addr_of!((*table).Table).cast(), n);
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            if flags & HARDWARE_INTERFACE != 0 && flags & FILTER_INTERFACE == 0 {
                recv += row.InOctets;
                send += row.OutOctets;
            }
        }
        FreeMibTable(table.cast());
    }
    Some((recv, send))
}

/// 通过 PDH 英文计数器读取磁盘活动（中文系统上本地化计数器名不同，必须用英文接口）。
pub(crate) struct DiskCounters {
    query: PDH_HQUERY,
    idle: PDH_HCOUNTER,
    read: PDH_HCOUNTER,
    write: PDH_HCOUNTER,
}

// PDH 句柄只在采样线程内使用。
unsafe impl Send for DiskCounters {}

impl DiskCounters {
    pub fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let add = |path: PCWSTR| {
                let mut c = PDH_HCOUNTER::default();
                (PdhAddEnglishCounterW(query, path, 0, &mut c) == 0).then_some(c)
            };
            let counters = (
                add(w!("\\PhysicalDisk(_Total)\\% Idle Time")),
                add(w!("\\PhysicalDisk(_Total)\\Disk Read Bytes/sec")),
                add(w!("\\PhysicalDisk(_Total)\\Disk Write Bytes/sec")),
            );
            let (Some(idle), Some(read), Some(write)) = counters else {
                PdhCloseQuery(query);
                return None;
            };
            // 速率类计数器需要两次收集，先收一次作为基线。
            PdhCollectQueryData(query);
            Some(Self { query, idle, read, write })
        }
    }

    /// 返回 (活动时间 %, 读 B/s, 写 B/s)。
    pub fn read(&mut self) -> (f32, f64, f64) {
        unsafe {
            PdhCollectQueryData(self.query);
            let value = |c: PDH_HCOUNTER| {
                let mut v = PDH_FMT_COUNTERVALUE::default();
                if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) == 0 { v.Anonymous.doubleValue } else { 0.0 }
            };
            let idle = value(self.idle).clamp(0.0, 100.0);
            ((100.0 - idle) as f32, value(self.read), value(self.write))
        }
    }
}

impl Drop for DiskCounters {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

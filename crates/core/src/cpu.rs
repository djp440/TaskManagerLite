//! CPU 详情：每个逻辑处理器的占用、当前频率，以及只读一次的静态信息（型号、拓扑、缓存、虚拟化）。

use std::sync::Arc;

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessorPerformanceInformation};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::Power::{CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetTickCount64, RelationAll, RelationCache, RelationProcessorCore,
    RelationProcessorPackage, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{GetActiveProcessorCount, IsProcessorFeaturePresent, PF_VIRT_FIRMWARE_ENABLED};
use windows::core::{PCWSTR, w};

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const CPU_KEY: PCWSTR = w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");

/// 只读一次的 CPU 静态信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuInfo {
    pub name: String,
    /// 基准频率，MHz。读不到时为 0。
    pub base_mhz: u32,
    pub sockets: u32,
    pub cores: u32,
    pub logical: u32,
    /// 各级缓存的总大小（所有实例求和，字节）。
    pub l1_bytes: u64,
    pub l2_bytes: u64,
    pub l3_bytes: u64,
    /// 固件中是否启用了虚拟化。
    pub virtualization: bool,
}

/// 每次采样附带的 CPU 详情。
#[derive(Debug, Clone, Default)]
pub struct CpuSample {
    /// 各逻辑处理器的占用，0–100。首次采样没有基线，全为 0。
    pub per_core: Vec<f32>,
    /// 当前频率（MHz）；PDH 计数器尚不可用时为 `None`。
    pub current_mhz: Option<f64>,
    pub threads: u32,
    pub handles: u32,
    /// 系统已运行的秒数。
    pub uptime_secs: u64,
    /// 静态信息；首个快照之后才读取，在此之前为 `None`。
    pub info: Option<Arc<CpuInfo>>,
}

/// 读取 CPU 静态信息：注册表（型号）、`CallNtPowerInformation`（基准频率）、`GetLogicalProcessorInformationEx`（拓扑与缓存）。
pub fn read_cpu_info() -> CpuInfo {
    let mut info = CpuInfo {
        name: registry_string("ProcessorNameString").map(|s| s.trim().to_string()).unwrap_or_default(),
        base_mhz: pick_base_mhz(power_max_mhz(), registry_dword("~MHz")),
        virtualization: unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) }.as_bool(),
        ..Default::default()
    };
    if let Some(buf) = logical_processor_information() {
        let t = parse_topology(&buf);
        info.sockets = t.sockets;
        info.cores = t.cores;
        info.logical = t.logical;
        info.l1_bytes = t.l1;
        info.l2_bytes = t.l2;
        info.l3_bytes = t.l3;
    }
    info
}

/// 基准频率：优先用电源接口的 MaxMhz（与原生任务管理器、WMI 一致，如 3900），取不到再退回注册表 `~MHz`（如 3893）。
fn pick_base_mhz(power: Option<u32>, registry: Option<u32>) -> u32 {
    power.filter(|&m| m > 0).or(registry).unwrap_or(0)
}

/// `CallNtPowerInformation(ProcessorInformation)` 返回每个逻辑处理器的 MaxMhz，取最大值。
fn power_max_mhz() -> Option<u32> {
    let count = unsafe { GetActiveProcessorCount(0xFFFF) }.max(1) as usize;
    let mut buf = vec![PROCESSOR_POWER_INFORMATION::default(); count];
    let bytes = (count * size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    let status = unsafe { CallNtPowerInformation(ProcessorInformation, None, 0, Some(buf.as_mut_ptr().cast()), bytes) };
    if status.0 < 0 {
        return None;
    }
    buf.iter().map(|p| p.MaxMhz).max().filter(|&m| m > 0)
}

fn registry_string(name: &str) -> Option<String> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut bytes = 0u32;
    unsafe {
        let ok = |e: windows::Win32::Foundation::WIN32_ERROR| e == ERROR_SUCCESS;
        if !ok(RegGetValueW(HKEY_LOCAL_MACHINE, CPU_KEY, PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes))) {
            return None;
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
        if !ok(RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CPU_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )) {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn registry_dword(name: &str) -> Option<u32> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let (mut value, mut bytes) = (0u32, 4u32);
    let e = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CPU_KEY,
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    (e == ERROR_SUCCESS).then_some(value)
}

/// 取回 `GetLogicalProcessorInformationEx(RelationAll)` 的原始缓冲区（8 字节对齐）。
fn logical_processor_information() -> Option<Vec<u64>> {
    let mut bytes = 0u32;
    // 第一次调用只为得到所需长度，必然以 ERROR_INSUFFICIENT_BUFFER 失败。
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut bytes) };
    if bytes == 0 {
        return None;
    }
    let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
    unsafe {
        GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr().cast()), &mut bytes).ok()?;
    }
    buf.truncate((bytes as usize).div_ceil(8));
    Some(buf)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Topology {
    sockets: u32,
    cores: u32,
    logical: u32,
    l1: u64,
    l2: u64,
    l3: u64,
}

/// 遍历变长记录链：封装、内核、缓存各自累加。缓存按所有实例求和（与原生任务管理器一致）。
fn parse_topology(buf: &[u64]) -> Topology {
    let base = buf.as_ptr().cast::<u8>();
    let total = buf.len() * 8;
    let mut t = Topology::default();
    let mut offset = 0usize;
    while offset + size_of::<u32>() * 2 <= total {
        // SAFETY: 记录起点按 8 字节对齐，Size 字段给出下一条记录的偏移，不会越过缓冲区。
        let rec = unsafe { &*base.add(offset).cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() };
        if rec.Size == 0 || offset + rec.Size as usize > total {
            break;
        }
        unsafe {
            match rec.Relationship {
                r if r == RelationProcessorPackage => t.sockets += 1,
                r if r == RelationProcessorCore => {
                    t.cores += 1;
                    let p = &rec.Anonymous.Processor;
                    let masks = std::slice::from_raw_parts(p.GroupMask.as_ptr(), p.GroupCount as usize);
                    t.logical += masks.iter().map(|m| m.Mask.count_ones()).sum::<u32>();
                }
                r if r == RelationCache => {
                    let c = &rec.Anonymous.Cache;
                    let size = u64::from(c.CacheSize);
                    match c.Level {
                        1 => t.l1 += size,
                        2 => t.l2 += size,
                        3 => t.l3 += size,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        offset += rec.Size as usize;
    }
    t
}

/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION`，每个逻辑处理器一条。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPerformance {
    idle_time: i64,
    /// 内核时间，已包含空闲时间。
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
}

/// 各逻辑处理器的累计时间 (空闲, 总计)，100ns 单位。
fn core_times(buf: &mut Vec<ProcessorPerformance>) -> Vec<(u64, u64)> {
    loop {
        let byte_len = (buf.len() * size_of::<ProcessorPerformance>()) as u32;
        let mut returned = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(SystemProcessorPerformanceInformation, buf.as_mut_ptr().cast(), byte_len, &mut returned)
        };
        if status.0 == STATUS_INFO_LENGTH_MISMATCH || (buf.is_empty() && status.is_ok()) {
            let want = (returned as usize).div_ceil(size_of::<ProcessorPerformance>()).max(buf.len() * 2).max(16);
            buf.resize(want, ProcessorPerformance::default());
            continue;
        }
        if status.is_err() {
            return Vec::new();
        }
        let n = returned as usize / size_of::<ProcessorPerformance>();
        return buf[..n]
            .iter()
            .map(|p| (p.idle_time.max(0) as u64, (p.kernel_time + p.user_time).max(0) as u64))
            .collect();
    }
}

/// 两次累计时间做差得到各核占用（0–100）。核数变化（热插拔）时按新核数重算，缺基线的核记 0。
fn usage_between(prev: &[(u64, u64)], cur: &[(u64, u64)]) -> Vec<f32> {
    cur.iter()
        .enumerate()
        .map(|(i, &(idle, total))| {
            let Some(&(pi, pt)) = prev.get(i) else { return 0.0 };
            let dt = total.saturating_sub(pt);
            if dt == 0 {
                return 0.0;
            }
            (100.0 * (1.0 - idle.saturating_sub(pi) as f64 / dt as f64)).clamp(0.0, 100.0) as f32
        })
        .collect()
}

/// PDH 英文计数器 `% Processor Performance`：当前频率相对基准频率的百分比（睿频时 > 100）。
struct FrequencyCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

// PDH 句柄只在采样线程内使用。
unsafe impl Send for FrequencyCounter {}

impl FrequencyCounter {
    fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, w!("\\Processor Information(_Total)\\% Processor Performance"), 0, &mut counter)
                != 0
            {
                PdhCloseQuery(query);
                return None;
            }
            // 需要两次收集才有数值，先收一次作为基线。
            PdhCollectQueryData(query);
            Some(Self { query, counter })
        }
    }

    fn read(&mut self) -> Option<f64> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut v = PDH_FMT_COUNTERVALUE::default();
            (PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, None, &mut v) == 0)
                .then(|| v.Anonymous.doubleValue)
                .filter(|p| *p > 0.0)
        }
    }
}

impl Drop for FrequencyCounter {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// 进程快照里汇总出的线程数与句柄数。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProcessTotals {
    pub threads: u32,
    pub handles: u32,
}

/// 采样线程上的 CPU 详情探针，持有每核基线与延迟打开的静态信息、PDH 计数器。
#[derive(Default)]
pub(crate) struct CpuProbe {
    buf: Vec<ProcessorPerformance>,
    prev: Vec<(u64, u64)>,
    info: Option<Arc<CpuInfo>>,
    frequency: Option<FrequencyCounter>,
    deferred_done: bool,
}

impl CpuProbe {
    /// `first`：这是第一次采样。静态信息与 PDH 计数器都推迟到首个快照之后，不拖慢冷启动。
    pub fn sample(&mut self, first: bool, totals: ProcessTotals) -> CpuSample {
        let cur = core_times(&mut self.buf);
        let per_core = if first { vec![0.0; cur.len()] } else { usage_between(&self.prev, &cur) };
        self.prev = cur;

        if !first && !self.deferred_done {
            self.deferred_done = true;
            self.info = Some(Arc::new(read_cpu_info()));
            self.frequency = FrequencyCounter::open();
        }
        let base = self.info.as_ref().map_or(0, |i| i.base_mhz);
        let current_mhz = match (&mut self.frequency, base) {
            (Some(f), b) if b > 0 => f.read().map(|p| f64::from(b) * p / 100.0),
            _ => None,
        };
        CpuSample {
            per_core,
            current_mhz,
            threads: totals.threads,
            handles: totals.handles,
            uptime_secs: unsafe { GetTickCount64() } / 1000,
            info: self.info.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_per_core_share_of_busy_time() {
        // 核 0 在 1000 个单位里空闲 250，核 1 全忙，核 2 全闲。
        let prev = [(0, 0), (0, 0), (0, 0)];
        let cur = [(250, 1000), (0, 1000), (1000, 1000)];
        let u = usage_between(&prev, &cur);
        assert!((u[0] - 75.0).abs() < 0.01);
        assert!((u[1] - 100.0).abs() < 0.01);
        assert_eq!(u[2], 0.0);
    }

    #[test]
    fn usage_handles_new_cores_and_zero_elapsed() {
        assert_eq!(usage_between(&[(5, 10)], &[(5, 10), (1, 2)]), vec![0.0, 0.0]);
        assert!(usage_between(&[(0, 0)], &[]).is_empty());
    }

    #[test]
    fn real_core_times_match_logical_count() {
        let mut buf = Vec::new();
        let times = core_times(&mut buf);
        let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(times.len(), logical);
        assert!(times.iter().all(|&(idle, total)| total >= idle && total > 0));
    }

    #[test]
    fn base_mhz_prefers_power_interface_then_registry() {
        assert_eq!(pick_base_mhz(Some(3900), Some(3893)), 3900);
        assert_eq!(pick_base_mhz(None, Some(3893)), 3893);
        assert_eq!(pick_base_mhz(Some(0), Some(3893)), 3893);
        assert_eq!(pick_base_mhz(None, None), 0);
    }

    #[test]
    fn static_info_is_populated() {
        let i = read_cpu_info();
        assert!(!i.name.is_empty());
        assert!(i.base_mhz > 0);
        // 基准频率来自电源接口（整百兆，如 3900），不是注册表里的 3893 之类。
        assert_eq!(i.base_mhz, pick_base_mhz(power_max_mhz(), registry_dword("~MHz")));
        assert!(i.sockets >= 1 && i.cores >= 1 && i.logical >= i.cores);
        assert!(i.l2_bytes > 0 && i.l3_bytes > 0);
    }
}

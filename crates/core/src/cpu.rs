//! CPU 详情：每个逻辑处理器的占用、当前频率，以及只读一次的静态信息（型号、拓扑、缓存、虚拟化）。

use std::sync::Arc;

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS, SystemProcessorPerformanceInformation,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, NTSTATUS};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Power::{CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetTickCount64, RelationAll, RelationCache, RelationProcessorCore,
    RelationProcessorPackage, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, IsProcessorFeaturePresent, PF_VIRT_FIRMWARE_ENABLED,
};
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

/// `NtQuerySystemInformationEx` 的签名：`ntdll` 导出，`windows` crate 没有绑定，只在多处理器组机器上才解析。
type NtQuerySystemInformationEx = unsafe extern "system" fn(
    class: SYSTEM_INFORMATION_CLASS,
    input: *const c_void,
    input_len: u32,
    output: *mut c_void,
    output_len: u32,
    returned: *mut u32,
) -> NTSTATUS;

fn nt_query_ex() -> Option<NtQuerySystemInformationEx> {
    static F: OnceLock<Option<NtQuerySystemInformationEx>> = OnceLock::new();
    *F.get_or_init(|| unsafe {
        let ntdll = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        let f = GetProcAddress(ntdll, windows::core::s!("NtQuerySystemInformationEx"))?;
        // SAFETY: 导出函数的签名即上面的类型。
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, NtQuerySystemInformationEx>(f))
    })
}

/// 把各组各自读到的数据按组号顺序拼成逐核序列。
///
/// `group_cores[g]` 是第 g 组的逻辑处理器数；`read_group(g)` 返回该组读到的数据。
/// 某组多返回的丢弃；少返回（读取失败）的沿用 `prev` 里同一位置上一次的值（没有则用默认值），
/// 使后面各组的核序号不错位，也不会因为一次偶发失败在下一轮差值里出现虚假尖峰。
fn collect_groups<T: Clone + Default>(
    group_cores: &[u32],
    prev: &[T],
    mut read_group: impl FnMut(u16) -> Vec<T>,
) -> Vec<T> {
    let mut all: Vec<T> = Vec::with_capacity(group_cores.iter().map(|&n| n as usize).sum());
    for (g, &cores) in group_cores.iter().enumerate() {
        let start = all.len();
        let mut data = read_group(g as u16);
        data.truncate(cores as usize);
        let got = data.len();
        all.extend(data);
        all.extend((got..cores as usize).map(|i| prev.get(start + i).cloned().unwrap_or_default()));
    }
    all
}

/// 多组路径下每组缓冲的最小项数（一个处理器组最多 64 个逻辑处理器）。
const MIN_GROUP_ENTRIES: usize = 64;

/// 调用一次查询并整理结果。`call(缓冲, 字节数, 返回字节数)` 返回状态。
///
/// `expected` 是该组的活动核数（普通接口传 0 表示不校验）。`NtQuerySystemInformationEx` 在缓冲偏小时
/// 不报 `STATUS_INFO_LENGTH_MISMATCH`，而是成功返回并只填满放得下的部分，所以：缓冲先扩到至少
/// `expected`（与 `MIN_GROUP_ENTRIES` 取大），并且返回的项数少于 `expected` 时扩容重试（最多 2 次：
/// 系统确实只返回更少的核，例如有核被停用时，不能无限重试）。
fn read_performance(
    buf: &mut Vec<ProcessorPerformance>,
    expected: usize,
    mut call: impl FnMut(*mut c_void, u32, &mut u32) -> NTSTATUS,
) -> Vec<(u64, u64)> {
    const ENTRY: usize = size_of::<ProcessorPerformance>();
    if expected > 0 && buf.len() < expected.max(MIN_GROUP_ENTRIES) {
        buf.resize(expected.max(MIN_GROUP_ENTRIES), ProcessorPerformance::default());
    }
    let mut short_retries = 0;
    loop {
        let byte_len = (buf.len() * ENTRY) as u32;
        let mut returned = 0u32;
        let status = call(buf.as_mut_ptr().cast(), byte_len, &mut returned);
        if status.0 == STATUS_INFO_LENGTH_MISMATCH || (buf.is_empty() && status.is_ok()) {
            let want = (returned as usize).div_ceil(ENTRY).max(buf.len() * 2).max(16);
            buf.resize(want, ProcessorPerformance::default());
            continue;
        }
        if status.is_err() {
            return Vec::new();
        }
        let n = returned as usize / ENTRY;
        if n < expected && short_retries < 2 {
            // 可能被静默截断：返回的核数少于该组的活动核数。
            short_retries += 1;
            let want = (buf.len() * 2).max(expected);
            buf.resize(want, ProcessorPerformance::default());
            continue;
        }
        return buf[..n]
            .iter()
            .map(|p| (p.idle_time.max(0) as u64, (p.kernel_time + p.user_time).max(0) as u64))
            .collect();
    }
}

/// 读取一次 `SystemProcessorPerformanceInformation`。`group` 为 `None` 时用普通接口（只含调用线程所在组），
/// 否则用 `Ex` 接口读取指定组（`expected` 为该组活动核数）。
fn query_performance(buf: &mut Vec<ProcessorPerformance>, group: Option<(u16, usize)>) -> Vec<(u64, u64)> {
    match group {
        None => read_performance(buf, 0, |out, len, returned| unsafe {
            NtQuerySystemInformation(SystemProcessorPerformanceInformation, out, len, returned)
        }),
        Some((g, expected)) => {
            let Some(f) = nt_query_ex() else { return Vec::new() };
            read_performance(buf, expected, |out, len, returned| unsafe {
                f(SystemProcessorPerformanceInformation, (&g as *const u16).cast(), 2, out, len, returned)
            })
        }
    }
}

/// 各逻辑处理器的累计时间 (空闲, 总计)，100ns 单位。
///
/// 只有一个处理器组时走原来的单次调用；超过 64 个逻辑处理器（多个组）时按组读取后拼接。
/// `prev` 是上一轮的结果，用来填补偶发读取失败的组。
fn core_times(buf: &mut Vec<ProcessorPerformance>, prev: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let groups = unsafe { GetActiveProcessorGroupCount() };
    // 解析不到 Ex 接口时退回普通接口：至少能拿到当前组的核，好过全部为空。
    if groups <= 1 || nt_query_ex().is_none() {
        return query_performance(buf, None);
    }
    let group_cores: Vec<u32> = (0..groups).map(|g| unsafe { GetActiveProcessorCount(g) }).collect();
    collect_groups(&group_cores, prev, |g| query_performance(buf, Some((g, group_cores[g as usize] as usize))))
}

/// 两次累计时间做差得到各核占用（0–100）。核数变化（热插拔）时按新核数重算，缺基线的核记 0。
fn usage_between(prev: &[(u64, u64)], cur: &[(u64, u64)]) -> Vec<f32> {
    cur.iter()
        .enumerate()
        .map(|(i, &(idle, total))| {
            let Some(&(pi, pt)) = prev.get(i) else { return 0.0 };
            // 基线总时间为 0 说明从未读到过这个核（首次采样某组失败时用默认值补齐），差值会是开机以来累计，不可信。
            if pt == 0 {
                return 0.0;
            }
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
        let cur = core_times(&mut self.buf, &self.prev);
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
        let prev = [(100, 100), (100, 100), (100, 100)];
        let cur = [(350, 1100), (100, 1100), (1100, 1100)];
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
    fn usage_without_baseline_is_zero() {
        // 首次采样某组读取失败被补成 (0, 0)：下一拍不能把开机以来的累计当成这一拍的占用。
        assert_eq!(usage_between(&[(0, 0), (100, 1000)], &[(300, 1000), (150, 1100)]), vec![0.0, 50.0]);
    }

    /// 模拟按组读取：第 g 组每个核的数据 = (组号 * 1000 + 组内序号)。
    fn fake_group(g: u16, n: usize) -> Vec<u32> {
        (0..n as u32).map(|i| u32::from(g) * 1000 + i).collect()
    }

    #[test]
    fn collect_groups_single_group_is_passthrough() {
        let all = collect_groups(&[12], &[], |g| fake_group(g, 12));
        assert_eq!(all, fake_group(0, 12));
    }

    #[test]
    fn collect_groups_concatenates_uneven_groups_in_order() {
        // 2 组核数不等（如 64 + 40）：先第 0 组再第 1 组，序号连续。
        let counts = [64, 40];
        let all = collect_groups(&counts, &[], |g| fake_group(g, counts[g as usize] as usize));
        assert_eq!(all.len(), 104);
        assert_eq!(all[63], 63);
        assert_eq!(all[64], 1000);
        assert_eq!(all[103], 1039);
        // 三组、含空组也不错位。
        let all = collect_groups(&[2, 0, 3], &[], |g| fake_group(g, [2, 0, 3][g as usize]));
        assert_eq!(all, vec![0, 1, 2000, 2001, 2002]);
    }

    #[test]
    fn collect_groups_pads_failed_group_and_truncates_extra() {
        // 第 0 组读取失败（空）：补默认值，第 1 组仍在 3.. 位置；第 1 组多返回的丢弃。
        let all = collect_groups(&[3, 2], &[], |g| if g == 0 { Vec::new() } else { fake_group(g, 5) });
        assert_eq!(all, vec![0, 0, 0, 1000, 1001]);
        // 少返回也补齐。
        let all = collect_groups(&[4, 2], &[], |g| fake_group(g, if g == 0 { 1 } else { 2 }));
        assert_eq!(all, vec![0, 0, 0, 0, 1000, 1001]);
        assert!(collect_groups::<u32>(&[], &[], |_| vec![1]).is_empty());
    }

    #[test]
    fn collect_groups_reuses_previous_values_for_failed_group() {
        // 第 1 组偶发读取失败：沿用上一轮同一位置的值，而不是 0（否则下一轮差值会出现虚假尖峰）。
        let prev = [10u32, 11, 12, 13, 14];
        let all = collect_groups(&[3, 2], &prev, |g| if g == 0 { fake_group(g, 3) } else { Vec::new() });
        assert_eq!(all, vec![0, 1, 2, 13, 14]);
        // 部分返回：已读到的用新值，缺的用旧值。
        let all = collect_groups(&[3, 2], &prev, |g| if g == 0 { vec![7] } else { fake_group(g, 2) });
        assert_eq!(all, vec![7, 11, 12, 1000, 1001]);
        // 旧值也没有（核数增加）：默认值。
        assert_eq!(collect_groups(&[3], &[5u32], |_| Vec::new()), vec![5, 0, 0]);
    }

    #[test]
    fn collect_groups_queries_each_group_once_in_order() {
        let mut seen = Vec::new();
        collect_groups(&[1, 1, 1], &[], |g| {
            seen.push(g);
            vec![0u8]
        });
        assert_eq!(seen, vec![0, 1, 2]);
    }

    /// 模拟 `NtQuerySystemInformationEx` 的真实行为：缓冲偏小时不报 MISMATCH，而是成功返回并只填满放得下的部分。
    fn fake_ex(cores: usize) -> impl FnMut(*mut c_void, u32, &mut u32) -> NTSTATUS {
        const ENTRY: usize = size_of::<ProcessorPerformance>();
        move |out, len, returned| {
            let n = cores.min(len as usize / ENTRY);
            let out = out.cast::<ProcessorPerformance>();
            for i in 0..n {
                // idle = i，kernel = 10 * (i + 1)，user = 0：total 随核号变化，便于检查没有错位。
                unsafe {
                    out.add(i).write(ProcessorPerformance {
                        idle_time: i as i64,
                        kernel_time: 10 * (i as i64 + 1),
                        ..Default::default()
                    })
                };
            }
            *returned = (n * ENTRY) as u32;
            NTSTATUS(0)
        }
    }

    #[test]
    fn ex_read_is_not_silently_truncated_by_a_small_reused_buffer() {
        // 缓冲只够 11 项（比如按核数较少的第 0 组扩容后复用）：后面核数更多的组仍要读全。
        let mut buf = vec![ProcessorPerformance::default(); 11];
        let times = read_performance(&mut buf, 64, fake_ex(64));
        assert_eq!(times.len(), 64);
        assert_eq!(times[63], (63, 640));
        // 超过 64 项（假设）同样读全。
        let mut buf = Vec::new();
        assert_eq!(read_performance(&mut buf, 100, fake_ex(100)).len(), 100);
        // 系统确实只返回更少的核（如有核被停用）：重试有限次后照实接受，不会死循环。
        let mut buf = Vec::new();
        let mut calls = 0;
        let mut inner = fake_ex(5);
        assert_eq!(read_performance(&mut buf, 8, |o, l, r| { calls += 1; inner(o, l, r) }).len(), 5);
        assert_eq!(calls, 3);
    }

    #[test]
    fn ex_read_retries_when_buffer_is_filled_but_cores_remain() {
        // 模拟预留失效：第一次只填了 64 项而活动核数是 130，必须扩容重试直到读全。
        let mut calls = 0;
        let mut inner = fake_ex(130);
        let mut buf = vec![ProcessorPerformance::default(); 64];
        let times = read_performance(&mut buf, 130, |out, len, ret| {
            calls += 1;
            let cap = if calls == 1 { 64 * size_of::<ProcessorPerformance>() } else { len as usize };
            inner(out, cap.min(len as usize) as u32, ret)
        });
        assert_eq!(times.len(), 130);
        assert_eq!(calls, 2);
    }

    #[test]
    fn plain_read_keeps_original_behavior() {
        // 普通接口（expected = 0）：空缓冲先成功返回长度则扩容；MISMATCH 时扩容重试；出错返回空。
        let mut buf = Vec::new();
        assert_eq!(read_performance(&mut buf, 0, fake_ex(12)).len(), 12);
        let mut buf = Vec::new();
        assert!(read_performance(&mut buf, 0, |_, _, _| NTSTATUS(0xC000_0022_u32 as i32)).is_empty());
    }

    #[test]
    fn real_core_times_match_logical_count() {
        let mut buf = Vec::new();
        let times = core_times(&mut buf, &[]);
        let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(times.len(), logical);
        assert!(times.iter().all(|&(idle, total)| total >= idle && total > 0));
    }

    #[test]
    fn ex_query_for_group_zero_matches_plain_query() {
        // 单组机器上，Ex 接口读第 0 组应与普通接口返回同样数量的核（验证动态解析的调用约定）。
        let mut buf = Vec::new();
        let plain = query_performance(&mut buf, None);
        let ex = query_performance(&mut buf, Some((0, plain.len())));
        assert_eq!(ex.len(), plain.len());
        assert!(ex.iter().all(|&(idle, total)| total >= idle && total > 0));
        // 不存在的组读不到数据，返回空而不是 panic。
        assert!(query_performance(&mut buf, Some((60, 4))).is_empty());
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

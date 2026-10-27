//! 内存详情：物理内存组成、提交、内核池，以及 SMBIOS 里的内存条硬件信息。
//!
//! 口径与原生任务管理器一致：
//! - 使用中 = 物理总量 − 可用（可用 = 空闲 + 零页 + 备用）。
//! - 已缓存 = 备用 + 已修改（`SystemMemoryListInformation`）。
//! - 已压缩 = Memory Compression 进程的工作集（由进程枚举顺带提供）。
//! - 已提交 / 提交限制、分页池、非分页池来自 `GetPerformanceInfo`。
//!
//! 内存条的速度、插槽、外形规格读取 SMBIOS Type 17（不使用 WMI），解析是纯函数，可脱离系统单测。

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{
    GetPhysicallyInstalledSystemMemory, GetSystemFirmwareTable, GlobalMemoryStatusEx, MEMORYSTATUSEX, RSMB,
};

/// 每个采样间隔读取的内存状态，单位字节。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryStatus {
    /// 操作系统可见的物理内存总量（已扣除为硬件保留的部分）。
    pub total: u64,
    /// 可用 = 空闲 + 零页 + 备用。
    pub available: u64,
    /// 空闲 + 零页。
    pub free: u64,
    pub modified: u64,
    pub standby: u64,
    pub compressed: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub committed: u64,
    pub commit_limit: u64,
}

impl MemoryStatus {
    pub fn in_use(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    /// 已缓存 = 备用 + 已修改。
    pub fn cached(&self) -> u64 {
        self.standby + self.modified
    }
}

/// `SystemMemoryListInformation` 返回的页链表，单位为页。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PageLists {
    zero: u64,
    free: u64,
    modified: u64,
    modified_no_write: u64,
    standby: u64,
}

/// 只读取需要的字段，其余按系统布局保留。
#[repr(C)]
#[allow(dead_code)]
struct SystemMemoryListInformation {
    zero_page_count: usize,
    free_page_count: usize,
    modified_page_count: usize,
    modified_no_write_page_count: usize,
    bad_page_count: usize,
    page_count_by_priority: [usize; 8],
    repurposed_pages_by_priority: [usize; 8],
    modified_page_count_page_file: usize,
}

const SYSTEM_MEMORY_LIST_INFORMATION: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(80);

fn page_lists() -> Option<PageLists> {
    let mut info = std::mem::MaybeUninit::<SystemMemoryListInformation>::zeroed();
    let mut returned = 0u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION,
            info.as_mut_ptr().cast(),
            size_of::<SystemMemoryListInformation>() as u32,
            &mut returned,
        )
    };
    if status.is_err() {
        return None;
    }
    // SAFETY: 已按全零初始化，调用成功后由系统填写。
    let info = unsafe { info.assume_init() };
    Some(PageLists {
        zero: info.zero_page_count as u64,
        free: info.free_page_count as u64,
        modified: info.modified_page_count as u64,
        modified_no_write: info.modified_no_write_page_count as u64,
        standby: info.page_count_by_priority.iter().map(|&p| p as u64).sum(),
    })
}

/// 读取当前内存状态。`compressed` 是 Memory Compression 进程的工作集（字节）。
pub fn read_status(compressed: u64) -> MemoryStatus {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_err() {
        return MemoryStatus::default();
    }
    let mut perf = PERFORMANCE_INFORMATION { cb: size_of::<PERFORMANCE_INFORMATION>() as u32, ..Default::default() };
    let have_perf = unsafe { GetPerformanceInfo(&mut perf, perf.cb) }.is_ok();
    let page = if have_perf { perf.PageSize as u64 } else { 4096 };
    let pages = |n: usize| n as u64 * page;

    let (total, available) = (m.ullTotalPhys, m.ullAvailPhys);
    // 页链表读取失败（极少见）时，退化为用系统缓存近似“已缓存”，其余可用内存都算空闲。
    let (free, modified, standby) = match page_lists() {
        Some(l) => ((l.zero + l.free) * page, (l.modified + l.modified_no_write) * page, l.standby * page),
        None => {
            let cache = pages(perf.SystemCache).min(available);
            (available - cache, 0, cache)
        }
    };
    MemoryStatus {
        total,
        available,
        free,
        modified,
        standby,
        compressed,
        paged_pool: pages(perf.KernelPaged),
        nonpaged_pool: pages(perf.KernelNonpaged),
        committed: pages(perf.CommitTotal),
        commit_limit: pages(perf.CommitLimit),
    }
}

/// 只需读取一次的内存硬件信息。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryHardware {
    /// 已安装的物理内存（`GetPhysicallyInstalledSystemMemory`），未知为 0。
    pub installed: u64,
    /// 内存条速度（MT/s），未知为 `None`。
    pub speed_mts: Option<u32>,
    pub slots_used: u32,
    /// 插槽总数，SMBIOS 不可用时为 0。
    pub slots_total: u32,
    /// 外形规格，如 DIMM、SODIMM。
    pub form_factor: Option<&'static str>,
    /// 内存类型，如 DDR4、DDR5。
    pub memory_type: Option<&'static str>,
}

/// 读取内存硬件信息：安装容量加 SMBIOS Type 17。比较慢（毫秒级），不要放在首个快照之前。
pub fn read_hardware() -> MemoryHardware {
    let mut kb = 0u64;
    let installed = if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.is_ok() { kb * 1024 } else { 0 };
    let devices = read_smbios().map(|raw| parse_memory_devices(smbios_table(&raw))).unwrap_or_default();
    summarize(installed, &devices)
}

fn read_smbios() -> Option<Vec<u8>> {
    let len = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) };
    if got == 0 || got > len {
        return None;
    }
    buf.truncate(got as usize);
    Some(buf)
}

/// 一根内存条（SMBIOS Type 17 的一个条目，空插槽也算）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryDevice {
    /// 容量（MB）。0 表示空插槽；容量未知（0xFFFF）时也记为 0 并置 `size_unknown`。
    pub size_mb: u64,
    pub size_unknown: bool,
    /// 额定速度（MT/s），0 为未知。
    pub speed: u32,
    /// 当前配置的速度（MT/s），0 为未知。
    pub configured_speed: u32,
    pub form_factor: u8,
    pub memory_type: u8,
}

impl MemoryDevice {
    pub fn populated(&self) -> bool {
        self.size_mb > 0 || self.size_unknown
    }
}

const SMBIOS_TYPE_MEMORY_DEVICE: u8 = 17;
const SMBIOS_TYPE_END: u8 = 127;
/// `RawSMBIOSData` 的头：Used20CallingMethod、MajorVersion、MinorVersion、DmiRevision 各 1 字节，再加 4 字节长度。
const RAW_HEADER: usize = 8;

/// 去掉 `GetSystemFirmwareTable` 返回的 `RawSMBIOSData` 头，得到结构表。
pub fn smbios_table(raw: &[u8]) -> &[u8] {
    if raw.len() < RAW_HEADER {
        return &[];
    }
    let declared = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let body = &raw[RAW_HEADER..];
    &body[..declared.min(body.len())]
}

fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(off)?, *d.get(off + 1)?]))
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*d.get(off)?, *d.get(off + 1)?, *d.get(off + 2)?, *d.get(off + 3)?]))
}

/// 解析 SMBIOS 结构表里的全部 Type 17 条目。表被截断或格式损坏时返回已解析出的部分。
pub fn parse_memory_devices(table: &[u8]) -> Vec<MemoryDevice> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= table.len() {
        let (kind, len) = (table[pos], table[pos + 1] as usize);
        if len < 4 || pos + len > table.len() {
            break;
        }
        let formatted = &table[pos..pos + len];
        // 字符串集合紧跟格式化区域，以连续两个 NUL 结束（没有字符串时是两个 NUL）。
        let strings_start = pos + len;
        let mut end = strings_start;
        while end + 1 < table.len() && !(table[end] == 0 && table[end + 1] == 0) {
            end += 1;
        }
        if end + 1 >= table.len() {
            // 没有终止符：表被截断，当前条目不可信。
            break;
        }
        if kind == SMBIOS_TYPE_END {
            break;
        }
        if kind == SMBIOS_TYPE_MEMORY_DEVICE {
            out.push(parse_memory_device(formatted));
        }
        pos = end + 2;
    }
    out
}

fn parse_memory_device(d: &[u8]) -> MemoryDevice {
    // 容量：0 空插槽；0xFFFF 未知；0x7FFF 见扩展容量；最高位为 1 时单位是 KB，否则 MB。
    let size_field = u16_at(d, 0x0C).unwrap_or(0);
    let (size_mb, size_unknown) = match size_field {
        0 => (0, false),
        0xFFFF => (0, true),
        0x7FFF => (u64::from(u32_at(d, 0x1C).unwrap_or(0) & 0x7FFF_FFFF), false),
        s if s & 0x8000 != 0 => (u64::from(s & 0x7FFF) / 1024, false),
        s => (u64::from(s), false),
    };
    // 速度：0xFFFF 表示实际值在 SMBIOS 3.3 的扩展字段里。
    let speed = match u16_at(d, 0x15).unwrap_or(0) {
        0xFFFF => u32_at(d, 0x54).unwrap_or(0),
        s => u32::from(s),
    };
    let configured_speed = match u16_at(d, 0x20).unwrap_or(0) {
        0xFFFF => u32_at(d, 0x58).unwrap_or(0),
        s => u32::from(s),
    };
    MemoryDevice {
        size_mb,
        size_unknown,
        speed,
        configured_speed,
        form_factor: d.get(0x0E).copied().unwrap_or(0),
        memory_type: d.get(0x12).copied().unwrap_or(0),
    }
}

pub fn form_factor_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x03 => "SIMM",
        0x04 => "SIP",
        0x05 => "Chip",
        0x06 => "DIP",
        0x07 => "ZIP",
        0x08 => "Proprietary Card",
        0x09 => "DIMM",
        0x0A => "TSOP",
        0x0B => "Row of chips",
        0x0C => "RIMM",
        0x0D => "SODIMM",
        0x0E => "SRIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        _ => return None,
    })
}

pub fn memory_type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x14 => "DDR2 FB-DIMM",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => return None,
    })
}

/// 把内存条列表汇总成界面需要的基本信息。
/// 速度取已安装内存条里最低的有效值（多条混插时系统按最慢的运行）；优先使用当前配置速度。
pub fn summarize(installed: u64, devices: &[MemoryDevice]) -> MemoryHardware {
    let used: Vec<&MemoryDevice> = devices.iter().filter(|d| d.populated()).collect();
    let speed_mts = used.iter().filter_map(|d| [d.configured_speed, d.speed].into_iter().find(|&s| s > 0)).min();
    let first = used.first();
    let installed = if installed > 0 { installed } else { used.iter().map(|d| d.size_mb * 1024 * 1024).sum() };
    MemoryHardware {
        installed,
        speed_mts,
        slots_used: used.len() as u32,
        slots_total: devices.len() as u32,
        form_factor: first.and_then(|d| form_factor_name(d.form_factor)),
        memory_type: first.and_then(|d| memory_type_name(d.memory_type)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 Type 17 条目：格式化区域长度 `len`（SMBIOS 2.3 为 0x1B，3.3 为 0x5C），后接字符串集合。
    struct Dev {
        len: usize,
        size: u16,
        ext_size: u32,
        form: u8,
        mem_type: u8,
        speed: u16,
        configured: u16,
        ext_speed: u32,
        ext_configured: u32,
        locator: Option<&'static str>,
        extra_strings: &'static [&'static str],
    }

    impl Default for Dev {
        fn default() -> Self {
            Dev {
                len: 0x5C,
                size: 0,
                ext_size: 0,
                form: 0x09,
                mem_type: 0x1A,
                speed: 0,
                configured: 0,
                ext_speed: 0,
                ext_configured: 0,
                locator: None,
                extra_strings: &[],
            }
        }
    }

    fn put16(b: &mut [u8], off: usize, v: u16) {
        if off + 2 <= b.len() {
            b[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
    }

    fn put32(b: &mut [u8], off: usize, v: u32) {
        if off + 4 <= b.len() {
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    fn build(d: &Dev) -> Vec<u8> {
        let mut b = vec![0u8; d.len];
        b[0] = 17;
        b[1] = d.len as u8;
        put16(&mut b, 2, 0x0040);
        put16(&mut b, 0x0C, d.size);
        b[0x0E] = d.form;
        if d.locator.is_some() {
            b[0x10] = 1;
        }
        b[0x12] = d.mem_type;
        put16(&mut b, 0x15, d.speed);
        put32(&mut b, 0x1C, d.ext_size);
        put16(&mut b, 0x20, d.configured);
        put32(&mut b, 0x54, d.ext_speed);
        put32(&mut b, 0x58, d.ext_configured);
        let mut strings: Vec<&str> = d.locator.into_iter().collect();
        strings.extend_from_slice(d.extra_strings);
        if strings.is_empty() {
            b.extend_from_slice(&[0, 0]);
        } else {
            for s in strings {
                b.extend_from_slice(s.as_bytes());
                b.push(0);
            }
            b.push(0);
        }
        b
    }

    /// 与 Type 17 无关的条目（Type 16），带两个字符串。
    fn other_struct() -> Vec<u8> {
        let mut b = vec![0u8; 0x17];
        b[0] = 16;
        b[1] = 0x17;
        b.extend_from_slice(b"Vendor\0Board\0\0");
        b
    }

    fn end_struct() -> Vec<u8> {
        vec![127, 4, 0xFF, 0xFF, 0, 0]
    }

    fn table(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn parses_device_followed_by_string_tables() {
        let t = table(&[
            other_struct(),
            build(&Dev {
                size: 16384,
                speed: 3200,
                configured: 2933,
                form: 0x0D,
                locator: Some("ChannelA-DIMM0"),
                extra_strings: &["BANK 0", "Samsung"],
                ..Default::default()
            }),
            end_struct(),
        ]);
        let devs = parse_memory_devices(&t);
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].size_mb, 16384);
        assert_eq!(devs[0].speed, 3200);
        assert_eq!(devs[0].configured_speed, 2933);
        assert_eq!(devs[0].form_factor, 0x0D);
        assert!(devs[0].populated());
    }

    #[test]
    fn empty_slot_is_counted_but_not_used() {
        let t = table(&[
            build(&Dev { size: 8192, speed: 2666, locator: Some("A"), ..Default::default() }),
            build(&Dev { size: 0, locator: Some("B"), ..Default::default() }),
            end_struct(),
        ]);
        let devs = parse_memory_devices(&t);
        assert_eq!(devs.len(), 2);
        assert!(!devs[1].populated());
        let hw = summarize(8 * 1024 * 1024 * 1024, &devs);
        assert_eq!((hw.slots_used, hw.slots_total), (1, 2));
        assert_eq!(hw.speed_mts, Some(2666));
        assert_eq!(hw.form_factor, Some("DIMM"));
        assert_eq!(hw.memory_type, Some("DDR4"));
    }

    #[test]
    fn size_encodings() {
        let sized = |size: u16, ext: u32| {
            let t = table(&[build(&Dev { size, ext_size: ext, ..Default::default() }), end_struct()]);
            parse_memory_devices(&t).remove(0)
        };
        // 最高位为 1：单位 KB。
        assert_eq!(sized(0x8000 | 2048, 0).size_mb, 2);
        // 0x7FFF：用扩展容量（MB），最高位保留。
        assert_eq!(sized(0x7FFF, 32768).size_mb, 32768);
        assert_eq!(sized(0x7FFF, 0x8000_0000 | 65536).size_mb, 65536);
        // 容量未知：算已安装，但没有容量数字。
        let unknown = sized(0xFFFF, 0);
        assert!(unknown.populated());
        assert_eq!(unknown.size_mb, 0);
    }

    #[test]
    fn speed_zero_falls_back_and_extended_fields() {
        let t = table(&[
            // 额定速度未知，配置速度有值。
            build(&Dev { size: 4096, speed: 0, configured: 1600, ..Default::default() }),
            // 速度 0xFFFF：用 SMBIOS 3.3 扩展字段。
            build(&Dev {
                size: 4096,
                speed: 0xFFFF,
                configured: 0xFFFF,
                ext_speed: 6400,
                ext_configured: 5600,
                ..Default::default()
            }),
            end_struct(),
        ]);
        let devs = parse_memory_devices(&t);
        assert_eq!((devs[0].speed, devs[0].configured_speed), (0, 1600));
        assert_eq!((devs[1].speed, devs[1].configured_speed), (6400, 5600));
        // 混插取最慢的有效速度。
        assert_eq!(summarize(0, &devs).speed_mts, Some(1600));
    }

    #[test]
    fn all_speeds_zero_means_unknown() {
        let t = table(&[build(&Dev { size: 4096, ..Default::default() }), end_struct()]);
        let hw = summarize(0, &parse_memory_devices(&t));
        assert_eq!(hw.speed_mts, None);
        // 固件没报安装容量时，用各条容量之和。
        assert_eq!(hw.installed, 4096 * 1024 * 1024);
    }

    #[test]
    fn short_smbios_2_3_structure_has_no_extended_fields() {
        // SMBIOS 2.3：长度 0x1B，没有配置速度与扩展字段，越界读取必须安全返回 0。
        let t = table(&[build(&Dev { len: 0x1B, size: 4096, speed: 2133, ..Default::default() }), end_struct()]);
        let devs = parse_memory_devices(&t);
        assert_eq!((devs[0].speed, devs[0].configured_speed), (2133, 0));
        assert_eq!(summarize(0, &devs).speed_mts, Some(2133));
    }

    #[test]
    fn truncated_table_and_garbage() {
        // 截断：第二个条目缺少终止符，只保留第一个。
        let mut t = table(&[build(&Dev { size: 4096, locator: Some("A"), ..Default::default() })]);
        t.extend_from_slice(&build(&Dev { size: 4096, ..Default::default() })[..0x30]);
        assert_eq!(parse_memory_devices(&t).len(), 1);
        // 垃圾数据不 panic。
        assert!(parse_memory_devices(&[17, 2, 0, 0, 0, 0]).is_empty());
        assert!(parse_memory_devices(&[]).is_empty());
    }

    #[test]
    fn raw_header_is_stripped() {
        let body = table(&[build(&Dev { size: 4096, ..Default::default() }), end_struct()]);
        let mut raw = vec![0, 3, 4, 0];
        raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
        raw.extend_from_slice(&body);
        // 声明长度之后的多余字节要被忽略。
        raw.extend_from_slice(&[17, 0x5C, 1, 2, 3]);
        assert_eq!(parse_memory_devices(smbios_table(&raw)).len(), 1);
        assert!(smbios_table(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn status_in_use_and_cached() {
        let s = MemoryStatus { total: 100, available: 30, standby: 20, modified: 5, ..Default::default() };
        assert_eq!(s.in_use(), 70);
        assert_eq!(s.cached(), 25);
        assert_eq!(MemoryStatus::default().in_use(), 0);
    }
}

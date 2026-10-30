//! 物理磁盘拓扑与按实例性能计数器。卷容量不会当作物理磁盘容量。
use std::collections::BTreeMap;
use windows::Win32::System::Performance::*;
use windows::core::PCWSTR;

#[derive(Debug, Clone, Default)]
pub struct DiskVolume {
    pub mount: String,
    pub filesystem: String,
    pub capacity: u64,
    pub free: u64,
}
/// 物理磁盘的介质类型；显示名称由界面层决定。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MediaType {
    /// 没有读到（例如权限不足）。
    #[default]
    Unknown,
    /// 固态硬盘（无寻道惩罚）。
    Ssd,
    /// 机械硬盘。
    Hdd,
}

#[derive(Debug, Clone, Default)]
pub struct PhysicalDisk {
    pub number: u32,
    pub model: String,
    pub media_type: MediaType,
    pub capacity: u64,
    pub volumes: Vec<DiskVolume>,
    pub system: bool,
    pub active: Option<f32>,
    pub response_ms: Option<f64>,
    pub read: Option<f64>,
    pub write: Option<f64>,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const u8,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn CloseHandle(handle: isize) -> i32;
    fn DeviceIoControl(
        handle: isize,
        code: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_len: u32,
        returned: *mut u32,
        overlapped: *mut u8,
    ) -> i32;
    fn FindFirstVolumeW(name:*mut u16,len:u32)->isize;
    fn FindNextVolumeW(handle:isize,name:*mut u16,len:u32)->i32;
    fn FindVolumeClose(handle:isize)->i32;
    fn GetVolumePathNamesForVolumeNameW(name:*const u16,paths:*mut u16,len:u32,required:*mut u32)->i32;
    fn GetDiskFreeSpaceExW(
        path: *const u16,
        available: *mut u64,
        total: *mut u64,
        free: *mut u64,
    ) -> i32;
    fn GetVolumeInformationW(
        path: *const u16,
        name: *mut u16,
        name_len: u32,
        serial: *mut u32,
        max: *mut u32,
        flags: *mut u32,
        fs: *mut u16,
        fs_len: u32,
    ) -> i32;
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
struct Device(isize);
impl Device {
    fn open(path: &str) -> Option<Self> {
        let h = unsafe { CreateFileW(wide(path).as_ptr(), 0, 7, std::ptr::null(), 3, 0, 0) };
        (h != -1).then_some(Self(h))
    }
    fn query(&self, code: u32, input: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; 65536];
        let mut n = 0;
        (unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.as_ptr(),
                input.len() as u32,
                out.as_mut_ptr(),
                out.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            )
        } != 0)
            .then(|| {
                out.truncate(n as usize);
                out
            })
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn string_at(b: &[u8], at: usize) -> String {
    b.get(at..)
        .map(|s| {
            String::from_utf8_lossy(s.split(|c| *c == 0).next().unwrap_or_default())
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

fn metadata(number: u32) -> PhysicalDisk {
    let mut disk = PhysicalDisk {
        number,
        media_type: MediaType::Unknown,
        ..Default::default()
    };
    if let Some(d) = Device::open(&format!(r"\\.\PhysicalDrive{number}")) {
        // GET_DRIVE_GEOMETRY_EX 支持零访问权打开，普通用户也能读取磁盘长度。
        if let Some(b) = d.query(0x700a0, &[]) {
            disk.capacity = u64_at(&b, 24).unwrap_or(0);
        }
        if let Some(b) = d.query(0x2d1400, &[0; 12]) {
            disk.model = string_at(&b, u32_at(&b, 16).unwrap_or(0) as usize);
        }
        let mut q = [0u8; 12];
        q[0] = 7; // StorageDeviceSeekPenaltyProperty
        if let Some(b) = d.query(0x2d1400, &q) {
            if b.len() >= 9 {
                disk.media_type = if b[8] == 0 { MediaType::Ssd } else { MediaType::Hdd };
            }
        }
    }
    disk
}
fn volumes(disks: &mut BTreeMap<u32, PhysicalDisk>) {
    let system = std::env::var("SystemDrive").unwrap_or_default();
    let mut name=[0u16;1024];
    let handle=unsafe{FindFirstVolumeW(name.as_mut_ptr(),name.len() as u32)};
    if handle == -1{return;}
    let mut roots=Vec::new();
    loop {
        let len=name.iter().position(|x|*x==0).unwrap_or(name.len());
        let root=String::from_utf16_lossy(&name[..len]);
        let mut paths=vec![0u16;32768];let mut required=0;
        unsafe{GetVolumePathNamesForVolumeNameW(name.as_ptr(),paths.as_mut_ptr(),paths.len() as u32,&mut required);}
        let mount=paths.split(|c|*c==0).filter(|p|!p.is_empty()).map(String::from_utf16_lossy).collect::<Vec<_>>().join(" · ");
        if !mount.is_empty(){roots.push((root,mount));}
        if unsafe{FindNextVolumeW(handle,name.as_mut_ptr(),name.len() as u32)}==0{break;}
    }
    unsafe{FindVolumeClose(handle);}
    for (root_path,mount) in roots {
        let root=wide(&root_path);
        let Some(d) = Device::open(root_path.trim_end_matches('\\')) else {
            continue;
        };
        let Some(b) = d.query(0x560000, &[]) else {
            continue;
        };
        let count = u32_at(&b, 0).unwrap_or(0) as usize;
        let (mut total, mut free, mut available) = (0, 0, 0);
        let mut fs = [0u16; 64];
        unsafe {
            GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free);
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                64,
            );
        }
        for e in 0..count {
            let Some(number) = u32_at(&b, 8 + 24 * e) else {
                break;
            };
            let disk = disks.entry(number).or_insert_with(|| metadata(number));
            disk.system |= mount.split(" · ").any(|m|m.trim_end_matches('\\').eq_ignore_ascii_case(&system));
            if disk.volumes.iter().any(|v| v.mount == mount) {
                continue;
            }
            disk.volumes.push(DiskVolume {
                mount: mount.clone(),
                filesystem: String::from_utf16_lossy(
                    &fs[..fs.iter().position(|c| *c == 0).unwrap_or(fs.len())],
                ),
                capacity: total,
                free,
            });
        }
    }
}

pub(crate) struct PhysicalCounters {
    query: PDH_HQUERY,
    counters: [PDH_HCOUNTER; 4],
    ready: bool,
    topology: BTreeMap<u32, PhysicalDisk>,
    refresh: Option<std::time::Instant>,
    previous: std::collections::BTreeSet<u32>,
}
unsafe impl Send for PhysicalCounters {}
impl PhysicalCounters {
    pub fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut this = Self {
                query,
                counters: [PDH_HCOUNTER::default(); 4],
                ready: false,
                topology: BTreeMap::new(),
                refresh: None,
                previous: Default::default(),
            };
            for (c, name) in this.counters.iter_mut().zip([
                "% Idle Time",
                "Avg. Disk sec/Transfer",
                "Disk Read Bytes/sec",
                "Disk Write Bytes/sec",
            ]) {
                if PdhAddEnglishCounterW(
                    query,
                    PCWSTR(wide(&format!(r"\PhysicalDisk(*)\{name}")).as_ptr()),
                    0,
                    c,
                ) != 0
                {
                    return None;
                }
            }
            PdhCollectQueryData(query);
            Some(this)
        }
    }
    pub fn sample(&mut self) -> Vec<PhysicalDisk> {
        unsafe {
            PdhCollectQueryData(self.query);
            let mut disks = BTreeMap::new();
            let refresh = self.refresh.is_none_or(|t| t.elapsed().as_secs() >= 5);
            let mut discovered=false;
            if refresh {
                self.topology.clear();
            }
            for (index, c) in self.counters.iter().enumerate() {
                let (mut bytes, mut count) = (0, 0);
                PdhGetFormattedCounterArrayW(*c, PDH_FMT_DOUBLE, &mut bytes, &mut count, None);
                if bytes == 0 {
                    continue;
                }
                // u64 backing guarantees alignment for PDH structures.
                let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
                if PdhGetFormattedCounterArrayW(
                    *c,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    Some(buffer.as_mut_ptr().cast()),
                ) != 0
                {
                    continue;
                }
                for item in std::slice::from_raw_parts(
                    buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                    count as usize,
                ) {
                    let Ok(name) = item.szName.to_string() else {
                        continue;
                    };
                    let Some(number) = name
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    discovered |= !self.topology.contains_key(&number);
                    let disk = disks.entry(number).or_insert_with(|| {
                        let mut d = self.topology
                            .entry(number)
                            .or_insert_with(|| metadata(number))
                            .clone();
                        d.active=None; d.response_ms=None; d.read=None; d.write=None;
                        d
                    });
                    let v = item.FmtValue.Anonymous.doubleValue;
                    if !self.ready || !self.previous.contains(&number) || item.FmtValue.CStatus > 1 || !v.is_finite() {
                        continue;
                    }
                    match index {
                        0 => disk.active = Some((100.0 - v).clamp(0.0, 100.0) as f32),
                        1 => disk.response_ms = Some(v.max(0.0) * 1000.0),
                        2 => disk.read = Some(v.max(0.0)),
                        _ => disk.write = Some(v.max(0.0)),
                    }
                }
            }
            if refresh || discovered {
                for disk in disks.values_mut(){disk.volumes.clear();disk.system=false;}
                volumes(&mut disks);
                self.topology = disks.clone();
                self.refresh = Some(std::time::Instant::now());
            }
            self.ready = true;
            self.previous=disks.keys().copied().collect();
            disks.into_values().collect()
        }
    }
}
impl Drop for PhysicalCounters {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_storage_buffers_are_safe() {
        assert_eq!(u32_at(&[0; 3], 0), None);
        assert_eq!(u64_at(&[0; 7], 0), None);
        assert_eq!(string_at(b"model\0trailing", 0), "model");
        assert_eq!(string_at(b"abc", 99), "");
    }
}

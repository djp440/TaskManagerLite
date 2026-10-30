//! “详细信息”列：进程列表在名称、PID、CPU、内存、磁盘之外可选显示的属性。
//!
//! 两类来源：
//! - 计数（[`Counters`]）：随每次采样从 `SYSTEM_PROCESS_INFORMATION` 直接得到，几乎没有额外开销。
//! - 静态信息（[`StaticInfo`]）：用户名、路径、命令行、架构、是否提权、文件描述与公司，
//!   需要打开进程或读取文件。进程存续期间不变，由 [`StaticReader`] 在后台按需读取一次并缓存，不进采样热路径。

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, SYSTEMTIME};
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, LookupAccountSidW, PSID, SID_NAME_USE, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
    TokenElevation, TokenUser,
};
use windows::Win32::Storage::FileSystem::{FILE_VER_GET_LOCALISED, GetFileVersionInfoExW, GetFileVersionInfoSizeExW, VerQueryValueW};
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM, IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_ARMNT,
    IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
};
use windows::Win32::System::Threading::{
    GetProcessInformation, IsWow64Process2, OpenProcessToken, PROCESS_MACHINE_INFORMATION, ProcessMachineTypeInfo,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::core::{PCWSTR, PWSTR};

use crate::procinfo::{ProcessHandle, query_command_line, query_image_path};
use crate::snapshot::{Process, ProcessKey};

/// 可选显示的列，顺序即列选择器中的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetailColumn {
    Status,
    User,
    Session,
    Priority,
    Handles,
    Threads,
    WorkingSet,
    PeakWorkingSet,
    Commit,
    CpuTime,
    StartTime,
    Path,
    CommandLine,
    Arch,
    Elevated,
    Description,
    Company,
    /// 进程在所有 GPU 引擎上的最大利用率；只有显示这一列时才做进程级聚合。
    Gpu,
    /// 进程是否处于效率模式（EcoQoS，#68）；只有显示这一列时才按行读取，系统不支持时不提供这一列。
    EcoMode,
}

impl DetailColumn {
    pub const ALL: [Self; 19] = [
        Self::Status,
        Self::User,
        Self::Session,
        Self::Priority,
        Self::Handles,
        Self::Threads,
        Self::WorkingSet,
        Self::PeakWorkingSet,
        Self::Commit,
        Self::CpuTime,
        Self::StartTime,
        Self::Path,
        Self::CommandLine,
        Self::Arch,
        Self::Elevated,
        Self::Description,
        Self::Company,
        Self::Gpu,
        Self::EcoMode,
    ];

    /// 设置文件里使用的名字。
    pub fn id(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::User => "user",
            Self::Session => "session",
            Self::Priority => "priority",
            Self::Handles => "handles",
            Self::Threads => "threads",
            Self::WorkingSet => "working_set",
            Self::PeakWorkingSet => "peak_working_set",
            Self::Commit => "commit",
            Self::CpuTime => "cpu_time",
            Self::StartTime => "start_time",
            Self::Path => "path",
            Self::CommandLine => "command_line",
            Self::Arch => "arch",
            Self::Elevated => "elevated",
            Self::Description => "description",
            Self::Company => "company",
            Self::Gpu => "gpu",
            Self::EcoMode => "eco_mode",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// 该列的值是否来自需要后台读取的 [`StaticInfo`]。
    pub fn needs_static(self) -> bool {
        matches!(
            self,
            Self::User | Self::Path | Self::CommandLine | Self::Arch | Self::Elevated | Self::Description | Self::Company
        )
    }
}

/// 每次采样从 `SYSTEM_PROCESS_INFORMATION` 直接得到的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub session_id: u32,
    pub base_priority: i32,
    pub handles: u32,
    pub threads: u32,
    pub working_set: u64,
    pub peak_working_set: u64,
    /// 提交大小（私有页面），字节。
    pub commit: u64,
    /// 用户态 + 内核态累计 CPU 时间，100ns 单位。
    pub cpu_time: u64,
    pub suspended: bool,
}

/// 进程的映像架构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Arch {
    X86,
    X64,
    Arm,
    Arm64,
    Other(u16),
}

impl Arch {
    fn from_machine(m: IMAGE_FILE_MACHINE) -> Option<Self> {
        Some(match m {
            IMAGE_FILE_MACHINE_UNKNOWN => return None,
            IMAGE_FILE_MACHINE_I386 => Self::X86,
            IMAGE_FILE_MACHINE_AMD64 => Self::X64,
            IMAGE_FILE_MACHINE_ARM64 => Self::Arm64,
            IMAGE_FILE_MACHINE_ARM | IMAGE_FILE_MACHINE_ARMNT => Self::Arm,
            other => Self::Other(other.0),
        })
    }

    pub fn label(self) -> String {
        match self {
            Self::X86 => "x86".into(),
            Self::X64 => "x64".into(),
            Self::Arm => "ARM".into(),
            Self::Arm64 => "ARM64".into(),
            Self::Other(m) => format!("0x{m:04X}"),
        }
    }
}

/// 进程存续期间不变的属性。读不到的项为 `None`（受保护进程、其他用户的进程等）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaticInfo {
    pub user: Option<String>,
    pub path: Option<PathBuf>,
    pub command_line: Option<String>,
    pub arch: Option<Arch>,
    pub elevated: Option<bool>,
    pub description: Option<String>,
    pub company: Option<String>,
}

/// 版本信息缓存的路径数上限。
const VERSION_CACHE_CAP: usize = 2048;

/// 后台读取 [`StaticInfo`]，并缓存跨进程共用的结果（账户名按 SID、版本信息按文件路径）。
#[derive(Default)]
pub struct StaticReader {
    users: HashMap<Vec<u8>, Option<String>>,
    versions: HashMap<PathBuf, (Option<String>, Option<String>)>,
}

impl StaticReader {
    /// 读取一个进程的静态信息。进程已退出或 PID 已被复用时返回 `None`；
    /// 权限不足时返回各项为 `None` 的结果（不必再重试）。
    pub fn read(&mut self, key: ProcessKey) -> Option<StaticInfo> {
        let Some(h) = ProcessHandle::open(key.pid) else {
            // 打不开：受保护进程或已退出，无从区分，按读不到处理。
            return Some(StaticInfo::default());
        };
        if h.create_time()? != key.create_time {
            return None;
        }
        let path = query_image_path(&h);
        let (description, company) = match &path {
            Some(p) => {
                // 按路径缓存，数量封顶：长期运行时不同路径的 exe 不会让缓存无限增长，清空后按需重读即可。
                if self.versions.len() >= VERSION_CACHE_CAP && !self.versions.contains_key(p) {
                    self.versions.clear();
                }
                self.versions.entry(p.clone()).or_insert_with(|| version_strings(p)).clone()
            }
            None => (None, None),
        };
        let token = Token::open(&h);
        let user = token.as_ref().and_then(|t| {
            let sid = t.user_sid()?;
            Some(self.users.entry(sid.clone()).or_insert_with(|| account_name(&sid)).clone())
        });
        Some(StaticInfo {
            user: user.flatten(),
            command_line: query_command_line(&h),
            arch: arch_of(&h),
            elevated: token.as_ref().and_then(Token::elevated),
            description,
            company,
            path,
        })
    }
}

struct Token(HANDLE);

impl Token {
    fn open(h: &ProcessHandle) -> Option<Self> {
        let mut t = HANDLE::default();
        unsafe { OpenProcessToken(h.0, TOKEN_QUERY, &mut t) }.ok()?;
        Some(Self(t))
    }

    /// 令牌用户的 SID（原始字节）。
    fn user_sid(&self) -> Option<Vec<u8>> {
        let mut len = 0u32;
        let _ = unsafe { GetTokenInformation(self.0, TokenUser, None, 0, &mut len) };
        if len == 0 {
            return None;
        }
        // u64 缓冲保证对齐满足 TOKEN_USER。
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        unsafe { GetTokenInformation(self.0, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) }.ok()?;
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let n = unsafe { GetLengthSid(sid) } as usize;
        Some(unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), n) }.to_vec())
    }

    fn elevated(&self) -> Option<bool> {
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        unsafe {
            GetTokenInformation(
                self.0,
                TokenElevation,
                Some((&raw mut e).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        }
        .ok()?;
        Some(e.TokenIsElevated != 0)
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// SID 对应的账户名（不含域名，与原生任务管理器一致）。
fn account_name(sid: &[u8]) -> Option<String> {
    let mut sid = sid.to_vec();
    let psid = PSID(sid.as_mut_ptr().cast());
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut usage = SID_NAME_USE::default();
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            psid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut usage,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&name[..name_len as usize]))
}

fn arch_of(h: &ProcessHandle) -> Option<Arch> {
    // Windows 11 起可直接取进程的机器类型（x64 仿真在 ARM64 上也能区分）。
    let mut info = PROCESS_MACHINE_INFORMATION::default();
    if unsafe {
        GetProcessInformation(
            h.0,
            ProcessMachineTypeInfo,
            (&raw mut info).cast(),
            size_of::<PROCESS_MACHINE_INFORMATION>() as u32,
        )
    }
    .is_ok()
        && let Some(a) = Arch::from_machine(info.ProcessMachine)
    {
        return Some(a);
    }
    let (mut process, mut native) = (IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_UNKNOWN);
    unsafe { IsWow64Process2(h.0, &mut process, Some(&mut native)) }.ok()?;
    // 非 WOW64 进程返回 UNKNOWN，此时与本机架构相同。
    Arch::from_machine(process).or_else(|| Arch::from_machine(native))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 文件版本信息里的（文件说明, 公司名称），优先取系统界面语言的本地化资源。
pub(crate) fn version_strings(path: &Path) -> (Option<String>, Option<String>) {
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let file = PCWSTR(file.as_ptr());
    let size = unsafe { GetFileVersionInfoSizeExW(FILE_VER_GET_LOCALISED, file, &mut 0) };
    if size == 0 {
        return (None, None);
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoExW(FILE_VER_GET_LOCALISED, file, None, size, data.as_mut_ptr().cast()) }.is_err() {
        return (None, None);
    }
    let query = |sub: &str| -> Option<&[u8]> {
        let sub = wide(sub);
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe { VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub.as_ptr()), &mut ptr, &mut len) }.as_bool().then_some(())?;
        if ptr.is_null() {
            return None;
        }
        // 字符串值的 len 是字符数，翻译表的 len 是字节数；这里统一按字节截取，字符串另行处理。
        Some(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len as usize) })
    };
    // 先用文件声明的第一种语言/代码页，再退到常见的美式英语组合。
    let mut langs: Vec<String> = query("\\VarFileInfo\\Translation")
        .map(|t| t.chunks_exact(4).map(|c| format!("{:04x}{:04x}", u16::from_le_bytes([c[0], c[1]]), u16::from_le_bytes([c[2], c[3]]))).collect())
        .unwrap_or_default();
    langs.extend(["040904b0".to_string(), "040904e4".to_string(), "04090000".to_string()]);
    let string = |name: &str| -> Option<String> {
        langs.iter().find_map(|lang| {
            let sub = wide(&format!("\\StringFileInfo\\{lang}\\{name}"));
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            unsafe { VerQueryValueW(data.as_ptr().cast(), PCWSTR(sub.as_ptr()), &mut ptr, &mut len) }.as_bool().then_some(())?;
            if ptr.is_null() || len == 0 {
                return None;
            }
            let units = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len as usize) };
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            let text = OsString::from_wide(&units[..end]).to_string_lossy().trim().to_string();
            (!text.is_empty()).then_some(text)
        })
    };
    (string("FileDescription"), string("CompanyName"))
}

/// FILETIME（UTC，100ns）转为本地时间 `yyyy-MM-dd HH:mm:ss`。
pub fn local_time_text(filetime: u64) -> String {
    let ft = FILETIME { dwLowDateTime: filetime as u32, dwHighDateTime: (filetime >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    if filetime == 0
        || unsafe { FileTimeToSystemTime(&ft, &mut utc) }.is_err()
        || unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.is_err()
    {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    )
}

/// 按某个详细信息列比较两个进程。静态信息尚未读到或读不到的进程排在最小一端。
pub(crate) fn compare(a: &Process, b: &Process, col: DetailColumn) -> Ordering {
    let (x, y) = (&a.counters, &b.counters);
    let text = |f: fn(&StaticInfo) -> Option<&str>| {
        let get = |p: &Process| p.info.as_deref().and_then(f).map(str::to_lowercase);
        get(a).cmp(&get(b))
    };
    let field = |f: fn(&StaticInfo) -> Option<u8>| {
        let get = |p: &Process| p.info.as_deref().and_then(f);
        get(a).cmp(&get(b))
    };
    match col {
        DetailColumn::Status => x.suspended.cmp(&y.suspended),
        DetailColumn::Session => x.session_id.cmp(&y.session_id),
        DetailColumn::Priority => x.base_priority.cmp(&y.base_priority),
        DetailColumn::Handles => x.handles.cmp(&y.handles),
        DetailColumn::Threads => x.threads.cmp(&y.threads),
        DetailColumn::WorkingSet => x.working_set.cmp(&y.working_set),
        DetailColumn::PeakWorkingSet => x.peak_working_set.cmp(&y.peak_working_set),
        DetailColumn::Commit => x.commit.cmp(&y.commit),
        DetailColumn::CpuTime => x.cpu_time.cmp(&y.cpu_time),
        DetailColumn::StartTime => a.key.create_time.cmp(&b.key.create_time),
        DetailColumn::User => text(|i| i.user.as_deref()),
        DetailColumn::Path => {
            let get = |p: &Process| p.info.as_ref().and_then(|i| i.path.as_ref()).map(|p| p.to_string_lossy().to_lowercase());
            get(a).cmp(&get(b))
        }
        DetailColumn::CommandLine => text(|i| i.command_line.as_deref()),
        DetailColumn::Description => text(|i| i.description.as_deref()),
        DetailColumn::Company => text(|i| i.company.as_deref()),
        DetailColumn::Arch => {
            let get = |p: &Process| p.info.as_ref().and_then(|i| i.arch);
            get(a).cmp(&get(b))
        }
        DetailColumn::Elevated => field(|i| i.elevated.map(u8::from)),
        // 没有读数（None）排在最小一端，0% 比没有读数大。
        DetailColumn::Gpu => match (a.gpu_usage, b.gpu_usage) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (x, y) => x.is_some().cmp(&y.is_some()),
        },
        // 没有读数（None）< 未启用 < 已启用。
        DetailColumn::EcoMode => a.eco_mode.cmp(&b.eco_mode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_key() -> ProcessKey {
        let h = ProcessHandle::open(std::process::id()).unwrap();
        ProcessKey { pid: std::process::id(), create_time: h.create_time().unwrap() }
    }

    #[test]
    fn ids_round_trip_and_are_unique() {
        for c in DetailColumn::ALL {
            assert_eq!(DetailColumn::from_id(c.id()), Some(c));
        }
        let mut ids: Vec<_> = DetailColumn::ALL.iter().map(|c| c.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), DetailColumn::ALL.len());
        assert_eq!(DetailColumn::from_id("nope"), None);
    }

    #[test]
    fn reads_own_static_info() {
        let mut r = StaticReader::default();
        let info = r.read(own_key()).expect("本进程存活");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(info.path.as_ref().and_then(|p| p.file_name()), exe.file_name());
        assert!(info.command_line.is_some());
        assert!(info.user.as_deref().is_some_and(|u| !u.is_empty() && !u.contains('\\')), "{:?}", info.user);
        assert_eq!(info.elevated, Some(crate::is_elevated()));
        #[cfg(target_arch = "x86_64")]
        assert_eq!(info.arch, Some(Arch::X64));
    }

    #[test]
    fn pid_reuse_is_detected() {
        let key = ProcessKey { create_time: 1, ..own_key() };
        assert_eq!(StaticReader::default().read(key), None);
    }

    #[test]
    fn system_dll_has_localized_version_strings() {
        let dir = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        let (desc, company) = version_strings(&dir.join("explorer.exe"));
        assert!(desc.is_some_and(|d| !d.is_empty()));
        assert!(company.is_some_and(|c| c.contains("Microsoft")));
        assert_eq!(version_strings(Path::new("C:\\nope\\missing.exe")), (None, None));
    }

    #[test]
    fn local_time_formats_and_rejects_zero() {
        assert_eq!(local_time_text(0), "");
        // 2020-01-01 00:00:00 UTC；只检查格式，本地时区不同结果不同。
        let t = local_time_text(132_223_104_000_000_000);
        assert_eq!(t.len(), 19, "{t}");
        assert!(t.starts_with("2019-12-31") || t.starts_with("2020-01-01"), "{t}");
    }
}

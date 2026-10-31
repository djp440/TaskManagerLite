//! Windows 服务。仅由服务页按需调用：一次枚举拿到状态与 PID，启动类型、描述、组按服务名缓存。
use crate::error::{ElevationError, SystemError};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
            ERROR_SERVICE_NOT_ACTIVE, ERROR_SERVICE_REQUEST_TIMEOUT,
        },
        System::Services::*,
    },
    core::{HSTRING, PCWSTR, PWSTR},
};

/// 服务的运行状态，取值与 `SERVICE_STATUS::dwCurrentState` 对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Unknown,
}

impl ServiceState {
    fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::Stopped,
            2 => Self::StartPending,
            3 => Self::StopPending,
            4 => Self::Running,
            5 => Self::ContinuePending,
            6 => Self::PausePending,
            7 => Self::Paused,
            _ => Self::Unknown,
        }
    }
}

/// 启动类型，取值与 `QUERY_SERVICE_CONFIGW::dwStartType` 对应；自动启动再区分是否延迟。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum StartType {
    Auto,
    AutoDelayed,
    Manual,
    Disabled,
    Boot,
    System,
    /// 没有读到配置（例如权限不足）。
    #[default]
    Unknown,
}

impl StartType {
    fn from_raw(v: u32, delayed: bool) -> Self {
        match v {
            0 => Self::Boot,
            1 => Self::System,
            2 if delayed => Self::AutoDelayed,
            2 => Self::Auto,
            3 => Self::Manual,
            4 => Self::Disabled,
            _ => Self::Unknown,
        }
    }

    /// 服务页可以设置的四种启动类型（引导、系统只属于驱动，未知不可设置）。
    pub const SETTABLE: [StartType; 4] = [Self::Auto, Self::AutoDelayed, Self::Manual, Self::Disabled];

    /// 设置时写入 `dwStartType` 的值与延迟启动标志；不可设置的类型为 `None`。
    pub fn to_raw(self) -> Option<(u32, bool)> {
        match self {
            Self::Auto => Some((2, false)),
            Self::AutoDelayed => Some((2, true)),
            Self::Manual => Some((3, false)),
            Self::Disabled => Some((4, false)),
            Self::Boot | Self::System | Self::Unknown => None,
        }
    }

    /// 命令行与测试钩子里的写法：`auto`、`auto-delayed`、`manual`、`disabled`。
    pub fn key(self) -> Option<&'static str> {
        match self {
            Self::Auto => Some("auto"),
            Self::AutoDelayed => Some("auto-delayed"),
            Self::Manual => Some("manual"),
            Self::Disabled => Some("disabled"),
            Self::Boot | Self::System | Self::Unknown => None,
        }
    }

    pub fn parse_key(s: &str) -> Option<Self> {
        Self::SETTABLE.into_iter().find(|t| t.key() == Some(s))
    }
}

/// 不能修改启动类型的原因（可在修改前预知，菜单据此置灰）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartTypeBlock {
    /// 驱动服务（服务页不列出驱动，直接按名称操作时仍会拒绝）。
    Driver,
    /// 受保护的服务（`SERVICE_CONFIG_LAUNCH_PROTECTED` 不为 NONE），管理员也无法修改配置。
    Protected,
}

/// 由服务类型（`dwServiceType`）与启动保护级别（`dwLaunchProtected`）判断能否修改启动类型。
pub fn start_type_block(service_type: u32, launch_protected: u32) -> Option<StartTypeBlock> {
    if service_type & SERVICE_DRIVER.0 != 0 {
        Some(StartTypeBlock::Driver)
    } else if launch_protected != SERVICE_LAUNCH_PROTECTED_NONE {
        Some(StartTypeBlock::Protected)
    } else {
        None
    }
}

/// 按需读取并缓存的配置：启动类型、描述与组（svchost 的 `-k` 参数，与原生任务管理器的“组”列一致）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub start_type: StartType,
    pub description: String,
    pub group: String,
}

#[derive(Clone, Debug, Default)]
pub struct Service {
    pub name: String,
    pub display_name: String,
    pub state: ServiceState,
    /// 所在进程的 PID；未运行时为 0。
    pub pid: u32,
    /// 服务当前接受停止请求（`SERVICE_ACCEPT_STOP`）。
    pub accepts_stop: bool,
    pub config: Config,
}

impl Default for ServiceState {
    fn default() -> Self {
        Self::Unknown
    }
}

impl Service {
    pub fn can_start(&self) -> bool {
        self.state == ServiceState::Stopped
    }
    pub fn can_stop(&self) -> bool {
        matches!(self.state, ServiceState::Running | ServiceState::Paused) && self.accepts_stop
    }

    /// 搜索：`needle` 已去空白并转小写；为空时匹配全部，否则服务名或显示名称包含它（不区分大小写）。
    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.name.to_lowercase().contains(needle)
            || self.display_name.to_lowercase().contains(needle)
    }
}

/// 启动、停止、重启、修改启动类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    /// 修改启动类型，只接受 [`StartType::SETTABLE`] 中的值。
    SetStartType(StartType),
}

impl Action {
    /// 解析提权子进程命令行里的动作：`start`、`stop`、`restart`、`start-type=<auto|auto-delayed|manual|disabled>`。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => s.strip_prefix("start-type=").and_then(StartType::parse_key).map(Self::SetStartType),
        }
    }
    fn arg(self) -> String {
        match self {
            Self::Start => "start".into(),
            Self::Stop => "stop".into(),
            Self::Restart => "restart".into(),
            Self::SetStartType(t) => format!("start-type={}", t.key().unwrap_or("invalid")),
        }
    }
}

/// 提权子进程的参数（不含 exe）：`--service-control <动作> <十六进制服务名>`。
fn elevated_args(name: &str, action: Action) -> String {
    format!("--service-control {} {}", action.arg(), crate::startup::hex(name))
}

/// 直接执行失败后是否改走 UAC 提权子进程：仅在允许提权、当前未提权、且失败原因是“拒绝访问”时。
fn should_elevate(e: &ServiceError, elevate: bool, elevated: bool) -> bool {
    elevate && !elevated && matches!(e, ServiceError::System(s) if s.win32_code() == Some(ERROR_ACCESS_DENIED.0 as i32))
}

struct Handle(SC_HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

/// 服务操作失败的原因。界面层按枚举映射成显示文案；系统错误保留错误码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// 系统调用失败（拒绝访问、服务不存在、请求超时等，见错误码）。
    System(SystemError),
    /// 经 UAC 提权子进程执行时失败。
    Elevation(ElevationError),
    /// 读到的服务配置为空。
    EmptyConfig,
    /// 服务启动后随即停止。
    StoppedAfterStart,
    /// 停止请求发出后服务没有停止。
    NotStopped,
    /// 服务名为空或含非法字符。
    InvalidName,
    /// 提权子进程命令行：参数数量错误。
    BadArgumentCount,
    /// 提权子进程命令行：动作错误。
    BadAction,
    /// 提权子进程命令行：服务名编码错误。
    BadNameEncoding,
    /// 不能修改该服务的启动类型（驱动或受保护的服务）。
    StartTypeBlocked(StartTypeBlock),
    /// 目标启动类型不可设置（引导、系统、未知）。
    BadStartType,
}

impl ServiceError {
    /// 提权子进程用作退出码：Win32 错误码，否则 1。
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::System(e) => e.exit_code(),
            _ => 1,
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System(e) => write!(f, "system call failed: {e}"),
            Self::Elevation(e) => write!(f, "{e}"),
            Self::EmptyConfig => f.write_str("empty service config"),
            Self::StoppedAfterStart => f.write_str("service stopped right after start"),
            Self::NotStopped => f.write_str("service did not stop"),
            Self::InvalidName => f.write_str("invalid service name"),
            Self::BadArgumentCount => f.write_str("bad argument count"),
            Self::BadAction => f.write_str("bad action"),
            Self::BadNameEncoding => f.write_str("bad name encoding"),
            Self::StartTypeBlocked(b) => write!(f, "start type cannot be changed: {b:?}"),
            Self::BadStartType => f.write_str("start type cannot be set"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<SystemError> for ServiceError {
    fn from(e: SystemError) -> Self {
        Self::System(e)
    }
}

impl From<ElevationError> for ServiceError {
    fn from(e: ElevationError) -> Self {
        Self::Elevation(e)
    }
}

type Result<T> = std::result::Result<T, ServiceError>;

fn win(e: windows::core::Error) -> ServiceError {
    ServiceError::System(SystemError::Win32(e.code().0 & 0xFFFF))
}

fn manager(access: u32) -> Result<Handle> {
    unsafe { OpenSCManagerW(None, None, access) }
        .map(Handle)
        .map_err(win)
}

fn open(scm: &Handle, name: &str, access: u32) -> Result<Handle> {
    unsafe { OpenServiceW(scm.0, &HSTRING::from(name), access) }
        .map(Handle)
        .map_err(win)
}

fn text(p: PWSTR) -> String {
    if p.is_null() {
        String::new()
    } else {
        String::from_utf16_lossy(unsafe { p.as_wide() })
    }
}

/// 把 `EnumServicesStatusExW` 返回的数组转换成服务列表（配置留空，由缓存补上）。
fn parse(items: &[ENUM_SERVICE_STATUS_PROCESSW]) -> Vec<Service> {
    items
        .iter()
        .map(|e| {
            let s = &e.ServiceStatusProcess;
            Service {
                name: text(e.lpServiceName),
                display_name: text(e.lpDisplayName),
                state: ServiceState::from_raw(s.dwCurrentState.0),
                pid: s.dwProcessId,
                accepts_stop: s.dwControlsAccepted & SERVICE_ACCEPT_STOP != 0,
                config: Config::default(),
            }
        })
        .collect()
}

/// 枚举全部 Win32 服务（含每用户服务实例，与 `Get-Service` 口径一致），按名称排序。
fn enumerate() -> Result<Vec<Service>> {
    let scm = manager(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE)?;
    let mut services = vec![];
    let mut resume = 0u32;
    // 用 u64 保证数组元素按指针对齐。
    let mut buf = vec![0u64; 32 * 1024];
    loop {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len() * 8)
        };
        let (mut needed, mut count) = (0u32, 0u32);
        let r = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                Some(bytes),
                &mut needed,
                &mut count,
                Some(&mut resume),
                None,
            )
        };
        let items = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                count as usize,
            )
        };
        services.extend(parse(items));
        match r {
            Ok(()) => break,
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                if count == 0 {
                    buf.resize((needed as usize).div_ceil(8) + 1, 0);
                }
            }
            Err(e) => return Err(win(e)),
        }
    }
    services.sort_by_key(|s| s.name.to_lowercase());
    Ok(services)
}

/// 进程 PID → 它承载的（正在运行的）服务名，名称按不区分大小写排序。
pub type HostMap = HashMap<u32, Arc<[String]>>;

/// 由服务列表得出“PID → 承载的服务名”。未运行的服务（PID 为 0）不计入；`services` 已按名称排序时结果也有序。
pub fn host_map(services: &[Service]) -> HostMap {
    let mut grouped: HashMap<u32, Vec<String>> = HashMap::new();
    for s in services.iter().filter(|s| s.pid > 0) {
        grouped.entry(s.pid).or_default().push(s.name.clone());
    }
    grouped
        .into_iter()
        .map(|(pid, mut names)| {
            names.sort_by_key(|n| n.to_lowercase());
            (pid, Arc::from(names))
        })
        .collect()
}

/// 只做一次服务枚举（不读启动类型、描述、组），得到“PID → 承载的服务名”。
/// 进程页的“服务”列与右键菜单“转到服务”按需调用，开销是一次 `EnumServicesStatusExW`。
pub fn hosted_services() -> Result<HostMap> {
    Ok(host_map(&enumerate()?))
}

/// 把 `map` 填进进程的 `services` 字段；不承载服务的进程保持 `None`。
pub fn apply_hosts(processes: &mut [crate::Process], map: &HostMap) {
    for p in processes {
        p.services = map.get(&p.key.pid).cloned();
    }
}

/// svchost 宿主的服务在命令行里用 `-k <组>` 指定共享进程的组名；其他服务没有组。
fn svchost_group(command: &str) -> String {
    let mut words = command.split_whitespace();
    while let Some(w) = words.next() {
        if w.eq_ignore_ascii_case("-k") || w.eq_ignore_ascii_case("/k") {
            return words
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_owned();
        }
    }
    String::new()
}

/// 读一个配置项；缓冲区不足时按返回的大小重试一次。
fn query(read: impl Fn(Option<&mut [u8]>, &mut u32) -> windows::core::Result<()>) -> Result<Vec<u64>> {
    let mut needed = 0u32;
    match read(None, &mut needed) {
        Ok(()) => return Ok(vec![]),
        Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {}
        Err(e) => return Err(win(e)),
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let bytes =
        unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len() * 8) };
    read(Some(bytes), &mut needed).map_err(win)?;
    Ok(buf)
}

fn read_config(scm: &Handle, name: &str) -> Result<Config> {
    let svc = open(scm, name, SERVICE_QUERY_CONFIG)?;
    let buf = query(|b, n| unsafe {
        let len = b.as_ref().map_or(0, |b| b.len() as u32);
        QueryServiceConfigW(
            svc.0,
            b.map(|b| b.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()),
            len,
            n,
        )
    })?;
    if buf.is_empty() {
        return Err(ServiceError::EmptyConfig);
    }
    let cfg = unsafe { &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let config2 = |level| query(|b, n| unsafe { QueryServiceConfig2W(svc.0, level, b, n) });
    let delayed = cfg.dwStartType == SERVICE_AUTO_START
        && config2(SERVICE_CONFIG_DELAYED_AUTO_START_INFO).is_ok_and(|b| {
            !b.is_empty()
                && unsafe { (*b.as_ptr().cast::<SERVICE_DELAYED_AUTO_START_INFO>()).fDelayedAutostart.as_bool() }
        });
    let description = config2(SERVICE_CONFIG_DESCRIPTION)
        .ok()
        .filter(|b| !b.is_empty())
        .map(|b| text(unsafe { (*b.as_ptr().cast::<SERVICE_DESCRIPTIONW>()).lpDescription }))
        .unwrap_or_default();
    Ok(Config {
        start_type: StartType::from_raw(cfg.dwStartType.0, delayed),
        description,
        group: svchost_group(&text(cfg.lpBinaryPathName)),
    })
}

/// 服务列表与配置缓存。配置只在第一次见到某个服务时读取，之后的刷新只做一次枚举。
#[derive(Default)]
pub struct ServiceList {
    cache: HashMap<String, Config>,
}

impl ServiceList {
    pub fn new() -> Self {
        Self::default()
    }

    /// 清空配置缓存（重新进入服务页时调用，以同步外部修改的启动类型）。
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// 让某个服务的配置在下次枚举时重新读取（修改启动类型后调用）。
    pub fn forget(&mut self, name: &str) {
        self.cache.remove(&name.to_lowercase());
    }

    pub fn list(&mut self) -> Result<Vec<Service>> {
        let mut services = enumerate()?;
        let scm = manager(SC_MANAGER_CONNECT)?;
        self.fill(&mut services, |name| read_config(&scm, name).ok());
        Ok(services)
    }

    /// 用缓存补齐配置，缺少的才调用 `read`；读不到（如权限不足）也记入缓存，不反复重试。
    /// 已消失的服务清出缓存。
    fn fill(&mut self, services: &mut [Service], mut read: impl FnMut(&str) -> Option<Config>) {
        let mut seen = HashMap::with_capacity(services.len());
        for s in services.iter_mut() {
            let key = s.name.to_lowercase();
            let config = match self.cache.remove(&key) {
                Some(c) => c,
                None => read(&s.name).unwrap_or_default(),
            };
            s.config = config.clone();
            seen.insert(key, config);
        }
        self.cache = seen;
    }
}

fn status(svc: &Handle) -> Result<SERVICE_STATUS_PROCESS> {
    let mut s = SERVICE_STATUS_PROCESS::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut s as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    let mut needed = 0;
    unsafe { QueryServiceStatusEx(svc.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }
        .map_err(win)?;
    Ok(s)
}

/// 等服务离开挂起状态；超时按“服务没有及时响应”报错。
fn wait(svc: &Handle, pending: ServiceState, timeout: Duration) -> Result<ServiceState> {
    let end = Instant::now() + timeout;
    loop {
        let state = ServiceState::from_raw(status(svc)?.dwCurrentState.0);
        if state != pending {
            return Ok(state);
        }
        if Instant::now() >= end {
            return Err(ServiceError::System(SystemError::Win32(ERROR_SERVICE_REQUEST_TIMEOUT.0 as i32)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

const WAIT: Duration = Duration::from_secs(30);

fn start(svc: &Handle) -> Result<()> {
    unsafe { StartServiceW(svc.0, None) }.map_err(win)?;
    match wait(svc, ServiceState::StartPending, WAIT)? {
        ServiceState::Stopped => Err(ServiceError::StoppedAfterStart),
        _ => Ok(()),
    }
}

fn stop(svc: &Handle) -> Result<()> {
    let mut s = SERVICE_STATUS::default();
    match unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut s) } {
        Ok(()) => {}
        // 已经停止的服务视为停止成功（重启时直接启动）。
        Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(e) => return Err(win(e)),
    }
    match wait(svc, ServiceState::StopPending, WAIT)? {
        ServiceState::Stopped => Ok(()),
        _ => Err(ServiceError::NotStopped),
    }
}

/// 读服务类型与启动保护级别，判断能否修改启动类型（只需 `SERVICE_QUERY_CONFIG`，普通权限即可）。
fn read_block(svc: &Handle) -> Result<Option<StartTypeBlock>> {
    let buf = query(|b, n| unsafe {
        let len = b.as_ref().map_or(0, |b| b.len() as u32);
        QueryServiceConfigW(svc.0, b.map(|b| b.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>()), len, n)
    })?;
    if buf.is_empty() {
        return Err(ServiceError::EmptyConfig);
    }
    let service_type = unsafe { (*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()).dwServiceType.0 };
    // 早于 Windows 8.1 的系统没有这一级别，读不到按未保护处理。
    let protected = query(|b, n| unsafe { QueryServiceConfig2W(svc.0, SERVICE_CONFIG_LAUNCH_PROTECTED, b, n) })
        .ok()
        .filter(|b| !b.is_empty())
        .map_or(SERVICE_LAUNCH_PROTECTED_NONE, |b| unsafe {
            (*b.as_ptr().cast::<SERVICE_LAUNCH_PROTECTED_INFO>()).dwLaunchProtected
        });
    Ok(start_type_block(service_type, protected))
}

/// 右键菜单弹出前调用：能否修改该服务的启动类型。读不到配置时返回错误（菜单按可用处理，执行时再报告原因）。
pub fn start_type_block_of(name: &str) -> Result<Option<StartTypeBlock>> {
    let scm = manager(SC_MANAGER_CONNECT)?;
    read_block(&open(&scm, name, SERVICE_QUERY_CONFIG)?)
}

fn set_delayed(svc: &Handle, delayed: bool) -> Result<()> {
    let info = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: delayed.into() };
    unsafe {
        ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some((&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast()),
        )
    }
    .map_err(win)
}

/// 修改启动类型。延迟标志只对自动启动有意义：改成自动 / 自动（延迟）时先改类型再设标志；
/// 改成手动 / 禁用时先清掉标志（此时还是自动则必须成功，否则系统会忽略），再改类型，保证读回一致。
fn set_start_type(svc: &Handle, target: StartType) -> Result<()> {
    let (raw, delayed) = target.to_raw().ok_or(ServiceError::BadStartType)?;
    let change = || unsafe {
        ChangeServiceConfigW(
            svc.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(raw),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
        .map_err(win)
    };
    if raw == SERVICE_AUTO_START.0 {
        change()?;
        set_delayed(svc, delayed)
    } else {
        // 当前不是自动启动时系统可能拒绝设置延迟标志（不影响读回），忽略这一步的失败；
        // 拒绝访问则照常返回，交给提权路径。
        match set_delayed(svc, false) {
            Err(e) if should_elevate(&e, true, false) => return Err(e),
            _ => {}
        }
        change()
    }
}

fn control_direct(name: &str, action: Action) -> Result<()> {
    if name.is_empty() || name.contains(['\0', '/', '\\']) {
        return Err(ServiceError::InvalidName);
    }
    let scm = manager(SC_MANAGER_CONNECT)?;
    let access = match action {
        Action::Start => SERVICE_QUERY_STATUS | SERVICE_START,
        Action::Stop => SERVICE_QUERY_STATUS | SERVICE_STOP,
        Action::Restart => SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP,
        Action::SetStartType(_) => SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG,
    };
    // 修改启动类型：先用只读权限判断驱动 / 受保护服务，避免为注定失败的修改弹 UAC。
    if let Action::SetStartType(t) = action {
        t.to_raw().ok_or(ServiceError::BadStartType)?;
        if let Some(block) = start_type_block_of(name)? {
            return Err(ServiceError::StartTypeBlocked(block));
        }
    }
    let svc = open(&scm, name, access)?;
    match action {
        Action::Start => start(&svc),
        Action::Stop => stop(&svc),
        Action::Restart => stop(&svc).and_then(|()| start(&svc)),
        Action::SetStartType(t) => set_start_type(&svc, t),
    }
}

/// 执行操作（启停时等待服务到达目标状态）。未提权时先直接尝试，权限不足且 `elevate` 为真时
/// 通过固定参数的提权子进程执行（弹出 UAC）；`elevate` 为假时直接返回“拒绝访问”。
pub fn control(name: &str, action: Action, elevate: bool) -> Result<()> {
    match control_direct(name, action) {
        Err(e) if should_elevate(&e, elevate, crate::is_elevated()) => {
            crate::startup::run_elevated(&elevated_args(name, action)).map_err(ServiceError::Elevation)
        }
        r => r,
    }
}

/// 提权子进程入口：`--service-control <start|stop|restart|start-type=<类型>> <十六进制服务名>`。
pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("--service-control") {
        return None;
    }
    let result = (|| {
        if args.len() != 4 {
            return Err(ServiceError::BadArgumentCount);
        }
        let action = Action::parse(&args[2]).ok_or(ServiceError::BadAction)?;
        let name = crate::startup::unhex(&args[3]).ok_or(ServiceError::BadNameEncoding)?;
        control_direct(&name, action)
    })();
    Some(result.map_or_else(|e| e.exit_code(), |()| 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    #[test]
    fn parses_enumerated_entries() {
        let names = [wide("Spooler"), wide("Print Spooler"), wide("Dummy"), wide("示例服务")];
        let item = |n: usize, state: u32, pid: u32, accepted: u32| ENUM_SERVICE_STATUS_PROCESSW {
            lpServiceName: PWSTR(names[n].as_ptr().cast_mut()),
            lpDisplayName: PWSTR(names[n + 1].as_ptr().cast_mut()),
            ServiceStatusProcess: SERVICE_STATUS_PROCESS {
                dwCurrentState: SERVICE_STATUS_CURRENT_STATE(state),
                dwProcessId: pid,
                dwControlsAccepted: accepted,
                ..Default::default()
            },
        };
        let items = [item(0, 4, 1234, SERVICE_ACCEPT_STOP), item(2, 1, 0, 0)];
        let rows = parse(&items);
        assert_eq!(rows[0].name, "Spooler");
        assert_eq!(rows[0].display_name, "Print Spooler");
        assert_eq!((rows[0].state, rows[0].pid), (ServiceState::Running, 1234));
        assert!(rows[0].can_stop() && !rows[0].can_start());
        assert_eq!(rows[1].display_name, "示例服务");
        assert_eq!(rows[1].state, ServiceState::Stopped);
        assert!(rows[1].can_start() && !rows[1].can_stop());
        assert_eq!(rows[1].config.start_type, StartType::Unknown);
    }

    #[test]
    fn search_matches_name_or_display_name_case_insensitive() {
        let s = Service { name: "Spooler".into(), display_name: "Print Spooler 打印后台处理程序".into(), ..Default::default() };
        assert!(s.matches(""));
        assert!(s.matches("spool"));
        assert!(s.matches("print"));
        assert!(s.matches("打印"));
        assert!(!s.matches("dhcp"));
    }

    #[test]
    fn maps_states_and_start_types() {
        use ServiceState as S;
        let states: Vec<_> = (0..=8).map(S::from_raw).collect();
        assert_eq!(
            states,
            [S::Unknown, S::Stopped, S::StartPending, S::StopPending, S::Running, S::ContinuePending, S::PausePending, S::Paused, S::Unknown]
        );
        assert_eq!(StartType::from_raw(2, false), StartType::Auto);
        assert_eq!(StartType::from_raw(2, true), StartType::AutoDelayed);
        assert_eq!(StartType::from_raw(3, true), StartType::Manual);
        assert_eq!(StartType::from_raw(4, false), StartType::Disabled);
        assert_eq!(StartType::from_raw(1, false), StartType::System);
        assert_eq!(StartType::from_raw(0, false), StartType::Boot);
        assert_eq!(StartType::from_raw(9, false), StartType::Unknown);
        // 运行但不接受停止（如 RpcSs）时不能停止。
        let s = Service { state: ServiceState::Running, ..Default::default() };
        assert!(!s.can_stop());
        for (a, arg) in [(Action::Start, "start"), (Action::Stop, "stop"), (Action::Restart, "restart")] {
            assert_eq!(Action::parse(arg), Some(a));
            assert_eq!(a.arg(), arg);
        }
        assert_eq!(Action::parse("pause"), None);
    }

    #[test]
    fn settable_start_types_round_trip_through_raw_and_key() {
        use StartType as T;
        for t in T::SETTABLE {
            let (raw, delayed) = t.to_raw().unwrap();
            assert_eq!(T::from_raw(raw, delayed), t, "{t:?}");
            let key = t.key().unwrap();
            assert_eq!(T::parse_key(key), Some(t));
            let action = Action::SetStartType(t);
            assert_eq!(Action::parse(&action.arg()), Some(action));
        }
        assert_eq!(T::AutoDelayed.to_raw(), Some((2, true)));
        assert_eq!(T::Auto.to_raw(), Some((2, false)));
        assert_eq!(T::Manual.to_raw(), Some((3, false)));
        assert_eq!(T::Disabled.to_raw(), Some((4, false)));
        for t in [T::Boot, T::System, T::Unknown] {
            assert_eq!((t.to_raw(), t.key()), (None, None));
        }
        assert_eq!(T::parse_key("boot"), None);
        assert_eq!(Action::parse("start-type=boot"), None);
        assert_eq!(Action::parse("start-type="), None);
        assert_eq!(Action::SetStartType(T::AutoDelayed).arg(), "start-type=auto-delayed");
    }

    #[test]
    fn drivers_and_protected_services_block_start_type_changes() {
        let own = SERVICE_WIN32_OWN_PROCESS.0;
        assert_eq!(start_type_block(own, SERVICE_LAUNCH_PROTECTED_NONE), None);
        assert_eq!(start_type_block(SERVICE_WIN32_SHARE_PROCESS.0, 0), None);
        assert_eq!(start_type_block(SERVICE_KERNEL_DRIVER.0, 0), Some(StartTypeBlock::Driver));
        assert_eq!(start_type_block(SERVICE_FILE_SYSTEM_DRIVER.0, 0), Some(StartTypeBlock::Driver));
        assert_eq!(start_type_block(own, SERVICE_LAUNCH_PROTECTED_ANTIMALWARE_LIGHT), Some(StartTypeBlock::Protected));
        assert_eq!(start_type_block(own, SERVICE_LAUNCH_PROTECTED_WINDOWS), Some(StartTypeBlock::Protected));
    }

    #[test]
    fn only_access_denied_without_elevation_goes_to_uac_child() {
        let denied = ServiceError::System(SystemError::Win32(ERROR_ACCESS_DENIED.0 as i32));
        assert!(should_elevate(&denied, true, false));
        assert!(!should_elevate(&denied, false, false), "测试钩子不弹 UAC");
        assert!(!should_elevate(&denied, true, true), "已提权不再提权");
        assert!(!should_elevate(&ServiceError::StartTypeBlocked(StartTypeBlock::Protected), true, false));
        assert!(!should_elevate(&ServiceError::System(SystemError::Win32(1060)), true, false));
        let args = elevated_args("Spooler", Action::SetStartType(StartType::Manual));
        assert_eq!(args, format!("--service-control start-type=manual {}", crate::startup::hex("Spooler")));
        // 子进程能解析回同一动作与服务名。
        let parts: Vec<String> = std::iter::once("tml".to_owned()).chain(args.split(' ').map(str::to_owned)).collect();
        assert_eq!(Action::parse(&parts[2]), Some(Action::SetStartType(StartType::Manual)));
        assert_eq!(crate::startup::unhex(&parts[3]).as_deref(), Some("Spooler"));
    }

    #[test]
    fn real_start_type_checks_are_read_only() {
        // 普通 Win32 服务可以修改；WinDefend 在装有 Defender 的系统上是受保护服务（没有时跳过）。
        assert_eq!(start_type_block_of("RpcSs").unwrap(), None);
        if let Ok(b) = start_type_block_of("WinDefend") {
            eprintln!("WinDefend: {b:?}");
        }
        // 未提权时修改被拒绝访问（不弹 UAC）；已提权时跳过，避免改动真实服务。
        if !crate::is_elevated() {
            let r = control("RpcSs", Action::SetStartType(StartType::Auto), false);
            assert!(matches!(&r, Err(ServiceError::System(s)) if s.is_access_denied()), "{r:?}");
        }
        assert_eq!(control("RpcSs", Action::SetStartType(StartType::Boot), false), Err(ServiceError::BadStartType));
        assert!(start_type_block_of("tml-no-such-service-xyz").is_err());
    }

    #[test]
    fn enum_declaration_order_is_the_sort_order() {
        // 服务页按状态、启动类型排序时直接比较枚举，顺序即声明顺序（与改动前一致）。
        use ServiceState as S;
        let states = [S::Stopped, S::StartPending, S::StopPending, S::Running, S::ContinuePending, S::PausePending, S::Paused, S::Unknown];
        assert!(states.windows(2).all(|w| w[0] < w[1]));
        use StartType as T;
        let types = [T::Auto, T::AutoDelayed, T::Manual, T::Disabled, T::Boot, T::System, T::Unknown];
        assert!(types.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn errors_keep_reason_and_system_code() {
        let denied = ServiceError::System(SystemError::Win32(ERROR_ACCESS_DENIED.0 as i32));
        assert_eq!(denied.exit_code(), 5);
        assert_eq!(ServiceError::NotStopped.exit_code(), 1);
        assert_eq!(ServiceError::System(SystemError::Other { code: -1, message: "x".into() }).exit_code(), 1);
    }

    #[test]
    fn extracts_svchost_group() {
        assert_eq!(svchost_group(r"C:\Windows\system32\svchost.exe -k netsvcs -p"), "netsvcs");
        assert_eq!(svchost_group(r#""C:\Windows\System32\svchost.exe" -K "LocalService""#), "LocalService");
        assert_eq!(svchost_group(r"C:\Windows\System32\spoolsv.exe"), "");
        assert_eq!(svchost_group("svchost.exe -k"), "");
    }

    #[test]
    fn cache_reads_each_service_once_and_drops_removed() {
        let svc = |n: &str| Service { name: n.into(), ..Default::default() };
        let mut list = ServiceList::new();
        let mut reads = vec![];
        let mut services = vec![svc("A"), svc("B")];
        list.fill(&mut services, |n| {
            reads.push(n.to_owned());
            (n == "A").then(|| Config { start_type: StartType::Manual, ..Default::default() })
        });
        assert_eq!(services[0].config.start_type, StartType::Manual);
        assert_eq!(services[1].config.start_type, StartType::Unknown);
        // 再次刷新：A、B（含读取失败的 B）都命中缓存，只读新出现的 C。
        let mut services = vec![svc("a"), svc("B"), svc("C")];
        list.fill(&mut services, |n| {
            reads.push(n.to_owned());
            None
        });
        assert_eq!(services[0].config.start_type, StartType::Manual);
        assert_eq!(reads, ["A", "B", "C"]);
        // B 消失后清出缓存，再出现时重新读取。
        let mut services = vec![svc("A")];
        list.fill(&mut services, |_| unreachable!());
        let mut services = vec![svc("B")];
        let mut again = 0;
        list.fill(&mut services, |_| {
            again += 1;
            None
        });
        assert_eq!(again, 1);
        list.clear_cache();
        assert!(list.cache.is_empty());
    }

    #[test]
    fn real_services_can_be_listed_without_writing() {
        let mut list = ServiceList::new();
        let rows = list.list().unwrap();
        eprintln!("枚举 {} 个服务", rows.len());
        assert!(rows.len() > 20);
        let rpc = rows.iter().find(|s| s.name.eq_ignore_ascii_case("RpcSs")).expect("RpcSs");
        assert_eq!(rpc.state, ServiceState::Running);
        assert!(rpc.pid > 0);
        assert_eq!(rpc.config.start_type, StartType::Auto);
        assert_eq!(rpc.config.group, "rpcss");
        assert!(!rpc.config.description.is_empty());
        assert!(rows.iter().filter(|s| s.state == ServiceState::Stopped).all(|s| s.pid == 0));
    }

    #[test]
    fn host_map_groups_running_services_by_pid_and_sorts_names() {
        let svc = |n: &str, pid| Service { name: n.into(), pid, ..Default::default() };
        let map = host_map(&[svc("Spooler", 700), svc("bits", 900), svc("Dhcp", 900), svc("Stopped", 0), svc("Alpha", 900)]);
        assert_eq!(map.len(), 2, "PID 0（未运行）不计入");
        assert_eq!(&*map[&900], ["Alpha", "bits", "Dhcp"]);
        assert_eq!(&*map[&700], ["Spooler"]);
        assert!(host_map(&[]).is_empty());
    }

    #[test]
    fn apply_hosts_fills_only_hosting_processes() {
        use crate::snapshot::{Process, ProcessKey};
        let mk = |pid| Process {
            key: ProcessKey { pid, create_time: 1 },
            parent_pid: 0,
            name: "p".into(),
            cpu_usage: 0.0,
            private_working_set: 0,
            disk_rate: 0.0,
            gpu_usage: None,
            eco_mode: None,
            services: Some(Arc::from(vec!["旧".to_owned()])),
            critical: false,
            counters: Default::default(),
            info: None,
        };
        let mut procs = vec![mk(5), mk(6)];
        let map: HostMap = [(5, Arc::from(vec!["A".to_owned(), "B".to_owned()]))].into_iter().collect();
        apply_hosts(&mut procs, &map);
        assert_eq!(procs[0].services.as_deref().map(<[String]>::len), Some(2));
        // 不承载服务的进程清回 None（不残留上一轮的值）。
        assert!(procs[1].services.is_none());
    }

    #[test]
    fn real_hosts_include_rpcss_process_and_are_cheap() {
        let started = Instant::now();
        let map = hosted_services().unwrap();
        eprintln!("hosted_services {} ms，{} 个宿主进程", started.elapsed().as_millis(), map.len());
        let rpc = ServiceList::new().list().unwrap().into_iter().find(|s| s.name.eq_ignore_ascii_case("RpcSs")).unwrap();
        assert!(map[&rpc.pid].iter().any(|n| n.eq_ignore_ascii_case("RpcSs")));
        assert!(map.values().all(|v| !v.is_empty()));
    }

    #[test]
    fn invalid_cli_cannot_control() {
        for tail in [vec!["pause", "61"], vec!["start", "a"], vec!["start"], vec!["stop", "2f78"], vec!["start-type=boot", "61"], vec!["start-type=manual", "2f78"]] {
            let args = [vec!["tml", "--service-control"], tail]
                .concat()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_ne!(run_cli(&args), Some(0));
        }
        assert_eq!(run_cli(&["tml".into(), "--startup-set".into()]), None);
    }
}

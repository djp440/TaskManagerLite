//! Windows 服务。仅由服务页按需调用：一次枚举拿到状态与 PID，启动类型、描述、组按服务名缓存。
use std::{
    collections::HashMap,
    io,
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
    core::{HSTRING, PWSTR},
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::StartPending => "正在启动",
            Self::StopPending => "正在停止",
            Self::Running => "正在运行",
            Self::ContinuePending => "正在继续",
            Self::PausePending => "正在暂停",
            Self::Paused => "已暂停",
            Self::Unknown => "未知",
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::AutoDelayed => "自动（延迟启动）",
            Self::Manual => "手动",
            Self::Disabled => "已禁用",
            Self::Boot => "引导",
            Self::System => "系统",
            Self::Unknown => "未知",
        }
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
}

/// 启动、停止、重启。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
    fn arg(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "启动",
            Self::Stop => "停止",
            Self::Restart => "重启",
        }
    }
}

struct Handle(SC_HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn win(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn manager(access: u32) -> io::Result<Handle> {
    unsafe { OpenSCManagerW(None, None, access) }
        .map(Handle)
        .map_err(win)
}

fn open(scm: &Handle, name: &str, access: u32) -> io::Result<Handle> {
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
fn enumerate() -> io::Result<Vec<Service>> {
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
fn query(read: impl Fn(Option<&mut [u8]>, &mut u32) -> windows::core::Result<()>) -> io::Result<Vec<u64>> {
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

fn read_config(scm: &Handle, name: &str) -> io::Result<Config> {
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
        return Err(io::Error::other("服务配置为空"));
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

    pub fn list(&mut self) -> io::Result<Vec<Service>> {
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

fn status(svc: &Handle) -> io::Result<SERVICE_STATUS_PROCESS> {
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
fn wait(svc: &Handle, pending: ServiceState, timeout: Duration) -> io::Result<ServiceState> {
    let end = Instant::now() + timeout;
    loop {
        let state = ServiceState::from_raw(status(svc)?.dwCurrentState.0);
        if state != pending {
            return Ok(state);
        }
        if Instant::now() >= end {
            return Err(io::Error::from_raw_os_error(ERROR_SERVICE_REQUEST_TIMEOUT.0 as i32));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

const WAIT: Duration = Duration::from_secs(30);

fn start(svc: &Handle) -> io::Result<()> {
    unsafe { StartServiceW(svc.0, None) }.map_err(win)?;
    match wait(svc, ServiceState::StartPending, WAIT)? {
        ServiceState::Stopped => Err(io::Error::other("服务启动后随即停止")),
        _ => Ok(()),
    }
}

fn stop(svc: &Handle) -> io::Result<()> {
    let mut s = SERVICE_STATUS::default();
    match unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut s) } {
        Ok(()) => {}
        // 已经停止的服务视为停止成功（重启时直接启动）。
        Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(e) => return Err(win(e)),
    }
    match wait(svc, ServiceState::StopPending, WAIT)? {
        ServiceState::Stopped => Ok(()),
        _ => Err(io::Error::other("服务没有停止")),
    }
}

fn control_direct(name: &str, action: Action) -> io::Result<()> {
    if name.is_empty() || name.contains(['\0', '/', '\\']) {
        return Err(io::Error::other("无效的服务名"));
    }
    let scm = manager(SC_MANAGER_CONNECT)?;
    let access = SERVICE_QUERY_STATUS
        | match action {
            Action::Start => SERVICE_START,
            Action::Stop => SERVICE_STOP,
            Action::Restart => SERVICE_START | SERVICE_STOP,
        };
    let svc = open(&scm, name, access)?;
    match action {
        Action::Start => start(&svc),
        Action::Stop => stop(&svc),
        Action::Restart => stop(&svc).and_then(|()| start(&svc)),
    }
}

/// 执行操作并等待服务到达目标状态。未提权时先直接尝试，权限不足且 `elevate` 为真时
/// 通过固定参数的提权子进程执行（弹出 UAC）；`elevate` 为假时直接返回“拒绝访问”。
pub fn control(name: &str, action: Action, elevate: bool) -> io::Result<()> {
    match control_direct(name, action) {
        Err(e)
            if elevate
                && !crate::is_elevated()
                && e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) =>
        {
            crate::startup::run_elevated(&format!(
                "--service-control {} {}",
                action.arg(),
                crate::startup::hex(name)
            ))
        }
        r => r,
    }
}

/// 提权子进程入口：`--service-control <start|stop|restart> <十六进制服务名>`。
pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("--service-control") {
        return None;
    }
    let result = (|| {
        if args.len() != 4 {
            return Err(io::Error::other("参数数量错误"));
        }
        let action = Action::parse(&args[2]).ok_or_else(|| io::Error::other("操作错误"))?;
        control_direct(&crate::startup::unhex(&args[3])?, action)
    })();
    Some(result.map_or_else(|e| e.raw_os_error().unwrap_or(1), |()| 0))
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
    fn maps_states_and_start_types() {
        let states: Vec<_> = (0..=8).map(|v| ServiceState::from_raw(v).label()).collect();
        assert_eq!(
            states,
            ["未知", "已停止", "正在启动", "正在停止", "正在运行", "正在继续", "正在暂停", "已暂停", "未知"]
        );
        assert_eq!(StartType::from_raw(2, false), StartType::Auto);
        assert_eq!(StartType::from_raw(2, true).label(), "自动（延迟启动）");
        assert_eq!(StartType::from_raw(3, true), StartType::Manual);
        assert_eq!(StartType::from_raw(4, false).label(), "已禁用");
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
    fn invalid_cli_cannot_control() {
        for tail in [vec!["pause", "61"], vec!["start", "a"], vec!["start"], vec!["stop", "2f78"]] {
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

//! 核心库枚举与错误到界面显示文案的映射（issue #88）。
//!
//! 核心库（`tml-core`）只返回枚举和带系统错误码的结构化错误，不返回显示文案；
//! 面向用户的中文文案集中在这里。以后接入翻译机制（issue #87）时，只需在这个模块的函数里
//! 把字面量包一层 `tr()`，调用方不用改。
//!
//! 系统自己给出的描述（Win32 错误码对应的文字，如“拒绝访问。”）由操作系统按系统语言提供，
//! 不是本程序撰写的文案，这里原样显示，不翻译。

use std::borrow::Cow;

use tml_core::ConnectionKind;
use tml_core::connections::{ConnectionError, TcpState};
use tml_core::services::{Action, ServiceError, ServiceState, StartType, StartTypeBlock};
use tml_core::startup::{SourceError, StartupError, StartupSource};
use tml_core::{
    AutostartError, DebuggerValue, ElevationError, IfeoError, MediaType, PriorityClass, ProcessControlError, RunError,
    ShellError, SystemError, UnsafeReason, WindowError, WindowOp,
};

/// TCP 连接状态（连接页“状态”列）；UDP 没有状态，显示为空。
pub fn tcp_state(state: Option<TcpState>) -> Cow<'static, str> {
    let Some(state) = state else { return "".into() };
    match state {
        TcpState::Closed => "已关闭".into(),
        TcpState::Listen => "监听".into(),
        TcpState::SynSent => "已发送 SYN".into(),
        TcpState::SynReceived => "已收到 SYN".into(),
        TcpState::Established => "已建立".into(),
        TcpState::FinWait1 => "FIN 等待 1".into(),
        TcpState::FinWait2 => "FIN 等待 2".into(),
        TcpState::CloseWait => "等待关闭".into(),
        TcpState::Closing => "正在关闭".into(),
        TcpState::LastAck => "最后确认".into(),
        TcpState::TimeWait => "等待超时".into(),
        TcpState::DeleteTcb => "删除 TCB".into(),
        TcpState::Unknown(v) => format!("未知（{v}）").into(),
    }
}

/// 读取连接表失败的原因（连接页底栏）。
pub fn connection_error(e: &ConnectionError) -> String {
    match e {
        ConnectionError::System { protocol, error } => format!("{} 连接表：{error}", protocol.name()),
        ConnectionError::KeepsGrowing { protocol } => format!("{} 连接表变化太快，多次重试仍未读到", protocol.name()),
    }
}

/// 服务状态（服务页“状态”列）。
pub fn service_state(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Stopped => "已停止",
        ServiceState::StartPending => "正在启动",
        ServiceState::StopPending => "正在停止",
        ServiceState::Running => "正在运行",
        ServiceState::ContinuePending => "正在继续",
        ServiceState::PausePending => "正在暂停",
        ServiceState::Paused => "已暂停",
        ServiceState::Unknown => "未知",
    }
}

/// 服务启动类型（服务页“启动类型”列）。
pub fn start_type(start_type: StartType) -> &'static str {
    match start_type {
        StartType::Auto => "自动",
        StartType::AutoDelayed => "自动（延迟启动）",
        StartType::Manual => "手动",
        StartType::Disabled => "已禁用",
        StartType::Boot => "引导",
        StartType::System => "系统",
        StartType::Unknown => "未知",
    }
}

/// 服务操作的动词（用于“正在{}服务…”“已{}服务…”“无法{}服务…”）。
pub fn service_action(action: Action) -> &'static str {
    match action {
        Action::Start => "启动",
        Action::Stop => "停止",
        Action::Restart => "重启",
        Action::SetStartType(_) => "修改启动类型",
    }
}

/// 服务操作进行中的提示（`hint` 为提权说明，可为空）。
pub fn service_pending(action: Action, name: &str, hint: &str) -> String {
    match action {
        Action::SetStartType(t) => format!("正在将服务“{name}”的启动类型改为“{}”{hint}…", start_type(t)),
        a => format!("正在{}服务“{name}”{hint}…", service_action(a)),
    }
}

/// 服务操作成功的提示。
pub fn service_done(action: Action, name: &str) -> String {
    match action {
        Action::SetStartType(t) => format!("已将服务“{name}”的启动类型改为“{}”", start_type(t)),
        a => format!("已{}服务“{name}”", service_action(a)),
    }
}

/// 服务操作失败的提示。
pub fn service_failed(action: Action, name: &str, e: &ServiceError) -> String {
    match action {
        Action::SetStartType(t) => format!("无法将服务“{name}”的启动类型改为“{}”：{}", start_type(t), service_error(e)),
        a => format!("无法{}服务“{name}”：{}", service_action(a), service_error(e)),
    }
}

/// 不能修改启动类型的原因（菜单标题后缀与失败提示）。
pub fn start_type_block(b: StartTypeBlock) -> &'static str {
    match b {
        StartTypeBlock::Driver => "驱动服务不能在此修改启动类型",
        StartTypeBlock::Protected => "受系统保护的服务，不能修改启动类型",
    }
}

/// 启动项来源（启动项页“来源”列）。
pub fn startup_source(source: StartupSource) -> &'static str {
    match source {
        StartupSource::HkcuRun => "HKCU · Run",
        StartupSource::HklmRun => "HKLM · Run",
        StartupSource::HkcuRun32 => "HKCU · Run32",
        StartupSource::HklmRun32 => "HKLM · Run32",
        StartupSource::FolderCurrentUser => "启动文件夹 · 当前用户",
        StartupSource::FolderAllUsers => "启动文件夹 · 所有用户",
    }
}

/// 网卡连接类型（性能页网络卡片标题与详情）。
pub fn connection_kind(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::WiFi => "Wi-Fi",
        ConnectionKind::Ethernet => "以太网",
        ConnectionKind::Other => "其他",
    }
}

/// 性能页曲线时间窗口的名称（#86）。设置文件里的键在 `history::Window::key`，不是显示文案。
pub fn window_name(window: crate::history::Window) -> &'static str {
    use crate::history::Window;
    match window {
        Window::Sec60 => "60 秒",
        Window::Min5 => "5 分钟",
        Window::Min10 => "10 分钟",
        Window::Hour1 => "1 小时",
    }
}

/// 曲线悬停提示里的“N 秒前”（#81）。不足 1 分钟按秒，不足 1 小时按“M 分 S 秒”，再长按“H 小时 M 分”；
/// `approx` 为真（聚合点代表一段时间）时前面加“约”。
pub fn hover_ago(secs: f64, approx: bool) -> String {
    let n = secs.max(0.0).round() as u64;
    let text = match n {
        0..=59 => format!("{n} 秒前"),
        60..=3599 => match n % 60 {
            0 => format!("{} 分钟前", n / 60),
            s => format!("{} 分 {s} 秒前", n / 60),
        },
        _ => match n % 3600 / 60 {
            0 => format!("{} 小时前", n / 3600),
            m => format!("{} 小时 {m} 分前", n / 3600),
        },
    };
    if approx { format!("约 {text}") } else { text }
}

/// 悬停处没有数据（采样暂停、窗口不可见且没开后台继续采样，或曲线尚未覆盖到这里）。
pub fn hover_no_data() -> &'static str {
    "该时刻没有数据"
}

/// 聚合点的说明：每个点覆盖的秒数，数值取这段时间内的最大值。
pub fn hover_bucket(bucket_secs: u64) -> String {
    format!("每点 {bucket_secs} 秒")
}

pub fn hover_peak(value: &str) -> String {
    format!("峰值 {value}")
}

pub fn hover_mean(value: &str) -> String {
    format!("均值 {value}")
}

/// 悬停时刻占用最高的进程。磁盘速率是整机口径（进程的读写不区分物理磁盘），有多块磁盘时注明。
pub fn hover_top_process(kind: crate::ResourceKind, multi_disk: bool, name: &str, pid: u32) -> String {
    let label = match kind {
        crate::ResourceKind::Disk if multi_disk => "全部磁盘读写最高",
        crate::ResourceKind::Disk => "读写最高",
        _ => "CPU 最高",
    };
    format!("{label}：{name}（PID {pid}）")
}

/// 物理磁盘介质类型（性能页磁盘详情）。
pub fn media_type(kind: MediaType) -> &'static str {
    match kind {
        MediaType::Unknown => "未知",
        MediaType::Ssd => "SSD",
        MediaType::Hdd => "HDD",
    }
}

/// 系统错误的描述：操作系统给出的文字，原样显示。
fn elevation_error(e: &ElevationError) -> String {
    match e {
        ElevationError::System(e) => e.to_string(),
        ElevationError::NoHandle => "提权进程没有返回句柄".into(),
        ElevationError::Timeout => "等待提权超时，请刷新核对状态".into(),
    }
}

/// 服务操作与枚举失败的原因（“无法启动服务…：{原因}”“无法读取服务列表：{原因}”）。
pub fn service_error(e: &ServiceError) -> String {
    match e {
        ServiceError::System(e) => e.to_string(),
        ServiceError::Elevation(e) => elevation_error(e),
        ServiceError::EmptyConfig => "服务配置为空".into(),
        ServiceError::StoppedAfterStart => "服务启动后随即停止".into(),
        ServiceError::NotStopped => "服务没有停止".into(),
        ServiceError::InvalidName => "无效的服务名".into(),
        ServiceError::BadArgumentCount => "参数数量错误".into(),
        ServiceError::BadAction => "操作错误".into(),
        ServiceError::BadNameEncoding => "名称编码错误".into(),
        ServiceError::StartTypeBlocked(b) => start_type_block(*b).into(),
        ServiceError::BadStartType => "不支持的启动类型".into(),
    }
}

/// 启动项修改失败的原因（“无法修改启动项：{原因}”）。
pub fn startup_error(e: &StartupError) -> String {
    match e {
        StartupError::System(e) => e.to_string(),
        StartupError::Elevation(e) => elevation_error(e),
        StartupError::InvalidEntry => "无效的启动项".into(),
        StartupError::EntryGone => "启动项已不存在，请刷新列表".into(),
        StartupError::BadNameEncoding => "名称编码错误".into(),
        StartupError::BadArgumentCount => "参数数量错误".into(),
        StartupError::BadState => "状态错误".into(),
    }
}

/// 枚举启动项时某个来源读取失败的提示（来源用编号，与改动前一致）。
pub fn startup_source_error(e: &SourceError) -> String {
    format!("来源 {} 读取失败：{}", e.source.index(), startup_error(&e.error))
}

/// 右键菜单动作（打开文件位置、属性、在线搜索）失败的原因。
pub fn shell_error(e: &ShellError) -> String {
    match e {
        ShellError::NotFound => "文件已不存在".into(),
        ShellError::Failed { message, .. } if message.trim().is_empty() => "系统调用失败".into(),
        ShellError::Failed { message, .. } => format!("系统调用失败：{}", message.trim()),
        ShellError::ShellExecute(code) => format!("系统调用失败：ShellExecute 返回 {code}"),
    }
}

/// “运行新任务”对话框里显示的失败原因。
pub fn run_error(e: &RunError) -> String {
    match e {
        RunError::Empty => "请输入程序、文件夹、文档或网址".into(),
        RunError::Cancelled => "已取消：没有在 UAC 对话框中允许以管理员权限运行".into(),
        // 2 找不到文件，3 找不到路径。
        RunError::System(SystemError::Win32(2 | 3)) => "找不到指定的程序、文件、文件夹或网址，请检查输入是否正确".into(),
        RunError::System(e) => format!("无法启动：{e}"),
    }
}

/// 进程优先级（菜单项、状态栏）。
pub fn priority_name(class: PriorityClass) -> &'static str {
    match class {
        PriorityClass::Realtime => "实时",
        PriorityClass::High => "高",
        PriorityClass::AboveNormal => "高于正常",
        PriorityClass::Normal => "正常",
        PriorityClass::BelowNormal => "低于正常",
        PriorityClass::Low => "低",
    }
}

/// 状态栏：优先级设置成功。系统实际生效的类别与请求不同（未提权时“实时”只会变成“高”）时附带说明。
pub fn priority_applied(name: &str, requested: PriorityClass, actual: PriorityClass) -> String {
    if requested == actual {
        format!("{name} → {}", priority_name(actual))
    } else {
        format!("{name} → {}（未以管理员身份运行，系统把“{}”降为了“{}”）", priority_name(actual), priority_name(requested), priority_name(actual))
    }
}

/// 状态栏：亲和性设置成功。
pub fn affinity_applied(name: &str, selected: u32, available: u32) -> String {
    format!("{name} → {selected}/{available} 个逻辑处理器")
}

/// 设置优先级、CPU 亲和性或效率模式失败的原因。
pub fn process_control_error(e: &ProcessControlError) -> String {
    match e {
        ProcessControlError::Gone => "进程已经退出".into(),
        ProcessControlError::AccessDenied => "没有权限修改该进程（受保护进程或需要管理员权限），可以以管理员身份重启后再试".into(),
        ProcessControlError::EmptyAffinity => "至少要选择一个逻辑处理器".into(),
        ProcessControlError::OutsideProcessorGroup => "所选处理器不在该进程所在的处理器组内".into(),
        ProcessControlError::Protected => "系统空闲 / System 进程或本程序自身，不允许此操作".into(),
        ProcessControlError::Unsupported => "当前 Windows 版本不支持效率模式（需要 Windows 10 1709 及以上）".into(),
        ProcessControlError::System(e) => format!("系统返回错误：{e}"),
    }
}

/// 状态栏：效率模式已开启（进程优先级随之降为低）。
pub fn eco_enabled(name: &str) -> String {
    format!("{name} → 已开启效率模式（优先级降为“低”）")
}

/// 状态栏：效率模式已关闭；`restored` 为关闭后的优先级（没有恢复优先级时为 `None`）。
pub fn eco_disabled(name: &str, restored: Option<PriorityClass>) -> String {
    match restored {
        Some(p) => format!("{name} → 已关闭效率模式（优先级恢复为“{}”）", priority_name(p)),
        None => format!("{name} → 已关闭效率模式"),
    }
}

/// 状态栏：对应用及其后代批量切换效率模式的结果。
pub fn eco_batch(name: &str, on: bool, ok: usize, failed: usize) -> String {
    let verb = if on { "开启" } else { "关闭" };
    if failed == 0 {
        format!("{name} → 已为 {ok} 个进程{verb}效率模式")
    } else {
        format!("{name} → 已为 {ok} 个进程{verb}效率模式，{failed} 个失败（权限不足或已退出）")
    }
}

/// 右键菜单“效率模式”项的标题；置灰时括号里写原因（`Some(true)` 关键 / 自身，`Some(false)` 无权限）。
pub fn eco_menu_title(disabled_reason: Option<bool>) -> &'static str {
    match disabled_reason {
        None => "效率模式",
        Some(true) => "效率模式（系统关键进程或本程序，不可开启）",
        Some(false) => "效率模式（没有权限）",
    }
}

/// 菜单项置灰时的原因（效率模式）。
pub fn eco_disabled_reason(critical: bool) -> &'static str {
    if critical {
        "系统关键进程或本程序自身不能开启效率模式"
    } else {
        "没有权限修改该进程的效率模式"
    }
}

/// 挂起 / 恢复菜单项置灰的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuBlock {
    /// 探测得到的错误（受保护、没有权限、进程已退出）。
    Error(ProcessControlError),
    /// 没有符合条件的目标：挂起时目标都已挂起，恢复时目标都没有挂起。
    NoTarget,
}

/// 右键菜单“挂起”项的标题；置灰时括号里写原因，`None` 表示可用。
pub fn suspend_menu_title(block: Option<&MenuBlock>) -> &'static str {
    match block {
        None => "挂起",
        Some(MenuBlock::NoTarget) => "挂起（已挂起）",
        Some(MenuBlock::Error(ProcessControlError::Protected)) => "挂起（系统进程或本程序，不可挂起）",
        Some(MenuBlock::Error(ProcessControlError::Gone)) => "挂起（进程已退出）",
        Some(MenuBlock::Error(_)) => "挂起（没有权限）",
    }
}

/// 右键菜单“恢复”项的标题，规则同 [`suspend_menu_title`]。
pub fn resume_menu_title(block: Option<&MenuBlock>) -> &'static str {
    match block {
        None => "恢复",
        Some(MenuBlock::NoTarget) => "恢复（未挂起）",
        Some(MenuBlock::Error(ProcessControlError::Protected)) => "恢复（系统进程或本程序，不可操作）",
        Some(MenuBlock::Error(ProcessControlError::Gone)) => "恢复（进程已退出）",
        Some(MenuBlock::Error(_)) => "恢复（没有权限）",
    }
}

/// 窗口操作菜单项置灰的原因（#70）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowBlock {
    /// 窗口已处于操作的目标状态（已最大化、已最小化、本来就是正常大小）。
    State,
    /// 探测得到的错误（窗口已关闭、需要提权）。
    Error(WindowError),
}

/// 窗口操作的动作名（菜单项文字与失败提示的前缀）。
pub fn window_op_name(op: WindowOp) -> &'static str {
    match op {
        WindowOp::Activate => "切换到",
        WindowOp::Maximize => "最大化",
        WindowOp::Minimize => "最小化",
        WindowOp::Restore => "还原",
    }
}

/// 窗口操作菜单项的标题；置灰时括号里写原因，`None` 表示可用。
pub fn window_menu_title(op: WindowOp, block: Option<&WindowBlock>) -> String {
    let name = window_op_name(op);
    let reason = match block {
        None => return name.to_string(),
        Some(WindowBlock::State) => match op {
            WindowOp::Maximize => "已最大化",
            WindowOp::Minimize => "已最小化",
            WindowOp::Restore => "窗口已是正常大小",
            WindowOp::Activate => "无需切换",
        },
        Some(WindowBlock::Error(WindowError::Gone)) => "窗口已关闭",
        Some(WindowBlock::Error(WindowError::NeedsElevation)) => "需要提权",
        Some(WindowBlock::Error(_)) => "无法操作该窗口",
    };
    format!("{name}（{reason}）")
}

/// 窗口操作失败的原因（状态栏“操作失败：{动作}（{原因}）”里的原因）。
pub fn window_error(e: &WindowError) -> String {
    match e {
        WindowError::Gone => "窗口已关闭".into(),
        WindowError::NeedsElevation => "该应用以更高权限运行，需要以管理员身份重启本程序后才能操作它的窗口".into(),
        WindowError::NotApplied => "窗口没有响应该操作（可能不支持，或该应用以更高权限运行）".into(),
        WindowError::System(e) => format!("系统返回错误：{e}"),
    }
}

/// 状态栏：窗口操作已完成。
pub fn window_done(name: &str, op: WindowOp) -> String {
    let done = match op {
        WindowOp::Activate => "已切换到该窗口",
        WindowOp::Maximize => "已最大化",
        WindowOp::Minimize => "已最小化",
        WindowOp::Restore => "已还原",
    };
    format!("{name} → {done}")
}

/// 状态栏：前台锁定拒绝了“切换到”，已改为闪烁任务栏按钮。
pub fn window_flashed(name: &str) -> String {
    format!("{name} → 系统阻止了自动切换，已闪烁任务栏，请手动切换")
}

/// 状态栏：挂起 / 恢复的结果；单个进程只写动作结果，多个进程写成功数与失败数。
pub fn suspend_result(name: &str, suspend: bool, ok: usize, failed: usize) -> String {
    let verb = if suspend { "挂起" } else { "恢复" };
    if failed == 0 && ok == 1 {
        format!("{name} → 已{verb}")
    } else if failed == 0 {
        format!("{name} → 已{verb} {ok} 个进程")
    } else {
        format!("{name} → 已{verb} {ok} 个进程，{failed} 个失败（权限不足或已退出）")
    }
}

/// “浏览…”文件选择对话框的标题。
pub fn browse_title() -> &'static str {
    "选择要运行的程序或文件"
}

/// “浏览…”文件选择对话框的文件类型：(显示名, 通配符)。
pub fn browse_filters() -> [(&'static str, &'static str); 2] {
    [("程序", "*.exe;*.com;*.bat;*.cmd;*.msc;*.lnk"), ("所有文件", "*.*")]
}

/// 开机启动操作失败的原因（“无法读取开机启动状态：{原因}”“无法修改开机启动：{原因}”）。
pub fn autostart_error(e: &AutostartError) -> String {
    match e {
        AutostartError::InvalidTestKey => format!(
            "{} 必须以 {} 开头且其后不为空",
            tml_core::AUTOSTART_TEST_KEY_ENV,
            tml_core::TEST_KEY_PREFIX
        ),
        AutostartError::System(e) => e.to_string(),
    }
}

/// `Debugger` 值的显示：字符串原文，非字符串类型给出类型号。
pub fn debugger_value(v: &DebuggerValue) -> Cow<'_, str> {
    match v {
        DebuggerValue::Text(t) => Cow::Borrowed(t),
        DebuggerValue::NonString { value_type } => Cow::Owned(format!("<非字符串值，类型 {value_type}>")),
    }
}

/// exe 所在位置不安全的具体原因（嵌在位置不安全的说明里）。
pub fn unsafe_reason(why: &UnsafeReason) -> String {
    match why {
        UnsafeReason::NoAcl { path } => format!("{} 没有访问控制（任何人可写）", path.display()),
        UnsafeReason::BroadWrite { path, who } => format!("{} 授予 {who} 写入权限", path.display()),
        UnsafeReason::ElevatedCheckFailed => "提权子进程的位置检查未通过".into(),
    }
}

/// 接管任务管理器失败的原因。位置不安全的情形界面另有说明（见 [`unsafe_location_note`]），
/// 这里保留的是原先核心库给出的说明（其中提到命令行的 `--force`）。
pub fn ifeo_error(e: &IfeoError) -> String {
    match e {
        IfeoError::AccessDenied => "没有写入系统注册表的权限（需要管理员）".into(),
        IfeoError::Occupied(v) => format!("已被其他程序登记：{}", debugger_value(v)),
        IfeoError::Cancelled => "已取消管理员授权".into(),
        IfeoError::UnsafeLocation(why) => format!(
            "exe 所在位置可被其他用户改写（{}）。请用安装包安装（默认装到 Program Files）或移到受保护目录，或使用 --force",
            unsafe_reason(why)
        ),
        IfeoError::Registry(code) => format!("注册表操作失败（错误码 {code}）"),
        IfeoError::TestKeyNotUnicode => format!("{} 不是合法的 Unicode", tml_core::TEST_KEY_ENV),
        IfeoError::TestKeyInvalid(k) => format!(
            "{} 必须以 {} 开头且其后不为空：{k}",
            tml_core::TEST_KEY_ENV,
            tml_core::TEST_KEY_PREFIX
        ),
        IfeoError::ExeNotFound(p) => format!("找不到程序文件：{}", p.display()),
        IfeoError::CurrentExeUnavailable(e) => format!("无法取得本程序路径：{e}"),
        IfeoError::ElevationLaunchFailed(e) => format!("无法启动提权子进程：{e}"),
        IfeoError::ElevationNoHandle => "提权子进程没有返回句柄".into(),
        IfeoError::ElevationTimeout => "等待提权子进程超时，结果未知，请重新读取登记状态".into(),
        IfeoError::ElevationChildFailed(code) => format!("提权子进程失败（退出码 {code}）"),
        IfeoError::AclUnreadable { path, code } => {
            format!("无法读取 {} 的访问控制列表（错误码 {code}）；确认安全后可使用 --force", path.display())
        }
        IfeoError::AclInfoFailed(e) => format!("读取 ACL 信息失败：{e}"),
        IfeoError::AceUnreadable { path, index } => {
            format!("无法读取 {} 的第 {index} 条访问控制项；确认安全后可使用 --force", path.display())
        }
        IfeoError::SidUnparseable { path, index } => {
            format!("无法解析 {} 的第 {index} 条访问控制项的 SID；确认安全后可使用 --force", path.display())
        }
        IfeoError::ThreadSpawnFailed(e) => format!("无法创建后台线程：{e}"),
        IfeoError::System(e) => e.to_string(),
    }
}

/// 设置页：登记因程序位置不安全被拒绝时的说明（不含命令行的 `--force`）。
pub fn unsafe_location_note(why: &UnsafeReason) -> String {
    format!(
        "程序所在位置可被其他用户改写（{}），为安全起见不予登记。请用安装包安装（默认装到 Program Files）后再试，或把程序移到受保护的目录",
        unsafe_reason(why)
    )
}

// ---- 导出为 CSV 与复制所选行（issue #82） ----
// 进程列表的列标题复用界面已有的（基本列来自 Slint 的 `Columns.title-*`，详细信息列来自 `details::title`），
// 这里只有导出新增的文案。

/// 导出的内容种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Process,
    Performance,
}

/// 分组视图中进程所属的组（导出的“分组”列）。
pub fn group_name(group: tml_core::Group) -> &'static str {
    match group {
        tml_core::Group::Apps => "应用",
        tml_core::Group::Background => "后台进程",
    }
}

/// 分组视图导出的“分组”列标题。
pub fn export_group_title() -> &'static str {
    "分组"
}

/// 树形、分组视图导出的“层级”列标题（0 为根，1 为根的子进程，依此类推）。
pub fn export_depth_title() -> &'static str {
    "层级"
}

/// 默认文件名里的前缀（后面接时间戳与 `.csv`）。
pub fn export_file_stem(kind: ExportKind) -> &'static str {
    match kind {
        ExportKind::Process => "进程列表",
        ExportKind::Performance => "性能快照",
    }
}

/// 保存对话框的标题。
pub fn export_save_title(kind: ExportKind) -> &'static str {
    match kind {
        ExportKind::Process => "导出进程列表为 CSV",
        ExportKind::Performance => "导出性能快照为 CSV",
    }
}

/// 保存对话框的文件类型：(显示名, 通配符)。
pub fn export_filters() -> [(&'static str, &'static str); 2] {
    [("CSV 文件", "*.csv"), ("所有文件", "*.*")]
}

/// 状态栏里的动作名（“操作失败：{动作}（{原因}）”）。
pub fn export_action() -> &'static str {
    "导出 CSV"
}

/// 复制所选行的动作名。
pub fn copy_row_action() -> &'static str {
    "复制所选行"
}

/// 导出前无法生成表格的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    /// 还没有采样到数据。
    NoData,
    /// 分组视图需要的窗口信息还没有到。
    GroupNotReady,
}

pub fn export_error(e: ExportError) -> &'static str {
    match e {
        ExportError::NoData => "还没有采样到数据，请稍后再试",
        ExportError::GroupNotReady => "分组视图的窗口信息尚未就绪，请稍后再试",
    }
}

/// 复制所选行时没有可复制的行。
pub fn no_selected_row() -> &'static str {
    "没有选中的进程"
}

/// 写 CSV 文件失败的原因（系统给出的描述原样显示，见模块说明）。
pub fn export_write_error(file: &str, e: &SystemError) -> String {
    format!("无法写入 {file}：{e}")
}

/// 导出的敏感列名之间的分隔。
pub fn list_separator() -> &'static str {
    "、"
}

/// 性能快照的行类型：当前数值，或时间窗口内的曲线数据。
pub fn perf_row_kind(series: bool) -> &'static str {
    if series { "曲线" } else { "当前" }
}

/// 性能快照的表头。
pub fn perf_header() -> [&'static str; 8] {
    ["类型", "资源", "实例", "指标", "时间", "距今（秒）", "数值", "单位"]
}

/// 性能快照里的指标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerfMetric {
    CpuUsage,
    MemoryUsed,
    MemoryTotal,
    MemoryPercent,
    DiskActive,
    DiskResponse,
    DiskRead,
    DiskWrite,
    NetReceive,
    NetSend,
    GpuUsage,
    GpuDedicated,
    GpuShared,
}

pub fn perf_metric(m: PerfMetric) -> &'static str {
    match m {
        PerfMetric::CpuUsage | PerfMetric::GpuUsage => "利用率",
        PerfMetric::MemoryUsed => "已用",
        PerfMetric::MemoryTotal => "总量",
        PerfMetric::MemoryPercent => "使用率",
        PerfMetric::DiskActive => "活动时间",
        PerfMetric::DiskResponse => "平均响应时间",
        PerfMetric::DiskRead => "读取速度",
        PerfMetric::DiskWrite => "写入速度",
        PerfMetric::NetReceive => "接收",
        PerfMetric::NetSend => "发送",
        PerfMetric::GpuDedicated => "专用显存",
        PerfMetric::GpuShared => "共享显存",
    }
}

/// 指标的单位（数值列是不带单位的原始数）。
pub fn perf_unit(m: PerfMetric) -> &'static str {
    match m {
        PerfMetric::CpuUsage
        | PerfMetric::MemoryPercent
        | PerfMetric::DiskActive
        | PerfMetric::GpuUsage => "%",
        PerfMetric::MemoryUsed
        | PerfMetric::MemoryTotal
        | PerfMetric::GpuDedicated
        | PerfMetric::GpuShared => "字节",
        PerfMetric::DiskResponse => "毫秒",
        PerfMetric::DiskRead | PerfMetric::DiskWrite | PerfMetric::NetReceive | PerfMetric::NetSend => "字节/秒",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 下面所有期望字符串都取自改动前核心库里的字面量（`git show HEAD:crates/core/src/...`），逐字相同。
    #[test]
    fn service_state_labels_match_the_original_text() {
        use ServiceState::*;
        let all = [
            (Stopped, "已停止"),
            (StartPending, "正在启动"),
            (StopPending, "正在停止"),
            (Running, "正在运行"),
            (ContinuePending, "正在继续"),
            (PausePending, "正在暂停"),
            (Paused, "已暂停"),
            (Unknown, "未知"),
        ];
        for (state, text) in all {
            assert_eq!(service_state(state), text, "{state:?}");
        }
    }

    #[test]
    fn start_type_labels_match_the_original_text() {
        use StartType::*;
        let all = [
            (Auto, "自动"),
            (AutoDelayed, "自动（延迟启动）"),
            (Manual, "手动"),
            (Disabled, "已禁用"),
            (Boot, "引导"),
            (System, "系统"),
            (Unknown, "未知"),
        ];
        for (kind, text) in all {
            assert_eq!(start_type(kind), text, "{kind:?}");
        }
    }

    #[test]
    fn service_action_verbs_match_the_original_text() {
        assert_eq!(service_action(Action::Start), "启动");
        assert_eq!(service_action(Action::Stop), "停止");
        assert_eq!(service_action(Action::Restart), "重启");
    }

    #[test]
    fn start_type_change_notes_name_service_target_and_reason() {
        let a = Action::SetStartType(StartType::AutoDelayed);
        assert_eq!(service_pending(a, "打印", ""), "正在将服务“打印”的启动类型改为“自动（延迟启动）”…");
        assert_eq!(service_done(a, "打印"), "已将服务“打印”的启动类型改为“自动（延迟启动）”");
        let blocked = ServiceError::StartTypeBlocked(StartTypeBlock::Protected);
        assert_eq!(service_failed(a, "打印", &blocked), "无法将服务“打印”的启动类型改为“自动（延迟启动）”：受系统保护的服务，不能修改启动类型");
        assert_eq!(service_error(&ServiceError::StartTypeBlocked(StartTypeBlock::Driver)), "驱动服务不能在此修改启动类型");
        assert_eq!(service_error(&ServiceError::BadStartType), "不支持的启动类型");
        // 启停的提示与改动前逐字相同。
        assert_eq!(service_pending(Action::Start, "X", "（提权）"), "正在启动服务“X”（提权）…");
        assert_eq!(service_done(Action::Stop, "X"), "已停止服务“X”");
        assert_eq!(service_failed(Action::Restart, "X", &ServiceError::NotStopped), "无法重启服务“X”：服务没有停止");
    }

    #[test]
    fn startup_source_labels_match_the_original_text() {
        // 改动前按来源编号 0..=5 排列。
        let originals = [
            "HKCU · Run",
            "HKLM · Run",
            "HKCU · Run32",
            "HKLM · Run32",
            "启动文件夹 · 当前用户",
            "启动文件夹 · 所有用户",
        ];
        for (index, text) in originals.iter().enumerate() {
            assert_eq!(startup_source(StartupSource::from_index(index as u8)), *text, "编号 {index}");
        }
        // 编号越界的兜底与改动前一致：所有用户的启动文件夹。
        assert_eq!(startup_source(StartupSource::from_index(200)), "启动文件夹 · 所有用户");
    }

    #[test]
    fn startup_source_enum_order_equals_the_old_label_order() {
        // 改动前“来源”列按显示文案的字节序排序；现在按枚举序，两者必须给出同样的顺序。
        let mut by_enum: Vec<_> = (0..6).map(StartupSource::from_index).collect();
        by_enum.sort();
        let mut by_label = by_enum.clone();
        by_label.sort_by(|a, b| startup_source(*a).cmp(startup_source(*b)));
        assert_eq!(by_enum, by_label);
        // 原来的顺序：HKCU Run、HKCU Run32、HKLM Run、HKLM Run32、当前用户文件夹、所有用户文件夹。
        let labels: Vec<_> = by_enum.iter().map(|s| startup_source(*s)).collect();
        assert_eq!(
            labels,
            ["HKCU · Run", "HKCU · Run32", "HKLM · Run", "HKLM · Run32", "启动文件夹 · 当前用户", "启动文件夹 · 所有用户"]
        );
    }

    #[test]
    fn connection_and_media_labels_match_the_original_text() {
        assert_eq!(connection_kind(ConnectionKind::WiFi), "Wi-Fi");
        assert_eq!(connection_kind(ConnectionKind::Ethernet), "以太网");
        assert_eq!(connection_kind(ConnectionKind::Other), "其他");
        assert_eq!(media_type(MediaType::Unknown), "未知");
        assert_eq!(media_type(MediaType::Ssd), "SSD");
        assert_eq!(media_type(MediaType::Hdd), "HDD");
    }

    #[test]
    fn service_errors_match_the_original_text() {
        let all = [
            (ServiceError::EmptyConfig, "服务配置为空"),
            (ServiceError::StoppedAfterStart, "服务启动后随即停止"),
            (ServiceError::NotStopped, "服务没有停止"),
            (ServiceError::InvalidName, "无效的服务名"),
            (ServiceError::BadArgumentCount, "参数数量错误"),
            (ServiceError::BadAction, "操作错误"),
            (ServiceError::BadNameEncoding, "名称编码错误"),
            (ServiceError::Elevation(ElevationError::NoHandle), "提权进程没有返回句柄"),
            (ServiceError::Elevation(ElevationError::Timeout), "等待提权超时，请刷新核对状态"),
        ];
        for (e, text) in all {
            assert_eq!(service_error(&e), text, "{e:?}");
        }
        // 系统错误：与改动前 `io::Error::from_raw_os_error(code)` 的 Display 相同。
        let denied = ServiceError::System(SystemError::Win32(5));
        assert_eq!(service_error(&denied), std::io::Error::from_raw_os_error(5).to_string());
        let timeout = ServiceError::Elevation(ElevationError::System(SystemError::Win32(1053)));
        assert_eq!(service_error(&timeout), std::io::Error::from_raw_os_error(1053).to_string());
    }

    #[test]
    fn startup_errors_match_the_original_text() {
        let all = [
            (StartupError::InvalidEntry, "无效的启动项"),
            (StartupError::EntryGone, "启动项已不存在，请刷新列表"),
            (StartupError::BadNameEncoding, "名称编码错误"),
            (StartupError::BadArgumentCount, "参数数量错误"),
            (StartupError::BadState, "状态错误"),
            (StartupError::Elevation(ElevationError::NoHandle), "提权进程没有返回句柄"),
            (StartupError::Elevation(ElevationError::Timeout), "等待提权超时，请刷新核对状态"),
        ];
        for (e, text) in all {
            assert_eq!(startup_error(&e), text, "{e:?}");
        }
        let denied = StartupError::System(SystemError::Win32(5));
        assert_eq!(startup_error(&denied), std::io::Error::from_raw_os_error(5).to_string());
        let hresult = StartupError::System(SystemError::Other { code: -2147023673, message: "操作已被用户取消。 (0x800704C7)".into() });
        assert_eq!(startup_error(&hresult), "操作已被用户取消。 (0x800704C7)");
        // 枚举失败的提示用来源编号。
        let source = SourceError { source: StartupSource::HklmRun32, error: StartupError::System(SystemError::Win32(5)) };
        assert_eq!(startup_source_error(&source), format!("来源 3 读取失败：{}", std::io::Error::from_raw_os_error(5)));
    }

    #[test]
    fn shell_errors_match_the_original_text() {
        assert_eq!(shell_error(&ShellError::NotFound), "文件已不存在");
        let failed = |message: &str| ShellError::Failed { code: -2147024891, message: message.into() };
        assert_eq!(shell_error(&failed("拒绝访问")), "系统调用失败：拒绝访问");
        assert_eq!(shell_error(&failed("  拒绝访问 \r\n")), "系统调用失败：拒绝访问");
        assert_eq!(shell_error(&failed("")), "系统调用失败");
        assert_eq!(shell_error(&failed("  ")), "系统调用失败");
        assert_eq!(shell_error(&ShellError::ShellExecute(2)), "系统调用失败：ShellExecute 返回 2");
    }

    #[test]
    fn run_errors_are_readable() {
        assert_eq!(run_error(&RunError::Empty), "请输入程序、文件夹、文档或网址");
        assert!(run_error(&RunError::Cancelled).contains("UAC"));
        let not_found = "找不到指定的程序、文件、文件夹或网址，请检查输入是否正确";
        assert_eq!(run_error(&RunError::System(SystemError::Win32(2))), not_found);
        assert_eq!(run_error(&RunError::System(SystemError::Win32(3))), not_found);
        let denied = run_error(&RunError::System(SystemError::Win32(5)));
        assert!(denied.starts_with("无法启动：") && denied.contains("5"), "{denied}");
        let other = SystemError::Other { code: -2147467259, message: "未指定的错误".into() };
        assert_eq!(run_error(&RunError::System(other)), "无法启动：未指定的错误");
        assert_eq!(browse_filters()[1], ("所有文件", "*.*"));
    }

    #[test]
    fn autostart_errors_match_the_original_text() {
        assert_eq!(
            autostart_error(&AutostartError::InvalidTestKey),
            r"TML_AUTOSTART_TEST_KEY 必须以 Software\TaskManagerLite-test\ 开头且其后不为空"
        );
        assert_eq!(
            autostart_error(&AutostartError::System(SystemError::Win32(5))),
            std::io::Error::from_raw_os_error(5).to_string()
        );
    }

    #[test]
    fn ifeo_errors_match_the_original_text() {
        let path = || PathBuf::from(r"C:\Tools\TaskManagerLite.exe");
        let sys = || SystemError::Other { code: 0, message: "系统消息".into() };
        let all = [
            (IfeoError::AccessDenied, "没有写入系统注册表的权限（需要管理员）".to_owned()),
            (IfeoError::Occupied("\"C:\\x.exe\"".into()), "已被其他程序登记：\"C:\\x.exe\"".to_owned()),
            (
                IfeoError::Occupied(DebuggerValue::NonString { value_type: 4 }),
                "已被其他程序登记：<非字符串值，类型 4>".to_owned(),
            ),
            (IfeoError::Cancelled, "已取消管理员授权".to_owned()),
            (
                IfeoError::UnsafeLocation(UnsafeReason::NoAcl { path: path() }),
                r"exe 所在位置可被其他用户改写（C:\Tools\TaskManagerLite.exe 没有访问控制（任何人可写））。请用安装包安装（默认装到 Program Files）或移到受保护目录，或使用 --force".to_owned(),
            ),
            (
                IfeoError::UnsafeLocation(UnsafeReason::BroadWrite { path: path(), who: "Everyone".into() }),
                r"exe 所在位置可被其他用户改写（C:\Tools\TaskManagerLite.exe 授予 Everyone 写入权限）。请用安装包安装（默认装到 Program Files）或移到受保护目录，或使用 --force".to_owned(),
            ),
            (
                IfeoError::UnsafeLocation(UnsafeReason::ElevatedCheckFailed),
                "exe 所在位置可被其他用户改写（提权子进程的位置检查未通过）。请用安装包安装（默认装到 Program Files）或移到受保护目录，或使用 --force".to_owned(),
            ),
            (IfeoError::Registry(87), "注册表操作失败（错误码 87）".to_owned()),
            (IfeoError::TestKeyNotUnicode, "TML_IFEO_TEST_KEY 不是合法的 Unicode".to_owned()),
            (
                IfeoError::TestKeyInvalid("bad".into()),
                r"TML_IFEO_TEST_KEY 必须以 Software\TaskManagerLite-test\ 开头且其后不为空：bad".to_owned(),
            ),
            (IfeoError::ExeNotFound(path()), r"找不到程序文件：C:\Tools\TaskManagerLite.exe".to_owned()),
            (IfeoError::CurrentExeUnavailable(sys()), "无法取得本程序路径：系统消息".to_owned()),
            (IfeoError::ElevationLaunchFailed(sys()), "无法启动提权子进程：系统消息".to_owned()),
            (IfeoError::ElevationNoHandle, "提权子进程没有返回句柄".to_owned()),
            (IfeoError::ElevationTimeout, "等待提权子进程超时，结果未知，请重新读取登记状态".to_owned()),
            (IfeoError::ElevationChildFailed(7), "提权子进程失败（退出码 7）".to_owned()),
            (
                IfeoError::AclUnreadable { path: path(), code: 5 },
                r"无法读取 C:\Tools\TaskManagerLite.exe 的访问控制列表（错误码 5）；确认安全后可使用 --force".to_owned(),
            ),
            (IfeoError::AclInfoFailed(sys()), "读取 ACL 信息失败：系统消息".to_owned()),
            (
                IfeoError::AceUnreadable { path: path(), index: 2 },
                r"无法读取 C:\Tools\TaskManagerLite.exe 的第 2 条访问控制项；确认安全后可使用 --force".to_owned(),
            ),
            (
                IfeoError::SidUnparseable { path: path(), index: 3 },
                r"无法解析 C:\Tools\TaskManagerLite.exe 的第 3 条访问控制项的 SID；确认安全后可使用 --force".to_owned(),
            ),
            (IfeoError::ThreadSpawnFailed(sys()), "无法创建后台线程：系统消息".to_owned()),
            (IfeoError::System(sys()), "系统消息".to_owned()),
        ];
        for (e, text) in all {
            assert_eq!(ifeo_error(&e), text, "{e:?}");
        }
    }

    #[test]
    fn unsafe_location_note_has_no_force_flag() {
        let why = UnsafeReason::BroadWrite { path: PathBuf::from(r"C:\Tools"), who: "BUILTIN\\Users".into() };
        let note = unsafe_location_note(&why);
        assert_eq!(
            note,
            r"程序所在位置可被其他用户改写（C:\Tools 授予 BUILTIN\Users 写入权限），为安全起见不予登记。请用安装包安装（默认装到 Program Files）后再试，或把程序移到受保护的目录"
        );
        assert!(!note.contains("--force"));
    }

    #[test]
    fn debugger_value_text_and_non_string() {
        assert_eq!(debugger_value(&DebuggerValue::Text("x.exe".into())), "x.exe");
        assert_eq!(debugger_value(&DebuggerValue::NonString { value_type: 3 }), "<非字符串值，类型 3>");
    }

    #[test]
    fn export_texts() {
        assert_eq!(group_name(tml_core::Group::Apps), "应用");
        assert_eq!(group_name(tml_core::Group::Background), "后台进程");
        assert_eq!(export_filters()[0], ("CSV 文件", "*.csv"));
        assert_eq!(perf_header().len(), 8);
        assert_eq!(perf_unit(PerfMetric::NetReceive), "字节/秒");
        assert_eq!(perf_metric(PerfMetric::CpuUsage), perf_metric(PerfMetric::GpuUsage));
        assert_eq!(
            export_write_error("a.csv", &SystemError::Win32(5)),
            format!("无法写入 a.csv：{}", std::io::Error::from_raw_os_error(5))
        );
    }
}

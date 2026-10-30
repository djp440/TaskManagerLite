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
use tml_core::services::{Action, ServiceError, ServiceState, StartType};
use tml_core::startup::{SourceError, StartupError, StartupSource};
use tml_core::{
    AutostartError, DebuggerValue, ElevationError, IfeoError, MediaType, ShellError, UnsafeReason,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tml_core::SystemError;

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
}

//! 接管 Ctrl+Shift+Esc：用映像文件执行选项（IFEO）的 `Debugger` 值把 `Taskmgr.exe` 重定向到本程序。
//! 设计取舍、提权方式、失效场景与还原办法见 `docs/adr/0003-replace-taskmgr-via-ifeo.md`。
//!
//! - 登记位置：`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe`
//!   下的字符串值 `Debugger`，内容为带引号的本程序路径，例如 `"C:\Tools\TaskManagerLite.exe"`。
//! - 写 HKLM 需要管理员权限：未提权时由一次性的提权子进程（本程序加 `--register-taskmgr` 等参数）完成，主进程不整体提权。
//! - 注册表根键与路径可注入（[`Ifeo::under`]），自动化测试只在 `HKCU\Software\TaskManagerLite-test\…` 下运行。
//!   环境变量 [`TEST_KEY_ENV`] 可让命令行开关改用这样的测试键，供端到端脚本使用。

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND,
    ERROR_SUCCESS, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HRESULT, HSTRING, PCWSTR, PWSTR, w};

/// 真实的 IFEO 键（相对 `HKEY_LOCAL_MACHINE`）。
const SYSTEM_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe";
const VALUE_NAME: &str = "Debugger";

/// 测试键必须位于此前缀（`HKCU`）之下：环境变量即使被设置，也无法把写入引到别处。
pub const TEST_KEY_PREFIX: &str = r"Software\TaskManagerLite-test\";
/// 设置后命令行开关改写 `HKCU\<该值>`（须以 [`TEST_KEY_PREFIX`] 开头），不碰 HKLM。仅用于端到端自动化测试。
pub const TEST_KEY_ENV: &str = "TML_IFEO_TEST_KEY";

pub const REGISTER_ARG: &str = "--register-taskmgr";
pub const UNREGISTER_ARG: &str = "--unregister-taskmgr";
pub const STATUS_ARG: &str = "--taskmgr-status";
pub const FORCE_ARG: &str = "--force";

/// 命令行退出码。登记/撤销：0 成功；其余为失败原因。`--taskmgr-status`：0/10/11/12。
pub mod exit {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const ACCESS_DENIED: i32 = 2;
    /// 已被其他程序（如 Process Explorer）登记，未加 `--force` 时拒绝覆盖。
    pub const OCCUPIED: i32 = 3;
    /// 用户在 UAC 提示中取消。
    pub const CANCELLED: i32 = 4;
    pub const NOT_REGISTERED: i32 = 10;
    pub const OTHER_EXE: i32 = 11;
    pub const OTHER_MISSING: i32 = 12;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfeoError {
    AccessDenied,
    /// 已被别的程序登记（附其 `Debugger` 原值）。
    Occupied(String),
    Cancelled,
    /// 其他注册表错误（Win32 错误码）。
    Registry(u32),
    Other(String),
}

impl fmt::Display for IfeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied => write!(f, "没有写入系统注册表的权限（需要管理员）"),
            Self::Occupied(v) => write!(f, "已被其他程序登记：{v}"),
            Self::Cancelled => write!(f, "已取消管理员授权"),
            Self::Registry(code) => write!(f, "注册表操作失败（错误码 {code}）"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for IfeoError {}

impl IfeoError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::AccessDenied => exit::ACCESS_DENIED,
            Self::Occupied(_) => exit::OCCUPIED,
            Self::Cancelled => exit::CANCELLED,
            Self::Registry(_) | Self::Other(_) => exit::FAILED,
        }
    }

    fn from_exit_code(code: u32) -> Self {
        match code as i32 {
            exit::ACCESS_DENIED => Self::AccessDenied,
            exit::OCCUPIED => Self::Occupied(String::new()),
            exit::CANCELLED => Self::Cancelled,
            c => Self::Other(format!("提权子进程失败（退出码 {c}）")),
        }
    }
}

fn from_win32(e: WIN32_ERROR) -> IfeoError {
    if e == ERROR_ACCESS_DENIED { IfeoError::AccessDenied } else { IfeoError::Registry(e.0) }
}

/// `Taskmgr.exe` 键下 `Debugger` 值的登记情况（相对某个 exe 路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    NotRegistered,
    /// 已指向给定的 exe。
    ThisExe,
    /// 指向别的路径：`exists` 为 false 表示路径已失效（exe 被移动或删除，或其他工具已卸载）。
    OtherExe { debugger: String, exists: bool },
}

impl Registration {
    /// 可安全替换或撤销而不影响别的程序：未登记、已是本 exe、或指向的文件已不存在。
    pub fn is_replaceable(&self) -> bool {
        !matches!(self, Self::OtherExe { exists: true, .. })
    }

    /// 命令行 `--taskmgr-status` 的退出码。
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ThisExe => exit::OK,
            Self::NotRegistered => exit::NOT_REGISTERED,
            Self::OtherExe { exists: true, .. } => exit::OTHER_EXE,
            Self::OtherExe { exists: false, .. } => exit::OTHER_MISSING,
        }
    }
}

/// `Debugger` 值的写入格式：带引号的路径。路径含空格时不加引号会解析出错。
pub fn debugger_value(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// 从 `Debugger` 值里取出 exe 路径：带引号取引号内；不带引号取到第一个 `.exe`（整体没有则取全部）。
/// 值里可能带有调试器参数（如 `-p %d`），这里只关心程序本身。
pub fn debugger_exe(value: &str) -> Option<PathBuf> {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('"') {
        let end = rest.find('"').unwrap_or(rest.len());
        let p = rest[..end].trim();
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    let lower = v.to_ascii_lowercase();
    let end = lower.find(".exe").map_or(v.len(), |i| i + 4);
    let p = v[..end].trim();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

/// 路径是否指向同一文件：先比较规范化后的文本（忽略大小写、`/`、`\\?\` 前缀、结尾反斜杠），不同时再比较已存在文件的规范路径。
pub fn same_path(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let s = p.to_string_lossy().replace('/', "\\");
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        s.trim_end_matches('\\').to_lowercase()
    }
    if norm(a) == norm(b) {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => norm(&x) == norm(&y),
        _ => false,
    }
}

/// 根据 `Debugger` 原值（`None` 表示没有该值）判定登记情况。纯函数，便于单测。
pub fn classify(value: Option<&str>, exe: &Path) -> Registration {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Registration::NotRegistered };
    match debugger_exe(value) {
        Some(p) if same_path(&p, exe) => Registration::ThisExe,
        Some(p) => Registration::OtherExe { debugger: value.to_owned(), exists: p.exists() },
        None => Registration::OtherExe { debugger: value.to_owned(), exists: false },
    }
}

/// 本次启动是否由 IFEO 重定向而来：系统把原命令行接在 `Debugger` 值之后，所以第一个参数是 `taskmgr.exe` 的路径
/// （之后可能还有 `/0` 等页签参数）。这些参数一律忽略，进程列表的行为不变。
pub fn launched_as_taskmgr_debugger(args: &[String]) -> bool {
    args.get(1).is_some_and(|a| {
        let a = a.trim().trim_matches('"').replace('/', "\\");
        Path::new(&a).file_name().is_some_and(|n| n.eq_ignore_ascii_case(OsStr::new("taskmgr.exe")))
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// 一个 IFEO 键的位置（根键 + 子路径），根键与路径可注入。
#[derive(Clone)]
pub struct Ifeo {
    root: HKEY,
    subkey: String,
}

impl Ifeo {
    /// 真实系统的 `Taskmgr.exe` IFEO 键（HKLM）。
    pub fn system() -> Self {
        Self { root: HKEY_LOCAL_MACHINE, subkey: SYSTEM_KEY.into() }
    }

    /// 任意根键与子路径，供测试使用。
    pub fn under(root: HKEY, subkey: impl Into<String>) -> Self {
        Self { root, subkey: subkey.into() }
    }

    /// 命令行开关使用的位置：设置了 [`TEST_KEY_ENV`] 则用 `HKCU` 下的测试键（值不合法时报错，而不是退回真实键）。
    pub fn from_env() -> Result<Self, IfeoError> {
        match std::env::var(TEST_KEY_ENV) {
            Ok(k) if k.starts_with(TEST_KEY_PREFIX) && !k.contains("..") => Ok(Self::under(HKEY_CURRENT_USER, k)),
            Ok(k) => Err(IfeoError::Other(format!("{TEST_KEY_ENV} 必须以 {TEST_KEY_PREFIX} 开头：{k}"))),
            Err(_) => Ok(Self::system()),
        }
    }

    pub fn is_system(&self) -> bool {
        self.root == HKEY_LOCAL_MACHINE
    }

    /// 读 `Debugger` 值；键或值不存在返回 `Ok(None)`。始终访问 64 位视图。
    pub fn debugger(&self) -> Result<Option<String>, IfeoError> {
        let mut key = HKEY::default();
        let sub = wide(&self.subkey);
        let e = unsafe { RegOpenKeyExW(self.root, PCWSTR(sub.as_ptr()), None, KEY_READ | KEY_WOW64_64KEY, &mut key) };
        if e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        let key = Key(key);
        let name = wide(VALUE_NAME);
        let (mut ty, mut bytes) = (REG_VALUE_TYPE(0), 0u32);
        let e = unsafe { RegQueryValueExW(key.0, PCWSTR(name.as_ptr()), None, Some(&mut ty), None, Some(&mut bytes)) };
        if e == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
        let mut bytes = (buf.len() * 2) as u32;
        let e = unsafe {
            RegQueryValueExW(key.0, PCWSTR(name.as_ptr()), None, Some(&mut ty), Some(buf.as_mut_ptr().cast()), Some(&mut bytes))
        };
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        // REG_SZ / REG_EXPAND_SZ 之外的类型当作没有登记（环境变量不展开，路径含 %…% 时会判为“其他程序”）。
        if ty != REG_SZ && ty.0 != 2 {
            return Ok(None);
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(Some(String::from_utf16_lossy(&buf[..len])))
    }

    /// 相对 `exe` 的登记情况。
    pub fn status(&self, exe: &Path) -> Result<Registration, IfeoError> {
        Ok(classify(self.debugger()?.as_deref(), exe))
    }

    /// 登记：写入 `Debugger = "<exe>"`。已指向 `exe` 时不写；被其他有效程序占用且未 `force` 时拒绝。
    /// 无写权限返回 [`IfeoError::AccessDenied`]（调用方决定是否提权）。
    pub fn register(&self, exe: &Path, force: bool) -> Result<(), IfeoError> {
        match self.status(exe)? {
            Registration::ThisExe => return Ok(()),
            Registration::OtherExe { debugger, exists: true } if !force => return Err(IfeoError::Occupied(debugger)),
            _ => {}
        }
        if !exe.is_file() {
            return Err(IfeoError::Other(format!("找不到程序文件：{}", exe.display())));
        }
        let mut key = HKEY::default();
        let sub = wide(&self.subkey);
        let e = unsafe {
            RegCreateKeyExW(
                self.root,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                None,
                &mut key,
                None,
            )
        };
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        let key = Key(key);
        let data: Vec<u8> = wide(&debugger_value(exe)).iter().flat_map(|c| c.to_le_bytes()).collect();
        let name = wide(VALUE_NAME);
        let e = unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)) };
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        Ok(())
    }

    /// 撤销：删除 `Debugger` 值；键里没有别的值时一并删掉空键。
    /// 指向别的有效程序且未 `force` 时拒绝（不动别人的登记）；本来就没登记则什么都不做。
    pub fn unregister(&self, exe: &Path, force: bool) -> Result<(), IfeoError> {
        match self.status(exe)? {
            Registration::NotRegistered => return Ok(()),
            Registration::OtherExe { debugger, exists: true } if !force => return Err(IfeoError::Occupied(debugger)),
            _ => {}
        }
        let mut key = HKEY::default();
        let sub = wide(&self.subkey);
        let e = unsafe {
            RegOpenKeyExW(self.root, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, &mut key)
        };
        if e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND {
            return Ok(());
        }
        if e != ERROR_SUCCESS {
            return Err(from_win32(e));
        }
        let key = Key(key);
        let name = wide(VALUE_NAME);
        let e = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if e != ERROR_SUCCESS && e != ERROR_FILE_NOT_FOUND {
            return Err(from_win32(e));
        }
        // 键里还有别的值就保留；没有则删空键（有子键时删除会失败，忽略）。
        let mut vname = [0u16; 16];
        let mut len = vname.len() as u32;
        let more = unsafe { RegEnumValueW(key.0, 0, Some(PWSTR(vname.as_mut_ptr())), &mut len, None, None, None, None) };
        drop(key);
        if more == ERROR_NO_MORE_ITEMS {
            unsafe {
                let _ = RegDeleteKeyExW(self.root, PCWSTR(sub.as_ptr()), KEY_WOW64_64KEY.0, None);
            }
        }
        Ok(())
    }
}

/// 登记或撤销。需要写系统键而当前未提权时，启动一次性的提权子进程（`--register-taskmgr` / `--unregister-taskmgr`）
/// 完成并等待其结果；用户在 UAC 中取消返回 [`IfeoError::Cancelled`]。会阻塞到子进程结束，界面应放到后台线程调用。
pub fn set_replace_taskmgr(enable: bool, force: bool) -> Result<(), IfeoError> {
    let ifeo = Ifeo::from_env()?;
    let exe = std::env::current_exe().map_err(|e| IfeoError::Other(format!("无法取得本程序路径：{e}")))?;
    let local = if enable { ifeo.register(&exe, force) } else { ifeo.unregister(&exe, force) };
    match local {
        Err(IfeoError::AccessDenied) if ifeo.is_system() && !crate::is_elevated() => run_elevated(enable, force, &exe),
        r => r,
    }
}

/// 以管理员身份运行本程序的开关并等待退出码（最多 60 秒）。
fn run_elevated(enable: bool, force: bool, exe: &Path) -> Result<(), IfeoError> {
    let file = HSTRING::from(exe.as_os_str());
    let mut args = (if enable { REGISTER_ARG } else { UNREGISTER_ARG }).to_owned();
    if force {
        args.push(' ');
        args.push_str(FORCE_ARG);
    }
    let args = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => {}
        Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Err(IfeoError::Cancelled),
        Err(e) => return Err(IfeoError::Other(format!("无法启动提权子进程：{}", e.message()))),
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return Err(IfeoError::Other("提权子进程没有返回句柄".into()));
    }
    let result = unsafe {
        if WaitForSingleObject(process, 60_000) == WAIT_OBJECT_0 {
            let mut code = 1u32;
            GetExitCodeProcess(process, &mut code).map(|()| code).map_err(|e| IfeoError::Other(e.message()))
        } else {
            Err(IfeoError::Other("等待提权子进程超时".into()))
        }
    };
    unsafe {
        let _ = CloseHandle(process);
    }
    match result? {
        0 => Ok(()),
        c => Err(IfeoError::from_exit_code(c)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Register { force: bool },
    Unregister { force: bool },
    Status,
}

/// 解析命令行（含程序名在内的完整参数）。没有相关开关返回 `None`。
pub fn parse_cli(args: &[String]) -> Option<Command> {
    let has = |flag: &str| args.iter().skip(1).any(|a| a == flag);
    let force = has(FORCE_ARG);
    if has(REGISTER_ARG) {
        Some(Command::Register { force })
    } else if has(UNREGISTER_ARG) {
        Some(Command::Unregister { force })
    } else if has(STATUS_ARG) {
        Some(Command::Status)
    } else {
        None
    }
}

/// 执行命令行开关并返回进程退出码；参数里没有相关开关返回 `None`（正常启动界面）。
pub fn run_cli(args: &[String]) -> Option<i32> {
    let cmd = parse_cli(args)?;
    Some(match cmd {
        Command::Register { force } => result_code(set_replace_taskmgr(true, force)),
        Command::Unregister { force } => result_code(set_replace_taskmgr(false, force)),
        Command::Status => match Ifeo::from_env().and_then(|i| std::env::current_exe().map_err(|e| IfeoError::Other(e.to_string())).and_then(|exe| i.status(&exe))) {
            Ok(r) => r.exit_code(),
            Err(e) => e.exit_code(),
        },
    })
}

fn result_code(r: Result<(), IfeoError>) -> i32 {
    r.map_or_else(|e| e.exit_code(), |()| exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 写入值带引号() {
        assert_eq!(debugger_value(Path::new(r"C:\Program Files\TML\TaskManagerLite.exe")), r#""C:\Program Files\TML\TaskManagerLite.exe""#);
    }

    #[test]
    fn 解析调试器值里的程序路径() {
        let p = |s: &str| debugger_exe(s).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(p(r#""C:\a b\x.exe""#).as_deref(), Some(r"C:\a b\x.exe"));
        assert_eq!(p(r#""C:\a b\x.exe" -p %ld -e %ld"#).as_deref(), Some(r"C:\a b\x.exe"));
        assert_eq!(p(r"C:\Program Files\x.EXE -p 1").as_deref(), Some(r"C:\Program Files\x.EXE"));
        assert_eq!(p(r"C:\x\tool").as_deref(), Some(r"C:\x\tool"));
        assert_eq!(p("  ").as_deref(), None);
        assert_eq!(p(r#""""#).as_deref(), None);
    }

    #[test]
    fn 路径比较忽略大小写与前缀() {
        assert!(same_path(Path::new(r"C:\Tools\TML.exe"), Path::new(r"c:\tools\tml.EXE")));
        assert!(same_path(Path::new(r"\\?\C:\Tools\TML.exe"), Path::new(r"C:\Tools\TML.exe")));
        assert!(same_path(Path::new("C:/Tools/TML.exe"), Path::new(r"C:\Tools\TML.exe")));
        assert!(!same_path(Path::new(r"C:\Tools\TML.exe"), Path::new(r"C:\Other\TML.exe")));
    }

    #[test]
    fn 登记情况判定() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(classify(None, &exe), Registration::NotRegistered);
        assert_eq!(classify(Some("  "), &exe), Registration::NotRegistered);
        assert_eq!(classify(Some(&debugger_value(&exe)), &exe), Registration::ThisExe);
        // 带调试器参数、不带引号的写法也认得出是自己。
        assert_eq!(classify(Some(&format!("{} -p 1", exe.display())), &exe), Registration::ThisExe);
        let gone = r#""C:\definitely\not\here\TaskManagerLite.exe""#;
        assert_eq!(classify(Some(gone), &exe), Registration::OtherExe { debugger: gone.into(), exists: false });
        // 指向存在的其他文件（这里用 cmd.exe）。
        let cmd = format!("\"{}\\System32\\cmd.exe\"", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
        assert!(matches!(classify(Some(&cmd), &exe), Registration::OtherExe { exists: true, .. }));
    }

    #[test]
    fn 失效或自己的登记可替换_别人的有效登记不可() {
        assert!(Registration::NotRegistered.is_replaceable());
        assert!(Registration::ThisExe.is_replaceable());
        assert!(Registration::OtherExe { debugger: "x".into(), exists: false }.is_replaceable());
        assert!(!Registration::OtherExe { debugger: "x".into(), exists: true }.is_replaceable());
    }

    #[test]
    fn 识别被_ifeo_启动() {
        // 系统把原命令行接在 Debugger 值后面：argv[1] 是 taskmgr.exe 的路径，其后可能有页签参数。
        assert!(launched_as_taskmgr_debugger(&args(&[r"C:\Tools\TaskManagerLite.exe", r"C:\WINDOWS\system32\taskmgr.exe"])));
        assert!(launched_as_taskmgr_debugger(&args(&["tml.exe", r#""C:\Windows\System32\Taskmgr.exe""#, "/2"])));
        assert!(launched_as_taskmgr_debugger(&args(&["tml.exe", "TASKMGR.EXE"])));
        assert!(!launched_as_taskmgr_debugger(&args(&["tml.exe"])));
        assert!(!launched_as_taskmgr_debugger(&args(&["tml.exe", "--allow-multiple", r"C:\x\taskmgr.exe"])));
        assert!(!launched_as_taskmgr_debugger(&args(&["tml.exe", r"C:\x\taskmgr.exe.bak"])));
    }

    #[test]
    fn 被_ifeo_启动时的附加参数不触发命令行开关() {
        // 被重定向启动时的参数里没有本程序的开关：正常启动界面。
        assert_eq!(parse_cli(&args(&["tml.exe", r"C:\WINDOWS\system32\taskmgr.exe", "/0"])), None);
    }

    #[test]
    fn 命令行开关解析() {
        assert_eq!(parse_cli(&args(&["tml.exe"])), None);
        assert_eq!(parse_cli(&args(&["tml.exe", "--allow-multiple"])), None);
        assert_eq!(parse_cli(&args(&["tml.exe", "--register-taskmgr"])), Some(Command::Register { force: false }));
        assert_eq!(parse_cli(&args(&["tml.exe", "--register-taskmgr", "--force"])), Some(Command::Register { force: true }));
        assert_eq!(parse_cli(&args(&["tml.exe", "--force", "--unregister-taskmgr"])), Some(Command::Unregister { force: true }));
        assert_eq!(parse_cli(&args(&["tml.exe", "--taskmgr-status"])), Some(Command::Status));
        // 程序名本身（第 0 项）不当作开关。
        assert_eq!(parse_cli(&args(&["--register-taskmgr"])), None);
    }

    #[test]
    fn 错误与退出码互转() {
        assert_eq!(IfeoError::AccessDenied.exit_code(), exit::ACCESS_DENIED);
        assert_eq!(IfeoError::Cancelled.exit_code(), exit::CANCELLED);
        assert_eq!(IfeoError::Occupied("x".into()).exit_code(), exit::OCCUPIED);
        assert_eq!(IfeoError::Registry(5).exit_code(), exit::FAILED);
        assert_eq!(IfeoError::from_exit_code(exit::CANCELLED as u32), IfeoError::Cancelled);
        assert_eq!(IfeoError::from_exit_code(exit::ACCESS_DENIED as u32), IfeoError::AccessDenied);
        assert!(matches!(IfeoError::from_exit_code(1), IfeoError::Other(_)));
    }

    #[test]
    fn 环境变量只接受_hkcu_测试前缀() {
        // 环境变量是进程级共享状态：仅在本测试中读写。
        unsafe { std::env::set_var(TEST_KEY_ENV, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe") };
        assert!(Ifeo::from_env().is_err());
        unsafe { std::env::set_var(TEST_KEY_ENV, r"Software\TaskManagerLite-test\..\..\x") };
        assert!(Ifeo::from_env().is_err());
        unsafe { std::env::set_var(TEST_KEY_ENV, r"Software\TaskManagerLite-test\abc\Taskmgr.exe") };
        let i = Ifeo::from_env().unwrap();
        assert!(!i.is_system());
        unsafe { std::env::remove_var(TEST_KEY_ENV) };
        assert!(Ifeo::from_env().unwrap().is_system());
    }
}

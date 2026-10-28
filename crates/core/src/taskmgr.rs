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
    ERROR_SUCCESS, HLOCAL, LocalFree, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, PSECURITY_DESCRIPTOR, PSID};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE,
    REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW,
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
/// 提权子进程专用：带它的进程绝不再次尝试提权（防止提权失败时无限套娃）。
pub const ELEVATED_CHILD_ARG: &str = "--elevated-child";

/// 命令行退出码。登记/撤销：0 成功；其余为失败原因。`--taskmgr-status`：0/10/11/12。
pub mod exit {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const ACCESS_DENIED: i32 = 2;
    /// 已被其他程序（如 Process Explorer）登记，未加 `--force` 时拒绝覆盖。
    pub const OCCUPIED: i32 = 3;
    /// 用户在 UAC 提示中取消。
    pub const CANCELLED: i32 = 4;
    /// exe 或其所在目录可被 Everyone / Authenticated Users / Users 写入，未加 `--force` 时拒绝登记。
    pub const UNSAFE_LOCATION: i32 = 5;
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
    /// exe 所在位置可被其他用户改写（附说明）。
    UnsafeLocation(String),
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
            Self::UnsafeLocation(why) => {
                write!(f, "exe 所在位置可被其他用户改写（{why}）。请移到 Program Files 等受保护目录，或使用 --force")
            }
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
            Self::UnsafeLocation(_) => exit::UNSAFE_LOCATION,
            Self::Registry(_) | Self::Other(_) => exit::FAILED,
        }
    }

    fn from_exit_code(code: u32) -> Self {
        match code as i32 {
            exit::ACCESS_DENIED => Self::AccessDenied,
            exit::OCCUPIED => Self::Occupied(String::new()),
            exit::CANCELLED => Self::Cancelled,
            exit::UNSAFE_LOCATION => Self::UnsafeLocation("提权子进程的位置检查未通过".into()),
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
    classify_with(value, exe, expand_env)
}

/// 同 [`classify`]，环境变量展开函数可注入。值含 `%` 时先展开再判断：展开失败按“被占用”处理（`exists: true`，需要 `--force`），
/// 不能因为读不懂就当成失效而静默覆盖别人的登记。
pub fn classify_with(value: Option<&str>, exe: &Path, expand: impl Fn(&str) -> Option<String>) -> Registration {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else { return Registration::NotRegistered };
    let occupied = || Registration::OtherExe { debugger: value.to_owned(), exists: true };
    let expanded;
    let text = if value.contains('%') {
        match expand(value) {
            Some(e) => {
                expanded = e;
                expanded.as_str()
            }
            None => return occupied(),
        }
    } else {
        value
    };
    match debugger_exe(text) {
        Some(p) if same_path(&p, exe) => Registration::ThisExe,
        Some(p) => Registration::OtherExe { debugger: value.to_owned(), exists: p.exists() },
        None => Registration::OtherExe { debugger: value.to_owned(), exists: false },
    }
}

/// 展开 `%VAR%`。未定义的变量系统会原样保留；API 失败返回 `None`。
pub fn expand_env(value: &str) -> Option<String> {
    let src = wide(value);
    let need = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), None) };
    if need == 0 {
        return None;
    }
    let mut buf = vec![0u16; need as usize];
    let got = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) };
    if got == 0 || got as usize > buf.len() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
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

enum RawValue {
    Text(String),
    /// 非字符串类型的注册表值类型号。
    Other(u32),
}

fn non_string_text(ty: u32) -> String {
    format!("<非字符串值，类型 {ty}>")
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
        Self::from_env_value(std::env::var(TEST_KEY_ENV))
    }

    /// [`Self::from_env`] 的纯函数部分：只有变量不存在才用真实键；存在但不是合法的测试键（含非 Unicode）一律报错。
    fn from_env_value(v: Result<String, std::env::VarError>) -> Result<Self, IfeoError> {
        match v {
            Err(std::env::VarError::NotPresent) => Ok(Self::system()),
            Err(std::env::VarError::NotUnicode(_)) => Err(IfeoError::Other(format!("{TEST_KEY_ENV} 不是合法的 Unicode"))),
            // 前缀之后至少要有一个字符，不能直接写在 TaskManagerLite-test 键本身上。
            Ok(k) if k.len() > TEST_KEY_PREFIX.len() && k.starts_with(TEST_KEY_PREFIX) && !k.contains("..") => {
                Ok(Self::under(HKEY_CURRENT_USER, k))
            }
            Ok(k) => Err(IfeoError::Other(format!("{TEST_KEY_ENV} 必须以 {TEST_KEY_PREFIX} 开头且其后不为空：{k}"))),
        }
    }

    pub fn is_system(&self) -> bool {
        self.root == HKEY_LOCAL_MACHINE
    }

    /// 读 `Debugger` 值的文本；键或值不存在返回 `Ok(None)`。始终访问 64 位视图。
    /// 值不是字符串类型时返回一段描述文字（`<非字符串值，类型 N>`），而不是 `None`：它是一个存在的登记。
    pub fn debugger(&self) -> Result<Option<String>, IfeoError> {
        Ok(self.read_raw()?.map(|r| match r {
            RawValue::Text(t) => t,
            RawValue::Other(ty) => non_string_text(ty),
        }))
    }

    fn read_raw(&self) -> Result<Option<RawValue>, IfeoError> {
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
        // REG_SZ / REG_EXPAND_SZ 之外的类型（DWORD、二进制等）不是我们写的，也读不懂：当作别人的登记。
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            return Ok(Some(RawValue::Other(ty.0)));
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(Some(RawValue::Text(String::from_utf16_lossy(&buf[..len]))))
    }

    /// 相对 `exe` 的登记情况。值含 `%` 时会展开环境变量；非字符串值、展开失败都按“被占用”处理，
    /// 所以 `register` / `unregister` 不加 `force` 时不会静默覆盖或删除它们。
    pub fn status(&self, exe: &Path) -> Result<Registration, IfeoError> {
        Ok(match self.read_raw()? {
            None => Registration::NotRegistered,
            Some(RawValue::Text(t)) => classify(Some(&t), exe),
            Some(RawValue::Other(ty)) => Registration::OtherExe { debugger: non_string_text(ty), exists: true },
        })
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
///
/// 登记前会检查 exe 及其目录的写权限（见 [`check_install_location`]），`force` 可绕过。
/// 等待提权子进程超时（60 秒）时返回错误，但子进程可能随后仍然完成：**调用方不能把超时当作失败，
/// 应重新读取注册表（`Ifeo::system().status(..)`）来确定当前状态。**
pub fn set_replace_taskmgr(enable: bool, force: bool) -> Result<(), IfeoError> {
    apply(enable, force, true)
}

/// `allow_elevation` 为 false（提权子进程）时绝不再次提权：拿不到写权限就直接报 `AccessDenied`。
fn apply(enable: bool, force: bool, allow_elevation: bool) -> Result<(), IfeoError> {
    let ifeo = Ifeo::from_env()?;
    let exe = std::env::current_exe().map_err(|e| IfeoError::Other(format!("无法取得本程序路径：{e}")))?;
    // 位置检查在真正写入的进程里做（提权子进程同样会走到这里），用户也能在不弹 UAC 的情况下先看到拒绝原因。
    if enable && !force {
        check_install_location(&exe)?;
    }
    let local = if enable { ifeo.register(&exe, force) } else { ifeo.unregister(&exe, force) };
    match local {
        Err(IfeoError::AccessDenied) if allow_elevation && ifeo.is_system() && !crate::is_elevated() => {
            run_elevated(enable, force, &exe)
        }
        r => r,
    }
}

/// 后台线程里调用 `ShellExecuteEx` 前需要 COM 单线程套间；已经以别的模式初始化过则沿用，不重复反初始化。
struct ComScope(bool);

impl ComScope {
    fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        // S_OK 与 S_FALSE（本线程已初始化）都要配对一次 CoUninitialize；RPC_E_CHANGED_MODE 表示失败，不能配对。
        Self(hr.is_ok() && hr != RPC_E_CHANGED_MODE)
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// 以管理员身份运行本程序的开关并等待退出码（最多 60 秒）。
fn run_elevated(enable: bool, force: bool, exe: &Path) -> Result<(), IfeoError> {
    let _com = ComScope::init();
    let file = HSTRING::from(exe.as_os_str());
    let mut args = format!("{} {ELEVATED_CHILD_ARG}", if enable { REGISTER_ARG } else { UNREGISTER_ARG });
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
            Err(IfeoError::Other("等待提权子进程超时，结果未知，请重新读取登记状态".into()))
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

/// 一条“允许”类 ACE（拒绝类 ACE 不参与判定，保守起见只看授予了什么）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    /// 字符串形式的 SID，例如 `S-1-1-0`。
    pub sid: String,
    pub mask: u32,
    /// 仅对子对象生效（`INHERIT_ONLY_ACE`），不影响对象本身。
    pub inherit_only: bool,
}

/// 被视为“普通用户也能改写”的主体：Everyone、Authenticated Users、BUILTIN\Users。
const BROAD_GROUPS: [(&str, &str); 3] = [("S-1-1-0", "Everyone"), ("S-1-5-11", "Authenticated Users"), ("S-1-5-32-545", "BUILTIN\\Users")];

/// 能改写文件内容或其权限的访问掩码：FILE_WRITE_DATA(0x2，目录上是创建文件)、FILE_APPEND_DATA(0x4，目录上是创建子目录)、
/// FILE_DELETE_CHILD(0x40，目录上可删除其中的 exe)、DELETE、WRITE_DAC、WRITE_OWNER、GENERIC_WRITE、GENERIC_ALL。
const WRITE_MASK: u32 = 0x2 | 0x4 | 0x40 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;

/// 在一个对象的“允许”ACE 里找出授予了写类权限的宽泛主体，返回其名称。纯函数。
pub fn broad_write_grantee(aces: &[Ace]) -> Option<&'static str> {
    aces.iter().filter(|a| !a.inherit_only && a.mask & WRITE_MASK != 0).find_map(|a| {
        BROAD_GROUPS.iter().find(|(sid, _)| a.sid.eq_ignore_ascii_case(sid)).map(|(_, name)| *name)
    })
}

/// 读取路径的 DACL 里的“允许”ACE。`Ok(None)` 表示空 DACL（任何人完全访问）。
fn read_allow_aces(path: &Path) -> Result<Option<Vec<Ace>>, IfeoError> {
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
    const INHERIT_ONLY_ACE: u8 = 0x08;
    let name = HSTRING::from(path.as_os_str());
    let (mut dacl, mut sd) = (std::ptr::null_mut::<ACL>(), PSECURITY_DESCRIPTOR::default());
    let e = unsafe { GetNamedSecurityInfoW(&name, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(&mut dacl), None, &mut sd) };
    if e != ERROR_SUCCESS {
        return Err(IfeoError::Other(format!("无法读取 {} 的访问控制列表（错误码 {}）；确认安全后可使用 --force", path.display(), e.0)));
    }
    let result = (|| unsafe {
        if dacl.is_null() {
            return Ok(None);
        }
        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(dacl, (&mut info as *mut ACL_SIZE_INFORMATION).cast(), size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation)
            .map_err(|e| IfeoError::Other(format!("读取 ACL 信息失败：{}", e.message())))?;
        let mut out = Vec::new();
        for i in 0..info.AceCount {
            let mut ace = std::ptr::null_mut();
            if GetAce(dacl, i, &mut ace).is_err() || ace.is_null() {
                continue;
            }
            // ACE 布局：Header{Type:u8, Flags:u8, Size:u16}, Mask:u32, Sid…（允许类与回调允许类相同）。
            let p = ace as *const u8;
            let (ty, flags) = (*p, *p.add(1));
            if ty != ACCESS_ALLOWED_ACE_TYPE && ty != ACCESS_ALLOWED_CALLBACK_ACE_TYPE {
                continue;
            }
            let mask = (p.add(4) as *const u32).read_unaligned();
            let mut s = windows::core::PWSTR::null();
            if ConvertSidToStringSidW(PSID(p.add(8) as *mut _), &mut s).is_err() {
                continue;
            }
            let sid = s.to_string().unwrap_or_default();
            let _ = LocalFree(Some(HLOCAL(s.0.cast())));
            out.push(Ace { sid, mask, inherit_only: flags & INHERIT_ONLY_ACE != 0 });
        }
        Ok(Some(out))
    })();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    result
}

/// 登记前的位置检查：exe 文件或其所在目录只要有“允许写入”的 ACE 授予 Everyone / Authenticated Users / Users，就拒绝
/// （[`IfeoError::UnsafeLocation`]）——否则普通用户或恶意软件可以替换 exe，在管理员下一次打开任务管理器时获得代码执行机会。
/// 空 DACL 同样拒绝；读不到 ACL 时报错而不是放行。`--force` 绕过。
pub fn check_install_location(exe: &Path) -> Result<(), IfeoError> {
    let mut targets = vec![exe.to_path_buf()];
    if let Some(dir) = exe.parent() {
        targets.push(dir.to_path_buf());
    }
    for t in targets {
        match read_allow_aces(&t)? {
            None => return Err(IfeoError::UnsafeLocation(format!("{} 没有访问控制（任何人可写）", t.display()))),
            Some(aces) => {
                if let Some(who) = broad_write_grantee(&aces) {
                    return Err(IfeoError::UnsafeLocation(format!("{} 授予 {who} 写入权限", t.display())));
                }
            }
        }
    }
    Ok(())
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
    let allow_elevation = !args.iter().skip(1).any(|a| a == ELEVATED_CHILD_ARG);
    Some(match cmd {
        Command::Register { force } => result_code(apply(true, force, allow_elevation)),
        Command::Unregister { force } => result_code(apply(false, force, allow_elevation)),
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
        use std::env::VarError;
        let v = |s: &str| Ifeo::from_env_value(Ok(s.to_owned()));
        assert!(v(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe").is_err());
        assert!(v(r"Software\TaskManagerLite-test\..\..\x").is_err());
        // 前缀之后必须还有名字；空值、只有前缀都不行。
        assert!(v("").is_err());
        assert!(v(TEST_KEY_PREFIX).is_err());
        assert!(v(r"Software\TaskManagerLite-test").is_err());
        assert!(v(r"software\taskmanagerlite-test\x").is_err(), "前缀区分严格匹配，大小写不同也拒绝，宁可误拒");
        let ok = v(r"Software\TaskManagerLite-test\abc\Taskmgr.exe").unwrap();
        assert!(!ok.is_system());
        assert!(!v(r"Software\TaskManagerLite-test\x").unwrap().is_system());
        // 只有变量不存在才用真实键；存在但不是 Unicode 必须报错，不能退回真实键。
        assert!(Ifeo::from_env_value(Err(VarError::NotPresent)).unwrap().is_system());
        assert!(Ifeo::from_env_value(Err(VarError::NotUnicode("x".into()))).is_err());
    }

    #[test]
    fn 值含百分号时先展开再判断_展开失败按被占用() {
        let exe = std::env::current_exe().unwrap();
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let cmd = Path::new(&sysroot).join(r"System32\cmd.exe");
        // 真实展开：%SystemRoot%\System32\cmd.exe 存在 -> 被他人有效占用。
        assert_eq!(
            classify(Some(r#""%SystemRoot%\System32\cmd.exe""#), &exe),
            Registration::OtherExe { debugger: r#""%SystemRoot%\System32\cmd.exe""#.into(), exists: true }
        );
        // 展开后恰好是本 exe。
        let fake = |_: &str| Some(exe.display().to_string());
        assert_eq!(classify_with(Some("%X%"), &exe, fake), Registration::ThisExe);
        // 展开后指向不存在的文件 -> 失效。
        let gone = |_: &str| Some(r"C:\definitely\not\here.exe".to_owned());
        assert_eq!(classify_with(Some("%X%"), &exe, gone), Registration::OtherExe { debugger: "%X%".into(), exists: false });
        // 展开失败 -> 按被占用处理，需要 --force。
        let r = classify_with(Some("%X%"), &exe, |_| None);
        assert_eq!(r, Registration::OtherExe { debugger: "%X%".into(), exists: true });
        assert!(!r.is_replaceable());
        // 展开函数只对含 % 的值调用。
        assert_eq!(classify_with(Some(&cmd.display().to_string()), &exe, |_| panic!("不应展开")).exit_code(), exit::OTHER_EXE);
        assert!(expand_env("%SystemRoot%").is_some_and(|s| !s.contains('%')));
    }

    fn ace(sid: &str, mask: u32) -> Ace {
        Ace { sid: sid.into(), mask, inherit_only: false }
    }

    #[test]
    fn 位置检查_宽泛主体的写权限被识别() {
        const READ_EXEC: u32 = 0x1200a9; // 读取和执行
        const MODIFY: u32 = 0x1301bf;
        const FULL: u32 = 0x1f01ff;
        // 只读的 Users + 完全控制的管理员/SYSTEM：安全（Program Files 的典型形态）。
        assert_eq!(broad_write_grantee(&[ace("S-1-5-32-544", FULL), ace("S-1-5-18", FULL), ace("S-1-5-32-545", READ_EXEC)]), None);
        assert_eq!(broad_write_grantee(&[ace("S-1-1-0", READ_EXEC)]), None);
        // 三个宽泛主体中任意一个有写权限都拒绝。
        assert_eq!(broad_write_grantee(&[ace("S-1-1-0", MODIFY)]), Some("Everyone"));
        assert_eq!(broad_write_grantee(&[ace("S-1-5-11", MODIFY)]), Some("Authenticated Users"));
        assert_eq!(broad_write_grantee(&[ace("S-1-5-32-545", FULL)]), Some("BUILTIN\\Users"));
        // 各类写权限单独出现也算。
        for m in [0x2, 0x4, 0x40, 0x1_0000, 0x4_0000, 0x8_0000, 0x4000_0000, 0x1000_0000] {
            assert_eq!(broad_write_grantee(&[ace("S-1-5-11", m)]), Some("Authenticated Users"), "掩码 {m:#x}");
        }
        // 与写无关的权限（读取属性、同步、执行）不算。
        assert_eq!(broad_write_grantee(&[ace("S-1-1-0", 0x1 | 0x20 | 0x80 | 0x20000 | 0x10_0000)]), None);
        // 其他主体（当前用户、CREATOR OWNER）有写权限不算。
        assert_eq!(broad_write_grantee(&[ace("S-1-5-21-1-2-3-1001", FULL), ace("S-1-3-0", FULL)]), None);
        // 只对子对象生效的继承项不影响对象本身。
        assert_eq!(broad_write_grantee(&[Ace { inherit_only: true, ..ace("S-1-1-0", FULL) }]), None);
        assert_eq!(broad_write_grantee(&[]), None);
    }

    #[test]
    fn 位置检查_真实目录的_acl_读取() {
        // 用户临时目录继承用户 profile 的 ACL，通常只授予本人、SYSTEM、管理员：应通过。
        // 如果本机把它改成了宽泛授权，这个断言反映的就是真实状态，无需断言通过——只验证读取本身可用。
        let dir = std::env::temp_dir().join(format!("tml-acl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let aces = read_allow_aces(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let aces = aces.expect("临时目录应有 DACL");
        assert!(!aces.is_empty());
        assert!(aces.iter().any(|a| a.sid == "S-1-5-18" || a.sid == "S-1-5-32-544"), "应含 SYSTEM 或管理员：{aces:?}");
        // 不存在的路径读取失败而不是放行。
        assert!(read_allow_aces(Path::new(r"C:\definitely\not\here.exe")).is_err());
        assert!(check_install_location(Path::new(r"C:\definitely\not\here.exe")).is_err());
    }

    #[test]
    fn 位置检查_显式授予_users_写权限的目录被拒绝() {
        let dir = std::env::temp_dir().join(format!("tml-acl-grant-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.exe");
        std::fs::write(&file, b"x").unwrap();
        let before = check_install_location(&file);
        // 只授予目录本身（不继承），文件的 ACL 不变：证明“所在目录”也在检查范围内。
        let granted = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/grant", "*S-1-5-32-545:M"])
            .output()
            .is_ok_and(|o| o.status.success());
        let after = check_install_location(&file);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(before.is_ok(), "用户临时目录默认应通过：{before:?}");
        assert!(granted, "icacls 授权失败");
        match after {
            Err(IfeoError::UnsafeLocation(why)) => assert!(why.contains("Users"), "{why}"),
            other => panic!("应因目录可被 Users 写入而拒绝：{other:?}"),
        }
    }

    #[test]
    fn 位置检查_windows_目录被写保护() {
        // C:\Windows\System32 下的 exe：Users 只有读取和执行 -> 通过。
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        check_install_location(&Path::new(&sysroot).join(r"System32\cmd.exe")).unwrap();
    }

    #[test]
    fn 提权子进程标志不影响开关解析() {
        let a = args(&["tml.exe", "--register-taskmgr", "--elevated-child", "--force"]);
        assert_eq!(parse_cli(&a), Some(Command::Register { force: true }));
        assert_eq!(IfeoError::UnsafeLocation("x".into()).exit_code(), exit::UNSAFE_LOCATION);
        assert!(matches!(IfeoError::from_exit_code(exit::UNSAFE_LOCATION as u32), IfeoError::UnsafeLocation(_)));
    }
}

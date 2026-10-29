//! 登录启动项。仅由启动项页按需调用，状态与 Explorer 的 StartupApproved 共用。
use std::{io, path::PathBuf};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND,
            ERROR_SUCCESS, WAIT_OBJECT_0, WIN32_ERROR,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
            },
            Registry::*,
            Threading::{GetExitCodeProcess, WaitForSingleObject},
        },
        UI::{
            Shell::{
                FOLDERID_CommonStartup, FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT,
                SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHGetKnownFolderPath,
                ShellExecuteExW, ShellLink,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{HSTRING, Interface, PCWSTR, w},
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

#[derive(Clone, Debug)]
pub struct Entry {
    pub source: u8,
    pub name: String,
    pub publisher: String,
    pub command: String,
    pub enabled: bool,
    /// 命令或快捷方式指向的可执行文件，用于取发布者和图标；解析不出时为 `None`。
    pub target: Option<PathBuf>,
}

impl Entry {
    pub fn needs_admin(&self) -> bool {
        self.source % 2 == 1
    }
    pub fn source_label(&self) -> &'static str {
        match self.source {
            0 => "HKCU · Run",
            1 => "HKLM · Run",
            2 => "HKCU · Run32",
            3 => "HKLM · Run32",
            4 => "启动文件夹 · 当前用户",
            _ => "启动文件夹 · 所有用户",
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn check(e: WIN32_ERROR) -> io::Result<()> {
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(e.0 as i32))
    }
}
fn missing(e: WIN32_ERROR) -> bool {
    e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn hive(source: u8) -> HKEY {
    if source % 2 == 1 {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    }
}
fn run_key(source: u8) -> &'static str {
    if source < 2 { RUN } else { RUN32 }
}
fn approved_key(source: u8) -> String {
    format!(
        r"{APPROVED}\{}",
        if source < 2 {
            "Run"
        } else if source < 4 {
            "Run32"
        } else {
            "StartupFolder"
        }
    )
}
fn open(root: HKEY, path: &str) -> io::Result<Option<Key>> {
    let path = wide(path);
    let mut key = HKEY::default();
    let e = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if missing(e) {
        return Ok(None);
    }
    check(e)?;
    Ok(Some(Key(key)))
}
fn read(root: HKEY, path: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
    let Some(key) = open(root, path)? else {
        return Ok(None);
    };
    let name = wide(name);
    let mut size = 0;
    let e = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        )
    };
    if missing(e) {
        return Ok(None);
    }
    check(e)?;
    let mut data = vec![0u8; size as usize];
    check(unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(data.as_mut_ptr()),
            Some(&mut size),
        )
    })?;
    data.truncate(size as usize);
    Ok(Some(data))
}
fn enabled(data: Option<&[u8]>) -> bool {
    !data.and_then(|v| v.first()).is_some_and(|b| b & 1 == 1)
}
fn state(source: u8, name: &str) -> io::Result<bool> {
    Ok(enabled(
        read(hive(source), &approved_key(source), name)?.as_deref(),
    ))
}

struct Com(bool);
impl Com {
    fn new() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}
fn folder(source: u8) -> io::Result<PathBuf> {
    let id = if source == 4 {
        &FOLDERID_Startup
    } else {
        &FOLDERID_CommonStartup
    };
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(io::Error::other)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0.cast()));
        Ok(PathBuf::from(s.map_err(io::Error::other)?))
    }
}
fn shortcut(path: &std::path::Path) -> Option<(PathBuf, String)> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist
            .Load(&HSTRING::from(path.as_os_str()), STGM_READ)
            .ok()?;
        let mut target = vec![0u16; 32768];
        link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        let mut args = vec![0u16; 32768];
        link.GetArguments(&mut args).ok()?;
        let text = |v: &[u16]| {
            String::from_utf16_lossy(&v[..v.iter().position(|c| *c == 0).unwrap_or(v.len())])
        };
        let target = text(&target);
        if target.is_empty() {
            return None;
        }
        Some((
            PathBuf::from(&target),
            format!("\"{target}\" {}", text(&args)).trim_end().into(),
        ))
    }
}
fn registry_entries(source: u8) -> io::Result<Vec<Entry>> {
    registry_entries_at(source, hive(source), run_key(source), &approved_key(source))
}
fn registry_entries_at(
    source: u8,
    root: HKEY,
    run: &str,
    approved: &str,
) -> io::Result<Vec<Entry>> {
    let Some(key) = open(root, run)? else {
        return Ok(vec![]);
    };
    let mut entries = vec![];
    let mut index = 0;
    loop {
        let mut name = vec![0u16; 16384];
        let mut len = name.len() as u32;
        let mut data = vec![0u8; 65536];
        let mut bytes = data.len() as u32;
        let mut ty = 0u32;
        let e = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                Some(&mut ty),
                Some(data.as_mut_ptr()),
                Some(&mut bytes),
            )
        };
        if e == ERROR_NO_MORE_ITEMS {
            break;
        }
        check(e)?;
        index += 1;
        if ty != REG_SZ.0 && ty != REG_EXPAND_SZ.0 {
            continue;
        }
        let name = String::from_utf16_lossy(&name[..len as usize]);
        let words: Vec<_> = data[..bytes as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let command = String::from_utf16_lossy(&words)
            .trim_end_matches('\0')
            .to_owned();
        let expanded = crate::taskmgr::expand_env(&command).unwrap_or_else(|| command.clone());
        let target = crate::taskmgr::debugger_exe(&expanded);
        let publisher = target
            .as_deref()
            .and_then(|p| crate::details::version_strings(p).1)
            .unwrap_or_default();
        entries.push(Entry {
            source,
            enabled: enabled(read(root, approved, &name)?.as_deref()),
            name,
            command,
            publisher,
            target,
        });
    }
    Ok(entries)
}
fn folder_entries(source: u8) -> io::Result<Vec<Entry>> {
    let dir = folder(source)?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = vec![];
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        if !item.file_type()?.is_file() {
            continue;
        }
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let (target, command) = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            shortcut(&path).unwrap_or_else(|| (path.clone(), path.display().to_string()))
        } else {
            (path.clone(), path.display().to_string())
        };
        entries.push(Entry {
            source,
            enabled: state(source, &name)?,
            name,
            command,
            publisher: crate::details::version_strings(&target)
                .1
                .unwrap_or_default(),
            target: Some(target),
        });
    }
    Ok(entries)
}

/// 各来源独立读取；部分失败时保留其他来源的结果并返回可见的提示。
pub fn list() -> (Vec<Entry>, Vec<String>) {
    let _com = Com::new();
    let mut entries = vec![];
    let mut errors = vec![];
    for source in 0..6 {
        match if source < 4 {
            registry_entries(source)
        } else {
            folder_entries(source)
        } {
            Ok(mut rows) => entries.append(&mut rows),
            Err(e) => errors.push(format!("来源 {source} 读取失败：{e}")),
        }
    }
    entries.sort_by_key(|e| (e.name.to_lowercase(), e.source));
    (entries, errors)
}

fn approval(on: bool) -> [u8; 12] {
    let mut data = [0; 12];
    data[0] = if on { 2 } else { 3 };
    if !on {
        let t =
            unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
        data[4..8].copy_from_slice(&t.dwLowDateTime.to_le_bytes());
        data[8..12].copy_from_slice(&t.dwHighDateTime.to_le_bytes());
    }
    data
}
fn write_state(root: HKEY, path: &str, name: &str, on: bool) -> io::Result<()> {
    let (path, name) = (wide(path), wide(name));
    let mut key = HKEY::default();
    check(unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(path.as_ptr()),
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &mut key,
            None,
        )
    })?;
    let key = Key(key);
    check(unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            Some(0),
            REG_BINARY,
            Some(&approval(on)),
        )
    })
}
fn set_direct(source: u8, name: &str, on: bool) -> io::Result<()> {
    if source > 5 || name.contains('\0') {
        return Err(io::Error::other("无效的启动项"));
    }
    let exists = if source < 4 {
        read(hive(source), run_key(source), name)?.is_some()
    } else {
        !name.contains(['\\', '/']) && folder(source)?.join(name).is_file()
    };
    if !exists {
        return Err(io::Error::other("启动项已不存在，请刷新列表"));
    }
    write_state(hive(source), &approved_key(source), name, on)
}
/// 系统级项通过固定参数的提权子进程写入；后台等待，不重启主窗口。
pub fn set(entry: &Entry, on: bool) -> io::Result<()> {
    if !entry.needs_admin() || crate::is_elevated() {
        return set_direct(entry.source, &entry.name, on);
    }
    run_elevated(&format!(
        "--startup-set {} {} {}",
        entry.source,
        u8::from(on),
        hex(&entry.name)
    ))
}
/// 名称按 UTF-8 字节转十六进制传给提权子进程，避免引号与空格转义问题。
pub(crate) fn hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn unhex(hex: &str) -> io::Result<String> {
    if !hex.is_ascii() || hex.len() % 2 != 0 {
        return Err(io::Error::other("名称编码错误"));
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    String::from_utf8(bytes).map_err(io::Error::other)
}
/// 以固定参数启动提权的本程序子进程并等待它退出；退出码非零时按 Win32 错误码返回。
pub(crate) fn run_elevated(args: &str) -> io::Result<()> {
    let _com = Com::new();
    let args = HSTRING::from(args);
    let file = HSTRING::from(std::env::current_exe()?.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(io::Error::other)?;
    if info.hProcess.is_invalid() {
        return Err(io::Error::other("提权进程没有返回句柄"));
    }
    let result = unsafe {
        if WaitForSingleObject(info.hProcess, 60_000) == WAIT_OBJECT_0 {
            let mut code = 1;
            GetExitCodeProcess(info.hProcess, &mut code)
                .map_err(io::Error::other)
                .and_then(|()| {
                    if code == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::from_raw_os_error(code as i32))
                    }
                })
        } else {
            Err(io::Error::other("等待提权超时，请刷新核对状态"))
        }
    };
    unsafe {
        let _ = CloseHandle(info.hProcess);
    }
    result
}
pub fn run_cli(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("--startup-set") {
        return None;
    }
    let result = (|| {
        if args.len() != 5 {
            return Err(io::Error::other("参数数量错误"));
        }
        let source: u8 = args[2].parse().map_err(io::Error::other)?;
        let on = match args[3].as_str() {
            "0" => false,
            "1" => true,
            _ => return Err(io::Error::other("状态错误")),
        };
        set_direct(source, &unhex(&args[4])?, on)
    })();
    Some(result.map_or_else(|e| e.raw_os_error().unwrap_or(1), |()| 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_states_and_routes() {
        for b in [2, 6] {
            assert!(enabled(Some(&[b])));
        }
        for b in [3, 7] {
            assert!(!enabled(Some(&[b])));
        }
        assert!(enabled(None));
        assert!(approved_key(3).ends_with("Run32"));
        assert!(approved_key(5).ends_with("StartupFolder"));
        assert_eq!(approval(true), [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(approval(false)[0], 3);
    }
    #[test]
    fn registry_roundtrip_preserves_original() {
        let path = format!(
            r"Software\TaskManagerLite-test\startup-{}",
            std::process::id()
        );
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wide(&self.0).as_ptr()));
                }
            }
        }
        let _cleanup = Cleanup(path.clone());
        let run = format!(r"{path}\Run");
        let approved = format!(r"{path}\Approved");
        let name = "中文测试";
        let command = wide(r#""C:\Windows\explorer.exe" /test"#);
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(wide(&run).as_ptr()),
                PCWSTR(wide(name).as_ptr()),
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        })
        .unwrap();
        let original = read(HKEY_CURRENT_USER, &run, name).unwrap();
        assert!(registry_entries_at(0, HKEY_CURRENT_USER, &run, &approved).unwrap()[0].enabled);
        write_state(HKEY_CURRENT_USER, &approved, name, false).unwrap();
        let data = read(HKEY_CURRENT_USER, &approved, name).unwrap().unwrap();
        assert_eq!(data.len(), 12);
        assert!(!enabled(Some(&data)));
        assert!(!registry_entries_at(0, HKEY_CURRENT_USER, &run, &approved).unwrap()[0].enabled);
        write_state(HKEY_CURRENT_USER, &approved, name, true).unwrap();
        let rows = registry_entries_at(0, HKEY_CURRENT_USER, &run, &approved).unwrap();
        assert!(rows[0].enabled);
        assert_eq!(rows[0].name, name);
        assert!(rows[0].command.contains("/test"));
        assert_eq!(original, read(HKEY_CURRENT_USER, &run, name).unwrap());
    }
    #[test]
    fn real_sources_can_be_read_without_writing() {
        let (entries, errors) = list();
        assert!(errors.is_empty(), "{errors:?}");
        eprintln!("读取 {} 个真实启动项", entries.len());
        assert!(entries.iter().all(|e| e.source <= 5));
    }
    #[test]
    fn invalid_cli_cannot_write() {
        for tail in [
            vec!["99", "0", "61"],
            vec!["0", "2", "61"],
            vec!["0", "0", "中文"],
            vec!["0", "0", "a"],
        ] {
            let args = [vec!["tml", "--startup-set"], tail]
                .concat()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_ne!(run_cli(&args), Some(0));
        }
    }
}

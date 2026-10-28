//! 开机启动（#16）：在 `HKCU\...\Run` 下登记本程序，登录时由资源管理器启动。只写当前用户的键，不需要提权。
//!
//! 系统设置的“启动应用”页（以及系统任务管理器的“启动”页）禁用某项时并不删除 Run 值，
//! 而是在 `StartupApproved\Run` 下写一个首字节为奇数的二进制值；读状态时一并考虑。

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::PCWSTR;

use crate::taskmgr::{TEST_KEY_PREFIX, debugger_exe, same_path};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE_NAME: &str = "TaskManagerLite";
/// 开机启动时附带的参数：程序据此决定是否直接隐藏到托盘。
pub const AUTOSTART_ARG: &str = "--autostart";
/// 设置后改写 `HKCU\<该值>\Run` 与 `HKCU\<该值>\Approved`（须以 [`TEST_KEY_PREFIX`] 开头），不碰真实的 Run 键。仅用于自动化测试。
pub const AUTOSTART_TEST_KEY_ENV: &str = "TML_AUTOSTART_TEST_KEY";

/// 开机启动的登记情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Autostart {
    Off,
    On,
    /// 登记的是另一个路径（例如 exe 被移动过）；开启时会改写成当前路径。
    OtherPath(String),
    /// 已登记，但在系统的“启动应用”里被禁用。
    DisabledBySystem,
}

impl Autostart {
    /// 界面上的开关状态：只有真正会随登录启动本程序时才算开启。
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On)
    }
}

/// Run 键与 StartupApproved 键的位置（均在 HKCU 下），可注入测试键。
#[derive(Debug, Clone)]
pub struct AutostartKey {
    run: String,
    approved: String,
}

impl AutostartKey {
    pub fn user() -> Self {
        Self { run: RUN_KEY.into(), approved: APPROVED_KEY.into() }
    }

    /// 设置了 [`AUTOSTART_TEST_KEY_ENV`] 且合法时用测试键，否则用真实位置。
    pub fn from_env() -> Self {
        match std::env::var(AUTOSTART_TEST_KEY_ENV) {
            Ok(k) if k.len() > TEST_KEY_PREFIX.len() && k.starts_with(TEST_KEY_PREFIX) && !k.contains("..") => {
                Self::under(&k)
            }
            _ => Self::user(),
        }
    }

    /// `HKCU\<base>\Run` 与 `HKCU\<base>\Approved`，供测试使用。
    pub fn under(base: &str) -> Self {
        let base = base.trim_end_matches('\\');
        Self { run: format!(r"{base}\Run"), approved: format!(r"{base}\Approved") }
    }

    pub fn status(&self, exe: &Path) -> std::io::Result<Autostart> {
        let Some(value) = read(&self.run, RRF_RT_REG_SZ.0)? else {
            return Ok(Autostart::Off);
        };
        let text = String::from_utf16_lossy(&value);
        let text = text.trim_end_matches('\0');
        if !debugger_exe(text).is_some_and(|p| same_path(&p, exe)) {
            return Ok(Autostart::OtherPath(text.to_owned()));
        }
        // 首字节为奇数（常见 03）表示被禁用；02、06 或不存在表示启用。
        let disabled =
            read(&self.approved, RRF_RT_REG_BINARY.0)?.and_then(|v| v.first().copied()).is_some_and(|w| w & 1 == 1);
        Ok(if disabled { Autostart::DisabledBySystem } else { Autostart::On })
    }

    /// 开启：写入 `"<exe>" --autostart`，并清掉系统里的禁用标记（用户在这里明确要开启）。关闭：删除两处的值。
    pub fn set(&self, exe: &Path, on: bool) -> std::io::Result<()> {
        if on {
            let value = wide(&format!("\"{}\" {AUTOSTART_ARG}", exe.display()));
            let (key, name) = (wide(&self.run), wide(VALUE_NAME));
            // SAFETY: 所有指针在调用期间有效；数据长度按字节计算，含结尾的 0。
            check(unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(key.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    REG_SZ.0,
                    Some(value.as_ptr().cast()),
                    (value.len() * 2) as u32,
                )
            })?;
        } else {
            delete(&self.run)?;
        }
        delete(&self.approved)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(e: WIN32_ERROR) -> std::io::Result<()> {
    if e == ERROR_SUCCESS { Ok(()) } else { Err(std::io::Error::from_raw_os_error(e.0 as i32)) }
}

fn missing(e: WIN32_ERROR) -> bool {
    e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND
}

/// 读 `HKCU\<subkey>` 下本程序的值（按 u16 返回原始数据）；键或值不存在返回 `None`。
fn read(subkey: &str, flags: u32) -> std::io::Result<Option<Vec<u16>>> {
    let (key, name) = (wide(subkey), wide(VALUE_NAME));
    let flags = windows::Win32::System::Registry::REG_ROUTINE_FLAGS(flags);
    let mut bytes = 0u32;
    // SAFETY: 只读查询，指针在调用期间有效。
    let e = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            flags,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if missing(e) {
        return Ok(None);
    }
    check(e)?;
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: 同上，缓冲区大小由 bytes 给出。
    let e = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if missing(e) {
        return Ok(None);
    }
    check(e)?;
    // 字节数为奇数的二进制值最后一个字节也要保留在 u16 里（首字节才是我们关心的）。
    buf.truncate((bytes as usize).div_ceil(2));
    Ok(Some(buf))
}

fn delete(subkey: &str) -> std::io::Result<()> {
    let (key, name) = (wide(subkey), wide(VALUE_NAME));
    // SAFETY: 指针在调用期间有效。
    let e = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr())) };
    if missing(e) { Ok(()) } else { check(e) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{REG_BINARY, RegDeleteTreeW};

    /// 每个测试一个独立的测试键，结束时整棵删除。
    struct TestKey(String, AutostartKey);

    impl TestKey {
        fn new(name: &str) -> Self {
            let base = format!(r"{TEST_KEY_PREFIX}autostart-{}-{name}", std::process::id());
            Self(base.clone(), AutostartKey::under(&base))
        }

        fn approve(&self, first: u8) {
            let (key, name) = (wide(&self.1.approved), wide(VALUE_NAME));
            let data = [first, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            check(unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(key.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    REG_BINARY.0,
                    Some(data.as_ptr().cast()),
                    data.len() as u32,
                )
            })
            .unwrap();
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let k = wide(&self.0);
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
                let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()));
            }
        }
    }

    #[test]
    fn 开启关闭往返() {
        let t = TestKey::new("roundtrip");
        let exe = Path::new(r"C:\Program Files\TML\TaskManagerLite.exe");
        assert_eq!(t.1.status(exe).unwrap(), Autostart::Off);
        t.1.set(exe, true).unwrap();
        assert_eq!(t.1.status(exe).unwrap(), Autostart::On);
        let raw = String::from_utf16_lossy(&read(&t.1.run, RRF_RT_REG_SZ.0).unwrap().unwrap());
        assert_eq!(raw.trim_end_matches('\0'), r#""C:\Program Files\TML\TaskManagerLite.exe" --autostart"#);
        t.1.set(exe, false).unwrap();
        assert_eq!(t.1.status(exe).unwrap(), Autostart::Off);
        // 已经关闭时再关闭不报错。
        t.1.set(exe, false).unwrap();
    }

    #[test]
    fn 其他路径与系统禁用() {
        let t = TestKey::new("other");
        let old = Path::new(r"D:\old\TaskManagerLite.exe");
        let exe = Path::new(r"C:\new\TaskManagerLite.exe");
        t.1.set(old, true).unwrap();
        assert!(matches!(t.1.status(exe).unwrap(), Autostart::OtherPath(p) if p.contains(r"D:\old")));
        t.1.set(exe, true).unwrap();
        t.approve(3);
        assert_eq!(t.1.status(exe).unwrap(), Autostart::DisabledBySystem);
        t.approve(2);
        assert_eq!(t.1.status(exe).unwrap(), Autostart::On);
        t.approve(3);
        // 在本程序里重新开启会清掉系统的禁用标记。
        t.1.set(exe, true).unwrap();
        assert_eq!(t.1.status(exe).unwrap(), Autostart::On);
    }
}

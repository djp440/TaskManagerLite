//! IFEO 登记的集成测试：只在 `HKCU\Software\TaskManagerLite-test\<随机名>` 下读写真实注册表，
//! 绝不触碰 HKLM 的真实 IFEO 键；每个测试结束（含 panic）都会删掉自己的测试键。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tml_core::{DebuggerValue, Ifeo, IfeoError, Registration, TEST_KEY_PREFIX, debugger_value};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, KEY_WOW64_64KEY, RegDeleteKeyExW, RegDeleteTreeW};
use windows::core::PCWSTR;

/// `Debugger` 值的字符串原文（非字符串类型为 `None`）。
fn text(v: Option<DebuggerValue>) -> Option<String> {
    v.and_then(|v| v.text().map(str::to_owned))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 使用测试键的测试串行执行：Drop 会在父键为空时删掉父键，与另一个测试在父键下建子键并发时，
/// 后者会遇到 ERROR_KEY_DELETED（1018）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 随机命名的测试键；Drop 时整棵删除。
struct TestKey {
    root: String,
    // 字段在 `Drop::drop` 执行完之后才释放：删除测试键期间仍持有锁。
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl TestKey {
    fn new(tag: &str) -> Self {
        // 前一个测试 panic 会毒化锁，这里不关心，照常继续。
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = format!("{TEST_KEY_PREFIX}{}-{nanos:x}-{tag}", std::process::id());
        assert!(root.starts_with(r"Software\TaskManagerLite-test\"));
        Self { root, _serial: serial }
    }

    /// 与真实键同构：`<随机名>\Taskmgr.exe`。
    fn ifeo(&self) -> Ifeo {
        Ifeo::under(HKEY_CURRENT_USER, format!(r"{}\Taskmgr.exe", self.root))
    }

    fn key_exists(&self) -> bool {
        let sub = wide(&format!(r"{}\Taskmgr.exe", self.root));
        let mut k = windows::Win32::System::Registry::HKEY::default();
        let e = unsafe {
            windows::Win32::System::Registry::RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                windows::Win32::System::Registry::KEY_READ | KEY_WOW64_64KEY,
                &mut k,
            )
        };
        if e.0 == 0 {
            unsafe {
                let _ = windows::Win32::System::Registry::RegCloseKey(k);
            }
        }
        e.0 == 0
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        let root = wide(&self.root);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(root.as_ptr()));
            let _ = RegDeleteKeyExW(HKEY_CURRENT_USER, PCWSTR(root.as_ptr()), KEY_WOW64_64KEY.0, None);
            // 测试父键为空时一并清掉，不在用户注册表里留垃圾。
            let parent = wide(TEST_KEY_PREFIX.trim_end_matches('\\'));
            let _ = RegDeleteKeyExW(HKEY_CURRENT_USER, PCWSTR(parent.as_ptr()), KEY_WOW64_64KEY.0, None);
        }
    }
}

fn self_exe() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn cmd_exe() -> PathBuf {
    Path::new(&std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\cmd.exe")
}

#[test]
fn 登记_读取_撤销_往返() {
    let k = TestKey::new("roundtrip");
    let ifeo = k.ifeo();
    let exe = self_exe();

    assert_eq!(ifeo.debugger().unwrap(), None);
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::NotRegistered);
    // 本来就没登记：撤销是空操作，也不会凭空建出键。
    ifeo.unregister(&exe, false).unwrap();
    assert!(!k.key_exists());

    ifeo.register(&exe, false).unwrap();
    assert_eq!(text(ifeo.debugger().unwrap()).as_deref(), Some(debugger_value(&exe).as_str()));
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::ThisExe);
    // 重复登记幂等。
    ifeo.register(&exe, false).unwrap();
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::ThisExe);

    ifeo.unregister(&exe, false).unwrap();
    assert_eq!(ifeo.debugger().unwrap(), None);
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::NotRegistered);
    // 键里没有别的值，空键被一并删除，恢复到登记前的样子。
    assert!(!k.key_exists());
}

#[test]
fn 写入的值带引号且路径含空格也能识别() {
    let k = TestKey::new("quote");
    let ifeo = k.ifeo();
    // 路径含空格的真实文件：复制当前测试 exe 到带空格的临时目录。
    let dir = std::env::temp_dir().join(format!("tml ifeo {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("Task Manager Lite.exe");
    std::fs::copy(self_exe(), &copy).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ifeo.register(&copy, false).unwrap();
        let raw = text(ifeo.debugger().unwrap()).unwrap();
        assert!(raw.starts_with('"') && raw.ends_with('"'), "值应带引号：{raw}");
        assert_eq!(ifeo.status(&copy).unwrap(), Registration::ThisExe);
        // 换成另一个路径查询：识别为他人登记且文件存在。
        assert!(matches!(ifeo.status(&self_exe()).unwrap(), Registration::OtherExe { exists: true, .. }));
    }));
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

#[test]
fn exe_被移动或删除后检测到路径不一致() {
    let k = TestKey::new("moved");
    let ifeo = k.ifeo();
    let dir = std::env::temp_dir().join(format!("tml ifeo-moved {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("TaskManagerLite.exe");
    std::fs::copy(self_exe(), &old).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ifeo.register(&old, false).unwrap();
        assert_eq!(ifeo.status(&old).unwrap(), Registration::ThisExe);

        // 模拟 exe 被删除（或移到别处）：登记仍在，但路径已失效。
        std::fs::remove_file(&old).unwrap();
        let now = self_exe();
        match ifeo.status(&now).unwrap() {
            Registration::OtherExe { debugger, exists: false } => assert_eq!(debugger, DebuggerValue::Text(debugger_value(&old))),
            other => panic!("应识别为失效的他人登记：{other:?}"),
        }
        assert_eq!(ifeo.status(&old).unwrap(), Registration::ThisExe, "路径比较只看文本，与文件是否存在无关");

        // 失效登记不算占用：不加 --force 也能由新位置的程序接管。
        ifeo.register(&now, false).unwrap();
        assert_eq!(ifeo.status(&now).unwrap(), Registration::ThisExe);
    }));
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap();
}

#[test]
fn 别的程序的有效登记不被覆盖或撤销_除非_force() {
    let k = TestKey::new("occupied");
    let ifeo = k.ifeo();
    let theirs = cmd_exe();
    assert!(theirs.is_file());
    // 模拟 Process Explorer 之类已登记：用 cmd.exe 代替。
    ifeo.register(&theirs, false).unwrap();

    let exe = self_exe();
    match ifeo.register(&exe, false) {
        Err(IfeoError::Occupied(v)) => assert_eq!(v, DebuggerValue::Text(debugger_value(&theirs))),
        other => panic!("应拒绝覆盖：{other:?}"),
    }
    assert!(matches!(ifeo.unregister(&exe, false), Err(IfeoError::Occupied(_))));
    assert_eq!(text(ifeo.debugger().unwrap()).as_deref(), Some(debugger_value(&theirs).as_str()), "拒绝时原值不变");

    ifeo.register(&exe, true).unwrap();
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::ThisExe);

    // 强制撤销别人的登记。
    ifeo.register(&theirs, true).unwrap();
    ifeo.unregister(&exe, true).unwrap();
    assert_eq!(ifeo.debugger().unwrap(), None);
}

#[test]
fn 撤销时保留键里的其他值() {
    let k = TestKey::new("othervalue");
    let ifeo = k.ifeo();
    let exe = self_exe();
    ifeo.register(&exe, false).unwrap();

    // 往同一个键里写一个别的值（例如用户自己加的 GlobalFlag）。
    let sub = wide(&format!(r"{}\Taskmgr.exe", k.root));
    unsafe {
        use windows::Win32::System::Registry::{HKEY, KEY_SET_VALUE, REG_DWORD, RegCloseKey, RegOpenKeyExW, RegSetValueExW};
        let mut h = HKEY::default();
        assert_eq!(RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE | KEY_WOW64_64KEY, &mut h).0, 0);
        let name = wide("GlobalFlag");
        assert_eq!(RegSetValueExW(h, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&0x200u32.to_le_bytes())).0, 0);
        let _ = RegCloseKey(h);
    }

    ifeo.unregister(&exe, false).unwrap();
    assert_eq!(ifeo.debugger().unwrap(), None);
    assert!(k.key_exists(), "键里还有别的值，不能删键");
}

/// 往测试键的 Taskmgr.exe 子键里直接写一个任意类型的 Debugger 值。
fn write_raw_debugger(k: &TestKey, ty: windows::Win32::System::Registry::REG_VALUE_TYPE, data: &[u8]) {
    use windows::Win32::System::Registry::{
        HKEY, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegSetValueExW,
    };
    let sub = wide(&format!(r"{}\Taskmgr.exe", k.root));
    unsafe {
        let mut h = HKEY::default();
        assert_eq!(
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_WOW64_64KEY,
                None,
                &mut h,
                None
            )
            .0,
            0
        );
        let name = wide("Debugger");
        assert_eq!(RegSetValueExW(h, PCWSTR(name.as_ptr()), None, ty, Some(data)).0, 0);
        let _ = RegCloseKey(h);
    }
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    wide(s).iter().flat_map(|c| c.to_le_bytes()).collect()
}

#[test]
fn reg_expand_sz_的值展开后判断_有效的他人登记受保护() {
    use windows::Win32::System::Registry::REG_EXPAND_SZ;
    let k = TestKey::new("expand");
    let ifeo = k.ifeo();
    let exe = self_exe();
    write_raw_debugger(&k, REG_EXPAND_SZ, &utf16_bytes(r#""%SystemRoot%\System32\cmd.exe""#));

    // 展开后是存在的 cmd.exe：被他人有效占用，不加 force 不能覆盖也不能删除。
    assert!(matches!(ifeo.status(&exe).unwrap(), Registration::OtherExe { exists: true, .. }));
    assert!(matches!(ifeo.register(&exe, false), Err(IfeoError::Occupied(_))));
    assert!(matches!(ifeo.unregister(&exe, false), Err(IfeoError::Occupied(_))));
    assert_eq!(text(ifeo.debugger().unwrap()).as_deref(), Some(r#""%SystemRoot%\System32\cmd.exe""#), "原值未被改动");

    // 展开后指向不存在的文件：失效，可直接替换。
    write_raw_debugger(&k, REG_EXPAND_SZ, &utf16_bytes(r#""%SystemRoot%\definitely-not-here\x.exe""#));
    assert!(matches!(ifeo.status(&exe).unwrap(), Registration::OtherExe { exists: false, .. }));
    ifeo.register(&exe, false).unwrap();
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::ThisExe);
}

#[test]
fn 非字符串类型的_debugger_值按被占用处理() {
    use windows::Win32::System::Registry::REG_DWORD;
    let k = TestKey::new("dword");
    let ifeo = k.ifeo();
    let exe = self_exe();
    write_raw_debugger(&k, REG_DWORD, &1u32.to_le_bytes());

    match ifeo.status(&exe).unwrap() {
        Registration::OtherExe { exists: true, debugger } => {
            // REG_DWORD 的类型号是 4。
            assert_eq!(debugger, DebuggerValue::NonString { value_type: 4 });
        }
        other => panic!("DWORD 值应按被占用处理：{other:?}"),
    }
    assert!(matches!(ifeo.register(&exe, false), Err(IfeoError::Occupied(_))));
    assert!(matches!(ifeo.unregister(&exe, false), Err(IfeoError::Occupied(_))));
    assert_eq!(ifeo.debugger().unwrap(), Some(DebuggerValue::NonString { value_type: 4 }), "值仍在");

    // force 才能覆盖。
    ifeo.register(&exe, true).unwrap();
    assert_eq!(ifeo.status(&exe).unwrap(), Registration::ThisExe);
}

#[test]
fn 目标文件不存在时拒绝登记() {
    let k = TestKey::new("nofile");
    let ifeo = k.ifeo();
    let missing = Path::new(r"C:\definitely\not\here\TaskManagerLite.exe");
    assert!(matches!(ifeo.register(missing, false), Err(IfeoError::ExeNotFound(_))));
    assert!(!k.key_exists());
}

#[test]
fn 测试键不是系统键() {
    let k = TestKey::new("notsystem");
    assert!(!k.ifeo().is_system());
    assert!(Ifeo::system().is_system());
}

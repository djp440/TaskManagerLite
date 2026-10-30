//! 运行新任务（#69）：把用户输入的“程序、文件夹、文档或网址”交给外壳打开，可选以管理员身份。
//!
//! 输入的拆分（`parse_command`）与环境变量展开是纯逻辑，可以脱离系统窗口测试；真正启动的只有最后的
//! `ShellExecuteExW`。核心库只返回结构化结果（带系统错误码），显示文案由界面层映射。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HRESULT, PCWSTR, w};

use crate::error::SystemError;
use crate::shellops::ensure_com;

/// 拆分后的启动命令：交给外壳的文件（程序、文件夹、文档或网址）与参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub file: String,
    pub parameters: String,
}

/// 运行新任务失败的原因。界面层按枚举映射成显示文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// 输入为空（全是空白）。
    Empty,
    /// 用户在 UAC 对话框里取消了提权（`ERROR_CANCELLED`）。
    Cancelled,
    /// 外壳启动失败：保留系统错误码（如 2 找不到文件、3 找不到路径、5 拒绝访问）。
    System(SystemError),
}

impl RunError {
    /// 写入日志用的错误码：Win32 错误码或 HRESULT；没有时为 0。
    pub fn code(&self) -> i32 {
        match self {
            RunError::Empty => 0,
            RunError::Cancelled => ERROR_CANCELLED.0 as i32,
            RunError::System(SystemError::Win32(code)) => *code,
            RunError::System(SystemError::Other { code, .. }) => *code,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Empty => f.write_str("empty input"),
            RunError::Cancelled => f.write_str("cancelled by user"),
            RunError::System(e) => write!(f, "run failed: {e}"),
        }
    }
}

impl std::error::Error for RunError {}

/// 展开 `%VAR%` 形式的环境变量（与“运行”对话框一致）；未定义的变量原样保留。
pub fn expand_env(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    let src: Vec<u16> = OsStr::new(input).encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 4096];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        // 失败或结果超长：按原文处理，由外壳报告错误。
        return input.to_owned();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

/// 把输入拆成文件与参数，规则（`exists` 判断一个路径是否存在）：
/// 1. 去掉首尾空白，空输入返回 `None`；
/// 2. 以引号开头：引号内是文件，其后的全部文字是参数（缺少收尾引号时，其后全部文字当作文件）；
/// 3. 否则若整段文字就是一个存在的路径，整段当作文件（含空格的未加引号路径、文件夹）；
/// 4. 否则按空白从短到长尝试前缀，第一个存在的路径当作文件（`C:\Program Files\x\a.exe --flag`）；
///    只有含路径分隔符或盘符的前缀才检查是否存在，`notepad foo.txt` 这类不会被当前目录里的同名文件干扰；
/// 5. 都不是则在第一个空白处拆开：`notepad foo.txt`、`cmd /k dir`；网址不含空白时整段就是文件。
pub fn parse_command_with(input: &str, exists: impl Fn(&str) -> bool) -> Option<Command> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(rest) = input.strip_prefix('"') {
        return Some(match rest.find('"') {
            Some(end) => Command { file: rest[..end].to_owned(), parameters: rest[end + 1..].trim().to_owned() },
            None => Command { file: rest.trim().to_owned(), parameters: String::new() },
        })
        .filter(|c| !c.file.is_empty());
    }
    let looks_like_path = |s: &str| s.contains(['\\', '/', ':']);
    if looks_like_path(input) && exists(input) {
        return Some(Command { file: input.to_owned(), parameters: String::new() });
    }
    // 空白处的边界：每段空白之前的位置。
    let mut first_split = None;
    let mut chars = input.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !c.is_whitespace() {
            continue;
        }
        let head = &input[..i];
        if first_split.is_none() {
            first_split = Some(i);
        }
        if looks_like_path(head) && exists(head) {
            return Some(Command { file: head.to_owned(), parameters: input[i..].trim().to_owned() });
        }
        // 连续空白只处理一次。
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            chars.next();
        }
    }
    Some(match first_split {
        Some(i) => Command { file: input[..i].to_owned(), parameters: input[i..].trim().to_owned() },
        None => Command { file: input.to_owned(), parameters: String::new() },
    })
}

/// 用文件系统判断路径是否存在的 [`parse_command_with`]。
pub fn parse_command(input: &str) -> Option<Command> {
    parse_command_with(input, |p| Path::new(p).exists())
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 外壳调用的错误：用户取消提权单独成类，其余保留错误码。
fn classify(e: &windows::core::Error) -> RunError {
    let code = e.code();
    if code == HRESULT::from_win32(ERROR_CANCELLED.0) {
        return RunError::Cancelled;
    }
    // `HRESULT_FROM_WIN32` 形式（高 16 位 0x8007）还原成 Win32 错误码，其余 HRESULT 带上系统描述。
    let raw = code.0 as u32;
    if raw >> 16 == 0x8007 {
        RunError::System(SystemError::Win32((raw & 0xFFFF) as i32))
    } else {
        RunError::System(SystemError::from_windows(e))
    }
}

/// 启动目录：用户目录（与“运行”对话框一致），不存在时交给系统取当前目录。
fn start_directory() -> Option<Vec<u16>> {
    let dir = std::env::var_os("USERPROFILE")?;
    Path::new(&dir).is_dir().then(|| dir.encode_wide().chain(std::iter::once(0)).collect())
}

/// 展开环境变量、拆分输入并交给外壳打开；`elevated` 时用 `runas` 动词（会弹出 UAC）。
///
/// 阻塞到外壳调用返回（以管理员身份运行时，包含等待用户在 UAC 对话框中确认），应在后台线程调用。
pub fn run_task(input: &str, elevated: bool) -> Result<(), RunError> {
    let command = parse_command(&expand_env(input)).ok_or(RunError::Empty)?;
    ensure_com();
    let file = wide(&command.file);
    let parameters = wide(&command.parameters);
    let directory = start_directory();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // 不弹系统自己的出错对话框（原因由调用方显示）；同步完成，线程随后退出也安全。
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: if elevated { w!("runas") } else { w!("open") },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if command.parameters.is_empty() { PCWSTR::null() } else { PCWSTR(parameters.as_ptr()) },
        lpDirectory: directory.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| classify(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假装只有这些路径存在。
    fn parse(input: &str, existing: &[&str]) -> Option<(String, String)> {
        parse_command_with(input, |p| existing.contains(&p)).map(|c| (c.file, c.parameters))
    }

    fn cmd(file: &str, parameters: &str) -> Option<(String, String)> {
        Some((file.to_owned(), parameters.to_owned()))
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(parse("", &[]), None);
        assert_eq!(parse("   \t ", &[]), None);
        assert_eq!(parse("\"\"", &[]), None, "空引号没有文件");
        assert_eq!(parse("\"\" abc", &[]), None);
    }

    #[test]
    fn bare_program_and_trimmed_input() {
        assert_eq!(parse("notepad", &[]), cmd("notepad", ""));
        assert_eq!(parse("  explorer  ", &[]), cmd("explorer", ""));
        assert_eq!(parse("explorer.exe", &[]), cmd("explorer.exe", ""));
    }

    #[test]
    fn program_with_arguments_splits_at_first_space() {
        assert_eq!(parse("notepad foo.txt", &[]), cmd("notepad", "foo.txt"));
        assert_eq!(parse("cmd /k   dir  C:\\", &[]), cmd("cmd", "/k   dir  C:\\"));
        // 当前目录里恰好有同名文件（相对路径）不影响拆分。
        assert_eq!(parse("notepad foo.txt", &["notepad"]), cmd("notepad", "foo.txt"));
    }

    #[test]
    fn quoted_path_with_arguments() {
        assert_eq!(
            parse(r#""C:\Program Files\x\a.exe" --flag "a b""#, &[]),
            cmd(r"C:\Program Files\x\a.exe", r#"--flag "a b""#)
        );
        assert_eq!(parse(r#""C:\Program Files\x\a.exe""#, &[]), cmd(r"C:\Program Files\x\a.exe", ""));
        assert_eq!(parse(r#"  "C:\a b\c.exe"   x "#, &[]), cmd(r"C:\a b\c.exe", "x"));
        // 缺少收尾引号：其后全部当作文件。
        assert_eq!(parse(r#""C:\a b\c.exe"#, &[]), cmd(r"C:\a b\c.exe", ""));
    }

    #[test]
    fn unquoted_path_with_spaces_uses_existing_prefix() {
        let exe = r"C:\Program Files\x\a.exe";
        assert_eq!(parse(exe, &[exe]), cmd(exe, ""));
        assert_eq!(parse(&format!("{exe} --flag a"), &[exe]), cmd(exe, "--flag a"));
        // 存在的文件夹（含空格）整段当作文件。
        let dir = r"C:\Program Files\Common Files";
        assert_eq!(parse(dir, &[dir]), cmd(dir, ""));
        // 路径不存在时在第一个空白处拆开，由外壳报告“找不到文件”。
        assert_eq!(parse(r"C:\Program Files\x\a.exe", &[]), cmd(r"C:\Program", r"Files\x\a.exe"));
    }

    #[test]
    fn directory_documents_and_urls() {
        assert_eq!(parse(r"C:\Windows", &[r"C:\Windows"]), cmd(r"C:\Windows", ""));
        assert_eq!(parse(r"C:\Windows\", &[]), cmd(r"C:\Windows\", ""));
        assert_eq!(parse(r"D:\docs\a.docx", &[]), cmd(r"D:\docs\a.docx", ""));
        assert_eq!(parse("https://example.com/a?b=1&c=2", &[]), cmd("https://example.com/a?b=1&c=2", ""));
        assert_eq!(parse("mailto:a@b.com", &[]), cmd("mailto:a@b.com", ""));
        assert_eq!(parse(r"\\server\share\x.exe /s", &[]), cmd(r"\\server\share\x.exe", "/s"));
    }

    #[test]
    fn environment_variables_are_expanded_before_parsing() {
        let windir = std::env::var("windir").expect("系统目录环境变量");
        assert_eq!(expand_env(r"%windir%\notepad.exe"), format!(r"{windir}\notepad.exe"));
        assert_eq!(expand_env("%WINDIR%"), windir, "变量名不区分大小写");
        assert_eq!(expand_env("plain"), "plain");
        assert_eq!(expand_env("%TML_NO_SUCH_VARIABLE%\\x"), "%TML_NO_SUCH_VARIABLE%\\x", "未定义的变量原样保留");
        assert_eq!(expand_env("100%"), "100%");
        // 网址里的百分号转义不受影响。
        assert_eq!(expand_env("https://x.com/a%20b/c"), "https://x.com/a%20b/c");
        // 展开后的结果交给拆分，真实路径存在。
        let expanded = expand_env(r"%windir%\notepad.exe -x");
        let c = parse_command(&expanded).unwrap();
        assert_eq!((c.file.as_str(), c.parameters.as_str()), (format!(r"{windir}\notepad.exe").as_str(), "-x"));
    }

    #[test]
    fn real_filesystem_prefix_detection() {
        // 真实存在的、带空格的路径：未加引号也整体当作文件。
        let dir = std::env::temp_dir().join(format!("tml run task {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("my app.exe");
        std::fs::write(&file, b"").unwrap();
        let text = file.to_string_lossy().into_owned();
        let c = parse_command(&format!("{text} /x")).unwrap();
        assert_eq!((c.file, c.parameters), (text.clone(), "/x".to_owned()));
        let c = parse_command(&dir.to_string_lossy()).unwrap();
        assert_eq!(c.file, dir.to_string_lossy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_input_never_reaches_the_shell() {
        assert_eq!(run_task("   ", false), Err(RunError::Empty));
        assert_eq!(run_task("", true), Err(RunError::Empty));
    }

    #[test]
    fn missing_program_reports_a_win32_error_code() {
        // 不存在的程序：外壳失败并带错误码（2 找不到文件，3 找不到路径），不弹任何窗口。
        match run_task("tml-no-such-program-69.exe", false) {
            Err(RunError::System(SystemError::Win32(code))) => assert!(matches!(code, 2 | 3), "错误码 {code}"),
            other => panic!("应该失败并带 Win32 错误码：{other:?}"),
        }
        match run_task(r"C:\tml-no-such-dir-69\a.exe /x", false) {
            Err(RunError::System(SystemError::Win32(code))) => assert!(matches!(code, 2 | 3), "错误码 {code}"),
            other => panic!("应该失败并带 Win32 错误码：{other:?}"),
        }
    }

    #[test]
    fn errors_are_classified() {
        let cancelled = windows::core::Error::from_hresult(HRESULT::from_win32(ERROR_CANCELLED.0));
        assert_eq!(classify(&cancelled), RunError::Cancelled);
        assert_eq!(RunError::Cancelled.code(), 1223);
        let denied = windows::core::Error::from_hresult(HRESULT::from_win32(5));
        assert_eq!(classify(&denied), RunError::System(SystemError::Win32(5)));
        assert_eq!(classify(&denied).code(), 5);
        let other = windows::core::Error::from_hresult(HRESULT(0x8000_4005_u32 as i32));
        assert!(matches!(classify(&other), RunError::System(SystemError::Other { code, .. }) if code == 0x8000_4005_u32 as i32));
    }
}

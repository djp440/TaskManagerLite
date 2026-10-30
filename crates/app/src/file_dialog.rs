//! 系统“打开文件”（`GetOpenFileNameW`）与“另存为”（`GetSaveFileNameW`）对话框，
//! 供“运行新任务”的“浏览…”和“导出为 CSV”使用。
//!
//! 对话框自己跑消息循环，会阻塞调用线程：调用方应在后台线程里调用，并把主窗口句柄作为所有者传入（使它成为模态）。

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NODEREFERENCELINKS,
    OFN_NOREADONLYRETURN, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::core::{PCWSTR, PWSTR};

/// 路径缓冲区长度（UTF-16 单元）；`MAX_PATH` 之上留足长路径的余量。
const PATH_BUFFER: usize = 4096;

/// 文件类型过滤器的系统格式：`显示名\0通配符\0…\0\0`。
pub fn filter_string(filters: &[(&str, &str)]) -> Vec<u16> {
    let mut out = Vec::new();
    for (name, pattern) in filters {
        out.extend(name.encode_utf16());
        out.push(0);
        out.extend(pattern.encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

/// UTF-16 缓冲区里第一个 0 之前的内容转成路径；为空时为 `None`。
fn path_from_buffer(buffer: &[u16]) -> Option<PathBuf> {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    (len > 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..len])))
}

fn wide_nul(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
}

/// 弹出“打开文件”对话框，返回所选文件的路径；用户取消（或出错）时为 `None`。
/// `owner` 为所有者窗口句柄（`HWND` 的数值，0 表示没有所有者）。
pub fn pick_file(owner: isize, title: &str, filters: &[(&str, &str)]) -> Option<PathBuf> {
    let filter = filter_string(filters);
    let title = wide_nul(title);
    let mut buffer = vec![0u16; PATH_BUFFER];
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut _),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        // 不解析快捷方式：选 .lnk 就运行 .lnk 本身。
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_NODEREFERENCELINKS,
        ..Default::default()
    };
    // SAFETY: ofn 中的指针都指向本函数里在调用期间保持有效的缓冲区。
    if !unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    path_from_buffer(&buffer)
}

/// 弹出“另存为”对话框，返回要保存到的路径；用户取消（或出错）时为 `None`。
/// `default_name` 是预填的文件名；`default_ext` 是用户没写扩展名时补上的扩展名（不带点）。
/// 文件已存在时由对话框自己询问是否覆盖。
pub fn pick_save_file(
    owner: isize,
    title: &str,
    default_name: &str,
    default_ext: &str,
    filters: &[(&str, &str)],
) -> Option<PathBuf> {
    let filter = filter_string(filters);
    let title = wide_nul(title);
    let ext = wide_nul(default_ext);
    let mut buffer = vec![0u16; PATH_BUFFER];
    // 预填文件名；留出结尾的 0。
    for (slot, c) in buffer.iter_mut().zip(default_name.encode_utf16().take(PATH_BUFFER - 1)) {
        *slot = c;
    }
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut _),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        lpstrDefExt: PCWSTR(ext.as_ptr()),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_NOREADONLYRETURN,
        ..Default::default()
    };
    // SAFETY: ofn 中的指针都指向本函数里在调用期间保持有效的缓冲区。
    if !unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
        return None;
    }
    path_from_buffer(&buffer)
}

/// 把选中的路径变成输入框里的文字：含空格时加引号，这样不会和参数混在一起。
pub fn input_text(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if text.contains(char::is_whitespace) { format!("\"{text}\"") } else { text.into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn filter_string_is_nul_separated_and_double_nul_terminated() {
        let f = filter_string(&[("程序", "*.exe;*.bat"), ("所有文件", "*.*")]);
        let parts: Vec<String> = f.split(|&c| c == 0).map(String::from_utf16_lossy).collect();
        assert_eq!(parts, vec!["程序", "*.exe;*.bat", "所有文件", "*.*", "", ""]);
        assert_eq!(&f[f.len() - 2..], &[0, 0]);
    }

    #[test]
    fn buffer_to_path_stops_at_the_first_nul() {
        let mut buf = vec![0u16; 16];
        for (slot, c) in buf.iter_mut().zip(r"C:\报告.csv".encode_utf16()) {
            *slot = c;
        }
        assert_eq!(path_from_buffer(&buf), Some(PathBuf::from(r"C:\报告.csv")));
        assert_eq!(path_from_buffer(&[0; 4]), None);
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        assert_eq!(input_text(Path::new(r"C:\Windows\notepad.exe")), r"C:\Windows\notepad.exe");
        assert_eq!(input_text(Path::new(r"C:\Program Files\x\a.exe")), r#""C:\Program Files\x\a.exe""#);
    }
}

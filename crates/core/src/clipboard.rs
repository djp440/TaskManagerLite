//! 剪贴板文本读写。

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// `CF_UNICODETEXT`
const CF_UNICODETEXT: u32 = 13;

/// 已打开的剪贴板，离开作用域时关闭。
struct Open;

impl Open {
    /// 剪贴板同一时刻只允许一个程序打开，被占用时短暂重试。
    fn acquire() -> Option<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// 把文本放入剪贴板（替换原有内容）。成功返回 `true`。
pub fn set_clipboard_text(text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * 2;
    let Some(_open) = Open::acquire() else { return false };
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes) else { return false };
        let dst = GlobalLock(mem).cast::<u16>();
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len());
        let _ = GlobalUnlock(mem);
        if EmptyClipboard().is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        // 成功后内存所有权归系统；失败时需要自己释放。
        if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
    }
    true
}

/// 读取剪贴板里的文本；没有文本时为 `None`。
pub fn clipboard_text() -> Option<String> {
    let _open = Open::acquire()?;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        let mem = HGLOBAL(h.0);
        let p = GlobalLock(mem).cast::<u16>();
        if p.is_null() {
            return None;
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(mem);
        Some(text)
    }
}

/// 剪贴板是空的，或只含文本（测试据此判断能否无损地还原）。
#[cfg(test)]
fn is_empty_or_text_only() -> bool {
    use windows::Win32::System::DataExchange::{CountClipboardFormats, IsClipboardFormatAvailable};
    let Some(_open) = Open::acquire() else { return false };
    let count = unsafe { CountClipboardFormats() };
    count == 0 || unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写入再读回；会临时改写系统剪贴板，所以先保存文本并在结束时还原，
    /// 剪贴板里是图片等非文本内容时跳过，避免破坏用户数据。
    #[test]
    fn text_round_trips_through_the_clipboard() {
        if !is_empty_or_text_only() {
            eprintln!("剪贴板含非文本内容，跳过");
            return;
        }
        let saved = clipboard_text();
        let sample = r#"C:\Program Files\示例 App\app.exe --flag "a b"
line2"#;
        assert!(set_clipboard_text(sample));
        assert_eq!(clipboard_text().as_deref(), Some(sample));
        assert!(set_clipboard_text(""));
        assert_eq!(clipboard_text().as_deref(), Some(""));
        if let Some(text) = saved {
            assert!(set_clipboard_text(&text));
        }
    }
}

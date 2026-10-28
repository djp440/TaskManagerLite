//! 托盘图标：直接用 `Shell_NotifyIconW` 实现，不引入 `tray-icon` crate（体积对比见 PR #8）。
//!
//! - 一个不可见的顶层窗口接收托盘回调消息。不用 message-only 窗口，是因为它收不到 `TaskbarCreated`
//!   广播，资源管理器重启后图标就不会自动恢复。
//! - 左键单击 → [`Event::Toggle`]；右键弹出“显示窗口 / 退出”菜单；菜单项也以 `WM_COMMAND` 送达，
//!   自动化测试可以直接向该窗口 `PostMessage` 同样的消息，不必真的弹出模态菜单。
//! - 图标在运行时按像素画出（蓝底圆角方块 + 白色折线），没有资源文件，也不依赖外壳的图标缓存。
//! - 全部在界面线程使用；回调由 Slint 的事件循环（同一个 Win32 消息循环）派发。

use std::cell::RefCell;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    GWLP_USERDATA, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, HICON, HMENU, MF_STRING, PostMessageW,
    RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, SetMenuDefaultItem, SetWindowLongPtrW,
    TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_COMMAND, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WNDCLASSEXW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

const CLASS: PCWSTR = w!("TaskManagerLiteTray");
/// 托盘图标的回调消息；`wParam` 是图标 id，`lParam` 是鼠标消息。
pub const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
/// 菜单命令 id，同时是 `WM_COMMAND` 的低位字。
pub const CMD_SHOW: usize = 1001;
pub const CMD_EXIT: usize = 1002;

/// 托盘产生的事件，由 [`Tray::new`] 的回调处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// 左键单击：窗口隐藏或最小化则显示，否则隐藏。
    Toggle,
    /// 菜单“显示窗口”。
    Show,
    /// 菜单“退出”。
    Quit,
}

/// 托盘回调里的鼠标消息 → 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mouse {
    Event(Event),
    Menu,
}

fn mouse_action(message: u32) -> Option<Mouse> {
    match message {
        WM_LBUTTONUP => Some(Mouse::Event(Event::Toggle)),
        WM_RBUTTONUP => Some(Mouse::Menu),
        _ => None,
    }
}

fn command_event(id: usize) -> Option<Event> {
    match id {
        CMD_SHOW => Some(Event::Show),
        CMD_EXIT => Some(Event::Quit),
        _ => None,
    }
}

/// 托盘提示文字（`szTip` 最多 127 个 UTF-16 单元）。`live` 为假表示采样已暂停，数值是最后一次读数。
pub fn tooltip(cpu: Option<(f32, u64, u64)>, live: bool) -> String {
    let Some((cpu_usage, used, total)) = cpu else { return "TaskManagerLite".into() };
    let pct = if total > 0 { used as f64 / total as f64 * 100.0 } else { 0.0 };
    let gib = |b: u64| b as f64 / 1024.0 / 1024.0 / 1024.0;
    let mut text = format!(
        "TaskManagerLite\nCPU {:.0}%  内存 {:.0}%（{:.1}/{:.1} GB）",
        cpu_usage,
        pct,
        gib(used),
        gib(total)
    );
    if !live {
        text.push_str("\n采样已暂停，开启后台采样可持续更新");
    }
    text
}

fn tip_units(text: &str) -> [u16; 128] {
    let mut out = [0u16; 128];
    // 保留末尾的 0，并避免把一对代理项截成两半。
    let mut units: Vec<u16> = text.encode_utf16().take(127).collect();
    if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
        units.pop();
    }
    out[..units.len()].copy_from_slice(&units);
    out
}

struct Inner {
    hwnd: HWND,
    icon: HICON,
    tip: RefCell<[u16; 128]>,
    taskbar_created: u32,
    on_event: Box<dyn Fn(Event)>,
}

impl Inner {
    fn data(&self, flags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: flags,
            uCallbackMessage: CALLBACK_MESSAGE,
            hIcon: self.icon,
            szTip: *self.tip.borrow(),
            ..Default::default()
        }
    }

    fn add(&self) -> bool {
        unsafe { Shell_NotifyIconW(NIM_ADD, &self.data(NIF_MESSAGE | NIF_ICON | NIF_TIP)) }.as_bool()
    }
}

pub struct Tray {
    inner: Box<Inner>,
}

impl Tray {
    /// 创建隐藏窗口并添加托盘图标。外壳不可用（例如没有任务栏）时返回 `None`。
    pub fn new(initial_tip: &str, on_event: impl Fn(Event) + 'static) -> Option<Tray> {
        let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }.ok()?.into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: CLASS,
            ..Default::default()
        };
        // 重复注册（例如先关闭再开启）会失败，类已存在，忽略即可。
        unsafe { RegisterClassExW(&class) };
        let hwnd = unsafe {
            CreateWindowExW(Default::default(), CLASS, w!("TaskManagerLite Tray"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)
        }
        .ok()?;
        let icon = create_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize)?;
        let inner = Box::new(Inner {
            hwnd,
            icon,
            tip: RefCell::new(tip_units(initial_tip)),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            on_event: Box::new(on_event),
        });
        // Box 的地址在 Tray 存活期间不变；窗口过程通过它取回状态，Tray 先于窗口销毁。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*inner as *const Inner as isize) };
        if !inner.add() {
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DestroyIcon(icon);
            }
            return None;
        }
        Some(Tray { inner })
    }

    /// 更新提示文字。内容没变时不调用外壳。
    pub fn set_tip(&self, text: &str) {
        let tip = tip_units(text);
        if *self.inner.tip.borrow() == tip {
            return;
        }
        *self.inner.tip.borrow_mut() = tip;
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.inner.data(NIF_TIP)) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let inner = &*self.inner;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &inner.data(Default::default()));
            SetWindowLongPtrW(inner.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(inner.hwnd);
            let _ = DestroyIcon(inner.icon);
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Inner;
    // SAFETY: 指针来自 `Tray::new` 中的 Box，Tray 销毁窗口之前先清零。
    let Some(inner) = (unsafe { ptr.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        CALLBACK_MESSAGE => match mouse_action(lparam.0 as u32 & 0xFFFF) {
            Some(Mouse::Event(e)) => (inner.on_event)(e),
            Some(Mouse::Menu) => {
                if let Some(e) = show_menu(hwnd).and_then(command_event) {
                    (inner.on_event)(e);
                }
            }
            None => {}
        },
        WM_COMMAND => {
            if let Some(e) = command_event(wparam.0 & 0xFFFF) {
                (inner.on_event)(e);
            }
        }
        m if m == inner.taskbar_created => {
            // 资源管理器重启后任务栏重建，重新添加图标。
            inner.add();
        }
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

/// 在光标处弹出菜单，返回选中的命令 id（取消返回 `None`）。
fn show_menu(hwnd: HWND) -> Option<usize> {
    let menu: HMENU = unsafe { CreatePopupMenu() }.ok()?;
    let chosen = unsafe {
        let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, w!("显示窗口"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出"));
        let _ = SetMenuDefaultItem(menu, CMD_SHOW as u32, 0);
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // 不先成为前台窗口，点菜单以外的地方菜单不会消失。
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD, at.x, at.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        id.0 as usize
    };
    (chosen != 0).then_some(chosen)
}

/// 画出托盘图标：`size`×`size` 的蓝底圆角方块，上面一条白色折线。4×4 超采样抗锯齿。
fn render_icon(size: usize) -> Vec<u8> {
    const BG: [f32; 3] = [0x4c as f32, 0x9b as f32, 0xf5 as f32];
    // 折线顶点（单位坐标，原点在左上）。
    const LINE: [(f32, f32); 6] = [(0.14, 0.66), (0.32, 0.66), (0.43, 0.30), (0.56, 0.78), (0.67, 0.46), (0.86, 0.46)];
    let s = size as f32;
    let radius = s * 0.24;
    let half_width = (s * 0.075).max(0.8);
    let dist_to_segment = |p: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        let (abx, aby) = (b.0 - a.0, b.1 - a.1);
        let len2 = abx * abx + aby * aby;
        let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
        ((p.0 - a.0 - t * abx).hypot(p.1 - a.1 - t * aby)).abs()
    };
    let inside = |x: f32, y: f32| {
        // 圆角矩形：到“内缩 radius 的矩形”的距离不超过 radius。
        let dx = (x - s / 2.0).abs() - (s / 2.0 - radius);
        let dy = (y - s / 2.0).abs() - (s / 2.0 - radius);
        dx.max(0.0).hypot(dy.max(0.0)) <= radius
    };
    let mut out = vec![0u8; size * size * 4];
    for py in 0..size {
        for px in 0..size {
            let (mut cover, mut line) = (0.0f32, 0.0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let (x, y) = (px as f32 + (sx as f32 + 0.5) / 4.0, py as f32 + (sy as f32 + 0.5) / 4.0);
                    if !inside(x, y) {
                        continue;
                    }
                    cover += 1.0;
                    let on_line = LINE.windows(2).any(|seg| {
                        dist_to_segment((x, y), (seg[0].0 * s, seg[0].1 * s), (seg[1].0 * s, seg[1].1 * s)) <= half_width
                    });
                    if on_line {
                        line += 1.0;
                    }
                }
            }
            let i = (py * size + px) * 4;
            if cover > 0.0 {
                let l = line / cover;
                // BGRA，非预乘。
                for c in 0..3 {
                    out[i + 2 - c] = (BG[c] * (1.0 - l) + 255.0 * l).round() as u8;
                }
                out[i + 3] = (cover / 16.0 * 255.0).round() as u8;
            }
        }
    }
    out
}

fn create_icon(size: usize) -> Option<HICON> {
    let colour = render_icon(size);
    // AND 掩码全 0：完全由 32 位颜色里的 alpha 决定透明度。每行按 16 位对齐。
    let and_mask = vec![0u8; size.div_ceil(16) * 2 * size];
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }.ok()?.into();
    unsafe { CreateIcon(Some(hinst), size as i32, size as i32, 1, 32, and_mask.as_ptr(), colour.as_ptr()) }.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_messages_map_to_actions() {
        assert_eq!(mouse_action(WM_LBUTTONUP), Some(Mouse::Event(Event::Toggle)));
        assert_eq!(mouse_action(WM_RBUTTONUP), Some(Mouse::Menu));
        // 按下和移动不触发动作，避免一次点击处理两遍。
        assert_eq!(mouse_action(0x0201), None);
        assert_eq!(mouse_action(0x0200), None);
    }

    #[test]
    fn menu_commands_map_to_events() {
        assert_eq!(command_event(CMD_SHOW), Some(Event::Show));
        assert_eq!(command_event(CMD_EXIT), Some(Event::Quit));
        assert_eq!(command_event(0), None);
    }

    #[test]
    fn tooltip_shows_cpu_and_memory() {
        let gib = 1024u64 * 1024 * 1024;
        let t = tooltip(Some((12.4, 8 * gib, 32 * gib)), true);
        assert!(t.contains("CPU 12%") && t.contains("内存 25%") && t.contains("8.0/32.0 GB"), "{t}");
        assert!(!t.contains("暂停"));
        assert!(tooltip(Some((1.0, gib, 2 * gib)), false).contains("采样已暂停"));
        assert_eq!(tooltip(None, true), "TaskManagerLite");
    }

    #[test]
    fn tooltip_fits_the_shell_limit() {
        let gib = 1024u64 * 1024 * 1024;
        let t = tooltip(Some((100.0, 1024 * gib, 1024 * gib)), false);
        assert!(t.encode_utf16().count() <= 127, "{}", t.encode_utf16().count());
        let units = tip_units(&"字".repeat(300));
        assert_eq!(units[127], 0);
        assert_eq!(units[126], '字' as u16);
        // 截断点落在代理对中间时丢掉前半个。
        let units = tip_units(&format!("{}😀", "a".repeat(126)));
        assert_eq!(units[126], 0);
    }

    #[test]
    fn rendered_icon_is_rounded_blue_square_with_white_line() {
        for size in [16usize, 24, 32] {
            let px = render_icon(size);
            assert_eq!(px.len(), size * size * 4);
            let at = |x: usize, y: usize| &px[(y * size + x) * 4..(y * size + x) * 4 + 4];
            // 圆角：左上角完全透明；中心偏上（折线之外）为不透明的蓝色（BGRA）。
            assert_eq!(at(0, 0)[3], 0, "size {size}");
            let c = at(size / 2, 1);
            assert_eq!((c[0], c[1], c[2], c[3]), (0xf5, 0x9b, 0x4c, 255), "size {size}");
            // 折线存在：至少有一部分像素接近白色。
            assert!(px.chunks(4).any(|p| p[3] == 255 && p[0] > 240 && p[1] > 240 && p[2] > 240), "size {size}");
        }
    }
}

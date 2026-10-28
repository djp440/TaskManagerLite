//! 窗口位置与大小的恢复与记录。

use slint::{ComponentHandle, PhysicalPosition};
use windows::Win32::Foundation::RECT;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, MonitorFromRect};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};

use crate::AppWindow;
use crate::settings::{Settings, WindowRect};

/// 标题栏探测区：窗口左上角向右 200 像素处的一小块，只要它落在某个显示器内，用户就还能抓住标题栏。
const PROBE: (i32, i32, i32, i32) = (168, 0, 232, 32);

/// 校正保存的窗口矩形：逻辑尺寸夹到合理范围；探测区不在任何显示器内（例如副屏已拔掉）时
/// 不恢复位置，让系统决定。`on_screen(left, top, right, bottom)` 判断矩形是否与某个显示器相交。
pub fn fit(saved: WindowRect, on_screen: impl Fn(i32, i32, i32, i32) -> bool) -> (Option<(i32, i32)>, (f32, f32)) {
    let size = (saved.width.clamp(320.0, 16384.0), saved.height.clamp(240.0, 16384.0));
    let (x, y) = (saved.x, saved.y);
    let visible = on_screen(
        x.saturating_add(PROBE.0),
        y.saturating_add(PROBE.1),
        x.saturating_add(PROBE.2),
        y.saturating_add(PROBE.3),
    );
    (visible.then_some((x, y)), size)
}

fn on_monitor(left: i32, top: i32, right: i32, bottom: i32) -> bool {
    let rect = RECT { left, top, right, bottom };
    // SAFETY: 只读查询，rect 在调用期间有效。
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// 小窗口（迷你模式）在物理坐标 (x, y) 处顶部一条是否落在某个显示器内，即用户是否还能看到并拖动它。
pub fn reachable(x: i32, y: i32, logical_width: f32, scale: f32) -> bool {
    let w = (logical_width * scale) as i32;
    on_monitor(x.saturating_add(w / 4), y, x.saturating_add(w * 3 / 4), y.saturating_add((24.0 * scale) as i32))
}

/// 物理坐标 (x, y) 所在显示器（不在任何显示器上时取主显示器）的缩放比例。
/// 窗口还没创建时 `Window::scale_factor()` 恒为 1，定位新窗口要用它。
pub fn scale_at(x: i32, y: i32) -> f32 {
    // SAFETY: 只读查询。
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY);
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx > 0 { dx as f32 / 96.0 } else { 1.0 }
    }
}

/// 主显示器的工作区（不含任务栏）：(left, top, right, bottom)，物理像素。
pub fn work_area() -> (i32, i32, i32, i32) {
    let mut rect = RECT::default();
    // SAFETY: 只读查询，rect 在调用期间有效。
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut rect as *mut RECT as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    if ok.is_ok() { (rect.left, rect.top, rect.right, rect.bottom) } else { (0, 0, 1280, 720) }
}

/// 在窗口显示之前调用。大小通过 `initial-width/height`（即 preferred-width/height）设置：
/// 窗口显示前直接 `set_size` 会被首选大小覆盖。
pub fn apply(ui: &AppWindow, settings: &Settings) {
    let Some(saved) = settings.window else { return };
    let (pos, size) = fit(saved, on_monitor);
    ui.set_initial_width(size.0);
    ui.set_initial_height(size.1);
    let window = ui.window();
    if let Some((x, y)) = pos {
        window.set_position(PhysicalPosition::new(x, y));
    }
    if settings.maximized {
        window.set_maximized(true);
    }
}

/// 把当前窗口状态写进 `settings`。由定时器周期调用并在退出时再调用一次，因此：
/// 普通状态下 `settings.window` 总是最近一次的普通矩形；最小化时位置无意义，保持原值；
/// 最大化时只更新 `maximized`，保留最大化之前记下的普通矩形（从未记录过才退而取当前矩形）。
pub fn capture(ui: &AppWindow, settings: &mut Settings) {
    let window = ui.window();
    if window.is_minimized() {
        return;
    }
    settings.maximized = window.is_maximized();
    if settings.maximized && settings.window.is_some() {
        return;
    }
    let (pos, size) = (window.position(), window.size().to_logical(window.scale_factor()));
    if size.width > 0.0 && size.height > 0.0 {
        settings.window = Some(WindowRect { x: pos.x, y: pos.y, width: size.width, height: size.height });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: WindowRect = WindowRect { x: 100, y: 50, width: 1000.0, height: 700.0 };

    #[test]
    fn 在屏幕内时原样恢复() {
        assert_eq!(fit(R, |_, _, _, _| true), (Some((100, 50)), (1000.0, 700.0)));
    }

    #[test]
    fn 显示器不存在时丢弃位置但保留大小() {
        assert_eq!(fit(R, |_, _, _, _| false), (None, (1000.0, 700.0)));
    }

    #[test]
    fn 探测区在窗口左上角附近() {
        let probe = std::cell::Cell::new((0, 0, 0, 0));
        fit(R, |l, t, r, b| {
            probe.set((l, t, r, b));
            true
        });
        assert_eq!(probe.get(), (268, 50, 332, 82));
    }

    #[test]
    fn 副屏在左侧负坐标时可恢复() {
        // 主屏 0..1920，副屏 -1920..0。
        let screens = |l: i32, _t: i32, r: i32, _b: i32| r > -1920 && l < 1920;
        let saved = WindowRect { x: -1500, ..R };
        assert_eq!(fit(saved, screens).0, Some((-1500, 50)));
        let gone = |l: i32, _t: i32, r: i32, _b: i32| r > 0 && l < 1920;
        assert_eq!(fit(saved, gone).0, None);
    }

    #[test]
    fn 尺寸夹到合理范围() {
        let tiny = WindowRect { width: 5.0, height: 5.0, ..R };
        assert_eq!(fit(tiny, |_, _, _, _| true).1, (320.0, 240.0));
        let huge = WindowRect { width: 1e9, height: 1e9, ..R };
        assert_eq!(fit(huge, |_, _, _, _| true).1, (16384.0, 16384.0));
        let edge = WindowRect { x: i32::MAX, y: i32::MIN, ..R };
        fit(edge, |_, _, _, _| true);
    }
}

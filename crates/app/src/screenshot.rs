//! 开发用截图模式：`--screenshot <目录>`。窗口放在屏幕外，依次切换界面状态并离屏截图，
//! 用于视觉验收，不需要操作桌面。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer};

use crate::{AppWindow, Columns};

type Step = (&'static str, fn(&AppWindow));

const STEPS: &[Step] = &[
    ("01-进程列表-深色", |_| {}),
    ("02-树形与搜索-深色", |ui| {
        ui.set_tree_mode(true);
        ui.invoke_tree_changed(true);
        ui.set_filter("svchost".into());
        ui.invoke_filter_edited("svchost".into());
    }),
    ("03-性能CPU-深色", |ui| {
        ui.set_tree_mode(false);
        ui.invoke_tree_changed(false);
        ui.set_filter("".into());
        ui.invoke_filter_edited("".into());
        ui.set_page(1);
        ui.invoke_resource_selected("cpu".into());
    }),
    ("04-性能内存-深色", |ui| ui.invoke_resource_selected("memory".into())),
    ("05-性能网络-深色", |ui| ui.invoke_resource_selected("network".into())),
    ("06-结束进程树确认-深色", |ui| {
        ui.set_page(0);
        ui.set_confirm_name("chrome.exe".into());
        ui.set_confirm_descendants(12);
        ui.set_confirm_critical(false);
        ui.set_confirm_kind(2);
    }),
    ("07-进程列表-浅色", |ui| {
        ui.set_confirm_kind(0);
        ui.invoke_set_dark(false);
    }),
    ("08-性能CPU-浅色", |ui| {
        ui.set_page(1);
        ui.invoke_resource_selected("cpu".into());
    }),
    // 用真实的指针事件拖动表头：“内存”列左边缘向左 60 像素，“PID”列向左 30 像素。
    ("09-列宽拖动-深色", |ui| {
        ui.invoke_set_dark(true);
        ui.set_page(0);
        let cols = ui.global::<Columns>();
        let right = ui.window().size().to_logical(ui.window().scale_factor()).width - 10.0;
        let left_of = |widths: f32| right - widths;
        // 列从右向左排列：磁盘、内存、CPU、PID。
        let memory_left = left_of(cols.get_disk() + cols.get_memory());
        drag(ui, memory_left, -60.0);
        let pid_left = left_of(cols.get_disk() + cols.get_memory() + cols.get_cpu() + cols.get_pid());
        drag(ui, pid_left, -30.0);
    }),
];

/// 在表头（y = 56 + 17）的 `x` 处按下并水平拖动 `dx` 逻辑像素。
fn drag(ui: &AppWindow, x: f32, dx: f32) {
    let y = 56.0 + 17.0;
    let w = ui.window();
    let at = |x: f32| LogicalPosition::new(x, y);
    // 抓在列边缘右侧 2 像素处，落在 8 像素宽的拖动区内。
    let start = x + 2.0;
    w.dispatch_event(WindowEvent::PointerMoved { position: at(start) });
    w.dispatch_event(WindowEvent::PointerPressed { position: at(start), button: PointerEventButton::Left });
    for i in 1..=6 {
        w.dispatch_event(WindowEvent::PointerMoved { position: at(start + dx * i as f32 / 6.0) });
    }
    w.dispatch_event(WindowEvent::PointerReleased { position: at(start + dx), button: PointerEventButton::Left });
}

/// 先采样一段时间让曲线有内容，再逐步截图，最后退出。
pub fn run(ui: &AppWindow, dir: PathBuf, warmup: Duration) {
    ui.window().set_position(PhysicalPosition::new(-6000, -6000));
    ui.invoke_set_dark(true);
    ui.set_interval_index(0);
    ui.invoke_interval_changed(0);
    let _ = std::fs::create_dir_all(&dir);
    let weak = ui.as_weak();
    Timer::single_shot(warmup, move || step(weak, Rc::new(dir), 0));
}

fn step(weak: slint::Weak<AppWindow>, dir: Rc<PathBuf>, i: usize) {
    let Some(ui) = weak.upgrade() else { return };
    let Some(&(name, apply)) = STEPS.get(i) else {
        let _ = slint::quit_event_loop();
        return;
    };
    apply(&ui);
    // 等待重新布局与渲染后再截图。
    Timer::single_shot(Duration::from_millis(400), move || {
        let Some(ui) = weak.upgrade() else { return };
        match ui.window().take_snapshot() {
            Ok(buf) => {
                let path = dir.join(format!("{name}.bmp"));
                if let Err(e) = write_bmp(&path, &buf) {
                    eprintln!("写入 {} 失败：{e}", path.display());
                }
            }
            Err(e) => eprintln!("截图失败：{e}"),
        }
        step(weak, dir, i + 1);
    });
}

/// 写 32 位自上而下的 BMP，避免为开发工具引入图像编码依赖。
fn write_bmp(path: &Path, buf: &SharedPixelBuffer<Rgba8Pixel>) -> std::io::Result<()> {
    let (w, h) = (buf.width(), buf.height());
    let pixels = w as usize * h as usize * 4;
    let mut out = Vec::with_capacity(54 + pixels);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pixels as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for p in buf.as_slice() {
        out.extend_from_slice(&[p.b, p.g, p.r, 255]);
    }
    std::fs::write(path, out)
}

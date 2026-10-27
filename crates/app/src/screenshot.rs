//! 开发用截图模式：`--screenshot <目录>`。窗口放在屏幕外，依次切换界面状态并离屏截图，
//! 用于视觉验收，不需要操作桌面。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, Model, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer};

use crate::{AppWindow, Columns};

type Step = (&'static str, fn(&AppWindow));

const STEPS: &[Step] = &[
    ("01-进程列表-深色", |_| {}),
    ("02-树形与搜索-深色", |ui| {
        ui.set_view_mode(1);
        ui.invoke_view_changed(1);
        ui.set_filter("svchost".into());
        ui.invoke_filter_edited("svchost".into());
    }),
    ("03-性能CPU-深色", |ui| {
        ui.set_view_mode(0);
        ui.invoke_view_changed(0);
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
    // 窗口缩到接近最小宽度、四列都是最大宽度（例如最大化时拉宽后又缩小窗口）：
    // 名称列至少保留 180px，数值列按比例缩小，表头与数据列对齐。
    ("10-窄窗口超宽列宽-深色", |ui| {
        ui.window().set_size(LogicalSize::new(780.0, 560.0));
        let cols = ui.global::<Columns>();
        cols.set_pid(480.0);
        cols.set_cpu(480.0);
        cols.set_memory(480.0);
        cols.set_disk(480.0);
    }),
    // 同样的设定值回到宽窗口：保存值没有被改动，列宽恢复为设定的 480。
    ("11-窗口放大后列宽恢复-深色", |ui| {
        ui.window().set_size(LogicalSize::new(2000.0, 700.0));
    }),
    // 分组视图：应用 / 后台进程两组，各有标题行与数量。
    ("12-分组视图-深色", |ui| {
        ui.window().set_size(LogicalSize::new(1040.0, 700.0));
        let cols = ui.global::<Columns>();
        cols.set_pid(cols.get_default_pid());
        cols.set_cpu(cols.get_default_cpu());
        cols.set_memory(cols.get_default_memory());
        cols.set_disk(cols.get_default_disk());
        ui.set_page(0);
        ui.set_view_mode(2);
        ui.invoke_view_changed(2);
    }),
    ("13-分组视图-浅色", |ui| ui.invoke_set_dark(false)),
    ("14-分组与搜索-深色", |ui| {
        ui.invoke_set_dark(true);
        ui.set_filter("conhost".into());
        ui.invoke_filter_edited("conhost".into());
    }),
    // 键盘：从最后一个应用按 ↓，越过“后台进程”标题行，选中后台组的第一行。
    ("15-分组键盘下移跳过标题-深色", |ui| {
        ui.set_filter("".into());
        ui.invoke_filter_edited("".into());
        // 按 PID 升序：CPU 排序下行会随采样跳动，选中项可能被挤出可视范围。
        ui.invoke_sort_by(1);
        let at = group_boundary(ui) - 1;
        ui.set_selected(at as i32);
        ui.invoke_select(at as i32);
        press(ui, Key::DownArrow);
    }),
    // 键盘：选中第一个应用按 ↑，停在原处而不是落到“应用”标题行。
    ("16-分组键盘上移不选标题-深色", |ui| {
        ui.set_selected(1);
        ui.invoke_select(1);
        press(ui, Key::UpArrow);
    }),
];

/// 第二个标题行（“后台进程”）在列表中的下标。
fn group_boundary(ui: &AppWindow) -> usize {
    let rows = ui.get_rows();
    (1..rows.row_count()).find(|&i| rows.row_data(i).is_some_and(|r| r.header)).unwrap_or(1)
}

/// 向窗口发送一次按键。
fn press(ui: &AppWindow, key: Key) {
    let w = ui.window();
    let text: slint::SharedString = key.into();
    w.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    w.dispatch_event(WindowEvent::KeyReleased { text });
}

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

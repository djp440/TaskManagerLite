//! 开发用截图模式：`--screenshot <目录>`。窗口放在屏幕外，依次切换界面状态并离屏截图，
//! 用于视觉验收，不需要操作桌面。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, LogicalSize, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer};

use crate::{AppWindow, Columns, ProcessMenu};

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
    // 编号从 12 起，避开其他功能新增的步骤；每步显式设置主题与窗口尺寸，不依赖前一步的状态。
    ("12-性能内存-浅色", |ui| {
        resize(ui, 1040.0, 700.0);
        ui.invoke_set_dark(false);
        ui.set_page(1);
        ui.invoke_resource_selected("memory".into());
    }),
    ("13-性能内存-深色-最小尺寸760x480", |ui| {
        ui.invoke_set_dark(true);
        resize(ui, 760.0, 480.0);
    }),
    ("14-性能内存-深色-900x600", |ui| resize(ui, 900.0, 600.0)),
    ("15-性能内存-深色-默认尺寸", |ui| resize(ui, 1040.0, 700.0)),
    // 进程图标：浏览器（有自己的图标）。依赖本机正在运行 chrome，没有时列表为空。
    ("16-进程图标-chrome-深色", |ui| {
        resize(ui, 1040.0, 700.0);
        ui.invoke_set_dark(true);
        ui.set_page(0);
        ui.set_filter("chrome".into());
        ui.invoke_filter_edited("chrome".into());
    }),
    // 通用图标：没有图标资源（svchost、RuntimeBroker）或拿不到路径（System）的进程都显示同一个默认图标。
    ("17-进程图标-默认图标-svchost-深色", |ui| {
        ui.set_filter("svchost".into());
        ui.invoke_filter_edited("svchost".into());
    }),
    ("18-进程图标-默认图标-RuntimeBroker-深色", |ui| {
        ui.set_filter("RuntimeBroker".into());
        ui.invoke_filter_edited("RuntimeBroker".into());
    }),
    ("19-进程图标-默认图标-System-深色", |ui| {
        ui.set_filter("System".into());
        ui.invoke_filter_edited("System".into());
    }),
];

/// `--menu-shot <过滤>[:pid]:<dark|light>`：切换主题、过滤并（`:pid` 时按 PID 升序，PID 4 排第一）
/// 在第一行右键，弹出原生右键菜单。原生菜单是系统窗口，`take_snapshot` 截不到，
/// 由 `scripts/menu-screenshots.ps1` 从外部抓取，所以这里不退出，菜单会一直阻塞到被脚本结束进程。
pub fn run_menu(ui: &AppWindow, spec: &str, warmup: Duration) {
    ui.window().set_position(PhysicalPosition::new(-6000, -6000));
    let parts: Vec<&str> = spec.split(':').collect();
    let filter = parts.first().copied().unwrap_or("").to_string();
    let by_pid = parts.contains(&"pid");
    let dark = !parts.contains(&"light");
    let weak = ui.as_weak();
    Timer::single_shot(warmup, move || {
        let Some(ui) = weak.upgrade() else { return };
        // 暂停采样，列表不再重排。
        ui.set_interval_index(3);
        ui.invoke_interval_changed(3);
        ui.invoke_set_dark(dark);
        ui.set_filter(filter.as_str().into());
        ui.invoke_filter_edited(filter.as_str().into());
        if by_pid {
            ui.invoke_sort_by(1);
        }
        // 等列表布局完成后，在第一行名称列上右键（顶栏 56 + 表头 34 + 半行 15）。
        Timer::single_shot(Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else { return };
            let at = LogicalPosition::new(120.0, 56.0 + 34.0 + 15.0);
            let w = ui.window();
            w.dispatch_event(WindowEvent::PointerMoved { position: at });
            w.dispatch_event(WindowEvent::PointerPressed { position: at, button: PointerEventButton::Right });
            w.dispatch_event(WindowEvent::PointerReleased { position: at, button: PointerEventButton::Right });
        });
    });
}

/// `--menu-action <动作>:<pid>`：选中指定 PID 的进程并直接触发一个右键菜单动作
/// （`open-location`、`properties`、`copy-path`、`copy-command-line`），用于 `scripts/test-menu-actions.ps1`
/// 自动验收，免去点击原生菜单。动作执行后不退出，由脚本检查结果并结束进程。
pub fn run_menu_action(ui: &AppWindow, spec: &str, warmup: Duration) {
    ui.window().set_position(PhysicalPosition::new(-6000, -6000));
    let (action, pid) = spec.split_once(':').unwrap_or((spec, "0"));
    let (action, pid) = (action.to_string(), pid.parse::<i32>().unwrap_or(0));
    let weak = ui.as_weak();
    Timer::single_shot(warmup, move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_filter(pid.to_string().as_str().into());
        ui.invoke_filter_edited(pid.to_string().as_str().into());
        Timer::single_shot(Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else { return };
            let rows = ui.get_rows();
            let Some(i) = (0..rows.row_count()).find(|&i| rows.row_data(i).is_some_and(|r| r.pid == pid)) else {
                eprintln!("列表中没有 pid {pid}");
                return;
            };
            ui.invoke_select(i as i32);
            let menu = ui.global::<ProcessMenu>();
            match action.as_str() {
                "open-location" => menu.invoke_open_location(),
                "properties" => menu.invoke_show_properties(),
                "copy-path" => menu.invoke_copy_path(),
                "copy-command-line" => menu.invoke_copy_command_line(),
                other => eprintln!("未知动作 {other}"),
            }
        });
    });
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

fn resize(ui: &AppWindow, width: f32, height: f32) {
    ui.window().set_size(LogicalSize::new(width, height));
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

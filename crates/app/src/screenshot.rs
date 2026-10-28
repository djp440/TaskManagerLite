//! 开发用截图模式：`--screenshot <目录>`。窗口放在屏幕外，依次切换界面状态并离屏截图，
//! 用于视觉验收，不需要操作桌面。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, Model, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer};

use crate::{AppWindow, Columns, CpuData, ProcessMenu};

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
    ("03b-性能CPU逻辑处理器-深色", |ui| use_demo_cores(ui, 0)),
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
        ui.global::<CpuData>().set_per_core(false);
        ui.set_page(1);
        ui.invoke_resource_selected("cpu".into());
    }),
    ("08b-性能CPU逻辑处理器-浅色", |ui| ui.global::<CpuData>().set_per_core(true)),
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
    // CPU 页（编号接在进程图标步骤之后）：整体曲线与逻辑处理器网格在最小尺寸、中等尺寸下的表现（逐核曲线用演示数据，便于核对形态）。
    ("20-性能CPU整体-深色-最小尺寸760x480", |ui| {
        ui.invoke_set_dark(true);
        ui.set_filter("".into());
        ui.invoke_filter_edited("".into());
        ui.set_page(1);
        ui.invoke_resource_selected("cpu".into());
        ui.global::<CpuData>().set_per_core(false);
        resize(ui, 760.0, 480.0);
    }),
    ("21-性能CPU整体-深色-900x600", |ui| resize(ui, 900.0, 600.0)),
    ("22-性能CPU逻辑处理器-深色-最小尺寸760x480", |ui| {
        use_demo_cores(ui, 0);
        resize(ui, 760.0, 480.0);
    }),
    ("23-性能CPU逻辑处理器-深色-900x600", |ui| resize(ui, 900.0, 600.0)),
    ("24-性能CPU逻辑处理器-浅色-最小尺寸760x480", |ui| {
        ui.invoke_set_dark(false);
        resize(ui, 760.0, 480.0);
    }),
    ("25-性能CPU整体-浅色-900x600", |ui| {
        ui.global::<CpuData>().set_per_core(false);
        resize(ui, 900.0, 600.0);
    }),
    // 核数很多（假装 64 个逻辑处理器）：小窗口里小图保持最小高度、网格区滚动；大窗口里放得下就不滚动。
    ("26-性能CPU64核-深色-最小尺寸760x480", |ui| {
        ui.invoke_set_dark(true);
        use_demo_cores(ui, 64);
        resize(ui, 760.0, 480.0);
    }),
    ("27-性能CPU64核-深色-1040x700", |ui| resize(ui, 1040.0, 700.0)),
    // 分组视图：应用 / 后台进程两组，各有标题行与数量；应用行与后台行各自显示图标，标题行没有图标。
    ("28-分组视图-深色", |ui| {
        ui.window().set_size(LogicalSize::new(1040.0, 700.0));
        ui.invoke_set_dark(true);
        ui.set_filter("".into());
        ui.invoke_filter_edited("".into());
        let cols = ui.global::<Columns>();
        cols.set_pid(cols.get_default_pid());
        cols.set_cpu(cols.get_default_cpu());
        cols.set_memory(cols.get_default_memory());
        cols.set_disk(cols.get_default_disk());
        ui.set_page(0);
        ui.set_view_mode(2);
        ui.invoke_view_changed(2);
    }),
    ("29-分组视图-浅色", |ui| {
        ui.invoke_set_dark(false);
        right_click_header_does_not_select(ui);
    }),
    ("30-分组与搜索-深色", |ui| {
        ui.invoke_set_dark(true);
        ui.set_filter("conhost".into());
        ui.invoke_filter_edited("conhost".into());
    }),
    // 键盘：从最后一个应用按 ↓，越过“后台进程”标题行，选中后台组的第一行。
    ("31-分组键盘下移跳过标题-深色", |ui| {
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
    ("32-分组键盘上移不选标题-深色", |ui| {
        ui.set_selected(1);
        ui.invoke_select(1);
        press(ui, Key::UpArrow);
    }),
    // 工具栏在窄窗口下的收缩。提权状态显式设置，不依赖运行者的权限（未提权才有“以管理员身份重启”）。
    ("33-工具栏-进程-未提权-深色-760x600", |ui| toolbar(ui, 0, false, true, 760.0)),
    ("34-工具栏-进程-未提权-深色-900x600", |ui| toolbar(ui, 0, false, true, 900.0)),
    ("35-工具栏-进程-未提权-深色-1000x600", |ui| toolbar(ui, 0, false, true, 1000.0)),
    ("36-工具栏-进程-未提权-深色-1040x600", |ui| toolbar(ui, 0, false, true, 1040.0)),
    ("37-工具栏-进程-已提权-深色-760x600", |ui| toolbar(ui, 0, true, true, 760.0)),
    ("38-工具栏-进程-已提权-深色-1000x600", |ui| toolbar(ui, 0, true, true, 1000.0)),
    ("39-工具栏-性能-未提权-深色-760x600", |ui| toolbar(ui, 1, false, true, 760.0)),
    ("40-工具栏-进程-未提权-浅色-760x600", |ui| toolbar(ui, 0, false, false, 760.0)),
    ("41-工具栏-进程-未提权-浅色-900x600", |ui| toolbar(ui, 0, false, false, 900.0)),
    // 置顶开关：按下态与实心图钉。
    ("42-工具栏-已置顶-深色-1040x600", |ui| {
        toolbar(ui, 0, false, true, 1040.0);
        ui.set_pinned(true);
    }),
    ("43-工具栏-已置顶-浅色-760x600", |ui| toolbar(ui, 0, false, false, 760.0)),
    ("44-工具栏-已置顶-性能-深色-760x600", |ui| toolbar(ui, 1, true, true, 760.0)),
    // 指针停在收起后的提权按钮上：先是悬停态，约 0.5 秒后出现完整文字的提示（下一步截图）。放在最后，免得悬停影响其他步骤。
    ("45-工具栏-提权图标悬停-深色-900x600", |ui| {
        toolbar(ui, 0, false, true, 900.0);
        ui.set_pinned(false);
        hover_compact_elevate(ui);
    }),
    ("46-工具栏-提权图标提示-深色-900x600", |_| {}),
    // 指针移到置顶按钮上（提权图标左侧 32 + 8）：约 0.5 秒后出现提示。
    ("47-工具栏-置顶按钮悬停-深色-900x600", |ui| {
        let width = ui.window().size().to_logical(ui.window().scale_factor()).width;
        let at = LogicalPosition::new(width - 12.0 - 32.0 - 8.0 - 16.0, 28.0);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: at });
    }),
    ("48-工具栏-置顶按钮提示-深色-900x600", |_| {}),
];

/// 工具栏截图的统一设定：页面、提权状态、主题与窗口宽度（高 600），平铺视图、无搜索词。
fn toolbar(ui: &AppWindow, page: i32, elevated: bool, dark: bool, width: f32) {
    ui.invoke_set_dark(dark);
    ui.set_elevated(elevated);
    ui.set_page(page);
    ui.set_view_mode(0);
    ui.invoke_view_changed(0);
    ui.set_filter("".into());
    ui.invoke_filter_edited("".into());
    resize(ui, width, 600.0);
}

/// 把指针移到收起后的提权按钮中心：顶栏高 56，右内边距 12，按钮 32×32。
fn hover_compact_elevate(ui: &AppWindow) {
    assert!(ui.get_toolbar_elevate_compact(), "900 宽时提权按钮应已收成图标");
    let width = ui.window().size().to_logical(ui.window().scale_factor()).width;
    let at = LogicalPosition::new(width - 12.0 - 16.0, 28.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at });
}

/// 在第一个标题行上右键：不能选中任何行，也不能弹出进程菜单（弹出会阻塞，截图流程会卡住）。
fn right_click_header_does_not_select(ui: &AppWindow) {
    let rows = ui.get_rows();
    assert!(rows.row_data(0).is_some_and(|r| r.header), "第一行应是“应用”标题行");
    let before = ui.get_selected();
    let w = ui.window();
    // 顶栏 56 + 表头 34 + 半行 15；在名称列和展开箭头位置各右键一次。
    for x in [120.0, 15.0] {
        let at = LogicalPosition::new(x, 56.0 + 34.0 + 15.0);
        w.dispatch_event(WindowEvent::PointerMoved { position: at });
        w.dispatch_event(WindowEvent::PointerPressed { position: at, button: PointerEventButton::Right });
        w.dispatch_event(WindowEvent::PointerReleased { position: at, button: PointerEventButton::Right });
    }
    assert_eq!(ui.get_selected(), before, "右键标题行不应改变选中项");
    eprintln!("右键标题行：未选中、未弹出菜单");
}

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

/// 切到逻辑处理器网格，并让各核显示演示曲线：核 0 恒 100%（贴顶边的横线）、核 1 从 0 升到 100%
/// （左下到右上的斜线）、核 2 方波、核 3 恒 50%、核 4 正弦……用来核对曲线是否按格子实际尺寸铺满。
/// `cores` 为 0 时用真实的逻辑处理器数量，否则假装有这么多个。
fn use_demo_cores(ui: &AppWindow, cores: usize) {
    crate::CONTROLLER.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.use_demo_cpu_history(cores);
        }
    });
    ui.global::<CpuData>().set_per_core(true);
}


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
/// 自动验收，免去点击原生菜单。末尾可加 `:group` 先切到分组视图（验证分组视图里选中的是正确的进程）。
/// 动作执行后不退出，由脚本检查结果并结束进程。
pub fn run_menu_action(ui: &AppWindow, spec: &str, warmup: Duration) {
    ui.window().set_position(PhysicalPosition::new(-6000, -6000));
    let parts: Vec<&str> = spec.split(':').collect();
    let action = parts.first().copied().unwrap_or("").to_string();
    let pid = parts.get(1).and_then(|p| p.parse::<i32>().ok()).unwrap_or(0);
    let group = parts.contains(&"group");
    let weak = ui.as_weak();
    Timer::single_shot(warmup, move || {
        let Some(ui) = weak.upgrade() else { return };
        if group {
            ui.set_view_mode(2);
            ui.invoke_view_changed(2);
        }
        ui.set_filter(pid.to_string().as_str().into());
        ui.invoke_filter_edited(pid.to_string().as_str().into());
        // 分组视图要等带窗口信息的快照回来才会出现，多等一会儿。
        let settle = if group { 1500 } else { 500 };
        Timer::single_shot(Duration::from_millis(settle), move || {
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
        // 出图前核对逻辑处理器网格的曲线路径尺寸是最新的；不一致就让截图直接失败，不留下错误的图。
        crate::CONTROLLER.with(|c| {
            if let Some(c) = c.borrow().as_ref()
                && let Err(e) = c.check_cpu_paths()
            {
                panic!("截图步骤 {name}：{e}");
            }
        });
        // 工具栏的最小宽度超过窗口宽度时，右侧控件会被窗口裁掉。
        let width = ui.window().size().to_logical(ui.window().scale_factor()).width;
        let need = ui.get_toolbar_min_width();
        if need > width + 0.5 {
            crate::CONTROLLER.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    c.log.borrow_mut().line(format!("截图步骤 {name}：工具栏最小宽度 {need} 超出窗口宽度 {width}"));
                }
            });
            panic!("截图步骤 {name}：工具栏最小宽度 {need} 超出窗口宽度 {width}");
        }
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

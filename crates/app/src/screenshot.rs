//! 开发用截图模式：`--screenshot <目录>`。窗口放在屏幕外，依次切换界面状态并离屏截图，
//! 用于视觉验收，不需要操作桌面。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, PhysicalPosition, Rgba8Pixel, SharedPixelBuffer, Timer};

use crate::AppWindow;

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
];

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

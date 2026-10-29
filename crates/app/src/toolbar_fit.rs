//! 工具栏在所有窗口宽度下都放得下：用最小软件窗口（不打开真实窗口）创建界面，逐个宽度检查布局。
//! 文字宽度来自系统字体，与 software 渲染的真实窗口一致。

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalSize, PlatformError};

use crate::AppWindow;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

/// 三种页面、两种提权状态下，从最小宽度 760 到 1600 逐像素检查：工具栏最小宽度不超过窗口宽度
/// （即没有控件被裁剪）；窗口变宽时档位只会放宽；默认宽度 1080 及以上完整显示品牌字样与提权按钮文字。
#[test]
fn toolbar_fits_every_width() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).expect("每个测试线程只设置一次平台");
    let ui = AppWindow::new().expect("创建界面");
    for page in [0, 1, 2, 3, 4] {
        for elevated in [false, true] {
            ui.set_page(page);
            ui.set_elevated(elevated);
            settle(&ui);
            // 档位：(显示置顶按钮, 显示品牌, 提权按钮显示文字)；已提权时没有提权按钮。
            let mut stages: Vec<(u32, (bool, bool, bool))> = Vec::new();
            for w in 760..=1600u32 {
                ui.window().set_size(LogicalSize::new(w as f32, 600.0));
                settle(&ui);
                let need = ui.get_toolbar_min_width();
                assert!(need <= w as f32 + 0.01, "页面 {page} 提权 {elevated} 宽 {w}：工具栏最小宽度 {need} 超出窗口");
                let stage = (
                    ui.get_toolbar_pin_shown(),
                    ui.get_toolbar_brand_shown(),
                    elevated || !ui.get_toolbar_elevate_compact(),
                );
                match stages.last() {
                    Some(&(_, prev)) if prev == stage => {}
                    Some(&(_, prev)) => {
                        assert!(
                            stage.0 >= prev.0 && stage.1 >= prev.1 && stage.2 >= prev.2,
                            "页面 {page} 提权 {elevated} 宽 {w}：变宽后反而收起 {prev:?} -> {stage:?}"
                        );
                        stages.push((w, stage));
                    }
                    None => stages.push((w, stage)),
                }
                if w >= 1080 {
                    assert_eq!(stage, (true, true, true), "页面 {page} 提权 {elevated} 宽 {w}：应完整显示");
                }
            }
            // 字体没加载时文字宽度为 0，上面的检查会形同虚设。
            assert!(ui.get_toolbar_min_width() > 400.0, "文字宽度异常，字体可能没有加载");
            eprintln!("页面 {page} 提权 {elevated}：(起始宽度, (显示置顶, 显示品牌, 提权按钮显示文字)) {stages:?}");
        }
    }
}

/// `if` 条件元素在渲染或处理输入时才增删实例，布局在那之前仍按旧实例计算。真实窗口每帧渲染前都会做这一步；
/// 这里不渲染，用一次无害的指针离开事件触发它。
fn settle(ui: &AppWindow) {
    ui.window().dispatch_event(WindowEvent::PointerExited);
}

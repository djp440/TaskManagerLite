//! 迷你模式（#19）：只显示资源卡片的无边框小窗口，与完整窗口二选一显示。
//!
//! - 迷你模式下只采系统指标（见 `sampling::plan`），完整窗口隐藏但保留全部页面状态。
//! - 迷你窗口默认置顶，可拖动，位置、置顶与不透明度写入设置；退出时处于迷你模式则下次启动直接进入。

use std::rc::Rc;
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, Model, ModelRc, PhysicalPosition, VecModel};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetLayeredWindowAttributes, GetWindowLongPtrW, HTCAPTION, LAYERED_WINDOW_ATTRIBUTES_FLAGS,
    LWA_ALPHA, PostMessageW, SetLayeredWindowAttributes, SetWindowLongPtrW, WM_NCLBUTTONDOWN, WS_EX_LAYERED,
};

use super::Controller;
use crate::sampling::Command;
use crate::settings::MiniSettings;
use crate::{AppWindow, MiniWindow, OpacityChoice, placement};

/// 迷你窗口的逻辑宽度（与 mini-window.slint 一致），用于第一次进入时计算默认位置。
const MINI_WIDTH: f32 = 250.0;

fn hwnd_of(window: &slint::Window) -> Option<HWND> {
    let handle = window.window_handle();
    match HasWindowHandle::window_handle(&handle).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// 设置窗口整体不透明度；100 时去掉分层样式，回到普通窗口。已是目标状态时什么也不做。
///
/// winit 不知道这个样式：显示/隐藏、切换置顶、最小化还原时它会整体重写扩展样式，把分层样式清掉，
/// 所以迷你模式期间由定时器反复调用来补回（见 `capture_mini`）。
fn set_opacity(hwnd: HWND, percent: u8) {
    // SAFETY: hwnd 是本进程刚取得的有效窗口句柄；只读写扩展样式与分层属性。
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        if percent >= 100 {
            if ex & layered != 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !layered);
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            return;
        }
        let alpha = (u32::from(percent) * 255 / 100) as u8;
        if ex & layered != 0 {
            let (mut key, mut current, mut flags) = (COLORREF(0), 0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
            if GetLayeredWindowAttributes(hwnd, Some(&mut key), Some(&mut current), Some(&mut flags)).is_ok()
                && flags.contains(LWA_ALPHA)
                && current == alpha
            {
                return;
            }
        } else {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | layered);
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

/// 让系统接管拖动（与按住标题栏拖动相同，支持贴边等系统行为）。左键已松开时不做任何事。
fn begin_system_drag(hwnd: HWND) -> bool {
    // SAFETY: 只读输入状态并向本进程自己的窗口投递消息。
    unsafe {
        // GetKeyState 反映的是逻辑按键（已考虑左右键互换）。
        if GetKeyState(i32::from(VK_LBUTTON.0)) >= 0 {
            return false;
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = ReleaseCapture();
        let lparam = ((pt.y as u32 & 0xffff) << 16 | (pt.x as u32 & 0xffff)) as isize;
        PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(lparam)).is_ok()
    }
}

fn opacity_choices(current: u8) -> Vec<OpacityChoice> {
    MiniSettings::OPACITIES
        .iter()
        .map(|&p| OpacityChoice { percent: i32::from(p), checked: p == current })
        .collect()
}

impl Controller {
    pub(super) fn bind_mini(self: &Rc<Self>, ui: &AppWindow) {
        let c = Rc::downgrade(self);
        ui.on_mini_mode(move || {
            if let Some(c) = c.upgrade() {
                c.enter_mini();
            }
        });
    }

    /// 已创建的迷你窗口（截图模式核对用）。
    pub fn mini(&self) -> Option<&MiniWindow> {
        self.mini.get()
    }

    /// 迷你窗口当前的分层不透明度（0–255）；没有分层样式时为 `None`。截图模式核对用。
    pub fn mini_alpha(&self) -> Option<u8> {
        let hwnd = self.mini.get().and_then(|m| hwnd_of(m.window()))?;
        // SAFETY: 只读查询本进程的窗口。
        unsafe {
            if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED.0 as isize == 0 {
                return None;
            }
            let (mut key, mut alpha, mut flags) = (COLORREF(0), 0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
            GetLayeredWindowAttributes(hwnd, Some(&mut key), Some(&mut alpha), Some(&mut flags)).ok()?;
            flags.contains(LWA_ALPHA).then_some(alpha)
        }
    }

    /// 是否处于迷你模式。
    pub fn is_mini(&self) -> bool {
        self.state.borrow().mini
    }

    /// 迷你窗口：第一次进入迷你模式时创建。
    fn mini_window(self: &Rc<Self>) -> Option<&MiniWindow> {
        if self.mini.get().is_none() {
            let w = match MiniWindow::new() {
                Ok(w) => w,
                Err(e) => {
                    self.log.borrow_mut().line(format!("迷你窗口创建失败 {e}"));
                    return None;
                }
            };
            let settings = self.settings.borrow().mini;
            w.set_resources(ModelRc::from(self.resources.clone()));
            w.set_pinned(settings.on_top);
            w.set_opacity_choices(ModelRc::new(VecModel::from(opacity_choices(settings.opacity))));
            let c = Rc::downgrade(self);
            w.on_restore(move || {
                if let Some(c) = c.upgrade() {
                    c.exit_mini();
                }
            });
            let c = Rc::downgrade(self);
            w.on_drag(move || {
                if let Some(c) = c.upgrade() {
                    c.drag_mini();
                }
            });
            let c = Rc::downgrade(self);
            w.on_opacity_selected(move |p| {
                if let Some(c) = c.upgrade() {
                    c.set_mini_opacity(p.clamp(0, 100) as u8);
                }
            });
            w.on_pinned_changed(|| Self::reapply_opacity_soon());
            let c = Rc::downgrade(self);
            w.on_quit(move || {
                if let Some(c) = c.upgrade() {
                    c.quit();
                }
            });
            let c = Rc::downgrade(self);
            w.window().on_close_requested(move || match c.upgrade() {
                Some(c) => c.close_requested(),
                None => slint::CloseRequestResponse::HideWindow,
            });
            let _ = self.mini.set(w);
        }
        self.mini.get()
    }

    /// 启动时按设置直接进入迷你模式（完整窗口此时还没显示过）。返回是否进入。
    pub fn start_mini(self: &Rc<Self>) -> bool {
        if self.store.is_none() || !self.settings.borrow().mini.active {
            return false;
        }
        self.show_mini()
    }

    /// 切换到迷你模式：完整窗口隐藏（页面状态保留），迷你窗口出现在上次的位置。
    pub fn enter_mini(self: &Rc<Self>) {
        if self.is_mini() {
            return;
        }
        // 记下完整窗口此刻的位置与大小，迷你模式期间不再跟踪它。
        self.track_window();
        if self.show_mini()
            && let Some(ui) = self.ui.upgrade()
        {
            let _ = ui.hide();
        }
    }

    fn show_mini(self: &Rc<Self>) -> bool {
        let Some(mini) = self.mini_window() else { return false };
        let Some(ui) = self.ui.upgrade() else { return false };
        let saved = self.settings.borrow().mini.position;
        // 缩放取目标位置所在显示器的：直接以迷你模式启动时窗口都还没创建，`scale_factor()` 恒为 1。
        let pos = saved.filter(|&(x, y)| placement::reachable(x, y, MINI_WIDTH, placement::scale_at(x, y))).unwrap_or_else(|| {
            // 默认放在完整窗口的右上角内侧；完整窗口还没显示过（直接以迷你模式启动）时放到主显示器工作区右上角。
            let (p, s) = (ui.window().position(), ui.window().size());
            let scale = ui.window().scale_factor();
            let margin = (16.0 * scale) as i32;
            let near = (p.x + s.width as i32 - (MINI_WIDTH * scale) as i32 - margin, p.y + (64.0 * scale) as i32);
            if s.width > 0 && placement::reachable(near.0, near.1, MINI_WIDTH, scale) {
                near
            } else {
                let area = placement::work_area();
                let scale = placement::scale_at(area.0, area.1);
                let margin = (16.0 * scale) as i32;
                (area.2 - (MINI_WIDTH * scale) as i32 - margin, area.1 + margin)
            }
        });
        mini.window().set_position(PhysicalPosition::new(pos.0, pos.1));
        self.state.borrow_mut().mini = true;
        self.sampler.send(Command::Mini(true));
        if let Err(e) = mini.show() {
            self.log.borrow_mut().line(format!("迷你窗口显示失败 {e}"));
            self.state.borrow_mut().mini = false;
            self.sampler.send(Command::Mini(false));
            return false;
        }
        self.apply_mini_opacity();
        // 卡片数据与完整窗口共用，立即按迷你模式（不画大图）重绘一次。
        self.redraw_graphs();
        self.log.borrow_mut().line(format!("进入迷你模式 位置={pos:?}"));
        true
    }

    /// 回到完整模式：完整窗口原样出现（页签、选中项、滚动位置都不变），随即补一次完整采样。
    pub fn exit_mini(&self) {
        if !self.is_mini() {
            return;
        }
        let (Some(mini), Some(ui)) = (self.mini.get(), self.ui.upgrade()) else { return };
        self.capture_mini();
        self.state.borrow_mut().mini = false;
        // 采样线程收到命令后立即补采一次完整快照。
        self.sampler.send(Command::Mini(false));
        let _ = ui.show();
        let _ = mini.hide();
        self.redraw_graphs();
        tml_core::activate_own_window();
        self.log.borrow_mut().line("回到完整模式");
    }

    /// 记录迷你窗口的位置（迷你模式期间由定时器周期调用，退出迷你模式与保存设置时也调用）。
    pub(super) fn capture_mini(&self) {
        if let Some(mini) = self.mini.get()
            && self.is_mini()
            && !mini.window().is_minimized()
        {
            let p = mini.window().position();
            {
                let mut settings = self.settings.borrow_mut();
                settings.mini.position = Some((p.x, p.y));
                settings.mini.on_top = mini.get_pinned();
            }
            self.apply_mini_opacity();
        }
    }

    /// 指针在迷你窗口里按下后移动：本次事件处理完之后交给系统拖动窗口。
    fn drag_mini(&self) {
        if self.drag_pending.replace(true) {
            return;
        }
        slint::Timer::single_shot(Duration::ZERO, || {
            let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
            let Some(c) = controller else { return };
            c.drag_pending.set(false);
            let Some(mini) = c.mini.get() else { return };
            // 系统接管拖动后界面收不到松开事件：先让触摸区域结束按下状态，避免松开后仍以为在拖动。
            mini.window().dispatch_event(WindowEvent::PointerExited);
            if let Some(hwnd) = hwnd_of(mini.window()) {
                begin_system_drag(hwnd);
            }
        });
    }

    fn set_mini_opacity(&self, percent: u8) {
        let percent = if MiniSettings::OPACITIES.contains(&percent) { percent } else { 100 };
        self.settings.borrow_mut().mini.opacity = percent;
        // 逐行改写勾选状态，不替换正在执行菜单回调的模型。
        if let Some(mini) = self.mini.get() {
            let model = mini.get_opacity_choices();
            for (i, choice) in opacity_choices(percent).into_iter().enumerate() {
                model.set_row_data(i, choice);
            }
        }
        self.apply_mini_opacity();
    }

    /// 稍后补回不透明度：winit 处理完显示、置顶等窗口标志变化（会清掉分层样式）之后。
    pub(super) fn reapply_opacity_soon() {
        slint::Timer::single_shot(Duration::from_millis(30), || {
            let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
            if let Some(c) = controller {
                c.apply_mini_opacity();
            }
        });
    }

    /// 按设置应用不透明度。启动时 `show()` 之后原生窗口要等事件循环开始才创建，取不到句柄就稍后重试。
    fn apply_mini_opacity(&self) {
        let percent = self.settings.borrow().mini.opacity;
        match self.mini.get().and_then(|m| hwnd_of(m.window())) {
            Some(hwnd) => set_opacity(hwnd, percent),
            None if self.is_mini() => slint::Timer::single_shot(Duration::from_millis(50), || {
                let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
                if let Some(c) = controller {
                    c.apply_mini_opacity();
                }
            }),
            None => {}
        }
    }

    /// 当前模式的窗口（完整窗口或迷你窗口）。
    pub(super) fn with_active<R>(&self, f: impl FnOnce(&slint::Window) -> R) -> Option<R> {
        if self.is_mini() {
            self.mini.get().map(|m| f(m.window()))
        } else {
            self.ui.upgrade().map(|ui| f(ui.window()))
        }
    }
}

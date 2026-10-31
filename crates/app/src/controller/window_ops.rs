//! 应用窗口操作（#70）：分组视图应用行的右键菜单“切换到”“最大化”“最小化”“还原”。
//!
//! - 窗口句柄来自采样线程判定“应用”时顺带记下的主窗口（`Sample::app_windows`），菜单弹出前读一次
//!   `IsIconic` / `IsZoomed` 决定各项是否可用；句柄会失效，探测与操作前都由 core 校验 `IsWindow`。
//! - 目标窗口属于更高完整性级别（提权应用）时，普通权限的本程序发不出窗口命令（UIPI）：四项都置灰，标题写“需要提权”。
//!   无法判断（令牌打不开）时允许尝试，操作后窗口状态没变则在状态栏说明原因。
//! - “切换到”被前台锁定拒绝时退化为闪烁任务栏，并在状态栏说明。
//! - 非应用行（普通进程行、进程树 / 平铺视图下的行）整组菜单项隐藏。日志只记 PID、结果与错误码，不记窗口标题。

use super::process_menu::failure;
use super::*;
use crate::{ProcessMenu, texts};
use texts::WindowBlock;
use tml_core::{WindowError, WindowOp, WindowOutcome, WindowState};

const STATUS_ERROR: i32 = 5;
const STATUS_WINDOW: i32 = 14;

/// 四个窗口操作菜单项的状态；`None` 表示可用，`Some(原因)` 表示置灰。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMenu {
    /// 整组菜单项是否显示（只有分组视图的应用行显示）。
    pub visible: bool,
    pub activate: Option<WindowBlock>,
    pub maximize: Option<WindowBlock>,
    pub minimize: Option<WindowBlock>,
    pub restore: Option<WindowBlock>,
}

/// 由所选行是否为应用行与窗口探测结果得出菜单状态（纯函数）：
/// 非应用行整组隐藏；探测失败（窗口已关闭、需要提权）四项都置灰并写明原因；
/// 否则按窗口当前状态让“已处于目标状态”的项置灰。
pub fn window_menu(is_app_row: bool, probe: &Result<WindowState, WindowError>) -> WindowMenu {
    let blocked = |on: bool, why: WindowBlock| (!on).then_some(why);
    match probe {
        _ if !is_app_row => WindowMenu { visible: false, activate: None, maximize: None, minimize: None, restore: None },
        Err(e) => {
            let b = Some(WindowBlock::Error(e.clone()));
            WindowMenu { visible: true, activate: b.clone(), maximize: b.clone(), minimize: b.clone(), restore: b }
        }
        Ok(state) => {
            let ops = tml_core::window_ops_enabled(*state);
            WindowMenu {
                visible: true,
                activate: blocked(ops.activate, WindowBlock::State),
                maximize: blocked(ops.maximize, WindowBlock::State),
                minimize: blocked(ops.minimize, WindowBlock::State),
                restore: blocked(ops.restore, WindowBlock::State),
            }
        }
    }
}

/// 菜单项序号（Slint 的 `window-op(int)`）到操作。
pub fn op_from_index(i: i32) -> Option<WindowOp> {
    match i {
        0 => Some(WindowOp::Activate),
        1 => Some(WindowOp::Maximize),
        2 => Some(WindowOp::Minimize),
        3 => Some(WindowOp::Restore),
        _ => None,
    }
}

impl Controller {
    pub(super) fn bind_window_ops(self: &Rc<Self>, ui: &AppWindow) {
        let c = Rc::downgrade(self);
        ui.global::<ProcessMenu>().on_window_op(move |i| {
            if let (Some(c), Some(op)) = (c.upgrade(), op_from_index(i)) {
                c.window_op(op);
            }
        });
    }

    /// 所选行是分组视图应用行时它的主窗口句柄。
    fn selected_window(&self) -> Option<(ProcessKey, String, Option<isize>)> {
        let (key, name, _) = self.selected_process()?;
        if !self.selected_is_app_row(key) {
            return None;
        }
        let handle = self.state.borrow().app_windows.as_ref().and_then(|w| w.get(&key.pid).copied());
        Some((key, name, handle))
    }

    /// 右键菜单弹出前（由 `menu_opening` 调用）：设置窗口操作各项的显示、可用性与标题。
    /// 只有应用行才去读窗口状态与完整性级别（各一次系统调用）。
    pub(super) fn update_window_menu(&self, ui: &AppWindow) {
        let selected = self.selected_window();
        let probe = match &selected {
            Some((_, _, Some(h))) => tml_core::probe_window(*h),
            _ => Err(WindowError::Gone),
        };
        let m = window_menu(selected.is_some(), &probe);
        let menu = ui.global::<ProcessMenu>();
        menu.set_window_visible(m.visible);
        let set = |op: WindowOp, block: &Option<WindowBlock>| {
            let title: slint::SharedString = texts::window_menu_title(op, block.as_ref()).into();
            (block.is_none(), title)
        };
        let (can, title) = set(WindowOp::Activate, &m.activate);
        menu.set_can_window_activate(can);
        menu.set_window_activate_title(title);
        let (can, title) = set(WindowOp::Maximize, &m.maximize);
        menu.set_can_window_maximize(can);
        menu.set_window_maximize_title(title);
        let (can, title) = set(WindowOp::Minimize, &m.minimize);
        menu.set_can_window_minimize(can);
        menu.set_window_minimize_title(title);
        let (can, title) = set(WindowOp::Restore, &m.restore);
        menu.set_can_window_restore(can);
        menu.set_window_restore_title(title);
    }

    /// 右键菜单的窗口操作：对所选应用行的主窗口执行并在状态栏汇报。
    fn window_op(&self, op: WindowOp) {
        let action = texts::window_op_name(op);
        let Some((key, name, handle)) = self.selected_window() else {
            self.log.borrow_mut().line(format!("窗口操作 拒绝 操作={op:?} 原因=所选行不是应用"));
            self.report(STATUS_ERROR, failure(action, "所选行不是应用"));
            return;
        };
        let result = match handle {
            Some(h) => tml_core::apply_window_op(h, op),
            None => Err(WindowError::Gone),
        };
        match result {
            Ok(WindowOutcome::Done) => {
                self.log.borrow_mut().line(format!("窗口操作 成功 pid={} 操作={op:?}", key.pid));
                self.report(STATUS_WINDOW, texts::window_done(&name, op));
            }
            Ok(WindowOutcome::Flashed) => {
                self.log.borrow_mut().line(format!("窗口操作 闪烁任务栏 pid={} 操作={op:?}", key.pid));
                self.report(STATUS_WINDOW, texts::window_flashed(&name));
            }
            Err(e) => {
                self.log
                    .borrow_mut()
                    .line(format!("窗口操作 失败 pid={} 操作={op:?} 原因={e:?} 错误码={}", key.pid, e.code()));
                self.report(STATUS_ERROR, failure(action, &texts::window_error(&e)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tml_core::SystemError;

    fn all(m: &WindowMenu) -> [&Option<WindowBlock>; 4] {
        [&m.activate, &m.maximize, &m.minimize, &m.restore]
    }

    #[test]
    fn non_app_rows_hide_the_whole_group() {
        for probe in [Ok(WindowState::Normal), Err(WindowError::Gone)] {
            let m = window_menu(false, &probe);
            assert!(!m.visible);
        }
    }

    #[test]
    fn normal_window_greys_only_restore() {
        let m = window_menu(true, &Ok(WindowState::Normal));
        assert!(m.visible);
        assert_eq!(m.activate, None);
        assert_eq!(m.maximize, None);
        assert_eq!(m.minimize, None);
        assert_eq!(m.restore, Some(WindowBlock::State));
    }

    #[test]
    fn maximized_window_greys_only_maximize() {
        let m = window_menu(true, &Ok(WindowState::Maximized));
        assert_eq!((&m.activate, &m.minimize, &m.restore), (&None, &None, &None));
        assert_eq!(m.maximize, Some(WindowBlock::State));
    }

    #[test]
    fn minimized_window_greys_only_minimize() {
        let m = window_menu(true, &Ok(WindowState::Minimized));
        assert_eq!((&m.activate, &m.maximize, &m.restore), (&None, &None, &None));
        assert_eq!(m.minimize, Some(WindowBlock::State));
    }

    #[test]
    fn elevated_target_greys_everything_with_reason() {
        let m = window_menu(true, &Err(WindowError::NeedsElevation));
        assert!(m.visible);
        for b in all(&m) {
            assert_eq!(b, &Some(WindowBlock::Error(WindowError::NeedsElevation)));
        }
        assert_eq!(texts::window_menu_title(WindowOp::Activate, m.activate.as_ref()), "切换到（需要提权）");
        assert_eq!(texts::window_menu_title(WindowOp::Maximize, m.maximize.as_ref()), "最大化（需要提权）");
    }

    #[test]
    fn stale_handle_greys_everything_without_crashing() {
        // 空句柄（行里没有窗口）与失效句柄同样处理。
        let m = window_menu(true, &Err(WindowError::Gone));
        assert!(m.visible);
        for b in all(&m) {
            assert_eq!(b, &Some(WindowBlock::Error(WindowError::Gone)));
        }
        assert_eq!(texts::window_menu_title(WindowOp::Restore, m.restore.as_ref()), "还原（窗口已关闭）");
        let real = tml_core::probe_window(0x7777_7777);
        assert_eq!(real, Err(WindowError::Gone));
    }

    #[test]
    fn titles_and_messages() {
        assert_eq!(texts::window_menu_title(WindowOp::Maximize, None), "最大化");
        assert_eq!(texts::window_menu_title(WindowOp::Maximize, Some(&WindowBlock::State)), "最大化（已最大化）");
        assert_eq!(texts::window_menu_title(WindowOp::Minimize, Some(&WindowBlock::State)), "最小化（已最小化）");
        assert_eq!(texts::window_menu_title(WindowOp::Restore, Some(&WindowBlock::State)), "还原（窗口已是正常大小）");
        assert_eq!(
            texts::window_menu_title(WindowOp::Activate, Some(&WindowBlock::Error(WindowError::NotApplied))),
            "切换到（无法操作该窗口）"
        );
        assert!(texts::window_flashed("记事本").contains("闪烁任务栏"));
        assert!(texts::window_error(&WindowError::NeedsElevation).contains("管理员"));
        assert!(texts::window_error(&WindowError::System(SystemError::Win32(5))).contains("系统返回错误"));
        assert_eq!(texts::window_done("记事本", WindowOp::Minimize), "记事本 → 已最小化");
    }

    #[test]
    fn menu_indices_map_to_operations() {
        assert_eq!(op_from_index(0), Some(WindowOp::Activate));
        assert_eq!(op_from_index(1), Some(WindowOp::Maximize));
        assert_eq!(op_from_index(2), Some(WindowOp::Minimize));
        assert_eq!(op_from_index(3), Some(WindowOp::Restore));
        assert_eq!(op_from_index(4), None);
        assert_eq!(op_from_index(-1), None);
    }
}

//! 进程优先级与 CPU 亲和性（#66）：右键菜单“设置优先级”子菜单与“设置亲和性…”对话框。
//!
//! 菜单弹出前探测一次（只读句柄，不进入采样热路径）：受保护进程等不可修改的置灰，读不到优先级则不打勾。
//! 修改后立即补采一次，“优先级”列在下一次采样内更新。日志只记 PID、结果与错误码。

use super::process_menu::failure;
use super::*;
use crate::{AffinityData, CpuBox, ProcessMenu, texts};
use tml_core::{ControlState, PriorityClass, ProcessControlError};

const STATUS_ERROR: i32 = 5;
const STATUS_PRIORITY_SET: i32 = 10;
const STATUS_AFFINITY_SET: i32 = 11;

/// 菜单状态：由探测结果得出，纯函数便于单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlMenu {
    pub can_set_priority: bool,
    pub can_set_affinity: bool,
    /// 当前优先级序号（`PriorityClass::index`）；读不到为 -1。
    pub priority: i32,
}

/// 由探测结果得出菜单状态：不能打开可写句柄（受保护进程、拒绝访问）或进程已退出时两项都置灰；
/// 读不到优先级时不打勾；读不到亲和性时“设置亲和性”置灰。
pub fn control_menu(state: &Result<ControlState, ProcessControlError>) -> ControlMenu {
    match state {
        Ok(s) => ControlMenu {
            can_set_priority: s.modify.is_ok(),
            can_set_affinity: s.modify.is_ok() && s.affinity.is_some(),
            priority: s.priority.map_or(-1, |p| p.index() as i32),
        },
        Err(_) => ControlMenu { can_set_priority: false, can_set_affinity: false, priority: -1 },
    }
}

impl Controller {
    pub(super) fn bind_process_control(self: &Rc<Self>, ui: &AppWindow) {
        let menu = ui.global::<ProcessMenu>();
        let c = Rc::downgrade(self);
        menu.on_set_priority(move |i| {
            if let Some(c) = c.upgrade() {
                c.request_set_priority(i);
            }
        });
        let c = Rc::downgrade(self);
        menu.on_set_affinity(move || {
            if let Some(c) = c.upgrade() {
                c.open_affinity();
            }
        });
        let data = ui.global::<AffinityData>();
        let c = Rc::downgrade(self);
        data.on_toggle(move |i| {
            if let Some(c) = c.upgrade() {
                c.toggle_cpu(i as usize);
            }
        });
        let c = Rc::downgrade(self);
        data.on_select_all(move || {
            if let Some(c) = c.upgrade() {
                c.set_all_cpus(true);
            }
        });
        let c = Rc::downgrade(self);
        data.on_select_none(move || {
            if let Some(c) = c.upgrade() {
                c.set_all_cpus(false);
            }
        });
        let c = Rc::downgrade(self);
        data.on_submit(move || {
            if let Some(c) = c.upgrade() {
                c.submit_affinity();
            }
        });
    }

    /// 右键菜单弹出前：探测所选进程，设置“设置优先级”“设置亲和性”的可用性与当前优先级的勾选。
    pub(super) fn update_control_menu(&self, ui: &AppWindow) {
        let m = match self.selected_process() {
            Some((key, ..)) => control_menu(&tml_core::control_state(key)),
            None => control_menu(&Err(ProcessControlError::Gone)),
        };
        let menu = ui.global::<ProcessMenu>();
        menu.set_can_set_priority(m.can_set_priority);
        menu.set_can_set_affinity(m.can_set_affinity);
        menu.set_priority(m.priority);
    }

    fn request_set_priority(&self, index: i32) {
        let Some(class) = usize::try_from(index).ok().and_then(|i| PriorityClass::ALL.get(i).copied()) else {
            return;
        };
        let Some((key, name, _)) = self.selected_process() else { return };
        if class == PriorityClass::Realtime {
            // 实时可能让系统失去响应：先确认。
            self.ask(CONFIRM_REALTIME, vec![key], name, 0, false);
        } else {
            self.apply_priority(key, &name, class);
        }
    }

    /// 确认框点“设为实时”。
    pub(super) fn confirm_realtime(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let keys = std::mem::take(&mut self.state.borrow_mut().pending);
        let name = ui.get_confirm_name().to_string();
        if let Some(&key) = keys.first() {
            self.apply_priority(key, &name, PriorityClass::Realtime);
        }
    }

    fn apply_priority(&self, key: ProcessKey, name: &str, class: PriorityClass) {
        match tml_core::set_priority(key, class) {
            Ok(actual) => {
                self.log.borrow_mut().line(format!(
                    "设置优先级 成功 pid={} 请求={} 实际={}",
                    key.pid,
                    class.key(),
                    actual.key()
                ));
                self.report(STATUS_PRIORITY_SET, texts::priority_applied(name, class, actual));
            }
            Err(e) => {
                self.log.borrow_mut().line(format!(
                    "设置优先级 失败 pid={} 请求={} 错误码={}",
                    key.pid,
                    class.key(),
                    e.code()
                ));
                self.report(STATUS_ERROR, failure("设置优先级", &texts::process_control_error(&e)));
            }
        }
        // 立即补采一次，“优先级”列在下一次采样内更新。
        self.sampler.send(Command::Wake);
    }

    /// 打开“设置亲和性”对话框，勾选进程当前允许的逻辑处理器。
    fn open_affinity(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some((key, name, _)) = self.selected_process() else { return };
        match tml_core::affinity(key) {
            Ok(info) => {
                let cpus: Vec<CpuBox> = info
                    .processors()
                    .into_iter()
                    .map(|i| CpuBox { index: i as i32, on: info.is_selected(i) })
                    .collect();
                let data = ui.global::<AffinityData>();
                data.set_process_name(name.into());
                data.set_pid(key.pid as i32);
                data.set_total(info.total_processors as i32);
                data.set_truncated(info.truncated());
                data.set_selected_count(cpus.iter().filter(|c| c.on).count() as i32);
                data.set_cpus(ModelRc::new(VecModel::from(cpus)));
                data.set_error("".into());
                *self.affinity_target.borrow_mut() = Some(key);
                data.set_open(true);
                self.log.borrow_mut().line(format!(
                    "设置亲和性 打开对话框 pid={} 逻辑处理器={} 截断={}",
                    key.pid,
                    info.total_processors,
                    info.truncated()
                ));
            }
            Err(e) => {
                self.log.borrow_mut().line(format!("设置亲和性 读取失败 pid={} 错误码={}", key.pid, e.code()));
                self.report(STATUS_ERROR, failure("设置亲和性", &texts::process_control_error(&e)));
            }
        }
    }

    fn toggle_cpu(&self, i: usize) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<AffinityData>();
        let cpus = data.get_cpus();
        if let Some(mut c) = cpus.row_data(i) {
            c.on = !c.on;
            cpus.set_row_data(i, c);
        }
        data.set_selected_count(count_on(&cpus));
    }

    fn set_all_cpus(&self, on: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<AffinityData>();
        let cpus = data.get_cpus();
        for i in 0..cpus.row_count() {
            if let Some(mut c) = cpus.row_data(i) {
                c.on = on;
                cpus.set_row_data(i, c);
            }
        }
        data.set_selected_count(count_on(&cpus));
    }

    fn submit_affinity(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(key) = *self.affinity_target.borrow() else { return };
        let data = ui.global::<AffinityData>();
        let cpus = data.get_cpus();
        let selected = (0..cpus.row_count()).filter_map(|i| cpus.row_data(i)).filter(|c| c.on).map(|c| c.index as u32);
        let mask = tml_core::mask_from_processors(selected);
        let name = data.get_process_name().to_string();
        match tml_core::set_affinity(key, mask) {
            Ok(info) => {
                let (selected, available) = (info.process_mask.count_ones(), info.system_mask.count_ones());
                self.log.borrow_mut().line(format!("设置亲和性 成功 pid={} 已选={selected}/{available}", key.pid));
                data.set_error("".into());
                ui.invoke_close_affinity();
                *self.affinity_target.borrow_mut() = None;
                self.report(STATUS_AFFINITY_SET, texts::affinity_applied(&name, selected, available));
                self.sampler.send(Command::Wake);
            }
            Err(e) => {
                // 失败时对话框保持打开，把原因显示在按钮上方。
                self.log.borrow_mut().line(format!("设置亲和性 失败 pid={} 错误码={}", key.pid, e.code()));
                data.set_error(texts::process_control_error(&e).into());
            }
        }
    }
}

fn count_on(cpus: &ModelRc<CpuBox>) -> i32 {
    (0..cpus.row_count()).filter_map(|i| cpus.row_data(i)).filter(|c| c.on).count() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use tml_core::AffinityInfo;

    fn state(priority: Option<PriorityClass>, affinity: bool, modify: Result<(), ProcessControlError>) -> ControlState {
        ControlState {
            priority,
            affinity: affinity.then_some(AffinityInfo { process_mask: 0b11, system_mask: 0b1111, total_processors: 4 }),
            modify,
        }
    }

    #[test]
    fn ordinary_process_enables_both_and_checks_priority() {
        let m = control_menu(&Ok(state(Some(PriorityClass::AboveNormal), true, Ok(()))));
        assert_eq!(m, ControlMenu { can_set_priority: true, can_set_affinity: true, priority: 2 });
    }

    #[test]
    fn protected_process_is_greyed_out_but_still_shows_priority() {
        let m = control_menu(&Ok(state(Some(PriorityClass::Normal), true, Err(ProcessControlError::AccessDenied))));
        assert_eq!(m, ControlMenu { can_set_priority: false, can_set_affinity: false, priority: 3 });
    }

    #[test]
    fn unreadable_priority_is_not_checked() {
        let m = control_menu(&Ok(state(None, true, Ok(()))));
        assert_eq!(m.priority, -1);
        assert!(m.can_set_priority && m.can_set_affinity);
    }

    #[test]
    fn missing_affinity_greys_out_only_affinity() {
        let m = control_menu(&Ok(state(Some(PriorityClass::Low), false, Ok(()))));
        assert!(m.can_set_priority && !m.can_set_affinity);
    }

    #[test]
    fn gone_process_is_greyed_out() {
        let m = control_menu(&Err(ProcessControlError::Gone));
        assert_eq!(m, ControlMenu { can_set_priority: false, can_set_affinity: false, priority: -1 });
    }

    #[test]
    fn texts_cover_every_error() {
        for e in [
            ProcessControlError::Gone,
            ProcessControlError::AccessDenied,
            ProcessControlError::EmptyAffinity,
            ProcessControlError::OutsideProcessorGroup,
        ] {
            assert!(!texts::process_control_error(&e).is_empty());
        }
        assert_eq!(texts::priority_applied("a.exe", PriorityClass::High, PriorityClass::High), "a.exe → 高");
        assert!(texts::priority_applied("a.exe", PriorityClass::Realtime, PriorityClass::High).contains("降为"));
    }
}

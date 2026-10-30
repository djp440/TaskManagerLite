//! 进程优先级与 CPU 亲和性（#66）：右键菜单“设置优先级”子菜单与“设置亲和性…”对话框。
//!
//! 菜单弹出前探测一次（只读句柄，不进入采样热路径）：受保护进程等不可修改的置灰，读不到优先级则不打勾。
//! 修改后立即补采一次，“优先级”列在下一次采样内更新。日志只记 PID、结果与错误码。

use super::process_menu::failure;
use super::*;
use crate::{AffinityData, CpuBox, ProcessMenu, texts};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use tml_core::{ControlState, PriorityClass, ProcessControlError};

const STATUS_ERROR: i32 = 5;
const STATUS_PRIORITY_SET: i32 = 10;
const STATUS_AFFINITY_SET: i32 = 11;
const STATUS_ECO: i32 = 12;

/// 效率模式（#68）的应用层状态。
///
/// 优先级联动策略：开启效率模式 = 节流 + 优先级降为“低”。本程序按 PID + 创建时间记住**开启前**的优先级
/// （`memory`）；关闭时：
/// - 当前优先级仍是“低”（开启时设的，用户没动过）：恢复到记住的优先级，记不到（本程序重启后才看到的、
///   别处开启的进程）则恢复为“正常”；
/// - 当前优先级已不是“低”（用户在开启期间手动改过）：不覆盖用户的选择，只关闭节流。
#[derive(Default)]
pub struct EcoState {
    memory: RefCell<HashMap<ProcessKey, PriorityClass>>,
    /// 等待确认的批量操作是开启（true）还是关闭（false），目标进程在 `State::pending` 里。
    pending_on: Cell<bool>,
}

/// 关闭效率模式时应恢复到的优先级；`None` 表示保持当前优先级不动。纯函数便于单测。
pub fn eco_restore_target(current: Option<PriorityClass>, remembered: Option<PriorityClass>) -> Option<PriorityClass> {
    match current {
        Some(PriorityClass::Low) => Some(remembered.unwrap_or(PriorityClass::Normal)),
        _ => None,
    }
}

/// “效率模式”菜单项的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcoMenu {
    /// 系统不支持（Windows 10 1709 以下）时整项隐藏。
    pub visible: bool,
    pub enabled: bool,
    pub checked: bool,
    /// 置灰原因；`Some(true)` 为系统关键进程或本程序自身，`Some(false)` 为没有权限。
    pub disabled_reason: Option<bool>,
}

/// 由系统支持情况、进程是否关键 / 受保护与探测结果得出菜单状态（纯函数）。
/// 勾选取自读回的状态；读不到状态但能修改时可点、不打勾。
pub fn eco_menu(
    supported: bool,
    protected: bool,
    state: &Result<ControlState, ProcessControlError>,
) -> EcoMenu {
    if !supported {
        return EcoMenu { visible: false, enabled: false, checked: false, disabled_reason: None };
    }
    if protected {
        return EcoMenu { visible: true, enabled: false, checked: false, disabled_reason: Some(true) };
    }
    match state {
        Ok(s) if s.modify.is_ok() => EcoMenu {
            visible: true,
            enabled: true,
            checked: s.eco == Some(true),
            disabled_reason: None,
        },
        Ok(s) => EcoMenu { visible: true, enabled: false, checked: s.eco == Some(true), disabled_reason: Some(false) },
        Err(_) => EcoMenu { visible: true, enabled: false, checked: false, disabled_reason: Some(false) },
    }
}

/// 应用行的效率模式口径与“结束进程树”一致：该应用及其全部后代；桌面外壳 explorer.exe 不收纳后代，
/// 只作用于它自己。受保护的（PID 0 / 4、本程序自身、关键进程）不参与。纯函数便于单测。
pub fn eco_targets(
    snapshot: &tml_core::ProcessSnapshot,
    root: ProcessKey,
    is_app_row: bool,
    own_pid: u32,
) -> Vec<ProcessKey> {
    let keys = match snapshot.processes.iter().find(|p| p.key == root) {
        Some(p) if is_app_row && tml_core::adopts_descendants(p) => tml_core::tree_from(snapshot, root),
        Some(_) => vec![root],
        None => Vec::new(),
    };
    keys.into_iter()
        .filter(|k| {
            snapshot
                .processes
                .iter()
                .find(|p| p.key == *k)
                .is_some_and(|p| !p.critical && !tml_core::eco_protected_pid(k.pid, own_pid))
        })
        .collect()
}

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
        let c = Rc::downgrade(self);
        menu.on_toggle_eco(move || {
            if let Some(c) = c.upgrade() {
                c.request_toggle_eco();
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
        let selected = self.selected_process();
        let state = match &selected {
            Some((key, ..)) => tml_core::control_state(*key),
            None => Err(ProcessControlError::Gone),
        };
        let m = control_menu(&state);
        let protected = selected
            .as_ref()
            .is_some_and(|(key, _, critical)| *critical || tml_core::eco_protected_pid(key.pid, std::process::id()));
        let eco = eco_menu(tml_core::eco_supported(), protected, &state);
        let menu = ui.global::<ProcessMenu>();
        menu.set_eco_visible(eco.visible);
        menu.set_can_eco(eco.enabled);
        menu.set_eco_checked(eco.checked);
        menu.set_eco_title(texts::eco_menu_title(eco.disabled_reason).into());
        menu.set_can_set_priority(m.can_set_priority);
        menu.set_can_set_affinity(m.can_set_affinity);
        menu.set_priority(m.priority);
    }

    /// 所选行是不是分组视图里的应用行（效率模式作用于它及其全部后代）。
    fn selected_is_app_row(&self, key: ProcessKey) -> bool {
        let s = self.state.borrow();
        s.opts.mode == ViewMode::Group && s.app_pids.as_ref().is_some_and(|a| a.contains(&key.pid))
    }

    /// 右键菜单“效率模式”：按所选行当前状态取反。应用行作用于全部后代，多于一个进程时先确认。
    fn request_toggle_eco(&self) {
        let Some((key, name, _)) = self.selected_process() else { return };
        let on = tml_core::eco_mode(key) != Ok(true);
        let is_app = self.selected_is_app_row(key);
        let keys = eco_targets(&self.state.borrow().snapshot, key, is_app, std::process::id());
        if keys.is_empty() {
            self.log.borrow_mut().line(format!("效率模式 拒绝 pid={} 原因=受保护", key.pid));
            self.report(STATUS_ERROR, failure("效率模式", texts::eco_disabled_reason(true)));
            return;
        }
        if keys.len() > 1 {
            self.eco.pending_on.set(on);
            let count = keys.len() as i32;
            self.ask(CONFIRM_ECO, keys, name, count, false);
            if let Some(ui) = self.ui.upgrade() {
                ui.set_confirm_eco_on(on);
            }
        } else {
            self.apply_eco(&keys, &name, on);
        }
    }

    /// 确认框点“开启 / 关闭”。
    pub(super) fn confirm_eco(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let keys = std::mem::take(&mut self.state.borrow_mut().pending);
        let name = ui.get_confirm_name().to_string();
        let on = self.eco.pending_on.get();
        self.apply_eco(&keys, &name, on);
    }

    /// 对一组进程开启 / 关闭效率模式并汇报结果（单个进程汇报它自己的优先级联动，多个进程汇报成功 / 失败数）。
    /// 日志只记 PID、结果与错误码。
    pub(super) fn apply_eco(&self, keys: &[ProcessKey], name: &str, on: bool) {
        {
            // 清掉已退出进程的记忆，避免无限增长。
            let s = self.state.borrow();
            self.eco.memory.borrow_mut().retain(|k, _| s.snapshot.processes.iter().any(|p| p.key == *k));
        }
        let (mut ok, mut failed) = (0usize, 0usize);
        let mut last_error = None;
        let mut single = None;
        for &key in keys {
            let before = tml_core::control_state(key).ok();
            let result = if on {
                tml_core::set_eco_mode(key, true, None)
            } else {
                let remembered = self.eco.memory.borrow().get(&key).copied();
                let restore = eco_restore_target(before.as_ref().and_then(|s| s.priority), remembered);
                tml_core::set_eco_mode(key, false, restore)
            };
            match result {
                Ok(o) => {
                    ok += 1;
                    if on {
                        // 已经处于效率模式时不覆盖记住的“开启前”优先级。
                        if before.as_ref().and_then(|s| s.eco) != Some(true) {
                            if let Some(prev) = o.previous_priority {
                                self.eco.memory.borrow_mut().insert(key, prev);
                            }
                        }
                    } else {
                        self.eco.memory.borrow_mut().remove(&key);
                    }
                    self.log.borrow_mut().line(format!(
                        "效率模式 成功 pid={} 目标={} 当前={} 优先级={}",
                        key.pid,
                        if on { "开" } else { "关" },
                        if o.enabled { "开" } else { "关" },
                        o.priority.map_or("?", |p| p.key())
                    ));
                    single = Some(o);
                }
                Err(e) => {
                    failed += 1;
                    self.log.borrow_mut().line(format!(
                        "效率模式 失败 pid={} 目标={} 错误码={}",
                        key.pid,
                        if on { "开" } else { "关" },
                        e.code()
                    ));
                    last_error = Some(e);
                }
            }
        }
        match (keys.len(), last_error.as_ref(), single) {
            (1, Some(e), _) => self.report(STATUS_ERROR, failure("效率模式", &texts::process_control_error(e))),
            (1, None, Some(o)) => {
                let text = if on { texts::eco_enabled(name) } else { texts::eco_disabled(name, o.priority) };
                self.report(STATUS_ECO, text);
            }
            _ => self.report(if ok == 0 { STATUS_ERROR } else { STATUS_ECO }, texts::eco_batch(name, on, ok, failed)),
        }
        // 立即补采一次，“效率模式”“优先级”列在下一次采样内更新。
        self.sampler.send(Command::Wake);
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
            eco: Some(false),
            modify,
        }
    }

    fn eco_proc(pid: u32, parent: u32, name: &str, critical: bool) -> tml_core::Process {
        tml_core::Process {
            key: ProcessKey { pid, create_time: u64::from(pid) },
            parent_pid: parent,
            name: name.into(),
            cpu_usage: 0.0,
            private_working_set: 0,
            disk_rate: 0.0,
            gpu_usage: None,
            eco_mode: None,
            critical,
            counters: Default::default(),
            info: None,
        }
    }

    #[test]
    fn eco_restore_keeps_a_priority_the_user_changed() {
        use PriorityClass::*;
        // 仍是开启时设的“低”：恢复到记住的优先级，记不到则恢复为正常。
        assert_eq!(eco_restore_target(Some(Low), Some(AboveNormal)), Some(AboveNormal));
        assert_eq!(eco_restore_target(Some(Low), None), Some(Normal));
        // 开启期间用户手动改过：不覆盖。
        assert_eq!(eco_restore_target(Some(High), Some(AboveNormal)), None);
        assert_eq!(eco_restore_target(Some(Normal), None), None);
        // 读不到当前优先级：不动。
        assert_eq!(eco_restore_target(None, Some(High)), None);
    }

    #[test]
    fn eco_menu_states() {
        let ok = Ok(ControlState { priority: Some(PriorityClass::Normal), affinity: None, eco: Some(true), modify: Ok(()) });
        assert_eq!(eco_menu(true, false, &ok), EcoMenu { visible: true, enabled: true, checked: true, disabled_reason: None });
        // 系统不支持：整项隐藏。
        assert!(!eco_menu(false, false, &ok).visible);
        // 关键进程 / 自身：置灰并给出“关键”原因。
        let m = eco_menu(true, true, &ok);
        assert!(m.visible && !m.enabled && !m.checked);
        assert_eq!(m.disabled_reason, Some(true));
        // 无权限：置灰，原因为没有权限，仍显示当前状态的勾选。
        let denied = Ok(ControlState {
            priority: None,
            affinity: None,
            eco: Some(false),
            modify: Err(ProcessControlError::AccessDenied),
        });
        let m = eco_menu(true, false, &denied);
        assert!(m.visible && !m.enabled && !m.checked);
        assert_eq!(m.disabled_reason, Some(false));
        // 进程已退出：置灰。
        let m = eco_menu(true, false, &Err(ProcessControlError::Gone));
        assert!(m.visible && !m.enabled);
        assert!(texts::eco_menu_title(None) == "效率模式" && texts::eco_menu_title(m.disabled_reason).contains("没有权限"));
    }

    #[test]
    fn eco_targets_follow_the_app_tree_and_skip_protected() {
        let snap = tml_core::ProcessSnapshot {
            processes: vec![
                eco_proc(10, 1, "app.exe", false),
                eco_proc(11, 10, "helper.exe", false),
                eco_proc(12, 11, "grandchild.exe", false),
                eco_proc(13, 10, "csrss.exe", true),
                eco_proc(14, 10, "self.exe", false),
                eco_proc(20, 1, "other.exe", false),
                eco_proc(30, 1, "explorer.exe", false),
                eco_proc(31, 30, "launched.exe", false),
            ],
        };
        let key = |pid: u32| ProcessKey { pid, create_time: u64::from(pid) };
        // 应用行：自身 + 全部后代（后代在前），关键进程与本程序自身不参与，其他应用不受影响。
        let mut got = eco_targets(&snap, key(10), true, 14);
        got.sort_by_key(|k| k.pid);
        assert_eq!(got, vec![key(10), key(11), key(12)]);
        // 非应用行（后台进程 / 子进程行 / 非分组视图）：只作用于自身。
        assert_eq!(eco_targets(&snap, key(10), false, 99), vec![key(10)]);
        // explorer.exe 不收纳后代：应用行也只作用于它自己。
        assert_eq!(eco_targets(&snap, key(30), true, 99), vec![key(30)]);
        // 关键进程单独选中：没有可作用的目标。
        assert!(eco_targets(&snap, key(13), true, 99).is_empty());
        assert!(eco_targets(&snap, key(99), true, 99).is_empty());
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

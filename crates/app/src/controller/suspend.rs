//! 挂起与恢复进程（#67）：右键菜单“挂起”“恢复”，以及退出前对“本程序挂起、仍保持挂起”的进程的提示。
//!
//! - “已挂起”直接取自采样快照（`Counters::suspended`，由 `SYSTEM_PROCESS_INFORMATION` 的线程等待原因得出），
//!   菜单弹出时不额外打开任何进程去查它；只有“有没有权限”会探测一次（`ControlState::suspend`）。
//! - 系统的挂起是计数式的，所以对已挂起的进程不再挂起（“挂起”置灰），恢复一次就够。
//! - 应用行（分组视图）作用于该应用及其全部后代，口径与“结束进程树”、效率模式一致（[`app_row_keys`]）。
//! - 关键进程、或一次挂起多个进程时先确认；“恢复”不需要确认。
//! - 退出（关闭按钮、托盘“退出”、迷你窗口的退出）时不自动恢复，但若本程序挂起过的进程仍存在且仍挂起，
//!   先提示一次；最小化与“关闭时最小化到托盘”不算退出。日志只记 PID、结果与错误码。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use super::process_control::app_row_keys;
use super::process_menu::failure;
use super::*;
use crate::{ProcessMenu, texts};
use texts::MenuBlock;
use tml_core::{ControlState, ProcessControlError};

const STATUS_ERROR: i32 = 5;
const STATUS_SUSPEND: i32 = 13;

/// 挂起 / 恢复的应用层状态。
#[derive(Default)]
pub struct SuspendState {
    /// 本程序在本次运行中成功挂起、尚未由本程序恢复的进程（PID + 创建时间，避免 PID 复用误判）。
    /// 退出前再与系统核对它们是否仍存在且仍处于挂起。
    mine: RefCell<HashSet<ProcessKey>>,
    /// 测试钩子（`--menu-action exit-prompt`）：开发模式下也走退出前提示。
    force_exit_prompt: Cell<bool>,
}

/// 一次挂起 / 恢复的目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendTargets {
    pub keys: Vec<ProcessKey>,
    /// 目标里是否有关键进程（挂起前需要二次确认）。
    pub critical: bool,
}

/// 挂起 / 恢复的目标进程（纯函数）：
/// 从 [`app_row_keys`]（单个进程，或应用行的应用及其全部后代）里去掉 PID 0 / 4 与本程序自身，
/// 挂起时再去掉已挂起的（避免计数式挂起叠加），恢复时只留已挂起的。
pub fn suspend_targets(
    snapshot: &tml_core::ProcessSnapshot,
    root: ProcessKey,
    is_app_row: bool,
    own_pid: u32,
    suspend: bool,
) -> SuspendTargets {
    let mut keys = Vec::new();
    let mut critical = false;
    for key in app_row_keys(snapshot, root, is_app_row) {
        let Some(p) = snapshot.processes.iter().find(|p| p.key == key) else { continue };
        if tml_core::suspend_protected_pid(key.pid, own_pid) || p.counters.suspended == suspend {
            continue;
        }
        critical |= p.critical;
        keys.push(key);
    }
    SuspendTargets { keys, critical }
}

/// “挂起”“恢复”两个菜单项的状态；`None` 表示可用，`Some(原因)` 表示置灰。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendMenu {
    pub suspend: Option<MenuBlock>,
    pub resume: Option<MenuBlock>,
}

/// 由探测结果与两类目标的个数得出菜单状态（纯函数）：
/// 进程已退出、PID 0 / 4 与本程序自身（`protected_pid`）、没有权限时两项都置灰并写明原因；
/// 否则“挂起”在没有未挂起的目标时置灰，“恢复”在没有已挂起的目标时置灰。
pub fn suspend_menu(
    probe: &Result<ControlState, ProcessControlError>,
    protected_pid: bool,
    suspendable: usize,
    resumable: usize,
) -> SuspendMenu {
    let blocked = match probe {
        Err(e) => Some(e.clone()),
        Ok(_) if protected_pid => Some(ProcessControlError::Protected),
        Ok(s) => s.suspend.clone().err(),
    };
    if let Some(e) = blocked {
        let block = Some(MenuBlock::Error(e));
        return SuspendMenu { suspend: block.clone(), resume: block };
    }
    SuspendMenu {
        suspend: (suspendable == 0).then_some(MenuBlock::NoTarget),
        resume: (resumable == 0).then_some(MenuBlock::NoTarget),
    }
}

/// 要不要先确认再挂起：含关键进程，或不止一个进程。
pub fn suspend_needs_confirm(targets: &SuspendTargets) -> bool {
    targets.critical || targets.keys.len() > 1
}

/// 退出时的决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDecision {
    /// 直接退出。
    Quit,
    /// 弹出提示；参数是仍保持挂起的进程个数。
    Prompt(usize),
    /// 提示已经在显示，同一次退出流程不再重复弹出。
    AlreadyPrompting,
}

/// 退出前提示的判断（纯函数）：
/// `enabled` 为假（开发 / 测试模式）直接退出；提示已在显示则不重复；`still_suspended` 是本程序挂起的进程里
/// 仍存在且仍挂起的个数，为 0 时直接退出。
pub fn exit_decision(enabled: bool, prompt_showing: bool, still_suspended: usize) -> ExitDecision {
    if !enabled {
        ExitDecision::Quit
    } else if prompt_showing {
        ExitDecision::AlreadyPrompting
    } else if still_suspended == 0 {
        ExitDecision::Quit
    } else {
        ExitDecision::Prompt(still_suspended)
    }
}

impl Controller {
    pub(super) fn bind_suspend(self: &Rc<Self>, ui: &AppWindow) {
        let menu = ui.global::<ProcessMenu>();
        let c = Rc::downgrade(self);
        menu.on_suspend(move || {
            if let Some(c) = c.upgrade() {
                c.request_suspend(true);
            }
        });
        let c = Rc::downgrade(self);
        menu.on_resume(move || {
            if let Some(c) = c.upgrade() {
                c.request_suspend(false);
            }
        });
    }

    /// 右键菜单弹出前（由 `update_control_menu` 调用，复用同一次权限探测）：设置“挂起”“恢复”的可用性与标题。
    pub(super) fn update_suspend_menu(
        &self,
        ui: &AppWindow,
        selected: Option<&(ProcessKey, String, bool)>,
        probe: &Result<ControlState, ProcessControlError>,
    ) {
        let own = std::process::id();
        let (suspendable, resumable, protected) = match selected {
            Some((key, ..)) => {
                let is_app = self.selected_is_app_row(*key);
                let s = self.state.borrow();
                (
                    suspend_targets(&s.snapshot, *key, is_app, own, true).keys.len(),
                    suspend_targets(&s.snapshot, *key, is_app, own, false).keys.len(),
                    tml_core::suspend_protected_pid(key.pid, own),
                )
            }
            None => (0, 0, false),
        };
        let m = suspend_menu(probe, protected, suspendable, resumable);
        let menu = ui.global::<ProcessMenu>();
        menu.set_can_suspend(m.suspend.is_none());
        menu.set_can_resume(m.resume.is_none());
        menu.set_suspend_title(texts::suspend_menu_title(m.suspend.as_ref()).into());
        menu.set_resume_title(texts::resume_menu_title(m.resume.as_ref()).into());
    }

    /// 右键菜单“挂起”“恢复”。目标为空（受保护、已是目标状态）时只提示不执行；
    /// 挂起关键进程或多个进程时先确认。
    fn request_suspend(&self, suspend: bool) {
        let Some((key, name, _)) = self.selected_process() else { return };
        let action = if suspend { "挂起" } else { "恢复" };
        let is_app = self.selected_is_app_row(key);
        let targets = suspend_targets(&self.state.borrow().snapshot, key, is_app, std::process::id(), suspend);
        if targets.keys.is_empty() {
            let protected = tml_core::suspend_protected_pid(key.pid, std::process::id());
            self.log.borrow_mut().line(format!(
                "{action} 拒绝 pid={} 原因={}",
                key.pid,
                if protected { "受保护" } else { "无可操作的进程" }
            ));
            let reason = if protected {
                texts::process_control_error(&ProcessControlError::Protected)
            } else if suspend {
                "已经处于挂起状态".to_string()
            } else {
                "没有处于挂起状态的进程".to_string()
            };
            self.report(STATUS_ERROR, failure(action, &reason));
            return;
        }
        if suspend && suspend_needs_confirm(&targets) {
            self.log.borrow_mut().line(format!(
                "挂起 需要确认 pid={} 进程数={} 含关键进程={}",
                key.pid,
                targets.keys.len(),
                targets.critical
            ));
            let count = targets.keys.len() as i32;
            self.ask(CONFIRM_SUSPEND, targets.keys, name, count, targets.critical);
        } else {
            self.apply_suspend(&targets.keys, &name, suspend);
        }
    }

    /// 确认框点“挂起”。
    pub(super) fn confirm_suspend(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let keys = std::mem::take(&mut self.state.borrow_mut().pending);
        let name = ui.get_confirm_name().to_string();
        self.apply_suspend(&keys, &name, true);
    }

    /// 对一组进程挂起 / 恢复并汇报结果；记录本程序挂起的进程供退出前提示使用。
    fn apply_suspend(&self, keys: &[ProcessKey], name: &str, suspend: bool) {
        let action = if suspend { "挂起" } else { "恢复" };
        let (mut ok, mut failed) = (0usize, 0usize);
        let mut last_error = None;
        for &key in keys {
            let result = if suspend { tml_core::suspend_process(key) } else { tml_core::resume_process(key) };
            match result {
                Ok(()) => {
                    ok += 1;
                    if suspend {
                        self.suspend.mine.borrow_mut().insert(key);
                    } else {
                        self.suspend.mine.borrow_mut().remove(&key);
                    }
                    self.log.borrow_mut().line(format!("{action} 成功 pid={}", key.pid));
                }
                Err(e) => {
                    failed += 1;
                    self.log.borrow_mut().line(format!("{action} 失败 pid={} 错误码={}", key.pid, e.code()));
                    last_error = Some(e);
                }
            }
        }
        match (keys.len(), last_error.as_ref()) {
            (1, Some(e)) => self.report(STATUS_ERROR, failure(action, &texts::process_control_error(e))),
            _ => self.report(
                if ok == 0 { STATUS_ERROR } else { STATUS_SUSPEND },
                texts::suspend_result(name, suspend, ok, failed),
            ),
        }
        // 立即补采一次，“状态”列在下一次采样内更新。
        self.sampler.send(Command::Wake);
    }

    /// 真正退出前的统一入口（关闭按钮、托盘“退出”、迷你窗口退出）：返回 `true` 表示可以立即退出；
    /// 返回 `false` 表示已弹出（或已在显示）“仍保持挂起”的提示，等用户选择“仍然退出”或“取消”。
    pub(super) fn try_exit(&self) -> bool {
        let Some(ui) = self.ui.upgrade() else { return true };
        let enabled = self.store.is_some() || self.suspend.force_exit_prompt.get();
        let showing = ui.get_confirm_kind() == CONFIRM_EXIT_SUSPENDED;
        let alive = if enabled && !showing {
            let mine: Vec<ProcessKey> = self.suspend.mine.borrow().iter().copied().collect();
            tml_core::still_suspended(&mine)
        } else {
            Vec::new()
        };
        match exit_decision(enabled, showing, alive.len()) {
            ExitDecision::Quit => {
                if enabled && !self.suspend.mine.borrow().is_empty() {
                    self.log.borrow_mut().line("退出前提示 跳过 原因=本程序挂起的进程都已退出或已恢复");
                } else if enabled {
                    self.log.borrow_mut().line("退出前提示 跳过 原因=本程序没有挂起过进程");
                }
                true
            }
            ExitDecision::AlreadyPrompting => false,
            ExitDecision::Prompt(count) => {
                self.log.borrow_mut().line(format!("退出前提示 弹出 进程数={count}"));
                // 提示在完整窗口里：迷你模式先回到完整模式，隐藏到托盘 / 最小化时先显示出来。
                if self.is_mini() {
                    self.exit_mini();
                }
                self.show_window();
                self.ask(CONFIRM_EXIT_SUSPENDED, Vec::new(), String::new(), count as i32, false);
                false
            }
        }
    }

    /// 确认框点“仍然退出”：不恢复被挂起的进程，直接退出。
    pub(super) fn confirm_exit_suspended(&self) {
        self.log.borrow_mut().line("退出前提示 选择=仍然退出");
        self.quit_now();
    }

    /// 测试钩子 `--menu-action exit-prompt`：开发模式下也走退出前提示，然后模拟一次“退出”。
    pub fn exit_hook(&self) {
        self.suspend.force_exit_prompt.set(true);
        self.quit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tml_core::{Counters, Process, ProcessSnapshot};

    fn proc(pid: u32, parent: u32, name: &str, suspended: bool, critical: bool) -> Process {
        Process {
            key: ProcessKey { pid, create_time: pid as u64 },
            parent_pid: parent,
            name: name.into(),
            cpu_usage: 0.0,
            private_working_set: 0,
            disk_rate: 0.0,
            gpu_usage: None,
            eco_mode: None,
            critical,
            counters: Counters { suspended, ..Default::default() },
            info: None,
        }
    }

    fn key(pid: u32) -> ProcessKey {
        ProcessKey { pid, create_time: pid as u64 }
    }

    /// 应用 100 有后代 101（已挂起）、102（关键）、103 与更深的 104；另有无关进程 200。
    fn tree() -> ProcessSnapshot {
        ProcessSnapshot {
            processes: vec![
                proc(100, 1, "app", false, false),
                proc(101, 100, "child-suspended", true, false),
                proc(102, 100, "child-critical", false, true),
                proc(103, 100, "child", false, false),
                proc(104, 103, "grandchild", false, false),
                proc(200, 1, "other", false, false),
            ],
        }
    }

    fn probe(suspend: Result<(), ProcessControlError>) -> Result<ControlState, ProcessControlError> {
        Ok(ControlState { priority: None, affinity: None, eco: None, modify: Ok(()), suspend })
    }

    #[test]
    fn single_process_target_follows_the_suspended_state() {
        let s = tree();
        let t = suspend_targets(&s, key(103), false, 9999, true);
        assert_eq!(t.keys, vec![key(103)]);
        assert!(!t.critical);
        // 已挂起的进程不能再挂起（挂起是计数式的，叠加后恢复一次不够）。
        assert!(suspend_targets(&s, key(101), false, 9999, true).keys.is_empty());
        // 恢复只针对已挂起的。
        assert_eq!(suspend_targets(&s, key(101), false, 9999, false).keys, vec![key(101)]);
        assert!(suspend_targets(&s, key(103), false, 9999, false).keys.is_empty());
        // 单个关键进程需要确认。
        let t = suspend_targets(&s, key(102), false, 9999, true);
        assert!(t.critical && suspend_needs_confirm(&t));
    }

    #[test]
    fn app_row_targets_cover_the_whole_tree_without_double_suspending() {
        let s = tree();
        let mut t = suspend_targets(&s, key(100), true, 9999, true);
        t.keys.sort_by_key(|k| k.pid);
        // 与“结束进程树”同口径：应用 + 全部后代，但已挂起的 101 不再挂起。
        assert_eq!(t.keys, vec![key(100), key(102), key(103), key(104)]);
        assert!(t.critical, "后代里有关键进程");
        assert!(suspend_needs_confirm(&t));
        // 恢复：只剩已挂起的那一个，不需要确认。
        let t = suspend_targets(&s, key(100), true, 9999, false);
        assert_eq!(t.keys, vec![key(101)]);
        // 应用行与其口径来源一致。
        let mut all = app_row_keys(&s, key(100), true);
        all.sort_by_key(|k| k.pid);
        assert_eq!(all, vec![key(100), key(101), key(102), key(103), key(104)]);
        // 非应用行只作用于自己。
        assert_eq!(suspend_targets(&s, key(100), false, 9999, true).keys, vec![key(100)]);
        // 多于一个进程要确认，没有关键进程时也一样。
        let mut t = suspend_targets(&s, key(103), true, 9999, true);
        t.keys.sort_by_key(|k| k.pid);
        assert_eq!(t.keys, vec![key(103), key(104)]);
        assert!(!t.critical && suspend_needs_confirm(&t));
    }

    #[test]
    fn protected_pids_and_self_are_never_targets() {
        let s = ProcessSnapshot {
            processes: vec![
                proc(4, 0, "System", false, true),
                proc(500, 4, "self", false, false),
                proc(501, 500, "child", false, false),
            ],
        };
        assert!(suspend_targets(&s, key(4), false, 500, true).keys.is_empty());
        assert!(suspend_targets(&s, key(500), false, 500, true).keys.is_empty());
        // 应用行是本程序自己时，后代仍可挂起，自身被排除。
        assert_eq!(suspend_targets(&s, key(500), true, 500, true).keys, vec![key(501)]);
        // 不存在的进程没有目标。
        assert!(suspend_targets(&s, key(9), false, 500, true).keys.is_empty());
    }

    #[test]
    fn menu_blocks_with_a_reason() {
        let ok = probe(Ok(()));
        // 可挂起、不可恢复。
        let m = suspend_menu(&ok, false, 1, 0);
        assert_eq!(m, SuspendMenu { suspend: None, resume: Some(MenuBlock::NoTarget) });
        // 已挂起：挂起置灰，恢复可用。
        let m = suspend_menu(&ok, false, 0, 1);
        assert_eq!(m, SuspendMenu { suspend: Some(MenuBlock::NoTarget), resume: None });
        // 受保护（PID 0 / 4 / 本程序）：两项都置灰。
        let m = suspend_menu(&ok, true, 1, 0);
        assert_eq!(m.suspend, Some(MenuBlock::Error(ProcessControlError::Protected)));
        assert_eq!(m.resume, m.suspend);
        // 没有权限：两项都置灰，原因是没有权限。
        let denied = probe(Err(ProcessControlError::AccessDenied));
        let m = suspend_menu(&denied, false, 1, 1);
        assert_eq!(m.suspend, Some(MenuBlock::Error(ProcessControlError::AccessDenied)));
        assert_eq!(m.resume, m.suspend);
        // 进程已退出。
        let m = suspend_menu(&Err(ProcessControlError::Gone), false, 0, 0);
        assert_eq!(m.suspend, Some(MenuBlock::Error(ProcessControlError::Gone)));
    }

    #[test]
    fn menu_titles_name_the_reason() {
        assert_eq!(texts::suspend_menu_title(None), "挂起");
        assert_eq!(texts::resume_menu_title(None), "恢复");
        assert_eq!(texts::suspend_menu_title(Some(&MenuBlock::NoTarget)), "挂起（已挂起）");
        assert_eq!(texts::resume_menu_title(Some(&MenuBlock::NoTarget)), "恢复（未挂起）");
        let denied = MenuBlock::Error(ProcessControlError::AccessDenied);
        assert!(texts::suspend_menu_title(Some(&denied)).contains("没有权限"));
        let protected = MenuBlock::Error(ProcessControlError::Protected);
        assert!(texts::suspend_menu_title(Some(&protected)).contains("不可挂起"));
    }

    #[test]
    fn result_text_for_single_and_batch() {
        assert_eq!(texts::suspend_result("a.exe", true, 1, 0), "a.exe → 已挂起");
        assert_eq!(texts::suspend_result("a.exe", false, 3, 0), "a.exe → 已恢复 3 个进程");
        assert!(texts::suspend_result("a.exe", true, 2, 1).contains("1 个失败"));
    }

    #[test]
    fn exit_prompt_only_when_our_suspended_processes_remain() {
        // 开发 / 测试模式从不拦截退出。
        assert_eq!(exit_decision(false, false, 3), ExitDecision::Quit);
        // 没有本程序挂起且仍存在的进程：直接退出（外部挂起的进程不在统计里）。
        assert_eq!(exit_decision(true, false, 0), ExitDecision::Quit);
        // 有：提示，带上个数。
        assert_eq!(exit_decision(true, false, 2), ExitDecision::Prompt(2));
        // 同一次退出流程只弹一次：提示已在显示时不重复。
        assert_eq!(exit_decision(true, true, 2), ExitDecision::AlreadyPrompting);
    }

    #[test]
    fn status_column_text_reflects_suspension() {
        // 状态列文字由详细信息列负责：已挂起 / 正在运行。
        let col = tml_core::DetailColumn::Status;
        let mut p = proc(1, 0, "x", true, false);
        assert_eq!(crate::details::cell(&p, col, None), "已挂起");
        p.counters.suspended = false;
        assert_eq!(crate::details::cell(&p, col, None), "正在运行");
    }
}

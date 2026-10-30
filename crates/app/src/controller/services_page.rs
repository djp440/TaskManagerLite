use super::*;
use crate::{ServiceData, ServiceRow};
use std::sync::{Arc, Mutex};
use crate::texts;
use tml_core::services::{Action, Service, ServiceError, ServiceList};

/// 服务页的页面编号（与 app.slint 中的 `page` 一致）。
pub const SERVICES_PAGE: i32 = 4;

/// 服务页的界面线程状态。
#[derive(Default)]
pub(super) struct ServicesState {
    /// 当前显示顺序的服务。
    entries: RefCell<Vec<Service>>,
    /// 原地更新的行模型，避免周期刷新时列表回到顶部。
    rows: Rc<VecModel<ServiceRow>>,
    /// 配置缓存，在后台线程里使用。
    list: Arc<Mutex<ServiceList>>,
    /// 后台枚举正在进行（周期刷新时跳过）。
    loading: Cell<bool>,
    /// 每次后台工作的序号；只应用不早于已应用结果的那次，避免慢的旧枚举覆盖操作后的新状态。
    issued: Cell<u64>,
    applied: Cell<u64>,
    /// 上次写进日志的服务数，变化时才记录。
    logged: Cell<usize>,
    /// 自动化验收：`--service-action <动作>:<服务名>`，首次列出服务后执行。
    hook: RefCell<Option<String>>,
}

impl Controller {
    pub(super) fn bind_services(self: &Rc<Self>, ui: &AppWindow) {
        let data = ui.global::<ServiceData>();
        data.set_rows(ModelRc::from(self.services.rows.clone()));
        data.set_elevated(tml_core::is_elevated());
        let weak = Rc::downgrade(self);
        data.on_sort(move |key| {
            if let Some(c) = weak.upgrade()
                && let Some(ui) = c.ui.upgrade()
            {
                let d = ui.global::<ServiceData>();
                d.set_descending(d.get_sort_key() == key && !d.get_descending());
                d.set_sort_key(key);
                let entries = std::mem::take(&mut *c.services.entries.borrow_mut());
                c.apply_services(entries);
            }
        });
        let weak = Rc::downgrade(self);
        data.on_refresh(move || {
            if let Some(c) = weak.upgrade() {
                c.services_work(None, true);
            }
        });
        let weak = Rc::downgrade(self);
        data.on_act(move |i| {
            if let Some(c) = weak.upgrade() {
                let action = [Action::Start, Action::Stop, Action::Restart][i.clamp(0, 2) as usize];
                c.service_action(action, true);
            }
        });
        let weak = Rc::downgrade(self);
        data.on_goto_process(move || {
            if let Some(c) = weak.upgrade() {
                c.goto_service_process();
            }
        });
        // 恢复到服务页时，组件的 init 可能早于回调绑定。
        if ui.get_page() == SERVICES_PAGE {
            self.services_work(None, true);
        }
    }

    /// 自动化验收入口：切到服务页，首次列出服务后对指定服务执行动作（不弹 UAC）。
    pub fn queue_service_action(&self, spec: &str) {
        *self.services.hook.borrow_mut() = Some(spec.to_owned());
        if let Some(ui) = self.ui.upgrade() {
            ui.set_page(SERVICES_PAGE);
        }
    }

    /// 每次完整采样后调用：服务页可见时按采样间隔刷新状态，不可见时什么也不做。
    pub(super) fn services_tick(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.get_page() != SERVICES_PAGE
            || ui.global::<ServiceData>().get_busy()
            || self.services.loading.get()
        {
            return;
        }
        self.services_work(None, false);
    }

    fn selected_service(&self) -> Option<Service> {
        let ui = self.ui.upgrade()?;
        let i = usize::try_from(ui.global::<ServiceData>().get_selected()).ok()?;
        self.services.entries.borrow().get(i).cloned()
    }

    fn service_action(&self, action: Action, elevate: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<ServiceData>();
        if data.get_busy() {
            return;
        }
        let Some(service) = self.selected_service() else { return };
        let name = service.display_name.clone();
        let hint = if tml_core::is_elevated() || !elevate { "" } else { "（需要管理员权限，请在 UAC 对话框中确认）" };
        data.set_note(format!("正在{}服务“{name}”{hint}…", texts::service_action(action)).into());
        data.set_busy(true);
        self.services_work(Some((service.name, name, action, elevate)), false);
    }

    /// 后台执行（可选的）操作并重新枚举；`reload` 时先清空配置缓存。
    fn services_work(&self, action: Option<(String, String, Action, bool)>, reload: bool) {
        let seq = self.services.issued.get() + 1;
        self.services.issued.set(seq);
        self.services.loading.set(true);
        let list = self.services.list.clone();
        let work = std::thread::Builder::new()
            .name("services".into())
            .spawn(move || {
                let note = action.map(|(name, display, action, elevate)| {
                    match tml_core::services::control(&name, action, elevate) {
                        Ok(()) => format!("已{}服务“{display}”", texts::service_action(action)),
                        Err(e) => format!("无法{}服务“{display}”：{}", texts::service_action(action), texts::service_error(&e)),
                    }
                });
                let started = Instant::now();
                let result = {
                    let mut list = list.lock().unwrap_or_else(|e| e.into_inner());
                    if reload {
                        list.clear_cache();
                    }
                    list.list()
                };
                let elapsed = started.elapsed();
                let _ = slint::invoke_from_event_loop(move || {
                    crate::CONTROLLER.with(|cell| {
                        if let Some(c) = cell.borrow().clone() {
                            c.services_done(seq, note, result, elapsed);
                        }
                    });
                });
            });
        if let Err(e) = work {
            self.services.loading.set(false);
            if let Some(ui) = self.ui.upgrade() {
                let data = ui.global::<ServiceData>();
                data.set_busy(false);
                data.set_note(format!("无法创建后台线程：{e}").into());
            }
        }
    }

    fn services_done(
        &self,
        seq: u64,
        note: Option<String>,
        result: Result<Vec<Service>, ServiceError>,
        elapsed: Duration,
    ) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<ServiceData>();
        if seq == self.services.issued.get() {
            self.services.loading.set(false);
        }
        if let Some(note) = &note {
            data.set_busy(false);
            data.set_note(note.as_str().into());
            self.log.borrow_mut().line(format!("服务操作 {note}"));
        }
        if seq < self.services.applied.get() {
            return;
        }
        self.services.applied.set(seq);
        match result {
            Ok(entries) => {
                if self.services.logged.replace(entries.len()) != entries.len() {
                    self.log.borrow_mut().line(format!(
                        "服务页 枚举 {} 个服务 {} ms",
                        entries.len(),
                        elapsed.as_millis()
                    ));
                }
                self.apply_services(entries);
            }
            Err(e) => {
                if note.is_none() {
                    data.set_note(format!("无法读取服务列表：{}", texts::service_error(&e)).into());
                }
                self.log.borrow_mut().line(format!("服务页 枚举失败 {}", texts::service_error(&e)));
            }
        }
        self.run_service_hook();
    }

    /// 按当前排序显示，并按服务名保留选中项。
    fn apply_services(&self, mut entries: Vec<Service>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<ServiceData>();
        let (key, descending) = (data.get_sort_key(), data.get_descending());
        entries.sort_by(|a, b| compare(a, b, key, descending));
        let selected = self
            .selected_service()
            .and_then(|old| entries.iter().position(|e| e.name == old.name));
        let rows = &self.services.rows;
        let old = rows.row_count();
        for (i, e) in entries.iter().enumerate() {
            let row = service_row(e);
            if i < old {
                if rows.row_data(i).as_ref() != Some(&row) {
                    rows.set_row_data(i, row);
                }
            } else {
                rows.push(row);
            }
        }
        for i in (entries.len()..old).rev() {
            rows.remove(i);
        }
        *self.services.entries.borrow_mut() = entries;
        data.set_selected(selected.map_or(-1, |i| i as i32));
    }

    /// 跳转到进程页并选中服务所在的进程。
    fn goto_service_process(&self) {
        let Some(service) = self.selected_service().filter(|s| s.pid > 0) else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        if self.reveal_pid(service.pid) {
            ui.set_page(0);
            ui.invoke_show_selected();
            self.log.borrow_mut().line(format!(
                "服务页 跳转到进程 服务={} PID={} 行={}",
                service.name,
                service.pid,
                ui.get_selected()
            ));
        } else {
            ui.global::<ServiceData>()
                .set_note(format!("进程列表中没有 PID {}（可能已退出），请稍后重试", service.pid).into());
        }
    }

    fn run_service_hook(&self) {
        // 等首个进程快照到达后再执行（转到进程需要它）；之后每次周期刷新都会再检查一次。
        if !self.state.borrow().first_data {
            return;
        }
        let Some(spec) = self.services.hook.borrow_mut().take() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        let (verb, name) = spec.split_once(':').unwrap_or((spec.as_str(), ""));
        let index = self
            .services
            .entries
            .borrow()
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(name));
        let Some(index) = index else {
            self.log.borrow_mut().line(format!("服务操作 找不到服务 {name}"));
            return;
        };
        ui.global::<ServiceData>().set_selected(index as i32);
        match verb {
            "goto" => self.goto_service_process(),
            v => match Action::parse(v) {
                Some(action) => self.service_action(action, false),
                None => self.log.borrow_mut().line(format!("服务操作 未知动作 {v}")),
            },
        }
    }
}

fn service_row(e: &Service) -> ServiceRow {
    ServiceRow {
        name: e.name.as_str().into(),
        display_name: e.display_name.as_str().into(),
        description: e.config.description.as_str().into(),
        state: texts::service_state(e.state).into(),
        start_type: texts::start_type(e.config.start_type).into(),
        pid: e.pid as i32,
        group: e.config.group.as_str().into(),
        can_start: e.can_start(),
        can_stop: e.can_stop(),
    }
}

fn compare(a: &Service, b: &Service, key: i32, descending: bool) -> std::cmp::Ordering {
    let lower = |s: &str| s.to_lowercase();
    let cmp = match key {
        1 => a.pid.cmp(&b.pid),
        2 => lower(&a.display_name).cmp(&lower(&b.display_name)),
        3 => a.state.cmp(&b.state),
        4 => a.config.start_type.cmp(&b.config.start_type),
        5 => lower(&a.config.group).cmp(&lower(&b.config.group)),
        _ => std::cmp::Ordering::Equal,
    }
    .then_with(|| lower(&a.name).cmp(&lower(&b.name)));
    if descending { cmp.reverse() } else { cmp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tml_core::services::{Config, ServiceState, StartType};

    #[test]
    fn service_columns_sort_both_directions() {
        let svc = |name: &str, pid, state, start_type, group: &str| Service {
            name: name.into(),
            display_name: format!("显示 {name}"),
            pid,
            state,
            config: Config { start_type, group: group.into(), ..Default::default() },
            ..Default::default()
        };
        for key in 0..6 {
            let mut rows = vec![
                svc("Beta", 0, ServiceState::Stopped, StartType::Manual, "netsvcs"),
                svc("Alpha", 812, ServiceState::Running, StartType::Auto, ""),
            ];
            rows.sort_by(|a, b| compare(a, b, key, false));
            let first = rows[0].name.clone();
            rows.sort_by(|a, b| compare(a, b, key, true));
            assert_ne!(rows[0].name, first, "列 {key}");
        }
        let row = service_row(&svc("Alpha", 812, ServiceState::Running, StartType::AutoDelayed, "g"));
        assert_eq!((row.state.as_str(), row.start_type.as_str(), row.pid), ("正在运行", "自动（延迟启动）", 812));
        assert!(!row.can_start && !row.can_stop);
    }
}

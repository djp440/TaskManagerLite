//! 界面线程上的状态与交互逻辑。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tml_core::{
    EndError, ProcessKey, ProcessSnapshot, Sample, SortKey, SystemMetrics, ViewOptions, ViewRow, build_view,
    end_process, tree_from,
};

mod cpu;
mod resources;

use crate::format;
use crate::logging::Log;
use crate::sampling::{Command, SamplerHandle};
use crate::{AppWindow, CpuData, ProcRow, Resource, ResourceKind};
use cpu::CpuState;
use resources::{Edit, Instance, ResourceSet};

const STATUS_ENDED: i32 = 1;
const STATUS_DENIED: i32 = 2;
const STATUS_GONE: i32 = 3;
const STATUS_ELEVATION_CANCELLED: i32 = 4;
const STATUS_ERROR: i32 = 5;

const CONFIRM_CRITICAL: i32 = 1;
const CONFIRM_TREE: i32 = 2;

pub struct Controller {
    ui: slint::Weak<AppWindow>,
    rows: Rc<VecModel<ProcRow>>,
    resources: Rc<VecModel<Resource>>,
    sampler: SamplerHandle,
    state: RefCell<State>,
    pub log: RefCell<Log>,
}

#[derive(Default)]
struct State {
    snapshot: ProcessSnapshot,
    view: Vec<ViewRow>,
    opts: ViewOptions,
    selected: Option<ProcessKey>,
    /// 性能页的资源实例、曲线历史与选中项。
    resources: ResourceSet,
    /// CPU 详情：每个逻辑处理器的历史与基本信息。
    cpu: CpuState,
    system: SystemMetrics,
    /// 采样中断后，下一个点前需要先插入断点。
    gap: bool,
    paused: bool,
    visible: bool,
    /// 等待用户确认要结束的进程，按结束顺序排列。
    pending: Vec<ProcessKey>,
    first_data: bool,
}

fn sort_key(i: i32) -> SortKey {
    match i {
        0 => SortKey::Name,
        1 => SortKey::Pid,
        3 => SortKey::Memory,
        4 => SortKey::Disk,
        _ => SortKey::Cpu,
    }
}

fn sort_index(k: SortKey) -> i32 {
    match k {
        SortKey::Name => 0,
        SortKey::Pid => 1,
        SortKey::Cpu => 2,
        SortKey::Memory => 3,
        SortKey::Disk => 4,
    }
}

impl Controller {
    pub fn new(ui: &AppWindow, sampler: SamplerHandle, log: Log) -> Rc<Self> {
        let rows = Rc::new(VecModel::default());
        let set = ResourceSet::new(resources::summary_instances());
        let resources = Rc::new(VecModel::from(set.instances().iter().map(Self::resource_row).collect::<Vec<_>>()));
        ui.set_rows(ModelRc::from(rows.clone()));
        ui.set_resources(ModelRc::from(resources.clone()));
        ui.set_elevated(tml_core::is_elevated());
        let state = State { visible: true, resources: set, ..Default::default() };
        let c = Rc::new(Self {
            ui: ui.as_weak(),
            rows,
            resources,
            sampler,
            state: RefCell::new(state),
            log: RefCell::new(log),
        });
        c.sync_selection(&c.state.borrow());
        c.state.borrow().cpu.attach(ui);
        c.bind(ui);
        c
    }

    /// 只含元数据、数值为空的资源卡片，在首个采样到来前占位。
    fn resource_row(i: &Instance) -> Resource {
        Resource {
            id: i.id.as_str().into(),
            kind: i.kind,
            tint: i.tint,
            title: i.title.as_str().into(),
            caption: i.caption.as_str().into(),
            stat_labels: ModelRc::new(VecModel::from(
                i.stat_labels.iter().map(|l| SharedString::from(l.as_str())).collect::<Vec<_>>(),
            )),
            ..Default::default()
        }
    }

    /// 选中项（实例 id 与下标）同步到界面。
    fn sync_selection(&self, s: &State) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected_resource(s.resources.selected().into());
            ui.set_resource_index(s.resources.selected_index() as i32);
        }
    }

    /// 选中指定实例（界面点击，或启动时恢复上次选中的资源）。实例不存在时保持原选中项。
    pub fn select_resource(&self, id: &str) -> bool {
        let ok = self.state.borrow_mut().resources.select(id);
        self.sync_selection(&self.state.borrow());
        ok
    }

    fn bind(self: &Rc<Self>, ui: &AppWindow) {
        let weak = Rc::downgrade(self);
        let with = move |f: fn(&Controller)| {
            let weak = weak.clone();
            move || {
                if let Some(c) = weak.upgrade() {
                    f(&c);
                }
            }
        };
        ui.on_end_process(with(Controller::request_end));
        ui.on_end_tree(with(Controller::request_end_tree));
        ui.on_confirm_accepted(with(Controller::confirm));
        ui.on_elevate(with(Controller::elevate));
        ui.on_graph_resized(with(Controller::redraw_graphs));
        ui.global::<CpuData>().on_cell_resized(with(Controller::redraw_graphs));
        let c = Rc::downgrade(self);
        ui.on_resource_selected(move |id| {
            if let Some(c) = c.upgrade() {
                c.select_resource(&id);
            }
        });

        let c = Rc::downgrade(self);
        ui.on_select(move |i| {
            if let Some(c) = c.upgrade() {
                let mut s = c.state.borrow_mut();
                s.selected = s.view.get(i as usize).map(|r| s.snapshot.processes[r.index].key);
            }
        });
        let c = Rc::downgrade(self);
        ui.on_sort_by(move |k| {
            if let Some(c) = c.upgrade() {
                {
                    let mut s = c.state.borrow_mut();
                    let key = sort_key(k);
                    if s.opts.sort_key == key {
                        s.opts.descending = !s.opts.descending;
                    } else {
                        s.opts.sort_key = key;
                        // 名称和 PID 默认升序，指标列默认降序。
                        s.opts.descending = !matches!(key, SortKey::Name | SortKey::Pid);
                    }
                }
                c.refresh_rows();
            }
        });
        let c = Rc::downgrade(self);
        ui.on_toggle(move |i| {
            if let Some(c) = c.upgrade() {
                {
                    let mut s = c.state.borrow_mut();
                    if let Some(key) = s.view.get(i as usize).map(|r| s.snapshot.processes[r.index].key)
                        && !s.opts.collapsed.remove(&key)
                    {
                        s.opts.collapsed.insert(key);
                    }
                }
                c.refresh_rows();
            }
        });
        let c = Rc::downgrade(self);
        ui.on_filter_edited(move |text| {
            if let Some(c) = c.upgrade() {
                c.state.borrow_mut().opts.filter = text.to_string();
                c.refresh_rows();
            }
        });
        let c = Rc::downgrade(self);
        ui.on_tree_changed(move |tree| {
            if let Some(c) = c.upgrade() {
                c.state.borrow_mut().opts.tree = tree;
                c.refresh_rows();
            }
        });
        let c = Rc::downgrade(self);
        ui.on_interval_changed(move |i| {
            if let Some(c) = c.upgrade() {
                let interval = match i {
                    0 => Some(Duration::from_millis(500)),
                    1 => Some(Duration::from_secs(1)),
                    2 => Some(Duration::from_secs(2)),
                    _ => None,
                };
                {
                    let mut s = c.state.borrow_mut();
                    if interval.is_none() || s.paused {
                        s.gap = true;
                    }
                    s.paused = interval.is_none();
                }
                c.sampler.send(Command::Interval(interval));
            }
        });
    }

    /// 由定时器轮询窗口是否最小化。
    pub fn set_visible(&self, visible: bool) {
        let mut s = self.state.borrow_mut();
        if s.visible == visible {
            return;
        }
        s.visible = visible;
        s.gap = true;
        drop(s);
        self.sampler.send(Command::Visible(visible));
    }

    /// 截图模式：逻辑处理器网格改用演示曲线。
    pub fn use_demo_cpu_history(&self) {
        self.state.borrow_mut().cpu.use_demo_history(Instant::now());
    }

    pub fn has_first_data(&self) -> bool {
        self.state.borrow().first_data
    }

    pub fn on_sample(&self, sample: Sample) {
        let now = Instant::now();
        {
            let mut s = self.state.borrow_mut();
            let m = sample.system;
            s.resources.update(resources::summary_instances());
            if std::mem::take(&mut s.gap) {
                s.resources.push_gap(now);
                s.cpu.push_gap(now);
            }
            // 首次采样没有基线，CPU、磁盘、网络都是 0，不画进曲线。
            if s.first_data {
                s.resources.push(resources::CPU, now, Some(m.cpu_usage as f64));
                s.resources.push(resources::MEMORY, now, Some(m.memory_used as f64));
                s.resources.push(resources::DISK, now, Some(m.disk_active as f64));
                s.resources.push(resources::NETWORK, now, Some(m.net_recv + m.net_send));
            }
            let with_history = s.first_data;
            s.cpu.record(now, sample.cpu, with_history);
            s.snapshot = sample.snapshot;
            s.system = m;
            s.first_data = true;
            self.update_resources(&s, now);
        }
        self.refresh_rows();
    }

    /// 迷你图的尺寸与 performance-page.slint 中 ResourceCard 的布局一致。
    const MINI: (f64, f64) = (72.0, 44.0);

    fn update_resources(&self, s: &State, now: Instant) {
        let Some(ui) = self.ui.upgrade() else { return };
        let m = &s.system;
        let detail = match (ui.get_graph_width() as f64, ui.get_graph_height() as f64) {
            (w, h) if w > 1.0 && h > 1.0 => (w, h),
            _ => (600.0, 300.0),
        };
        let mem_pct = if m.memory_total > 0 { m.memory_used as f64 / m.memory_total as f64 * 100.0 } else { 0.0 };
        let cpu = format::percent(m.cpu_usage);
        let gib = |b: u64| b as f64 / 1024.0 / 1024.0 / 1024.0;
        let mem = format!("{:.1}/{:.1} GB", gib(m.memory_used), gib(m.memory_total));
        let rows: Vec<Resource> = s
            .resources
            .instances()
            .iter()
            .map(|inst| {
                let history = s.resources.history(&inst.id);
                // (纵轴顶端取值, 卡片摘要, 纵轴标注, 统计项取值)
                let (scale, value, scale_label, stats) = match inst.kind {
                    ResourceKind::Cpu => {
                        (100.0, s.cpu.card_value(&cpu), "100%".to_string(), s.cpu.stats(&cpu, s.snapshot.processes.len()))
                    }
                    ResourceKind::Memory => (
                        m.memory_total.max(1) as f64,
                        format!("{mem} ({mem_pct:.0}%)"),
                        format::gigabytes(m.memory_total),
                        vec![
                            format::gigabytes(m.memory_used),
                            format::gigabytes(m.memory_total - m.memory_used),
                            format::gigabytes(m.memory_total),
                        ],
                    ),
                    ResourceKind::Disk => (
                        100.0,
                        format::percent(m.disk_active),
                        "100%".to_string(),
                        vec![format::percent(m.disk_active), format::rate(m.disk_read), format::rate(m.disk_write)],
                    ),
                    ResourceKind::Network => {
                        let scale = format::nice_scale(history.max(), 100.0 * 1024.0);
                        (
                            scale,
                            format!("↓ {}  ↑ {}", format::rate(m.net_recv), format::rate(m.net_send)),
                            format::rate(scale),
                            vec![format::rate(m.net_recv), format::rate(m.net_send)],
                        )
                    }
                };
                let (line, area) = history.paths(now, scale, detail);
                let (mini_line, mini_area) = history.paths(now, scale, Self::MINI);
                Resource {
                    detail: if inst.kind == ResourceKind::Cpu { s.cpu.model_name() } else { String::new() }.into(),
                    value: value.into(),
                    line: line.into(),
                    area: area.into(),
                    mini_line: mini_line.into(),
                    mini_area: mini_area.into(),
                    scale: scale_label.into(),
                    stats: ModelRc::new(VecModel::from(stats.into_iter().map(SharedString::from).collect::<Vec<_>>())),
                    ..Self::resource_row(inst)
                }
            })
            .collect();
        self.apply_resources(s, rows);
        s.cpu.refresh(&ui, now);
        ui.set_process_count(s.snapshot.processes.len() as i32);
        ui.set_cpu_summary(cpu.into());
        ui.set_memory_summary(format!("{mem_pct:.0}%").into());
    }

    /// 把最新的卡片写入界面模型：先按实例 id 增删行，再逐行更新数据，最后同步选中项。
    fn apply_resources(&self, s: &State, rows: Vec<Resource>) {
        let old: Vec<String> = (0..self.resources.row_count())
            .filter_map(|i| self.resources.row_data(i))
            .map(|r| r.id.to_string())
            .collect();
        for edit in resources::diff_ids(&old, &s.resources.ids()) {
            match edit {
                Edit::Remove(i) => {
                    self.resources.remove(i);
                }
                Edit::Insert(i) => self.resources.insert(i, Resource::default()),
            }
        }
        for (i, r) in rows.into_iter().enumerate() {
            self.resources.set_row_data(i, r);
        }
        self.sync_selection(s);
    }

    fn redraw_graphs(&self) {
        let s = self.state.borrow();
        if s.first_data {
            self.update_resources(&s, Instant::now());
        }
    }

    /// 按当前视图选项重建进程列表，并保持选中的进程不变。
    fn refresh_rows(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut s = self.state.borrow_mut();
        s.view = build_view(&s.snapshot, &s.opts);
        let procs = &s.snapshot.processes;
        let mut selected_index = -1;
        let rows: Vec<ProcRow> = s
            .view
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let p = &procs[r.index];
                if Some(p.key) == s.selected {
                    selected_index = i as i32;
                }
                ProcRow {
                    name: p.name.as_str().into(),
                    pid: p.key.pid as i32,
                    cpu: format::percent(p.cpu_usage).into(),
                    memory: format::megabytes(p.private_working_set).into(),
                    disk: format::disk_rate(p.disk_rate).into(),
                    cpu_heat: (p.cpu_usage / 40.0).min(1.0),
                    memory_heat: (p.private_working_set as f32 / (1024.0 * 1024.0 * 1024.0)).min(1.0),
                    disk_heat: (p.disk_rate / (20.0 * 1024.0 * 1024.0)).min(1.0) as f32,
                    depth: r.depth as i32,
                    has_children: r.has_children,
                    expanded: r.expanded,
                    critical: p.critical,
                }
            })
            .collect();
        if selected_index < 0 {
            s.selected = None;
        }
        drop(s);

        // 原地更新，避免列表重置滚动位置。
        let old = self.rows.row_count();
        for (i, row) in rows.iter().enumerate() {
            if i < old {
                if self.rows.row_data(i).as_ref() != Some(row) {
                    self.rows.set_row_data(i, row.clone());
                }
            } else {
                self.rows.push(row.clone());
            }
        }
        for i in (rows.len()..old).rev() {
            self.rows.remove(i);
        }

        let st = self.state.borrow();
        ui.set_selected(selected_index);
        ui.set_sort_key(sort_index(st.opts.sort_key));
        ui.set_sort_descending(st.opts.descending);
    }

    fn selected_process(&self) -> Option<(ProcessKey, String, bool)> {
        let s = self.state.borrow();
        let key = s.selected?;
        let p = s.snapshot.processes.iter().find(|p| p.key == key)?;
        Some((key, p.name.clone(), p.critical))
    }

    fn request_end(&self) {
        let Some((key, name, critical)) = self.selected_process() else { return };
        if critical {
            self.ask(CONFIRM_CRITICAL, vec![key], name, 0, true);
        } else {
            self.end(vec![key], &name);
        }
    }

    fn request_end_tree(&self) {
        let Some((key, name, _)) = self.selected_process() else { return };
        let (keys, critical) = {
            let s = self.state.borrow();
            let keys = tree_from(&s.snapshot, key);
            let critical = s.snapshot.processes.iter().any(|p| p.critical && keys.contains(&p.key));
            (keys, critical)
        };
        let descendants = keys.len().saturating_sub(1) as i32;
        self.ask(CONFIRM_TREE, keys, name, descendants, critical);
    }

    fn ask(&self, kind: i32, keys: Vec<ProcessKey>, name: String, descendants: i32, critical: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        self.state.borrow_mut().pending = keys;
        ui.set_confirm_name(name.into());
        ui.set_confirm_descendants(descendants);
        ui.set_confirm_critical(critical);
        ui.set_confirm_kind(kind);
    }

    fn confirm(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_confirm_kind(0);
        let keys = std::mem::take(&mut self.state.borrow_mut().pending);
        let name = ui.get_confirm_name().to_string();
        self.end(keys, &name);
    }

    /// 依次结束进程（后代在前）。报告最严重的一个失败。
    fn end(&self, keys: Vec<ProcessKey>, name: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut worst: Option<EndError> = None;
        for key in keys {
            if let Err(e) = end_process(key) {
                self.log.borrow_mut().line(format!("结束进程失败 pid={} {e:?}", key.pid));
                let rank = |e: &EndError| match e {
                    EndError::Gone => 0,
                    EndError::AccessDenied => 1,
                    EndError::Other(_) => 2,
                };
                if worst.as_ref().is_none_or(|w| rank(&e) > rank(w)) {
                    worst = Some(e);
                }
            }
        }
        let (kind, arg) = match worst {
            None => (STATUS_ENDED, name.to_string()),
            Some(EndError::Gone) => (STATUS_GONE, name.to_string()),
            Some(EndError::AccessDenied) => (STATUS_DENIED, name.to_string()),
            Some(EndError::Other(msg)) => (STATUS_ERROR, msg),
        };
        ui.set_status_arg(arg.into());
        ui.set_status_kind(kind);
        self.sampler.send(Command::Wake);
    }

    fn elevate(&self) {
        if tml_core::relaunch_elevated() {
            let _ = slint::quit_event_loop();
        } else if let Some(ui) = self.ui.upgrade() {
            ui.set_status_kind(STATUS_ELEVATION_CANCELLED);
        }
    }
}

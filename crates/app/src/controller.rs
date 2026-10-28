//! 界面线程上的状态与交互逻辑。

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{
    CloseRequestResponse, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};
use tml_core::{
    EndError, Group, GroupHeader, MemoryHardware, MemoryStatus, Process, ProcessKey,
    ProcessSnapshot, Sample, SortKey, SystemMetrics, ViewMode, ViewOptions, ViewRow,
    build_group_view, build_view, end_process, tree_from,
};

mod columns;
mod cpu;
mod memory;
mod mini;
mod process_menu;
mod settings_page;

pub use columns::sort_index as detail_sort_index;
mod resources;

use crate::details::{InfoCache, InfoWorker, Shown};
use crate::format;
use crate::icons::{self, IconCache, IconWorker};
use crate::logging::Log;
use crate::sampling::{Command, SamplerHandle};
use crate::settings::{self, ColumnWidths, Settings, Store};
use crate::tray::{self, Event as TrayEvent, Tray};
use crate::{
    AppWindow, ColumnLayout, Columns, CpuData, DetailChoice, DetailHeader, ProcRow, Resource,
    ResourceKind, placement,
};
use cpu::CpuState;
use resources::{Edit, Instance, ResourceSet};

const STATUS_ENDED: i32 = 1;
const STATUS_DENIED: i32 = 2;
const STATUS_GONE: i32 = 3;
const STATUS_ELEVATION_CANCELLED: i32 = 4;
const STATUS_ERROR: i32 = 5;

const CONFIRM_CRITICAL: i32 = 1;
const CONFIRM_TREE: i32 = 2;
/// 隐藏启动时托盘图标创建失败的重试次数（每秒一次）。
const TRAY_RETRIES: u32 = 30;

pub struct Controller {
    ui: slint::Weak<AppWindow>,
    rows: Rc<VecModel<ProcRow>>,
    resources: Rc<VecModel<Resource>>,
    sampler: SamplerHandle,
    icon_worker: IconWorker,
    /// 静态信息（用户名、路径等）的后台读取线程，第一次显示这类列时才创建。
    info_worker: OnceCell<InfoWorker>,
    /// 显示中的详细信息列（表头）与列选择菜单的选项；原地更新，见 `columns::assign`。
    detail_headers: Rc<VecModel<DetailHeader>>,
    detail_choices: Rc<VecModel<DetailChoice>>,
    /// 当前取图标用的物理像素边长（随显示缩放变化）。
    icon_px: Cell<u32>,
    /// 图标尚未到达时占位用的 1×1 透明图（空 Image 之间比较恒不相等，会让行更新判断失效）。
    blank_icon: Image,
    state: RefCell<State>,
    pub log: RefCell<Log>,
    /// 启动时读到的设置；保存时在它的基础上更新，未识别的字段因此得以保留。
    settings: RefCell<Settings>,
    /// 开发模式（`--screenshot`、`--measure-startup`）下为 `None`，不读写用户设置。
    store: Option<Store>,
    /// 托盘图标：开启“关闭时最小化到托盘”后在首个快照之后创建。
    tray: RefCell<Option<Tray>>,
    /// 托盘图标创建失败过一次就不再重试（例如没有任务栏）。
    tray_failed: Cell<bool>,
    /// 隐藏启动时托盘图标创建失败后已重试的次数（见 `ensure_tray`）。
    tray_retries: Cell<u32>,
    /// 窗口被隐藏到托盘（与最小化不同：任务栏上没有按钮，只能从托盘或再次启动唤起）。
    hidden: Cell<bool>,
    /// 迷你模式的窗口，第一次进入迷你模式时创建（见 `mini.rs`）。
    mini: OnceCell<crate::MiniWindow>,
    /// 已安排在本次事件处理之后开始系统拖动，避免一次拖动里重复投递。
    drag_pending: Cell<bool>,
}

#[derive(Default)]
struct State {
    snapshot: ProcessSnapshot,
    /// 应用进程的 PID；采样线程只在分组视图下枚举窗口，其余时候为 `None`。
    app_pids: Option<HashSet<u32>>,
    view: Vec<ViewRow>,
    opts: ViewOptions,
    selected: Option<ProcessKey>,
    /// 性能页的资源实例、曲线历史与选中项。
    resources: ResourceSet,
    /// CPU 详情：每个逻辑处理器的历史与基本信息。
    cpu: CpuState,
    system: SystemMetrics,
    disks: Vec<tml_core::PhysicalDisk>,
    networks: Vec<tml_core::NetworkAdapter>,
    gpus: Vec<tml_core::Gpu>,
    memory: MemoryStatus,
    /// 内存硬件信息只读取一次，首个快照之后才有。
    memory_hardware: Option<MemoryHardware>,
    /// 采样中断后，下一个点前需要先插入断点。
    gap: bool,
    paused: bool,
    visible: bool,
    /// 迷你模式：只显示资源卡片，只采系统指标，卡片不画详情大图。
    mini: bool,
    /// 后台继续采样：窗口不可见时只采系统指标，见 `sampling::plan`。
    background: bool,
    /// 恢复可见后的下一次完整采样把曲线连续性写进日志（供自动化验收核对）。
    report_history: bool,
    /// 等待用户确认要结束的进程，按结束顺序排列。
    pending: Vec<ProcessKey>,
    first_data: bool,
    icons: IconCache<Image>,
    /// 显示中的“详细信息”列（有序）及其宽度。
    details: Vec<Shown>,
    /// 静态信息缓存；只在显示了需要它的列时维护。
    info: InfoCache,
}

impl State {
    /// 托盘提示里的采样状态：手动暂停优先于“隐藏且没开后台采样”。
    fn tray_sampling(&self) -> tray::Sampling {
        if self.paused {
            tray::Sampling::Paused
        } else if self.sampling() {
            tray::Sampling::Live
        } else {
            tray::Sampling::Hidden
        }
    }

    /// 当前是否在采样（见 `sampling::plan`）；它的变化意味着曲线上要留断点。
    fn sampling(&self) -> bool {
        crate::sampling::plan(self.visible, self.mini, self.background, self.paused)
            != crate::sampling::Plan::Idle
    }
}

fn sort_key(i: i32) -> SortKey {
    match i {
        0 => SortKey::Name,
        1 => SortKey::Pid,
        3 => SortKey::Memory,
        4 => SortKey::Disk,
        _ => columns::sort_column(i).map_or(SortKey::Cpu, SortKey::Detail),
    }
}

fn view_mode(i: i32) -> ViewMode {
    match i {
        1 => ViewMode::Tree,
        2 => ViewMode::Group,
        _ => ViewMode::Flat,
    }
}

fn view_mode_index(m: ViewMode) -> i32 {
    match m {
        ViewMode::Flat => 0,
        ViewMode::Tree => 1,
        ViewMode::Group => 2,
    }
}

/// 分组标题行上显示的文字。
fn group_title(h: GroupHeader) -> String {
    match h.group {
        Group::Apps => format!("应用 ({})", h.count),
        Group::Background => format!("后台进程 ({})", h.count),
    }
}

fn sort_index(k: SortKey) -> i32 {
    match k {
        SortKey::Name => 0,
        SortKey::Pid => 1,
        SortKey::Cpu => 2,
        SortKey::Memory => 3,
        SortKey::Disk => 4,
        SortKey::Detail(c) => columns::sort_index(c),
    }
}

impl Controller {
    pub fn new(
        ui: &AppWindow,
        sampler: SamplerHandle,
        log: Log,
        settings: Settings,
        store: Option<Store>,
    ) -> Rc<Self> {
        let rows = Rc::new(VecModel::default());
        let set = ResourceSet::new(resources::summary_instances());
        let resources = Rc::new(VecModel::from(
            set.instances()
                .iter()
                .map(Self::resource_row)
                .collect::<Vec<_>>(),
        ));
        ui.set_rows(ModelRc::from(rows.clone()));
        ui.set_resources(ModelRc::from(resources.clone()));
        ui.set_elevated(tml_core::is_elevated());
        let mut state = State {
            visible: true,
            background: settings.background_sampling,
            resources: set,
            ..Default::default()
        };
        state.opts.sort_key = settings.sort_key;
        state.opts.descending = settings.descending;
        state.opts.mode = settings.view;
        state.details = settings.details.clone();
        // 排序列已不在显示中（例如设置文件被手动改过）时回到默认排序。
        columns::fix_sort(
            &mut state.opts.sort_key,
            &mut state.opts.descending,
            &state.details,
        );
        ui.set_view_mode(view_mode_index(settings.view));
        ui.set_page(settings.page);
        let cols = ui.global::<Columns>();
        cols.set_pid(settings.columns.pid);
        cols.set_cpu(settings.columns.cpu);
        cols.set_memory(settings.columns.memory);
        cols.set_disk(settings.columns.disk);
        // 列宽总和超出可用宽度时的有效列宽（保存值不变，窗口放大后自然恢复）。
        cols.on_fit(|available, name_min, pid, cpu, memory, disk| {
            let w = ColumnWidths {
                pid,
                cpu,
                memory,
                disk,
            }
            .fit(available, name_min);
            ColumnLayout {
                pid: w.pid,
                cpu: w.cpu,
                memory: w.memory,
                disk: w.disk,
            }
        });
        let c = Rc::new(Self {
            ui: ui.as_weak(),
            rows,
            resources,
            sampler,
            icon_worker: IconWorker::spawn(|event| {
                let _ = slint::invoke_from_event_loop(move || {
                    crate::CONTROLLER.with(|c| {
                        if let Some(c) = c.borrow().as_ref() {
                            c.on_icon_event(event);
                        }
                    })
                });
            }),
            info_worker: OnceCell::new(),
            detail_headers: Rc::new(VecModel::default()),
            detail_choices: Rc::new(VecModel::default()),
            icon_px: Cell::new(0),
            blank_icon: Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(1, 1)),
            state: RefCell::new(state),
            log: RefCell::new(log),
            settings: RefCell::new(settings.clone()),
            store,
            tray: RefCell::new(None),
            tray_failed: Cell::new(false),
            tray_retries: Cell::new(0),
            hidden: Cell::new(false),
            mini: OnceCell::new(),
            drag_pending: Cell::new(false),
        });
        // 恢复上次选中的资源实例；实例暂时不存在时先用 CPU，实例出现后自动切回。
        let restored = c.state.borrow_mut().resources.restore(&settings.resource);
        c.sync_selection(&c.state.borrow());
        c.state.borrow().cpu.attach(ui);
        if !restored {
            c.log.borrow_mut().line(format!(
                "设置中的资源 {} 暂不存在，待其出现后恢复",
                settings.resource
            ));
        }
        c.bind(ui);
        c.bind_process_menu(ui);
        c.bind_columns(ui);
        c.bind_mini(ui);
        c.bind_settings(ui);
        ui.set_pinned(settings.always_on_top);
        // 默认的 1 秒与采样线程的初始值一致，无需通知。
        if settings.interval_index != 1 {
            ui.set_interval_index(settings.interval_index);
            c.set_interval(settings.interval_index);
        }
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
                i.stat_labels
                    .iter()
                    .map(|l| SharedString::from(l.as_str()))
                    .collect::<Vec<_>>(),
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
        let s = self.state.borrow();
        self.sync_selection(&s);
        if s.first_data {
            self.update_resources(&s, Instant::now());
        }
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
        ui.global::<CpuData>()
            .on_grid_resized(with(Controller::redraw_graphs));
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
                // 分组标题行不可选中，忽略。
                if let Some(key) = process_key_at(&s.view, &s.snapshot.processes, i as usize) {
                    s.selected = Some(key);
                }
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
                        // 名称、PID 与文字列默认升序，指标列默认降序。
                        s.opts.descending = settings::default_descending(key);
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
                    if let Some(key) = process_key_at(&s.view, &s.snapshot.processes, i as usize) {
                        // 树形默认展开，记录折叠项；分组默认折叠，记录展开项。
                        let set = if s.opts.mode == ViewMode::Group {
                            &mut s.opts.expanded
                        } else {
                            &mut s.opts.collapsed
                        };
                        if !set.remove(&key) {
                            set.insert(key);
                        }
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
        ui.on_view_changed(move |mode| {
            if let Some(c) = c.upgrade() {
                c.set_view_mode(view_mode(mode));
            }
        });
        let c = Rc::downgrade(self);
        ui.on_interval_changed(move |i| {
            if let Some(c) = c.upgrade() {
                c.set_interval(i);
            }
        });
    }

    /// 切换视图模式。分组视图需要采样线程枚举窗口：切到分组时通知它并等下一个快照带着应用进程回来再刷新列表。
    fn set_view_mode(&self, mode: ViewMode) {
        let was_group = {
            let mut s = self.state.borrow_mut();
            let was = s.opts.mode == ViewMode::Group;
            s.opts.mode = mode;
            if mode != ViewMode::Group {
                s.app_pids = None;
            }
            was
        };
        if (mode == ViewMode::Group) != was_group {
            self.sampler.send(Command::Apps(mode == ViewMode::Group));
        }
        self.refresh_rows();
    }

    fn set_interval(&self, index: i32) {
        let interval = match index {
            0 => Some(Duration::from_millis(500)),
            1 => Some(Duration::from_secs(1)),
            2 => Some(Duration::from_secs(2)),
            _ => None,
        };
        {
            let mut s = self.state.borrow_mut();
            if interval.is_none() || s.paused {
                s.gap = true;
            }
            s.paused = interval.is_none();
        }
        self.sampler.send(Command::Interval(interval));
    }

    /// 记录最近一次的普通（非最大化、非最小化）窗口矩形，由定时器周期调用。
    /// 迷你模式下完整窗口隐藏，改为记录迷你窗口的位置。
    pub fn track_window(&self) {
        if self.is_mini() {
            self.capture_mini();
        } else if let Some(ui) = self.ui.upgrade() {
            placement::capture(&ui, &mut self.settings.borrow_mut());
        }
    }

    /// 收集当前状态并写入设置文件（退出前、以管理员身份重启前调用）。
    pub fn save_settings(&self) {
        let (Some(store), Some(ui)) = (&self.store, self.ui.upgrade()) else {
            return;
        };
        self.track_window();
        let mut settings = self.settings.borrow_mut();
        {
            let s = self.state.borrow();
            settings.sort_key = s.opts.sort_key;
            settings.descending = s.opts.descending;
            settings.view = s.opts.mode;
            settings.resource = s.resources.selected_for_save().to_owned();
            settings.details = s.details.clone();
        }
        settings.mini.active = self.is_mini();
        settings.interval_index = ui.get_interval_index();
        // 设置页不记录，记录进入它之前的页面。
        settings.page = match ui.get_page() {
            2 => ui.get_page_before_settings(),
            p => p,
        };
        settings.always_on_top = ui.get_pinned();
        let cols = ui.global::<Columns>();
        settings.columns = ColumnWidths {
            pid: cols.get_pid(),
            cpu: cols.get_cpu(),
            memory: cols.get_memory(),
            disk: cols.get_disk(),
        }
        .clamped();
        let result = store.save(&settings);
        drop(settings);
        let mut log = self.log.borrow_mut();
        match result {
            Ok(()) => log.line(format!("设置已保存 {}", store.path().display())),
            Err(e) => log.line(format!("设置保存失败 {e}")),
        }
    }

    /// 由定时器轮询窗口是否最小化。
    pub fn set_visible(&self, visible: bool) {
        let mut s = self.state.borrow_mut();
        if s.visible == visible {
            return;
        }
        let before = s.sampling();
        s.visible = visible;
        // 采样真的中断（或从中断中恢复）才在曲线上留断点；后台继续采样时曲线保持连续。
        if before != s.sampling() {
            s.gap = true;
        }
        s.report_history = visible;
        self.update_tray_tip(&s);
        drop(s);
        self.sampler.send(Command::Visible(visible));
    }

    /// 由定时器轮询：窗口隐藏到托盘或最小化时不可见。
    pub fn poll_visible(&self) {
        if let Some(minimized) = self.with_active(|w| w.is_minimized()) {
            self.set_visible(!self.hidden.get() && !minimized);
        }
    }

    /// 关闭窗口：开启了“关闭时最小化到托盘”且托盘图标可用则隐藏到托盘，否则保存设置并退出。
    pub fn close_requested(&self) -> CloseRequestResponse {
        self.save_settings();
        if self.settings.borrow().close_to_tray && self.tray.borrow().is_some() {
            self.hidden.set(true);
            self.set_visible(false);
            self.log_hidden();
        } else {
            let _ = slint::quit_event_loop();
        }
        CloseRequestResponse::HideWindow
    }

    /// 显示并前置窗口（托盘菜单、托盘单击、再次启动 exe 都走这里）；隐藏时先重新显示。
    pub fn show_window(&self) {
        if self.hidden.replace(false) {
            let _ = self.with_active(|w| w.show());
            if self.is_mini() {
                Self::reapply_opacity_soon();
            }
        }
        // 立即恢复完整采样，数据随即刷新；窗口若仍最小化，下一次轮询会改回。
        self.set_visible(true);
        tml_core::activate_own_window();
    }

    /// 隐藏窗口到托盘；没有托盘图标时窗口将无法唤回，所以什么也不做。
    fn hide_window(&self) {
        if self.tray.borrow().is_none() {
            return;
        }
        self.track_window();
        self.hidden.set(true);
        let _ = self.with_active(|w| w.hide());
        self.set_visible(false);
        self.log_hidden();
    }

    /// 隐藏到托盘时记一行日志，带上此刻的托盘提示文字（自动化验收据此核对 CPU/内存读数）。
    fn log_hidden(&self) {
        let s = self.state.borrow();
        let tip = tray::tooltip(self.tray_reading(&s), s.tray_sampling()).replace('\n', " | ");
        self.log.borrow_mut().line(format!("隐藏到托盘 提示={tip}"));
    }

    fn toggle_window(&self) {
        let Some(minimized) = self.with_active(|w| w.is_minimized()) else {
            return;
        };
        if self.hidden.get() || minimized {
            self.show_window();
        } else {
            self.hide_window();
        }
    }

    /// 托盘菜单“退出”：保存设置后结束事件循环。
    fn quit(&self) {
        self.save_settings();
        let _ = slint::quit_event_loop();
    }

    fn on_tray_event(&self, event: TrayEvent) {
        match event {
            TrayEvent::Toggle => self.toggle_window(),
            TrayEvent::Show => self.show_window(),
            TrayEvent::Quit => self.quit(),
        }
    }

    /// 开启“关闭时最小化到托盘”且还没有图标时创建托盘图标。
    fn ensure_tray(&self) {
        if !self.settings.borrow().close_to_tray
            || self.tray.borrow().is_some()
            || self.tray_failed.get()
        {
            return;
        }
        let tip = {
            let s = self.state.borrow();
            tray::tooltip(self.tray_reading(&s), s.tray_sampling())
        };
        let tray = Tray::new(&tip, |event| {
            let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
            if let Some(c) = controller {
                c.on_tray_event(event);
            }
        });
        self.tray_failed.set(tray.is_none());
        self.log.borrow_mut().line(if tray.is_some() {
            "托盘图标已创建"
        } else {
            "托盘图标创建失败"
        });
        let failed = tray.is_none();
        *self.tray.borrow_mut() = tray;
        if failed && self.hidden.get() {
            // 开机启动时直接隐藏到托盘：登录早期通知区可能还没就绪，每秒重试；一直建不出来就把窗口显示出来，否则用户找不到它。
            let tries = self.tray_retries.get();
            if tries < TRAY_RETRIES {
                self.tray_retries.set(tries + 1);
                slint::Timer::single_shot(Duration::from_secs(1), || {
                    let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
                    if let Some(c) = controller {
                        c.tray_failed.set(false);
                        c.ensure_tray();
                    }
                });
            } else {
                self.show_window();
            }
        }
    }

    /// 设置页的“关闭时最小化到托盘”开关。
    pub fn set_close_to_tray(&self, on: bool) {
        self.settings.borrow_mut().close_to_tray = on;
        if on {
            self.ensure_tray();
        } else {
            // 先把隐藏的窗口唤回，再移除图标，否则窗口无处可找。
            if self.hidden.get() {
                self.show_window();
            }
            self.tray.borrow_mut().take();
        }
    }

    /// 退出事件循环之后移除托盘图标，避免残留“幽灵图标”。
    pub fn remove_tray(&self) {
        self.tray.borrow_mut().take();
    }

    /// 托盘提示用的读数：最近一次系统指标；还没有采样时为 `None`。
    fn tray_reading(&self, s: &State) -> Option<(f32, u64, u64)> {
        s.first_data.then(|| {
            (
                s.system.cpu_usage,
                s.system.memory_used,
                s.system.memory_total,
            )
        })
    }

    fn update_tray_tip(&self, s: &State) {
        if let Some(tray) = &*self.tray.borrow() {
            tray.set_tip(&tray::tooltip(self.tray_reading(s), s.tray_sampling()));
        }
    }

    /// 设置页的“后台继续采样”开关。
    pub fn set_background_sampling(&self, on: bool) {
        let mut s = self.state.borrow_mut();
        if s.background == on {
            return;
        }
        let before = s.sampling();
        s.background = on;
        if before != s.sampling() {
            s.gap = true;
        }
        drop(s);
        self.settings.borrow_mut().background_sampling = on;
        self.sampler.send(Command::Background(on));
    }

    /// 截图模式：逻辑处理器网格改用演示曲线。
    pub fn use_demo_cpu_history(&self, cores: usize) {
        self.state.borrow_mut().cpu.use_demo_history(cores);
    }

    /// 截图模式：核对逻辑处理器网格的曲线路径尺寸是否是最新的（见 `CpuState::check_paths_current`）。
    pub fn check_cpu_paths(&self) -> Result<(), String> {
        let Some(ui) = self.ui.upgrade() else {
            return Ok(());
        };
        let s = self.state.borrow();
        s.cpu
            .check_paths_current(&ui, s.resources.selected() == resources::CPU)
    }

    pub fn has_first_data(&self) -> bool {
        self.state.borrow().first_data
    }

    pub fn on_sample(&self, sample: Sample) {
        let now = Instant::now();
        // 只采系统指标的采样（窗口不可见）不带进程快照：保留上一份，界面也不刷新，回到前台时马上会补一次完整采样。
        let fresh = sample.processes_fresh;
        if let Some(ui) = self.ui.upgrade() {
            let etw = sample.disk_source == tml_core::DiskSource::Etw;
            ui.global::<Columns>().set_disk_accurate(etw);
            ui.global::<crate::SettingsData>()
                .set_accurate_disk_active(etw);
        }
        {
            let mut s = self.state.borrow_mut();
            let m = sample.system;
            s.resources
                .sync_devices(&sample.disks, &sample.networks);
            s.resources.sync_gpus(&sample.gpus);
            let gpu_gap = s.gap;
            s.resources.push_gpu_memory(&sample.gpus, now, gpu_gap);
            let resumed = std::mem::take(&mut s.gap);
            if resumed {
                s.resources.push_gap(now);
                s.cpu.push_gap(now);
            }
            // 首次采样没有基线，CPU、磁盘、网络都是 0，不画进曲线。
            if s.first_data {
                for (id, value) in &sample.readings {
                    s.resources.push(id, now, if resumed { None } else { *value });
                }
                // 恢复后的第一份差值覆盖了暂停区间，只用来刷新读数与基线，不计入窗口统计。
                if !resumed {
                    for n in &sample.networks { s.resources.push_network(n, now); }
                }
            }
            let with_history = s.first_data;
            s.cpu.record(now, sample.cpu, with_history);
            if fresh {
                s.snapshot = sample.snapshot;
                s.app_pids = sample.app_pids;
            }
            s.system = m;
            s.disks = sample.disks;
            s.networks = sample.networks;
            s.gpus = sample.gpus;
            s.memory = sample.memory;
            if sample.memory_hardware.is_some() {
                s.memory_hardware = sample.memory_hardware;
            }
            s.first_data = true;
            self.update_tray_tip(&s);
            if fresh || s.mini {
                if fresh && std::mem::take(&mut s.report_history) {
                    let (points, gaps, step) = s.resources.history(resources::CPU).stats();
                    self.log.borrow_mut().line(format!(
                        "恢复可见 后台采样={} CPU曲线 点数={points} 断点={gaps} 最大间隔={} ms",
                        s.background,
                        step.as_millis()
                    ));
                }
                self.update_resources(&s, now);
            }
        }
        // 托盘图标在首个快照之后才创建，不拖慢冷启动。
        self.ensure_tray();
        if fresh {
            self.track_icons();
            self.track_info();
            self.refresh_rows();
        }
    }

    /// 同步图标缓存：显示缩放变化时按新尺寸重取；新进程交给后台解析路径；已退出的进程清出缓存。
    fn track_icons(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let px = icons::physical_size(ui.window().scale_factor());
        let mut guard = self.state.borrow_mut();
        let s = &mut *guard;
        if px != self.icon_px.replace(px) {
            self.log.borrow_mut().line(format!(
                "图标尺寸 {px}px 缩放={}",
                ui.window().scale_factor()
            ));
            s.icons.reset();
            self.icon_worker.extract(vec![None], px);
        }
        let fresh = s.icons.track(s.snapshot.processes.iter().map(|p| p.key));
        drop(guard);
        self.icon_worker.resolve(fresh);
    }

    /// 后台线程送回的路径或图标。图标到达时只更新用到它的行，不重建列表。
    pub fn on_icon_event(&self, event: icons::Event) {
        match event {
            icons::Event::Paths(paths) => {
                let mut wanted = Vec::new();
                let mut touched = std::collections::HashSet::new();
                {
                    let mut s = self.state.borrow_mut();
                    for (key, path) in paths {
                        touched.insert(key);
                        if let Some(path) = s.icons.resolved(key, path) {
                            wanted.push(Some(path));
                        }
                    }
                }
                self.icon_worker.extract(wanted, self.icon_px.get());
                // 拿不到路径的进程此时就能用默认图标。
                self.update_icons(|_, key| touched.contains(&key));
            }
            icons::Event::Icon { path, px, pixels } => {
                // 显示缩放变化前发出的请求，尺寸已不对。
                if px != self.icon_px.get() {
                    return;
                }
                let image = pixels.map(Image::from_rgba8);
                self.state.borrow_mut().icons.loaded(path.as_deref(), image);
                self.update_icons(|s, key| s.icons.depends_on(key, path.as_deref()));
            }
        }
    }

    /// 刷新满足条件的进程所在行的图标。
    fn update_icons(&self, affected: impl Fn(&State, ProcessKey) -> bool) {
        let s = self.state.borrow();
        for (i, key) in icon_rows(&s.view, &s.snapshot.processes, |key| affected(&s, key)) {
            if let Some(mut row) = self.rows.row_data(i) {
                row.icon = s
                    .icons
                    .icon(key)
                    .cloned()
                    .unwrap_or_else(|| self.blank_icon.clone());
                self.rows.set_row_data(i, row);
            }
        }
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
        let mem_pct = if m.memory_total > 0 {
            m.memory_used as f64 / m.memory_total as f64 * 100.0
        } else {
            0.0
        };
        let cpu = format::percent(m.cpu_usage);
        let gib = |b: u64| b as f64 / 1024.0 / 1024.0 / 1024.0;
        let mem = format!("{:.1}/{:.1} GB", gib(m.memory_used), gib(m.memory_total));
        // 内存详情只在选中内存卡片时整形；切换选中项时 select_resource 会立即刷新。
        let mem_text = (s.resources.selected() == resources::MEMORY)
            .then(|| memory::describe(&s.memory, s.memory_hardware.as_ref()));
        let rows: Vec<Resource> = s
            .resources
            .instances()
            .iter()
            .map(|inst| {
                let disk = s
                    .disks
                    .iter()
                    .find(|d| resources::disk_id(d.number) == inst.id);
                let gpu = s.gpus.iter().find(|g| g.id == inst.id);
                let network = s.networks.iter().find(|n| n.id == inst.id);
                let history = s.resources.history(&inst.id);
                // (纵轴顶端取值, 卡片摘要, 纵轴标注, 统计项取值)
                let (scale, value, scale_label, stats) = match inst.kind {
                    // CPU、内存详情页的统计项在各自的数据里（CpuData / MemoryView），卡片只需要摘要。
                    ResourceKind::Cpu => {
                        (100.0, s.cpu.card_value(&cpu), "100%".to_string(), vec![])
                    }
                    ResourceKind::Memory => (
                        m.memory_total.max(1) as f64,
                        format!("{mem} ({mem_pct:.0}%)"),
                        format::gigabytes(m.memory_total),
                        vec![],
                    ),
                    ResourceKind::Disk => (
                        100.0,
                        disk.and_then(|d| d.active)
                            .map(format::percent)
                            .unwrap_or_else(|| "—".into()),
                        "100%".to_string(),
                        disk.map(|d| {
                            vec![
                                d.active.map(format::percent).unwrap_or_else(|| "—".into()),
                                d.response_ms
                                    .map(|v| format!("{v:.1} ms"))
                                    .unwrap_or_else(|| "—".into()),
                                d.read.map(format::rate).unwrap_or_else(|| "—".into()),
                                d.write.map(format::rate).unwrap_or_else(|| "—".into()),
                            ]
                        })
                        .unwrap_or_default(),
                    ),
                    ResourceKind::Gpu => {
                        let g = gpu.unwrap();
                        let memory_ratio = |used: Option<f64>, total: u64| {
                            let used = used.map(|v| format::gigabytes(v as u64)).unwrap_or_else(|| "—".into());
                            if total > 0 { format!("{used} / {}", format::gigabytes(total)) }
                            else { format!("{used} / —") }
                        };
                        (100.0, g.usage.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into()), "100%".into(),
                         vec![g.usage.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "—".into()),
                              memory_ratio(g.dedicated, g.dedicated_total),
                              memory_ratio(g.shared, g.shared_total)])
                    }
                    ResourceKind::Network => {
                        let scale = format::nice_scale(history.max(), 100.0 * 1024.0);
                        (
                            scale,
                            format!(
                                "↓ {}  ↑ {}",
                                network.and_then(|n| n.recv).map(format::rate).unwrap_or_else(|| "—".into()),
                                network.and_then(|n| n.send).map(format::rate).unwrap_or_else(|| "—".into())
                            ),
                            format::rate(scale),
                            vec![network.and_then(|n| n.recv).map(format::rate).unwrap_or_else(|| "—".into()), network.and_then(|n| n.send).map(format::rate).unwrap_or_else(|| "—".into())],
                        )
                    }
                };
                // 迷你模式下完整窗口隐藏，详情大图不画；回到完整模式时 redraw_graphs 立即补画。
                let (line, area) = if s.mini || s.resources.selected() != inst.id {
                    Default::default()
                } else {
                    history.paths(now, scale, detail)
                };
                let (mini_line, mini_area) = history.paths(now, scale, Self::MINI);
                let memory_graphs = gpu.map(|g| s.resources.gpu_memory_paths(g, now, (detail.0, 64.0))).unwrap_or_default();
                let detail = match (&mem_text, inst.kind) {
                    (Some(t), ResourceKind::Memory) => t.header.clone(),
                    (_, ResourceKind::Gpu) => gpu.map(|g| g.name.clone()).unwrap_or_default(),
                    _ => network.map(|n| format!("{} · 链路 {:.0} Mbps{}\n{}\n{}", n.connection, n.link_speed as f64 / 1e6,
                        if n.ssid.is_empty() { String::new() } else { format!(" · SSID {}", n.ssid) },
                        if n.addresses.is_empty() { "地址不可用" } else { &n.addresses }, s.resources.network_stats(&n.id, now))).unwrap_or_default(),
                };

                Resource {
                    gpu_lines: ModelRc::new(VecModel::from(memory_graphs.iter().map(|p| SharedString::from(p.0.clone())).collect::<Vec<_>>())),
                    gpu_areas: ModelRc::new(VecModel::from(memory_graphs.iter().map(|p| SharedString::from(p.2.clone())).collect::<Vec<_>>())),
                    gpu_scales: ModelRc::new(VecModel::from(memory_graphs.iter().map(|p| SharedString::from(p.1.clone())).collect::<Vec<_>>())),
                    adapter_name: gpu.map(|g| g.name.clone()).or_else(|| network.map(|n| n.name.clone())).unwrap_or_default().into(),
                    adapter_description: network.map(|n| n.description.clone()).unwrap_or_default().into(),
                    disk_info: disk
                        .map(|d| {
                            let free = d.volumes.iter().map(|v| v.free).sum::<u64>();
                            let used = d
                                .volumes
                                .iter()
                                .map(|v| v.capacity.saturating_sub(v.free))
                                .sum::<u64>();
                            format!(
                                "{} · {} · 容量 {} · {}\n已挂载卷合计：已用 {} · 可用 {}",
                                d.model,
                                d.media_type,
                                format::gigabytes(d.capacity),
                                if d.system {
                                    "系统盘"
                                } else {
                                    "非系统盘"
                                },
                                format::gigabytes(used),
                                format::gigabytes(free)
                            )
                        })
                        .unwrap_or_default()
                        .into(),
                    volumes: ModelRc::new(VecModel::from(
                        disk.map(|d| {
                            d.volumes
                                .iter()
                                .map(|v| {
                                    SharedString::from(format!(
                                        "{}  {}    容量 {}    已用 {}    可用 {}",
                                        v.mount,
                                        v.filesystem,
                                        format::gigabytes(v.capacity),
                                        format::gigabytes(v.capacity.saturating_sub(v.free)),
                                        format::gigabytes(v.free)
                                    ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    )),
                    detail: if inst.kind == ResourceKind::Cpu {
                        s.cpu.model_name()
                    } else {
                        detail
                    }
                    .into(),
                    value: value.into(),
                    line: line.into(),
                    area: area.into(),
                    mini_line: mini_line.into(),
                    mini_area: mini_area.into(),
                    scale: scale_label.into(),
                    stats: ModelRc::new(VecModel::from(
                        stats
                            .into_iter()
                            .map(SharedString::from)
                            .collect::<Vec<_>>(),
                    )),
                    ..Self::resource_row(inst)
                }
            })
            .collect();
        self.apply_resources(s, rows);
        if s.mini {
            return;
        }
        s.cpu.refresh(
            &ui,
            now,
            s.snapshot.processes.len(),
            &cpu,
            s.resources.selected() == resources::CPU,
        );
        if let Some(t) = mem_text {
            ui.set_memory_view(memory::view(t));
        }
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
        let mut guard = self.state.borrow_mut();
        let s = &mut *guard;
        // 静态信息由后台读到后缓存在 Controller 里；排序和显示前挂到快照的进程上。
        if !s.details.is_empty() {
            for p in &mut s.snapshot.processes {
                p.info = s.info.get(p.key);
            }
        }
        s.view = match (&s.app_pids, s.opts.mode) {
            (Some(apps), ViewMode::Group) => build_group_view(&s.snapshot, apps, &s.opts),
            // 刚切到分组、带窗口信息的快照还没到：保持现有列表，快照一到就会刷新。
            (None, ViewMode::Group) => return,
            _ => build_view(&s.snapshot, &s.opts),
        };
        let procs = &s.snapshot.processes;
        let mut selected_index = -1;
        let rows: Vec<ProcRow> = s
            .view
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if let Some(h) = r.header {
                    // 标题行没有图标；用同一个空白图标，否则空 Image 互不相等，每次刷新都会重写这一行。
                    return ProcRow {
                        name: group_title(h).into(),
                        header: true,
                        icon: self.blank_icon.clone(),
                        ..Default::default()
                    };
                }
                let p = &procs[r.index];
                if Some(p.key) == s.selected {
                    selected_index = i as i32;
                }
                // 分组视图的应用行显示“自身 + 全部后代”的合计，其余行显示各自的值。
                let (cpu, memory, disk) = r
                    .totals
                    .map_or((p.cpu_usage, p.private_working_set, p.disk_rate), |t| {
                        (t.cpu_usage, t.private_working_set, t.disk_rate)
                    });
                ProcRow {
                    name: p.name.as_str().into(),
                    pid: p.key.pid as i32,
                    cpu: format::percent(cpu).into(),
                    memory: format::megabytes(memory).into(),
                    disk: format::disk_rate(disk).into(),
                    cpu_heat: (cpu / 40.0).min(1.0),
                    memory_heat: (memory as f32 / (1024.0 * 1024.0 * 1024.0)).min(1.0),
                    disk_heat: (disk / (20.0 * 1024.0 * 1024.0)).min(1.0) as f32,
                    depth: r.depth as i32,
                    has_children: r.has_children,
                    expanded: r.expanded,
                    critical: p.critical,
                    icon: s
                        .icons
                        .icon(p.key)
                        .cloned()
                        .unwrap_or_else(|| self.blank_icon.clone()),
                    header: false,
                    extra: Self::detail_cells(s, p),
                }
            })
            .collect();
        if selected_index < 0 {
            s.selected = None;
        }
        drop(guard);

        // 原地更新，避免列表重置滚动位置。
        let old = self.rows.row_count();
        for (i, row) in rows.iter().enumerate() {
            if i < old {
                if !self.rows.row_data(i).is_some_and(|old| same_row(&old, row)) {
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
        let Some((key, name, critical)) = self.selected_process() else {
            return;
        };
        if critical {
            self.ask(CONFIRM_CRITICAL, vec![key], name, 0, true);
        } else {
            self.end(vec![key], &name);
        }
    }

    fn request_end_tree(&self) {
        let Some((key, name, _)) = self.selected_process() else {
            return;
        };
        let (keys, critical) = {
            let s = self.state.borrow();
            let keys = tree_from(&s.snapshot, key);
            let critical = s
                .snapshot
                .processes
                .iter()
                .any(|p| p.critical && keys.contains(&p.key));
            (keys, critical)
        };
        let descendants = keys.len().saturating_sub(1) as i32;
        self.ask(CONFIRM_TREE, keys, name, descendants, critical);
    }

    fn ask(
        &self,
        kind: i32,
        keys: Vec<ProcessKey>,
        name: String,
        descendants: i32,
        critical: bool,
    ) {
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
                self.log
                    .borrow_mut()
                    .line(format!("结束进程失败 pid={} {e:?}", key.pid));
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
        // 新进程会立刻读取设置，先保存。
        self.save_settings();
        if tml_core::relaunch_elevated() {
            let _ = slint::quit_event_loop();
        } else if let Some(ui) = self.ui.upgrade() {
            ui.set_status_kind(STATUS_ELEVATION_CANCELLED);
        }
    }
}

/// 两行显示的内容是否相同。`ModelRc` 按指针比较，详细信息单元格要逐项比较内容，
/// 否则每次采样都会重写所有行。
fn same_row(a: &ProcRow, b: &ProcRow) -> bool {
    a.extra.row_count() == b.extra.row_count()
        && a.extra.iter().zip(b.extra.iter()).all(|(x, y)| x == y)
        && ProcRow {
            extra: b.extra.clone(),
            ..a.clone()
        } == *b
}

/// 列表第 `row` 行对应的进程；分组视图的标题行（`index == HEADER_INDEX`）和越界行没有进程。
fn process_key_at(view: &[ViewRow], procs: &[Process], row: usize) -> Option<ProcessKey> {
    let pi = view.get(row).and_then(ViewRow::process)?;
    procs.get(pi).map(|p| p.key)
}

/// 满足 `affected` 的进程所在的行（行下标与进程身份）；标题行没有进程，直接跳过。
fn icon_rows(
    view: &[ViewRow],
    procs: &[Process],
    affected: impl Fn(ProcessKey) -> bool,
) -> Vec<(usize, ProcessKey)> {
    (0..view.len())
        .filter_map(|i| process_key_at(view, procs, i).map(|k| (i, k)))
        .filter(|&(_, k)| affected(k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn p(pid: u32, parent: u32, name: &str) -> Process {
        Process {
            key: ProcessKey {
                pid,
                create_time: pid as u64,
            },
            parent_pid: parent,
            name: name.into(),
            cpu_usage: 0.0,
            private_working_set: 0,
            disk_rate: 0.0,
            critical: false,
            counters: Default::default(),
            info: None,
        }
    }

    /// 应用 chrome(10)（子进程 11 折叠不显示）+ 应用 notepad(20) + 后台进程 svc(30)：
    /// 视图是 [标题, chrome, notepad, 标题, svc]，标题行的 index 是 HEADER_INDEX。
    fn group() -> (ProcessSnapshot, Vec<ViewRow>) {
        let snapshot = ProcessSnapshot {
            processes: vec![
                p(10, 1, "chrome"),
                p(11, 10, "gpu"),
                p(20, 1, "notepad"),
                p(30, 1, "svc"),
            ],
        };
        let apps: HashSet<u32> = [10, 20].into();
        let opts = ViewOptions {
            mode: ViewMode::Group,
            sort_key: SortKey::Pid,
            descending: false,
            ..Default::default()
        };
        let view = build_group_view(&snapshot, &apps, &opts);
        (snapshot, view)
    }

    #[test]
    fn group_view_has_header_rows_with_out_of_range_index() {
        let (_, view) = group();
        assert_eq!(view.len(), 5);
        assert!(view[0].header.is_some() && view[3].header.is_some());
        assert_eq!(view[0].index, tml_core::HEADER_INDEX);
    }

    #[test]
    fn icon_event_in_group_view_skips_headers_without_panic() {
        let (snapshot, view) = group();
        // 所有进程的图标一起到达（含标题行下标越界的情形）：只返回进程行。
        let all = icon_rows(&view, &snapshot.processes, |_| true);
        let pids: Vec<(usize, u32)> = all.iter().map(|&(i, k)| (i, k.pid)).collect();
        assert_eq!(pids, [(1, 10), (2, 20), (4, 30)]);
        // 只刷新后台进程：行下标 4。
        let svc = icon_rows(&view, &snapshot.processes, |k| k.pid == 30);
        assert_eq!(svc.iter().map(|&(i, _)| i).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn selecting_in_group_view_picks_the_row_process_and_ignores_headers() {
        let (snapshot, view) = group();
        let pid_at = |i| process_key_at(&view, &snapshot.processes, i).map(|k| k.pid);
        // 标题行（应用箭头 / 右键 / 点击都走这里）不选中任何进程。
        assert_eq!(pid_at(0), None);
        assert_eq!(pid_at(3), None);
        // 应用行选中的是应用本身（而不是后代或相邻行），后台行同理。
        assert_eq!(pid_at(1), Some(10));
        assert_eq!(pid_at(2), Some(20));
        assert_eq!(pid_at(4), Some(30));
        // 越界不 panic。
        assert_eq!(pid_at(99), None);
    }
}

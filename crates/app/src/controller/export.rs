//! 导出为 CSV 与复制所选行（issue #82）。
//!
//! - 进程页：按当前视图、可见列和排序导出整个列表；树形、分组视图包含层级信息，并且不管节点是否折叠都导出全部后代。
//!   单元格文字与界面完全一致（共用 `basic_texts` / `details::cell`），表头复用界面已有的列标题。
//!   含命令行、映像路径、用户名等敏感列时，先在 Slint 对话框里提示，确认后再弹保存对话框。
//! - 性能页：各资源的当前数值，加上时间窗口内（固定 60 秒）的曲线数据，长表格式，数值是不带单位的原始数。
//! - 保存对话框和写文件都在后台线程里完成，界面线程只负责显示结果；取消保存不报错。
//! - 日志只记录行数、列数、结果与错误码，不记录导出的内容和文件路径。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::ComponentHandle;
use tml_core::{
    DetailColumn, ProcessKey, ProcessSnapshot, SystemError, ViewMode, ViewOptions, ViewRow, build_group_view,
    build_view,
};

use super::process_menu::failure;
use super::resources::Instance;
use super::{CONFIRM_EXPORT, Controller, STATUS_ERROR, State, basic_texts, mini, row_values};
use crate::details::{self, Shown};
use crate::format::History;
use crate::texts::{self, ExportError, ExportKind, PerfMetric};
use crate::{AppWindow, Columns, ProcessMenu, ResourceKind, file_dialog};

const STATUS_EXPORTED: i32 = 8;
const STATUS_ROW_COPIED: i32 = 9;

/// 导出的运行状态。
#[derive(Default)]
pub(super) struct ExportState {
    /// 保存对话框已打开（或正在写文件），不再接受新的导出。
    busy: Cell<bool>,
    /// 等待用户在敏感列提示里确认的导出。
    pending: RefCell<Option<(ExportKind, Vec<Vec<String>>)>>,
}

/// 保存结果。
enum Outcome {
    Saved(PathBuf),
    Cancelled,
    Failed(PathBuf, SystemError),
}

/// 基本列的标题（来自界面，见 process-page.slint 的 `Columns.title-*`）。
#[derive(Debug, Clone)]
pub struct Titles {
    pub name: String,
    pub pid: String,
    pub cpu: String,
    pub memory: String,
    pub disk: String,
}

/// 进程列表导出的表格：表头与逐行（带进程身份，复制所选行时按它找行）。
pub struct ProcessTable {
    pub header: Vec<String>,
    pub rows: Vec<(ProcessKey, Vec<String>)>,
}

impl ProcessTable {
    /// 表头加数据行，交给 CSV 编码。
    pub fn into_rows(self) -> Vec<Vec<String>> {
        std::iter::once(self.header).chain(self.rows.into_iter().map(|(_, cells)| cells)).collect()
    }
}

/// 导出用的视图：与界面同样的搜索、排序，但树形与分组视图展开全部节点（折叠只是界面上的收纳）。
pub fn export_view(
    snapshot: &ProcessSnapshot,
    apps: Option<&HashSet<u32>>,
    opts: &ViewOptions,
) -> Result<Vec<ViewRow>, ExportError> {
    let mut opts = opts.clone();
    match opts.mode {
        ViewMode::Flat => {}
        ViewMode::Tree => opts.collapsed.clear(),
        ViewMode::Group => opts.expanded = snapshot.processes.iter().map(|p| p.key).collect(),
    }
    match (opts.mode, apps) {
        (ViewMode::Group, Some(apps)) => Ok(build_group_view(snapshot, apps, &opts)),
        (ViewMode::Group, None) => Err(ExportError::GroupNotReady),
        _ => Ok(build_view(snapshot, &opts)),
    }
}

/// 把视图整理成表格。
///
/// 列：分组视图先有“分组”（应用/后台进程），树形与分组视图再有“层级”（0 为根，1 为根的子进程……），
/// 然后是界面上的 名称、PID、CPU、内存、磁盘 和已选的详细信息列。分组标题行不导出（信息在“分组”列里）。
pub fn process_table(
    snapshot: &ProcessSnapshot,
    view: &[ViewRow],
    mode: ViewMode,
    details: &[Shown],
    titles: &Titles,
) -> ProcessTable {
    let mut header = Vec::new();
    if mode == ViewMode::Group {
        header.push(texts::export_group_title().to_owned());
    }
    if mode != ViewMode::Flat {
        header.push(texts::export_depth_title().to_owned());
    }
    header.extend([&titles.name, &titles.pid, &titles.cpu, &titles.memory, &titles.disk].map(String::clone));
    header.extend(details.iter().map(|d| details::title(d.column).to_owned()));

    let mut rows = Vec::with_capacity(view.len());
    let mut group = None;
    for r in view {
        if let Some(h) = r.header {
            group = Some(h.group);
            continue;
        }
        let p = &snapshot.processes[r.index];
        let (cpu, memory, disk, gpu) = row_values(p, r);
        let mut cells = Vec::with_capacity(header.len());
        if mode == ViewMode::Group {
            cells.push(group.map_or("", texts::group_name).to_owned());
        }
        if mode != ViewMode::Flat {
            cells.push(r.depth.to_string());
        }
        cells.push(p.name.clone());
        cells.push(p.key.pid.to_string());
        cells.extend(basic_texts(cpu, memory, disk));
        cells.extend(details.iter().map(|d| details::cell(p, d.column, gpu)));
        rows.push((p.key, cells));
    }
    ProcessTable { header, rows }
}

/// 可能带有密码、令牌、个人信息的列：导出前要提示。
pub fn sensitive_columns(details: &[Shown]) -> Vec<DetailColumn> {
    details
        .iter()
        .map(|d| d.column)
        .filter(|c| matches!(c, DetailColumn::CommandLine | DetailColumn::Path | DetailColumn::User))
        .collect()
}

/// 数值的文字：最多三位小数，去掉多余的 0。
fn number(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

/// 距导出时刻的秒数：当前值为 0，曲线上的点为负数。
fn offset_text(age: f64) -> String {
    if age < 0.05 { "0".into() } else { format!("-{age:.1}") }
}

/// 现在的 FILETIME（UTC，100ns）。
fn filetime_now() -> u64 {
    let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (since_unix.as_nanos() / 100) as u64 + 116_444_736_000_000_000
}

/// 性能快照表格的构造器。
struct PerfRows {
    out: Vec<Vec<String>>,
    now: Instant,
    /// 导出时刻的 FILETIME，曲线上各点的时间由它减去点的“年龄”得到。
    wall: u64,
}

impl PerfRows {
    fn current(&mut self, inst: &Instance, metric: PerfMetric, value: Option<f64>) {
        self.out.push(vec![
            texts::perf_row_kind(false).into(),
            inst.title.clone(),
            inst.id.clone(),
            texts::perf_metric(metric).into(),
            tml_core::local_time_text(self.wall),
            offset_text(0.0),
            value.map(number).unwrap_or_default(),
            texts::perf_unit(metric).into(),
        ]);
    }

    fn series(&mut self, inst: &Instance, metric: PerfMetric, history: Option<&History>) {
        for (age, value) in history.map(|h| h.samples(self.now)).unwrap_or_default() {
            let at = self.wall.saturating_sub((age * 1e7) as u64);
            self.out.push(vec![
                texts::perf_row_kind(true).into(),
                inst.title.clone(),
                inst.id.clone(),
                texts::perf_metric(metric).into(),
                tml_core::local_time_text(at),
                offset_text(age),
                number(value),
                texts::perf_unit(metric).into(),
            ]);
        }
    }
}

/// 性能快照：每个资源实例先是当前数值，再是各指标在时间窗口内的曲线数据（旧的在前，采样中断的点不导出）。
/// 磁盘和网络的实例信息还没到（只有汇总卡片）时，当前值取整机汇总。
fn performance_table(s: &State, now: Instant, wall: u64) -> Vec<Vec<String>> {
    let mut t = PerfRows { out: vec![texts::perf_header().map(String::from).to_vec()], now, wall };
    let m = &s.system;
    for inst in s.resources.instances() {
        let history = Some(s.resources.history(&inst.id));
        match inst.kind {
            ResourceKind::Cpu => {
                t.current(inst, PerfMetric::CpuUsage, Some(f64::from(m.cpu_usage)));
                t.series(inst, PerfMetric::CpuUsage, history);
            }
            ResourceKind::Memory => {
                let percent = (m.memory_total > 0).then(|| m.memory_used as f64 / m.memory_total as f64 * 100.0);
                t.current(inst, PerfMetric::MemoryUsed, Some(m.memory_used as f64));
                t.current(inst, PerfMetric::MemoryTotal, Some(m.memory_total as f64));
                t.current(inst, PerfMetric::MemoryPercent, percent);
                t.series(inst, PerfMetric::MemoryUsed, history);
            }
            ResourceKind::Disk => {
                let disk = s.disks.iter().find(|d| super::resources::disk_id(d.number) == inst.id);
                match disk {
                    Some(d) => {
                        t.current(inst, PerfMetric::DiskActive, d.active.map(f64::from));
                        t.current(inst, PerfMetric::DiskResponse, d.response_ms);
                        t.current(inst, PerfMetric::DiskRead, d.read);
                        t.current(inst, PerfMetric::DiskWrite, d.write);
                    }
                    None => {
                        t.current(inst, PerfMetric::DiskActive, Some(f64::from(m.disk_active)));
                        t.current(inst, PerfMetric::DiskRead, Some(m.disk_read));
                        t.current(inst, PerfMetric::DiskWrite, Some(m.disk_write));
                    }
                }
                t.series(inst, PerfMetric::DiskActive, history);
            }
            ResourceKind::Network => {
                let adapter = s.networks.iter().find(|n| n.id == inst.id);
                let (recv, send) = match adapter {
                    Some(n) => (n.recv, n.send),
                    None => (Some(m.net_recv), Some(m.net_send)),
                };
                t.current(inst, PerfMetric::NetReceive, recv);
                t.current(inst, PerfMetric::NetSend, send);
                let (rx, tx) = s.resources.network_histories(&inst.id);
                t.series(inst, PerfMetric::NetReceive, rx);
                t.series(inst, PerfMetric::NetSend, tx);
            }
            ResourceKind::Gpu => {
                let gpu = s.gpus.iter().find(|g| g.id == inst.id);
                t.current(inst, PerfMetric::GpuUsage, gpu.and_then(|g| g.usage));
                t.current(inst, PerfMetric::GpuDedicated, gpu.and_then(|g| g.dedicated));
                t.current(inst, PerfMetric::GpuShared, gpu.and_then(|g| g.shared));
                t.series(inst, PerfMetric::GpuUsage, history);
                let memory = s.resources.gpu_memory_histories(&inst.id);
                t.series(inst, PerfMetric::GpuDedicated, memory.map(|h| &h[0]));
                t.series(inst, PerfMetric::GpuShared, memory.map(|h| &h[1]));
            }
        }
    }
    t.out
}

fn shape(rows: &[Vec<String>]) -> (usize, usize) {
    (rows.len().saturating_sub(1), rows.first().map_or(0, Vec::len))
}

impl Controller {
    pub(super) fn bind_export(self: &Rc<Self>, ui: &AppWindow) {
        let menu = ui.global::<ProcessMenu>();
        let c = Rc::downgrade(self);
        menu.on_copy_row(move || {
            if let Some(c) = c.upgrade() {
                c.copy_selected_row();
            }
        });
        let c = Rc::downgrade(self);
        menu.on_export_csv(move || {
            if let Some(c) = c.upgrade() {
                c.export_process_csv();
            }
        });
        let c = Rc::downgrade(self);
        ui.on_export_snapshot(move || {
            if let Some(c) = c.upgrade() {
                c.export_snapshot();
            }
        });
    }

    fn titles(ui: &AppWindow) -> Titles {
        let c = ui.global::<Columns>();
        Titles {
            name: c.get_title_name().to_string(),
            pid: c.get_title_pid().to_string(),
            cpu: c.get_title_cpu().to_string(),
            memory: c.get_title_memory().to_string(),
            disk: c.get_title_disk().to_string(),
        }
    }

    /// 复制所选行：制表符分隔的一行文字，列与导出的 CSV 一致（不含表头）。
    fn copy_selected_row(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let titles = Self::titles(&ui);
        let copied = {
            let s = self.state.borrow();
            s.selected.and_then(|key| {
                let table = process_table(&s.snapshot, &s.view, s.opts.mode, &s.details, &titles);
                let (_, cells) = table.rows.into_iter().find(|(k, _)| *k == key)?;
                let name = s.snapshot.processes.iter().find(|p| p.key == key)?.name.clone();
                Some((name, tml_core::csv::tab_line(&cells), cells.len()))
            })
        };
        let action = texts::copy_row_action();
        match copied {
            None => self.report(STATUS_ERROR, failure(action, texts::no_selected_row())),
            Some((name, line, columns)) => {
                if tml_core::set_clipboard_text(&line) {
                    self.log.borrow_mut().line(format!("复制所选行 成功 列={columns}"));
                    self.report(STATUS_ROW_COPIED, name);
                } else {
                    self.log.borrow_mut().line("复制所选行 失败 剪贴板被占用");
                    self.report(STATUS_ERROR, failure(action, "剪贴板被占用"));
                }
            }
        }
    }

    /// 生成要导出的进程列表（含表头）与其中的敏感列。
    fn process_export_rows(&self) -> Result<(Vec<Vec<String>>, Vec<DetailColumn>), ExportError> {
        let ui = self.ui.upgrade().ok_or(ExportError::NoData)?;
        let titles = Self::titles(&ui);
        let s = self.state.borrow();
        if s.snapshot.processes.is_empty() {
            return Err(ExportError::NoData);
        }
        let view = export_view(&s.snapshot, s.app_pids.as_ref(), &s.opts)?;
        let table = process_table(&s.snapshot, &view, s.opts.mode, &s.details, &titles);
        Ok((table.into_rows(), sensitive_columns(&s.details)))
    }

    fn performance_export_rows(&self) -> Result<Vec<Vec<String>>, ExportError> {
        let s = self.state.borrow();
        if !s.first_data {
            return Err(ExportError::NoData);
        }
        Ok(performance_table(&s, Instant::now(), filetime_now()))
    }

    fn export_failed(&self, e: ExportError) {
        self.log.borrow_mut().line(format!("导出 失败 原因={e:?}"));
        self.report(STATUS_ERROR, failure(texts::export_action(), texts::export_error(e)));
    }

    /// 进程页“导出为 CSV…”：有敏感列时先提示，确认后才弹保存对话框。
    fn export_process_csv(&self) {
        if self.export.busy.get() {
            return;
        }
        let (rows, sensitive) = match self.process_export_rows() {
            Ok(v) => v,
            Err(e) => return self.export_failed(e),
        };
        if sensitive.is_empty() {
            self.start_export(ExportKind::Process, rows);
            return;
        }
        let names = sensitive
            .iter()
            .map(|&c| details::title(c))
            .collect::<Vec<_>>()
            .join(texts::list_separator());
        self.log.borrow_mut().line(format!("导出 进程列表 含敏感列 个数={} 等待确认", sensitive.len()));
        *self.export.pending.borrow_mut() = Some((ExportKind::Process, rows));
        self.ask(CONFIRM_EXPORT, Vec::new(), names, 0, false);
    }

    /// 用户在敏感列提示里点了“继续导出”。
    pub(super) fn confirm_export(&self) {
        let pending = self.export.pending.borrow_mut().take();
        if let Some((kind, rows)) = pending {
            self.log.borrow_mut().line("导出 进程列表 敏感列已确认");
            self.start_export(kind, rows);
        }
    }

    /// 性能页“导出当前快照…”。
    fn export_snapshot(&self) {
        if self.export.busy.get() {
            return;
        }
        match self.performance_export_rows() {
            Ok(rows) => self.start_export(ExportKind::Performance, rows),
            Err(e) => self.export_failed(e),
        }
    }

    /// 在后台线程里弹出保存对话框（以主窗口为所有者，模态）并写文件。
    fn start_export(&self, kind: ExportKind, rows: Vec<Vec<String>>) {
        if self.export.busy.replace(true) {
            return;
        }
        let owner = self.with_active(mini::hwnd_of).flatten().map_or(0, |h| h.0 as isize);
        let name = format!("{}-{}.csv", texts::export_file_stem(kind), tml_core::local_timestamp());
        let shape = shape(&rows);
        self.log.borrow_mut().line(format!(
            "导出 {} 打开保存对话框 行={} 列={}",
            texts::export_file_stem(kind),
            shape.0,
            shape.1
        ));
        let spawned = std::thread::Builder::new().name("export-csv".into()).spawn(move || {
            let outcome = match file_dialog::pick_save_file(
                owner,
                texts::export_save_title(kind),
                &name,
                "csv",
                &texts::export_filters(),
            ) {
                None => Outcome::Cancelled,
                Some(path) => match tml_core::csv::write_file(&path, &rows) {
                    Ok(()) => Outcome::Saved(path),
                    Err(e) => Outcome::Failed(path, e),
                },
            };
            let _ = slint::invoke_from_event_loop(move || {
                crate::CONTROLLER.with(|c| {
                    if let Some(c) = c.borrow().as_ref() {
                        c.finish_export(kind, shape, outcome);
                    }
                })
            });
        });
        if let Err(e) = spawned {
            self.finish_export(kind, shape, Outcome::Failed(PathBuf::new(), SystemError::from(e)));
        }
    }

    /// 保存结果回到界面线程：状态栏给出结果；取消保存什么也不提示。
    fn finish_export(&self, kind: ExportKind, (data_rows, columns): (usize, usize), outcome: Outcome) {
        self.export.busy.set(false);
        let what = texts::export_file_stem(kind);
        let file_name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match outcome {
            Outcome::Saved(path) => {
                self.log.borrow_mut().line(format!("导出 {what} 成功 行={data_rows} 列={columns}"));
                self.report(STATUS_EXPORTED, file_name(&path));
            }
            Outcome::Cancelled => self.log.borrow_mut().line(format!("导出 {what} 已取消")),
            Outcome::Failed(path, e) => {
                let code = e.win32_code().map_or_else(|| "无".to_owned(), |c| c.to_string());
                self.log.borrow_mut().line(format!("导出 {what} 失败 错误码={code}"));
                let reason = texts::export_write_error(&file_name(&path), &e);
                self.report(STATUS_ERROR, failure(texts::export_action(), &reason));
            }
        }
    }

    /// 自动化验收入口：`--export-csv process=<路径>` 或 `performance=<路径>`。
    /// 窗口移到屏幕外，等采样与静态信息就绪后不弹对话框、不提示敏感列，直接写到给定路径，随后退出。
    pub fn queue_export(self: &Rc<Self>, spec: &str) {
        if let Some(ui) = self.ui.upgrade() {
            ui.window().set_position(slint::PhysicalPosition::new(-6000, -6000));
        }
        let spec = spec.to_owned();
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_secs(5), move || {
            if let Some(c) = weak.upgrade() {
                c.run_export_hook(&spec);
            }
            let _ = slint::quit_event_loop();
        });
    }

    fn run_export_hook(&self, spec: &str) {
        let (kind, path) = spec.split_once('=').unwrap_or((spec, ""));
        let (kind, rows) = match kind {
            "process" => (ExportKind::Process, self.process_export_rows().map(|(rows, _)| rows)),
            "performance" => (ExportKind::Performance, self.performance_export_rows()),
            other => {
                self.log.borrow_mut().line(format!("导出 未知的测试参数 {other}"));
                return;
            }
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => return self.export_failed(e),
        };
        let path = PathBuf::from(path);
        let outcome = match tml_core::csv::write_file(&path, &rows) {
            Ok(()) => Outcome::Saved(path),
            Err(e) => Outcome::Failed(path, e),
        };
        self.export.busy.set(true);
        self.finish_export(kind, shape(&rows), outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::resources::{self, ResourceSet, summary_instances};
    use std::sync::Arc;
    use tml_core::{Counters, Process, StaticInfo};

    fn proc(pid: u32, parent: u32, create: u64, name: &str, cpu: f32) -> Process {
        Process {
            key: ProcessKey { pid, create_time: create },
            parent_pid: parent,
            name: name.into(),
            cpu_usage: cpu,
            private_working_set: u64::from(pid) * 1024 * 1024,
            disk_rate: 0.0,
            gpu_usage: None,
            critical: false,
            counters: Counters::default(),
            info: None,
        }
    }

    fn titles() -> Titles {
        Titles {
            name: "名称".into(),
            pid: "PID".into(),
            cpu: "CPU".into(),
            memory: "内存".into(),
            disk: "磁盘 ≈".into(),
        }
    }

    fn snapshot() -> ProcessSnapshot {
        let mut app = proc(10, 1, 5, "app.exe", 3.0);
        app.info = Some(Arc::new(StaticInfo {
            command_line: Some("\"C:\\a b\\app.exe\" --x=\"1,2\"".into()),
            ..Default::default()
        }));
        ProcessSnapshot {
            processes: vec![
                proc(1, 0, 1, "root.exe", 1.0),
                app,
                proc(11, 10, 6, "child.exe", 2.0),
                proc(12, 11, 7, "grand.exe", 1.0),
                proc(20, 0, 2, "svc.exe", 0.5),
            ],
        }
    }

    fn shown(cols: &[DetailColumn]) -> Vec<Shown> {
        cols.iter().map(|&column| Shown { column, width: 100.0 }).collect()
    }

    fn opts(mode: ViewMode) -> ViewOptions {
        ViewOptions { mode, sort_key: tml_core::SortKey::Pid, descending: false, ..Default::default() }
    }

    #[test]
    fn flat_table_matches_the_list_columns_and_sort() {
        let snap = snapshot();
        let o = opts(ViewMode::Flat);
        let view = export_view(&snap, None, &o).unwrap();
        let d = shown(&[DetailColumn::CommandLine, DetailColumn::Threads]);
        let t = process_table(&snap, &view, ViewMode::Flat, &d, &titles());
        assert_eq!(t.header, ["名称", "PID", "CPU", "内存", "磁盘 ≈", "命令行", "线程"]);
        let pids: Vec<&str> = t.rows.iter().map(|(_, c)| c[1].as_str()).collect();
        assert_eq!(pids, ["1", "10", "11", "12", "20"]);
        // 单元格与界面格式一致，命令行原样（含逗号、引号）。
        let app = &t.rows[1].1;
        assert_eq!(app[..5], ["app.exe", "10", "3.0%", "10.0 MB", "0 MB/s"]);
        assert_eq!(app[5], "\"C:\\a b\\app.exe\" --x=\"1,2\"");
        assert_eq!(app[6], "0");
        // 经 CSV 编码后表头一行、数据行数一致。
        let rows = t.into_rows();
        assert_eq!(rows.len(), 6);
    }

    #[test]
    fn tree_table_has_depth_and_includes_collapsed_descendants() {
        let snap = snapshot();
        let mut o = opts(ViewMode::Tree);
        // 界面里 app.exe 折叠了，导出仍包含它的后代。
        o.collapsed.insert(snap.processes[1].key);
        let view = export_view(&snap, None, &o).unwrap();
        let t = process_table(&snap, &view, ViewMode::Tree, &[], &titles());
        assert_eq!(t.header[..3], ["层级", "名称", "PID"]);
        let got: Vec<(String, String)> = t.rows.iter().map(|(_, c)| (c[0].clone(), c[1].clone())).collect();
        let expect: Vec<(String, String)> = [("0", "root.exe"), ("1", "app.exe"), ("2", "child.exe"), ("3", "grand.exe"), ("0", "svc.exe")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(got, expect);
        // 界面上折叠的列表只有 app 本身，说明导出确实展开了。
        let ui_view = build_view(&snap, &o);
        assert_eq!(ui_view.len(), 3);
    }

    #[test]
    fn group_table_has_group_and_depth_columns_and_no_header_rows() {
        let snap = snapshot();
        let apps: HashSet<u32> = [10].into_iter().collect();
        let o = opts(ViewMode::Group);
        let view = export_view(&snap, Some(&apps), &o).unwrap();
        let t = process_table(&snap, &view, ViewMode::Group, &[], &titles());
        assert_eq!(t.header[..4], ["分组", "层级", "名称", "PID"]);
        let got: Vec<(&str, &str, &str)> =
            t.rows.iter().map(|(_, c)| (c[0].as_str(), c[1].as_str(), c[2].as_str())).collect();
        assert_eq!(
            got,
            [
                ("应用", "0", "app.exe"),
                ("应用", "1", "child.exe"),
                ("应用", "2", "grand.exe"),
                ("后台进程", "0", "root.exe"),
                ("后台进程", "0", "svc.exe"),
            ]
        );
        // 应用行是合计：3.0 + 2.0 + 1.0。
        assert_eq!(t.rows[0].1[4], "6.0%");
        assert_eq!(t.rows[1].1[4], "2.0%");
    }

    #[test]
    fn group_view_without_window_info_is_not_ready() {
        assert_eq!(export_view(&snapshot(), None, &opts(ViewMode::Group)).unwrap_err(), ExportError::GroupNotReady);
    }

    #[test]
    fn filter_applies_to_the_export() {
        let snap = snapshot();
        let mut o = opts(ViewMode::Flat);
        o.filter = "exe".into();
        assert_eq!(export_view(&snap, None, &o).unwrap().len(), 5);
        o.filter = "svc".into();
        assert_eq!(export_view(&snap, None, &o).unwrap().len(), 1);
    }

    #[test]
    fn sensitive_columns_are_command_line_path_and_user_only() {
        let all = shown(&DetailColumn::ALL);
        assert_eq!(
            sensitive_columns(&all),
            [DetailColumn::User, DetailColumn::Path, DetailColumn::CommandLine]
        );
        assert!(sensitive_columns(&shown(&[DetailColumn::Threads, DetailColumn::Gpu])).is_empty());
        assert!(sensitive_columns(&[]).is_empty());
    }

    #[test]
    fn numbers_drop_trailing_zeros() {
        assert_eq!(number(0.0), "0");
        assert_eq!(number(100.0), "100");
        assert_eq!(number(12.3000001), "12.3");
        assert_eq!(number(0.0004), "0");
        assert_eq!(number(-0.0001), "0");
        assert_eq!(number(8_589_934_592.0), "8589934592");
        assert_eq!(number(1.2346), "1.235");
        assert_eq!(offset_text(0.0), "0");
        assert_eq!(offset_text(12.34), "-12.3");
    }

    fn perf_state(now: Instant) -> State {
        let mut s = State { resources: ResourceSet::new(summary_instances()), ..Default::default() };
        s.system = tml_core::SystemMetrics {
            cpu_usage: 12.5,
            memory_used: 4 * 1024 * 1024 * 1024,
            memory_total: 16 * 1024 * 1024 * 1024,
            disk_active: 7.0,
            disk_read: 1024.0,
            disk_write: 2048.0,
            net_recv: 100.0,
            net_send: 50.0,
        };
        for (age, v) in [(3, 10.0), (2, 20.0), (1, 30.0)] {
            s.resources.push(resources::CPU, now - Duration::from_secs(age), Some(v));
        }
        s.resources.push(resources::CPU, now - Duration::from_millis(500), None);
        s.resources.push(resources::MEMORY, now, Some(4.0 * 1024.0 * 1024.0 * 1024.0));
        s
    }

    #[test]
    fn performance_table_has_current_values_and_window_series() {
        let now = Instant::now();
        let s = perf_state(now);
        let rows = performance_table(&s, now, 133_000_000_000_000_000);
        assert_eq!(rows[0], ["类型", "资源", "实例", "指标", "时间", "距今（秒）", "数值", "单位"]);
        assert!(rows.iter().all(|r| r.len() == 8));
        let find = |kind: &str, id: &str, metric: &str| -> Vec<&Vec<String>> {
            rows.iter().filter(|r| r[0] == kind && r[2] == id && r[3] == metric).collect()
        };
        let cpu_now = find("当前", "cpu", "利用率");
        assert_eq!(cpu_now.len(), 1);
        assert_eq!((cpu_now[0][5].as_str(), cpu_now[0][6].as_str(), cpu_now[0][7].as_str()), ("0", "12.5", "%"));
        // 曲线：三个有效点，旧的在前；中断点不导出；每行都有本地时间。
        let cpu_series = find("曲线", "cpu", "利用率");
        let values: Vec<(&str, &str)> = cpu_series.iter().map(|r| (r[5].as_str(), r[6].as_str())).collect();
        assert_eq!(values, [("-3.0", "10"), ("-2.0", "20"), ("-1.0", "30")]);
        assert!(cpu_series.iter().all(|r| r[4].len() == 19), "{:?}", cpu_series[0]);
        // 内存：已用、总量、使用率。
        assert_eq!(find("当前", "memory", "已用")[0][6], "4294967296");
        assert_eq!(find("当前", "memory", "总量")[0][6], "17179869184");
        assert_eq!(find("当前", "memory", "使用率")[0][6], "25");
        assert_eq!(find("曲线", "memory", "已用").len(), 1);
        // 只有汇总卡片时，磁盘与网络的当前值取整机汇总。
        assert_eq!(find("当前", "disk", "活动时间")[0][6], "7");
        assert_eq!(find("当前", "disk", "写入速度")[0][6], "2048");
        assert_eq!(find("当前", "network", "接收")[0][6], "100");
        assert_eq!(find("当前", "network", "发送")[0][7], "字节/秒");
    }

    #[test]
    fn unavailable_current_values_keep_the_row_with_an_empty_value() {
        let now = Instant::now();
        let mut s = perf_state(now);
        let gpu = tml_core::Gpu { id: "gpu:0".into(), name: "G".into(), ..Default::default() };
        s.resources.sync_gpus(&[gpu.clone()]);
        s.gpus = vec![gpu];
        let rows = performance_table(&s, now, 133_000_000_000_000_000);
        let usage: Vec<_> = rows.iter().filter(|r| r[2] == "gpu:0" && r[0] == "当前").collect();
        assert_eq!(usage.len(), 3);
        assert!(usage.iter().all(|r| r[6].is_empty()));
    }

    #[test]
    fn shape_counts_data_rows_and_columns() {
        assert_eq!(shape(&[]), (0, 0));
        assert_eq!(shape(&[vec!["a".into(), "b".into()]]), (0, 2));
        assert_eq!(shape(&[vec!["a".into()], vec!["1".into()], vec!["2".into()]]), (2, 1));
    }

    #[test]
    fn filetime_now_is_after_2020() {
        // 2020-01-01 的 FILETIME 约为 132_223_104_000_000_000。
        assert!(filetime_now() > 132_223_104_000_000_000);
    }
}

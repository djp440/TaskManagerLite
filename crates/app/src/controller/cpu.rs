//! CPU 详情页的数据整形：每个逻辑处理器的历史曲线、统计项与“基本信息”键值区的文本。
//!
//! 文本格式化是纯函数，可单测；界面模型的写入集中在 `CpuState::refresh`。

mod grid;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tml_core::{CpuInfo, CpuSample};

use crate::format::History;
use crate::history::Window;
use crate::{AppWindow, CoreGraph, CpuData, GridLayout, StatItem};

/// 单张小图在布局尚未给出网格区尺寸前的默认绘制尺寸。
const DEFAULT_CELL: (f64, f64) = (100.0, 50.0);

#[derive(Default)]
pub struct CpuState {
    /// 按逻辑处理器下标保存的历史；暂停/最小化造成的断口与其他曲线一样用断点表示。
    histories: Vec<History>,
    latest: CpuSample,
    /// 截图模式：逻辑处理器网格用演示曲线代替真实历史，便于目视核对曲线形态。
    demo: bool,
    /// 截图模式下假装有这么多个逻辑处理器（0 表示用真实数量），用来核对核数很多时的网格。
    demo_cores: usize,
    /// 最近一次生成曲线路径时用的小图绘制尺寸，供截图模式核对路径是否与网格最新尺寸一致。
    last_cell: Cell<Option<(f64, f64)>>,
    cores: Rc<VecModel<CoreGraph>>,
    stats: Rc<VecModel<StatItem>>,
    info: Rc<VecModel<StatItem>>,
}

impl CpuState {
    /// 把模型挂到界面的全局对象上，之后只做原地更新。
    pub fn attach(&self, ui: &AppWindow) {
        let data = ui.global::<CpuData>();
        data.set_cores(ModelRc::from(self.cores.clone()));
        data.set_stats(ModelRc::from(self.stats.clone()));
        data.set_info(ModelRc::from(self.info.clone()));
        data.on_grid_layout(|count, w, h| {
            let p = grid::grid_plan(count.max(0) as usize, w, h);
            GridLayout {
                cols: p.cols as i32,
                rows: p.rows as i32,
                gap: grid::GAP,
                cell_w: p.cell_w,
                cell_h: p.cell_h,
                content_h: p.content_h,
                scroll: p.scroll,
            }
        });
    }

    /// 记录一次采样。`with_history`：首次采样没有基线（占用为 0），不写入曲线。
    pub fn record(&mut self, now: Instant, sample: CpuSample, with_history: bool) {
        let n = sample.per_core.len();
        self.histories.resize_with(n, History::default);
        if with_history {
            for (h, &u) in self.histories.iter_mut().zip(&sample.per_core) {
                h.push(now, Some(f64::from(u)));
            }
        }
        // 保留已读到的静态信息：个别采样可能没带。
        let info = sample.info.clone().or_else(|| self.latest.info.clone());
        self.latest = CpuSample { info, ..sample };
    }

    /// 测试钩子：给每个逻辑处理器的曲线灌入合成历史。
    pub fn prefill(&mut self, now: Instant, secs: u64) {
        for h in &mut self.histories {
            h.prefill(now, secs);
        }
    }

    /// 全部逻辑处理器曲线历史占用的堆内存估算（字节）。
    pub fn history_bytes(&self) -> usize {
        self.histories.iter().map(History::heap_bytes).sum()
    }

    /// 采样中断：所有逻辑处理器的曲线在此断开。
    pub fn push_gap(&mut self, now: Instant) {
        for h in &mut self.histories {
            h.push(now, None);
        }
    }

    /// 型号名称，显示在详情页标题右侧。
    pub fn model_name(&self) -> String {
        self.latest.info.as_ref().map(|i| i.name.clone()).unwrap_or_default()
    }

    /// 资源卡片的摘要：利用率与当前频率。
    pub fn card_value(&self, usage: &str) -> String {
        match self.latest.current_mhz {
            Some(mhz) => format!("{usage} {}", ghz(mhz)),
            None => usage.to_string(),
        }
    }

    /// 详情页的统计项（大号数值），交给通用 StatsPanel 显示。
    pub fn stats(&self, usage: &str, processes: usize) -> Vec<(&'static str, String)> {
        let s = &self.latest;
        vec![
            ("利用率", usage.to_string()),
            ("速度", s.current_mhz.map_or_else(|| "—".into(), ghz)),
            ("进程", processes.to_string()),
            ("线程", s.threads.to_string()),
            ("句柄", s.handles.to_string()),
            ("正常运行时间", uptime(s.uptime_secs)),
        ]
    }

    /// 每个逻辑处理器的小图：路径按小图的实际绘制尺寸 `cell` 生成，纵轴 0–100%，横轴为 `window`（与整体曲线同步切换）。
    fn core_graphs(&self, now: Instant, cell: (f64, f64), window: Window) -> Vec<CoreGraph> {
        let demo;
        let histories = if self.demo {
            let n = if self.demo_cores > 0 { self.demo_cores } else { self.latest.per_core.len() };
            demo = demo_histories(n, now);
            &demo
        } else {
            &self.histories
        };
        histories
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let u = self.latest.per_core.get(i).copied().unwrap_or(50.0);
                let (line, area) = h.paths(now, 100.0, cell, window);
                CoreGraph {
                    label: i.to_string().into(),
                    value: crate::format::percent(u).into(),
                    line: line.into(),
                    area: area.into(),
                }
            })
            .collect()
    }

    /// 截图用：逻辑处理器网格改用演示曲线。
    pub fn use_demo_history(&mut self, cores: usize) {
        self.demo = true;
        self.demo_cores = cores;
    }

    /// 把最新的统计项、基本信息与小图写入界面，只整形当前看得见的部分（见 `Scope::of`）。
    /// 小图尺寸由网格布局经 `CpuData` 报告。切换选中项由 `select_resource` 触发刷新，
    /// 切到“逻辑处理器”视图时网格创建，其 init 会上报尺寸并触发刷新，所以都不必等下一次采样。
    pub fn refresh(&self, ui: &AppWindow, now: Instant, processes: usize, usage: &str, selected: bool, window: Window) {
        let data = ui.global::<CpuData>();
        let scope = Scope::of(selected, data.get_per_core());
        if scope.stats {
            sync(&self.stats, items(self.stats(usage, processes)));
            let info = self.latest.info.as_deref().map(info_rows).unwrap_or_default();
            sync(&self.info, items(info));
        }
        if scope.cores {
            let cell = self.expected_cell(&data);
            self.last_cell.set(Some(cell));
            sync(&self.cores, self.core_graphs(now, cell, window));
        }
    }
}

impl CpuState {
    /// 网格里小图的数量：演示模式用假装的核数，否则是真实的逻辑处理器数。
    fn core_count(&self) -> usize {
        if self.demo && self.demo_cores > 0 { self.demo_cores } else { self.latest.per_core.len() }
    }

    /// 按界面当前上报的网格区尺寸与小图数量算出的小图绘制尺寸（与 Slint 里排布小图用同一个 `grid_plan`）。
    fn expected_cell(&self, data: &CpuData<'_>) -> (f64, f64) {
        cell_size(self.core_count(), f64::from(data.get_grid_width()), f64::from(data.get_grid_height()))
    }

    /// 截图模式的核对：逻辑处理器网格正在显示时，最近一次生成的曲线路径尺寸必须等于小图按最新网格区尺寸
    /// 得出的绘制尺寸，且每个核都有一张小图。不一致说明路径是按过期尺寸生成的（会被裁剪或只占一角）。
    pub fn check_paths_current(&self, ui: &AppWindow, selected: bool) -> Result<(), String> {
        let data = ui.global::<CpuData>();
        if !Scope::of(selected, data.get_per_core()).cores {
            return Ok(());
        }
        let expected = self.expected_cell(&data);
        let used = self.last_cell.get().ok_or("逻辑处理器网格显示时尚未生成过曲线路径")?;
        if (used.0 - expected.0).abs() > 0.01 || (used.1 - expected.1).abs() > 0.01 {
            return Err(format!("曲线路径按 {used:?} 生成，但小图的实际绘制尺寸是 {expected:?}"));
        }
        let rows = self.cores.row_count();
        if rows != self.core_count() {
            return Err(format!("小图数量 {rows} 与逻辑处理器数 {} 不一致", self.core_count()));
        }
        Ok(())
    }
}

/// 每次刷新需要整形哪些部分：没选中 CPU 时都不算；选中后统计与信息总要算，
/// 每个核的 line/area 字符串开销最大，只在“逻辑处理器”视图下才算。
#[derive(Debug, PartialEq, Eq)]
struct Scope {
    stats: bool,
    cores: bool,
}

impl Scope {
    fn of(selected: bool, per_core_view: bool) -> Self {
        Scope { stats: selected, cores: selected && per_core_view }
    }
}

fn items(rows: Vec<(&'static str, String)>) -> Vec<StatItem> {
    rows.into_iter().map(|(label, value)| StatItem { label: SharedString::from(label), value: value.into() }).collect()
}

/// 演示曲线（60 秒窗口、每秒一点，以 `now` 为终点）：核按序号循环取 恒 100%、0→100% 斜坡、方波、恒 50%、正弦。
fn demo_histories(n: usize, now: Instant) -> Vec<History> {
    (0..n.max(1))
        .map(|core| {
            let mut h = History::default();
            for sec in 0..=60u64 {
                let t = now - std::time::Duration::from_secs(60 - sec);
                let x = sec as f64 / 60.0;
                let v = match core % 5 {
                    0 => 100.0,
                    1 => x * 100.0,
                    2 => {
                        if (sec / 10) % 2 == 0 { 100.0 } else { 0.0 }
                    }
                    3 => 50.0,
                    _ => 50.0 + 50.0 * (x * 12.0 + core as f64).sin(),
                };
                h.push(t, Some(v));
            }
            h
        })
        .collect()
}

/// 小图的绘制尺寸：由小图数量与网格区尺寸经 `grid_plan` 得出；布局尚未上报（≤1px）时用默认值。
fn cell_size(count: usize, area_w: f64, area_h: f64) -> (f64, f64) {
    if area_w > 1.0 && area_h > 1.0 {
        let (w, h) = grid::grid_plan(count, area_w as f32, area_h as f32).inner_size();
        (f64::from(w), f64::from(h))
    } else {
        DEFAULT_CELL
    }
}

/// 把新数据原地写入模型，避免整体替换造成界面重建。
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old = model.row_count();
    for (i, row) in rows.iter().enumerate() {
        if i < old {
            if model.row_data(i).as_ref() != Some(row) {
                model.set_row_data(i, row.clone());
            }
        } else {
            model.push(row.clone());
        }
    }
    for i in (rows.len()..old).rev() {
        model.remove(i);
    }
}

/// “基本信息”键值区，按列从上到下排列。
pub fn info_rows(i: &CpuInfo) -> Vec<(&'static str, String)> {
    vec![
        ("基准速度", if i.base_mhz > 0 { ghz(f64::from(i.base_mhz)) } else { "—".into() }),
        ("插槽", i.sockets.to_string()),
        ("内核", i.cores.to_string()),
        ("逻辑处理器", i.logical.to_string()),
        ("虚拟化", if i.virtualization { "已启用" } else { "已禁用" }.into()),
        ("L1 缓存", cache_size(i.l1_bytes)),
        ("L2 缓存", cache_size(i.l2_bytes)),
        ("L3 缓存", cache_size(i.l3_bytes)),
    ]
}

/// 频率，2 位小数的 GHz，与原生任务管理器一致。
pub fn ghz(mhz: f64) -> String {
    format!("{:.2} GHz", mhz / 1000.0)
}

/// 缓存大小：不足 1 MB 用 KB，其余用 MB。
pub fn cache_size(bytes: u64) -> String {
    if bytes == 0 {
        "—".into()
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// 正常运行时间，`天:时:分:秒`，与原生任务管理器一致。
pub fn uptime(secs: u64) -> String {
    format!("{}:{:02}:{:02}:{:02}", secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn sample(per_core: &[f32]) -> CpuSample {
        CpuSample { per_core: per_core.to_vec(), ..Default::default() }
    }

    #[test]
    fn formats_like_native_task_manager() {
        assert_eq!(ghz(3893.0), "3.89 GHz");
        assert_eq!(ghz(5523.4), "5.52 GHz");
        assert_eq!(cache_size(480 * 1024), "480 KB");
        assert_eq!(cache_size(6 * 1024 * 1024), "6.0 MB");
        assert_eq!(cache_size(32 * 1024 * 1024), "32.0 MB");
        assert_eq!(cache_size(0), "—");
        assert_eq!(uptime(0), "0:00:00:00");
        assert_eq!(uptime(86_400 + 3 * 3600 + 4 * 60 + 5), "1:03:04:05");
    }

    #[test]
    fn info_rows_cover_topology_and_caches() {
        let info = CpuInfo {
            name: "X".into(),
            base_mhz: 3900,
            sockets: 1,
            cores: 6,
            logical: 12,
            l1_bytes: 480 * 1024,
            l2_bytes: 6 << 20,
            l3_bytes: 32 << 20,
            virtualization: true,
        };
        let rows = info_rows(&info);
        let get = |k: &str| rows.iter().find(|r| r.0 == k).unwrap().1.clone();
        assert_eq!(get("基准速度"), "3.90 GHz");
        assert_eq!(get("内核"), "6");
        assert_eq!(get("逻辑处理器"), "12");
        assert_eq!(get("虚拟化"), "已启用");
        assert_eq!(get("L2 缓存"), "6.0 MB");
        assert_eq!(get("L3 缓存"), "32.0 MB");
    }

    #[test]
    fn stats_and_card_value_follow_frequency_availability() {
        let mut cpu = CpuState::default();
        let now = Instant::now();
        cpu.record(now, CpuSample { threads: 4000, handles: 90_000, uptime_secs: 61, ..sample(&[1.0, 2.0]) }, true);
        let values = |cpu: &CpuState| cpu.stats("5%", 300).into_iter().map(|r| r.1).collect::<Vec<_>>();
        assert_eq!(values(&cpu), ["5%", "—", "300", "4000", "90000", "0:00:01:01"]);
        assert_eq!(cpu.card_value("5%"), "5%");
        cpu.record(now, CpuSample { current_mhz: Some(5200.0), ..sample(&[1.0, 2.0]) }, true);
        assert_eq!(values(&cpu)[1], "5.20 GHz");
        assert_eq!(cpu.card_value("5%"), "5% 5.20 GHz");
    }

    #[test]
    fn histories_are_per_core_and_first_sample_is_not_drawn() {
        let mut cpu = CpuState::default();
        let t0 = Instant::now();
        cpu.record(t0, sample(&[0.0, 0.0, 0.0]), false);
        assert_eq!(cpu.histories.len(), 3);
        assert!(cpu.histories.iter().all(|h| h.max_in(t0, Window::Sec60) == 0.0));
        cpu.record(t0 + Duration::from_secs(1), sample(&[10.0, 90.0, 40.0]), true);
        let maxes: Vec<f64> = cpu.histories.iter().map(|h| h.max_in(t0 + Duration::from_secs(1), Window::Sec60)).collect();
        assert_eq!(maxes, [10.0, 90.0, 40.0]);
    }

    /// 取路径里所有坐标对 (x, y)。
    fn coords(path: &str) -> Vec<(f64, f64)> {
        let nums: Vec<f64> = path.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        nums.chunks(2).map(|c| (c[0], c[1])).collect()
    }

    #[test]
    fn core_graph_paths_cover_the_whole_cell() {
        // 60 秒窗口内从 0% 线性升到 100%：曲线必须横跨整个格子宽度，纵向从底边到顶边。
        let mut cpu = CpuState::default();
        let now = Instant::now();
        cpu.record(now - Duration::from_secs(60), sample(&[0.0, 100.0]), true);
        for sec in 1..=60u64 {
            let v = sec as f32 / 60.0 * 100.0;
            cpu.record(now - Duration::from_secs(60 - sec), sample(&[v, 100.0]), true);
        }
        for cell in [(176.0, 96.0), (64.0, 40.0), (300.0, 180.0)] {
            let graphs = cpu.core_graphs(now, cell, Window::Sec60);
            assert_eq!(graphs.len(), 2);
            let ramp = coords(&graphs[0].line);
            let (min_x, max_x) = ramp.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
            let (min_y, max_y) = ramp.iter().fold((f64::MAX, f64::MIN), |a, p| (a.1.min(p.1), a.1.max(p.1)));
            assert!(min_x.abs() < 0.2 && (max_x - cell.0).abs() < 0.2, "横向应铺满 {cell:?}：{min_x}..{max_x}");
            assert!((max_y - cell.1).abs() < 0.2 && min_y.abs() < 0.2, "纵向应覆盖 0–100% {cell:?}：{min_y}..{max_y}");
            // 恒为 100% 的核：整条线贴着格子顶边。
            assert!(coords(&graphs[1].line).iter().all(|p| p.1.abs() < 0.2));
            // 填充区域闭合到格子底边。
            assert!(coords(&graphs[0].area).iter().any(|p| (p.1 - cell.1).abs() < 0.2));
        }
    }

    #[test]
    fn refresh_only_shapes_what_is_visible() {
        // 没选中 CPU：什么都不算；选中但在整体视图：只算统计与信息；选中且在逻辑处理器视图：连小图一起算。
        assert_eq!(Scope::of(false, false), Scope { stats: false, cores: false });
        assert_eq!(Scope::of(false, true), Scope { stats: false, cores: false });
        assert_eq!(Scope::of(true, false), Scope { stats: true, cores: false });
        assert_eq!(Scope::of(true, true), Scope { stats: true, cores: true });
    }

    #[test]
    fn cell_size_falls_back_until_layout_reports() {
        assert_eq!(cell_size(12, 0.0, 0.0), DEFAULT_CELL);
        assert_eq!(cell_size(12, 724.0, 1.0), DEFAULT_CELL);
    }

    #[test]
    fn cell_size_follows_the_grid_plan_for_any_count_and_area() {
        // 回归：小图尺寸由“数量 + 网格区尺寸”决定，数量从 0 变到 12、网格区变化时都要跟着变，
        // 路径尺寸才不会停留在“只有一个大格子”或上一次布局的尺寸。
        let area_of = |count| cell_size(count, 724.0, 241.0);
        assert_ne!(area_of(1), area_of(12), "核数变了小图尺寸必然不同");
        for (count, w, h) in [(12, 724.0, 241.0), (12, 464.0, 80.0), (64, 724.0, 241.0), (64, 464.0, 80.0), (128, 1100.0, 600.0)] {
            let plan = grid::grid_plan(count, w as f32, h as f32);
            let (cw, ch) = cell_size(count, w, h);
            assert!((cw - f64::from(plan.cell_w) + 2.0).abs() < 1e-3 && (ch - f64::from(plan.cell_h) + 2.0).abs() < 1e-3);
            // 绘制尺寸一定落在小图内部。
            assert!(cw <= f64::from(plan.cell_w) && ch <= f64::from(plan.cell_h));
        }
        // 12 核的格子在默认窗口约 4x3：绘制尺寸约 173x74，而不是首次创建时那个铺满整个网格区的 722x239。
        let (w, h) = cell_size(12, 724.0, 241.0);
        assert!(w < 200.0 && h < 80.0, "{w}x{h}");
    }

    #[test]
    fn gap_breaks_every_core_curve() {
        let mut cpu = CpuState::default();
        let t0 = Instant::now();
        cpu.record(t0, sample(&[10.0, 20.0]), true);
        cpu.push_gap(t0 + Duration::from_secs(1));
        cpu.record(t0 + Duration::from_secs(30), sample(&[30.0, 40.0]), true);
        for h in &cpu.histories {
            let (line, _) = h.paths(t0 + Duration::from_secs(30), 100.0, (100.0, 50.0), Window::Sec60);
            assert_eq!(line.matches('M').count(), 2, "{line}");
        }
    }

    #[test]
    fn core_graphs_follow_the_history_window() {
        // 每秒一点跑 5 分钟，前半段 0%、后半段 100%：5 分钟窗口里横跨整个格子，60 秒窗口里只有 100% 的一条线。
        let mut cpu = CpuState::default();
        let t0 = Instant::now();
        for sec in 0..=300u64 {
            let v = if sec < 150 { 0.0 } else { 100.0 };
            cpu.record(t0 + Duration::from_secs(sec), sample(&[v]), true);
        }
        let now = t0 + Duration::from_secs(300);
        let cell = (300.0, 100.0);
        let long = coords(&cpu.core_graphs(now, cell, Window::Min5)[0].line);
        let short = coords(&cpu.core_graphs(now, cell, Window::Sec60)[0].line);
        assert!(long.iter().any(|p| p.1 > 99.0) && long.iter().any(|p| p.1 < 1.0), "5 分钟窗口应同时有 0% 与 100%");
        assert!(long.first().unwrap().0 < 3.0 && (long.last().unwrap().0 - cell.0).abs() < 0.2);
        assert!(short.iter().all(|p| p.1 < 1.0), "60 秒窗口里只有 100%");
    }

    #[test]
    fn static_info_survives_samples_without_it() {
        let mut cpu = CpuState::default();
        let now = Instant::now();
        let info = std::sync::Arc::new(CpuInfo { name: "Test CPU".into(), ..Default::default() });
        cpu.record(now, CpuSample { info: Some(info), ..sample(&[1.0]) }, true);
        cpu.record(now, sample(&[2.0]), true);
        assert_eq!(cpu.model_name(), "Test CPU");
    }
}

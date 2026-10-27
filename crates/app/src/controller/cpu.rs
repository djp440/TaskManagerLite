//! CPU 详情页的数据整形：每个逻辑处理器的历史曲线、统计项与“基本信息”键值区的文本。
//!
//! 文本格式化是纯函数，可单测；界面模型的写入集中在 `CpuState::refresh`。

use std::rc::Rc;
use std::time::Instant;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tml_core::{CpuInfo, CpuSample};

use crate::format::History;
use crate::{AppWindow, CoreGraph, CpuData, InfoRow};

/// 单张小图在布局尚未给出尺寸前的默认绘制尺寸。
const DEFAULT_CELL: (f64, f64) = (100.0, 50.0);

#[derive(Default)]
pub struct CpuState {
    /// 按逻辑处理器下标保存的历史；暂停/最小化造成的断口与其他曲线一样用断点表示。
    histories: Vec<History>,
    latest: CpuSample,
    /// 截图模式下替换真实历史的演示曲线，便于目视核对曲线形态。
    demo: Option<Vec<History>>,
    cores: Rc<VecModel<CoreGraph>>,
    info: Rc<VecModel<InfoRow>>,
}

impl CpuState {
    /// 把模型挂到界面的全局对象上，之后只做原地更新。
    pub fn attach(&self, ui: &AppWindow) {
        let data = ui.global::<CpuData>();
        data.set_cores(ModelRc::from(self.cores.clone()));
        data.set_info(ModelRc::from(self.info.clone()));
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

    /// 详情页的统计项，顺序与 `resources::summary_instances` 中 CPU 的标签一致。
    pub fn stats(&self, usage: &str, processes: usize) -> Vec<String> {
        let s = &self.latest;
        vec![
            usage.to_string(),
            s.current_mhz.map_or_else(|| "—".into(), ghz),
            processes.to_string(),
            s.threads.to_string(),
            s.handles.to_string(),
            uptime(s.uptime_secs),
        ]
    }

    /// 每个逻辑处理器的小图：路径按小图的实际绘制尺寸 `cell` 生成，纵轴 0–100%，横轴为 60 秒窗口。
    fn core_graphs(&self, now: Instant, cell: (f64, f64)) -> Vec<CoreGraph> {
        let histories = self.demo.as_ref().unwrap_or(&self.histories);
        histories
            .iter()
            .zip(&self.latest.per_core)
            .enumerate()
            .map(|(i, (h, &u))| {
                let (line, area) = h.paths(now, 100.0, cell);
                CoreGraph {
                    label: i.to_string().into(),
                    value: crate::format::percent(u).into(),
                    line: line.into(),
                    area: area.into(),
                }
            })
            .collect()
    }

    /// 截图用：把每个核的历史换成形态各异的演示曲线（满载、斜坡、方波、半载、正弦）。
    pub fn use_demo_history(&mut self, now: Instant) {
        let n = self.histories.len().max(1);
        let demo = (0..n)
            .map(|core| {
                let mut h = History::default();
                for sec in 0..=60u64 {
                    let t = now - std::time::Duration::from_secs(60 - sec);
                    let x = sec as f64 / 60.0;
                    let v = match core % 5 {
                        0 => 100.0,
                        1 => x * 100.0,
                        2 => if (sec / 10) % 2 == 0 { 100.0 } else { 0.0 },
                        3 => 50.0,
                        _ => 50.0 + 50.0 * (x * 12.0 + core as f64).sin(),
                    };
                    h.push(t, Some(v));
                }
                h
            })
            .collect();
        self.demo = Some(demo);
    }

    /// 把最新的小图与基本信息写入界面。小图尺寸由网格布局经 `CpuData` 报告。
    pub fn refresh(&self, ui: &AppWindow, now: Instant) {
        let data = ui.global::<CpuData>();
        let cell = cell_size(f64::from(data.get_cell_width()), f64::from(data.get_cell_height()));
        sync(&self.cores, self.core_graphs(now, cell));
        let info: Vec<InfoRow> = self
            .latest
            .info
            .as_deref()
            .map(info_rows)
            .unwrap_or_default()
            .into_iter()
            .map(|(label, value)| InfoRow { label: SharedString::from(label), value: value.into() })
            .collect();
        sync(&self.info, info);
    }
}

/// 小图的绘制尺寸：布局尚未上报（≤1px）时用默认值。
fn cell_size(w: f64, h: f64) -> (f64, f64) {
    if w > 1.0 && h > 1.0 { (w, h) } else { DEFAULT_CELL }
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
        assert_eq!(cpu.stats("5%", 300), ["5%", "—", "300", "4000", "90000", "0:00:01:01"]);
        assert_eq!(cpu.card_value("5%"), "5%");
        cpu.record(now, CpuSample { current_mhz: Some(5200.0), ..sample(&[1.0, 2.0]) }, true);
        assert_eq!(cpu.stats("5%", 300)[1], "5.20 GHz");
        assert_eq!(cpu.card_value("5%"), "5% 5.20 GHz");
    }

    #[test]
    fn histories_are_per_core_and_first_sample_is_not_drawn() {
        let mut cpu = CpuState::default();
        let t0 = Instant::now();
        cpu.record(t0, sample(&[0.0, 0.0, 0.0]), false);
        assert_eq!(cpu.histories.len(), 3);
        assert!(cpu.histories.iter().all(|h| h.max() == 0.0));
        cpu.record(t0 + Duration::from_secs(1), sample(&[10.0, 90.0, 40.0]), true);
        let maxes: Vec<f64> = cpu.histories.iter().map(|h| h.max()).collect();
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
            let graphs = cpu.core_graphs(now, cell);
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
    fn cell_size_falls_back_until_layout_reports() {
        assert_eq!(cell_size(0.0, 0.0), DEFAULT_CELL);
        assert_eq!(cell_size(176.0, 1.0), DEFAULT_CELL);
        assert_eq!(cell_size(176.0, 96.0), (176.0, 96.0));
    }

    #[test]
    fn gap_breaks_every_core_curve() {
        let mut cpu = CpuState::default();
        let t0 = Instant::now();
        cpu.record(t0, sample(&[10.0, 20.0]), true);
        cpu.push_gap(t0 + Duration::from_secs(1));
        cpu.record(t0 + Duration::from_secs(30), sample(&[30.0, 40.0]), true);
        for h in &cpu.histories {
            let (line, _) = h.paths(t0 + Duration::from_secs(30), 100.0, (100.0, 50.0));
            assert_eq!(line.matches('M').count(), 2, "{line}");
        }
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

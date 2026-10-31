//! 性能页曲线的悬停提示（#81）的界面接线：鼠标位置 -> `hover::build` -> 写回 `AppWindow::graph_hover`。
//!
//! - 数值、时间、进程都取曲线现有的历史（`History::hover` 找到曲线实际绘制的那个点），不额外采集。
//! - 提示用的“现在”与量程取自最近一次重画曲线时（`State::drawn`），所以引导线下的点就是画出来的点；
//!   每次重画（采样、切换窗口 / 曲线、改变窗口大小）后，如果鼠标还停在曲线上，提示随之刷新。
//! - 日志只记数值与结果（鼠标进入时一行，进程只记 PID，不记进程名）。
//! - 测试钩子 `--hover <曲线>:<比例|peak>[,…][@秒]`：不需要真实鼠标，把读到的提示写进日志，见 `scripts/test-hover.ps1`。

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{Controller, State};
use crate::history::Hover;
use crate::hover::{self, Input, Shown};
use crate::{AppWindow, CpuData, GraphHover, ResourceKind};

/// 性能页在侧边栏里的页码。
const PERFORMANCE_PAGE: i32 = 1;
/// 钩子默认等多少秒再读（让曲线上有足够的历史）。
const HOOK_DELAY_SECS: u64 = 12;

/// 最近一次画曲线时的时间基准、量程与区域尺寸。
#[derive(Clone, Copy, Debug)]
pub(super) struct Drawn {
    pub now: Instant,
    pub scale: f64,
    pub size: (f64, f64),
}

#[derive(Default)]
pub(super) struct HoverState {
    /// 鼠标在曲线区域内的位置；不在曲线上为 `None`。
    pos: Cell<Option<(f64, f64)>>,
    /// 界面上是否正显示着提示（用来只在进入时记一行日志，离开时清掉）。
    shown: Cell<bool>,
}

fn to_slint(s: &Shown) -> GraphHover {
    let (dot_x, dot_y) = s.dot.unwrap_or_default();
    GraphHover {
        active: true,
        guide_x: s.guide_x as f32,
        dot_visible: s.dot.is_some(),
        dot_x: dot_x as f32,
        dot_y: dot_y as f32,
        card_x: s.card.0 as f32,
        card_y: s.card.1 as f32,
        card_width: s.card.2 as f32,
        card_height: s.card.3 as f32,
        lines: ModelRc::new(VecModel::from(s.lines.iter().map(SharedString::from).collect::<Vec<_>>())),
    }
}

/// 一行只含数值的日志：曲线、窗口、位置、命中的点（数值 / 均值 / 距现在多少秒）、圆点纵坐标与量程（核对与曲线高度一致）、峰值进程 PID。
fn reading_line(curve: &str, window: &str, size: (f64, f64), scale: f64, s: &Shown) -> String {
    let head = format!("曲线={curve} 窗口={window} x={:.1} 比例={:.4}", s.guide_x, s.guide_x / size.0.max(1.0));
    match s.hover {
        Hover::NoData => format!("{head} 状态=无数据"),
        Hover::Point(p) => format!(
            "{head} 状态=有数据 数值={:.3} 均值={:.3} 点距今={:.2} 量程={scale} 图高={:.1} 圆点y={:.2} 进程PID={} 卡片行数={}",
            p.max,
            p.mean,
            p.age,
            size.1,
            s.dot.map_or(f64::NAN, |d| d.1),
            s.pid.map_or_else(|| "无".to_string(), |p| p.to_string()),
            s.lines.len()
        ),
    }
}

impl Controller {
    pub(super) fn bind_graph_hover(self: &Rc<Self>, ui: &AppWindow) {
        let c = Rc::downgrade(self);
        ui.on_graph_hover_moved(move |x, y| {
            if let Some(c) = c.upgrade() {
                c.hover.pos.set(Some((f64::from(x), f64::from(y))));
                let s = c.state.borrow();
                c.refresh_hover(&s);
            }
        });
        let c = Rc::downgrade(self);
        ui.on_graph_hover_left(move || {
            if let Some(c) = c.upgrade() {
                c.hover.pos.set(None);
                c.hide_hover();
            }
        });
    }

    fn hide_hover(&self) {
        if self.hover.shown.replace(false)
            && let Some(ui) = self.ui.upgrade()
        {
            ui.set_graph_hover(GraphHover::default());
            self.log.borrow_mut().line("悬停 离开");
        }
    }

    /// 选中曲线在鼠标位置处的提示；没有可悬停的曲线（迷你模式、逻辑处理器小图、尚无数据）为 `None`。
    fn hover_at(&self, s: &State, ui: &AppWindow, pos: (f64, f64)) -> Option<(Shown, Drawn)> {
        let drawn = s.drawn.get()?;
        if s.mini || (s.resources.instances().is_empty()) {
            return None;
        }
        let id = s.resources.selected();
        let kind = s.resources.instances().iter().find(|i| i.id == id)?.kind;
        if kind == ResourceKind::Cpu && ui.global::<CpuData>().get_per_core() {
            return None;
        }
        let shown = hover::build(&Input {
            kind,
            history: s.resources.history(id),
            names: &s.peak_names,
            now: drawn.now,
            window: s.graph_window(),
            scale: drawn.scale,
            size: drawn.size,
            pos,
            memory_total: s.system.memory_total,
            disks: s.disks.len(),
        });
        Some((shown, drawn))
    }

    /// 鼠标还停在曲线上时，按最新的曲线重算提示（采样、切换曲线 / 窗口、窗口缩放之后调用）。
    pub(super) fn refresh_hover(&self, s: &State) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(pos) = self.hover.pos.get() else { return };
        if ui.get_page() != PERFORMANCE_PAGE {
            return self.hide_hover();
        }
        let Some((shown, drawn)) = self.hover_at(s, &ui, pos) else { return self.hide_hover() };
        ui.set_graph_hover(to_slint(&shown));
        if !self.hover.shown.replace(true) {
            let line = reading_line(s.resources.selected(), s.graph_window().key(), drawn.size, drawn.scale, &shown);
            self.log.borrow_mut().line(format!("悬停 进入 {line}"));
        }
    }

    /// 测试钩子 `--hover <曲线>:<比例|peak>[,…][@秒]`。曲线为 cpu / memory / disk / network / gpu；
    /// 比例 0 为曲线最左、1 为最右，`peak` 为该曲线在当前窗口里数值最高的位置，`pid=<N>` 为峰值进程是 N 的点里数值最高的位置。每一项在日志里写一行“悬停 钩子 …”。
    pub fn queue_hover(self: &Rc<Self>, spec: &str) {
        let (list, delay) = match spec.rsplit_once('@') {
            Some((list, secs)) => (list.to_owned(), secs.parse().unwrap_or(HOOK_DELAY_SECS)),
            None => (spec.to_owned(), HOOK_DELAY_SECS),
        };
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_secs(delay), move || {
            if let Some(c) = weak.upgrade() {
                for item in list.split(',') {
                    c.run_hover_hook(item);
                }
            }
        });
    }

    fn run_hover_hook(&self, item: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        let (curve, at) = item.split_once(':').unwrap_or((item, "1"));
        let kind = match curve {
            "cpu" => ResourceKind::Cpu,
            "memory" => ResourceKind::Memory,
            "disk" => ResourceKind::Disk,
            "network" => ResourceKind::Network,
            "gpu" => ResourceKind::Gpu,
            other => return self.log.borrow_mut().line(format!("悬停 钩子 未知的曲线 {other}")),
        };
        let id = self.state.borrow().resources.instances().iter().find(|i| i.kind == kind).map(|i| i.id.clone());
        let Some(id) = id else { return self.log.borrow_mut().line(format!("悬停 钩子 曲线={curve} 状态=没有这类曲线")) };
        ui.set_page(PERFORMANCE_PAGE);
        self.select_resource(&id);
        let s = self.state.borrow();
        let Some(drawn) = s.drawn.get() else { return self.log.borrow_mut().line(format!("悬停 钩子 曲线={curve} 状态=尚未画出曲线")) };
        let (w, h) = drawn.size;
        let at_x = |x: f64| self.hover_at(&s, &ui, (x, h / 2.0));
        // `peak`：数值最高的位置（并列取最左）；`pid=<N>`：峰值进程是 N 的点里数值最高的位置。
        let only_pid = at.strip_prefix("pid=").and_then(|v| v.parse::<u32>().ok());
        let found = if at == "peak" || only_pid.is_some() {
            // 以 1 像素为步长扫描整条曲线。
            let mut best: Option<(f64, Shown, Drawn)> = None;
            for x in 0..=(w as u32) {
                if let Some((shown, d)) = at_x(f64::from(x))
                    && let Hover::Point(p) = shown.hover
                    && only_pid.is_none_or(|pid| shown.pid == Some(pid))
                    && best.as_ref().is_none_or(|(v, ..)| p.max > *v)
                {
                    best = Some((p.max, shown, d));
                }
            }
            best.map(|(_, shown, d)| (shown, d))
        } else {
            at_x(at.parse::<f64>().unwrap_or(1.0).clamp(0.0, 1.0) * w)
        };
        let line = match found {
            Some((shown, d)) => reading_line(id.as_str(), s.graph_window().key(), d.size, d.scale, &shown),
            None => format!("曲线={curve} 状态=无法悬停"),
        };
        self.log.borrow_mut().line(format!("悬停 钩子 {line}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    #[test]
    fn slint_struct_mirrors_the_shown_card() {
        let shown = Shown {
            guide_x: 120.5,
            dot: Some((120.5, 33.0)),
            card: (134.5, 10.0, 244.0, 50.0),
            lines: vec!["a".into(), "b".into()],
            hover: Hover::NoData,
            pid: None,
        };
        let g = to_slint(&shown);
        assert!(g.active && g.dot_visible);
        assert_eq!((g.guide_x, g.dot_y, g.card_x, g.card_height), (120.5, 33.0, 134.5, 50.0));
        assert_eq!(g.lines.row_count(), 2);
        let none = to_slint(&Shown { dot: None, ..shown });
        assert!(!none.dot_visible);
    }

    #[test]
    fn reading_line_has_only_numbers_and_no_process_name() {
        use crate::history::Pt;
        let p = Pt { age: 12.0, max: 97.5, mean: 97.5, who: 3 };
        let shown = Shown {
            guide_x: 300.0,
            dot: Some((300.0, 7.5)),
            card: (314.0, 0.0, 244.0, 67.0),
            lines: vec!["97.5%".into(), "12 秒前".into(), "CPU 最高：secret.exe（PID 99）".into()],
            hover: Hover::Point(p),
            pid: Some(99),
        };
        let line = reading_line("cpu", "60s", (600.0, 300.0), 100.0, &shown);
        assert!(line.contains("数值=97.500") && line.contains("进程PID=99") && line.contains("比例=0.5000"), "{line}");
        assert!(!line.contains("secret"), "进程名不进日志：{line}");
        let empty = reading_line("cpu", "60s", (600.0, 300.0), 100.0, &Shown { hover: Hover::NoData, dot: None, pid: None, ..shown });
        assert!(empty.ends_with("状态=无数据"), "{empty}");
    }
}

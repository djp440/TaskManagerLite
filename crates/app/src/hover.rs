//! 性能曲线的悬停提示（#81）：把鼠标位置变成“引导线、曲线上的圆点、提示卡片的文字与位置”。
//!
//! 数值与时间直接取曲线现有的历史（`History::hover` 找到的就是曲线实际绘制的那个点，长窗口是该桶的最大值与均值），
//! CPU、磁盘曲线再带上该点当时占用最高的进程（`peak::ProcNames` 里查名字）；不额外采集任何东西。
//! 纯逻辑，不依赖界面对象，可直接单测；界面接线在 `controller/hover.rs`。

use std::time::Instant;

use crate::format;
use crate::history::{History, Hover, Pt, Window};
use crate::peak::ProcNames;
use crate::{ResourceKind, texts};

/// 提示卡片的固定宽度（逻辑像素）；过长的行（进程名）在卡片里省略。
pub const CARD_W: f64 = 288.0;
const PAD: f64 = 8.0;
const LINE_H: f64 = 17.0;
/// 卡片与引导线之间的间隙。
const GAP: f64 = 14.0;
/// 曲线区域右缘留出的一条窄带：卡片不会压到最新的数据。
const RIGHT_RESERVE: f64 = 28.0;
/// 卡片与鼠标指针在竖直方向上的间隙。
const POINTER_GAP: f64 = 16.0;

/// 鼠标 x（曲线区域内，逻辑像素）对应“距现在多少秒”：右边缘 0，左边缘为窗口时长。
pub fn age_at(x: f64, width: f64, window: Window) -> f64 {
    let ratio = if width > 0.0 { (1.0 - x / width).clamp(0.0, 1.0) } else { 0.0 };
    ratio * window.duration().as_secs_f64()
}

/// [`age_at`] 的逆运算：距现在 `age` 秒的点在曲线区域里的 x（与 `History::paths` 的横坐标一致）。
pub fn x_of(age: f64, width: f64, window: Window) -> f64 {
    width * (1.0 - age / window.duration().as_secs_f64())
}

/// 卡片的高度：上下内边距加每行固定行高。
pub fn card_height(lines: usize) -> f64 {
    PAD * 2.0 + LINE_H * lines.max(1) as f64
}

/// 提示卡片的左上角（相对曲线区域）。
///
/// - 水平：默认放在引导线右侧；右侧放不下（卡片右缘会进入右缘预留带，即压到最新的数据）就翻到引导线左侧；
///   两侧都放不下（曲线区域太窄）时选空间更大的一侧并夹紧在区域内。
/// - 竖直：默认在指针上方，上方放不下放到指针下方，最后夹紧在区域内（区域比卡片矮时贴顶）。
pub fn place_card(guide_x: f64, pointer_y: f64, area: (f64, f64), card: (f64, f64)) -> (f64, f64) {
    let (aw, ah) = area;
    let (cw, ch) = card;
    let right = guide_x + GAP;
    let left = guide_x - GAP - cw;
    let fits_right = right + cw <= aw - RIGHT_RESERVE;
    let fits_left = left >= 0.0;
    let x = if fits_right {
        right
    } else if fits_left {
        left
    } else if aw - right >= guide_x - GAP {
        right
    } else {
        left
    };
    let x = x.min(aw - cw).max(0.0);
    let above = pointer_y - POINTER_GAP - ch;
    let y = if above >= 0.0 { above } else { pointer_y + POINTER_GAP };
    let y = y.min(ah - ch).max(0.0);
    (x, y)
}

/// 悬停的输入：曲线、窗口、绘制时的时间基准与量程，以及鼠标位置。
pub struct Input<'a> {
    pub kind: ResourceKind,
    pub history: &'a History,
    pub names: &'a ProcNames,
    /// 与曲线路径同一个“现在”，保证引导线下的点就是画出来的点。
    pub now: Instant,
    pub window: Window,
    /// 曲线顶端对应的取值（与 `History::paths` 用的相同），决定圆点高度。
    pub scale: f64,
    /// 曲线区域的尺寸（逻辑像素）。
    pub size: (f64, f64),
    /// 鼠标在曲线区域内的位置。
    pub pos: (f64, f64),
    pub memory_total: u64,
    /// 物理磁盘个数：多块磁盘时进程的读写口径是整机，卡片里注明。
    pub disks: usize,
}

/// 一次悬停的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub guide_x: f64,
    /// 曲线上对应点的 (x, y)；没有数据时为 `None`。
    pub dot: Option<(f64, f64)>,
    /// 卡片的 (x, y, 宽, 高)。
    pub card: (f64, f64, f64, f64),
    /// 卡片每一行的文字，第一行是主数值。
    pub lines: Vec<String>,
    pub hover: Hover,
    /// 峰值进程的 PID（只有 CPU、磁盘曲线的点带进程；名字已被淘汰时也没有）。
    pub pid: Option<u32>,
}

/// 按曲线类型显示取值；内存附带占物理内存的百分比。
fn value_text(kind: ResourceKind, v: f64, memory_total: u64) -> String {
    match kind {
        ResourceKind::Memory => {
            let gib = |b: f64| b / 1024.0 / 1024.0 / 1024.0;
            let total = memory_total.max(1) as f64;
            format!("{:.1}/{:.1} GB ({:.0}%)", gib(v), gib(total), v / total * 100.0)
        }
        ResourceKind::Network => format::rate(v),
        _ => format::percent(v as f32),
    }
}

/// 曲线是否记录峰值进程：CPU、磁盘有；内存、GPU 没有这个概念；网络没有进程级数据。
pub fn has_process(kind: ResourceKind) -> bool {
    matches!(kind, ResourceKind::Cpu | ResourceKind::Disk)
}

pub fn build(i: &Input) -> Shown {
    let (w, h) = i.size;
    let guide_x = i.pos.0.clamp(0.0, w.max(0.0));
    let hover = i.history.hover(i.now, i.window, age_at(guide_x, w, i.window));
    let (lines, dot, pid) = match hover {
        Hover::NoData => (vec![texts::hover_no_data().to_string()], None, None),
        Hover::Point(p) => point_lines(i, &p),
    };
    let ch = card_height(lines.len());
    let (cx, cy) = place_card(guide_x, i.pos.1.clamp(0.0, h.max(0.0)), i.size, (CARD_W, ch));
    Shown { guide_x, dot, card: (cx, cy, CARD_W, ch), lines, hover, pid }
}

fn point_lines(i: &Input, p: &Pt) -> (Vec<String>, Option<(f64, f64)>, Option<u32>) {
    let value = |v: f64| value_text(i.kind, v, i.memory_total);
    let aggregated = i.window.bucket_secs();
    let mut lines = Vec::with_capacity(5);
    match aggregated {
        Some(bucket) => {
            lines.push(texts::hover_peak(&value(p.max)));
            lines.push(texts::hover_mean(&value(p.mean)));
            lines.push(format!("{}（{}）", texts::hover_ago(p.age, true), texts::hover_bucket(bucket)));
        }
        None => {
            lines.push(value(p.max));
            lines.push(texts::hover_ago(p.age, false));
        }
    }
    let who = has_process(i.kind).then(|| i.names.get(p.who)).flatten();
    if let Some((pid, name)) = who {
        lines.push(texts::hover_top_process(i.kind, i.disks > 1, name, pid));
    }
    let x = x_of(p.age, i.size.0, i.window).clamp(0.0, i.size.0);
    let y = i.size.1 * (1.0 - (p.max / i.scale.max(f64::MIN_POSITIVE)).clamp(0.0, 1.0));
    (lines, Some((x, y)), who.map(|(pid, _)| pid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tml_core::ProcessKey;

    const AREA: (f64, f64) = (600.0, 300.0);

    #[test]
    fn x_and_age_are_inverse_in_every_window() {
        for w in Window::ALL {
            let span = w.duration().as_secs_f64();
            assert_eq!(age_at(600.0, 600.0, w), 0.0, "右边缘是现在");
            assert_eq!(age_at(0.0, 600.0, w), span, "左边缘是窗口时长");
            assert!((age_at(300.0, 600.0, w) - span / 2.0).abs() < 1e-9);
            for x in [0.0, 17.0, 300.0, 599.0, 600.0] {
                assert!((x_of(age_at(x, 600.0, w), 600.0, w) - x).abs() < 1e-9);
            }
            // 鼠标超出区域按边缘算。
            assert_eq!(age_at(-5.0, 600.0, w), span);
            assert_eq!(age_at(900.0, 600.0, w), 0.0);
        }
        assert_eq!(age_at(10.0, 0.0, Window::Sec60), 0.0, "零宽度不除零");
    }

    #[test]
    fn ago_text_is_readable_for_short_and_long_windows() {
        assert_eq!(texts::hover_ago(0.2, false), "0 秒前");
        assert_eq!(texts::hover_ago(12.4, false), "12 秒前");
        assert_eq!(texts::hover_ago(59.6, false), "1 分钟前");
        assert_eq!(texts::hover_ago(60.0, false), "1 分钟前");
        assert_eq!(texts::hover_ago(185.0, false), "3 分 5 秒前");
        assert_eq!(texts::hover_ago(599.0, true), "约 9 分 59 秒前");
        assert_eq!(texts::hover_ago(3600.0, false), "1 小时前");
        assert_eq!(texts::hover_ago(3600.0 + 25.0 * 60.0, true), "约 1 小时 25 分前");
        assert_eq!(texts::hover_ago(-1.0, false), "0 秒前");
    }

    #[test]
    fn card_flips_left_near_the_right_edge_and_keeps_the_newest_data_free() {
        let (cw, ch) = (CARD_W, card_height(3));
        // 引导线靠左：卡片在右侧。
        let (x, _) = place_card(100.0, 150.0, AREA, (cw, ch));
        assert_eq!(x, 100.0 + GAP);
        // 引导线靠右：翻到左侧，右缘离引导线一个间隙，不压到最新的数据。
        let (x, _) = place_card(560.0, 150.0, AREA, (cw, ch));
        assert_eq!(x, 560.0 - GAP - cw);
        assert!(x + cw < 560.0);
        // 恰好放得下（右缘贴着预留带）仍在右侧，再多一像素就翻。
        let wide = (900.0, 300.0);
        let edge = wide.0 - RIGHT_RESERVE - cw - GAP;
        assert_eq!(place_card(edge, 150.0, wide, (cw, ch)).0, edge + GAP);
        assert_eq!(place_card(edge + 1.0, 150.0, wide, (cw, ch)).0, edge + 1.0 - GAP - cw);
        // 任何位置卡片都不会越出区域。
        for gx in (0..=600).step_by(7) {
            let (x, y) = place_card(f64::from(gx), 150.0, AREA, (cw, ch));
            assert!(x >= 0.0 && x + cw <= AREA.0 && y >= 0.0 && y + ch <= AREA.1, "{gx}: {x},{y}");
        }
    }

    #[test]
    fn card_is_clamped_vertically_and_in_narrow_areas() {
        let (cw, ch) = (CARD_W, card_height(4));
        // 指针在上方：放不下指针上方就放到下方。
        let (_, y) = place_card(100.0, 20.0, AREA, (cw, ch));
        assert_eq!(y, 20.0 + POINTER_GAP);
        // 指针靠下：在指针上方。
        let (_, y) = place_card(100.0, 290.0, AREA, (cw, ch));
        assert_eq!(y, 290.0 - POINTER_GAP - ch);
        // 区域比卡片矮：贴顶，不出现负坐标。
        let (_, y) = place_card(100.0, 40.0, (600.0, 80.0), (cw, ch));
        assert_eq!(y, 0.0);
        // 指针靠下且下方也放不下：夹紧到底部。
        let (_, y) = place_card(100.0, 30.0, (600.0, 100.0), (cw, ch));
        assert!(y + ch <= 100.0 || y == 0.0);
        // 区域比卡片窄：选空间更大的一侧并夹紧（不为负）。
        let (x, _) = place_card(60.0, 50.0, (200.0, 300.0), (cw, ch));
        assert_eq!(x, 0.0);
        let (x, _) = place_card(150.0, 50.0, (200.0, 300.0), (cw, ch));
        assert_eq!(x, 0.0);
    }

    fn names_with(pid: u32, name: &str) -> (ProcNames, u32) {
        let mut names = ProcNames::default();
        let id = names.intern(ProcessKey { pid, create_time: 1 }, name);
        (names, id)
    }

    fn input<'a>(
        kind: ResourceKind,
        history: &'a History,
        names: &'a ProcNames,
        now: Instant,
        window: Window,
        pos: (f64, f64),
    ) -> Input<'a> {
        Input { kind, history, names, now, window, scale: 100.0, size: AREA, pos, memory_total: 16 << 30, disks: 1 }
    }

    #[test]
    fn short_window_card_shows_value_age_and_process_consistent_with_the_curve() {
        let t0 = Instant::now();
        let (names, id) = names_with(4242, "burner.exe");
        let mut h = History::default();
        for s in 0..=59u64 {
            let (v, who) = if s == 40 { (97.5, id) } else { (5.0, crate::peak::NO_PROC) };
            h.push_with(t0 + Duration::from_secs(s), Some(v), who);
        }
        let now = t0 + Duration::from_secs(59);
        // 尖峰在距现在 19 秒处；窗口 60 秒、宽 600，x = 600 * (1 - 19/60)。
        let x = x_of(19.0, 600.0, Window::Sec60);
        let shown = build(&input(ResourceKind::Cpu, &h, &names, now, Window::Sec60, (x, 100.0)));
        assert_eq!(shown.lines, ["97.5%", "19 秒前", "CPU 最高：burner.exe（PID 4242）"]);
        assert_eq!(shown.pid, Some(4242));
        let (dx, dy) = shown.dot.unwrap();
        assert!((dx - x).abs() < 1e-9);
        // 圆点高度与曲线路径的 y 一致：y = h * (1 - v / scale)。
        assert!((dy - 300.0 * (1.0 - 0.975)).abs() < 1e-9, "{dy}");
        // 内存、网络、GPU 不显示进程。
        for kind in [ResourceKind::Memory, ResourceKind::Network, ResourceKind::Gpu] {
            let s = build(&input(kind, &h, &names, now, Window::Sec60, (x, 100.0)));
            assert_eq!(s.lines.len(), 2, "{kind:?}: {:?}", s.lines);
            assert_eq!(s.pid, None);
        }
        // 引导线跟着鼠标，不是吸附到点。
        let off = build(&input(ResourceKind::Cpu, &h, &names, now, Window::Sec60, (x + 3.0, 100.0)));
        assert_eq!(off.guide_x, x + 3.0);
    }

    #[test]
    fn long_window_card_shows_bucket_peak_mean_and_the_peak_samples_process() {
        let t0 = Instant::now();
        let (names, id) = names_with(7, "spike.exe");
        let mut h = History::default();
        // 桶宽 2 秒：第 10、11 秒同桶，11 秒是尖峰，由 spike.exe 造成。
        for s in 0..=40u64 {
            let (v, who) = if s == 11 { (80.0, id) } else { (10.0, crate::peak::NO_PROC) };
            h.push_with(t0 + Duration::from_secs(s), Some(v), who);
        }
        let now = t0 + Duration::from_secs(40);
        let x = x_of(29.0, 600.0, Window::Min5);
        let shown = build(&input(ResourceKind::Disk, &h, &names, now, Window::Min5, (x, 100.0)));
        assert_eq!(shown.lines[0], "峰值 80.0%");
        assert_eq!(shown.lines[1], "均值 45.0%");
        assert_eq!(shown.lines[2], "约 29 秒前（每点 2 秒）");
        assert_eq!(shown.lines[3], "读写最高：spike.exe（PID 7）");
        // 多块磁盘时注明是整机口径。
        let multi = build(&Input { disks: 2, ..input(ResourceKind::Disk, &h, &names, now, Window::Min5, (x, 100.0)) });
        assert_eq!(multi.lines[3], "全部磁盘读写最高：spike.exe（PID 7）");
    }

    #[test]
    fn gap_and_empty_regions_show_no_data_without_a_dot() {
        let t0 = Instant::now();
        let names = ProcNames::default();
        let mut h = History::default();
        for s in 0..10u64 {
            h.push(t0 + Duration::from_secs(s), Some(20.0));
        }
        h.push(t0 + Duration::from_secs(10), None);
        for s in 30..40u64 {
            h.push(t0 + Duration::from_secs(s), Some(20.0));
        }
        let now = t0 + Duration::from_secs(39);
        // 断档中间（距现在 20 秒）。
        let gap = build(&input(ResourceKind::Cpu, &h, &names, now, Window::Sec60, (x_of(20.0, 600.0, Window::Sec60), 50.0)));
        assert_eq!(gap.lines, ["该时刻没有数据"]);
        assert!(gap.dot.is_none() && gap.pid.is_none());
        // 曲线开始之前（窗口左端）。
        let early = build(&input(ResourceKind::Cpu, &h, &names, now, Window::Sec60, (0.0, 50.0)));
        assert_eq!(early.hover, Hover::NoData);
        // 有数据的位置则有圆点，卡片行数随之变化。
        let on = build(&input(ResourceKind::Cpu, &h, &names, now, Window::Sec60, (x_of(5.0, 600.0, Window::Sec60), 50.0)));
        assert!(on.dot.is_some() && on.lines[0] == "20.0%");
        // 卡片高度与行数一致，且始终在区域内。
        for s in [&gap, &early, &on] {
            assert_eq!(s.card.3, card_height(s.lines.len()));
            assert!(s.card.0 >= 0.0 && s.card.0 + s.card.2 <= AREA.0 && s.card.1 >= 0.0);
        }
    }

    #[test]
    fn memory_card_shows_amount_and_share_of_physical_memory() {
        let t0 = Instant::now();
        let names = ProcNames::default();
        let mut h = History::default();
        h.push(t0, Some(8.0 * 1024.0 * 1024.0 * 1024.0));
        let shown = build(&input(ResourceKind::Memory, &h, &names, t0, Window::Sec60, (600.0, 10.0)));
        assert_eq!(shown.lines[0], "8.0/16.0 GB (50%)");
    }
}

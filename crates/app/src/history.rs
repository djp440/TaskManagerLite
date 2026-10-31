//! 资源曲线的采样历史：分层环形缓冲。
//!
//! - 原始层：最近 60 秒的原始采样（带时间戳），悬停 / 当前数值、导出 CSV 与 60 秒窗口都用它。
//! - 聚合层：5 分钟、10 分钟、1 小时窗口各一层，按固定时长的桶对原始采样降采样，每个桶同时保留
//!   最大值与均值；曲线按最大值绘制，短暂的尖峰不会被抹平。每层的点数有上限，总内存与采样间隔无关。
//! - 所有点按时间戳定位（距“现在”多少秒），而不是按个数，所以采样间隔改变、暂停、窗口不可见之后
//!   时间轴依然正确。采样中断（暂停、窗口不可见）用“空点”标记，绘制时在此断开，断档处留空不连线。
//!
//! - CPU、磁盘曲线的每个点还带一个峰值进程标识（`ProcId`，4 字节，名字在 `peak::ProcNames`）；聚合桶里记的是
//!   桶内峰值那次采样对应的进程。悬停提示（#81）按 x 位置找到曲线实际绘制的那个点，读出数值、均值与进程。
//!
//! 仅存内存，不落盘。纯逻辑，不依赖界面，可直接单测。

use std::collections::VecDeque;
use std::fmt::Write;
use std::time::{Duration, Instant};

use crate::peak::{NO_PROC, ProcId};

/// 性能页曲线的时间窗口。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Window {
    #[default]
    Sec60,
    Min5,
    Min10,
    Hour1,
}

impl Window {
    pub const ALL: [Window; 4] = [Window::Sec60, Window::Min5, Window::Min10, Window::Hour1];

    pub fn duration(self) -> Duration {
        Duration::from_secs(match self {
            Window::Sec60 => 60,
            Window::Min5 => 5 * 60,
            Window::Min10 => 10 * 60,
            Window::Hour1 => 60 * 60,
        })
    }

    /// 在界面选择器里的下标，也是 [`Window::ALL`] 的下标。
    pub fn index(self) -> i32 {
        Window::ALL.iter().position(|&w| w == self).unwrap_or(0) as i32
    }

    /// 越界的下标回退为 60 秒。
    pub fn from_index(i: i32) -> Self {
        usize::try_from(i).ok().and_then(|i| Window::ALL.get(i).copied()).unwrap_or_default()
    }

    /// 写进设置文件的名字。
    pub fn key(self) -> &'static str {
        match self {
            Window::Sec60 => "60s",
            Window::Min5 => "5m",
            Window::Min10 => "10m",
            Window::Hour1 => "1h",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Window::ALL.iter().copied().find(|w| w.key() == s)
    }

    /// 聚合桶宽（秒）；60 秒窗口读原始采样，没有桶。
    pub fn bucket_secs(self) -> Option<u64> {
        self.layer().map(|i| BUCKET_MS[i] / 1000)
    }

    /// 聚合层的下标；60 秒窗口直接读原始层。
    fn layer(self) -> Option<usize> {
        match self {
            Window::Sec60 => None,
            Window::Min5 => Some(0),
            Window::Min10 => Some(1),
            Window::Hour1 => Some(2),
        }
    }
}

/// 各聚合层的桶宽（毫秒），与 [`Window::layer`] 的下标对应。
/// 5 分钟 150 点、10 分钟 150 点、1 小时 180 点，对常见的图表宽度足够。
const BUCKET_MS: [u64; 3] = [2_000, 4_000, 20_000];
const LAYER_WINDOWS: [Window; 3] = [Window::Min5, Window::Min10, Window::Hour1];

/// 聚合层的一个点。`max` 为 NaN 表示采样中断的空点。
#[derive(Clone, Copy, Debug)]
struct Bucket {
    /// 桶内最后一次采样的时刻（距历史起点的毫秒数）。
    t: u64,
    max: f32,
    mean: f32,
    /// 桶内取得最大值的那次采样的峰值进程。
    who: ProcId,
}

impl Bucket {
    fn is_gap(&self) -> bool {
        self.max.is_nan()
    }
}

/// 一层聚合：窗口内按桶宽降采样的点，最后一个桶是正在累积的那个。
struct Layer {
    bucket_ms: u64,
    span_ms: u64,
    buckets: VecDeque<Bucket>,
    /// 最后一个桶的编号；没有正在累积的桶（刚断开）时为 `u64::MAX`。
    open: u64,
    count: u32,
    sum: f64,
}

impl Layer {
    fn new(bucket_ms: u64, span_ms: u64) -> Self {
        Self { bucket_ms, span_ms, buckets: VecDeque::new(), open: u64::MAX, count: 0, sum: 0.0 }
    }

    /// 点数上限：窗口内的桶数，加上断点与窗口外的一个连线点留出的余量。
    fn cap(&self) -> usize {
        (self.span_ms / self.bucket_ms) as usize * 2 + 4
    }

    fn push(&mut self, t: u64, value: Option<f64>, who: ProcId) {
        match value.filter(|v| v.is_finite()) {
            Some(v) => {
                let id = t / self.bucket_ms;
                match self.buckets.back_mut() {
                    Some(b) if self.open == id => {
                        b.t = t;
                        // 只在刷新最大值时换进程；并列且之前没有记录时补上。
                        if v as f32 > b.max || (v as f32 == b.max && b.who == NO_PROC) {
                            b.max = v as f32;
                            b.who = who;
                        }
                        self.sum += v;
                        self.count += 1;
                        b.mean = (self.sum / f64::from(self.count)) as f32;
                    }
                    _ => {
                        self.buckets.push_back(Bucket { t, max: v as f32, mean: v as f32, who });
                        self.open = id;
                        self.sum = v;
                        self.count = 1;
                    }
                }
            }
            None => {
                self.open = u64::MAX;
                // 开头的断点与连续的断点没有意义。
                if self.buckets.back().is_some_and(|b| !b.is_gap()) {
                    self.buckets.push_back(Bucket { t, max: f32::NAN, mean: f32::NAN, who: NO_PROC });
                }
            }
        }
        // 保留一个窗口外的点，让曲线从左边缘连续进入。
        while self.buckets.len() > 2 && t.saturating_sub(self.buckets[1].t) > self.span_ms {
            self.buckets.pop_front();
        }
        while self.buckets.len() > self.cap() {
            self.buckets.pop_front();
        }
    }
}

/// 原始层的一个采样点。
#[derive(Clone, Copy, Debug)]
struct Raw {
    at: Instant,
    value: Option<f64>,
    who: ProcId,
}

/// 一类资源最近一段时间的取值。`None` 表示采样中断（窗口不可见或暂停），曲线在此断开。
pub struct History {
    origin: Option<Instant>,
    /// 原始层：最近 60 秒。
    points: VecDeque<Raw>,
    layers: [Layer; 3],
}

impl Default for History {
    fn default() -> Self {
        Self {
            origin: None,
            points: VecDeque::new(),
            layers: std::array::from_fn(|i| Layer::new(BUCKET_MS[i], LAYER_WINDOWS[i].duration().as_millis() as u64)),
        }
    }
}

/// 原始层保留的时长。
const RAW_WINDOW: Duration = Duration::from_secs(60);

/// 曲线上一个实际绘制的点：距“现在”多少秒、取值（长窗口为桶内最大值）、桶均值与峰值进程。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub age: f64,
    pub max: f64,
    /// 原始层的点没有聚合，均值等于取值。
    pub mean: f64,
    pub who: ProcId,
}

/// 一段窗口内的数据，统一成点序列，`None` 是断点。
type Point = Option<Pt>;

/// 悬停定位的结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hover {
    /// 鼠标所在位置对应曲线上的这个点。
    Point(Pt),
    /// 鼠标在断档（暂停、窗口不可见）里，或在最早的点之前、最新的点之后太远处：该处没有数据。
    NoData,
}

/// 原始层：鼠标落在最新的点右侧不超过它时仍算该点（采样间隔最长 2 秒，留出抖动）。
const RAW_END_REACH_SECS: f64 = 3.0;
/// 原始层：鼠标落在断档边缘或最早的点之前不超过它时仍算边缘的点。
const RAW_EDGE_REACH_SECS: f64 = 1.0;

impl History {
    pub fn push(&mut self, at: Instant, value: Option<f64>) {
        self.push_with(at, value, NO_PROC);
    }

    /// 写入一个点，并记下这次采样时的峰值进程（没有则 `NO_PROC`）。热路径：每层只做 O(1) 更新，不分配。
    pub fn push_with(&mut self, at: Instant, value: Option<f64>, who: ProcId) {
        let origin = *self.origin.get_or_insert(at);
        let t = at.saturating_duration_since(origin).as_millis() as u64;
        for layer in &mut self.layers {
            layer.push(t, value, who);
        }
        self.points.push_back(Raw { at, value, who });
        // 保留一个窗口外的点，让曲线从左边缘连续进入。
        while self.points.len() > 2 && at.duration_since(self.points[1].at) > RAW_WINDOW {
            self.points.pop_front();
        }
    }

    /// 窗口内的点（含一个用于左边缘连线的窗口外的点），旧的在前。
    /// 60 秒窗口取原始采样，其余窗口取聚合层的最大值。
    fn series(&self, now: Instant, window: Window) -> impl Iterator<Item = Point> + '_ {
        let raw = window.layer().is_none();
        let origin = self.origin;
        let layer = window.layer().map(|i| &self.layers[i]);
        let raw_points = self.points.iter().take(if raw { self.points.len() } else { 0 }).map(move |r| {
            r.value.map(|v| Pt { age: now.saturating_duration_since(r.at).as_secs_f64(), max: v, mean: v, who: r.who })
        });
        let layer_points = layer.into_iter().flat_map(|l| l.buckets.iter()).filter_map(move |b| {
            let origin = origin?;
            Some((!b.is_gap()).then(|| {
                let at = origin + Duration::from_millis(b.t);
                Pt {
                    age: now.saturating_duration_since(at).as_secs_f64(),
                    max: f64::from(b.max),
                    mean: f64::from(b.mean),
                    who: b.who,
                }
            }))
        });
        raw_points.chain(layer_points)
    }

    /// 悬停定位（#81）：`age` 是鼠标位置对应的“距现在多少秒”（右边缘为 0，左边缘为窗口时长）。
    /// 返回曲线上实际绘制的、离鼠标最近的那个点（长窗口是该桶：最大值、均值与峰值进程）。
    ///
    /// - 两侧都有点且之间没有断档：取更近的一个；窗口左边缘之外、仅用于连线的那个点不会被选中。
    /// - 鼠标落在断档里：两侧的点离鼠标都超过 `edge_reach`（原始层 1 秒，长窗口一个桶宽）则没有数据；
    ///   断档边缘附近仍算断档外的那个点，否则 1 像素的偏差就会让提示忽有忽无。
    /// - 鼠标在最早的点之前：同样按 `edge_reach`；在最新的点之后：按 `end_reach`（最新的点可能比“现在”旧一个采样间隔）。
    pub fn hover(&self, now: Instant, window: Window, age: f64) -> Hover {
        let span = window.duration().as_secs_f64();
        let bucket = window.bucket_secs().map(|b| b as f64);
        let edge_reach = bucket.unwrap_or(RAW_EDGE_REACH_SECS);
        let end_reach = bucket.unwrap_or(0.0).max(RAW_END_REACH_SECS);
        let age = age.clamp(0.0, span);
        // 最后一个比鼠标更旧的点，以及它与当前位置之间是否隔着断档。
        let mut older: Option<Pt> = None;
        let mut gap = false;
        let mut newer: Option<Pt> = None;
        for p in self.series(now, window) {
            match p {
                None => gap = true,
                Some(p) if p.age > age => {
                    older = Some(p);
                    gap = false;
                }
                Some(p) => {
                    newer = Some(p);
                    break;
                }
            }
        }
        let nearest = |o: Option<Pt>, n: Option<Pt>, reach: f64| {
            let candidates = [o.map(|o| (o.age - age, o)), n.map(|n| (age - n.age, n))];
            candidates
                .into_iter()
                .flatten()
                .filter(|(d, _)| *d <= reach)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map_or(Hover::NoData, |(_, p)| Hover::Point(p))
        };
        match (older, newer) {
            (Some(o), Some(n)) if !gap => {
                // 并列时取更新的点。
                if o.age <= span && o.age - age < age - n.age { Hover::Point(o) } else { Hover::Point(n) }
            }
            (o, Some(n)) => nearest(o.filter(|o| o.age <= span), Some(n), edge_reach),
            (o, None) => nearest(o.filter(|o| o.age <= span), None, end_reach),
        }
    }

    /// 供日志核对曲线是否连续：(点数, 断点数, 相邻记录之间的最大间隔)。原始层。
    pub fn stats(&self) -> (usize, usize, Duration) {
        let gaps = self.points.iter().filter(|p| p.value.is_none()).count();
        let step = self.points.iter().zip(self.points.iter().skip(1)).map(|(a, b)| b.at.duration_since(a.at)).max();
        (self.points.len(), gaps, step.unwrap_or_default())
    }

    /// 某个窗口的 (点数, 断点数, 时间跨度)，供日志核对。跨度是最旧的有效点距 `now` 的时长，
    /// 即曲线在时间轴上从右边缘向左铺开了多远（含中间的断档）。
    pub fn window_shape(&self, now: Instant, window: Window) -> (usize, usize, Duration) {
        let (mut points, mut gaps, mut oldest) = (0, 0, 0f64);
        for p in self.series(now, window) {
            match p {
                Some(pt) => {
                    points += 1;
                    oldest = oldest.max(pt.age);
                }
                None => gaps += 1,
            }
        }
        (points, gaps, Duration::from_secs_f64(oldest))
    }

    /// 测试钩子（`--history-prefill`）：丢弃已有的点，改为以 0.5 秒间隔灌入以 `now` 为终点、长 `secs` 秒的合成数据
    /// （每 10 分钟一个断点），用来在不等待的情况下测各窗口填满后的内存与开销。
    pub fn prefill(&mut self, now: Instant, secs: u64) {
        const STEP_MS: u64 = 500;
        *self = History::default();
        for i in (1..=secs * 1000 / STEP_MS).rev() {
            let Some(at) = now.checked_sub(Duration::from_millis(i * STEP_MS)) else { continue };
            if i % (600_000 / STEP_MS) == 0 {
                self.push(at, None);
            }
            self.push(at, Some(((i * 37) % 100) as f64));
        }
    }

    /// 估算占用的堆内存（字节）：各层缓冲区容量之和。
    pub fn heap_bytes(&self) -> usize {
        self.points.capacity() * std::mem::size_of::<Raw>()
            + self.layers.iter().map(|l| l.buckets.capacity() * std::mem::size_of::<Bucket>()).sum::<usize>()
    }

    /// 只统计最近 60 秒内的有效样本，不含用于左边缘连线的过期点。
    pub fn window_stats(&self, now: Instant) -> Option<(f64, f64, f64)> {
        let values: Vec<_> = self.points.iter().filter(|r| now.saturating_duration_since(r.at) <= RAW_WINDOW)
            .filter_map(|r| r.value).filter(|v| v.is_finite()).collect();
        if values.is_empty() { return None; }
        Some((values.iter().copied().fold(f64::INFINITY, f64::min), values.iter().copied().fold(0.0, f64::max), values.iter().sum::<f64>() / values.len() as f64))
    }

    /// 较长窗口内的 (最大值, 均值)：取窗口内各聚合点的最大值与均值（桶均值的算术平均）；窗口内没有有效点为 `None`。
    pub fn layer_summary(&self, now: Instant, window: Window) -> Option<(f64, f64)> {
        let layer = &self.layers[window.layer()?];
        let origin = self.origin?;
        let (mut peak, mut sum, mut n) = (f64::NEG_INFINITY, 0.0, 0u32);
        for b in layer.buckets.iter().filter(|b| !b.is_gap()) {
            let age = now.saturating_duration_since(origin + Duration::from_millis(b.t));
            if age <= window.duration() {
                peak = peak.max(f64::from(b.max));
                sum += f64::from(b.mean);
                n += 1;
            }
        }
        (n > 0).then(|| (peak, sum / f64::from(n)))
    }

    /// 最近 60 秒窗口内的有效样本：(距 `now` 的秒数, 值)，旧的在前。
    /// 不含采样中断的断点与窗口外用于左边缘连线的过期点；导出曲线数据用（固定 60 秒，与窗口选择无关）。
    pub fn samples(&self, now: Instant) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .filter_map(|r| {
                let age = now.saturating_duration_since(r.at);
                r.value.filter(|v| v.is_finite() && age <= RAW_WINDOW).map(|v| (age.as_secs_f64(), v))
            })
            .collect()
    }

    /// 指定窗口内的最大值（较长窗口取聚合层的最大值），用来给纵轴定量程。
    pub fn max_in(&self, now: Instant, window: Window) -> f64 {
        self.series(now, window).flatten().map(|p| p.max).fold(0.0, f64::max)
    }

    /// 按图表的实际尺寸（逻辑像素）生成 (折线, 填充区域) 两条 SVG 路径；`scale` 对应图表顶端的取值，
    /// 横轴为 `window`（右边缘是 `now`）。较长窗口按各桶的最大值绘制。
    /// Slint 的 Path 会等比缩放 viewbox，所以坐标必须与图表尺寸一致，不能用固定坐标系。
    pub fn paths(&self, now: Instant, scale: f64, (view_w, view_h): (f64, f64), window: Window) -> (String, String) {
        let span = window.duration().as_secs_f64();
        let mut segments: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        for p in self.series(now, window) {
            match p {
                Some(Pt { age, max: v, .. }) => {
                    let x = view_w * (1.0 - age / span);
                    let y = view_h * (1.0 - (v / scale).clamp(0.0, 1.0));
                    segments.last_mut().unwrap().push((x, y));
                }
                None => segments.push(Vec::new()),
            }
        }
        let (mut line, mut area) = (String::new(), String::new());
        for seg in segments.iter().filter(|s| !s.is_empty()) {
            for (i, (x, y)) in seg.iter().enumerate() {
                let _ = write!(line, "{}{x:.1} {y:.1} ", if i == 0 { "M " } else { "L " });
            }
            let (x0, xn) = (seg[0].0, seg[seg.len() - 1].0);
            let _ = write!(area, "M {x0:.1} {view_h:.1} ");
            for (x, y) in seg {
                let _ = write!(area, "L {x:.1} {y:.1} ");
            }
            let _ = write!(area, "L {xn:.1} {view_h:.1} Z ");
        }
        // 空路径会让 Path 元素报警告，用一个不可见的点占位。
        if line.is_empty() {
            line = format!("M {view_w:.1} {view_h:.1}");
            area = line.clone();
        }
        (line, area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;
    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// 取折线里每个点的 (x, y)，M 与 L 都算。
    fn coords(line: &str) -> Vec<(f64, f64)> {
        let t: Vec<&str> = line.split_whitespace().collect();
        t.chunks(3).map(|c| (c[1].parse().unwrap(), c[2].parse().unwrap())).collect()
    }

    #[test]
    fn window_index_key_and_fallback() {
        for w in Window::ALL {
            assert_eq!(Window::from_index(w.index()), w);
            assert_eq!(Window::from_key(w.key()), Some(w));
        }
        assert_eq!(Window::from_index(-1), Window::Sec60);
        assert_eq!(Window::from_index(9), Window::Sec60);
        assert_eq!(Window::from_key("2h"), None);
        assert_eq!(Window::default(), Window::Sec60);
        assert_eq!(Window::Hour1.duration(), S(3600));
    }

    #[test]
    fn raw_layer_keeps_only_one_window_of_points() {
        let t0 = Instant::now();
        let mut h = History::default();
        for s in 0..200 {
            h.push(t0 + S(s), Some(1.0));
        }
        assert!(h.points.len() <= 62);
    }

    #[test]
    fn aggregate_keeps_max_and_mean_per_bucket() {
        let t0 = Instant::now();
        let mut h = History::default();
        // 桶宽 2 秒：每个桶里两个采样（0.5 秒间隔时是四个），其中一个是尖峰。
        for i in 0..60u64 {
            let v = if i % 10 == 3 { 100.0 } else { 10.0 };
            h.push(t0 + S(i), Some(v));
        }
        let now = t0 + S(59);
        let (peak, mean) = h.layer_summary(now, Window::Min5).unwrap();
        assert_eq!(peak, 100.0);
        // 桶均值的平均介于基线与尖峰之间，说明均值与最大值是分开保存的。
        assert!(mean > 10.0 && mean < 100.0, "{mean}");
        assert_eq!(h.max_in(now, Window::Min10), 100.0);
        // 曲线按最大值绘制：尖峰所在桶的 y 在顶端。
        let (line, _) = h.paths(now, 100.0, (300.0, 100.0), Window::Min5);
        assert!(coords(&line).iter().any(|&(_, y)| y < 0.5), "{line}");
        // 60 秒窗口不走聚合层，摘要为 None。
        assert!(h.layer_summary(now, Window::Sec60).is_none());
    }

    #[test]
    fn spike_survives_in_every_long_window() {
        for window in [Window::Min5, Window::Min10, Window::Hour1] {
            let t0 = Instant::now();
            let mut h = History::default();
            let total = window.duration().as_secs();
            for i in 0..=total {
                // 一次只有 1 秒的尖峰，落在窗口中部，其余时间为 5%。
                let v = if i == total / 2 { 97.0 } else { 5.0 };
                h.push(t0 + S(i), Some(v));
            }
            let now = t0 + S(total);
            let (line, _) = h.paths(now, 100.0, (600.0, 100.0), window);
            let pts = coords(&line);
            let top = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
            assert!(top < 4.0, "{window:?} 的尖峰被抹平了：{top}");
            // 尖峰在横轴上接近窗口正中（误差不超过一个桶宽）。
            let x = pts.iter().find(|p| p.1 < 4.0).unwrap().0;
            let bucket_px = 600.0 * (BUCKET_MS[window.layer().unwrap()] as f64 / 1000.0) / total as f64;
            assert!((x - 300.0).abs() <= bucket_px * 1.5 + 1.0, "{window:?} x={x}");
        }
    }

    #[test]
    fn points_are_placed_by_timestamp_not_by_count() {
        let t0 = Instant::now();
        let mut h = History::default();
        // 前 100 秒每 0.5 秒一点，之后改成每 2 秒一点；时间轴上最后一点必须在右边缘，
        // 第 100 秒那一点距现在 100 秒，位于 10 分钟窗口的 5/6 处。
        let mut t = 0u64;
        while t < 100_000 {
            h.push(t0 + MS(t), Some(10.0));
            t += 500;
        }
        h.push(t0 + MS(100_000), Some(80.0));
        let mut t = 102_000u64;
        while t <= 200_000 {
            h.push(t0 + MS(t), Some(10.0));
            t += 2_000;
        }
        let now = t0 + MS(200_000);
        let (line, _) = h.paths(now, 100.0, (600.0, 100.0), Window::Min10);
        let pts = coords(&line);
        let last = pts.last().unwrap();
        assert!((last.0 - 600.0).abs() < 0.2, "最新点应在右边缘：{last:?}");
        let spike = pts.iter().find(|p| p.1 < 25.0).unwrap();
        // 距现在 100 秒 -> x = 600 * (1 - 100/600) = 500，误差为一个桶宽（4 秒 = 4px）。
        assert!((spike.0 - 500.0).abs() <= 4.5, "{spike:?}");
        // 同一份数据在 5 分钟窗口里按 300 秒标度：x = 600 * (1 - 100/300) = 400。
        let (line5, _) = h.paths(now, 100.0, (600.0, 100.0), Window::Min5);
        let spike5 = coords(&line5).into_iter().find(|p| p.1 < 25.0).unwrap();
        assert!((spike5.0 - 400.0).abs() <= 4.5, "{spike5:?}");
    }

    #[test]
    fn pause_leaves_a_gap_in_every_window() {
        let t0 = Instant::now();
        let mut h = History::default();
        for i in 0..30u64 {
            h.push(t0 + S(i), Some(50.0));
        }
        // 暂停 40 秒：期间没有任何采样；恢复时先写断点，再从下一个采样继续（与 Controller::on_sample 一致）。
        let resume = t0 + S(70);
        h.push(resume, None);
        h.push(resume, None);
        for i in 71..90u64 {
            h.push(t0 + S(i), Some(50.0));
        }
        let now = t0 + S(89);
        for window in Window::ALL {
            let (line, area) = h.paths(now, 100.0, (600.0, 100.0), window);
            assert_eq!(line.matches('M').count(), 2, "{window:?} 应断成两段：{line}");
            assert_eq!(area.matches('Z').count(), 2, "{window:?}");
            // 断档（暂停那 40 秒）里没有任何点。
            let span = window.duration().as_secs_f64();
            let (lo, hi) = (600.0 * (1.0 - 60.0 / span), 600.0 * (1.0 - 20.0 / span));
            let in_gap: Vec<_> = coords(&line).into_iter().filter(|p| p.0 > lo + 0.5 && p.0 < hi - 0.5).collect();
            // 这里的区间从暂停开头之后到恢复之前（距现在 20..60 秒）。
            assert!(in_gap.is_empty(), "{window:?} {in_gap:?}");
        }
        // 连续的断点只留一个：聚合层里恰好一个空点。
        for layer in &h.layers {
            assert_eq!(layer.buckets.iter().filter(|b| b.is_gap()).count(), 1);
        }
        let (points, gaps, _) = h.window_shape(now, Window::Min5);
        assert_eq!(gaps, 1);
        assert!(points > 0);
    }

    #[test]
    fn gap_at_start_or_repeated_adds_no_markers() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push(t0, None);
        h.push(t0 + S(1), None);
        assert!(h.layers.iter().all(|l| l.buckets.is_empty()));
        h.push(t0 + S(2), Some(1.0));
        h.push(t0 + S(3), None);
        h.push(t0 + S(3), None);
        assert!(h.layers.iter().all(|l| l.buckets.iter().filter(|b| b.is_gap()).count() == 1));
    }

    #[test]
    fn point_counts_and_memory_are_bounded_for_every_window() {
        let t0 = Instant::now();
        let mut h = History::default();
        // 0.5 秒间隔连续跑 3 小时，每 7 分钟断一次（带来额外的空点）。
        let mut t = 0u64;
        while t < 3 * 3600 * 1000 {
            if t % (7 * 60 * 1000) == 0 {
                h.push(t0 + MS(t), None);
            }
            h.push(t0 + MS(t), Some((t % 97) as f64));
            t += 500;
        }
        let now = t0 + MS(t);
        for (i, w) in LAYER_WINDOWS.iter().enumerate() {
            let layer = &h.layers[i];
            assert!(layer.buckets.len() <= layer.cap(), "{w:?}: {}", layer.buckets.len());
            // 点数约等于窗口 / 桶宽，而不是采样个数。
            let nominal = (w.duration().as_millis() as u64 / BUCKET_MS[i]) as usize;
            let (points, _, span) = h.window_shape(now, *w);
            assert!(points <= nominal + 3 && points + 3 >= nominal * 9 / 10, "{w:?}: {points} 点，标称 {nominal}");
            assert!(span.as_secs() + 60 >= w.duration().as_secs() * 9 / 10, "{w:?} 跨度 {span:?}");
        }
        assert!(h.points.len() <= 130);
        // 一个 History 在 3 个聚合层加原始层下的堆内存上限：不到 32 KB。
        assert!(h.heap_bytes() < 32 * 1024, "{} 字节", h.heap_bytes());
    }

    #[test]
    fn window_shape_reports_span_and_gaps() {
        let t0 = Instant::now();
        let mut h = History::default();
        for i in 0..=100u64 {
            h.push(t0 + S(i), Some(1.0));
        }
        let now = t0 + S(100);
        let (points, gaps, span) = h.window_shape(now, Window::Min5);
        assert_eq!(gaps, 0);
        assert!((50..=52).contains(&points), "{points}");
        assert!(span.as_secs() >= 98 && span.as_secs() <= 100, "{span:?}");
        // 中间有断档时跨度仍从最旧的点算到现在。
        let mut gapped = History::default();
        gapped.push(t0, Some(1.0));
        gapped.push(t0 + S(1), None);
        gapped.push(t0 + S(60), Some(1.0));
        let (_, gaps, span) = gapped.window_shape(t0 + S(61), Window::Min5);
        assert_eq!(gaps, 1);
        assert!(span.as_secs() >= 60, "{span:?}");
        let (raw_points, _, raw_span) = h.window_shape(now, Window::Sec60);
        assert!(raw_points >= 61 && raw_points <= 62, "{raw_points}");
        assert!(raw_span.as_secs() >= 60);
    }

    #[test]
    fn mean_is_not_drawn_but_summarised() {
        let t0 = Instant::now();
        let mut h = History::default();
        // 同一个桶（2 秒）里 0 和 100：均值 50，最大值 100。
        h.push(t0, Some(0.0));
        h.push(t0 + MS(500), Some(100.0));
        let (peak, mean) = h.layer_summary(t0 + S(1), Window::Min5).unwrap();
        assert_eq!((peak, mean), (100.0, 50.0));
        let (line, _) = h.paths(t0 + S(1), 100.0, (300.0, 100.0), Window::Min5);
        assert_eq!(coords(&line).len(), 1);
        assert_eq!(coords(&line)[0].1, 0.0, "按最大值绘制");
    }

    #[test]
    fn window_statistics_ignore_gaps_and_expired_edge_point() {
        let t = Instant::now();
        let mut h = History::default();
        h.push(t, Some(999.0));
        h.push(t + S(2), Some(10.0));
        h.push(t + S(3), None);
        h.push(t + S(61), Some(30.0));
        assert_eq!(h.window_stats(t + S(61)), Some((10.0, 30.0, 20.0)));
        assert_eq!(h.window_stats(t + S(122)), None);
    }

    #[test]
    fn samples_skip_gaps_and_expired_edge_point() {
        let t = Instant::now();
        let mut h = History::default();
        h.push(t, Some(999.0));
        h.push(t + S(2), Some(10.0));
        h.push(t + S(3), None);
        h.push(t + S(61), Some(30.0));
        // 第一个点距 now 61 秒，已出窗口；断点不导出。
        assert_eq!(h.samples(t + S(61)), vec![(59.0, 10.0), (0.0, 30.0)]);
        assert!(h.samples(t + S(200)).is_empty());
    }

    #[test]
    fn graph_breaks_at_gap_and_maps_to_viewbox() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push(t0, Some(0.0));
        h.push(t0 + S(1), Some(50.0));
        h.push(t0 + S(2), None);
        h.push(t0 + S(30), Some(100.0));
        let (line, area) = h.paths(t0 + S(30), 100.0, (600.0, 100.0), Window::Sec60);
        assert_eq!(line.matches('M').count(), 2, "{line}");
        assert!(line.ends_with("M 600.0 0.0 "), "{line}");
        assert_eq!(area.matches('Z').count(), 2);
    }

    #[test]
    fn history_stats_expose_gaps_and_largest_step() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push(t0, Some(1.0));
        h.push(t0 + S(1), Some(1.0));
        assert_eq!(h.stats(), (2, 0, S(1)));
        h.push(t0 + S(9), None);
        assert_eq!(h.stats(), (3, 1, S(8)));
    }

    #[test]
    fn prefill_fills_every_window_with_bounded_points() {
        let now = Instant::now();
        let mut h = History::default();
        h.prefill(now, 3600);
        for (w, nominal) in [(Window::Sec60, 121), (Window::Min5, 150), (Window::Min10, 150), (Window::Hour1, 180)] {
            let (points, gaps, span) = h.window_shape(now, w);
            assert!(points >= nominal - 3 && points <= nominal + 3, "{w:?}: {points}");
            assert!(span.as_secs_f64() >= w.duration().as_secs_f64() * 0.9, "{w:?}: {span:?}");
            assert_eq!(gaps > 0, w != Window::Sec60 && w != Window::Min5, "{w:?} 缺口 {gaps}");
        }
        println!("一小时填满后单条曲线历史堆内存 {} B", h.heap_bytes());
    }

    #[test]
    fn empty_history_draws_placeholder_in_every_window() {
        let h = History::default();
        for w in Window::ALL {
            let (line, area) = h.paths(Instant::now(), 100.0, (10.0, 10.0), w);
            assert_eq!(line, "M 10.0 10.0");
            assert_eq!(area, line);
            assert_eq!(h.max_in(Instant::now(), w), 0.0);
        }
    }

    // ---- 悬停定位与峰值进程（#81） ----

    fn point(h: &Hover) -> Pt {
        match h {
            Hover::Point(p) => *p,
            Hover::NoData => panic!("应有数据"),
        }
    }

    /// 60 秒窗口：x 位置映射到离鼠标最近的原始采样，数值、距现在多少秒与峰值进程都取自那个采样。
    #[test]
    fn hover_picks_the_nearest_raw_sample_in_the_short_window() {
        let t0 = Instant::now();
        let mut h = History::default();
        for s in 0..=30u64 {
            h.push_with(t0 + S(s), Some(s as f64), 100 + s as u32);
        }
        let now = t0 + S(30);
        // 恰好在某个采样上。
        let p = point(&h.hover(now, Window::Sec60, 10.0));
        assert_eq!((p.max, p.age, p.who), (20.0, 10.0, 120));
        // 取更近的一侧：10.3 秒前离 10 秒前的点更近，10.7 秒前离 11 秒前的点更近。
        assert_eq!(point(&h.hover(now, Window::Sec60, 10.3)).max, 20.0);
        assert_eq!(point(&h.hover(now, Window::Sec60, 10.7)).max, 19.0);
        // 最新的采样；鼠标在右边缘（0 秒前）和最新点之后不远处（采样间隔内）都算它。
        assert_eq!(point(&h.hover(now, Window::Sec60, 0.0)).max, 30.0);
        assert_eq!(point(&h.hover(now + MS(1500), Window::Sec60, 0.0)).max, 30.0);
        // 离最新点太远（采样已经停了很久）没有数据。
        assert_eq!(h.hover(now + S(10), Window::Sec60, 0.0), Hover::NoData);
        // 曲线开始之前（窗口左端，早于第一个采样）没有数据。
        assert_eq!(h.hover(now, Window::Sec60, 55.0), Hover::NoData);
        // 超出窗口范围的鼠标位置按边缘处理，不会 panic。
        assert_eq!(h.hover(now, Window::Sec60, -3.0), h.hover(now, Window::Sec60, 0.0));
        assert_eq!(h.hover(now, Window::Sec60, 999.0), Hover::NoData);
        // 空历史。
        assert_eq!(History::default().hover(now, Window::Sec60, 0.0), Hover::NoData);
    }

    /// 窗口左边缘之外、仅用于连线的那个点不能被悬停选中。
    #[test]
    fn hover_ignores_the_offscreen_point_kept_for_edge_continuity() {
        let t0 = Instant::now();
        let mut h = History::default();
        for s in 0..=100u64 {
            h.push(t0 + S(s), Some(s as f64));
        }
        let now = t0 + S(100);
        // 保留的最老点在 61 秒前之外；窗口最左端（60 秒前）选到窗口内的点。
        let p = point(&h.hover(now, Window::Sec60, 60.0));
        assert!(p.age <= 60.0, "{p:?}");
        assert_eq!(p.max, 40.0);
    }

    /// 长窗口：悬停取曲线上实际绘制的那个桶，数值是桶内最大值（均值另给），位置与路径横坐标一致。
    #[test]
    fn hover_in_long_windows_reads_the_bucket_that_is_drawn() {
        for (window, bucket) in [(Window::Min5, 2u64), (Window::Min10, 4), (Window::Hour1, 20)] {
            let t0 = Instant::now();
            let mut h = History::default();
            for s in 0..=1200u64 {
                // 每 100 秒一个尖峰：0.5 秒间隔的真实采样在此简化成 1 秒。
                h.push_with(t0 + S(s), Some(if s % 100 == 50 { 90.0 } else { 10.0 }), if s % 100 == 50 { 7 } else { 1 });
            }
            let now = t0 + S(1200);
            // 对每个绘制的点：鼠标放在它的横坐标上，悬停读到的就是它自己。
            let span = window.duration().as_secs_f64();
            let mut checked = 0;
            for drawn in h.series(now, window).flatten().filter(|p| p.age <= span) {
                let got = point(&h.hover(now, window, drawn.age));
                assert_eq!(got, drawn, "{window:?}");
                checked += 1;
            }
            assert!(checked > 5, "{window:?}: {checked}");
            // 尖峰所在的桶：最大值 90、均值介于基线与尖峰之间，峰值进程是尖峰采样的进程 7，而不是桶里别的采样。
            let spike_age = 1200.0 - 1150.0;
            let p = point(&h.hover(now, window, spike_age));
            assert_eq!((p.max, p.who), (90.0, 7), "{window:?}");
            assert!(p.mean > 10.0 && p.mean < 90.0, "{window:?}: {}", p.mean);
            assert!(p.age <= spike_age + bucket as f64 && p.age >= spike_age - bucket as f64, "{window:?}: {}", p.age);
        }
    }

    /// 暂停留下的缺口：鼠标在缺口里没有数据，缺口两侧各自读到自己的点，缺口不会把两段“吸”在一起。
    #[test]
    fn hover_in_a_gap_has_no_data_in_every_window() {
        for window in Window::ALL {
            let t0 = Instant::now();
            let mut h = History::default();
            for s in 0..20u64 {
                h.push(t0 + S(s), Some(20.0));
            }
            h.push(t0 + S(20), None);
            for s in 80..100u64 {
                h.push(t0 + S(s), Some(70.0));
            }
            let now = t0 + S(99);
            // 缺口在距现在 19～79 秒之间；窗口够长时取中间 50 秒前。
            assert_eq!(h.hover(now, window, 50.0), Hover::NoData, "{window:?}");
            assert_eq!(h.hover(now, window, 30.0), Hover::NoData, "{window:?}");
            assert_eq!(point(&h.hover(now, window, 5.0)).max, 70.0, "{window:?}");
            if window != Window::Sec60 {
                assert_eq!(point(&h.hover(now, window, 95.0)).max, 20.0, "{window:?}");
            }
        }
    }

    /// 聚合桶里记的是桶内峰值那次采样的进程：之后出现更小的值不换进程，更大的值换进程，并列时保留先到的。
    #[test]
    fn bucket_keeps_the_process_of_its_peak_sample() {
        let t0 = Instant::now();
        let mut h = History::default();
        // 桶宽 2 秒：同一桶里依次来 30(进程 1)、80(进程 2)、50(进程 3)。
        h.push_with(t0, Some(30.0), 1);
        h.push_with(t0 + MS(500), Some(80.0), 2);
        h.push_with(t0 + MS(1000), Some(50.0), 3);
        let now = t0 + MS(1000);
        let p = point(&h.hover(now, Window::Min5, 0.0));
        assert_eq!((p.max, p.who), (80.0, 2));
        assert!((p.mean - 160.0 / 3.0).abs() < 0.01);
        // 并列最大值：保留先到的进程。
        h.push_with(t0 + MS(1500), Some(80.0), 4);
        assert_eq!(point(&h.hover(t0 + MS(1500), Window::Min5, 0.0)).who, 2);
        // 没有进程标识的采样（只采系统指标）先到，之后并列的有标识采样补上标识。
        let mut g = History::default();
        g.push(t0, Some(60.0));
        g.push_with(t0 + MS(500), Some(60.0), 9);
        assert_eq!(point(&g.hover(t0 + MS(500), Window::Min5, 0.0)).who, 9);
        // 原始层的点就是它自己的进程。
        assert_eq!(point(&h.hover(t0 + MS(1500), Window::Sec60, 0.0)).who, 4);
    }

    /// 下一个桶开始后，新桶有自己的峰值进程，旧桶不受影响。
    #[test]
    fn each_bucket_has_its_own_peak_process() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push_with(t0, Some(10.0), 1);
        h.push_with(t0 + S(2), Some(20.0), 2);
        h.push_with(t0 + S(4), Some(30.0), 3);
        let now = t0 + S(4);
        assert_eq!(point(&h.hover(now, Window::Min5, 4.0)).who, 1);
        assert_eq!(point(&h.hover(now, Window::Min5, 2.0)).who, 2);
        assert_eq!(point(&h.hover(now, Window::Min5, 0.0)).who, 3);
    }

    /// 热路径不分配：窗口填满、桶滚动之后，反复写入点时各层缓冲区容量不再增长。
    #[test]
    fn push_with_process_ids_does_not_grow_buffers_once_full() {
        let t0 = Instant::now();
        let mut h = History::default();
        for s in 0..8000u64 {
            h.push_with(t0 + S(s), Some((s % 100) as f64), (s % 7) as u32);
        }
        let bytes = h.heap_bytes();
        for s in 8000..16000u64 {
            h.push_with(t0 + S(s), Some((s % 100) as f64), (s % 7) as u32);
        }
        assert_eq!(h.heap_bytes(), bytes);
    }
}

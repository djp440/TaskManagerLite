//! 数值格式化与资源曲线路径生成。

use std::collections::VecDeque;
use std::fmt::Write;
use std::time::{Duration, Instant};

/// 资源曲线显示的时间窗口。
pub const WINDOW: Duration = Duration::from_secs(60);

pub fn percent(v: f32) -> String {
    if v < 0.05 { "0%".into() } else { format!("{v:.1}%") }
}

pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
}

pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

pub fn rate(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    match bytes_per_sec {
        v if v < 0.05 * K * K => format!("{:.0} KB/s", v / K).replace("-0", "0"),
        v if v < K * K * K => format!("{:.1} MB/s", v / K / K),
        v => format!("{:.2} GB/s", v / K / K / K),
    }
}

/// 进程列表里 MB/s 精度的磁盘速率，与原生任务管理器一致。
pub fn disk_rate(bytes_per_sec: f64) -> String {
    let mb = bytes_per_sec / 1024.0 / 1024.0;
    if mb < 0.05 { "0 MB/s".into() } else { format!("{mb:.1} MB/s") }
}

/// 一类资源最近一段时间的取值。`None` 表示采样中断（窗口不可见或暂停），曲线在此断开。
#[derive(Default)]
pub struct History {
    points: VecDeque<(Instant, Option<f64>)>,
}

impl History {
    pub fn push(&mut self, at: Instant, value: Option<f64>) {
        self.points.push_back((at, value));
        // 保留一个窗口外的点，让曲线从左边缘连续进入。
        while self.points.len() > 2 && at.duration_since(self.points[1].0) > WINDOW {
            self.points.pop_front();
        }
    }

    /// 供日志核对曲线是否连续：(点数, 断点数, 相邻记录之间的最大间隔)。
    pub fn stats(&self) -> (usize, usize, Duration) {
        let gaps = self.points.iter().filter(|p| p.1.is_none()).count();
        let step = self.points.iter().zip(self.points.iter().skip(1)).map(|(a, b)| b.0.duration_since(a.0)).max();
        (self.points.len(), gaps, step.unwrap_or_default())
    }

    pub fn max(&self) -> f64 {
        self.points.iter().filter_map(|p| p.1).fold(0.0, f64::max)
    }

    /// 按图表的实际尺寸（逻辑像素）生成 (折线, 填充区域) 两条 SVG 路径；`scale` 对应图表顶端的取值。
    /// Slint 的 Path 会等比缩放 viewbox，所以坐标必须与图表尺寸一致，不能用固定坐标系。
    pub fn paths(&self, now: Instant, scale: f64, (view_w, view_h): (f64, f64)) -> (String, String) {
        let mut segments: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        for &(at, value) in &self.points {
            match value {
                Some(v) => {
                    let age = now.saturating_duration_since(at).as_secs_f64();
                    let x = view_w * (1.0 - age / WINDOW.as_secs_f64());
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

/// 自动量程：向上取到 1、2、5 × 10ⁿ，最小 `floor`。
pub fn nice_scale(max: f64, floor: f64) -> f64 {
    let v = max.max(floor);
    let exp = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * exp).find(|&s| s >= v).unwrap_or(10.0 * exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_native_task_manager() {
        assert_eq!(percent(0.01), "0%");
        assert_eq!(percent(12.345), "12.3%");
        assert_eq!(megabytes(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(disk_rate(0.0), "0 MB/s");
        assert_eq!(disk_rate(2.0 * 1024.0 * 1024.0), "2.0 MB/s");
        assert_eq!(rate(10.0 * 1024.0), "10 KB/s");
        assert_eq!(rate(3.5 * 1024.0 * 1024.0), "3.5 MB/s");
    }

    #[test]
    fn graph_breaks_at_gap_and_maps_to_viewbox() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push(t0, Some(0.0));
        h.push(t0 + Duration::from_secs(1), Some(50.0));
        h.push(t0 + Duration::from_secs(2), None);
        h.push(t0 + Duration::from_secs(30), Some(100.0));
        let (line, area) = h.paths(t0 + Duration::from_secs(30), 100.0, (600.0, 100.0));
        assert_eq!(line.matches('M').count(), 2, "{line}");
        assert!(line.ends_with("M 600.0 0.0 "), "{line}");
        assert_eq!(area.matches('Z').count(), 2);
    }

    #[test]
    fn history_stats_expose_gaps_and_largest_step() {
        let t0 = Instant::now();
        let mut h = History::default();
        h.push(t0, Some(1.0));
        h.push(t0 + Duration::from_secs(1), Some(1.0));
        assert_eq!(h.stats(), (2, 0, Duration::from_secs(1)));
        h.push(t0 + Duration::from_secs(9), None);
        assert_eq!(h.stats(), (3, 1, Duration::from_secs(8)));
    }

    #[test]
    fn history_drops_points_outside_window() {
        let t0 = Instant::now();
        let mut h = History::default();
        for s in 0..200 {
            h.push(t0 + Duration::from_secs(s), Some(1.0));
        }
        assert!(h.points.len() <= 62);
    }

    #[test]
    fn nice_scale_rounds_up() {
        assert_eq!(nice_scale(0.0, 1000.0), 1000.0);
        assert_eq!(nice_scale(130.0, 1.0), 200.0);
        assert_eq!(nice_scale(4.2e6, 1.0), 5e6);
    }
}

//! 数值格式化。资源曲线的历史与路径生成在 `history.rs`。

pub use crate::history::History;

pub fn percent(v: f32) -> String {
    if v < 0.05 { "0%".into() } else { format!("{v:.1}%") }
}

/// 进程 GPU 占用；没有读数（未显示 GPU 列、无 GPU 或计数器不可用）时为“—”，有计数器但无负载为“0%”。
pub fn gpu_percent(v: Option<f32>) -> String {
    v.map_or_else(|| "—".into(), percent)
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
    fn nice_scale_rounds_up() {
        assert_eq!(nice_scale(0.0, 1000.0), 1000.0);
        assert_eq!(nice_scale(130.0, 1.0), 200.0);
        assert_eq!(nice_scale(4.2e6, 1.0), 5e6);
    }
}

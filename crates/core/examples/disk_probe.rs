//! 磁盘速率探针：每秒采样一次，输出每个进程的磁盘速率 CSV（`t,source,pid,name,bytes_per_sec`）。
//! 用于验收脚本与资源监视器对比。提权运行时 `source` 为 `etw`，否则（或带 `--no-etw`）为 `approx`。
//!
//! ```text
//! cargo run -p tml-core --release --example disk_probe -- [--secs 20] [--no-etw] [--pid N]
//! ```
//! `--pid` 只输出该进程（速率为 0 也输出）；否则输出速率大于 0 的全部进程。

use std::time::{Duration, Instant};

use tml_core::{DiskSource, Sampler};

fn main() {
    let _guard = tml_core::TraceGuard;
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let secs: u64 = value("--secs").and_then(|v| v.parse().ok()).unwrap_or(20);
    let pid: Option<u32> = value("--pid").and_then(|v| v.parse().ok());

    let mut sampler = Sampler::default();
    if args.iter().any(|a| a == "--no-etw") {
        sampler.disable_etw();
    }
    println!("t,source,pid,name,bytes_per_sec");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        let sample = sampler.sample();
        let source = if sample.disk_source == DiskSource::Etw { "etw" } else { "approx" };
        let t = start.elapsed().as_secs_f64();
        for p in &sample.snapshot.processes {
            if pid.map_or(p.disk_rate > 0.0, |x| x == p.key.pid) {
                println!("{t:.1},{source},{},{},{:.0}", p.key.pid, p.name, p.disk_rate);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

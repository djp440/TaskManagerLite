//! 进程级 GPU 占用探针：开启 `Sampler::track_gpu_processes`，每秒输出一次 `Process::gpu_usage`。
//! 用于验收时和原生任务管理器的“GPU”列对比，也可在无 GPU 的机器上确认输出为 `None`（“—”）。
//!
//! ```text
//! cargo run -p tml-core --release --example process_gpu_probe -- [--secs 10] [--pid N] [--top 8]
//! ```
//! 输出 CSV `t,pid,name,gpu`：`gpu` 为百分比，没有读数时为 `none`。
//! `--pid` 只输出该进程（0 也输出）；否则每秒输出 GPU 占用最高的前 `--top` 个进程。
//! 开头几行（探针尚未就绪）读数为 `none` 属正常，之后无负载的进程应为 `0.0` 而不是 `none`。

use std::time::{Duration, Instant};

use tml_core::Sampler;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let secs: u64 = value("--secs").and_then(|v| v.parse().ok()).unwrap_or(10);
    let pid: Option<u32> = value("--pid").and_then(|v| v.parse().ok());
    let top: usize = value("--top").and_then(|v| v.parse().ok()).unwrap_or(8);

    let mut sampler = Sampler::default();
    sampler.track_gpu_processes(true);
    println!("t,pid,name,gpu");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        let sample = sampler.sample();
        let t = start.elapsed().as_secs_f64();
        let show = |p: &tml_core::Process| {
            let gpu = p.gpu_usage.map_or_else(|| "none".to_string(), |v| format!("{v:.1}"));
            println!("{t:.1},{},{},{gpu}", p.key.pid, p.name);
        };
        match pid {
            Some(pid) => sample.snapshot.processes.iter().filter(|p| p.key.pid == pid).for_each(show),
            None => {
                let mut busy: Vec<_> = sample.snapshot.processes.iter().filter(|p| p.gpu_usage.is_some_and(|v| v > 0.0)).collect();
                busy.sort_by(|a, b| b.gpu_usage.partial_cmp(&a.gpu_usage).unwrap());
                busy.into_iter().take(top).for_each(show);
                // 没有任何进程有读数：计数器不可用或尚未就绪。
                if sample.snapshot.processes.iter().all(|p| p.gpu_usage.is_none()) {
                    println!("{t:.1},-,(全部进程),none");
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

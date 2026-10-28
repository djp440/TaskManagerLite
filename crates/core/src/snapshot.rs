//! 进程快照：把两次原始采样做差，得到 CPU 占用率与磁盘速率。

use std::collections::HashMap;
use std::sync::Arc;

use crate::details::{Counters, StaticInfo};

use crate::nt::RawProcess;

/// 进程身份。PID 会被复用，必须配合创建时间才能唯一确定一个进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    pub create_time: u64,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub key: ProcessKey,
    pub parent_pid: u32,
    pub name: String,
    /// 占整机全部逻辑处理器的百分比，0–100。
    pub cpu_usage: f32,
    pub private_working_set: u64,
    /// 字节/秒。
    pub disk_rate: f64,
    pub critical: bool,
    /// “详细信息”列的计数，随采样更新。
    pub counters: Counters,
    /// “详细信息”列的静态信息。采样线程不读取，始终为 `None`；界面线程在后台读到后填入，用于显示和排序。
    pub info: Option<Arc<StaticInfo>>,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessSnapshot {
    pub processes: Vec<Process>,
}

/// 上一次采样留下的计数器，用来做差。
#[derive(Default)]
pub struct Baseline {
    counters: HashMap<ProcessKey, (u64, u64)>,
}

impl Baseline {
    /// `cpu_total_delta`：整机所有逻辑处理器在这段时间内的总可用时间（100ns），
    /// `elapsed_secs`：两次采样的间隔。首次采样（无基线）时占用率与速率为 0。
    pub fn advance(
        &mut self,
        raw: Vec<RawProcess>,
        cpu_total_delta: u64,
        elapsed_secs: f64,
        is_critical: &mut dyn FnMut(&RawProcess) -> bool,
    ) -> ProcessSnapshot {
        let mut next = HashMap::with_capacity(raw.len());
        let mut processes = Vec::with_capacity(raw.len());
        for r in raw {
            // System Idle Process 的“CPU 时间”其实是空闲时间，不进入进程列表。
            if r.pid == 0 {
                continue;
            }
            let key = ProcessKey { pid: r.pid, create_time: r.create_time };
            let (cpu_usage, disk_rate) = match self.counters.get(&key) {
                Some(&(cpu, io)) => (
                    if cpu_total_delta > 0 {
                        (r.cpu_time.saturating_sub(cpu) as f64 / cpu_total_delta as f64 * 100.0).min(100.0) as f32
                    } else {
                        0.0
                    },
                    if elapsed_secs > 0.0 { r.io_bytes.saturating_sub(io) as f64 / elapsed_secs } else { 0.0 },
                ),
                None => (0.0, 0.0),
            };
            next.insert(key, (r.cpu_time, r.io_bytes));
            let critical = is_critical(&r);
            let counters = Counters {
                session_id: r.session_id,
                base_priority: r.base_priority,
                handles: r.handles,
                threads: r.threads,
                working_set: r.working_set,
                peak_working_set: r.peak_working_set,
                commit: r.commit,
                cpu_time: r.cpu_time,
                suspended: r.suspended,
            };
            processes.push(Process {
                key,
                parent_pid: r.parent_pid,
                name: r.name,
                cpu_usage,
                private_working_set: r.private_working_set,
                disk_rate,
                critical,
                counters,
                info: None,
            });
        }
        self.counters = next;
        ProcessSnapshot { processes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: u32, create_time: u64, cpu_time: u64, io_bytes: u64) -> RawProcess {
        RawProcess {
            pid,
            parent_pid: 0,
            name: format!("p{pid}"),
            create_time,
            cpu_time,
            private_working_set: 0,
            io_bytes,
            session_id: 1,
            base_priority: 8,
            handles: 0,
            threads: 1,
            working_set: 0,
            peak_working_set: 0,
            commit: 0,
            suspended: false,
        }
    }

    #[test]
    fn first_sample_has_zero_usage_and_idle_is_dropped() {
        let mut b = Baseline::default();
        let s = b.advance(vec![raw(0, 0, 999, 0), raw(4, 1, 100, 100)], 1000, 1.0, &mut |_| false);
        assert_eq!(s.processes.len(), 1);
        assert_eq!(s.processes[0].cpu_usage, 0.0);
    }

    #[test]
    fn usage_is_share_of_whole_machine() {
        let mut b = Baseline::default();
        b.advance(vec![raw(10, 5, 1_000, 0)], 0, 0.0, &mut |_| false);
        // 12 个逻辑处理器 × 1 秒 = 120_000_000 个 100ns；进程用掉一个处理器的整秒。
        let s = b.advance(vec![raw(10, 5, 1_000 + 10_000_000, 4096)], 120_000_000, 2.0, &mut |_| false);
        let p = &s.processes[0];
        assert!((p.cpu_usage - 100.0 / 12.0).abs() < 0.01);
        assert_eq!(p.disk_rate, 2048.0);
    }

    #[test]
    fn reused_pid_does_not_inherit_old_counters() {
        let mut b = Baseline::default();
        b.advance(vec![raw(10, 5, 50_000_000, 0)], 0, 0.0, &mut |_| false);
        // 同一 PID、不同创建时间：是新进程，不能和旧进程做差。
        let s = b.advance(vec![raw(10, 9, 1_000, 0)], 120_000_000, 1.0, &mut |_| false);
        assert_eq!(s.processes[0].cpu_usage, 0.0);
    }
}

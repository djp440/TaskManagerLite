//! 采样器：每调用一次 `sample` 生成一个进程快照和一组系统级指标。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::apps::app_pids;
use crate::control::is_critical;
use crate::cpu::{CpuProbe, CpuSample};
use crate::diskio::rates;
use crate::etw::{DiskTrace, TraceState};
use crate::memory::{MemoryHardware, MemoryStatus, read_hardware, read_status};
use crate::nt::ProcessQuery;
use crate::snapshot::{Baseline, ProcessKey, ProcessSnapshot};
use crate::system::{CpuTimes, DiskCounters, SystemMetrics, cpu_times, network_octets};

/// 进程磁盘速率的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskSource {
    /// 进程全部 I/O 字节数的差值（含管道、网络等非磁盘 I/O），偏高。
    #[default]
    Approximate,
    /// ETW 磁盘事件按发起进程累计，与原生任务管理器同源。
    Etw,
}

pub struct Sample {
    pub snapshot: ProcessSnapshot,
    pub system: SystemMetrics,
    /// 应用进程的 PID（见 [`crate::apps`]）；未开启 [`Sampler::track_apps`] 时为 `None`。
    pub app_pids: Option<HashSet<u32>>,
    pub cpu: CpuSample,
    /// 内存详情（组成、提交、内核池）。
    pub memory: MemoryStatus,
    /// 内存硬件信息（SMBIOS），只在读取完成的那次采样里出现一次。
    pub memory_hardware: Option<MemoryHardware>,
    /// 本次 `snapshot` 里 `disk_rate` 的来源。
    pub disk_source: DiskSource,
}

#[derive(Default)]
pub struct Sampler {
    query: ProcessQuery,
    baseline: Baseline,
    cpu: CpuTimes,
    net: Option<(u64, u64)>,
    last: Option<Instant>,
    disk: Option<DiskCounters>,
    disk_tried: bool,
    hardware_tried: bool,
    critical: HashMap<ProcessKey, bool>,
    track_apps: bool,
    cpu_probe: CpuProbe,
    etw: Option<DiskTrace>,
    etw_tried: bool,
    /// 上一次采样时 ETW 已在运行：此时累计窗口与采样间隔一致，数据可用。
    etw_ready: bool,
    etw_disabled: bool,
}

impl Sampler {
    /// 开启后每次采样都枚举窗口，找出应用进程（仅分组视图需要）。
    pub fn track_apps(&mut self, on: bool) {
        self.track_apps = on;
    }

    /// 不使用 ETW 磁盘事件，始终用近似速率（对比测量用）。
    pub fn disable_etw(&mut self) {
        self.etw_disabled = true;
    }

    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();
        let elapsed = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);

        let cpu = cpu_times();
        let first = self.cpu.total == 0;
        let total_delta = if first { 0 } else { cpu.total.saturating_sub(self.cpu.total) };
        let idle_delta = cpu.idle.saturating_sub(self.cpu.idle);
        self.cpu = cpu;
        let cpu_usage =
            if total_delta > 0 { (100.0 * (1.0 - idle_delta as f64 / total_delta as f64)).clamp(0.0, 100.0) as f32 } else { 0.0 };

        let raw = self.query.query().unwrap_or_default();
        let mut seen = HashMap::with_capacity(raw.len());
        let cache = &mut self.critical;
        let mut snapshot = self.baseline.advance(raw, total_delta, elapsed, &mut |r| {
            let key = ProcessKey { pid: r.pid, create_time: r.create_time };
            let c = *cache.entry(key).or_insert_with(|| is_critical(r));
            seen.insert(key, c);
            c
        });
        // 只保留仍在运行的进程，避免缓存无限增长。
        self.critical = seen;

        let disk_source = self.apply_etw_disk(first, &mut snapshot, elapsed);

        let cpu_detail = self.cpu_probe.sample(first, self.query.totals);

        // 内存只读一次，SystemMetrics 与 MemoryStatus 共用同一份数据。
        let memory_status = read_status(self.query.compressed_working_set);
        let (memory_used, memory_total) = (memory_status.in_use(), memory_status.total);
        // SMBIOS 解析稍慢，放在首次采样之后，避免拖慢第一个快照。
        let memory_hardware = (!first && !std::mem::replace(&mut self.hardware_tried, true)).then(read_hardware);

        // PDH 初始化稍慢，放在首次采样之后，避免拖慢第一个快照。
        let (disk_active, disk_read, disk_write) = match &mut self.disk {
            Some(d) => d.read(),
            None => {
                if !first && !self.disk_tried {
                    self.disk_tried = true;
                    self.disk = DiskCounters::open();
                }
                (0.0, 0.0, 0.0)
            }
        };

        let (mut net_recv, mut net_send) = (0.0, 0.0);
        if let Some(cur) = network_octets() {
            if let Some(prev) = self.net
                && elapsed > 0.0
            {
                net_recv = cur.0.saturating_sub(prev.0) as f64 / elapsed;
                net_send = cur.1.saturating_sub(prev.1) as f64 / elapsed;
            }
            self.net = Some(cur);
        }

        let app_pids = self.track_apps.then(app_pids);

        Sample {
            snapshot,
            app_pids,
            system: SystemMetrics {
                cpu_usage,
                memory_used,
                memory_total,
                disk_active,
                disk_read,
                disk_write,
                net_recv,
                net_send,
            },
            cpu: cpu_detail,
            memory: memory_status,
            memory_hardware,
            disk_source,
        }
    }

    /// 有 ETW 会话时用按进程归属的磁盘字节数覆盖近似速率。会话在首次采样之后才启动，
    /// 并且需要管理员权限；未提权、启动中或失败时保持近似值。
    fn apply_etw_disk(&mut self, first: bool, snapshot: &mut ProcessSnapshot, elapsed: f64) -> DiskSource {
        if !first && !self.etw_tried && !self.etw_disabled {
            self.etw_tried = true;
            self.etw = DiskTrace::start().ok();
        }
        let Some(trace) = &self.etw else { return DiskSource::Approximate };
        match trace.state() {
            TraceState::Starting => DiskSource::Approximate,
            TraceState::Failed(_) => {
                self.etw = None;
                self.etw_ready = false;
                DiskSource::Approximate
            }
            TraceState::Running => {
                let bytes = trace.take();
                // 刚就绪的这一次，累计窗口只有半截，丢弃；下一次起窗口与采样间隔一致。
                if !std::mem::replace(&mut self.etw_ready, true) {
                    return DiskSource::Approximate;
                }
                let rate = rates(&bytes, elapsed);
                for p in &mut snapshot.processes {
                    p.disk_rate = rate.get(&p.key.pid).copied().unwrap_or(0.0);
                }
                DiskSource::Etw
            }
        }
    }
}

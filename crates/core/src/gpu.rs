//! GPU PDH 采样。按物理引擎先跨进程求和，再取最忙引擎作为整卡利用率。
use std::collections::{BTreeMap, HashMap};
use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};

#[derive(Clone, Debug, Default)]
pub struct Gpu {
    pub id: String,
    pub name: String,
    pub dedicated_total: u64,
    pub shared_total: u64,
    pub usage: Option<f64>,
    pub dedicated: Option<f64>,
    pub shared: Option<f64>,
    pub engines: Vec<(String, f64)>,
}

// DXGI 是设备清单的来源；PDH 还可能包含软件渲染器等实例，不能直接拿来生成卡片。
fn adapters() -> Option<Vec<Gpu>> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut result = Vec::new();
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break; };
            let Ok(desc) = adapter.GetDesc1() else { continue; };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 { continue; }
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            result.push(Gpu {
                id: format!("gpu:luid_0x{:08X}_0x{:08X}:0", desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart),
                name: String::from_utf16_lossy(&desc.Description[..end]),
                dedicated_total: desc.DedicatedVideoMemory as u64,
                shared_total: desc.SharedSystemMemory as u64,
                ..Default::default()
            });
        }
        Some(result)
    }
}

fn attach_readings(adapters: &[Gpu], readings: Vec<Gpu>) -> Vec<Gpu> {
    // 取交集：排除软件 PDH 实例，以及 DXGI 中没有活动 PDH 实例的重复/失效适配器。
    // 不能按名称去重，同型号的多张物理显卡应当各自显示。
    adapters.iter().filter_map(|adapter| {
        let mut g = readings.iter().find(|g| g.id == adapter.id).cloned()?;
        g.name = adapter.name.clone();
        g.dedicated_total = adapter.dedicated_total;
        g.shared_total = adapter.shared_total;
        Some(g)
    }).collect()
}

fn identity(name: &str) -> Option<(&str, &str)> {
    let name = name.get(name.find("luid_")?..)?;
    let (luid, rest) = name.split_once("_phys_")?;
    let end = rest.find('_').unwrap_or(rest.len());
    Some((luid, &rest[..end]))
}

pub(crate) struct GpuProbe {
    query: PDH_HQUERY,
    counters: [PDH_HCOUNTER; 3],
    ready: bool,
    adapters: Vec<Gpu>,
    refreshed: std::time::Instant,
}
unsafe impl Send for GpuProbe {}
impl Drop for GpuProbe {
    fn drop(&mut self) { unsafe { PdhCloseQuery(self.query); } }
}
impl GpuProbe {
    pub fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 { return None; }
            let mut this = Self { query, counters: Default::default(), ready: false,
                adapters: adapters().unwrap_or_default(), refreshed: std::time::Instant::now() };
            for (c, path) in this.counters.iter_mut().zip([
                r"\GPU Engine(*)\Utilization Percentage",
                r"\GPU Adapter Memory(*)\Dedicated Usage",
                r"\GPU Adapter Memory(*)\Shared Usage",
            ]) {
                let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
                if PdhAddEnglishCounterW(query, PCWSTR(wide.as_ptr()), 0, c) != 0 { return None; }
            }
            PdhCollectQueryData(query);
            Some(this)
        }
    }
    /// 采一次样。`per_process` 为真时，顺带从同一份 `GPU Engine` 读数里按 PID 聚合进程级占用
    /// （见 [`process_usage`]）；为假时不做聚合，返回 `None`，不增加任何开销。
    pub fn sample(&mut self, per_process: bool) -> (Vec<Gpu>, Option<ProcessUsage>) {
        if self.refreshed.elapsed().as_secs() >= 5 {
            if let Some(adapters) = adapters() { self.adapters = adapters; }
            self.refreshed = std::time::Instant::now();
        }
        unsafe {
            if PdhCollectQueryData(self.query) != 0 { return (attach_readings(&self.adapters, vec![]), None); }
            let ready = std::mem::replace(&mut self.ready, true);
            let arrays: Vec<_> = self.counters.iter().map(|c| {
                let (mut bytes, mut count) = (0, 0);
                PdhGetFormattedCounterArrayW(*c, PDH_FMT_DOUBLE, &mut bytes, &mut count, None);
                if bytes == 0 { return vec![]; }
                let mut buf = vec![0u64; (bytes as usize).div_ceil(8)];
                if PdhGetFormattedCounterArrayW(*c, PDH_FMT_DOUBLE, &mut bytes, &mut count, Some(buf.as_mut_ptr().cast())) != 0 { return vec![]; }
                std::slice::from_raw_parts(buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(), count as usize)
                    .iter().filter_map(|v| {
                        let value = v.FmtValue.Anonymous.doubleValue;
                        (v.FmtValue.CStatus <= 1 && value.is_finite() && value >= 0.0)
                            .then(|| v.szName.to_string().ok().map(|n| (n, value))).flatten()
                    }).collect()
            }).collect();
            let processes = if per_process && ready { process_usage(&arrays[0]) } else { None };
            (attach_readings(&self.adapters, aggregate(&arrays, ready)), processes)
        }
    }
}

/// 各进程的 GPU 占用，百分比（0–100），键为 PID。只含当前有 GPU 引擎实例的进程；
/// 不在表里的进程没有任何 GPU 上下文，视为 0。
pub(crate) type ProcessUsage = HashMap<u32, f32>;

/// 从实例名 `pid_<PID>_luid_..._phys_..._eng_..._engtype_...` 取出 PID 和 PID 之后的引擎标识。
/// 名字不符合格式时返回 `None`（调用方忽略该实例，不报错）。
fn split_pid(name: &str) -> Option<(u32, &str)> {
    let (pid, engine) = name.strip_prefix("pid_")?.split_once('_')?;
    // PID 只接受纯数字：`parse` 会放过 "+5"。
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) { return None; }
    Some((pid.parse().ok()?, engine))
}

/// 把聚合结果写到快照的每个进程上。`usage` 为 `None`（计数器不可用或尚无有效基线）时保持 `None`（显示“—”）；
/// 否则不在表里的进程记为 0。
pub(crate) fn apply_usage(processes: &mut [crate::snapshot::Process], usage: Option<&ProcessUsage>) {
    for p in processes {
        p.gpu_usage = usage.map(|u| u.get(&p.key.pid).copied().unwrap_or(0.0));
    }
}

/// 按 PID 聚合 `GPU Engine\Utilization Percentage` 的读数：每个进程取它在所有 GPU 引擎上的最大利用率。
/// 同一进程在同一引擎上的多个实例（正常不会出现）先相加，再跨引擎、跨显卡取最大值，并限制在 0–100。
/// 没有任何可解析的实例（计数器不可用、无 GPU 或格式不认识）时返回 `None`。
fn process_usage(rows: &[(String, f64)]) -> Option<ProcessUsage> {
    let mut per_engine = HashMap::<(u32, &str), f64>::with_capacity(rows.len());
    for (name, value) in rows {
        let Some((pid, engine)) = split_pid(name) else { continue; };
        *per_engine.entry((pid, engine)).or_default() += value;
    }
    if per_engine.is_empty() { return None; }
    let mut usage = ProcessUsage::new();
    for ((pid, _), value) in per_engine {
        let value = value.clamp(0.0, 100.0) as f32;
        let slot = usage.entry(pid).or_insert(0.0);
        *slot = slot.max(value);
    }
    Some(usage)
}

fn aggregate(arrays: &[Vec<(String, f64)>], ready: bool) -> Vec<Gpu> {
    let mut gpus = BTreeMap::<String, Gpu>::new();
    let mut engines = BTreeMap::<(String, String, String), f64>::new();
    for (index, values) in arrays.iter().enumerate() {
        for (name, value) in values {
            let Some((luid, phys)) = identity(name) else { continue; };
            let id = format!("gpu:{luid}:{phys}");
            let gpu = gpus.entry(id.clone()).or_insert_with(|| Gpu { id: id.clone(), ..Default::default() });
            match index {
                0 if ready => {
                    let Some((_, engine)) = name.split_once("_eng_") else { continue; };
                    let Some((number, kind)) = engine.split_once("_engtype_") else { continue; };
                    *engines.entry((id, number.into(), kind.into())).or_default() += value;
                }
                1 => gpu.dedicated = Some(value + gpu.dedicated.unwrap_or(0.0)),
                2 => gpu.shared = Some(value + gpu.shared.unwrap_or(0.0)),
                _ => {}
            }
        }
    }
    for ((id, _, kind), value) in engines {
        let gpu = gpus.get_mut(&id).unwrap();
        let value = value.clamp(0.0, 100.0);
        gpu.usage = Some(gpu.usage.unwrap_or(0.0).max(value));
        if let Some(e) = gpu.engines.iter_mut().find(|e| e.0 == kind) { e.1 = e.1.max(value); }
        else { gpu.engines.push((kind, value)); }
    }
    gpus.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardware_inventory_filters_extra_pdh_instances_and_preserves_capacity() {
        let adapters = vec![Gpu { id: "integrated".into(), name: "核显".into(), shared_total: 24 << 30, ..Default::default() },
            Gpu { id: "discrete".into(), name: "独显".into(), dedicated_total: 8 << 30, ..Default::default() },
            Gpu { id: "stale".into(), name: "独显".into(), ..Default::default() }];
        let readings = vec![Gpu { id: "software".into(), usage: Some(1.0), ..Default::default() },
            Gpu { id: "integrated".into(), ..Default::default() },
            Gpu { id: "discrete".into(), usage: Some(60.0), dedicated: Some(4e9), ..Default::default() }];
        let result = attach_readings(&adapters, readings);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "核显");
        assert_eq!(result[0].shared_total, 24 << 30);
        assert_eq!(result[0].usage, None);
        assert_eq!(result[1].dedicated_total, 8 << 30);
        assert_eq!(result[1].usage, Some(60.0));
    }
    #[test]
    fn sums_processes_per_engine_but_does_not_sum_independent_engines() {
        let rows = vec![
            ("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D".into(), 40.0),
            ("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3D".into(), 30.0),
            ("pid_2_luid_0x0_0x1_phys_0_eng_1_engtype_Copy".into(), 60.0),
            ("pid_2_luid_0x0_0x2_phys_0_eng_0_engtype_3D".into(), 20.0),
        ];
        let gpus = aggregate(&[rows.clone()], true);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].usage, Some(70.0));
        assert_eq!(gpus[1].usage, Some(20.0));
        assert_eq!(aggregate(&[rows], false)[0].usage, None);
    }

    fn rows(items: &[(&str, f64)]) -> Vec<(String, f64)> {
        items.iter().map(|&(n, v)| (n.to_string(), v)).collect()
    }

    #[test]
    fn splits_pid_from_instance_name() {
        assert_eq!(split_pid("pid_1234_luid_0x0_0x1_phys_0_eng_0_engtype_3D"), Some((1234, "luid_0x0_0x1_phys_0_eng_0_engtype_3D")));
        assert_eq!(split_pid("pid_0_luid_x"), Some((0, "luid_x")));
    }

    #[test]
    fn malformed_instance_names_are_ignored_without_panic() {
        for bad in ["", "pid_", "pid_12", "pid__luid", "pid_x_luid_1", "pid_-5_luid", "pid_+5_luid", "pid_99999999999_luid_1",
            "luid_0x0_0x1_phys_0", "_total", "pid_1_", "中文实例名", "pid_中_luid"] {
            // 只要求不 panic；能解析出来的（如 "pid_1_"）PID 也必须是合法数字。
            if let Some((pid, _)) = split_pid(bad) { assert!(pid == 1, "{bad}"); }
        }
        assert_eq!(split_pid("pid_12"), None);
        assert_eq!(split_pid("pid_+5_luid"), None);
        assert_eq!(split_pid("pid_99999999999_luid_1"), None);
        // 全是坏名字：当作计数器不可用。
        assert_eq!(process_usage(&rows(&[("garbage", 5.0), ("pid_x_luid", 5.0)])), None);
    }

    #[test]
    fn process_takes_max_over_engines_and_gpus() {
        let usage = process_usage(&rows(&[
            ("pid_7_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
            ("pid_7_luid_0x0_0x1_phys_0_eng_1_engtype_Copy", 55.0),
            ("pid_7_luid_0x0_0x2_phys_0_eng_0_engtype_3D", 80.0),
            ("pid_8_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 12.5),
            ("pid_9_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 0.0),
        ])).unwrap();
        // 不跨引擎相加：取最大值。
        assert_eq!(usage[&7], 80.0);
        assert_eq!(usage[&8], 12.5);
        assert_eq!(usage[&9], 0.0);
        assert_eq!(usage.len(), 3);
    }

    #[test]
    fn same_engine_instances_add_then_clamp() {
        // 同 PID、同引擎的重复实例相加；不同引擎不相加；结果限制在 100。
        let usage = process_usage(&rows(&[
            ("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 60.0),
            ("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 70.0),
            ("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
            ("pid_2_luid_0x0_0x1_phys_0_eng_1_engtype_3D", 30.0),
            ("pid_3_luid_0x0_0x1_phys_0_eng_0_engtype_3D", -4.0),
        ])).unwrap();
        assert_eq!(usage[&1], 100.0);
        assert_eq!(usage[&2], 30.0);
        assert_eq!(usage[&3], 0.0);
    }

    #[test]
    fn no_rows_means_unavailable() {
        assert_eq!(process_usage(&[]), None);
    }

    #[test]
    fn apply_marks_missing_as_zero_and_unavailable_as_none() {
        use crate::snapshot::{Process, ProcessKey};
        let mk = |pid| Process {
            key: ProcessKey { pid, create_time: 1 }, parent_pid: 0, name: "p".into(), cpu_usage: 0.0, private_working_set: 0,
            disk_rate: 0.0, gpu_usage: Some(99.0), eco_mode: None, critical: false, counters: Default::default(), info: None,
        };
        let mut procs = vec![mk(7), mk(8)];
        let usage: ProcessUsage = [(7, 42.0)].into_iter().collect();
        apply_usage(&mut procs, Some(&usage));
        assert_eq!((procs[0].gpu_usage, procs[1].gpu_usage), (Some(42.0), Some(0.0)));
        apply_usage(&mut procs, None);
        assert_eq!((procs[0].gpu_usage, procs[1].gpu_usage), (None, None));
    }
}

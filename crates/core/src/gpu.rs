//! GPU PDH 采样。按物理引擎先跨进程求和，再取最忙引擎作为整卡利用率。
use std::collections::BTreeMap;
use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;

#[derive(Clone, Debug, Default)]
pub struct Gpu {
    pub id: String,
    pub usage: Option<f64>,
    pub dedicated: Option<f64>,
    pub shared: Option<f64>,
    pub engines: Vec<(String, f64)>,
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
            let mut this = Self { query, counters: Default::default(), ready: false };
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
    pub fn sample(&mut self) -> Vec<Gpu> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 { return vec![]; }
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
            aggregate(&arrays, ready)
        }
    }
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
}

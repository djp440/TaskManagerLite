//! CPU 详情在真实系统上的验收：拓扑与 WMI 对照（只在测试里用 WMI）、单核满载。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tml_core::{Sampler, read_cpu_info};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

#[test]
fn sample_carries_cpu_detail() {
    let mut s = Sampler::default();
    let first = s.sample();
    assert!(first.cpu.info.is_none(), "静态信息应推迟到首个快照之后读取");
    assert!(first.cpu.per_core.iter().all(|&u| u == 0.0));
    std::thread::sleep(Duration::from_millis(500));
    let second = s.sample();

    let logical = std::thread::available_parallelism().unwrap().get();
    assert_eq!(second.cpu.per_core.len(), logical);
    assert!(second.cpu.per_core.iter().all(|u| (0.0..=100.0).contains(u)));
    assert!(second.cpu.info.is_some(), "第二次采样应带上静态信息");
    assert!(second.cpu.threads as usize > second.snapshot.processes.len(), "线程数应多于进程数");
    assert!(second.cpu.handles > 100);
    assert!(second.cpu.uptime_secs > 0);

    // 频率计数器在第二次采样时才打开，第三次起应有数值。
    std::thread::sleep(Duration::from_millis(500));
    let third = s.sample();
    let mhz = third.cpu.current_mhz.expect("第三次采样应读到当前频率");
    let base = f64::from(third.cpu.info.as_ref().unwrap().base_mhz);
    assert!(mhz > base * 0.1 && mhz < base * 3.0, "当前频率 {mhz} MHz 偏离基准 {base} MHz 过多");
}

/// 用 PowerShell 的 `Get-CimInstance Win32_Processor` 取参照值，各字段用 `|` 分隔，多插槽时求和。
fn wmi_reference() -> (String, u32, u32, u64, u64, u32) {
    let script = "Get-CimInstance Win32_Processor | ForEach-Object { \
        \"$($_.Name.Trim())|$($_.NumberOfCores)|$($_.NumberOfLogicalProcessors)|$($_.L2CacheSize)|$($_.L3CacheSize)|$($_.MaxClockSpeed)\" }";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("无法运行 powershell");
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut name, mut cores, mut logical, mut l2, mut l3, mut mhz) = (String::new(), 0, 0, 0, 0, 0);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.trim().split('|').collect();
        assert_eq!(f.len(), 6, "无法解析 WMI 输出：{line}");
        name = f[0].to_string();
        cores += f[1].parse::<u32>().unwrap();
        logical += f[2].parse::<u32>().unwrap();
        l2 += f[3].parse::<u64>().unwrap();
        l3 += f[4].parse::<u64>().unwrap();
        mhz = mhz.max(f[5].parse::<u32>().unwrap());
    }
    (name, cores, logical, l2, l3, mhz)
}

#[test]
fn topology_and_model_match_wmi() {
    let info = read_cpu_info();
    let (name, cores, logical, l2_kb, l3_kb, mhz) = wmi_reference();
    eprintln!("{info:#?}\nWMI: {name} {cores}核 {logical}线程 L2 {l2_kb} KB L3 {l3_kb} KB {mhz} MHz");
    assert_eq!(info.name, name, "型号名称");
    assert_eq!(info.cores, cores, "内核数");
    assert_eq!(info.logical, logical, "逻辑处理器数");
    assert_eq!(info.l2_bytes / 1024, l2_kb, "L2 缓存总量");
    assert_eq!(info.l3_bytes / 1024, l3_kb, "L3 缓存总量");
    assert_eq!(info.base_mhz, mhz, "基准频率（WMI MaxClockSpeed）");
    assert!(info.l1_bytes > 0 && info.sockets >= 1);
}

/// 在一次尝试里让一个线程绑到逻辑处理器 `core` 上忙循环，返回两次采样之间各核的占用。
fn burn_core(core: usize) -> Vec<f32> {
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let worker = {
        let (stop, ready) = (stop.clone(), ready.clone());
        std::thread::spawn(move || {
            unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << core) };
            ready.store(true, Ordering::SeqCst);
            let mut x = 1u64;
            while !stop.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        })
    };
    while !ready.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    let mut s = Sampler::default();
    s.sample();
    std::thread::sleep(Duration::from_millis(1200));
    let usage = s.sample().cpu.per_core;
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
    usage
}

#[test]
fn pinned_busy_thread_lights_up_its_core() {
    let logical = std::thread::available_parallelism().unwrap().get();
    // 选中间的核，避开 0 号核上常见的系统中断噪声。
    let core = logical / 2;
    // 其他程序同时在编译时各核都可能很忙，允许重试；断言：目标核 > 70% 且不低于任何其他核（容差 1%）。
    let mut last = Vec::new();
    for _ in 0..3 {
        last = burn_core(core);
        let max_other = last.iter().enumerate().filter(|(i, _)| *i != core).map(|(_, u)| *u).fold(0.0, f32::max);
        if last[core] > 70.0 && last[core] + 1.0 >= max_other {
            eprintln!("核 {core} 占用 {:.1}%，其余最高 {max_other:.1}%", last[core]);
            return;
        }
    }
    panic!("核 {core} 满载时占用不符合预期：{last:?}");
}

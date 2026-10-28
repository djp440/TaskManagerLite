//! 在真实系统上采样，确认采集路径可用。

use std::time::{Duration, Instant};

use tml_core::{Sampler, Scope, ViewMode, ViewOptions, build_view};

#[test]
fn samples_real_system() {
    let mut s = Sampler::default();
    let t = Instant::now();
    let first = s.sample();
    let first_ms = t.elapsed().as_secs_f64() * 1000.0;
    std::thread::sleep(Duration::from_millis(500));
    let second = s.sample();

    let me = std::process::id();
    let own = second.snapshot.processes.iter().find(|p| p.key.pid == me).expect("快照中应包含本进程");
    assert!(own.private_working_set > 0);
    assert!(own.critical, "本程序自身视为关键进程");
    assert!(first.snapshot.processes.len() > 20);
    assert!(second.system.memory_total > 0 && second.system.memory_used > 0);
    assert!((0.0..=100.0).contains(&second.system.cpu_usage));
    let total_cpu: f32 = second.snapshot.processes.iter().map(|p| p.cpu_usage).sum();
    assert!(total_cpu <= 100.5, "单进程占用率合计不应超过整机 100%: {total_cpu}");

    let tree = build_view(&second.snapshot, &ViewOptions { mode: ViewMode::Tree, ..Default::default() });
    assert_eq!(tree.len(), second.snapshot.processes.len(), "树形视图应包含全部进程");
    eprintln!("首次采样 {first_ms:.1} ms，进程数 {}", first.snapshot.processes.len());
}

#[test]
fn metrics_scope_skips_processes_but_keeps_baseline() {
    let mut s = Sampler::default();
    assert!(s.sample().processes_fresh);
    std::thread::sleep(Duration::from_millis(200));
    let m = s.sample_scope(Scope::Metrics);
    assert!(!m.processes_fresh && m.snapshot.processes.is_empty() && m.app_pids.is_none());
    assert!(m.system.memory_total > 0 && !m.cpu.per_core.is_empty());
    assert!((0.0..=100.0).contains(&m.system.cpu_usage));
    std::thread::sleep(Duration::from_millis(300));
    // 夹着只采指标的采样，进程占用率仍按上一次进程快照做差，合计不会超过 100%。
    let f = s.sample();
    assert!(f.processes_fresh && f.snapshot.processes.len() > 20);
    let total: f32 = f.snapshot.processes.iter().map(|p| p.cpu_usage).sum();
    assert!(total <= 100.5, "{total}");
}

#[test]
fn ends_a_real_process_tree() {
    use std::os::windows::process::CommandExt;
    use tml_core::{end_process, tree_from, EndError};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut child = std::process::Command::new("cmd")
        .args(["/c", "ping -t 127.0.0.1 >nul"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));

    let mut s = Sampler::default();
    let snap = s.sample().snapshot;
    let root = snap.processes.iter().find(|p| p.key.pid == child.id()).expect("找到 cmd").key;
    let tree = tree_from(&snap, root);
    let names: Vec<&str> = tree
        .iter()
        .map(|k| snap.processes.iter().find(|p| p.key == *k).unwrap().name.as_str())
        .collect();
    assert!(names.iter().any(|n| n.eq_ignore_ascii_case("PING.EXE")), "进程树应包含 ping：{names:?}");
    assert_eq!(*tree.last().unwrap(), root, "根进程最后结束");

    for k in &tree {
        assert_eq!(end_process(*k), Ok(()));
    }
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let after = s.sample().snapshot;
    for k in &tree {
        assert!(!after.processes.iter().any(|p| p.key == *k), "进程 {} 应已结束", k.pid);
        // 已结束的进程再结束一次，应报告“已退出”而不是误杀复用 PID 的进程。
        assert_eq!(end_process(*k), Err(EndError::Gone));
    }
}

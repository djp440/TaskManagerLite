//! 在真实桌面上验证“应用”判定。需要交互式桌面（会打开记事本窗口），所以默认忽略：
//! `pwsh scripts/test-apps.ps1`，或 `cargo test -p tml-core --test apps -- --ignored --nocapture`。
//!
//! 只启动、只结束测试自己创建的进程。

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tml_core::{
    EndError, Group, ProcessKey, Sampler, ViewMode, ViewOptions, app_pids, build_group_view, end_process, tree_from,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};
use windows::core::BOOL;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 轮询直到条件成立或超时，返回最后一次的结果。
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 向进程的所有可见顶层窗口发送 WM_CLOSE（相当于点关闭按钮）。
fn close_windows_of(pid: u32) {
    struct Ctx {
        pid: u32,
        hits: Vec<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if owner == ctx.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            ctx.hits.push(hwnd);
        }
        true.into()
    }
    let mut ctx = Ctx { pid, hits: Vec::new() };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut ctx as *mut Ctx as isize)) };
    for h in ctx.hits {
        let _ = unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

/// 结束进程及其后代（只用于测试自己启动的进程）。
fn end_tree(sampler: &mut Sampler, pid: u32) {
    let snap = sampler.sample().snapshot;
    if let Some(p) = snap.processes.iter().find(|p| p.key.pid == pid) {
        for k in tree_from(&snap, p.key) {
            let _ = end_process(k);
        }
    }
}

/// 测试自己启动的子进程：离开作用域（包括断言失败 panic）时一定结束它及其后代，不留下记事本等残留。
/// `Child` 仍持有句柄，PID 在 `wait` 之前不会被复用，所以按 PID 找到的一定是它。
struct Owned(Child);

impl Owned {
    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        end_tree(&mut Sampler::default(), self.0.id());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 测试自己启动的 UWP 进程（按 `ProcessKey` 结束，不会误伤复用 PID 的进程）：离开作用域时关闭窗口并结束。
struct OwnedKey(ProcessKey);

impl Drop for OwnedKey {
    fn drop(&mut self) {
        close_windows_of(self.0.pid);
        let _ = end_process(self.0);
    }
}

fn spawn_background() -> Owned {
    // 无窗口的控制台程序：隐藏控制台，也没有其他窗口。
    Owned(
        Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("启动 cmd"),
    )
}

#[test]
#[ignore = "需要交互式桌面，用 scripts/test-apps.ps1 运行"]
fn notepad_is_app_and_windowless_child_is_background() {
    let mut sampler = Sampler::default();
    sampler.track_apps(true);

    // 两个守卫保证断言失败时也会结束记事本和 ping。
    let notepad = Owned(Command::new("notepad.exe").spawn().expect("启动记事本"));
    let bg = spawn_background();
    let (np, bgp) = (notepad.id(), bg.id());

    // 记事本窗口出现后：判定为应用。
    assert!(
        wait_until(Duration::from_secs(10), || app_pids().contains(&np)),
        "记事本 {np} 应在窗口出现后被判定为应用"
    );
    let apps = app_pids();
    assert!(!apps.contains(&bgp), "无窗口的后台进程 {bgp} 不应是应用");

    // 同一份快照放进分组视图：记事本是应用组里的一个根，且只出现一次。
    // （后台进程所在的组取决于测试是从哪里启动的：从有窗口的终端 / 编辑器里跑，它们也是那个应用的后代，
    // 所以这里只断言“不是应用”；后代归组的规则由 view.rs 的单元测试覆盖。）
    let snap = sampler.sample().snapshot;
    let rows = build_group_view(&snap, &apps, &ViewOptions { mode: ViewMode::Group, ..Default::default() });
    let mut current = None;
    let mut notepad_rows = Vec::new();
    for r in &rows {
        match r.process() {
            None => current = r.header.map(|h| h.group),
            Some(i) if snap.processes[i].key.pid == np => notepad_rows.push((current, r.depth)),
            Some(_) => {}
        }
    }
    assert_eq!(notepad_rows, [(Some(Group::Apps), 0)]);

    // 关闭记事本窗口后：不再是应用。
    close_windows_of(np);
    assert!(
        wait_until(Duration::from_secs(10), || !app_pids().contains(&np)),
        "关闭窗口后记事本 {np} 不应再是应用"
    );

    // 守卫在函数结束时结束记事本与 ping；此处先确认窗口关闭后的记事本确实可以被结束。
    drop(notepad);
    assert_eq!(end_process(snap.processes.iter().find(|p| p.key.pid == np).unwrap().key), Err(EndError::Gone));
    drop(bg);
}

/// 计算器（UWP）：窗口属于 ApplicationFrameHost，要通过 CoreWindow 找到真实进程。
/// 本机没有计算器、或已有实例（再次激活不产生新进程）时跳过。
#[test]
#[ignore = "需要交互式桌面，用 scripts/test-apps.ps1 运行"]
fn uwp_calculator_is_app_through_core_window() {
    let mut sampler = Sampler::default();
    let before: HashSet<u32> = sampler.sample().snapshot.processes.iter().map(|p| p.key.pid).collect();

    // 用协议启动：不依赖 calc.exe 在不同系统版本上的行为。
    let launcher = Command::new("cmd").args(["/c", "start", "", "calculator:"]).creation_flags(CREATE_NO_WINDOW).status();
    if !launcher.is_ok_and(|s| s.success()) {
        eprintln!("跳过：无法启动计算器");
        return;
    }
    let is_calc = |name: &str| name.eq_ignore_ascii_case("CalculatorApp.exe") || name.eq_ignore_ascii_case("Calculator.exe");
    let mut mine = None;
    wait_until(Duration::from_secs(15), || {
        let snap = sampler.sample().snapshot;
        mine = snap.processes.iter().find(|p| is_calc(&p.name) && !before.contains(&p.key.pid)).map(|p| p.key);
        mine.is_some()
    });
    let Some(key) = mine else {
        eprintln!("跳过：没有产生新的计算器进程（可能已有实例，或本机未安装计算器）");
        return;
    };

    // 守卫保证无论断言是否失败，都会关闭并结束自己启动的计算器。
    let _guard = OwnedKey(key);
    let appeared = wait_until(Duration::from_secs(15), || app_pids().contains(&key.pid));
    assert!(appeared, "计算器 {} 应通过 CoreWindow 被判定为应用", key.pid);
    eprintln!("计算器 pid={} 通过 CoreWindow 判定为应用", key.pid);
    close_windows_of(key.pid);
    let ended = end_process(key);
    eprintln!("结束自己启动的计算器 pid={}：{ended:?}", key.pid);
    assert!(wait_until(Duration::from_secs(10), || !app_pids().contains(&key.pid)));
}

#[test]
#[ignore = "耗时测量，用 scripts/test-apps.ps1 运行"]
fn enumeration_cost() {
    app_pids();
    const N: u32 = 300;
    let t = Instant::now();
    let mut count = 0;
    for _ in 0..N {
        count = app_pids().len();
    }
    let per_call = t.elapsed().as_secs_f64() * 1000.0 / N as f64;
    eprintln!("app_pids 平均每次 {per_call:.3} ms（{count} 个应用进程，{N} 次）");
    assert!(per_call < 20.0, "枚举过慢：{per_call:.2} ms");
}

/// 守卫在测试 panic（断言失败）时也要结束自己启动的进程及其后代。不需要桌面，随普通测试运行。
#[test]
fn guard_cleans_up_when_the_test_panics() {
    let mut sampler = Sampler::default();
    let mut seen = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let bg = spawn_background();
        std::thread::sleep(Duration::from_millis(800));
        let snap = sampler.sample().snapshot;
        let root = snap.processes.iter().find(|p| p.key.pid == bg.id()).expect("找到 cmd").key;
        seen = tree_from(&snap, root);
        assert!(seen.len() >= 2, "应包含 cmd 与 ping：{seen:?}");
        panic!("模拟断言失败");
    }));
    assert!(result.is_err());
    std::thread::sleep(Duration::from_millis(300));
    let after = sampler.sample().snapshot;
    for k in &seen {
        assert!(!after.processes.iter().any(|p| p.key == *k), "panic 后进程 {} 仍在运行", k.pid);
    }
}

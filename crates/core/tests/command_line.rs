//! 在真实系统上验证命令行读取（右键菜单“复制命令行”所依赖）。

use std::os::windows::process::CommandExt;

use tml_core::{ProcessKey, Sampler, command_line};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 以挂起状态创建：进程不会退出，命令行在 CreateProcess 时已写入 PEB，可以读取。
const CREATE_SUSPENDED: u32 = 0x4;

/// 自己启动的子进程，测试结束时只结束它。
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn key_of(pid: u32) -> ProcessKey {
    let snap = Sampler::default().sample().snapshot;
    snap.processes.iter().find(|p| p.key.pid == pid).unwrap_or_else(|| panic!("快照中应有 pid {pid}")).key
}

#[test]
fn reads_the_exact_command_line_of_a_spawned_child() {
    // 带特定参数（含带空格、需要引号的参数）：ping 一直运行直到被结束。
    let child = Child(
        std::process::Command::new("ping")
            .args(["-t", "-w", "7777", "127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("启动子进程"),
    );
    let line = command_line(key_of(child.0.id())).expect("应能读到子进程命令行");
    assert_eq!(line, r#""ping" -t -w 7777 127.0.0.1"#);
}

#[test]
fn quoted_arguments_are_preserved() {
    // 直接启动 ping（不经 cmd，避免遗留孙进程）；主机名参数含空格，会被加引号。
    // 目标无法解析，ping 会很快退出，读命令行时进程可能已不存在，所以挂起创建（Drop 里的 kill 对挂起进程同样有效）。
    let child = Child(
        std::process::Command::new("ping")
            .args(["-t", "-w", "7777", "no such host.invalid"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
            .spawn()
            .expect("启动子进程"),
    );
    let line = command_line(key_of(child.0.id())).expect("应能读到子进程命令行");
    assert!(line.ends_with(r#" -t -w 7777 "no such host.invalid""#), "{line}");
}

#[test]
fn stale_key_returns_none() {
    let child = Child(
        std::process::Command::new("ping")
            .args(["-t", "127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("启动子进程"),
    );
    let key = key_of(child.0.id());
    assert!(command_line(key).is_some());
    assert_eq!(command_line(ProcessKey { create_time: key.create_time + 1, ..key }), None);
}

#[test]
fn protected_and_missing_processes_return_none_without_error() {
    // PID 4（System）只读，不做任何操作；PID 0 与不存在的 PID 同样安全地返回 None。
    let system = Sampler::default().sample().snapshot.processes.iter().find(|p| p.key.pid == 4).map(|p| p.key);
    if let Some(key) = system {
        assert_eq!(command_line(key), None);
    }
    assert_eq!(command_line(ProcessKey { pid: 0, create_time: 0 }), None);
    assert_eq!(command_line(ProcessKey { pid: 0xFFFF_FFF0, create_time: 1 }), None);
}

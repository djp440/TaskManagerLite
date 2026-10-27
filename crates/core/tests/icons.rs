//! 在真实系统上验证进程路径读取与图标提取。

use std::os::windows::process::CommandExt;
use std::path::Path;

use tml_core::{ProcessKey, Sampler, default_icon, file_icon, image_path, image_path_of};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 自己启动的、可预期路径的子进程（cmd 里 ping 挂起），测试结束时只结束它。
struct Child(std::process::Child);

impl Child {
    fn spawn() -> Self {
        Self(
            std::process::Command::new("cmd")
                .args(["/c", "ping -t 127.0.0.1 >nul"])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .expect("启动子进程"),
        )
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn reads_path_of_a_spawned_process() {
    let child = Child::spawn();
    let path = image_path(child.0.id()).expect("应能读到子进程路径");
    assert_eq!(path.file_name().unwrap().to_ascii_lowercase(), "cmd.exe");
    assert!(path.is_file());
}

#[test]
fn path_lookup_by_key_checks_creation_time() {
    let child = Child::spawn();
    let mut sampler = Sampler::default();
    let snap = sampler.sample().snapshot;
    let p = snap.processes.iter().find(|p| p.key.pid == child.0.id()).expect("快照中应有子进程");
    assert!(image_path_of(p.key).is_some());
    assert_eq!(image_path_of(ProcessKey { create_time: p.key.create_time + 1, ..p.key }), None);
}

#[test]
fn protected_and_missing_processes_have_no_path() {
    assert_eq!(image_path(0), None);
    // PID 4（System）只读不操作；没有可用的映像文件路径。
    assert_eq!(image_path(4), None);
    // 几乎不可能存在的 PID。
    assert_eq!(image_path(0xFFFF_FFF0), None);
}

#[test]
fn extracts_non_empty_icons_at_requested_sizes() {
    let child = Child::spawn();
    let path = image_path(child.0.id()).unwrap();
    for px in [16, 24, 32, 48] {
        let icon = file_icon(&path, px).unwrap_or_else(|| panic!("应能取到 {px}px 图标"));
        assert_eq!((icon.width, icon.height), (px, px));
        assert_eq!(icon.rgba.len(), (px * px * 4) as usize);
        assert!(icon.has_visible_pixels());
    }
}

#[test]
fn own_test_binary_has_an_icon_or_falls_back_to_the_shell_one() {
    // 测试可执行文件没有图标资源：取到的是外壳的通用图标，也应非空。
    let me = std::env::current_exe().unwrap();
    let icon = file_icon(&me, 32).expect("应取到图标");
    assert!(icon.has_visible_pixels());
}

#[test]
fn missing_file_has_no_icon_but_default_icon_exists() {
    assert!(file_icon(Path::new(r"C:\no\such\thing.exe"), 16).is_none());
    let d = default_icon(32).expect("应有默认图标");
    assert_eq!((d.width, d.height), (32, 32));
    assert!(d.has_visible_pixels());
}

#[test]
fn common_system_programs_have_distinct_icons() {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let explorer = file_icon(&Path::new(&windir).join("explorer.exe"), 32).expect("explorer 图标");
    let notepad = file_icon(&Path::new(&windir).join("notepad.exe"), 32);
    let default = default_icon(32).unwrap();
    assert_ne!(explorer, default, "explorer 应有自己的图标，而不是通用图标");
    if let Some(n) = notepad {
        assert_ne!(n, explorer);
    }
}

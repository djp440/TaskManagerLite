//! `NtQuerySystemInformation(SystemProcessInformation)` 的原始读取。

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};

use crate::cpu::ProcessTotals;

const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

/// 一个进程在某一时刻的原始计数器，尚未与上一次采样做差。
#[derive(Debug, Clone)]
pub struct RawProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    /// FILETIME 单位（100ns，自 1601 年起）。
    pub create_time: u64,
    /// 用户态 + 内核态 CPU 时间，100ns 单位。
    pub cpu_time: u64,
    pub private_working_set: u64,
    /// 累计读写字节数。
    pub io_bytes: u64,
    pub session_id: u32,
    pub base_priority: i32,
    pub handles: u32,
    pub threads: u32,
    pub working_set: u64,
    pub peak_working_set: u64,
    /// 提交大小（私有页面），字节。
    pub commit: u64,
    /// 所有线程都因挂起而等待（与原生任务管理器的“已挂起”一致）。
    pub suspended: bool,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// 完整的 SYSTEM_PROCESS_INFORMATION 布局（windows crate 只公开了裁剪版）。
#[repr(C)]
struct SystemProcessInfo {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

/// SYSTEM_THREAD_INFORMATION：紧跟在每个进程条目之后，共 `number_of_threads` 个。
#[repr(C)]
struct SystemThreadInfo {
    kernel_time: i64,
    user_time: i64,
    create_time: i64,
    wait_time: u32,
    start_address: usize,
    unique_process: usize,
    unique_thread: usize,
    priority: i32,
    base_priority: i32,
    context_switches: u32,
    thread_state: u32,
    wait_reason: u32,
}

/// KTHREAD_STATE::Waiting 与 KWAIT_REASON::Suspended。
const THREAD_STATE_WAITING: u32 = 5;
const WAIT_REASON_SUSPENDED: u32 = 5;

/// 进程是否处于挂起状态：至少有一个线程，且所有线程都在因挂起而等待。
fn all_suspended(threads: &[SystemThreadInfo]) -> bool {
    !threads.is_empty()
        && threads.iter().all(|t| t.thread_state == THREAD_STATE_WAITING && t.wait_reason == WAIT_REASON_SUSPENDED)
}

/// 复用缓冲区的进程枚举器。
pub struct ProcessQuery {
    buffer: Vec<u64>,
    /// 最近一次枚举中 Memory Compression 进程的工作集（字节），没有该进程时为 0。
    pub compressed_working_set: u64,
    /// 最近一次查询里全部进程的线程数与句柄数之和。
    pub totals: ProcessTotals,
}

impl Default for ProcessQuery {
    fn default() -> Self {
        Self { buffer: vec![0; 256 * 1024 / 8], compressed_working_set: 0, totals: ProcessTotals::default() }
    }
}

impl ProcessQuery {
    pub fn query(&mut self) -> windows::core::Result<Vec<RawProcess>> {
        loop {
            let mut needed = 0u32;
            let byte_len = (self.buffer.len() * 8) as u32;
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    self.buffer.as_mut_ptr().cast(),
                    byte_len,
                    &mut needed,
                )
            };
            if status.0 == STATUS_INFO_LENGTH_MISMATCH {
                // 留出余量，进程数在两次调用之间可能增长。
                let want = (needed as usize).max(byte_len as usize) * 3 / 2;
                self.buffer.resize(want.div_ceil(8), 0);
                continue;
            }
            status.ok()?;
            return Ok(self.parse());
        }
    }

    fn parse(&mut self) -> Vec<RawProcess> {
        self.compressed_working_set = 0;
        let buffer_end = self.buffer.len() * 8;
        let mut out = Vec::with_capacity(512);
        let mut totals = ProcessTotals::default();
        let base = self.buffer.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            // SAFETY: 系统保证每个条目按 8 字节对齐，且 next_entry_offset 链在缓冲区内。
            let info = unsafe { &*base.add(offset).cast::<SystemProcessInfo>() };
            let pid = info.unique_process_id as u32;
            totals.threads += info.number_of_threads;
            totals.handles += info.handle_count;
            let name = if info.image_name.buffer.is_null() {
                if pid == 0 { "System Idle Process".into() } else { String::new() }
            } else {
                let units = unsafe {
                    std::slice::from_raw_parts(info.image_name.buffer, info.image_name.length as usize / 2)
                };
                String::from_utf16_lossy(units)
            };
            if name == "Memory Compression" {
                self.compressed_working_set = info.working_set_size as u64;
            }
            // 线程数组紧跟在进程条目之后；条目链里的下一项（或缓冲区末尾）是它的上界。
            let threads_at = offset + size_of::<SystemProcessInfo>();
            let limit = if info.next_entry_offset == 0 { buffer_end } else { offset + info.next_entry_offset as usize };
            let count = (info.number_of_threads as usize).min(limit.saturating_sub(threads_at) / size_of::<SystemThreadInfo>());
            // SAFETY: 上面已把线程数截断到条目边界之内；条目 8 字节对齐，线程结构大小是 8 的倍数。
            let threads = unsafe { std::slice::from_raw_parts(base.add(threads_at).cast::<SystemThreadInfo>(), count) };
            out.push(RawProcess {
                pid,
                parent_pid: info.inherited_from_unique_process_id as u32,
                name,
                create_time: info.create_time.max(0) as u64,
                cpu_time: (info.user_time + info.kernel_time).max(0) as u64,
                private_working_set: info.working_set_private_size.max(0) as u64,
                io_bytes: (info.read_transfer_count + info.write_transfer_count).max(0) as u64,
                session_id: info.session_id,
                base_priority: info.base_priority,
                handles: info.handle_count,
                threads: info.number_of_threads,
                working_set: info.working_set_size as u64,
                peak_working_set: info.peak_working_set_size as u64,
                commit: info.private_page_count as u64,
                suspended: all_suspended(threads),
            });
            if info.next_entry_offset == 0 {
                break;
            }
            offset += info.next_entry_offset as usize;
        }
        self.totals = totals;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_64_bit_abi() {
        assert_eq!(size_of::<SystemProcessInfo>(), 256);
        assert_eq!(size_of::<SystemThreadInfo>(), 80);
        assert_eq!(std::mem::offset_of!(SystemThreadInfo, thread_state), 0x44);
    }

    #[test]
    fn own_process_has_threads_handles_and_is_running() {
        let me = std::process::id();
        let procs = ProcessQuery::default().query().unwrap();
        let p = procs.iter().find(|p| p.pid == me).expect("快照里应有本进程");
        assert!(p.threads >= 1 && p.handles > 0, "{p:?}");
        assert!(p.working_set > 0 && p.peak_working_set >= p.working_set && p.commit > 0, "{p:?}");
        assert!(!p.suspended);
        assert_eq!(p.base_priority, 8, "测试进程应为正常优先级");
    }

    #[test]
    fn process_created_suspended_is_reported_suspended() {
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x4;
        let mut child = std::process::Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .creation_flags(CREATE_SUSPENDED)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let procs = ProcessQuery::default().query().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let p = procs.iter().find(|p| p.pid == child.id()).expect("快照里应有子进程");
        assert!(p.suspended, "{p:?}");
    }

    #[test]
    fn suspended_means_every_thread_waits_for_suspension() {
        let t = |state, reason| SystemThreadInfo {
            kernel_time: 0,
            user_time: 0,
            create_time: 0,
            wait_time: 0,
            start_address: 0,
            unique_process: 0,
            unique_thread: 0,
            priority: 0,
            base_priority: 0,
            context_switches: 0,
            thread_state: state,
            wait_reason: reason,
        };
        assert!(all_suspended(&[t(5, 5), t(5, 5)]));
        assert!(!all_suspended(&[t(5, 5), t(5, 6)]));
        assert!(!all_suspended(&[t(2, 0)]));
        assert!(!all_suspended(&[]));
    }
}

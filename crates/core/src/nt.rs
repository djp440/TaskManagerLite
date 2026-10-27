//! `NtQuerySystemInformation(SystemProcessInformation)` 的原始读取。

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};

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

/// 复用缓冲区的进程枚举器。
pub struct ProcessQuery {
    buffer: Vec<u64>,
}

impl Default for ProcessQuery {
    fn default() -> Self {
        Self { buffer: vec![0; 256 * 1024 / 8] }
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

    fn parse(&self) -> Vec<RawProcess> {
        let mut out = Vec::with_capacity(512);
        let base = self.buffer.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            // SAFETY: 系统保证每个条目按 8 字节对齐，且 next_entry_offset 链在缓冲区内。
            let info = unsafe { &*base.add(offset).cast::<SystemProcessInfo>() };
            let pid = info.unique_process_id as u32;
            let name = if info.image_name.buffer.is_null() {
                if pid == 0 { "System Idle Process".into() } else { String::new() }
            } else {
                let units = unsafe {
                    std::slice::from_raw_parts(info.image_name.buffer, info.image_name.length as usize / 2)
                };
                String::from_utf16_lossy(units)
            };
            out.push(RawProcess {
                pid,
                parent_pid: info.inherited_from_unique_process_id as u32,
                name,
                create_time: info.create_time.max(0) as u64,
                cpu_time: (info.user_time + info.kernel_time).max(0) as u64,
                private_working_set: info.working_set_private_size.max(0) as u64,
                io_bytes: (info.read_transfer_count + info.write_transfer_count).max(0) as u64,
                session_id: info.session_id,
            });
            if info.next_entry_offset == 0 {
                break;
            }
            offset += info.next_entry_offset as usize;
        }
        out
    }
}

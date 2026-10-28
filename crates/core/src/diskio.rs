//! ETW 磁盘事件的解析与按进程聚合：纯函数，不接触系统接口，可用构造的事件序列单测。
//!
//! 事件来自内核 `DiskIo` 与 `Thread` 两类 MOF 事件（见 ADR-0003）。磁盘事件在 DPC 中完成时记录，
//! 事件头里的进程号不可靠，要用负载里的“发起线程”（`IssuingThreadId`，版本 3）经线程表还原出进程。

use std::collections::{HashMap, HashSet};

/// `DiskIo` 事件类 GUID：{3d6fa8d4-fe05-11d0-9dda-00c04fd7ba7c}。
pub const DISK_IO_GUID: u128 = 0x3d6fa8d4_fe05_11d0_9dda_00c04fd7ba7c;
/// `Thread` 事件类 GUID：{3d6fa8d1-fe05-11d0-9dda-00c04fd7ba7c}。
pub const THREAD_GUID: u128 = 0x3d6fa8d1_fe05_11d0_9dda_00c04fd7ba7c;

const OPCODE_THREAD_START: u8 = 1;
const OPCODE_THREAD_END: u8 = 2;
/// 会话启动时对已有线程的“枚举”事件。
const OPCODE_THREAD_DC_START: u8 = 3;
const OPCODE_DISK_READ: u8 = 10;
const OPCODE_DISK_WRITE: u8 = 11;

/// 事件头 `Flags` 中表示“64 位事件头”（负载里的指针占 8 字节）的位。
pub const FLAG_64_BIT_HEADER: u16 = 0x40;

/// 解析后的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    ThreadStart { pid: u32, tid: u32 },
    ThreadEnd { tid: u32 },
    /// `tid` 为负载里的发起线程（版本低于 3 时没有）；`header_pid` 是事件头里的进程号，只在无法还原时兜底。
    DiskIo { tid: Option<u32>, header_pid: u32, bytes: u32 },
}

fn u32_at(p: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(p.get(off..off + 4)?.try_into().ok()?))
}

/// 解析一条事件。`provider` 是事件类 GUID，`flags` 是事件头 `Flags`，`header_pid` 是事件头进程号。
/// 不关心的事件（其他类、其他操作码、负载过短）返回 `None`。
pub fn parse(provider: u128, opcode: u8, version: u8, flags: u16, header_pid: u32, payload: &[u8]) -> Option<Event> {
    match provider {
        THREAD_GUID => match opcode {
            // 负载前两项在所有版本里都是 ProcessId、TThreadId。
            OPCODE_THREAD_START | OPCODE_THREAD_DC_START => {
                Some(Event::ThreadStart { pid: u32_at(payload, 0)?, tid: u32_at(payload, 4)? })
            }
            OPCODE_THREAD_END => Some(Event::ThreadEnd { tid: u32_at(payload, 4)? }),
            _ => None,
        },
        DISK_IO_GUID if matches!(opcode, OPCODE_DISK_READ | OPCODE_DISK_WRITE) => {
            // DiskNumber、IrpFlags、TransferSize、Reserved、ByteOffset(8)、FileObject(指针)、Irp(指针)、
            // HighResResponseTime(8)，版本 3 起末尾多一个 IssuingThreadId。
            let ptr = if flags & FLAG_64_BIT_HEADER != 0 { 8 } else { 4 };
            let bytes = u32_at(payload, 8)?;
            let tid = if version >= 3 { u32_at(payload, 24 + 2 * ptr + 8) } else { None };
            Some(Event::DiskIo { tid, header_pid, bytes })
        }
        _ => None,
    }
}

/// 按进程累计磁盘字节数。
#[derive(Default)]
pub struct DiskAggregator {
    threads: HashMap<u32, u32>,
    /// 已结束的线程：晚到的磁盘事件仍要能还原，所以推迟到下一次 `take` 才从线程表删除。
    ended: HashSet<u32>,
    bytes: HashMap<u32, u64>,
    /// 无法还原发起线程、退回事件头进程号的事件数（诊断用）。
    pub unresolved: u64,
}

impl DiskAggregator {
    /// 线程表是否已有该线程（没有时调用方可以向系统查询后用 [`learn`](Self::learn) 补上）。
    pub fn knows(&self, tid: u32) -> bool {
        self.threads.contains_key(&tid)
    }

    pub fn learn(&mut self, tid: u32, pid: u32) {
        self.threads.insert(tid, pid);
    }

    pub fn apply(&mut self, e: Event) {
        match e {
            Event::ThreadStart { pid, tid } => {
                // 线程号会复用：重新启动的线程不能被上一轮的结束标记删掉。
                self.ended.remove(&tid);
                self.threads.insert(tid, pid);
            }
            Event::ThreadEnd { tid } => {
                self.ended.insert(tid);
            }
            Event::DiskIo { tid, header_pid, bytes } => {
                let pid = match tid.and_then(|t| self.threads.get(&t)) {
                    Some(&pid) => pid,
                    None => {
                        self.unresolved += 1;
                        header_pid
                    }
                };
                *self.bytes.entry(pid).or_default() += u64::from(bytes);
            }
        }
    }

    /// 取出自上次调用以来每个进程累计的字节数，并清零；顺带回收已结束的线程。
    pub fn take(&mut self) -> HashMap<u32, u64> {
        for t in self.ended.drain() {
            self.threads.remove(&t);
        }
        std::mem::take(&mut self.bytes)
    }
}

/// 把累计字节数换算成字节/秒。`elapsed_secs` 不是正数时返回空表。
pub fn rates(bytes: &HashMap<u32, u64>, elapsed_secs: f64) -> HashMap<u32, f64> {
    if elapsed_secs <= 0.0 {
        return HashMap::new();
    }
    bytes.iter().map(|(&pid, &b)| (pid, b as f64 / elapsed_secs)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_payload(pid: u32, tid: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend(pid.to_le_bytes());
        p.extend(tid.to_le_bytes());
        p.extend([0u8; 56]);
        p
    }

    /// 版本 3、64 位事件头的 DiskIo 负载。
    fn disk_payload(size: u32, issuing: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend(1u32.to_le_bytes()); // DiskNumber
        p.extend(0u32.to_le_bytes()); // IrpFlags
        p.extend(size.to_le_bytes()); // TransferSize
        p.extend(0u32.to_le_bytes()); // Reserved
        p.extend(4096u64.to_le_bytes()); // ByteOffset
        p.extend(0xdead_u64.to_le_bytes()); // FileObject
        p.extend(0xbeef_u64.to_le_bytes()); // Irp
        p.extend(77u64.to_le_bytes()); // HighResResponseTime
        p.extend(issuing.to_le_bytes()); // IssuingThreadId
        p
    }

    fn feed(agg: &mut DiskAggregator, provider: u128, opcode: u8, version: u8, header_pid: u32, payload: &[u8]) {
        if let Some(e) = parse(provider, opcode, version, FLAG_64_BIT_HEADER, header_pid, payload) {
            agg.apply(e);
        }
    }

    #[test]
    fn parses_thread_start_end_and_rundown() {
        let p = thread_payload(100, 200);
        for op in [OPCODE_THREAD_START, OPCODE_THREAD_DC_START] {
            assert_eq!(parse(THREAD_GUID, op, 3, FLAG_64_BIT_HEADER, 0, &p), Some(Event::ThreadStart { pid: 100, tid: 200 }));
        }
        assert_eq!(parse(THREAD_GUID, OPCODE_THREAD_END, 3, FLAG_64_BIT_HEADER, 0, &p), Some(Event::ThreadEnd { tid: 200 }));
        // 线程结束的枚举（DCEnd=4）和其他操作码忽略。
        assert_eq!(parse(THREAD_GUID, 4, 3, FLAG_64_BIT_HEADER, 0, &p), None);
        assert_eq!(parse(THREAD_GUID, OPCODE_THREAD_START, 3, FLAG_64_BIT_HEADER, 0, &p[..6]), None);
    }

    #[test]
    fn parses_disk_io_issuing_thread_by_version_and_pointer_size() {
        let p = disk_payload(65536, 321);
        assert_eq!(
            parse(DISK_IO_GUID, OPCODE_DISK_READ, 3, FLAG_64_BIT_HEADER, 4, &p),
            Some(Event::DiskIo { tid: Some(321), header_pid: 4, bytes: 65536 })
        );
        // 版本 2 没有发起线程。
        assert_eq!(
            parse(DISK_IO_GUID, OPCODE_DISK_WRITE, 2, FLAG_64_BIT_HEADER, 4, &p),
            Some(Event::DiskIo { tid: None, header_pid: 4, bytes: 65536 })
        );
        // 32 位事件头：指针 4 字节，发起线程在偏移 40。
        let mut p32 = Vec::new();
        p32.extend([0u8; 8]);
        p32.extend(512u32.to_le_bytes());
        p32.extend([0u8; 4]);
        p32.extend([0u8; 8]); // ByteOffset
        p32.extend([0u8; 8]); // 两个 4 字节指针
        p32.extend([0u8; 8]); // HighResResponseTime
        p32.extend(555u32.to_le_bytes());
        assert_eq!(
            parse(DISK_IO_GUID, OPCODE_DISK_READ, 3, 0x20, 9, &p32),
            Some(Event::DiskIo { tid: Some(555), header_pid: 9, bytes: 512 })
        );
        // 负载被截断时不解析。
        assert_eq!(parse(DISK_IO_GUID, OPCODE_DISK_READ, 3, FLAG_64_BIT_HEADER, 4, &p[..8]), None);
        // 刷新（14）、预读初始化（12）不是实际传输。
        assert_eq!(parse(DISK_IO_GUID, 14, 3, FLAG_64_BIT_HEADER, 4, &p), None);
        assert_eq!(parse(DISK_IO_GUID, 12, 3, FLAG_64_BIT_HEADER, 4, &p), None);
        // 其他事件类忽略。
        assert_eq!(parse(0x1234, OPCODE_DISK_READ, 3, FLAG_64_BIT_HEADER, 4, &p), None);
    }

    #[test]
    fn aggregates_by_issuing_thread_not_header_pid() {
        let mut a = DiskAggregator::default();
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_DC_START, 3, 0, &thread_payload(100, 200));
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_DC_START, 3, 0, &thread_payload(300, 400));
        // 事件头进程号全是 4（System），但发起线程分属两个进程。
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4, &disk_payload(4096, 200));
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_WRITE, 3, 4, &disk_payload(8192, 200));
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_WRITE, 3, 4, &disk_payload(1000, 400));
        let t = a.take();
        assert_eq!(t.get(&100), Some(&12288));
        assert_eq!(t.get(&300), Some(&1000));
        assert_eq!(t.get(&4), None);
        assert_eq!(a.unresolved, 0);
        // take 清零。
        assert!(a.take().is_empty());
    }

    #[test]
    fn unknown_thread_falls_back_to_header_pid() {
        let mut a = DiskAggregator::default();
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4321, &disk_payload(100, 999));
        // 版本 2 的事件没有发起线程，同样退回事件头。
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 2, 4321, &disk_payload(50, 999));
        assert_eq!(a.take().get(&4321), Some(&150));
        assert_eq!(a.unresolved, 2);
        // 向系统查询到之后，同一线程不再兜底。
        assert!(!a.knows(999));
        a.learn(999, 77);
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4321, &disk_payload(10, 999));
        assert_eq!(a.take().get(&77), Some(&10));
    }

    #[test]
    fn ended_thread_stays_resolvable_until_next_take() {
        let mut a = DiskAggregator::default();
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_START, 3, 0, &thread_payload(100, 200));
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_END, 3, 0, &thread_payload(100, 200));
        // 晚到的磁盘事件仍归属原进程。
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4, &disk_payload(10, 200));
        assert_eq!(a.take().get(&100), Some(&10));
        assert!(!a.knows(200));
    }

    #[test]
    fn reused_thread_id_is_not_dropped_by_stale_end_mark() {
        let mut a = DiskAggregator::default();
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_START, 3, 0, &thread_payload(100, 200));
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_END, 3, 0, &thread_payload(100, 200));
        // 同一线程号被另一个进程的新线程复用。
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_START, 3, 0, &thread_payload(500, 200));
        a.take();
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_WRITE, 3, 4, &disk_payload(7, 200));
        assert_eq!(a.take().get(&500), Some(&7));
    }

    #[test]
    fn rates_divide_by_elapsed() {
        let mut m = HashMap::new();
        m.insert(1, 3_000_000u64);
        let r = rates(&m, 2.0);
        assert_eq!(r[&1], 1_500_000.0);
        assert!(rates(&m, 0.0).is_empty());
    }
}

//! ETW 磁盘事件的解析与按进程聚合：纯函数，不接触系统接口，可用构造的事件序列单测。
//!
//! 事件来自内核 `DiskIo` 与 `Thread` 两类 MOF 事件（见 ADR-0004）。磁盘事件在 DPC 中完成时记录，
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

/// 线程表容量上限（系统同时存在的线程通常不到十万）。窗口不可见时采样线程不取数，聚合器靠上限保持有界。
const MAX_THREADS: usize = 1 << 17;
/// 已结束线程标记的上限，超过就提前回收。
const MAX_ENDED: usize = 1 << 14;
/// 按进程累计表与“查不到”缓存的上限。
const MAX_PIDS: usize = 1 << 16;

/// 一个取数窗口内的累计结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Window {
    /// 每个进程读 + 写的字节数。
    pub bytes: HashMap<u32, u64>,
    /// 窗口内的磁盘事件数，以及其中无法还原发起线程、退回事件头进程号的事件数。
    pub events: u64,
    pub unresolved: u64,
}

/// 窗口内事件太少时不足以判断归属质量，按可信处理。
const MIN_EVENTS_TO_JUDGE: u64 = 20;

impl Window {
    /// “准确”的运行时校验：无法还原发起线程的事件超过 20% 时，按进程的归属不可信，调用方应回退到近似值。
    pub fn trustworthy(&self) -> bool {
        self.events < MIN_EVENTS_TO_JUDGE || self.unresolved * 5 <= self.events
    }
}

/// 按进程累计磁盘字节数。所有表都有容量上限：采样暂停期间事件仍在到达，但内存不会无限增长。
#[derive(Default)]
pub struct DiskAggregator {
    threads: HashMap<u32, u32>,
    /// 已结束的线程：晚到的磁盘事件仍要能还原，所以推迟到下一次 `take`（或超过上限时）才从线程表删除。
    ended: HashSet<u32>,
    /// 向系统查询也查不到所属进程的线程（已退出），本窗口内不再重复查询。
    missing: HashSet<u32>,
    bytes: HashMap<u32, u64>,
    events: u64,
    unresolved: u64,
}

impl DiskAggregator {
    /// 线程表是否已有该线程（没有时调用方可以向系统查询后用 [`learn`](Self::learn) 补上）。
    pub fn knows(&self, tid: u32) -> bool {
        self.threads.contains_key(&tid)
    }

    /// 本窗口内已经向系统查询过且查不到。
    pub fn is_missing(&self, tid: u32) -> bool {
        self.missing.contains(&tid)
    }

    pub fn mark_missing(&mut self, tid: u32) {
        if self.missing.len() >= MAX_PIDS {
            self.missing.clear();
        }
        self.missing.insert(tid);
    }

    pub fn learn(&mut self, tid: u32, pid: u32) {
        self.insert_thread(tid, pid);
    }

    fn insert_thread(&mut self, tid: u32, pid: u32) {
        if self.threads.len() >= MAX_THREADS && !self.threads.contains_key(&tid) {
            self.prune_ended();
            if self.threads.len() >= MAX_THREADS {
                // 仍然满（不应发生）：丢弃线程表，之后靠系统查询重建。
                self.threads.clear();
            }
        }
        self.threads.insert(tid, pid);
    }

    fn prune_ended(&mut self) {
        for t in self.ended.drain() {
            self.threads.remove(&t);
        }
    }

    pub fn apply(&mut self, e: Event) {
        match e {
            Event::ThreadStart { pid, tid } => {
                // 线程号会复用：重新启动的线程不能被上一轮的结束标记删掉。
                self.ended.remove(&tid);
                self.insert_thread(tid, pid);
            }
            Event::ThreadEnd { tid } => {
                self.ended.insert(tid);
                if self.ended.len() >= MAX_ENDED {
                    self.prune_ended();
                }
            }
            Event::DiskIo { tid, header_pid, bytes } => {
                self.events += 1;
                let pid = match tid.and_then(|t| self.threads.get(&t)) {
                    Some(&pid) => pid,
                    None => {
                        self.unresolved += 1;
                        header_pid
                    }
                };
                // 表满时不再接纳新进程（只是少记，不影响已有条目）。
                if self.bytes.len() < MAX_PIDS || self.bytes.contains_key(&pid) {
                    *self.bytes.entry(pid).or_default() += u64::from(bytes);
                }
            }
        }
    }

    /// 取出自上次调用以来的累计结果并清零；顺带回收已结束的线程。
    pub fn take(&mut self) -> Window {
        self.prune_ended();
        self.missing.clear();
        Window {
            bytes: std::mem::take(&mut self.bytes),
            events: std::mem::take(&mut self.events),
            unresolved: std::mem::take(&mut self.unresolved),
        }
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
        let t = a.take().bytes;
        assert_eq!(t.get(&100), Some(&12288));
        assert_eq!(t.get(&300), Some(&1000));
        assert_eq!(t.get(&4), None);
        // take 清零。
        assert_eq!(a.take(), Window::default());
    }

    #[test]
    fn unknown_thread_falls_back_to_header_pid() {
        let mut a = DiskAggregator::default();
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4321, &disk_payload(100, 999));
        // 版本 2 的事件没有发起线程，同样退回事件头。
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 2, 4321, &disk_payload(50, 999));
        let w = a.take();
        assert_eq!(w.bytes.get(&4321), Some(&150));
        assert_eq!((w.events, w.unresolved), (2, 2));
        // 向系统查询到之后，同一线程不再兜底。
        assert!(!a.knows(999));
        a.learn(999, 77);
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4321, &disk_payload(10, 999));
        assert_eq!(a.take().bytes.get(&77), Some(&10));
    }

    #[test]
    fn ended_thread_stays_resolvable_until_next_take() {
        let mut a = DiskAggregator::default();
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_START, 3, 0, &thread_payload(100, 200));
        feed(&mut a, THREAD_GUID, OPCODE_THREAD_END, 3, 0, &thread_payload(100, 200));
        // 晚到的磁盘事件仍归属原进程。
        feed(&mut a, DISK_IO_GUID, OPCODE_DISK_READ, 3, 4, &disk_payload(10, 200));
        assert_eq!(a.take().bytes.get(&100), Some(&10));
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
        assert_eq!(a.take().bytes.get(&500), Some(&7));
    }

    #[test]
    fn tables_stay_bounded_when_never_taken() {
        let mut a = DiskAggregator::default();
        // 模拟窗口长时间不可见：线程不断创建、结束，磁盘事件不断到达，但没有人取数。
        for i in 0..(3 * MAX_THREADS as u32) {
            a.apply(Event::ThreadStart { pid: i % 1000 + 1, tid: i });
            if i % 2 == 0 {
                a.apply(Event::ThreadEnd { tid: i });
            }
            a.apply(Event::DiskIo { tid: Some(i), header_pid: 4, bytes: 1 });
            a.apply(Event::DiskIo { tid: None, header_pid: 1_000_000 + i, bytes: 1 });
            a.mark_missing(i);
        }
        assert!(a.threads.len() <= MAX_THREADS, "threads {}", a.threads.len());
        assert!(a.ended.len() < MAX_ENDED, "ended {}", a.ended.len());
        assert!(a.missing.len() <= MAX_PIDS, "missing {}", a.missing.len());
        assert!(a.bytes.len() <= MAX_PIDS, "bytes {}", a.bytes.len());
        // 取数后仍可正常使用，并清空窗口。
        let w = a.take();
        assert!(w.events > 0);
        assert_eq!(a.take(), Window::default());
        a.apply(Event::ThreadStart { pid: 9, tid: 1 });
        a.apply(Event::DiskIo { tid: Some(1), header_pid: 4, bytes: 5 });
        assert_eq!(a.take().bytes.get(&9), Some(&5));
    }

    #[test]
    fn missing_threads_are_remembered_until_next_take() {
        let mut a = DiskAggregator::default();
        assert!(!a.is_missing(5));
        a.mark_missing(5);
        assert!(a.is_missing(5));
        a.take();
        assert!(!a.is_missing(5));
    }

    #[test]
    fn trustworthy_depends_on_unresolved_share() {
        let w = |events, unresolved| Window { events, unresolved, ..Window::default() };
        // 事件太少不判断。
        assert!(w(0, 0).trustworthy());
        assert!(w(19, 19).trustworthy());
        // 恰好 20% 仍可信，超过则不可信。
        assert!(w(100, 20).trustworthy());
        assert!(!w(100, 21).trustworthy());
        assert!(!w(20, 20).trustworthy());
        assert!(w(1000, 0).trustworthy());
    }

    #[test]
    fn window_from_events_reports_unresolved_share() {
        let mut a = DiskAggregator::default();
        a.apply(Event::ThreadStart { pid: 10, tid: 1 });
        for _ in 0..50 {
            a.apply(Event::DiskIo { tid: Some(1), header_pid: 4, bytes: 1 });
        }
        for _ in 0..50 {
            a.apply(Event::DiskIo { tid: Some(999), header_pid: 4, bytes: 1 });
        }
        let w = a.take();
        assert_eq!((w.events, w.unresolved), (100, 50));
        assert!(!w.trustworthy());
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

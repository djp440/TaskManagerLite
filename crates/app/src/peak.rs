//! 曲线峰值进程的标识（#81）：每个历史点只多存一个很小的 `ProcId`，进程名集中放在这张去重的名称表里。
//!
//! 采样热路径里只做“每次采样最多一次哈希查找”：同一个进程连续成为峰值进程时命中已有条目，不分配；
//! 只有新出现的进程才分配一次名字。表有容量上限，满了淘汰最久没有成为峰值的条目，
//! 对应历史点之后查不到名字（提示里只显示数值与时间），所以内存与运行时长无关。
//!
//! 纯逻辑，不依赖界面，可直接单测。

use std::collections::HashMap;

use tml_core::ProcessKey;

/// 历史点上的峰值进程标识；[`NO_PROC`] 表示没有记录（只采系统指标、该指标没有最高进程、或这类曲线不记录进程）。
pub type ProcId = u32;
pub const NO_PROC: ProcId = 0;

/// 名称表的容量。每个条目只有一个进程名与键，几十 KB 封顶。
const CAPACITY: usize = 512;

struct Entry {
    key: ProcessKey,
    name: String,
    /// 最近一次被记录为峰值进程的序号，用来淘汰最久没用的条目。
    used: u64,
}

pub struct ProcNames {
    by_key: HashMap<ProcessKey, ProcId>,
    entries: HashMap<ProcId, Entry>,
    next: ProcId,
    tick: u64,
    capacity: usize,
}

impl Default for ProcNames {
    fn default() -> Self {
        Self::with_capacity(CAPACITY)
    }
}

impl ProcNames {
    fn with_capacity(capacity: usize) -> Self {
        Self { by_key: HashMap::new(), entries: HashMap::new(), next: NO_PROC, tick: 0, capacity: capacity.max(1) }
    }

    /// 取进程对应的标识；第一次见到才记下名字（分配一次），已有条目只刷新“最近使用”。
    pub fn intern(&mut self, key: ProcessKey, name: &str) -> ProcId {
        self.tick += 1;
        if let Some(&id) = self.by_key.get(&key) {
            if let Some(e) = self.entries.get_mut(&id) {
                e.used = self.tick;
            }
            return id;
        }
        if self.entries.len() >= self.capacity
            && let Some((&oldest, _)) = self.entries.iter().min_by_key(|(_, e)| e.used)
            && let Some(e) = self.entries.remove(&oldest)
        {
            self.by_key.remove(&e.key);
        }
        self.next += 1;
        self.by_key.insert(key, self.next);
        self.entries.insert(self.next, Entry { key, name: name.to_owned(), used: self.tick });
        self.next
    }

    /// 标识对应的 (PID, 进程名)；未记录或已被淘汰为 `None`。
    pub fn get(&self, id: ProcId) -> Option<(u32, &str)> {
        self.entries.get(&id).map(|e| (e.key.pid, e.name.as_str()))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: u32) -> ProcessKey {
        ProcessKey { pid, create_time: 1 }
    }

    #[test]
    fn same_process_reuses_one_id_and_one_name() {
        let mut t = ProcNames::default();
        let a = t.intern(key(10), "a.exe");
        assert_ne!(a, NO_PROC);
        for _ in 0..1000 {
            assert_eq!(t.intern(key(10), "a.exe"), a);
        }
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(a), Some((10, "a.exe")));
        assert_eq!(t.get(NO_PROC), None);
    }

    #[test]
    fn reused_pid_with_new_create_time_is_a_different_process() {
        let mut t = ProcNames::default();
        let a = t.intern(key(10), "old.exe");
        let b = t.intern(ProcessKey { pid: 10, create_time: 2 }, "new.exe");
        assert_ne!(a, b);
        assert_eq!(t.get(a), Some((10, "old.exe")));
        assert_eq!(t.get(b), Some((10, "new.exe")));
    }

    #[test]
    fn table_is_bounded_and_evicts_the_least_recently_used() {
        let mut t = ProcNames::with_capacity(4);
        let ids: Vec<_> = (1..=4).map(|p| t.intern(key(p), &format!("p{p}"))).collect();
        // 第 1 个再次成为峰值：最久没用的变成第 2 个。
        t.intern(key(1), "p1");
        let fifth = t.intern(key(5), "p5");
        assert_eq!(t.len(), 4);
        assert_eq!(t.get(ids[1]), None, "最久没用的被淘汰，旧历史点查不到名字");
        assert_eq!(t.get(ids[0]), Some((1, "p1")));
        assert_eq!(t.get(fifth), Some((5, "p5")));
        // 被淘汰的进程再出现拿到新标识，不会与别的进程混淆。
        let again = t.intern(key(2), "p2");
        assert!(!ids.contains(&again) && again != fifth);
        for p in 100..2000u32 {
            t.intern(key(p), "x");
        }
        assert_eq!(t.len(), 4, "无论多少进程来来去去，表都不增长");
    }
}

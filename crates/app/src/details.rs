//! “详细信息”列在界面侧的部分：列的标题与默认宽度、单元格文字，以及静态信息的后台读取与缓存。
//!
//! 静态信息（用户名、路径、命令行、架构、是否提权、描述、公司）只在显示了这类列时才读取：
//! 新出现的进程交给后台线程读一次，结果按进程缓存到进程退出，不进采样热路径。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use tml_core::{DetailColumn, Process, ProcessKey, StaticInfo, StaticReader};

use crate::format;

/// 可选列的宽度范围（逻辑像素）。命令行、路径往往很长，上限比基本列宽。
pub const MIN_WIDTH: f32 = 40.0;
pub const MAX_WIDTH: f32 = 1200.0;

/// 列标题。
pub fn title(c: DetailColumn) -> &'static str {
    match c {
        DetailColumn::Status => "状态",
        DetailColumn::User => "用户名",
        DetailColumn::Session => "会话 ID",
        DetailColumn::Priority => "优先级",
        DetailColumn::Handles => "句柄",
        DetailColumn::Threads => "线程",
        DetailColumn::WorkingSet => "工作集",
        DetailColumn::PeakWorkingSet => "峰值工作集",
        DetailColumn::Commit => "提交大小",
        DetailColumn::CpuTime => "CPU 时间",
        DetailColumn::StartTime => "启动时间",
        DetailColumn::Path => "映像路径",
        DetailColumn::CommandLine => "命令行",
        DetailColumn::Arch => "架构",
        DetailColumn::Elevated => "已提权",
        DetailColumn::Description => "描述",
        DetailColumn::Company => "公司",
    }
}

/// 列的默认宽度（逻辑像素）。
pub fn default_width(c: DetailColumn) -> f32 {
    match c {
        DetailColumn::Session | DetailColumn::Threads | DetailColumn::Arch => 72.0,
        DetailColumn::Elevated | DetailColumn::Handles => 76.0,
        DetailColumn::Status | DetailColumn::Priority | DetailColumn::CpuTime => 88.0,
        DetailColumn::WorkingSet | DetailColumn::PeakWorkingSet | DetailColumn::Commit => 108.0,
        DetailColumn::User => 120.0,
        DetailColumn::StartTime => 156.0,
        DetailColumn::Company => 180.0,
        DetailColumn::Description => 220.0,
        DetailColumn::Path => 300.0,
        DetailColumn::CommandLine => 360.0,
    }
}

/// 数值列右对齐，文字列左对齐。
pub fn right_aligned(c: DetailColumn) -> bool {
    matches!(
        c,
        DetailColumn::Session
            | DetailColumn::Handles
            | DetailColumn::Threads
            | DetailColumn::WorkingSet
            | DetailColumn::PeakWorkingSet
            | DetailColumn::Commit
            | DetailColumn::CpuTime
    )
}

/// 点击列标题时的默认排序方向：数值与时间降序，文字升序。
pub fn default_descending(c: DetailColumn) -> bool {
    right_aligned(c)
        || matches!(
            c,
            DetailColumn::Status | DetailColumn::Priority | DetailColumn::Elevated
        )
}

/// 基本优先级的名称，与原生任务管理器一致；不在常见档位上的显示数值。
fn priority(base: i32) -> String {
    match base {
        24.. => "实时".into(),
        13..=23 => "高".into(),
        10..=12 => "高于正常".into(),
        8 | 9 => "正常".into(),
        6 | 7 => "低于正常".into(),
        1..=5 => "低".into(),
        other => other.to_string(),
    }
}

/// CPU 时间（100ns）显示为 `时:分:秒`。
pub fn cpu_time(t: u64) -> String {
    let secs = t / 10_000_000;
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// 单元格文字。静态信息尚未读到或读不到时为空。
pub fn cell(p: &Process, c: DetailColumn) -> String {
    let n = &p.counters;
    let info = p.info.as_deref();
    let text =
        |f: fn(&StaticInfo) -> Option<&str>| info.and_then(f).unwrap_or_default().to_string();
    match c {
        DetailColumn::Status => if n.suspended {
            "已挂起"
        } else {
            "正在运行"
        }
        .into(),
        DetailColumn::Session => n.session_id.to_string(),
        DetailColumn::Priority => priority(n.base_priority),
        DetailColumn::Handles => n.handles.to_string(),
        DetailColumn::Threads => n.threads.to_string(),
        DetailColumn::WorkingSet => format::megabytes(n.working_set),
        DetailColumn::PeakWorkingSet => format::megabytes(n.peak_working_set),
        DetailColumn::Commit => format::megabytes(n.commit),
        DetailColumn::CpuTime => cpu_time(n.cpu_time),
        DetailColumn::StartTime => tml_core::local_time_text(p.key.create_time),
        DetailColumn::User => text(|i| i.user.as_deref()),
        DetailColumn::Path => info
            .and_then(|i| i.path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        DetailColumn::CommandLine => text(|i| i.command_line.as_deref()),
        DetailColumn::Arch => info
            .and_then(|i| i.arch)
            .map(|a| a.label())
            .unwrap_or_default(),
        DetailColumn::Elevated => match info.and_then(|i| i.elevated) {
            Some(true) => "是".into(),
            Some(false) => "否".into(),
            None => String::new(),
        },
        DetailColumn::Description => text(|i| i.description.as_deref()),
        DetailColumn::Company => text(|i| i.company.as_deref()),
    }
}

/// 显示中的可选列（有序）与各自宽度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub column: DetailColumn,
    pub width: f32,
}

/// 静态信息缓存：按进程存放后台读到的结果，进程退出后清除。
#[derive(Default)]
pub struct InfoCache {
    /// `None` 表示已交给后台、结果未到。
    procs: HashMap<ProcessKey, Option<Arc<StaticInfo>>>,
}

impl InfoCache {
    /// 以最新快照的存活进程同步缓存，返回新出现、需要后台读取的进程。
    pub fn track(&mut self, live: impl IntoIterator<Item = ProcessKey>) -> Vec<ProcessKey> {
        let live: HashSet<ProcessKey> = live.into_iter().collect();
        self.procs.retain(|k, _| live.contains(k));
        let mut fresh = Vec::new();
        for key in live {
            self.procs.entry(key).or_insert_with(|| {
                fresh.push(key);
                None
            });
        }
        fresh
    }

    /// 后台读到结果。读取期间进程已退出（不在缓存里）时丢弃，返回是否被采用。
    pub fn loaded(&mut self, key: ProcessKey, info: StaticInfo) -> bool {
        match self.procs.get_mut(&key) {
            Some(slot @ None) => {
                *slot = Some(Arc::new(info));
                true
            }
            _ => false,
        }
    }

    /// 读取期间 PID 被复用（读不到）：从缓存里去掉，下次快照时重新读取。
    pub fn forget(&mut self, key: ProcessKey) {
        self.procs.remove(&key);
    }

    pub fn get(&self, key: ProcessKey) -> Option<Arc<StaticInfo>> {
        self.procs.get(&key)?.clone()
    }

    /// 不再显示静态列时释放全部缓存。
    pub fn clear(&mut self) {
        self.procs.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.procs.len()
    }
}

/// 后台读取结果每读完这么多个进程投递一次。
const CHUNK: usize = 32;

/// 后台读取线程。结果分块送回：每读完 [`CHUNK`] 个进程投递一次，界面每块只重建一次列表。
#[derive(Clone)]

pub struct InfoWorker {
    tx: Sender<Vec<ProcessKey>>,
}

impl InfoWorker {
    /// `deliver` 在后台线程上调用，负责把结果投递到界面线程。
    pub fn spawn(deliver: impl Fn(Vec<(ProcessKey, Option<StaticInfo>)>) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<ProcessKey>>();
        std::thread::Builder::new()
            .name("details".into())
            .spawn(move || {
                let mut reader = StaticReader::default();
                while let Ok(keys) = rx.recv() {
                    // 分块投递：首次显示静态列时要读几百个进程，个别读取（网络路径上的版本资源、
                    // 域账户名）可能很慢，不让它们拖住全部结果。
                    for chunk in keys.chunks(CHUNK) {
                        deliver(chunk.iter().map(|&k| (k, reader.read(k))).collect());
                    }
                }
            })
            .expect("无法创建详细信息线程");
        Self { tx }
    }

    pub fn read(&self, keys: Vec<ProcessKey>) {
        if !keys.is_empty() {
            let _ = self.tx.send(keys);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: u32) -> ProcessKey {
        ProcessKey {
            pid,
            create_time: pid as u64,
        }
    }

    #[test]
    fn cache_reads_each_process_once_and_forgets_exited_ones() {
        let mut c = InfoCache::default();
        assert_eq!(c.track([key(1), key(2)]).len(), 2);
        assert!(
            c.track([key(1), key(2)]).is_empty(),
            "已在读取中的进程不重复请求"
        );
        assert!(c.loaded(
            key(1),
            StaticInfo {
                user: Some("u".into()),
                ..Default::default()
            }
        ));
        assert!(!c.loaded(key(1), StaticInfo::default()), "重复结果不覆盖");
        assert_eq!(c.get(key(1)).unwrap().user.as_deref(), Some("u"));
        assert!(c.get(key(2)).is_none(), "结果未到");
        assert_eq!(c.track([key(2), key(3)]), vec![key(3)]);
        assert_eq!(c.len(), 2, "退出的进程被清除");
        assert!(
            !c.loaded(key(1), StaticInfo::default()),
            "已退出进程的迟到结果被丢弃"
        );
        c.forget(key(2));
        assert_eq!(
            c.track([key(2), key(3)]),
            vec![key(2)],
            "读不到的进程下次重新读取"
        );
    }

    #[test]
    fn every_column_has_title_and_sane_width() {
        for c in DetailColumn::ALL {
            assert!(!title(c).is_empty());
            assert!((MIN_WIDTH..=MAX_WIDTH).contains(&default_width(c)));
        }
    }

    #[test]
    fn formats_priority_and_cpu_time() {
        assert_eq!(priority(8), "正常");
        assert_eq!(priority(13), "高");
        assert_eq!(priority(4), "低");
        assert_eq!(priority(24), "实时");
        assert_eq!(priority(6), "低于正常");
        assert_eq!(priority(10), "高于正常");
        assert_eq!(cpu_time(0), "0:00:00");
        // 1 小时 2 分 3 秒。
        assert_eq!(cpu_time(3723 * 10_000_000 + 9_999_999), "1:02:03");
    }

    #[test]
    fn static_cells_are_blank_until_loaded() {
        let p = Process {
            key: key(9),
            parent_pid: 0,
            name: "x".into(),
            cpu_usage: 0.0,
            private_working_set: 0,
            disk_rate: 0.0,
            critical: false,
            counters: tml_core::Counters {
                suspended: true,
                handles: 12,
                ..Default::default()
            },
            info: None,
        };
        assert_eq!(cell(&p, DetailColumn::Status), "已挂起");
        assert_eq!(cell(&p, DetailColumn::Handles), "12");
        assert_eq!(cell(&p, DetailColumn::User), "");
        let p = Process {
            info: Some(Arc::new(StaticInfo {
                elevated: Some(true),
                ..Default::default()
            })),
            ..p
        };
        assert_eq!(cell(&p, DetailColumn::Elevated), "是");
    }
}

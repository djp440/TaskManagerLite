//! 把进程快照整理成进程列表要显示的行：排序、搜索、进程树。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::snapshot::{Process, ProcessKey, ProcessSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Pid,
    Cpu,
    Memory,
    Disk,
}

#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub sort_key: SortKey,
    pub descending: bool,
    pub tree: bool,
    /// 按名称（不区分大小写）或 PID 前缀匹配；空串表示不过滤。
    pub filter: String,
    /// 进程树中被折叠的节点。
    pub collapsed: HashSet<ProcessKey>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            sort_key: SortKey::Cpu,
            descending: true,
            tree: false,
            filter: String::new(),
            collapsed: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    /// 在 `ProcessSnapshot::processes` 中的下标。
    pub index: usize,
    pub depth: u32,
    pub has_children: bool,
    pub expanded: bool,
}

/// 父进程在快照中的下标。只有父进程早于子进程创建，才算真正的父子关系；
/// 否则子进程是孤儿进程。
pub fn parent_index(snapshot: &ProcessSnapshot) -> Vec<Option<usize>> {
    let by_pid: HashMap<u32, usize> =
        snapshot.processes.iter().enumerate().map(|(i, p)| (p.key.pid, i)).collect();
    snapshot
        .processes
        .iter()
        .map(|p| {
            let &pi = by_pid.get(&p.parent_pid)?;
            let parent = &snapshot.processes[pi];
            (parent.key.pid != p.key.pid && parent.key.create_time < p.key.create_time).then_some(pi)
        })
        .collect()
}

fn children_of(parents: &[Option<usize>]) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); parents.len()];
    for (i, p) in parents.iter().enumerate() {
        if let Some(pi) = p {
            children[*pi].push(i);
        }
    }
    children
}

fn compare(a: &Process, b: &Process, key: SortKey) -> Ordering {
    let primary = match key {
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::Pid => a.key.pid.cmp(&b.key.pid),
        SortKey::Cpu => a.cpu_usage.total_cmp(&b.cpu_usage),
        SortKey::Memory => a.private_working_set.cmp(&b.private_working_set),
        SortKey::Disk => a.disk_rate.total_cmp(&b.disk_rate),
    };
    primary.then_with(|| a.key.pid.cmp(&b.key.pid))
}

fn matches(p: &Process, needle: &str) -> bool {
    p.name.to_lowercase().contains(needle) || p.key.pid.to_string().starts_with(needle)
}

pub fn build_view(snapshot: &ProcessSnapshot, opts: &ViewOptions) -> Vec<ViewRow> {
    let procs = &snapshot.processes;
    let needle = opts.filter.trim().to_lowercase();
    let order = |a: &usize, b: &usize| {
        let o = compare(&procs[*a], &procs[*b], opts.sort_key);
        if opts.descending { o.reverse() } else { o }
    };

    if !opts.tree {
        let mut idx: Vec<usize> =
            (0..procs.len()).filter(|&i| needle.is_empty() || matches(&procs[i], &needle)).collect();
        idx.sort_by(order);
        return idx
            .into_iter()
            .map(|index| ViewRow { index, depth: 0, has_children: false, expanded: false })
            .collect();
    }

    let parents = parent_index(snapshot);
    let mut children = children_of(&parents);
    let mut roots: Vec<usize> = (0..procs.len()).filter(|&i| parents[i].is_none()).collect();

    // 搜索时保留命中的进程及其全部祖先，并强制展开，保证命中项可见。
    let visible: Option<Vec<bool>> = (!needle.is_empty()).then(|| {
        let mut keep = vec![false; procs.len()];
        for i in 0..procs.len() {
            if matches(&procs[i], &needle) {
                let mut cur = Some(i);
                while let Some(c) = cur {
                    if keep[c] {
                        break;
                    }
                    keep[c] = true;
                    cur = parents[c];
                }
            }
        }
        keep
    });
    let shown = |i: usize| visible.as_ref().is_none_or(|v| v[i]);

    for list in children.iter_mut() {
        list.sort_by(order);
    }
    roots.sort_by(order);

    let mut out = Vec::with_capacity(procs.len());
    let mut stack: Vec<(usize, u32)> = roots.into_iter().rev().filter(|&i| shown(i)).map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let kids: Vec<usize> = children[i].iter().copied().filter(|&c| shown(c)).collect();
        let expanded = visible.is_some() || !opts.collapsed.contains(&procs[i].key);
        out.push(ViewRow { index: i, depth, has_children: !kids.is_empty(), expanded });
        if expanded {
            stack.extend(kids.into_iter().rev().map(|c| (c, depth + 1)));
        }
    }
    out
}

/// 某进程在进程树中的全部后代，后代在前、自身在最后，便于自底向上结束。
pub fn tree_from(snapshot: &ProcessSnapshot, root: ProcessKey) -> Vec<ProcessKey> {
    let procs = &snapshot.processes;
    let Some(root_i) = procs.iter().position(|p| p.key == root) else {
        return Vec::new();
    };
    let children = children_of(&parent_index(snapshot));
    let mut out = Vec::new();
    // 用显式栈做后序遍历，避免极深进程链导致栈溢出。
    let mut stack = vec![(root_i, false)];
    while let Some((i, visited)) = stack.pop() {
        if visited {
            out.push(procs[i].key);
        } else {
            stack.push((i, true));
            stack.extend(children[i].iter().map(|&c| (c, false)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, create: u64, name: &str, cpu: f32) -> Process {
        Process {
            key: ProcessKey { pid, create_time: create },
            parent_pid: parent,
            name: name.into(),
            cpu_usage: cpu,
            private_working_set: 0,
            disk_rate: 0.0,
            critical: false,
        }
    }

    fn snap(v: Vec<Process>) -> ProcessSnapshot {
        ProcessSnapshot { processes: v }
    }

    #[test]
    fn flat_sorts_by_cpu_desc_by_default() {
        let s = snap(vec![p(1, 0, 1, "a", 1.0), p(2, 0, 1, "b", 5.0), p(3, 0, 1, "c", 3.0)]);
        let rows = build_view(&s, &ViewOptions::default());
        let pids: Vec<u32> = rows.iter().map(|r| s.processes[r.index].key.pid).collect();
        assert_eq!(pids, [2, 3, 1]);
    }

    #[test]
    fn filter_matches_name_case_insensitive_or_pid_prefix() {
        let s = snap(vec![p(1234, 0, 1, "Chrome.exe", 0.0), p(99, 0, 1, "code.exe", 0.0)]);
        let opts = ViewOptions { filter: "CHROME".into(), ..Default::default() };
        assert_eq!(build_view(&s, &opts).len(), 1);
        let opts = ViewOptions { filter: "12".into(), ..Default::default() };
        assert_eq!(s.processes[build_view(&s, &opts)[0].index].key.pid, 1234);
    }

    #[test]
    fn reused_parent_pid_makes_orphan() {
        // 子进程 20 的父 PID 10 已被一个更晚创建的进程复用。
        let s = snap(vec![p(10, 0, 500, "newcomer", 0.0), p(20, 10, 100, "child", 0.0)]);
        let opts = ViewOptions { tree: true, ..Default::default() };
        let rows = build_view(&s, &opts);
        assert!(rows.iter().all(|r| r.depth == 0));
        assert_eq!(tree_from(&s, s.processes[0].key), vec![s.processes[0].key]);
    }

    #[test]
    fn tree_nests_and_collapses() {
        let s = snap(vec![p(1, 0, 1, "root", 0.0), p(2, 1, 2, "kid", 0.0), p(3, 2, 3, "grandkid", 0.0)]);
        let mut opts = ViewOptions { tree: true, ..Default::default() };
        let depths: Vec<u32> = build_view(&s, &opts).iter().map(|r| r.depth).collect();
        assert_eq!(depths, [0, 1, 2]);
        opts.collapsed.insert(s.processes[1].key);
        let rows = build_view(&s, &opts);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].has_children && !rows[1].expanded);
    }

    #[test]
    fn tree_search_keeps_ancestors_expanded() {
        let s = snap(vec![p(1, 0, 1, "root", 0.0), p(2, 1, 2, "kid", 0.0), p(3, 0, 1, "other", 0.0)]);
        let mut opts = ViewOptions { tree: true, filter: "kid".into(), ..Default::default() };
        opts.collapsed.insert(s.processes[0].key);
        let names: Vec<&str> = build_view(&s, &opts).iter().map(|r| s.processes[r.index].name.as_str()).collect();
        assert_eq!(names, ["root", "kid"]);
    }

    #[test]
    fn tree_from_lists_descendants_first() {
        let s = snap(vec![p(1, 0, 1, "root", 0.0), p(2, 1, 2, "kid", 0.0), p(3, 2, 3, "grandkid", 0.0)]);
        let pids: Vec<u32> = tree_from(&s, s.processes[0].key).iter().map(|k| k.pid).collect();
        assert_eq!(pids, [3, 2, 1]);
    }
}

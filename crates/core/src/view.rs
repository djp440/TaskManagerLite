//! 把进程快照整理成进程列表要显示的行：排序、搜索、进程树、按应用分组。

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

/// 进程列表的视图模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// 平铺：所有进程一个列表。
    #[default]
    Flat,
    /// 树形：按父子关系缩进。
    Tree,
    /// 分组：分为“应用”和“后台进程”两组，见 [`build_group_view`]。
    Group,
}

#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub sort_key: SortKey,
    pub descending: bool,
    pub mode: ViewMode,
    /// 按名称（不区分大小写）或 PID 前缀匹配；空串表示不过滤。
    pub filter: String,
    /// 树形模式中被折叠的节点（树形默认展开）。
    pub collapsed: HashSet<ProcessKey>,
    /// 分组模式中被展开的应用（分组默认折叠，应用的后代收在它的一行下）。
    pub expanded: HashSet<ProcessKey>,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            sort_key: SortKey::Cpu,
            descending: true,
            mode: ViewMode::Flat,
            filter: String::new(),
            collapsed: HashSet::new(),
            expanded: HashSet::new(),
        }
    }
}

/// 分组模式中的组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// 应用：有可见顶层窗口的进程及其后代。
    Apps,
    /// 后台进程：其余进程。
    Background,
}

/// 分组标题行的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupHeader {
    pub group: Group,
    /// 组内（搜索过滤后）的条目数：应用组数应用（后代不计），后台组数进程。
    pub count: usize,
}

/// 标题行的 `index`：它不对应任何进程，误当下标用会立即越界而不是悄悄指向别的进程。
pub const HEADER_INDEX: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct ViewRow {
    /// 在 `ProcessSnapshot::processes` 中的下标；标题行为 [`HEADER_INDEX`]。
    pub index: usize,
    pub depth: u32,
    pub has_children: bool,
    pub expanded: bool,
    /// 分组模式的标题行。
    pub header: Option<GroupHeader>,
}

impl ViewRow {
    /// 进程行对应的快照下标；标题行返回 `None`。
    pub fn process(&self) -> Option<usize> {
        self.header.is_none().then_some(self.index)
    }
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

    // 分组模式由 `build_group_view` 处理（需要应用 PID 集合）；这里按平铺处理。
    if opts.mode != ViewMode::Tree {
        let mut idx: Vec<usize> =
            (0..procs.len()).filter(|&i| needle.is_empty() || matches(&procs[i], &needle)).collect();
        idx.sort_by(order);
        return idx
            .into_iter()
            .map(|index| ViewRow { index, depth: 0, has_children: false, expanded: false, header: None })
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
        out.push(ViewRow { index: i, depth, has_children: !kids.is_empty(), expanded, header: None });
        if expanded {
            stack.extend(kids.into_iter().rev().map(|c| (c, depth + 1)));
        }
    }
    out
}

/// 不向下收纳后代的进程：桌面外壳是用户从桌面、开始菜单启动的所有程序的父进程，
/// 它自己有窗口（文件资源管理器）时，若收纳后代，几乎所有程序都会被挤进它的名下。
fn adopts_descendants(p: &Process) -> bool {
    !p.name.eq_ignore_ascii_case("explorer.exe")
}

/// 每个进程所属的组。有窗口的进程是应用；其余进程跟随父进程的组（后代归入应用），
/// 没有父进程（孤儿进程）或父进程不收纳后代时属于后台进程。
fn group_of(procs: &[Process], parents: &[Option<usize>], apps: &HashSet<u32>) -> Vec<Group> {
    let mut group: Vec<Option<Group>> = vec![None; procs.len()];
    let mut chain = Vec::new();
    for start in 0..procs.len() {
        // 沿父进程向上直到遇到已确定的进程、应用进程或根，再自上而下回填。
        let mut cur = Some(start);
        while let Some(c) = cur.filter(|&c| group[c].is_none()) {
            chain.push(c);
            cur = if apps.contains(&procs[c].key.pid) { None } else { parents[c] };
        }
        while let Some(c) = chain.pop() {
            group[c] = Some(if apps.contains(&procs[c].key.pid) {
                Group::Apps
            } else {
                match parents[c] {
                    // 链上的父进程已先于子进程回填，或此前已确定。
                    Some(p) if adopts_descendants(&procs[p]) => group[p].unwrap_or(Group::Background),
                    _ => Group::Background,
                }
            });
        }
    }
    group.into_iter().map(|g| g.unwrap_or(Group::Background)).collect()
}

/// 分组视图：先“应用”后“后台进程”，各有一个标题行（组为空时不显示）。
///
/// - `apps`：有可见顶层窗口的进程 PID（采样线程用 `EnumWindows` 得到）。
/// - 应用组：每个应用进程是一个根，其后代按进程树收在下面，默认折叠（`opts.expanded` 里的才展开）。
///   应用启动的另一个应用（也有窗口）自成一个根。
/// - 后台进程组：平铺，不嵌套。
/// - 搜索与排序在组内生效；应用组搜索时与树形一致：保留命中项及其祖先并强制展开。
pub fn build_group_view(snapshot: &ProcessSnapshot, apps: &HashSet<u32>, opts: &ViewOptions) -> Vec<ViewRow> {
    let procs = &snapshot.processes;
    let needle = opts.filter.trim().to_lowercase();
    let order = |a: &usize, b: &usize| {
        let o = compare(&procs[*a], &procs[*b], opts.sort_key);
        if opts.descending { o.reverse() } else { o }
    };
    let parents = parent_index(snapshot);
    let group = group_of(procs, &parents, apps);
    let in_group = |i: usize, g: Group| group[i] == g;

    // 应用组的树：应用进程是根，其余成员挂在父进程下。
    let mut children = vec![Vec::new(); procs.len()];
    let mut roots = Vec::new();
    for i in (0..procs.len()).filter(|&i| in_group(i, Group::Apps)) {
        if apps.contains(&procs[i].key.pid) {
            roots.push(i);
        } else if let Some(p) = parents[i] {
            children[p].push(i);
        }
    }
    let searching = !needle.is_empty();
    let mut keep = vec![!searching; procs.len()];
    if searching {
        for i in (0..procs.len()).filter(|&i| in_group(i, Group::Apps) && matches(&procs[i], &needle)) {
            let mut cur = Some(i);
            while let Some(c) = cur.filter(|&c| in_group(c, Group::Apps) && !keep[c]) {
                keep[c] = true;
                // 应用进程是根，不再向上（它的父进程不属于应用树）。
                cur = if apps.contains(&procs[c].key.pid) { None } else { parents[c] };
            }
        }
    }
    roots.retain(|&i| keep[i]);
    roots.sort_by(order);
    for list in children.iter_mut() {
        list.retain(|&c| keep[c]);
        list.sort_by(order);
    }

    let mut out = Vec::with_capacity(procs.len() + 2);
    if !roots.is_empty() {
        out.push(ViewRow {
            index: HEADER_INDEX,
            depth: 0,
            has_children: false,
            expanded: false,
            header: Some(GroupHeader { group: Group::Apps, count: roots.len() }),
        });
    }
    let mut stack: Vec<(usize, u32)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let expanded = searching || opts.expanded.contains(&procs[i].key);
        out.push(ViewRow { index: i, depth, has_children: !children[i].is_empty(), expanded, header: None });
        if expanded {
            stack.extend(children[i].iter().rev().map(|&c| (c, depth + 1)));
        }
    }

    let mut background: Vec<usize> = (0..procs.len())
        .filter(|&i| in_group(i, Group::Background) && (!searching || matches(&procs[i], &needle)))
        .collect();
    background.sort_by(order);
    if !background.is_empty() {
        out.push(ViewRow {
            index: HEADER_INDEX,
            depth: 0,
            has_children: false,
            expanded: false,
            header: Some(GroupHeader { group: Group::Background, count: background.len() }),
        });
        out.extend(background.into_iter().map(|index| ViewRow {
            index,
            depth: 0,
            has_children: false,
            expanded: false,
            header: None,
        }));
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
        let opts = ViewOptions { mode: ViewMode::Tree, ..Default::default() };
        let rows = build_view(&s, &opts);
        assert!(rows.iter().all(|r| r.depth == 0));
        assert_eq!(tree_from(&s, s.processes[0].key), vec![s.processes[0].key]);
    }

    #[test]
    fn tree_nests_and_collapses() {
        let s = snap(vec![p(1, 0, 1, "root", 0.0), p(2, 1, 2, "kid", 0.0), p(3, 2, 3, "grandkid", 0.0)]);
        let mut opts = ViewOptions { mode: ViewMode::Tree, ..Default::default() };
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
        let mut opts = ViewOptions { mode: ViewMode::Tree, filter: "kid".into(), ..Default::default() };
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

    // ---- 分组视图 ----

    fn apps(pids: &[u32]) -> HashSet<u32> {
        pids.iter().copied().collect()
    }

    /// 按 PID 升序、展开全部应用的分组选项（分组默认折叠，测试树结构时需要展开）。
    fn group_opts(s: &ProcessSnapshot) -> ViewOptions {
        let expanded = s.processes.iter().map(|p| p.key).collect();
        ViewOptions { mode: ViewMode::Group, sort_key: SortKey::Pid, descending: false, expanded, ..Default::default() }
    }

    /// 把视图渲染成 "H:Apps(2)" / "名称@深度" 的文本，便于整体断言。
    fn show(s: &ProcessSnapshot, rows: &[ViewRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r.header {
                Some(h) => format!("H:{:?}({})", h.group, h.count),
                None => format!("{}@{}", s.processes[r.index].name, r.depth),
            })
            .collect()
    }

    #[test]
    fn group_descendants_follow_their_app() {
        // chrome(10) 有窗口，渲染进程 11、12 无窗口，孙进程 13；notepad(20) 有窗口；svc(30) 无窗口。
        let s = snap(vec![
            p(10, 1, 10, "chrome", 0.0),
            p(11, 10, 11, "renderer1", 0.0),
            p(12, 10, 12, "renderer2", 0.0),
            p(13, 12, 13, "gpu", 0.0),
            p(20, 1, 20, "notepad", 0.0),
            p(30, 1, 30, "svc", 0.0),
        ]);
        let rows = build_group_view(&s, &apps(&[10, 20]), &group_opts(&s));
        assert_eq!(
            show(&s, &rows),
            ["H:Apps(2)", "chrome@0", "renderer1@1", "renderer2@1", "gpu@2", "notepad@0", "H:Background(1)", "svc@0"]
        );
        assert!(rows[1].has_children && rows[1].expanded && !rows[5].has_children);
    }

    #[test]
    fn group_each_process_appears_once() {
        let s = snap(vec![
            p(10, 1, 10, "app", 0.0),
            p(11, 10, 11, "kid", 0.0),
            p(12, 11, 12, "grandkid", 0.0),
            p(20, 1, 20, "bg", 0.0),
            p(21, 20, 21, "bgkid", 0.0),
        ]);
        let rows = build_group_view(&s, &apps(&[10]), &group_opts(&s));
        let mut seen: Vec<usize> = rows.iter().filter_map(ViewRow::process).collect();
        assert_eq!(seen.len(), s.processes.len());
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), s.processes.len(), "同一进程不能出现两次");
        // 后台进程的后代仍在后台组，且平铺。
        assert_eq!(show(&s, &rows)[4..], ["H:Background(2)", "bg@0", "bgkid@0"]);
    }

    #[test]
    fn group_app_started_by_app_is_its_own_root() {
        // 资源管理器(10) 有窗口，用户从中打开了记事本(11，有窗口)，还有一个后台工具(12，无窗口)。
        let s = snap(vec![
            p(10, 1, 10, "explorer.exe", 0.0),
            p(11, 10, 11, "notepad", 0.0),
            p(12, 10, 12, "tool", 0.0),
            p(13, 11, 13, "notepad-helper", 0.0),
        ]);
        let rows = build_group_view(&s, &apps(&[10, 11]), &group_opts(&s));
        // explorer 不收纳后代；记事本自成一个根并收纳自己的后代；tool 属于后台进程。
        assert_eq!(
            show(&s, &rows),
            ["H:Apps(2)", "explorer.exe@0", "notepad@0", "notepad-helper@1", "H:Background(1)", "tool@0"]
        );
    }

    #[test]
    fn group_non_explorer_app_adopts_non_window_children() {
        let s = snap(vec![p(10, 1, 10, "launcher", 0.0), p(12, 10, 12, "tool", 0.0)]);
        let rows = build_group_view(&s, &apps(&[10]), &group_opts(&s));
        assert_eq!(show(&s, &rows), ["H:Apps(1)", "launcher@0", "tool@1"]);
    }

    #[test]
    fn group_orphan_without_window_is_background() {
        // 20 的父进程 PID 已被更晚创建的进程复用，不能算 10 的后代。
        let s = snap(vec![p(10, 1, 500, "newcomer", 0.0), p(20, 10, 100, "orphan", 0.0)]);
        let rows = build_group_view(&s, &apps(&[10]), &group_opts(&s));
        assert_eq!(show(&s, &rows), ["H:Apps(1)", "newcomer@0", "H:Background(1)", "orphan@0"]);
    }

    #[test]
    fn group_sorts_within_each_group() {
        let s = snap(vec![
            p(1, 0, 1, "a-app", 1.0),
            p(2, 0, 1, "b-app", 9.0),
            p(3, 2, 2, "b-kid-low", 1.0),
            p(4, 2, 2, "b-kid-high", 5.0),
            p(5, 0, 1, "bg-low", 2.0),
            p(6, 0, 1, "bg-high", 8.0),
        ]);
        let opts = ViewOptions { expanded: group_opts(&s).expanded, mode: ViewMode::Group, ..Default::default() }; // CPU 降序
        let rows = build_group_view(&s, &apps(&[1, 2]), &opts);
        assert_eq!(
            show(&s, &rows),
            ["H:Apps(2)", "b-app@0", "b-kid-high@1", "b-kid-low@1", "a-app@0", "H:Background(2)", "bg-high@0", "bg-low@0"]
        );
    }

    #[test]
    fn group_apps_are_collapsed_by_default() {
        let s = snap(vec![p(1, 0, 1, "app", 0.0), p(2, 1, 2, "kid", 0.0)]);
        let mut opts = ViewOptions { mode: ViewMode::Group, ..Default::default() };
        let rows = build_group_view(&s, &apps(&[1]), &opts);
        assert_eq!(show(&s, &rows), ["H:Apps(1)", "app@0"]);
        assert!(rows[1].has_children && !rows[1].expanded);
        opts.expanded.insert(s.processes[0].key);
        assert_eq!(show(&s, &build_group_view(&s, &apps(&[1]), &opts)), ["H:Apps(1)", "app@0", "kid@1"]);
    }

    #[test]
    fn group_search_filters_within_groups_and_keeps_ancestors() {
        let s = snap(vec![
            p(1, 0, 1, "chrome", 0.0),
            p(2, 1, 2, "renderer", 0.0),
            p(3, 1, 2, "gpu", 0.0),
            p(4, 0, 1, "notepad", 0.0),
            p(5, 0, 1, "chromedriver-bg", 0.0),
            p(6, 0, 1, "svc", 0.0),
        ]);
        let set = apps(&[1, 4]);
        let mut opts = group_opts(&s);
        opts.expanded.clear(); // 搜索强制展开
        opts.filter = "REND".into();
        // 命中子进程：应用组保留它和祖先 chrome；后台组没有命中，标题也不显示。
        assert_eq!(show(&s, &build_group_view(&s, &set, &opts)), ["H:Apps(1)", "chrome@0", "renderer@1"]);
        opts.filter = "chrome".into();
        assert_eq!(
            show(&s, &build_group_view(&s, &set, &opts)),
            ["H:Apps(1)", "chrome@0", "H:Background(1)", "chromedriver-bg@0"]
        );
        // 按 PID 前缀搜索。
        opts.filter = "6".into();
        assert_eq!(show(&s, &build_group_view(&s, &set, &opts)), ["H:Background(1)", "svc@0"]);
        opts.filter = "没有".into();
        assert!(build_group_view(&s, &set, &opts).is_empty());
    }

    #[test]
    fn group_empty_group_has_no_header() {
        let s = snap(vec![p(1, 0, 1, "a", 0.0)]);
        assert_eq!(show(&s, &build_group_view(&s, &apps(&[]), &group_opts(&s))), ["H:Background(1)", "a@0"]);
        assert_eq!(show(&s, &build_group_view(&s, &apps(&[1]), &group_opts(&s))), ["H:Apps(1)", "a@0"]);
    }

    #[test]
    fn group_selection_survives_a_regroup() {
        // 选中按 ProcessKey 保持：窗口关闭后同一进程从应用组移到后台组，仍能按 key 找到。
        let s = snap(vec![p(1, 0, 1, "app", 0.0), p(2, 0, 1, "other", 0.0)]);
        let selected = s.processes[0].key;
        let find = |rows: &[ViewRow]| {
            rows.iter().position(|r| r.process().is_some_and(|i| s.processes[i].key == selected))
        };
        let before = build_group_view(&s, &apps(&[1]), &group_opts(&s));
        let after = build_group_view(&s, &apps(&[]), &group_opts(&s));
        assert_eq!(find(&before), Some(1));
        assert_eq!(find(&after), Some(1));
        assert_eq!(show(&s, &after), ["H:Background(2)", "app@0", "other@0"]);
        // 进程退出后找不到，由调用方清除选中。
        let gone = snap(vec![p(2, 0, 1, "other", 0.0)]);
        let rows = build_group_view(&gone, &apps(&[]), &group_opts(&gone));
        assert!(rows.iter().all(|r| r.process().is_none_or(|i| gone.processes[i].key != selected)));
    }

    #[test]
    fn flat_and_tree_rows_have_no_headers() {
        let s = snap(vec![p(1, 0, 1, "a", 0.0), p(2, 1, 2, "b", 0.0)]);
        for mode in [ViewMode::Flat, ViewMode::Tree] {
            let rows = build_view(&s, &ViewOptions { mode, ..Default::default() });
            assert!(rows.iter().all(|r| r.header.is_none()));
        }
    }
}

//! 性能页资源卡片的实例列表：卡片元数据、按实例 id 保存的曲线历史、选中项保持与回退。
//!
//! 这里只有纯逻辑，不依赖界面对象，可直接单测。磁盘、网卡、GPU 等设备增减时，
//! 只需让 `ResourceSet::update` 收到新的实例列表即可。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::format::History;
use crate::history::Window;
use crate::{ResourceKind, ResourceTint};

/// 汇总卡片的实例 id。设置里持久化“性能页选中的资源”时保存的就是实例 id。
pub const CPU: &str = "cpu";
pub const MEMORY: &str = "memory";
pub const DISK: &str = "disk";
pub const NETWORK: &str = "network";

/// 一张资源卡片的稳定描述。`id` 在设备存续期间不变（不能用列表下标）。
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub id: String,
    pub kind: ResourceKind,
    pub tint: ResourceTint,
    pub title: String,
    /// 曲线图上方的说明文字。
    pub caption: String,
    /// 详情页底部各统计项的标签，顺序与取值一致。
    pub stat_labels: Vec<String>,
}

impl Instance {
    fn new(
        id: &str,
        kind: ResourceKind,
        tint: ResourceTint,
        title: &str,
        caption: &str,
        labels: &[&str],
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            tint,
            title: title.into(),
            caption: caption.into(),
            stat_labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// 当前的资源实例：CPU、内存、磁盘、网络各一张汇总卡片。
/// 后续按物理磁盘、网卡、GPU 拆分时，在这里按设备枚举结果生成。
pub fn summary_instances() -> Vec<Instance> {
    vec![
        Instance::new(
            CPU,
            ResourceKind::Cpu,
            ResourceTint::Cpu,
            "CPU",
            "% 利用率",
            &[],
        ),
        Instance::new(
            MEMORY,
            ResourceKind::Memory,
            ResourceTint::Memory,
            "内存",
            "内存使用量",
            &[],
        ),
        Instance::new(
            DISK,
            ResourceKind::Disk,
            ResourceTint::Disk,
            "磁盘",
            "% 活动时间",
            &["活动时间", "读取速度", "写入速度"],
        ),
        Instance::new(
            NETWORK,
            ResourceKind::Network,
            ResourceTint::Network,
            "网络",
            "吞吐量（接收 + 发送）",
            &["接收", "发送"],
        ),
    ]
}

pub fn disk_id(number: u32) -> String {
    format!("disk:{number}")
}

pub fn device_instances(disks: &[tml_core::PhysicalDisk]) -> Vec<Instance> {
    let mut result = summary_instances();
    result.retain(|i| i.id != DISK);
    for (offset, disk) in disks.iter().enumerate() {
        let mounts = disk
            .volumes
            .iter()
            .map(|v| v.mount.trim_end_matches('\\'))
            .collect::<Vec<_>>()
            .join(" ");
        let title = if mounts.is_empty() {
            format!("磁盘 {}", disk.number)
        } else {
            format!("磁盘 {} ({mounts})", disk.number)
        };
        result.insert(
            2 + offset,
            Instance::new(
                &disk_id(disk.number),
                ResourceKind::Disk,
                ResourceTint::Disk,
                &title,
                "% 活动时间",
                &["活动时间", "平均响应时间", "读取速度", "写入速度"],
            ),
        );
    }
    result
}

/// 实例描述只在设备名称或挂载卷变化时重建。
pub fn network_instances(disks: &[tml_core::PhysicalDisk], networks: &[tml_core::NetworkAdapter]) -> Vec<Instance> {
    let mut result = device_instances(disks);
    result.retain(|i| i.id != NETWORK);
    for n in networks {
        result.push(Instance::new(&n.id, ResourceKind::Network, ResourceTint::Network,
            crate::texts::connection_kind(n.connection), "吞吐量（接收 + 发送）", &["接收", "发送"]));
    }
    result
}

/// 把界面模型从旧 id 序列变成新 id 序列所需的行编辑，按顺序依次执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// 删除第 i 行。
    Remove(usize),
    /// 在第 i 行位置插入一行新数据。
    Insert(usize),
}

/// 计算最少的行删除、插入，使模型保持已有行的顺序和滚动状态。
/// 位置变化的 id 视为先删后插；调用方随后会用最新数据覆盖每一行。
pub fn diff_ids(old: &[String], new: &[String]) -> Vec<Edit> {
    let mut cur: Vec<&String> = old.iter().collect();
    let mut edits = Vec::new();
    for (i, id) in new.iter().enumerate() {
        if cur.get(i) == Some(&id) {
            continue;
        }
        match cur[i.min(cur.len())..].iter().position(|c| *c == id) {
            Some(offset) => {
                for _ in 0..offset {
                    cur.remove(i);
                    edits.push(Edit::Remove(i));
                }
            }
            None => {
                cur.insert(i.min(cur.len()), id);
                edits.push(Edit::Insert(i));
            }
        }
    }
    while cur.len() > new.len() {
        cur.pop();
        edits.push(Edit::Remove(new.len()));
    }
    edits
}

/// 资源实例列表与按实例 id 保存的状态。
#[derive(Default)]
pub struct ResourceSet {
    instances: Vec<Instance>,
    histories: HashMap<String, History>,
    selected: String,
    /// 想选中但当前不存在的实例 id（启动时恢复的设置，或选中项暂时消失）。
    /// 实例出现时自动选中；用户明确选中别的实例后清除。退出时优先保存它，而不是回退用的 CPU。
    pending: Option<String>,
    network_key: Vec<(String, String, tml_core::ConnectionKind)>,
    gpu_memory: HashMap<String, [History; 2]>,
    disk_key: Vec<(u32, Vec<String>)>,
    receive: HashMap<String, History>,
    send: HashMap<String, History>,
}

impl ResourceSet {
    pub fn new(instances: Vec<Instance>) -> Self {
        let mut set = Self {
            selected: CPU.into(),
            ..Default::default()
        };
        set.update(instances);
        set
    }

    pub fn sync_devices(&mut self, disks: &[tml_core::PhysicalDisk], networks: &[tml_core::NetworkAdapter]) {
        let nk: Vec<_> = networks.iter().map(|n| (n.id.clone(), n.name.clone(), n.connection)).collect();
        let dk: Vec<_> = disks.iter().map(|d| (d.number, d.volumes.iter().map(|v| v.mount.clone()).collect())).collect();
        if nk != self.network_key || dk != self.disk_key || self.instances.iter().any(|i| i.id == NETWORK || i.id == DISK) {
            let gpu_instances: Vec<_> = self.instances.iter().filter(|i| i.kind == ResourceKind::Gpu).cloned().collect();
            let mut instances = network_instances(disks, networks);
            instances.extend(gpu_instances);
            self.update(instances);
            self.network_key = nk;
            self.disk_key = dk;
            self.receive.retain(|id,_| networks.iter().any(|n| &n.id == id));
            self.send.retain(|id,_| networks.iter().any(|n| &n.id == id));
        }
    }

    pub fn sync_gpus(&mut self, gpus: &[tml_core::Gpu]) {
        let mut instances: Vec<_> = self.instances.iter().filter(|i| i.kind != ResourceKind::Gpu).cloned().collect();
        for (index, g) in gpus.iter().enumerate() {
            instances.push(Instance::new(&g.id, ResourceKind::Gpu, ResourceTint::Gpu,
                &format!("GPU {index}"), "% 利用率（最忙引擎）", &["利用率", "专用显存", "共享显存"]));
        }
        if instances != self.instances { self.update(instances); }
        self.gpu_memory.retain(|id,_| gpus.iter().any(|g| &g.id == id));
    }

    pub fn push_gpu_memory(&mut self, gpus: &[tml_core::Gpu], now: Instant, gap: bool) {
        for g in gpus {
            let h = self.gpu_memory.entry(g.id.clone()).or_default();
            h[0].push(now, if gap { None } else { g.dedicated });
            h[1].push(now, if gap { None } else { g.shared });
        }
    }

    pub fn gpu_memory_paths(&self, gpu: &tml_core::Gpu, now: Instant, size: (f64, f64), window: Window) -> Vec<(String, String, String)> {
        self.gpu_memory.get(&gpu.id).map(|h| h.iter().enumerate().map(|(index,h)| {
            let total = if index == 0 { gpu.dedicated_total } else { gpu.shared_total };
            let scale = if total > 0 { total as f64 } else { h.max_in(now, window).max(1.0) };
            let label = if total > 0 { crate::format::gigabytes(total) } else { "容量不可用".into() };
            let (line, area) = h.paths(now, scale, size, window);
            (line, label, area)
        }).collect()).unwrap_or_default()
    }

    pub fn push_network(&mut self, n: &tml_core::NetworkAdapter, now: Instant) {
        self.receive.entry(n.id.clone()).or_default().push(now, n.recv);
        self.send.entry(n.id.clone()).or_default().push(now, n.send);
    }

    /// 网卡接收、发送的曲线历史（导出用）。
    pub fn network_histories(&self, id: &str) -> (Option<&History>, Option<&History>) {
        (self.receive.get(id), self.send.get(id))
    }

    /// GPU 专用、共享显存的曲线历史（导出用）。
    pub fn gpu_memory_histories(&self, id: &str) -> Option<&[History; 2]> {
        self.gpu_memory.get(id)
    }

    /// 网络详情里的收发统计。60 秒窗口取原始采样（峰值、谷值、均值）；更长的窗口取聚合层（峰值、均值，不含谷值）。
    pub fn network_stats(&self, id: &str, now: Instant, window: Window) -> String {
        let describe = |map: &HashMap<String, History>| -> String {
            let Some(h) = map.get(id) else { return "—".into() };
            if window == Window::Sec60 {
                h.window_stats(now).map(|(min, max, mean)| {
                    format!("峰值 {} · 谷值 {} · 均值 {}", crate::format::rate(max), crate::format::rate(min), crate::format::rate(mean))
                })
            } else {
                h.layer_summary(now, window).map(|(max, mean)| {
                    format!("峰值 {} · 均值 {}", crate::format::rate(max), crate::format::rate(mean))
                })
            }
            .unwrap_or_else(|| "—".into())
        };
        let name = crate::texts::window_name(window);
        format!("最近 {name} 接收：{}\n最近 {name} 发送：{}", describe(&self.receive), describe(&self.send))
    }

    /// 测试钩子：给全部已有的曲线灌入合成历史，见 `History::prefill`。
    pub fn prefill(&mut self, now: Instant, secs: u64) {
        for h in self.histories.values_mut().chain(self.receive.values_mut()).chain(self.send.values_mut()) {
            h.prefill(now, secs);
        }
        for h in self.gpu_memory.values_mut().flatten() {
            h.prefill(now, secs);
        }
    }

    /// 全部曲线历史占用的堆内存估算（字节），供日志核对内存增量。
    pub fn history_bytes(&self) -> usize {
        self.histories.values().chain(self.receive.values()).chain(self.send.values()).map(History::heap_bytes).sum::<usize>()
            + self.gpu_memory.values().flatten().map(History::heap_bytes).sum::<usize>()
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn ids(&self) -> Vec<String> {
        self.instances.iter().map(|i| i.id.clone()).collect()
    }

    /// 用最新的实例列表更新：新增实例的历史从空开始，消失实例的历史被清理；
    /// 选中的实例消失时回退到 CPU，并记为待恢复；待恢复的实例出现时重新选中。
    pub fn update(&mut self, instances: Vec<Instance>) {
        let alive: HashSet<&str> = instances.iter().map(|i| i.id.as_str()).collect();
        self.histories.retain(|id, _| alive.contains(id.as_str()));
        for i in &instances {
            self.histories.entry(i.id.clone()).or_default();
        }
        if !alive.contains(self.selected.as_str()) {
            self.pending = Some(std::mem::replace(&mut self.selected, CPU.into()));
        }
        if let Some(p) = self.pending.take_if(|p| alive.contains(p.as_str())) {
            self.selected = p;
        }
        self.instances = instances;
    }

    pub fn selected(&self) -> &str {
        &self.selected
    }

    /// 要写进设置的选中项：待恢复的实例还没出现时保存它，避免临时的回退把偏好丢掉。
    pub fn selected_for_save(&self) -> &str {
        self.pending.as_deref().unwrap_or(&self.selected)
    }

    /// 启动时恢复设置里的选中项。实例已存在则选中；否则记为待恢复并返回 false。
    pub fn restore(&mut self, id: &str) -> bool {
        if self.select(id) {
            return true;
        }
        self.pending = Some(id.into());
        false
    }

    /// 选中项在当前列表中的下标；列表里没有时为 0。
    pub fn selected_index(&self) -> usize {
        self.instances
            .iter()
            .position(|i| i.id == self.selected)
            .unwrap_or(0)
    }

    /// 选中指定实例。实例不存在时保持原选中项并返回 false。
    pub fn select(&mut self, id: &str) -> bool {
        let exists = self.instances.iter().any(|i| i.id == id);
        if exists {
            self.selected = id.into();
            self.pending = None;
        }
        exists
    }

    /// 当前实例的曲线历史。`id` 必须来自 `instances()`。
    pub fn history(&self, id: &str) -> &History {
        &self.histories[id]
    }

    #[cfg(test)]
    pub fn push(&mut self, id: &str, at: Instant, value: Option<f64>) {
        self.push_with(id, at, value, crate::peak::NO_PROC);
    }

    /// 同 `push`，并记下这次采样时占用最高的进程（#81，只有 CPU、磁盘曲线有）。
    pub fn push_with(&mut self, id: &str, at: Instant, value: Option<f64>, who: crate::peak::ProcId) {
        if let Some(h) = self.histories.get_mut(id) {
            h.push_with(at, value, who);
        }
    }

    /// 采样中断：所有曲线在此断开。
    pub fn push_gap(&mut self, at: Instant) {
        for h in self.receive.values_mut().chain(self.send.values_mut()) { h.push(at, None); }
        for pair in self.gpu_memory.values_mut() {
            for h in pair { h.push(at, None); }
        }
        for h in self.histories.values_mut() {
            h.push(at, None);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_hotplug_and_directional_statistics_are_independent() {
        use super::*;
        let now = Instant::now();
        let a = tml_core::NetworkAdapter { id: "network:1".into(), name: "以太网".into(), recv: Some(1024.0), send: Some(2048.0), ..Default::default() };
        let b = tml_core::NetworkAdapter { id: "network:2".into(), name: "Wi-Fi".into(), recv: Some(4096.0), send: None, ..Default::default() };
        let mut set = ResourceSet::new(summary_instances());
        set.sync_devices(&[], &[a.clone(), b.clone()]);
        assert_eq!(set.ids(), ["cpu", "memory", "network:1", "network:2"]);
        set.push_network(&a, now);
        set.push_network(&b, now);
        set.push(&a.id, now, a.recv.zip(a.send).map(|(r,s)| r+s));
        set.push(&b.id, now, b.recv.zip(b.send).map(|(r,s)| r+s));
        set.push_gap(now);
        assert_eq!(set.history(&a.id).max_in(now, Window::Sec60), 3072.0);
        assert_eq!(set.receive[&a.id].window_stats(now), Some((1024.0,1024.0,1024.0)));
        assert_eq!(set.send[&b.id].window_stats(now), None);
        set.select(&b.id);
        set.sync_devices(&[], &[a.clone()]);
        assert_eq!(set.selected(), CPU);
        assert!(!set.receive.contains_key(&b.id));
        set.sync_devices(&[], &[a,b]);
        assert_eq!(set.selected(), "network:2");
        assert_eq!(set.history("network:2").max_in(now, Window::Sec60), 0.0);
    }
    #[test]
    fn physical_disks_replace_summary_and_hotplug_preserves_selection() {
        let a = tml_core::PhysicalDisk {
            number: 0,
            ..Default::default()
        };
        let b = tml_core::PhysicalDisk {
            number: 2,
            ..Default::default()
        };
        let mut set = super::ResourceSet::new(super::device_instances(&[a.clone(), b.clone()]));
        assert_eq!(set.ids(), ["cpu", "memory", "disk:0", "disk:2", "network"]);
        assert!(set.select("disk:2"));
        set.update(super::device_instances(&[a.clone()]));
        assert_eq!(set.selected(), "cpu");
        set.update(super::device_instances(&[a, b]));
        assert_eq!(set.selected(), "disk:2");
        assert_eq!(super::device_instances(&[]).len(), 3);
    }
    use super::*;

    fn inst(id: &str) -> Instance {
        Instance::new(id, ResourceKind::Disk, ResourceTint::Disk, id, "", &[])
    }

    fn insts(ids: &[&str]) -> Vec<Instance> {
        ids.iter().map(|id| inst(id)).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 模拟界面模型：把编辑应用到 old 上，插入的行用新 id 占位。
    fn apply(old: &[&str], new: &[&str]) -> Vec<String> {
        let mut rows = ids(old);
        for e in diff_ids(&ids(old), &ids(new)) {
            match e {
                Edit::Remove(i) => {
                    rows.remove(i);
                }
                Edit::Insert(i) => rows.insert(i, new[i].to_string()),
            }
        }
        rows
    }

    #[test]
    fn diff_unchanged_is_empty() {
        assert!(diff_ids(&ids(&["cpu", "memory"]), &ids(&["cpu", "memory"])).is_empty());
    }

    #[test]
    fn diff_append_and_insert() {
        assert_eq!(
            diff_ids(&ids(&["cpu"]), &ids(&["cpu", "disk0"])),
            vec![Edit::Insert(1)]
        );
        assert_eq!(
            diff_ids(&ids(&["cpu", "net"]), &ids(&["cpu", "disk0", "net"])),
            vec![Edit::Insert(1)]
        );
    }

    #[test]
    fn diff_remove_keeps_other_rows() {
        assert_eq!(
            diff_ids(&ids(&["cpu", "disk0", "net"]), &ids(&["cpu", "net"])),
            vec![Edit::Remove(1)]
        );
        assert_eq!(
            diff_ids(&ids(&["cpu", "disk0"]), &ids(&["cpu"])),
            vec![Edit::Remove(1)]
        );
    }

    #[test]
    fn diff_result_matches_target() {
        let cases: &[(&[&str], &[&str])] = &[
            (&[], &["a", "b"]),
            (&["a", "b"], &[]),
            (&["a", "b", "c"], &["c", "a"]),
            (&["a", "b", "c"], &["c", "b", "a"]),
            (&["a", "b"], &["x", "a", "y", "b", "z"]),
            (&["a", "b", "c", "d"], &["b", "d"]),
        ];
        for (old, new) in cases {
            assert_eq!(apply(old, new), ids(new), "{old:?} -> {new:?}");
        }
    }

    #[test]
    fn summary_has_four_stable_ids() {
        let set = ResourceSet::new(summary_instances());
        assert_eq!(set.ids(), ids(&["cpu", "memory", "disk", "network"]));
        assert_eq!(set.selected(), "cpu");
        assert_eq!(set.selected_index(), 0);
    }

    #[test]
    fn selection_follows_id_not_index() {
        let mut set = ResourceSet::new(insts(&["cpu", "memory", "disk0", "net0"]));
        assert!(set.select("net0"));
        assert_eq!(set.selected_index(), 3);
        // 前面新增一块磁盘，选中项仍是 net0，下标随之变化。
        set.update(insts(&["cpu", "memory", "disk0", "disk1", "net0"]));
        assert_eq!(set.selected(), "net0");
        assert_eq!(set.selected_index(), 4);
        // 前面的磁盘消失，选中项仍保持。
        set.update(insts(&["cpu", "memory", "disk1", "net0"]));
        assert_eq!(set.selected(), "net0");
        assert_eq!(set.selected_index(), 3);
    }

    #[test]
    fn selection_falls_back_to_cpu_when_instance_disappears() {
        let mut set = ResourceSet::new(insts(&["cpu", "memory", "disk0", "disk1"]));
        assert!(set.select("disk1"));
        set.update(insts(&["cpu", "memory", "disk0"]));
        assert_eq!(set.selected(), "cpu");
        assert_eq!(set.selected_index(), 0);
        // 设备回来后恢复选中（消失只是暂时的回退）。
        set.update(insts(&["cpu", "memory", "disk0", "disk1"]));
        assert_eq!(set.selected(), "disk1");
    }

    #[test]
    fn restore_of_absent_instance_is_pending_and_saved_until_it_appears() {
        let mut set = ResourceSet::new(insts(&["cpu", "memory"]));
        assert!(!set.restore("gpu0"));
        assert_eq!(set.selected(), "cpu");
        // 仍未出现：保存的是待恢复的 id，而不是回退的 cpu。
        set.update(insts(&["cpu", "memory", "disk0"]));
        assert_eq!(set.selected(), "cpu");
        assert_eq!(set.selected_for_save(), "gpu0");
        // 出现后自动选中，待恢复清除。
        set.update(insts(&["cpu", "memory", "disk0", "gpu0"]));
        assert_eq!(set.selected(), "gpu0");
        assert_eq!(set.selected_for_save(), "gpu0");
        // 再次消失又回来，同样恢复。
        set.update(insts(&["cpu", "memory"]));
        assert_eq!(set.selected_for_save(), "gpu0");
        set.update(insts(&["cpu", "memory", "gpu0"]));
        assert_eq!(set.selected(), "gpu0");
    }

    #[test]
    fn explicit_selection_clears_pending() {
        let mut set = ResourceSet::new(insts(&["cpu", "memory"]));
        assert!(!set.restore("gpu0"));
        assert!(set.select("memory"));
        assert_eq!(set.selected_for_save(), "memory");
        set.update(insts(&["cpu", "memory", "gpu0"]));
        assert_eq!(set.selected(), "memory");
    }

    #[test]
    fn restore_existing_selects_immediately() {
        let mut set = ResourceSet::new(summary_instances());
        assert!(set.restore("network"));
        assert_eq!(set.selected_for_save(), "network");
    }

    #[test]
    fn select_unknown_keeps_current() {
        let mut set = ResourceSet::new(summary_instances());
        assert!(set.select("memory"));
        assert!(!set.select("disk9"));
        assert_eq!(set.selected(), "memory");
    }

    #[test]
    fn history_is_per_instance_new_starts_empty_removed_is_cleaned() {
        let now = Instant::now();
        let mut set = ResourceSet::new(insts(&["cpu", "disk0"]));
        set.push("cpu", now, Some(10.0));
        set.push("disk0", now, Some(50.0));
        assert_eq!(set.history("disk0").max_in(now, Window::Sec60), 50.0);

        // 新增实例从空开始，已有实例的历史不受影响。
        set.update(insts(&["cpu", "disk0", "disk1"]));
        assert_eq!(set.history("disk1").max_in(now, Window::Sec60), 0.0);
        assert_eq!(set.history("disk0").max_in(now, Window::Sec60), 50.0);
        assert_eq!(set.history("cpu").max_in(now, Window::Sec60), 10.0);

        // 移除时清理，之后同 id 再出现也是空历史。
        set.push("disk1", now, Some(70.0));
        set.update(insts(&["cpu", "disk0"]));
        assert_eq!(set.histories.len(), 2);
        assert!(!set.histories.contains_key("disk1"));
        set.update(insts(&["cpu", "disk0", "disk1"]));
        assert_eq!(set.history("disk1").max_in(now, Window::Sec60), 0.0);
    }

    #[test]
    fn network_stats_follow_the_window() {
        let t0 = Instant::now();
        let a = tml_core::NetworkAdapter { id: "network:1".into(), recv: Some(1024.0), send: Some(2048.0), ..Default::default() };
        let mut set = ResourceSet::new(summary_instances());
        set.sync_devices(&[], &[a.clone()]);
        for i in 0..100u64 {
            set.push_network(&a, t0 + std::time::Duration::from_secs(i));
        }
        let now = t0 + std::time::Duration::from_secs(99);
        let short = set.network_stats("network:1", now, Window::Sec60);
        assert!(short.starts_with("最近 60 秒 接收：峰值") && short.contains("谷值"), "{short}");
        let long = set.network_stats("network:1", now, Window::Min5);
        assert!(long.starts_with("最近 5 分钟 接收：峰值") && !long.contains("谷值"), "{long}");
        // 没有数据的网卡显示“—”。
        assert!(set.network_stats("network:9", now, Window::Hour1).contains("接收：—"));
    }

    #[test]
    fn gap_applies_to_all_histories() {
        let now = Instant::now();
        let mut set = ResourceSet::new(insts(&["cpu", "disk0"]));
        set.push("cpu", now, Some(1.0));
        set.push_gap(now);
        set.push("cpu", now, Some(2.0));
        // 断点把曲线分成两段：折线里有两个 M。
        let (line, _) = set.history("cpu").paths(now, 100.0, (100.0, 50.0), Window::Sec60);
        assert_eq!(line.matches('M').count(), 2);
    }
}

//! 进程图标：后台线程读取路径并提取图标，界面线程按路径缓存。
//!
//! 流程：新进程出现 -> 后台解析路径 -> 路径首次出现时后台提取图标 -> 图标到达后只更新用到它的行。
//! 缓存按 exe 路径存放，同一程序的多个进程共用一份；进程全部退出后对应图标随之清理。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tml_core::ProcessKey;

/// 进程当前的图标来源。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// 路径还在后台解析。
    Resolving,
    /// 拿不到路径（System、受保护进程），使用默认图标。
    NoPath,
    Path(PathBuf),
}

#[derive(Debug)]
enum Icon<V> {
    /// 已请求提取，尚未到达。
    Pending,
    Ready(V),
    /// 提取失败，使用默认图标。
    Failed,
}

#[derive(Debug)]
struct Entry<V> {
    /// 使用该路径的存活进程数，归零时清除条目。
    refs: usize,
    icon: Icon<V>,
}

/// 图标缓存。`V` 在界面中是 `slint::Image`；用泛型是为了脱离界面单测。
#[derive(Debug)]
pub struct IconCache<V> {
    procs: HashMap<ProcessKey, Source>,
    paths: HashMap<PathBuf, Entry<V>>,
    default: Option<V>,
}

impl<V> Default for IconCache<V> {
    fn default() -> Self {
        Self { procs: HashMap::new(), paths: HashMap::new(), default: None }
    }
}

impl<V> IconCache<V> {
    /// 以最新快照里的存活进程同步缓存：返回新出现、需要解析路径的进程，
    /// 已退出的进程被遗忘，不再有进程使用的路径连同图标一起清除。
    pub fn track(&mut self, live: impl IntoIterator<Item = ProcessKey>) -> Vec<ProcessKey> {
        let live: HashSet<ProcessKey> = live.into_iter().collect();
        let mut fresh = Vec::new();
        for &key in &live {
            if !self.procs.contains_key(&key) {
                self.procs.insert(key, Source::Resolving);
                fresh.push(key);
            }
        }
        let gone: Vec<ProcessKey> = self.procs.keys().filter(|k| !live.contains(k)).copied().collect();
        for key in gone {
            if let Some(Source::Path(path)) = self.procs.remove(&key)
                && let Some(entry) = self.paths.get_mut(&path)
            {
                entry.refs -= 1;
                if entry.refs == 0 {
                    self.paths.remove(&path);
                }
            }
        }
        fresh
    }

    /// 后台解析出了进程路径（`None` 表示拿不到）。返回需要提取图标的路径（该路径首次出现时才返回）。
    pub fn resolved(&mut self, key: ProcessKey, path: Option<PathBuf>) -> Option<PathBuf> {
        // 解析期间进程已退出。
        let source = self.procs.get_mut(&key)?;
        // 已有结果：显示缩放变化后 reset() 会把存活进程重新当作新进程，新旧两次 Resolve 都会回来，
        // 只接受第一个，否则同一进程会把路径的引用计数加两次，进程退出后条目永远释放不掉。
        if !matches!(source, Source::Resolving) {
            return None;
        }
        let Some(path) = path else {
            *source = Source::NoPath;
            return None;
        };
        *source = Source::Path(path.clone());
        let entry = self.paths.entry(path.clone());
        let is_new = matches!(entry, std::collections::hash_map::Entry::Vacant(_));
        entry.or_insert(Entry { refs: 0, icon: Icon::Pending }).refs += 1;
        is_new.then_some(path)
    }

    /// 后台提取完成。`path` 为 `None` 表示默认图标。路径已无进程使用时丢弃。
    pub fn loaded(&mut self, path: Option<&Path>, icon: Option<V>) {
        match path {
            None => self.default = icon,
            Some(path) => {
                if let Some(entry) = self.paths.get_mut(path) {
                    entry.icon = icon.map_or(Icon::Failed, Icon::Ready);
                }
            }
        }
    }

    /// 进程应显示的图标；还在解析或提取中时为 `None`（显示空白，图标到达后再更新）。
    pub fn icon(&self, key: ProcessKey) -> Option<&V> {
        match self.procs.get(&key)? {
            Source::Resolving => None,
            Source::NoPath => self.default.as_ref(),
            Source::Path(path) => match &self.paths.get(path)?.icon {
                Icon::Pending => None,
                Icon::Ready(v) => Some(v),
                Icon::Failed => self.default.as_ref(),
            },
        }
    }

    /// 进程的图标是否取决于 `path`（`None` 表示默认图标）：用于图标到达后确定要刷新的行。
    pub fn depends_on(&self, key: ProcessKey, path: Option<&Path>) -> bool {
        match (self.procs.get(&key), path) {
            (Some(Source::Path(p)), Some(want)) => p == want,
            (Some(Source::NoPath), None) => true,
            (Some(Source::Path(p)), None) => matches!(self.paths.get(p), Some(Entry { icon: Icon::Failed, .. })),
            _ => false,
        }
    }

    /// 丢弃全部缓存（显示缩放变化后图标需要按新尺寸重取）。
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    fn cached_paths(&self) -> usize {
        self.paths.len()
    }
}

/// 发给后台线程的任务。
enum Job {
    /// 解析这些进程的可执行文件路径。
    Resolve(Vec<ProcessKey>),
    /// 提取图标；`None` 表示默认图标。
    Extract(Vec<Option<PathBuf>>, u32),
}

/// 后台线程送回界面线程的结果。
pub enum Event {
    Paths(Vec<(ProcessKey, Option<PathBuf>)>),
    Icon { path: Option<PathBuf>, px: u32, pixels: Option<SharedPixelBuffer<Rgba8Pixel>> },
}

#[derive(Clone)]
pub struct IconWorker {
    tx: Sender<Job>,
}

impl IconWorker {
    /// `deliver` 在后台线程上调用，负责把结果投递到界面线程。
    pub fn spawn(deliver: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("icons".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    match job {
                        Job::Resolve(keys) => {
                            let paths = keys.into_iter().map(|k| (k, tml_core::image_path_of(k))).collect();
                            deliver(Event::Paths(paths));
                        }
                        Job::Extract(paths, px) => {
                            for path in paths {
                                let bitmap = match &path {
                                    Some(p) => tml_core::file_icon(p, px),
                                    None => tml_core::default_icon(px),
                                };
                                let pixels = bitmap.map(|b| {
                                    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&b.rgba, b.width, b.height)
                                });
                                deliver(Event::Icon { path, px, pixels });
                            }
                        }
                    }
                }
            })
            .expect("无法创建图标线程");
        Self { tx }
    }

    pub fn resolve(&self, keys: Vec<ProcessKey>) {
        if !keys.is_empty() {
            let _ = self.tx.send(Job::Resolve(keys));
        }
    }

    pub fn extract(&self, paths: Vec<Option<PathBuf>>, px: u32) {
        if !paths.is_empty() {
            let _ = self.tx.send(Job::Extract(paths, px));
        }
    }
}

/// 图标的逻辑边长（像素）。
pub const LOGICAL_SIZE: f32 = 16.0;

/// 显示缩放下图标应取的物理像素边长。
pub fn physical_size(scale_factor: f32) -> u32 {
    (LOGICAL_SIZE * scale_factor.max(1.0)).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: u32) -> ProcessKey {
        ProcessKey { pid, create_time: pid as u64 }
    }

    fn p(s: &str) -> Option<PathBuf> {
        Some(PathBuf::from(s))
    }

    #[test]
    fn same_path_is_extracted_once() {
        let mut c = IconCache::<u32>::default();
        assert_eq!(c.track([key(1), key(2), key(3)]).len(), 3);
        assert_eq!(c.resolved(key(1), p(r"C:\a.exe")), p(r"C:\a.exe"));
        // 第二、三个进程使用同一程序：命中缓存，不再要求提取。
        assert_eq!(c.resolved(key(2), p(r"C:\a.exe")), None);
        assert_eq!(c.resolved(key(3), p(r"C:\a.exe")), None);
        assert_eq!(c.cached_paths(), 1);
        assert_eq!(c.icon(key(2)), None, "图标到达前为空");
        c.loaded(p(r"C:\a.exe").as_deref(), Some(7));
        assert_eq!([key(1), key(2), key(3)].map(|k| c.icon(k).copied()), [Some(7); 3]);
    }

    #[test]
    fn known_processes_are_not_tracked_as_new_again() {
        let mut c = IconCache::<u32>::default();
        assert_eq!(c.track([key(1)]), vec![key(1)]);
        assert!(c.track([key(1)]).is_empty());
    }

    #[test]
    fn entries_are_dropped_after_all_processes_exit() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1), key(2)]);
        c.resolved(key(1), p(r"C:\a.exe"));
        c.resolved(key(2), p(r"C:\a.exe"));
        c.loaded(p(r"C:\a.exe").as_deref(), Some(1));
        // 还剩一个进程在用：保留。
        c.track([key(2)]);
        assert_eq!(c.cached_paths(), 1);
        assert_eq!(c.icon(key(2)), Some(&1));
        // 全部退出：清理。
        c.track([]);
        assert_eq!(c.cached_paths(), 0);
        assert_eq!(c.icon(key(2)), None);
        // 程序再次启动时重新提取。
        c.track([key(9)]);
        assert_eq!(c.resolved(key(9), p(r"C:\a.exe")), p(r"C:\a.exe"));
    }

    #[test]
    fn reused_pid_is_a_different_process() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1)]);
        c.resolved(key(1), p(r"C:\a.exe"));
        c.loaded(p(r"C:\a.exe").as_deref(), Some(1));
        let reused = ProcessKey { pid: 1, create_time: 999 };
        assert_eq!(c.track([reused]), vec![reused]);
        assert_eq!(c.icon(reused), None);
        assert_eq!(c.cached_paths(), 0);
    }

    #[test]
    fn processes_without_path_or_icon_fall_back_to_default() {
        let mut c = IconCache::<&str>::default();
        c.track([key(4), key(5), key(6)]);
        assert_eq!(c.resolved(key(4), None), None);
        c.resolved(key(5), p(r"C:\broken.exe"));
        c.resolved(key(6), p(r"C:\ok.exe"));
        // 默认图标尚未到达：不显示。
        assert_eq!(c.icon(key(4)), None);
        c.loaded(None, Some("default"));
        c.loaded(p(r"C:\broken.exe").as_deref(), None);
        c.loaded(p(r"C:\ok.exe").as_deref(), Some("ok"));
        assert_eq!(c.icon(key(4)), Some(&"default"));
        assert_eq!(c.icon(key(5)), Some(&"default"), "提取失败回退默认图标");
        assert_eq!(c.icon(key(6)), Some(&"ok"));
    }

    #[test]
    fn depends_on_selects_rows_to_refresh() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1), key(2), key(3)]);
        c.resolved(key(1), p(r"C:\a.exe"));
        c.resolved(key(2), p(r"C:\b.exe"));
        c.resolved(key(3), None);
        let a = p(r"C:\a.exe");
        assert!(c.depends_on(key(1), a.as_deref()));
        assert!(!c.depends_on(key(2), a.as_deref()));
        assert!(c.depends_on(key(3), None));
        assert!(!c.depends_on(key(1), None));
        // 提取失败后，该路径的进程改用默认图标，默认图标到达时也要刷新它们。
        c.loaded(p(r"C:\b.exe").as_deref(), None);
        assert!(c.depends_on(key(2), None));
    }

    #[test]
    fn exit_during_resolve_is_ignored() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1)]);
        c.track([]);
        assert_eq!(c.resolved(key(1), p(r"C:\a.exe")), None);
        assert_eq!(c.cached_paths(), 0);
        // 路径已无人使用，迟到的图标被丢弃。
        c.loaded(p(r"C:\a.exe").as_deref(), Some(1));
        assert_eq!(c.cached_paths(), 0);
    }

    #[test]
    fn duplicate_resolve_after_reset_does_not_double_count_refs() {
        // DPI 变化：reset 后进程被重新跟踪，旧的和新的 Resolve 结果都会到达。
        let mut c = IconCache::<u32>::default();
        c.track([key(1)]);
        c.reset();
        assert_eq!(c.track([key(1)]), vec![key(1)]);
        assert_eq!(c.resolved(key(1), p(r"C:\a.exe")), p(r"C:\a.exe"), "第一个结果触发提取");
        assert_eq!(c.resolved(key(1), p(r"C:\a.exe")), None, "第二个结果被忽略");
        c.loaded(p(r"C:\a.exe").as_deref(), Some(7));
        assert_eq!(c.icon(key(1)), Some(&7));
        // 进程退出后条目必须能释放（引用计数没有被多加）。
        c.track([]);
        assert_eq!(c.cached_paths(), 0);
    }

    #[test]
    fn duplicate_resolve_without_path_is_ignored_too() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1)]);
        assert_eq!(c.resolved(key(1), None), None);
        // 第二个结果带路径也不能改变已确定的来源。
        assert_eq!(c.resolved(key(1), p(r"C:\a.exe")), None);
        assert_eq!(c.cached_paths(), 0);
        assert!(c.depends_on(key(1), None));
    }

    #[test]
    fn reset_clears_everything() {
        let mut c = IconCache::<u32>::default();
        c.track([key(1)]);
        c.resolved(key(1), None);
        c.loaded(None, Some(3));
        c.reset();
        assert_eq!(c.icon(key(1)), None);
        assert_eq!(c.track([key(1)]), vec![key(1)]);
    }

    #[test]
    fn cloned_images_compare_equal_so_unchanged_rows_are_skipped() {
        // Image 按像素缓冲的指针比较：克隆后相等；空 Image 彼此不相等，所以不能拿它当占位。
        let a = slint::Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(1, 1));
        assert!(a.clone() == a);
        assert!(slint::Image::default() != slint::Image::default());
    }

    #[test]
    fn icon_size_follows_display_scale() {
        assert_eq!(physical_size(1.0), 16);
        assert_eq!(physical_size(1.25), 20);
        assert_eq!(physical_size(1.5), 24);
        assert_eq!(physical_size(2.0), 32);
        assert_eq!(physical_size(0.0), 16);
    }
}

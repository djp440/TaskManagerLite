//! 用户设置：`%APPDATA%\TaskManagerLite\settings.json`。
//!
//! - 读取逐项容错：文件缺失、损坏、字段缺失或类型不对时，对应项回退默认值，不影响启动。
//! - 未识别的字段原样保留并在保存时写回，后续版本新增的字段不会被旧版本丢掉。
//! - 保存先写临时文件再改名，避免崩溃时留下半截文件。
//! - 新增设置项：在 `Settings` 加字段，并在 `from_value` / `apply_to` 中各补一行。

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::{Map, Number, Value};
use tml_core::{DetailColumn, SortKey, ViewMode};

use crate::details::{self, Shown};

const FILE_NAME: &str = "settings.json";
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// 窗口外框位置（物理像素）与客户区大小（逻辑像素，换到不同缩放的显示器上也保持相同的视觉大小）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: f32,
    pub height: f32,
}

/// 迷你模式（#19）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MiniSettings {
    /// 退出时处于迷你模式：下次启动直接进入。
    pub active: bool,
    /// 迷你窗口左上角（物理像素）；从未进入过迷你模式时为 `None`。
    pub position: Option<(i32, i32)>,
    /// 迷你窗口置顶，默认开启。
    pub on_top: bool,
    /// 不透明度百分比，见 [`MiniSettings::OPACITIES`]。
    pub opacity: u8,
}

impl MiniSettings {
    /// 右键菜单提供的不透明度档位。
    pub const OPACITIES: [u8; 5] = [100, 90, 80, 70, 60];
}

impl Default for MiniSettings {
    fn default() -> Self {
        Self { active: false, position: None, on_top: true, opacity: 100 }
    }
}

/// 进程列表中可调宽度的列，单位为逻辑像素；名称列自动伸缩，不在其中。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnWidths {
    pub pid: f32,
    pub cpu: f32,
    pub memory: f32,
    pub disk: f32,
}

impl ColumnWidths {
    pub const MIN: f32 = 48.0;
    pub const MAX: f32 = 480.0;

    pub fn clamped(self) -> Self {
        let c = |v: f32| v.clamp(Self::MIN, Self::MAX);
        Self { pid: c(self.pid), cpu: c(self.cpu), memory: c(self.memory), disk: c(self.disk) }
    }

    /// 在可用宽度 `available`（列表宽度减去右侧留白）内求有效列宽：名称列至少保留 `name_min`，
    /// 其余列超出预算时，把各自高出下限的部分按同一比例缩小。保存值不变，窗口放大后自然恢复。
    /// 可用宽度未知（<= 0）时原样返回。界面布局通过 `Columns.fit` 调用本函数。
    pub fn fit(self, available: f32, name_min: f32) -> Self {
        let w = self.clamped();
        let sum = w.pid + w.cpu + w.memory + w.disk;
        // 预算不足以容纳四列的下限时，各列取下限（窗口的最小宽度保证这种情况不会出现）。
        let budget = (available - name_min).max(4.0 * Self::MIN);
        if available <= 0.0 || sum <= budget {
            return w;
        }
        let f = (budget - 4.0 * Self::MIN) / (sum - 4.0 * Self::MIN);
        let s = |v: f32| Self::MIN + (v - Self::MIN) * f;
        Self { pid: s(w.pid), cpu: s(w.cpu), memory: s(w.memory), disk: s(w.disk) }
    }
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self { pid: 76.0, cpu: 76.0, memory: 108.0, disk: 108.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub window: Option<WindowRect>,
    pub maximized: bool,
    /// 采样间隔：0 = 0.5 秒，1 = 1 秒，2 = 2 秒，3 = 暂停。
    pub interval_index: i32,
    /// 进程列表的视图模式：平铺、树形、分组。
    pub view: ViewMode,
    pub sort_key: SortKey,
    pub descending: bool,
    /// 0 = 进程，1 = 性能。
    pub page: i32,
    /// 性能页选中的资源实例 id（`cpu`、`memory`、`disk`、`network`……）。
    pub resource: String,
    pub columns: ColumnWidths,
    /// 显示中的“详细信息”列，按显示顺序。
    pub details: Vec<Shown>,
    /// 用 ETW 统计进程磁盘速率（需要管理员权限，默认关闭，见 ADR-0004）。
    pub accurate_disk_io: bool,
    /// 窗口置顶。
    pub always_on_top: bool,
    /// 用户选择用本程序接管 Ctrl+Shift+Esc（IFEO 登记，见 ADR 0003）。这只是意愿，登记是否有效以注册表为准。
    pub replace_taskmgr: bool,
    /// 后台继续采样：窗口最小化或隐藏时继续采系统指标，曲线不断开。默认关闭。
    pub background_sampling: bool,
    /// 关闭窗口时最小化到托盘（窗口隐藏、进程常驻、托盘图标可唤起）。默认关闭，此时关闭窗口即退出。
    pub close_to_tray: bool,
    pub mini: MiniSettings,
    /// 本版本不认识的顶层字段，保存时原样写回。
    extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window: None,
            maximized: false,
            interval_index: 1,
            view: ViewMode::Flat,
            sort_key: SortKey::Cpu,
            descending: true,
            page: 0,
            resource: "cpu".into(),
            columns: ColumnWidths::default(),
            details: Vec::new(),
            accurate_disk_io: false,
            always_on_top: false,
            replace_taskmgr: false,
            background_sampling: false,
            close_to_tray: false,
            mini: MiniSettings::default(),
            extra: Map::new(),
        }
    }
}

// 顶层字段名。新增字段时同步加入 KNOWN。
// `tree` 是旧版的布尔视图开关，读取时迁移为 `view`，保存时不再写出。
const KNOWN: &[&str] = &[
    "window",
    "interval_index",
    "tree",
    "view",
    "sort",
    "page",
    "resource",
    "columns",
    "detail_columns",
    "always_on_top",
    "replace_taskmgr",
    "accurate_disk_io",
    "background_sampling",
    "close_to_tray",
    "mini",
];

fn int<T: TryFrom<i64>>(o: &Map<String, Value>, key: &str) -> Option<T> {
    T::try_from(o.get(key)?.as_i64()?).ok()
}

fn float(o: &Map<String, Value>, key: &str) -> Option<f32> {
    o.get(key)?.as_f64().filter(|v| v.is_finite()).map(|v| v as f32)
}

fn view_mode_name(m: ViewMode) -> &'static str {
    match m {
        ViewMode::Flat => "flat",
        ViewMode::Tree => "tree",
        ViewMode::Group => "group",
    }
}

fn view_mode_from_name(s: &str) -> Option<ViewMode> {
    Some(match s {
        "flat" => ViewMode::Flat,
        "tree" => ViewMode::Tree,
        "group" => ViewMode::Group,
        _ => return None,
    })
}

/// 排序列的名字；详细信息列写作 `detail:<列名>`。
fn sort_key_name(k: SortKey) -> String {
    match k {
        SortKey::Name => "name".into(),
        SortKey::Pid => "pid".into(),
        SortKey::Cpu => "cpu".into(),
        SortKey::Memory => "memory".into(),
        SortKey::Disk => "disk".into(),
        SortKey::Detail(c) => format!("detail:{}", c.id()),
    }
}

fn sort_key_from_name(s: &str) -> Option<SortKey> {
    Some(match s {
        "name" => SortKey::Name,
        "pid" => SortKey::Pid,
        "cpu" => SortKey::Cpu,
        "memory" => SortKey::Memory,
        "disk" => SortKey::Disk,
        _ => SortKey::Detail(DetailColumn::from_id(s.strip_prefix("detail:")?)?),
    })
}

/// 排序列的默认方向（设置里缺少方向时使用）。
pub fn default_descending(k: SortKey) -> bool {
    match k {
        SortKey::Name | SortKey::Pid => false,
        SortKey::Cpu | SortKey::Memory | SortKey::Disk => true,
        SortKey::Detail(c) => details::default_descending(c),
    }
}

/// 保留两位小数，避免把 f32 的二进制误差写进文件。
fn number(v: f32) -> Value {
    Number::from_f64((v as f64 * 100.0).round() / 100.0).map_or(Value::Null, Value::Number)
}

impl Settings {
    /// 从 JSON 文本解析。文本损坏时返回默认值，`bool` 表示解析是否成功。
    pub fn parse(text: &str) -> (Self, bool) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(o)) => (Self::from_object(&o), true),
            _ => (Self::default(), false),
        }
    }

    fn from_object(o: &Map<String, Value>) -> Self {
        let mut s = Self::default();
        if let Some(w) = o.get("window").and_then(Value::as_object)
            && let (Some(x), Some(y), Some(width), Some(height)) =
                (int(w, "x"), int(w, "y"), float(w, "width"), float(w, "height"))
            && width >= 1.0
            && height >= 1.0
        {
            s.window = Some(WindowRect { x, y, width, height });
            s.maximized = w.get("maximized").and_then(Value::as_bool).unwrap_or(false);
        }
        if let Some(i) = int::<i32>(o, "interval_index")
            && (0..=3).contains(&i)
        {
            s.interval_index = i;
        }
        // 旧版只有“树形”开关：`tree: true` 迁移为树形；新字段 `view` 优先。
        if o.get("tree").and_then(Value::as_bool) == Some(true) {
            s.view = ViewMode::Tree;
        }
        if let Some(m) = o.get("view").and_then(Value::as_str).and_then(view_mode_from_name) {
            s.view = m;
        }
        if let Some(sort) = o.get("sort").and_then(Value::as_object) {
            if let Some(k) = sort.get("key").and_then(Value::as_str).and_then(sort_key_from_name) {
                s.sort_key = k;
                // 方向缺失时用该列的默认方向（名称、PID 升序，其余降序）。
                s.descending = default_descending(k);
            }
            if let Some(d) = sort.get("descending").and_then(Value::as_bool) {
                s.descending = d;
            }
        }
        if let Some(p) = int::<i32>(o, "page")
            && (0..=1).contains(&p)
        {
            s.page = p;
        }
        if let Some(r) = o.get("resource").and_then(Value::as_str)
            && !r.is_empty()
        {
            s.resource = r.to_owned();
        }
        if let Some(c) = o.get("columns").and_then(Value::as_object) {
            let get = |key: &str, default: f32| -> f32 {
                match c.get(key).and_then(Value::as_f64) {
                    Some(v) if v.is_finite() => (v as f32).clamp(ColumnWidths::MIN, ColumnWidths::MAX),
                    _ => default,
                }
            };
            let d = ColumnWidths::default();
            s.columns = ColumnWidths {
                pid: get("pid", d.pid),
                cpu: get("cpu", d.cpu),
                memory: get("memory", d.memory),
                disk: get("disk", d.disk),
            };
        }
        if let Some(b) = o.get("background_sampling").and_then(Value::as_bool) {
            s.background_sampling = b;
        }
        if let Some(b) = o.get("close_to_tray").and_then(Value::as_bool) {
            s.close_to_tray = b;
        }
        if let Some(m) = o.get("mini").and_then(Value::as_object) {
            let flag = |key: &str, default: bool| m.get(key).and_then(Value::as_bool).unwrap_or(default);
            s.mini.active = flag("active", false);
            s.mini.on_top = flag("on_top", true);
            if let (Some(x), Some(y)) = (int(m, "x"), int(m, "y")) {
                s.mini.position = Some((x, y));
            }
            // 只接受菜单里的档位，其余值回退为不透明。
            if let Some(p) = int::<u8>(m, "opacity")
                && MiniSettings::OPACITIES.contains(&p)
            {
                s.mini.opacity = p;
            }
        }
        if let Some(b) = o.get("accurate_disk_io").and_then(Value::as_bool) {
            s.accurate_disk_io = b;
        }
        if let Some(b) = o.get("always_on_top").and_then(Value::as_bool) {
            s.always_on_top = b;
        }
        // 未知的列名、重复项与非法宽度被跳过；宽度缺失时用该列的默认宽度。
        if let Some(list) = o.get("detail_columns").and_then(Value::as_array) {
            for item in list.iter().filter_map(Value::as_object) {
                let Some(column) = item.get("id").and_then(Value::as_str).and_then(DetailColumn::from_id) else { continue };
                if s.details.iter().any(|d| d.column == column) {
                    continue;
                }
                let width = match item.get("width").and_then(Value::as_f64) {
                    Some(v) if v.is_finite() => (v as f32).clamp(details::MIN_WIDTH, details::MAX_WIDTH),
                    _ => details::default_width(column),
                };
                s.details.push(Shown { column, width });
            }
        }
        if let Some(b) = o.get("replace_taskmgr").and_then(Value::as_bool) {
            s.replace_taskmgr = b;
        }
        s.extra = o.iter().filter(|(k, _)| !KNOWN.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
        s
    }

    pub fn to_json(&self) -> String {
        let mut o = self.extra.clone();
        if let Some(w) = self.window {
            let mut m = Map::new();
            m.insert("x".into(), w.x.into());
            m.insert("y".into(), w.y.into());
            m.insert("width".into(), number(w.width));
            m.insert("height".into(), number(w.height));
            m.insert("maximized".into(), self.maximized.into());
            o.insert("window".into(), Value::Object(m));
        }
        o.insert("interval_index".into(), self.interval_index.into());
        o.insert("view".into(), view_mode_name(self.view).into());
        let mut sort = Map::new();
        sort.insert("key".into(), sort_key_name(self.sort_key).into());
        sort.insert("descending".into(), self.descending.into());
        o.insert("sort".into(), Value::Object(sort));
        o.insert("page".into(), self.page.into());
        o.insert("resource".into(), self.resource.clone().into());
        let mut c = Map::new();
        c.insert("pid".into(), number(self.columns.pid));
        c.insert("cpu".into(), number(self.columns.cpu));
        c.insert("memory".into(), number(self.columns.memory));
        c.insert("disk".into(), number(self.columns.disk));
        o.insert("columns".into(), Value::Object(c));
        let shown: Vec<Value> = self
            .details
            .iter()
            .map(|d| {
                let mut m = Map::new();
                m.insert("id".into(), d.column.id().into());
                m.insert("width".into(), number(d.width));
                Value::Object(m)
            })
            .collect();
        o.insert("detail_columns".into(), Value::Array(shown));
        o.insert("accurate_disk_io".into(), self.accurate_disk_io.into());
        o.insert("always_on_top".into(), self.always_on_top.into());
        o.insert("replace_taskmgr".into(), self.replace_taskmgr.into());
        o.insert("background_sampling".into(), self.background_sampling.into());
        o.insert("close_to_tray".into(), self.close_to_tray.into());
        let mut m = Map::new();
        m.insert("active".into(), self.mini.active.into());
        if let Some((x, y)) = self.mini.position {
            m.insert("x".into(), x.into());
            m.insert("y".into(), y.into());
        }
        m.insert("on_top".into(), self.mini.on_top.into());
        m.insert("opacity".into(), self.mini.opacity.into());
        o.insert("mini".into(), Value::Object(m));
        let mut text = serde_json::to_string_pretty(&Value::Object(o)).unwrap_or_default();
        text.push('\n');
        text
    }
}

/// 读取结果，用于写日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOutcome {
    Loaded,
    Missing,
    /// 文件无法读取或不是合法 JSON，已使用默认值。
    Corrupt,
}

/// 设置文件的位置。`None` 的场合（开发模式）既不读也不写。
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { path: dir.into().join(FILE_NAME) }
    }

    /// 目录：环境变量 `TML_SETTINGS_DIR`，否则 `%APPDATA%\TaskManagerLite`。
    pub fn from_env() -> Option<Self> {
        if let Some(d) = std::env::var_os("TML_SETTINGS_DIR") {
            return Some(Self::new(d));
        }
        Some(Self::new(PathBuf::from(std::env::var_os("APPDATA")?).join("TaskManagerLite")))
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 临时文件名带进程 id：提权重启期间新旧实例可能同时存在，不能争用同一个临时文件。
    fn tmp_path(&self) -> PathBuf {
        self.path.with_extension(format!("json.{}.tmp", std::process::id()))
    }

    pub fn load(&self) -> (Settings, LoadOutcome) {
        // 设置文件只有几百字节；异常巨大的文件视为损坏，避免启动时被它拖住。
        if fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            return (Settings::default(), LoadOutcome::Corrupt);
        }
        match fs::read(&self.path) {
            Ok(bytes) => {
                let (s, ok) = Settings::parse(&String::from_utf8_lossy(&bytes));
                (s, if ok { LoadOutcome::Loaded } else { LoadOutcome::Corrupt })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (Settings::default(), LoadOutcome::Missing),
            Err(_) => (Settings::default(), LoadOutcome::Corrupt),
        }
    }

    /// 先写同目录的临时文件，落盘后改名覆盖正式文件。
    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.tmp_path();
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(settings.to_json().as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Settings {
        Settings::parse(text).0
    }

    fn sample() -> Settings {
        Settings {
            window: Some(WindowRect { x: -120, y: 80, width: 1200.0, height: 760.5 }),
            maximized: true,
            interval_index: 2,
            view: ViewMode::Group,
            sort_key: SortKey::Memory,
            descending: false,
            page: 1,
            resource: "network".into(),
            columns: ColumnWidths { pid: 90.0, cpu: 64.5, memory: 140.0, disk: 200.0 },
            details: vec![
                Shown { column: DetailColumn::CommandLine, width: 420.5 },
                Shown { column: DetailColumn::Handles, width: 76.0 },
            ],
            accurate_disk_io: true,
            always_on_top: true,
            replace_taskmgr: true,
            background_sampling: true,
            close_to_tray: true,
            mini: MiniSettings { active: true, position: Some((-1500, 40)), on_top: false, opacity: 80 },
            extra: Map::new(),
        }
    }

    #[test]
    fn 迷你模式容错() {
        assert_eq!(parse("{}").mini, MiniSettings::default());
        let s = parse(r#"{"mini": {"active": "yes", "x": 10, "opacity": 55, "on_top": 0}}"#);
        assert_eq!(s.mini, MiniSettings::default(), "类型不对、缺 y、非档位的不透明度都回退默认");
        let s = parse(r#"{"mini": {"x": 10, "y": -20, "opacity": 70}}"#);
        assert_eq!(s.mini.position, Some((10, -20)));
        assert_eq!(s.mini.opacity, 70);
        assert!(s.mini.on_top);
        assert_eq!(parse(r#"{"mini": true}"#).mini, MiniSettings::default());
    }

    #[test]
    fn 往返一致() {
        let s = sample();
        assert_eq!(parse(&s.to_json()), s);
        let d = Settings::default();
        assert_eq!(parse(&d.to_json()), d);
    }

    #[test]
    fn 空对象与非对象回退默认() {
        assert_eq!(Settings::parse("{}"), (Settings::default(), true));
        for bad in ["", "   ", "[1,2]", "null", "42", "{\"view\": tru", "\u{0}\u{1}垃圾"] {
            assert_eq!(Settings::parse(bad), (Settings::default(), false), "{bad:?}");
        }
    }

    #[test]
    fn 后台采样默认关闭且只认布尔值() {
        assert!(!Settings::default().background_sampling);
        assert!(parse(r#"{"background_sampling": true}"#).background_sampling);
        assert!(!parse(r#"{"background_sampling": "yes"}"#).background_sampling);
        assert!(!parse("{}").background_sampling);
    }

    #[test]
    fn 关闭到托盘默认关闭且只认布尔值() {
        assert!(!Settings::default().close_to_tray);
        assert!(parse(r#"{"close_to_tray": true}"#).close_to_tray);
        assert!(!parse(r#"{"close_to_tray": 1}"#).close_to_tray);
    }

    #[test]
    fn 带_bom_也能读() {
        assert_eq!(parse("\u{feff}{\"view\": \"tree\"}").view, ViewMode::Tree);
    }

    #[test]
    fn 类型不对的字段逐项回退() {
        let s = parse(
            r#"{"tree": "yes", "view": 5, "interval_index": "2", "page": 5, "resource": 3,
                "sort": {"key": 7, "descending": "no"},
                "window": {"x": 1, "y": 2, "width": "wide", "height": 3},
                "columns": {"pid": "x", "cpu": 100, "memory": null}, "accurate_disk_io": "yes", "always_on_top": 1}"#,
        );
        let d = Settings::default();
        assert_eq!(s.view, d.view);
        assert_eq!(s.interval_index, d.interval_index);
        assert_eq!(s.page, d.page);
        assert_eq!(s.resource, d.resource);
        assert_eq!(s.sort_key, d.sort_key);
        assert_eq!(s.descending, d.descending);
        assert_eq!(s.window, None);
        assert!(!s.accurate_disk_io);
        assert_eq!(s.columns, ColumnWidths { cpu: 100.0, ..ColumnWidths::default() });
        assert!(!s.always_on_top);
    }

    #[test]
    fn 详细信息列保持顺序并跳过非法项() {
        let s = parse(
            r#"{"detail_columns": [{"id": "user", "width": 5000}, {"id": "nope"}, 3, {"id": "threads"},
                {"id": "user", "width": 90}, {"id": "path", "width": "wide"}],
                "sort": {"key": "detail:handles"}}"#,
        );
        assert_eq!(
            s.details,
            vec![
                Shown { column: DetailColumn::User, width: details::MAX_WIDTH },
                Shown { column: DetailColumn::Threads, width: details::default_width(DetailColumn::Threads) },
                Shown { column: DetailColumn::Path, width: details::default_width(DetailColumn::Path) },
            ]
        );
        assert_eq!(s.sort_key, SortKey::Detail(DetailColumn::Handles));
        assert!(s.descending, "数值列默认降序");
        assert_eq!(parse(r#"{"sort": {"key": "detail:nope"}}"#).sort_key, SortKey::Cpu);
        assert_eq!(parse(r#"{"detail_columns": {"id": "user"}}"#).details, vec![]);
        // 详细信息列排序也能往返。
        let t = Settings { sort_key: SortKey::Detail(DetailColumn::CommandLine), ..Settings::default() };
        assert_eq!(parse(&t.to_json()).sort_key, t.sort_key);
    }

    #[test]
    fn 精确磁盘开关默认关闭且可读写() {
        assert!(!Settings::default().accurate_disk_io);
        assert!(!parse("{}").accurate_disk_io);
        assert!(parse(r#"{"accurate_disk_io": true}"#).accurate_disk_io);
        // 非布尔值回退为关闭，而不是被当成“未知字段”原样保留。
        for bad in ["1", "\"true\"", "null"] {
            let s = parse(&format!("{{\"accurate_disk_io\": {bad}}}"));
            assert!(!s.accurate_disk_io, "{bad}");
            let out: Value = serde_json::from_str(&s.to_json()).unwrap();
            assert_eq!(out["accurate_disk_io"], false);
        }
    }

    #[test]
    fn 部分字段缺失时其余照常读取() {
        let s = parse(r#"{"view": "group", "sort": {"key": "name"}, "interval_index": 3}"#);
        assert_eq!(s.view, ViewMode::Group);
        assert_eq!(s.interval_index, 3);
        assert_eq!(s.sort_key, SortKey::Name);
        // 方向缺失：名称列默认升序。
        assert!(!s.descending);
        assert_eq!(s.page, 0);
        assert_eq!(s.window, None);
    }

    fn total(c: ColumnWidths) -> f32 {
        c.pid + c.cpu + c.memory + c.disk
    }

    #[test]
    fn 列宽在预算内时不变() {
        let c = ColumnWidths::default();
        assert_eq!(c.fit(1040.0 - 10.0, 180.0), c);
        // 可用宽度未知时不约束。
        let wide = ColumnWidths { pid: 480.0, cpu: 480.0, memory: 480.0, disk: 480.0 };
        assert_eq!(wide.fit(0.0, 180.0), wide);
    }

    #[test]
    fn 列宽超出预算时按比例缩小并保证名称列下限() {
        let wide = ColumnWidths { pid: 480.0, cpu: 480.0, memory: 480.0, disk: 480.0 };
        for available in [750.0f32, 800.0, 1000.0, 1500.0] {
            let e = wide.fit(available, 180.0);
            let budget = available - 180.0;
            assert!(total(e) <= budget + 0.01, "{available}: {e:?}");
            assert!((total(e) - budget).abs() < 0.01, "超出预算时应正好用满预算");
            for v in [e.pid, e.cpu, e.memory, e.disk] {
                assert!(v >= ColumnWidths::MIN);
            }
        }
        // 各列按高出下限的部分等比缩小，大小次序不变。
        let mixed = ColumnWidths { pid: 100.0, cpu: 100.0, memory: 400.0, disk: 480.0 };
        let e = mixed.fit(760.0, 180.0);
        assert!(e.memory > e.pid && e.disk > e.memory);
        assert!(total(e) <= 580.01);
    }

    #[test]
    fn 预算小于四列下限时取下限() {
        let wide = ColumnWidths { pid: 300.0, cpu: 300.0, memory: 300.0, disk: 300.0 };
        let e = wide.fit(100.0, 180.0);
        assert_eq!(e, ColumnWidths { pid: 48.0, cpu: 48.0, memory: 48.0, disk: 48.0 });
    }

    #[test]
    fn 超过_1_mb_的文件视为损坏() {
        let dir = temp_dir("big");
        fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        let mut text = String::from("{\"view\": \"tree\", \"pad\": \"");
        text.push_str(&"x".repeat(MAX_FILE_BYTES as usize));
        text.push_str("\"}");
        fs::write(store.path(), text).unwrap();
        assert_eq!(store.load(), (Settings::default(), LoadOutcome::Corrupt));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 数值越界被忽略或夹紧() {
        let s = parse(
            r#"{"interval_index": 9, "page": -1,
                "window": {"x": 0, "y": 0, "width": 0, "height": 100},
                "columns": {"pid": 1, "cpu": 99999, "memory": -5}}"#,
        );
        assert_eq!(s.interval_index, 1);
        assert_eq!(s.page, 0);
        assert_eq!(s.window, None);
        assert_eq!(s.columns.pid, ColumnWidths::MIN);
        assert_eq!(s.columns.cpu, ColumnWidths::MAX);
        assert_eq!(s.columns.memory, ColumnWidths::MIN);
        let big = parse(r#"{"window": {"x": 99999999999, "y": 0, "width": 10, "height": 10}}"#);
        assert_eq!(big.window, None);
    }

    #[test]
    fn 未知字段被忽略且保存时保留() {
        let s = parse(r#"{"view": "tree", "future": {"a": [1, 2]}, "another": "x"}"#);
        assert_eq!(s.view, ViewMode::Tree);
        let out: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(out["future"]["a"][1], 2);
        assert_eq!(out["another"], "x");
        assert_eq!(out["view"], "tree");
    }

    #[test]
    fn 接管任务管理器开关默认关闭且类型不对时回退() {
        assert!(!Settings::default().replace_taskmgr);
        assert!(!parse("{}").replace_taskmgr);
        assert!(parse(r#"{"replace_taskmgr": true}"#).replace_taskmgr);
        assert!(!parse(r#"{"replace_taskmgr": "yes"}"#).replace_taskmgr);
        let out: Value = serde_json::from_str(&parse(r#"{"replace_taskmgr": true}"#).to_json()).unwrap();
        assert_eq!(out["replace_taskmgr"], true);
    }

    #[test]
    fn 旧版树形开关迁移为视图模式() {
        assert_eq!(parse(r#"{"tree": true}"#).view, ViewMode::Tree);
        assert_eq!(parse(r#"{"tree": false}"#).view, ViewMode::Flat);
        // 新字段优先；未识别的取值回退到旧开关 / 默认。
        assert_eq!(parse(r#"{"tree": true, "view": "group"}"#).view, ViewMode::Group);
        assert_eq!(parse(r#"{"tree": true, "view": "bogus"}"#).view, ViewMode::Tree);
        assert_eq!(parse(r#"{"view": 3}"#).view, ViewMode::Flat);
        // 保存后不再写出旧字段。
        let out: Value = serde_json::from_str(&parse(r#"{"tree": true}"#).to_json()).unwrap();
        assert_eq!(out["view"], "tree");
        assert!(out.get("tree").is_none());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tml-settings-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn 读取缺失与损坏的文件() {
        let dir = temp_dir("load");
        let store = Store::new(&dir);
        assert_eq!(store.load(), (Settings::default(), LoadOutcome::Missing));
        fs::create_dir_all(&dir).unwrap();
        fs::write(store.path(), b"{\"view\": \"tr").unwrap();
        assert_eq!(store.load(), (Settings::default(), LoadOutcome::Corrupt));
        fs::write(store.path(), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert_eq!(store.load(), (Settings::default(), LoadOutcome::Corrupt));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 保存覆盖旧文件且不留临时文件() {
        let dir = temp_dir("save");
        let store = Store::new(dir.join("nested"));
        let s = sample();
        store.save(&s).unwrap();
        assert_eq!(store.load(), (s.clone(), LoadOutcome::Loaded));
        let s2 = Settings { view: ViewMode::Flat, page: 0, ..s };
        store.save(&s2).unwrap();
        assert_eq!(store.load(), (s2, LoadOutcome::Loaded));
        let names: Vec<_> =
            fs::read_dir(dir.join("nested")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec![FILE_NAME.to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn 保存失败时旧文件不受影响() {
        let dir = temp_dir("fail");
        let store = Store::new(&dir);
        let s = sample();
        store.save(&s).unwrap();
        // 把正式文件换成同名目录，改名必然失败；旧的临时文件也应被清理。
        let blocked = Store::new(dir.join("blocked"));
        fs::create_dir_all(blocked.path()).unwrap();
        assert!(blocked.save(&s).is_err());
        assert!(!blocked.tmp_path().exists());
        assert_eq!(store.load(), (s, LoadOutcome::Loaded));
        let _ = fs::remove_dir_all(&dir);
    }
}

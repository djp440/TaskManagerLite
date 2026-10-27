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
use tml_core::SortKey;

const FILE_NAME: &str = "settings.json";

/// 窗口外框位置（物理像素）与客户区大小（逻辑像素，换到不同缩放的显示器上也保持相同的视觉大小）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: f32,
    pub height: f32,
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
    pub tree: bool,
    pub sort_key: SortKey,
    pub descending: bool,
    /// 0 = 进程，1 = 性能。
    pub page: i32,
    /// 性能页选中的资源实例 id（`cpu`、`memory`、`disk`、`network`……）。
    pub resource: String,
    pub columns: ColumnWidths,
    /// 本版本不认识的顶层字段，保存时原样写回。
    extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window: None,
            maximized: false,
            interval_index: 1,
            tree: false,
            sort_key: SortKey::Cpu,
            descending: true,
            page: 0,
            resource: "cpu".into(),
            columns: ColumnWidths::default(),
            extra: Map::new(),
        }
    }
}

// 顶层字段名。新增字段时同步加入 KNOWN。
const KNOWN: &[&str] = &["window", "interval_index", "tree", "sort", "page", "resource", "columns"];

fn int<T: TryFrom<i64>>(o: &Map<String, Value>, key: &str) -> Option<T> {
    T::try_from(o.get(key)?.as_i64()?).ok()
}

fn float(o: &Map<String, Value>, key: &str) -> Option<f32> {
    o.get(key)?.as_f64().filter(|v| v.is_finite()).map(|v| v as f32)
}

fn sort_key_name(k: SortKey) -> &'static str {
    match k {
        SortKey::Name => "name",
        SortKey::Pid => "pid",
        SortKey::Cpu => "cpu",
        SortKey::Memory => "memory",
        SortKey::Disk => "disk",
    }
}

fn sort_key_from_name(s: &str) -> Option<SortKey> {
    Some(match s {
        "name" => SortKey::Name,
        "pid" => SortKey::Pid,
        "cpu" => SortKey::Cpu,
        "memory" => SortKey::Memory,
        "disk" => SortKey::Disk,
        _ => return None,
    })
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
        if let Some(b) = o.get("tree").and_then(Value::as_bool) {
            s.tree = b;
        }
        if let Some(sort) = o.get("sort").and_then(Value::as_object) {
            if let Some(k) = sort.get("key").and_then(Value::as_str).and_then(sort_key_from_name) {
                s.sort_key = k;
                // 方向缺失时用该列的默认方向（名称、PID 升序，其余降序）。
                s.descending = !matches!(k, SortKey::Name | SortKey::Pid);
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
        o.insert("tree".into(), self.tree.into());
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

    pub fn load(&self) -> (Settings, LoadOutcome) {
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
        let tmp = self.path.with_extension("json.tmp");
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
            tree: true,
            sort_key: SortKey::Memory,
            descending: false,
            page: 1,
            resource: "network".into(),
            columns: ColumnWidths { pid: 90.0, cpu: 64.5, memory: 140.0, disk: 200.0 },
            extra: Map::new(),
        }
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
        for bad in ["", "   ", "[1,2]", "null", "42", "{\"tree\": tru", "\u{0}\u{1}垃圾"] {
            assert_eq!(Settings::parse(bad), (Settings::default(), false), "{bad:?}");
        }
    }

    #[test]
    fn 带_bom_也能读() {
        assert!(parse("\u{feff}{\"tree\": true}").tree);
    }

    #[test]
    fn 类型不对的字段逐项回退() {
        let s = parse(
            r#"{"tree": "yes", "interval_index": "2", "page": 5, "resource": 3,
                "sort": {"key": 7, "descending": "no"},
                "window": {"x": 1, "y": 2, "width": "wide", "height": 3},
                "columns": {"pid": "x", "cpu": 100, "memory": null}}"#,
        );
        let d = Settings::default();
        assert_eq!(s.tree, d.tree);
        assert_eq!(s.interval_index, d.interval_index);
        assert_eq!(s.page, d.page);
        assert_eq!(s.resource, d.resource);
        assert_eq!(s.sort_key, d.sort_key);
        assert_eq!(s.descending, d.descending);
        assert_eq!(s.window, None);
        assert_eq!(s.columns, ColumnWidths { cpu: 100.0, ..ColumnWidths::default() });
    }

    #[test]
    fn 部分字段缺失时其余照常读取() {
        let s = parse(r#"{"tree": true, "sort": {"key": "name"}, "interval_index": 3}"#);
        assert!(s.tree);
        assert_eq!(s.interval_index, 3);
        assert_eq!(s.sort_key, SortKey::Name);
        // 方向缺失：名称列默认升序。
        assert!(!s.descending);
        assert_eq!(s.page, 0);
        assert_eq!(s.window, None);
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
        let s = parse(r#"{"tree": true, "future": {"a": [1, 2]}, "another": "x"}"#);
        assert!(s.tree);
        let out: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(out["future"]["a"][1], 2);
        assert_eq!(out["another"], "x");
        assert_eq!(out["tree"], true);
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
        fs::write(store.path(), b"{\"tree\": tr").unwrap();
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
        let s2 = Settings { tree: false, page: 0, ..s };
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
        assert!(!blocked.path().with_extension("json.tmp").exists());
        assert_eq!(store.load(), (s, LoadOutcome::Loaded));
        let _ = fs::remove_dir_all(&dir);
    }
}

//! 连接页（#77）：TCP / UDP 连接与监听端口。与启动项页、服务页相同，进入页面才枚举，
//! 页面可见时随每次完整采样刷新，离开页面后不再刷新。

use super::*;
use crate::texts;
use crate::{ConnectionData, ConnectionRow};
use std::cmp::Ordering;
use tml_core::connections::{Connection, ConnectionError, ConnectionKey};

/// 连接页的页面编号（与 app.slint 中的 `page` 一致）。
pub const CONNECTIONS_PAGE: i32 = 5;

/// 自动化验收：设置后每次刷新把全部连接写到该文件（制表符分隔，覆盖写），并在日志里记一行刷新。
const DUMP_ENV: &str = "TML_CONNECTIONS_DUMP";

/// 连接页的界面线程状态。
#[derive(Default)]
pub(super) struct ConnectionsState {
    /// 最近一次枚举到的全部连接（未过滤、未排序）。
    all: RefCell<Vec<Connection>>,
    /// 当前显示（过滤并排序后）的连接，与行模型一一对应。
    entries: RefCell<Vec<Shown>>,
    /// 原地更新的行模型，刷新时列表不回到顶部。
    rows: Rc<VecModel<ConnectionRow>>,
    /// 后台枚举正在进行（周期刷新时跳过）。
    loading: Cell<bool>,
    /// 每次后台枚举的序号；只应用不早于已应用结果的那次。
    issued: Cell<u64>,
    applied: Cell<u64>,
    /// 上次写进日志的连接数，变化时才记录。
    logged: Cell<Option<usize>>,
    /// 自动化验收：`--connections-action <动作>`，首次列出连接后执行。
    hook: RefCell<Option<String>>,
}

/// 显示中的一条连接：连接本身加上按 PID 从进程快照查到的进程名。
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    conn: Connection,
    /// 进程名；快照里查不到为 `None`（显示“—”）。
    name: Option<String>,
}

impl Shown {
    /// “转到进程”可用：PID 不为 0 且当前快照里有该进程。
    fn can_goto(&self) -> bool {
        self.conn.pid != 0 && self.name.is_some()
    }

    fn name_text(&self) -> &str {
        self.name.as_deref().unwrap_or("—")
    }
}

impl Controller {
    pub(super) fn bind_connections(self: &Rc<Self>, ui: &AppWindow) {
        let data = ui.global::<ConnectionData>();
        data.set_rows(ModelRc::from(self.connections.rows.clone()));
        let weak = Rc::downgrade(self);
        data.on_sort(move |key| {
            if let Some(c) = weak.upgrade()
                && let Some(ui) = c.ui.upgrade()
            {
                let d = ui.global::<ConnectionData>();
                d.set_descending(d.get_sort_key() == key && !d.get_descending());
                d.set_sort_key(key);
                c.show_connections();
            }
        });
        let weak = Rc::downgrade(self);
        data.on_filter_edited(move || {
            if let Some(c) = weak.upgrade() {
                c.show_connections();
                // 搜索文字可能含地址：日志只记显示条数。
                c.log.borrow_mut().line(format!(
                    "连接页 搜索 显示 {}/{}",
                    c.connections.entries.borrow().len(),
                    c.connections.all.borrow().len()
                ));
            }
        });
        let weak = Rc::downgrade(self);
        data.on_refresh(move || {
            if let Some(c) = weak.upgrade() {
                c.log.borrow_mut().line("连接页 重新枚举");
                c.connections_work();
            }
        });
        let weak = Rc::downgrade(self);
        data.on_goto_process(move || {
            if let Some(c) = weak.upgrade() {
                c.goto_connection_process();
            }
        });
        // 恢复到连接页时，组件的 init 可能早于回调绑定。
        if ui.get_page() == CONNECTIONS_PAGE {
            self.connections_work();
        }
    }

    /// 自动化验收入口：切到连接页，首次列出连接后执行动作（`dump`、`goto:<pid>[:<本地端口>]`）。
    pub fn queue_connections_action(&self, spec: &str) {
        *self.connections.hook.borrow_mut() = Some(spec.to_owned());
        if let Some(ui) = self.ui.upgrade() {
            ui.set_page(CONNECTIONS_PAGE);
        }
    }

    /// 每次完整采样后调用：连接页可见时按采样间隔刷新，不可见时什么也不做。
    pub(super) fn connections_tick(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.get_page() != CONNECTIONS_PAGE || self.connections.loading.get() {
            return;
        }
        self.connections_work();
    }

    /// 后台枚举连接表。
    fn connections_work(&self) {
        let seq = self.connections.issued.get() + 1;
        self.connections.issued.set(seq);
        self.connections.loading.set(true);
        let work = std::thread::Builder::new().name("connections".into()).spawn(move || {
            let started = Instant::now();
            let result = tml_core::connections::list();
            let elapsed = started.elapsed();
            let _ = slint::invoke_from_event_loop(move || {
                crate::CONTROLLER.with(|cell| {
                    if let Some(c) = cell.borrow().clone() {
                        c.connections_done(seq, result, elapsed);
                    }
                });
            });
        });
        if let Err(e) = work {
            self.connections.loading.set(false);
            if let Some(ui) = self.ui.upgrade() {
                ui.global::<ConnectionData>().set_note(format!("无法创建后台线程：{e}").into());
            }
        }
    }

    fn connections_done(&self, seq: u64, result: Result<Vec<Connection>, ConnectionError>, elapsed: Duration) {
        let Some(ui) = self.ui.upgrade() else { return };
        if seq == self.connections.issued.get() {
            self.connections.loading.set(false);
        }
        if seq < self.connections.applied.get() {
            return;
        }
        self.connections.applied.set(seq);
        let dump = std::env::var_os(DUMP_ENV);
        match result {
            Ok(all) => {
                let n = all.len();
                if dump.is_some() || self.connections.logged.replace(Some(n)) != Some(n) {
                    self.log.borrow_mut().line(format!("连接页 枚举 {n} 条连接 {} ms", elapsed.as_millis()));
                }
                if let Some(path) = &dump {
                    let names = self.process_names();
                    let text: String = all.iter().map(|c| dump_line(c, names.get(&c.pid).map(String::as_str))).collect();
                    if let Err(e) = std::fs::write(path, text) {
                        self.log.borrow_mut().line(format!("连接页 写出数据失败 错误码={:?}", e.raw_os_error()));
                    }
                }
                *self.connections.all.borrow_mut() = all;
                ui.global::<ConnectionData>().set_note("".into());
                self.show_connections();
            }
            Err(e) => {
                ui.global::<ConnectionData>()
                    .set_note(format!("无法读取连接列表：{}", texts::connection_error(&e)).into());
                self.log.borrow_mut().line(format!(
                    "连接页 枚举失败 协议={} 错误码={:?}",
                    e.protocol().name(),
                    e.code()
                ));
            }
        }
        self.run_connections_hook();
    }

    /// 当前进程快照的 PID → 进程名。
    fn process_names(&self) -> HashMap<u32, String> {
        self.state.borrow().snapshot.processes.iter().map(|p| (p.key.pid, p.name.clone())).collect()
    }

    fn selected_connection(&self) -> Option<Shown> {
        let ui = self.ui.upgrade()?;
        let i = usize::try_from(ui.global::<ConnectionData>().get_selected()).ok()?;
        self.connections.entries.borrow().get(i).cloned()
    }

    /// 按当前搜索文字过滤、按当前排序显示，并按稳定键（协议 + 四元组 + PID）保留选中项。
    fn show_connections(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let data = ui.global::<ConnectionData>();
        let (key, descending) = (data.get_sort_key(), data.get_descending());
        let needle = data.get_filter().trim().to_lowercase();
        let names = self.process_names();
        let mut entries = visible(&self.connections.all.borrow(), &names, &needle);
        entries.sort_by(|a, b| compare(a, b, key, descending));
        let selected = self
            .selected_connection()
            .and_then(|old| entries.iter().position(|e| e.conn.key() == old.conn.key()));
        let rows = &self.connections.rows;
        let old = rows.row_count();
        for (i, e) in entries.iter().enumerate() {
            let row = connection_row(e);
            if i < old {
                if rows.row_data(i).as_ref() != Some(&row) {
                    rows.set_row_data(i, row);
                }
            } else {
                rows.push(row);
            }
        }
        for i in (entries.len()..old).rev() {
            rows.remove(i);
        }
        *self.connections.entries.borrow_mut() = entries;
        data.set_selected(selected.map_or(-1, |i| i as i32));
    }

    /// 跳转到进程页并选中连接所属的进程；PID 0 或进程已不存在时给出提示。
    fn goto_connection_process(&self) {
        let Some(shown) = self.selected_connection() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        let pid = shown.conn.pid;
        if pid != 0 && self.reveal_pid(pid) {
            ui.set_page(0);
            ui.invoke_show_selected();
            let chosen = self.state.borrow().selected.map(|k| k.pid);
            self.log.borrow_mut().line(format!(
                "连接页 跳转到进程 PID={pid} 行={} 选中PID={}",
                ui.get_selected(),
                chosen.map_or_else(|| "-".to_string(), |p| p.to_string())
            ));
        } else {
            let note = if pid == 0 {
                "PID 0 不是可跳转的进程".to_string()
            } else {
                format!("进程列表中没有 PID {pid}（可能已退出），请稍后重试")
            };
            ui.global::<ConnectionData>().set_note(note.into());
            self.log.borrow_mut().line(format!("连接页 跳转到进程 PID={pid} 结果=找不到"));
        }
    }

    fn run_connections_hook(&self) {
        // 等首个进程快照到达后再执行（进程名与转到进程需要它）；之后每次周期刷新都会再检查一次。
        if !self.state.borrow().first_data {
            return;
        }
        let Some(spec) = self.connections.hook.borrow_mut().take() else { return };
        let Some(ui) = self.ui.upgrade() else { return };
        // 快照刚到：先按新快照重算进程名。
        self.show_connections();
        let mut parts = spec.split(':');
        match (parts.next(), parts.next().and_then(|p| p.parse::<u32>().ok())) {
            (Some("dump"), _) => {
                self.log.borrow_mut().line(format!("连接页 测试钩子 dump 显示 {}", self.connections.entries.borrow().len()));
            }
            (Some("goto"), Some(pid)) => {
                let port = parts.next().and_then(|p| p.parse::<u16>().ok());
                let index = self
                    .connections
                    .entries
                    .borrow()
                    .iter()
                    .position(|e| e.conn.pid == pid && port.is_none_or(|p| e.conn.local.port() == p));
                match index {
                    Some(i) => {
                        ui.global::<ConnectionData>().set_selected(i as i32);
                        let can = self.selected_connection().is_some_and(|s| s.can_goto());
                        self.log.borrow_mut().line(format!("连接页 测试钩子 选中 PID={pid} 可转到={can}"));
                        self.goto_connection_process();
                    }
                    None => self.log.borrow_mut().line(format!("连接页 测试钩子 找不到 PID={pid} 的连接")),
                }
            }
            _ => self.log.borrow_mut().line("连接页 测试钩子 未知动作"),
        }
    }
}

/// 自动化验收写出的一行：协议、本地、远程、状态、PID、进程名（制表符分隔）。
fn dump_line(c: &Connection, name: Option<&str>) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\r\n",
        c.protocol.name(),
        c.local,
        c.remote.map(|r| r.to_string()).unwrap_or_default(),
        c.state.map(|s| format!("{s:?}")).unwrap_or_default(),
        c.pid,
        name.unwrap_or("")
    )
}

/// 搜索文字（已去空白转小写）过滤后的连接，附带进程名。可搜进程名、PID、协议、地址、端口、状态。
fn visible(all: &[Connection], names: &HashMap<u32, String>, needle: &str) -> Vec<Shown> {
    all.iter()
        .map(|c| Shown { conn: c.clone(), name: names.get(&c.pid).cloned() })
        .filter(|s| needle.is_empty() || matches(s, needle))
        .collect()
}

fn matches(s: &Shown, needle: &str) -> bool {
    let row = connection_row(s);
    [row.name.as_str(), &s.conn.pid.to_string(), row.protocol.as_str(), row.local.as_str(), row.remote.as_str(), row.state.as_str()]
        .iter()
        .any(|t| t.to_lowercase().contains(needle))
}

fn connection_row(s: &Shown) -> ConnectionRow {
    let c = &s.conn;
    ConnectionRow {
        name: s.name_text().into(),
        pid: c.pid as i32,
        protocol: c.protocol.name().into(),
        local: c.local.to_string().into(),
        remote: c.remote.map(|r| r.to_string()).unwrap_or_default().into(),
        state: texts::tcp_state(c.state).as_ref().into(),
        can_goto: s.can_goto(),
    }
}

fn compare(a: &Shown, b: &Shown, key: i32, descending: bool) -> Ordering {
    let lower = |s: &Shown| s.name_text().to_lowercase();
    let cmp = match key {
        0 => lower(a).cmp(&lower(b)),
        1 => a.conn.pid.cmp(&b.conn.pid),
        2 => a.conn.protocol.cmp(&b.conn.protocol),
        3 => a.conn.local.cmp(&b.conn.local),
        4 => a.conn.remote.cmp(&b.conn.remote),
        5 => a.conn.state.cmp(&b.conn.state),
        _ => Ordering::Equal,
    }
    .then_with(|| stable_order(&a.conn.key(), &b.conn.key()));
    if descending { cmp.reverse() } else { cmp }
}

/// 稳定键的全序，作为各列排序的次序依据，使相同列值的行在刷新之间不交换位置。
fn stable_order(a: &ConnectionKey, b: &ConnectionKey) -> Ordering {
    a.cmp(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tml_core::connections::{Protocol, TcpState};

    fn tcp(local: &str, remote: &str, state: TcpState, pid: u32) -> Connection {
        Connection {
            protocol: if local.starts_with('[') { Protocol::Tcp6 } else { Protocol::Tcp },
            local: local.parse().unwrap(),
            remote: Some(remote.parse().unwrap()),
            state: Some(state),
            pid,
        }
    }

    fn udp(local: &str, pid: u32) -> Connection {
        Connection { protocol: Protocol::Udp, local: local.parse().unwrap(), remote: None, state: None, pid }
    }

    fn sample() -> (Vec<Connection>, HashMap<u32, String>) {
        let all = vec![
            tcp("0.0.0.0:135", "0.0.0.0:0", TcpState::Listen, 1200),
            tcp("192.168.1.5:50000", "20.1.2.3:443", TcpState::Established, 4000),
            tcp("[::]:445", "[::]:0", TcpState::Listen, 4),
            udp("0.0.0.0:5353", 3000),
            tcp("127.0.0.1:9000", "127.0.0.1:9001", TcpState::TimeWait, 0),
        ];
        let names = [(1200, "svchost.exe"), (4000, "chrome.exe"), (4, "System")]
            .into_iter()
            .map(|(p, n)| (p, n.to_string()))
            .collect();
        (all, names)
    }

    fn pids(v: &[Shown]) -> Vec<u32> {
        v.iter().map(|s| s.conn.pid).collect()
    }

    #[test]
    fn search_matches_name_pid_address_port_and_state() {
        let (all, names) = sample();
        assert_eq!(visible(&all, &names, "").len(), 5);
        assert_eq!(pids(&visible(&all, &names, "chrome")), [4000]);
        assert_eq!(pids(&visible(&all, &names, "4000")), [4000]);
        assert_eq!(pids(&visible(&all, &names, "20.1.2.3")), [4000]);
        assert_eq!(pids(&visible(&all, &names, ":5353")), [3000]);
        assert_eq!(pids(&visible(&all, &names, "监听")), [1200, 4]);
        assert_eq!(pids(&visible(&all, &names, "udp")), [3000]);
        assert_eq!(pids(&visible(&all, &names, "tcp6")), [4]);
        assert!(visible(&all, &names, "zzz").is_empty());
    }

    #[test]
    fn process_name_resolution_and_goto_availability() {
        let (all, names) = sample();
        let shown = visible(&all, &names, "");
        let by_pid = |pid| shown.iter().find(|s| s.conn.pid == pid).unwrap();
        assert_eq!((by_pid(1200).name_text(), by_pid(1200).can_goto()), ("svchost.exe", true));
        // 快照里没有 3000：显示“—”，不可跳转。
        assert_eq!((by_pid(3000).name_text(), by_pid(3000).can_goto()), ("—", false));
        // PID 0（TIME_WAIT 等）不可跳转。
        assert!(!by_pid(0).can_goto());
        let row = connection_row(by_pid(3000));
        assert_eq!((row.protocol.as_str(), row.remote.as_str(), row.state.as_str()), ("UDP", "", ""));
        let row = connection_row(by_pid(4));
        assert_eq!((row.protocol.as_str(), row.local.as_str(), row.state.as_str()), ("TCP6", "[::]:445", "监听"));
    }

    #[test]
    fn every_column_sorts_both_directions() {
        let (all, names) = sample();
        for key in 0..6 {
            let mut rows = visible(&all, &names, "");
            rows.sort_by(|a, b| compare(a, b, key, false));
            let asc = rows.iter().map(|s| s.conn.key()).collect::<Vec<_>>();
            rows.sort_by(|a, b| compare(a, b, key, true));
            let mut desc = rows.iter().map(|s| s.conn.key()).collect::<Vec<_>>();
            desc.reverse();
            assert_eq!(asc, desc, "列 {key}：降序应恰为升序的逆序（稳定键打破平局）");
        }
        let mut rows = visible(&all, &names, "");
        rows.sort_by(|a, b| compare(a, b, 1, false));
        assert_eq!(pids(&rows), [0, 4, 1200, 3000, 4000]);
        rows.sort_by(|a, b| compare(a, b, 3, false));
        // 本地地址：IPv4 在 IPv6 之前，再按地址、端口。
        assert_eq!(rows[0].conn.local, "0.0.0.0:135".parse::<SocketAddr>().unwrap());
        assert_eq!(rows.last().unwrap().conn.protocol, Protocol::Tcp6);
    }

    #[test]
    fn stable_key_ignores_state_and_tells_pids_apart() {
        let a = tcp("10.0.0.1:5000", "10.0.0.2:80", TcpState::Established, 10);
        let mut b = a.clone();
        b.state = Some(TcpState::CloseWait);
        assert_eq!(a.key(), b.key(), "状态变化仍是同一条连接");
        let mut c = a.clone();
        c.pid = 11;
        assert_ne!(a.key(), c.key());
        let mut d = a.clone();
        d.protocol = Protocol::Udp;
        assert_ne!(a.key(), d.key());
        // 同一列值相同的两行：排序结果与输入顺序无关。
        let names = HashMap::new();
        let x = visible(&[a.clone(), c.clone()], &names, "");
        let y = visible(&[c, a], &names, "");
        let order = |mut v: Vec<Shown>| {
            v.sort_by(|p, q| compare(p, q, 0, false));
            pids(&v)
        };
        assert_eq!(order(x), order(y));
    }

    #[test]
    fn dump_line_has_no_chinese_state_and_uses_tabs() {
        let line = dump_line(&tcp("127.0.0.1:80", "0.0.0.0:0", TcpState::Listen, 7), Some("a.exe"));
        assert_eq!(line, "TCP\t127.0.0.1:80\t0.0.0.0:0\tListen\t7\ta.exe\r\n");
        assert_eq!(dump_line(&udp("0.0.0.0:53", 8), None), "UDP\t0.0.0.0:53\t\t\t8\t\r\n");
    }

    /// Ctrl+1…6 的页序（连接页插在服务之后、设置之前）；连接页的工具栏在各宽度下与服务页一样放得下。
    #[test]
    fn shortcut_pages_and_toolbar_on_connections_page() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter, WindowEvent};
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).expect("每个测试线程只设置一次平台");
        let ui = AppWindow::new().expect("创建界面");
        let pages: Vec<i32> = ["1", "2", "3", "4", "5", "6", "7", "0"].iter().map(|k| ui.invoke_shortcut_page_of((*k).into())).collect();
        assert_eq!(pages, [0, 1, 3, 4, CONNECTIONS_PAGE, 2, -1, -1]);
        for page in [SERVICES_PAGE, CONNECTIONS_PAGE] {
            ui.set_page(page);
            for w in [760u32, 800, 900, 1080, 1400] {
                ui.window().set_size(slint::LogicalSize::new(w as f32, 600.0));
                ui.window().dispatch_event(WindowEvent::PointerExited);
                let room = w as f32 - ui.get_sidebar_width();
                assert!(ui.get_toolbar_min_width() <= room + 0.01, "页面 {page} 宽 {w}：工具栏放不下");
            }
        }
    }

    #[test]
    fn tcp_state_texts() {
        assert_eq!(texts::tcp_state(None), "");
        assert_eq!(texts::tcp_state(Some(TcpState::Listen)), "监听");
        assert_eq!(texts::tcp_state(Some(TcpState::Established)), "已建立");
        assert_eq!(texts::tcp_state(Some(TcpState::CloseWait)), "等待关闭");
        assert_eq!(texts::tcp_state(Some(TcpState::Unknown(99))), "未知（99）");
    }
}

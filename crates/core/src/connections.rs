//! TCP / UDP 连接与监听端口（#77）：`GetExtendedTcpTable` / `GetExtendedUdpTable`（`*_OWNER_PID`），
//! IPv4 与 IPv6 都读。普通权限即可。不做反向域名解析。
//!
//! 表项按固定的内存布局从字节里解析（便于单测字节序）：地址按网络字节序原样存放，
//! 端口在 32 位字段的低 16 位、网络字节序。

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use crate::SystemError;

/// 协议与地址族，一列显示：TCP、TCP6、UDP、UDP6（与 `netstat` 的协议名一致，6 表示 IPv6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Protocol {
    Tcp,
    Tcp6,
    Udp,
    Udp6,
}

impl Protocol {
    pub const ALL: [Protocol; 4] = [Protocol::Tcp, Protocol::Tcp6, Protocol::Udp, Protocol::Udp6];

    /// 协议名（不是界面文案，是协议标识本身，界面与日志可以直接用）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Tcp6 => "TCP6",
            Self::Udp => "UDP",
            Self::Udp6 => "UDP6",
        }
    }

    pub fn is_tcp(self) -> bool {
        matches!(self, Self::Tcp | Self::Tcp6)
    }

    pub fn is_ipv6(self) -> bool {
        matches!(self, Self::Tcp6 | Self::Udp6)
    }
}

/// TCP 连接状态（`MIB_TCP_STATE`）。UDP 没有状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
    /// 系统返回了未知的状态值。
    Unknown(u32),
}

impl TcpState {
    /// 由 `MIB_TCP_STATE` 的数值得出（1 = CLOSED … 12 = DELETE_TCB）。
    pub fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::Closed,
            2 => Self::Listen,
            3 => Self::SynSent,
            4 => Self::SynReceived,
            5 => Self::Established,
            6 => Self::FinWait1,
            7 => Self::FinWait2,
            8 => Self::CloseWait,
            9 => Self::Closing,
            10 => Self::LastAck,
            11 => Self::TimeWait,
            12 => Self::DeleteTcb,
            v => Self::Unknown(v),
        }
    }
}

/// 一条连接或监听端点。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Connection {
    pub protocol: Protocol,
    pub local: SocketAddr,
    /// 远程端点；UDP 没有（`None`）。监听中的 TCP 是全零地址与端口 0。
    pub remote: Option<SocketAddr>,
    /// TCP 状态；UDP 为 `None`。
    pub state: Option<TcpState>,
    pub pid: u32,
}

/// 连接的稳定标识：协议 + 本地端点 + 远程端点 + PID（不含状态，状态变化时仍是同一条）。
pub type ConnectionKey = (Protocol, SocketAddr, Option<SocketAddr>, u32);

impl Connection {
    pub fn key(&self) -> ConnectionKey {
        (self.protocol, self.local, self.remote, self.pid)
    }
}

/// 读取连接表失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionError {
    /// 系统调用失败，带出错的表（协议）与系统错误码。
    System { protocol: Protocol, error: SystemError },
    /// 表在两次调用之间不断变大，重试多次仍未读到。
    KeepsGrowing { protocol: Protocol },
}

impl ConnectionError {
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::System { protocol, .. } | Self::KeepsGrowing { protocol } => *protocol,
        }
    }

    /// Win32 错误码；没有时为 `None`。
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::System { error, .. } => error.win32_code(),
            Self::KeepsGrowing { .. } => None,
        }
    }
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System { protocol, error } => write!(f, "{} table: {error}", protocol.name()),
            Self::KeepsGrowing { protocol } => write!(f, "{} table kept growing", protocol.name()),
        }
    }
}

impl std::error::Error for ConnectionError {}

const NO_ERROR: u32 = 0;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// 表大小在两次调用之间变化时的最多尝试次数。
const MAX_ATTEMPTS: usize = 8;

/// 第一次调用得到所需大小、按需扩大缓冲区后再调用，直到成功；`call(buf, size)` 返回 Win32 错误码。
/// 缓冲区以 `u32` 为单位，保证表项按 4 字节对齐。返回有效的字节数据（长度为系统给出的大小）。
pub(crate) fn fetch_table(mut call: impl FnMut(&mut [u32], &mut u32) -> u32) -> Result<Vec<u8>, Option<u32>> {
    let mut buf: Vec<u32> = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        let mut size = (buf.len() * 4) as u32;
        match call(&mut buf, &mut size) {
            NO_ERROR => {
                let len = (size as usize).min(buf.len() * 4);
                let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).take(len).collect();
                return Ok(bytes);
            }
            ERROR_INSUFFICIENT_BUFFER => {
                // 多留一些余量：表可能在下一次调用前又变大。
                let need = size as usize + size as usize / 8 + 256;
                buf = vec![0; need.div_ceil(4)];
            }
            code => return Err(Some(code)),
        }
    }
    Err(None)
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// 端口：32 位字段的前两个字节，网络字节序（大端）。
fn port_at(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

fn v4_at(b: &[u8], off: usize) -> Ipv4Addr {
    Ipv4Addr::new(b[off], b[off + 1], b[off + 2], b[off + 3])
}

fn v6_at(b: &[u8], off: usize) -> Ipv6Addr {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[off..off + 16]);
    Ipv6Addr::from(a)
}

fn sock6(b: &[u8], addr: usize, scope: usize, port: usize) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(v6_at(b, addr), port_at(b, port), 0, u32_at(b, scope)))
}

/// 表项字节数：`MIB_TCPROW_OWNER_PID` 24、`MIB_TCP6ROW_OWNER_PID` 56、`MIB_UDPROW_OWNER_PID` 12、`MIB_UDP6ROW_OWNER_PID` 28。
fn row_size(p: Protocol) -> usize {
    match p {
        Protocol::Tcp => 24,
        Protocol::Tcp6 => 56,
        Protocol::Udp => 12,
        Protocol::Udp6 => 28,
    }
}

/// 解析一个 `*_OWNER_PID` 表：开头 4 字节为表项数，表项紧随其后。截断的数据只解析完整的表项。
pub fn parse_table(protocol: Protocol, bytes: &[u8], out: &mut Vec<Connection>) {
    if bytes.len() < 4 {
        return;
    }
    let count = u32_at(bytes, 0) as usize;
    let size = row_size(protocol);
    let available = (bytes.len() - 4) / size;
    for i in 0..count.min(available) {
        let r = &bytes[4 + i * size..4 + (i + 1) * size];
        let c = match protocol {
            Protocol::Tcp => Connection {
                protocol,
                state: Some(TcpState::from_raw(u32_at(r, 0))),
                local: SocketAddr::V4(SocketAddrV4::new(v4_at(r, 4), port_at(r, 8))),
                remote: Some(SocketAddr::V4(SocketAddrV4::new(v4_at(r, 12), port_at(r, 16)))),
                pid: u32_at(r, 20),
            },
            Protocol::Tcp6 => Connection {
                protocol,
                local: sock6(r, 0, 16, 20),
                remote: Some(sock6(r, 24, 40, 44)),
                state: Some(TcpState::from_raw(u32_at(r, 48))),
                pid: u32_at(r, 52),
            },
            Protocol::Udp => Connection {
                protocol,
                local: SocketAddr::V4(SocketAddrV4::new(v4_at(r, 0), port_at(r, 4))),
                remote: None,
                state: None,
                pid: u32_at(r, 8),
            },
            Protocol::Udp6 => Connection {
                protocol,
                local: sock6(r, 0, 16, 20),
                remote: None,
                state: None,
                pid: u32_at(r, 24),
            },
        };
        out.push(c);
    }
}

fn read_table(protocol: Protocol) -> Result<Vec<u8>, ConnectionError> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    let af = if protocol.is_ipv6() { AF_INET6.0 } else { AF_INET.0 } as u32;
    let result = fetch_table(|buf, size| {
        let ptr = (!buf.is_empty()).then(|| buf.as_mut_ptr().cast());
        unsafe {
            if protocol.is_tcp() {
                GetExtendedTcpTable(ptr, size, false, af, TCP_TABLE_OWNER_PID_ALL, 0)
            } else {
                GetExtendedUdpTable(ptr, size, false, af, UDP_TABLE_OWNER_PID, 0)
            }
        }
    });
    match result {
        Ok(bytes) => Ok(bytes),
        // 没有该协议栈（如禁用了 IPv6）：视为空表。
        Err(Some(ERROR_NOT_SUPPORTED)) => Ok(Vec::new()),
        Err(Some(code)) => Err(ConnectionError::System { protocol, error: SystemError::Win32(code as i32) }),
        Err(None) => Err(ConnectionError::KeepsGrowing { protocol }),
    }
}

/// 枚举全部 TCP / UDP（IPv4、IPv6）连接与监听端点。任何一张表读取失败即返回错误。
pub fn list() -> Result<Vec<Connection>, ConnectionError> {
    let mut out = Vec::new();
    for p in Protocol::ALL {
        let bytes = read_table(p)?;
        parse_table(p, &bytes, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[Vec<u8>]) -> Vec<u8> {
        let mut b = (rows.len() as u32).to_ne_bytes().to_vec();
        for r in rows {
            b.extend_from_slice(r);
        }
        b
    }

    /// 端口字段：低 16 位网络字节序，高 16 位是垃圾（系统不保证为 0）。
    fn port_field(port: u16) -> [u8; 4] {
        let p = port.to_be_bytes();
        [p[0], p[1], 0xAB, 0xCD]
    }

    #[test]
    fn parses_tcp4_rows_with_network_byte_order() {
        let mut r = Vec::new();
        r.extend_from_slice(&5u32.to_ne_bytes());
        r.extend_from_slice(&[192, 168, 1, 20]);
        r.extend_from_slice(&port_field(50123));
        r.extend_from_slice(&[93, 184, 216, 34]);
        r.extend_from_slice(&port_field(443));
        r.extend_from_slice(&4321u32.to_ne_bytes());
        let mut out = Vec::new();
        parse_table(Protocol::Tcp, &table(&[r]), &mut out);
        assert_eq!(out.len(), 1);
        let c = &out[0];
        assert_eq!(c.local, "192.168.1.20:50123".parse().unwrap());
        assert_eq!(c.remote, Some("93.184.216.34:443".parse().unwrap()));
        assert_eq!((c.state, c.pid, c.protocol), (Some(TcpState::Established), 4321, Protocol::Tcp));
    }

    #[test]
    fn parses_tcp6_and_udp_rows() {
        let local: Ipv6Addr = "fe80::1".parse().unwrap();
        let mut r = Vec::new();
        r.extend_from_slice(&local.octets());
        r.extend_from_slice(&7u32.to_ne_bytes());
        r.extend_from_slice(&port_field(8080));
        r.extend_from_slice(&Ipv6Addr::UNSPECIFIED.octets());
        r.extend_from_slice(&0u32.to_ne_bytes());
        r.extend_from_slice(&port_field(0));
        r.extend_from_slice(&2u32.to_ne_bytes());
        r.extend_from_slice(&99u32.to_ne_bytes());
        assert_eq!(r.len(), 56);
        let mut out = Vec::new();
        parse_table(Protocol::Tcp6, &table(&[r]), &mut out);
        assert_eq!(out[0].local, SocketAddr::V6(SocketAddrV6::new(local, 8080, 0, 7)));
        assert_eq!(out[0].local.to_string(), "[fe80::1%7]:8080");
        assert_eq!((out[0].state, out[0].pid), (Some(TcpState::Listen), 99));

        let mut u = Vec::new();
        u.extend_from_slice(&[127, 0, 0, 1]);
        u.extend_from_slice(&port_field(53));
        u.extend_from_slice(&8u32.to_ne_bytes());
        let mut u6 = Vec::new();
        u6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        u6.extend_from_slice(&0u32.to_ne_bytes());
        u6.extend_from_slice(&port_field(5353));
        u6.extend_from_slice(&9u32.to_ne_bytes());
        let mut out = Vec::new();
        parse_table(Protocol::Udp, &table(&[u]), &mut out);
        parse_table(Protocol::Udp6, &table(&[u6]), &mut out);
        assert_eq!(out[0].local.to_string(), "127.0.0.1:53");
        assert_eq!((out[0].remote, out[0].state, out[0].pid), (None, None, 8));
        assert_eq!(out[1].local.to_string(), "[::1]:5353");
        assert_eq!(out[1].pid, 9);
    }

    #[test]
    fn truncated_table_only_yields_complete_rows() {
        let mut b = table(&[vec![0u8; 12]]);
        b[0..4].copy_from_slice(&3u32.to_ne_bytes());
        let mut out = Vec::new();
        parse_table(Protocol::Udp, &b, &mut out);
        assert_eq!(out.len(), 1);
        parse_table(Protocol::Udp, &[1, 0], &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn tcp_state_mapping() {
        let all = (1..=12).map(TcpState::from_raw).collect::<Vec<_>>();
        assert_eq!(all[0], TcpState::Closed);
        assert_eq!(all[1], TcpState::Listen);
        assert_eq!(all[4], TcpState::Established);
        assert_eq!(all[7], TcpState::CloseWait);
        assert_eq!(all[10], TcpState::TimeWait);
        assert_eq!(all[11], TcpState::DeleteTcb);
        assert!(all.iter().all(|s| !matches!(s, TcpState::Unknown(_))));
        assert_eq!(TcpState::from_raw(0), TcpState::Unknown(0));
        assert_eq!(TcpState::from_raw(13), TcpState::Unknown(13));
    }

    #[test]
    fn fetch_retries_while_the_table_grows() {
        // 第一次：缓冲区为空，要 100 字节；第二次表又长到 1000 字节；第三次成功。
        let mut needs = vec![100u32, 1000, 1000].into_iter();
        let mut calls = 0;
        let bytes = fetch_table(|buf, size| {
            calls += 1;
            let need = needs.next().unwrap();
            if (buf.len() * 4) < need as usize {
                *size = need;
                return ERROR_INSUFFICIENT_BUFFER;
            }
            buf[0] = 0x0102_0304;
            *size = need;
            NO_ERROR
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(bytes.len(), 1000);
        assert_eq!(u32_at(&bytes, 0), 0x0102_0304);
    }

    #[test]
    fn fetch_gives_up_and_reports_codes() {
        let mut n = 0u32;
        let r = fetch_table(|buf, size| {
            n += 1;
            *size = (buf.len() * 4) as u32 + 4096;
            ERROR_INSUFFICIENT_BUFFER
        });
        assert_eq!(r, Err(None));
        assert_eq!(n as usize, MAX_ATTEMPTS);
        assert_eq!(fetch_table(|_, _| 5), Err(Some(5)));
    }

    #[test]
    fn error_carries_protocol_and_code() {
        let e = ConnectionError::System { protocol: Protocol::Udp6, error: SystemError::Win32(5) };
        assert_eq!((e.protocol(), e.code()), (Protocol::Udp6, Some(5)));
        assert_eq!(ConnectionError::KeepsGrowing { protocol: Protocol::Tcp }.code(), None);
    }

    /// 真实系统调用：自己开监听并连上，按本进程 PID 找到监听端口与已建立的连接。
    #[test]
    fn lists_own_listener_and_established_connection() {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let client_port = client.local_addr().unwrap().port();
        let me = std::process::id();
        let all = list().unwrap();
        assert!(!all.is_empty());
        assert!(
            all.iter().any(|c| c.protocol == Protocol::Tcp
                && c.pid == me
                && c.state == Some(TcpState::Listen)
                && c.local.port() == port),
            "找不到本进程的监听端口 {port}"
        );
        assert!(
            all.iter().any(|c| c.protocol == Protocol::Tcp
                && c.pid == me
                && c.state == Some(TcpState::Established)
                && c.local.port() == client_port
                && c.remote.map(|r| r.port()) == Some(port)),
            "找不到本进程的已建立连接 {client_port} -> {port}"
        );
    }
}

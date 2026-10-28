//! 活动物理网卡：以 LUID 为稳定标识，独立维护累计字节基线。
use std::collections::HashMap;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::{IpHelper::*, WiFi::*};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkAdapter {
    pub id: String,
    pub name: String,
    pub connection: String,
    pub link_speed: u64,
    pub addresses: String,
    pub ssid: String,
    pub recv: Option<f64>,
    pub send: Option<f64>,
}

#[derive(Default)]
pub(crate) struct NetworkProbe {
    previous: HashMap<u64, (u64, u64)>,
}

fn delta(
    previous: Option<(u64, u64)>,
    current: (u64, u64),
    elapsed: f64,
) -> (Option<f64>, Option<f64>) {
    match previous {
        Some(p) if elapsed > 0.0 && current.0 >= p.0 && current.1 >= p.1 => (
            Some((current.0 - p.0) as f64 / elapsed),
            Some((current.1 - p.1) as f64 / elapsed),
        ),
        _ => (None, None),
    }
}

impl NetworkProbe {
    pub fn sample(&mut self, elapsed: f64) -> Vec<NetworkAdapter> {
        unsafe {
            let mut table = std::ptr::null_mut();
            if GetIfTable2(&mut table).is_err() || table.is_null() {
                self.previous.clear();
                return vec![];
            }
            let mut size = 0;
            let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
            GetAdaptersAddresses(0, flags, None, None, &mut size);
            // u64 对齐，且缓冲区在链表遍历期间保持存活。
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let mut addr = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            if GetAdaptersAddresses(0, flags, None, Some(addr), &mut size) != 0 {
                addr = std::ptr::null_mut();
            }
            let mut addresses = HashMap::new();
            while !addr.is_null() {
                let a = &*addr;
                let mut values = Vec::new();
                let mut u = a.FirstUnicastAddress;
                while !u.is_null() {
                    let socket = (*u).Address;
                    if !socket.lpSockaddr.is_null() {
                        let bytes = std::slice::from_raw_parts(
                            socket.lpSockaddr.cast::<u8>(),
                            socket.iSockaddrLength.max(0) as usize,
                        );
                        match (*socket.lpSockaddr).sa_family.0 {
                            2 if bytes.len() >= 8 => values.push(
                                std::net::Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7])
                                    .to_string(),
                            ),
                            23 if bytes.len() >= 24 => values.push(
                                std::net::Ipv6Addr::from(
                                    <[u8; 16]>::try_from(&bytes[8..24]).unwrap(),
                                )
                                .to_string(),
                            ),
                            _ => {}
                        }
                    }
                    u = (*u).Next;
                }
                addresses.insert(a.Luid.Value, values.join(" · "));
                addr = a.Next;
            }
            let mut handle = HANDLE::default();
            let mut version = 0;
            let wlan = WlanOpenHandle(2, None, &mut version, &mut handle) == 0;
            let mut next = HashMap::new();
            let mut result = Vec::new();
            for r in std::slice::from_raw_parts(
                std::ptr::addr_of!((*table).Table).cast::<MIB_IF_ROW2>(),
                (*table).NumEntries as usize,
            ) {
                if r.InterfaceAndOperStatusFlags._bitfield & 3 != 1 || r.OperStatus.0 != 1 {
                    continue;
                }
                let key = r.InterfaceLuid.Value;
                let current = (r.InOctets, r.OutOctets);
                let (recv, send) = delta(self.previous.get(&key).copied(), current, elapsed);
                next.insert(key, current);
                let mut ssid = String::new();
                if wlan && r.Type == 71 {
                    let mut data = std::ptr::null_mut();
                    let mut len = 0;
                    if WlanQueryInterface(
                        handle,
                        &r.InterfaceGuid,
                        wlan_intf_opcode_current_connection,
                        None,
                        &mut len,
                        &mut data,
                        None,
                    ) == 0
                        && !data.is_null()
                    {
                        if len as usize >= std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() {
                            let s = &(*(data.cast::<WLAN_CONNECTION_ATTRIBUTES>()))
                                .wlanAssociationAttributes
                                .dot11Ssid;
                            ssid = String::from_utf8_lossy(
                                &s.ucSSID[..(s.uSSIDLength as usize).min(32)],
                            )
                            .into_owned();
                        }
                        WlanFreeMemory(data);
                    }
                }
                result.push(NetworkAdapter {
                    id: format!("network:{key}"),
                    name: String::from_utf16_lossy(
                        &r.Alias[..r
                            .Alias
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(r.Alias.len())],
                    ),
                    connection: match r.Type {
                        71 => "Wi-Fi",
                        6 => "以太网",
                        _ => "其他",
                    }
                    .into(),
                    link_speed: r.ReceiveLinkSpeed.min(r.TransmitLinkSpeed),
                    addresses: addresses.remove(&key).unwrap_or_default(),
                    ssid,
                    recv,
                    send,
                });
            }
            if wlan {
                WlanCloseHandle(handle, None);
            }
            FreeMibTable(table.cast());
            self.previous = next;
            result.sort_by(|a, b| a.id.cmp(&b.id));
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_enumeration_has_unique_ids_and_independent_baselines() {
        let mut probe = NetworkProbe::default();
        let first = probe.sample(0.0);
        let ids: std::collections::HashSet<_> = first.iter().map(|n| &n.id).collect();
        assert_eq!(ids.len(), first.len());
        assert!(first.iter().all(|n| n.recv.is_none() && n.send.is_none()));
        let second = probe.sample(1.0);
        for n in second {
            if ids.contains(&n.id) {
                assert!(n.recv.is_some_and(|v| v.is_finite() && v >= 0.0));
                assert!(n.send.is_some_and(|v| v.is_finite() && v >= 0.0));
            }
        }
    }
    #[test]
    fn baselines_are_required_and_resets_are_gaps() {
        assert_eq!(delta(None, (100, 200), 1.0), (None, None));
        assert_eq!(
            delta(Some((100, 200)), (300, 300), 2.0),
            (Some(100.0), Some(50.0))
        );
        assert_eq!(delta(Some((100, 200)), (0, 300), 1.0), (None, None));
    }
}

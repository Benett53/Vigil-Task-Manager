//! Network adapters through the IP Helper API. The adapter list and addresses are read
//! every 30 seconds; each refresh only reads the counters of the listed adapters.

use crate::model::NetSample;
use std::collections::HashMap;
use std::time::Instant;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    GetAdaptersAddresses, GetIfEntry2, GetIfTable2, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
    MIB_IF_TABLE2,
};

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_LOOPBACK: u32 = 24;
const IF_TYPE_WIFI: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;
const OPER_STATUS_UP: i32 = 1;
const MEDIA_CONNECTED: i32 = 1;

struct Adapter {
    row: Box<MIB_IF_ROW2>,
    name: String,
    kind: &'static str,
    is_virtual: bool,
    ipv4: Vec<String>,
    ipv6: Vec<String>,
    mac: String,
    last: Option<(u64, u64)>,
}

#[derive(Default)]
pub struct Networks {
    adapters: Vec<Adapter>,
    listed: Option<Instant>,
    last: Option<Instant>,
}

fn wide_str(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

impl Networks {
    pub fn sample(&mut self) -> Vec<NetSample> {
        let now = Instant::now();
        if self
            .listed
            .is_none_or(|t| now.duration_since(t).as_secs() >= 30)
        {
            self.enumerate();
            self.listed = Some(now);
        }
        let elapsed = self
            .last
            .map(|l| now.duration_since(l).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.05);
        self.last = Some(now);
        let mut out: Vec<NetSample> = self
            .adapters
            .iter_mut()
            .filter_map(|a| {
                // SAFETY: the row keeps its LUID/index; GetIfEntry2 refreshes the rest in place.
                if unsafe { GetIfEntry2(&mut *a.row) } != 0 {
                    return None;
                }
                let r = &a.row;
                let (rx, tx) = (r.InOctets, r.OutOctets);
                let (rx_bps, tx_bps) = match a.last {
                    Some((prx, ptx)) => (
                        rx.saturating_sub(prx) as f64 / elapsed,
                        tx.saturating_sub(ptx) as f64 / elapsed,
                    ),
                    None => (0.0, 0.0),
                };
                a.last = Some((rx, tx));
                let connected =
                    r.OperStatus == OPER_STATUS_UP && r.MediaConnectState == MEDIA_CONNECTED;
                // Like Task Manager: disconnected adapters are hidden, except Wi-Fi.
                if !connected && a.kind != "Wi-Fi" {
                    return None;
                }
                Some(NetSample {
                    id: a.name.clone(),
                    kind: a.kind.into(),
                    rx_bps,
                    tx_bps,
                    rx_total: rx,
                    tx_total: tx,
                    link_mbps: (connected
                        && r.ReceiveLinkSpeed > 0
                        && r.ReceiveLinkSpeed != u64::MAX)
                        .then_some(r.ReceiveLinkSpeed / 1_000_000),
                    ipv4: a.ipv4.clone(),
                    ipv6: a.ipv6.clone(),
                    mac: a.mac.clone(),
                    connected,
                    is_virtual: a.is_virtual,
                    temperature: None,
                    signal: None,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (a.is_virtual, !a.connected, &a.id).cmp(&(b.is_virtual, !b.connected, &b.id))
        });
        out
    }

    fn enumerate(&mut self) {
        let addresses = addresses();
        let mut previous: HashMap<u32, Option<(u64, u64)>> = self
            .adapters
            .iter()
            .map(|a| (a.row.InterfaceIndex, a.last))
            .collect();
        self.adapters.clear();
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: the table is allocated by the API and released with FreeMibTable.
        unsafe {
            if GetIfTable2(&mut table) != 0 || table.is_null() {
                return;
            }
            let n = (*table).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), n);
            for r in rows {
                let flags = r.InterfaceAndOperStatusFlags._bitfield;
                let hardware = flags & 0x01 != 0;
                let filter = flags & 0x02 != 0;
                let up = r.OperStatus == OPER_STATUS_UP;
                if filter || r.Type == IF_TYPE_LOOPBACK || !(hardware || up) {
                    continue;
                }
                let name = wide_str(&r.Alias);
                let description = wide_str(&r.Description).to_lowercase();
                let is_virtual = !hardware;
                let kind = if r.Type == IF_TYPE_WIFI {
                    "Wi-Fi"
                } else if description.contains("bluetooth") {
                    "Bluetooth"
                } else if r.Type == IF_TYPE_TUNNEL || description.contains("vpn") {
                    "VPN / Tunnel"
                } else if is_virtual {
                    "Virtual adapter"
                } else if r.Type == IF_TYPE_ETHERNET {
                    "Ethernet"
                } else {
                    "Network"
                };
                let mac = r.PhysicalAddress[..(r.PhysicalAddressLength as usize).min(32)]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":");
                let (ipv4, ipv6) = addresses
                    .get(&r.InterfaceIndex)
                    .cloned()
                    .unwrap_or_default();
                self.adapters.push(Adapter {
                    row: Box::new(*r),
                    name,
                    kind,
                    is_virtual,
                    ipv4,
                    ipv6,
                    mac,
                    last: previous.remove(&r.InterfaceIndex).flatten(),
                });
            }
            FreeMibTable(table.cast());
        }
    }
}

/// IPv4 and IPv6 addresses per interface index.
fn addresses() -> HashMap<u32, (Vec<String>, Vec<String>)> {
    let mut out: HashMap<u32, (Vec<String>, Vec<String>)> = HashMap::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut buf: Vec<u64> = vec![0; 16 * 1024 / 8];
    for _ in 0..3 {
        let mut len = (buf.len() * 8) as u32;
        // SAFETY: the buffer is 8-byte aligned and `len` bytes long.
        let ret = unsafe {
            GetAdaptersAddresses(
                0,
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if ret == 111 {
            // ERROR_BUFFER_OVERFLOW: retry with the size the API asked for.
            buf = vec![0; (len as usize).div_ceil(8) + 1];
            continue;
        }
        if ret != 0 {
            return out;
        }
        // SAFETY: the API wrote a linked list of adapters into `buf`.
        unsafe {
            let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
            while !a.is_null() {
                let index = (*a).Anonymous1.Anonymous.IfIndex;
                let entry = out.entry(index).or_default();
                let mut u = (*a).FirstUnicastAddress;
                while !u.is_null() {
                    let sa = (*u).Address.lpSockaddr as *const u8;
                    if !sa.is_null() {
                        match *(sa as *const u16) {
                            2 => {
                                let b = std::slice::from_raw_parts(sa.add(4), 4);
                                entry.0.push(
                                    std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string(),
                                );
                            }
                            23 => {
                                let mut b = [0u8; 16];
                                b.copy_from_slice(std::slice::from_raw_parts(sa.add(8), 16));
                                entry.1.push(std::net::Ipv6Addr::from(b).to_string());
                            }
                            _ => {}
                        }
                    }
                    u = (*u).Next;
                }
                a = (*a).Next;
            }
        }
        return out;
    }
    out
}

//! Disk and network adapter monitoring.

use super::procfs::{DiskStat, parse_diskstats, parse_net_dev, read_into, read_trimmed, read_u64};
use crate::model::{DiskSample, NetSample};
use std::collections::HashMap;
use std::path::Path;

#[derive(Default)]
pub struct DiskMonitor {
    prev: HashMap<String, DiskStat>,
    meta: HashMap<String, DiskMeta>,
    mounts: HashMap<String, Vec<String>>,
    system_disks: Vec<String>,
    buf: String,
    tick: u64,
}

#[derive(Clone, Default)]
struct DiskMeta {
    model: String,
    kind: String,
    capacity: u64,
    removable: bool,
    physical: bool,
}

impl DiskMonitor {
    pub fn sample(&mut self, elapsed: f64) -> Vec<DiskSample> {
        if self.tick.is_multiple_of(15) {
            self.refresh_mounts();
        }
        self.tick += 1;
        if read_into("/proc/diskstats", &mut self.buf).is_err() {
            return Vec::new();
        }
        let buf = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        for (name, stat) in parse_diskstats(&buf) {
            if !self.meta.contains_key(name) {
                self.meta.insert(name.to_owned(), disk_meta(name));
            }
            let meta = &self.meta[name];
            if !meta.physical || meta.capacity == 0 {
                continue;
            }
            let prev = self.prev.insert(name.to_owned(), stat);
            let mut s = DiskSample {
                id: name.to_owned(),
                model: meta.model.clone(),
                kind: meta.kind.clone(),
                capacity: meta.capacity,
                removable: meta.removable,
                mounts: self.mounts.get(name).cloned().unwrap_or_default(),
                system_disk: self.system_disks.iter().any(|d| d == name),
                ..Default::default()
            };
            if let Some(p) = prev {
                let ms = (elapsed * 1000.0).max(1.0);
                s.active = Some(
                    (stat.io_ms.saturating_sub(p.io_ms) as f64 / ms * 100.0).clamp(0.0, 100.0)
                        as f32,
                );
                s.read_bps =
                    stat.sectors_read.saturating_sub(p.sectors_read) as f64 * 512.0 / elapsed;
                s.write_bps =
                    stat.sectors_written.saturating_sub(p.sectors_written) as f64 * 512.0 / elapsed;
                let ios = (stat.reads + stat.writes).saturating_sub(p.reads + p.writes);
                let wait = (stat.read_ms + stat.write_ms).saturating_sub(p.read_ms + p.write_ms);
                s.avg_response_ms = Some(if ios == 0 {
                    0.0
                } else {
                    wait as f64 / ios as f64
                });
            } else {
                s.active = Some(0.0);
                s.avg_response_ms = Some(0.0);
            }
            out.push(s);
        }
        self.buf = buf;
        out
    }

    fn refresh_mounts(&mut self) {
        self.mounts.clear();
        self.system_disks.clear();
        let Ok(content) = std::fs::read_to_string("/proc/self/mounts") else {
            return;
        };
        for line in content.lines() {
            let mut f = line.split_ascii_whitespace();
            let (Some(dev), Some(mnt)) = (f.next(), f.next()) else {
                continue;
            };
            if !dev.starts_with("/dev/") {
                continue;
            }
            let mnt = mnt.replace("\\040", " ");
            for disk in parent_disks(dev) {
                if mnt == "/" {
                    self.system_disks.push(disk.clone());
                }
                let list = self.mounts.entry(disk).or_default();
                if !list.contains(&mnt) && list.len() < 16 {
                    list.push(mnt.clone());
                }
            }
        }
    }
}

/// Resolves a block device path (partition, LVM/LUKS volume ...) to the physical disk(s) backing it.
fn parent_disks(dev_path: &str) -> Vec<String> {
    let resolved = std::fs::canonicalize(dev_path).unwrap_or_else(|_| dev_path.into());
    let Some(name) = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    resolve_block(&name, &mut out, 0);
    out
}

fn resolve_block(name: &str, out: &mut Vec<String>, depth: u32) {
    if depth > 8 {
        return;
    }
    let class = Path::new("/sys/class/block").join(name);
    // Device-mapper / md: follow the slaves.
    if let Ok(slaves) = std::fs::read_dir(class.join("slaves")) {
        let mut any = false;
        for s in slaves.flatten() {
            any = true;
            resolve_block(&s.file_name().to_string_lossy(), out, depth + 1);
        }
        if any {
            return;
        }
    }
    if class.join("partition").exists()
        && let Ok(real) = std::fs::canonicalize(&class)
        && let Some(parent) = real.parent().and_then(|p| p.file_name())
    {
        let p = parent.to_string_lossy().into_owned();
        if !out.contains(&p) {
            out.push(p);
        }
        return;
    }
    if !out.iter().any(|o| o == name) {
        out.push(name.to_owned());
    }
}

fn disk_meta(name: &str) -> DiskMeta {
    let block = Path::new("/sys/block").join(name);
    let excluded = ["loop", "ram", "zram", "dm-", "md", "nbd", "fd"]
        .iter()
        .any(|p| name.starts_with(p))
        || name.contains("boot")
        || name.contains("rpmb");
    let physical = !excluded && block.join("device").exists();
    let capacity = read_u64(block.join("size")).unwrap_or(0) * 512;
    let removable = read_u64(block.join("removable")).unwrap_or(0) == 1;
    let rotational = read_u64(block.join("queue/rotational")).unwrap_or(0) == 1;
    let model = read_trimmed(block.join("device/model"))
        .or_else(|| read_trimmed(block.join("device/name")))
        .unwrap_or_default();
    let kind = if name.starts_with("nvme") {
        "SSD (NVMe)"
    } else if name.starts_with("mmcblk") {
        "SD / eMMC"
    } else if name.starts_with("sr") {
        "Optical drive"
    } else if removable {
        "Removable"
    } else if name.starts_with("vd") || name.starts_with("xvd") {
        "Virtual disk"
    } else if rotational {
        "HDD"
    } else {
        "SSD"
    }
    .to_owned();
    // USB sticks often report removable=0; check the bus.
    let removable = removable
        || std::fs::canonicalize(&block)
            .map(|p| p.to_string_lossy().contains("/usb"))
            .unwrap_or(false);
    DiskMeta {
        model,
        kind,
        capacity,
        removable,
        physical,
    }
}

#[derive(Default)]
pub struct NetMonitor {
    prev: HashMap<String, (u64, u64)>,
    meta: HashMap<String, NetMeta>,
    addrs: HashMap<String, (Vec<String>, Vec<String>)>,
    buf: String,
    tick: u64,
}

#[derive(Clone, Default)]
struct NetMeta {
    kind: String,
    is_virtual: bool,
    mac: String,
}

impl NetMonitor {
    pub fn sample(&mut self, elapsed: f64) -> Vec<NetSample> {
        if self.tick.is_multiple_of(10) {
            self.addrs = interface_addresses();
            self.meta.clear(); // re-detect adapters occasionally (hotplug)
        }
        self.tick += 1;
        if read_into("/proc/net/dev", &mut self.buf).is_err() {
            return Vec::new();
        }
        let buf = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        for (name, rx, tx) in parse_net_dev(&buf) {
            if name == "lo" {
                continue;
            }
            let meta = self
                .meta
                .entry(name.to_owned())
                .or_insert_with(|| net_meta(name))
                .clone();
            let prev = self.prev.insert(name.to_owned(), (rx, tx));
            let (rx_bps, tx_bps) = match prev {
                Some((prx, ptx)) => (
                    rx.saturating_sub(prx) as f64 / elapsed,
                    tx.saturating_sub(ptx) as f64 / elapsed,
                ),
                None => (0.0, 0.0),
            };
            let sys = Path::new("/sys/class/net").join(name);
            let oper = read_trimmed(sys.join("operstate")).unwrap_or_default();
            let carrier = read_u64(sys.join("carrier")).unwrap_or(0) == 1;
            let connected = oper == "up" || (oper == "unknown" && carrier);
            let link = read_trimmed(sys.join("speed"))
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|s| *s > 0)
                .map(|s| s as u64);
            let (ipv4, ipv6) = self.addrs.get(name).cloned().unwrap_or_default();
            out.push(NetSample {
                id: name.to_owned(),
                kind: meta.kind,
                rx_bps,
                tx_bps,
                rx_total: rx,
                tx_total: tx,
                link_mbps: link,
                ipv4,
                ipv6,
                mac: meta.mac,
                connected,
                is_virtual: meta.is_virtual,
            });
        }
        self.buf = buf;
        out
    }
}

fn net_meta(name: &str) -> NetMeta {
    let sys = Path::new("/sys/class/net").join(name);
    let wireless = sys.join("wireless").exists() || sys.join("phy80211").exists();
    let has_device = sys.join("device").exists();
    let type_id = read_u64(sys.join("type")).unwrap_or(1);
    let kind = if wireless {
        "Wi-Fi"
    } else if name.starts_with("ww") || type_id == 519 {
        "Mobile broadband"
    } else if name.starts_with("wg")
        || name.starts_with("tun")
        || name.starts_with("tap")
        || type_id == 65534
    {
        "VPN / Tunnel"
    } else if name.starts_with("docker")
        || name.starts_with("br")
        || name.starts_with("virbr")
        || sys.join("bridge").exists()
    {
        "Bridge"
    } else if name.starts_with("veth") {
        "Virtual Ethernet"
    } else if has_device {
        "Ethernet"
    } else {
        "Virtual adapter"
    }
    .to_owned();
    NetMeta {
        kind,
        is_virtual: !has_device,
        mac: read_trimmed(sys.join("address")).unwrap_or_default(),
    }
}

/// IPv4/IPv6 addresses per interface via `getifaddrs`.
fn interface_addresses() -> HashMap<String, (Vec<String>, Vec<String>)> {
    let mut out: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    // SAFETY: getifaddrs allocates a linked list we walk read-only and free with freeifaddrs.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name)
                .to_string_lossy()
                .into_owned();
            let family = (*ifa.ifa_addr).sa_family as i32;
            if family == libc::AF_INET {
                let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                out.entry(name).or_default().0.push(ip.to_string());
            } else if family == libc::AF_INET6 {
                let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                let ip = std::net::Ipv6Addr::from(sa.sin6_addr.s6_addr);
                out.entry(name).or_default().1.push(ip.to_string());
            }
        }
        libc::freeifaddrs(ifap);
    }
    out
}

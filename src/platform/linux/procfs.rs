//! Allocation-light parsers for the text files exported by procfs/sysfs.
//!
//! All parsers are pure functions operating on `&str` so they can be unit tested
//! without a running Linux system.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Reads a whole file into `buf` (cleared first), reusing its allocation.
pub fn read_into(path: impl AsRef<Path>, buf: &mut String) -> io::Result<()> {
    buf.clear();
    File::open(path)?.read_to_string(buf)?;
    Ok(())
}

/// Reads a small sysfs file and returns it trimmed.
pub fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

/// Fields of `/proc/<pid>/stat` that Vigil uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidStat<'a> {
    pub comm: &'a str,
    pub state: u8,
    pub ppid: u32,
    pub flags: u64,
    pub utime: u64,
    pub stime: u64,
    pub nice: i32,
    pub threads: u32,
    pub start_ticks: u64,
    pub vsize: u64,
    pub rss_pages: u64,
}

/// Kernel thread flag (include/linux/sched.h).
pub const PF_KTHREAD: u64 = 0x0020_0000;

/// Parses `/proc/<pid>/stat`. The command name may contain spaces and parentheses,
/// therefore the *last* closing parenthesis terminates it.
pub fn parse_pid_stat(s: &str) -> Option<PidStat<'_>> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = &s[open + 1..close];
    let mut it = s.get(close + 2..)?.split_ascii_whitespace();
    // Field numbers follow proc(5); field 3 is the first one after comm.
    let state = it.next()?.bytes().next()?;
    let ppid = it.next()?.parse().ok()?; // 4
    let mut it = it.skip(3); // 5 pgrp, 6 session, 7 tty_nr
    let _tpgid = it.next()?; // 8
    let flags = it.next()?.parse().ok()?; // 9
    let mut it = it.skip(4); // 10..13 fault counters
    let utime = it.next()?.parse().ok()?; // 14
    let stime = it.next()?.parse().ok()?; // 15
    let mut it = it.skip(3); // 16 cutime, 17 cstime, 18 priority
    let nice = it.next()?.parse().ok()?; // 19
    let threads = it.next()?.parse().ok()?; // 20
    let mut it = it.skip(1); // 21 itrealvalue
    let start_ticks = it.next()?.parse().ok()?; // 22
    let vsize = it.next()?.parse().ok()?; // 23
    let rss_pages = it.next()?.parse::<i64>().ok()?.max(0) as u64; // 24
    Some(PidStat {
        comm,
        state,
        ppid,
        flags,
        utime,
        stime,
        nice,
        threads,
        start_ticks,
        vsize,
        rss_pages,
    })
}

/// Parses `/proc/<pid>/statm` and returns `(resident_pages, shared_pages)`.
pub fn parse_statm(s: &str) -> Option<(u64, u64)> {
    let mut it = s.split_ascii_whitespace();
    let _size = it.next()?;
    let resident = it.next()?.parse().ok()?;
    let shared = it.next()?.parse().ok()?;
    Some((resident, shared))
}

/// Parses `/proc/<pid>/io` and returns `(read_bytes, write_bytes)`.
pub fn parse_pid_io(s: &str) -> Option<(u64, u64)> {
    let mut read = None;
    let mut write = None;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("read_bytes:") {
            read = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("write_bytes:") {
            write = v.trim().parse().ok();
        }
    }
    Some((read?, write?))
}

/// Joins the NUL separated `/proc/<pid>/cmdline` into a single display string.
pub fn parse_cmdline(raw: &[u8]) -> String {
    let trimmed = raw.strip_suffix(&[0]).unwrap_or(raw);
    let mut out = String::with_capacity(trimmed.len());
    for (i, part) in trimmed.split(|b| *b == 0).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(part));
    }
    out
}

/// First argument of a NUL separated command line.
pub fn cmdline_argv0(raw: &[u8]) -> &[u8] {
    raw.split(|b| *b == 0).next().unwrap_or(&[])
}

/// Aggregate CPU times from `/proc/stat` (in clock ticks).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    pub fn idle_all(&self) -> u64 {
        self.idle + self.iowait
    }

    pub fn kernel(&self) -> u64 {
        self.system + self.irq + self.softirq
    }

    /// Returns `(busy%, kernel%)` between two samples.
    pub fn usage_since(&self, prev: &CpuTimes) -> (f32, f32) {
        let total = self.total().saturating_sub(prev.total());
        if total == 0 {
            return (0.0, 0.0);
        }
        let idle = self.idle_all().saturating_sub(prev.idle_all());
        let kernel = self.kernel().saturating_sub(prev.kernel());
        let busy = total.saturating_sub(idle);
        (
            (busy as f64 * 100.0 / total as f64) as f32,
            (kernel as f64 * 100.0 / total as f64) as f32,
        )
    }
}

/// Parses `/proc/stat`. Returns the aggregate line and one entry per CPU, indexed by CPU number.
pub fn parse_proc_stat(s: &str) -> (CpuTimes, Vec<CpuTimes>) {
    let mut total = CpuTimes::default();
    let mut cores: Vec<CpuTimes> = Vec::new();
    for line in s.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            // cpu lines are always first; stop at the first other line.
            if !cores.is_empty() || total != CpuTimes::default() {
                break;
            }
            continue;
        };
        let mut it = rest.split_ascii_whitespace();
        let Some(label) = (if rest.starts_with(' ') {
            Some("")
        } else {
            it.next()
        }) else {
            continue;
        };
        let mut v = [0u64; 8];
        for slot in v.iter_mut() {
            *slot = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        }
        let t = CpuTimes {
            user: v[0],
            nice: v[1],
            system: v[2],
            idle: v[3],
            iowait: v[4],
            irq: v[5],
            softirq: v[6],
            steal: v[7],
        };
        if label.is_empty() {
            total = t;
        } else if let Ok(idx) = label.parse::<usize>() {
            if cores.len() <= idx {
                cores.resize(idx + 1, CpuTimes::default());
            }
            cores[idx] = t;
        }
    }
    (total, cores)
}

/// Parses `/proc/meminfo`. Values are converted to bytes.
pub fn parse_meminfo(s: &str) -> Meminfo {
    let mut m = Meminfo::default();
    for line in s.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let mut parts = rest.split_ascii_whitespace();
        let Some(v) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        let v = if parts.next() == Some("kB") {
            v * 1024
        } else {
            v
        };
        let slot = match key {
            "MemTotal" => &mut m.total,
            "MemFree" => &mut m.free,
            "MemAvailable" => &mut m.available,
            "Buffers" => &mut m.buffers,
            "Cached" => &mut m.cached,
            "SwapCached" => &mut m.swap_cached,
            "Dirty" => &mut m.dirty,
            "Writeback" => &mut m.writeback,
            "Shmem" => &mut m.shmem,
            "SReclaimable" => &mut m.sreclaimable,
            "SUnreclaim" => &mut m.sunreclaim,
            "KernelStack" => &mut m.kernel_stack,
            "PageTables" => &mut m.page_tables,
            "SwapTotal" => &mut m.swap_total,
            "SwapFree" => &mut m.swap_free,
            "CommitLimit" => &mut m.commit_limit,
            "Committed_AS" => &mut m.committed_as,
            "Zswap" => {
                m.zswap = Some(v);
                continue;
            }
            _ => continue,
        };
        *slot = v;
    }
    if m.available == 0 {
        // Kernels older than 3.14 lack MemAvailable; approximate it.
        m.available = m.free + m.cached + m.buffers + m.sreclaimable;
    }
    m
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meminfo {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub swap_cached: u64,
    pub dirty: u64,
    pub writeback: u64,
    pub shmem: u64,
    pub sreclaimable: u64,
    pub sunreclaim: u64,
    pub kernel_stack: u64,
    pub page_tables: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub commit_limit: u64,
    pub committed_as: u64,
    pub zswap: Option<u64>,
}

/// One line of `/proc/diskstats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskStat {
    pub reads: u64,
    pub sectors_read: u64,
    pub read_ms: u64,
    pub writes: u64,
    pub sectors_written: u64,
    pub write_ms: u64,
    pub io_ms: u64,
}

pub fn parse_diskstats(s: &str) -> Vec<(&str, DiskStat)> {
    let mut out = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        out.push((
            f[2],
            DiskStat {
                reads: n(3),
                sectors_read: n(5),
                read_ms: n(6),
                writes: n(7),
                sectors_written: n(9),
                write_ms: n(10),
                io_ms: n(12),
            },
        ));
    }
    out
}

/// Parses `/proc/net/dev` into `(interface, rx_bytes, tx_bytes)`.
pub fn parse_net_dev(s: &str) -> Vec<(&str, u64, u64)> {
    let mut out = Vec::new();
    for line in s.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let f: Vec<&str> = rest.split_ascii_whitespace().collect();
        if f.len() < 9 {
            continue;
        }
        let rx = f[0].parse().unwrap_or(0);
        let tx = f[8].parse().unwrap_or(0);
        out.push((name.trim(), rx, tx));
    }
    out
}

/// Parses a kernel CPU list such as `0-3,8,10-11` into indices.
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>())
                && b >= a
                && b - a < 65_536
            {
                out.extend(a..=b);
            }
        } else if let Ok(v) = part.trim().parse() {
            out.push(v);
        }
    }
    out
}

/// Parses cache sizes like `32K`, `1024K`, `16M`.
pub fn parse_size_suffix(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = match s.as_bytes().last()? {
        b'K' | b'k' => (&s[..s.len() - 1], 1024),
        b'M' | b'm' => (&s[..s.len() - 1], 1024 * 1024),
        b'G' | b'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().map(|v| v * mul)
}

/// Unescapes systemd unit name escapes (`\x2d` -> `-`).
pub fn systemd_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1] == b'x'
            && let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Information derived from the cgroup a process lives in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CgroupInfo {
    /// Application ID if the process runs in an application scope (e.g. `org.mozilla.firefox`).
    pub app_id: Option<String>,
    /// systemd unit (service or scope) the process belongs to.
    pub unit: Option<String>,
    /// True when the cgroup is inside a user manager (`user@UID.service`).
    pub user_session: bool,
}

/// Parses `/proc/<pid>/cgroup` (cgroup v2 preferred, v1 "name=systemd" as fallback).
pub fn parse_cgroup(s: &str) -> CgroupInfo {
    let mut path = None;
    for line in s.lines() {
        if let Some(p) = line.strip_prefix("0::") {
            path = Some(p);
            break;
        }
        if let Some((_, p)) = line.split_once(":name=systemd:") {
            path = Some(p);
        }
    }
    let Some(path) = path else {
        return CgroupInfo::default();
    };
    let mut info = CgroupInfo {
        user_session: path.contains("/user@"),
        ..Default::default()
    };
    // The innermost unit wins (a scope inside a service, etc.).
    for comp in path.split('/').rev() {
        if comp.ends_with(".service") || comp.ends_with(".scope") {
            if info.unit.is_none() {
                info.unit = Some(comp.to_owned());
            }
            if info.app_id.is_none() {
                info.app_id = app_id_from_unit(comp);
            }
        } else if comp.ends_with(".slice") && info.app_id.is_none() {
            // e.g. app-org.gnome.Terminal.slice/vte-spawn-....scope
            if let Some(id) = comp
                .strip_prefix("app-")
                .and_then(|c| c.strip_suffix(".slice"))
                && id.contains('.')
                && !id.starts_with("dbus")
            {
                info.app_id = Some(systemd_unescape(id));
            }
        }
    }
    info
}

/// Extracts the application ID from a systemd unit name following the
/// `app[-<launcher>]-<ApplicationID>[@<RANDOM>|-<RANDOM>].(scope|service)` convention,
/// or a snap scope `snap.<name>.<app>-<uuid>.scope`.
pub fn app_id_from_unit(unit: &str) -> Option<String> {
    if let Some(rest) = unit.strip_prefix("snap.") {
        let name = rest.split('.').next()?;
        return if name.is_empty() {
            None
        } else {
            Some(format!("snap:{name}"))
        };
    }
    let rest = unit.strip_prefix("app-")?;
    let rest = rest
        .strip_suffix(".scope")
        .or_else(|| rest.strip_suffix(".service"))?;
    // Remove the random suffix.
    let rest = match rest.find('@') {
        Some(at) => &rest[..at],
        None => match rest.rfind('-') {
            Some(dash)
                if !rest[dash + 1..].is_empty()
                    && rest[dash + 1..].bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                &rest[..dash]
            }
            _ => rest,
        },
    };
    // Remove an optional launcher prefix.
    let rest = ["gnome-", "flatpak-", "kde-", "cosmic-", "xfce-", "budgie-"]
        .iter()
        .find_map(|p| rest.strip_prefix(p))
        .unwrap_or(rest);
    if rest.is_empty() || rest.starts_with("dbus") {
        return None;
    }
    Some(systemd_unescape(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_stat_with_weird_name() {
        let s = "1234 (Web Content) (x)) S 1000 1234 1234 0 -1 4194560 100 0 0 0 250 75 0 0 20 0 12 0 98765 1234567 890 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let st = parse_pid_stat(s).unwrap();
        assert_eq!(st.comm, "Web Content) (x)");
        assert_eq!(st.state, b'S');
        assert_eq!(st.ppid, 1000);
        assert_eq!(st.flags, 4194560);
        assert_eq!(st.utime, 250);
        assert_eq!(st.stime, 75);
        assert_eq!(st.nice, 0);
        assert_eq!(st.threads, 12);
        assert_eq!(st.start_ticks, 98765);
        assert_eq!(st.vsize, 1234567);
        assert_eq!(st.rss_pages, 890);
    }

    #[test]
    fn pid_stat_kernel_thread() {
        let s = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 2 0 0 18446744073709551615 0 0 0 0 0 0 0 2147483647 0 0 0 0 0 1 0 0 0 0 0";
        let st = parse_pid_stat(s).unwrap();
        assert!(st.flags & PF_KTHREAD != 0);
        assert_eq!(st.ppid, 0);
    }

    #[test]
    fn pid_stat_negative_nice() {
        let s = "77 (pipewire) S 1 77 77 0 -1 4194560 1 0 0 0 1 1 0 0 9 -11 3 0 500 1000 10 0";
        let st = parse_pid_stat(s).unwrap();
        assert_eq!(st.nice, -11);
    }

    #[test]
    fn pid_stat_truncated_is_none() {
        assert!(parse_pid_stat("12 (x) S 1 2 3").is_none());
        assert!(parse_pid_stat("").is_none());
    }

    #[test]
    fn statm_and_io() {
        assert_eq!(parse_statm("1000 200 50 10 0 300 0\n"), Some((200, 50)));
        let io = "rchar: 1\nwchar: 2\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_pid_io(io), Some((4096, 8192)));
    }

    #[test]
    fn cmdline_joining() {
        assert_eq!(
            parse_cmdline(b"/usr/bin/foo\0--bar\0baz qux\0"),
            "/usr/bin/foo --bar baz qux"
        );
        assert_eq!(cmdline_argv0(b"/usr/bin/foo\0--bar\0"), b"/usr/bin/foo");
        assert_eq!(parse_cmdline(b""), "");
    }

    #[test]
    fn proc_stat() {
        let s = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 50 0 25 400 25 0 0 0 0 0\ncpu1 50 0 25 400 25 0 0 0 0 0\nintr 1 2 3\nctxt 5\n";
        let (total, cores) = parse_proc_stat(s);
        assert_eq!(total.user, 100);
        assert_eq!(total.total(), 1000);
        assert_eq!(cores.len(), 2);
        let prev = CpuTimes::default();
        let (busy, kernel) = total.usage_since(&prev);
        assert!((busy - 15.0).abs() < 0.01);
        assert!((kernel - 5.0).abs() < 0.01);
    }

    #[test]
    fn meminfo() {
        let s = "MemTotal:       16000000 kB\nMemFree:         1000000 kB\nMemAvailable:    8000000 kB\nCached:          5000000 kB\nZswap:              1024 kB\nHugePages_Total:       0\n";
        let m = parse_meminfo(s);
        assert_eq!(m.total, 16_000_000 * 1024);
        assert_eq!(m.available, 8_000_000 * 1024);
        assert_eq!(m.zswap, Some(1024 * 1024));
    }

    #[test]
    fn diskstats() {
        let s = " 259       0 nvme0n1 1000 10 20000 300 2000 20 40000 500 0 700 800 0 0 0 0\n   7       0 loop0 1 0 2 0 0 0 0 0 0 0 0\n";
        let d = parse_diskstats(s);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].0, "nvme0n1");
        assert_eq!(d[0].1.sectors_read, 20000);
        assert_eq!(d[0].1.sectors_written, 40000);
        assert_eq!(d[0].1.io_ms, 700);
    }

    #[test]
    fn net_dev() {
        let s = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 100 1 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n  eth0: 5000 10 0 0 0 0 0 0 7000 12 0 0 0 0 0 0\n";
        let n = parse_net_dev(s);
        assert_eq!(n, vec![("lo", 100, 100), ("eth0", 5000, 7000)]);
    }

    #[test]
    fn cpu_lists_and_sizes() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
        assert_eq!(parse_size_suffix("32K"), Some(32 * 1024));
        assert_eq!(parse_size_suffix("16M"), Some(16 * 1024 * 1024));
    }

    #[test]
    fn cgroup_apps() {
        let gnome = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-4321.scope\n";
        let c = parse_cgroup(gnome);
        assert_eq!(c.app_id.as_deref(), Some("firefox"));
        assert!(c.user_session);

        let flatpak = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.mozilla.firefox-1234.scope";
        assert_eq!(
            parse_cgroup(flatpak).app_id.as_deref(),
            Some("org.mozilla.firefox")
        );

        let kde = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.kde.konsole@0a1b2c.service";
        assert_eq!(parse_cgroup(kde).app_id.as_deref(), Some("org.kde.konsole"));

        let escaped = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.gnome.Text\\x2dEditor-99.scope";
        assert_eq!(
            parse_cgroup(escaped).app_id.as_deref(),
            Some("org.gnome.Text-Editor")
        );

        let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.gnome.Terminal.slice/vte-spawn-abc.scope";
        let t = parse_cgroup(terminal);
        assert_eq!(t.app_id.as_deref(), Some("org.gnome.Terminal"));
        assert_eq!(t.unit.as_deref(), Some("vte-spawn-abc.scope"));

        let snap = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap.firefox.firefox-0f2c.scope";
        assert_eq!(parse_cgroup(snap).app_id.as_deref(), Some("snap:firefox"));

        let service = "0::/system.slice/NetworkManager.service";
        let s = parse_cgroup(service);
        assert_eq!(s.app_id, None);
        assert_eq!(s.unit.as_deref(), Some("NetworkManager.service"));
        assert!(!s.user_session);

        let v1 = "12:pids:/user.slice\n1:name=systemd:/system.slice/cron.service\n";
        assert_eq!(parse_cgroup(v1).unit.as_deref(), Some("cron.service"));
        assert_eq!(parse_cgroup(""), CgroupInfo::default());
    }

    #[test]
    fn unescape() {
        assert_eq!(systemd_unescape("a\\x2db\\x2fc"), "a-b/c");
        assert_eq!(systemd_unescape("plain"), "plain");
        assert_eq!(systemd_unescape("bad\\x"), "bad\\x");
    }
}

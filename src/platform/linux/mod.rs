//! Linux backend: reads `/proc` and `/sys` directly. No daemons, no root required.

pub mod actions;
pub mod desktop;
mod devices;
mod gpu;
mod process;
pub mod procfs;
pub mod services;
pub mod startup;
pub mod users;

use crate::model::{CpuSample, MemSample, Snapshot, StaticInfo, SystemSample};
use procfs::{
    CpuTimes, parse_cpu_list, parse_meminfo, parse_proc_stat, parse_size_suffix, read_into,
    read_trimmed, read_u64,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub struct Collector {
    info: Arc<StaticInfo>,
    scanner: process::ProcessScanner,
    gpu: gpu::GpuMonitor,
    disks: devices::DiskMonitor,
    nets: devices::NetMonitor,
    apps: desktop::AppIndex,
    apps_loaded: Instant,
    users: users::UserCache,
    prev_cpu: Option<(CpuTimes, Vec<CpuTimes>)>,
    last: Option<Instant>,
    seq: u64,
    buf: String,
    freq_paths: Vec<PathBuf>,
    temp_path: Option<PathBuf>,
}

impl Collector {
    pub fn new() -> Collector {
        let info = Arc::new(static_info());
        let boot_time = boot_time();
        let freq_paths = (0..info.logical.max(1))
            .map(|i| {
                PathBuf::from(format!(
                    "/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"
                ))
            })
            .filter(|p| p.exists())
            .collect();
        Collector {
            scanner: process::ProcessScanner::new(boot_time, info.current_uid),
            info,
            gpu: gpu::GpuMonitor::new(),
            disks: devices::DiskMonitor::default(),
            nets: devices::NetMonitor::default(),
            apps: desktop::AppIndex::load(),
            apps_loaded: Instant::now(),
            users: users::UserCache::default(),
            prev_cpu: None,
            last: None,
            seq: 0,
            buf: String::with_capacity(8192),
            freq_paths,
            temp_path: find_cpu_temp(),
        }
    }

    pub fn sample(&mut self, with_processes: bool) -> Snapshot {
        let now = Instant::now();
        let elapsed = self
            .last
            .map(|l| now.duration_since(l).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.05);
        self.last = Some(now);
        self.seq += 1;

        // Pick up newly installed applications every few minutes.
        if now.duration_since(self.apps_loaded).as_secs() > 300 {
            self.apps = desktop::AppIndex::load();
            self.apps_loaded = now;
        }

        let cpu = self.sample_cpu();
        let mem = self.sample_mem();
        let disks = self.disks.sample(elapsed);
        let nets = self.nets.sample(elapsed);

        self.gpu.begin(elapsed);
        let (procs, threads) = if with_processes {
            let r = self.scanner.scan(
                elapsed,
                self.info.logical,
                &self.apps,
                &mut self.users,
                &mut self.gpu,
            );
            (r.procs, r.threads)
        } else {
            (Vec::new(), 0)
        };
        let gpus = self.gpu.finish();

        let uptime = read_trimmed("/proc/uptime")
            .and_then(|s| {
                s.split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
            })
            .unwrap_or(0.0) as u64;
        let handles = read_trimmed("/proc/sys/fs/file-nr")
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()));

        Snapshot {
            seq: self.seq,
            cpu,
            mem,
            disks,
            nets,
            gpus,
            sys: SystemSample {
                uptime_secs: uptime,
                processes: procs.len(),
                threads,
                handles,
            },
            procs,
            info: self.info.clone(),
        }
    }

    fn sample_cpu(&mut self) -> CpuSample {
        let mut s = CpuSample::default();
        if read_into("/proc/stat", &mut self.buf).is_ok() {
            let (total, cores) = parse_proc_stat(&self.buf);
            if let Some((ptotal, pcores)) = &self.prev_cpu {
                let (busy, kernel) = total.usage_since(ptotal);
                s.total = busy;
                s.kernel = kernel;
                s.per_core = cores
                    .iter()
                    .enumerate()
                    .map(|(i, c)| pcores.get(i).map(|p| c.usage_since(p).0).unwrap_or(0.0))
                    .collect();
            } else {
                s.per_core = vec![0.0; cores.len()];
            }
            self.prev_cpu = Some((total, cores));
        }
        let mut sum = 0u64;
        let mut n = 0u64;
        for p in &self.freq_paths {
            if let Some(khz) = read_u64(p) {
                sum += khz;
                n += 1;
            }
        }
        if n > 0 {
            s.freq_mhz = Some(sum as f64 / n as f64 / 1000.0);
        }
        s.temperature = self
            .temp_path
            .as_ref()
            .and_then(read_u64)
            .map(|m| m as f32 / 1000.0);
        s.load_avg = read_trimmed("/proc/loadavg").and_then(|l| {
            let mut it = l.split_whitespace().map(|v| v.parse::<f32>().ok());
            Some([it.next()??, it.next()??, it.next()??])
        });
        s
    }

    fn sample_mem(&mut self) -> MemSample {
        if read_into("/proc/meminfo", &mut self.buf).is_err() {
            return MemSample::default();
        }
        let m = parse_meminfo(&self.buf);
        let cached_reclaimable = m.available.saturating_sub(m.free);
        MemSample {
            total: m.total,
            available: m.available,
            used: m.total.saturating_sub(m.available),
            free: m.free,
            cached: cached_reclaimable,
            modified: m.dirty + m.writeback,
            shared: m.shmem,
            buffers: m.buffers,
            committed: m.committed_as,
            commit_limit: m.commit_limit,
            kernel_paged: m.sreclaimable,
            kernel_nonpaged: m.sunreclaim + m.kernel_stack + m.page_tables,
            swap_total: m.swap_total,
            swap_used: m.swap_total.saturating_sub(m.swap_free + m.swap_cached),
            compressed: m.zswap,
        }
    }
}

fn boot_time() -> u64 {
    let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    stat.lines()
        .find_map(|l| l.strip_prefix("btime ").and_then(|v| v.trim().parse().ok()))
        .unwrap_or_else(|| {
            let up = read_trimmed("/proc/uptime")
                .and_then(|s| {
                    s.split_whitespace()
                        .next()
                        .and_then(|v| v.parse::<f64>().ok())
                })
                .unwrap_or(0.0);
            crate::util::now_unix().saturating_sub(up as u64)
        })
}

fn find_cpu_temp() -> Option<PathBuf> {
    let read = std::fs::read_dir("/sys/class/hwmon").ok()?;
    let mut candidates: Vec<(u8, PathBuf)> = Vec::new();
    for ent in read.flatten() {
        let dir = ent.path();
        let name = read_trimmed(dir.join("name")).unwrap_or_default();
        let prio = match name.as_str() {
            "k10temp" | "zenpower" => 0,
            "coretemp" => 1,
            "cpu_thermal" | "soc_thermal" | "cpu-thermal" => 2,
            "acpitz" => 5,
            _ => continue,
        };
        // Prefer the "Tctl"/"Package" sensor, otherwise temp1.
        let mut best = dir.join("temp1_input");
        for i in 1..=8 {
            let label = read_trimmed(dir.join(format!("temp{i}_label"))).unwrap_or_default();
            if label.starts_with("Tctl")
                || label.starts_with("Tdie")
                || label.starts_with("Package")
            {
                best = dir.join(format!("temp{i}_input"));
                break;
            }
        }
        if best.exists() {
            candidates.push((prio, best));
        }
    }
    candidates.sort_by_key(|c| c.0);
    candidates.into_iter().next().map(|c| c.1)
}

fn static_info() -> StaticInfo {
    let mut info = StaticInfo::default();
    info.hostname = read_trimmed("/proc/sys/kernel/hostname").unwrap_or_default();
    info.kernel = read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_default();
    info.os_name = ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_owned())
            })
        })
        .unwrap_or_else(|| "Linux".to_owned());
    info.desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default();
    info.session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "wayland".into()
        } else if std::env::var_os("DISPLAY").is_some() {
            "x11".into()
        } else {
            String::new()
        }
    });
    // SAFETY: getuid never fails.
    info.current_uid = unsafe { libc::getuid() };
    info.current_user =
        users::lookup(info.current_uid).unwrap_or_else(|| info.current_uid.to_string());

    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let field = |key: &str| {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    };
    info.cpu_model = field("model name")
        .or_else(|| field("Model"))
        .or_else(|| field("Hardware"))
        .or_else(|| field("cpu model"))
        .or_else(|| field("Processor"))
        .or_else(|| field("uarch"))
        .or_else(|| {
            read_trimmed("/sys/firmware/devicetree/base/model")
                .map(|m| m.trim_end_matches('\0').to_owned())
        })
        .unwrap_or_else(|| "Unknown processor".to_owned());
    let flags = field("flags")
        .or_else(|| field("Features"))
        .unwrap_or_default();
    let flag_set: HashSet<&str> = flags.split_whitespace().collect();
    info.virtualization = if flag_set.contains("vmx") {
        Some("Enabled (VT-x)".into())
    } else if flag_set.contains("svm") {
        Some("Enabled (AMD-V)".into())
    } else if flag_set.contains("hypervisor") {
        Some("Running in a virtual machine".into())
    } else if !flags.is_empty() && cpuinfo.contains("vendor_id") {
        Some("Not available".into())
    } else {
        None
    };

    // Topology
    let cpu_root = Path::new("/sys/devices/system/cpu");
    let online = read_trimmed(cpu_root.join("online"))
        .map(|s| parse_cpu_list(&s))
        .unwrap_or_default();
    let logical = if online.is_empty() {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        online.len()
    };
    info.logical = logical;
    let mut packages = HashSet::new();
    let mut cores = HashSet::new();
    for cpu in &online {
        let topo = cpu_root.join(format!("cpu{cpu}/topology"));
        let pkg = read_u64(topo.join("physical_package_id")).unwrap_or(0);
        let core = read_u64(topo.join("core_id")).unwrap_or(*cpu as u64);
        let cluster = read_u64(topo.join("cluster_id")).unwrap_or(0);
        packages.insert(pkg);
        cores.insert((pkg, cluster, core));
    }
    info.sockets = packages.len().max(1);
    info.cores = if cores.is_empty() {
        logical
    } else {
        cores.len()
    };

    // Caches: sum unique cache instances per level.
    let mut seen = HashSet::new();
    let (mut l1, mut l2, mut l3) = (0u64, 0u64, 0u64);
    for cpu in &online {
        let cache_dir = cpu_root.join(format!("cpu{cpu}/cache"));
        let Ok(read) = std::fs::read_dir(&cache_dir) else {
            continue;
        };
        for ent in read.flatten() {
            let d = ent.path();
            if !d
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("index"))
            {
                continue;
            }
            let level = read_u64(d.join("level")).unwrap_or(0);
            let kind = read_trimmed(d.join("type")).unwrap_or_default();
            let shared = read_trimmed(d.join("shared_cpu_list")).unwrap_or_else(|| cpu.to_string());
            let id = read_trimmed(d.join("id")).unwrap_or_default();
            if !seen.insert((level, kind.clone(), shared, id)) {
                continue;
            }
            let size = read_trimmed(d.join("size"))
                .and_then(|s| parse_size_suffix(&s))
                .unwrap_or(0);
            match level {
                1 => l1 += size,
                2 => l2 += size,
                3 => l3 += size,
                _ => {}
            }
        }
    }
    info.l1_cache = (l1 > 0).then_some(l1);
    info.l2_cache = (l2 > 0).then_some(l2);
    info.l3_cache = (l3 > 0).then_some(l3);

    // Base clock
    let freq = cpu_root.join("cpu0/cpufreq");
    if let Some(base) = read_u64(freq.join("base_frequency")) {
        info.cpu_base_mhz = Some(base as f64 / 1000.0);
    } else if let Some(max) = read_u64(freq.join("cpuinfo_max_freq")) {
        info.cpu_base_mhz = Some(max as f64 / 1000.0);
        info.cpu_base_is_max = true;
    } else if let Some(mhz) = field("cpu MHz").and_then(|v| v.parse::<f64>().ok()) {
        info.cpu_base_mhz = Some(mhz);
    }

    // Hardware reserved = physical memory blocks - MemTotal
    if let (Some(block), Ok(read)) = (
        read_trimmed("/sys/devices/system/memory/block_size_bytes")
            .and_then(|s| u64::from_str_radix(&s, 16).ok()),
        std::fs::read_dir("/sys/devices/system/memory"),
    ) {
        let online_blocks = read
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("memory"))
            .filter(|e| read_trimmed(e.path().join("online")).as_deref() == Some("1"))
            .count() as u64;
        let total = std::fs::read_to_string("/proc/meminfo")
            .map(|s| parse_meminfo(&s).total)
            .unwrap_or(0);
        let physical = online_blocks * block;
        if physical > total && total > 0 {
            info.hardware_reserved = Some(physical - total);
        }
    }
    info
}

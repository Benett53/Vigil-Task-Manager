//! Windows backend: `sysinfo` for the heavy lifting plus a few direct Win32 calls
//! for the data Task Manager users expect (priority classes, efficiency mode,
//! window-owning apps, commit charge, handles).

pub mod actions;
pub mod services;
pub mod startup;

use crate::model::{
    CpuSample, DiskSample, GpuSample, MemSample, NetSample, ProcKind, ProcState, Process, Snapshot,
    StaticInfo, SystemSample,
};
use crate::platform::nvidia::Nvidia;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use std::time::Instant;
use sysinfo::{
    CpuRefreshKind, DiskKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks,
    ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users,
};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::Threading::{
    GetPriorityClass, GetProcessInformation, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION, ProcessPowerThrottling,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId,
    IsWindowVisible,
};

pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Stable numeric id for a SID string (the model uses numeric user ids).
fn sid_hash(sid: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in sid.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn well_known_account(sid: &str) -> Option<&'static str> {
    match sid {
        "S-1-5-18" => Some("SYSTEM"),
        "S-1-5-19" => Some("LOCAL SERVICE"),
        "S-1-5-20" => Some("NETWORK SERVICE"),
        _ => None,
    }
}

struct Cached {
    start: u64,
    name: Arc<str>,
    exe: Arc<str>,
    cmdline: Arc<str>,
    app_name: Option<Arc<str>>,
    last_cpu_ms: u64,
}

pub struct Collector {
    sys: System,
    nets: Networks,
    disks: Disks,
    users: Users,
    info: Arc<StaticInfo>,
    current_sid: Option<String>,
    cache: HashMap<u32, Cached>,
    descriptions: HashMap<String, Option<Arc<str>>>,
    service_pids: HashMap<u32, Arc<str>>,
    nvidia: Option<Nvidia>,
    last: Option<Instant>,
    seq: u64,
}

impl Collector {
    pub fn new() -> Collector {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        let users = Users::new_with_refreshed_list();
        let current_pid = sysinfo::get_current_pid().ok();
        if let Some(pid) = current_pid {
            sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                false,
                ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
            );
        }
        let current_sid = current_pid
            .and_then(|p| sys.process(p))
            .and_then(|p| p.user_id())
            .map(|u| u.to_string());
        let current_user = current_sid
            .as_ref()
            .and_then(|sid| users.iter().find(|u| u.id().to_string() == *sid))
            .map(|u| u.name().to_owned())
            .or_else(|| std::env::var("USERNAME").ok())
            .unwrap_or_default();
        let cpus = sys.cpus();
        let info = StaticInfo {
            hostname: System::host_name().unwrap_or_default(),
            os_name: System::long_os_version().unwrap_or_else(|| "Windows".into()),
            kernel: System::kernel_version().unwrap_or_default(),
            desktop: "Windows".into(),
            session_type: String::new(),
            cpu_model: cpus
                .first()
                .map(|c| c.brand().trim().to_owned())
                .unwrap_or_default(),
            cpu_base_mhz: cpus
                .first()
                .map(|c| c.frequency() as f64)
                .filter(|f| *f > 0.0),
            cpu_base_is_max: false,
            sockets: 1,
            cores: System::physical_core_count().unwrap_or(cpus.len()),
            logical: cpus.len().max(1),
            virtualization: None,
            l1_cache: None,
            l2_cache: None,
            l3_cache: None,
            hardware_reserved: None,
            current_uid: current_sid.as_deref().map(sid_hash).unwrap_or(0),
            current_user,
        };
        Collector {
            sys,
            nets: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list_specifics(DiskRefreshKind::everything()),
            users,
            info: Arc::new(info),
            current_sid,
            cache: HashMap::new(),
            descriptions: HashMap::new(),
            service_pids: HashMap::new(),
            nvidia: Nvidia::init(),
            last: None,
            seq: 0,
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
        if self.seq % 10 == 1 {
            self.service_pids = services::pid_map();
        }

        self.sys
            .refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage().with_frequency());
        self.sys
            .refresh_memory_specifics(MemoryRefreshKind::everything());
        let cpus = self.sys.cpus();
        let freq = if cpus.is_empty() {
            None
        } else {
            Some(cpus.iter().map(|c| c.frequency() as f64).sum::<f64>() / cpus.len() as f64)
        };
        let cpu = CpuSample {
            total: self.sys.global_cpu_usage(),
            kernel: 0.0,
            per_core: cpus.iter().map(|c| c.cpu_usage()).collect(),
            freq_mhz: freq.filter(|f| *f > 0.0),
            temperature: None,
            load_avg: None,
        };

        let perf = performance_info();
        let page = perf.as_ref().map_or(4096, |p| p.PageSize as u64);
        let mem = MemSample {
            total: self.sys.total_memory(),
            available: self.sys.available_memory(),
            used: self
                .sys
                .total_memory()
                .saturating_sub(self.sys.available_memory()),
            free: self.sys.free_memory(),
            cached: perf.as_ref().map_or(0, |p| p.SystemCache as u64 * page),
            modified: 0,
            shared: 0,
            buffers: 0,
            committed: perf.as_ref().map_or(0, |p| p.CommitTotal as u64 * page),
            commit_limit: perf.as_ref().map_or(0, |p| p.CommitLimit as u64 * page),
            kernel_paged: perf.as_ref().map_or(0, |p| p.KernelPaged as u64 * page),
            kernel_nonpaged: perf.as_ref().map_or(0, |p| p.KernelNonpaged as u64 * page),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            compressed: None,
        };

        let procs = if with_processes {
            self.sample_processes(elapsed)
        } else {
            Vec::new()
        };
        let disks = self.sample_disks(elapsed);
        let nets = self.sample_nets(elapsed);
        let gpus = self.sample_gpus();

        Snapshot {
            seq: self.seq,
            cpu,
            mem,
            disks,
            nets,
            gpus,
            sys: SystemSample {
                uptime_secs: System::uptime(),
                processes: perf
                    .as_ref()
                    .map_or(procs.len(), |p| p.ProcessCount as usize),
                threads: perf.as_ref().map_or(0, |p| p.ThreadCount as usize),
                handles: perf.as_ref().map(|p| p.HandleCount as u64),
            },
            procs,
            info: self.info.clone(),
        }
    }

    fn user_of(&self, sid: Option<String>) -> (Arc<str>, u32) {
        match sid {
            Some(sid) => {
                let name = well_known_account(&sid)
                    .map(str::to_owned)
                    .or_else(|| {
                        self.users
                            .iter()
                            .find(|u| u.id().to_string() == sid)
                            .map(|u| u.name().to_owned())
                    })
                    .unwrap_or_else(|| sid.clone());
                (Arc::from(name), sid_hash(&sid))
            }
            None => (Arc::from("SYSTEM"), sid_hash("S-1-5-18")),
        }
    }

    fn sample_processes(&mut self, elapsed: f64) -> Vec<Process> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .with_exe(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet),
        );
        let threads = thread_counts();
        let windows = window_pids();
        let ncpu = self.info.logical.max(1) as f32;
        let gpu_procs: HashMap<u32, (Option<f32>, Option<u64>)> = self
            .nvidia
            .as_mut()
            .map(|nv| {
                let mut m: HashMap<u32, (Option<f32>, Option<u64>)> = HashMap::new();
                for g in nv.sample(true) {
                    for (pid, util, mem) in g.procs {
                        let e = m.entry(pid).or_insert((None, None));
                        e.0 = util.or(e.0);
                        e.1 = mem.or(e.1);
                    }
                }
                m
            })
            .unwrap_or_default();

        let mut out = Vec::with_capacity(self.sys.processes().len());
        let mut seen = HashSet::new();
        #[allow(clippy::type_complexity)]
        let entries: Vec<(
            u32,
            u32,
            u64,
            String,
            String,
            String,
            Option<String>,
            f32,
            u64,
            u64,
            u64,
            u64,
            u64,
            ProcState,
        )> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let du = p.disk_usage();
                (
                    pid.as_u32(),
                    p.parent().map(|pp| pp.as_u32()).unwrap_or(0),
                    p.start_time(),
                    p.name().to_string_lossy().into_owned(),
                    p.exe()
                        .map(|e| e.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    p.cmd()
                        .iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                    p.user_id().map(|u| u.to_string()),
                    p.cpu_usage(),
                    p.accumulated_cpu_time(),
                    p.memory(),
                    p.virtual_memory(),
                    du.read_bytes,
                    du.written_bytes,
                    map_status(p.status()),
                )
            })
            .collect();
        for (
            pid,
            ppid,
            start,
            name,
            exe,
            cmd,
            sid,
            cpu,
            cpu_ms,
            mem,
            virt,
            read,
            written,
            status,
        ) in entries
        {
            if pid == 0 {
                continue; // System Idle Process
            }
            seen.insert(pid);
            let fresh = !matches!(self.cache.get(&pid), Some(c) if c.start == start);
            if fresh {
                let app_name = if exe.is_empty() {
                    None
                } else {
                    self.description(&exe)
                };
                self.cache.insert(
                    pid,
                    Cached {
                        start,
                        name: Arc::from(name.as_str()),
                        exe: Arc::from(exe.as_str()),
                        cmdline: Arc::from(cmd.as_str()),
                        app_name,
                        last_cpu_ms: cpu_ms,
                    },
                );
            }
            let (user, uid) = self.user_of(sid.clone());
            let c = self.cache.get_mut(&pid).expect("inserted above");
            let delta_ms = cpu_ms.saturating_sub(c.last_cpu_ms);
            c.last_cpu_ms = cpu_ms;
            let is_current = sid.is_some() && sid == self.current_sid;
            let (prio_nice, efficiency) = priority_and_efficiency(pid);
            let has_window = windows.contains(&pid);
            let kind = if !is_current {
                ProcKind::System
            } else if has_window {
                ProcKind::App
            } else {
                ProcKind::Background
            };
            let app_name = c
                .app_name
                .clone()
                .unwrap_or_else(|| Arc::from(c.name.trim_end_matches(".exe")));
            let (gpu, gpu_mem) = gpu_procs.get(&pid).copied().unwrap_or((None, None));
            out.push(Process {
                pid,
                ppid,
                name: c.name.clone(),
                exe: c.exe.clone(),
                cmdline: c.cmdline.clone(),
                user,
                uid,
                state: status,
                kind,
                app_id: has_window.then(|| Arc::from(format!("exe:{}", c.exe.to_lowercase()))),
                app_name: has_window.then_some(app_name),
                unit: self.service_pids.get(&pid).cloned(),
                cpu: (cpu / ncpu).clamp(0.0, 100.0),
                cpu_time: cpu_ms as f64 / 1000.0,
                cpu_delta: delta_ms as f64 / 1000.0,
                mem,
                rss: mem,
                virt,
                threads: threads.get(&pid).copied().unwrap_or(0),
                nice: prio_nice,
                start_time: start,
                disk_read: Some(read as f64 / elapsed),
                disk_write: Some(written as f64 / elapsed),
                disk_read_delta: read,
                disk_write_delta: written,
                gpu,
                gpu_mem,
                gpu_delta: gpu.map_or(0.0, |g| g as f64 / 100.0 * elapsed),
                efficiency,
                kernel_thread: false,
            });
        }
        self.cache.retain(|pid, _| seen.contains(pid));
        crate::platform::common::inherit_apps(&mut out);
        out
    }

    /// File description from the executable's version resource ("Microsoft Edge").
    fn description(&mut self, exe: &str) -> Option<Arc<str>> {
        if let Some(d) = self.descriptions.get(exe) {
            return d.clone();
        }
        let d = actions::file_description(exe).map(Arc::from);
        if self.descriptions.len() > 4096 {
            self.descriptions.clear();
        }
        self.descriptions.insert(exe.to_owned(), d.clone());
        d
    }

    fn sample_disks(&mut self, elapsed: f64) -> Vec<DiskSample> {
        self.disks
            .refresh_specifics(true, DiskRefreshKind::everything());
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        self.disks
            .list()
            .iter()
            .map(|d| {
                let usage = d.usage();
                let mount = d.mount_point().to_string_lossy().into_owned();
                DiskSample {
                    id: mount.clone(),
                    model: d.name().to_string_lossy().into_owned(),
                    kind: match d.kind() {
                        DiskKind::SSD => "SSD".into(),
                        DiskKind::HDD => "HDD".into(),
                        _ => if d.is_removable() {
                            "Removable"
                        } else {
                            "Disk"
                        }
                        .into(),
                    },
                    capacity: d.total_space(),
                    active: None,
                    read_bps: usage.read_bytes as f64 / elapsed,
                    write_bps: usage.written_bytes as f64 / elapsed,
                    avg_response_ms: None,
                    system_disk: mount
                        .to_uppercase()
                        .starts_with(&system_drive.to_uppercase()),
                    mounts: vec![format!(
                        "{mount} ({} free)",
                        crate::util::fmt_bytes(d.available_space())
                    )],
                    removable: d.is_removable(),
                }
            })
            .collect()
    }

    fn sample_nets(&mut self, elapsed: f64) -> Vec<NetSample> {
        self.nets.refresh(true);
        let mut out: Vec<NetSample> = self
            .nets
            .iter()
            .map(|(name, n)| {
                let lower = name.to_lowercase();
                let is_virtual = [
                    "loopback",
                    "vethernet",
                    "virtual",
                    "vmware",
                    "virtualbox",
                    "hyper-v",
                    "tap",
                    "wsl",
                    "pseudo",
                ]
                .iter()
                .any(|k| lower.contains(k));
                let kind = if lower.contains("wi-fi")
                    || lower.contains("wireless")
                    || lower.contains("wlan")
                {
                    "Wi-Fi"
                } else if lower.contains("bluetooth") {
                    "Bluetooth"
                } else if is_virtual {
                    "Virtual adapter"
                } else {
                    "Ethernet"
                };
                let (mut v4, mut v6) = (Vec::new(), Vec::new());
                for ip in n.ip_networks() {
                    match ip.addr {
                        std::net::IpAddr::V4(a) => v4.push(a.to_string()),
                        std::net::IpAddr::V6(a) => v6.push(a.to_string()),
                    }
                }
                let connected = matches!(
                    n.operational_state(),
                    sysinfo::InterfaceOperationalState::Up
                ) || (!v4.is_empty()
                    && !matches!(
                        n.operational_state(),
                        sysinfo::InterfaceOperationalState::Down
                    ));
                NetSample {
                    id: name.clone(),
                    kind: kind.into(),
                    rx_bps: n.received() as f64 / elapsed,
                    tx_bps: n.transmitted() as f64 / elapsed,
                    rx_total: n.total_received(),
                    tx_total: n.total_transmitted(),
                    link_mbps: None,
                    ipv4: v4,
                    ipv6: v6,
                    mac: n.mac_address().to_string(),
                    connected,
                    is_virtual,
                }
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn sample_gpus(&mut self) -> Vec<GpuSample> {
        let Some(nv) = self.nvidia.as_mut() else {
            return Vec::new();
        };
        nv.sample(false)
            .into_iter()
            .enumerate()
            .map(|(i, g)| GpuSample {
                id: format!("nvidia{i}"),
                name: g.name,
                vendor: "NVIDIA".into(),
                driver: g.driver,
                util: g.util,
                mem_used: g.mem_used,
                mem_total: g.mem_total,
                shared_mem_used: None,
                temperature: g.temperature,
                pci_slot: g.pci,
            })
            .collect()
    }
}

fn map_status(s: sysinfo::ProcessStatus) -> ProcState {
    use sysinfo::ProcessStatus as S;
    match s {
        S::Run => ProcState::Running,
        S::Sleep => ProcState::Sleeping,
        S::Idle => ProcState::Idle,
        S::Stop => ProcState::Stopped,
        S::Zombie => ProcState::Zombie,
        S::Dead => ProcState::Dead,
        S::UninterruptibleDiskSleep => ProcState::DiskSleep,
        _ => ProcState::Unknown,
    }
}

fn performance_info() -> Option<PERFORMANCE_INFORMATION> {
    // SAFETY: the struct is plain data and its size is passed to the API.
    unsafe {
        let mut p: PERFORMANCE_INFORMATION = std::mem::zeroed();
        let size = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
        p.cb = size;
        (GetPerformanceInfo(&mut p, size) != 0).then_some(p)
    }
}

/// Number of threads per process from a ToolHelp snapshot (one system call for all processes).
fn thread_counts() -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    // SAFETY: standard ToolHelp iteration; the snapshot handle is closed at the end.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == -1 {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                out.insert(e.th32ProcessID, e.cntThreads);
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Processes that own a visible, top-level, titled window (Task Manager's "Apps").
fn window_pids() -> HashSet<u32> {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: lparam is the &mut HashSet passed below, valid for the duration of EnumWindows.
        unsafe {
            let set = &mut *(lparam as *mut HashSet<u32>);
            if IsWindowVisible(hwnd) != 0
                && GetWindow(hwnd, GW_OWNER).is_null()
                && GetWindowTextLengthW(hwnd) > 0
            {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                if pid != 0 {
                    set.insert(pid);
                }
            }
        }
        1
    }
    let mut set: HashSet<u32> = HashSet::new();
    // SAFETY: the callback only touches `set` through the pointer we pass.
    unsafe {
        EnumWindows(Some(callback), &mut set as *mut HashSet<u32> as LPARAM);
    }
    set
}

/// Maps the priority class to the nice scale used by the model and reports EcoQoS throttling.
fn priority_and_efficiency(pid: u32) -> (i32, bool) {
    // SAFETY: handle is checked and closed; out-structs are plain data.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return (0, false);
        }
        let class = GetPriorityClass(h);
        let mut state: PROCESS_POWER_THROTTLING_STATE = std::mem::zeroed();
        state.Version = PROCESS_POWER_THROTTLING_CURRENT_VERSION;
        let ok = GetProcessInformation(
            h,
            ProcessPowerThrottling,
            &mut state as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) != 0;
        CloseHandle(h);
        let eco = ok
            && state.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
            && state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0;
        (actions::class_to_nice(class), eco)
    }
}

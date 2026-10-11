//! Windows backend. The hot path uses the same native interfaces as Task Manager:
//! one `NtQuerySystemInformation` call for all processes, processor times and disk
//! performance counters. Everything that cannot change during a process' lifetime
//! (path, command line, user, description) is read once and cached.

pub mod actions;
mod disks;
pub mod icons;
mod net;
mod nt;
pub mod services;
pub mod startup;

use crate::model::{
    CpuSample, GpuSample, MemSample, ProcKind, ProcState, Process, Snapshot, StaticInfo,
    SystemSample,
};
use crate::platform::nvidia::{NvGpu, Nvidia};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use std::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::SystemInformation::{
    CACHE_RELATIONSHIP, GetLogicalProcessorInformationEx, GetPhysicallyInstalledSystemMemory,
    GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, RelationAll, RelationCache,
    RelationProcessorCore, RelationProcessorPackage, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessInformation, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, ProcessPowerThrottling,
    QueryFullProcessImageNameW,
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

/// A process handle that is closed on drop.
struct ProcHandle(HANDLE);

impl ProcHandle {
    fn open(pid: u32) -> Option<ProcHandle> {
        // SAFETY: OpenProcess returns null on failure.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!h.is_null()).then_some(ProcHandle(h))
    }

    fn exe(&self) -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buffer and length describe the same allocation.
        let ok = unsafe {
            QueryFullProcessImageNameW(self.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }

    fn cmdline(&self) -> Option<String> {
        let mut buf: Vec<u64> = vec![0; 512];
        for _ in 0..3 {
            let mut ret = 0u32;
            // SAFETY: the buffer is 8-byte aligned and `len` bytes long.
            let status = unsafe {
                nt::NtQueryInformationProcess(
                    self.0,
                    nt::PROCESS_COMMAND_LINE_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                    &mut ret,
                )
            };
            if status == nt::STATUS_INFO_LENGTH_MISMATCH || status == nt::STATUS_BUFFER_TOO_SMALL {
                buf = vec![0; (ret as usize).div_ceil(8) + 1];
                continue;
            }
            if status < 0 {
                return None;
            }
            // SAFETY: the kernel wrote a UNICODE_STRING whose buffer points into `buf`.
            return unsafe {
                let us = &*(buf.as_ptr() as *const nt::UnicodeString);
                if us.buffer.is_null() || us.length == 0 {
                    return Some(String::new());
                }
                let s = std::slice::from_raw_parts(us.buffer, us.length as usize / 2);
                Some(String::from_utf16_lossy(s))
            };
        }
        None
    }

    /// SID string of the process owner.
    fn sid(&self) -> Option<(String, Vec<u8>)> {
        // SAFETY: token handle is closed below; buffers are sized by the first call.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(self.0, TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut buf = [0u64; 64];
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast(),
                size_of_val(&buf) as u32,
                &mut len,
            );
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                return None;
            }
            let n = (0..).take_while(|&i| *s.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            let raw = std::slice::from_raw_parts(
                user.User.Sid as *const u8,
                windows_sys::Win32::Security::GetLengthSid(user.User.Sid) as usize,
            )
            .to_vec();
            Some((text, raw))
        }
    }

    /// Whether EcoQoS ("efficiency mode") throttling is on.
    fn eco_qos(&self) -> bool {
        // SAFETY: out-struct is plain data of the given size.
        unsafe {
            let mut state: PROCESS_POWER_THROTTLING_STATE = std::mem::zeroed();
            state.Version = PROCESS_POWER_THROTTLING_CURRENT_VERSION;
            let ok = GetProcessInformation(
                self.0,
                ProcessPowerThrottling,
                &mut state as *mut _ as *mut core::ffi::c_void,
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            ) != 0;
            ok && state.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                && state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
        }
    }
}

impl Drop for ProcHandle {
    fn drop(&mut self) {
        // SAFETY: opened by OpenProcess, closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Account name of a binary SID.
fn account_name(sid: &[u8]) -> Option<String> {
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let mut name_len = name.len() as u32;
    let mut domain_len = domain.len() as u32;
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: buffers and lengths match; the SID bytes are a valid SID.
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid.as_ptr() as *mut core::ffi::c_void,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    (ok != 0).then(|| String::from_utf16_lossy(&name[..name_len as usize]))
}

/// Everything about a process that is fixed for its lifetime.
struct Cached {
    create_time: i64,
    name: Arc<str>,
    exe: Arc<str>,
    cmdline: Arc<str>,
    desc: Option<Arc<str>>,
    user: Arc<str>,
    uid: u32,
    current_user: bool,
    handle: Option<ProcHandle>,
    last_cpu: u64,
    last_read: u64,
    last_write: u64,
    base_priority: i32,
    efficiency: bool,
}

pub struct Collector {
    info: Arc<StaticInfo>,
    current_sid: Option<String>,
    proc_buf: nt::ProcessBuffer,
    cache: HashMap<u32, Cached>,
    descriptions: HashMap<Arc<str>, Option<Arc<str>>>,
    accounts: HashMap<String, Arc<str>>,
    service_pids: HashMap<u32, Arc<str>>,
    cpu_times: Vec<[u64; 3]>,
    cpu_prev: Vec<[u64; 3]>,
    nets: net::Networks,
    scans: u64,
    disks: disks::Disks,
    nvidia: Option<Nvidia>,
    components: Option<sysinfo::Components>,
    cpu_temp: Option<f32>,
    seq: u64,
    procs: Arc<Vec<Process>>,
    procs_seq: u64,
    last_proc_scan: Option<Instant>,
}

impl Collector {
    pub fn new() -> Collector {
        // SAFETY: trivial getter.
        let me = unsafe { GetCurrentProcessId() };
        let mut buf = nt::ProcessBuffer::default();
        buf.refresh();
        let own = ProcHandle::open(me).and_then(|h| h.sid());
        let current_sid = own.as_ref().map(|(s, _)| s.clone());
        let current_user = own
            .as_ref()
            .and_then(|(_, raw)| account_name(raw))
            .or_else(|| std::env::var("USERNAME").ok())
            .unwrap_or_default();
        let mut cpu_times = Vec::new();
        nt::processor_times(&mut cpu_times);
        let topo = topology();
        let freq = processor_mhz();
        let info = StaticInfo {
            hostname: sysinfo::System::host_name().unwrap_or_default(),
            os_name: sysinfo::System::long_os_version().unwrap_or_else(|| "Windows".into()),
            kernel: sysinfo::System::kernel_version().unwrap_or_default(),
            desktop: "Windows".into(),
            session_type: String::new(),
            cpu_model: cpu_model(),
            cpu_base_mhz: freq.first().map(|f| f.0 as f64).filter(|f| *f > 0.0),
            cpu_base_is_max: false,
            sockets: topo.sockets.max(1),
            cores: topo.cores.max(1),
            logical: cpu_times.len().max(1),
            virtualization: None,
            l1_cache: topo.l1,
            l2_cache: topo.l2,
            l3_cache: topo.l3,
            hardware_reserved: hardware_reserved(),
            current_uid: current_sid.as_deref().map(sid_hash).unwrap_or(0),
            current_user,
        };
        // WMI thermal zones are slow and usually need administrator rights;
        // probe once and keep polling only if they actually report something.
        let mut components = sysinfo::Components::new();
        components.refresh(true);
        let has_temps = components.list().iter().any(|c| c.temperature().is_some());
        Collector {
            info: Arc::new(info),
            current_sid,
            proc_buf: buf,
            cache: HashMap::new(),
            descriptions: HashMap::new(),
            accounts: HashMap::new(),
            service_pids: HashMap::new(),
            cpu_prev: cpu_times.clone(),
            cpu_times,
            nets: net::Networks::default(),
            scans: 0,
            disks: disks::Disks::default(),
            nvidia: Nvidia::init(),
            components: has_temps.then_some(components),
            cpu_temp: None,
            seq: 0,
            procs: Arc::new(Vec::new()),
            procs_seq: 0,
            last_proc_scan: None,
        }
    }

    pub fn sample(&mut self, with_processes: bool) -> Snapshot {
        let now = Instant::now();
        self.seq += 1;
        if self.seq % 10 == 1 {
            self.service_pids = services::pid_map();
        }

        let cpu = self.sample_cpu();
        let perf = performance_info();
        let mem = memory(perf.as_ref());

        let scan = with_processes || self.procs_seq == 0;
        let nv: Vec<NvGpu> = self
            .nvidia
            .as_mut()
            .map(|nv| nv.sample(scan))
            .unwrap_or_default();
        if scan {
            let proc_elapsed = self
                .last_proc_scan
                .map(|l| now.duration_since(l).as_secs_f64())
                .unwrap_or(1.0)
                .max(0.05);
            self.last_proc_scan = Some(now);
            self.procs = Arc::new(self.sample_processes(proc_elapsed, &nv));
            self.procs_seq += 1;
        }
        let disks = self.disks.sample();
        let nets = self.nets.sample();
        let gpus = gpus(nv);

        if let Some(c) = self.components.as_mut()
            && self.seq % 5 == 1
        {
            c.refresh(true);
            self.cpu_temp = c
                .list()
                .iter()
                .filter_map(|c| c.temperature())
                .filter(|t| (1.0..150.0).contains(t))
                .reduce(f32::max);
        }
        let sensors = self
            .components
            .iter()
            .flat_map(|c| c.list())
            .filter_map(|c| {
                Some(crate::model::SensorReading {
                    chip: "ACPI thermal zone".into(),
                    label: c.label().to_owned(),
                    kind: crate::model::SensorKind::Temperature,
                    value: c.temperature()?,
                    high: c.max(),
                    crit: c.critical(),
                })
            })
            .collect();

        Snapshot {
            cpu: CpuSample {
                temperature: self.cpu_temp,
                ..cpu
            },
            mem,
            disks,
            nets,
            gpus,
            sensors,
            batteries: battery().into_iter().collect(),
            sys: SystemSample {
                // SAFETY: trivial getter.
                uptime_secs: unsafe { GetTickCount64() } / 1000,
                processes: perf
                    .as_ref()
                    .map_or(self.procs.len(), |p| p.ProcessCount as usize),
                threads: perf.as_ref().map_or(0, |p| p.ThreadCount as usize),
                handles: perf.as_ref().map(|p| p.HandleCount as u64),
            },
            procs: self.procs.clone(),
            procs_seq: self.procs_seq,
            info: self.info.clone(),
        }
    }

    fn sample_cpu(&mut self) -> CpuSample {
        std::mem::swap(&mut self.cpu_prev, &mut self.cpu_times);
        if !nt::processor_times(&mut self.cpu_times) || self.cpu_times.len() != self.cpu_prev.len()
        {
            self.cpu_prev = self.cpu_times.clone();
            return CpuSample::default();
        }
        let (mut busy_sum, mut kernel_sum, mut total_sum) = (0u64, 0u64, 0u64);
        let per_core: Vec<f32> = self
            .cpu_times
            .iter()
            .zip(&self.cpu_prev)
            .map(|(now, prev)| {
                let idle = now[0].saturating_sub(prev[0]);
                let kernel = now[1].saturating_sub(prev[1]);
                let user = now[2].saturating_sub(prev[2]);
                let total = kernel + user;
                busy_sum += total.saturating_sub(idle);
                kernel_sum += kernel.saturating_sub(idle);
                total_sum += total;
                if total == 0 {
                    0.0
                } else {
                    (total.saturating_sub(idle) as f64 / total as f64 * 100.0) as f32
                }
            })
            .collect();
        let pct = |v: u64| {
            if total_sum == 0 {
                0.0
            } else {
                (v as f64 / total_sum as f64 * 100.0).clamp(0.0, 100.0) as f32
            }
        };
        let freqs = processor_mhz();
        let per_core_freq: Vec<f32> = freqs.iter().map(|f| f.1 as f32).collect();
        let freq = (!per_core_freq.is_empty())
            .then(|| {
                per_core_freq.iter().map(|f| *f as f64).sum::<f64>() / per_core_freq.len() as f64
            })
            .filter(|f| *f > 0.0);
        CpuSample {
            total: pct(busy_sum),
            kernel: pct(kernel_sum),
            per_core,
            per_core_freq,
            freq_mhz: freq,
            ..Default::default()
        }
    }

    fn account(&mut self, sid: &str, raw: &[u8]) -> Arc<str> {
        if let Some(name) = self.accounts.get(sid) {
            return name.clone();
        }
        let name: Arc<str> = Arc::from(
            well_known_account(sid)
                .map(str::to_owned)
                .or_else(|| account_name(raw))
                .unwrap_or_else(|| sid.to_owned()),
        );
        self.accounts.insert(sid.to_owned(), name.clone());
        name
    }

    fn description(&mut self, exe: &Arc<str>) -> Option<Arc<str>> {
        if let Some(d) = self.descriptions.get(exe) {
            return d.clone();
        }
        let d = actions::file_description(exe).map(Arc::from);
        if self.descriptions.len() > 4096 {
            self.descriptions.clear();
        }
        self.descriptions.insert(exe.clone(), d.clone());
        d
    }

    /// Reads the fixed properties of a newly seen process.
    fn inspect(&mut self, pid: u32, create_time: i64, image: &[u16]) -> Cached {
        let handle = ProcHandle::open(pid);
        let name: Arc<str> = Arc::from(match pid {
            4 => "System".to_owned(),
            _ if image.is_empty() => format!("PID {pid}"),
            _ => String::from_utf16_lossy(image),
        });
        let exe: Arc<str> = Arc::from(handle.as_ref().and_then(|h| h.exe()).unwrap_or_default());
        let cmdline: Arc<str> = Arc::from(
            handle
                .as_ref()
                .and_then(|h| h.cmdline())
                .unwrap_or_default(),
        );
        let sid = handle.as_ref().and_then(|h| h.sid());
        let (user, uid, current_user) = match &sid {
            Some((s, raw)) => (
                self.account(s, raw),
                sid_hash(s),
                self.current_sid.as_deref() == Some(s.as_str()),
            ),
            // Processes we cannot inspect belong to the system (services, DWM, ...).
            None => (Arc::from("SYSTEM"), sid_hash("S-1-5-18"), false),
        };
        let desc = if exe.is_empty() {
            None
        } else {
            self.description(&exe)
        };
        Cached {
            create_time,
            name,
            exe,
            cmdline,
            desc,
            user,
            uid,
            current_user,
            handle,
            last_cpu: 0,
            last_read: 0,
            last_write: 0,
            base_priority: i32::MIN,
            efficiency: false,
        }
    }

    fn sample_processes(&mut self, elapsed: f64, nv: &[NvGpu]) -> Vec<Process> {
        if !self.proc_buf.refresh() {
            return self.procs.as_ref().clone();
        }
        let windows = window_pids();
        let first_scan = self.procs_seq == 0;
        // 100 ns units available across all processors during the interval.
        let capacity = elapsed * 1e7 * self.info.logical.max(1) as f64;
        let mut gpu_procs: HashMap<u32, (Option<f32>, Option<u64>)> = HashMap::new();
        for g in nv {
            for &(pid, util, mem) in &g.procs {
                let e = gpu_procs.entry(pid).or_insert((None, None));
                e.0 = util.or(e.0);
                e.1 = mem.or(e.1);
            }
        }

        self.scans += 1;
        let buf = std::mem::take(&mut self.proc_buf);
        let mut out = Vec::with_capacity(self.cache.len() + 16);
        let mut seen = HashSet::with_capacity(self.cache.len() + 16);
        for e in buf.iter() {
            if e.pid == 0 {
                continue; // System Idle Process
            }
            seen.insert(e.pid);
            let fresh = self
                .cache
                .get(&e.pid)
                .is_none_or(|c| c.create_time != e.create_time);
            if fresh {
                let mut c = self.inspect(e.pid, e.create_time, e.image_name);
                // Processes that started during this interval have no previous sample;
                // only on the very first scan is everything new, so start from zero there.
                if !first_scan {
                    c.last_cpu = e.cpu_time;
                    c.last_read = e.read_bytes;
                    c.last_write = e.write_bytes;
                }
                self.cache.insert(e.pid, c);
            }
            let c = self.cache.get_mut(&e.pid).expect("inserted above");
            let (cpu_delta, read_delta, write_delta) = if first_scan {
                (0, 0, 0)
            } else {
                (
                    e.cpu_time.saturating_sub(c.last_cpu),
                    e.read_bytes.saturating_sub(c.last_read),
                    e.write_bytes.saturating_sub(c.last_write),
                )
            };
            c.last_cpu = e.cpu_time;
            c.last_read = e.read_bytes;
            c.last_write = e.write_bytes;

            let has_window = windows.contains(&e.pid);
            let kind = if !c.current_user {
                ProcKind::System
            } else if has_window {
                ProcKind::App
            } else {
                ProcKind::Background
            };
            let (gpu, gpu_mem) = gpu_procs.get(&e.pid).copied().unwrap_or((None, None));
            // Efficiency mode also changes the priority class, so a priority change (or every
            // 5th scan, for changes made elsewhere) triggers a fresh EcoQoS query.
            if c.base_priority != e.base_priority || self.scans.is_multiple_of(5) {
                c.base_priority = e.base_priority;
                c.efficiency = c.handle.as_ref().is_some_and(ProcHandle::eco_qos);
            }
            let efficiency = c.efficiency;
            let display = c
                .desc
                .clone()
                .unwrap_or_else(|| Arc::from(c.name.trim_end_matches(".exe")));
            out.push(Process {
                pid: e.pid,
                ppid: e.ppid,
                name: c.name.clone(),
                exe: c.exe.clone(),
                cmdline: c.cmdline.clone(),
                user: c.user.clone(),
                uid: c.uid,
                state: if e.suspended {
                    ProcState::Stopped
                } else {
                    ProcState::Running
                },
                kind,
                app_id: has_window.then(|| Arc::from(format!("exe:{}", c.exe.to_lowercase()))),
                // Task Manager names processes by their file description.
                app_name: (c.desc.is_some() || has_window).then_some(display),
                unit: self.service_pids.get(&e.pid).cloned(),
                cpu: (cpu_delta as f64 / capacity * 100.0).clamp(0.0, 100.0) as f32,
                cpu_time: e.cpu_time as f64 / 1e7,
                cpu_delta: cpu_delta as f64 / 1e7,
                mem: e.private_ws,
                rss: e.working_set,
                virt: e.virtual_size,
                threads: e.threads,
                nice: nt::base_priority_to_nice(e.base_priority),
                start_time: nt::filetime_to_unix(e.create_time),
                disk_read: Some(read_delta as f64 / elapsed),
                disk_write: Some(write_delta as f64 / elapsed),
                disk_read_delta: read_delta,
                disk_write_delta: write_delta,
                gpu,
                gpu_mem,
                gpu_delta: gpu.map_or(0.0, |g| g as f64 / 100.0 * elapsed),
                efficiency,
                kernel_thread: false,
            });
        }
        self.proc_buf = buf;
        // Dropping the cache entries closes the handles of exited processes.
        self.cache.retain(|pid, _| seen.contains(pid));
        crate::platform::common::inherit_apps(&mut out);
        out
    }
}

fn gpus(nv: Vec<NvGpu>) -> Vec<GpuSample> {
    nv.into_iter()
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
            power_w: g.power_w,
            fan_rpm: None,
            fan_percent: g.fan_percent,
            clock_mhz: g.clock_mhz,
        })
        .collect()
}

fn memory(perf: Option<&PERFORMANCE_INFORMATION>) -> MemSample {
    // SAFETY: plain data struct with its size set.
    let st = unsafe {
        let mut st: MEMORYSTATUSEX = std::mem::zeroed();
        st.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        GlobalMemoryStatusEx(&mut st);
        st
    };
    let page = perf.map_or(4096, |p| p.PageSize as u64);
    let pages = |f: fn(&PERFORMANCE_INFORMATION) -> usize| perf.map_or(0, |p| f(p) as u64 * page);
    let total = st.ullTotalPhys;
    let used = total.saturating_sub(st.ullAvailPhys);
    let committed = pages(|p| p.CommitTotal);
    let commit_limit = pages(|p| p.CommitLimit);
    // The page file is the part of the commit limit beyond physical memory.
    let swap_total = commit_limit.saturating_sub(total);
    MemSample {
        total,
        available: st.ullAvailPhys,
        used,
        free: st.ullAvailPhys,
        cached: pages(|p| p.SystemCache),
        modified: 0,
        shared: 0,
        buffers: 0,
        committed,
        commit_limit,
        kernel_paged: pages(|p| p.KernelPaged),
        kernel_nonpaged: pages(|p| p.KernelNonpaged),
        swap_total,
        swap_used: committed.saturating_sub(used).min(swap_total),
        compressed: None,
        temperature: None,
    }
}

/// (max/base MHz, current MHz) per logical processor.
fn processor_mhz() -> Vec<(u32, u32)> {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Info {
        number: u32,
        max_mhz: u32,
        current_mhz: u32,
        mhz_limit: u32,
        max_idle_state: u32,
        current_idle_state: u32,
    }
    let mut buf = [Info::default(); 256];
    // SAFETY: the output buffer is plain data of the given size.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            std::ptr::null(),
            0,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
        )
    };
    if status < 0 {
        return Vec::new();
    }
    buf.iter()
        .take_while(|i| i.max_mhz > 0)
        .map(|i| (i.max_mhz, i.current_mhz))
        .collect()
}

fn cpu_model() -> String {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0")
        .and_then(|k| k.get_value::<String, _>("ProcessorNameString"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[derive(Default)]
struct Topology {
    sockets: usize,
    cores: usize,
    l1: Option<u64>,
    l2: Option<u64>,
    l3: Option<u64>,
}

fn topology() -> Topology {
    let mut t = Topology::default();
    let mut len = 0u32;
    // SAFETY: first call only reports the size; the second fills a buffer of that size.
    unsafe {
        GetLogicalProcessorInformationEx(RelationAll, std::ptr::null_mut(), &mut len);
        if len == 0 {
            return t;
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetLogicalProcessorInformationEx(RelationAll, buf.as_mut_ptr().cast(), &mut len) == 0 {
            return t;
        }
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        let (mut l1, mut l2, mut l3) = (0u64, 0u64, 0u64);
        while off + 8 <= len as usize {
            let item = &*(base.add(off) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX);
            #[allow(non_upper_case_globals)]
            match item.Relationship {
                RelationProcessorCore => t.cores += 1,
                RelationProcessorPackage => t.sockets += 1,
                RelationCache => {
                    let c: &CACHE_RELATIONSHIP = &item.Anonymous.Cache;
                    let size = c.CacheSize as u64;
                    match c.Level {
                        1 => l1 += size,
                        2 => l2 += size,
                        3 => l3 += size,
                        _ => {}
                    }
                }
                _ => {}
            }
            if item.Size == 0 {
                break;
            }
            off += item.Size as usize;
        }
        t.l1 = (l1 > 0).then_some(l1);
        t.l2 = (l2 > 0).then_some(l2);
        t.l3 = (l3 > 0).then_some(l3);
    }
    t
}

/// Installed RAM that the firmware or devices keep for themselves.
fn hardware_reserved() -> Option<u64> {
    let mut kb = 0u64;
    // SAFETY: out-pointers are valid.
    unsafe {
        if GetPhysicallyInstalledSystemMemory(&mut kb) == 0 {
            return None;
        }
        let mut st: MEMORYSTATUSEX = std::mem::zeroed();
        st.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        GlobalMemoryStatusEx(&mut st);
        Some((kb * 1024).saturating_sub(st.ullTotalPhys))
    }
}

/// Battery state from the Windows power API (laptops and UPS-backed desktops).
fn battery() -> Option<crate::model::BatterySample> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: plain data struct filled by the API.
    let st = unsafe {
        let mut st: SYSTEM_POWER_STATUS = std::mem::zeroed();
        if GetSystemPowerStatus(&mut st) == 0 {
            return None;
        }
        st
    };
    // 128 = no system battery, 255 = unknown status.
    if st.BatteryFlag & 128 != 0 || st.BatteryFlag == 255 || st.BatteryLifePercent > 100 {
        return None;
    }
    let charging = st.BatteryFlag & 8 != 0;
    let on_ac = match st.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    Some(crate::model::BatterySample {
        id: "Battery".into(),
        percent: st.BatteryLifePercent as f32,
        status: if charging {
            "Charging"
        } else if on_ac == Some(true) {
            if st.BatteryLifePercent >= 100 {
                "Full"
            } else {
                "Not charging"
            }
        } else {
            "Discharging"
        }
        .into(),
        on_ac,
        time_remaining_secs: (st.BatteryLifeTime != u32::MAX).then_some(st.BatteryLifeTime as u64),
        ..Default::default()
    })
}

fn performance_info() -> Option<PERFORMANCE_INFORMATION> {
    // SAFETY: the struct is plain data and its size is passed to the API.
    unsafe {
        let mut p: PERFORMANCE_INFORMATION = std::mem::zeroed();
        let size = size_of::<PERFORMANCE_INFORMATION>() as u32;
        p.cb = size;
        (GetPerformanceInfo(&mut p, size) != 0).then_some(p)
    }
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

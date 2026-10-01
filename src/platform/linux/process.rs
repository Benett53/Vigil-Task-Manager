//! Process enumeration via procfs with per-process caching.
//!
//! Strings that never change for the lifetime of a process (name, executable,
//! command line, user) are read once and shared through `Arc<str>`, so a refresh
//! only reads three small files per process (`stat`, `statm`, `io`).

use super::desktop::AppIndex;
use super::gpu::GpuMonitor;
use super::procfs::{
    CgroupInfo, PF_KTHREAD, cmdline_argv0, parse_cgroup, parse_cmdline, parse_pid_io,
    parse_pid_stat, parse_statm, read_into,
};
use super::users::UserCache;
use crate::model::{ProcKind, ProcState, Process};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

struct Cached {
    start_ticks: u64,
    name: Arc<str>,
    exe: Arc<str>,
    cmdline: Arc<str>,
    user: Arc<str>,
    uid: u32,
    kernel_thread: bool,
    cgroup: CgroupInfo,
    cgroup_read_at: Instant,
    app: Option<(Arc<str>, Arc<str>)>,
    app_checked: bool,
    unit: Option<Arc<str>>,
    last_cpu_ticks: u64,
    last_io: Option<(u64, u64)>,
    drm_fds: Vec<u32>,
    fds_scanned_at: Option<Instant>,
    seen: u64,
}

pub struct ProcessScanner {
    cache: HashMap<u32, Cached>,
    seq: u64,
    clk_tck: f64,
    page_size: u64,
    boot_time: u64,
    current_uid: u32,
    buf: String,
    path: String,
    fdinfo: Vec<String>,
    pub show_gpu: bool,
}

pub struct ScanResult {
    pub procs: Vec<Process>,
    pub threads: usize,
}

impl ProcessScanner {
    pub fn new(boot_time: u64, current_uid: u32) -> ProcessScanner {
        // SAFETY: sysconf is always safe to call.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        ProcessScanner {
            cache: HashMap::new(),
            seq: 0,
            clk_tck: if clk_tck > 0 { clk_tck as f64 } else { 100.0 },
            page_size: if page_size > 0 {
                page_size as u64
            } else {
                4096
            },
            boot_time,
            current_uid,
            buf: String::with_capacity(1024),
            path: String::with_capacity(64),
            fdinfo: Vec::new(),
            show_gpu: true,
        }
    }

    pub fn scan(
        &mut self,
        elapsed: f64,
        ncpu: usize,
        apps: &AppIndex,
        users: &mut UserCache,
        gpu: &mut GpuMonitor,
    ) -> ScanResult {
        self.seq += 1;
        let seq = self.seq;
        let now = Instant::now();
        let mut procs = Vec::with_capacity(self.cache.len().max(256));
        let mut threads = 0usize;
        let cpu_denominator = (elapsed * self.clk_tck * ncpu.max(1) as f64).max(1e-9);

        let Ok(dir) = std::fs::read_dir("/proc") else {
            return ScanResult { procs, threads };
        };
        for entry in dir.flatten() {
            let fname = entry.file_name();
            let Some(pid) = fname.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };

            self.path.clear();
            let _ = write!(self.path, "/proc/{pid}/stat");
            if read_into(&self.path, &mut self.buf).is_err() {
                continue; // process exited
            }
            let Some(stat) = parse_pid_stat(&self.buf) else {
                continue;
            };
            let (state, ppid, flags, utime, stime, nice, nthreads, start_ticks, vsize, rss_pages) = (
                stat.state,
                stat.ppid,
                stat.flags,
                stat.utime,
                stat.stime,
                stat.nice,
                stat.threads,
                stat.start_ticks,
                stat.vsize,
                stat.rss_pages,
            );
            threads += nthreads as usize;

            // (Re)create the cache entry if the pid is new or was reused.
            let fresh = !matches!(self.cache.get(&pid), Some(c) if c.start_ticks == start_ticks);
            if fresh {
                let comm = stat.comm.to_owned();
                let c = self.load_static(pid, &comm, start_ticks, flags, users, now);
                self.cache.insert(pid, c);
            }
            let cache = self.cache.get_mut(&pid).expect("inserted above");
            cache.seen = seq;

            // Newly started processes are often moved into their app scope a moment later.
            if !cache.kernel_thread
                && now.duration_since(cache.cgroup_read_at).as_secs_f64() < 15.0
                && !fresh
            {
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/cgroup");
                if read_into(&self.path, &mut self.buf).is_ok() {
                    let cg = parse_cgroup(&self.buf);
                    if cg != cache.cgroup {
                        cache.cgroup = cg;
                        cache.app = None;
                        cache.app_checked = false;
                        cache.unit = cache.cgroup.unit.as_deref().map(Arc::from);
                    }
                }
            }

            let cpu_ticks = utime + stime;
            let delta_ticks = if fresh {
                0
            } else {
                cpu_ticks.saturating_sub(cache.last_cpu_ticks)
            };
            cache.last_cpu_ticks = cpu_ticks;

            // Memory
            let (mut mem, mut rss) = (0, rss_pages * self.page_size);
            if !cache.kernel_thread {
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/statm");
                if read_into(&self.path, &mut self.buf).is_ok()
                    && let Some((res, shared)) = parse_statm(&self.buf)
                {
                    rss = res * self.page_size;
                    mem = res.saturating_sub(shared) * self.page_size;
                }
            }

            // Disk I/O (only readable for own processes unless running as root)
            let mut disk = None;
            if !cache.kernel_thread && (self.current_uid == 0 || cache.uid == self.current_uid) {
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/io");
                if read_into(&self.path, &mut self.buf).is_ok()
                    && let Some((r, w)) = parse_pid_io(&self.buf)
                {
                    let (dr, dw) = match cache.last_io {
                        Some((pr, pw)) if !fresh => (r.saturating_sub(pr), w.saturating_sub(pw)),
                        _ => (0, 0),
                    };
                    cache.last_io = Some((r, w));
                    disk = Some((dr, dw));
                }
            }

            // GPU via DRM fdinfo
            let mut gpu_usage = None;
            if self.show_gpu
                && !cache.kernel_thread
                && (self.current_uid == 0 || cache.uid == self.current_uid)
            {
                let rescan = cache
                    .fds_scanned_at
                    .is_none_or(|t| now.duration_since(t).as_secs() >= 5);
                if rescan {
                    cache.drm_fds = drm_fds(pid);
                    cache.fds_scanned_at = Some(now);
                }
                if !cache.drm_fds.is_empty() {
                    let mut infos = std::mem::take(&mut self.fdinfo);
                    infos.clear();
                    for fd in &cache.drm_fds {
                        if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) {
                            infos.push(s);
                        }
                    }
                    gpu_usage = gpu.account_process(pid, infos.iter().map(String::as_str));
                    self.fdinfo = infos;
                } else {
                    gpu_usage = gpu.account_process(pid, std::iter::empty());
                }
            }

            // Application classification
            if !cache.app_checked {
                cache.app = classify_app(cache, apps);
                cache.app_checked = true;
            }
            let kind = if cache.kernel_thread || cache.uid != self.current_uid {
                ProcKind::System
            } else if cache.app.is_some() {
                ProcKind::App
            } else {
                ProcKind::Background
            };

            let st = match state {
                b'R' => ProcState::Running,
                b'S' => ProcState::Sleeping,
                b'D' => ProcState::DiskSleep,
                b'T' | b't' => ProcState::Stopped,
                b'Z' => ProcState::Zombie,
                b'I' => ProcState::Idle,
                b'X' | b'x' => ProcState::Dead,
                _ => ProcState::Unknown,
            };

            procs.push(Process {
                pid,
                ppid,
                name: cache.name.clone(),
                exe: cache.exe.clone(),
                cmdline: cache.cmdline.clone(),
                user: cache.user.clone(),
                uid: cache.uid,
                state: st,
                kind,
                app_id: cache.app.as_ref().map(|a| a.0.clone()),
                app_name: cache.app.as_ref().map(|a| a.1.clone()),
                unit: cache.unit.clone(),
                cpu: (delta_ticks as f64 * 100.0 / cpu_denominator).clamp(0.0, 100.0) as f32,
                cpu_time: cpu_ticks as f64 / self.clk_tck,
                cpu_delta: delta_ticks as f64 / self.clk_tck,
                mem,
                rss,
                virt: vsize,
                threads: nthreads,
                nice,
                start_time: self.boot_time + (start_ticks as f64 / self.clk_tck) as u64,
                disk_read: disk.map(|d| d.0 as f64 / elapsed),
                disk_write: disk.map(|d| d.1 as f64 / elapsed),
                disk_read_delta: disk.map_or(0, |d| d.0),
                disk_write_delta: disk.map_or(0, |d| d.1),
                gpu: gpu_usage.map(|g| g.util),
                gpu_mem: gpu_usage.and_then(|g| g.mem),
                gpu_delta: gpu_usage.map_or(0.0, |g| g.busy_secs),
                efficiency: nice >= 19,
                kernel_thread: cache.kernel_thread,
            });
        }

        // Child processes inherit the application of their parent (e.g. browser helpers,
        // shells inside a terminal) when they run as the same user.
        crate::platform::common::inherit_apps(&mut procs);

        self.cache.retain(|_, c| c.seen == seq);
        ScanResult { procs, threads }
    }

    fn load_static(
        &mut self,
        pid: u32,
        comm: &str,
        start_ticks: u64,
        flags: u64,
        users: &mut UserCache,
        now: Instant,
    ) -> Cached {
        let base = format!("/proc/{pid}");
        let uid = std::fs::metadata(&base)
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .unwrap_or(0);
        let kernel_thread = flags & PF_KTHREAD != 0 || pid == 2;
        let (exe, cmdline, name) = if kernel_thread {
            (String::new(), String::new(), comm.to_owned())
        } else {
            let exe = std::fs::read_link(format!("{base}/exe"))
                .map(|p| {
                    p.to_string_lossy()
                        .trim_end_matches(" (deleted)")
                        .to_owned()
                })
                .unwrap_or_default();
            let raw = std::fs::read(format!("{base}/cmdline")).unwrap_or_default();
            let name = display_name(comm, &exe, cmdline_argv0(&raw));
            (exe, parse_cmdline(&raw), name)
        };
        let cgroup = if kernel_thread {
            CgroupInfo::default()
        } else {
            std::fs::read_to_string(format!("{base}/cgroup"))
                .map(|s| parse_cgroup(&s))
                .unwrap_or_default()
        };
        Cached {
            start_ticks,
            name: Arc::from(name),
            exe: Arc::from(exe),
            cmdline: Arc::from(cmdline),
            user: users.name(uid),
            uid,
            kernel_thread,
            unit: cgroup.unit.as_deref().map(Arc::from),
            cgroup,
            cgroup_read_at: now,
            app: None,
            app_checked: false,
            last_cpu_ticks: 0,
            last_io: None,
            drm_fds: Vec::new(),
            fds_scanned_at: None,
            seen: 0,
        }
    }
}

/// `comm` is limited to 15 characters; use the executable or argv[0] when they extend it.
pub fn display_name(comm: &str, exe: &str, argv0: &[u8]) -> String {
    let exe_base = Path::new(exe)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned());
    if let Some(b) = &exe_base
        && (b == comm || (comm.len() >= 15 && b.starts_with(comm)))
    {
        return b.clone();
    }
    let argv0 = String::from_utf8_lossy(argv0);
    let arg_base = argv0
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split(' ')
        .next()
        .unwrap_or("");
    if !arg_base.is_empty()
        && (arg_base == comm || (comm.len() >= 15 && arg_base.starts_with(comm)))
    {
        return arg_base.to_owned();
    }
    comm.to_owned()
}

fn classify_app(c: &Cached, apps: &AppIndex) -> Option<(Arc<str>, Arc<str>)> {
    if c.kernel_thread {
        return None;
    }
    if let Some(id) = &c.cgroup.app_id {
        // An app scope is only shown as an app if it has a visible launcher entry.
        // Autostarted helpers (also launched into app-*.scope) remain background processes.
        return match apps.by_id(id) {
            Some(e) if !e.no_display => Some((
                Arc::from(format!("app:{}", e.id)),
                Arc::from(e.name.as_str()),
            )),
            _ => None,
        };
    }
    if c.cgroup.unit.is_some() && c.cgroup.user_session {
        // Inside a systemd user manager but not an app scope: a user service.
        return None;
    }
    // Non-systemd fallback: match by executable name.
    let exe_base = Path::new(&*c.exe)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned());
    let entry = exe_base
        .as_deref()
        .and_then(|e| apps.by_exec(e))
        .or_else(|| apps.by_exec(&c.name))?;
    Some((
        Arc::from(format!("app:{}", entry.id)),
        Arc::from(entry.name.as_str()),
    ))
}

/// File descriptors of `pid` that refer to a DRM device node.
fn drm_fds(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let dir = format!("/proc/{pid}/fd");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return out;
    };
    for ent in read.flatten() {
        let Ok(target) = std::fs::read_link(ent.path()) else {
            continue;
        };
        if target
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(b"/dev/dri/")
            && let Some(fd) = ent.file_name().to_str().and_then(|s| s.parse().ok())
        {
            out.push(fd);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn names() {
        assert_eq!(
            display_name(
                "firefox",
                "/usr/lib/firefox/firefox",
                b"/usr/lib/firefox/firefox"
            ),
            "firefox"
        );
        assert_eq!(
            display_name("gnome-terminal-", "/usr/libexec/gnome-terminal-server", b""),
            "gnome-terminal-server"
        );
        assert_eq!(
            display_name(
                "Web Content",
                "/usr/lib/firefox/firefox",
                b"/usr/lib/firefox/firefox"
            ),
            "Web Content"
        );
        assert_eq!(
            display_name("python3", "/usr/bin/python3.12", b"python3"),
            "python3"
        );
        assert_eq!(display_name("kworker/0:1", "", b""), "kworker/0:1");
    }
}

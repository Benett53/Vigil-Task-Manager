//! Platform independent data model. Collectors fill these structures, the UI only reads them.

use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcState {
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    Zombie,
    Idle,
    Dead,
    Unknown,
}

impl ProcState {
    pub fn label(self) -> &'static str {
        match self {
            ProcState::Running => "Running",
            ProcState::Sleeping => "Sleeping",
            ProcState::DiskSleep => "Waiting (I/O)",
            ProcState::Stopped => "Suspended",
            ProcState::Zombie => "Zombie",
            ProcState::Idle => "Idle",
            ProcState::Dead => "Terminated",
            ProcState::Unknown => "Unknown",
        }
    }
}

/// Task Manager style classification of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProcKind {
    /// A desktop application the user launched (grouped by app).
    App,
    /// Other processes of the current user.
    Background,
    /// Processes of other users, daemons and the kernel.
    System,
}

impl ProcKind {
    pub fn label(self) -> &'static str {
        match self {
            ProcKind::App => "Apps",
            ProcKind::Background => "Background processes",
            ProcKind::System => "System processes",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    /// Short display name.
    pub name: Arc<str>,
    /// Absolute path of the executable, empty if unknown/inaccessible.
    pub exe: Arc<str>,
    pub cmdline: Arc<str>,
    pub user: Arc<str>,
    pub uid: u32,
    pub state: ProcState,
    pub kind: ProcKind,
    /// Stable key used to group the processes of one application.
    pub app_id: Option<Arc<str>>,
    /// Human friendly application name (from a .desktop file when available).
    pub app_name: Option<Arc<str>>,
    /// Service / unit the process belongs to (systemd unit on Linux, service name on Windows).
    pub unit: Option<Arc<str>>,
    /// CPU usage in percent of the whole machine (0..=100).
    pub cpu: f32,
    /// Total CPU time consumed in seconds.
    pub cpu_time: f64,
    /// CPU seconds consumed since the previous sample.
    pub cpu_delta: f64,
    /// Private (non shared) resident memory in bytes.
    pub mem: u64,
    pub rss: u64,
    pub virt: u64,
    pub threads: u32,
    /// Nice value (-20..=19). On Windows the priority class is mapped onto this scale.
    pub nice: i32,
    /// Process start time as unix timestamp (seconds).
    pub start_time: u64,
    /// Disk throughput in bytes/second, `None` when not permitted to read.
    pub disk_read: Option<f64>,
    pub disk_write: Option<f64>,
    pub disk_read_delta: u64,
    pub disk_write_delta: u64,
    /// GPU engine utilisation in percent, if the driver exposes it.
    pub gpu: Option<f32>,
    pub gpu_mem: Option<u64>,
    /// GPU busy time consumed since the previous sample (seconds).
    pub gpu_delta: f64,
    /// Low priority ("efficiency mode") is active.
    pub efficiency: bool,
    pub kernel_thread: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CpuSample {
    /// Overall utilisation in percent.
    pub total: f32,
    /// Kernel ("system") time portion in percent.
    pub kernel: f32,
    pub per_core: Vec<f32>,
    /// Average current clock in MHz.
    pub freq_mhz: Option<f64>,
    pub temperature: Option<f32>,
    pub load_avg: Option<[f32; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct MemSample {
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub free: u64,
    /// Page cache that can be reclaimed ("Standby" on Windows).
    pub cached: u64,
    /// Dirty + writeback pages ("Modified" on Windows).
    pub modified: u64,
    pub shared: u64,
    pub buffers: u64,
    pub committed: u64,
    pub commit_limit: u64,
    pub kernel_paged: u64,
    pub kernel_nonpaged: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Memory stored compressed (zswap / zram), if available.
    pub compressed: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    pub id: String,
    pub model: String,
    pub kind: String,
    pub capacity: u64,
    /// Percentage of time the disk was busy.
    pub active: Option<f32>,
    pub read_bps: f64,
    pub write_bps: f64,
    pub avg_response_ms: Option<f64>,
    pub mounts: Vec<String>,
    pub system_disk: bool,
    pub removable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct NetSample {
    pub id: String,
    pub kind: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub link_mbps: Option<u64>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub mac: String,
    pub connected: bool,
    pub is_virtual: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub driver: String,
    pub util: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub shared_mem_used: Option<u64>,
    pub temperature: Option<f32>,
    pub pci_slot: String,
}

#[derive(Clone, Debug, Default)]
pub struct SystemSample {
    pub uptime_secs: u64,
    pub processes: usize,
    pub threads: usize,
    /// Open file handles system wide.
    pub handles: Option<u64>,
}

/// Information that does not change while running.
#[derive(Clone, Debug, Default)]
pub struct StaticInfo {
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    pub desktop: String,
    pub session_type: String,
    pub cpu_model: String,
    pub cpu_base_mhz: Option<f64>,
    /// True if `cpu_base_mhz` is the maximum (boost) clock rather than the base clock.
    pub cpu_base_is_max: bool,
    pub sockets: usize,
    pub cores: usize,
    pub logical: usize,
    pub virtualization: Option<String>,
    pub l1_cache: Option<u64>,
    pub l2_cache: Option<u64>,
    pub l3_cache: Option<u64>,
    pub hardware_reserved: Option<u64>,
    pub current_uid: u32,
    pub current_user: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub seq: u64,
    pub cpu: CpuSample,
    pub mem: MemSample,
    pub disks: Vec<DiskSample>,
    pub nets: Vec<NetSample>,
    pub gpus: Vec<GpuSample>,
    pub sys: SystemSample,
    pub procs: Vec<Process>,
    pub info: Arc<StaticInfo>,
}

impl Snapshot {
    pub fn empty(info: Arc<StaticInfo>) -> Self {
        Snapshot {
            seq: 0,
            cpu: CpuSample::default(),
            mem: MemSample::default(),
            disks: Vec::new(),
            nets: Vec::new(),
            gpus: Vec::new(),
            sys: SystemSample::default(),
            procs: Vec::new(),
            info,
        }
    }
}

/// Signals / termination modes a process can receive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcSignal {
    /// Polite termination (SIGTERM / WM_CLOSE-like).
    Terminate,
    /// Forced termination (SIGKILL / TerminateProcess).
    Kill,
    Suspend,
    Resume,
}

impl ProcSignal {
    pub fn label(self) -> &'static str {
        match self {
            ProcSignal::Terminate => "End task",
            ProcSignal::Kill => "Kill",
            ProcSignal::Suspend => "Suspend",
            ProcSignal::Resume => "Resume",
        }
    }
}

/// Task Manager priority levels mapped onto nice values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Realtime,
    High,
    AboveNormal,
    Normal,
    BelowNormal,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 6] = [
        Priority::Realtime,
        Priority::High,
        Priority::AboveNormal,
        Priority::Normal,
        Priority::BelowNormal,
        Priority::Low,
    ];

    pub fn label(self) -> &'static str {
        if cfg!(windows) {
            return match self {
                Priority::Realtime => "Realtime",
                _ => self.short_label(),
            };
        }
        match self {
            Priority::Realtime => "Highest (nice -20)",
            Priority::High => "High (nice -10)",
            Priority::AboveNormal => "Above normal (nice -5)",
            Priority::Normal => "Normal (nice 0)",
            Priority::BelowNormal => "Below normal (nice 5)",
            Priority::Low => "Low (nice 19)",
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn nice(self) -> i32 {
        match self {
            Priority::Realtime => -20,
            Priority::High => -10,
            Priority::AboveNormal => -5,
            Priority::Normal => 0,
            Priority::BelowNormal => 5,
            Priority::Low => 19,
        }
    }

    pub fn from_nice(nice: i32) -> Priority {
        match nice {
            i32::MIN..=-15 => Priority::Realtime,
            -14..=-8 => Priority::High,
            -7..=-1 => Priority::AboveNormal,
            0 => Priority::Normal,
            1..=9 => Priority::BelowNormal,
            _ => Priority::Low,
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Priority::Realtime => "Highest",
            Priority::High => "High",
            Priority::AboveNormal => "Above normal",
            Priority::Normal => "Normal",
            Priority::BelowNormal => "Below normal",
            Priority::Low => "Low",
        }
    }
}

/// An entry of the "Startup apps" page.
#[derive(Clone, Debug)]
pub struct StartupEntry {
    /// Opaque identifier understood by the platform backend.
    pub id: String,
    pub name: String,
    pub command: String,
    pub description: String,
    pub source: String,
    pub enabled: bool,
    /// The entry can be removed by the user (it is user owned).
    pub removable: bool,
    /// Location of the backing file / registry key, for "Open file location".
    pub location: String,
}

/// An entry of the "Services" page.
#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub description: String,
    /// "running", "exited", "dead", "failed", ...
    pub state: String,
    pub active: bool,
    pub failed: bool,
    /// Start-up type ("enabled", "disabled", "static", "Automatic", ...).
    pub startup: String,
    pub main_pid: Option<u32>,
    pub group: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl ServiceAction {
    pub fn label(self) -> &'static str {
        match self {
            ServiceAction::Start => "Start",
            ServiceAction::Stop => "Stop",
            ServiceAction::Restart => "Restart",
            ServiceAction::Enable => "Enable at boot",
            ServiceAction::Disable => "Disable at boot",
        }
    }
}

/// Detailed, on-demand information for the properties dialog.
#[derive(Clone, Debug, Default)]
pub struct ProcessDetails {
    pub rows: Vec<(String, String)>,
}

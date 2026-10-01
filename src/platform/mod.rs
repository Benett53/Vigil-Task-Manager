//! Platform abstraction. Each backend exposes the same free functions and a `Collector`.

pub mod common;
pub mod nvidia;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(any(target_os = "linux", windows)))]
compile_error!("Vigil currently supports Linux and Windows only.");

use crate::model::{
    Priority, ProcSignal, ProcessDetails, Service, ServiceAction, Snapshot, StartupEntry,
};
use std::fmt;

#[derive(Debug, Clone)]
pub enum ActionError {
    /// The operation needs more privileges; the UI can offer to retry elevated.
    PermissionDenied(String),
    Other(String),
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::PermissionDenied(m) => write!(f, "Access denied. {m}"),
            ActionError::Other(m) => f.write_str(m),
        }
    }
}

pub type ActionResult = Result<(), ActionError>;

/// Collects system snapshots. Not `Sync`; owned by the sampler thread.
pub struct Collector(imp::Collector);

impl Collector {
    pub fn new() -> Collector {
        Collector(imp::Collector::new())
    }

    pub fn sample(&mut self, with_processes: bool) -> Snapshot {
        self.0.sample(with_processes)
    }
}

/// Whether elevating through the platform's mechanism (polkit / UAC) is possible.
pub fn can_elevate() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::actions::which("pkexec").is_some()
    }
    #[cfg(windows)]
    {
        true
    }
}

pub const ELEVATION_NAME: &str = if cfg!(windows) {
    "administrator"
} else {
    "administrator (polkit)"
};

pub fn send_signal(pid: u32, sig: ProcSignal, elevated: bool) -> ActionResult {
    #[cfg(target_os = "linux")]
    {
        if elevated {
            linux::actions::send_signal_elevated(pid, sig)
        } else {
            linux::actions::send_signal(pid, sig)
        }
    }
    #[cfg(windows)]
    {
        windows::actions::send_signal(pid, sig, elevated)
    }
}

pub fn set_priority(pid: u32, prio: Priority, elevated: bool) -> ActionResult {
    #[cfg(target_os = "linux")]
    {
        if elevated {
            linux::actions::set_nice_elevated(pid, prio.nice())
        } else {
            linux::actions::set_nice(pid, prio.nice())
        }
    }
    #[cfg(windows)]
    {
        let _ = elevated;
        windows::actions::set_priority(pid, prio)
    }
}

pub fn set_efficiency(pid: u32, enabled: bool) -> ActionResult {
    imp::actions::set_efficiency(pid, enabled)
}

pub fn get_affinity(pid: u32) -> Result<Vec<bool>, ActionError> {
    imp::actions::get_affinity(pid)
}

pub fn set_affinity(pid: u32, mask: &[bool], elevated: bool) -> ActionResult {
    #[cfg(target_os = "linux")]
    {
        if elevated {
            linux::actions::set_affinity_elevated(pid, mask)
        } else {
            linux::actions::set_affinity(pid, mask)
        }
    }
    #[cfg(windows)]
    {
        let _ = elevated;
        windows::actions::set_affinity(pid, mask)
    }
}

pub fn process_details(pid: u32) -> ProcessDetails {
    imp::actions::details(pid)
}

pub fn run_task(command: &str, elevated: bool) -> ActionResult {
    imp::actions::run_task(command, elevated)
}

pub fn open_location(path: &str) -> ActionResult {
    imp::actions::open_location(path)
}

pub fn open_url(url: &str) -> ActionResult {
    imp::actions::open_url(url)
}

pub fn startup_list() -> Vec<StartupEntry> {
    imp::startup::list()
}

pub fn startup_set_enabled(entry: &StartupEntry, enabled: bool) -> ActionResult {
    imp::startup::set_enabled(entry, enabled)
}

pub fn startup_add(name: &str, command: &str) -> ActionResult {
    imp::startup::add(name, command)
}

pub fn startup_remove(entry: &StartupEntry) -> ActionResult {
    imp::startup::remove(entry)
}

pub fn last_boot_summary() -> Option<String> {
    imp::startup::last_boot_summary()
}

pub fn services_list(user: bool) -> Result<Vec<Service>, String> {
    imp::services::list(user)
}

pub fn service_control(name: &str, user: bool, action: ServiceAction) -> ActionResult {
    imp::services::control(name, user, action)
}

/// Name of the service manager ("systemd", "OpenRC", "Service Control Manager").
pub fn service_manager_name() -> &'static str {
    imp::services::init_name()
}

/// Whether the platform has a separate per-user service manager.
pub fn has_user_services() -> bool {
    cfg!(target_os = "linux") && service_manager_name() == "systemd"
}

/// Full display name of a user (GECOS / account full name).
pub fn user_full_name(uid: u32, name: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let _ = name;
        linux::users::full_name(uid)
    }
    #[cfg(windows)]
    {
        let _ = (uid, name);
        None
    }
}

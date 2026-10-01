//! Windows services through the Service Control Manager.

use super::actions::run_elevated_wait;
use super::wide;
use crate::model::{Service, ServiceAction};
use crate::platform::ActionError;
use std::collections::HashMap;
use std::sync::Arc;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW,
    OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, SC_ENUM_PROCESS_INFO, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START, SERVICE_BOOT_START,
    SERVICE_DEMAND_START, SERVICE_DISABLED, SERVICE_QUERY_CONFIG, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_SYSTEM_START, SERVICE_WIN32,
};

pub fn init_name() -> &'static str {
    "Service Control Manager"
}

struct Scm(SC_HANDLE);

impl Drop for Scm {
    fn drop(&mut self) {
        // SAFETY: handle obtained from OpenSCManagerW/OpenServiceW, closed once.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

unsafe fn pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller guarantees a NUL terminated UTF-16 string.
    unsafe {
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Enumerates all Win32 services: (name, display name, state, pid).
fn enumerate() -> Result<Vec<(String, String, u32, u32)>, String> {
    // SAFETY: buffer sized from the first call; entries point into that buffer.
    unsafe {
        let scm = OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
        );
        if scm.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let scm = Scm(scm);
        let mut needed = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        let mut buf: Vec<u8> = Vec::new();
        let mut out = Vec::new();
        loop {
            let ok = EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                if buf.is_empty() {
                    std::ptr::null_mut()
                } else {
                    buf.as_mut_ptr()
                },
                buf.len() as u32,
                &mut needed,
                &mut count,
                &mut resume,
                std::ptr::null(),
            );
            let items = buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW;
            for i in 0..count as usize {
                let e = &*items.add(i);
                out.push((
                    pwstr(e.lpServiceName),
                    pwstr(e.lpDisplayName),
                    e.ServiceStatusProcess.dwCurrentState,
                    e.ServiceStatusProcess.dwProcessId,
                ));
            }
            if ok != 0 {
                break;
            }
            if needed == 0 || buf.len() > 16 * 1024 * 1024 {
                break;
            }
            buf = vec![0u8; needed as usize + 4096];
            count = 0;
        }
        Ok(out)
    }
}

/// pid -> service name, used to link processes with their service.
pub fn pid_map() -> HashMap<u32, Arc<str>> {
    enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.3 != 0)
        .map(|s| (s.3, Arc::from(s.0.as_str())))
        .collect()
}

fn config(scm: SC_HANDLE, name: &str) -> Option<(String, String)> {
    // SAFETY: buffer sized by the API; strings point into it.
    unsafe {
        let svc = OpenServiceW(scm, wide(name).as_ptr(), SERVICE_QUERY_CONFIG);
        if svc.is_null() {
            return None;
        }
        let svc = Scm(svc);
        let mut needed = 0u32;
        QueryServiceConfigW(svc.0, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let cfg = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
        if QueryServiceConfigW(svc.0, cfg, needed, &mut needed) == 0 {
            return None;
        }
        let start = match (*cfg).dwStartType {
            SERVICE_AUTO_START => "Automatic",
            SERVICE_DEMAND_START => "Manual",
            SERVICE_DISABLED => "disabled",
            SERVICE_BOOT_START | SERVICE_SYSTEM_START => "Boot",
            _ => "",
        };
        Some((start.to_owned(), pwstr((*cfg).lpLoadOrderGroup)))
    }
}

pub fn list(_user: bool) -> Result<Vec<Service>, String> {
    let services = enumerate()?;
    // SAFETY: handle checked; closed by Scm.
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    let scm = (!scm.is_null()).then(|| Scm(scm));
    let mut out: Vec<Service> = services
        .into_iter()
        .map(|(name, display, state, pid)| {
            let (startup, group) = scm
                .as_ref()
                .and_then(|s| config(s.0, &name))
                .unwrap_or_default();
            Service {
                description: display,
                state: match state {
                    SERVICE_RUNNING => "running",
                    SERVICE_STOPPED => "stopped",
                    SERVICE_START_PENDING => "starting",
                    SERVICE_STOP_PENDING => "stopping",
                    _ => "paused",
                }
                .into(),
                active: state != SERVICE_STOPPED,
                failed: false,
                startup,
                main_pid: (pid != 0).then_some(pid),
                group,
                name,
            }
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

pub fn control(name: &str, _user: bool, action: ServiceAction) -> Result<(), ActionError> {
    if name.is_empty() || name.contains(['"', ' ', '/', '\\']) {
        return Err(ActionError::Other("Invalid service name".into()));
    }
    // Controlling services needs administrator rights; sc.exe is run through UAC.
    let params = match action {
        ServiceAction::Start => format!("start {name}"),
        ServiceAction::Stop => format!("stop {name}"),
        ServiceAction::Restart => {
            return run_elevated_wait(
                "cmd.exe",
                &format!("/C sc stop {name} & ping -n 3 127.0.0.1 >NUL & sc start {name}"),
            );
        }
        ServiceAction::Enable => format!("config {name} start= auto"),
        ServiceAction::Disable => format!("config {name} start= disabled"),
    };
    run_elevated_wait("sc.exe", &params)
}

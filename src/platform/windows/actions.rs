//! Process actions and launching helpers for Windows.

use super::wide;
use crate::model::{Priority, ProcSignal, ProcessDetails};
use crate::platform::ActionError;
use std::process::{Command, Stdio};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetExitCodeProcess, GetPriorityClass,
    GetProcessAffinityMask, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, INFINITE,
    NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SUSPEND_RESUME,
    PROCESS_TERMINATE, ProcessPowerThrottling, REALTIME_PRIORITY_CLASS, SetPriorityClass,
    SetProcessAffinityMask, SetProcessInformation, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(handle: HANDLE) -> i32;
    fn NtResumeProcess(handle: HANDLE) -> i32;
}

/// RAII wrapper around a process handle.
struct Proc(HANDLE);

impl Proc {
    fn open(pid: u32, access: u32, what: &str) -> Result<Proc, ActionError> {
        // SAFETY: OpenProcess returns null on failure, which we check.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            Err(last_error(what))
        } else {
            Ok(Proc(h))
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenProcess and is closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn last_error(what: &str) -> ActionError {
    // SAFETY: trivial getter.
    let code = unsafe { GetLastError() };
    let err = std::io::Error::from_raw_os_error(code as i32);
    if code == ERROR_ACCESS_DENIED {
        ActionError::PermissionDenied(format!("{what}: {err}"))
    } else {
        ActionError::Other(format!("{what}: {err}"))
    }
}

pub fn class_to_nice(class: u32) -> i32 {
    match class {
        REALTIME_PRIORITY_CLASS => -20,
        HIGH_PRIORITY_CLASS => -10,
        ABOVE_NORMAL_PRIORITY_CLASS => -5,
        BELOW_NORMAL_PRIORITY_CLASS => 5,
        IDLE_PRIORITY_CLASS => 19,
        _ => 0,
    }
}

fn priority_class(p: Priority) -> u32 {
    match p {
        Priority::Realtime => REALTIME_PRIORITY_CLASS,
        Priority::High => HIGH_PRIORITY_CLASS,
        Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        Priority::Normal => NORMAL_PRIORITY_CLASS,
        Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        Priority::Low => IDLE_PRIORITY_CLASS,
    }
}

pub fn send_signal(pid: u32, sig: ProcSignal, elevated: bool) -> Result<(), ActionError> {
    if pid == 0 || pid == 4 {
        return Err(ActionError::Other(
            "System processes cannot be ended.".into(),
        ));
    }
    if elevated {
        return match sig {
            ProcSignal::Terminate | ProcSignal::Kill => {
                run_elevated_wait("taskkill.exe", &format!("/F /PID {pid}"))
            }
            _ => Err(ActionError::Other(
                "Elevated suspend/resume is not supported.".into(),
            )),
        };
    }
    match sig {
        ProcSignal::Terminate => {
            // Graceful first (WM_CLOSE to the windows of the process), like Task Manager's "End task".
            let status = Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Ok(());
            }
            terminate(pid)
        }
        ProcSignal::Kill => terminate(pid),
        ProcSignal::Suspend | ProcSignal::Resume => {
            let p = Proc::open(pid, PROCESS_SUSPEND_RESUME, sig.label())?;
            // SAFETY: valid handle with SUSPEND_RESUME access.
            let status = unsafe {
                if sig == ProcSignal::Suspend {
                    NtSuspendProcess(p.0)
                } else {
                    NtResumeProcess(p.0)
                }
            };
            if status >= 0 {
                Ok(())
            } else {
                Err(ActionError::Other(format!(
                    "{} failed (NTSTATUS {status:#x})",
                    sig.label()
                )))
            }
        }
    }
}

fn terminate(pid: u32) -> Result<(), ActionError> {
    let p = Proc::open(pid, PROCESS_TERMINATE, "End task")?;
    // SAFETY: valid handle with TERMINATE access.
    if unsafe { TerminateProcess(p.0, 1) } != 0 {
        Ok(())
    } else {
        Err(last_error("End task"))
    }
}

pub fn set_priority(pid: u32, prio: Priority) -> Result<(), ActionError> {
    let p = Proc::open(pid, PROCESS_SET_INFORMATION, "Set priority")?;
    // SAFETY: valid handle with SET_INFORMATION access.
    if unsafe { SetPriorityClass(p.0, priority_class(prio)) } != 0 {
        Ok(())
    } else {
        Err(last_error("Set priority"))
    }
}

/// Efficiency mode as in Windows 11 Task Manager: EcoQoS power throttling + idle priority class.
pub fn set_efficiency(pid: u32, enabled: bool) -> Result<(), ActionError> {
    let p = Proc::open(pid, PROCESS_SET_INFORMATION, "Efficiency mode")?;
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if enabled {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    // SAFETY: valid handle; the struct pointer and size match.
    unsafe {
        if SetProcessInformation(
            p.0,
            ProcessPowerThrottling,
            &state as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        ) == 0
        {
            return Err(last_error("Efficiency mode"));
        }
        SetPriorityClass(
            p.0,
            if enabled {
                IDLE_PRIORITY_CLASS
            } else {
                NORMAL_PRIORITY_CLASS
            },
        );
    }
    Ok(())
}

pub fn get_affinity(pid: u32) -> Result<Vec<bool>, ActionError> {
    let p = Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION, "Get affinity")?;
    let (mut proc_mask, mut sys_mask) = (0usize, 0usize);
    // SAFETY: valid handle, out pointers to locals.
    if unsafe { GetProcessAffinityMask(p.0, &mut proc_mask, &mut sys_mask) } == 0 {
        return Err(last_error("Get affinity"));
    }
    let n = (usize::BITS - sys_mask.leading_zeros()) as usize;
    Ok((0..n.max(1)).map(|i| proc_mask & (1 << i) != 0).collect())
}

pub fn set_affinity(pid: u32, mask: &[bool]) -> Result<(), ActionError> {
    let bits = mask
        .iter()
        .take(usize::BITS as usize)
        .enumerate()
        .filter(|(_, on)| **on)
        .fold(0usize, |acc, (i, _)| acc | (1 << i));
    if bits == 0 {
        return Err(ActionError::Other(
            "At least one processor must be selected.".into(),
        ));
    }
    let p = Proc::open(pid, PROCESS_SET_INFORMATION, "Set affinity")?;
    // SAFETY: valid handle with SET_INFORMATION access.
    if unsafe { SetProcessAffinityMask(p.0, bits) } != 0 {
        Ok(())
    } else {
        Err(last_error("Set affinity"))
    }
}

/// Runs a program with a UAC prompt and waits for it to finish.
pub fn run_elevated_wait(file: &str, params: &str) -> Result<(), ActionError> {
    let verb = wide("runas");
    let file_w = wide(file);
    let params_w = wide(params);
    // SAFETY: all strings outlive the call; the process handle is closed below.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file_w.as_ptr();
        info.lpParameters = params_w.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            return Err(ActionError::Other(
                "Authorization was cancelled or denied.".into(),
            ));
        }
        if info.hProcess.is_null() {
            return Ok(());
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code == 0 {
            Ok(())
        } else {
            Err(ActionError::Other(format!(
                "{file} failed with exit code {code}"
            )))
        }
    }
}

fn shell_open(verb: &str, file: &str, params: Option<&str>) -> Result<(), ActionError> {
    let verb = wide(verb);
    let file_w = wide(file);
    let params_w = params.map(wide);
    // SAFETY: strings outlive the call.
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file_w.as_ptr(),
            params_w.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if rc as usize > 32 {
        Ok(())
    } else {
        Err(ActionError::Other(format!("Could not open {file}")))
    }
}

pub fn run_task(command: &str, elevated: bool) -> Result<(), ActionError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ActionError::Other("Please enter a command.".into()));
    }
    // Split program and arguments so ShellExecute can resolve App Paths, documents and URLs.
    let (file, params) = split_command(command);
    shell_open(
        if elevated { "runas" } else { "open" },
        &file,
        (!params.is_empty()).then_some(params.as_str()),
    )
}

fn split_command(command: &str) -> (String, String) {
    if let Some(rest) = command.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return (rest[..end].to_owned(), rest[end + 1..].trim().to_owned());
    }
    match command.split_once(' ') {
        Some((f, p)) => (f.to_owned(), p.trim().to_owned()),
        None => (command.to_owned(), String::new()),
    }
}

pub fn open_url(url: &str) -> Result<(), ActionError> {
    shell_open("open", url, None)
}

pub fn open_location(path: &str) -> Result<(), ActionError> {
    if path.is_empty() {
        return Err(ActionError::Other(
            "The location of this file is unknown.".into(),
        ));
    }
    Command::new("explorer.exe")
        .arg(format!("/select,{path}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| ActionError::Other(e.to_string()))
}

/// Reads `FileDescription` from an executable's version resource.
pub fn file_description(exe: &str) -> Option<String> {
    let path = wide(exe);
    // SAFETY: buffers are sized by the API; returned pointers point into `data`.
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let translation = wide("\\VarFileInfo\\Translation");
        let (lang, cp) = if VerQueryValueW(
            data.as_ptr().cast(),
            translation.as_ptr(),
            &mut ptr,
            &mut len,
        ) != 0
            && len >= 4
        {
            let pair = ptr as *const u16;
            (*pair, *pair.add(1))
        } else {
            (0x0409, 0x04b0)
        };
        let key = wide(&format!(
            "\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"
        ));
        if VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut ptr, &mut len) == 0 || len == 0 {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let s = String::from_utf16_lossy(slice)
            .trim_end_matches('\0')
            .trim()
            .to_owned();
        if s.is_empty() { None } else { Some(s) }
    }
}

pub fn details(pid: u32) -> ProcessDetails {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut rows = vec![("PID".to_owned(), pid.to_string())];
    let mut sys = System::new();
    let spid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        false,
        ProcessRefreshKind::everything()
            .with_cwd(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    if let Some(p) = sys.process(spid) {
        let mut push = |k: &str, v: String| {
            if !v.is_empty() {
                rows.push((k.to_owned(), v));
            }
        };
        push("Name", p.name().to_string_lossy().into_owned());
        push(
            "Parent PID",
            p.parent().map(|pp| pp.to_string()).unwrap_or_default(),
        );
        push(
            "Executable",
            p.exe()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        if let Some(exe) = p.exe() {
            push(
                "Description",
                file_description(&exe.to_string_lossy()).unwrap_or_default(),
            );
        }
        push(
            "Command line",
            p.cmd()
                .iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
        );
        push(
            "Working directory",
            p.cwd()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        push("Started", crate::util::fmt_unix_time(p.start_time()));
        push("Working set", crate::util::fmt_bytes(p.memory()));
        push("Virtual memory", crate::util::fmt_bytes(p.virtual_memory()));
        push(
            "CPU time",
            crate::util::fmt_cpu_time(p.accumulated_cpu_time() as f64 / 1000.0),
        );
        let du = p.disk_usage();
        push("Total read", crate::util::fmt_bytes(du.total_read_bytes));
        push(
            "Total written",
            crate::util::fmt_bytes(du.total_written_bytes),
        );
        push("Environment variables", p.environ().len().to_string());
    }
    if let Ok(proc) = Proc::open(pid, PROCESS_QUERY_LIMITED_INFORMATION, "query") {
        // SAFETY: valid handle.
        let class = unsafe { GetPriorityClass(proc.0) };
        rows.push((
            "Priority class".into(),
            crate::model::Priority::from_nice(class_to_nice(class))
                .short_label()
                .into(),
        ));
    }
    ProcessDetails { rows }
}

#[cfg(test)]
mod tests {
    use super::split_command;

    #[test]
    fn command_splitting() {
        assert_eq!(split_command("notepad"), ("notepad".into(), String::new()));
        assert_eq!(
            split_command("notepad a.txt"),
            ("notepad".into(), "a.txt".into())
        );
        assert_eq!(
            split_command("\"C:\\Program Files\\App\\app.exe\" --x"),
            ("C:\\Program Files\\App\\app.exe".into(), "--x".into())
        );
    }
}

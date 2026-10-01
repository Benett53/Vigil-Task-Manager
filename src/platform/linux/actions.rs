//! Actions on processes and helpers to launch external programs.

use super::procfs::{parse_cmdline, parse_pid_stat, read_trimmed};
use crate::model::{ProcSignal, ProcessDetails};
use crate::platform::ActionError;
use std::path::Path;
use std::process::{Command, Stdio};

fn os_error(what: &str) -> ActionError {
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EPERM) | Some(libc::EACCES) => {
            ActionError::PermissionDenied(format!("{what}: {err}"))
        }
        Some(libc::ESRCH) => ActionError::Other(format!("{what}: the process no longer exists")),
        _ => ActionError::Other(format!("{what}: {err}")),
    }
}

fn signal_number(sig: ProcSignal) -> i32 {
    match sig {
        ProcSignal::Terminate => libc::SIGTERM,
        ProcSignal::Kill => libc::SIGKILL,
        ProcSignal::Suspend => libc::SIGSTOP,
        ProcSignal::Resume => libc::SIGCONT,
    }
}

fn signal_name(sig: ProcSignal) -> &'static str {
    match sig {
        ProcSignal::Terminate => "TERM",
        ProcSignal::Kill => "KILL",
        ProcSignal::Suspend => "STOP",
        ProcSignal::Resume => "CONT",
    }
}

pub fn send_signal(pid: u32, sig: ProcSignal) -> Result<(), ActionError> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(ActionError::Other("invalid process id".into()));
    }
    // SAFETY: kill(2) with a positive pid only targets that single process.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal_number(sig)) };
    if rc == 0 {
        Ok(())
    } else {
        Err(os_error(sig.label()))
    }
}

/// Sends a signal through polkit (`pkexec kill`). Blocks until the user answered the prompt.
pub fn send_signal_elevated(pid: u32, sig: ProcSignal) -> Result<(), ActionError> {
    run_pkexec(&["kill", "-s", signal_name(sig), &pid.to_string()])
}

fn task_ids(pid: u32) -> Vec<i32> {
    let mut tids: Vec<i32> = std::fs::read_dir(format!("/proc/{pid}/task"))
        .map(|r| {
            r.flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse().ok()))
                .collect()
        })
        .unwrap_or_default();
    if tids.is_empty() {
        tids.push(pid as i32);
    }
    tids
}

/// Sets the nice value of every thread of the process (nice is per-thread on Linux).
pub fn set_nice(pid: u32, nice: i32) -> Result<(), ActionError> {
    let mut first_err = None;
    let mut ok = 0;
    for tid in task_ids(pid) {
        // SAFETY: setpriority only affects the given thread id.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) };
        if rc == 0 {
            ok += 1;
        } else if first_err.is_none() {
            first_err = Some(os_error("Set priority"));
        }
    }
    match first_err {
        Some(e) if ok == 0 || matches!(e, ActionError::PermissionDenied(_)) => Err(e),
        _ => Ok(()),
    }
}

pub fn set_nice_elevated(pid: u32, nice: i32) -> Result<(), ActionError> {
    let script = format!(
        "for t in /proc/{pid}/task/*; do renice -n {nice} -p \"${{t##*/}}\" >/dev/null || exit 1; done"
    );
    run_pkexec(&["sh", "-c", &script])
}

const IOPRIO_WHO_PROCESS: libc::c_long = 1;
const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
const IOPRIO_CLASS_IDLE: libc::c_long = 3;

/// "Efficiency mode": lowest CPU priority plus idle I/O scheduling class.
pub fn set_efficiency(pid: u32, enabled: bool) -> Result<(), ActionError> {
    set_nice(pid, if enabled { 19 } else { 0 })?;
    let ioprio = if enabled {
        IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT
    } else {
        0
    };
    for tid in task_ids(pid) {
        // SAFETY: raw ioprio_set syscall, only affects the given thread.
        unsafe {
            libc::syscall(
                libc::SYS_ioprio_set,
                IOPRIO_WHO_PROCESS,
                tid as libc::c_long,
                ioprio,
            );
        }
    }
    Ok(())
}

pub fn get_affinity(pid: u32) -> Result<Vec<bool>, ActionError> {
    // SAFETY: cpu_set_t is plain data; sched_getaffinity fills it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &mut set,
        ) != 0
        {
            return Err(os_error("Get affinity"));
        }
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(online_cpu_count());
        Ok((0..n).map(|i| libc::CPU_ISSET(i, &set)).collect())
    }
}

fn online_cpu_count() -> usize {
    read_trimmed("/sys/devices/system/cpu/online")
        .map(|s| {
            super::procfs::parse_cpu_list(&s)
                .into_iter()
                .max()
                .map_or(0, |m| m + 1)
        })
        .unwrap_or(0)
}

pub fn set_affinity(pid: u32, mask: &[bool]) -> Result<(), ActionError> {
    if !mask.iter().any(|b| *b) {
        return Err(ActionError::Other(
            "At least one processor must be selected.".into(),
        ));
    }
    let mut first_err = None;
    for tid in task_ids(pid) {
        // SAFETY: see get_affinity.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            for (i, on) in mask.iter().enumerate() {
                if *on {
                    libc::CPU_SET(i, &mut set);
                }
            }
            if libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0
                && first_err.is_none()
            {
                first_err = Some(os_error("Set affinity"));
            }
        }
    }
    first_err.map_or(Ok(()), Err)
}

pub fn set_affinity_elevated(pid: u32, mask: &[bool]) -> Result<(), ActionError> {
    let list: Vec<String> = mask
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(i, _)| i.to_string())
        .collect();
    run_pkexec(&[
        "taskset",
        "-a",
        "-p",
        "-c",
        &list.join(","),
        &pid.to_string(),
    ])
}

pub fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
    std::env::split_paths(&path)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

fn run_pkexec(args: &[&str]) -> Result<(), ActionError> {
    if which("pkexec").is_none() {
        return Err(ActionError::Other(
            "Administrator rights are required, but pkexec (polkit) is not installed.".into(),
        ));
    }
    let out = Command::new("pkexec")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ActionError::Other(format!("Could not run pkexec: {e}")))?;
    match out.status.code() {
        Some(0) => Ok(()),
        Some(126) | Some(127) => Err(ActionError::Other(
            "Authorization was cancelled or denied.".into(),
        )),
        _ => {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            Err(ActionError::Other(if msg.is_empty() {
                "The command failed.".into()
            } else {
                msg
            }))
        }
    }
}

/// Spawns a detached program and reaps it in the background so no zombie is left behind.
pub fn spawn_detached(mut cmd: Command) -> Result<(), ActionError> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn().map_err(|e| ActionError::Other(e.to_string()))?;
    std::thread::Builder::new()
        .name("vigil-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .ok();
    Ok(())
}

pub fn run_task(command: &str, elevated: bool) -> Result<(), ActionError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ActionError::Other("Please enter a command.".into()));
    }
    let mut cmd = if elevated {
        if which("pkexec").is_none() {
            return Err(ActionError::Other(
                "pkexec (polkit) is not installed.".into(),
            ));
        }
        let mut c = Command::new("pkexec");
        c.args(["sh", "-c", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    if let Some(home) = std::env::var_os("HOME") {
        cmd.current_dir(home);
    }
    spawn_detached(cmd)
}

pub fn open_url(url: &str) -> Result<(), ActionError> {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url);
    spawn_detached(cmd)
}

/// Shows a file in the file manager (selected if supported), falling back to opening its folder.
pub fn open_location(path: &str) -> Result<(), ActionError> {
    if path.is_empty() {
        return Err(ActionError::Other(
            "The location of this file is unknown.".into(),
        ));
    }
    let p = Path::new(path);
    if which("dbus-send").is_some() && p.exists() {
        let uri = format!("file://{}", path_to_uri(path));
        let status = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{uri}"),
                "string:",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    let dir = if p.is_dir() {
        p
    } else {
        p.parent().unwrap_or(Path::new("/"))
    };
    open_url(&dir.to_string_lossy())
}

fn path_to_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Collects detailed information about a process for the properties dialog.
pub fn details(pid: u32) -> ProcessDetails {
    let mut rows: Vec<(String, String)> = Vec::new();
    let base = format!("/proc/{pid}");
    let mut push = |k: &str, v: String| {
        if !v.is_empty() {
            rows.push((k.to_owned(), v));
        }
    };
    push("PID", pid.to_string());
    let stat = std::fs::read_to_string(format!("{base}/stat")).unwrap_or_default();
    if let Some(st) = parse_pid_stat(&stat) {
        push("Name (comm)", st.comm.to_owned());
        push("Parent PID", st.ppid.to_string());
        push("Nice", st.nice.to_string());
        push("Threads", st.threads.to_string());
    }
    push(
        "Executable",
        std::fs::read_link(format!("{base}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "(access denied)".into()),
    );
    push(
        "Command line",
        std::fs::read(format!("{base}/cmdline"))
            .map(|r| parse_cmdline(&r))
            .unwrap_or_default(),
    );
    push(
        "Working directory",
        std::fs::read_link(format!("{base}/cwd"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "(access denied)".into()),
    );
    if let Ok(status) = std::fs::read_to_string(format!("{base}/status")) {
        for line in status.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v = v.trim();
            let label = match k {
                "State" => "State",
                "Uid" => "UID (real, effective, saved, fs)",
                "Gid" => "GID (real, effective, saved, fs)",
                "VmPeak" => "Peak virtual memory",
                "VmHWM" => "Peak resident memory",
                "VmRSS" => "Resident memory",
                "RssAnon" => "Resident anonymous",
                "RssFile" => "Resident file-backed",
                "RssShmem" => "Resident shared memory",
                "VmSwap" => "Swapped out",
                "voluntary_ctxt_switches" => "Voluntary context switches",
                "nonvoluntary_ctxt_switches" => "Involuntary context switches",
                "Cpus_allowed_list" => "Allowed CPUs",
                "Seccomp" => "Seccomp mode",
                "NoNewPrivs" => "No new privileges",
                _ => continue,
            };
            push(label, v.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    if let Ok(read) = std::fs::read_dir(format!("{base}/fd")) {
        push("Open file descriptors", read.count().to_string());
    }
    if let Some(cg) = read_trimmed(format!("{base}/cgroup")) {
        push("Control group", cg.lines().last().unwrap_or("").to_owned());
    }
    if let Some(oom) = read_trimmed(format!("{base}/oom_score")) {
        push("OOM score", oom);
    }
    // SAFETY: plain syscalls returning integers.
    let policy = unsafe { libc::sched_getscheduler(pid as libc::pid_t) };
    let policy_name = match policy {
        libc::SCHED_OTHER => "Normal (SCHED_OTHER)",
        libc::SCHED_BATCH => "Batch (SCHED_BATCH)",
        libc::SCHED_IDLE => "Idle (SCHED_IDLE)",
        libc::SCHED_FIFO => "Real-time FIFO",
        libc::SCHED_RR => "Real-time round robin",
        _ => "",
    };
    push("Scheduling policy", policy_name.to_owned());
    let ioprio = unsafe {
        libc::syscall(
            libc::SYS_ioprio_get,
            IOPRIO_WHO_PROCESS,
            pid as libc::c_long,
        )
    };
    if ioprio >= 0 {
        let class = match ioprio >> IOPRIO_CLASS_SHIFT {
            0 => "Default (best effort)".to_owned(),
            1 => format!("Real-time, level {}", ioprio & 0xff),
            2 => format!("Best effort, level {}", ioprio & 0xff),
            3 => "Idle".to_owned(),
            _ => String::new(),
        };
        push("I/O priority", class);
    }
    if let Ok(env) = std::fs::read(format!("{base}/environ")) {
        push(
            "Environment variables",
            env.split(|b| *b == 0)
                .filter(|v| !v.is_empty())
                .count()
                .to_string(),
        );
    }
    ProcessDetails { rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcSignal;

    fn state_of(pid: u32) -> u8 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        parse_pid_stat(&s).unwrap().state
    }

    fn nice_of(pid: u32) -> i32 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        parse_pid_stat(&s).unwrap().nice
    }

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if f() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn process_actions_on_child() {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        send_signal(pid, ProcSignal::Suspend).unwrap();
        assert!(
            wait_for(|| state_of(pid) == b'T'),
            "process should be stopped"
        );
        send_signal(pid, ProcSignal::Resume).unwrap();
        assert!(
            wait_for(|| state_of(pid) != b'T'),
            "process should run again"
        );

        set_nice(pid, 5).unwrap();
        assert_eq!(nice_of(pid), 5);
        set_efficiency(pid, true).unwrap();
        assert_eq!(nice_of(pid), 19);
        let io = unsafe {
            libc::syscall(
                libc::SYS_ioprio_get,
                IOPRIO_WHO_PROCESS,
                pid as libc::c_long,
            )
        };
        assert_eq!(io >> IOPRIO_CLASS_SHIFT, IOPRIO_CLASS_IDLE);

        let mask = get_affinity(pid).unwrap();
        assert!(mask.iter().any(|b| *b));
        let mut only_first = vec![false; mask.len()];
        only_first[0] = true;
        set_affinity(pid, &only_first).unwrap();
        assert_eq!(get_affinity(pid).unwrap()[..1], [true]);
        assert!(get_affinity(pid).unwrap()[1..].iter().all(|b| !b));
        assert!(set_affinity(pid, &vec![false; mask.len()]).is_err());

        let details = details(pid);
        // `sleep` may be a multi-call binary (BusyBox on Alpine), so compare with the real path.
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
        assert!(
            details
                .rows
                .iter()
                .any(|(k, v)| k == "Executable" && *v == exe.to_string_lossy())
        );

        send_signal(pid, ProcSignal::Terminate).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(send_signal(pid, ProcSignal::Kill).is_err());
    }

    #[test]
    fn invalid_pids_are_rejected() {
        assert!(send_signal(0, ProcSignal::Kill).is_err());
        assert!(send_signal(u32::MAX, ProcSignal::Kill).is_err());
    }
}

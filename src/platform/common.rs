//! Helpers shared by all platform backends.

use crate::model::{ProcKind, Process};
use std::collections::HashMap;

/// Child processes inherit the application of their parent (e.g. browser helpers,
/// shells inside a terminal) when they run as the same user, so they are grouped
/// under that application like in Windows Task Manager.
pub fn inherit_apps(procs: &mut [Process]) {
    let index: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    for i in 0..procs.len() {
        if procs[i].app_id.is_some() || procs[i].kind == ProcKind::System {
            continue;
        }
        // Walk up a few levels to find an application ancestor of the same user.
        let mut cur = procs[i].ppid;
        let mut found = None;
        for _ in 0..8 {
            let Some(&pi) = index.get(&cur) else { break };
            let parent = &procs[pi];
            if parent.uid != procs[i].uid {
                break;
            }
            if parent.app_id.is_some() {
                found = Some((parent.app_id.clone(), parent.app_name.clone()));
                break;
            }
            // Do not cross user service boundaries.
            if parent.unit != procs[i].unit {
                break;
            }
            cur = parent.ppid;
        }
        if let Some((id, name)) = found {
            procs[i].app_id = id;
            procs[i].app_name = name;
            procs[i].kind = ProcKind::App;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcState;
    use std::sync::Arc;

    fn proc(pid: u32, ppid: u32, app: Option<&str>, kind: ProcKind) -> Process {
        Process {
            pid,
            ppid,
            name: Arc::from(format!("p{pid}")),
            exe: Arc::from(""),
            cmdline: Arc::from(""),
            user: Arc::from("me"),
            uid: 1000,
            state: ProcState::Sleeping,
            kind,
            app_id: app.map(Arc::from),
            app_name: app.map(Arc::from),
            unit: None,
            cpu: 0.0,
            cpu_time: 0.0,
            cpu_delta: 0.0,
            mem: 0,
            rss: 0,
            virt: 0,
            threads: 1,
            nice: 0,
            start_time: 0,
            disk_read: None,
            disk_write: None,
            disk_read_delta: 0,
            disk_write_delta: 0,
            gpu: None,
            gpu_mem: None,
            gpu_delta: 0.0,
            efficiency: false,
            kernel_thread: false,
        }
    }

    #[test]
    fn children_join_their_app() {
        let mut v = vec![
            proc(10, 1, Some("browser"), ProcKind::App),
            proc(11, 10, None, ProcKind::Background),
            proc(12, 11, None, ProcKind::Background),
            proc(20, 1, None, ProcKind::Background),
            proc(30, 10, None, ProcKind::System),
        ];
        inherit_apps(&mut v);
        assert_eq!(v[1].app_id.as_deref(), Some("browser"));
        assert_eq!(v[2].kind, ProcKind::App);
        assert_eq!(v[3].app_id, None);
        assert_eq!(v[4].app_id, None, "other users' processes are not grouped");
    }
}

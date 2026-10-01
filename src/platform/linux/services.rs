//! Services page: systemd (system and user managers) with an OpenRC fallback.

use super::actions::which;
use crate::model::{Service, ServiceAction};
use crate::platform::ActionError;
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitSystem {
    Systemd,
    OpenRc,
    Unsupported,
}

pub fn init_system() -> InitSystem {
    if Path::new("/run/systemd/system").exists() && which("systemctl").is_some() {
        InitSystem::Systemd
    } else if which("rc-status").is_some() && which("rc-service").is_some() {
        InitSystem::OpenRc
    } else {
        InitSystem::Unsupported
    }
}

pub fn init_name() -> &'static str {
    match init_system() {
        InitSystem::Systemd => "systemd",
        InitSystem::OpenRc => "OpenRC",
        InitSystem::Unsupported => "unknown",
    }
}

pub fn list(user: bool) -> Result<Vec<Service>, String> {
    match init_system() {
        InitSystem::Systemd => list_systemd(user),
        InitSystem::OpenRc if !user => list_openrc(),
        InitSystem::OpenRc => Ok(Vec::new()),
        InitSystem::Unsupported => {
            Err("No supported service manager was found. Vigil supports systemd and OpenRC.".into())
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .env("SYSTEMD_COLORS", "0")
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn list_systemd(user: bool) -> Result<Vec<Service>, String> {
    let scope = if user { "--user" } else { "--system" };
    let units = run(
        "systemctl",
        &[
            scope,
            "list-units",
            "--type=service",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
        ],
    )?;
    let files = run(
        "systemctl",
        &[
            scope,
            "list-unit-files",
            "--type=service",
            "--plain",
            "--no-legend",
            "--no-pager",
        ],
    )
    .unwrap_or_default();
    Ok(merge_systemd(
        &units,
        &files,
        if user { "User" } else { "System" },
    ))
}

/// Combines `list-units` and `list-unit-files` output.
pub fn merge_systemd(units: &str, files: &str, group: &str) -> Vec<Service> {
    let mut startup: HashMap<&str, &str> = HashMap::new();
    for line in files.lines() {
        let mut f = line.split_whitespace();
        if let (Some(unit), Some(state)) = (f.next(), f.next()) {
            startup.insert(unit, state);
        }
    }
    let mut out: Vec<Service> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in units.lines() {
        let line = line.trim_start_matches(['●', '*', ' ']);
        let mut f = line.split_whitespace();
        let (Some(unit), Some(load), Some(active), Some(sub)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        if load == "not-found" || !unit.ends_with(".service") {
            continue;
        }
        let description = f.collect::<Vec<_>>().join(" ");
        seen.insert(unit.to_owned());
        out.push(Service {
            name: unit.trim_end_matches(".service").to_owned(),
            description,
            state: sub.to_owned(),
            active: active == "active" || active == "reloading" || active == "activating",
            failed: active == "failed",
            startup: startup
                .get(unit)
                .copied()
                .unwrap_or(if load == "masked" { "masked" } else { "static" })
                .to_owned(),
            main_pid: None,
            group: group.to_owned(),
        });
    }
    // Installed but not loaded units (typically disabled services).
    for (unit, state) in startup {
        if unit.contains("@.") || seen.contains(unit) {
            continue;
        }
        out.push(Service {
            name: unit.trim_end_matches(".service").to_owned(),
            description: String::new(),
            state: "dead".into(),
            active: false,
            failed: false,
            startup: state.to_owned(),
            main_pid: None,
            group: group.to_owned(),
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn list_openrc() -> Result<Vec<Service>, String> {
    let status = run("rc-status", &["--all", "--nocolor"])?;
    let enabled = run("rc-update", &["show", "--verbose"]).unwrap_or_default();
    Ok(parse_openrc(&status, &enabled))
}

pub fn parse_openrc(status: &str, rc_update: &str) -> Vec<Service> {
    let mut runlevels: HashMap<&str, String> = HashMap::new();
    for line in rc_update.lines() {
        if let Some((name, levels)) = line.split_once('|') {
            let levels = levels.trim();
            runlevels.insert(
                name.trim(),
                if levels.is_empty() {
                    "disabled".into()
                } else {
                    levels.to_owned()
                },
            );
        }
    }
    let mut out: Vec<Service> = Vec::new();
    for line in status.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("Runlevel:") || t.starts_with("Dynamic Runlevel:") {
            continue;
        }
        let (Some(open), Some(close)) = (t.rfind('['), t.rfind(']')) else {
            continue;
        };
        if close < open {
            continue;
        }
        let name = t[..open].trim().to_owned();
        let state = t[open + 1..close].trim().to_owned();
        if name.is_empty() || out.iter().any(|s| s.name == name) {
            continue;
        }
        out.push(Service {
            startup: runlevels
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "disabled".into()),
            description: String::new(),
            active: state == "started",
            failed: state == "crashed" || state == "failed",
            state,
            main_pid: None,
            group: "System".into(),
            name,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Runs a service action. System services may trigger a polkit authentication dialog.
pub fn control(name: &str, user: bool, action: ServiceAction) -> Result<(), ActionError> {
    if name.is_empty() || name.starts_with('-') || name.contains(['/', ' ']) {
        return Err(ActionError::Other("Invalid service name".into()));
    }
    match init_system() {
        InitSystem::Systemd => {
            let verb = match action {
                ServiceAction::Start => "start",
                ServiceAction::Stop => "stop",
                ServiceAction::Restart => "restart",
                ServiceAction::Enable => "enable",
                ServiceAction::Disable => "disable",
            };
            let unit = format!("{name}.service");
            let scope = if user { "--user" } else { "--system" };
            let out = Command::new("systemctl")
                .args([scope, verb, "--", &unit])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| ActionError::Other(e.to_string()))?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            let auth = err.contains("authentication")
                || err.contains("Access denied")
                || err.contains("authorized");
            if !user && auth && which("pkexec").is_some() {
                let out = Command::new("pkexec")
                    .args(["systemctl", verb, "--", &unit])
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|e| ActionError::Other(e.to_string()))?;
                if out.status.success() {
                    return Ok(());
                }
                return Err(ActionError::Other(
                    String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                ));
            }
            Err(if auth {
                ActionError::PermissionDenied(err)
            } else {
                ActionError::Other(err)
            })
        }
        InitSystem::OpenRc => {
            let args: Vec<&str> = match action {
                ServiceAction::Start => vec!["rc-service", name, "start"],
                ServiceAction::Stop => vec!["rc-service", name, "stop"],
                ServiceAction::Restart => vec!["rc-service", name, "restart"],
                ServiceAction::Enable => vec!["rc-update", "add", name, "default"],
                ServiceAction::Disable => vec!["rc-update", "del", name],
            };
            // SAFETY: getuid never fails.
            let is_root = unsafe { libc::getuid() } == 0;
            let mut cmd = if is_root {
                let mut c = Command::new(args[0]);
                c.args(&args[1..]);
                c
            } else {
                let mut c = Command::new("pkexec");
                c.args(&args);
                c
            };
            let out = cmd
                .stdin(Stdio::null())
                .output()
                .map_err(|e| ActionError::Other(e.to_string()))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(ActionError::Other(
                    String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                ))
            }
        }
        InitSystem::Unsupported => Err(ActionError::Other("No supported service manager".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_merge() {
        let units = "cron.service loaded active running Regular background program processing daemon\n● bad.service not-found inactive dead bad.service\nfoo.service loaded failed failed Foo Service\n";
        let files = "cron.service enabled enabled\nfoo.service disabled enabled\nssh.service disabled enabled\ngetty@.service enabled enabled\n";
        let s = merge_systemd(units, files, "System");
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["cron", "foo", "ssh"]);
        assert!(s[0].active);
        assert_eq!(
            s[0].description,
            "Regular background program processing daemon"
        );
        assert_eq!(s[0].startup, "enabled");
        assert!(s[1].failed);
        assert_eq!(s[2].state, "dead");
    }

    #[test]
    fn openrc_parse() {
        let status = "Runlevel: default\n sshd                                     [  started  ]\n cronie                                   [  stopped  ]\nDynamic Runlevel: hotplugged\nDynamic Runlevel: needed/wanted\n udev                                     [  started  ]\n";
        let update = "                sshd | default\n              cronie | \n";
        let s = parse_openrc(status, update);
        assert_eq!(s.len(), 3);
        let sshd = s.iter().find(|x| x.name == "sshd").unwrap();
        assert!(sshd.active);
        assert_eq!(sshd.startup, "default");
        assert_eq!(
            s.iter().find(|x| x.name == "cronie").unwrap().startup,
            "disabled"
        );
    }
}

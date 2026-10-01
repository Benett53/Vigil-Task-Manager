//! "Startup apps" on Linux: XDG autostart entries plus user-enabled systemd user services.

use super::actions::which;
use super::desktop::{config_dirs, config_home, current_desktops, parse_desktop_entry};
use crate::model::StartupEntry;
use crate::platform::ActionError;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn user_autostart_dir() -> Option<PathBuf> {
    config_home().map(|c| c.join("autostart"))
}

pub fn list() -> Vec<StartupEntry> {
    let mut out = Vec::new();
    let desktops = current_desktops();
    // filename -> (path, is_user)
    let mut files: BTreeMap<String, (PathBuf, bool, bool)> = BTreeMap::new();
    for dir in config_dirs().iter().rev() {
        scan_dir(&dir.join("autostart"), false, &mut files);
    }
    if let Some(user) = user_autostart_dir() {
        scan_dir(&user, true, &mut files);
    }
    for (file, (path, is_user, has_system)) in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(entry) = parse_desktop_entry(&file, &content) else {
            continue;
        };
        if !desktops.is_empty() && !entry.shown_in(&desktops) {
            continue;
        }
        let enabled = !entry.hidden && entry.gnome_autostart_enabled != Some(false);
        out.push(StartupEntry {
            id: format!("xdg:{file}"),
            name: entry.name.clone(),
            command: entry.exec.clone(),
            description: if entry.comment.is_empty() {
                entry.generic_name.clone()
            } else {
                entry.comment.clone()
            },
            source: match (is_user, has_system) {
                (true, false) => "Autostart (user)".into(),
                (true, true) => "Autostart (system, user override)".into(),
                _ => "Autostart (system)".into(),
            },
            enabled,
            removable: is_user && !has_system,
            location: path.to_string_lossy().into_owned(),
        });
    }
    out.extend(systemd_user_units());
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

fn scan_dir(dir: &Path, is_user: bool, files: &mut BTreeMap<String, (PathBuf, bool, bool)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in read.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".desktop") {
            continue;
        }
        let had_system = files.get(&name).is_some_and(|(_, user, _)| !user);
        files.insert(name, (ent.path(), is_user, had_system));
    }
}

/// systemd user services whose enablement differs from the distribution preset,
/// i.e. services the user enabled (or disabled) themselves.
fn systemd_user_units() -> Vec<StartupEntry> {
    if !Path::new("/run/systemd/system").exists() || which("systemctl").is_none() {
        return Vec::new();
    }
    let Ok(out) = Command::new("systemctl")
        .args([
            "--user",
            "list-unit-files",
            "--type=service",
            "--plain",
            "--no-legend",
            "--no-pager",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let user_dir = config_home().map(|c| c.join("systemd/user"));
    let mut entries = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(unit), Some(state)) = (f.next(), f.next()) else {
            continue;
        };
        let preset = f.next().unwrap_or("");
        if unit.contains('@') || !(state == "enabled" || state == "disabled") {
            continue;
        }
        let user_owned = user_dir.as_ref().is_some_and(|d| d.join(unit).exists());
        if !user_owned && (preset.is_empty() || preset == state) {
            continue;
        }
        let location = user_dir
            .as_ref()
            .map(|d| d.join(unit))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        entries.push(StartupEntry {
            id: format!("systemd-user:{unit}"),
            name: unit.trim_end_matches(".service").to_owned(),
            command: String::new(),
            description: "systemd user service".into(),
            source: "systemd (user)".into(),
            enabled: state == "enabled",
            removable: false,
            location,
        });
    }
    entries
}

/// Sets `key=value` inside the `[Desktop Entry]` group, adding the key when missing.
pub fn set_desktop_key(content: &str, key: &str, value: &str) -> String {
    let mut out = Vec::new();
    let mut in_group = false;
    let mut done = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_group && !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            in_group = trimmed == "[Desktop Entry]";
            out.push(line.to_owned());
            continue;
        }
        if in_group
            && trimmed
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == key)
        {
            if !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        if !out.iter().any(|l| l.trim() == "[Desktop Entry]") {
            out.insert(0, "[Desktop Entry]".to_owned());
        }
        // Group was last (or only) in the file.
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

pub fn set_enabled(entry: &StartupEntry, enabled: bool) -> Result<(), ActionError> {
    if let Some(unit) = entry.id.strip_prefix("systemd-user:") {
        let out = Command::new("systemctl")
            .args(["--user", if enabled { "enable" } else { "disable" }, unit])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| ActionError::Other(e.to_string()))?;
        return if out.status.success() {
            Ok(())
        } else {
            Err(ActionError::Other(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            ))
        };
    }
    let Some(file) = entry.id.strip_prefix("xdg:") else {
        return Err(ActionError::Other("Unknown startup entry".into()));
    };
    let dir = user_autostart_dir().ok_or_else(|| ActionError::Other("$HOME is not set".into()))?;
    std::fs::create_dir_all(&dir).map_err(|e| ActionError::Other(e.to_string()))?;
    let target = dir.join(file);
    // Start from the user copy if present, otherwise from the system file.
    let content = std::fs::read_to_string(&target)
        .or_else(|_| std::fs::read_to_string(&entry.location))
        .map_err(|e| ActionError::Other(e.to_string()))?;
    let content = set_desktop_key(&content, "Hidden", if enabled { "false" } else { "true" });
    let content = set_desktop_key(
        &content,
        "X-GNOME-Autostart-enabled",
        if enabled { "true" } else { "false" },
    );
    write_atomic(&target, &content)
}

pub fn add(name: &str, command: &str) -> Result<(), ActionError> {
    let name = name.trim();
    let command = command.trim();
    if name.is_empty() || command.is_empty() {
        return Err(ActionError::Other("Name and command are required.".into()));
    }
    if name.contains('\n') || command.contains('\n') {
        return Err(ActionError::Other(
            "Name and command must be a single line.".into(),
        ));
    }
    let dir = user_autostart_dir().ok_or_else(|| ActionError::Other("$HOME is not set".into()))?;
    std::fs::create_dir_all(&dir).map_err(|e| ActionError::Other(e.to_string()))?;
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut path = dir.join(format!("vigil-{slug}.desktop"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("vigil-{slug}-{n}.desktop"));
    }
    let content = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={command}\nComment=Added with Vigil Task Manager\nX-GNOME-Autostart-enabled=true\nHidden=false\n"
    );
    write_atomic(&path, &content)
}

pub fn remove(entry: &StartupEntry) -> Result<(), ActionError> {
    if !entry.removable {
        return Err(ActionError::Other(
            "Only entries created for your user can be removed.".into(),
        ));
    }
    std::fs::remove_file(&entry.location).map_err(|e| ActionError::Other(e.to_string()))
}

fn write_atomic(path: &Path, content: &str) -> Result<(), ActionError> {
    let tmp = path.with_extension("desktop.vigil-tmp");
    std::fs::write(&tmp, content).map_err(|e| ActionError::Other(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ActionError::Other(e.to_string())
    })
}

/// Boot time breakdown from `systemd-analyze` (e.g. "Firmware 5.1s, loader 2.0s, kernel 1.2s, userspace 6.3s").
pub fn last_boot_summary() -> Option<String> {
    which("systemd-analyze")?;
    let out = Command::new("systemd-analyze")
        .arg("time")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    let rest = line.strip_prefix("Startup finished in ")?;
    Some(
        rest.split(" = ")
            .next()
            .unwrap_or(rest)
            .replace(" + ", ", "),
    )
}

#[cfg(test)]
mod tests {
    use super::set_desktop_key;

    #[test]
    fn set_key_replaces_and_adds() {
        let src = "[Desktop Entry]\nName=A\nHidden=false\n\n[Desktop Action x]\nName=B\n";
        let out = set_desktop_key(src, "Hidden", "true");
        assert!(out.contains("Hidden=true"));
        assert!(!out.contains("Hidden=false"));
        let out = set_desktop_key(&out, "X-GNOME-Autostart-enabled", "false");
        let idx_key = out.find("X-GNOME-Autostart-enabled=false").unwrap();
        let idx_action = out.find("[Desktop Action x]").unwrap();
        assert!(
            idx_key < idx_action,
            "key must be inside [Desktop Entry]: {out}"
        );
        let simple = set_desktop_key("[Desktop Entry]\nName=A", "Hidden", "true");
        assert_eq!(simple, "[Desktop Entry]\nName=A\nHidden=true\n");
    }
}

#[cfg(test)]
mod fs_tests {
    use super::*;

    #[test]
    fn add_toggle_remove_in_temp_profile() {
        let base = std::env::temp_dir().join(format!("vigil-test-{}", std::process::id()));
        let user = base.join("config");
        let system = base.join("xdg");
        std::fs::create_dir_all(system.join("autostart")).unwrap();
        std::fs::write(
            system.join("autostart/sys.desktop"),
            "[Desktop Entry]\nType=Application\nName=Sys Entry\nExec=/bin/true\n",
        )
        .unwrap();
        // SAFETY: this is the only test touching these variables.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &user);
            std::env::set_var("XDG_CONFIG_DIRS", &system);
            std::env::remove_var("XDG_CURRENT_DESKTOP");
        }

        add("My App", "/usr/bin/my-app --flag").unwrap();
        let mine = list_find("My App");
        assert!(mine.enabled && mine.removable);
        let sys = list_find("Sys Entry");
        assert!(sys.enabled && !sys.removable);

        // Disabling a system entry writes a user override; the system file stays untouched.
        set_enabled(&sys, false).unwrap();
        let sys2 = list_find("Sys Entry");
        assert!(!sys2.enabled);
        assert!(sys2.source.contains("override"));
        assert!(
            !std::fs::read_to_string(system.join("autostart/sys.desktop"))
                .unwrap()
                .contains("Hidden")
        );
        set_enabled(&sys2, true).unwrap();
        assert!(list_find("Sys Entry").enabled);

        remove(&list_find("My App")).unwrap();
        assert!(list().iter().all(|e| e.name != "My App"));
        assert!(remove(&list_find("Sys Entry")).is_err());
        assert!(add("", "x").is_err());
        assert!(add("a\nb", "x").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    fn list_find(name: &str) -> StartupEntry {
        list().into_iter().find(|e| e.name == name).unwrap()
    }
}

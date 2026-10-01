//! "Startup apps" on Windows: registry Run keys and Startup folders, enabled/disabled
//! through the same `StartupApproved` values Windows Task Manager uses.

use crate::model::StartupEntry;
use crate::platform::ActionError;
use std::path::PathBuf;
use winreg::enums::{KEY_READ, KEY_SET_VALUE, REG_BINARY};
use winreg::{HKCU, HKLM, RegKey, RegValue};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

#[derive(Clone, Copy)]
enum Hive {
    User,
    Machine,
}

impl Hive {
    fn key(self) -> &'static RegKey {
        match self {
            Hive::User => HKCU,
            Hive::Machine => HKLM,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Hive::User => "HKCU",
            Hive::Machine => "HKLM",
        }
    }
}

/// An approved value whose first byte is odd means "disabled".
fn approved(hive: Hive, sub: &str, name: &str) -> bool {
    hive.key()
        .open_subkey_with_flags(format!(r"{APPROVED}\{sub}"), KEY_READ)
        .and_then(|k| k.get_raw_value(name))
        .map(|v| v.bytes.first().is_none_or(|b| b % 2 == 0))
        .unwrap_or(true)
}

fn startup_folders() -> Vec<(PathBuf, Hive)> {
    let mut v = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        v.push((
            PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup"),
            Hive::User,
        ));
    }
    if let Some(pd) = std::env::var_os("ProgramData") {
        v.push((
            PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs\StartUp"),
            Hive::Machine,
        ));
    }
    v
}

pub fn list() -> Vec<StartupEntry> {
    let mut out = Vec::new();
    for (hive, path, sub) in [
        (Hive::User, RUN, "Run"),
        (Hive::Machine, RUN, "Run"),
        (Hive::Machine, RUN32, "Run32"),
    ] {
        let Ok(key) = hive.key().open_subkey_with_flags(path, KEY_READ) else {
            continue;
        };
        for (name, value) in key.enum_values().flatten() {
            let command = value.to_string().trim_matches('"').to_owned();
            out.push(StartupEntry {
                id: format!("reg:{}:{sub}:{name}", hive.tag()),
                name: name.clone(),
                command: value.to_string(),
                description: String::new(),
                source: format!(
                    "Registry ({})",
                    if matches!(hive, Hive::User) {
                        "current user"
                    } else {
                        "all users"
                    }
                ),
                enabled: approved(hive, sub, &name),
                removable: matches!(hive, Hive::User),
                location: command,
            });
        }
    }
    for (dir, hive) in startup_folders() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in read.flatten() {
            let file = ent.file_name().to_string_lossy().into_owned();
            if file.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            out.push(StartupEntry {
                id: format!("folder:{}:{file}", hive.tag()),
                name: file.trim_end_matches(".lnk").to_owned(),
                command: ent.path().to_string_lossy().into_owned(),
                description: String::new(),
                source: "Startup folder".into(),
                enabled: approved(hive, "StartupFolder", &file),
                removable: matches!(hive, Hive::User),
                location: ent.path().to_string_lossy().into_owned(),
            });
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

fn parse_id(id: &str) -> Option<(&str, Hive, &str, &str)> {
    let mut parts = id.splitn(4, ':');
    let kind = parts.next()?;
    let hive = match parts.next()? {
        "HKCU" => Hive::User,
        "HKLM" => Hive::Machine,
        _ => return None,
    };
    if kind == "folder" {
        let rest = id.splitn(3, ':').nth(2)?;
        return Some((kind, hive, "StartupFolder", rest));
    }
    Some((kind, hive, parts.next()?, parts.next()?))
}

fn io_err(e: std::io::Error) -> ActionError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        ActionError::PermissionDenied(
            "Changing entries for all users requires administrator rights.".into(),
        )
    } else {
        ActionError::Other(e.to_string())
    }
}

pub fn set_enabled(entry: &StartupEntry, enabled: bool) -> Result<(), ActionError> {
    let (_, hive, sub, name) =
        parse_id(&entry.id).ok_or_else(|| ActionError::Other("Unknown startup entry".into()))?;
    let (key, _) = hive
        .key()
        .create_subkey(format!(r"{APPROVED}\{sub}"))
        .map_err(io_err)?;
    let mut bytes = vec![0u8; 12];
    bytes[0] = if enabled { 2 } else { 3 };
    if !enabled {
        // Bytes 4..12 hold the FILETIME when the entry was disabled.
        let ft = (crate::util::now_unix() + 11_644_473_600) * 10_000_000;
        bytes[4..12].copy_from_slice(&ft.to_le_bytes());
    }
    key.set_raw_value(
        name,
        &RegValue {
            bytes: bytes.into(),
            vtype: REG_BINARY,
        },
    )
    .map_err(io_err)
}

pub fn add(name: &str, command: &str) -> Result<(), ActionError> {
    let (name, command) = (name.trim(), command.trim());
    if name.is_empty() || command.is_empty() {
        return Err(ActionError::Other("Name and command are required.".into()));
    }
    let key = HKCU
        .open_subkey_with_flags(RUN, KEY_SET_VALUE)
        .map_err(io_err)?;
    key.set_value(name, &command.to_owned()).map_err(io_err)
}

pub fn remove(entry: &StartupEntry) -> Result<(), ActionError> {
    if !entry.removable {
        return Err(ActionError::Other(
            "Only entries of the current user can be removed.".into(),
        ));
    }
    let (kind, _, _, name) =
        parse_id(&entry.id).ok_or_else(|| ActionError::Other("Unknown startup entry".into()))?;
    if kind == "folder" {
        std::fs::remove_file(&entry.location).map_err(io_err)
    } else {
        let key = HKCU
            .open_subkey_with_flags(RUN, KEY_SET_VALUE)
            .map_err(io_err)?;
        key.delete_value(name).map_err(io_err)
    }
}

pub fn last_boot_summary() -> Option<String> {
    None
}

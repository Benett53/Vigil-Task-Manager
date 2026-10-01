//! Minimal freedesktop.org `.desktop` file support.
//!
//! Used to give processes friendly application names (Task Manager's "Apps" group)
//! and for the XDG autostart based "Startup apps" page.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub exec: String,
    pub icon: String,
    pub wm_class: String,
    pub no_display: bool,
    pub hidden: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    /// `X-GNOME-Autostart-enabled=false` turns an autostart entry off.
    pub gnome_autostart_enabled: Option<bool>,
    pub is_application: bool,
}

impl DesktopEntry {
    /// Is the entry shown in the current desktop environment?
    pub fn shown_in(&self, desktops: &[String]) -> bool {
        if !self.only_show_in.is_empty()
            && !self
                .only_show_in
                .iter()
                .any(|d| desktops.iter().any(|c| c.eq_ignore_ascii_case(d)))
        {
            return false;
        }
        !self
            .not_show_in
            .iter()
            .any(|d| desktops.iter().any(|c| c.eq_ignore_ascii_case(d)))
    }

    /// Basename of the executable from the `Exec` key, skipping `env VAR=x` wrappers.
    pub fn exec_basename(&self) -> Option<String> {
        let mut words = split_exec(&self.exec).into_iter();
        let mut first = words.next()?;
        if Path::new(&first).file_name().is_some_and(|f| f == "env") {
            for w in words.by_ref() {
                if !w.contains('=') && !w.starts_with('-') {
                    first = w;
                    break;
                }
            }
        }
        if first.starts_with("flatpak") || first.contains('=') {
            return None;
        }
        Path::new(&first)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
    }
}

/// Parses the `[Desktop Entry]` group of a desktop file.
pub fn parse_desktop_entry(id: &str, content: &str) -> Option<DesktopEntry> {
    let mut entry = DesktopEntry {
        id: id.to_owned(),
        ..Default::default()
    };
    let mut in_group = false;
    let mut seen_group = false;
    let mut kind = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            seen_group |= in_group;
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "Name" => entry.name = unescape_value(value),
            "GenericName" => entry.generic_name = unescape_value(value),
            "Comment" => entry.comment = unescape_value(value),
            "Exec" => entry.exec = value.to_owned(),
            "Icon" => entry.icon = value.to_owned(),
            "StartupWMClass" => entry.wm_class = value.to_owned(),
            "NoDisplay" => entry.no_display = value.eq_ignore_ascii_case("true"),
            "Hidden" => entry.hidden = value.eq_ignore_ascii_case("true"),
            "OnlyShowIn" => entry.only_show_in = split_list(value),
            "NotShowIn" => entry.not_show_in = split_list(value),
            "X-GNOME-Autostart-enabled" => {
                entry.gnome_autostart_enabled = Some(!value.eq_ignore_ascii_case("false"))
            }
            "Type" => kind = value.to_owned(),
            _ => {}
        }
    }
    if !seen_group {
        return None;
    }
    entry.is_application = kind.is_empty() || kind == "Application";
    if entry.name.is_empty() {
        entry.name = id.trim_end_matches(".desktop").to_owned();
    }
    Some(entry)
}

fn split_list(v: &str) -> Vec<String> {
    v.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn unescape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits an `Exec` value into words honouring double quotes, dropping field codes (`%f`, `%U`, ...).
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_word = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_word = true;
            }
            '\\' if in_quotes => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !in_quotes => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            _ => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words.retain(|w| !(w.len() == 2 && w.starts_with('%')));
    words
}

/// The XDG data directories, most important first.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = data_home() {
        dirs.push(home.clone());
        dirs.push(home.join("flatpak/exports/share"));
    }
    let sys = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for d in sys.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d));
    }
    for extra in [
        "/var/lib/flatpak/exports/share",
        "/var/lib/snapd/desktop",
        "/usr/share",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".local/share")))
}

pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".config")))
}

pub fn config_dirs() -> Vec<PathBuf> {
    std::env::var("XDG_CONFIG_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/etc/xdg".to_owned())
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Names from `$XDG_CURRENT_DESKTOP` (e.g. `["ubuntu", "GNOME"]`).
pub fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Index of installed applications for fast lookups by id / executable / window class.
#[derive(Default)]
pub struct AppIndex {
    by_id: HashMap<String, Arc<DesktopEntry>>,
    by_exec: HashMap<String, Arc<DesktopEntry>>,
}

impl AppIndex {
    pub fn load() -> AppIndex {
        let mut index = AppIndex::default();
        for dir in data_dirs() {
            let apps = dir.join("applications");
            index.scan(&apps, &apps, 0);
        }
        index
    }

    fn scan(&mut self, root: &Path, dir: &Path, depth: u32) {
        if depth > 3 {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for ent in read.flatten() {
            let path = ent.path();
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_dir() {
                self.scan(root, &path, depth + 1);
                continue;
            }
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let id = rel.to_string_lossy().replace('/', "-");
            let id = id.trim_end_matches(".desktop").to_owned();
            if self.by_id.contains_key(&id) {
                continue; // earlier directories take precedence
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(entry) = parse_desktop_entry(&id, &content) else {
                continue;
            };
            if !entry.is_application || entry.hidden {
                continue;
            }
            let entry = Arc::new(entry);
            if !entry.no_display {
                if let Some(exe) = entry.exec_basename() {
                    self.by_exec
                        .entry(exe.to_lowercase())
                        .or_insert_with(|| entry.clone());
                }
                if !entry.wm_class.is_empty() {
                    self.by_exec
                        .entry(entry.wm_class.to_lowercase())
                        .or_insert_with(|| entry.clone());
                }
            }
            self.by_id.insert(id.to_lowercase(), entry);
        }
    }

    /// Looks up an application by desktop id (case insensitive; also tries the last
    /// reverse-DNS component, e.g. `org.gnome.Nautilus` -> `nautilus`).
    pub fn by_id(&self, id: &str) -> Option<&Arc<DesktopEntry>> {
        let lower = id.to_lowercase();
        if let Some(snap) = lower.strip_prefix("snap:") {
            return self
                .by_id
                .get(&format!("{snap}_{snap}"))
                .or_else(|| self.by_id.get(snap))
                .or_else(|| self.by_exec.get(snap));
        }
        self.by_id
            .get(&lower)
            .or_else(|| {
                lower
                    .rsplit('.')
                    .next()
                    .and_then(|last| self.by_id.get(last))
            })
            .or_else(|| self.by_exec.get(&lower))
    }

    /// Looks up a visible application by executable basename.
    pub fn by_exec(&self, exe: &str) -> Option<&Arc<DesktopEntry>> {
        self.by_exec.get(&exe.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_entry() {
        let content = "[Desktop Entry]\nType=Application\nName=Firefox Web Browser\nName[de]=Firefox-Webbrowser\nExec=env MOZ_X=1 /usr/lib/firefox/firefox %u\nIcon=firefox\nOnlyShowIn=GNOME;KDE;\n\n[Desktop Action new-window]\nName=New Window\nExec=firefox --new-window\n";
        let e = parse_desktop_entry("firefox", content).unwrap();
        assert_eq!(e.name, "Firefox Web Browser");
        assert_eq!(e.exec_basename().as_deref(), Some("firefox"));
        assert_eq!(e.only_show_in, vec!["GNOME", "KDE"]);
        assert!(e.shown_in(&["GNOME".to_owned()]));
        assert!(!e.shown_in(&["XFCE".to_owned()]));
        assert!(e.is_application);
    }

    #[test]
    fn exec_splitting() {
        assert_eq!(
            split_exec(r#""/opt/My App/app" --flag %F"#),
            vec!["/opt/My App/app".to_owned(), "--flag".to_owned()]
        );
        assert_eq!(split_exec("  a   b "), vec!["a", "b"]);
    }

    #[test]
    fn autostart_flags() {
        let e = parse_desktop_entry(
            "x",
            "[Desktop Entry]\nName=X\nExec=x\nHidden=true\nX-GNOME-Autostart-enabled=false\n",
        )
        .unwrap();
        assert!(e.hidden);
        assert_eq!(e.gnome_autostart_enabled, Some(false));
        assert!(parse_desktop_entry("y", "Name=Y\n").is_none());
    }
}

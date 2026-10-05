//! Export of the process list as CSV (opens in any spreadsheet application).

use crate::model::Process;
use std::io::Write;
use std::path::PathBuf;

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

pub fn to_csv(procs: &[Process]) -> String {
    let mut out = String::from(
        "name,pid,parent_pid,user,status,cpu_percent,memory_bytes,working_set_bytes,virtual_bytes,threads,nice,disk_read_bps,disk_write_bps,gpu_percent,started,executable,command_line\n",
    );
    for p in procs {
        let row = [
            csv_field(&p.name),
            p.pid.to_string(),
            p.ppid.to_string(),
            csv_field(&p.user),
            p.state.label().to_owned(),
            format!("{:.2}", p.cpu),
            p.mem.to_string(),
            p.rss.to_string(),
            p.virt.to_string(),
            p.threads.to_string(),
            p.nice.to_string(),
            p.disk_read.map(|v| format!("{v:.0}")).unwrap_or_default(),
            p.disk_write.map(|v| format!("{v:.0}")).unwrap_or_default(),
            p.gpu.map(|v| format!("{v:.2}")).unwrap_or_default(),
            crate::util::fmt_unix_time(p.start_time),
            csv_field(&p.exe),
            csv_field(&p.cmdline),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

/// Downloads folder if it exists, otherwise the home directory.
fn target_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(target_os = "linux")]
    if let Some(dir) = xdg_download_dir(&home) {
        return dir;
    }
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

/// Reads `XDG_DOWNLOAD_DIR` from `user-dirs.dirs` (localized folder names).
#[cfg(target_os = "linux")]
fn xdg_download_dir(home: &std::path::Path) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let content = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    let value = content
        .lines()
        .find_map(|l| l.strip_prefix("XDG_DOWNLOAD_DIR="))?
        .trim_matches('"')
        .replace("$HOME", &home.to_string_lossy());
    let dir = PathBuf::from(value);
    dir.is_dir().then_some(dir)
}

pub fn write_csv(procs: &[Process]) -> std::io::Result<PathBuf> {
    let stamp: String = crate::util::fmt_unix_time(crate::util::now_unix())
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let path = target_dir().join(format!("vigil-processes-{stamp}.csv"));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(to_csv(procs).as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::csv_field;

    #[test]
    fn escaping() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}

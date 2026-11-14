//! Window placement the toolkit cannot do on Wayland.
//!
//! Wayland has no protocol for "always on top", so winit ignores the request there.
//! KWin (KDE Plasma) can still keep a window above others: a small KWin script, loaded
//! over D-Bus, sets `keepAbove` on the windows of this process.

use std::process::{Command, Stdio};

/// Is this a KDE Plasma Wayland session (where [`kwin_keep_above`] works)?
pub fn kwin_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && super::desktop::current_desktops()
            .iter()
            .any(|d| d.eq_ignore_ascii_case("KDE"))
}

/// Asks KWin to keep the windows of this process above others (or not).
pub fn kwin_keep_above(on: bool) -> Result<(), String> {
    let dir = crate::update::private_temp_dir("vigil-kwin-").map_err(|e| e.to_string())?;
    let script = dir.join("keep-above.js");
    let result = (|| {
        std::fs::write(&script, keep_above_script(std::process::id(), on))
            .map_err(|e| e.to_string())?;
        let name = format!("vigil-keep-above-{}", std::process::id());
        let loaded = gdbus(&[
            "--object-path",
            "/Scripting",
            "--method",
            "org.kde.kwin.Scripting.loadScript",
            &script.to_string_lossy(),
            &name,
        ])?;
        // The reply looks like "(7,)".
        let id: String = loaded.chars().filter(char::is_ascii_digit).collect();
        if id.is_empty() {
            return Err(format!("KWin did not load the script: {loaded}"));
        }
        let run = gdbus(&[
            "--object-path",
            &format!("/Scripting/Script{id}"),
            "--method",
            "org.kde.kwin.Script.run",
        ]);
        let _ = gdbus(&[
            "--object-path",
            "/Scripting",
            "--method",
            "org.kde.kwin.Scripting.unloadScript",
            &name,
        ]);
        run.map(|_| ())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn keep_above_script(pid: u32, on: bool) -> String {
    // Plasma 6 has `windowList`, Plasma 5 `clientList`.
    format!(
        "const list = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (const w of list) {{ if (w.pid === {pid}) {{ w.keepAbove = {on}; }} }}\n"
    )
}

fn gdbus(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.kde.KWin"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("gdbus: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn script_only_contains_numbers_and_flags() {
        let s = super::keep_above_script(1234, true);
        assert!(s.contains("w.pid === 1234"));
        assert!(s.contains("w.keepAbove = true"));
    }
}

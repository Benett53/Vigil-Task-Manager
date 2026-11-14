//! Application icons from the freedesktop.org icon theme.
//!
//! An icon key is either `app:<desktop id>` (a process grouped into an application) or the
//! path of an executable. The desktop entry gives the icon name, which is looked up in the
//! user's icon theme, the themes it inherits and `hicolor`, then in `/usr/share/pixmaps`.
//! Runs on the icon thread only, so plain file system lookups are fine here.

use super::desktop::{self, AppIndex};
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Pixel size the icons are rendered at (shown at 16 and 28 points, with room for scaling).
const SIZE: u32 = 48;

struct Dir {
    path: String,
    size: u32,
    min: u32,
    max: u32,
    threshold: u32,
    kind: DirKind,
    scale: u32,
    apps: bool,
}

#[derive(PartialEq)]
enum DirKind {
    Fixed,
    Scalable,
    Threshold,
}

impl Dir {
    /// Distance to the wanted size (0 = usable as is), as in the icon theme spec.
    fn distance(&self, want: u32) -> u32 {
        let (min, max) = match self.kind {
            DirKind::Fixed => (self.size, self.size),
            DirKind::Scalable => (self.min, self.max),
            DirKind::Threshold => (
                self.size.saturating_sub(self.threshold),
                self.size + self.threshold,
            ),
        };
        let (min, max) = (min * self.scale, max * self.scale);
        if want < min {
            min - want
        } else {
            want.saturating_sub(max)
        }
    }
}

struct Theme {
    /// Directories ordered by how well they fit [`SIZE`].
    dirs: Vec<Dir>,
    inherits: Vec<String>,
}

pub struct Resolver {
    apps: AppIndex,
    /// Base directories that contain icon themes (`~/.icons`, `<data dir>/icons`).
    bases: Vec<PathBuf>,
    /// Theme chain: the user's theme, everything it inherits, then `hicolor`.
    themes: Vec<Theme>,
}

impl Resolver {
    pub fn new() -> Resolver {
        let mut bases = Vec::new();
        if let Some(home) = desktop::home_dir() {
            bases.push(home.join(".icons"));
        }
        bases.extend(desktop::data_dirs().into_iter().map(|d| d.join("icons")));
        bases.retain(|b| b.is_dir());

        let mut themes = Vec::new();
        let mut queue: Vec<String> = vec![current_theme()];
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = (!queue.is_empty()).then(|| queue.remove(0)) {
            if name == "hicolor" || !seen.insert(name.clone()) || themes.len() > 8 {
                continue;
            }
            if let Some(t) = load_theme(&bases, &name) {
                queue.extend(t.inherits.iter().cloned());
                themes.push(t);
            }
        }
        if let Some(t) = load_theme(&bases, "hicolor") {
            themes.push(t);
        }
        Resolver {
            apps: AppIndex::load(),
            bases,
            themes,
        }
    }

    /// RGBA pixels of the icon for `key`, see the module docs.
    pub fn icon(&self, key: &str) -> Option<(usize, usize, Vec<u8>)> {
        let path = self.find(key)?;
        load_image(&path)
    }

    fn find(&self, key: &str) -> Option<PathBuf> {
        if let Some(id) = key.strip_prefix("app:") {
            let entry = self.apps.by_id(id)?;
            return self.lookup(&entry.icon, false);
        }
        let base = Path::new(key).file_name()?.to_str()?;
        if let Some(entry) = self.apps.by_exec(base)
            && let Some(p) = self.lookup(&entry.icon, false)
        {
            return Some(p);
        }
        // No launcher: many themes still have an application icon named after the program.
        self.lookup(&base.to_lowercase(), true)
    }

    /// Path of the icon `name` (an icon name or an absolute path).
    fn lookup(&self, name: &str, apps_only: bool) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        if name.starts_with('/') {
            let p = PathBuf::from(name);
            return p.is_file().then_some(p);
        }
        // Some entries spell the file name, e.g. `Icon=foo.png`.
        let name = name
            .strip_suffix(".png")
            .or_else(|| name.strip_suffix(".svg"))
            .unwrap_or(name);
        if name.contains('/') {
            return None;
        }
        for theme in &self.themes {
            for dir in theme.dirs.iter().filter(|d| d.apps || !apps_only) {
                for base in &self.bases {
                    for ext in ["svg", "png"] {
                        let p = base.join(&dir.path).join(format!("{name}.{ext}"));
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }
            }
        }
        if apps_only {
            return None;
        }
        for base in desktop::data_dirs() {
            for ext in ["svg", "png"] {
                let p = base.join("pixmaps").join(format!("{name}.{ext}"));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
}

/// Name of the user's icon theme.
fn current_theme() -> String {
    let desktops = desktop::current_desktops();
    let kde = desktops.iter().any(|d| d.eq_ignore_ascii_case("KDE"));
    if kde
        && let Some(cfg) = desktop::config_home()
        && let Ok(text) = std::fs::read_to_string(cfg.join("kdeglobals"))
        && let Some(theme) = ini_value(&text, "Icons", "Theme")
    {
        return theme;
    }
    if !kde
        && let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .stderr(std::process::Stdio::null())
            .output()
        && out.status.success()
    {
        let theme = String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_matches('\'')
            .to_owned();
        if !theme.is_empty() {
            return theme;
        }
    }
    if let Some(cfg) = desktop::config_home()
        && let Ok(text) = std::fs::read_to_string(cfg.join("gtk-3.0/settings.ini"))
        && let Some(theme) = ini_value(&text, "Settings", "gtk-icon-theme-name")
    {
        return theme;
    }
    if kde { "breeze" } else { "Adwaita" }.to_owned()
}

/// Value of `key` in `[group]` of an ini style file.
fn ini_value(text: &str, group: &str, key: &str) -> Option<String> {
    let mut in_group = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == format!("[{group}]");
        } else if in_group
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            let v = v.trim();
            return (!v.is_empty()).then(|| v.to_owned());
        }
    }
    None
}

fn load_theme(bases: &[PathBuf], name: &str) -> Option<Theme> {
    let text = bases
        .iter()
        .find_map(|b| std::fs::read_to_string(b.join(name).join("index.theme")).ok())?;
    Some(parse_theme(name, &text))
}

fn parse_theme(name: &str, text: &str) -> Theme {
    let mut groups: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    let mut current = None;
    for line in text.lines().map(str::trim) {
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = Some(g);
            groups.entry(g).or_default();
        } else if let (Some(g), Some((k, v))) = (current, line.split_once('=')) {
            groups.entry(g).or_default().insert(k.trim(), v.trim());
        }
    }
    let head = groups.get("Icon Theme");
    let list = |key: &str| -> Vec<&str> {
        head.and_then(|h| h.get(key))
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let num = |g: &HashMap<&str, &str>, k: &str| g.get(k).and_then(|v| v.parse::<u32>().ok());
    let mut dirs: Vec<Dir> = list("Directories")
        .into_iter()
        .chain(list("ScaledDirectories"))
        .filter_map(|d| {
            let g = groups.get(d)?;
            let size = num(g, "Size")?;
            let kind = match g.get("Type").copied() {
                Some("Fixed") => DirKind::Fixed,
                Some("Scalable") => DirKind::Scalable,
                _ => DirKind::Threshold,
            };
            Some(Dir {
                path: format!("{name}/{d}"),
                size,
                min: num(g, "MinSize").unwrap_or(size),
                max: num(g, "MaxSize").unwrap_or(size),
                threshold: num(g, "Threshold").unwrap_or(2),
                kind,
                scale: num(g, "Scale").unwrap_or(1).max(1),
                apps: g.get("Context").is_some_and(|c| *c == "Applications"),
            })
        })
        .collect();
    // Best fit first; among equals prefer vector images, then larger bitmaps (scale down well).
    dirs.sort_by_key(|d| {
        (
            d.distance(SIZE),
            d.kind != DirKind::Scalable,
            u32::MAX - d.size * d.scale,
        )
    });
    Theme {
        dirs,
        inherits: list("Inherits").into_iter().map(str::to_owned).collect(),
    }
}

fn load_image(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let is_svg = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"));
    if is_svg {
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        let size = tree.size();
        let scale = SIZE as f32 / size.width().max(size.height());
        let dx = (SIZE as f32 - size.width() * scale) / 2.0;
        let dy = (SIZE as f32 - size.height() * scale) / 2.0;
        resvg::render(
            &tree,
            tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
            &mut pixmap.as_mut(),
        );
    } else {
        let src = tiny_skia::Pixmap::decode_png(&data).ok()?;
        let scale = SIZE as f32 / src.width().max(src.height()) as f32;
        let dx = (SIZE as f32 - src.width() as f32 * scale) / 2.0;
        let dy = (SIZE as f32 - src.height() as f32 * scale) / 2.0;
        pixmap.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bicubic,
                ..Default::default()
            },
            tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
            None,
        );
    }
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        return None;
    }
    Some((SIZE as usize, SIZE as usize, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = "[Icon Theme]\nName=T\nInherits=breeze,hicolor\n\
        Directories=16x16/apps,48x48/apps,scalable/apps,48x48/places\n\n\
        [16x16/apps]\nSize=16\nType=Fixed\nContext=Applications\n\n\
        [48x48/apps]\nSize=48\nType=Fixed\nContext=Applications\n\n\
        [scalable/apps]\nSize=128\nMinSize=8\nMaxSize=512\nType=Scalable\nContext=Applications\n\n\
        [48x48/places]\nSize=48\nType=Fixed\nContext=Places\n";

    #[test]
    fn theme_dirs_are_ordered_by_fit() {
        let t = parse_theme("T", THEME);
        assert_eq!(t.inherits, vec!["breeze", "hicolor"]);
        let order: Vec<&str> = t.dirs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(order[0], "T/scalable/apps");
        assert_eq!(order.last(), Some(&"T/16x16/apps"));
        assert!(
            !t.dirs
                .iter()
                .find(|d| d.path.ends_with("places"))
                .unwrap()
                .apps
        );
    }

    #[test]
    fn ini_values() {
        let text = "[General]\nTheme=x\n[Icons]\nTheme = Papirus-Dark\n";
        assert_eq!(
            ini_value(text, "Icons", "Theme").as_deref(),
            Some("Papirus-Dark")
        );
        assert_eq!(ini_value(text, "Icons", "Missing"), None);
    }

    /// `cargo test --release -- --ignored --nocapture icon_lookup`
    #[test]
    #[ignore]
    fn icon_lookup() {
        let t = std::time::Instant::now();
        let r = Resolver::new();
        println!("resolver: {:?}, theme {}", t.elapsed(), current_theme());
        for key in [
            "/usr/bin/konsole",
            "/usr/lib/firefox/firefox",
            "/usr/bin/dolphin",
            "/usr/bin/bash",
        ] {
            let t = std::time::Instant::now();
            let found = r.find(key);
            let img = r.icon(key).is_some();
            println!("{key}: {found:?} image={img} in {:?}", t.elapsed());
        }
    }
}

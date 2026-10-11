//! Visual style: system UI font, a quiet neutral palette in light and dark variants,
//! and the icon font.

use super::widgets::colors;
use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Shadow, Stroke, Theme, Visuals};

/// Font family for titles and emphasized numbers (falls back to the body font).
pub const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn install(ctx: &Context) {
    ctx.set_fonts(fonts());
    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.set_visuals_of(Theme::Light, light());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.window_margin = egui::Margin::same(16);
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.bar_inner_margin = 2.0;
        style.interaction.tooltip_delay = 0.4;
        // A task manager should feel instant.
        style.animation_time = 0.0;
        use egui::TextStyle;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        style.text_styles.insert(TextStyle::Heading, semibold(20.0));
    });
}

/// The platform UI font (Segoe UI on Windows, the fontconfig default elsewhere) in front of
/// egui's built-in fonts, plus the Phosphor icon font.
fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let (regular, semi) = system_fonts();
    let mut body = fallback.clone();
    if let Some(data) = regular {
        fonts
            .font_data
            .insert("system".into(), std::sync::Arc::new(data));
        body.insert(0, "system".into());
        fonts
            .families
            .insert(FontFamily::Proportional, body.clone());
    }
    let mut strong = body;
    if let Some(data) = semi {
        fonts
            .font_data
            .insert("system-semibold".into(), std::sync::Arc::new(data));
        strong.insert(0, "system-semibold".into());
    }
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), strong);
    fonts
}

#[cfg(windows)]
fn system_fonts() -> (Option<egui::FontData>, Option<egui::FontData>) {
    let dir = std::path::PathBuf::from(
        std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()),
    )
    .join("Fonts");
    let load = |name: &str| {
        std::fs::read(dir.join(name))
            .ok()
            .map(egui::FontData::from_owned)
    };
    (load("segoeui.ttf"), load("seguisb.ttf"))
}

#[cfg(not(windows))]
fn system_fonts() -> (Option<egui::FontData>, Option<egui::FontData>) {
    // Ask fontconfig for the desktop's sans-serif face; egui's own fonts remain the fallback.
    let lookup = |pattern: &str| -> Option<egui::FontData> {
        let out = std::process::Command::new("fc-match")
            .args(["-f", "%{file}\n%{index}", pattern])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let mut lines = text.lines();
        let file = lines.next()?.trim();
        let index = lines
            .next()
            .and_then(|i| i.trim().parse().ok())
            .unwrap_or(0);
        let lower = file.to_ascii_lowercase();
        if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
            return None;
        }
        let mut data = egui::FontData::from_owned(std::fs::read(file).ok()?);
        data.index = index;
        Some(data)
    };
    let regular = lookup("sans-serif:weight=regular");
    let semi = lookup("sans-serif:weight=semibold");
    (regular, semi)
}

fn common(v: &mut Visuals) {
    let r = CornerRadius::same(6);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = colors::ACCENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.hovered.expansion = 0.0;
    v.widgets.active.expansion = 0.0;
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(45),
    };
    v.striped = false;
    v.indent_has_left_vline = false;
}

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    common(&mut v);
    v.panel_fill = Color32::from_rgb(28, 29, 32);
    v.window_fill = Color32::from_rgb(34, 35, 39);
    v.extreme_bg_color = Color32::from_rgb(22, 23, 26);
    v.faint_bg_color = Color32::from_rgb(33, 34, 38);
    v.code_bg_color = Color32::from_rgb(40, 41, 46);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(46, 48, 53));
    v.selection.bg_fill = Color32::from_rgb(38, 66, 74);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(190, 240, 234));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(42, 44, 49));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(214));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(40, 41, 46);
    v.widgets.inactive.bg_fill = Color32::from_rgb(48, 50, 55);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_gray(222));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(50, 52, 58);
    v.widgets.hovered.bg_fill = Color32::from_rgb(58, 60, 66);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_gray(240));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(58, 60, 66);
    v.widgets.active.bg_fill = Color32::from_rgb(66, 68, 75);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(50, 52, 58);
    v.weak_text_color = Some(Color32::from_gray(140));
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    common(&mut v);
    v.panel_fill = Color32::from_rgb(250, 250, 251);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(244, 245, 247);
    v.code_bg_color = Color32::from_rgb(238, 240, 243);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(226, 228, 232));
    v.selection.bg_fill = Color32::from_rgb(214, 238, 235);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(8, 76, 72));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(232, 234, 237));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(36));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(240, 241, 244);
    v.widgets.inactive.bg_fill = Color32::from_rgb(232, 234, 238);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_gray(30));
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(232, 234, 238);
    v.widgets.hovered.bg_fill = Color32::from_rgb(224, 227, 232);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_gray(10));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(222, 225, 230);
    v.widgets.active.bg_fill = Color32::from_rgb(214, 218, 224);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(232, 234, 238);
    v.weak_text_color = Some(Color32::from_gray(112));
    v
}

/// Background of the navigation rail, slightly different from the content area.
pub fn nav_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(22, 23, 26)
    } else {
        Color32::from_rgb(242, 243, 245)
    }
}

/// Background for cards and panels that sit on the panel fill.
pub fn card_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(33, 34, 38)
    } else {
        Color32::WHITE
    }
}

pub fn card_stroke(dark: bool) -> Stroke {
    Stroke::new(
        1.0,
        if dark {
            Color32::from_rgb(42, 44, 49)
        } else {
            Color32::from_rgb(230, 232, 236)
        },
    )
}

/// Hairline used between rows and sections.
pub fn hairline(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(38, 40, 44)
    } else {
        Color32::from_rgb(236, 237, 240)
    }
}

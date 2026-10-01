//! Visual style: a calm, Windows 11-like look in light and dark variants.

use super::widgets::colors;
use egui::{Color32, Context, CornerRadius, Stroke, Theme, Visuals};

pub fn install(ctx: &Context) {
    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.set_visuals_of(Theme::Light, light());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.interaction.tooltip_delay = 0.4;
        // A task manager should feel instant.
        style.animation_time = 0.08;
    });
}

fn common(v: &mut Visuals) {
    let r = CornerRadius::same(5);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.hyperlink_color = colors::ACCENT;
}

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    common(&mut v);
    v.panel_fill = Color32::from_gray(32);
    v.window_fill = Color32::from_gray(40);
    v.extreme_bg_color = Color32::from_gray(22);
    v.faint_bg_color = Color32::from_gray(38);
    v.selection.bg_fill = Color32::from_rgb(28, 82, 96);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(150, 230, 225));
    v.widgets.inactive.weak_bg_fill = Color32::from_gray(48);
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(60);
    v.widgets.active.weak_bg_fill = Color32::from_gray(70);
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    common(&mut v);
    v.panel_fill = Color32::from_gray(246);
    v.window_fill = Color32::from_gray(252);
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_gray(238);
    v.selection.bg_fill = Color32::from_rgb(196, 232, 229);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(10, 80, 76));
    v.widgets.inactive.weak_bg_fill = Color32::from_gray(232);
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(222);
    v.widgets.active.weak_bg_fill = Color32::from_gray(210);
    v
}

/// Background of the navigation rail, slightly different from the content area.
pub fn nav_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_gray(26)
    } else {
        Color32::from_gray(236)
    }
}

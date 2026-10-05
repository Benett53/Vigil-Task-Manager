//! Visual style: a calm, modern look in light and dark variants, plus the icon font.

use super::widgets::colors;
use egui::{Color32, Context, CornerRadius, Shadow, Stroke, Theme, Visuals};

pub fn install(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.set_visuals_of(Theme::Light, light());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.window_margin = egui::Margin::same(14);
        style.spacing.scroll.bar_width = 8.0;
        style.interaction.tooltip_delay = 0.35;
        // A task manager should feel instant.
        style.animation_time = 0.08;
        use egui::{FontId, TextStyle};
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(13.5));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.5));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(20.0));
    });
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
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(70),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(55),
    };
    v.striped = true;
}

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    common(&mut v);
    v.panel_fill = Color32::from_rgb(30, 31, 34);
    v.window_fill = Color32::from_rgb(38, 39, 43);
    v.extreme_bg_color = Color32::from_rgb(22, 23, 25);
    v.faint_bg_color = Color32::from_rgb(35, 36, 40);
    v.code_bg_color = Color32::from_rgb(44, 45, 50);
    v.selection.bg_fill = Color32::from_rgb(24, 86, 96);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(160, 236, 230));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(52, 54, 60));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(46, 48, 53);
    v.widgets.inactive.bg_fill = Color32::from_rgb(46, 48, 53);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(58, 60, 66);
    v.widgets.hovered.bg_fill = Color32::from_rgb(58, 60, 66);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(68, 70, 77);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_gray(205));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_gray(200));
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    common(&mut v);
    v.panel_fill = Color32::from_rgb(248, 249, 251);
    v.window_fill = Color32::WHITE;
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(242, 244, 247);
    v.code_bg_color = Color32::from_rgb(236, 239, 243);
    v.selection.bg_fill = Color32::from_rgb(198, 236, 232);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(8, 76, 72));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(222, 226, 232));
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(234, 237, 241);
    v.widgets.inactive.bg_fill = Color32::from_rgb(234, 237, 241);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(224, 228, 234);
    v.widgets.hovered.bg_fill = Color32::from_rgb(224, 228, 234);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(212, 217, 224);
    v
}

/// Background of the navigation rail, slightly different from the content area.
pub fn nav_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(24, 25, 28)
    } else {
        Color32::from_rgb(237, 240, 244)
    }
}

/// Background for cards and panels that sit on the panel fill.
pub fn card_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(37, 38, 42)
    } else {
        Color32::WHITE
    }
}

pub fn card_stroke(dark: bool) -> Stroke {
    Stroke::new(
        1.0,
        if dark {
            Color32::from_rgb(50, 52, 58)
        } else {
            Color32::from_rgb(224, 228, 234)
        },
    )
}

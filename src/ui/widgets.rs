//! Custom painted widgets: graphs, sparklines, heat-map cells, meters, badges and cards.

use super::theme;
use crate::history::Series;
use egui::{
    Color32, CornerRadius, FontId, Mesh, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};

pub mod colors {
    use egui::Color32;
    pub const CPU: Color32 = Color32::from_rgb(56, 148, 230);
    pub const MEMORY: Color32 = Color32::from_rgb(168, 104, 230);
    pub const DISK: Color32 = Color32::from_rgb(92, 178, 70);
    pub const NETWORK: Color32 = Color32::from_rgb(232, 138, 48);
    pub const GPU: Color32 = Color32::from_rgb(24, 178, 164);
    pub const SENSORS: Color32 = Color32::from_rgb(226, 88, 96);
    pub const BATTERY: Color32 = Color32::from_rgb(110, 190, 90);
    pub const ACCENT: Color32 = Color32::from_rgb(22, 166, 156);
    pub const WARN: Color32 = Color32::from_rgb(230, 150, 40);
    pub const DANGER: Color32 = Color32::from_rgb(224, 72, 72);
    pub const GOOD: Color32 = Color32::from_rgb(70, 170, 100);
}

pub struct Line<'a> {
    pub series: &'a Series,
    pub color: Color32,
    pub fill: bool,
    pub dashed: bool,
}

fn graph_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(26, 27, 30)
    } else {
        Color32::from_rgb(252, 252, 253)
    }
}

/// Draws a Task Manager style graph. `max` is the value mapped to the top edge.
/// When `fmt` is given, hovering shows the value under the pointer.
#[allow(clippy::too_many_arguments)]
pub fn graph(
    ui: &mut Ui,
    size: Vec2,
    lines: &[Line<'_>],
    max: f32,
    capacity: usize,
    frame_color: Color32,
    fmt: Option<&dyn Fn(f32) -> String>,
    seconds_per_point: f32,
) -> egui::Response {
    let (response, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = response.rect;
    let dark = ui.visuals().dark_mode;
    let radius = CornerRadius::same(6);
    painter.rect_filled(rect, radius, graph_bg(dark));

    let grid = frame_color.gamma_multiply(if dark { 0.10 } else { 0.09 });
    for i in 1..4 {
        let y = (rect.top() + rect.height() * i as f32 / 4.0).round() + 0.5;
        painter.hline(rect.x_range(), y, Stroke::new(1.0, grid));
    }

    let max = if max.is_finite() && max > 0.0 {
        max
    } else {
        1.0
    };
    let capacity = capacity.max(2);
    let step = rect.width() / (capacity - 1) as f32;
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    for line in lines {
        let n = line.series.len();
        if n < 2 {
            continue;
        }
        let points: Vec<Pos2> = line
            .series
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = rect.right() - (n - 1 - i) as f32 * step;
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0);
                pos2(x, y)
            })
            .collect();
        if line.fill {
            clip.add(Shape::mesh(area_mesh(
                &points,
                rect.top(),
                rect.bottom(),
                line.color,
                dark,
            )));
        }
        if line.dashed {
            clip.extend(Shape::dashed_line(
                &points,
                Stroke::new(1.0, line.color.gamma_multiply(0.8)),
                4.0,
                3.0,
            ));
        } else {
            clip.add(Shape::line(points, Stroke::new(1.5, line.color)));
        }
    }

    // Hover: vertical marker with the values of all lines at that point in time.
    if let (Some(fmt), Some(pos)) = (fmt, response.hover_pos()) {
        let back = ((rect.right() - pos.x) / step).round().max(0.0) as usize;
        let x = rect.right() - back as f32 * step;
        let mut texts = Vec::new();
        for line in lines {
            let n = line.series.len();
            if back < n {
                let v = line.series.iter().nth(n - 1 - back).unwrap_or(0.0);
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0);
                clip.circle_filled(pos2(x, y), 3.5, line.color);
                texts.push(fmt(v));
            }
        }
        if !texts.is_empty() {
            clip.vline(
                x,
                rect.y_range(),
                Stroke::new(1.0, frame_color.gamma_multiply(0.6)),
            );
            let ago = back as f32 * seconds_per_point;
            let when = if back == 0 {
                "now".to_owned()
            } else {
                format!("{ago:.0} s ago")
            };
            response
                .clone()
                .on_hover_text_at_pointer(format!("{}  ·  {when}", texts.join("  /  ")));
        }
    }
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(
            1.0,
            frame_color.gamma_multiply(if dark { 0.35 } else { 0.4 }),
        ),
        StrokeKind::Inside,
    );
    response
}

/// Filled area under a line with a vertical gradient.
fn area_mesh(points: &[Pos2], top: f32, baseline: f32, color: Color32, dark: bool) -> Mesh {
    let mut mesh = Mesh::default();
    let h = (baseline - top).max(1.0);
    let strong = if dark { 0.30 } else { 0.22 };
    for p in points {
        let t = ((baseline - p.y) / h).clamp(0.0, 1.0);
        mesh.colored_vertex(*p, color.gamma_multiply(0.06 + strong * t));
        mesh.colored_vertex(pos2(p.x, baseline), color.gamma_multiply(0.02));
    }
    for i in 0..(points.len() as u32 - 1) {
        let a = i * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    mesh
}

/// Small sparkline used in the performance side bar and detail pane.
pub fn sparkline(
    ui: &mut Ui,
    size: Vec2,
    series: &Series,
    series2: Option<&Series>,
    max: f32,
    capacity: usize,
    color: Color32,
) {
    let (response, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = response.rect;
    let dark = ui.visuals().dark_mode;
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, color.gamma_multiply(0.25)),
    );
    let max = if max > 0.0 { max } else { 1.0 };
    let step = rect.width() / (capacity.max(2) - 1) as f32;
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    for (idx, s) in std::iter::once(series).chain(series2).enumerate() {
        let n = s.len();
        if n < 2 {
            continue;
        }
        let points: Vec<Pos2> = s
            .iter()
            .enumerate()
            .map(|(i, v)| {
                pos2(
                    rect.right() - (n - 1 - i) as f32 * step,
                    rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0),
                )
            })
            .collect();
        if idx == 0 {
            clip.add(Shape::mesh(area_mesh(
                &points,
                rect.top(),
                rect.bottom(),
                color,
                dark,
            )));
            clip.add(Shape::line(points, Stroke::new(1.2, color)));
        } else {
            clip.extend(Shape::dashed_line(
                &points,
                Stroke::new(1.0, color.gamma_multiply(0.8)),
                3.0,
                2.0,
            ));
        }
    }
}

/// Background tint for heat-map cells (CPU/Memory/... columns) like Windows Task Manager:
/// invisible when idle, a warm amber that strengthens with load.
pub fn heat_color(intensity: f32, dark: bool) -> Option<Color32> {
    // Small values stay untinted so the eye goes to what actually uses resources.
    if !intensity.is_finite() || intensity < 0.04 {
        return None;
    }
    let t = intensity.clamp(0.0, 1.0).sqrt();
    let alpha = if dark {
        0.05 + 0.40 * t
    } else {
        0.08 + 0.50 * t
    };
    Some(Color32::from_rgb(255, 184, 64).gamma_multiply(alpha))
}

pub fn paint_cell_bg(ui: &Ui, color: Option<Color32>) {
    if let Some(c) = color {
        let r = ui
            .max_rect()
            .expand2(vec2(ui.spacing().item_spacing.x * 0.5, 0.0));
        ui.painter().rect_filled(r, 0.0, c);
    }
}

/// Horizontal stacked bar (memory composition).
pub fn stacked_bar(
    ui: &mut Ui,
    height: f32,
    segments: &[(f64, Color32, String)],
    total: f64,
    frame: Color32,
) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let dark = ui.visuals().dark_mode;
    painter.rect_filled(rect, 6.0, graph_bg(dark));
    let mut x = rect.left();
    let mut hovered_label = None;
    for (value, color, label) in segments {
        if total <= 0.0 {
            break;
        }
        let w = (value / total) as f32 * rect.width();
        let seg = Rect::from_min_size(pos2(x, rect.top()), vec2(w.max(0.0), rect.height()));
        painter.rect_filled(seg, 0.0, *color);
        painter.vline(seg.right(), rect.y_range(), Stroke::new(1.0, frame));
        if response.hovered()
            && ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|p| seg.contains(p))
        {
            hovered_label = Some(label.clone());
        }
        x += w;
    }
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0, frame), StrokeKind::Inside);
    if let Some(l) = hovered_label {
        response.on_hover_text(l);
    }
}

/// Small rounded label with a tinted background.
pub fn pill(ui: &mut Ui, text: impl Into<String>, color: Color32) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let text = text.into();
    let galley = ui.painter().layout_no_wrap(
        text,
        FontId::proportional(11.5),
        if dark {
            color.gamma_multiply(1.15)
        } else {
            color
        },
    );
    let size = galley.size() + vec2(12.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(
        rect,
        rect.height() / 2.0,
        color.gamma_multiply(if dark { 0.22 } else { 0.14 }),
    );
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        Color32::PLACEHOLDER,
    );
    resp
}

/// Severity color of a temperature in °C.
pub fn temp_color(t: f32, ui: &Ui) -> Color32 {
    if t >= 85.0 {
        colors::DANGER
    } else if t >= 70.0 {
        colors::WARN
    } else {
        ui.visuals().text_color()
    }
}

pub fn fmt_temp(t: f32) -> String {
    format!("{t:.0} °C")
}

/// Thin horizontal usage bar (0..=1).
pub fn meter(ui: &mut Ui, width: f32, fraction: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 5.0), Sense::hover());
    let dark = ui.visuals().dark_mode;
    ui.painter().rect_filled(
        rect,
        2.5,
        if dark {
            Color32::from_rgb(52, 54, 60)
        } else {
            Color32::from_rgb(226, 230, 236)
        },
    );
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    if fill.width() > 0.5 {
        ui.painter().rect_filled(fill, 2.5, color);
    }
}

/// A rounded card frame.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(theme::card_fill(dark))
        .stroke(theme::card_stroke(dark))
        .corner_radius(10.0)
        .inner_margin(12.0)
        .show(ui, add)
}

/// Value text that is dimmed when it is zero (reduces visual noise in tables).
pub fn dim_if_zero(ui: &Ui, text: String, is_zero: bool) -> RichText {
    if is_zero {
        RichText::new(text).color(ui.visuals().weak_text_color().gamma_multiply(0.8))
    } else {
        RichText::new(text)
    }
}

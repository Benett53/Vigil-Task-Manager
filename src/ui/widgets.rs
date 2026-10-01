//! Custom painted widgets: performance graphs, sparklines, heat-map cells and bars.

use crate::history::Series;
use egui::{Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

pub mod colors {
    use egui::Color32;
    pub const CPU: Color32 = Color32::from_rgb(38, 139, 210);
    pub const MEMORY: Color32 = Color32::from_rgb(160, 90, 220);
    pub const DISK: Color32 = Color32::from_rgb(86, 170, 60);
    pub const NETWORK: Color32 = Color32::from_rgb(222, 128, 40);
    pub const GPU: Color32 = Color32::from_rgb(20, 168, 156);
    pub const ACCENT: Color32 = Color32::from_rgb(20, 160, 150);
}

pub struct Line<'a> {
    pub series: &'a Series,
    pub color: Color32,
    pub fill: bool,
    pub dashed: bool,
}

/// Draws a Task Manager style graph. `max` is the value mapped to the top edge.
pub fn graph(
    ui: &mut Ui,
    size: Vec2,
    lines: &[Line<'_>],
    max: f32,
    capacity: usize,
    frame_color: Color32,
) -> egui::Response {
    let (response, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = response.rect;
    let dark = ui.visuals().dark_mode;
    let bg = if dark {
        Color32::from_gray(24)
    } else {
        Color32::WHITE
    };
    painter.rect_filled(rect, 0.0, bg);

    // Grid: 10 rows, columns every 10% of the width.
    let grid = frame_color.gamma_multiply(if dark { 0.22 } else { 0.18 });
    for i in 1..10 {
        let y = rect.top() + rect.height() * i as f32 / 10.0;
        painter.hline(rect.x_range(), y, Stroke::new(1.0, grid));
    }
    let cols = 10;
    for i in 1..cols {
        let x = rect.left() + rect.width() * i as f32 / cols as f32;
        painter.vline(x, rect.y_range(), Stroke::new(1.0, grid));
    }

    let max = if max.is_finite() && max > 0.0 {
        max
    } else {
        1.0
    };
    let capacity = capacity.max(2);
    let step = rect.width() / (capacity - 1) as f32;
    for line in lines {
        let n = line.series.len();
        if n == 0 {
            continue;
        }
        let points: Vec<Pos2> = line
            .series
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = rect.right() - (n - 1 - i) as f32 * step;
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * rect.height();
                pos2(x, y)
            })
            .collect();
        if line.fill && points.len() >= 2 {
            painter.add(Shape::mesh(area_mesh(
                &points,
                rect.bottom(),
                line.color.gamma_multiply(0.25),
            )));
        }
        if points.len() >= 2 {
            if line.dashed {
                painter.extend(Shape::dashed_line(
                    &points,
                    Stroke::new(1.0, line.color),
                    4.0,
                    3.0,
                ));
            } else {
                painter.add(Shape::line(points, Stroke::new(1.5, line.color)));
            }
        }
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, frame_color), StrokeKind::Inside);
    response
}

fn area_mesh(points: &[Pos2], baseline: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    for p in points {
        mesh.colored_vertex(*p, color);
        mesh.colored_vertex(pos2(p.x, baseline), color);
    }
    for i in 0..(points.len() as u32 - 1) {
        let a = i * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    mesh
}

/// Small sparkline used in the performance side bar.
pub fn sparkline(
    ui: &mut Ui,
    size: Vec2,
    series: &Series,
    series2: Option<&Series>,
    max: f32,
    capacity: usize,
    color: Color32,
) {
    let mut lines = vec![Line {
        series,
        color,
        fill: true,
        dashed: false,
    }];
    if let Some(s2) = series2 {
        lines.push(Line {
            series: s2,
            color,
            fill: false,
            dashed: true,
        });
    }
    let (response, painter) = ui.allocate_painter(size, Sense::hover());
    let rect = response.rect;
    let dark = ui.visuals().dark_mode;
    painter.rect_filled(
        rect,
        0.0,
        if dark {
            Color32::from_gray(24)
        } else {
            Color32::WHITE
        },
    );
    let max = if max > 0.0 { max } else { 1.0 };
    let step = rect.width() / (capacity.max(2) - 1) as f32;
    for line in &lines {
        let n = line.series.len();
        if n < 2 {
            continue;
        }
        let points: Vec<Pos2> = line
            .series
            .iter()
            .enumerate()
            .map(|(i, v)| {
                pos2(
                    rect.right() - (n - 1 - i) as f32 * step,
                    rect.bottom() - (v / max).clamp(0.0, 1.0) * rect.height(),
                )
            })
            .collect();
        if line.fill {
            painter.add(Shape::mesh(area_mesh(
                &points,
                rect.bottom(),
                line.color.gamma_multiply(0.25),
            )));
        }
        if line.dashed {
            painter.extend(Shape::dashed_line(
                &points,
                Stroke::new(1.0, line.color),
                3.0,
                2.0,
            ));
        } else {
            painter.add(Shape::line(points, Stroke::new(1.0, line.color)));
        }
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
}

/// Background color for heat-map cells (CPU/Memory/... columns) like Windows Task Manager.
pub fn heat_color(intensity: f32, dark: bool) -> Option<Color32> {
    if !intensity.is_finite() || intensity <= 0.001 {
        return None;
    }
    let t = intensity.clamp(0.0, 1.0).sqrt();
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Some(if dark {
        Color32::from_rgb(lerp(58, 150), lerp(50, 92), lerp(22, 10))
    } else {
        Color32::from_rgb(lerp(255, 255), lerp(248, 186), lerp(220, 70))
    })
}

pub fn paint_cell_bg(ui: &Ui, color: Option<Color32>) {
    if let Some(c) = color {
        let r = ui
            .max_rect()
            .expand2(vec2(ui.spacing().item_spacing.x * 0.5, 1.0));
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
    painter.rect_filled(
        rect,
        0.0,
        if dark {
            Color32::from_gray(24)
        } else {
            Color32::WHITE
        },
    );
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
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, frame), StrokeKind::Inside);
    if let Some(l) = hovered_label {
        response.on_hover_text(l);
    }
}

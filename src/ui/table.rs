//! Shared helpers for the sortable tables.

use egui::{Align, Layout, RichText, Ui};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Col {
    Name,
    Pid,
    Status,
    User,
    Cpu,
    Memory,
    Disk,
    Gpu,
    GpuMemory,
    Threads,
    Priority,
    Rss,
    Virt,
    Command,
    CpuTime,
    Start,
    // Startup / services / history specific
    Source,
    Description,
    Startup,
    Group,
    DiskRead,
    DiskWrite,
    GpuTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sort {
    pub col: Col,
    pub asc: bool,
}

impl Sort {
    pub fn new(col: Col, asc: bool) -> Sort {
        Sort { col, asc }
    }

    pub fn apply(&self, ord: Ordering) -> Ordering {
        if self.asc { ord } else { ord.reverse() }
    }
}

/// A clickable column header. `summary` is shown above the title (e.g. "37%").
pub fn header(
    ui: &mut Ui,
    title: &str,
    summary: Option<&str>,
    col: Col,
    sort: &mut Sort,
    numeric: bool,
) {
    let arrow = if sort.col == col {
        if sort.asc { " ⏶" } else { " ⏷" }
    } else {
        ""
    };
    // Bottom-up so every title sits on the same baseline, with the summary above it.
    let layout = if numeric {
        Layout::bottom_up(Align::Max)
    } else {
        Layout::bottom_up(Align::Min)
    };
    let resp = ui
        .with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let title = ui.label(RichText::new(format!("{title}{arrow}")).small().weak());
            if let Some(s) = summary {
                ui.label(
                    RichText::new(s)
                        .font(super::theme::semibold(15.0))
                        .color(ui.visuals().strong_text_color()),
                );
            }
            title
        })
        .response;
    let resp = ui.interact(
        resp.rect.union(ui.max_rect()),
        ui.id().with(("hdr", title)),
        egui::Sense::click(),
    );
    if resp.clicked() {
        if sort.col == col {
            sort.asc = !sort.asc;
        } else {
            sort.col = col;
            // Numbers are most useful largest-first, text alphabetically.
            sort.asc = !numeric;
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("Sort by {title}"));
}

pub fn right_label(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add(egui::Label::new(text).truncate());
    });
}

pub fn cell_label(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

pub fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

pub fn cmp_str(a: &str, b: &str) -> Ordering {
    // Case-insensitive without allocation for the common ASCII case.
    let ai = a.bytes().map(|c| c.to_ascii_lowercase());
    let bi = b.bytes().map(|c| c.to_ascii_lowercase());
    ai.cmp(bi)
}

/// Hides the column dividers egui_extras draws through the whole table body. They still
/// appear while a divider is hovered or dragged.
pub fn quiet_lines(ui: &mut Ui) {
    ui.visuals_mut().widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
}

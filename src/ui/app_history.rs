//! "App history": resource usage accumulated per application since a date.

use super::table::{Col, Sort, cell_label, cmp_f64, cmp_str, header, right_label};
use super::{Action, View};
use crate::history::AppUsage;
use crate::util::{contains_ci, fmt_bytes, fmt_cpu_time, fmt_unix_time};
use egui::{Align, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};

pub struct State {
    pub sort: Sort,
}

impl Default for State {
    fn default() -> Self {
        State {
            sort: Sort::new(Col::CpuTime, false),
        }
    }
}

pub fn show(
    ui: &mut Ui,
    state: &mut State,
    view: &View<'_>,
    all_processes: &mut bool,
    actions: &mut Vec<Action>,
) {
    let hist = view.app_history;
    ui.horizontal(|ui| {
        ui.label(format!(
            "Resource usage since {}",
            fmt_unix_time(hist.since)
        ));
        ui.separator();
        ui.checkbox(all_processes, "Show history for all processes");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Delete usage history").clicked() {
                actions.push(Action::ResetAppHistory);
            }
        });
    });
    ui.add_space(4.0);
    let mut rows: Vec<&AppUsage> = hist
        .apps
        .values()
        .filter(|u| *all_processes || u.is_app)
        .filter(|u| view.search.is_empty() || contains_ci(&u.name, view.search))
        .collect();
    let sort = state.sort;
    rows.sort_by(|a, b| {
        let ord = match sort.col {
            Col::Name => cmp_str(&a.name, &b.name),
            Col::CpuTime => cmp_f64(a.cpu_secs, b.cpu_secs),
            Col::DiskRead => a.disk_read.cmp(&b.disk_read),
            Col::DiskWrite => a.disk_write.cmp(&b.disk_write),
            Col::GpuTime => cmp_f64(a.gpu_secs, b.gpu_secs),
            Col::Start => a.last_seen.cmp(&b.last_seen),
            _ => std::cmp::Ordering::Equal,
        };
        sort.apply(ord.then_with(|| cmp_str(&a.name, &b.name)))
    });
    if rows.is_empty() {
        ui.label(
            RichText::new(if *all_processes {
                "No usage recorded yet."
            } else {
                "No application usage recorded yet. Enable \"Show history for all processes\" to include background processes."
            })
            .weak(),
        );
        return;
    }
    let mut sort_state = state.sort;
    TableBuilder::new(ui)
        .id_salt("app_history")
        .striped(true)
        .resizable(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .auto_shrink([false, false])
        .column(Column::initial(280.0).at_least(120.0).clip(true))
        .column(Column::initial(100.0))
        .column(Column::initial(110.0))
        .column(Column::initial(110.0))
        .column(Column::initial(100.0))
        .column(Column::remainder())
        .header(24.0, |mut h| {
            h.col(|ui| header(ui, "Name", None, Col::Name, &mut sort_state, false));
            h.col(|ui| header(ui, "CPU time", None, Col::CpuTime, &mut sort_state, true));
            h.col(|ui| header(ui, "Disk read", None, Col::DiskRead, &mut sort_state, true));
            h.col(|ui| {
                header(
                    ui,
                    "Disk written",
                    None,
                    Col::DiskWrite,
                    &mut sort_state,
                    true,
                )
            });
            h.col(|ui| header(ui, "GPU time", None, Col::GpuTime, &mut sort_state, true));
            h.col(|ui| header(ui, "Last active", None, Col::Start, &mut sort_state, false));
        })
        .body(|body| {
            body.rows(22.0, rows.len(), |mut row| {
                let u = rows[row.index()];
                row.col(|ui| {
                    ui.add_space(4.0);
                    cell_label(ui, &u.name);
                });
                row.col(|ui| right_label(ui, fmt_cpu_time(u.cpu_secs)));
                row.col(|ui| right_label(ui, fmt_bytes(u.disk_read)));
                row.col(|ui| right_label(ui, fmt_bytes(u.disk_write)));
                row.col(|ui| right_label(ui, fmt_cpu_time(u.gpu_secs)));
                row.col(|ui| cell_label(ui, RichText::new(fmt_unix_time(u.last_seen)).weak()));
            });
        });
    state.sort = sort_state;
}

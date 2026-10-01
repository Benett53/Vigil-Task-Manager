//! "Details" page: a flat, sortable list of every process with technical columns.

use super::proc_menu::process_menu;
use super::processes::{col_title, column_spec, is_numeric, status_text, visible_columns};
use super::table::{Col, Sort, cell_label, cmp_f64, cmp_str, header, right_label};
use super::{Action, View};
use crate::model::{Priority, ProcSignal, Process};
use crate::util::{contains_ci, fmt_bytes, fmt_cpu_time, fmt_rate, fmt_unix_time};
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

pub struct State {
    pub sort: Sort,
    pub selected: Option<u32>,
    pub scroll_to: Option<u32>,
    pending_move: i32,
    cache: Option<(u64, Sort, String, bool, Vec<usize>)>,
}

impl Default for State {
    fn default() -> Self {
        State {
            sort: Sort::new(Col::Name, true),
            selected: None,
            scroll_to: None,
            pending_move: 0,
            cache: None,
        }
    }
}

fn matches(p: &Process, search: &str) -> bool {
    search.is_empty()
        || contains_ci(&p.name, search)
        || contains_ci(&p.cmdline, search)
        || contains_ci(&p.user, search)
        || p.pid.to_string().starts_with(search)
}

fn cmp(a: &Process, b: &Process, sort: Sort) -> std::cmp::Ordering {
    let disk = |p: &Process| p.disk_read.unwrap_or(-1.0) + p.disk_write.unwrap_or(0.0);
    let ord = match sort.col {
        Col::Name => cmp_str(&a.name, &b.name),
        Col::Pid => a.pid.cmp(&b.pid),
        Col::Status => status_text(a)
            .cmp(status_text(b))
            .then(a.state.label().cmp(b.state.label())),
        Col::User => cmp_str(&a.user, &b.user),
        Col::Cpu => cmp_f64(a.cpu as f64, b.cpu as f64),
        Col::CpuTime => cmp_f64(a.cpu_time, b.cpu_time),
        Col::Memory => a.mem.cmp(&b.mem),
        Col::Disk => cmp_f64(disk(a), disk(b)),
        Col::Gpu => cmp_f64(a.gpu.unwrap_or(-1.0) as f64, b.gpu.unwrap_or(-1.0) as f64),
        Col::GpuMemory => a.gpu_mem.cmp(&b.gpu_mem),
        Col::Threads => a.threads.cmp(&b.threads),
        Col::Priority => b.nice.cmp(&a.nice),
        Col::Rss => a.rss.cmp(&b.rss),
        Col::Virt => a.virt.cmp(&b.virt),
        Col::Start => a.start_time.cmp(&b.start_time),
        Col::Command => cmp_str(&a.cmdline, &b.cmdline),
        _ => std::cmp::Ordering::Equal,
    };
    sort.apply(ord.then_with(|| a.pid.cmp(&b.pid)))
}

impl State {
    pub fn end_task_action(&self, view: &View<'_>) -> Option<Action> {
        let pid = self.selected?;
        let p = view.snap.procs.iter().find(|p| p.pid == pid)?;
        Some(Action::Signal {
            pids: vec![pid],
            sig: ProcSignal::Terminate,
            target: format!("{} (PID {})", p.name, p.pid),
        })
    }

    pub fn move_selection(&mut self, delta: i32) {
        self.pending_move = delta;
    }
}

pub fn show(ui: &mut Ui, state: &mut State, view: &View<'_>, actions: &mut Vec<Action>) {
    let snap = view.snap;
    let kernel = view.settings.show_kernel_threads;
    let stale = !matches!(&state.cache, Some((seq, sort, search, k, _)) if *seq == snap.seq && *sort == state.sort && search == view.search && *k == kernel);
    if stale {
        let mut idx: Vec<usize> = (0..snap.procs.len())
            .filter(|&i| {
                (kernel || !snap.procs[i].kernel_thread) && matches(&snap.procs[i], view.search)
            })
            .collect();
        let sort = state.sort;
        idx.sort_by(|&a, &b| cmp(&snap.procs[a], &snap.procs[b], sort));
        state.cache = Some((snap.seq, state.sort, view.search.to_owned(), kernel, idx));
    }
    let cache = state.cache.take().expect("built above");
    let rows = &cache.4;

    if state.pending_move != 0 {
        let cur = state
            .selected
            .and_then(|pid| rows.iter().position(|&i| snap.procs[i].pid == pid));
        let next = match cur {
            Some(i) => {
                (i as i32 + state.pending_move).clamp(0, rows.len().saturating_sub(1) as i32)
                    as usize
            }
            None => 0,
        };
        if let Some(&i) = rows.get(next) {
            state.selected = Some(snap.procs[i].pid);
            state.scroll_to = state.selected;
        }
        state.pending_move = 0;
    }

    let mut cols = visible_columns(&view.settings.details_columns);
    if let Some(pos) = cols.iter().position(|c| *c == Col::Cpu) {
        cols.insert(pos + 1, Col::CpuTime);
    }
    let ncpu = snap.info.logical.max(1) as f32;
    let cpu_scale = if view.settings.cpu_per_core_scale {
        ncpu
    } else {
        1.0
    };

    let mut sort = state.sort;
    let mut new_sel = None;
    egui::ScrollArea::horizontal()
        .id_salt("details_h")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .id_salt("details")
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false]);
            if let Some(pid) = state.scroll_to.take()
                && let Some(row) = rows.iter().position(|&i| snap.procs[i].pid == pid)
            {
                tb = tb.scroll_to_row(row, Some(Align::Center));
            }
            for c in &cols {
                tb = tb.column(column_spec(*c));
            }
            if !cols.contains(&Col::Command) {
                tb = tb.column(Column::remainder().at_least(0.0));
            }
            tb.header(24.0, |mut h| {
                for c in &cols {
                    h.col(|ui| header(ui, col_title(*c), None, *c, &mut sort, is_numeric(*c)));
                }
                if !cols.contains(&Col::Command) {
                    h.col(|_| {});
                }
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let p = &snap.procs[rows[row.index()]];
                    row.set_selected(state.selected == Some(p.pid));
                    for c in &cols {
                        row.col(|ui| match c {
                            Col::Name => {
                                ui.add_space(4.0);
                                cell_label(ui, &*p.name);
                            }
                            Col::Pid => right_label(ui, p.pid.to_string()),
                            Col::Status => {
                                let s = status_text(p);
                                cell_label(ui, if s.is_empty() { p.state.label() } else { s });
                            }
                            Col::User => cell_label(ui, &*p.user),
                            Col::Cpu => right_label(ui, format!("{:.1}%", p.cpu * cpu_scale)),
                            Col::CpuTime => right_label(ui, fmt_cpu_time(p.cpu_time)),
                            Col::Memory => right_label(ui, fmt_bytes(p.mem)),
                            Col::Disk => right_label(
                                ui,
                                match (p.disk_read, p.disk_write) {
                                    (None, None) => "—".into(),
                                    (r, w) => fmt_rate(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
                                },
                            ),
                            Col::Gpu => right_label(
                                ui,
                                p.gpu.map(|g| format!("{g:.1}%")).unwrap_or_default(),
                            ),
                            Col::GpuMemory => {
                                right_label(ui, p.gpu_mem.map(fmt_bytes).unwrap_or_default())
                            }
                            Col::Threads => right_label(ui, p.threads.to_string()),
                            Col::Priority => cell_label(
                                ui,
                                format!(
                                    "{} ({})",
                                    Priority::from_nice(p.nice).short_label(),
                                    p.nice
                                ),
                            ),
                            Col::Rss => right_label(ui, fmt_bytes(p.rss)),
                            Col::Virt => right_label(ui, fmt_bytes(p.virt)),
                            Col::Start => cell_label(ui, fmt_unix_time(p.start_time)),
                            Col::Command => cell_label(ui, RichText::new(&*p.cmdline).weak()),
                            _ => {}
                        });
                    }
                    if !cols.contains(&Col::Command) {
                        row.col(|_| {});
                    }
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        new_sel = Some(p.pid);
                    }
                    if resp.double_clicked() {
                        actions.push(Action::OpenProperties(p.pid, p.name.to_string()));
                    }
                    resp.context_menu(|ui| process_menu(ui, p, snap, actions, false));
                });
            });
        });
    state.cache = Some(cache);
    state.sort = sort;
    if new_sel.is_some() {
        state.selected = new_sel;
    }
}

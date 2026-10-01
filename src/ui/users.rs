//! "Users" page: resource usage per user account.

use super::proc_menu::process_menu;
use super::table::{Col, Sort, cell_label, cmp_f64, cmp_str, header, right_label};
use super::widgets::{heat_color, paint_cell_bg};
use super::{Action, View};
use crate::model::{ProcKind, ProcSignal};
use crate::util::{fmt_bytes, fmt_percent, fmt_rate};
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeMap, HashSet};

pub struct State {
    pub sort: Sort,
    pub expanded: HashSet<u32>,
    pub selected: Option<u32>,
    full_names: BTreeMap<u32, Option<String>>,
}

impl Default for State {
    fn default() -> Self {
        State {
            sort: Sort::new(Col::Cpu, false),
            expanded: HashSet::new(),
            selected: None,
            full_names: BTreeMap::new(),
        }
    }
}

struct UserAgg {
    uid: u32,
    name: String,
    procs: Vec<usize>,
    cpu: f32,
    mem: u64,
    disk: f64,
    gpu: f32,
    has_session: bool,
}

pub fn show(ui: &mut Ui, state: &mut State, view: &View<'_>, actions: &mut Vec<Action>) {
    let snap = view.snap;
    let mut users: BTreeMap<u32, UserAgg> = BTreeMap::new();
    for (i, p) in snap.procs.iter().enumerate() {
        if p.kernel_thread && !view.settings.show_kernel_threads {
            continue;
        }
        let u = users.entry(p.uid).or_insert_with(|| UserAgg {
            uid: p.uid,
            name: p.user.to_string(),
            procs: Vec::new(),
            cpu: 0.0,
            mem: 0,
            disk: 0.0,
            gpu: 0.0,
            has_session: false,
        });
        u.procs.push(i);
        u.cpu += p.cpu;
        u.mem += p.mem;
        u.disk += p.disk_read.unwrap_or(0.0) + p.disk_write.unwrap_or(0.0);
        u.gpu += p.gpu.unwrap_or(0.0);
        u.has_session |= p.kind == ProcKind::App;
    }
    if view.settings.show_full_account_name {
        for uid in users.keys() {
            state
                .full_names
                .entry(*uid)
                .or_insert_with(|| crate::platform::user_full_name(*uid, ""));
        }
    }
    let mut list: Vec<UserAgg> = users.into_values().collect();
    let sort = state.sort;
    list.sort_by(|a, b| {
        let ord = match sort.col {
            Col::User => cmp_str(&a.name, &b.name),
            Col::Cpu => cmp_f64(a.cpu as f64, b.cpu as f64),
            Col::Memory => a.mem.cmp(&b.mem),
            Col::Disk => cmp_f64(a.disk, b.disk),
            Col::Gpu => cmp_f64(a.gpu as f64, b.gpu as f64),
            Col::Threads => a.procs.len().cmp(&b.procs.len()),
            _ => std::cmp::Ordering::Equal,
        };
        sort.apply(ord.then_with(|| a.uid.cmp(&b.uid)))
    });
    for u in &mut list {
        u.procs
            .sort_by(|&x, &y| cmp_f64(snap.procs[y].cpu as f64, snap.procs[x].cpu as f64));
    }

    enum Row {
        User(usize),
        Proc(usize),
    }
    let mut rows = Vec::new();
    for (ui_idx, u) in list.iter().enumerate() {
        if !view.search.is_empty()
            && !crate::util::contains_ci(&u.name, view.search)
            && !u
                .procs
                .iter()
                .any(|&i| crate::util::contains_ci(&snap.procs[i].name, view.search))
        {
            continue;
        }
        rows.push(Row::User(ui_idx));
        if state.expanded.contains(&u.uid) {
            rows.extend(u.procs.iter().map(|&i| Row::Proc(i)));
        }
    }

    let mem_total = snap.mem.total.max(1) as f32;
    let dark = ui.visuals().dark_mode;
    let heat = view.settings.heatmap;
    let mut sort_state = state.sort;
    let mut toggle = None;
    let mut new_sel = None;
    let current_uid = snap.info.current_uid;

    egui::ScrollArea::horizontal()
        .id_salt("users_h")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("users")
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::initial(300.0).at_least(150.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .column(Column::remainder())
                .header(38.0, |mut h| {
                    h.col(|ui| header(ui, "User", None, Col::User, &mut sort_state, false));
                    h.col(|ui| header(ui, "Status", None, Col::Status, &mut sort_state, false));
                    h.col(|ui| header(ui, "Processes", None, Col::Threads, &mut sort_state, true));
                    h.col(|ui| {
                        header(
                            ui,
                            "CPU",
                            Some(&fmt_percent(snap.cpu.total)),
                            Col::Cpu,
                            &mut sort_state,
                            true,
                        )
                    });
                    h.col(|ui| {
                        header(
                            ui,
                            "Memory",
                            Some(&fmt_percent(snap.mem.used as f32 / mem_total * 100.0)),
                            Col::Memory,
                            &mut sort_state,
                            true,
                        )
                    });
                    h.col(|ui| header(ui, "Disk", None, Col::Disk, &mut sort_state, true));
                    h.col(|ui| header(ui, "GPU", None, Col::Gpu, &mut sort_state, true));
                    h.col(|_| {});
                })
                .body(|body| {
                    body.rows(24.0, rows.len(), |mut row| match rows[row.index()] {
                        Row::User(i) => {
                            let u = &list[i];
                            row.set_selected(state.selected == Some(u.uid));
                            let expanded = state.expanded.contains(&u.uid);
                            let mut clicked_arrow = false;
                            row.col(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(if expanded { "⏷" } else { "⏵" })
                                            .frame(false)
                                            .small(),
                                    )
                                    .clicked()
                                {
                                    clicked_arrow = true;
                                }
                                let full = state.full_names.get(&u.uid).cloned().flatten();
                                let label = match full {
                                    Some(f) if view.settings.show_full_account_name => {
                                        format!("{} ({f})", u.name)
                                    }
                                    _ => u.name.clone(),
                                };
                                let label = if u.uid == current_uid {
                                    format!("{label}  (you)")
                                } else {
                                    label
                                };
                                cell_label(ui, RichText::new(label).strong());
                            });
                            row.col(|ui| {
                                cell_label(
                                    ui,
                                    if u.has_session || u.uid == current_uid {
                                        "Active"
                                    } else {
                                        ""
                                    },
                                )
                            });
                            row.col(|ui| right_label(ui, u.procs.len().to_string()));
                            row.col(|ui| {
                                if heat {
                                    paint_cell_bg(ui, heat_color(u.cpu / 100.0, dark));
                                }
                                right_label(ui, format!("{:.1}%", u.cpu.min(100.0)));
                            });
                            row.col(|ui| {
                                if heat {
                                    paint_cell_bg(
                                        ui,
                                        heat_color(u.mem as f32 / mem_total * 4.0, dark),
                                    );
                                }
                                right_label(ui, fmt_bytes(u.mem));
                            });
                            row.col(|ui| right_label(ui, fmt_rate(u.disk)));
                            row.col(|ui| right_label(ui, format!("{:.1}%", u.gpu.min(100.0))));
                            row.col(|_| {});
                            let resp = row.response();
                            if clicked_arrow || resp.double_clicked() {
                                toggle = Some(u.uid);
                            }
                            if resp.clicked() || resp.secondary_clicked() {
                                new_sel = Some(u.uid);
                            }
                            resp.context_menu(|ui| {
                                if ui
                                    .button(if expanded { "Collapse" } else { "Expand" })
                                    .clicked()
                                {
                                    toggle = Some(u.uid);
                                    ui.close();
                                }
                                ui.separator();
                                let pids: Vec<u32> =
                                    u.procs.iter().map(|&i| snap.procs[i].pid).collect();
                                let warn = if u.uid == current_uid {
                                    " — this signs you out"
                                } else {
                                    ""
                                };
                                if ui
                                    .button(format!("End all processes of {}{warn}", u.name))
                                    .clicked()
                                {
                                    actions.push(Action::Signal {
                                        pids,
                                        sig: ProcSignal::Terminate,
                                        target: format!(
                                            "all {} processes of user {}",
                                            u.procs.len(),
                                            u.name
                                        ),
                                    });
                                    ui.close();
                                }
                                if ui.button("Copy user name").clicked() {
                                    actions.push(Action::Copy(u.name.clone()));
                                    ui.close();
                                }
                            });
                        }
                        Row::Proc(i) => {
                            let p = &snap.procs[i];
                            row.col(|ui| {
                                ui.add_space(40.0);
                                cell_label(ui, &*p.name);
                            });
                            row.col(|ui| {
                                cell_label(ui, RichText::new(format!("PID {}", p.pid)).weak())
                            });
                            row.col(|_| {});
                            row.col(|ui| right_label(ui, format!("{:.1}%", p.cpu)));
                            row.col(|ui| right_label(ui, fmt_bytes(p.mem)));
                            row.col(|ui| {
                                right_label(
                                    ui,
                                    match (p.disk_read, p.disk_write) {
                                        (None, None) => "—".into(),
                                        (r, w) => fmt_rate(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
                                    },
                                )
                            });
                            row.col(|ui| {
                                right_label(
                                    ui,
                                    p.gpu.map(|g| format!("{g:.1}%")).unwrap_or_default(),
                                )
                            });
                            row.col(|_| {});
                            row.response()
                                .context_menu(|ui| process_menu(ui, p, snap, actions, true));
                        }
                    });
                });
        });
    state.sort = sort_state;
    if let Some(uid) = toggle
        && !state.expanded.remove(&uid)
    {
        state.expanded.insert(uid);
    }
    if new_sel.is_some() {
        state.selected = new_sel;
    }
}

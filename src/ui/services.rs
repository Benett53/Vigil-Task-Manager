//! "Services" page.

use super::table::{Col, Sort, cell_label, cmp_str, header, right_label};
use super::{Action, View};
use crate::model::{Service, ServiceAction};
use crate::util::contains_ci;
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::HashMap;
use std::time::Instant;

pub struct State {
    pub list: Option<Result<Vec<Service>, String>>,
    pub loaded_for_user: bool,
    pub loading: bool,
    pub last_loaded: Option<Instant>,
    pub sort: Sort,
    pub selected: Option<String>,
    pub scroll_to: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            list: None,
            loaded_for_user: false,
            loading: false,
            last_loaded: None,
            sort: Sort::new(Col::Name, true),
            selected: None,
            scroll_to: None,
        }
    }
}

impl State {
    pub fn needs_refresh(&self, user: bool) -> bool {
        !self.loading
            && (self.list.is_none()
                || self.loaded_for_user != user
                || self.last_loaded.is_none_or(|t| t.elapsed().as_secs() >= 10))
    }
}

pub fn show(
    ui: &mut Ui,
    state: &mut State,
    view: &View<'_>,
    user_services: &mut bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        if ui.button("⟳ Refresh").clicked() {
            actions.push(Action::RefreshServices);
        }
        if crate::platform::has_user_services() {
            ui.separator();
            ui.selectable_value(user_services, false, "System services");
            ui.selectable_value(user_services, true, "User services");
        }
        ui.separator();
        let sel = state.selected.clone().and_then(|n| {
            state
                .list
                .as_ref()?
                .as_ref()
                .ok()?
                .iter()
                .find(|s| s.name == n)
                .cloned()
        });
        for action in [
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Restart,
        ] {
            let enabled = sel.as_ref().is_some_and(|s| match action {
                ServiceAction::Start => !s.active,
                ServiceAction::Stop => s.active,
                _ => true,
            });
            if ui
                .add_enabled(enabled, egui::Button::new(action.label()))
                .clicked()
                && let Some(s) = &sel
            {
                actions.push(Action::ServiceControl {
                    name: s.name.clone(),
                    user: *user_services,
                    action,
                });
            }
        }
        if state.loading {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "Service manager: {}",
                    crate::platform::service_manager_name()
                ))
                .weak(),
            );
        });
    });
    ui.add_space(4.0);

    let list = match &state.list {
        None => {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(ui.visuals().warn_fg_color));
            return;
        }
        Some(Ok(l)) => l,
    };

    // Map service -> PIDs from the process list (cgroup on Linux, service name on Windows).
    let mut pids: HashMap<&str, Vec<u32>> = HashMap::new();
    for p in &view.snap.procs {
        if let Some(unit) = &p.unit {
            pids.entry(unit.trim_end_matches(".service"))
                .or_default()
                .push(p.pid);
        }
    }
    let mut rows: Vec<&Service> = list
        .iter()
        .filter(|s| {
            view.search.is_empty()
                || contains_ci(&s.name, view.search)
                || contains_ci(&s.description, view.search)
        })
        .collect();
    let sort = state.sort;
    rows.sort_by(|a, b| {
        let ord = match sort.col {
            Col::Name => cmp_str(&a.name, &b.name),
            Col::Description => cmp_str(&a.description, &b.description),
            Col::Status => b
                .active
                .cmp(&a.active)
                .then(b.failed.cmp(&a.failed))
                .then(cmp_str(&a.state, &b.state)),
            Col::Startup => cmp_str(&a.startup, &b.startup),
            Col::Pid => pids
                .get(a.name.as_str())
                .and_then(|v| v.iter().min())
                .cmp(&pids.get(b.name.as_str()).and_then(|v| v.iter().min())),
            Col::Group => cmp_str(&a.group, &b.group),
            _ => std::cmp::Ordering::Equal,
        };
        sort.apply(ord.then_with(|| cmp_str(&a.name, &b.name)))
    });

    let mut sort_state = state.sort;
    let mut new_sel = None;
    egui::ScrollArea::horizontal()
        .id_salt("services_h")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .id_salt("services")
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::initial(240.0).at_least(80.0).clip(true))
                .column(Column::initial(70.0))
                .column(Column::initial(320.0).at_least(80.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::remainder().clip(true));
            if let Some(name) = state.scroll_to.take()
                && let Some(i) = rows.iter().position(|s| s.name == name)
            {
                tb = tb.scroll_to_row(i, Some(Align::Center));
                new_sel = Some(name);
            }
            tb.header(24.0, |mut h| {
                h.col(|ui| header(ui, "Name", None, Col::Name, &mut sort_state, false));
                h.col(|ui| header(ui, "PID", None, Col::Pid, &mut sort_state, true));
                h.col(|ui| {
                    header(
                        ui,
                        "Description",
                        None,
                        Col::Description,
                        &mut sort_state,
                        false,
                    )
                });
                h.col(|ui| header(ui, "Status", None, Col::Status, &mut sort_state, false));
                h.col(|ui| {
                    header(
                        ui,
                        "Startup type",
                        None,
                        Col::Startup,
                        &mut sort_state,
                        false,
                    )
                });
                h.col(|ui| header(ui, "Group", None, Col::Group, &mut sort_state, false));
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let s = rows[row.index()];
                    row.set_selected(state.selected.as_deref() == Some(s.name.as_str()));
                    let pid = s.main_pid.or_else(|| {
                        pids.get(s.name.as_str())
                            .and_then(|v| v.iter().min().copied())
                    });
                    row.col(|ui| {
                        ui.add_space(4.0);
                        cell_label(ui, &s.name);
                    });
                    row.col(|ui| right_label(ui, pid.map(|p| p.to_string()).unwrap_or_default()));
                    row.col(|ui| cell_label(ui, &s.description));
                    row.col(|ui| {
                        let (text, color) = if s.failed {
                            ("Failed".to_owned(), ui.visuals().error_fg_color)
                        } else if s.active {
                            (capitalize(&s.state), ui.visuals().text_color())
                        } else {
                            ("Stopped".to_owned(), ui.visuals().weak_text_color())
                        };
                        cell_label(ui, RichText::new(text).color(color));
                    });
                    row.col(|ui| cell_label(ui, capitalize(&s.startup)));
                    row.col(|ui| cell_label(ui, &s.group));
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        new_sel = Some(s.name.clone());
                    }
                    resp.context_menu(|ui| {
                        for action in [
                            ServiceAction::Start,
                            ServiceAction::Stop,
                            ServiceAction::Restart,
                            ServiceAction::Enable,
                            ServiceAction::Disable,
                        ] {
                            let enabled = match action {
                                ServiceAction::Start => !s.active,
                                ServiceAction::Stop => s.active,
                                ServiceAction::Enable => s.startup == "disabled",
                                ServiceAction::Disable => !matches!(
                                    s.startup.as_str(),
                                    "disabled" | "static" | "masked" | "Boot" | ""
                                ),
                                ServiceAction::Restart => true,
                            };
                            if ui
                                .add_enabled(enabled, egui::Button::new(action.label()))
                                .clicked()
                            {
                                actions.push(Action::ServiceControl {
                                    name: s.name.clone(),
                                    user: *user_services,
                                    action,
                                });
                                ui.close();
                            }
                        }
                        ui.separator();
                        if let Some(pid) = pid
                            && ui.button("Go to details").clicked()
                        {
                            actions.push(Action::GoToDetails(pid));
                            ui.close();
                        }
                        if ui.button("Search online").clicked() {
                            actions.push(Action::SearchOnline(format!("{} service", s.name)));
                            ui.close();
                        }
                        if ui.button("Copy name").clicked() {
                            actions.push(Action::Copy(s.name.clone()));
                            ui.close();
                        }
                    });
                });
            });
        });
    state.sort = sort_state;
    if new_sel.is_some() {
        state.selected = new_sel;
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

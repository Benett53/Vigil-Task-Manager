//! "Startup apps" page.

use super::table::{Col, Sort, cell_label, cmp_str, header};
use super::{Action, View};
use crate::model::StartupEntry;
use crate::util::contains_ci;
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

pub struct State {
    pub list: Option<Vec<StartupEntry>>,
    pub boot_summary: Option<String>,
    pub loading: bool,
    pub sort: Sort,
    pub selected: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            list: None,
            boot_summary: None,
            loading: false,
            sort: Sort::new(Col::Name, true),
            selected: None,
        }
    }
}

pub fn show(ui: &mut Ui, state: &mut State, view: &View<'_>, actions: &mut Vec<Action>) {
    let selected = state
        .selected
        .as_ref()
        .and_then(|id| state.list.as_ref()?.iter().find(|e| &e.id == id).cloned());
    ui.horizontal(|ui| {
        if ui.button("➕ Add…").clicked() {
            actions.push(Action::StartupAddDialog);
        }
        let (label, enable) = match &selected {
            Some(e) if e.enabled => ("Disable", false),
            _ => ("Enable", true),
        };
        if ui
            .add_enabled(selected.is_some(), egui::Button::new(label))
            .clicked()
            && let Some(e) = &selected
        {
            actions.push(Action::StartupSetEnabled(e.clone(), enable));
        }
        if ui
            .add_enabled(
                selected.as_ref().is_some_and(|e| e.removable),
                egui::Button::new("Remove"),
            )
            .on_hover_text("Only entries that belong to your user can be removed")
            .clicked()
            && let Some(e) = &selected
        {
            actions.push(Action::StartupRemove(e.clone()));
        }
        if ui.button("⟳ Refresh").clicked() {
            actions.push(Action::RefreshStartup);
        }
        if state.loading {
            ui.spinner();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(b) = &state.boot_summary {
                ui.label(RichText::new(format!("Last boot: {b}")).weak());
            }
        });
    });
    ui.add_space(4.0);
    let Some(list) = &state.list else {
        ui.centered_and_justified(|ui| ui.spinner());
        return;
    };
    let mut rows: Vec<&StartupEntry> = list
        .iter()
        .filter(|e| {
            view.search.is_empty()
                || contains_ci(&e.name, view.search)
                || contains_ci(&e.command, view.search)
        })
        .collect();
    let sort = state.sort;
    rows.sort_by(|a, b| {
        let ord = match sort.col {
            Col::Name => cmp_str(&a.name, &b.name),
            Col::Command => cmp_str(&a.command, &b.command),
            Col::Status => b.enabled.cmp(&a.enabled),
            Col::Source => cmp_str(&a.source, &b.source),
            _ => std::cmp::Ordering::Equal,
        };
        sort.apply(ord.then_with(|| cmp_str(&a.name, &b.name)))
    });
    if rows.is_empty() {
        ui.label(RichText::new("No startup apps found.").weak());
        return;
    }
    let mut sort_state = state.sort;
    let mut new_sel = None;
    TableBuilder::new(ui)
        .id_salt("startup")
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .auto_shrink([false, false])
        .column(Column::initial(260.0).at_least(100.0).clip(true))
        .column(Column::initial(100.0))
        .column(Column::initial(220.0).clip(true))
        .column(Column::remainder().clip(true))
        .header(24.0, |mut h| {
            h.col(|ui| header(ui, "Name", None, Col::Name, &mut sort_state, false));
            h.col(|ui| header(ui, "Status", None, Col::Status, &mut sort_state, false));
            h.col(|ui| header(ui, "Source", None, Col::Source, &mut sort_state, false));
            h.col(|ui| header(ui, "Command", None, Col::Command, &mut sort_state, false));
        })
        .body(|body| {
            body.rows(24.0, rows.len(), |mut row| {
                let e = rows[row.index()];
                row.set_selected(state.selected.as_deref() == Some(e.id.as_str()));
                row.col(|ui| {
                    ui.add_space(4.0);
                    cell_label(ui, &e.name);
                });
                row.col(|ui| {
                    let (t, c) = if e.enabled {
                        ("Enabled", ui.visuals().text_color())
                    } else {
                        ("Disabled", ui.visuals().weak_text_color())
                    };
                    cell_label(ui, RichText::new(t).color(c));
                });
                row.col(|ui| cell_label(ui, &e.source));
                row.col(|ui| cell_label(ui, RichText::new(&e.command).weak()));
                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    new_sel = Some(e.id.clone());
                }
                let resp = if e.description.is_empty() {
                    resp
                } else {
                    resp.on_hover_text(&e.description)
                };
                resp.context_menu(|ui| {
                    if ui
                        .button(if e.enabled { "Disable" } else { "Enable" })
                        .clicked()
                    {
                        actions.push(Action::StartupSetEnabled(e.clone(), !e.enabled));
                        ui.close();
                    }
                    if ui
                        .add_enabled(e.removable, egui::Button::new("Remove"))
                        .clicked()
                    {
                        actions.push(Action::StartupRemove(e.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            !e.location.is_empty(),
                            egui::Button::new("Open file location"),
                        )
                        .clicked()
                    {
                        actions.push(Action::OpenLocation(e.location.clone()));
                        ui.close();
                    }
                    if ui.button("Search online").clicked() {
                        actions.push(Action::SearchOnline(e.name.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(!e.command.is_empty(), egui::Button::new("Copy command"))
                        .clicked()
                    {
                        actions.push(Action::Copy(e.command.clone()));
                        ui.close();
                    }
                });
            });
        });
    state.sort = sort_state;
    if new_sel.is_some() {
        state.selected = new_sel;
    }
}

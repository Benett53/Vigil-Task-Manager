//! Modal dialogs and auxiliary windows.

use super::Action;
use crate::model::{Priority, ProcSignal, ProcessDetails};
use egui::{Context, Id, Key, Modal, RichText, ScrollArea};

/// An operation that failed with "permission denied" and can be retried elevated.
#[derive(Clone, Debug)]
pub enum Retry {
    Signal(Vec<u32>, ProcSignal),
    Priority(u32, Priority),
    Affinity(u32, Vec<bool>),
}

pub struct Confirm {
    pub action: Action,
    pub dont_ask: bool,
}

pub struct RunTask {
    pub command: String,
    pub elevated: bool,
    pub focus: bool,
}

pub struct PropertiesWindow {
    pub pid: u32,
    pub name: String,
    pub details: Option<ProcessDetails>,
    pub open: bool,
}

pub struct AffinityDialog {
    pub pid: u32,
    pub name: String,
    pub mask: Option<Result<Vec<bool>, String>>,
}

#[derive(Default)]
pub struct Dialogs {
    pub confirm: Option<Confirm>,
    pub elevate: Option<(String, String, Retry)>,
    pub run_task: Option<RunTask>,
    pub add_startup: Option<(String, String)>,
    pub properties: Vec<PropertiesWindow>,
    pub affinity: Option<AffinityDialog>,
    pub about: bool,
    pub shortcuts: bool,
    pub message: Option<(String, String)>,
}

pub enum Outcome {
    Confirmed(Action, bool),
    Elevate(Retry),
    RunTask(String, bool),
    AddStartup(String, String),
    SetAffinity(u32, Vec<bool>),
}

impl Dialogs {
    pub fn any_modal_open(&self) -> bool {
        self.confirm.is_some()
            || self.elevate.is_some()
            || self.run_task.is_some()
            || self.add_startup.is_some()
            || self.affinity.is_some()
            || self.about
            || self.shortcuts
            || self.message.is_some()
    }

    pub fn show(&mut self, ctx: &Context, can_elevate: bool) -> Vec<Outcome> {
        let mut out = Vec::new();
        self.show_confirm(ctx, &mut out);
        self.show_elevate(ctx, can_elevate, &mut out);
        self.show_run_task(ctx, &mut out);
        self.show_add_startup(ctx, &mut out);
        self.show_affinity(ctx, &mut out);
        self.show_properties(ctx);
        self.show_about(ctx);
        self.show_shortcuts(ctx);
        self.show_message(ctx);
        out
    }

    fn show_confirm(&mut self, ctx: &Context, out: &mut Vec<Outcome>) {
        let Some(c) = &mut self.confirm else { return };
        let mut close = false;
        let (verb, target) = match &c.action {
            Action::Signal { sig, target, .. } => (*sig, target.clone()),
            _ => (ProcSignal::Terminate, String::new()),
        };
        let resp = Modal::new(Id::new("confirm")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            let title = match verb {
                ProcSignal::Kill => "Force kill",
                ProcSignal::Suspend => "Suspend",
                ProcSignal::Resume => "Resume",
                ProcSignal::Terminate => "End task",
            };
            ui.heading(format!("{title}?"));
            ui.add_space(6.0);
            ui.label(format!("Do you want to {} {target}?", title.to_lowercase()));
            if verb == ProcSignal::Kill {
                ui.label(
                    RichText::new(
                        "The process gets no chance to save its data. Unsaved work will be lost.",
                    )
                    .color(ui.visuals().warn_fg_color),
                );
            } else if verb == ProcSignal::Terminate {
                ui.label(RichText::new("Unsaved data may be lost.").weak());
            }
            ui.add_space(8.0);
            ui.checkbox(&mut c.dont_ask, "Don't ask me again");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = ui.button(RichText::new(title).strong());
                if ok.clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    out.push(Outcome::Confirmed(c.action.clone(), c.dont_ask));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.confirm = None;
        }
    }

    fn show_elevate(&mut self, ctx: &Context, can_elevate: bool, out: &mut Vec<Outcome>) {
        let Some((what, err, retry)) = &self.elevate else {
            return;
        };
        let mut close = false;
        let resp = Modal::new(Id::new("elevate")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading("Access denied");
            ui.add_space(6.0);
            ui.label(format!("{what} failed: {err}"));
            ui.add_space(4.0);
            if can_elevate {
                ui.label(format!(
                    "This process belongs to another user or the system. You can retry with {} rights.",
                    crate::platform::ELEVATION_NAME
                ));
            } else {
                ui.label("Administrator rights are required, but no elevation helper (pkexec) is available.");
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if can_elevate && ui.button(RichText::new("🛡 Retry as administrator").strong()).clicked() {
                    out.push(Outcome::Elevate(retry.clone()));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.elevate = None;
        }
    }

    fn show_run_task(&mut self, ctx: &Context, out: &mut Vec<Outcome>) {
        let Some(rt) = &mut self.run_task else { return };
        let mut close = false;
        let resp = Modal::new(Id::new("run_task")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Run new task");
            ui.add_space(4.0);
            ui.label("Type the name of a program, command or script and Vigil will start it for you.");
            ui.add_space(6.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut rt.command)
                    .hint_text(if cfg!(windows) { "e.g. notepad" } else { "e.g. firefox" })
                    .desired_width(f32::INFINITY),
            );
            if rt.focus {
                edit.request_focus();
                rt.focus = false;
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.checkbox(&mut rt.elevated, "Create this task with administrative privileges");
            if rt.elevated && cfg!(target_os = "linux") {
                ui.label(
                    RichText::new("Note: pkexec does not pass the graphical session to programs; use this for command-line tools.")
                        .small()
                        .weak(),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!rt.command.trim().is_empty(), egui::Button::new("OK")).clicked() || (enter && !rt.command.trim().is_empty()) {
                    out.push(Outcome::RunTask(rt.command.clone(), rt.elevated));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.run_task = None;
        }
    }

    fn show_add_startup(&mut self, ctx: &Context, out: &mut Vec<Outcome>) {
        let Some((name, command)) = &mut self.add_startup else {
            return;
        };
        let mut close = false;
        let resp = Modal::new(Id::new("add_startup")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Add startup app");
            ui.add_space(6.0);
            egui::Grid::new("add_startup_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                    ui.end_row();
                    ui.label("Command:");
                    ui.add(
                        egui::TextEdit::singleline(command)
                            .desired_width(320.0)
                            .hint_text("/usr/bin/program --option"),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = !name.trim().is_empty() && !command.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new("Add")).clicked() {
                    out.push(Outcome::AddStartup(name.clone(), command.clone()));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.add_startup = None;
        }
    }

    fn show_affinity(&mut self, ctx: &Context, out: &mut Vec<Outcome>) {
        let Some(dlg) = &mut self.affinity else {
            return;
        };
        let mut close = false;
        let resp = Modal::new(Id::new("affinity")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Processor affinity");
            ui.label(format!(
                "Which processors are allowed to run {} (PID {})?",
                dlg.name, dlg.pid
            ));
            ui.add_space(6.0);
            match &mut dlg.mask {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.as_str()).color(ui.visuals().error_fg_color));
                }
                Some(Ok(mask)) => {
                    let mut all = mask.iter().all(|b| *b);
                    if ui.checkbox(&mut all, "<All processors>").changed() {
                        mask.iter_mut().for_each(|b| *b = all);
                    }
                    ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        egui::Grid::new("aff_grid").num_columns(4).show(ui, |ui| {
                            for (i, b) in mask.iter_mut().enumerate() {
                                ui.checkbox(b, format!("CPU {i}"));
                                if (i + 1) % 4 == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                    });
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let valid = matches!(&dlg.mask, Some(Ok(m)) if m.iter().any(|b| *b));
                if ui.add_enabled(valid, egui::Button::new("OK")).clicked() {
                    if let Some(Ok(m)) = &dlg.mask {
                        out.push(Outcome::SetAffinity(dlg.pid, m.clone()));
                    }
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.affinity = None;
        }
    }

    fn show_properties(&mut self, ctx: &Context) {
        for w in &mut self.properties {
            let mut open = w.open;
            egui::Window::new(format!("{} (PID {}) — Properties", w.name, w.pid))
                .id(Id::new(("props", w.pid)))
                .open(&mut open)
                .default_width(560.0)
                .default_height(420.0)
                .resizable(true)
                .collapsible(false)
                .show(ctx, |ui| match &w.details {
                    None => {
                        ui.spinner();
                    }
                    Some(d) => {
                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Grid::new(("props_grid", w.pid))
                                    .num_columns(2)
                                    .striped(true)
                                    .spacing([14.0, 4.0])
                                    .show(ui, |ui| {
                                        for (k, v) in &d.rows {
                                            ui.label(RichText::new(k).weak());
                                            ui.add(egui::Label::new(v).wrap());
                                            ui.end_row();
                                        }
                                    });
                            });
                    }
                });
            w.open = open;
        }
        self.properties.retain(|w| w.open);
    }

    fn show_about(&mut self, ctx: &Context) {
        if !self.about {
            return;
        }
        let mut close = false;
        let resp = Modal::new(Id::new("about")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(format!("Vigil Task Manager {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label("A fast, free and open source task manager for Linux and Windows.");
            ui.label("No telemetry, no accounts, no paid tiers — ever.");
            ui.add_space(6.0);
            ui.label(
                RichText::new("Licensed under the GNU General Public License v3.0 or later.")
                    .weak(),
            );
            ui.hyperlink_to("Source code on GitHub", env!("CARGO_PKG_REPOSITORY"));
            ui.add_space(8.0);
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if close || resp.should_close() {
            self.about = false;
        }
    }

    fn show_shortcuts(&mut self, ctx: &Context) {
        if !self.shortcuts {
            return;
        }
        let mut close = false;
        let resp = Modal::new(Id::new("shortcuts")).show(ctx, |ui| {
            ui.heading("Keyboard shortcuts");
            ui.add_space(6.0);
            egui::Grid::new("shortcut_grid")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for (k, v) in [
                        ("Ctrl+F", "Search"),
                        ("Esc", "Clear search / close dialog"),
                        ("Delete", "End task"),
                        ("Shift+Delete", "Kill (force)"),
                        ("↑ / ↓", "Move selection"),
                        ("Ctrl+N", "Run new task"),
                        ("F5", "Refresh now"),
                        ("Ctrl+1 … Ctrl+7", "Switch page"),
                        ("Ctrl+, ", "Settings"),
                        ("Ctrl + / Ctrl −", "Zoom in / out"),
                        ("Ctrl+Q", "Quit"),
                    ] {
                        ui.label(RichText::new(k).strong());
                        ui.label(v);
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if close || resp.should_close() {
            self.shortcuts = false;
        }
    }

    fn show_message(&mut self, ctx: &Context) {
        let Some((title, body)) = &self.message else {
            return;
        };
        let mut close = false;
        let resp = Modal::new(Id::new("message")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(title.as_str());
            ui.add_space(6.0);
            ui.label(body.as_str());
            ui.add_space(8.0);
            if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                close = true;
            }
        });
        if close || resp.should_close() {
            self.message = None;
        }
    }
}

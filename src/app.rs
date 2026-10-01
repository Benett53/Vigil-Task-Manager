//! Application shell: navigation, header, status bar, settings application and the
//! execution of user actions (in background threads so the UI never blocks).

use crate::history::{AppHistory, History};
use crate::model::{ProcSignal, ProcessDetails, Service, Snapshot, StartupEntry, StaticInfo};
use crate::platform::{self, ActionError, ActionResult};
use crate::sampler::{Command, Sampler};
use crate::settings::{Page, ProcessColumns, Settings, ThemeChoice, UpdateSpeed};
use crate::ui::dialogs::{
    AffinityDialog, Confirm, Dialogs, Outcome, PropertiesWindow, Retry, RunTask,
};
use crate::ui::{self, Action, View, theme};
use egui::{Align, Key, Layout, Modifiers, RichText, Ui, ViewportCommand};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const SETTINGS_KEY: &str = "vigil.settings";
const APP_HISTORY_KEY: &str = "vigil.app_history";

enum Refresh {
    None,
    Processes,
    Startup,
    Services,
}

enum JobResult {
    Done {
        what: String,
        result: ActionResult,
        retry: Option<Retry>,
        refresh: Refresh,
    },
    Startup(Vec<StartupEntry>, Option<String>),
    Services(bool, Result<Vec<Service>, String>),
    Details(u32, ProcessDetails),
    Affinity(u32, Result<Vec<bool>, ActionError>),
}

#[derive(PartialEq)]
struct Applied {
    theme: ThemeChoice,
    zoom: f32,
    on_top: bool,
    speed: UpdateSpeed,
}

pub struct VigilApp {
    settings: Settings,
    applied: Option<Applied>,
    page: Page,
    sampler: Sampler,
    snap: Arc<Snapshot>,
    has_data: bool,
    history: History,
    app_history: AppHistory,
    search: String,
    search_lower: String,
    focus_search: bool,
    processes: ui::processes::State,
    performance: ui::performance::State,
    app_hist: ui::app_history::State,
    startup: ui::startup::State,
    users: ui::users::State,
    details: ui::details::State,
    services: ui::services::State,
    dialogs: Dialogs,
    jobs_tx: Sender<JobResult>,
    jobs_rx: Receiver<JobResult>,
    status: Option<(String, Instant, bool)>,
    ctx: egui::Context,
    can_elevate: bool,
    running_jobs: usize,
}

impl VigilApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start_page: Option<Page>) -> VigilApp {
        theme::install(&cc.egui_ctx);
        let mut settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        settings.sanitize();
        let app_history: AppHistory = cc
            .storage
            .and_then(|s| eframe::get_value(s, APP_HISTORY_KEY))
            .unwrap_or_default();
        let ctx = cc.egui_ctx.clone();
        let wake_ctx = ctx.clone();
        let sampler = Sampler::spawn(
            settings.update_speed.interval(),
            settings.update_speed == UpdateSpeed::Paused,
            move || wake_ctx.request_repaint(),
        );
        let (jobs_tx, jobs_rx) = mpsc::channel();
        VigilApp {
            page: start_page.unwrap_or(settings.start_page),
            settings,
            applied: None,
            sampler,
            snap: Arc::new(Snapshot::empty(Arc::new(StaticInfo::default()))),
            has_data: false,
            history: History::default(),
            app_history,
            search: String::new(),
            search_lower: String::new(),
            focus_search: false,
            processes: Default::default(),
            performance: Default::default(),
            app_hist: Default::default(),
            startup: Default::default(),
            users: Default::default(),
            details: Default::default(),
            services: Default::default(),
            dialogs: Dialogs::default(),
            jobs_tx,
            jobs_rx,
            status: None,
            ctx,
            can_elevate: platform::can_elevate(),
            running_jobs: 0,
        }
    }

    fn spawn_job(&mut self, f: impl FnOnce() -> JobResult + Send + 'static) {
        let tx = self.jobs_tx.clone();
        let ctx = self.ctx.clone();
        self.running_jobs += 1;
        let spawned = std::thread::Builder::new()
            .name("vigil-job".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                    .unwrap_or_else(|_| JobResult::Done {
                        what: "Operation".into(),
                        result: Err(ActionError::Other("internal error".into())),
                        retry: None,
                        refresh: Refresh::None,
                    });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.running_jobs -= 1;
            self.set_status("Could not start a background task".into(), true);
        }
    }

    fn set_status(&mut self, msg: String, error: bool) {
        self.status = Some((msg, Instant::now(), error));
    }

    /// Pulls new snapshots and job results. Runs even while the window is hidden.
    fn pump(&mut self) {
        for snap in self.sampler.drain() {
            self.history.record(&snap, self.settings.graph_points);
            self.app_history.record(&snap);
            self.snap = snap;
            self.has_data = true;
        }
        while let Ok(job) = self.jobs_rx.try_recv() {
            self.running_jobs = self.running_jobs.saturating_sub(1);
            self.handle_job(job);
        }
    }

    fn handle_job(&mut self, job: JobResult) {
        match job {
            JobResult::Done {
                what,
                result,
                retry,
                refresh,
            } => {
                match result {
                    Ok(()) => self.set_status(format!("{what}: done"), false),
                    Err(ActionError::PermissionDenied(e)) if retry.is_some() => {
                        self.dialogs.elevate = Some((what, e, retry.expect("checked")));
                    }
                    Err(e) => {
                        self.set_status(format!("{what} failed: {e}"), true);
                        self.dialogs.message = Some((format!("{what} failed"), e.to_string()));
                    }
                }
                match refresh {
                    Refresh::None => {}
                    Refresh::Processes => self.sampler.send(Command::RefreshNow),
                    Refresh::Startup => self.handle_action(Action::RefreshStartup),
                    Refresh::Services => self.handle_action(Action::RefreshServices),
                }
            }
            JobResult::Startup(list, boot) => {
                self.startup.list = Some(list);
                if boot.is_some() {
                    self.startup.boot_summary = boot;
                }
                self.startup.loading = false;
            }
            JobResult::Services(user, list) => {
                self.services.list = Some(list);
                self.services.loaded_for_user = user;
                self.services.loading = false;
                self.services.last_loaded = Some(Instant::now());
            }
            JobResult::Details(pid, details) => {
                if let Some(w) = self.dialogs.properties.iter_mut().find(|w| w.pid == pid) {
                    w.details = Some(details);
                }
            }
            JobResult::Affinity(pid, mask) => {
                if let Some(d) = self.dialogs.affinity.as_mut().filter(|d| d.pid == pid) {
                    d.mask = Some(mask.map_err(|e| e.to_string()));
                }
            }
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Signal { pids, sig, target } => {
                let needs_confirm = self.settings.confirm_end_task
                    && matches!(sig, ProcSignal::Terminate | ProcSignal::Kill);
                if needs_confirm {
                    self.dialogs.confirm = Some(Confirm {
                        action: Action::Signal { pids, sig, target },
                        dont_ask: false,
                    });
                } else {
                    self.do_signal(pids, sig, target, false);
                }
            }
            Action::SetPriority(pid, prio) => self.spawn_job(move || JobResult::Done {
                what: format!("Set priority of PID {pid}"),
                result: platform::set_priority(pid, prio, false),
                retry: Some(Retry::Priority(pid, prio)),
                refresh: Refresh::Processes,
            }),
            Action::SetEfficiency(pids, on) => self.spawn_job(move || {
                let mut result = Ok(());
                for pid in &pids {
                    if let Err(e) = platform::set_efficiency(*pid, on) {
                        result = Err(e);
                    }
                }
                JobResult::Done {
                    what: if on {
                        "Enable efficiency mode"
                    } else {
                        "Disable efficiency mode"
                    }
                    .into(),
                    result,
                    retry: None,
                    refresh: Refresh::Processes,
                }
            }),
            Action::OpenAffinity(pid, name) => {
                self.dialogs.affinity = Some(AffinityDialog {
                    pid,
                    name,
                    mask: None,
                });
                self.spawn_job(move || JobResult::Affinity(pid, platform::get_affinity(pid)));
            }
            Action::OpenProperties(pid, name) => {
                if !self.dialogs.properties.iter().any(|w| w.pid == pid) {
                    self.dialogs.properties.push(PropertiesWindow {
                        pid,
                        name,
                        details: None,
                        open: true,
                    });
                }
                self.spawn_job(move || JobResult::Details(pid, platform::process_details(pid)));
            }
            Action::OpenLocation(path) => {
                if let Err(e) = platform::open_location(&path) {
                    self.set_status(format!("Open file location failed: {e}"), true);
                }
            }
            Action::SearchOnline(query) => {
                let url = format!(
                    "https://duckduckgo.com/?q={}",
                    crate::util::url_encode(&query)
                );
                if let Err(e) = platform::open_url(&url) {
                    self.set_status(format!("Could not open the browser: {e}"), true);
                }
            }
            Action::Copy(text) => {
                self.ctx.copy_text(text);
                self.set_status("Copied to clipboard".into(), false);
            }
            Action::GoToDetails(pid) => {
                self.page = Page::Details;
                self.details.selected = Some(pid);
                self.details.scroll_to = Some(pid);
                self.clear_search();
            }
            Action::GoToService(name) => {
                self.page = Page::Services;
                self.services.selected = Some(name.clone());
                self.services.scroll_to = Some(name);
                self.clear_search();
            }
            Action::RunTaskDialog => {
                self.dialogs.run_task = Some(RunTask {
                    command: String::new(),
                    elevated: false,
                    focus: true,
                });
            }
            Action::ServiceControl { name, user, action } => {
                self.spawn_job(move || JobResult::Done {
                    what: format!("{} {name}", action.label()),
                    result: platform::service_control(&name, user, action),
                    retry: None,
                    refresh: Refresh::Services,
                })
            }
            Action::RefreshServices => {
                if !self.services.loading {
                    self.services.loading = true;
                    let user = self.settings.show_user_services;
                    self.spawn_job(move || {
                        JobResult::Services(user, platform::services_list(user))
                    });
                }
            }
            Action::RefreshStartup => {
                if !self.startup.loading {
                    self.startup.loading = true;
                    let want_boot = self.startup.boot_summary.is_none();
                    self.spawn_job(move || {
                        let boot = if want_boot {
                            platform::last_boot_summary()
                        } else {
                            None
                        };
                        JobResult::Startup(platform::startup_list(), boot)
                    });
                }
            }
            Action::StartupSetEnabled(entry, enabled) => self.spawn_job(move || JobResult::Done {
                what: format!(
                    "{} {}",
                    if enabled { "Enable" } else { "Disable" },
                    entry.name
                ),
                result: platform::startup_set_enabled(&entry, enabled),
                retry: None,
                refresh: Refresh::Startup,
            }),
            Action::StartupRemove(entry) => self.spawn_job(move || JobResult::Done {
                what: format!("Remove {}", entry.name),
                result: platform::startup_remove(&entry),
                retry: None,
                refresh: Refresh::Startup,
            }),
            Action::StartupAddDialog => {
                self.dialogs.add_startup = Some((String::new(), String::new()))
            }
            Action::ResetAppHistory => {
                self.app_history.reset();
                self.set_status("Usage history deleted".into(), false);
            }
            Action::RefreshNow => self.sampler.send(Command::RefreshNow),
        }
    }

    fn do_signal(&mut self, pids: Vec<u32>, sig: ProcSignal, target: String, elevated: bool) {
        self.spawn_job(move || {
            let mut denied = Vec::new();
            let mut other: Option<ActionError> = None;
            for pid in &pids {
                match platform::send_signal(*pid, sig, elevated) {
                    Ok(()) => {}
                    Err(ActionError::PermissionDenied(_)) => denied.push(*pid),
                    Err(e) => other = Some(e),
                }
            }
            let what = format!("{} {target}", sig.label());
            let result = if !denied.is_empty() {
                Err(ActionError::PermissionDenied(format!(
                    "you are not allowed to signal {} process(es)",
                    denied.len()
                )))
            } else if let Some(e) = other.filter(|_| pids.len() == 1) {
                Err(e)
            } else {
                Ok(())
            };
            JobResult::Done {
                what,
                result,
                retry: (!denied.is_empty() && !elevated).then_some(Retry::Signal(denied, sig)),
                refresh: Refresh::Processes,
            }
        });
    }

    fn handle_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Confirmed(action, dont_ask) => {
                if dont_ask {
                    self.settings.confirm_end_task = false;
                }
                if let Action::Signal { pids, sig, target } = action {
                    self.do_signal(pids, sig, target, false);
                }
            }
            Outcome::Elevate(retry) => match retry {
                Retry::Signal(pids, sig) => {
                    self.do_signal(pids, sig, "(as administrator)".into(), true)
                }
                Retry::Priority(pid, prio) => self.spawn_job(move || JobResult::Done {
                    what: format!("Set priority of PID {pid}"),
                    result: platform::set_priority(pid, prio, true),
                    retry: None,
                    refresh: Refresh::Processes,
                }),
                Retry::Affinity(pid, mask) => self.spawn_job(move || JobResult::Done {
                    what: format!("Set affinity of PID {pid}"),
                    result: platform::set_affinity(pid, &mask, true),
                    retry: None,
                    refresh: Refresh::None,
                }),
            },
            Outcome::RunTask(cmd, elevated) => {
                if self.settings.minimize_on_use {
                    self.ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
                self.spawn_job(move || JobResult::Done {
                    what: format!("Run \"{cmd}\""),
                    result: platform::run_task(&cmd, elevated),
                    retry: None,
                    refresh: Refresh::Processes,
                });
            }
            Outcome::AddStartup(name, cmd) => self.spawn_job(move || JobResult::Done {
                what: format!("Add {name}"),
                result: platform::startup_add(&name, &cmd),
                retry: None,
                refresh: Refresh::Startup,
            }),
            Outcome::SetAffinity(pid, mask) => self.spawn_job(move || {
                let result = platform::set_affinity(pid, &mask, false);
                JobResult::Done {
                    what: format!("Set affinity of PID {pid}"),
                    retry: Some(Retry::Affinity(pid, mask)),
                    result,
                    refresh: Refresh::None,
                }
            }),
        }
    }

    fn clear_search(&mut self) {
        self.search.clear();
        self.search_lower.clear();
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        // Keep the zoom setting in sync with egui's built-in Ctrl +/- zoom.
        if let Some(a) = &self.applied {
            let z = ctx.zoom_factor();
            if (z - a.zoom).abs() > 0.001 && (self.settings.zoom - a.zoom).abs() < 0.001 {
                self.settings.zoom = z.clamp(0.6, 2.5);
            }
        }
        let want = Applied {
            theme: self.settings.theme,
            zoom: self.settings.zoom,
            on_top: self.settings.always_on_top,
            speed: self.settings.update_speed,
        };
        let first = self.applied.is_none();
        let prev = self.applied.take();
        if first || prev.as_ref().is_some_and(|p| p.theme != want.theme) {
            ctx.set_theme(match want.theme {
                ThemeChoice::System => egui::ThemePreference::System,
                ThemeChoice::Light => egui::ThemePreference::Light,
                ThemeChoice::Dark => egui::ThemePreference::Dark,
            });
        }
        if first
            || prev
                .as_ref()
                .is_some_and(|p| (p.zoom - want.zoom).abs() > 0.001)
        {
            ctx.set_zoom_factor(want.zoom);
        }
        if (first && want.on_top) || prev.as_ref().is_some_and(|p| p.on_top != want.on_top) {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if want.on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
        }
        if prev.as_ref().is_some_and(|p| p.speed != want.speed) {
            self.sampler
                .send(Command::SetInterval(want.speed.interval()));
            self.sampler
                .send(Command::SetPaused(want.speed == UpdateSpeed::Paused));
        }
        self.applied = Some(want);
    }

    fn view(&self) -> View<'_> {
        View {
            snap: &self.snap,
            history: &self.history,
            app_history: &self.app_history,
            settings: &self.settings,
            search: &self.search_lower,
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.dialogs.any_modal_open() {
            return;
        }
        let typing = ctx.text_edit_focused();
        let (cmd_f, esc, del, shift_del, up, down, run, f5, quit, settings, page_key) = ctx
            .input_mut(|i| {
                let mut page = None;
                for (n, key) in [
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                    Key::Num4,
                    Key::Num5,
                    Key::Num6,
                    Key::Num7,
                ]
                .into_iter()
                .enumerate()
                {
                    if i.consume_key(Modifiers::COMMAND, key) {
                        page = Some(n);
                    }
                }
                (
                    i.consume_key(Modifiers::COMMAND, Key::F),
                    i.key_pressed(Key::Escape),
                    !typing && i.consume_key(Modifiers::NONE, Key::Delete),
                    !typing && i.consume_key(Modifiers::SHIFT, Key::Delete),
                    !typing && i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    !typing && i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::COMMAND, Key::N),
                    i.consume_key(Modifiers::NONE, Key::F5),
                    i.consume_key(Modifiers::COMMAND, Key::Q),
                    i.consume_key(Modifiers::COMMAND, Key::Comma),
                    page,
                )
            });
        if cmd_f {
            self.focus_search = true;
        }
        if esc && !self.search.is_empty() {
            self.clear_search();
        }
        if let Some(n) = page_key {
            self.page = Page::MAIN[n];
        }
        if settings {
            self.page = Page::Settings;
        }
        if run {
            actions.push(Action::RunTaskDialog);
        }
        if f5 {
            actions.push(Action::RefreshNow);
        }
        if quit {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if del || shift_del {
            let view = self.view();
            let action = match self.page {
                Page::Processes => self.processes.end_task_action(&view),
                Page::Details => self.details.end_task_action(&view),
                _ => None,
            };
            if let Some(Action::Signal { pids, target, .. }) = action {
                actions.push(Action::Signal {
                    pids,
                    sig: if shift_del {
                        ProcSignal::Kill
                    } else {
                        ProcSignal::Terminate
                    },
                    target,
                });
            }
        }
        if up || down {
            let delta = if up { -1 } else { 1 };
            match self.page {
                Page::Processes => {
                    let snap = self.snap.clone();
                    let view = View {
                        snap: &snap,
                        history: &self.history,
                        app_history: &self.app_history,
                        settings: &self.settings,
                        search: &self.search_lower,
                    };
                    self.processes.move_selection(&view, delta);
                }
                Page::Details => self.details.move_selection(delta),
                _ => {}
            }
        }
    }

    fn nav_rail(&mut self, ui: &mut Ui) {
        let collapsed = self.settings.nav_collapsed;
        let width = if collapsed { 52.0 } else { 196.0 };
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(width)
            .frame(
                egui::Frame::new()
                    .fill(theme::nav_fill(ui.visuals().dark_mode))
                    .inner_margin(6.0),
            )
            .show(ui, |ui| {
                ui.add_space(2.0);
                if ui
                    .add(egui::Button::new(RichText::new("☰").size(18.0)).frame(false))
                    .on_hover_text(if collapsed {
                        "Expand navigation"
                    } else {
                        "Collapse navigation"
                    })
                    .clicked()
                {
                    self.settings.nav_collapsed = !collapsed;
                }
                ui.add_space(8.0);
                for (i, page) in Page::MAIN.into_iter().enumerate() {
                    self.nav_button(ui, page, collapsed, Some(i + 1));
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(4.0);
                    self.nav_button(ui, Page::Settings, collapsed, None);
                });
            });
    }

    fn nav_button(&mut self, ui: &mut Ui, page: Page, collapsed: bool, shortcut: Option<usize>) {
        let selected = self.page == page;
        let text = if collapsed {
            RichText::new(page.icon()).size(16.0)
        } else {
            RichText::new(format!("{}   {}", page.icon(), page.title())).size(14.0)
        };
        let button = egui::Button::new(text)
            .selected(selected)
            .frame(selected)
            .min_size(egui::vec2(ui.available_width(), 34.0));
        let mut resp = ui.add(button);
        if collapsed || shortcut.is_some() {
            let hint = match shortcut {
                Some(n) => format!("{}  (Ctrl+{n})", page.title()),
                None => format!("{}  (Ctrl+,)", page.title()),
            };
            resp = resp.on_hover_text(hint);
        }
        if resp.clicked() {
            self.page = page;
            if page == Page::Startup && self.startup.list.is_none() {
                self.handle_action(Action::RefreshStartup);
            }
        }
    }

    fn header(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.page.title()).size(20.0).strong());
                    ui.add_space(16.0);
                    if self.page != Page::Settings {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("🔍 Search name, PID, user or command  (Ctrl+F)")
                                .desired_width(ui.available_width().min(380.0)),
                        );
                        if self.focus_search {
                            edit.request_focus();
                            self.focus_search = false;
                        }
                        if edit.changed() {
                            self.search_lower = self.search.trim().to_lowercase();
                        }
                        if !self.search.is_empty()
                            && ui.small_button("✖").on_hover_text("Clear search").clicked()
                        {
                            self.clear_search();
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.more_menu(ui, actions);
                        self.page_actions(ui, actions);
                    });
                });
            });
    }

    fn page_actions(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let view = View {
            snap: &self.snap,
            history: &self.history,
            app_history: &self.app_history,
            settings: &self.settings,
            search: &self.search_lower,
        };
        match self.page {
            Page::Processes | Page::Details => {
                let (columns_label, details) = ("Columns", self.page == Page::Details);
                let end = if details {
                    self.details.end_task_action(&view)
                } else {
                    self.processes.end_task_action(&view)
                };
                let selection = if details {
                    self.details
                        .selected
                        .and_then(|pid| view.snap.procs.iter().find(|p| p.pid == pid))
                        .map(|p| (vec![p.pid], p.efficiency))
                } else {
                    self.processes.selection_pids(&view)
                };
                let mut group = self.settings.group_by_type;
                let mut kernel = self.settings.show_kernel_threads;
                let mut heat = self.settings.heatmap;
                let mut expand_all = None;
                let mut cols = if details {
                    self.settings.details_columns.clone()
                } else {
                    self.settings.processes_columns.clone()
                };
                ui.menu_button(format!("{columns_label} ⏷"), |ui| {
                    for (label, flag) in cols.toggles() {
                        ui.checkbox(flag, label);
                    }
                    ui.separator();
                    if ui.button("Reset columns").clicked() {
                        cols = if details {
                            ProcessColumns::details_default()
                        } else {
                            ProcessColumns::default()
                        };
                    }
                });
                if !details {
                    ui.menu_button("View ⏷", |ui| {
                        ui.checkbox(&mut group, "Group by type");
                        ui.checkbox(&mut heat, "Heat map colors");
                        ui.checkbox(&mut kernel, "Show kernel threads");
                        ui.separator();
                        if ui.button("Expand all").clicked() {
                            expand_all = Some(true);
                            ui.close();
                        }
                        if ui.button("Collapse all").clicked() {
                            expand_all = Some(false);
                            ui.close();
                        }
                    });
                } else {
                    ui.checkbox(&mut kernel, "Kernel threads");
                }
                let eff_label = match &selection {
                    Some((_, true)) => "🍃 Efficiency mode ✔",
                    _ => "🍃 Efficiency mode",
                };
                if ui
                    .add_enabled(selection.is_some(), egui::Button::new(eff_label))
                    .on_hover_text("Lower the priority of the selected process (nice 19, idle I/O) to save resources")
                    .clicked()
                    && let Some((pids, on)) = selection {
                        actions.push(Action::SetEfficiency(pids, !on));
                    }
                if ui
                    .add_enabled(end.is_some(), egui::Button::new("⛔ End task"))
                    .on_hover_text("Delete")
                    .clicked()
                {
                    actions.extend(end);
                }
                if let Some(e) = expand_all {
                    self.processes.set_all_expanded(&view, e);
                }
                self.settings.group_by_type = group;
                self.settings.show_kernel_threads = kernel;
                self.settings.heatmap = heat;
                if details {
                    self.settings.details_columns = cols;
                } else {
                    self.settings.processes_columns = cols;
                }
            }
            _ => {}
        }
        if self.page != Page::Settings
            && ui
                .button("▶ Run new task")
                .on_hover_text("Ctrl+N")
                .clicked()
        {
            actions.push(Action::RunTaskDialog);
        }
    }

    fn more_menu(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.menu_button(RichText::new("…").size(18.0), |ui| {
            if ui.button("Refresh now  (F5)").clicked() {
                actions.push(Action::RefreshNow);
                ui.close();
            }
            ui.menu_button("Update speed", |ui| {
                for s in UpdateSpeed::ALL {
                    if ui
                        .radio(self.settings.update_speed == s, s.label())
                        .clicked()
                    {
                        self.settings.update_speed = s;
                        ui.close();
                    }
                }
            });
            ui.checkbox(&mut self.settings.always_on_top, "Always on top");
            ui.separator();
            if ui.button("Keyboard shortcuts").clicked() {
                self.dialogs.shortcuts = true;
                ui.close();
            }
            if ui.button("About Vigil").clicked() {
                self.dialogs.about = true;
                ui.close();
            }
            ui.separator();
            if ui.button("Quit  (Ctrl+Q)").clicked() {
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let s = &self.snap;
                    let mem_pct = s.mem.used as f64 / s.mem.total.max(1) as f64 * 100.0;
                    let weak = |t: String| RichText::new(t).small().weak();
                    ui.label(weak(format!("Processes: {}", s.sys.processes)));
                    ui.separator();
                    ui.label(weak(format!("CPU: {:.0}%", s.cpu.total)));
                    ui.separator();
                    ui.label(weak(format!("Memory: {mem_pct:.0}%")));
                    ui.separator();
                    ui.label(weak(format!(
                        "Up time: {}",
                        crate::util::fmt_uptime(s.sys.uptime_secs)
                    )));
                    if self.settings.update_speed == UpdateSpeed::Paused {
                        ui.separator();
                        ui.label(
                            RichText::new("⏸ Updates paused")
                                .small()
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                    if self.running_jobs > 0 {
                        ui.separator();
                        ui.spinner();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some((msg, at, err)) = &self.status {
                            let age = at.elapsed();
                            if age < Duration::from_secs(6) {
                                let color = if *err {
                                    ui.visuals().error_fg_color
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(msg).small().color(color))
                                        .truncate(),
                                );
                                ui.ctx().request_repaint_after(Duration::from_secs(6) - age);
                            }
                        }
                    });
                });
            });
    }
}

impl eframe::App for VigilApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_settings(&ctx);
        let mut actions = Vec::new();
        self.keyboard(&ctx, &mut actions);

        // Lazy loading of slow pages.
        if self.page == Page::Startup && self.startup.list.is_none() && !self.startup.loading {
            actions.push(Action::RefreshStartup);
        }
        if self.page == Page::Services
            && self
                .services
                .needs_refresh(self.settings.show_user_services)
        {
            actions.push(Action::RefreshServices);
        }
        if self.page == Page::Services {
            // Periodic refresh while visible.
            ctx.request_repaint_after(Duration::from_secs(10));
        }

        self.nav_rail(ui);
        self.header(ui, &mut actions);
        self.status_bar(ui);

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if !self.has_data && self.page != Page::Settings {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
                return;
            }
            let snap = self.snap.clone();
            let view = View {
                snap: &snap,
                history: &self.history,
                app_history: &self.app_history,
                settings: &self.settings,
                search: &self.search_lower,
            };
            match self.page {
                Page::Processes => {
                    ui::processes::show(ui, &mut self.processes, &view, &mut actions)
                }
                Page::Performance => {
                    let mut logical = self.settings.cpu_logical_view;
                    let mut kernel = self.settings.cpu_show_kernel;
                    ui::performance::show(
                        ui,
                        &mut self.performance,
                        &view,
                        &mut logical,
                        &mut kernel,
                    );
                    self.settings.cpu_logical_view = logical;
                    self.settings.cpu_show_kernel = kernel;
                }
                Page::AppHistory => {
                    let mut all = self.settings.history_all_processes;
                    ui::app_history::show(ui, &mut self.app_hist, &view, &mut all, &mut actions);
                    self.settings.history_all_processes = all;
                }
                Page::Startup => ui::startup::show(ui, &mut self.startup, &view, &mut actions),
                Page::Users => ui::users::show(ui, &mut self.users, &view, &mut actions),
                Page::Details => ui::details::show(ui, &mut self.details, &view, &mut actions),
                Page::Services => {
                    let mut user = self.settings.show_user_services;
                    ui::services::show(ui, &mut self.services, &view, &mut user, &mut actions);
                    self.settings.show_user_services = user;
                }
                Page::Settings => {
                    if ui::settings_page::show(ui, &mut self.settings) {
                        self.settings = Settings::default();
                    }
                }
            }
        });

        for outcome in self.dialogs.show(&ctx, self.can_elevate) {
            self.handle_outcome(outcome);
        }
        for action in actions {
            self.handle_action(action);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
        eframe::set_value(storage, APP_HISTORY_KEY, &self.app_history);
    }

    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(60)
    }
}

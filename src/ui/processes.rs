//! "Processes" page: apps grouped like Windows Task Manager, with heat-map columns.

use super::proc_menu::{app_menu, process_menu};
use super::table::{Col, Sort, cell_label, cmp_f64, cmp_str, header, right_label};
use super::widgets::{heat_color, paint_cell_bg};
use super::{Action, View};
use crate::model::{Priority, ProcKind, ProcSignal, ProcState, Process};
use crate::settings::ProcessColumns;
use crate::util::{contains_ci, fmt_bytes, fmt_percent, fmt_rate, fmt_unix_time};
use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder, TableRow};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    App(Arc<str>),
    Pid(u32),
}

pub struct State {
    pub sort: Sort,
    pub expanded: HashSet<Arc<str>>,
    expanded_gen: u64,
    pub selected: Option<Sel>,
    cache: Option<Cache>,
}

impl Default for State {
    fn default() -> Self {
        State {
            sort: Sort::new(Col::Name, true),
            expanded: HashSet::new(),
            expanded_gen: 0,
            selected: None,
            cache: None,
        }
    }
}

#[derive(PartialEq, Clone)]
struct CacheKey {
    seq: u64,
    sort: Sort,
    search: String,
    group: bool,
    kernel: bool,
    expanded_gen: u64,
}

struct Cache {
    key: CacheKey,
    rows: Vec<Row>,
    apps: Vec<AppAgg>,
}

#[derive(Clone, Copy)]
enum Row {
    Group(ProcKind, usize),
    App(usize),
    Proc { idx: usize, depth: u8 },
}

struct AppAgg {
    id: Arc<str>,
    name: Arc<str>,
    members: Vec<usize>,
    cpu: f32,
    mem: u64,
    disk: Option<f64>,
    gpu: Option<f32>,
    gpu_mem: Option<u64>,
    threads: u32,
    all_stopped: bool,
    efficiency: bool,
    user: Arc<str>,
}

fn proc_matches(p: &Process, search: &str) -> bool {
    search.is_empty()
        || contains_ci(&p.name, search)
        || p.app_name
            .as_deref()
            .is_some_and(|a| contains_ci(a, search))
        || contains_ci(&p.cmdline, search)
        || contains_ci(&p.user, search)
        || p.pid.to_string().starts_with(search)
}

fn disk_of(p: &Process) -> Option<f64> {
    match (p.disk_read, p.disk_write) {
        (None, None) => None,
        (r, w) => Some(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
    }
}

fn add_opt<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

fn cmp_procs(a: &Process, b: &Process, sort: Sort) -> std::cmp::Ordering {
    let ord = match sort.col {
        Col::Name => cmp_str(&a.name, &b.name),
        Col::Pid => a.pid.cmp(&b.pid),
        Col::Status => status_text(a).cmp(status_text(b)),
        Col::User => cmp_str(&a.user, &b.user),
        Col::Cpu => cmp_f64(a.cpu as f64, b.cpu as f64),
        Col::Memory => a.mem.cmp(&b.mem),
        Col::Disk => cmp_f64(disk_of(a).unwrap_or(-1.0), disk_of(b).unwrap_or(-1.0)),
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

fn cmp_apps(a: &AppAgg, b: &AppAgg, sort: Sort) -> std::cmp::Ordering {
    let ord = match sort.col {
        Col::Name | Col::Command => cmp_str(&a.name, &b.name),
        Col::Pid => a.members.len().cmp(&b.members.len()),
        Col::User => cmp_str(&a.user, &b.user),
        Col::Cpu => cmp_f64(a.cpu as f64, b.cpu as f64),
        Col::Memory => a.mem.cmp(&b.mem),
        Col::Disk => cmp_f64(a.disk.unwrap_or(-1.0), b.disk.unwrap_or(-1.0)),
        Col::Gpu => cmp_f64(a.gpu.unwrap_or(-1.0) as f64, b.gpu.unwrap_or(-1.0) as f64),
        Col::GpuMemory => a.gpu_mem.cmp(&b.gpu_mem),
        Col::Threads => a.threads.cmp(&b.threads),
        _ => std::cmp::Ordering::Equal,
    };
    sort.apply(ord.then_with(|| cmp_str(&a.name, &b.name)))
}

pub fn status_text(p: &Process) -> &'static str {
    match p.state {
        ProcState::Stopped => "Suspended",
        ProcState::Zombie => "Zombie",
        ProcState::DiskSleep => "Waiting (I/O)",
        _ if p.efficiency => "Efficiency mode",
        _ => "",
    }
}

impl State {
    fn rebuild(&mut self, view: &View<'_>) {
        let key = CacheKey {
            seq: view.snap.seq,
            sort: self.sort,
            search: view.search.to_owned(),
            group: view.settings.group_by_type,
            kernel: view.settings.show_kernel_threads,
            expanded_gen: self.expanded_gen,
        };
        if self.cache.as_ref().is_some_and(|c| c.key == key) {
            return;
        }
        let procs = &view.snap.procs;
        let search = view.search;

        // Aggregate applications.
        let mut app_index: HashMap<&str, usize> = HashMap::new();
        let mut apps: Vec<AppAgg> = Vec::new();
        let mut loose: Vec<usize> = Vec::new();
        for (i, p) in procs.iter().enumerate() {
            if p.kernel_thread && !view.settings.show_kernel_threads {
                continue;
            }
            if p.kind == ProcKind::App
                && let (Some(id), Some(name)) = (&p.app_id, &p.app_name)
            {
                let ai = *app_index.entry(id).or_insert_with(|| {
                    apps.push(AppAgg {
                        id: id.clone(),
                        name: name.clone(),
                        members: Vec::new(),
                        cpu: 0.0,
                        mem: 0,
                        disk: None,
                        gpu: None,
                        gpu_mem: None,
                        threads: 0,
                        all_stopped: true,
                        efficiency: true,
                        user: p.user.clone(),
                    });
                    apps.len() - 1
                });
                let a = &mut apps[ai];
                a.members.push(i);
                a.cpu += p.cpu;
                a.mem += p.mem;
                a.disk = add_opt(a.disk, disk_of(p));
                a.gpu = add_opt(a.gpu, p.gpu);
                a.gpu_mem = add_opt(a.gpu_mem, p.gpu_mem);
                a.threads += p.threads;
                a.all_stopped &= p.state == ProcState::Stopped;
                a.efficiency &= p.efficiency;
                continue;
            }
            if proc_matches(p, search) {
                loose.push(i);
            }
        }
        for a in &mut apps {
            a.gpu = a.gpu.map(|g| g.min(100.0));
            a.cpu = a.cpu.min(100.0);
        }
        // Search: keep apps whose name or any member matches.
        let visible_apps: Vec<usize> = (0..apps.len())
            .filter(|&ai| {
                search.is_empty()
                    || contains_ci(&apps[ai].name, search)
                    || apps[ai]
                        .members
                        .iter()
                        .any(|&m| proc_matches(&procs[m], search))
            })
            .collect();

        let sort = self.sort;
        let mut sorted_apps = visible_apps;
        sorted_apps.sort_by(|&a, &b| cmp_apps(&apps[a], &apps[b], sort));
        for a in &mut apps {
            a.members
                .sort_by(|&x, &y| cmp_procs(&procs[x], &procs[y], sort));
        }
        loose.sort_by(|&a, &b| cmp_procs(&procs[a], &procs[b], sort));

        let mut rows = Vec::with_capacity(loose.len() + sorted_apps.len() + 3);
        let push_app =
            |rows: &mut Vec<Row>, ai: usize, apps: &Vec<AppAgg>, expanded: &HashSet<Arc<str>>| {
                rows.push(Row::App(ai));
                if expanded.contains(&apps[ai].id) {
                    for &m in &apps[ai].members {
                        rows.push(Row::Proc { idx: m, depth: 1 });
                    }
                }
            };
        if view.settings.group_by_type {
            if !sorted_apps.is_empty() {
                rows.push(Row::Group(ProcKind::App, sorted_apps.len()));
                for &ai in &sorted_apps {
                    push_app(&mut rows, ai, &apps, &self.expanded);
                }
            }
            for kind in [ProcKind::App, ProcKind::Background, ProcKind::System] {
                let members: Vec<usize> = loose
                    .iter()
                    .copied()
                    .filter(|&i| procs[i].kind == kind)
                    .collect();
                if members.is_empty() {
                    continue;
                }
                if kind == ProcKind::App {
                    // Apps without a resolvable group are listed as plain rows under "Apps".
                    for i in members {
                        rows.push(Row::Proc { idx: i, depth: 0 });
                    }
                    continue;
                }
                rows.push(Row::Group(kind, members.len()));
                rows.extend(members.into_iter().map(|i| Row::Proc { idx: i, depth: 0 }));
            }
        } else {
            // Mixed list: merge apps and loose processes by the sort order.
            enum Item {
                App(usize),
                Proc(usize),
            }
            let mut items: Vec<Item> = sorted_apps.iter().map(|&a| Item::App(a)).collect();
            items.extend(loose.iter().map(|&i| Item::Proc(i)));
            items.sort_by(|a, b| {
                let key = |it: &Item| -> (f64, String) {
                    match it {
                        Item::App(a) => {
                            let a = &apps[*a];
                            let v = match sort.col {
                                Col::Cpu => a.cpu as f64,
                                Col::Memory => a.mem as f64,
                                Col::Disk => a.disk.unwrap_or(-1.0),
                                Col::Gpu => a.gpu.unwrap_or(-1.0) as f64,
                                Col::GpuMemory => a.gpu_mem.unwrap_or(0) as f64,
                                Col::Threads => a.threads as f64,
                                _ => 0.0,
                            };
                            (v, a.name.to_lowercase())
                        }
                        Item::Proc(i) => {
                            let p = &procs[*i];
                            let v = match sort.col {
                                Col::Cpu => p.cpu as f64,
                                Col::Memory => p.mem as f64,
                                Col::Disk => disk_of(p).unwrap_or(-1.0),
                                Col::Gpu => p.gpu.unwrap_or(-1.0) as f64,
                                Col::GpuMemory => p.gpu_mem.unwrap_or(0) as f64,
                                Col::Threads => p.threads as f64,
                                Col::Pid => p.pid as f64,
                                _ => 0.0,
                            };
                            (v, p.name.to_lowercase())
                        }
                    }
                };
                let (ka, kb) = (key(a), key(b));
                sort.apply(cmp_f64(ka.0, kb.0).then_with(|| ka.1.cmp(&kb.1)))
            });
            for it in items {
                match it {
                    Item::App(a) => push_app(&mut rows, a, &apps, &self.expanded),
                    Item::Proc(i) => rows.push(Row::Proc { idx: i, depth: 0 }),
                }
            }
        }
        self.cache = Some(Cache { key, rows, apps });
    }

    /// The action for "End task" on the current selection.
    pub fn end_task_action(&self, view: &View<'_>) -> Option<Action> {
        let cache = self.cache.as_ref()?;
        match self.selected.as_ref()? {
            Sel::Pid(pid) => {
                let p = view.snap.procs.iter().find(|p| p.pid == *pid)?;
                Some(Action::Signal {
                    pids: vec![p.pid],
                    sig: ProcSignal::Terminate,
                    target: format!("{} (PID {})", p.name, p.pid),
                })
            }
            Sel::App(id) => {
                let app = cache.apps.iter().find(|a| &a.id == id)?;
                let pids: Vec<u32> = app
                    .members
                    .iter()
                    .filter_map(|&i| view.snap.procs.get(i))
                    .map(|p| p.pid)
                    .collect();
                Some(Action::Signal {
                    target: format!("{} ({} processes)", app.name, pids.len()),
                    pids,
                    sig: ProcSignal::Terminate,
                })
            }
        }
    }

    /// Pids of the current selection with their combined efficiency state.
    pub fn selection_pids(&self, view: &View<'_>) -> Option<(Vec<u32>, bool)> {
        let cache = self.cache.as_ref()?;
        match self.selected.as_ref()? {
            Sel::Pid(pid) => view
                .snap
                .procs
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| (vec![p.pid], p.efficiency)),
            Sel::App(id) => {
                let app = cache.apps.iter().find(|a| &a.id == id)?;
                let pids = app
                    .members
                    .iter()
                    .filter_map(|&i| view.snap.procs.get(i))
                    .map(|p| p.pid)
                    .collect();
                Some((pids, app.efficiency))
            }
        }
    }

    pub fn set_all_expanded(&mut self, view: &View<'_>, expand: bool) {
        self.expanded.clear();
        if expand {
            for p in &view.snap.procs {
                if let Some(id) = &p.app_id {
                    self.expanded.insert(id.clone());
                }
            }
        }
        self.expanded_gen += 1;
    }

    /// Moves the selection up/down by `delta` rows.
    pub fn move_selection(&mut self, view: &View<'_>, delta: i32) {
        let Some(cache) = &self.cache else { return };
        let keys: Vec<Sel> = cache
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::App(a) => Some(Sel::App(cache.apps[*a].id.clone())),
                Row::Proc { idx, .. } => view.snap.procs.get(*idx).map(|p| Sel::Pid(p.pid)),
                Row::Group(..) => None,
            })
            .collect();
        if keys.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|s| keys.iter().position(|k| k == s));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, keys.len() as i32 - 1) as usize,
            None => 0,
        };
        self.selected = Some(keys[next].clone());
    }
}

struct Cells<'a> {
    name: &'a str,
    count: Option<usize>,
    depth: u8,
    expander: Option<bool>,
    pid: Option<u32>,
    status: &'a str,
    user: &'a str,
    cpu: f32,
    mem: u64,
    disk: Option<f64>,
    gpu: Option<f32>,
    gpu_mem: Option<u64>,
    threads: u32,
    nice: Option<i32>,
    rss: Option<u64>,
    virt: Option<u64>,
    started: Option<u64>,
    command: &'a str,
}

pub fn visible_columns(cols: &ProcessColumns) -> Vec<Col> {
    let mut v = vec![Col::Name];
    let flags = [
        (cols.pid, Col::Pid),
        (cols.status, Col::Status),
        (cols.user, Col::User),
        (cols.cpu, Col::Cpu),
        (cols.memory, Col::Memory),
        (cols.disk, Col::Disk),
        (cols.gpu, Col::Gpu),
        (cols.gpu_memory, Col::GpuMemory),
        (cols.threads, Col::Threads),
        (cols.priority, Col::Priority),
        (cols.rss, Col::Rss),
        (cols.virt, Col::Virt),
        (cols.started, Col::Start),
        (cols.command, Col::Command),
    ];
    v.extend(flags.iter().filter(|f| f.0).map(|f| f.1));
    v
}

pub fn column_spec(col: Col) -> Column {
    match col {
        Col::Name => Column::initial(260.0).at_least(120.0).clip(true),
        Col::Pid => Column::initial(64.0).at_least(40.0),
        Col::Status => Column::initial(110.0).at_least(50.0).clip(true),
        Col::User => Column::initial(100.0).at_least(50.0).clip(true),
        Col::Command => Column::remainder().at_least(120.0).clip(true),
        Col::Priority => Column::initial(90.0).at_least(50.0).clip(true),
        Col::Start => Column::initial(140.0).at_least(60.0).clip(true),
        _ => Column::initial(84.0).at_least(50.0).clip(true),
    }
}

pub fn is_numeric(col: Col) -> bool {
    matches!(
        col,
        Col::Pid
            | Col::Cpu
            | Col::Memory
            | Col::Disk
            | Col::Gpu
            | Col::GpuMemory
            | Col::Threads
            | Col::CpuTime
            | Col::Rss
            | Col::Virt
    )
}

pub fn col_title(col: Col) -> &'static str {
    match col {
        Col::Name => "Name",
        Col::Pid => "PID",
        Col::Status => "Status",
        Col::User => "User name",
        Col::Cpu => "CPU",
        Col::Memory => "Memory",
        Col::Disk => "Disk",
        Col::Gpu => "GPU",
        Col::GpuMemory => "GPU memory",
        Col::Threads => "Threads",
        Col::Priority => "Priority",
        Col::Command => "Command line",
        Col::CpuTime => "CPU time",
        Col::Rss => "Working set",
        Col::Virt => "Virtual memory",
        Col::Start => "Started",
        _ => "",
    }
}

pub fn show(ui: &mut Ui, state: &mut State, view: &View<'_>, actions: &mut Vec<Action>) {
    state.rebuild(view);
    let cache = state.cache.take().expect("built above");
    let cols = visible_columns(&view.settings.processes_columns);
    let snap = view.snap;
    let ncpu = snap.info.logical.max(1) as f32;
    let cpu_scale = if view.settings.cpu_per_core_scale {
        ncpu
    } else {
        1.0
    };
    let dark = ui.visuals().dark_mode;
    let heat = view.settings.heatmap;
    let mem_total = snap.mem.total.max(1) as f32;
    let max_disk = snap
        .disks
        .iter()
        .filter_map(|d| d.active)
        .fold(0.0f32, f32::max);
    let max_gpu = snap
        .gpus
        .iter()
        .filter_map(|g| g.util)
        .fold(0.0f32, f32::max);

    let mut toggle_expand: Option<Arc<str>> = None;
    let mut new_selection: Option<Sel> = None;

    let row_h = 24.0;
    let mut sort = state.sort;
    egui::ScrollArea::horizontal()
        .id_salt("processes_h")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .id_salt("processes")
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false]);
            for c in &cols {
                tb = tb.column(column_spec(*c));
            }
            if !cols.contains(&Col::Command) {
                tb = tb.column(Column::remainder().at_least(0.0));
            }
            tb.header(38.0, |mut h| {
                for c in &cols {
                    h.col(|ui| {
                        let summary = match c {
                            Col::Cpu => Some(fmt_percent(snap.cpu.total)),
                            Col::Memory => {
                                Some(fmt_percent(snap.mem.used as f32 / mem_total * 100.0))
                            }
                            Col::Disk if !snap.disks.is_empty() => Some(fmt_percent(max_disk)),
                            Col::Gpu if !snap.gpus.is_empty() => Some(fmt_percent(max_gpu)),
                            _ => None,
                        };
                        header(
                            ui,
                            col_title(*c),
                            summary.as_deref(),
                            *c,
                            &mut sort,
                            is_numeric(*c),
                        );
                    });
                }
                if !cols.contains(&Col::Command) {
                    h.col(|_| {});
                }
            })
            .body(|body| {
                body.rows(row_h, cache.rows.len(), |mut row| {
                    let r = cache.rows[row.index()];
                    match r {
                        Row::Group(kind, n) => {
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(format!("{} ({n})", kind.label()))
                                        .strong()
                                        .color(ui.visuals().strong_text_color()),
                                );
                            });
                            for _ in 1..cols.len() + usize::from(!cols.contains(&Col::Command)) {
                                row.col(|_| {});
                            }
                        }
                        Row::App(ai) => {
                            let a = &cache.apps[ai];
                            let sel = Sel::App(a.id.clone());
                            row.set_selected(state.selected.as_ref() == Some(&sel));
                            let expanded = state.expanded.contains(&a.id);
                            let status = if a.all_stopped {
                                "Suspended"
                            } else if a.efficiency {
                                "Efficiency mode"
                            } else {
                                ""
                            };
                            let cells = Cells {
                                name: &a.name,
                                count: Some(a.members.len()),
                                depth: 0,
                                expander: Some(expanded),
                                pid: None,
                                status,
                                user: &a.user,
                                cpu: a.cpu * cpu_scale,
                                mem: a.mem,
                                disk: a.disk,
                                gpu: a.gpu,
                                gpu_mem: a.gpu_mem,
                                threads: a.threads,
                                nice: None,
                                rss: None,
                                virt: None,
                                started: None,
                                command: "",
                            };
                            let clicked_expander = draw_cells(
                                &mut row, &cols, &cells, dark, heat, mem_total, cpu_scale,
                            );
                            let resp = row.response();
                            if clicked_expander || resp.double_clicked() {
                                toggle_expand = Some(a.id.clone());
                            }
                            if resp.clicked() || resp.secondary_clicked() {
                                new_selection = Some(sel);
                            }
                            let members: Vec<&Process> = a
                                .members
                                .iter()
                                .filter_map(|&i| snap.procs.get(i))
                                .collect();
                            let mut exp = expanded;
                            resp.context_menu(|ui| {
                                app_menu(ui, &a.name, &members, &mut exp, actions);
                            });
                            if exp != expanded {
                                toggle_expand = Some(a.id.clone());
                            }
                        }
                        Row::Proc { idx, depth } => {
                            let Some(p) = snap.procs.get(idx) else {
                                for _ in 0..cols.len() + usize::from(!cols.contains(&Col::Command))
                                {
                                    row.col(|_| {});
                                }
                                return;
                            };
                            let sel = Sel::Pid(p.pid);
                            row.set_selected(state.selected.as_ref() == Some(&sel));
                            let cells = Cells {
                                name: &p.name,
                                count: None,
                                depth,
                                expander: None,
                                pid: Some(p.pid),
                                status: status_text(p),
                                user: &p.user,
                                cpu: p.cpu * cpu_scale,
                                mem: p.mem,
                                disk: disk_of(p),
                                gpu: p.gpu,
                                gpu_mem: p.gpu_mem,
                                threads: p.threads,
                                nice: Some(p.nice),
                                rss: Some(p.rss),
                                virt: Some(p.virt),
                                started: Some(p.start_time),
                                command: &p.cmdline,
                            };
                            draw_cells(&mut row, &cols, &cells, dark, heat, mem_total, cpu_scale);
                            let resp = row.response();
                            if resp.clicked() || resp.secondary_clicked() {
                                new_selection = Some(sel);
                            }
                            if resp.double_clicked() {
                                actions.push(Action::OpenProperties(p.pid, p.name.to_string()));
                            }
                            let resp = if p.cmdline.is_empty() {
                                resp
                            } else {
                                resp.on_hover_text(&*p.cmdline)
                            };
                            resp.context_menu(|ui| process_menu(ui, p, snap, actions, true));
                        }
                    }
                });
            });
        });
    state.cache = Some(cache);
    if sort != state.sort {
        state.sort = sort;
    }
    if let Some(sel) = new_selection {
        state.selected = Some(sel);
    }
    if let Some(id) = toggle_expand {
        if !state.expanded.remove(&id) {
            state.expanded.insert(id);
        }
        state.expanded_gen += 1;
    }
}

/// Draws all cells of a row; returns true if the expand arrow was clicked.
fn draw_cells(
    row: &mut TableRow<'_, '_>,
    cols: &[Col],
    c: &Cells<'_>,
    dark: bool,
    heat: bool,
    mem_total: f32,
    cpu_scale: f32,
) -> bool {
    let mut expander_clicked = false;
    for col in cols {
        row.col(|ui| match col {
            Col::Name => {
                ui.add_space(4.0 + c.depth as f32 * 22.0);
                if let Some(expanded) = c.expander {
                    let arrow = if expanded { "⏷" } else { "⏵" };
                    if ui
                        .add(egui::Button::new(arrow).frame(false).small())
                        .clicked()
                    {
                        expander_clicked = true;
                    }
                } else if c.depth == 0 {
                    ui.add_space(18.0);
                }
                let text = match c.count {
                    Some(n) if n > 1 => format!("{} ({n})", c.name),
                    _ => c.name.to_owned(),
                };
                cell_label(ui, text);
            }
            Col::Pid => right_label(ui, c.pid.map(|p| p.to_string()).unwrap_or_default()),
            Col::Status => {
                let color = if c.status == "Suspended" {
                    ui.visuals().warn_fg_color
                } else {
                    ui.visuals().text_color()
                };
                cell_label(ui, RichText::new(c.status).color(color));
            }
            Col::User => cell_label(ui, c.user),
            Col::Cpu => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.cpu / 100.0 / cpu_scale.max(1.0), dark));
                }
                right_label(ui, format!("{:.1}%", c.cpu));
            }
            Col::Memory => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.mem as f32 / mem_total * 8.0, dark));
                }
                right_label(ui, fmt_bytes(c.mem));
            }
            Col::Disk => {
                if heat {
                    paint_cell_bg(
                        ui,
                        heat_color(
                            c.disk.unwrap_or(0.0) as f32 / (50.0 * 1024.0 * 1024.0),
                            dark,
                        ),
                    );
                }
                right_label(ui, c.disk.map(fmt_rate).unwrap_or_else(|| "—".into()));
            }
            Col::Gpu => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.gpu.unwrap_or(0.0) / 100.0, dark));
                }
                right_label(ui, c.gpu.map(|g| format!("{g:.1}%")).unwrap_or_default());
            }
            Col::GpuMemory => right_label(ui, c.gpu_mem.map(fmt_bytes).unwrap_or_default()),
            Col::Threads => right_label(ui, c.threads.to_string()),
            Col::Priority => cell_label(
                ui,
                c.nice
                    .map(|n| Priority::from_nice(n).short_label())
                    .unwrap_or(""),
            ),
            Col::Rss => right_label(ui, c.rss.map(fmt_bytes).unwrap_or_default()),
            Col::Virt => right_label(ui, c.virt.map(fmt_bytes).unwrap_or_default()),
            Col::Start => cell_label(ui, c.started.map(fmt_unix_time).unwrap_or_default()),
            Col::Command => cell_label(ui, RichText::new(c.command).weak()),
            _ => {}
        });
    }
    if !cols.contains(&Col::Command) {
        row.col(|_| {});
    }
    expander_clicked
}

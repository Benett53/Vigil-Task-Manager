//! "Processes" page: apps grouped like Windows Task Manager with heat-map columns and an
//! optional detail strip for the selected entry.

use super::icons::app_icon;
use super::proc_menu::{app_menu, process_menu};
use super::table::{Col, Sort, cell_label, cmp_f64, cmp_str, header, quiet_lines, right_label};
use super::theme;
use super::widgets::{colors, dim_if_zero, heat_color, paint_cell_bg, pill, sparkline};
use super::{Action, View};
use crate::model::{Priority, ProcKind, ProcSignal, ProcState, Process};
use crate::settings::ProcessColumns;
use crate::util::{contains_ci, fmt_bytes, fmt_cpu_time, fmt_percent, fmt_rate, fmt_unix_time};
use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder, TableRow};
use egui_phosphor::regular as icon;
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
    pub collapsed_groups: HashSet<ProcKind>,
    expanded_gen: u64,
    pub selected: Option<Sel>,
    cache: Option<Cache>,
}

impl Default for State {
    fn default() -> Self {
        State {
            sort: Sort::new(Col::Name, true),
            expanded: HashSet::new(),
            collapsed_groups: HashSet::new(),
            expanded_gen: 0,
            selected: None,
            cache: None,
        }
    }
}

#[derive(PartialEq, Clone)]
struct CacheKey {
    procs_seq: u64,
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
    /// kind, number of entries, summed CPU %, summed memory
    Group(ProcKind, usize, f32, u64),
    App(usize),
    Proc {
        idx: usize,
        depth: u8,
        last: bool,
    },
}

pub struct AppAgg {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub exe: Arc<str>,
    pub members: Vec<usize>,
    pub cpu: f32,
    pub mem: u64,
    pub disk: Option<f64>,
    pub gpu: Option<f32>,
    pub gpu_mem: Option<u64>,
    pub threads: u32,
    pub all_stopped: bool,
    pub efficiency: bool,
    pub user: Arc<str>,
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

/// Name shown on the Processes page: the file description where known (like Task Manager).
pub fn display_name(p: &Process) -> &str {
    p.app_name.as_deref().unwrap_or(&p.name)
}

pub fn disk_of(p: &Process) -> Option<f64> {
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
        Col::Name => cmp_str(display_name(a), display_name(b)),
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
        Col::Memory | Col::Rss => a.mem.cmp(&b.mem),
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

/// Paints a status as a colored badge (or nothing for normal processes).
pub fn status_badge(ui: &mut Ui, status: &str) {
    match status {
        "Suspended" => {
            pill(ui, format!("{} Suspended", icon::PAUSE), colors::WARN);
        }
        "Efficiency mode" => {
            pill(ui, format!("{} Efficiency", icon::LEAF), colors::GOOD);
        }
        "Zombie" => {
            pill(ui, format!("{} Zombie", icon::SKULL), colors::DANGER);
        }
        "" => {}
        other => {
            ui.label(RichText::new(other).weak());
        }
    }
}

/// Aggregated (cpu %, private memory, disk B/s) of a selection, for the detail pane history.
pub fn selection_values(sel: &Sel, procs: &[Process]) -> Option<(f32, u64, f64)> {
    match sel {
        Sel::Pid(pid) => procs
            .iter()
            .find(|p| p.pid == *pid)
            .map(|p| (p.cpu, p.mem, disk_of(p).unwrap_or(0.0))),
        Sel::App(id) => {
            let mut any = false;
            let mut acc = (0.0f32, 0u64, 0.0f64);
            for p in procs.iter().filter(|p| p.app_id.as_ref() == Some(id)) {
                any = true;
                acc.0 += p.cpu;
                acc.1 += p.mem;
                acc.2 += disk_of(p).unwrap_or(0.0);
            }
            any.then_some(acc)
        }
    }
}

impl State {
    fn rebuild(&mut self, view: &View<'_>) {
        let key = CacheKey {
            procs_seq: view.snap.procs_seq,
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
                        exe: p.exe.clone(),
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
        let visible_apps: Vec<usize> = (0..apps.len())
            .filter(|&ai| {
                let a = &apps[ai];
                search.is_empty()
                    || contains_ci(&a.name, search)
                    || a.members.iter().any(|&m| proc_matches(&procs[m], search))
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
                    let n = apps[ai].members.len();
                    for (k, &m) in apps[ai].members.iter().enumerate() {
                        rows.push(Row::Proc {
                            idx: m,
                            depth: 1,
                            last: k + 1 == n,
                        });
                    }
                }
            };
        if view.settings.group_by_type {
            for kind in [ProcKind::App, ProcKind::Background, ProcKind::System] {
                let members: Vec<usize> = loose
                    .iter()
                    .copied()
                    .filter(|&i| procs[i].kind == kind)
                    .collect();
                let app_rows: &[usize] = if kind == ProcKind::App {
                    &sorted_apps
                } else {
                    &[]
                };
                let n = members.len() + app_rows.len();
                if n == 0 {
                    continue;
                }
                let cpu: f32 = members.iter().map(|&i| procs[i].cpu).sum::<f32>()
                    + app_rows.iter().map(|&a| apps[a].cpu).sum::<f32>();
                let mem: u64 = members.iter().map(|&i| procs[i].mem).sum::<u64>()
                    + app_rows.iter().map(|&a| apps[a].mem).sum::<u64>();
                rows.push(Row::Group(kind, n, cpu.min(100.0), mem));
                if self.collapsed_groups.contains(&kind) {
                    continue;
                }
                for &ai in app_rows {
                    push_app(&mut rows, ai, &apps, &self.expanded);
                }
                rows.extend(members.into_iter().map(|i| Row::Proc {
                    idx: i,
                    depth: 0,
                    last: false,
                }));
            }
        } else {
            // Mixed list: merge apps and loose processes by the sort order.
            enum Item {
                App(usize),
                Proc(usize),
            }
            let mut items: Vec<Item> = sorted_apps.iter().map(|&a| Item::App(a)).collect();
            items.extend(loose.iter().map(|&i| Item::Proc(i)));
            let key = |it: &Item| -> (f64, String) {
                let (v, name) = match it {
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
                        (v, &*a.name)
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
                        (v, &*p.name)
                    }
                };
                (v, name.to_lowercase())
            };
            let mut keyed: Vec<((f64, String), Item)> =
                items.into_iter().map(|it| (key(&it), it)).collect();
            keyed.sort_by(|(ka, _), (kb, _)| {
                sort.apply(cmp_f64(ka.0, kb.0).then_with(|| ka.1.cmp(&kb.1)))
            });
            for (_, it) in keyed {
                match it {
                    Item::App(a) => push_app(&mut rows, a, &apps, &self.expanded),
                    Item::Proc(i) => rows.push(Row::Proc {
                        idx: i,
                        depth: 0,
                        last: false,
                    }),
                }
            }
        }
        self.cache = Some(Cache { key, rows, apps });
    }

    fn selected_app(&self) -> Option<&AppAgg> {
        let cache = self.cache.as_ref()?;
        match self.selected.as_ref()? {
            Sel::App(id) => cache.apps.iter().find(|a| &a.id == id),
            Sel::Pid(_) => None,
        }
    }

    /// The action for "End task" on the current selection.
    pub fn end_task_action(&self, view: &View<'_>) -> Option<Action> {
        match self.selected.as_ref()? {
            Sel::Pid(pid) => {
                let p = view.snap.procs.iter().find(|p| p.pid == *pid)?;
                Some(Action::Signal {
                    pids: vec![p.pid],
                    sig: ProcSignal::Terminate,
                    target: format!("{} (PID {})", p.name, p.pid),
                })
            }
            Sel::App(_) => {
                let app = self.selected_app()?;
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
        match self.selected.as_ref()? {
            Sel::Pid(pid) => view
                .snap
                .procs
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| (vec![p.pid], p.efficiency)),
            Sel::App(_) => {
                let app = self.selected_app()?;
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
        self.collapsed_groups.clear();
        if expand {
            for p in view.snap.procs.iter() {
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
    exe: &'a Arc<str>,
    count: Option<usize>,
    depth: u8,
    last: bool,
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
    muted: bool,
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
        Col::Name => Column::initial(320.0).at_least(160.0).clip(true),
        Col::Pid => Column::initial(64.0).at_least(40.0),
        Col::Status => Column::initial(108.0).at_least(50.0).clip(true),
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

    // Detail strip for the selected entry.
    if view.settings.show_process_details
        && let Some(sel) = state.selected.clone()
    {
        egui::Panel::bottom("process_detail")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 12,
                bottom: 4,
            }))
            .show(ui, |ui| detail_pane(ui, state, &sel, view));
    }

    let cache = state.cache.take().expect("built above");
    let snap = view.snap;
    let mut cols = visible_columns(&view.settings.processes_columns);
    // GPU columns stay empty on machines without GPU accounting.
    if snap.gpus.is_empty() && !snap.procs.iter().any(|p| p.gpu.is_some()) {
        cols.retain(|c| !matches!(c, Col::Gpu | Col::GpuMemory));
    }
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
    // Without a command line column the name column takes the spare width, so the
    // numbers line up at the right edge of the window.
    let name_fills = !cols.contains(&Col::Command);

    let mut toggle_expand: Option<Arc<str>> = None;
    let mut toggle_group: Option<ProcKind> = None;
    let mut new_selection: Option<Sel> = None;
    let mut sort = state.sort;

    ui.scope(|ui| {
        quiet_lines(ui);
        let mut tb = TableBuilder::new(ui)
            .id_salt("processes_table")
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink([false, false]);
        for c in &cols {
            tb = tb.column(if name_fills && *c == Col::Name {
                Column::remainder()
                    .at_least(180.0)
                    .clip(true)
                    .resizable(false)
            } else {
                column_spec(*c)
            });
        }
        tb.header(44.0, |mut h| {
            for c in &cols {
                h.col(|ui| {
                    header_rule(ui, dark);
                    let summary = match c {
                        Col::Cpu => Some(fmt_percent(snap.cpu.total)),
                        Col::Memory => Some(fmt_percent(snap.mem.used as f32 / mem_total * 100.0)),
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
        })
        .body(|body| {
            let heights = cache.rows.iter().map(|r| {
                if matches!(r, Row::Group(..)) {
                    36.0
                } else {
                    30.0
                }
            });
            body.heterogeneous_rows(heights, |mut row| {
                let r = cache.rows[row.index()];
                match r {
                    Row::Group(kind, n, cpu, mem) => {
                        let collapsed = state.collapsed_groups.contains(&kind);
                        for (i, c) in cols.iter().enumerate() {
                            row.col(|ui| {
                                if i == 0 {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    let caret = if collapsed {
                                        icon::CARET_RIGHT
                                    } else {
                                        icon::CARET_DOWN
                                    };
                                    ui.label(RichText::new(caret).size(11.0).weak());
                                    ui.label(
                                        RichText::new(kind.label())
                                            .font(theme::semibold(12.5))
                                            .color(ui.visuals().strong_text_color()),
                                    );
                                    ui.label(RichText::new(n.to_string()).small().weak());
                                } else if *c == Col::Cpu {
                                    right_label(
                                        ui,
                                        RichText::new(format!("{:.1}%", cpu * cpu_scale))
                                            .small()
                                            .weak(),
                                    );
                                } else if *c == Col::Memory {
                                    right_label(ui, RichText::new(fmt_bytes(mem)).small().weak());
                                }
                            });
                        }
                        let resp = row.response();
                        if resp.clicked() {
                            toggle_group = Some(kind);
                        }
                        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
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
                            exe: &a.exe,
                            count: Some(a.members.len()),
                            depth: 0,
                            last: false,
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
                            muted: false,
                        };
                        let clicked_expander = draw_cells(
                            &mut row, &cols, &cells, view, dark, heat, mem_total, cpu_scale,
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
                    Row::Proc { idx, depth, last } => {
                        let Some(p) = snap.procs.get(idx) else {
                            for _ in 0..cols.len() {
                                row.col(|_| {});
                            }
                            return;
                        };
                        let sel = Sel::Pid(p.pid);
                        row.set_selected(state.selected.as_ref() == Some(&sel));
                        let cells = Cells {
                            name: display_name(p),
                            exe: &p.exe,
                            count: None,
                            depth,
                            last,
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
                            muted: p.kind == ProcKind::System,
                        };
                        draw_cells(
                            &mut row, &cols, &cells, view, dark, heat, mem_total, cpu_scale,
                        );
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
    if let Some(kind) = toggle_group {
        if !state.collapsed_groups.remove(&kind) {
            state.collapsed_groups.insert(kind);
        }
        state.expanded_gen += 1;
    }
}

/// Hairline under a header cell, spanning the gap to the next cell.
pub fn header_rule(ui: &Ui, dark: bool) {
    let r = ui.max_rect();
    let half_gap = ui.spacing().item_spacing.x * 0.5;
    ui.painter().hline(
        (r.left() - half_gap)..=(r.right() + half_gap),
        r.bottom() + 2.5,
        egui::Stroke::new(1.0, theme::hairline(dark)),
    );
}

/// Draws all cells of a row; returns true if the expand arrow was clicked.
#[allow(clippy::too_many_arguments)]
fn draw_cells(
    row: &mut TableRow<'_, '_>,
    cols: &[Col],
    c: &Cells<'_>,
    view: &View<'_>,
    dark: bool,
    heat: bool,
    mem_total: f32,
    cpu_scale: f32,
) -> bool {
    let mut expander_clicked = false;
    for col in cols {
        row.col(|ui| match col {
            Col::Name => {
                ui.spacing_mut().item_spacing.x = 8.0;
                if c.depth > 0 {
                    // Tree guide for child processes of an app.
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(26.0, ui.available_height()), Sense::hover());
                    let x = rect.left() + 9.5;
                    let stroke = egui::Stroke::new(1.0, theme::hairline(dark).gamma_multiply(1.6));
                    let bottom = if c.last {
                        rect.center().y
                    } else {
                        rect.bottom() + 3.0
                    };
                    ui.painter().vline(x, rect.top() - 3.0..=bottom, stroke);
                    ui.painter()
                        .hline(x..=rect.right() - 2.0, rect.center().y, stroke);
                } else if let Some(expanded) = c.expander {
                    let caret = if expanded {
                        icon::CARET_DOWN
                    } else {
                        icon::CARET_RIGHT
                    };
                    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    let color = if resp.hovered() {
                        ui.visuals().strong_text_color()
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        caret,
                        egui::FontId::proportional(11.0),
                        color,
                    );
                    if resp.clicked() {
                        expander_clicked = true;
                    }
                } else {
                    ui.add_space(18.0);
                }
                app_icon(ui, view.icons, c.exe, 16.0, c.muted);
                let text = if c.muted {
                    RichText::new(c.name).color(ui.visuals().text_color().gamma_multiply(0.8))
                } else {
                    RichText::new(c.name).color(ui.visuals().strong_text_color())
                };
                cell_label(ui, text);
                if let Some(n) = c.count.filter(|n| *n > 1) {
                    ui.label(RichText::new(format!("({n})")).weak());
                }
            }
            Col::Pid => right_label(
                ui,
                RichText::new(c.pid.map(|p| p.to_string()).unwrap_or_default()).weak(),
            ),
            Col::Status => status_badge(ui, c.status),
            Col::User => cell_label(ui, RichText::new(c.user).weak()),
            Col::Cpu => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.cpu / 100.0 / cpu_scale.max(1.0), dark));
                }
                right_label(ui, dim_if_zero(ui, format!("{:.1}%", c.cpu), c.cpu < 0.05));
            }
            Col::Memory => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.mem as f32 / mem_total * 5.0, dark));
                }
                right_label(ui, dim_if_zero(ui, fmt_bytes(c.mem), c.mem == 0));
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
                match c.disk {
                    Some(d) => right_label(ui, dim_if_zero(ui, fmt_rate(d), d < 1.0)),
                    None => right_label(ui, RichText::new("—").weak()),
                }
            }
            Col::Gpu => {
                if heat {
                    paint_cell_bg(ui, heat_color(c.gpu.unwrap_or(0.0) / 100.0, dark));
                }
                if let Some(g) = c.gpu {
                    right_label(ui, dim_if_zero(ui, format!("{g:.1}%"), g < 0.05));
                }
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
    expander_clicked
}

/// Compact strip with live graphs for the selected process or app.
fn detail_pane(ui: &mut Ui, state: &mut State, sel: &Sel, view: &View<'_>) {
    let snap = view.snap;
    let dark = ui.visuals().dark_mode;
    let (title, members): (String, Vec<&Process>) = match sel {
        Sel::Pid(pid) => match snap.procs.iter().find(|p| p.pid == *pid) {
            Some(p) => (p.name.to_string(), vec![p]),
            None => (String::new(), Vec::new()),
        },
        Sel::App(id) => {
            let m: Vec<&Process> = snap
                .procs
                .iter()
                .filter(|p| p.app_id.as_ref() == Some(id))
                .collect();
            let name = m
                .first()
                .and_then(|p| p.app_name.as_deref())
                .unwrap_or("")
                .to_owned();
            (name, m)
        }
    };
    let top = ui.max_rect().top() - 6.0;
    ui.painter().hline(
        ui.max_rect().x_range(),
        top,
        egui::Stroke::new(1.0, theme::hairline(dark)),
    );
    let Some(main) = members.iter().min_by_key(|p| p.pid).copied() else {
        ui.horizontal(|ui| {
            ui.label(RichText::new("The selected process has exited.").weak());
            if ui.small_button("Clear selection").clicked() {
                state.selected = None;
            }
        });
        return;
    };
    let cpu: f32 = members.iter().map(|p| p.cpu).sum::<f32>().min(100.0);
    let mem: u64 = members.iter().map(|p| p.mem).sum();
    let threads: u32 = members.iter().map(|p| p.threads).sum();
    let disk: f64 = members.iter().filter_map(|p| disk_of(p)).sum();
    let hist = view.proc_history;
    let cap = view.settings.graph_points;

    let spark_w = 132.0;
    let graphs_w = 3.0 * (spark_w + 16.0);
    let height = 52.0;
    ui.horizontal(|ui| {
        let left_w = (ui.available_width() - graphs_w).max(160.0);
        ui.allocate_ui_with_layout(
            vec2(left_w, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                app_icon(ui, view.icons, &main.exe, 28.0, false);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&title)
                                .font(theme::semibold(14.5))
                                .color(ui.visuals().strong_text_color()),
                        );
                        status_badge(ui, status_text(main));
                    });
                    let who = if members.len() > 1 {
                        format!("{} processes", members.len())
                    } else {
                        format!("PID {}", main.pid)
                    };
                    let facts = format!(
                        "{who}  ·  {}  ·  {threads} threads  ·  CPU time {}  ·  started {}",
                        main.user,
                        fmt_cpu_time(members.iter().map(|p| p.cpu_time).sum()),
                        fmt_unix_time(main.start_time)
                    );
                    ui.add(egui::Label::new(RichText::new(facts).small().weak()).truncate());
                    let path = if main.exe.is_empty() {
                        &main.cmdline
                    } else {
                        &main.exe
                    };
                    ui.add(egui::Label::new(RichText::new(&**path).small().weak()).truncate());
                });
            },
        );
        for (label, value, series, max, color) in [
            (
                "CPU",
                format!("{cpu:.1}%"),
                &hist.cpu,
                100.0f32,
                colors::CPU,
            ),
            (
                "Memory",
                fmt_bytes(mem),
                &hist.mem,
                hist.mem.max().max(1.0) * 1.2,
                colors::MEMORY,
            ),
            (
                "Disk",
                fmt_rate(disk),
                &hist.disk,
                hist.disk.max().max(1024.0) * 1.2,
                colors::DISK,
            ),
        ] {
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).small().weak());
                    ui.label(
                        RichText::new(value)
                            .small()
                            .color(ui.visuals().strong_text_color()),
                    );
                });
                sparkline(ui, vec2(spark_w, 30.0), series, None, max, cap, color);
            });
        }
    });
}

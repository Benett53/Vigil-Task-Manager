//! "Performance" page: device list with sparklines and a detail view with large graphs,
//! temperatures, sensors and batteries.

use super::View;
use super::widgets::{
    Line, card, colors, fmt_temp, graph, meter, sparkline, stacked_bar, temp_color,
};
use crate::history::{DeviceHistory, Series};
use crate::model::{BatterySample, DiskSample, GpuSample, NetSample, SensorKind};
use crate::util::{fmt_bits, fmt_bytes, fmt_freq_mhz, fmt_percent, fmt_rate, fmt_uptime};
use egui::{Color32, RichText, ScrollArea, Sense, Ui, vec2};
use egui_phosphor::regular as icon;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PerfSel {
    Cpu,
    Memory,
    Disk(String),
    Net(String),
    Gpu(String),
    Battery(String),
    Sensors,
}

pub struct State {
    pub selected: PerfSel,
    /// Device kind to select once devices are known (from `--mini`).
    pub pending_kind: Option<DeviceKind>,
}

impl Default for State {
    fn default() -> Self {
        State {
            selected: PerfSel::Cpu,
            pending_kind: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
    Battery,
    Sensors,
}

impl DeviceKind {
    pub fn from_cli(name: &str) -> Option<DeviceKind> {
        Some(match name.to_ascii_lowercase().as_str() {
            "cpu" => DeviceKind::Cpu,
            "memory" | "mem" | "ram" => DeviceKind::Memory,
            "disk" => DeviceKind::Disk,
            "network" | "net" => DeviceKind::Network,
            "gpu" => DeviceKind::Gpu,
            "battery" => DeviceKind::Battery,
            "sensors" => DeviceKind::Sensors,
            _ => return None,
        })
    }

    fn matches(self, sel: &PerfSel) -> bool {
        matches!(
            (self, sel),
            (DeviceKind::Cpu, PerfSel::Cpu)
                | (DeviceKind::Memory, PerfSel::Memory)
                | (DeviceKind::Disk, PerfSel::Disk(_))
                | (DeviceKind::Network, PerfSel::Net(_))
                | (DeviceKind::Gpu, PerfSel::Gpu(_))
                | (DeviceKind::Battery, PerfSel::Battery(_))
                | (DeviceKind::Sensors, PerfSel::Sensors)
        )
    }
}

/// Size of the mini view window, in points.
pub const GRAPH_WINDOW_SIZE: [f32; 2] = [420.0, 260.0];

/// Rounds up to a "nice" axis maximum (1, 2, 5 x 10^n).
pub fn nice_max(v: f32, floor: f32) -> f32 {
    let v = v.max(floor);
    let exp = v.log10().floor();
    let base = 10f32.powf(exp);
    for m in [1.0, 2.0, 5.0, 10.0] {
        if v <= m * base {
            return m * base;
        }
    }
    10.0 * base
}

fn visible_nets<'a>(view: &View<'a>) -> Vec<&'a NetSample> {
    let all = &view.snap.nets;
    let physical: Vec<&NetSample> = all.iter().filter(|n| !n.is_virtual).collect();
    if view.settings.show_virtual_adapters || physical.is_empty() {
        all.iter()
            .filter(|n| !n.is_virtual || n.connected || view.settings.show_virtual_adapters)
            .collect()
    } else {
        physical
    }
}

fn disk_title(i: usize, d: &DiskSample) -> String {
    let mounts: Vec<&str> = d.mounts.iter().take(2).map(String::as_str).collect();
    if mounts.is_empty() {
        format!("Disk {i}")
    } else {
        format!("Disk {i} ({})", mounts.join(", "))
    }
}

/// "Wi-Fi", or "Ethernet (Office LAN)" when the adapter has its own name.
fn net_title(n: &NetSample) -> String {
    if n.id == n.kind || n.id.is_empty() {
        n.kind.clone()
    } else {
        format!("{} ({})", n.kind, n.id)
    }
}

fn net_icon(n: &NetSample) -> &'static str {
    match n.kind.as_str() {
        "Wi-Fi" => icon::WIFI_HIGH,
        "Bluetooth" => icon::BLUETOOTH,
        "VPN / Tunnel" => icon::SHIELD_CHECK,
        _ => icon::NETWORK,
    }
}

fn battery_icon(b: &BatterySample) -> &'static str {
    if b.status == "Charging" {
        icon::BATTERY_CHARGING
    } else if b.percent > 75.0 {
        icon::BATTERY_FULL
    } else if b.percent > 40.0 {
        icon::BATTERY_HIGH
    } else if b.percent > 15.0 {
        icon::BATTERY_LOW
    } else {
        icon::BATTERY_WARNING
    }
}

fn seconds_per_point(view: &View<'_>) -> f32 {
    view.settings.update_speed.interval().as_secs_f32()
}

/// The two small window modes of the page (Task Manager's "summary view").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compact {
    /// Only the device list.
    Summary,
    /// Only the graph of the selected device.
    Graph,
}

impl Compact {
    /// Window title; distinct per mode so window managers remember each size separately.
    pub fn window_title(self) -> &'static str {
        match self {
            Compact::Summary => "Vigil – Summary",
            Compact::Graph => "Vigil – Mini view",
        }
    }
}

/// What the page asks the window to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Compact(Compact),
    FullView,
    ToggleOnTop,
}

/// One entry of the device list, with what its sparkline and mini view draw.
struct Device<'a> {
    spec: CardSpec,
    series: &'a Series,
    series2: Option<&'a Series>,
    max: f32,
}

fn devices<'a>(view: &View<'a>) -> Vec<Device<'a>> {
    let snap = view.snap;
    let h = view.history;
    let mut out = Vec::new();
    let freq = snap.cpu.freq_mhz.map(fmt_freq_mhz).unwrap_or_default();
    out.push(Device {
        spec: CardSpec {
            sel: PerfSel::Cpu,
            icon: icon::CPU,
            title: "CPU".into(),
            value: fmt_percent(snap.cpu.total),
            subtitle: freq,
            color: colors::CPU,
        },
        series: &h.cpu,
        series2: None,
        max: 100.0,
    });
    out.push(Device {
        spec: CardSpec {
            sel: PerfSel::Memory,
            icon: icon::MEMORY,
            title: "Memory".into(),
            value: fmt_percent(snap.mem.used as f32 / snap.mem.total.max(1) as f32 * 100.0),
            subtitle: format!(
                "{} / {}",
                fmt_bytes(snap.mem.used),
                fmt_bytes(snap.mem.total)
            ),
            color: colors::MEMORY,
        },
        series: &h.mem,
        series2: None,
        max: 100.0,
    });
    for (i, d) in snap.disks.iter().enumerate() {
        if let Some(dh) = h.disks.get(&d.id) {
            out.push(Device {
                spec: CardSpec {
                    sel: PerfSel::Disk(d.id.clone()),
                    icon: icon::HARD_DRIVES,
                    title: disk_title(i, d),
                    value: fmt_percent(d.active.unwrap_or(0.0)),
                    subtitle: d.kind.clone(),
                    color: colors::DISK,
                },
                series: &dh.a,
                series2: None,
                max: 100.0,
            });
        }
    }
    for n in visible_nets(view) {
        if let Some(nh) = h.nets.get(&n.id) {
            let sub = if n.connected {
                format!(
                    "{} {}  {} {}",
                    icon::ARROW_DOWN,
                    fmt_bits(n.rx_bps),
                    icon::ARROW_UP,
                    fmt_bits(n.tx_bps)
                )
            } else {
                "Not connected".into()
            };
            out.push(Device {
                spec: CardSpec {
                    sel: PerfSel::Net(n.id.clone()),
                    icon: net_icon(n),
                    title: net_title(n),
                    value: String::new(),
                    subtitle: sub,
                    color: colors::NETWORK,
                },
                series: &nh.a,
                series2: Some(&nh.b),
                max: nice_max(nh.a.max().max(nh.b.max()), 12_500.0),
            });
        }
    }
    for (i, g) in snap.gpus.iter().enumerate() {
        if let Some(gh) = h.gpus.get(&g.id) {
            out.push(Device {
                spec: CardSpec {
                    sel: PerfSel::Gpu(g.id.clone()),
                    icon: icon::GRAPHICS_CARD,
                    title: format!("GPU {i}"),
                    value: g.util.map(fmt_percent).unwrap_or_default(),
                    subtitle: g.name.clone(),
                    color: colors::GPU,
                },
                series: &gh.a,
                series2: None,
                max: 100.0,
            });
        }
    }
    for b in &snap.batteries {
        if let Some(bh) = h.batteries.get(&b.id) {
            out.push(Device {
                spec: CardSpec {
                    sel: PerfSel::Battery(b.id.clone()),
                    icon: battery_icon(b),
                    title: "Battery".into(),
                    value: format!("{:.0}%", b.percent),
                    subtitle: b.status.clone(),
                    color: colors::BATTERY,
                },
                series: &bh.a,
                series2: None,
                max: 100.0,
            });
        }
    }
    if !snap.sensors.is_empty() {
        let hottest = snap
            .sensors
            .iter()
            .filter(|s| s.kind == SensorKind::Temperature)
            .map(|s| s.value)
            .reduce(f32::max);
        let fans = snap
            .sensors
            .iter()
            .filter(|s| s.kind == SensorKind::Fan)
            .count();
        let sub = if fans > 0 {
            format!("{} readings, {fans} fans", snap.sensors.len())
        } else {
            format!("{} readings", snap.sensors.len())
        };
        out.push(Device {
            spec: CardSpec {
                sel: PerfSel::Sensors,
                icon: icon::THERMOMETER,
                title: "Sensors".into(),
                value: hottest.map(fmt_temp).unwrap_or_default(),
                subtitle: sub,
                color: colors::SENSORS,
            },
            series: &h.cpu_temp,
            series2: None,
            max: 100.0,
        });
    }
    out
}

/// Spacing between the device list, the separator and the details.
const GUTTER: i8 = 12;
/// Height of one entry in the device list (for sizing the summary window).
const CARD_HEIGHT: f32 = 92.0;

pub fn show(
    ui: &mut Ui,
    state: &mut State,
    view: &View<'_>,
    settings_cpu_logical: &mut bool,
    settings_cpu_kernel: &mut bool,
) -> Option<Request> {
    let devices = devices(view);
    if !devices.iter().any(|d| d.spec.sel == state.selected) {
        state.selected = PerfSel::Cpu;
    }
    let mut request = None;

    egui::Panel::left("perf_devices")
        .resizable(true)
        .default_size(258.0)
        .min_size(190.0)
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            left: 0,
            right: GUTTER,
            top: 0,
            bottom: 0,
        }))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for d in devices {
                        let sel = d.spec.sel.clone();
                        let resp = card_entry(ui, state, d, view.settings.graph_points);
                        if resp.double_clicked() {
                            state.selected = sel.clone();
                            request = Some(Request::Compact(Compact::Graph));
                        }
                        resp.on_hover_text_at_pointer("Double-click for a mini view")
                            .context_menu(|ui| {
                                if ui
                                    .button(format!("{} Mini view", icon::PICTURE_IN_PICTURE))
                                    .clicked()
                                {
                                    state.selected = sel.clone();
                                    request = Some(Request::Compact(Compact::Graph));
                                }
                                if ui
                                    .button(format!("{} Summary view", icon::CORNERS_IN))
                                    .clicked()
                                {
                                    request = Some(Request::Compact(Compact::Summary));
                                }
                            });
                    }
                    ui.add_space(8.0);
                });
        });

    egui::CentralPanel::no_frame()
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            left: GUTTER + 4,
            ..Default::default()
        }))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Keep clear of the (overlay) scroll bar, as far as from the separator.
                    ui.set_max_width(ui.available_width() - 16.0);
                    let top_right = ui.max_rect().right_top();
                    show_selected(ui, state, view, settings_cpu_logical, settings_cpu_kernel);
                    ui.add_space(8.0);
                    let button = egui::Rect::from_min_max(
                        top_right + vec2(-28.0, 0.0),
                        top_right + vec2(0.0, 28.0),
                    );
                    if ui
                        .put(
                            button,
                            egui::Button::new(RichText::new(icon::PICTURE_IN_PICTURE).size(16.0))
                                .frame(false),
                        )
                        .on_hover_text("Mini view: only this graph in a small window")
                        .clicked()
                    {
                        request = Some(Request::Compact(Compact::Graph));
                    }
                });
        });
    request
}

/// Size of the window for a compact mode, in points.
pub fn compact_size(mode: Compact, view: &View<'_>) -> egui::Vec2 {
    match mode {
        Compact::Graph => GRAPH_WINDOW_SIZE.into(),
        Compact::Summary => {
            let n = devices(view).len() as f32;
            vec2(280.0, (n * (CARD_HEIGHT + 4.0) + 16.0).clamp(220.0, 900.0))
        }
    }
}

/// The page in a small window: the device list or a single graph.
pub fn show_compact(
    ui: &mut Ui,
    state: &mut State,
    view: &View<'_>,
    mode: Compact,
    on_top: bool,
) -> Option<Request> {
    let devices = devices(view);
    // Wait until a device of that kind appears (disks and adapters come with the first sample).
    if let Some(kind) = state.pending_kind {
        // Network: prefer a connected adapter.
        let connected = |d: &&Device<'_>| match &d.spec.sel {
            PerfSel::Net(id) => view.snap.nets.iter().any(|n| &n.id == id && n.connected),
            _ => true,
        };
        if let Some(d) = devices
            .iter()
            .filter(|d| kind.matches(&d.spec.sel))
            .find(connected)
            .or_else(|| devices.iter().find(|d| kind.matches(&d.spec.sel)))
        {
            state.selected = d.spec.sel.clone();
            state.pending_kind = None;
        }
    }
    if !devices.iter().any(|d| d.spec.sel == state.selected) {
        state.selected = PerfSel::Cpu;
    }
    let mut request = None;
    let menu = |ui: &mut Ui, state: &mut State, request: &mut Option<Request>| {
        if ui
            .button(format!("{} Full view  (Esc)", icon::CORNERS_OUT))
            .clicked()
        {
            *request = Some(Request::FullView);
        }
        let (other, label) = match mode {
            Compact::Graph => (
                Compact::Summary,
                format!("{} Summary view", icon::CORNERS_IN),
            ),
            Compact::Summary => (
                Compact::Graph,
                format!("{} Mini view", icon::PICTURE_IN_PICTURE),
            ),
        };
        if ui.button(label).clicked() {
            *request = Some(Request::Compact(other));
        }
        let mut top = on_top;
        if ui.checkbox(&mut top, "Keep on top").clicked() {
            *request = Some(Request::ToggleOnTop);
        }
        if mode == Compact::Graph {
            ui.separator();
            for d in &devices {
                if ui
                    .radio(state.selected == d.spec.sel, &d.spec.title)
                    .clicked()
                {
                    state.selected = d.spec.sel.clone();
                }
            }
        }
    };

    match mode {
        Compact::Summary => {
            let mut clicked = None;
            // Registered first, so the cards on top of it get their clicks.
            let bg = ui.interact(ui.max_rect(), ui.id().with("summary_bg"), Sense::click());
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for d in &devices {
                        let sel = d.spec.sel.clone();
                        let r = card_entry(
                            ui,
                            state,
                            Device {
                                spec: d.spec.clone(),
                                ..*d
                            },
                            view.settings.graph_points,
                        );
                        if r.double_clicked() {
                            clicked = Some(sel.clone());
                        }
                        r.context_menu(|ui| {
                            state.selected = sel.clone();
                            menu(ui, state, &mut request);
                        });
                    }
                });
            if bg.double_clicked() {
                request = Some(Request::FullView);
            }
            bg.context_menu(|ui| menu(ui, state, &mut request));
            if let Some(sel) = clicked {
                state.selected = sel;
                request = Some(Request::Compact(Compact::Graph));
            }
        }
        Compact::Graph => {
            let d = devices.iter().find(|d| d.spec.sel == state.selected)?;
            let resp = ui
                .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    egui::Sides::new().shrink_left().truncate().show(
                        ui,
                        |ui| {
                            ui.label(RichText::new(d.spec.icon).color(d.spec.color));
                            ui.label(
                                RichText::new(&d.spec.title)
                                    .font(super::theme::semibold(13.0))
                                    .color(ui.visuals().strong_text_color()),
                            );
                        },
                        |ui| {
                            ui.label(
                                RichText::new(&d.spec.value)
                                    .font(super::theme::semibold(15.0))
                                    .color(ui.visuals().strong_text_color()),
                            );
                        },
                    );
                    ui.add(
                        egui::Label::new(RichText::new(&d.spec.subtitle).small().weak()).truncate(),
                    );
                    ui.add_space(6.0);
                    let mut lines = vec![Line {
                        series: d.series,
                        color: d.spec.color,
                        fill: true,
                        dashed: false,
                    }];
                    if let Some(s2) = d.series2 {
                        lines.push(Line {
                            series: s2,
                            color: d.spec.color,
                            fill: false,
                            dashed: true,
                        });
                    }
                    graph(
                        ui,
                        vec2(ui.available_width(), ui.available_height().max(40.0)),
                        &lines,
                        d.max,
                        view.settings.graph_points,
                        d.spec.color,
                        None,
                        seconds_per_point(view),
                    );
                })
                .response;
            if resp.double_clicked() {
                request = Some(Request::FullView);
            }
            resp.on_hover_text_at_pointer(
                "Double-click for the full view, right-click for options",
            )
            .context_menu(|ui| menu(ui, state, &mut request));
        }
    }
    request
}

fn show_selected(
    ui: &mut Ui,
    state: &State,
    view: &View<'_>,
    settings_cpu_logical: &mut bool,
    settings_cpu_kernel: &mut bool,
) {
    let snap = view.snap;
    let h = view.history;
    match state.selected.clone() {
        PerfSel::Cpu => cpu_view(ui, view, settings_cpu_logical, settings_cpu_kernel),
        PerfSel::Memory => memory_view(ui, view),
        PerfSel::Disk(id) => {
            if let Some((i, d)) = snap.disks.iter().enumerate().find(|(_, d)| d.id == id)
                && let Some(dh) = h.disks.get(&id)
            {
                disk_view(ui, view, i, d, dh);
            }
        }
        PerfSel::Net(id) => {
            if let (Some(n), Some(nh)) = (snap.nets.iter().find(|n| n.id == id), h.nets.get(&id)) {
                net_view(ui, view, n, nh);
            }
        }
        PerfSel::Gpu(id) => {
            if let Some((i, g)) = snap.gpus.iter().enumerate().find(|(_, g)| g.id == id)
                && let Some(gh) = h.gpus.get(&id)
            {
                gpu_view(ui, view, i, g, gh);
            }
        }
        PerfSel::Battery(id) => {
            if let (Some(b), Some(bh)) = (
                snap.batteries.iter().find(|b| b.id == id),
                h.batteries.get(&id),
            ) {
                battery_view(ui, view, b, bh);
            }
        }
        PerfSel::Sensors => sensors_view(ui, view),
    }
}

#[derive(Clone)]
struct CardSpec {
    sel: PerfSel,
    icon: &'static str,
    title: String,
    value: String,
    subtitle: String,
    color: Color32,
}

/// One device in the list on the left: title, value, subtitle and a sparkline.
fn card_entry(ui: &mut Ui, state: &mut State, d: Device<'_>, cap: usize) -> egui::Response {
    let spec = d.spec;
    let selected = state.selected == spec.sel;
    let resp = ui
        .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
            let hovered = ui.response().hovered();
            let visuals = ui.visuals();
            let fill = if selected {
                visuals.widgets.active.weak_bg_fill
            } else if hovered {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 2.0;
                    egui::Sides::new().shrink_left().truncate().show(
                        ui,
                        |ui| {
                            ui.label(RichText::new(spec.icon).color(spec.color));
                            ui.label(
                                RichText::new(&spec.title)
                                    .font(super::theme::semibold(13.0))
                                    .color(ui.visuals().strong_text_color()),
                            );
                        },
                        |ui| {
                            if !spec.value.is_empty() {
                                ui.label(
                                    RichText::new(&spec.value)
                                        .color(ui.visuals().strong_text_color()),
                                );
                            }
                        },
                    );
                    ui.add(
                        egui::Label::new(RichText::new(&spec.subtitle).small().weak()).truncate(),
                    );
                    ui.add_space(6.0);
                    sparkline(
                        ui,
                        vec2(ui.available_width(), 26.0),
                        d.series,
                        d.series2,
                        d.max,
                        cap,
                        spec.color,
                    );
                });
        })
        .response;
    if resp.clicked() {
        state.selected = spec.sel;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn title_row(ui: &mut Ui, icon_str: &str, color: Color32, title: &str, subtitle: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon_str).size(22.0).color(color));

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new(title)
                    .font(super::theme::semibold(20.0))
                    .color(ui.visuals().strong_text_color()),
            );
            ui.add(egui::Label::new(RichText::new(subtitle).weak()).truncate());
        });
    });
    ui.add_space(16.0);
}

/// A graph with caption, scale label and hover values.
#[allow(clippy::too_many_arguments)]
fn graph_card(
    ui: &mut Ui,
    view: &View<'_>,
    caption: &str,
    scale: &str,
    lines: &[Line<'_>],
    max: f32,
    color: Color32,
    height: f32,
    fmt: &dyn Fn(f32) -> String,
) {
    ui.spacing_mut().item_spacing.y = 4.0;
    egui::Sides::new().show(
        ui,
        |ui| ui.label(RichText::new(caption).small().weak()),
        |ui| ui.label(RichText::new(scale).small().weak()),
    );
    graph(
        ui,
        vec2(ui.available_width(), height),
        lines,
        max,
        view.settings.graph_points,
        color,
        Some(fmt),
        seconds_per_point(view),
    );
    egui::Sides::new().show(
        ui,
        |ui| ui.label(RichText::new(seconds_label(view)).small().weak()),
        |ui| ui.label(RichText::new("0").small().weak()),
    );
    ui.add_space(16.0);
}

fn seconds_label(view: &View<'_>) -> String {
    let secs =
        view.settings.graph_points as f64 * view.settings.update_speed.interval().as_secs_f64();
    if secs >= 120.0 {
        format!("{:.0} minutes", secs / 60.0)
    } else {
        format!("{secs:.0} seconds")
    }
}

/// Key figures as caption/value pairs in a grid that fits the width (no boxes).
fn stat_tiles(ui: &mut Ui, id: &str, stats: &[(&str, String)]) {
    let col_w = 150.0;
    let gap = 24.0;
    let cols = ((ui.available_width() + gap) / (col_w + gap))
        .floor()
        .max(1.0) as usize;
    let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
    egui::Grid::new(id)
        .num_columns(cols)
        .spacing([gap, 16.0])
        .show(ui, |ui| {
            for (i, (caption, value)) in stats.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new(*caption).small().weak());
                    ui.add(
                        egui::Label::new(
                            RichText::new(value)
                                .size(16.0)
                                .color(ui.visuals().strong_text_color()),
                        )
                        .truncate(),
                    );
                });
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    ui.add_space(16.0);
}

/// Static facts below the figures, as a quiet two-column list.
fn kv_card(ui: &mut Ui, id: &str, title: &str, rows: &[(&str, String)]) {
    let dark = ui.visuals().dark_mode;
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(
        r.x_range(),
        r.top(),
        egui::Stroke::new(1.0, super::theme::hairline(dark)),
    );
    ui.add_space(12.0);
    ui.label(
        RichText::new(title)
            .font(super::theme::semibold(13.0))
            .color(ui.visuals().strong_text_color()),
    );
    ui.add_space(8.0);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([32.0, 6.0])
        .min_row_height(18.0)
        .striped(false)
        .show(ui, |ui| {
            for (k, v) in rows {
                if v.is_empty() {
                    continue;
                }
                ui.label(RichText::new(*k).weak());
                ui.add(egui::Label::new(v.as_str()).wrap());
                ui.end_row();
            }
        });
    ui.add_space(16.0);
}

fn graph_height(ui: &Ui, share: f32) -> f32 {
    (ui.available_height() * share).clamp(110.0, 380.0)
}

fn cpu_view(ui: &mut Ui, view: &View<'_>, logical: &mut bool, kernel: &mut bool) {
    let snap = view.snap;
    let info = &snap.info;
    let h = view.history;
    let cap = view.settings.graph_points;
    title_row(ui, icon::CPU, colors::CPU, "CPU", &info.cpu_model);
    ui.horizontal(|ui| {
        ui.selectable_value(logical, false, format!("{} Overall", icon::CHART_LINE));
        ui.selectable_value(
            logical,
            true,
            format!("{} Logical processors", icon::SQUARES_FOUR),
        );
        ui.separator();
        ui.checkbox(kernel, "Show kernel times");
    });
    ui.add_space(8.0);
    if *logical && !h.cores.is_empty() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            let n = h.cores.len();
            let width = ui.available_width();
            let height = graph_height(ui, 0.5);
            let aspect = width / height;
            let cols = ((n as f32 * aspect).sqrt().ceil() as usize).clamp(1, n);
            let rows = n.div_ceil(cols);
            let gap = 4.0;
            let cell_w = (width - gap * (cols - 1) as f32) / cols as f32;
            let cell_h = ((height - gap * (rows - 1) as f32) / rows as f32).max(28.0);
            ui.label(
                RichText::new("% Utilization per logical processor")
                    .small()
                    .weak(),
            );
            egui::Grid::new("cores").spacing([gap, gap]).show(ui, |ui| {
                for (i, s) in h.cores.iter().enumerate() {
                    let resp = graph(
                        ui,
                        vec2(cell_w, cell_h),
                        &[Line {
                            series: s,
                            color: colors::CPU,
                            fill: true,
                            dashed: false,
                        }],
                        100.0,
                        cap,
                        colors::CPU,
                        None,
                        seconds_per_point(view),
                    );
                    let freq = snap
                        .cpu
                        .per_core_freq
                        .get(i)
                        .filter(|f| **f > 0.0)
                        .map(|f| format!("  ·  {}", fmt_freq_mhz(*f as f64)))
                        .unwrap_or_default();
                    resp.on_hover_text(format!(
                        "CPU {i}: {}{freq}",
                        fmt_percent(snap.cpu.per_core.get(i).copied().unwrap_or(0.0))
                    ));
                    if (i + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });
        });
        ui.add_space(16.0);
    } else {
        let mut lines = vec![Line {
            series: &h.cpu,
            color: colors::CPU,
            fill: true,
            dashed: false,
        }];
        if *kernel {
            lines.push(Line {
                series: &h.cpu_kernel,
                color: colors::CPU,
                fill: false,
                dashed: true,
            });
        }
        let caption = if *kernel {
            "% Utilization  (dashed: kernel time)"
        } else {
            "% Utilization"
        };
        graph_card(
            ui,
            view,
            caption,
            "100%",
            &lines,
            100.0,
            colors::CPU,
            graph_height(ui, 0.42),
            &|v| format!("{v:.1}%"),
        );
    }

    let mut big = vec![
        ("Utilization", fmt_percent(snap.cpu.total)),
        (
            "Speed",
            snap.cpu
                .freq_mhz
                .map(fmt_freq_mhz)
                .unwrap_or_else(|| "—".into()),
        ),
        ("Processes", snap.sys.processes.to_string()),
        ("Threads", snap.sys.threads.to_string()),
        (
            "Handles",
            snap.sys
                .handles
                .map(|h| h.to_string())
                .unwrap_or_else(|| "—".into()),
        ),
        ("Up time", fmt_uptime(snap.sys.uptime_secs)),
    ];
    if let Some(t) = snap.cpu.temperature {
        big.push(("Temperature", fmt_temp(t)));
    }
    if let Some(c) = snap.cpu.ctx_switches {
        big.push(("Context switches/s", format!("{c:.0}")));
    }
    if let Some(c) = snap.cpu.interrupts {
        big.push(("Interrupts/s", format!("{c:.0}")));
    }
    if let Some(l) = snap.cpu.load_avg {
        big.push((
            "Load (1 / 5 / 15 min)",
            format!("{:.2} / {:.2} / {:.2}", l[0], l[1], l[2]),
        ));
    }
    stat_tiles(ui, "cpu_big", &big);

    if !snap.cpu.core_temps.is_empty() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!("{} Core temperatures", icon::THERMOMETER_SIMPLE)).strong(),
            );
            ui.horizontal_wrapped(|ui| {
                for (label, t) in &snap.cpu.core_temps {
                    ui.label(RichText::new(format!("{label}: ")).weak());
                    ui.label(RichText::new(fmt_temp(*t)).color(temp_color(*t, ui)));
                    ui.add_space(8.0);
                }
            });
        });
        ui.add_space(16.0);
    }

    let cache = |v: Option<u64>| v.map(fmt_bytes).unwrap_or_default();
    kv_card(
        ui,
        "cpu_kv",
        "Processor",
        &[
            ("Model", info.cpu_model.clone()),
            (
                if info.cpu_base_is_max {
                    "Maximum speed"
                } else {
                    "Base speed"
                },
                info.cpu_base_mhz.map(fmt_freq_mhz).unwrap_or_default(),
            ),
            ("Sockets", info.sockets.to_string()),
            ("Cores", info.cores.to_string()),
            ("Logical processors", info.logical.to_string()),
            (
                "Virtualization",
                info.virtualization.clone().unwrap_or_default(),
            ),
            ("L1 cache", cache(info.l1_cache)),
            ("L2 cache", cache(info.l2_cache)),
            ("L3 cache", cache(info.l3_cache)),
        ],
    );
}

fn memory_view(ui: &mut Ui, view: &View<'_>) {
    let snap = view.snap;
    let m = &snap.mem;
    let h = view.history;
    title_row(
        ui,
        icon::MEMORY,
        colors::MEMORY,
        "Memory",
        &format!("{} installed", fmt_bytes(m.total)),
    );
    let mut lines = vec![Line {
        series: &h.mem,
        color: colors::MEMORY,
        fill: true,
        dashed: false,
    }];
    if m.swap_total > 0 {
        lines.push(Line {
            series: &h.swap,
            color: colors::MEMORY,
            fill: false,
            dashed: true,
        });
    }
    let total = m.total as f64;
    graph_card(
        ui,
        view,
        if m.swap_total > 0 {
            "Memory usage  (dashed: swap)"
        } else {
            "Memory usage"
        },
        &fmt_bytes(m.total),
        &lines,
        100.0,
        colors::MEMORY,
        graph_height(ui, 0.36),
        &|v| {
            format!(
                "{:.0}% ({})",
                v,
                fmt_bytes((v as f64 / 100.0 * total) as u64)
            )
        },
    );
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Memory composition").small().weak());
        let in_use = m.used.saturating_sub(m.modified) as f64;
        // used + available == total, and available - free == reclaimable cache.
        let standby = m.cached as f64;
        let segs = [
            (
                in_use,
                colors::MEMORY.gamma_multiply(0.85),
                format!("In use: {}", fmt_bytes(in_use as u64)),
            ),
            (
                m.modified as f64,
                colors::MEMORY.gamma_multiply(0.55),
                format!("Modified (dirty): {}", fmt_bytes(m.modified)),
            ),
            (
                standby,
                colors::MEMORY.gamma_multiply(0.28),
                format!("Standby (cache): {}", fmt_bytes(standby as u64)),
            ),
            (
                m.free as f64,
                Color32::TRANSPARENT,
                format!("Free: {}", fmt_bytes(m.free)),
            ),
        ];
        stacked_bar(ui, 30.0, &segs, m.total as f64, colors::MEMORY);
        ui.horizontal_wrapped(|ui| {
            for (_, c, label) in &segs {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, *c);
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    egui::Stroke::new(1.0, colors::MEMORY),
                    egui::StrokeKind::Inside,
                );
                ui.label(RichText::new(label).small());
                ui.add_space(8.0);
            }
        });
    });
    ui.add_space(16.0);
    let compressed = m
        .compressed
        .filter(|c| *c > 0)
        .map(|c| format!(" ({} compressed)", fmt_bytes(c)))
        .unwrap_or_default();
    let mut stats = vec![
        ("In use", format!("{}{compressed}", fmt_bytes(m.used))),
        ("Available", fmt_bytes(m.available)),
        (
            "Committed",
            format!("{} / {}", fmt_bytes(m.committed), fmt_bytes(m.commit_limit)),
        ),
        ("Cached", fmt_bytes(m.cached)),
        ("Kernel (reclaimable)", fmt_bytes(m.kernel_paged)),
        ("Kernel (unreclaimable)", fmt_bytes(m.kernel_nonpaged)),
        (
            "Swap",
            if m.swap_total > 0 {
                format!("{} / {}", fmt_bytes(m.swap_used), fmt_bytes(m.swap_total))
            } else {
                "Not configured".into()
            },
        ),
    ];
    if let Some(t) = m.temperature {
        stats.push(("Module temperature", fmt_temp(t)));
    }
    stat_tiles(ui, "mem_big", &stats);
    kv_card(
        ui,
        "mem_kv",
        "Details",
        &[
            ("Shared", fmt_bytes(m.shared)),
            ("Buffers", fmt_bytes(m.buffers)),
            ("Free", fmt_bytes(m.free)),
            (
                "Hardware reserved",
                snap.info
                    .hardware_reserved
                    .map(fmt_bytes)
                    .unwrap_or_default(),
            ),
        ],
    );
}

fn disk_view(ui: &mut Ui, view: &View<'_>, i: usize, d: &DiskSample, dh: &DeviceHistory) {
    let subtitle = if d.model.is_empty() {
        format!("{}  ·  {}  ·  {}", d.id, d.kind, fmt_bytes(d.capacity))
    } else {
        format!("{}  ·  {}  ·  {}", d.model, d.kind, fmt_bytes(d.capacity))
    };
    title_row(
        ui,
        icon::HARD_DRIVES,
        colors::DISK,
        &disk_title(i, d),
        &subtitle,
    );
    graph_card(
        ui,
        view,
        "Active time",
        "100%",
        &[Line {
            series: &dh.a,
            color: colors::DISK,
            fill: true,
            dashed: false,
        }],
        100.0,
        colors::DISK,
        graph_height(ui, 0.24),
        &|v| format!("{v:.0}% active"),
    );
    let max = nice_max(dh.b.max().max(dh.c.max()), 1024.0 * 1024.0);
    graph_card(
        ui,
        view,
        "Transfer rate  (solid: read, dashed: write)",
        &fmt_rate(max as f64),
        &[
            Line {
                series: &dh.b,
                color: colors::DISK,
                fill: true,
                dashed: false,
            },
            Line {
                series: &dh.c,
                color: colors::DISK,
                fill: false,
                dashed: true,
            },
        ],
        max,
        colors::DISK,
        graph_height(ui, 0.3),
        &|v| fmt_rate(v as f64),
    );
    let mut stats = vec![
        ("Active time", fmt_percent(d.active.unwrap_or(0.0))),
        (
            "Average response time",
            d.avg_response_ms
                .map(|v| format!("{v:.1} ms"))
                .unwrap_or_else(|| "—".into()),
        ),
        ("Read speed", fmt_rate(d.read_bps)),
        ("Write speed", fmt_rate(d.write_bps)),
    ];
    if let Some(t) = d.temperature {
        stats.push(("Temperature", fmt_temp(t)));
    }
    stat_tiles(ui, "disk_big", &stats);
    kv_card(
        ui,
        "disk_kv",
        "Drive",
        &[
            ("Model", d.model.clone()),
            ("Capacity", fmt_bytes(d.capacity)),
            ("Type", d.kind.clone()),
            ("Device", d.id.clone()),
            (
                "System disk",
                if d.system_disk { "Yes" } else { "No" }.into(),
            ),
            ("Removable", if d.removable { "Yes" } else { "No" }.into()),
            ("Mount points", d.mounts.join(", ")),
        ],
    );
}

fn net_view(ui: &mut Ui, view: &View<'_>, n: &NetSample, nh: &DeviceHistory) {
    let status = if n.connected {
        "Connected"
    } else {
        "Not connected"
    };
    title_row(
        ui,
        net_icon(n),
        colors::NETWORK,
        &n.kind,
        &format!("{}  ·  {status}", n.id),
    );
    let max = nice_max(nh.a.max().max(nh.b.max()), 12_500.0);
    graph_card(
        ui,
        view,
        "Throughput  (solid: receive, dashed: send)",
        &fmt_bits(max as f64),
        &[
            Line {
                series: &nh.a,
                color: colors::NETWORK,
                fill: true,
                dashed: false,
            },
            Line {
                series: &nh.b,
                color: colors::NETWORK,
                fill: false,
                dashed: true,
            },
        ],
        max,
        colors::NETWORK,
        graph_height(ui, 0.42),
        &|v| fmt_bits(v as f64),
    );
    let mut stats = vec![
        ("Receive", fmt_bits(n.rx_bps)),
        ("Send", fmt_bits(n.tx_bps)),
        ("Received (total)", fmt_bytes(n.rx_total)),
        ("Sent (total)", fmt_bytes(n.tx_total)),
    ];
    if let Some(s) = n.signal {
        stats.push(("Signal strength", format!("{s:.0}%")));
    }
    if let Some(t) = n.temperature {
        stats.push(("Adapter temperature", fmt_temp(t)));
    }
    stat_tiles(ui, "net_big", &stats);
    kv_card(
        ui,
        "net_kv",
        "Adapter",
        &[
            ("Adapter name", n.id.clone()),
            ("Connection type", n.kind.clone()),
            ("Status", status.into()),
            (
                "Link speed",
                n.link_mbps
                    .map(|s| {
                        if s >= 1000 {
                            format!("{:.1} Gbps", s as f64 / 1000.0)
                        } else {
                            format!("{s} Mbps")
                        }
                    })
                    .unwrap_or_default(),
            ),
            ("IPv4 address", n.ipv4.join(", ")),
            ("IPv6 address", n.ipv6.join("\n")),
            ("MAC address", n.mac.clone()),
        ],
    );
}

fn gpu_view(ui: &mut Ui, view: &View<'_>, i: usize, g: &GpuSample, gh: &DeviceHistory) {
    title_row(
        ui,
        icon::GRAPHICS_CARD,
        colors::GPU,
        &format!("GPU {i}"),
        &g.name,
    );
    if g.util.is_some() {
        graph_card(
            ui,
            view,
            "Utilization",
            "100%",
            &[Line {
                series: &gh.a,
                color: colors::GPU,
                fill: true,
                dashed: false,
            }],
            100.0,
            colors::GPU,
            graph_height(ui, 0.3),
            &|v| format!("{v:.0}%"),
        );
    } else {
        ui.label(RichText::new("This driver does not report GPU utilization.").weak());
    }
    if let Some(total) = g.mem_total {
        let t = total as f64;
        graph_card(
            ui,
            view,
            "Dedicated memory usage",
            &fmt_bytes(total),
            &[Line {
                series: &gh.b,
                color: colors::GPU,
                fill: true,
                dashed: false,
            }],
            100.0,
            colors::GPU,
            graph_height(ui, 0.2),
            &|v| fmt_bytes((v as f64 / 100.0 * t) as u64),
        );
    }
    if g.power_w.is_some() {
        let max = nice_max(gh.c.max() * 1.1, 10.0);
        graph_card(
            ui,
            view,
            "Power draw",
            &format!("{max:.0} W"),
            &[Line {
                series: &gh.c,
                color: colors::WARN,
                fill: true,
                dashed: false,
            }],
            max,
            colors::WARN,
            80.0,
            &|v| format!("{v:.1} W"),
        );
    }
    let mut big = vec![(
        "Utilization",
        g.util.map(fmt_percent).unwrap_or_else(|| "—".into()),
    )];
    if let (Some(u), Some(t)) = (g.mem_used, g.mem_total) {
        big.push((
            "Dedicated memory",
            format!("{} / {}", fmt_bytes(u), fmt_bytes(t)),
        ));
    }
    if let Some(s) = g.shared_mem_used {
        big.push(("Shared memory", fmt_bytes(s)));
    }
    if let Some(t) = g.temperature {
        big.push(("Temperature", fmt_temp(t)));
    }
    if let Some(p) = g.power_w {
        big.push(("Power", format!("{p:.1} W")));
    }
    if let Some(c) = g.clock_mhz {
        big.push(("Clock", fmt_freq_mhz(c as f64)));
    }
    if let Some(f) = g.fan_rpm {
        big.push(("Fan", format!("{f:.0} RPM")));
    } else if let Some(f) = g.fan_percent {
        big.push(("Fan", format!("{f:.0}%")));
    }
    stat_tiles(ui, "gpu_big", &big);
    kv_card(
        ui,
        "gpu_kv",
        "Adapter",
        &[
            ("Name", g.name.clone()),
            ("Vendor", g.vendor.clone()),
            ("Driver", g.driver.clone()),
            ("Location", g.pci_slot.clone()),
        ],
    );
}

fn battery_view(ui: &mut Ui, view: &View<'_>, b: &BatterySample, bh: &DeviceHistory) {
    let ac = match b.on_ac {
        Some(true) => "  ·  Plugged in",
        Some(false) => "  ·  On battery",
        None => "",
    };
    title_row(
        ui,
        battery_icon(b),
        colors::BATTERY,
        &format!("Battery  {:.0}%", b.percent),
        &format!("{}{ac}", b.status),
    );
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let color = if b.percent < 15.0 {
            colors::DANGER
        } else {
            colors::BATTERY
        };
        meter(ui, ui.available_width(), b.percent / 100.0, color);
        if let Some(secs) = b.time_remaining_secs {
            let what = if b.status == "Charging" {
                "until full"
            } else {
                "remaining"
            };
            ui.label(format!(
                "{}:{:02} h {what}",
                secs / 3600,
                (secs % 3600) / 60
            ));
        }
    });
    ui.add_space(16.0);
    graph_card(
        ui,
        view,
        "Charge",
        "100%",
        &[Line {
            series: &bh.a,
            color: colors::BATTERY,
            fill: true,
            dashed: false,
        }],
        100.0,
        colors::BATTERY,
        graph_height(ui, 0.25),
        &|v| format!("{v:.0}%"),
    );
    if b.power_w.is_some() {
        let max = nice_max(bh.b.max() * 1.1, 5.0);
        graph_card(
            ui,
            view,
            "Power",
            &format!("{max:.0} W"),
            &[Line {
                series: &bh.b,
                color: colors::WARN,
                fill: true,
                dashed: false,
            }],
            max,
            colors::WARN,
            90.0,
            &|v| format!("{v:.1} W"),
        );
    }
    let wh = |v: Option<f32>| {
        v.map(|v| format!("{v:.1} Wh"))
            .unwrap_or_else(|| "—".into())
    };
    let mut stats = vec![
        ("Charge", format!("{:.0}%", b.percent)),
        ("Status", b.status.clone()),
        (
            "Power",
            b.power_w
                .map(|p| format!("{p:.1} W"))
                .unwrap_or_else(|| "—".into()),
        ),
        ("Energy", wh(b.energy_wh)),
        ("Full charge capacity", wh(b.energy_full_wh)),
        ("Design capacity", wh(b.energy_design_wh)),
    ];
    if let Some(hl) = b.health() {
        stats.push(("Health", format!("{hl:.0}%")));
    }
    if let Some(c) = b.cycles {
        stats.push(("Charge cycles", c.to_string()));
    }
    if let Some(t) = b.temperature {
        stats.push(("Temperature", fmt_temp(t)));
    }
    stat_tiles(ui, "bat_big", &stats);
    if !b.model.is_empty() {
        kv_card(
            ui,
            "bat_kv",
            "Battery",
            &[("Model", b.model.clone()), ("Device", b.id.clone())],
        );
    }
}

fn sensors_view(ui: &mut Ui, view: &View<'_>) {
    let snap = view.snap;
    title_row(
        ui,
        icon::THERMOMETER,
        colors::SENSORS,
        "Sensors",
        "Temperatures, fans, power and voltages reported by the hardware",
    );
    if snap.sensors.is_empty() {
        ui.label(RichText::new("No hardware sensors were found on this system.").weak());
        return;
    }
    // Group by chip, keeping discovery order.
    let mut chips: Vec<&str> = Vec::new();
    for s in &snap.sensors {
        if !chips.contains(&s.chip.as_str()) {
            chips.push(&s.chip);
        }
    }
    let render_chip = |ui: &mut Ui, chip: &str| {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(chip).strong());
            ui.add_space(2.0);
            let mut readings: Vec<_> = snap.sensors.iter().filter(|s| s.chip == chip).collect();
            readings.sort_by_key(|s| s.kind);
            egui::Grid::new(("sensor", chip))
                .num_columns(3)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for r in readings {
                        let kind_icon = match r.kind {
                            SensorKind::Temperature => icon::THERMOMETER_SIMPLE,
                            SensorKind::Fan => icon::FAN,
                            SensorKind::Power => icon::LIGHTNING,
                            SensorKind::Voltage => icon::WAVEFORM,
                            SensorKind::Current => icon::PULSE,
                        };
                        ui.label(RichText::new(format!("{kind_icon}  {}", r.label)).weak());
                        let text_color = ui.visuals().text_color();
                        let (text, color, frac, bar) = match r.kind {
                            SensorKind::Temperature => {
                                let limit = r.crit.or(r.high).unwrap_or(100.0).max(r.value);
                                let c = temp_color(r.value, ui);
                                let bar = if c == text_color { colors::GOOD } else { c };
                                (fmt_temp(r.value), c, r.value / limit, bar)
                            }
                            SensorKind::Fan => (
                                format!("{:.0} RPM", r.value),
                                text_color,
                                r.value / 3000.0,
                                colors::CPU,
                            ),
                            SensorKind::Power => (
                                format!("{:.1} W", r.value),
                                text_color,
                                r.value / 200.0,
                                colors::WARN,
                            ),
                            SensorKind::Voltage => {
                                (format!("{:.3} V", r.value), text_color, 0.0, text_color)
                            }
                            SensorKind::Current => {
                                (format!("{:.2} A", r.value), text_color, 0.0, text_color)
                            }
                        };
                        if frac > 0.0 {
                            meter(ui, 90.0, frac, bar);
                        } else {
                            ui.label("");
                        }
                        let resp = ui.label(RichText::new(text).color(color));
                        if let Some(c) = r.crit {
                            resp.on_hover_text(format!("Critical at {}", fmt_temp(c)));
                        }
                        ui.end_row();
                    }
                });
        });
        ui.add_space(12.0);
    };
    if ui.available_width() > 760.0 {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.columns(2, |cols| {
            for (i, chip) in chips.iter().enumerate() {
                render_chip(&mut cols[i % 2], chip);
            }
        });
    } else {
        for chip in chips {
            render_chip(ui, chip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nice_max;

    #[test]
    fn nice_axis() {
        assert_eq!(nice_max(0.0, 100.0), 100.0);
        assert_eq!(nice_max(130.0, 1.0), 200.0);
        assert_eq!(nice_max(4_100.0, 1.0), 5_000.0);
        assert_eq!(nice_max(9_000.0, 1.0), 10_000.0);
    }
}

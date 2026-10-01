//! "Performance" page: device list with sparklines and a detail view with large graphs.

use super::View;
use super::widgets::{Line, colors, graph, sparkline, stacked_bar};
use crate::history::{DeviceHistory, Series};
use crate::model::{DiskSample, GpuSample, NetSample};
use crate::util::{fmt_bits, fmt_bytes, fmt_freq_mhz, fmt_percent, fmt_rate, fmt_uptime};
use egui::{Align, Color32, Layout, RichText, ScrollArea, Sense, Ui, vec2};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PerfSel {
    Cpu,
    Memory,
    Disk(String),
    Net(String),
    Gpu(String),
}

pub struct State {
    pub selected: PerfSel,
}

impl Default for State {
    fn default() -> Self {
        State {
            selected: PerfSel::Cpu,
        }
    }
}

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

pub fn show(
    ui: &mut Ui,
    state: &mut State,
    view: &View<'_>,
    settings_cpu_logical: &mut bool,
    settings_cpu_kernel: &mut bool,
) {
    let snap = view.snap;
    let h = view.history;
    let cap = view.settings.graph_points;
    let nets = visible_nets(view);

    // Fall back to CPU if the selected device disappeared.
    let exists = match &state.selected {
        PerfSel::Cpu | PerfSel::Memory => true,
        PerfSel::Disk(id) => snap.disks.iter().any(|d| &d.id == id),
        PerfSel::Net(id) => nets.iter().any(|n| &n.id == id),
        PerfSel::Gpu(id) => snap.gpus.iter().any(|g| &g.id == id),
    };
    if !exists {
        state.selected = PerfSel::Cpu;
    }

    egui::Panel::left("perf_devices")
        .resizable(true)
        .default_size(240.0)
        .min_size(160.0)
        .show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    let freq = snap.cpu.freq_mhz.map(fmt_freq_mhz).unwrap_or_default();
                    card(
                        ui,
                        state,
                        PerfSel::Cpu,
                        "CPU",
                        &format!("{}  {freq}", fmt_percent(snap.cpu.total)),
                        &h.cpu,
                        None,
                        100.0,
                        cap,
                        colors::CPU,
                    );
                    let mem_sub = format!(
                        "{}/{} ({:.0}%)",
                        fmt_bytes(snap.mem.used),
                        fmt_bytes(snap.mem.total),
                        snap.mem.used as f64 / snap.mem.total.max(1) as f64 * 100.0
                    );
                    card(
                        ui,
                        state,
                        PerfSel::Memory,
                        "Memory",
                        &mem_sub,
                        &h.mem,
                        None,
                        100.0,
                        cap,
                        colors::MEMORY,
                    );
                    for (i, d) in snap.disks.iter().enumerate() {
                        if let Some(dh) = h.disks.get(&d.id) {
                            let sub =
                                format!("{}\n{}", d.kind, fmt_percent(d.active.unwrap_or(0.0)));
                            card(
                                ui,
                                state,
                                PerfSel::Disk(d.id.clone()),
                                &disk_title(i, d),
                                &sub,
                                &dh.a,
                                None,
                                100.0,
                                cap,
                                colors::DISK,
                            );
                        }
                    }
                    for n in &nets {
                        if let Some(nh) = h.nets.get(&n.id) {
                            let sub = if n.connected {
                                format!(
                                    "{}\nS: {}  R: {}",
                                    n.id,
                                    fmt_bits(n.tx_bps),
                                    fmt_bits(n.rx_bps)
                                )
                            } else {
                                format!("{}\nNot connected", n.id)
                            };
                            let max = nice_max(nh.a.max().max(nh.b.max()), 12_500.0);
                            card(
                                ui,
                                state,
                                PerfSel::Net(n.id.clone()),
                                &n.kind,
                                &sub,
                                &nh.a,
                                Some(&nh.b),
                                max,
                                cap,
                                colors::NETWORK,
                            );
                        }
                    }
                    for (i, g) in snap.gpus.iter().enumerate() {
                        if let Some(gh) = h.gpus.get(&g.id) {
                            let sub = format!(
                                "{}\n{}{}",
                                g.name,
                                g.util.map(fmt_percent).unwrap_or_else(|| "—".into()),
                                g.temperature
                                    .map(|t| format!(" ({t:.0} °C)"))
                                    .unwrap_or_default()
                            );
                            card(
                                ui,
                                state,
                                PerfSel::Gpu(g.id.clone()),
                                &format!("GPU {i}"),
                                &sub,
                                &gh.a,
                                None,
                                100.0,
                                cap,
                                colors::GPU,
                            );
                        }
                    }
                });
        });

    egui::CentralPanel::default_margins().show(ui, |ui| {
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Keep clear of the (overlay) scroll bar.
                ui.set_max_width(ui.available_width() - 14.0);
                show_selected(ui, state, view, settings_cpu_logical, settings_cpu_kernel)
            });
    });
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
    }
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    state: &mut State,
    sel: PerfSel,
    title: &str,
    subtitle: &str,
    series: &Series,
    series2: Option<&Series>,
    max: f32,
    cap: usize,
    color: Color32,
) {
    let selected = state.selected == sel;
    let resp = ui
        .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
            let fill = if selected {
                ui.visuals().selection.bg_fill.gamma_multiply(0.6)
            } else {
                Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(6.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        sparkline(ui, vec2(64.0, 44.0), series, series2, max, cap, color);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
                            for line in subtitle.lines() {
                                ui.add(egui::Label::new(RichText::new(line).small()).truncate());
                            }
                        });
                    });
                });
        })
        .response;
    if resp.clicked() {
        state.selected = sel;
    }
    if resp.hovered() && !selected {
        ui.painter().rect_stroke(
            resp.rect,
            6.0,
            ui.visuals().widgets.hovered.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
}

fn title_row(ui: &mut Ui, title: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(26.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(right).size(15.0)).truncate());
        });
    });
    ui.add_space(6.0);
}

fn graph_caption(ui: &mut Ui, left: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(left).small().weak());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(right).small().weak());
        });
    });
}

fn seconds_label(view: &View<'_>) -> String {
    let secs =
        view.settings.graph_points as f64 * view.settings.update_speed.interval().as_secs_f64();
    format!("{secs:.0} seconds")
}

/// Large statistics (small caption above a big value), laid out in a grid that fits the width.
fn big_stats(ui: &mut Ui, id: &str, stats: &[(&str, String)]) {
    let col_w = 132.0;
    let cols = ((ui.available_width() / col_w).floor() as usize).max(1);
    egui::Grid::new(id)
        .num_columns(cols)
        .min_col_width(col_w - 8.0)
        .spacing([8.0, 8.0])
        .show(ui, |ui| {
            for (i, (caption, value)) in stats.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new(*caption).small().weak());
                    ui.label(RichText::new(value).size(19.0));
                });
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
}

fn kv_grid(ui: &mut Ui, id: &str, rows: &[(&str, String)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 3.0])
        .show(ui, |ui| {
            for (k, v) in rows {
                if v.is_empty() {
                    continue;
                }
                ui.label(RichText::new(*k).weak());
                ui.label(v);
                ui.end_row();
            }
        });
}

fn stats_area(ui: &mut Ui, id: &str, big: &[(&str, String)], kv: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    if ui.available_width() > 640.0 {
        ui.columns(2, |cols| {
            big_stats(&mut cols[0], id, big);
            kv(&mut cols[1]);
        });
    } else {
        big_stats(ui, id, big);
        ui.add_space(8.0);
        kv(ui);
    }
}

fn graph_height(ui: &Ui, share: f32) -> f32 {
    (ui.available_height() * share).clamp(120.0, 420.0)
}

fn cpu_view(ui: &mut Ui, view: &View<'_>, logical: &mut bool, kernel: &mut bool) {
    let snap = view.snap;
    let info = &snap.info;
    let h = view.history;
    let cap = view.settings.graph_points;
    title_row(ui, "CPU", &info.cpu_model);
    ui.horizontal(|ui| {
        ui.selectable_value(logical, false, "Overall utilization");
        ui.selectable_value(logical, true, "Logical processors");
        ui.separator();
        ui.checkbox(kernel, "Show kernel times");
    });
    ui.add_space(4.0);
    let width = ui.available_width();
    let height = graph_height(ui, 0.55);
    if *logical && !h.cores.is_empty() {
        let n = h.cores.len();
        let aspect = width / height;
        let cols = ((n as f32 * aspect).sqrt().ceil() as usize).clamp(1, n);
        let rows = n.div_ceil(cols);
        let gap = 4.0;
        let cell_w = (width - gap * (cols - 1) as f32) / cols as f32;
        let cell_h = ((height - gap * (rows - 1) as f32) / rows as f32).max(24.0);
        graph_caption(ui, "% Utilization per logical processor", "100%");
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
                );
                resp.on_hover_text(format!(
                    "CPU {i}: {}",
                    fmt_percent(snap.cpu.per_core.get(i).copied().unwrap_or(0.0))
                ));
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    } else {
        graph_caption(ui, "% Utilization", "100%");
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
        graph(ui, vec2(width, height), &lines, 100.0, cap, colors::CPU);
    }
    graph_caption(ui, &seconds_label(view), "0");

    let cache = |v: Option<u64>| v.map(fmt_bytes).unwrap_or_default();
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
        big.push(("Temperature", format!("{t:.0} °C")));
    }
    stats_area(ui, "cpu_big", &big, |ui| {
        kv_grid(
            ui,
            "cpu_kv",
            &[
                (
                    if info.cpu_base_is_max {
                        "Maximum speed:"
                    } else {
                        "Base speed:"
                    },
                    info.cpu_base_mhz.map(fmt_freq_mhz).unwrap_or_default(),
                ),
                ("Sockets:", info.sockets.to_string()),
                ("Cores:", info.cores.to_string()),
                ("Logical processors:", info.logical.to_string()),
                (
                    "Virtualization:",
                    info.virtualization.clone().unwrap_or_default(),
                ),
                ("L1 cache:", cache(info.l1_cache)),
                ("L2 cache:", cache(info.l2_cache)),
                ("L3 cache:", cache(info.l3_cache)),
                (
                    "Load average:",
                    snap.cpu
                        .load_avg
                        .map(|l| format!("{:.2}  {:.2}  {:.2}", l[0], l[1], l[2]))
                        .unwrap_or_default(),
                ),
            ],
        );
    });
}

fn memory_view(ui: &mut Ui, view: &View<'_>) {
    let snap = view.snap;
    let m = &snap.mem;
    let h = view.history;
    let cap = view.settings.graph_points;
    title_row(ui, "Memory", &fmt_bytes(m.total));
    let width = ui.available_width();
    graph_caption(ui, "Memory usage", &fmt_bytes(m.total));
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
    graph(
        ui,
        vec2(width, graph_height(ui, 0.42)),
        &lines,
        100.0,
        cap,
        colors::MEMORY,
    );
    graph_caption(
        ui,
        &seconds_label(view),
        if m.swap_total > 0 {
            "dashed: swap usage   0"
        } else {
            "0"
        },
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Memory composition").small().weak());
    let in_use = m.used.saturating_sub(m.modified) as f64;
    // used + available == total, and available - free == reclaimable cache.
    let standby = m.cached as f64;
    let segs = [
        (
            in_use,
            colors::MEMORY.gamma_multiply(0.75),
            format!("In use: {}", fmt_bytes(in_use as u64)),
        ),
        (
            m.modified as f64,
            colors::MEMORY.gamma_multiply(0.5),
            format!("Modified (dirty): {}", fmt_bytes(m.modified)),
        ),
        (
            standby,
            colors::MEMORY.gamma_multiply(0.25),
            format!("Standby (cache): {}", fmt_bytes(standby as u64)),
        ),
        (
            m.free as f64,
            Color32::TRANSPARENT,
            format!("Free: {}", fmt_bytes(m.free)),
        ),
    ];
    stacked_bar(ui, 40.0, &segs, m.total as f64, colors::MEMORY);

    let compressed = m
        .compressed
        .filter(|c| *c > 0)
        .map(|c| format!(" ({} compressed)", fmt_bytes(c)))
        .unwrap_or_default();
    stats_area(
        ui,
        "mem_big",
        &[
            ("In use", format!("{}{compressed}", fmt_bytes(m.used))),
            ("Available", fmt_bytes(m.available)),
            (
                "Committed",
                format!("{}/{}", fmt_bytes(m.committed), fmt_bytes(m.commit_limit)),
            ),
            ("Cached", fmt_bytes(m.cached)),
            ("Kernel (reclaimable)", fmt_bytes(m.kernel_paged)),
            ("Kernel (unreclaimable)", fmt_bytes(m.kernel_nonpaged)),
        ],
        |ui| {
            kv_grid(
                ui,
                "mem_kv",
                &[
                    (
                        "Swap:",
                        if m.swap_total > 0 {
                            format!("{} / {}", fmt_bytes(m.swap_used), fmt_bytes(m.swap_total))
                        } else {
                            "Not configured".into()
                        },
                    ),
                    ("Shared:", fmt_bytes(m.shared)),
                    ("Buffers:", fmt_bytes(m.buffers)),
                    ("Free:", fmt_bytes(m.free)),
                    (
                        "Hardware reserved:",
                        snap.info
                            .hardware_reserved
                            .map(fmt_bytes)
                            .unwrap_or_default(),
                    ),
                ],
            );
        },
    );
}

fn disk_view(ui: &mut Ui, view: &View<'_>, i: usize, d: &DiskSample, dh: &DeviceHistory) {
    let cap = view.settings.graph_points;
    title_row(
        ui,
        &disk_title(i, d),
        &if d.model.is_empty() {
            d.id.clone()
        } else {
            d.model.clone()
        },
    );
    let width = ui.available_width();
    graph_caption(ui, "Active time", "100%");
    graph(
        ui,
        vec2(width, graph_height(ui, 0.3)),
        &[Line {
            series: &dh.a,
            color: colors::DISK,
            fill: true,
            dashed: false,
        }],
        100.0,
        cap,
        colors::DISK,
    );
    graph_caption(ui, &seconds_label(view), "0");
    ui.add_space(6.0);
    let max = nice_max(dh.b.max().max(dh.c.max()), 1024.0 * 1024.0);
    graph_caption(
        ui,
        "Disk transfer rate (solid: read, dashed: write)",
        &fmt_rate(max as f64),
    );
    graph(
        ui,
        vec2(width, graph_height(ui, 0.35)),
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
        cap,
        colors::DISK,
    );
    graph_caption(ui, &seconds_label(view), "0");
    stats_area(
        ui,
        "disk_big",
        &[
            ("Active time", fmt_percent(d.active.unwrap_or(0.0))),
            (
                "Average response time",
                d.avg_response_ms
                    .map(|v| format!("{v:.1} ms"))
                    .unwrap_or_else(|| "—".into()),
            ),
            ("Read speed", fmt_rate(d.read_bps)),
            ("Write speed", fmt_rate(d.write_bps)),
        ],
        |ui| {
            kv_grid(
                ui,
                "disk_kv",
                &[
                    ("Capacity:", fmt_bytes(d.capacity)),
                    ("Type:", d.kind.clone()),
                    ("Device:", d.id.clone()),
                    (
                        "System disk:",
                        if d.system_disk { "Yes" } else { "No" }.into(),
                    ),
                    ("Removable:", if d.removable { "Yes" } else { "No" }.into()),
                    ("Mount points:", d.mounts.join(", ")),
                ],
            );
        },
    );
}

fn net_view(ui: &mut Ui, view: &View<'_>, n: &NetSample, nh: &DeviceHistory) {
    let cap = view.settings.graph_points;
    title_row(ui, &n.kind, &n.id);
    let width = ui.available_width();
    let max = nice_max(nh.a.max().max(nh.b.max()), 12_500.0);
    graph_caption(
        ui,
        "Throughput (solid: receive, dashed: send)",
        &fmt_bits(max as f64),
    );
    graph(
        ui,
        vec2(width, graph_height(ui, 0.55)),
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
        cap,
        colors::NETWORK,
    );
    graph_caption(ui, &seconds_label(view), "0");
    stats_area(
        ui,
        "net_big",
        &[
            ("Send", fmt_bits(n.tx_bps)),
            ("Receive", fmt_bits(n.rx_bps)),
            ("Sent (total)", fmt_bytes(n.tx_total)),
            ("Received (total)", fmt_bytes(n.rx_total)),
        ],
        |ui| {
            kv_grid(
                ui,
                "net_kv",
                &[
                    ("Adapter name:", n.id.clone()),
                    ("Connection type:", n.kind.clone()),
                    (
                        "Status:",
                        if n.connected {
                            "Connected"
                        } else {
                            "Not connected"
                        }
                        .into(),
                    ),
                    (
                        "Link speed:",
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
                    ("IPv4 address:", n.ipv4.join(", ")),
                    ("IPv6 address:", n.ipv6.join("\n")),
                    ("MAC address:", n.mac.clone()),
                ],
            );
        },
    );
}

fn gpu_view(ui: &mut Ui, view: &View<'_>, i: usize, g: &GpuSample, gh: &DeviceHistory) {
    let cap = view.settings.graph_points;
    title_row(ui, &format!("GPU {i}"), &g.name);
    let width = ui.available_width();
    if g.util.is_some() {
        graph_caption(ui, "Utilization", "100%");
        graph(
            ui,
            vec2(width, graph_height(ui, 0.35)),
            &[Line {
                series: &gh.a,
                color: colors::GPU,
                fill: true,
                dashed: false,
            }],
            100.0,
            cap,
            colors::GPU,
        );
        graph_caption(ui, &seconds_label(view), "0");
    } else {
        ui.label(RichText::new("This driver does not report GPU utilization.").weak());
    }
    if let Some(total) = g.mem_total {
        ui.add_space(6.0);
        graph_caption(ui, "Dedicated GPU memory usage", &fmt_bytes(total));
        graph(
            ui,
            vec2(width, graph_height(ui, 0.25)),
            &[Line {
                series: &gh.b,
                color: colors::GPU,
                fill: true,
                dashed: false,
            }],
            100.0,
            cap,
            colors::GPU,
        );
    }
    let mut big = vec![(
        "Utilization",
        g.util.map(fmt_percent).unwrap_or_else(|| "—".into()),
    )];
    if let (Some(u), Some(t)) = (g.mem_used, g.mem_total) {
        big.push((
            "Dedicated memory",
            format!("{}/{}", fmt_bytes(u), fmt_bytes(t)),
        ));
    }
    if let Some(s) = g.shared_mem_used {
        big.push(("Shared memory", fmt_bytes(s)));
    }
    if let Some(t) = g.temperature {
        big.push(("Temperature", format!("{t:.0} °C")));
    }
    stats_area(ui, "gpu_big", &big, |ui| {
        kv_grid(
            ui,
            "gpu_kv",
            &[
                ("Vendor:", g.vendor.clone()),
                ("Driver:", g.driver.clone()),
                ("Location:", g.pci_slot.clone()),
            ],
        );
    });
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

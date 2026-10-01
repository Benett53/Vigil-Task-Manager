//! Vigil Task Manager — a free and open source task manager for Linux and Windows.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod history;
mod model;
mod platform;
mod sampler;
mod settings;
mod ui;
mod util;

use std::process::ExitCode;

const APP_ID: &str = "io.github.baba537.Vigil";

fn print_help() {
    println!(
        "Vigil Task Manager {}\n\n\
         USAGE:\n    vigil [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --page <PAGE>  Open a page: processes, performance, history, startup,\n\
         \x20                  users, details, services or settings\n\
         \x20   --snapshot     Print a one-shot system summary to stdout and exit\n\
         \x20   -V, --version  Print version information\n\
         \x20   -h, --help     Print this help\n\n\
         ENVIRONMENT:\n\
         \x20   LIBGL_ALWAYS_SOFTWARE=1  Use software rendering if the GPU driver has problems",
        env!("CARGO_PKG_VERSION")
    );
}

/// Text summary used for bug reports and for testing the collectors without a display.
fn print_snapshot() {
    use util::{fmt_bits, fmt_bytes, fmt_rate, fmt_uptime};
    let mut collector = platform::Collector::new();
    let _ = collector.sample(true);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let s = collector.sample(true);
    let i = &s.info;
    println!(
        "Host:        {} ({}, kernel {})",
        i.hostname, i.os_name, i.kernel
    );
    println!("User:        {}", i.current_user);
    if !i.desktop.is_empty() || !i.session_type.is_empty() {
        println!("Desktop:     {} {}", i.desktop, i.session_type);
    }
    println!(
        "CPU:         {} — {} sockets, {} cores, {} logical — {:.1}% used",
        i.cpu_model, i.sockets, i.cores, i.logical, s.cpu.total
    );
    println!(
        "Memory:      {} / {} used, swap {} / {}",
        fmt_bytes(s.mem.used),
        fmt_bytes(s.mem.total),
        fmt_bytes(s.mem.swap_used),
        fmt_bytes(s.mem.swap_total)
    );
    for d in &s.disks {
        println!(
            "Disk:        {} {} {} — {:.0}% active, R {} W {} {:?}",
            d.id,
            d.kind,
            fmt_bytes(d.capacity),
            d.active.unwrap_or(0.0),
            fmt_rate(d.read_bps),
            fmt_rate(d.write_bps),
            d.mounts
        );
    }
    for n in &s.nets {
        println!(
            "Network:     {} ({}) — {} down {} up {:?}",
            n.id,
            n.kind,
            fmt_bits(n.rx_bps),
            fmt_bits(n.tx_bps),
            n.ipv4
        );
    }
    for g in &s.gpus {
        println!(
            "GPU:         {} [{}] — util {:?}, vram {:?}/{:?}",
            g.name, g.driver, g.util, g.mem_used, g.mem_total
        );
    }
    println!(
        "System:      {} processes, {} threads, up {}",
        s.sys.processes,
        s.sys.threads,
        fmt_uptime(s.sys.uptime_secs)
    );
    let mut procs: Vec<_> = s.procs.iter().collect();
    procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
    println!(
        "\n{:>8}  {:<10} {:>6}  {:>10}  {:<12} NAME",
        "PID", "USER", "CPU%", "MEMORY", "KIND"
    );
    for p in procs.iter().take(15) {
        println!(
            "{:>8}  {:<10} {:>6.1}  {:>10}  {:<12} {}{}",
            p.pid,
            p.user.chars().take(10).collect::<String>(),
            p.cpu,
            fmt_bytes(p.mem),
            format!("{:?}", p.kind),
            p.name,
            p.app_name
                .as_ref()
                .map(|a| format!("  [{a}]"))
                .unwrap_or_default()
        );
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut start_page = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--page" => {
                let name = it.next().map(String::as_str).unwrap_or("");
                match settings::Page::from_cli(name) {
                    Some(p) => start_page = Some(p),
                    None => {
                        eprintln!("vigil: unknown page '{name}' (see --help)");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("vigil {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--snapshot" => {
                print_snapshot();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("vigil: unknown option '{other}' (see --help)");
                return ExitCode::from(2);
            }
        }
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/vigil-128.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Vigil Task Manager")
        .with_app_id(APP_ID)
        .with_inner_size([1120.0, 740.0])
        .with_min_inner_size([640.0, 420.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    match eframe::run_native(
        APP_ID,
        options,
        Box::new(move |cc| Ok(Box::new(app::VigilApp::new(cc, start_page)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vigil: could not start the user interface: {e}");
            eprintln!(
                "vigil: if this is a graphics driver problem, try LIBGL_ALWAYS_SOFTWARE=1 vigil"
            );
            ExitCode::FAILURE
        }
    }
}

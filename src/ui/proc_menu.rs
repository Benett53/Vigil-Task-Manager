//! Context menus shared by the Processes, Details and Users pages.

use super::Action;
use crate::model::{Priority, ProcSignal, ProcState, Process, Snapshot};
use egui::Ui;

/// All descendants of `pid` (children first is not required; the caller sends signals in order).
pub fn descendants(snap: &Snapshot, pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![pid];
    let mut guard = 0;
    while let Some(p) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        for c in snap.procs.iter().filter(|c| c.ppid == p && c.pid != p) {
            if !out.contains(&c.pid) {
                out.push(c.pid);
                stack.push(c.pid);
            }
        }
    }
    out
}

pub fn process_menu(
    ui: &mut Ui,
    p: &Process,
    snap: &Snapshot,
    actions: &mut Vec<Action>,
    show_details_link: bool,
) {
    let target = format!("{} (PID {})", p.name, p.pid);
    if ui.button("End task").clicked() {
        actions.push(Action::Signal {
            pids: vec![p.pid],
            sig: ProcSignal::Terminate,
            target: target.clone(),
        });
        ui.close();
    }
    if ui.button("End process tree").clicked() {
        let mut pids = descendants(snap, p.pid);
        pids.reverse();
        pids.push(p.pid);
        actions.push(Action::Signal {
            pids,
            sig: ProcSignal::Terminate,
            target: format!("{target} and its child processes"),
        });
        ui.close();
    }
    if ui.button("Kill (force)").clicked() {
        actions.push(Action::Signal {
            pids: vec![p.pid],
            sig: ProcSignal::Kill,
            target: target.clone(),
        });
        ui.close();
    }
    if p.state == ProcState::Stopped {
        if ui.button("Resume").clicked() {
            actions.push(Action::Signal {
                pids: vec![p.pid],
                sig: ProcSignal::Resume,
                target: target.clone(),
            });
            ui.close();
        }
    } else if ui.button("Suspend").clicked() {
        actions.push(Action::Signal {
            pids: vec![p.pid],
            sig: ProcSignal::Suspend,
            target: target.clone(),
        });
        ui.close();
    }
    ui.separator();
    let mut eff = p.efficiency;
    if ui.checkbox(&mut eff, "Efficiency mode").clicked() {
        actions.push(Action::SetEfficiency(vec![p.pid], eff));
        ui.close();
    }
    ui.menu_button("Set priority", |ui| {
        let current = Priority::from_nice(p.nice);
        for prio in Priority::ALL {
            if ui.radio(current == prio, prio.label()).clicked() {
                actions.push(Action::SetPriority(p.pid, prio));
                ui.close();
            }
        }
    });
    if ui.button("Set affinity…").clicked() {
        actions.push(Action::OpenAffinity(p.pid, p.name.to_string()));
        ui.close();
    }
    ui.separator();
    if show_details_link && ui.button("Go to details").clicked() {
        actions.push(Action::GoToDetails(p.pid));
        ui.close();
    }
    if let Some(unit) = p.unit.as_deref().filter(|u| is_service(u))
        && ui
            .button(format!(
                "Go to service ({})",
                unit.trim_end_matches(".service")
            ))
            .clicked()
    {
        actions.push(Action::GoToService(
            unit.trim_end_matches(".service").to_owned(),
        ));
        ui.close();
    }
    if ui
        .add_enabled(!p.exe.is_empty(), egui::Button::new("Open file location"))
        .clicked()
    {
        actions.push(Action::OpenLocation(p.exe.to_string()));
        ui.close();
    }
    if ui.button("Search online").clicked() {
        actions.push(Action::SearchOnline(p.name.to_string()));
        ui.close();
    }
    ui.menu_button("Copy", |ui| {
        copy_item(ui, "Name", &p.name, actions);
        copy_item(ui, "PID", &p.pid.to_string(), actions);
        copy_item(ui, "Executable path", &p.exe, actions);
        copy_item(ui, "Command line", &p.cmdline, actions);
        let summary = format!(
            "{}\tPID {}\t{}\tCPU {:.1}%\t{}\t{}",
            p.name,
            p.pid,
            p.user,
            p.cpu,
            crate::util::fmt_bytes(p.mem),
            p.cmdline
        );
        copy_item(ui, "All columns", &summary, actions);
    });
    ui.separator();
    if ui.button("Properties").clicked() {
        actions.push(Action::OpenProperties(p.pid, p.name.to_string()));
        ui.close();
    }
}

fn is_service(unit: &str) -> bool {
    unit.ends_with(".service") && !unit.starts_with("user@") && !unit.starts_with("app-")
}

fn copy_item(ui: &mut Ui, label: &str, value: &str, actions: &mut Vec<Action>) {
    if ui
        .add_enabled(!value.is_empty(), egui::Button::new(label))
        .clicked()
    {
        actions.push(Action::Copy(value.to_owned()));
        ui.close();
    }
}

/// Menu for an application group (several processes).
pub fn app_menu(
    ui: &mut Ui,
    name: &str,
    members: &[&Process],
    expanded: &mut bool,
    actions: &mut Vec<Action>,
) {
    if ui
        .button(if *expanded { "Collapse" } else { "Expand" })
        .clicked()
    {
        *expanded = !*expanded;
        ui.close();
    }
    ui.separator();
    let pids: Vec<u32> = members.iter().map(|p| p.pid).collect();
    if ui.button("End task").clicked() {
        actions.push(Action::Signal {
            pids: pids.clone(),
            sig: ProcSignal::Terminate,
            target: format!("{name} ({} processes)", pids.len()),
        });
        ui.close();
    }
    if ui.button("Kill (force)").clicked() {
        actions.push(Action::Signal {
            pids: pids.clone(),
            sig: ProcSignal::Kill,
            target: format!("{name} ({} processes)", pids.len()),
        });
        ui.close();
    }
    let all_stopped = members.iter().all(|p| p.state == ProcState::Stopped);
    let (label, sig) = if all_stopped {
        ("Resume", ProcSignal::Resume)
    } else {
        ("Suspend", ProcSignal::Suspend)
    };
    if ui.button(label).clicked() {
        actions.push(Action::Signal {
            pids: pids.clone(),
            sig,
            target: name.to_owned(),
        });
        ui.close();
    }
    let mut eff = members.iter().all(|p| p.efficiency);
    if ui.checkbox(&mut eff, "Efficiency mode").clicked() {
        actions.push(Action::SetEfficiency(pids.clone(), eff));
        ui.close();
    }
    ui.separator();
    if let Some(first) = members.iter().min_by_key(|p| p.pid) {
        if ui.button("Go to details").clicked() {
            actions.push(Action::GoToDetails(first.pid));
            ui.close();
        }
        if ui
            .add_enabled(
                !first.exe.is_empty(),
                egui::Button::new("Open file location"),
            )
            .clicked()
        {
            actions.push(Action::OpenLocation(first.exe.to_string()));
            ui.close();
        }
        if ui.button("Search online").clicked() {
            actions.push(Action::SearchOnline(name.to_owned()));
            ui.close();
        }
        copy_item(ui, "Copy name", name, actions);
        ui.separator();
        if ui.button("Properties").clicked() {
            actions.push(Action::OpenProperties(first.pid, first.name.to_string()));
            ui.close();
        }
    }
}

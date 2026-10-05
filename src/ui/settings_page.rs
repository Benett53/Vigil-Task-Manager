//! "Settings" page.

use crate::settings::{Page, Settings, ThemeChoice, UpdateSpeed};
use egui::{RichText, ScrollArea, Ui};
use egui_phosphor::regular as icon;

fn section(ui: &mut Ui, ic: &str, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(ic)
                .size(16.0)
                .color(super::widgets::colors::ACCENT),
        );
        ui.label(RichText::new(title).size(16.0).strong());
    });
    ui.add_space(4.0);
    ui.scope(|ui| {
        ui.set_max_width(760.0);
        super::widgets::card(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    });
}

/// Returns `true` if the user asked to reset all settings.
pub fn show(ui: &mut Ui, s: &mut Settings) -> bool {
    let mut reset = false;
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, icon::PALETTE, "Appearance", |ui| {
            ui.horizontal(|ui| {
                ui.label("Theme:");
                ui.selectable_value(&mut s.theme, ThemeChoice::System, "Use system setting");
                ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
                ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
            });
            ui.horizontal(|ui| {
                ui.label("Zoom:");
                ui.add(egui::Slider::new(&mut s.zoom, 0.6..=2.5).step_by(0.05).suffix("×"));
            });
            ui.checkbox(&mut s.heatmap, "Highlight resource usage with colors (heat map)");
            ui.checkbox(&mut s.nav_collapsed, "Collapse the navigation bar");
        });

        section(ui, icon::ROCKET_LAUNCH, "Start-up", |ui| {
            egui::ComboBox::from_label("Default start page")
                .selected_text(s.start_page.title())
                .show_ui(ui, |ui| {
                    for p in Page::MAIN {
                        ui.selectable_value(&mut s.start_page, p, p.title());
                    }
                });
        });

        section(ui, icon::CHART_LINE, "Data", |ui| {
            egui::ComboBox::from_label("Real time update speed")
                .selected_text(s.update_speed.label())
                .show_ui(ui, |ui| {
                    for sp in UpdateSpeed::ALL {
                        ui.selectable_value(&mut s.update_speed, sp, sp.label());
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Graph history:");
                ui.add(egui::Slider::new(&mut s.graph_points, 20..=600).suffix(" samples"));
            });
            ui.checkbox(
                &mut s.cpu_per_core_scale,
                "Show process CPU usage relative to one core (100% = one full core)",
            );
        });

        section(ui, icon::APP_WINDOW, "Window management", |ui| {
            ui.checkbox(&mut s.always_on_top, "Always on top")
                .on_hover_text("Not every Wayland compositor allows applications to stay on top.");
            ui.checkbox(&mut s.minimize_on_use, "Minimize after starting a new task");
        });

        section(ui, icon::LIST_BULLETS, "Processes", |ui| {
            ui.checkbox(&mut s.confirm_end_task, "Ask for confirmation before ending processes");
            ui.checkbox(&mut s.group_by_type, "Group processes by type");
            ui.checkbox(&mut s.show_kernel_threads, "Show kernel threads");
            ui.checkbox(&mut s.show_full_account_name, "Show full account name on the Users page");
            ui.checkbox(&mut s.show_overview, "Show the resource overview above the process list");
            ui.checkbox(&mut s.show_process_details, "Show details of the selected process");
            ui.checkbox(&mut s.details_tree, "Show the Details page as a process tree");
        });

        section(ui, icon::GAUGE, "Performance page", |ui| {
            ui.checkbox(&mut s.show_virtual_adapters, "Show virtual network adapters");
        });

        section(ui, icon::SHIELD_CHECK, "Privacy", |ui| {
            ui.label("Vigil never sends any data anywhere. There is no telemetry, no update check and no account.");
            ui.label(RichText::new("Settings and app history are stored locally in your user profile.").weak());
        });

        ui.add_space(12.0);
        if ui.button(format!("{} Restore default settings", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
            reset = true;
        }
        ui.add_space(20.0);
    });
    reset
}

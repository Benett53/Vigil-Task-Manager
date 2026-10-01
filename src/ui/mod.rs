//! User interface. Pages are pure views over the latest snapshot; everything that
//! changes the system is emitted as an [`Action`] and executed centrally by the app.

pub mod app_history;
pub mod details;
pub mod dialogs;
pub mod performance;
pub mod proc_menu;
pub mod processes;
pub mod services;
pub mod settings_page;
pub mod startup;
pub mod table;
pub mod theme;
pub mod users;
pub mod widgets;

use crate::history::{AppHistory, History};
use crate::model::{Priority, ProcSignal, ServiceAction, Snapshot, StartupEntry};
use crate::settings::Settings;

/// Something the user asked for that the app executes (often in the background).
#[derive(Clone, Debug)]
pub enum Action {
    Signal {
        pids: Vec<u32>,
        sig: ProcSignal,
        /// Description used for confirmation and error messages ("Firefox", "PID 1234").
        target: String,
    },
    SetPriority(u32, Priority),
    SetEfficiency(Vec<u32>, bool),
    OpenAffinity(u32, String),
    OpenProperties(u32, String),
    OpenLocation(String),
    SearchOnline(String),
    Copy(String),
    GoToDetails(u32),
    GoToService(String),
    RunTaskDialog,
    ServiceControl {
        name: String,
        user: bool,
        action: ServiceAction,
    },
    RefreshServices,
    RefreshStartup,
    StartupSetEnabled(StartupEntry, bool),
    StartupRemove(StartupEntry),
    StartupAddDialog,
    ResetAppHistory,
    RefreshNow,
}

/// Read-only data a page needs to render.
pub struct View<'a> {
    pub snap: &'a Snapshot,
    pub history: &'a History,
    pub app_history: &'a AppHistory,
    pub settings: &'a Settings,
    /// Lower-cased search text.
    pub search: &'a str,
}

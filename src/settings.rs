//! User preferences, persisted by eframe (RON file in the platform data directory).

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Page {
    Processes,
    Performance,
    AppHistory,
    Startup,
    Users,
    Details,
    Services,
    Settings,
}

impl Page {
    pub const MAIN: [Page; 7] = [
        Page::Processes,
        Page::Performance,
        Page::AppHistory,
        Page::Startup,
        Page::Users,
        Page::Details,
        Page::Services,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Processes => "Processes",
            Page::Performance => "Performance",
            Page::AppHistory => "App history",
            Page::Startup => "Startup apps",
            Page::Users => "Users",
            Page::Details => "Details",
            Page::Services => "Services",
            Page::Settings => "Settings",
        }
    }

    pub fn from_cli(name: &str) -> Option<Page> {
        Some(match name.to_ascii_lowercase().as_str() {
            "processes" => Page::Processes,
            "performance" => Page::Performance,
            "history" | "app-history" => Page::AppHistory,
            "startup" => Page::Startup,
            "users" => Page::Users,
            "details" => Page::Details,
            "services" => Page::Services,
            "settings" => Page::Settings,
            _ => return None,
        })
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::Processes => "☰",
            Page::Performance => "📈",
            Page::AppHistory => "🕘",
            Page::Startup => "🚀",
            Page::Users => "👥",
            Page::Details => "📋",
            Page::Services => "⚙",
            Page::Settings => "🔧",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateSpeed {
    High,
    Normal,
    Low,
    Paused,
}

impl UpdateSpeed {
    pub const ALL: [UpdateSpeed; 4] = [
        UpdateSpeed::High,
        UpdateSpeed::Normal,
        UpdateSpeed::Low,
        UpdateSpeed::Paused,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UpdateSpeed::High => "High (0.5 s)",
            UpdateSpeed::Normal => "Normal (1 s)",
            UpdateSpeed::Low => "Low (4 s)",
            UpdateSpeed::Paused => "Paused",
        }
    }

    pub fn interval(self) -> Duration {
        match self {
            UpdateSpeed::High => Duration::from_millis(500),
            UpdateSpeed::Normal | UpdateSpeed::Paused => Duration::from_secs(1),
            UpdateSpeed::Low => Duration::from_secs(4),
        }
    }
}

/// Optional columns of the process tables.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessColumns {
    pub pid: bool,
    pub status: bool,
    pub user: bool,
    pub cpu: bool,
    pub memory: bool,
    pub disk: bool,
    pub gpu: bool,
    pub gpu_memory: bool,
    pub threads: bool,
    pub priority: bool,
    pub rss: bool,
    pub virt: bool,
    pub started: bool,
    pub command: bool,
}

impl Default for ProcessColumns {
    fn default() -> Self {
        ProcessColumns {
            pid: false,
            status: true,
            user: false,
            cpu: true,
            memory: true,
            disk: true,
            gpu: true,
            gpu_memory: false,
            threads: false,
            priority: false,
            rss: false,
            virt: false,
            started: false,
            command: false,
        }
    }
}

impl ProcessColumns {
    pub fn details_default() -> Self {
        ProcessColumns {
            pid: true,
            status: true,
            user: true,
            cpu: true,
            memory: true,
            disk: false,
            gpu: false,
            gpu_memory: false,
            threads: true,
            priority: true,
            rss: true,
            virt: false,
            started: true,
            command: true,
        }
    }

    pub fn toggles(&mut self) -> [(&'static str, &mut bool); 14] {
        [
            ("PID", &mut self.pid),
            ("Status", &mut self.status),
            ("User name", &mut self.user),
            ("CPU", &mut self.cpu),
            ("Memory", &mut self.memory),
            ("Disk", &mut self.disk),
            ("GPU", &mut self.gpu),
            ("GPU memory", &mut self.gpu_memory),
            ("Threads", &mut self.threads),
            ("Priority", &mut self.priority),
            ("Working set (RSS)", &mut self.rss),
            ("Virtual memory", &mut self.virt),
            ("Started", &mut self.started),
            ("Command line", &mut self.command),
        ]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub start_page: Page,
    pub update_speed: UpdateSpeed,
    pub always_on_top: bool,
    pub minimize_on_use: bool,
    pub confirm_end_task: bool,
    pub show_kernel_threads: bool,
    pub group_by_type: bool,
    /// Per-process CPU relative to a single core (top/"Irix" mode) instead of the whole system.
    pub cpu_per_core_scale: bool,
    pub show_virtual_adapters: bool,
    pub graph_points: usize,
    pub show_full_account_name: bool,
    pub zoom: f32,
    pub heatmap: bool,
    pub nav_collapsed: bool,
    pub cpu_logical_view: bool,
    pub cpu_show_kernel: bool,
    pub processes_columns: ProcessColumns,
    pub details_columns: ProcessColumns,
    pub history_all_processes: bool,
    pub show_user_services: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::System,
            start_page: Page::Processes,
            update_speed: UpdateSpeed::Normal,
            always_on_top: false,
            minimize_on_use: false,
            confirm_end_task: true,
            show_kernel_threads: false,
            group_by_type: true,
            cpu_per_core_scale: false,
            show_virtual_adapters: false,
            graph_points: 60,
            show_full_account_name: false,
            zoom: 1.0,
            heatmap: true,
            nav_collapsed: false,
            cpu_logical_view: false,
            cpu_show_kernel: true,
            processes_columns: ProcessColumns::default(),
            details_columns: ProcessColumns::details_default(),
            history_all_processes: false,
            show_user_services: false,
        }
    }
}

impl Settings {
    /// Clamps values loaded from disk into valid ranges.
    pub fn sanitize(&mut self) {
        self.graph_points = self.graph_points.clamp(20, 600);
        if !self.zoom.is_finite() {
            self.zoom = 1.0;
        }
        self.zoom = self.zoom.clamp(0.6, 2.5);
        if self.start_page == Page::Settings {
            self.start_page = Page::Processes;
        }
    }
}

//! Rolling history for the performance graphs and accumulated per-app usage ("App history").

use crate::model::{ProcKind, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, Default)]
pub struct Series {
    data: VecDeque<f32>,
}

impl Series {
    pub fn push(&mut self, v: f32, cap: usize) {
        self.data.push_back(if v.is_finite() { v } else { 0.0 });
        while self.data.len() > cap {
            self.data.pop_front();
        }
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.data.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(0.0, f32::max)
    }
}

#[derive(Default)]
pub struct DeviceHistory {
    pub a: Series,
    pub b: Series,
    pub c: Series,
}

#[derive(Default)]
pub struct History {
    pub cpu: Series,
    pub cpu_kernel: Series,
    pub cores: Vec<Series>,
    pub mem: Series,
    pub swap: Series,
    /// Per disk: active %, read B/s, write B/s.
    pub disks: HashMap<String, DeviceHistory>,
    /// Per adapter: receive B/s, send B/s.
    pub nets: HashMap<String, DeviceHistory>,
    /// Per GPU: utilisation %, memory %.
    pub gpus: HashMap<String, DeviceHistory>,
}

impl History {
    pub fn record(&mut self, s: &Snapshot, cap: usize) {
        self.cpu.push(s.cpu.total, cap);
        self.cpu_kernel.push(s.cpu.kernel, cap);
        if self.cores.len() != s.cpu.per_core.len() {
            self.cores
                .resize_with(s.cpu.per_core.len(), Series::default);
        }
        for (series, v) in self.cores.iter_mut().zip(&s.cpu.per_core) {
            series.push(*v, cap);
        }
        let pct = |a: u64, b: u64| {
            if b == 0 {
                0.0
            } else {
                (a as f64 / b as f64 * 100.0) as f32
            }
        };
        self.mem.push(pct(s.mem.used, s.mem.total), cap);
        self.swap.push(pct(s.mem.swap_used, s.mem.swap_total), cap);

        self.disks.retain(|k, _| s.disks.iter().any(|d| &d.id == k));
        for d in &s.disks {
            let h = self.disks.entry(d.id.clone()).or_default();
            h.a.push(d.active.unwrap_or(0.0), cap);
            h.b.push(d.read_bps as f32, cap);
            h.c.push(d.write_bps as f32, cap);
        }
        self.nets.retain(|k, _| s.nets.iter().any(|n| &n.id == k));
        for n in &s.nets {
            let h = self.nets.entry(n.id.clone()).or_default();
            h.a.push(n.rx_bps as f32, cap);
            h.b.push(n.tx_bps as f32, cap);
        }
        self.gpus.retain(|k, _| s.gpus.iter().any(|g| &g.id == k));
        for g in &s.gpus {
            let h = self.gpus.entry(g.id.clone()).or_default();
            h.a.push(g.util.unwrap_or(0.0), cap);
            let mem = match (g.mem_used, g.mem_total) {
                (Some(u), Some(t)) => pct(u, t),
                _ => 0.0,
            };
            h.b.push(mem, cap);
        }
    }
}

/// Accumulated resource usage of one application (persisted across sessions).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppUsage {
    pub name: String,
    pub is_app: bool,
    pub cpu_secs: f64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub gpu_secs: f64,
    pub last_seen: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppHistory {
    pub since: u64,
    pub apps: HashMap<String, AppUsage>,
}

impl AppHistory {
    pub fn new() -> AppHistory {
        AppHistory {
            since: crate::util::now_unix(),
            apps: HashMap::new(),
        }
    }

    pub fn record(&mut self, s: &Snapshot) {
        if self.since == 0 {
            self.since = crate::util::now_unix();
        }
        let now = crate::util::now_unix();
        for p in &s.procs {
            if p.kernel_thread {
                continue;
            }
            let active = p.cpu_delta > 0.0
                || p.disk_read_delta > 0
                || p.disk_write_delta > 0
                || p.gpu_delta > 0.0;
            let (key, name, is_app) = match (&p.app_id, &p.app_name) {
                (Some(id), Some(name)) => (&**id, &**name, true),
                _ => (&*p.name, &*p.name, false),
            };
            if !active && self.apps.contains_key(key) {
                if let Some(u) = self.apps.get_mut(key) {
                    u.last_seen = now;
                }
                continue;
            }
            if !active {
                continue;
            }
            let u = self.apps.entry(key.to_owned()).or_insert_with(|| AppUsage {
                name: name.to_owned(),
                is_app: is_app && p.kind == ProcKind::App,
                ..Default::default()
            });
            u.cpu_secs += p.cpu_delta;
            u.disk_read += p.disk_read_delta;
            u.disk_write += p.disk_write_delta;
            u.gpu_secs += p.gpu_delta;
            u.last_seen = now;
        }
        // Keep the persisted state bounded.
        if self.apps.len() > 4000 {
            let mut v: Vec<(String, f64)> = self
                .apps
                .iter()
                .map(|(k, u)| (k.clone(), u.cpu_secs))
                .collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in v.into_iter().take(1000) {
                self.apps.remove(&k);
            }
        }
    }

    pub fn reset(&mut self) {
        *self = AppHistory::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_is_bounded() {
        let mut s = Series::default();
        for i in 0..100 {
            s.push(i as f32, 10);
        }
        assert_eq!(s.len(), 10);
        assert_eq!(s.max(), 99.0);
        s.push(f32::NAN, 10);
        assert_eq!(s.iter().last(), Some(0.0));
    }
}

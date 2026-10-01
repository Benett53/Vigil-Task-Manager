//! GPU monitoring for Linux.
//!
//! * Device discovery and totals come from `/sys/class/drm` (amdgpu exposes
//!   utilisation and VRAM there), NVIDIA cards are queried through NVML.
//! * Per-process usage uses the DRM `fdinfo` interface (amdgpu, i915, xe, msm,
//!   panfrost, v3d, nouveau ...), which is what Windows Task Manager's "GPU"
//!   column corresponds to. Its totals also provide utilisation for drivers that
//!   have no sysfs busy counter (e.g. Intel).

use super::procfs::{read_trimmed, read_u64};
use crate::model::GpuSample;
use crate::platform::nvidia::{Nvidia, normalize_pci};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Card {
    sysfs: PathBuf,
    pci: String,
    vendor_id: u32,
    name: String,
    vendor: String,
    driver: String,
    hwmon: Option<PathBuf>,
}

/// Per process GPU usage computed for one sample.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcGpu {
    pub util: f32,
    pub busy_secs: f64,
    pub mem: Option<u64>,
}

pub struct GpuMonitor {
    cards: Vec<Card>,
    nvidia: Option<Nvidia>,
    /// Last engine counters per (pdev, client id, engine) in ns.
    last_engine_ns: HashMap<(String, u64, String), u64>,
    /// Busy ns per (pdev, engine) accumulated in the current sample.
    engine_busy: HashMap<(String, String), u64>,
    engine_capacity: HashMap<(String, String), u64>,
    clients_seen: HashMap<(String, u64), u32>,
    seen_keys: std::collections::HashSet<(String, u64, String)>,
    elapsed_ns: f64,
    nv_procs: HashMap<u32, (Option<f32>, Option<u64>)>,
    nv_samples: Vec<crate::platform::nvidia::NvGpu>,
}

impl GpuMonitor {
    pub fn new() -> GpuMonitor {
        let cards = discover_cards();
        let has_nvidia = cards.iter().any(|c| c.vendor_id == 0x10de)
            || Path::new("/proc/driver/nvidia/version").exists();
        let nvidia = if has_nvidia { Nvidia::init() } else { None };
        GpuMonitor {
            cards,
            nvidia,
            last_engine_ns: HashMap::new(),
            engine_busy: HashMap::new(),
            engine_capacity: HashMap::new(),
            clients_seen: HashMap::new(),
            seen_keys: Default::default(),
            elapsed_ns: 1e9,
            nv_procs: HashMap::new(),
            nv_samples: Vec::new(),
        }
    }

    /// Starts a new sampling round.
    pub fn begin(&mut self, elapsed_secs: f64) {
        self.elapsed_ns = (elapsed_secs * 1e9).max(1.0);
        self.engine_busy.clear();
        self.clients_seen.clear();
        self.seen_keys.clear();
        self.nv_procs.clear();
        if let Some(nv) = self.nvidia.as_mut() {
            self.nv_samples = nv.sample(true);
            for g in &self.nv_samples {
                for &(pid, util, mem) in &g.procs {
                    let e = self.nv_procs.entry(pid).or_insert((None, None));
                    if let Some(u) = util {
                        e.0 = Some(e.0.unwrap_or(0.0) + u);
                    }
                    if let Some(m) = mem {
                        e.1 = Some(e.1.unwrap_or(0) + m);
                    }
                }
            }
        }
    }

    /// Accounts the DRM clients of a process. `fdinfo` is the content of each DRM fd's fdinfo.
    pub fn account_process<'a>(
        &mut self,
        pid: u32,
        fdinfos: impl Iterator<Item = &'a str>,
    ) -> Option<ProcGpu> {
        let mut busy_ns_max: f64 = 0.0;
        let mut busy_total: u64 = 0;
        let mut mem: Option<u64> = None;
        let mut any = false;
        for info in fdinfos {
            let Some(client) = parse_fdinfo(info) else {
                continue;
            };
            let client_key = (client.pdev.clone(), client.client_id);
            // Several fds may refer to the same client; count it only once.
            if self.clients_seen.contains_key(&client_key) {
                continue;
            }
            self.clients_seen.insert(client_key, pid);
            any = true;
            if let Some(m) = client.mem {
                mem = Some(mem.unwrap_or(0) + m);
            }
            for (engine, ns, cap) in client.engines {
                let key = (client.pdev.clone(), client.client_id, engine.clone());
                let prev = self.last_engine_ns.insert(key.clone(), ns);
                self.seen_keys.insert(key);
                let delta = match prev {
                    Some(p) if ns >= p => ns - p,
                    _ => 0,
                };
                let cap = cap.max(1);
                let ekey = (client.pdev.clone(), engine);
                *self.engine_busy.entry(ekey.clone()).or_insert(0) += delta;
                self.engine_capacity.insert(ekey, cap);
                busy_ns_max = busy_ns_max.max(delta as f64 / cap as f64);
                busy_total += delta;
            }
        }
        let nv = self.nv_procs.get(&pid).copied();
        if !any && nv.is_none() {
            return None;
        }
        let mut util = (busy_ns_max / self.elapsed_ns * 100.0).clamp(0.0, 100.0) as f32;
        let mut busy_secs = busy_total as f64 / 1e9;
        if let Some((nv_util, nv_mem)) = nv {
            if let Some(u) = nv_util {
                util = util.max(u.min(100.0));
                busy_secs = busy_secs.max(u as f64 / 100.0 * self.elapsed_ns / 1e9);
            }
            if nv_mem.is_some() {
                mem = Some(mem.unwrap_or(0) + nv_mem.unwrap_or(0));
            }
        }
        Some(ProcGpu {
            util,
            busy_secs,
            mem,
        })
    }

    /// Finishes the round and returns per-GPU totals.
    pub fn finish(&mut self) -> Vec<GpuSample> {
        // Forget counters of clients that disappeared.
        let seen = std::mem::take(&mut self.seen_keys);
        self.last_engine_ns.retain(|k, _| seen.contains(k));
        self.seen_keys = seen;

        let mut out = Vec::with_capacity(self.cards.len());
        let mut used_nv = vec![false; self.nv_samples.len()];
        for card in &self.cards {
            let mut s = GpuSample {
                id: card
                    .sysfs
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                name: card.name.clone(),
                vendor: card.vendor.clone(),
                driver: card.driver.clone(),
                pci_slot: card.pci.clone(),
                ..Default::default()
            };
            let dev = card.sysfs.join("device");
            if let Some(busy) = read_u64(dev.join("gpu_busy_percent")) {
                s.util = Some(busy.min(100) as f32);
            }
            if let (Some(used), Some(total)) = (
                read_u64(dev.join("mem_info_vram_used")),
                read_u64(dev.join("mem_info_vram_total")),
            ) {
                s.mem_used = Some(used);
                s.mem_total = Some(total);
            }
            s.shared_mem_used = read_u64(dev.join("mem_info_gtt_used"));
            if let Some(hw) = &card.hwmon {
                s.temperature = read_u64(hw.join("temp1_input")).map(|v| v as f32 / 1000.0);
            }
            // Utilisation from fdinfo for drivers without a busy counter.
            if s.util.is_none() {
                let mut best: Option<f64> = None;
                for ((pdev, engine), busy) in &self.engine_busy {
                    if pdev == &card.pci {
                        let cap = self
                            .engine_capacity
                            .get(&(pdev.clone(), engine.clone()))
                            .copied()
                            .unwrap_or(1);
                        let pct = *busy as f64 / cap as f64 / self.elapsed_ns * 100.0;
                        best = Some(best.unwrap_or(0.0).max(pct));
                    }
                }
                if best.is_some() || card.vendor_id == 0x8086 {
                    s.util = Some(best.unwrap_or(0.0).clamp(0.0, 100.0) as f32);
                }
            }
            if let Some(idx) = self.nv_samples.iter().position(|g| g.pci == card.pci) {
                used_nv[idx] = true;
                let nv = &self.nv_samples[idx];
                s.name = nv.name.clone();
                s.driver = nv.driver.clone();
                s.util = nv.util.or(s.util);
                s.mem_used = nv.mem_used.or(s.mem_used);
                s.mem_total = nv.mem_total.or(s.mem_total);
                s.temperature = nv.temperature.or(s.temperature);
            }
            out.push(s);
        }
        // NVIDIA devices that were not visible in sysfs (unusual, but possible in containers).
        for (i, nv) in self.nv_samples.iter().enumerate() {
            if used_nv[i] {
                continue;
            }
            out.push(GpuSample {
                id: format!("nvidia{i}"),
                name: nv.name.clone(),
                vendor: "NVIDIA".into(),
                driver: nv.driver.clone(),
                util: nv.util,
                mem_used: nv.mem_used,
                mem_total: nv.mem_total,
                temperature: nv.temperature,
                pci_slot: nv.pci.clone(),
                ..Default::default()
            });
        }
        out
    }
}

struct FdinfoClient {
    pdev: String,
    client_id: u64,
    engines: Vec<(String, u64, u64)>,
    mem: Option<u64>,
}

fn parse_fdinfo(s: &str) -> Option<FdinfoClient> {
    let mut pdev = None;
    let mut client_id = None;
    let mut engines: Vec<(String, u64, u64)> = Vec::new();
    let mut caps: Vec<(String, u64)> = Vec::new();
    let mut mem_vram: Option<u64> = None;
    let mut mem_resident: Option<u64> = None;
    for line in s.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if key == "drm-pdev" {
            pdev = Some(normalize_pci(value));
        } else if key == "drm-client-id" {
            client_id = value.parse().ok();
        } else if let Some(engine) = key.strip_prefix("drm-engine-capacity-") {
            caps.push((engine.to_owned(), value.parse().unwrap_or(1)));
        } else if let Some(engine) = key.strip_prefix("drm-engine-") {
            let ns = value
                .split_ascii_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            engines.push((engine.to_owned(), ns, 1));
        } else if let Some(region) = key.strip_prefix("drm-memory-") {
            if region == "vram" {
                mem_vram = parse_mem_value(value);
            }
        } else if let Some(region) = key.strip_prefix("drm-resident-")
            && (region == "vram"
                || region == "local0"
                || (mem_resident.is_none() && region == "memory"))
        {
            mem_resident = parse_mem_value(value);
        }
    }
    for (engine, cap) in caps {
        if let Some(e) = engines.iter_mut().find(|e| e.0 == engine) {
            e.2 = cap;
        }
    }
    Some(FdinfoClient {
        pdev: pdev.unwrap_or_default(),
        client_id: client_id?,
        engines,
        mem: mem_resident.or(mem_vram),
    })
}

fn parse_mem_value(v: &str) -> Option<u64> {
    let mut it = v.split_ascii_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    Some(match it.next() {
        Some("KiB") => n * 1024,
        Some("MiB") => n * 1024 * 1024,
        Some("GiB") => n * 1024 * 1024 * 1024,
        _ => n,
    })
}

fn discover_cards() -> Vec<Card> {
    let mut cards = Vec::new();
    let Ok(read) = std::fs::read_dir("/sys/class/drm") else {
        return cards;
    };
    let mut names: Vec<String> = read
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with("card") && n[4..].bytes().all(|b| b.is_ascii_digit()) && n.len() > 4
        })
        .collect();
    names.sort_by_key(|n| n[4..].parse::<u32>().unwrap_or(0));
    let mut seen_pci = std::collections::HashSet::new();
    for name in names {
        let sysfs = PathBuf::from("/sys/class/drm").join(&name);
        let dev = sysfs.join("device");
        let pci = std::fs::canonicalize(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .map(|p| normalize_pci(&p))
            .unwrap_or_default();
        if !pci.is_empty() && !seen_pci.insert(pci.clone()) {
            continue;
        }
        let vendor_id = read_hex(dev.join("vendor")).unwrap_or(0);
        let device_id = read_hex(dev.join("device")).unwrap_or(0);
        let driver = std::fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_default();
        // Skip pure display controllers without render capability (e.g. simpledrm).
        if driver == "simple-framebuffer" || driver == "simpledrm" || driver == "vkms" {
            continue;
        }
        let vendor = match vendor_id {
            0x1002 => "AMD",
            0x10de => "NVIDIA",
            0x8086 => "Intel",
            0x1af4 => "Virtio",
            0x15ad => "VMware",
            0x1234 => "QEMU",
            0x80ee => "VirtualBox",
            _ => "",
        }
        .to_owned();
        let name = read_trimmed(dev.join("product_name"))
            .or_else(|| pci_ids_lookup(vendor_id, device_id))
            .unwrap_or_else(|| {
                let v = if vendor.is_empty() {
                    "GPU".to_owned()
                } else {
                    format!("{vendor} GPU")
                };
                if driver.is_empty() {
                    v
                } else {
                    format!("{v} ({driver})")
                }
            });
        let hwmon = std::fs::read_dir(dev.join("hwmon"))
            .ok()
            .and_then(|mut r| r.next())
            .and_then(|e| e.ok())
            .map(|e| e.path());
        cards.push(Card {
            sysfs,
            pci,
            vendor_id,
            name,
            vendor,
            driver,
            hwmon,
        });
    }
    cards
}

fn read_hex(path: impl AsRef<Path>) -> Option<u32> {
    let s = read_trimmed(path)?;
    u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// Resolves a PCI device name from the system `pci.ids` database, if installed.
fn pci_ids_lookup(vendor: u32, device: u32) -> Option<String> {
    let paths = [
        "/usr/share/hwdata/pci.ids",
        "/usr/share/misc/pci.ids",
        "/usr/share/pci.ids",
        "/usr/local/share/pci.ids",
    ];
    let content = paths.iter().find_map(|p| std::fs::read_to_string(p).ok())?;
    lookup_pci_name(&content, vendor, device)
}

fn lookup_pci_name(content: &str, vendor: u32, device: u32) -> Option<String> {
    let vendor_hex = format!("{vendor:04x}");
    let device_hex = format!("{device:04x}");
    let mut in_vendor = false;
    let mut vendor_name = "";
    for line in content.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if in_vendor {
                break;
            }
            if line.starts_with(&vendor_hex) {
                in_vendor = true;
                vendor_name = line[4..].trim();
            }
            continue;
        }
        if in_vendor && !line.starts_with("\t\t") {
            let l = &line[1..];
            if l.starts_with(&device_hex) {
                let dev_name = l[4..].trim();
                // Prefer the marketing name in brackets, e.g. "Navi 21 [Radeon RX 6800]".
                let short_vendor = vendor_name
                    .split(['[', ','])
                    .next()
                    .unwrap_or(vendor_name)
                    .replace("Advanced Micro Devices, Inc.", "AMD")
                    .replace(" Corporation", "")
                    .trim()
                    .to_owned();
                let short_vendor = if vendor_name.contains("[AMD/ATI]") || vendor == 0x1002 {
                    "AMD".to_owned()
                } else {
                    short_vendor
                };
                let model = match (dev_name.find('['), dev_name.rfind(']')) {
                    (Some(a), Some(b)) if b > a => &dev_name[a + 1..b],
                    _ => dev_name,
                };
                return Some(format!("{short_vendor} {model}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdinfo_amdgpu() {
        let s = "pos:\t0\nflags:\t02100002\ndrm-driver:\tamdgpu\ndrm-pdev:\t0000:03:00.0\ndrm-client-id:\t42\ndrm-memory-vram:\t1024 KiB\ndrm-engine-gfx:\t1000000 ns\ndrm-engine-compute:\t0 ns\n";
        let c = parse_fdinfo(s).unwrap();
        assert_eq!(c.pdev, "0000:03:00.0");
        assert_eq!(c.client_id, 42);
        assert_eq!(c.mem, Some(1024 * 1024));
        assert_eq!(c.engines[0], ("gfx".to_owned(), 1_000_000, 1));
    }

    #[test]
    fn fdinfo_i915_capacity() {
        let s = "drm-driver:\ti915\ndrm-pdev:\t0000:00:02.0\ndrm-client-id:\t7\ndrm-engine-render:\t500 ns\ndrm-engine-video:\t0 ns\ndrm-engine-capacity-video:\t2\n";
        let c = parse_fdinfo(s).unwrap();
        assert_eq!(c.engines.iter().find(|e| e.0 == "video").unwrap().2, 2);
        assert!(parse_fdinfo("pos: 0\nflags: 0\n").is_none());
    }

    #[test]
    fn process_accounting() {
        let mut m = GpuMonitor {
            cards: Vec::new(),
            nvidia: None,
            last_engine_ns: HashMap::new(),
            engine_busy: HashMap::new(),
            engine_capacity: HashMap::new(),
            clients_seen: HashMap::new(),
            seen_keys: Default::default(),
            elapsed_ns: 1e9,
            nv_procs: HashMap::new(),
            nv_samples: Vec::new(),
        };
        let first = "drm-pdev:\t0000:03:00.0\ndrm-client-id:\t1\ndrm-engine-gfx:\t1000000000 ns\n";
        let second = "drm-pdev:\t0000:03:00.0\ndrm-client-id:\t1\ndrm-engine-gfx:\t1250000000 ns\n";
        m.begin(1.0);
        let p = m.account_process(10, [first, first].into_iter()).unwrap();
        assert_eq!(p.util, 0.0);
        m.finish();
        m.begin(1.0);
        let p = m.account_process(10, [second].into_iter()).unwrap();
        assert!((p.util - 25.0).abs() < 0.01);
        assert!((p.busy_secs - 0.25).abs() < 1e-6);
    }

    #[test]
    fn pci_ids() {
        let db = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]\n\t\t1002 0e3a  Some subsystem\n10de  NVIDIA Corporation\n\t2684  AD102 [GeForce RTX 4090]\n8086  Intel Corporation\n\t46a6  Alder Lake-P GT2 [Iris Xe Graphics]\n";
        assert_eq!(
            lookup_pci_name(db, 0x1002, 0x73bf).as_deref(),
            Some("AMD Radeon RX 6800/6800 XT / 6900 XT")
        );
        assert_eq!(
            lookup_pci_name(db, 0x10de, 0x2684).as_deref(),
            Some("NVIDIA GeForce RTX 4090")
        );
        assert_eq!(
            lookup_pci_name(db, 0x8086, 0x46a6).as_deref(),
            Some("Intel Iris Xe Graphics")
        );
        assert_eq!(lookup_pci_name(db, 0x8086, 0xffff), None);
    }
}

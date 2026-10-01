//! NVIDIA GPU statistics through NVML, which is loaded at runtime.
//! When the driver (and therefore the library) is absent this module is a no-op.

#[derive(Debug, Clone, Default)]
pub struct NvGpu {
    /// Normalized PCI address (`0000:01:00.0`).
    pub pci: String,
    pub name: String,
    pub util: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub temperature: Option<f32>,
    pub driver: String,
    /// (pid, utilization %, memory bytes)
    pub procs: Vec<(u32, Option<f32>, Option<u64>)>,
}

/// Normalizes `00000000:01:00.0` / `0000:01:00.0` to `0000:01:00.0`.
pub fn normalize_pci(id: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    let mut parts = id.splitn(2, ':');
    match (parts.next(), parts.next()) {
        (Some(domain), Some(rest)) => match u32::from_str_radix(domain, 16) {
            Ok(d) => format!("{d:04x}:{rest}"),
            Err(_) => id.clone(),
        },
        _ => id.clone(),
    }
}

#[cfg(feature = "nvidia")]
mod imp {
    use super::*;
    use nvml_wrapper::Nvml;
    use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
    use nvml_wrapper::enums::device::UsedGpuMemory;

    pub struct Nvidia {
        nvml: Nvml,
        last_ts: Vec<Option<u64>>,
        driver: String,
    }

    impl Nvidia {
        pub fn init() -> Option<Nvidia> {
            let nvml = Nvml::init().ok()?;
            let count = nvml.device_count().ok()? as usize;
            if count == 0 {
                return None;
            }
            let driver = nvml.sys_driver_version().unwrap_or_default();
            Some(Nvidia {
                nvml,
                last_ts: vec![None; count],
                driver,
            })
        }

        pub fn sample(&mut self, with_processes: bool) -> Vec<NvGpu> {
            let mut out = Vec::with_capacity(self.last_ts.len());
            for i in 0..self.last_ts.len() {
                let Ok(dev) = self.nvml.device_by_index(i as u32) else {
                    continue;
                };
                let mut g = NvGpu {
                    pci: dev
                        .pci_info()
                        .map(|p| normalize_pci(&p.bus_id))
                        .unwrap_or_default(),
                    name: dev.name().unwrap_or_else(|_| "NVIDIA GPU".to_owned()),
                    driver: format!("nvidia {}", self.driver),
                    ..Default::default()
                };
                g.util = dev.utilization_rates().ok().map(|u| u.gpu as f32);
                if let Ok(m) = dev.memory_info() {
                    g.mem_used = Some(m.used);
                    g.mem_total = Some(m.total);
                }
                g.temperature = dev
                    .temperature(TemperatureSensor::Gpu)
                    .ok()
                    .map(|t| t as f32);
                if with_processes {
                    let mut procs: Vec<(u32, Option<f32>, Option<u64>)> = Vec::new();
                    let mut add_mem = |pid: u32, mem: &UsedGpuMemory| {
                        let bytes = match mem {
                            UsedGpuMemory::Used(b) => Some(*b),
                            UsedGpuMemory::Unavailable => None,
                        };
                        match procs.iter_mut().find(|p| p.0 == pid) {
                            Some(p) => p.2 = p.2.or(bytes),
                            None => procs.push((pid, None, bytes)),
                        }
                    };
                    for p in dev.running_graphics_processes().unwrap_or_default() {
                        add_mem(p.pid, &p.used_gpu_memory);
                    }
                    for p in dev.running_compute_processes().unwrap_or_default() {
                        add_mem(p.pid, &p.used_gpu_memory);
                    }
                    if let Ok(samples) = dev.process_utilization_stats(self.last_ts[i]) {
                        let mut newest = self.last_ts[i].unwrap_or(0);
                        for s in samples {
                            newest = newest.max(s.timestamp);
                            let util = s.sm_util.max(s.enc_util).max(s.dec_util).min(100) as f32;
                            match procs.iter_mut().find(|p| p.0 == s.pid) {
                                Some(p) => p.1 = Some(p.1.unwrap_or(0.0).max(util)),
                                None => procs.push((s.pid, Some(util), None)),
                            }
                        }
                        if newest > 0 {
                            self.last_ts[i] = Some(newest);
                        }
                    }
                    g.procs = procs;
                }
                out.push(g);
            }
            out
        }
    }
}

#[cfg(not(feature = "nvidia"))]
mod imp {
    use super::*;

    pub struct Nvidia;

    impl Nvidia {
        pub fn init() -> Option<Nvidia> {
            None
        }

        pub fn sample(&mut self, _with_processes: bool) -> Vec<NvGpu> {
            Vec::new()
        }
    }
}

pub use imp::Nvidia;

#[cfg(test)]
mod tests {
    use super::normalize_pci;

    #[test]
    fn pci_normalization() {
        assert_eq!(normalize_pci("00000000:01:00.0"), "0000:01:00.0");
        assert_eq!(normalize_pci("0000:0A:00.0"), "0000:0a:00.0");
    }
}

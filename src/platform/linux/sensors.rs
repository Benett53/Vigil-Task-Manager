//! Hardware sensors: hwmon chips (temperatures, fans, power, voltages), thermal zones
//! and batteries.
//!
//! Chips are discovered once a minute; each refresh only reads the `*_input` files.
//! Every chip remembers the canonical sysfs path of the device it belongs to, which
//! lets the collector attach temperatures to disks, network adapters and GPUs.

use super::procfs::{read_trimmed, read_u64};
use crate::model::{BatterySample, SensorKind, SensorReading};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Input {
    kind: SensorKind,
    label: String,
    input: PathBuf,
    high: Option<f32>,
    crit: Option<f32>,
}

#[derive(Debug)]
struct Chip {
    /// Driver name from `name` (e.g. `k10temp`, `nvme`, `drivetemp`).
    driver: String,
    display: String,
    /// Canonical path of the device this chip belongs to.
    device: Option<PathBuf>,
    inputs: Vec<Input>,
}

/// Latest values of one chip.
#[derive(Debug, Clone)]
pub struct ChipValues {
    pub driver: String,
    pub device: Option<PathBuf>,
    pub temps: Vec<(String, f32)>,
}

pub struct Sensors {
    root: PathBuf,
    chips: Vec<Chip>,
    thermal_cpu: Option<PathBuf>,
    discovered: Option<Instant>,
    pub readings: Vec<SensorReading>,
    pub chip_values: Vec<ChipValues>,
}

fn scale(kind: SensorKind) -> f32 {
    match kind {
        SensorKind::Temperature | SensorKind::Voltage | SensorKind::Current => 1000.0,
        SensorKind::Power => 1_000_000.0,
        SensorKind::Fan => 1.0,
    }
}

fn friendly_driver(driver: &str) -> &str {
    match driver {
        "k10temp" | "zenpower" => "CPU (AMD)",
        "coretemp" => "CPU (Intel)",
        "cpu_thermal" | "cpu-thermal" | "soc_thermal" => "CPU",
        "nvme" => "NVMe SSD",
        "drivetemp" => "Disk",
        "amdgpu" => "AMD GPU",
        "radeon" => "AMD GPU (radeon)",
        "nouveau" => "NVIDIA GPU (nouveau)",
        "i915" | "xe" => "Intel GPU",
        "iwlwifi" | "iwlwifi_1" => "Wi-Fi",
        "mt7921_phy0" | "ath11k_hwmon" | "ath10k_hwmon" => "Wi-Fi",
        "acpitz" => "ACPI thermal zone",
        "spd5118" | "jc42" => "Memory module",
        "thinkpad" => "ThinkPad embedded controller",
        "dell_smm" => "Dell embedded controller",
        "asus" | "asus_ec_sensors" | "asus-ec-sensors" => "ASUS embedded controller",
        "nct6775" | "nct6683" | "nct6687" | "it87" | "it8686" | "it8688" | "f71882fg" => {
            "Motherboard"
        }
        "BAT0" | "BAT1" => "Battery",
        "pch_skylake" | "pch_cannonlake" | "pch_cometlake" | "pch_tigerlake" => "Chipset",
        "r8169" | "r8169_0_100:00" | "igc" | "e1000e" | "ixgbe" | "ice" => "Ethernet",
        other if other.starts_with("r8169") => "Ethernet",
        other if other.starts_with("pch_") => "Chipset",
        other => other,
    }
}

impl Sensors {
    pub fn with_root(root: impl Into<PathBuf>) -> Sensors {
        Sensors {
            root: root.into(),
            chips: Vec::new(),
            thermal_cpu: None,
            discovered: None,
            readings: Vec::new(),
            chip_values: Vec::new(),
        }
    }

    fn discover(&mut self) {
        self.chips.clear();
        let hwmon_dir = self.root.join("class/hwmon");
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&hwmon_dir)
            .map(|r| r.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort_by_key(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.trim_start_matches("hwmon").parse::<u32>().ok())
                .unwrap_or(u32::MAX)
        });
        let mut counts: std::collections::HashMap<String, usize> = Default::default();
        for dir in entries {
            let driver = read_trimmed(dir.join("name")).unwrap_or_else(|| "unknown".into());
            let device = std::fs::canonicalize(dir.join("device")).ok();
            let mut inputs = Vec::new();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names: Vec<String> = files
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter(|n| {
                    n.ends_with("_input") || (n.starts_with("power") && n.ends_with("_average"))
                })
                .collect();
            names.sort_by_key(|a| natural_key(a));
            for file in names {
                let prefix = file.trim_end_matches("_input").trim_end_matches("_average");
                let kind = if prefix.starts_with("temp") {
                    SensorKind::Temperature
                } else if prefix.starts_with("fan") {
                    SensorKind::Fan
                } else if prefix.starts_with("power") {
                    SensorKind::Power
                } else if prefix.starts_with("in") {
                    SensorKind::Voltage
                } else if prefix.starts_with("curr") {
                    SensorKind::Current
                } else {
                    continue;
                };
                // Prefer power*_input over *_average when both exist.
                if file.ends_with("_average") && dir.join(format!("{prefix}_input")).exists() {
                    continue;
                }
                let label = read_trimmed(dir.join(format!("{prefix}_label")))
                    .unwrap_or_else(|| default_label(kind, prefix));
                let limit = |suffix: &str| {
                    read_trimmed(dir.join(format!("{prefix}_{suffix}")))
                        .and_then(|v| v.parse::<f32>().ok())
                        .map(|v| v / scale(kind))
                        .filter(|v| *v > 0.0 && *v < 1000.0)
                };
                inputs.push(Input {
                    kind,
                    label,
                    input: dir.join(&file),
                    high: limit("max"),
                    crit: limit("crit"),
                });
            }
            if inputs.is_empty() {
                continue;
            }
            let base = friendly_driver(&driver).to_owned();
            let n = counts.entry(base.clone()).or_default();
            *n += 1;
            let display = if *n > 1 { format!("{base} #{n}") } else { base };
            self.chips.push(Chip {
                driver,
                display,
                device,
                inputs,
            });
        }
        // Fallback CPU temperature from the thermal framework (ARM boards, some laptops).
        self.thermal_cpu = None;
        if let Ok(read) = std::fs::read_dir(self.root.join("class/thermal")) {
            let mut zones: Vec<PathBuf> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone"))
                })
                .collect();
            zones.sort();
            for z in zones {
                let ty = read_trimmed(z.join("type")).unwrap_or_default();
                if [
                    "x86_pkg_temp",
                    "cpu-thermal",
                    "cpu_thermal",
                    "cpu",
                    "soc_thermal",
                    "cpu0-thermal",
                    "cpuss0-thermal",
                ]
                .iter()
                .any(|t| ty.eq_ignore_ascii_case(t))
                {
                    self.thermal_cpu = Some(z.join("temp"));
                    break;
                }
            }
        }
        self.discovered = Some(Instant::now());
    }

    pub fn refresh(&mut self) {
        if self
            .discovered
            .is_none_or(|t| t.elapsed() > Duration::from_secs(60))
        {
            self.discover();
        }
        self.readings.clear();
        self.chip_values.clear();
        for chip in &self.chips {
            let mut temps = Vec::new();
            for input in &chip.inputs {
                let Some(raw) = read_trimmed(&input.input).and_then(|v| v.parse::<f64>().ok())
                else {
                    continue;
                };
                let value = (raw / scale(input.kind) as f64) as f32;
                // Discard obviously bogus readings (disconnected probes report -127 / 127 °C etc.).
                if input.kind == SensorKind::Temperature && !(-40.0..=150.0).contains(&value) {
                    continue;
                }
                if input.kind == SensorKind::Temperature && value != 0.0 {
                    temps.push((input.label.clone(), value));
                }
                self.readings.push(SensorReading {
                    chip: chip.display.clone(),
                    label: input.label.clone(),
                    kind: input.kind,
                    value,
                    high: input.high,
                    crit: input.crit,
                });
            }
            self.chip_values.push(ChipValues {
                driver: chip.driver.clone(),
                device: chip.device.clone(),
                temps,
            });
        }
    }

    /// Package temperature plus individual core temperatures.
    pub fn cpu(&self) -> (Option<f32>, Vec<(String, f32)>) {
        const CPU_DRIVERS: [&str; 6] = [
            "k10temp",
            "zenpower",
            "coretemp",
            "cpu_thermal",
            "cpu-thermal",
            "soc_thermal",
        ];
        let mut package = None;
        let mut cores = Vec::new();
        for chip in self
            .chip_values
            .iter()
            .filter(|c| CPU_DRIVERS.contains(&c.driver.as_str()))
        {
            for (label, v) in &chip.temps {
                let is_package = label.starts_with("Tctl")
                    || label.starts_with("Tdie")
                    || label.starts_with("Package")
                    || label.starts_with("CPU");
                if is_package {
                    // Tdie (no offset) wins over Tctl.
                    if package.is_none() || label.starts_with("Tdie") {
                        package = Some(*v);
                    }
                } else {
                    cores.push((label.clone(), *v));
                }
            }
        }
        if package.is_none() {
            package = cores.iter().map(|c| c.1).reduce(f32::max);
        }
        if package.is_none() {
            package = self
                .thermal_cpu
                .as_ref()
                .and_then(read_u64)
                .map(|m| m as f32 / 1000.0)
                .filter(|t| (-40.0..=150.0).contains(t));
        }
        (package, cores)
    }

    /// Highest temperature of a chip belonging to `device` (or one of its child devices).
    pub fn temp_for_device(&self, device: &Path) -> Option<f32> {
        self.chip_values
            .iter()
            .filter(|c| c.device.as_ref().is_some_and(|d| d.starts_with(device)))
            .flat_map(|c| {
                // NVMe exposes "Composite" (the drive's own estimate) — prefer it when present.
                let composite = c.temps.iter().find(|t| t.0 == "Composite").map(|t| t.1);
                composite
                    .into_iter()
                    .chain(c.temps.iter().map(|t| t.1).take(1))
            })
            .reduce(f32::max)
    }

    /// Temperature of the memory modules (maximum over all DIMM sensors).
    pub fn memory(&self) -> Option<f32> {
        self.chip_values
            .iter()
            .filter(|c| c.driver == "spd5118" || c.driver == "jc42")
            .flat_map(|c| c.temps.iter().map(|t| t.1))
            .reduce(f32::max)
    }
}

fn default_label(kind: SensorKind, prefix: &str) -> String {
    let n = prefix.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let what = match kind {
        SensorKind::Temperature => "Temperature",
        SensorKind::Fan => "Fan",
        SensorKind::Power => "Power",
        SensorKind::Voltage => "Voltage",
        SensorKind::Current => "Current",
    };
    if n.is_empty() || n == "1" && kind != SensorKind::Voltage {
        what.to_owned()
    } else {
        format!("{what} {n}")
    }
}

/// Sort key so that `temp10` comes after `temp2`.
fn natural_key(s: &str) -> (String, u32) {
    let alpha: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let num: u32 = s[alpha.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    (alpha, num)
}

/// Batteries from `/sys/class/power_supply`.
pub fn batteries(root: &Path) -> Vec<BatterySample> {
    let dir = root.join("class/power_supply");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut supplies: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    supplies.sort();
    let on_ac = supplies
        .iter()
        .filter(|p| read_trimmed(p.join("type")).as_deref() == Some("Mains"))
        .map(|p| read_u64(p.join("online")) == Some(1))
        .reduce(|a, b| a || b);
    let mut out = Vec::new();
    for p in supplies {
        if read_trimmed(p.join("type")).as_deref() != Some("Battery") {
            continue;
        }
        // Peripheral batteries (mice, headsets) report scope=Device.
        if read_trimmed(p.join("scope")).as_deref() == Some("Device") {
            continue;
        }
        let micro = |f: &str| read_u64(p.join(f)).map(|v| v as f32 / 1_000_000.0);
        let voltage = micro("voltage_now").or_else(|| micro("voltage_min_design"));
        // Energy is given either in µWh (energy_*) or as charge in µAh (charge_*).
        let energy = |e: &str, c: &str| micro(e).or_else(|| Some(micro(c)? * voltage?));
        let energy_now = energy("energy_now", "charge_now");
        let energy_full = energy("energy_full", "charge_full");
        let energy_design = energy("energy_full_design", "charge_full_design");
        let power = micro("power_now")
            .or_else(|| Some(micro("current_now")? * voltage?))
            .map(f32::abs)
            .filter(|w| *w > 0.0);
        let percent = read_u64(p.join("capacity"))
            .map(|c| c as f32)
            .or_else(|| match (energy_now, energy_full) {
                (Some(n), Some(f)) if f > 0.0 => Some(n / f * 100.0),
                _ => None,
            })
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let status = read_trimmed(p.join("status")).unwrap_or_else(|| "Unknown".into());
        let time_remaining_secs = match (power, energy_now, energy_full) {
            (Some(w), Some(now), _) if status == "Discharging" => Some((now / w * 3600.0) as u64),
            (Some(w), Some(now), Some(full)) if status == "Charging" && full > now => {
                Some(((full - now) / w * 3600.0) as u64)
            }
            _ => None,
        };
        let model = [
            read_trimmed(p.join("manufacturer")),
            read_trimmed(p.join("model_name")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        out.push(BatterySample {
            id: p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            model,
            percent,
            status,
            on_ac,
            power_w: power,
            energy_wh: energy_now,
            energy_full_wh: energy_full,
            energy_design_wh: energy_design,
            cycles: read_u64(p.join("cycle_count"))
                .map(|c| c as u32)
                .filter(|c| *c > 0),
            time_remaining_secs,
            temperature: read_u64(p.join("temp")).map(|t| t as f32 / 10.0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn write(p: &Path, content: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn fake_sys(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vigil-sys-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // CPU (k10temp)
        let cpu = root.join("devices/pci0000:00/0000:00:18.3");
        let h0 = root.join("class/hwmon/hwmon0");
        write(&h0.join("name"), "k10temp\n");
        write(&h0.join("temp1_input"), "65250\n");
        write(&h0.join("temp1_label"), "Tctl\n");
        write(&h0.join("temp3_input"), "58000\n");
        write(&h0.join("temp3_label"), "Tccd1\n");
        fs::create_dir_all(&cpu).unwrap();
        symlink(&cpu, h0.join("device")).unwrap();
        // NVMe drive
        let nvme = root.join("devices/pci0000:00/0000:01:00.0/nvme/nvme0");
        let h1 = root.join("class/hwmon/hwmon1");
        write(&h1.join("name"), "nvme\n");
        write(&h1.join("temp1_input"), "41850\n");
        write(&h1.join("temp1_label"), "Composite\n");
        write(&h1.join("temp1_crit"), "84850\n");
        write(&h1.join("temp2_input"), "45850\n");
        fs::create_dir_all(&nvme).unwrap();
        symlink(&nvme, h1.join("device")).unwrap();
        fs::create_dir_all(root.join("block/nvme0n1")).unwrap();
        symlink(&nvme, root.join("block/nvme0n1/device")).unwrap();
        // Motherboard fan, bogus probe and voltage
        let h2 = root.join("class/hwmon/hwmon2");
        write(&h2.join("name"), "nct6775\n");
        write(&h2.join("fan2_input"), "1043\n");
        write(&h2.join("temp7_input"), "-127000\n");
        write(&h2.join("in0_input"), "1210\n");
        // DDR5 module
        let h3 = root.join("class/hwmon/hwmon3");
        write(&h3.join("name"), "spd5118\n");
        write(&h3.join("temp1_input"), "38500\n");
        // Laptop battery and AC adapter
        let bat = root.join("class/power_supply/BAT0");
        write(&bat.join("type"), "Battery\n");
        write(&bat.join("status"), "Discharging\n");
        write(&bat.join("capacity"), "80\n");
        write(&bat.join("energy_now"), "40000000\n");
        write(&bat.join("energy_full"), "50000000\n");
        write(&bat.join("energy_full_design"), "57000000\n");
        write(&bat.join("power_now"), "10000000\n");
        write(&bat.join("cycle_count"), "123\n");
        write(&bat.join("model_name"), "5B10W\n");
        let ac = root.join("class/power_supply/AC");
        write(&ac.join("type"), "Mains\n");
        write(&ac.join("online"), "0\n");
        let mouse = root.join("class/power_supply/hidpp_battery_0");
        write(&mouse.join("type"), "Battery\n");
        write(&mouse.join("scope"), "Device\n");
        root
    }

    #[test]
    fn hwmon_discovery_and_mapping() {
        let root = fake_sys("hwmon");
        let mut s = Sensors::with_root(&root);
        s.refresh();

        let (pkg, cores) = s.cpu();
        assert_eq!(pkg, Some(65.25));
        assert_eq!(cores, vec![("Tccd1".to_owned(), 58.0)]);

        let disk_dev = fs::canonicalize(root.join("block/nvme0n1/device")).unwrap();
        assert_eq!(s.temp_for_device(&disk_dev), Some(41.85));
        assert_eq!(s.memory(), Some(38.5));

        let fan = s
            .readings
            .iter()
            .find(|r| r.kind == SensorKind::Fan)
            .unwrap();
        assert_eq!(fan.value, 1043.0);
        assert_eq!(fan.chip, "Motherboard");
        assert_eq!(fan.label, "Fan 2");
        let volt = s
            .readings
            .iter()
            .find(|r| r.kind == SensorKind::Voltage)
            .unwrap();
        assert!((volt.value - 1.21).abs() < 1e-4);
        assert!(
            s.readings.iter().all(|r| r.value > -100.0),
            "bogus -127 °C reading must be dropped"
        );
        let crit = s
            .readings
            .iter()
            .find(|r| r.label == "Composite")
            .unwrap()
            .crit;
        assert_eq!(crit, Some(84.85));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn battery_parsing() {
        let root = fake_sys("battery");
        let b = batteries(&root);
        assert_eq!(b.len(), 1, "peripheral batteries are skipped");
        let b = &b[0];
        assert_eq!(b.percent, 80.0);
        assert_eq!(b.on_ac, Some(false));
        assert_eq!(b.power_w, Some(10.0));
        assert_eq!(b.time_remaining_secs, Some(4 * 3600));
        assert_eq!(b.cycles, Some(123));
        assert!((b.health().unwrap() - 87.719).abs() < 0.01);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn labels_and_order() {
        assert_eq!(default_label(SensorKind::Fan, "fan1"), "Fan");
        assert_eq!(default_label(SensorKind::Fan, "fan3"), "Fan 3");
        assert_eq!(default_label(SensorKind::Voltage, "in1"), "Voltage 1");
        let mut v = vec!["temp10_input", "temp2_input", "fan1_input"];
        v.sort_by_key(|a| natural_key(a));
        assert_eq!(v, vec!["fan1_input", "temp2_input", "temp10_input"]);
    }
}

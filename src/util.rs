//! Small, dependency-free helpers shared by the collectors and the UI.

/// Formats a byte count using binary prefixes ("1.5 GB" style, as Task Manager does).
pub fn fmt_bytes(bytes: u64) -> String {
    fmt_bytes_f(bytes as f64)
}

pub fn fmt_bytes_f(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if !bytes.is_finite() || bytes <= 0.0 {
        return "0 B".to_owned();
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Formats a transfer rate given in bytes per second.
pub fn fmt_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", fmt_bytes_f(bytes_per_sec))
}

/// Formats a transfer rate in bits per second (used for network adapters).
pub fn fmt_bits(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    let mut value = (bytes_per_sec * 8.0).max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a duration as `d:hh:mm:ss` like the Task Manager "Up time" field.
pub fn fmt_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;
    format!("{days}:{hours:02}:{mins:02}:{s:02}")
}

/// Formats accumulated CPU time as `h:mm:ss`.
pub fn fmt_cpu_time(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    };
    let hours = total / 3_600;
    let mins = (total % 3_600) / 60;
    let s = total % 60;
    format!("{hours}:{mins:02}:{s:02}")
}

pub fn fmt_freq_mhz(mhz: f64) -> String {
    if mhz >= 1000.0 {
        format!("{:.2} GHz", mhz / 1000.0)
    } else {
        format!("{mhz:.0} MHz")
    }
}

pub fn fmt_percent(p: f32) -> String {
    if p >= 10.0 || p == 0.0 {
        format!("{p:.0}%")
    } else {
        format!("{p:.1}%")
    }
}

/// Formats a unix timestamp (seconds) as local `YYYY-MM-DD HH:MM:SS`.
pub fn fmt_unix_time(secs: u64) -> String {
    let offset = local_utc_offset_secs(secs as i64);
    let t = secs as i64 + offset;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date.
/// Algorithm by Howard Hinnant (public domain).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(unix)]
fn local_utc_offset_secs(at: i64) -> i64 {
    // SAFETY: localtime_r only writes into the provided, properly sized struct.
    unsafe {
        let t: libc::time_t = at as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

#[cfg(windows)]
fn local_utc_offset_secs(_at: i64) -> i64 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // SAFETY: plain Win32 call writing into a zeroed struct we own.
    unsafe {
        let mut tzi: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tzi);
        // 2 == TIME_ZONE_ID_DAYLIGHT
        let bias = tzi.Bias
            + if id == 2 {
                tzi.DaylightBias
            } else {
                tzi.StandardBias
            };
        -(bias as i64) * 60
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Percent-encodes a string for use in a URL query component.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Case-insensitive substring match without allocating for ASCII needles.
pub fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h
            .windows(n.len())
            .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b));
    }
    haystack.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_formatting() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.50 KB");
        assert_eq!(fmt_bytes(15 * 1024 * 1024), "15.0 MB");
        assert_eq!(fmt_bytes(300 * 1024 * 1024 * 1024), "300 GB");
    }

    #[test]
    fn bits_formatting() {
        assert_eq!(fmt_bits(0.0), "0 bps");
        assert_eq!(fmt_bits(125.0), "1.0 Kbps");
        assert_eq!(fmt_bits(125_000_000.0), "1.0 Gbps");
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(fmt_uptime(0), "0:00:00:00");
        assert_eq!(fmt_uptime(90_061), "1:01:01:01");
        assert_eq!(fmt_cpu_time(3_725.9), "1:02:05");
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn url_encoding() {
        assert_eq!(url_encode("a b&c/ä"), "a+b%26c%2F%C3%A4");
    }

    #[test]
    fn case_insensitive_contains() {
        assert!(contains_ci("FireFox", "fox"));
        assert!(!contains_ci("abc", "abcd"));
        assert!(contains_ci("anything", ""));
        assert!(contains_ci("Ärger", "ärg"));
    }
}

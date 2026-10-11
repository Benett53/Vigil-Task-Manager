//! Physical disks with Task Manager's metrics: active time, throughput and response time
//! from the disk performance counters (`IOCTL_DISK_PERFORMANCE`), drive letters per disk.

use super::wide;
use crate::model::DiskSample;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeNvme, BusTypeUsb, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetLogicalDrives,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_GEOMETRY_EX, DISK_PERFORMANCE,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_PERFORMANCE, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty, StorageDeviceSeekPenaltyProperty, VOLUME_DISK_EXTENTS,
};

struct Handle(HANDLE);

impl Handle {
    /// Opens a device for queries only (no read/write access, no admin rights needed).
    fn open(path: &str) -> Option<Handle> {
        let p = wide(path);
        // SAFETY: plain CreateFileW call; the handle is closed on drop.
        let h = unsafe {
            CreateFileW(
                p.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Handle(h))
    }

    /// Sends an IOCTL with an optional input struct and reads the output into `out`.
    fn ioctl<I, O>(&self, code: u32, input: Option<&I>, out: &mut O) -> bool {
        let mut ret = 0u32;
        // SAFETY: input and output point to live values of the stated sizes.
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                input.map_or(std::ptr::null(), |i| i as *const I as *const c_void),
                input.map_or(0, |_| size_of::<I>() as u32),
                out as *mut O as *mut c_void,
                size_of::<O>() as u32,
                &mut ret,
                std::ptr::null_mut(),
            ) != 0
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Disk {
    number: u32,
    handle: Handle,
    model: String,
    kind: String,
    capacity: u64,
    removable: bool,
    letters: Vec<char>,
    last: Option<DISK_PERFORMANCE>,
}

#[derive(Default)]
pub struct Disks {
    disks: Vec<Disk>,
    listed: Option<Instant>,
    last: Option<Instant>,
}

impl Disks {
    pub fn sample(&mut self) -> Vec<DiskSample> {
        let now = Instant::now();
        // Disks and drive letters rarely change; re-enumerate every 30 seconds.
        if self
            .listed
            .is_none_or(|t| now.duration_since(t).as_secs() >= 30)
        {
            self.enumerate();
            self.listed = Some(now);
        }
        let elapsed = self
            .last
            .map(|l| now.duration_since(l).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.05);
        self.last = Some(now);
        let system_letter = std::env::var("SystemDrive")
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('C')
            .to_ascii_uppercase();

        self.disks
            .iter_mut()
            .map(|d| {
                // SAFETY: plain data out-struct.
                let mut perf: DISK_PERFORMANCE = unsafe { zeroed() };
                let ok = d
                    .handle
                    .ioctl::<(), _>(IOCTL_DISK_PERFORMANCE, None, &mut perf);
                let mut sample = DiskSample {
                    id: format!("PhysicalDrive{}", d.number),
                    model: d.model.clone(),
                    kind: d.kind.clone(),
                    capacity: d.capacity,
                    mounts: d.letters.iter().map(|l| format!("{l}:")).collect(),
                    system_disk: d.letters.contains(&system_letter),
                    removable: d.removable,
                    ..Default::default()
                };
                if ok {
                    if let Some(prev) = &d.last {
                        let span = (perf.QueryTime - prev.QueryTime).max(1) as f64;
                        let idle = (perf.IdleTime - prev.IdleTime).max(0) as f64;
                        sample.active =
                            Some(((1.0 - idle / span) * 100.0).clamp(0.0, 100.0) as f32);
                        sample.read_bps = (perf.BytesRead - prev.BytesRead).max(0) as f64 / elapsed;
                        sample.write_bps =
                            (perf.BytesWritten - prev.BytesWritten).max(0) as f64 / elapsed;
                        let ops = perf.ReadCount.wrapping_sub(prev.ReadCount) as f64
                            + perf.WriteCount.wrapping_sub(prev.WriteCount) as f64;
                        let busy = (perf.ReadTime - prev.ReadTime + perf.WriteTime - prev.WriteTime)
                            .max(0) as f64;
                        // Times are in 100 ns units.
                        sample.avg_response_ms = Some(if ops > 0.0 {
                            busy / ops / 10_000.0
                        } else {
                            0.0
                        });
                    }
                    d.last = Some(perf);
                }
                sample
            })
            .collect()
    }

    fn enumerate(&mut self) {
        let letters = drive_letters();
        let mut old: Vec<Disk> = std::mem::take(&mut self.disks);
        for number in 0..32u32 {
            let Some(handle) = Handle::open(&format!("\\\\.\\PhysicalDrive{number}")) else {
                continue;
            };
            let mut d = match old.iter().position(|d| d.number == number) {
                // Keep the previous counters so rates continue smoothly.
                Some(i) => {
                    let mut d = old.swap_remove(i);
                    d.handle = handle;
                    d
                }
                None => describe(number, handle),
            };
            d.letters = letters
                .iter()
                .filter(|(_, disk)| *disk == number)
                .map(|(l, _)| *l)
                .collect();
            self.disks.push(d);
        }
    }
}

fn describe(number: u32, handle: Handle) -> Disk {
    let mut model = String::new();
    let mut bus_usb = false;
    let mut nvme = false;
    let mut removable = false;
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut buf = [0u64; 128];
    if handle.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, Some(&query), &mut buf) {
        let bytes = buf.as_ptr() as *const u8;
        // SAFETY: the descriptor sits at the start of the buffer, strings at offsets into it.
        unsafe {
            let desc = &*(bytes as *const STORAGE_DEVICE_DESCRIPTOR);
            let text = |off: u32| -> String {
                if off == 0 || off as usize >= size_of_val(&buf) {
                    return String::new();
                }
                let start = bytes.add(off as usize);
                let max = size_of_val(&buf) - off as usize;
                let len = (0..max).take_while(|&i| *start.add(i) != 0).count();
                String::from_utf8_lossy(std::slice::from_raw_parts(start, len))
                    .trim()
                    .to_owned()
            };
            let vendor = text(desc.VendorIdOffset);
            let product = text(desc.ProductIdOffset);
            model = if vendor.is_empty() || product.starts_with(&vendor) {
                product
            } else {
                format!("{vendor} {product}")
            };
            bus_usb = desc.BusType == BusTypeUsb;
            nvme = desc.BusType == BusTypeNvme;
            removable = desc.RemovableMedia || bus_usb;
        }
    }
    let seek = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // SAFETY: plain data out-struct.
    let mut penalty: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { zeroed() };
    let ssd = handle.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, Some(&seek), &mut penalty)
        && !penalty.IncursSeekPenalty;
    let kind = match (nvme, ssd, bus_usb) {
        (true, _, _) => "SSD (NVMe)",
        (_, _, true) => "Removable",
        (_, true, _) => "SSD",
        _ => "HDD",
    };
    // The geometry query works without read access (the length query does not).
    // SAFETY: plain data out-struct.
    let mut geo: DISK_GEOMETRY_EX = unsafe { zeroed() };
    let capacity = if handle.ioctl::<(), _>(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, None, &mut geo) {
        geo.DiskSize.max(0) as u64
    } else {
        0
    };
    Disk {
        number,
        handle,
        model,
        kind: kind.into(),
        capacity,
        removable,
        letters: Vec::new(),
        last: None,
    }
}

/// Drive letters and the physical disk each one lives on (first extent).
fn drive_letters() -> Vec<(char, u32)> {
    // SAFETY: trivial call.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let Some(h) = Handle::open(&format!("\\\\.\\{letter}:")) else {
            continue;
        };
        // SAFETY: plain data out-struct (one extent is enough for the common case).
        let mut ext: VOLUME_DISK_EXTENTS = unsafe { zeroed() };
        if h.ioctl::<(), _>(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, &mut ext)
            && ext.NumberOfDiskExtents >= 1
        {
            out.push((letter, ext.Extents[0].DiskNumber));
        }
    }
    out
}

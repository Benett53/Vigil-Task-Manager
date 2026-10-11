//! Native system queries. One `NtQuerySystemInformation` call returns every process with
//! its CPU times, memory, I/O counters and threads, which is what Task Manager uses too.

use std::ffi::c_void;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
    pub fn NtQueryInformationProcess(
        handle: *mut c_void,
        class: u32,
        buf: *mut c_void,
        len: u32,
        ret: *mut u32,
    ) -> i32;
}

const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
const SYSTEM_PROCESS_INFORMATION: u32 = 5;
pub const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
pub const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: i32 = 0xC000_0023_u32 as i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *const u16,
}

/// Full `SYSTEM_PROCESS_INFORMATION` layout (the SDK header hides most fields).
#[repr(C)]
struct RawProcess {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

/// `SYSTEM_THREAD_INFORMATION`, which follows each process entry.
#[repr(C)]
struct RawThread {
    kernel_time: i64,
    user_time: i64,
    create_time: i64,
    wait_time: u32,
    start_address: usize,
    client_id: [usize; 2],
    priority: i32,
    base_priority: i32,
    context_switches: u32,
    thread_state: u32,
    wait_reason: u32,
}

const THREAD_STATE_WAITING: u32 = 5;
const WAIT_REASON_SUSPENDED: u32 = 5;

/// One process as reported by the kernel.
pub struct ProcEntry<'a> {
    pub pid: u32,
    pub ppid: u32,
    /// FILETIME of the process start (100 ns since 1601).
    pub create_time: i64,
    /// Kernel + user time in 100 ns units.
    pub cpu_time: u64,
    pub threads: u32,
    pub base_priority: i32,
    pub private_ws: u64,
    pub working_set: u64,
    pub virtual_size: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    /// Every thread is suspended (Task Manager's "Suspended" status).
    pub suspended: bool,
    pub image_name: &'a [u16],
}

/// Reusable buffer for the process list.
#[derive(Default)]
pub struct ProcessBuffer {
    buf: Vec<u64>,
}

impl ProcessBuffer {
    /// Refreshes the snapshot. Returns false if the call failed.
    pub fn refresh(&mut self) -> bool {
        if self.buf.is_empty() {
            self.buf = vec![0; 512 * 1024 / 8];
        }
        for _ in 0..8 {
            let len = (self.buf.len() * 8) as u32;
            let mut needed = 0u32;
            // SAFETY: the buffer is valid for `len` bytes and 8-byte aligned.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION,
                    self.buf.as_mut_ptr().cast(),
                    len,
                    &mut needed,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                // Leave headroom for processes started in the meantime.
                let want = (needed as usize).max(len as usize) + 64 * 1024;
                self.buf = vec![0; want.div_ceil(8)];
                continue;
            }
            return status >= 0;
        }
        false
    }

    pub fn iter(&self) -> impl Iterator<Item = ProcEntry<'_>> {
        let base = self.buf.as_ptr() as *const u8;
        let end = self.buf.len() * 8;
        let mut offset = 0usize;
        let mut done = self.buf.is_empty();
        std::iter::from_fn(move || {
            if done || offset + size_of::<RawProcess>() > end {
                return None;
            }
            // SAFETY: the kernel wrote a chain of entries into the buffer; offsets stay
            // within it and every entry is pointer aligned.
            unsafe {
                let p = &*(base.add(offset) as *const RawProcess);
                let threads_ptr = base.add(offset + size_of::<RawProcess>()) as *const RawThread;
                let n = p.number_of_threads as usize;
                let threads_fit =
                    offset + size_of::<RawProcess>() + n * size_of::<RawThread>() <= end;
                let suspended = threads_fit
                    && n > 0
                    && (0..n).all(|i| {
                        let t = &*threads_ptr.add(i);
                        t.thread_state == THREAD_STATE_WAITING
                            && t.wait_reason == WAIT_REASON_SUSPENDED
                    });
                let image_name = if p.image_name.buffer.is_null() || p.image_name.length == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(
                        p.image_name.buffer,
                        p.image_name.length as usize / 2,
                    )
                };
                let entry = ProcEntry {
                    pid: p.unique_process_id as u32,
                    ppid: p.inherited_from_unique_process_id as u32,
                    create_time: p.create_time,
                    cpu_time: (p.kernel_time + p.user_time).max(0) as u64,
                    threads: p.number_of_threads,
                    base_priority: p.base_priority,
                    private_ws: p.working_set_private_size.max(0) as u64,
                    working_set: p.working_set_size as u64,
                    virtual_size: p.virtual_size as u64,
                    read_bytes: p.read_transfer_count.max(0) as u64,
                    write_bytes: p.write_transfer_count.max(0) as u64,
                    suspended,
                    image_name,
                };
                if p.next_entry_offset == 0 {
                    done = true;
                } else {
                    offset += p.next_entry_offset as usize;
                }
                Some(entry)
            }
        })
    }
}

/// Per logical processor: (idle, kernel including idle, user) in 100 ns units.
pub fn processor_times(out: &mut Vec<[u64; 3]>) -> bool {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Raw {
        idle: i64,
        kernel: i64,
        user: i64,
        dpc: i64,
        interrupt: i64,
        interrupt_count: u32,
    }
    let mut buf = [Raw::default(); 256];
    let mut ret = 0u32;
    // SAFETY: the buffer is plain data of the given size.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            buf.as_mut_ptr().cast(),
            size_of_val(&buf) as u32,
            &mut ret,
        )
    };
    if status < 0 {
        return false;
    }
    let n = ret as usize / size_of::<Raw>();
    out.clear();
    out.extend(buf[..n.min(buf.len())].iter().map(|r| {
        [
            r.idle.max(0) as u64,
            r.kernel.max(0) as u64,
            r.user.max(0) as u64,
        ]
    }));
    true
}

/// Base priority (thread priority level of the class) to the nice scale used by the model.
pub fn base_priority_to_nice(p: i32) -> i32 {
    match p {
        ..=4 => 19,
        5..=6 => 5,
        7..=9 => 0,
        10..=12 => -5,
        13..=23 => -10,
        _ => -20,
    }
}

/// FILETIME (100 ns since 1601) to unix seconds.
pub fn filetime_to_unix(ft: i64) -> u64 {
    (ft.saturating_sub(116_444_736_000_000_000) / 10_000_000).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_kernel() {
        assert_eq!(size_of::<RawProcess>(), 0x100);
        assert_eq!(size_of::<RawThread>(), 0x50);
    }

    #[test]
    fn lists_this_process() {
        let mut b = ProcessBuffer::default();
        assert!(b.refresh());
        let me = std::process::id();
        let entry = b.iter().find(|p| p.pid == me).expect("own process listed");
        assert!(entry.threads >= 1);
        assert!(entry.working_set > 0);
        assert!(!entry.suspended);
        assert!(b.iter().count() > 5);
    }

    #[test]
    fn processor_times_are_reported() {
        let mut v = Vec::new();
        assert!(processor_times(&mut v));
        assert!(!v.is_empty());
    }
}

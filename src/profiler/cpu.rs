//! The CPU's side of a profile (`--profile cpu`): the run thread's CPU cycles, the process's CPU time, page faults and
//! peak memory. Nothing here is estimated. The CPU's hardware counters (cache misses, branch misses, instructions retired)
//! are the `hw` kind's ([`super::hw`]): this report lists the ones the CPU offers and points there.
//!
//! On Windows a user process cannot read the performance counters itself: `rdpmc` faults (the kernel leaves CR4.PCE
//! off), and ETW's PMC sources (the `TraceProfileSourceListInfo` list, which any process may read) are only counted by a
//! kernel trace session, which needs the system's profiling right (an Administrator's full token, or a service):
//! `--profile hw` reads them through the counter service (tools/hw-counters). The cycles here come from
//! QueryThreadCycleTime (the thread's share of the timestamp counter, which runs at a constant rate: reference cycles,
//! not core cycles).

/// What the CPU did over a profile: from the VM's start (the run, not the command's startup) to the profile's end
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The run thread's CPU cycles (timestamp-counter ticks while it ran), and them in milliseconds (the counter's rate)
    pub thread_cycles: Option<u64>,
    pub thread_ms: Option<f64>,
    /// The counter's ticks a millisecond (0: not known)
    pub cycles_per_ms: f64,
    /// The process's user and kernel CPU seconds
    pub process_cpu: Option<(f64, f64)>,
    pub page_faults: Option<u64>,
    /// The working set and the private bytes when the run began, and what they grew by to its end; the most working set seen
    /// at a frame's end or the end (the process's lifetime peak would hold the startup's)
    pub working_set: Option<(u64, i64)>,
    pub private_bytes: Option<(u64, i64)>,
    pub run_peak_working_set: Option<u64>,
    /// The hardware counters the CPU offers (ETW's PMC sources), and why they were not read when they were not
    pub pmc_sources: Vec<String>,
    pub pmc: Option<Vec<(String, u64)>>,
    pub pmc_missing: Option<String>,
}

/// The counters at the run's start (the VM's: install), and the most working set seen since
pub struct Counters {
    on: bool,
    started: bool,
    thread_cycles: u64,
    process: Option<(f64, f64)>,
    memory: Option<Memory>,
    peak: u64,
    cycles_per_ms: f64,
}

impl Counters {
    /// Off until [`Counters::start`] (the profile begins before the engine; the run's figures start with its VM)
    pub fn begin(on: bool) -> Counters {
        Counters { on, started: false, thread_cycles: 0, process: None, memory: None, peak: 0, cycles_per_ms: 0.0 }
    }

    /// The run begins (the VM is installed): the baseline
    pub fn start(&mut self) {
        if !self.on || self.started {
            return;
        }
        self.started = true;
        self.cycles_per_ms = cycles_per_ms();
        self.thread_cycles = thread_cycles();
        self.process = process_times();
        self.memory = memory();
        self.peak = self.memory.map_or(0, |memory| memory.working_set);
    }

    /// A frame ended: the working set now (the run's peak is the most seen); returns it
    pub fn sample(&mut self) -> Option<u64> {
        if !self.started {
            return None;
        }
        let working_set = memory()?.working_set;
        self.peak = self.peak.max(working_set);
        Some(working_set)
    }

    pub fn cycles_per_ms(&self) -> f64 {
        self.cycles_per_ms
    }

    pub fn finish(&mut self) -> Report {
        if !self.on || !self.started {
            return Report::default();
        }
        let cycles = cfg!(windows).then(|| thread_cycles().saturating_sub(self.thread_cycles));
        let mut report = Report {
            thread_cycles: cycles,
            thread_ms: cycles.filter(|_| self.cycles_per_ms > 0.0).map(|cycles| cycles as f64 / self.cycles_per_ms),
            cycles_per_ms: self.cycles_per_ms,
            ..Report::default()
        };
        if let (Some((user, kernel)), Some((user0, kernel0))) = (process_times(), self.process) {
            report.process_cpu = Some((user - user0, kernel - kernel0));
        }
        if let (Some(now), Some(then)) = (memory(), self.memory) {
            self.peak = self.peak.max(now.working_set);
            report.page_faults = Some(now.faults.saturating_sub(then.faults));
            report.working_set = Some((then.working_set, now.working_set as i64 - then.working_set as i64));
            report.private_bytes = Some((then.private, now.private as i64 - then.private as i64));
            report.run_peak_working_set = Some(self.peak);
        }
        report.pmc_sources = pmc_sources();
        report.pmc_missing = Some(pmc_missing(&report.pmc_sources));
        report
    }
}

/// The process's memory now: page faults so far, the working set, the private bytes
#[derive(Clone, Copy, Debug)]
struct Memory {
    faults: u64,
    working_set: u64,
    private: u64,
}

fn memory() -> Option<Memory> {
    #[cfg(windows)]
    unsafe {
        let mut counters = sys::MemoryCounters { cb: std::mem::size_of::<sys::MemoryCounters>() as u32, ..Default::default() };
        if sys::K32GetProcessMemoryInfo(sys::GetCurrentProcess(), &mut counters, counters.cb) != 0 {
            return Some(Memory { faults: counters.page_fault_count as u64, working_set: counters.working_set_size as u64, private: counters.private_usage as u64 });
        }
    }
    None
}

/// The timestamp counter's ticks a millisecond (QueryThreadCycleTime counts them while the thread runs): measured against the
/// wall clock over a few milliseconds by the counter itself (a thread preempted while it measured its own cycles would read a
/// lower rate); 0 where the thread's cycles cannot be read
pub fn cycles_per_ms() -> f64 {
    if thread_cycles() == 0 {
        return 0.0;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let (start, counter) = (std::time::Instant::now(), unsafe { core::arch::x86_64::_rdtsc() });
        while start.elapsed().as_secs_f64() < 0.005 {
            std::hint::spin_loop();
        }
        let (elapsed, ticks) = (start.elapsed().as_secs_f64(), unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(counter));
        if elapsed > 0.0 { ticks as f64 / (elapsed * 1e3) } else { 0.0 }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0.0
    }
}

/// Why the hardware counters were not read
fn pmc_missing(sources: &[String]) -> String {
    let offered = if sources.is_empty() { String::from("the CPU's counters are not listed") } else { format!("the CPU offers {}", sources.join(", ")) };
    if !cfg!(windows) {
        return format!("hardware counters are read on Windows only so far ({offered})");
    }
    format!(
        "hardware counters (instructions, cycles, cache and branch misses) are the hw kind's: --profile hw reads them through ree's counter service, \
         which tools/hw-counters/setup.ps1 installs once from an elevated PowerShell (docs/cli.md \"Hardware counters\"); {offered}"
    )
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    pub struct MemoryCounters {
        pub cb: u32,
        pub page_fault_count: u32,
        pub peak_working_set_size: usize,
        pub working_set_size: usize,
        pub quota_peak_paged_pool_usage: usize,
        pub quota_paged_pool_usage: usize,
        pub quota_peak_non_paged_pool_usage: usize,
        pub quota_non_paged_pool_usage: usize,
        pub pagefile_usage: usize,
        pub peak_pagefile_usage: usize,
        pub private_usage: usize,
    }

    unsafe extern "system" {
        pub fn GetCurrentThread() -> *mut c_void;
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
        // (FILETIMEs as u64, as watchdog.rs declares it)
        pub fn GetProcessTimes(process: *mut c_void, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        pub fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut MemoryCounters, size: u32) -> i32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        pub fn GetTokenInformation(token: *mut c_void, class: u32, information: *mut c_void, length: u32, returned: *mut u32) -> i32;
        pub fn TraceQueryInformation(session: u64, class: u32, information: *mut c_void, length: u32, returned: *mut u32) -> u32;
    }
}

/// The calling thread's CPU cycles so far (0 where they cannot be read)
pub fn thread_cycles() -> u64 {
    #[cfg(windows)]
    unsafe {
        let mut cycles = 0u64;
        if sys::QueryThreadCycleTime(sys::GetCurrentThread(), &mut cycles) != 0 {
            return cycles;
        }
    }
    0
}

fn process_times() -> Option<(f64, f64)> {
    #[cfg(windows)]
    unsafe {
        let (mut creation, mut exit, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
        if sys::GetProcessTimes(sys::GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) != 0 {
            return Some((user as f64 * 1e-7, kernel as f64 * 1e-7));
        }
    }
    None
}

/// Whether this process runs elevated (an Administrator's full token)
pub fn elevated() -> bool {
    #[cfg(windows)]
    unsafe {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ELEVATION: u32 = 20;
        let mut token = std::ptr::null_mut();
        if sys::OpenProcessToken(sys::GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = 0u32;
        let mut returned = 0u32;
        let ok = sys::GetTokenInformation(token, TOKEN_ELEVATION, &mut elevation as *mut u32 as *mut _, 4, &mut returned) != 0;
        sys::CloseHandle(token);
        return ok && elevation != 0;
    }
    #[allow(unreachable_code)]
    false
}

/// The hardware counters the CPU offers to ETW (any process may list them; sampling them needs elevation)
pub fn pmc_sources() -> Vec<String> {
    #[cfg(windows)]
    unsafe {
        const TRACE_PROFILE_SOURCE_LIST_INFO: u32 = 7;
        let mut buffer = vec![0u8; 1 << 16];
        let mut returned = 0u32;
        if sys::TraceQueryInformation(0, TRACE_PROFILE_SOURCE_LIST_INFO, buffer.as_mut_ptr() as *mut _, buffer.len() as u32, &mut returned) != 0 {
            return Vec::new();
        }
        // PROFILE_SOURCE_INFO: NextEntryOffset, Source, MinInterval, MaxInterval (u32 each), Reserved (u64), Description (UTF-16)
        let mut names = Vec::new();
        let mut offset = 0usize;
        while offset + 24 < returned as usize {
            let next = u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap_or_default()) as usize;
            let text: Vec<u16> = buffer[offset + 24..].chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|&unit| unit != 0).collect();
            let name = String::from_utf16_lossy(&text);
            if name != "Timer" {
                names.push(name);
            }
            if next == 0 {
                break;
            }
            offset += next;
        }
        return names;
    }
    #[allow(unreachable_code)]
    Vec::new()
}

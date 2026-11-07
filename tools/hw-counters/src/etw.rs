//! The kernel trace session: context switches, with the CPU's counters' values on each (the PMC counters ETW records on
//! the events it is told to), and thread starts; consumed in real time and turned into each of one process's threads'
//! slices (protocol.rs), the rest dropped as it arrives.
//!
//! A slice is what a thread did on one CPU from being switched in to being switched out: a switch event names the thread
//! that leaves and the one that comes, on the CPU that logged it, and carries the counters' values there and then, so a
//! thread's counts over a slice are its switch-out values less its switch-in values on that CPU. The counters count in
//! both user and kernel mode, so a slice holds the interrupts the CPU took meanwhile, as the CPU-counter tables of
//! Windows Performance Analyzer do.

use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::{Arc, Mutex, mpsc},
};

use crate::{
    ffi::{self, Owned},
    protocol::{self, MAX_COUNTERS, Slice},
};

/// The sessions' names: the service runs one at a time, and stops one of its name a crash left (`serve` in a console
/// has a name of its own, so it never stops the service's)
pub const SESSION_NAME: &str = "ree-hw-counters";
pub const CONSOLE_SESSION_NAME: &str = "ree-hw-counters-console";
const KERNEL_LOGGER_NAME: &str = "NT Kernel Logger";

/// Stops a session of the service's name that a crash left running
pub fn stop_stale(name: &str) {
    stop_by_name(Logger::System, name);
}

/// A profile source the CPU offers: its id and name (TraceProfileSourceListInfo; any process may list them)
pub fn sources() -> Result<Vec<(u32, String, u32, u32)>, String> {
    let mut buffer = vec![0u8; 1 << 16];
    let mut returned = 0u32;
    let status = unsafe { ffi::TraceQueryInformation(0, ffi::TRACE_PROFILE_SOURCE_LIST_INFO, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32, &mut returned) };
    if status != 0 {
        return Err(format!("listing the CPU's counters: error {status}"));
    }
    // PROFILE_SOURCE_INFO: NextEntryOffset, Source, MinInterval, MaxInterval (u32 each), Reserved (u64), Description
    let mut list = Vec::new();
    let mut offset = 0usize;
    let returned = (returned as usize).min(buffer.len());
    while offset + 24 <= returned {
        let read = |at: usize| u32::from_le_bytes(buffer[offset + at..offset + at + 4].try_into().unwrap());
        let (next, source, min, max) = (read(0) as usize, read(4), read(8), read(12));
        let text: Vec<u16> = buffer[offset + 24..returned].as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).take_while(|&unit| unit != 0).collect();
        list.push((source, String::from_utf16_lossy(&text), min, max));
        if next == 0 {
            break;
        }
        offset += next;
    }
    Ok(list)
}

/// How many counters the CPU counts at once (TraceMaxPmcCounterQuery)
pub fn max_counters() -> Option<usize> {
    let mut value = 0u32;
    let mut returned = 0u32;
    let status = unsafe { ffi::TraceQueryInformation(0, ffi::TRACE_MAX_PMC_COUNTER_QUERY, &mut value as *mut u32 as *mut c_void, 4, &mut returned) };
    (status == 0 && value > 0).then_some(value as usize)
}

/// What the consumer keeps: the per-CPU switch state and the process's threads' slices
pub struct Collector {
    process: u32,
    counters: usize,
    /// The process's threads (the snapshot at the start, and those that started since)
    tracked: HashSet<u32>,
    registered: HashSet<u32>,
    /// Per CPU: the thread switched in, when, and the counters' values then (None: values not known)
    cpus: Vec<Option<(u32, i64, [u64; MAX_COUNTERS])>>,
    /// Per CPU: whether a switch with values has come there (the switches before it came before the counters were set)
    counting: Vec<bool>,
    slices: HashMap<u32, Vec<Slice>>,
    switches: u64,
    /// The process's threads' switches without values once their CPU was counting: stretches missing from the slices
    without_counters: u64,
}

/// A counter's count between two of its values: they are 48 bits wide on the CPUs this runs on, and may wrap
fn delta(before: u64, after: u64) -> u64 {
    const WIDTH: u64 = 1 << 48;
    if after >= before {
        after - before
    } else if before < WIDTH && after < WIDTH {
        after + WIDTH - before
    } else {
        after.wrapping_sub(before)
    }
}

impl Collector {
    pub fn new(process: u32, counters: usize, registered: &[u32]) -> Collector {
        Collector {
            process,
            counters,
            tracked: registered.iter().copied().collect(),
            registered: registered.iter().copied().collect(),
            cpus: Vec::new(),
            counting: Vec::new(),
            slices: HashMap::new(),
            switches: 0,
            without_counters: 0,
        }
    }

    pub fn track(&mut self, thread: u32) {
        self.tracked.insert(thread);
    }

    /// A context switch on `cpu` at `at`: `old` leaves, `new` comes; `values` the counters' (None when the event carried
    /// none)
    pub fn switch(&mut self, cpu: usize, at: i64, new: u32, old: u32, values: Option<&[u64]>) {
        self.switches += 1;
        if self.cpus.len() <= cpu {
            self.cpus.resize(cpu + 1, None);
            self.counting.resize(cpu + 1, false);
        }
        let Some(values) = values.filter(|values| values.len() >= self.counters) else {
            // (the switches every CPU logs between the session's start and the counters being set carry none, and
            // matter to nobody; one of the process's threads' once its CPU counts loses that stretch)
            if self.counting[cpu] && (self.tracked.contains(&old) || self.tracked.contains(&new)) {
                self.without_counters += 1;
            }
            self.cpus[cpu] = None;
            return;
        };
        self.counting[cpu] = true;
        let mut now = [0u64; MAX_COUNTERS];
        now[..self.counters].copy_from_slice(&values[..self.counters]);
        if let Some((thread, start, before)) = self.cpus[cpu].take()
            && thread == old
            && self.tracked.contains(&old)
            && at >= start
        {
            let mut slice = Slice { cpu: cpu as u16, start, end: at, counts: [0; MAX_COUNTERS] };
            for index in 0..self.counters {
                slice.counts[index] = delta(before[index], now[index]);
            }
            self.slices.entry(old).or_default().push(slice);
        }
        self.cpus[cpu] = Some((new, at, now));
    }

    /// A thread started (or was there when the session started: a rundown's DCStart)
    pub fn thread_started(&mut self, process: u32, thread: u32) {
        if process == self.process {
            self.tracked.insert(thread);
        }
    }

    pub fn results(self) -> protocol::Results {
        let mut threads: Vec<protocol::Thread> = self
            .tracked
            .iter()
            .map(|&id| {
                let mut slices = self.slices.get(&id).cloned().unwrap_or_default();
                slices.sort_by_key(|slice| slice.end);
                protocol::Thread { id, registered: self.registered.contains(&id), slices }
            })
            .collect();
        threads.sort_by(|a, b| b.registered.cmp(&a.registered).then(a.id.cmp(&b.id)));
        // (what to look at when nothing could be counted)
        let mut notes = Vec::new();
        if self.switches == 0 {
            notes.push("the session saw no context switch".to_string());
        } else if !self.counting.iter().any(|&counting| counting) {
            notes.push(format!("none of the {} context switches carried the counters' values: the session took the counters but did not put them on its switches", self.switches));
        }
        protocol::Results { counters: self.counters as u8, threads, switches: self.switches, without_counters: self.without_counters, notes, ..protocol::Results::default() }
    }
}

/// The counters' values an event carries (its PMC extended data item)
///
/// # Safety
/// `record` is the record ETW passed the callback.
unsafe fn counter_values(record: &ffi::EventRecord) -> Option<&[u64]> {
    if record.extended_data.is_null() {
        return None;
    }
    let items = unsafe { std::slice::from_raw_parts(record.extended_data, record.extended_data_count as usize) };
    let item = items.iter().find(|item| item.ext_type == ffi::EVENT_HEADER_EXT_TYPE_PMC_COUNTERS && item.data_ptr != 0)?;
    Some(unsafe { std::slice::from_raw_parts(item.data_ptr as *const u64, item.data_size as usize / 8) })
}

/// The CPU an event was logged on
fn processor(record: &ffi::EventRecord) -> usize {
    if record.header.flags & ffi::EVENT_HEADER_FLAG_PROCESSOR_INDEX != 0 { record.processor as usize } else { (record.processor & 0xff) as usize }
}

/// ETW's callback, on the consumer's thread: switches and thread starts go to the collector the context names
unsafe extern "system" fn on_event(record: *mut ffi::EventRecord) {
    let record = unsafe { &*record };
    if record.header.provider_id != ffi::THREAD_GUID || record.user_context.is_null() {
        return;
    }
    let collector = unsafe { &*(record.user_context as *const Mutex<Collector>) };
    let data: &[u8] = if record.user_data.is_null() { &[] } else { unsafe { std::slice::from_raw_parts(record.user_data as *const u8, record.user_data_length as usize) } };
    if data.len() < 8 {
        return;
    }
    let first = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let second = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let Ok(mut collector) = collector.lock() else { return };
    match record.header.descriptor.opcode {
        // CSwitch: NewThreadId, OldThreadId, ...
        36 => {
            let values = unsafe { counter_values(record) };
            collector.switch(processor(record), record.header.timestamp, first, second, values);
        }
        // Start, DCStart: ProcessId, TThreadId, ...
        1 | 3 => collector.thread_started(first, second),
        _ => {}
    }
}

/// Which kind of kernel session counts
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Logger {
    /// A session of its own (Windows 8 and later: up to eight of them)
    System,
    /// The NT Kernel Logger (one in the system; used only when a system logger refuses the counters)
    Kernel,
}

fn properties(logger: Logger, name: &str) -> ffi::Properties {
    let mut properties = ffi::Properties { properties: ffi::EventTraceProperties::default(), name: [0; 1024] };
    let p = &mut properties.properties;
    p.wnode.buffer_size = std::mem::size_of::<ffi::Properties>() as u32;
    p.wnode.flags = ffi::WNODE_FLAG_TRACED_GUID;
    // (QueryPerformanceCounter: the client's readings are in the same clock)
    p.wnode.client_context = 1;
    p.wnode.guid = if logger == Logger::Kernel { ffi::SYSTEM_TRACE_CONTROL_GUID } else { ffi::SESSION_GUID };
    let cpus = unsafe { ffi::GetActiveProcessorCount(0xFFFF) }.clamp(1, 1024);
    // (128 KB buffers, a few per CPU: about 40 MB at most on 32 CPUs, a second of switches at a busy machine's rate)
    p.buffer_size = 128;
    p.minimum_buffers = 2 * cpus + 16;
    p.maximum_buffers = 8 * cpus + 64;
    p.flush_timer = 1;
    p.log_file_mode = ffi::EVENT_TRACE_REAL_TIME_MODE | if logger == Logger::System { ffi::EVENT_TRACE_SYSTEM_LOGGER_MODE } else { 0 };
    p.enable_flags = ffi::EVENT_TRACE_FLAG_THREAD | ffi::EVENT_TRACE_FLAG_CSWITCH;
    p.logger_name_offset = std::mem::offset_of!(ffi::Properties, name) as u32;
    for (slot, unit) in properties.name.iter_mut().zip(name.encode_utf16()) {
        *slot = unit;
    }
    properties
}

/// Stops a session by name (one a crash of this service left)
fn stop_by_name(logger: Logger, name: &str) -> u32 {
    let mut stop = properties(logger, name);
    let wide = ffi::wide(name);
    unsafe { ffi::ControlTraceW(0, wide.as_ptr(), &mut stop.properties, ffi::EVENT_TRACE_CONTROL_STOP) }
}

/// What a Win32 error means here
fn explain(status: u32) -> String {
    match status {
        ffi::ERROR_ACCESS_DENIED => "access denied (error 5): the service must run as LocalSystem with the system's profiling right".into(),
        ffi::ERROR_PRIVILEGE_NOT_HELD => "error 1314, a privilege is not held: the service must run as LocalSystem with the system's profiling right".into(),
        ffi::ERROR_BUSY => "error 170, the CPU's counters are in use by another trace session (`wpr -pmcsessions` lists it): try again when it ends".into(),
        ffi::ERROR_ALREADY_EXISTS => "error 183, a session of that name is running".into(),
        status => format!("error {status}"),
    }
}

/// A running session
pub struct Session {
    handle: u64,
    logger: Logger,
    name: String,
    collector: Arc<Mutex<Collector>>,
    consumer: Option<std::thread::JoinHandle<u32>>,
    trace: Arc<Mutex<u64>>,
    /// What the session had lost when counting started
    lost_before: (u32, u32),
    stopped: bool,
    /// The registered threads' handles, held so their ids are not reused while counting
    _threads: Vec<Owned>,
}

impl Session {
    /// Starts counting `sources` (profile source ids) for `process`'s threads; `registered` are the threads the client
    /// named (checked to be its own)
    pub fn start(name: &str, sources: &[u32], process: u32, registered: &[u32], threads: Vec<Owned>, notes: &mut Vec<String>) -> Result<Session, String> {
        let mut session = match Session::start_with(Logger::System, name, sources, process, registered) {
            Ok(session) => session,
            Err((ffi::ERROR_INVALID_PARAMETER | ffi::ERROR_NOT_SUPPORTED | ffi::ERROR_INVALID_FUNCTION, message)) => {
                // (a system logger that refuses the counters: the NT Kernel Logger takes them)
                notes.push(format!("a session of its own refused the counters ({message}): counting in the NT Kernel Logger"));
                Session::start_with(Logger::Kernel, KERNEL_LOGGER_NAME, sources, process, registered).map_err(|(_, message)| message)?
            }
            Err((_, message)) => return Err(message),
        };
        session._threads = threads;
        Ok(session)
    }

    fn start_with(logger: Logger, name: &str, sources: &[u32], process: u32, registered: &[u32]) -> Result<Session, (u32, String)> {
        let wide = ffi::wide(name);
        let mut handle = 0u64;
        let mut start = properties(logger, name);
        let mut status = unsafe { ffi::StartTraceW(&mut handle, wide.as_ptr(), &mut start.properties) };
        if status == ffi::ERROR_ALREADY_EXISTS {
            if logger == Logger::Kernel {
                return Err((status, "the NT Kernel Logger is running for another tool (`logman query -ets` lists it): try again when it ends".into()));
            }
            stop_by_name(logger, name);
            start = properties(logger, name);
            status = unsafe { ffi::StartTraceW(&mut handle, wide.as_ptr(), &mut start.properties) };
        }
        if status != 0 {
            return Err((status, format!("starting the trace session: {}", explain(status))));
        }
        let collector = Arc::new(Mutex::new(Collector::new(process, sources.len(), registered)));
        let mut session = Session { handle, logger, name: name.into(), collector: collector.clone(), consumer: None, trace: Arc::new(Mutex::new(ffi::INVALID_PROCESSTRACE_HANDLE)), lost_before: (0, 0), stopped: false, _threads: Vec::new() };
        // the consumer first, so the session's buffers drain from the start
        let (opened_tx, opened_rx) = mpsc::channel::<Result<(), u32>>();
        let (trace, consumer_name) = (session.trace.clone(), name.to_string());
        let context = Arc::as_ptr(&collector) as usize;
        let consumer = std::thread::Builder::new()
            .name("ree-hw-counters-consumer".into())
            .spawn(move || {
                let mut name = ffi::wide(&consumer_name);
                let mut logfile: ffi::EventTraceLogfile = unsafe { std::mem::zeroed() };
                logfile.logger_name = name.as_mut_ptr();
                logfile.process_trace_mode = ffi::PROCESS_TRACE_MODE_REAL_TIME | ffi::PROCESS_TRACE_MODE_EVENT_RECORD | ffi::PROCESS_TRACE_MODE_RAW_TIMESTAMP;
                logfile.event_record_callback = Some(on_event);
                logfile.context = context as *mut c_void;
                let handle = unsafe { ffi::OpenTraceW(&mut logfile) };
                if handle == ffi::INVALID_PROCESSTRACE_HANDLE {
                    let _ = opened_tx.send(Err(unsafe { ffi::GetLastError() }));
                    return 0;
                }
                *trace.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = handle;
                let _ = opened_tx.send(Ok(()));
                let status = unsafe { ffi::ProcessTrace(&handle, 1, std::ptr::null_mut(), std::ptr::null_mut()) };
                let mut trace = trace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if *trace != ffi::INVALID_PROCESSTRACE_HANDLE {
                    unsafe { ffi::CloseTrace(*trace) };
                    *trace = ffi::INVALID_PROCESSTRACE_HANDLE;
                }
                status
            })
            .map_err(|error| (0, format!("the consumer's thread: {error}")))?;
        session.consumer = Some(consumer);
        match opened_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(status)) => return Err((status, format!("reading the trace session: {}", explain(status)))),
            Err(_) => return Err((0, "reading the trace session: no answer in 10 s".into())),
        }
        // the counters, and the event they ride on
        let status = unsafe { ffi::TraceSetInformation(handle, ffi::TRACE_PMC_COUNTER_LIST_INFO, sources.as_ptr() as *const c_void, (sources.len() * 4) as u32) };
        if status != 0 {
            return Err((status, format!("setting the counters: {}", explain(status))));
        }
        let cswitch = ffi::ClassicEventId { guid: ffi::THREAD_GUID, event_type: 36, reserved: [0; 7] };
        let status = unsafe { ffi::TraceSetInformation(handle, ffi::TRACE_PMC_EVENT_LIST_INFO, &cswitch as *const ffi::ClassicEventId as *const c_void, std::mem::size_of::<ffi::ClassicEventId>() as u32) };
        if status != 0 {
            return Err((status, format!("putting the counters on context switches: {}", explain(status))));
        }
        // the process's threads now (those that start later come as events)
        for thread in process_threads(process) {
            collector.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).track(thread);
        }
        session.lost_before = session.lost();
        Ok(session)
    }

    /// What the session has lost so far (events, real-time buffers)
    fn lost(&self) -> (u32, u32) {
        let mut query = properties(self.logger, &self.name);
        let status = unsafe { ffi::ControlTraceW(self.handle, std::ptr::null(), &mut query.properties, ffi::EVENT_TRACE_CONTROL_QUERY) };
        if status == 0 { (query.properties.events_lost, query.properties.real_time_buffers_lost) } else { (0, 0) }
    }

    fn stop_session(&mut self) -> (u32, u32) {
        if self.stopped {
            return (0, 0);
        }
        self.stopped = true;
        let mut stop = properties(self.logger, &self.name);
        let status = unsafe { ffi::ControlTraceW(self.handle, std::ptr::null(), &mut stop.properties, ffi::EVENT_TRACE_CONTROL_STOP) };
        if status != 0 {
            return (0, 0);
        }
        (stop.properties.events_lost, stop.properties.real_time_buffers_lost)
    }

    /// Stops counting: the process's threads' slices
    pub fn stop(mut self) -> Result<protocol::Results, String> {
        let lost = self.stop_session();
        // the consumer delivers what the buffers still held, then returns; one that does not is closed
        if let Some(consumer) = self.consumer.take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while !consumer.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if !consumer.is_finished() {
                let mut trace = self.trace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if *trace != ffi::INVALID_PROCESSTRACE_HANDLE {
                    unsafe { ffi::CloseTrace(*trace) };
                    *trace = ffi::INVALID_PROCESSTRACE_HANDLE;
                }
            }
            let _ = consumer.join();
        }
        let collector = std::mem::replace(&mut *self.collector.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), Collector::new(0, 0, &[]));
        let mut results = collector.results();
        results.lost_events = lost.0.saturating_sub(self.lost_before.0);
        results.lost_buffers = lost.1.saturating_sub(self.lost_before.1);
        if self.logger == Logger::Kernel {
            results.notes.push("counted in the NT Kernel Logger".into());
        }
        Ok(results)
    }
}

impl Drop for Session {
    /// A session is never left running: a client that went away mid-count stops it
    fn drop(&mut self) {
        self.stop_session();
        if let Some(consumer) = self.consumer.take() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while !consumer.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if consumer.is_finished() {
                let _ = consumer.join();
            }
        }
    }
}

/// A process's threads now (a Toolhelp snapshot)
pub fn process_threads(process: u32) -> Vec<u32> {
    const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
    let mut threads = Vec::new();
    unsafe {
        let snapshot = Owned(ffi::CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0));
        if snapshot.0 == ffi::INVALID_HANDLE {
            return threads;
        }
        let mut entry = ffi::ThreadEntry32 { size: std::mem::size_of::<ffi::ThreadEntry32>() as u32, usage: 0, thread_id: 0, owner_process_id: 0, base_priority: 0, delta_priority: 0, flags: 0 };
        let mut more = ffi::Thread32First(snapshot.0, &mut entry) != 0;
        while more {
            if entry.owner_process_id == process {
                threads.push(entry.thread_id);
            }
            more = ffi::Thread32Next(snapshot.0, &mut entry) != 0;
        }
    }
    threads
}

/// Whether a thread id is one of `process`'s threads: its handle, held while counting
pub fn open_thread_of(process: u32, thread: u32) -> Option<Owned> {
    const THREAD_QUERY_LIMITED_INFORMATION: u32 = 0x0800;
    let handle = Owned(unsafe { ffi::OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, thread) });
    (!handle.0.is_null() && unsafe { ffi::GetProcessIdOfThread(handle.0) } == process).then_some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(counts: &[u64]) -> Vec<u64> {
        counts.to_vec()
    }

    /// Switches become slices: a tracked thread's from its switch-in to its switch-out on the same CPU, counts the
    /// difference of the values; untracked threads and events without values make none; a thread that starts while
    /// counting is tracked; a counter that wrapped at 48 bits still counts forward
    #[test]
    fn switches_make_slices_of_the_tracked_threads() {
        let mut collector = Collector::new(100, 2, &[7]);
        // (before the counters were set: CPU 2's switches carry no values, and count as missing nothing)
        collector.switch(2, 5, 7, 0, None);
        collector.switch(2, 6, 0, 7, None);
        collector.switch(0, 10, 7, 0, Some(&values(&[1000, 50])));
        collector.switch(1, 12, 9, 0, Some(&values(&[5, 5])));
        collector.switch(0, 20, 0, 7, Some(&values(&[1600, 80])));
        // (another process's thread on CPU 1: no slice)
        collector.switch(1, 25, 0, 9, Some(&values(&[50, 50])));
        // a new thread of the process starts, runs on CPU 1
        collector.thread_started(100, 11);
        collector.thread_started(200, 12);
        collector.switch(1, 30, 11, 0, Some(&values(&[(1 << 48) - 10, 0])));
        collector.switch(1, 40, 0, 11, Some(&values(&[30, 4])));
        // (another process's switch without values, on a counting CPU: none of ours missing)
        collector.switch(1, 45, 9, 0, None);
        // an event of ours without values breaks the CPU's chain: the next switch-out makes no slice, and it is counted
        collector.switch(0, 50, 7, 0, None);
        collector.switch(0, 60, 0, 7, Some(&values(&[2000, 90])));
        let results = collector.results();
        assert_eq!((results.switches, results.without_counters), (11, 1));
        assert!(results.notes.is_empty(), "{:?}", results.notes);
        let thread = |id: u32| results.threads.iter().find(|thread| thread.id == id).unwrap();
        let seven = thread(7);
        assert!(seven.registered);
        assert_eq!(seven.slices.len(), 1);
        assert_eq!((seven.slices[0].cpu, seven.slices[0].start, seven.slices[0].end, seven.slices[0].counts[0], seven.slices[0].counts[1]), (0, 10, 20, 600, 30));
        let eleven = thread(11);
        assert!(!eleven.registered);
        assert_eq!(eleven.slices[0].counts[0], 40, "wrapped at 48 bits");
        assert!(results.threads.iter().all(|thread| thread.id != 9 && thread.id != 12), "not the process's");
        assert_eq!(results.threads[0].id, 7, "the registered thread first");
        // a session whose switches never carried values says so
        let mut bare = Collector::new(100, 2, &[7]);
        bare.switch(0, 1, 7, 0, None);
        bare.switch(0, 2, 0, 7, None);
        assert!(bare.results().notes.iter().any(|note| note.starts_with("none of the 2 context switches")));
    }

    /// What the consumer saw of an event
    #[derive(Debug)]
    struct Seen {
        provider: ffi::Guid,
        id: u16,
        timestamp: i64,
        thread: u32,
        cpu: usize,
        payload: Vec<u8>,
        extended: Vec<(u16, u16)>,
    }

    unsafe extern "system" fn keep(record: *mut ffi::EventRecord) {
        let record = unsafe { &*record };
        let seen = unsafe { &*(record.user_context as *const Mutex<Vec<Seen>>) };
        let payload = if record.user_data.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(record.user_data as *const u8, record.user_data_length as usize) }.to_vec() };
        let extended = if record.extended_data.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(record.extended_data, record.extended_data_count as usize) }.iter().map(|item| (item.ext_type, item.data_size)).collect() };
        seen.lock().unwrap().push(Seen { provider: record.header.provider_id, id: record.header.descriptor.id, timestamp: record.header.timestamp, thread: record.header.thread_id, cpu: processor(record), payload, extended });
    }

    /// The consumer's layouts against a real session: an ordinary real-time session (one a member of Performance Log
    /// Users may run, as kernel sessions it may not) with a provider of the test's own, its events written on this
    /// thread with a known payload and their user's SID asked for as extended data, read back through OpenTraceW and
    /// ProcessTrace in the clock the client's readings use. Skipped where the account may not run a session.
    #[test]
    fn a_real_time_session_reads_back_through_the_consumers_layouts() {
        let stamp = crate::client::qpc() as u64;
        let name = format!("ree-hw-counters-test-{}-{stamp:x}", std::process::id());
        let session_guid = ffi::Guid { data1: stamp as u32, data2: (stamp >> 32) as u16, data3: std::process::id() as u16, data4: [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x17, 0x28] };
        let provider = ffi::Guid { data1: (stamp >> 7) as u32 ^ 0x5a5a_5a5a, ..session_guid };
        let mut start = properties(Logger::System, &name);
        start.properties.wnode.guid = session_guid;
        start.properties.log_file_mode = ffi::EVENT_TRACE_REAL_TIME_MODE;
        start.properties.enable_flags = 0;
        start.properties.minimum_buffers = 4;
        start.properties.maximum_buffers = 16;
        let wide = ffi::wide(&name);
        let mut handle = 0u64;
        let status = unsafe { ffi::StartTraceW(&mut handle, wide.as_ptr(), &mut start.properties) };
        if status != 0 {
            eprintln!("skipped: this account may not start a trace session (error {status})");
            return;
        }
        let stop = |handle: u64| {
            let mut stop = properties(Logger::System, &name);
            unsafe { ffi::ControlTraceW(handle, std::ptr::null(), &mut stop.properties, ffi::EVENT_TRACE_CONTROL_STOP) }
        };
        let mut registration = 0u64;
        assert_eq!(unsafe { ffi::provider::EventRegister(&provider, std::ptr::null_mut(), std::ptr::null_mut(), &mut registration) }, 0);
        let parameters = ffi::provider::EnableTraceParameters { version: ffi::provider::ENABLE_TRACE_PARAMETERS_VERSION_2, enable_property: ffi::provider::EVENT_ENABLE_PROPERTY_SID, control_flags: 0, source_id: ffi::Guid::default(), enable_filter_desc: std::ptr::null_mut(), filter_desc_count: 0 };
        let enabled = unsafe { ffi::provider::EnableTraceEx2(handle, &provider, ffi::provider::EVENT_CONTROL_CODE_ENABLE_PROVIDER, 5, u64::MAX, 0, 5000, &parameters) };
        if enabled != 0 {
            unsafe { ffi::provider::EventUnregister(registration) };
            stop(handle);
            panic!("enabling the test's provider: error {enabled}");
        }
        // the consumer, as the service's
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let (context, consumer_name) = (Arc::as_ptr(&seen) as usize, name.clone());
        let consumer = std::thread::spawn(move || {
            let mut name = ffi::wide(&consumer_name);
            let mut logfile: ffi::EventTraceLogfile = unsafe { std::mem::zeroed() };
            logfile.logger_name = name.as_mut_ptr();
            logfile.process_trace_mode = ffi::PROCESS_TRACE_MODE_REAL_TIME | ffi::PROCESS_TRACE_MODE_EVENT_RECORD | ffi::PROCESS_TRACE_MODE_RAW_TIMESTAMP;
            logfile.event_record_callback = Some(keep);
            logfile.context = context as *mut c_void;
            let trace = unsafe { ffi::OpenTraceW(&mut logfile) };
            assert_ne!(trace, ffi::INVALID_PROCESSTRACE_HANDLE, "OpenTraceW: error {}", unsafe { ffi::GetLastError() });
            let status = unsafe { ffi::ProcessTrace(&trace, 1, std::ptr::null_mut(), std::ptr::null_mut()) };
            unsafe { ffi::CloseTrace(trace) };
            status
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        let descriptor = ffi::EventDescriptor { id: 7, version: 0, channel: 0, level: 4, opcode: 0, task: 0, keyword: 1 };
        let before = crate::client::qpc();
        for value in 0u64..100 {
            let data = ffi::provider::EventDataDescriptor { ptr: &value as *const u64 as u64, size: 8, reserved: 0 };
            assert_eq!(unsafe { ffi::provider::EventWrite(registration, &descriptor, 1, &data) }, 0);
        }
        let after = crate::client::qpc();
        let thread = crate::client::current_thread_id();
        unsafe { ffi::provider::EventUnregister(registration) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(stop(handle), 0);
        consumer.join().unwrap();
        let seen = seen.lock().unwrap();
        let ours: Vec<&Seen> = seen.iter().filter(|event| event.provider == provider && event.id == 7).collect();
        assert_eq!(ours.len(), 100, "every event read back: {:?}", seen.iter().take(3).collect::<Vec<_>>());
        let cpus = unsafe { ffi::GetActiveProcessorCount(0xFFFF) } as usize;
        for (value, event) in ours.iter().enumerate() {
            assert_eq!(event.payload, (value as u64).to_le_bytes(), "the payload, in order");
            assert_eq!(event.thread, thread, "the writing thread");
            assert!(event.timestamp >= before && event.timestamp <= after, "the raw timestamp is the client's clock: {} not in {before}..{after}", event.timestamp);
            assert!(event.cpu < cpus, "the CPU index {}", event.cpu);
            assert!(event.extended.iter().any(|&(ext_type, size)| ext_type == 2 && size >= 12), "the SID's extended item: {:?}", event.extended);
        }
    }

    /// The list of the CPU's counters needs no right at all, so a test reads it: the cycles and instructions are there
    /// on a PC with counters (a virtual machine may offer none)
    #[test]
    fn the_cpus_counters_are_listed() {
        let list = sources().unwrap();
        if list.len() > 1 {
            assert!(list.iter().any(|(_, name, _, _)| name == "TotalCycles"), "{list:?}");
        }
    }
}

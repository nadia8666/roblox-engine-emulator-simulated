//! The service's pipe: up to eight clients at a time, from the one account it was installed for (the pipe's ACL grants
//! only that account and SYSTEM, and the client process's token is checked again). A client asks to count some of the
//! CPU's counters and gets back only its own process's threads' slices. One trace session at a time holds the CPU's
//! counters, so the clients share one ([`Counting`]): the first starts it with its counters, later ones count those or
//! some of them, and the last one out stops it. A client's counting ends when it says so, goes away, or has counted for
//! 30 minutes.

use std::{
    collections::HashSet,
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    etw,
    ffi::{self, Owned, Sid},
    protocol::{self, Reply, Request},
    register,
};

/// How many clients count at once (each holds one of the pipe's instances), how long a client may count, and how long
/// it may take to ask
const MAX_CLIENTS: u32 = 8;
const MAX_SESSION: Duration = Duration::from_secs(30 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the listener looks whether the service has idled out, or a pipe instance came free
const LISTEN_TICK: Duration = Duration::from_secs(1);
const ERROR_PIPE_BUSY: u32 = 231;

/// The stop event, waited on by every client's thread
#[derive(Clone, Copy)]
struct Stop(ffi::Handle);

// (an event handle is the process's: any thread may wait on it)
unsafe impl Send for Stop {}
unsafe impl Sync for Stop {}

/// Serves clients on `pipe_name`, each on a thread of its own, until `stop` is set or nobody has counted for `idle`;
/// `session` names the trace session
pub fn run(allowed: &Sid, pipe_name: &str, session: &str, stop: ffi::Handle, idle: Option<Duration>, log: &(dyn Fn(&str) + Sync)) -> Result<(), String> {
    let counting = Counting::new(session);
    let server = Sid::of_process(unsafe { ffi::GetCurrentProcess() })?;
    let stop = Stop(stop);
    let active = AtomicUsize::new(0);
    let connected = Mutex::new(HashSet::new());
    let last_client = Mutex::new(Instant::now());
    let idled_out = || idle.is_some_and(|idle| active.load(Ordering::SeqCst) == 0 && last_client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).elapsed() >= idle);
    // (a scope: the service stops only once every client's thread has ended, and with it the trace session)
    std::thread::scope(|scope| -> Result<(), String> {
        let mut first = true;
        'listen: loop {
            let pipe = match create_pipe(pipe_name, allowed, &server, first) {
                Ok(Some(pipe)) => pipe,
                // (the first instance claims the name: without it the service cannot serve)
                Err(error) if first => return Err(error),
                // (every instance holds a client, and one comes free when it ends; or this one failed, and clients
                // counting go on while the next tick tries again)
                other => {
                    if let Err(error) = other {
                        log(&format!("a pipe instance: {error}"));
                    }
                    if signaled(stop.0, LISTEN_TICK) {
                        break;
                    }
                    continue;
                }
            };
            first = false;
            loop {
                match wait_for_client(&pipe, stop.0, Some(LISTEN_TICK)) {
                    Ok(true) => break,
                    Ok(false) if signaled(stop.0, Duration::ZERO) || idled_out() => break 'listen,
                    Ok(false) => {}
                    Err(error) => {
                        // (a client that came and went before it was seen, say: this instance closes and the next
                        // tick listens on a new one, while clients counting go on)
                        log(&error);
                        if signaled(stop.0, LISTEN_TICK) {
                            break 'listen;
                        }
                        continue 'listen;
                    }
                }
            }
            active.fetch_add(1, Ordering::SeqCst);
            let (counting, active, last_client, connected) = (&counting, &active, &last_client, &connected);
            scope.spawn(move || {
                // (the wrapper whole: a closure would otherwise take just its raw handle, which may not move threads)
                let stop = stop;
                match serve(&pipe, allowed, counting, connected, stop.0) {
                    Ok(summary) => log(&summary),
                    Err(error) => log(&format!("a client: {error}")),
                }
                unsafe {
                    ffi::FlushFileBuffers(pipe.0);
                    ffi::DisconnectNamedPipe(pipe.0);
                }
                *last_client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    })
}

/// Whether `event` is set, waiting at most `timeout`
fn signaled(event: ffi::Handle, timeout: Duration) -> bool {
    let handles = [event];
    unsafe { ffi::WaitForMultipleObjects(1, handles.as_ptr(), 0, timeout.as_millis().min(u32::MAX as u128 - 1) as u32) == ffi::WAIT_OBJECT_0 }
}

/// An instance of the pipe, for `allowed` (and SYSTEM) only and local clients only; None when every instance holds a
/// client. The first one claims the name (a process that took it first makes this fail rather than serve beside it).
/// `server`: the account this runs as, which creates the further instances.
fn create_pipe(name: &str, allowed: &Sid, server: &Sid, first: bool) -> Result<Option<Owned>, String> {
    // FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE for the account: what a client
    // needs, and not FILE_CREATE_PIPE_INSTANCE. Creating another instance takes that right and generic read and write
    // on the pipe, which the service has as SYSTEM; `serve` in a console runs as an account (the clients', usually),
    // which gets them too.
    let server = server.text();
    let instances = if server == "S-1-5-18" { String::new() } else { format!("(A;;GA;;;{server})") };
    let sddl = ffi::wide(&format!("D:P(A;;GA;;;SY)(A;;0x120083;;;{}){instances}", allowed.text()));
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    if unsafe { ffi::ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) } == 0 {
        return Err(format!("the pipe's security: error {}", unsafe { ffi::GetLastError() }));
    }
    let mut attributes = ffi::SecurityAttributes { length: std::mem::size_of::<ffi::SecurityAttributes>() as u32, descriptor, inherit: 0 };
    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
    let wide = ffi::wide(name);
    let mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
    let pipe = unsafe { ffi::CreateNamedPipeW(wide.as_ptr(), mode, PIPE_REJECT_REMOTE_CLIENTS, MAX_CLIENTS, 1 << 16, 1 << 16, 0, &mut attributes) };
    let error = unsafe { ffi::GetLastError() };
    unsafe { ffi::LocalFree(descriptor) };
    if pipe == ffi::INVALID_HANDLE {
        if !first && error == ERROR_PIPE_BUSY {
            return Ok(None);
        }
        return Err(format!("creating the pipe {name}: error {error} (another process holds the name?)"));
    }
    Ok(Some(Owned(pipe)))
}

/// An event for one overlapped operation
fn event() -> Result<Owned, String> {
    let event = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    if event.is_null() { Err(format!("an event: error {}", unsafe { ffi::GetLastError() })) } else { Ok(Owned(event)) }
}

/// Waits for an overlapped operation on `pipe`, or `stop`, or the timeout: the bytes moved, or None when it was given up
fn finish(pipe: &Owned, overlapped: &mut ffi::Overlapped, started: i32, stop: ffi::Handle, timeout: Option<Duration>) -> Result<Option<u32>, String> {
    let mut moved = 0u32;
    if started != 0 {
        unsafe { ffi::GetOverlappedResult(pipe.0, overlapped, &mut moved, 0) };
        return Ok(Some(moved));
    }
    match unsafe { ffi::GetLastError() } {
        ffi::ERROR_IO_PENDING => {}
        ffi::ERROR_PIPE_CONNECTED => return Ok(Some(0)),
        error => return Err(format!("error {error}")),
    }
    let handles = [overlapped.event, stop];
    let milliseconds = timeout.map_or(ffi::INFINITE, |timeout| timeout.as_millis().min(u32::MAX as u128 - 1) as u32);
    let waited = unsafe { ffi::WaitForMultipleObjects(2, handles.as_ptr(), 0, milliseconds) };
    if waited == ffi::WAIT_OBJECT_0 {
        if unsafe { ffi::GetOverlappedResult(pipe.0, overlapped, &mut moved, 0) } == 0 {
            return Err(format!("error {}", unsafe { ffi::GetLastError() }));
        }
        return Ok(Some(moved));
    }
    // (stopped or timed out: the operation is cancelled and waited for, so its buffer is not written later)
    unsafe {
        ffi::CancelIoEx(pipe.0, overlapped);
        ffi::GetOverlappedResult(pipe.0, overlapped, &mut moved, 1);
    }
    Ok(None)
}

/// Whether a client connected (false: stopped, or none came in `timeout`; the instance listens on, and a client that
/// came meanwhile is found by the next wait)
fn wait_for_client(pipe: &Owned, stop: ffi::Handle, timeout: Option<Duration>) -> Result<bool, String> {
    let event = event()?;
    let mut overlapped = ffi::Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: event.0 };
    let started = unsafe { ffi::ConnectNamedPipe(pipe.0, &mut overlapped) };
    Ok(finish(pipe, &mut overlapped, started, stop, timeout).map_err(|error| format!("waiting for a client: {error}"))?.is_some())
}

/// Reads from the pipe with a deadline (std::io::Read for protocol::read_message)
struct Reader<'a> {
    pipe: &'a Owned,
    stop: ffi::Handle,
    deadline: Instant,
}

impl std::io::Read for Reader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let event = event().map_err(std::io::Error::other)?;
        let mut overlapped = ffi::Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: event.0 };
        let wanted = buffer.len().min(1 << 16) as u32;
        let mut read = 0u32;
        let started = unsafe { ffi::ReadFile(self.pipe.0, buffer.as_mut_ptr(), wanted, &mut read, &mut overlapped as *mut ffi::Overlapped as *mut c_void) };
        let left = self.deadline.saturating_duration_since(Instant::now());
        match finish(self.pipe, &mut overlapped, started, self.stop, Some(left)) {
            Ok(Some(0)) => Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the client went away")),
            Ok(Some(read)) => Ok(read as usize),
            Ok(None) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no request in time")),
            Err(error) => Err(std::io::Error::other(error)),
        }
    }
}

fn write(pipe: &Owned, stop: ffi::Handle, bytes: &[u8]) -> Result<(), String> {
    let mut sent = 0usize;
    while sent < bytes.len() {
        let event = event()?;
        let mut overlapped = ffi::Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: event.0 };
        let chunk = (bytes.len() - sent).min(1 << 20) as u32;
        let mut written = 0u32;
        let started = unsafe { ffi::WriteFile(pipe.0, bytes[sent..].as_ptr(), chunk, &mut written, &mut overlapped as *mut ffi::Overlapped as *mut c_void) };
        match finish(pipe, &mut overlapped, started, stop, Some(Duration::from_secs(60)))? {
            Some(written) if written > 0 => sent += written as usize,
            _ => return Err("the client stopped reading".into()),
        }
    }
    Ok(())
}

fn reply(pipe: &Owned, stop: ffi::Handle, reply: &Reply) -> Result<(), String> {
    write(pipe, stop, &reply.encode())
}

/// The client's process, checked to run as the allowed account
fn client_process(pipe: &Owned, allowed: &Sid) -> Result<u32, String> {
    let mut process = 0u32;
    if unsafe { ffi::GetNamedPipeClientProcessId(pipe.0, &mut process) } == 0 {
        return Err(format!("the client's process: error {}", unsafe { ffi::GetLastError() }));
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    let handle = Owned(unsafe { ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process) });
    if handle.0.is_null() {
        return Err(format!("opening the client's process {process}: error {}", unsafe { ffi::GetLastError() }));
    }
    let user = Sid::of_process(handle.0)?;
    if !user.equals(allowed) {
        return Err(format!("process {process} runs as {}, not the account the service serves", user.text()));
    }
    Ok(process)
}

/// The processes connected, one connection each: a process that opened every pipe instance would keep every other
/// client out
struct Connected<'a> {
    processes: &'a Mutex<HashSet<u32>>,
    process: u32,
}

impl Connected<'_> {
    /// `process`'s connection, unless it has one already (the guard made only for a new one, after the lock is let go:
    /// one made and dropped for a refused claim would take the first connection's place away)
    fn claim(processes: &Mutex<HashSet<u32>>, process: u32) -> Option<Connected<'_>> {
        let claimed = processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(process);
        claimed.then(|| Connected { processes, process })
    }
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.processes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&self.process);
    }
}

/// One client: its request, its counting, its results; a summary line for the log
fn serve(pipe: &Owned, allowed: &Sid, counting: &Counting, connected: &Mutex<HashSet<u32>>, stop: ffi::Handle) -> Result<String, String> {
    let process = match client_process(pipe, allowed) {
        Ok(process) => process,
        Err(error) => {
            let _ = reply(pipe, stop, &Reply::Error(format!("the counter service refused the client: {error}")));
            return Err(error);
        }
    };
    let Some(_connection) = Connected::claim(connected, process) else {
        let error = format!("process {process} already has a connection to the counter service");
        let _ = reply(pipe, stop, &Reply::Error(error.clone()));
        return Err(error);
    };
    let mut reader = Reader { pipe, stop, deadline: Instant::now() + REQUEST_TIMEOUT };
    let (kind, body) = protocol::read_message(&mut reader, protocol::MAX_REQUEST_BYTES)?;
    let (version, counters, threads) = match Request::decode(kind, &body)? {
        Request::Start { version, counters, threads } => (version, counters, threads),
        Request::Stop => return Err("a stop before any start".into()),
    };
    let (member, notes) = match counting.join(process, version, &counters, &threads) {
        Ok(joined) => joined,
        Err(error) => {
            reply(pipe, stop, &Reply::Error(error.clone()))?;
            return Err(error);
        }
    };
    // (from here the client counts until it leaves, however this ends)
    let mut membership = Membership { counting, member, left: false };
    let frequency = {
        let mut frequency = 0i64;
        unsafe { ffi::QueryPerformanceFrequency(&mut frequency) };
        frequency as u64
    };
    reply(pipe, stop, &Reply::Started { version: protocol::VERSION, sources: counters.clone(), qpc_frequency: frequency, notes })?;
    // the client counts, then asks to stop (or goes away, and its counting stops with it)
    let mut reader = Reader { pipe, stop, deadline: Instant::now() + MAX_SESSION };
    let request = protocol::read_message(&mut reader, protocol::MAX_REQUEST_BYTES).and_then(|(kind, body)| Request::decode(kind, &body));
    let results = membership.leave();
    match (request, results) {
        (Ok(Request::Stop), Ok(results)) => {
            let line = format!("counted {} for process {process}: {} threads, {} switches", counters.join(", "), results.threads.len(), results.switches);
            reply(pipe, stop, &Reply::Results(results))?;
            Ok(line)
        }
        (Ok(Request::Stop), Err(error)) => {
            reply(pipe, stop, &Reply::Error(error.clone()))?;
            Err(error)
        }
        (Ok(Request::Start { .. }), _) => {
            let _ = reply(pipe, stop, &Reply::Error("a second start while counting".into()));
            Err("a second start".into())
        }
        (Err(error), _) => {
            let _ = reply(pipe, stop, &Reply::Error(format!("the counting stopped: {error}")));
            Err(error)
        }
    }
}

/// The trace session the clients share, while any counts
struct Shared {
    session: etw::Session,
    /// The profile sources it counts, in its order
    sources: Vec<String>,
    /// The processes counting in it
    processes: Vec<u32>,
}

/// The service's counting: one trace session at a time, joined and left by its clients
struct Counting {
    name: String,
    shared: Mutex<Option<Shared>>,
}

/// One client's place in the shared session
struct Member {
    process: u32,
    /// For each counter the client asked for, its index among the session's
    order: Vec<usize>,
    /// What the session had lost when the client joined
    lost_at_join: (u32, u32),
    /// The threads the client named, their handles held so their ids are not reused while it counts
    _threads: Vec<Owned>,
}

/// A member that leaves once, when asked or when dropped (a client that went away, an error, a panic)
struct Membership<'a> {
    counting: &'a Counting,
    member: Member,
    left: bool,
}

impl Membership<'_> {
    fn leave(&mut self) -> Result<protocol::Results, String> {
        self.left = true;
        self.counting.leave(&self.member)
    }
}

impl Drop for Membership<'_> {
    fn drop(&mut self) {
        if !self.left {
            let _ = self.counting.leave(&self.member);
        }
    }
}

impl Counting {
    fn new(name: &str) -> Counting {
        Counting { name: name.into(), shared: Mutex::new(None) }
    }

    /// Checks a client's request and counts its process: in a session started for it when none runs, else in the
    /// running one, when that counts every counter it asks for; with notes for its reply
    fn join(&self, process: u32, version: u32, counters: &[String], threads: &[u32]) -> Result<(Member, Vec<String>), String> {
        if version != protocol::VERSION {
            return Err(format!("the counter service speaks version {}, the client {version}: build both from the same checkout", protocol::VERSION));
        }
        let max = etw::max_counters().unwrap_or(4).min(protocol::MAX_COUNTERS);
        if counters.is_empty() || counters.len() > max {
            return Err(format!("{} counters asked for: the CPU counts 1 to {max} at once", counters.len()));
        }
        for (index, name) in counters.iter().enumerate() {
            if counters[..index].contains(name) {
                return Err(format!("{name} asked for twice"));
            }
        }
        // the threads the client named are its own (their handles are held while counting, so their ids stay theirs)
        let mut held = Vec::with_capacity(threads.len());
        for &thread in threads {
            held.push(etw::open_thread_of(process, thread).ok_or_else(|| format!("thread {thread} is not one of the client's"))?);
        }
        let mut notes = Vec::new();
        let mut shared = self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if shared.is_none() {
            register::ensure(counters, &mut notes)?;
            let list = etw::sources()?;
            let ids: Vec<u32> = counters.iter().map(|name| list.iter().find(|(_, source, _, _)| source == name).map(|(id, _, _, _)| *id).ok_or_else(|| format!("the CPU offers no counter {name}"))).collect::<Result<_, _>>()?;
            let session = etw::Session::start(&self.name, &ids, &mut notes)?;
            *shared = Some(Shared { session, sources: counters.to_vec(), processes: Vec::new() });
        }
        let current = shared.as_mut().expect("a session runs");
        let order = admit(&current.sources, counters)?;
        if current.processes.contains(&process) {
            return Err(format!("process {process} is already counting"));
        }
        if !current.processes.is_empty() {
            notes.push(format!("counting beside {} other client(s) in one trace session (it counts {})", current.processes.len(), current.sources.join(", ")));
        }
        let lost_at_join = current.session.join(process, threads);
        current.processes.push(process);
        Ok((Member { process, order, lost_at_join, _threads: held }, notes))
    }

    /// Stops counting a member's process: its results in its counters' order, and what the session lost meanwhile. The
    /// last member out stops the session.
    fn leave(&self, member: &Member) -> Result<protocol::Results, String> {
        let mut shared = self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(current) = shared.as_mut() else {
            return Err("no trace session runs".into());
        };
        current.processes.retain(|&process| process != member.process);
        let (mut results, lost) = if current.processes.is_empty() {
            let finished = shared.take().expect("a session runs");
            finished.session.finish(member.process)?
        } else {
            current.session.leave(member.process)?
        };
        results.lost_events = lost.0.saturating_sub(member.lost_at_join.0);
        results.lost_buffers = lost.1.saturating_sub(member.lost_at_join.1);
        reorder(&mut results, &member.order);
        Ok(results)
    }
}

/// For each counter `asked` names, its index among those the running session counts; Err when it counts one not
fn admit(counting: &[String], asked: &[String]) -> Result<Vec<usize>, String> {
    asked
        .iter()
        .map(|name| {
            counting.iter().position(|counted| counted == name).ok_or_else(|| {
                format!("busy: the counter service counts {} for another client, and not {name}: ask for those (or some of them), or try again when it ends", counting.join(", "))
            })
        })
        .collect()
}

/// A client's results in its counters' order (`order`: each of its counters' index among the session's)
fn reorder(results: &mut protocol::Results, order: &[usize]) {
    for thread in &mut results.threads {
        for slice in &mut thread.slices {
            let counts = slice.counts;
            slice.counts = [0; protocol::MAX_COUNTERS];
            for (index, &from) in order.iter().enumerate() {
                slice.counts[index] = counts[from];
            }
        }
    }
    results.counters = order.len() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client counts in the running session when it asks for the session's counters or some of them, in its own
    /// order; one asking for another is told the service is busy, and what it counts
    #[test]
    fn a_client_joins_a_session_that_counts_what_it_asks_for() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        let session = names(&["InstructionRetired", "TotalCycles", "ReeL1dMisses"]);
        assert_eq!(admit(&session, &names(&["InstructionRetired", "TotalCycles"])), Ok(vec![0, 1]));
        assert_eq!(admit(&session, &names(&["TotalCycles", "InstructionRetired"])), Ok(vec![1, 0]));
        assert_eq!(admit(&session, &names(&["ReeL1dMisses"])), Ok(vec![2]));
        let refused = admit(&names(&["InstructionRetired", "TotalCycles"]), &names(&["InstructionRetired", "BranchMispredictions"])).unwrap_err();
        assert!(refused.starts_with("busy: the counter service counts InstructionRetired, TotalCycles for another client, and not BranchMispredictions"), "{refused}");
    }

    /// A process holds one connection at a time: a second is refused while the first is open, allowed once it closed
    #[test]
    fn a_process_holds_one_connection_at_a_time() {
        let connected = Mutex::new(HashSet::new());
        let first = Connected::claim(&connected, 42);
        assert!(first.is_some());
        assert!(Connected::claim(&connected, 42).is_none(), "a second connection of the same process");
        assert!(connected.lock().unwrap().contains(&42), "and the first keeps its place");
        assert!(Connected::claim(&connected, 43).is_some(), "another process's");
        drop(first);
        assert!(Connected::claim(&connected, 42).is_some(), "once the first closed");
    }

    /// The counts a client gets are in its counters' order, the rest zero
    #[test]
    fn results_come_in_the_clients_order() {
        let slice = |counts: [u64; 3]| {
            let mut slice = protocol::Slice { cpu: 0, start: 1, end: 2, counts: [0; protocol::MAX_COUNTERS] };
            slice.counts[..3].copy_from_slice(&counts);
            slice
        };
        let mut results = protocol::Results { counters: 3, threads: vec![protocol::Thread { id: 7, registered: true, slices: vec![slice([100, 200, 300])] }], ..protocol::Results::default() };
        reorder(&mut results, &[1, 0]);
        assert_eq!(results.counters, 2);
        assert_eq!(&results.threads[0].slices[0].counts[..3], &[200, 100, 0]);
    }
}

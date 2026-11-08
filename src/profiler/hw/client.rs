//! The counter service's client (shared with tools/hw-counters' probe, so it uses nothing but std): connecting to the
//! service's pipe, starting the service when it is installed and stopped, and checking that the pipe is the service's
//! own; the requests; and the [`Reader`], whose readings mark the moments a thread's counts are split at
//! (readings.rs).
//!
//! A reading makes the calling thread switch out: it signals a helper thread and waits for its answer
//! (SignalObjectAndWait, which starts the wait as it signals, so the thread always blocks). The kernel's context switch
//! event then carries the counters' values at that moment, which is what the service hands back as the end of a
//! slice. A reading takes about 3 us and a few thousand cycles of the thread's own.

use super::protocol::{self, Reply, Request, Results};

/// What the service started counting
#[derive(Clone, Debug, Default)]
pub struct Started {
    /// The profile sources counted, in the request's order
    pub sources: Vec<String>,
    pub qpc_frequency: u64,
    pub notes: Vec<String>,
}

/// Where the service listens: the installed service, or `ree-hw-counters serve` in a console (tests)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Service,
    Console,
}

/// What a client says when the service is not installed (docs/cli.md "Hardware counters")
pub const NOT_INSTALLED: &str = "the counter service is not installed: run tools/hw-counters/setup.ps1 once from an elevated PowerShell (docs/cli.md \"Hardware counters\")";

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub const INVALID_HANDLE: Handle = -1isize as Handle;
    pub const INFINITE: u32 = 0xFFFF_FFFF;
    pub const ERROR_FILE_NOT_FOUND: u32 = 2;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    pub const ERROR_PIPE_BUSY: u32 = 231;
    pub const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;
    pub const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;
    // FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE: what the service's pipe grants
    pub const PIPE_ACCESS: u32 = 0x0001 | 0x0002 | 0x0080 | 0x0010_0000;
    pub const OPEN_EXISTING: u32 = 3;
    // SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION: the server may learn who the client is, never act as it
    pub const PIPE_FLAGS: u32 = 0x0010_0000 | 0x0001_0000;
    pub const SC_MANAGER_CONNECT: u32 = 0x0001;
    pub const SERVICE_QUERY_STATUS: u32 = 0x0004;
    pub const SERVICE_START: u32 = 0x0010;
    pub const SC_STATUS_PROCESS_INFO: u32 = 0;
    pub const SERVICE_RUNNING: u32 = 4;
    pub const THREAD_QUERY_LIMITED_INFORMATION: u32 = 0x0800;

    unsafe extern "system" {
        pub fn CreateFileW(name: *const u16, access: u32, share: u32, security: *mut c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
        pub fn ReadFile(file: Handle, buffer: *mut u8, to_read: u32, read: *mut u32, overlapped: *mut c_void) -> i32;
        pub fn WriteFile(file: Handle, buffer: *const u8, to_write: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
        pub fn CloseHandle(handle: Handle) -> i32;
        pub fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
        pub fn GetNamedPipeServerProcessId(pipe: Handle, process: *mut u32) -> i32;
        pub fn GetLastError() -> u32;
        pub fn CreateEventW(attributes: *mut c_void, manual_reset: i32, initial: i32, name: *const u16) -> Handle;
        pub fn SetEvent(event: Handle) -> i32;
        pub fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        pub fn SignalObjectAndWait(signal: Handle, wait: Handle, milliseconds: u32, alertable: i32) -> u32;
        pub fn QueryPerformanceCounter(count: *mut i64) -> i32;
        pub fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
        pub fn GetCurrentThreadId() -> u32;
        pub fn OpenThread(access: u32, inherit: i32, thread: u32) -> Handle;
        pub fn GetThreadDescription(thread: Handle, description: *mut *mut u16) -> i32;
        pub fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
        pub fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
        pub fn StartServiceW(service: Handle, count: u32, arguments: *const *const u16) -> i32;
        pub fn QueryServiceStatusEx(service: Handle, level: u32, buffer: *mut u8, size: u32, needed: *mut u32) -> i32;
        pub fn CloseServiceHandle(handle: Handle) -> i32;
    }

    pub fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

/// The clock the service's slices are in (QueryPerformanceCounter ticks; elsewhere nanoseconds since the first call)
pub fn qpc() -> i64 {
    #[cfg(windows)]
    {
        let mut count = 0i64;
        unsafe { sys::QueryPerformanceCounter(&mut count) };
        count
    }
    #[cfg(not(windows))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as i64
    }
}

/// Ticks of [`qpc`] a second
pub fn qpc_frequency() -> i64 {
    #[cfg(windows)]
    {
        let mut frequency = 0i64;
        unsafe { sys::QueryPerformanceFrequency(&mut frequency) };
        frequency.max(1)
    }
    #[cfg(not(windows))]
    {
        1_000_000_000
    }
}

pub fn current_thread_id() -> u32 {
    #[cfg(windows)]
    {
        unsafe { sys::GetCurrentThreadId() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// A thread of this process's name (its description), while it runs
pub fn thread_name(thread: u32) -> Option<String> {
    #[cfg(windows)]
    unsafe {
        let handle = sys::OpenThread(sys::THREAD_QUERY_LIMITED_INFORMATION, 0, thread);
        if handle.is_null() {
            return None;
        }
        let mut description: *mut u16 = std::ptr::null_mut();
        let result = sys::GetThreadDescription(handle, &mut description);
        sys::CloseHandle(handle);
        if result < 0 || description.is_null() {
            return None;
        }
        let mut length = 0;
        while *description.add(length) != 0 {
            length += 1;
        }
        let name = String::from_utf16_lossy(std::slice::from_raw_parts(description, length));
        sys::LocalFree(description as *mut _);
        return (!name.is_empty()).then_some(name);
    }
    #[allow(unreachable_code)]
    {
        let _ = thread;
        None
    }
}

/// A connection to the counter service
pub struct Client {
    #[cfg(windows)]
    pipe: sys::Handle,
}

#[cfg(windows)]
struct Pipe(sys::Handle);

#[cfg(windows)]
impl std::io::Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        let wanted = buffer.len().min(1 << 20) as u32;
        if unsafe { sys::ReadFile(self.0, buffer.as_mut_ptr(), wanted, &mut read, std::ptr::null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(read as usize)
    }
}

#[cfg(windows)]
impl Client {
    /// Connects to the service at `endpoint`, starting the installed service when it is stopped
    pub fn connect(endpoint: Endpoint) -> Result<Client, String> {
        let server = match endpoint {
            Endpoint::Service => Some(start_service()?),
            Endpoint::Console => None,
        };
        let name = sys::wide(if endpoint == Endpoint::Service { protocol::PIPE_NAME } else { protocol::CONSOLE_PIPE_NAME });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pipe = loop {
            let pipe = unsafe { sys::CreateFileW(name.as_ptr(), sys::PIPE_ACCESS, 0, std::ptr::null_mut(), sys::OPEN_EXISTING, sys::PIPE_FLAGS, std::ptr::null_mut()) };
            if pipe != sys::INVALID_HANDLE {
                break pipe;
            }
            let error = unsafe { sys::GetLastError() };
            // (no pipe in a console means nobody serves it; the service's appears once it has started)
            if std::time::Instant::now() >= deadline || (error == sys::ERROR_FILE_NOT_FOUND && endpoint == Endpoint::Console) {
                return Err(match error {
                    sys::ERROR_PIPE_BUSY => "the counter service is counting for as many clients as it serves at once (eight); try again when one ends".into(),
                    sys::ERROR_FILE_NOT_FOUND if endpoint == Endpoint::Console => "no `ree-hw-counters serve` is listening".into(),
                    sys::ERROR_ACCESS_DENIED => "the counter service's pipe refused this account (setup.ps1 gives it to the account that installs it)".into(),
                    error => format!("the counter service's pipe: error {error}"),
                });
            }
            match error {
                // (every instance of its pipe holds a client: wait for one to end; not there yet: the service is starting)
                sys::ERROR_PIPE_BUSY => unsafe {
                    sys::WaitNamedPipeW(name.as_ptr(), 2000);
                },
                sys::ERROR_FILE_NOT_FOUND => std::thread::sleep(std::time::Duration::from_millis(20)),
                _ => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        };
        let client = Client { pipe };
        // the pipe is the service's own: a process that took its name first is refused
        if let Some(service_process) = server {
            let mut serving = 0u32;
            if unsafe { sys::GetNamedPipeServerProcessId(pipe, &mut serving) } == 0 || serving != service_process {
                return Err(format!("the counter service's pipe is served by process {serving}, not the service's ({service_process})"));
            }
        }
        Ok(client)
    }

    fn send(&mut self, request: &Request) -> Result<(), String> {
        let bytes = request.encode();
        let mut sent = 0usize;
        while sent < bytes.len() {
            let mut written = 0u32;
            let chunk = (bytes.len() - sent).min(1 << 20) as u32;
            if unsafe { sys::WriteFile(self.pipe, bytes[sent..].as_ptr(), chunk, &mut written, std::ptr::null_mut()) } == 0 {
                return Err(format!("writing to the counter service: {}", std::io::Error::last_os_error()));
            }
            sent += written as usize;
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Reply, String> {
        let (kind, body) = protocol::read_message(&mut Pipe(self.pipe), protocol::MAX_REPLY_BYTES).map_err(|error| format!("the counter service: {error}"))?;
        Reply::decode(kind, &body).map_err(|error| format!("the counter service's reply: {error}"))
    }

    /// Starts counting `sources` (Windows profile sources) on this process's threads; `threads` are read apart
    pub fn start(&mut self, sources: &[String], threads: &[u32]) -> Result<Started, String> {
        self.send(&Request::Start { version: protocol::VERSION, counters: sources.to_vec(), threads: threads.to_vec() })?;
        match self.receive()? {
            Reply::Started { version, sources, qpc_frequency, notes } if version == protocol::VERSION => Ok(Started { sources, qpc_frequency, notes }),
            Reply::Started { version, .. } => Err(format!("the counter service speaks version {version}, this build {} (reinstall it with setup.ps1)", protocol::VERSION)),
            Reply::Error(message) => Err(message),
            Reply::Results(_) => Err("the counter service sent results before counting".into()),
        }
    }

    /// Stops counting: every thread's slices
    pub fn stop(mut self) -> Result<Results, String> {
        self.send(&Request::Stop)?;
        match self.receive()? {
            Reply::Results(results) => Ok(results),
            Reply::Error(message) => Err(message),
            Reply::Started { .. } => Err("the counter service started again instead of stopping".into()),
        }
    }
}

#[cfg(windows)]
impl Drop for Client {
    fn drop(&mut self) {
        unsafe { sys::CloseHandle(self.pipe) };
    }
}

/// Starts the installed service when it is stopped: the process that runs it
#[cfg(windows)]
fn start_service() -> Result<u32, String> {
    unsafe {
        let manager = sys::OpenSCManagerW(std::ptr::null(), std::ptr::null(), sys::SC_MANAGER_CONNECT);
        if manager.is_null() {
            return Err(format!("the service manager: error {}", sys::GetLastError()));
        }
        let name = sys::wide(protocol::SERVICE_NAME);
        let mut service = sys::OpenServiceW(manager, name.as_ptr(), sys::SERVICE_QUERY_STATUS | sys::SERVICE_START);
        if service.is_null() && sys::GetLastError() == sys::ERROR_ACCESS_DENIED {
            // (an account that may not start it can still use it while it runs)
            service = sys::OpenServiceW(manager, name.as_ptr(), sys::SERVICE_QUERY_STATUS);
        }
        if service.is_null() {
            let error = sys::GetLastError();
            sys::CloseServiceHandle(manager);
            return Err(match error {
                sys::ERROR_SERVICE_DOES_NOT_EXIST => NOT_INSTALLED.into(),
                sys::ERROR_ACCESS_DENIED => "this account may not use the counter service (setup.ps1 gives it to the account that installs it)".into(),
                error => format!("the counter service: error {error}"),
            });
        }
        let status = |service: sys::Handle| -> Option<(u32, u32)> {
            let mut buffer = [0u8; 36];
            let mut needed = 0u32;
            (sys::QueryServiceStatusEx(service, sys::SC_STATUS_PROCESS_INFO, buffer.as_mut_ptr(), buffer.len() as u32, &mut needed) != 0)
                .then(|| (u32::from_le_bytes(buffer[4..8].try_into().unwrap()), u32::from_le_bytes(buffer[28..32].try_into().unwrap())))
        };
        let mut result = Err("the counter service did not start in 10 s".to_string());
        if status(service).is_some_and(|(state, _)| state != sys::SERVICE_RUNNING) && sys::StartServiceW(service, 0, std::ptr::null()) == 0 {
            let error = sys::GetLastError();
            if error != sys::ERROR_SERVICE_ALREADY_RUNNING {
                result = Err(if error == sys::ERROR_ACCESS_DENIED { "this account may not start the counter service (setup.ps1 gives it to the account that installs it)".to_string() } else { format!("starting the counter service: error {error}") });
                sys::CloseServiceHandle(service);
                sys::CloseServiceHandle(manager);
                return result;
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match status(service) {
                Some((sys::SERVICE_RUNNING, process)) if process != 0 => {
                    result = Ok(process);
                    break;
                }
                Some(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                None => {
                    result = Err(format!("the counter service's status: error {}", sys::GetLastError()));
                    break;
                }
            }
        }
        sys::CloseServiceHandle(service);
        sys::CloseServiceHandle(manager);
        result
    }
}

#[cfg(not(windows))]
impl Client {
    pub fn connect(_endpoint: Endpoint) -> Result<Client, String> {
        Err("hardware counters are read through a Windows service; this build runs elsewhere".into())
    }

    pub fn start(&mut self, _sources: &[String], _threads: &[u32]) -> Result<Started, String> {
        Err("hardware counters are read on Windows only".into())
    }

    pub fn stop(self) -> Result<Results, String> {
        Err("hardware counters are read on Windows only".into())
    }
}

/// Readings: each one makes the calling thread switch out, so the service's slices end there (see the module's notes)
pub struct Reader {
    #[cfg(windows)]
    events: (usize, usize),
    #[cfg(windows)]
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(windows)]
    helper: Option<std::thread::JoinHandle<()>>,
}

impl Reader {
    #[cfg(windows)]
    pub fn new() -> Result<Reader, String> {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let (go, done) = unsafe { (sys::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null()), sys::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null())) };
        if go.is_null() || done.is_null() {
            return Err(format!("the reader's events: {}", std::io::Error::last_os_error()));
        }
        let quit = Arc::new(AtomicBool::new(false));
        let (helper_quit, events) = (quit.clone(), (go as usize, done as usize));
        let helper = std::thread::Builder::new()
            .name("ree-hw-counters-reader".into())
            .spawn(move || {
                let (go, done) = (events.0 as sys::Handle, events.1 as sys::Handle);
                loop {
                    unsafe { sys::WaitForSingleObject(go, sys::INFINITE) };
                    unsafe { sys::SetEvent(done) };
                    if helper_quit.load(Ordering::Relaxed) {
                        return;
                    }
                }
            })
            .map_err(|error| format!("the reader's thread: {error}"))?;
        Ok(Reader { events, quit, helper: Some(helper) })
    }

    #[cfg(not(windows))]
    pub fn new() -> Result<Reader, String> {
        Ok(Reader {})
    }

    /// A reading: the moment (in [`qpc`]'s clock) the calling thread switched out for it
    #[inline]
    pub fn read(&self) -> i64 {
        let at = qpc();
        #[cfg(windows)]
        unsafe {
            sys::SignalObjectAndWait(self.events.0 as sys::Handle, self.events.1 as sys::Handle, sys::INFINITE, 0);
        }
        at
    }
}

#[cfg(windows)]
impl Drop for Reader {
    fn drop(&mut self) {
        self.quit.store(true, std::sync::atomic::Ordering::Relaxed);
        unsafe { sys::SignalObjectAndWait(self.events.0 as sys::Handle, self.events.1 as sys::Handle, 2000, 0) };
        if let Some(helper) = self.helper.take() {
            let _ = helper.join();
        }
        unsafe {
            sys::CloseHandle(self.events.0 as sys::Handle);
            sys::CloseHandle(self.events.1 as sys::Handle);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A reading switches the thread out and back: its helper answers every one, a reading takes microseconds, and the
    /// reader's thread ends with it
    #[test]
    fn readings_switch_out_and_come_back() {
        let reader = Reader::new().unwrap();
        let started = std::time::Instant::now();
        let mut last = 0;
        for _ in 0..2000 {
            let at = reader.read();
            assert!(at >= last, "readings come in order");
            last = at;
        }
        let each = started.elapsed().as_secs_f64() / 2000.0;
        assert!(each < 1e-3, "a reading took {:.1} us", each * 1e6);
        drop(reader);
    }

    /// Without an installed service the client says how to install it (this PC's agents never have one installed for
    /// tests: a test that finds one is skipped)
    #[test]
    fn a_console_endpoint_with_nobody_listening_is_named() {
        if let Err(message) = Client::connect(Endpoint::Console) {
            assert!(message.contains("serve") || message.contains("pipe"), "{message}");
        }
    }
}

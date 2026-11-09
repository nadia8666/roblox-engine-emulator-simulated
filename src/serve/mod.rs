//! `ree serve`: a warm process that runs `ree record` / `ree run` jobs, so a recording does not
//! pay for starting the engine, the bundle worker and the caches each time. It keeps the
//! reflection database, module sources and their bytecode ([`modules::reuse_compiled_modules`])
//! and asset manifest bundles converted (one store its workers share:
//! [`crate::project::model_assets::keep_bundles_in`]); every job still gets its own engine, VM and
//! DataModel, dropped after its reply. Its workers ([`default_workers`]) run jobs side by side. A
//! worker whose recording's (or run's) harness called `Context.Checkpoint()` runs the next one with the
//! same command and arguments ahead, up to that call, and the job that asks for it goes on from there
//! when what it read is unchanged (crate::record::prepare).
//!
//! `ree serve` listens on 127.0.0.1 (a port the OS assigns) and records itself in
//! `<state dir>/services/<build>-<env>.json` (`build`: the binary, [`build_id`]; `env`: the
//! environment variables a run reads, [`env_digest`]), so a client finds the service that runs
//! its own build with its own environment. [`client`] (`ree record --service ...`) starts one when
//! it is missing. Requests need an `X-REE: 1` header and a local Host.
//!
//! - `GET /health`: build, pid, uptime, job counters, prepared recordings (made, used, stale, held)
//! - `POST /job`: `{"command": "record" | "run" | "warm", "args": [...]}` (paths absolute) →
//!   `{"code", "stdout", "stderr"}`: what the command would have printed and its exit code (`warm`:
//!   `ree serve --warm`, record::run::warm)
//! - `POST /shutdown`: exits once running jobs finish
//!
//! It runs in its state dir and exits after `--idle` seconds without jobs (default 20, [`IDLE_EXIT`]);
//! health requests and runs prepared ahead are not jobs. A job lives no longer than its client (exit 125).

pub mod client;
pub mod http;

use std::{
    collections::VecDeque,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    launch,
    lua::modules,
    record::{prepare, run as command},
};

/// What a service's record file holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub port: u16,
    pub pid: u32,
    /// The binary the service runs ([`build_id`]: its starter's, the service runs a copy)
    pub build: String,
    /// The environment it runs jobs with ([`env_digest`])
    pub env: String,
    pub started: u64,
    /// The executable the starting client ran: a newer build of it replaces this service
    #[serde(default)]
    pub source: Option<String>,
}

pub fn services_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("services")
}

pub fn record_path(state_dir: &Path, build: &str, env: &str) -> PathBuf {
    services_dir(state_dir).join(format!("{build}-{env}.json"))
}

/// Identifies the running binary: version, size and modification time of the executable.
pub fn build_id() -> String {
    let exe = std::env::current_exe().ok();
    let meta = exe.as_ref().and_then(|exe| std::fs::metadata(exe).ok());
    let modified = meta.as_ref().and_then(|meta| meta.modified().ok()).and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|time| time.as_millis()).unwrap_or(0);
    format!("{}-{}-{}", env!("CARGO_PKG_VERSION"), meta.map(|meta| meta.len()).unwrap_or(0), modified)
}

/// The environment variables a run can read, hashed: a job runs in a service started with the same.
/// (The engine's RUR_* / REE_*, ror's ROR_* and LOCALAPPDATA.)
/// Only an unkeyed 64-bit hash of their values reaches disk (the service record's name), never the values.
pub fn env_digest() -> String {
    use std::hash::{Hash, Hasher};
    if crate::is_standalone_binary() {
        // The key is the environment the service is started with and its workers receive (launch::worker_environment),
        // so the same environment always reaches the same service, and any variable that differs reaches another.
        let mut hasher = rustc_hash::FxHasher::default();
        launch::worker_environment().hash(&mut hasher);
        return format!("{:016x}", hasher.finish());
    }
    let mut variables: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.to_string_lossy().into_owned())))
        .filter(|(name, _)| {
            let upper = name.to_ascii_uppercase();
            ["RUR_", "REE_", "ROR_"].iter().any(|prefix| upper.starts_with(prefix)) || upper == "LOCALAPPDATA"
        })
        .map(|(name, value)| (name.to_ascii_uppercase(), value))
        .collect();
    variables.sort();
    let mut hasher = rustc_hash::FxHasher::default();
    variables.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The state dir of the default repo, for a command that has no explicit one. A standalone ree can lack a per-user
/// cache folder: this prints that as `<command>: <reason>` and returns `None`, and the caller exits with code 2.
fn default_state_dir(command: &str) -> Option<PathBuf> {
    match crate::try_state_dir(&crate::default_repo()) {
        Ok(dir) => Some(dir),
        Err(error) => {
            eprintln!("{command}: {error}");
            None
        }
    }
}

/// How long a service waits for its next job before it exits (`--idle`). What it keeps warm (compiled
/// modules, the reflection, converted bundles) saves a sim ~0.2 s and ~0.4 core-s; held idle, it is
/// ~400 MB: the jobs of a burst (a batch, @align passes, a tool's sims between their renders) come
/// within this of each other, and a lone sim later starts a service again.
const IDLE_EXIT: Duration = Duration::from_secs(20);

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0)
}

const USAGE: &str = "usage: ree serve [--idle <s>] [--workers <n>] [--state-dir <dir>]   run the service in the foreground (a client starts one detached when missing);
                                                                     --workers: jobs at once (default REE_SERVE_WORKERS, else one per 4 hardware threads and 4 GB, 1-4)
       ree serve --status | --stop                                    the state dir's services: their health, or ask them to exit
       ree serve --warm [record's engine options]                    start this build's service when missing and read in what a first recording of the
                                                                     project reads (compiled modules, the bundles recent recordings imported, the
                                                                     project scan, the Studio hierarchy); records nothing; prints a JSON summary";

/// `ree serve`'s options, and whether a value follows each.
const OPTIONS: [(&str, bool); 8] = [("--state-dir", true), ("--build", true), ("--env", true), ("--source", true), ("--idle", true), ("--workers", true), ("--status", false), ("--stop", false)];

/// `ree serve [--state-dir <dir>] [--build <id>] [--env <digest>] [--source <exe>] [--idle <s>]
/// [--workers <n>]`, `ree serve --status`, `ree serve --stop`, `ree serve --help`.
pub fn cli(argv: &[String]) -> i32 {
    let args = launch::split_equals(argv);
    // (`--warm` takes record's engine options, not serve's)
    if args.first().is_some_and(|arg| arg == "--warm") {
        return client::warm(&args[1..]);
    }
    match check(&args) {
        Ok(true) => {
            println!("{USAGE}");
            return 0;
        }
        Err(error) => {
            eprintln!("ree serve: {error}\n{USAGE}");
            return 2;
        }
        Ok(false) => {}
    }
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned();
    // It runs from a copy in its state dir: its checkout is its starter's. Ree's own binary already chose that
    // in `init_binary` (and the first choice stands), so this is for a library host that calls `cli` itself,
    // which finds the starter's checkout the way a host does: the nearest `Cargo.lock`.
    let starter_checkout = value("--source").and_then(|source| crate::checkout_of(std::path::Path::new(&source)));
    if let Some(repo) = starter_checkout {
        crate::set_default_repo(repo);
    }
    // An explicit directory needs no per-user cache, so that lookup (which can fail) runs only without one
    let state_dir = match value("--state-dir") {
        Some(dir) => PathBuf::from(dir),
        None => match default_state_dir("ree serve") {
            Some(dir) => dir,
            None => return 2,
        },
    };
    if args.iter().any(|a| a == "--status") {
        for (info, health) in client::status(&state_dir) {
            println!("{}", json!({ "port": info.port, "pid": info.pid, "build": info.build, "env": info.env, "health": health }));
        }
        return 0;
    }
    if args.iter().any(|a| a == "--stop") {
        let stopped = client::stop(&state_dir);
        eprintln!("ree: asked {stopped} service(s) to exit");
        return 0;
    }
    // (the service runs in its state dir: relative ones are the caller's)
    let state_dir = std::path::absolute(&state_dir).unwrap_or(state_dir);
    let env = value("--env").unwrap_or_else(env_digest);
    for (name, value) in launch::path_variables() {
        // SAFETY: before this process starts any thread of its own
        unsafe { std::env::set_var(name, value) };
    }
    let options = ServeOptions {
        build: value("--build").unwrap_or_else(build_id),
        env,
        source: value("--source"),
        idle: value("--idle").and_then(|v| v.parse().ok()).map_or(IDLE_EXIT, Duration::from_secs_f64),
        workers: value("--workers").and_then(|v| v.parse().ok()).unwrap_or_else(default_workers).clamp(1, 8),
        state_dir,
    };
    match serve(options) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ree serve: {error}");
            1
        }
    }
}

/// `Ok(true)` when the arguments ask for the usage; an error names an option `ree serve` does not take
/// or one missing its value (it would otherwise start a service that waits for jobs).
fn check(args: &[String]) -> Result<bool, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(true);
    }
    let mut index = 0;
    while index < args.len() {
        index += match OPTIONS.iter().find(|(name, _)| *name == args[index]) {
            Some((_, false)) => 1,
            Some((_, true)) if index + 1 < args.len() => 2,
            Some((name, true)) => return Err(format!("{name} needs a value")),
            None => return Err(format!("unknown option {}", args[index])),
        };
    }
    Ok(false)
}

/// How many jobs a service runs at once unless told (`--workers`): REE_SERVE_WORKERS, else one per 4
/// hardware threads and per 4 GB of memory, from 1 to 4. Callers share a service, and with one worker
/// their recordings waited in its queue; jobs are independent (an engine and a VM each), so running
/// them side by side changes no recording. A job's memory is bounded by [`wait_for_memory`].
pub fn default_workers() -> usize {
    if let Some(workers) = std::env::var("REE_SERVE_WORKERS").ok().and_then(|value| value.trim().parse::<usize>().ok()) {
        return workers.clamp(1, 8);
    }
    let threads = std::thread::available_parallelism().map_or(1, |threads| threads.get());
    let memory_gb = memory().map_or(16, |(total, _)| (total >> 30) as usize);
    (threads / 4).min(memory_gb / 4).clamp(1, 4)
}

/// Sizes rayon's global pool before its first use (a command, a service). Runs list the project and read
/// ahead on it, which gains nothing past a few threads, and every thread's heap keeps pages committed:
/// a profile's rigs scene peaked at 330 MB private with one thread per hardware thread on a 32-thread
/// machine, 290 MB with 8, in the same time. RAYON_NUM_THREADS still decides when set.
pub fn size_pool() {
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        let threads = std::thread::available_parallelism().map_or(4, |count| count.get()).min(8);
        let _ = rayon::ThreadPoolBuilder::new().num_threads(threads).build_global();
    }
}

/// The machine's physical memory: (total, available) bytes.
#[cfg(windows)]
fn memory() -> Option<(u64, u64)> {
    #[repr(C)]
    struct Status {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page: u64,
        available_page: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended: u64,
    }
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status = Status { length: std::mem::size_of::<Status>() as u32, load: 0, total_physical: 0, available_physical: 0, total_page: 0, available_page: 0, total_virtual: 0, available_virtual: 0, available_extended: 0 };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some((status.total_physical, status.available_physical))
}

#[cfg(not(windows))]
fn memory() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| text.lines().find(|line| line.starts_with(name))?.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024);
    Some((field("MemTotal:")?, field("MemAvailable:")?))
}

/// The memory a job may need on top of what is in use: a job does not start beside others while less
/// is available (a busy machine runs them one after another instead of paging).
const JOB_MEMORY: u64 = 2 << 30;

/// Waits, while other jobs run, until the machine has [`JOB_MEMORY`] available; false once the job's client has gone
fn wait_for_memory(service: &Service, caller: &TcpStream) -> bool {
    let mut waited = 0u32;
    while service.running.load(Ordering::SeqCst) > 0 && memory().is_some_and(|(_, available)| available < JOB_MEMORY) {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
        if waited % 10 == 0 && http::closed(caller) {
            return false;
        }
    }
    true
}

pub struct ServeOptions {
    pub state_dir: PathBuf,
    pub build: String,
    pub env: String,
    pub source: Option<String>,
    /// Exit after this long without jobs
    pub idle: Duration,
    /// Jobs run at once (each worker thread runs its jobs one after another)
    pub workers: usize,
}

struct Job {
    command: String,
    args: Vec<String>,
    stream: TcpStream,
    /// A recording's or run's arguments as prepared runs serve them (prepare::Keys)
    key: Option<prepare::Keys>,
}

/// A worker as the others see it: the recording it holds prepared (its key), and whether it is
/// running a job or preparing a recording now (and when that is expected to be ready).
#[derive(Clone, Default)]
struct Slot {
    key: Option<prepare::Key>,
    running: bool,
    preparing: bool,
    ready_by: Option<Instant>,
}

/// A job waits for the worker preparing its recording when that is expected to be ready within this;
/// otherwise a free worker records it from the start (~0.3 s for a cosmetics sim, against ~0.05 s from
/// a prepared run), and prepares it too after.
const WAIT_FOR_PREPARED: Duration = Duration::from_millis(200);

/// What a worker's first preparation is expected to take, from its reply on (drop, collect, prepare)
const FIRST_PREPARATION: Duration = Duration::from_millis(400);

/// Workers holding or preparing the most recent recording: an idle one holding nothing prepares it
/// too until this many do, so the next job like it finds one ready while another is re-prepared
/// (a tool's sims come ~0.3 s apart, about what re-preparing one takes).
const HOLDERS: usize = 2;

/// A recording is prepared ahead only when another job it serves started this recently (or waits):
/// jobs that repeat (a batch, @align passes, parallel lanes) use their prepared runs; a lone sim's
/// would be dropped unused when the service exits, its setup run and its engine held for nothing.
const REPEATS_WITHIN: Duration = Duration::from_secs(10);

/// The jobs [`REPEATS_WITHIN`] looks back over
const STARTS_KEPT: usize = 32;

/// What a worker takes next: a job, or a recording (or run) to prepare ahead of the jobs.
enum Work {
    Job(Job),
    Prepare(Ahead),
}

/// A command to prepare ahead: its arguments, whether its harness has Prepare, and whether it records.
#[derive(Clone)]
struct Ahead {
    args: Vec<String>,
    split: bool,
    /// Its harness parked between frames (Context.FrameCheckpoint)
    framed: bool,
    record: bool,
}

impl Ahead {
    fn key(&self) -> prepare::Key {
        prepare::Key::of_framed(&self.args, self.split, self.framed, self.record)
    }
}

/// How the prepared recordings went: made, used by their job, dropped as stale when it came, and why
/// the last one that could not be made was not.
#[derive(Default)]
struct PreparedCounts {
    made: AtomicU64,
    used: AtomicU64,
    stale: AtomicU64,
    /// Not prepared: the job's own run read what a prepared run cannot check, or no job like it repeated
    skipped: AtomicU64,
    unmade: Mutex<Option<String>>,
}

struct Service {
    queue: Mutex<VecDeque<Job>>,
    queued: Condvar,
    /// One per worker (lock order: `queue`, then `slots`)
    slots: Mutex<Vec<Slot>>,
    preparing: AtomicU64,
    prepared: PreparedCounts,
    started: Instant,
    /// When the last job came or finished: the service exits `idle` after it (requests for its health,
    /// and preparing ahead, are not jobs)
    last_job: Mutex<Instant>,
    /// The latest jobs' keys and when each started ([`REPEATS_WITHIN`])
    starts: Mutex<VecDeque<(prepare::Keys, Instant)>>,
    running: AtomicU64,
    done: AtomicU64,
    failed: AtomicU64,
    /// Jobs not run, or stopped, because their client had gone
    abandoned: AtomicU64,
    shutting_down: AtomicBool,
    /// The most recent recording or run prepared after its job
    recent: Mutex<Option<Ahead>>,
    build: String,
    env: String,
    workers: usize,
}

pub fn serve(options: ServeOptions) -> Result<(), String> {
    // a process holds the folder it runs in (Windows will not delete it): never the caller's, a worktree say, for the service's lifetime
    if let Err(error) = std::fs::create_dir_all(&options.state_dir).and_then(|()| std::env::set_current_dir(&options.state_dir)) {
        eprintln!("ree serve: working from {}: {error}", options.state_dir.display());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("binding the service port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    size_pool();
    modules::reuse_compiled_modules();
    // (what every run reads, made before the first one: in the long-lived heap, crate::alloc)
    crate::alloc::long_lived(|| {
        let _ = rbx_reflection_database::get();
        crate::Reflection::shared()
    })?;
    let service = Arc::new(Service {
        queue: Mutex::new(VecDeque::new()),
        queued: Condvar::new(),
        slots: Mutex::new(vec![Slot::default(); options.workers]),
        preparing: AtomicU64::new(0),
        prepared: PreparedCounts::default(),
        started: Instant::now(),
        last_job: Mutex::new(Instant::now()),
        starts: Mutex::new(VecDeque::new()),
        running: AtomicU64::new(0),
        done: AtomicU64::new(0),
        failed: AtomicU64::new(0),
        abandoned: AtomicU64::new(0),
        shutting_down: AtomicBool::new(false),
        recent: Mutex::new(None),
        build: options.build.clone(),
        env: options.env.clone(),
        workers: options.workers,
    });
    let info = ServiceInfo { port, pid: std::process::id(), build: options.build.clone(), env: options.env.clone(), started: unix_now(), source: options.source.clone() };
    let record = record_path(&options.state_dir, &options.build, &options.env);
    std::fs::create_dir_all(services_dir(&options.state_dir)).map_err(|e| e.to_string())?;
    write_atomic(&record, serde_json::to_string_pretty(&info).map_err(|e| e.to_string())?.as_bytes()).map_err(|e| format!("{}: {e}", record.display()))?;
    eprintln!("ree: service on 127.0.0.1:{port} (pid {}, build {}, env {}, {} worker(s))", info.pid, info.build, info.env, options.workers);

    // (the workers keep converted bundles in one store: a bundle is converted once for all of them)
    #[cfg(feature = "bundles")]
    let bundles = crate::alloc::long_lived(crate::project::model_assets::BundleStore::default);
    for index in 0..options.workers {
        let service = service.clone();
        #[cfg(feature = "bundles")]
        let bundles = bundles.clone();
        std::thread::Builder::new()
            .name(format!("ree-worker-{index}"))
            .stack_size(64 << 20)
            .spawn(move || {
                #[cfg(feature = "bundles")]
                crate::project::model_assets::keep_bundles_in(bundles);
                work(&service, index)
            })
            .map_err(|e| format!("starting a worker: {e}"))?;
    }
    {
        let service = service.clone();
        let idle = options.idle;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let quiet = service.last_job.lock().map(|time| time.elapsed()).unwrap_or_default();
                // (a run prepared ahead, or being prepared, is for a job that has not come: it goes with the process)
                let busy = service.running.load(Ordering::SeqCst) > 0 || service.queue.lock().is_ok_and(|queue| !queue.is_empty());
                if !busy && (service.shutting_down.load(Ordering::SeqCst) || quiet > idle) {
                    // leave the record to a newer service that already replaced it
                    if let Ok(text) = std::fs::read_to_string(&record) {
                        if serde_json::from_str::<ServiceInfo>(&text).is_ok_and(|current| current.pid == std::process::id()) {
                            let _ = std::fs::remove_file(&record);
                        }
                    }
                    if let Some(hook) = crate::BEFORE_EXIT.get() {
                        hook();
                    }
                    std::process::exit(0);
                }
            }
        });
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let service = service.clone();
        std::thread::spawn(move || {
            let _ = stream.set_nodelay(true);
            if let Err(error) = handle(&service, stream.try_clone().ok(), &stream) {
                let _ = http::write_response(&stream, 500, json!({ "error": error }).to_string().as_bytes());
            }
        });
    }
    Ok(())
}

fn handle(service: &Service, owned: Option<TcpStream>, stream: &TcpStream) -> Result<(), String> {
    let request = http::read_request(stream)?;
    let local_host = request.header("host").is_some_and(|host| host.starts_with("127.0.0.1:") || host.starts_with("localhost:"));
    if request.header("x-ree") != Some("1") || !local_host {
        return http::write_response(stream, 403, br#"{"error":"requests need the X-REE: 1 header and a local Host"}"#).map_err(|e| e.to_string());
    }
    let reply = |status: u16, body: serde_json::Value| http::write_response(stream, status, body.to_string().as_bytes()).map_err(|e| e.to_string());
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => reply(
            200,
            json!({
                "ok": true,
                "build": service.build,
                "env": service.env,
                "pid": std::process::id(),
                "workers": service.workers,
                "uptimeSeconds": service.started.elapsed().as_secs_f64(),
                "jobs": {
                    "running": service.running.load(Ordering::SeqCst),
                    "queued": service.queue.lock().map(|queue| queue.len()).unwrap_or(0),
                    "done": service.done.load(Ordering::SeqCst),
                    "failed": service.failed.load(Ordering::SeqCst),
                    "abandoned": service.abandoned.load(Ordering::SeqCst),
                },
                "idleSeconds": service.last_job.lock().map(|time| time.elapsed().as_secs_f64()).unwrap_or_default(),
                "prepared": {
                    "made": service.prepared.made.load(Ordering::SeqCst),
                    "used": service.prepared.used.load(Ordering::SeqCst),
                    "stale": service.prepared.stale.load(Ordering::SeqCst),
                    "skipped": service.prepared.skipped.load(Ordering::SeqCst),
                    "unmade": service.prepared.unmade.lock().ok().and_then(|why| why.clone()),
                    "held": service.slots.lock().map(|slots| slots.iter().filter(|slot| slot.key.is_some() && !slot.preparing).count()).unwrap_or(0),
                },
                "shuttingDown": service.shutting_down.load(Ordering::SeqCst),
            }),
        ),
        ("POST", "/job") => {
            #[derive(Deserialize)]
            struct Body {
                command: String,
                args: Vec<String>,
            }
            let body: Body = serde_json::from_slice(&request.body).map_err(|e| format!("job JSON: {e}"))?;
            if !matches!(body.command.as_str(), "record" | "run" | "warm") {
                return reply(400, json!({ "error": format!("unknown command {}", body.command) }));
            }
            if service.shutting_down.load(Ordering::SeqCst) {
                return reply(503, json!({ "error": "the service is shutting down" }));
            }
            let stream = owned.ok_or("no stream")?;
            let key = (body.command != "warm").then(|| prepare::Keys::of(&body.args, body.command == "record"));
            let mut queue = service.queue.lock().map_err(|_| "queue poisoned")?;
            if let Ok(mut time) = service.last_job.lock() {
                *time = Instant::now();
            }
            queue.push_back(Job { command: body.command, args: body.args, stream, key });
            // (every worker looks: the one holding this recording prepared takes it)
            service.queued.notify_all();
            Ok(())
        }
        ("POST", "/shutdown") => {
            service.shutting_down.store(true, Ordering::SeqCst);
            reply(200, json!({ "ok": true }))
        }
        _ => reply(404, json!({ "error": "not found" })),
    }
}

/// A worker: takes jobs one at a time; each job's engine is dropped after its reply is sent. After a
/// recording whose harness has a checkpoint it prepares the next one (crate::record::prepare), which it
/// holds until a job takes it or another job comes.
fn work(service: &Service, index: usize) {
    let mut parked: Option<prepare::Prepared> = None;
    // (how long this worker's last preparation took from its reply on: when the next is expected ready)
    let mut took: Option<Duration> = None;
    loop {
        let job = match take(service, index, took) {
            Some(Work::Job(job)) => job,
            Some(Work::Prepare(ahead)) => {
                let started = Instant::now();
                parked = prepare_next(service, index, &ahead);
                if parked.is_some() {
                    took = Some(started.elapsed());
                }
                continue;
            }
            None => return,
        };
        // (its client left while it waited: nobody would read what it records)
        if http::closed(&job.stream) || !wait_for_memory(service, &job.stream) {
            service.abandoned.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut slots) = service.slots.lock() {
                slots[index].running = false;
            }
            service.queued.notify_all();
            continue;
        }
        service.running.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        if let (Some(keys), Ok(mut starts)) = (&job.key, service.starts.lock()) {
            starts.push_back((keys.clone(), started));
            if starts.len() > STARTS_KEPT {
                starts.pop_front();
            }
        }
        // (a recording prepared for other arguments is dropped after the reply, with the job's engine)
        let other = parked.take_if(|prepared| !job.key.as_ref().is_some_and(|keys| prepared.key().serves(keys)));
        // (its frames stop once its client has gone: watchdog::caller_gone)
        let peer = job.stream.try_clone().ok();
        let caller_gone = move || peer.as_ref().is_some_and(http::closed);
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::watchdog::for_caller(caller_gone, || run_job(&job.command, &job.args, &mut parked, &service.prepared))));
        parked = None;
        let (reply, emulator, next) = match ran {
            Ok(ran) => ran,
            Err(_) => (json!({ "code": 101, "stdout": "", "stderr": format!("ree {}: the job panicked\n", job.command) }), None, None),
        };
        let abandoned = reply["code"].as_i64() == Some(i64::from(crate::watchdog::EXIT_ORPHANED));
        // (a burst whose caller has gone has no next job to prepare for)
        let next = next.filter(|ahead| {
            let repeats = !abandoned && repeats(service, &ahead.key(), started);
            if !repeats {
                service.prepared.skipped.fetch_add(1, Ordering::SeqCst);
            }
            repeats
        });
        let failed = reply["code"].as_i64() != Some(0);
        // (from its reply on, a job like it waits for the run this worker prepares next, when that is
        // expected soon enough: take)
        let replied = Instant::now();
        if let (Some(ahead), Ok(mut slots)) = (&next, service.slots.lock()) {
            slots[index] = Slot { key: Some(ahead.key()), running: false, preparing: true, ready_by: Some(replied + took.unwrap_or(FIRST_PREPARATION)) };
        }
        let _ = http::write_response(&job.stream, 200, reply.to_string().as_bytes());
        drop(job.stream);
        drop(emulator);
        drop(other);
        // what the caches keep of files that changed since (a new bundle version, an edited module)
        #[cfg(feature = "bundles")]
        crate::project::model_assets::forget_stale_bundles();
        modules::forget_stale_modules();
        // (the heap gives the engine's pages back while the service waits)
        unsafe extern "C" {
            fn mi_collect(force: bool);
        }
        unsafe { mi_collect(true) };
        // and the thread pool's, where the project listing ran
        rayon::broadcast(|_| unsafe { mi_collect(true) });
        if let Ok(mut time) = service.last_job.lock() {
            *time = Instant::now();
        }
        if next.is_none() {
            if let Ok(mut slots) = service.slots.lock() {
                slots[index] = Slot::default();
            }
        }
        service.running.fetch_sub(1, Ordering::SeqCst);
        if abandoned {
            &service.abandoned
        } else if failed {
            &service.failed
        } else {
            &service.done
        }
        .fetch_add(1, Ordering::SeqCst);
        if let (Some(ahead), Ok(mut recent)) = (&next, service.recent.lock()) {
            *recent = Some(ahead.clone());
        }
        // (others may take the jobs this worker held, or prepare the recording ahead too: take)
        service.queued.notify_all();
        if let Some(ahead) = next {
            parked = prepare_next(service, index, &ahead);
            if parked.is_some() {
                took = Some(replied.elapsed());
            }
        }
    }
}

/// Whether a run prepared for `key` would likely be used: another job it serves started within
/// [`REPEATS_WITHIN`] (not the one that started at `own`), or one waits in the queue.
fn repeats(service: &Service, key: &prepare::Key, own: Instant) -> bool {
    let recent = service.starts.lock().is_ok_and(|starts| starts.iter().any(|(keys, at)| *at != own && at.elapsed() <= REPEATS_WITHIN && key.serves(keys)));
    recent || service.queue.lock().is_ok_and(|queue| queue.iter().any(|job| job.key.as_ref().is_some_and(|keys| key.serves(keys))))
}

/// The next job for worker `index`: the one for the recording it holds prepared, else the first one
/// no other worker holds prepared while it is idle, or is preparing to be ready within
/// [`WAIT_FOR_PREPARED`] (that worker takes it). A worker holding a prepared recording leaves other
/// jobs to a worker that is idle and holds none. With no job for it, a worker holding nothing
/// prepares the most recent recording while fewer than [`HOLDERS`] workers hold or prepare it (`took`:
/// how long its last preparation took).
fn take(service: &Service, index: usize, took: Option<Duration>) -> Option<Work> {
    let mut queue = service.queue.lock().ok()?;
    loop {
        let picked = {
            let mut slots = service.slots.lock().ok()?;
            let mine = slots[index].key.clone();
            let soon = Instant::now() + WAIT_FOR_PREPARED;
            let serves = |slot: &Slot, job: &Job| matches!((&slot.key, &job.key), (Some(key), Some(keys)) if key.serves(keys));
            let held_elsewhere = |job: &Job| slots.iter().enumerate().any(|(other, slot)| other != index && !slot.running && serves(slot, job) && (!slot.preparing || slot.ready_by.is_some_and(|ready| ready <= soon)));
            let free_elsewhere = slots.iter().enumerate().any(|(other, slot)| other != index && !slot.running && !slot.preparing && slot.key.is_none());
            let position = queue
                .iter()
                .position(|job| serves(&slots[index], job))
                .or_else(|| if mine.is_some() && free_elsewhere { None } else { queue.iter().position(|job| !held_elsewhere(job)) });
            if position.is_some() {
                slots[index].running = true;
            }
            position
        };
        if let Some(job) = picked.and_then(|position| queue.remove(position)) {
            if !queue.is_empty() {
                service.queued.notify_all();
            }
            return Some(Work::Job(job));
        }
        if let Some(work) = ahead(service, index, took) {
            return Some(work);
        }
        queue = service.queued.wait(queue).ok()?;
    }
}

/// The most recent recording for worker `index` to prepare ahead (its slot then says so), when it is idle
/// and holds nothing, fewer than [`HOLDERS`] workers hold or prepare that recording, the service is not
/// ending and memory is not short. Called with the queue locked.
fn ahead(service: &Service, index: usize, took: Option<Duration>) -> Option<Work> {
    if service.shutting_down.load(Ordering::SeqCst) {
        return None;
    }
    let mut slots = service.slots.lock().ok()?;
    let mine = &slots[index];
    if mine.key.is_some() || mine.running || mine.preparing {
        return None;
    }
    let ahead = service.recent.lock().ok()?.clone()?;
    let key = ahead.key();
    if slots.iter().filter(|slot| slot.key.as_ref() == Some(&key)).count() >= HOLDERS || memory().is_some_and(|(_, available)| available < JOB_MEMORY) {
        return None;
    }
    slots[index] = Slot { key: Some(key), running: false, preparing: true, ready_by: Some(Instant::now() + took.unwrap_or(FIRST_PREPARATION)) };
    Some(Work::Prepare(ahead))
}

/// The recording (or run) `ahead` names, prepared again up to its harness's checkpoint for the next job
/// like it (worker `index`'s slot says so already). None when the service is ending, memory is short, or
/// other jobs wait with no other worker free for them.
fn prepare_next(service: &Service, index: usize, ahead: &Ahead) -> Option<prepare::Prepared> {
    let key = ahead.key();
    let others_wait = || {
        let Ok(queue) = service.queue.lock() else { return true };
        let Ok(slots) = service.slots.lock() else { return true };
        let free = slots.iter().enumerate().any(|(other, slot)| other != index && !slot.running && !slot.preparing);
        !free && queue.iter().any(|job| !job.key.as_ref().is_some_and(|keys| key.serves(keys)))
    };
    if service.shutting_down.load(Ordering::SeqCst) || memory().is_some_and(|(_, available)| available < JOB_MEMORY) || others_wait() {
        if let Ok(mut slots) = service.slots.lock() {
            slots[index] = Slot::default();
        }
        service.queued.notify_all();
        return None;
    }
    service.preparing.fetch_add(1, Ordering::SeqCst);
    let made = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare::prepare(&ahead.args, &launch::Host::default(), ahead.record))) {
        Ok(Ok(prepared)) => Some(prepared),
        Ok(Err(why)) => {
            if let Ok(mut unmade) = service.prepared.unmade.lock() {
                *unmade = Some(why);
            }
            None
        }
        Err(_) => None,
    };
    if made.is_some() {
        service.prepared.made.fetch_add(1, Ordering::SeqCst);
    } else if let Ok(mut recent) = service.recent.lock() {
        // (what failed would fail again for the idle workers: the next job like it tries once more)
        recent.take_if(|recent| recent.key() == key);
    }
    if let Ok(mut slots) = service.slots.lock() {
        slots[index] = Slot { key: made.as_ref().map(|prepared| prepared.key().clone()), running: false, preparing: false, ready_by: None };
    }
    service.preparing.fetch_sub(1, Ordering::SeqCst);
    service.queued.notify_all();
    made
}

/// One `ree record` / `ree run` command as [`command::cli`] runs it, its output captured: the
/// reply, the emulator to drop once the reply is sent, and the command whose harness has a checkpoint
/// (to prepare the next one). A recording or run `parked` holds prepared is used when it is for this
/// command and these arguments and still current; it is dropped otherwise.
fn run_job(name: &str, args: &[String], parked: &mut Option<prepare::Prepared>, counts: &PreparedCounts) -> (serde_json::Value, Option<crate::Emulator>, Option<Ahead>) {
    let started = Instant::now();
    let record = name == "record";
    let args = launch::split_equals(args);
    crate::files::forget_unchecked();
    crate::output::capture();
    if name == "warm" {
        let warmed = command::warm(&args, &launch::Host::default());
        let stderr = crate::output::take_captured();
        return match warmed {
            Ok(summary) => (json!({ "code": 0, "stdout": format!("{summary}
"), "stderr": stderr }), None, None),
            Err(error) => (json!({ "code": 1, "stdout": "", "stderr": format!("{stderr}ree serve --warm: {error}
") }), None, None),
        };
    }
    if args.iter().any(|arg| arg == "--profile") {
        crate::timing::begin(started);
    }
    let host = launch::Host::default();
    let prepared = match parked.take() {
        Some(prepared) if prepared.key().serves(&prepare::Keys::of(&args, record)) => Some(prepared),
        // (one engine at a time on a worker: a recording prepared for other arguments goes first)
        Some(other) => {
            crate::timing::time("prepared: dropped (other arguments)", || drop(other));
            None
        }
        None => None,
    };
    let used = prepared.and_then(|prepared| match prepared.run(&args, &host, started) {
        Ok(result) => {
            crate::timing::tally("prepared: used", 1);
            counts.used.fetch_add(1, Ordering::SeqCst);
            Some(result)
        }
        Err(why) => {
            // (the job starts over: what the stale run wrote to the capture and the profile goes)
            let _ = crate::output::take_captured();
            crate::output::capture();
            if crate::timing::enabled() {
                crate::timing::begin(started);
            }
            crate::timing::tally(format!("prepared: stale ({why})"), 1);
            counts.stale.fetch_add(1, Ordering::SeqCst);
            None
        }
    });
    let result = match used {
        Some(result) => result,
        None => command::parse(&args, record).and_then(|parsed| command::execute(&parsed, &host, record)),
    };
    let next = match &result {
        // (a run that read what cannot be checked before its checkpoint would be prepared in vain)
        Ok((done, _)) if done.checkpoint && done.unpreparable.is_some() => {
            counts.skipped.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut unmade) = counts.unmade.lock() {
                *unmade = done.unpreparable.clone();
            }
            None
        }
        Ok((done, _)) if done.checkpoint => Some(Ahead { args: args.clone(), split: done.split, framed: done.framed, record }),
        _ => None,
    };
    let mut stderr = crate::output::take_captured();
    let profile = crate::timing::finish().map(|profile| format!("{profile}\n")).unwrap_or_default();
    match result {
        Ok((done, emulator)) => {
            stderr.push_str(&done.line);
            stderr.push('\n');
            stderr.push_str(&profile);
            let stdout = format!("{}\n", serde_json::to_string(&done.summary).unwrap_or_default());
            (json!({ "code": done.code, "stdout": stdout, "stderr": stderr }), Some(emulator), next)
        }
        Err(error) => {
            stderr.push_str(&profile);
            stderr.push_str(&format!("{} {name}: {error}\n", host.name));
            (json!({ "code": 1, "stdout": "", "stderr": stderr }), None, None)
        }
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::files::write_atomic(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job as a worker holding nothing prepared runs it: its reply and emulator.
    fn job(name: &str, args: &[String]) -> (serde_json::Value, Option<crate::Emulator>) {
        let (reply, emulator, _) = run_job(name, args, &mut None, &PreparedCounts::default());
        (reply, emulator)
    }

    /// `--help` asks for the usage, and a word `ree serve` does not take is refused, instead of starting a service.
    #[test]
    fn help_and_unknown_options_do_not_start_a_service() {
        let check = |words: &[&str]| check(&words.iter().map(|word| word.to_string()).collect::<Vec<_>>());
        assert_eq!(check(&["--help"]), Ok(true));
        assert_eq!(check(&["--idle", "5", "-h"]), Ok(true));
        assert_eq!(check(&["--idle", "5", "--workers", "2", "--state-dir", "d"]), Ok(false));
        assert_eq!(check(&["--status"]), Ok(false));
        assert_eq!(check(&["--bogus"]), Err("unknown option --bogus".into()));
        assert_eq!(check(&["status"]), Err("unknown option status".into()));
        assert_eq!(check(&["--idle"]), Err("--idle needs a value".into()));
    }

    /// A service's jobs print what the command prints, one after another on one thread, each in
    /// an engine of its own (nothing a job made is seen by the next).
    #[test]
    fn jobs_run_like_the_command_one_after_another() {
        let root = std::env::temp_dir().join(format!("ree-serve-test-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let harness = root.join("Grow.luau");
        std::fs::write(
            &harness,
            r#"return function(Context)
                assert(workspace:FindFirstChild("Grown") == nil, "a fresh world")
                _G.Runs = (_G.Runs or 0) + 1
                local Part = Instance.new("Part")
                Part.Name = "Grown"
                Part.Anchored = true
                Part.Parent = workspace
                game:GetService("TweenService"):Create(Part, TweenInfo.new(0.25), {Size = Vector3.new(4, 4, 4)}):Play()
                print("runs", _G.Runs, Context.Scenario)
            end"#,
        )
        .unwrap();
        #[cfg(feature = "bundles")]
        crate::project::model_assets::keep_bundles();
        let mut replies = Vec::new();
        for index in 0..2 {
            let out = root.join(format!("out{index}"));
            let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &out.to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.5", "--context", r#"{"Scenario":"grow"}"#]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let (reply, emulator) = job("record", &args);
            assert_eq!(reply["code"], 0, "{reply}");
            drop(emulator);
            let mut summary: serde_json::Value = serde_json::from_str(reply["stdout"].as_str().unwrap().trim()).unwrap();
            summary["out"] = serde_json::Value::Null;
            replies.push((summary, std::fs::read_to_string(out.join("playback.json")).unwrap(), std::fs::read_to_string(out.join("events.json")).unwrap()));
            assert!(reply["stderr"].as_str().unwrap().starts_with("ree: record ok; 31 frames;"), "{reply}");
        }
        assert_eq!(replies[0], replies[1]);
        assert!(replies[0].2.contains("runs 1 grow"), "{}", replies[0].2);
        assert!(replies[0].1.contains("Grown"));
        let (reply, _) = job("record", &["--harness".to_string(), root.join("Missing.luau").to_string_lossy().into_owned(), "--out".into(), root.join("x").to_string_lossy().into_owned()]);
        assert_eq!(reply["code"], 1);
        assert!(reply["stderr"].as_str().unwrap().contains("ree record:"), "{reply}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A warm-up job reads in what a recording of the project reads and records nothing: its reply is a
    /// summary, and the recording after it is what it is without it.
    #[test]
    fn a_warm_up_reads_ahead_and_changes_no_recording() {
        let root = std::env::temp_dir().join(format!("ree-serve-warm-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let modules = root.join("game").join("ReplicatedStorage");
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("Value.luau"), "return 41 + 1").unwrap();
        let harness = root.join("Use.luau");
        std::fs::write(&harness, r#"return function() print(require(game:GetService("ReplicatedStorage").Value)) end"#).unwrap();
        let roots = root.join("game").to_string_lossy().into_owned();
        let record = |out: &str| {
            let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &root.join(out).to_string_lossy(), "--roots", &roots, "--seconds", "0.2"].iter().map(|s| s.to_string()).collect();
            let (reply, emulator) = job("record", &args);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            (std::fs::read(root.join(out).join("playback.json")).unwrap(), std::fs::read_to_string(root.join(out).join("events.json")).unwrap())
        };
        let before = record("before");
        let (reply, emulator) = job("warm", &["--roots".to_string(), roots.clone()]);
        assert!(emulator.is_none());
        assert_eq!(reply["code"], 0, "{reply}");
        let summary: serde_json::Value = serde_json::from_str(reply["stdout"].as_str().unwrap().trim()).unwrap();
        assert_eq!(summary["warm"], true, "{summary}");
        assert_eq!(record("after"), before);
        assert!(before.1.contains("42"), "{}", before.1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A recording prepared up to its harness's checkpoint records what the recording records without
    /// it. It is used by a job with its arguments; it is stale once a module it read changed (the job
    /// records the change); a module read after the checkpoint is read as it is when the job comes; a
    /// job with other arguments drops it. The checkpoint does nothing a second time, on another thread
    /// or after a yield.
    #[test]
    fn a_prepared_recording_records_what_one_records() {
        modules::reuse_compiled_modules();
        let root = std::env::temp_dir().join(format!("ree-serve-prepared-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let storage = root.join("game").join("ReplicatedStorage");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::write(storage.join("Before.luau"), "return 'before 1'").unwrap();
        std::fs::write(storage.join("After.luau"), "return 'after 1'").unwrap();
        let harness = root.join("Prep.luau");
        std::fs::write(
            &harness,
            r#"return function(Context)
                local Storage = game:GetService("ReplicatedStorage")
                local Before = require(Storage.Before)
                local Part = Instance.new("Part")
                Part.Name = "Made " .. Before
                Part.Anchored = true
                Part.Parent = workspace
                task.defer(print, "deferred before the checkpoint")
                print("before", Before, Context.Scenario)
                Context.Checkpoint()
                Context.Checkpoint()
                print("after", require(Storage.After))
                task.spawn(function()
                    Context.Checkpoint()
                    print("spawned")
                end)
                task.wait(0.1)
                Context.Checkpoint()
                Part.Size = Vector3.new(2, 2, 2)
            end"#,
        )
        .unwrap();
        let roots = root.join("game").to_string_lossy().into_owned();
        let arguments = |out: &str, scenario: &str| -> Vec<String> {
            ["--harness", &harness.to_string_lossy(), "--out", &root.join(out).to_string_lossy(), "--roots", &roots, "--seconds", "0.3", "--context", &format!(r#"{{"Scenario":"{scenario}"}}"#)].iter().map(|s| s.to_string()).collect()
        };
        let counts = PreparedCounts::default();
        let record = |out: &str, scenario: &str, parked: &mut Option<prepare::Prepared>| {
            let (reply, emulator, next) = run_job("record", &arguments(out, scenario), parked, &counts);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            assert!(next.is_some(), "the harness reached its checkpoint");
            let mut summary: serde_json::Value = serde_json::from_str(reply["stdout"].as_str().unwrap().trim()).unwrap();
            summary["out"] = serde_json::Value::Null;
            (std::fs::read(root.join(out).join("playback.json")).unwrap(), std::fs::read_to_string(root.join(out).join("events.json")).unwrap(), summary)
        };
        let prepared = |out: &str| Some(prepare::prepare(&arguments(out, "x"), &launch::Host::default(), true).unwrap());
        let used = |counts: &PreparedCounts| (counts.used.load(Ordering::SeqCst), counts.stale.load(Ordering::SeqCst));

        let alone = record("alone", "x", &mut None);
        assert!(alone.1.contains("before 1") && alone.1.contains("after 1") && alone.1.contains("spawned") && alone.1.contains("deferred before the checkpoint"), "{}", alone.1);
        assert_eq!(record("ahead", "x", &mut prepared("p")), alone);
        assert_eq!(used(&counts), (1, 0));

        let mut parked = prepared("p");
        std::fs::write(storage.join("Before.luau"), "return 'before two'").unwrap();
        let changed = record("changed", "x", &mut parked);
        assert_eq!(used(&counts), (1, 1));
        assert!(changed.1.contains("before two"), "{}", changed.1);

        let mut parked = prepared("p");
        std::fs::write(storage.join("After.luau"), "return 'after two'").unwrap();
        let later = record("later", "x", &mut parked);
        assert_eq!(used(&counts), (2, 1));
        assert!(later.1.contains("after two"), "{}", later.1);
        assert_eq!(later, record("later alone", "x", &mut None));

        let mut parked = prepared("p");
        let other = record("other", "y", &mut parked);
        assert!(parked.is_none());
        assert_eq!(used(&counts), (2, 1));
        assert!(other.1.contains("before two y"), "{}", other.1);

        // a checkpoint the harness cannot yield at (in table.sort's comparator) parks nothing
        let sorted = root.join("Sorted.luau");
        std::fs::write(&sorted, r#"return function(Context)
                local Values = { 3, 1, 2 }
                table.sort(Values, function(A, B)
                    Context.Checkpoint()
                    return A < B
                end)
                print("sorted", table.concat(Values, ","))
            end"#)
        .unwrap();
        let sorted_args: Vec<String> = ["--harness", &sorted.to_string_lossy(), "--out", &root.join("sorted").to_string_lossy(), "--roots", &roots, "--seconds", "0.1"].iter().map(|s| s.to_string()).collect();
        assert!(prepare::prepare(&sorted_args, &launch::Host::default(), true).is_err());
        let (reply, emulator, _) = run_job("record", &sorted_args, &mut None, &counts);
        drop(emulator);
        assert_eq!(reply["code"], 0, "{reply}");
        assert!(std::fs::read_to_string(root.join("sorted").join("events.json")).unwrap().contains("sorted 1,2,3"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A harness with Prepare: its Prepare gets a Context without the command's fields, Seconds and Fps,
    /// its Run gets them, and a run prepared up to Prepare's end serves jobs with other `--context`,
    /// `--seconds` and `--fps` (what each mounts with Context.Mount too), recording what each records
    /// alone. Other engine options are another key; a Prepare that yields parks nothing.
    #[test]
    fn a_prepared_harness_with_prepare_serves_every_context() {
        modules::reuse_compiled_modules();
        let root = std::env::temp_dir().join(format!("ree-serve-split-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let storage = root.join("game").join("ReplicatedStorage");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::write(storage.join("Setup.luau"), "return 'setup 1'").unwrap();
        for (name, value) in [("PropA", "'prop a'"), ("PropB", "'prop b'")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("Value.luau"), format!("return {value}")).unwrap();
        }
        let harness = root.join("Split.luau");
        std::fs::write(
            &harness,
            r#"local Setup
            return {
                Prepare = function(Context)
                    assert(Context.Scenario == nil and Context.Seconds == nil and Context.Fps == nil, "Prepare sees no fields")
                    assert(type(Context.StandIn) == "function" and type(Context.Mount) == "function", "Prepare has StandIn and Mount")
                    Setup = require(game:GetService("ReplicatedStorage").Setup)
                    local Part = Instance.new("Part")
                    Part.Name = "Stage"
                    Part.Anchored = true
                    Part.Parent = workspace
                    task.defer(print, "deferred in Prepare")
                end,
                Run = function(Context)
                    print("run", Setup, Context.Scenario, Context.Seconds, Context.Fps)
                    local Prop = Context.Mount("ReplicatedStorage/Sim/Prop", Context.Prop)
                    print("mounted", Prop:GetFullName(), require(Prop.Value))
                    print("relative", pcall(Context.Mount, "Workspace/X", "relative/path"))
                    local Part = workspace.Stage
                    game:GetService("RunService").Heartbeat:Connect(function(Step)
                        Part.CFrame *= CFrame.Angles(0, Step * Context.Speed, 0)
                    end)
                end,
            }"#,
        )
        .unwrap();
        let roots = root.join("game").to_string_lossy().into_owned();
        let arguments = |out: &str, scenario: &str, seconds: &str, fps: &str, prop: &str, speed: u32| -> Vec<String> {
            let prop = root.join(prop).to_string_lossy().replace('\\', "\\\\");
            ["--harness", &harness.to_string_lossy(), "--out", &root.join(out).to_string_lossy(), "--roots", &roots, "--seconds", seconds, "--fps", fps, "--context", &format!(r#"{{"Scenario":"{scenario}","Prop":"{prop}","Speed":{speed}}}"#)]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        let counts = PreparedCounts::default();
        let record = |args: &[String], parked: &mut Option<prepare::Prepared>| {
            let (reply, emulator, next) = run_job("record", args, parked, &counts);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            assert_eq!(next.map(|ahead| ahead.split), Some(true), "the harness reached its checkpoint, after Prepare");
            let out = std::path::PathBuf::from(&args[args.iter().position(|a| a == "--out").unwrap() + 1]);
            let mut summary: serde_json::Value = serde_json::from_str(reply["stdout"].as_str().unwrap().trim()).unwrap();
            summary["out"] = serde_json::Value::Null;
            (std::fs::read(out.join("playback.json")).unwrap(), std::fs::read_to_string(out.join("events.json")).unwrap(), summary)
        };
        let ahead = arguments("p", "x", "0.3", "60", "PropA", 1);
        let prepared = || Some(prepare::prepare(&ahead, &launch::Host::default(), true).unwrap());
        let used = |counts: &PreparedCounts| (counts.used.load(Ordering::SeqCst), counts.stale.load(Ordering::SeqCst));

        let job = arguments("y", "y", "0.5", "30", "PropB", 2);
        let alone = record(&job, &mut None);
        assert!(alone.1.contains("run setup 1 y 0.5 30") && alone.1.contains("mounted ReplicatedStorage.Sim.Prop prop b") && alone.1.contains("deferred in Prepare"), "{}", alone.1);
        assert!(alone.1.contains("relative false") && alone.1.contains("not an absolute path"), "{}", alone.1);
        assert_eq!(alone.2["fps"], 30.0, "{}", alone.2);
        let mut parked = prepared();
        assert!(parked.as_ref().unwrap().key().split);
        assert_eq!(record(&arguments("y2", "y", "0.5", "30", "PropB", 2), &mut parked), alone);
        assert_eq!(used(&counts), (1, 0));
        let again = arguments("x", "x", "0.3", "60", "PropA", 1);
        assert_eq!(record(&again, &mut prepared()), record(&arguments("x alone", "x", "0.3", "60", "PropA", 1), &mut None));
        assert_eq!(used(&counts), (2, 0));

        // other engine options are another key
        let key = prepared().unwrap().key().clone();
        assert!(key.serves(&prepare::Keys::of(&job, true)));
        assert!(!key.serves(&prepare::Keys::of(&job, false)), "a recording serves no run");
        let mut other = job.clone();
        other.extend(["--player".to_string(), "Someone".to_string()]);
        assert!(!key.serves(&prepare::Keys::of(&other, true)));
        let mut server = job.clone();
        server.extend(["--run-context".to_string(), "server".to_string()]);
        assert!(!key.serves(&prepare::Keys::of(&server, true)), "a client's prepared run serves no server job");

        // a Prepare that yields parks nothing, and runs as before
        let waits = root.join("Waits.luau");
        std::fs::write(&waits, r#"return { Prepare = function(Context) task.wait(0.05) end, Run = function(Context) print("ran", Context.Scenario) end }"#).unwrap();
        let wait_args: Vec<String> = ["--harness", &waits.to_string_lossy(), "--out", &root.join("waits").to_string_lossy(), "--roots", &roots, "--seconds", "0.2", "--context", r#"{"Scenario":"w"}"#].iter().map(|s| s.to_string()).collect();
        assert!(prepare::prepare(&wait_args, &launch::Host::default(), true).is_err());
        let (reply, emulator, next) = run_job("record", &wait_args, &mut None, &counts);
        drop(emulator);
        assert_eq!(reply["code"], 0, "{reply}");
        assert!(next.is_none());
        assert!(std::fs::read_to_string(root.join("waits").join("events.json")).unwrap().contains("ran w"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `ree run` job is prepared as a recording is (a prepared run serves runs of its arguments, never a
    /// recording), in a server run context too, and a line `@done` ends its frames: the job prints what a
    /// run from the start prints, with the frames it ran.
    #[test]
    fn a_prepared_server_run_ends_at_done_as_one_from_the_start() {
        modules::reuse_compiled_modules();
        let root = std::env::temp_dir().join(format!("ree-serve-done-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let harness = root.join("Done.luau");
        std::fs::write(
            &harness,
            r#"local Store
            return {
                Prepare = function(Context)
                    Store = game:GetService("DataStoreService"):GetDataStore("Runs")
                end,
                Run = function(Context)
                    assert(game:GetService("RunService"):IsServer() and game:GetService("Players").LocalPlayer == nil)
                    print("before", Store:GetAsync("Count"))
                    Store:SetAsync("Count", Context.Count)
                    task.wait(0.1)
                    print("@done")
                    task.wait(1)
                    print("never")
                end,
            }"#,
        )
        .unwrap();
        let roots = root.join("game").to_string_lossy().into_owned();
        let arguments = |count: u32| -> Vec<String> {
            ["--harness", &harness.to_string_lossy(), "--roots", &roots, "--seconds", "5", "--run-context", "server", "--context", &format!(r#"{{"Count":{count}}}"#)].iter().map(|s| s.to_string()).collect()
        };
        let counts = PreparedCounts::default();
        let run = |args: &[String], parked: &mut Option<prepare::Prepared>| {
            let (reply, emulator, next) = run_job("run", args, parked, &counts);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            let next = next.expect("the harness reached its checkpoint");
            assert!(next.split && !next.record);
            serde_json::from_str::<serde_json::Value>(reply["stdout"].as_str().unwrap().trim()).unwrap()
        };
        let alone = run(&arguments(1), &mut None);
        assert_eq!((alone["done"].as_bool(), alone["runContext"].as_str(), alone["status"].as_str()), (Some(true), Some("server"), Some("yielded")), "{alone}");
        assert!(alone["frames"].as_u64().unwrap() < 20, "{alone}");
        let messages: Vec<&str> = alone["output"].as_array().unwrap().iter().map(|line| line["message"].as_str().unwrap()).collect();
        assert_eq!(messages, ["before nil", "@done"]);
        let mut parked = Some(prepare::prepare(&arguments(7), &launch::Host::default(), false).unwrap());
        assert!(!parked.as_ref().unwrap().key().serves(&prepare::Keys::of(&arguments(1), true)), "a prepared run serves no recording");
        assert_eq!(run(&arguments(1), &mut parked), alone);
        assert_eq!(counts.used.load(Ordering::SeqCst), 1);
        // a recording ends there too, with the frames it sampled
        let mut recording = arguments(1);
        recording.extend(["--out".to_string(), root.join("out").to_string_lossy().into_owned()]);
        let (reply, emulator, _) = run_job("record", &recording, &mut None, &counts);
        drop(emulator);
        let summary: serde_json::Value = serde_json::from_str(reply["stdout"].as_str().unwrap().trim()).unwrap();
        assert_eq!((summary["done"].as_bool(), summary["frames"].as_u64()), (Some(true), alone["frames"].as_u64()), "{summary}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A Prepare that runs frames (a server's boot) and ends at Context.FrameCheckpoint parks between
    /// frames: its run prepared ahead serves a job with other `--context` and `--seconds` but the same
    /// `--fps`, printing what the job prints alone, frames and clocks included; Context.ReseedRandoms
    /// restarts the Randoms the Prepare made from the job's seed.
    #[test]
    fn a_prepare_that_runs_frames_parks_between_frames() {
        modules::reuse_compiled_modules();
        let root = std::env::temp_dir().join(format!("ree-serve-frames-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join("game")).unwrap();
        let harness = root.join("Frames.luau");
        std::fs::write(
            &harness,
            r#"local Beats, Unseeded, Seeded = 0, nil, nil
            return {
                Prepare = function(Context)
                    game:GetService("RunService").Heartbeat:Connect(function()
                        Beats += 1
                    end)
                    Unseeded, Seeded = Random.new(), Random.new(5)
                    task.wait(0.2)
                    task.defer(print, "booted", Beats)
                    task.wait(0.1)
                    print("checkpoint", pcall(Context.FrameCheckpoint), Beats)
                    print("after", Beats, Context.Seed)
                end,
                Run = function(Context)
                    print("reseeded", Context.ReseedRandoms(Context.Seed) == 2)
                    print("draws", Context.Seed, Unseeded:NextInteger(1, 1e6), Seeded:NextInteger(1, 1e6), Seeded:Clone():NextInteger(1, 1e6))
                    print("run", Context.Seed, Context.Seconds, Context.Fps, Beats, os.clock())
                    task.wait(0.25)
                    print("waited", Beats, os.clock(), workspace:GetServerTimeNow())
                    print("@done")
                end,
            }"#,
        )
        .unwrap();
        let roots = root.join("game").to_string_lossy().into_owned();
        let arguments = |seed: u32, seconds: &str, fps: &str| -> Vec<String> {
            ["--harness", &harness.to_string_lossy(), "--roots", &roots, "--seconds", seconds, "--fps", fps, "--run-context", "server", "--context", &format!(r#"{{"Seed":{seed}}}"#)].iter().map(|s| s.to_string()).collect()
        };
        let counts = PreparedCounts::default();
        let job = |args: &[String], parked: &mut Option<prepare::Prepared>| {
            let (reply, emulator, next) = run_job("run", args, parked, &counts);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            let summary = serde_json::from_str::<serde_json::Value>(reply["stdout"].as_str().unwrap().trim()).unwrap();
            let lines: Vec<String> = summary["output"].as_array().unwrap().iter().map(|line| format!("{} {}", line["time"], line["message"].as_str().unwrap())).collect();
            ((lines, summary["frames"].clone(), summary["status"].clone()), next)
        };
        let run = |args: &[String], parked: &mut Option<prepare::Prepared>| {
            let (ran, next) = job(args, parked);
            let next = next.expect("the harness reached its checkpoint");
            assert!(next.split && next.framed && !next.record);
            ran
        };
        let word = |lines: &[String], name: &str| -> Vec<String> {
            lines.iter().find(|line| line.split_whitespace().nth(1) == Some(name)).map(|line| line.split_whitespace().skip(2).map(str::to_string).collect()).unwrap_or_default()
        };
        let alone = run(&arguments(1, "3", "60"), &mut None);
        let at = word(&alone.0, "checkpoint");
        assert_eq!(at.first().map(String::as_str), Some("true"), "{:?}", alone.0);
        assert_eq!(word(&alone.0, "after"), [at[1].clone(), "nil".into()], "resumed before the next frame; the Prepare saw no fields: {:?}", alone.0);
        assert_eq!(word(&alone.0, "reseeded"), ["true"], "{:?}", alone.0);
        let again = run(&arguments(1, "3", "60"), &mut None);
        assert_eq!(again, alone);
        let other = run(&arguments(2, "3", "60"), &mut None);
        assert_ne!(word(&other.0, "draws")[1..], word(&alone.0, "draws")[1..], "another seed draws otherwise");

        let mut parked = Some(prepare::prepare(&arguments(7, "2", "60"), &launch::Host::default(), false).unwrap());
        let key = parked.as_ref().unwrap().key().clone();
        assert!(key.framed && key.serves(&prepare::Keys::of(&arguments(1, "3", "60"), false)));
        assert!(!key.serves(&prepare::Keys::of(&arguments(1, "3", "30"), false)), "frames ran at its --fps");
        assert_eq!(run(&arguments(1, "3", "60"), &mut parked), alone);
        assert_eq!(counts.used.load(Ordering::SeqCst), 1);
        let mut parked = Some(prepare::prepare(&arguments(7, "2", "60"), &launch::Host::default(), false).unwrap());
        assert_eq!(run(&arguments(2, "3", "60"), &mut parked), other);
        assert_eq!(counts.used.load(Ordering::SeqCst), 2);
        let mut parked = Some(prepare::prepare(&arguments(7, "2", "60"), &launch::Host::default(), false).unwrap());
        let short = job(&arguments(1, "0.1", "60"), &mut parked);
        assert_eq!((counts.used.load(Ordering::SeqCst), counts.stale.load(Ordering::SeqCst)), (2, 1), "a job that ends before the checkpoint starts over");
        assert!(short.1.is_none() && short.0.0.is_empty(), "{:?}", short.0);
        assert_eq!(short.0, job(&arguments(1, "0.1", "60"), &mut None).0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An idle worker holding nothing prepares the most recent recording ahead until [`HOLDERS`] workers
    /// hold or prepare it; a job waits for a worker preparing its recording only when that is expected
    /// ready within [`WAIT_FOR_PREPARED`].
    fn idle_service(workers: usize) -> Service {
        Service {
            queue: Mutex::new(VecDeque::new()),
            queued: Condvar::new(),
            slots: Mutex::new(vec![Slot::default(); workers]),
            preparing: AtomicU64::new(0),
            prepared: PreparedCounts::default(),
            started: Instant::now(),
            last_job: Mutex::new(Instant::now()),
            starts: Mutex::new(VecDeque::new()),
            running: AtomicU64::new(0),
            done: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            abandoned: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            recent: Mutex::new(None),
            build: String::new(),
            env: String::new(),
            workers,
        }
    }

    /// A client waiting for its reply has an open connection; one that went away has a closed one
    #[test]
    fn a_client_that_went_away_is_seen() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let server = listener.accept().unwrap().0;
        client.write_all(b"job!").unwrap();
        let mut request = [0u8; 4];
        (&server).read_exact(&mut request).unwrap();
        assert!(!http::closed(&server), "a client waiting for its reply");
        assert!(!http::closed(&server), "asked again");
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !http::closed(&server) {
            assert!(Instant::now() < deadline, "the closed connection was never seen");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A job whose client has gone stops at its next frame (exit 125, no files); one whose client waits runs to its end
    #[test]
    fn a_job_stops_once_its_caller_has_gone() {
        let root = std::env::temp_dir().join(format!("ree-serve-caller-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let harness = root.join("Long.luau");
        std::fs::write(
            &harness,
            r#"return function(Context)
                local Part = Instance.new("Part")
                Part.Anchored = true
                Part.Parent = workspace
                game:GetService("RunService").Heartbeat:Connect(function(Step)
                    Part.CFrame *= CFrame.Angles(0, Step, 0)
                end)
                Context.Checkpoint()
            end"#,
        )
        .unwrap();
        let args = |seconds: &str, out: &str| -> Vec<String> {
            ["--harness", &harness.to_string_lossy(), "--out", &root.join(out).to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", seconds].iter().map(|s| s.to_string()).collect()
        };
        let summary = |reply: &serde_json::Value| serde_json::from_str::<serde_json::Value>(reply["stdout"].as_str().unwrap().trim()).unwrap();
        // (an hour at 60 fps: 216,000 frames, which the job does not get far into)
        let started = Instant::now();
        let counts = PreparedCounts::default();
        let (reply, emulator, next) = crate::watchdog::for_caller(|| true, || run_job("record", &args("3600", "gone"), &mut None, &counts));
        drop(emulator);
        assert_eq!(reply["code"], crate::watchdog::EXIT_ORPHANED, "{reply}");
        let gone = summary(&reply);
        assert_eq!(gone["status"], "error", "{gone}");
        assert!(gone["failure"].as_str().unwrap().contains("caller has gone"), "{gone}");
        assert!(gone["frames"].as_u64().unwrap() <= 1, "{gone}");
        assert!(!root.join("gone").join("playback.json").exists(), "nobody reads its files: none are written");
        assert!(started.elapsed() < Duration::from_secs(60), "stopped at once, not after the schedule");
        assert!(next.is_some(), "the run reached its checkpoint (the worker skips preparing after an abandoned job)");
        let (reply, emulator, _) = crate::watchdog::for_caller(|| false, || run_job("record", &args("0.5", "waits"), &mut None, &counts));
        drop(emulator);
        assert_eq!(reply["code"], 0, "{reply}");
        let waited = summary(&reply);
        assert_eq!(waited["status"], "ok", "{waited}");
        assert!(waited["frames"].as_u64().unwrap() >= 30, "{waited}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_worker_prepares_the_recent_recording_ahead() {
        let service = idle_service(4);
        assert!(ahead(&service, 1, None).is_none(), "nothing recorded yet");
        let args: Vec<String> = ["--harness", "h.luau", "--context", r#"{"Unit":"A"}"#].iter().map(|s| s.to_string()).collect();
        *service.recent.lock().unwrap() = Some(Ahead { args: args.clone(), split: true, framed: false, record: true });
        let key = prepare::Key::of(&args, true, true);
        service.slots.lock().unwrap()[0] = Slot { key: Some(key.clone()), running: false, preparing: true, ready_by: Some(Instant::now() + Duration::from_secs(1)) };
        assert!(matches!(ahead(&service, 1, None), Some(Work::Prepare(Ahead { split: true, record: true, .. }))));
        assert!(service.slots.lock().unwrap()[1].preparing);
        assert!(ahead(&service, 2, None).is_none(), "two hold or prepare it already");
        // a worker holding another recording does not prepare this one
        service.slots.lock().unwrap()[1] = Slot::default();
        service.slots.lock().unwrap()[3] = Slot { key: Some(prepare::Key { split: false, framed: false, record: true, args: vec!["other".into()] }), ..Slot::default() };
        assert!(ahead(&service, 3, None).is_none());
        // a job like it is left to worker 0 only once that is expected ready soon
        let job_keys = prepare::Keys::of(&["--harness", "h.luau", "--context", r#"{"Unit":"B"}"#].iter().map(|s| s.to_string()).collect::<Vec<_>>(), true);
        assert!(key.serves(&job_keys));
        let soon = Instant::now() + WAIT_FOR_PREPARED;
        let slot = service.slots.lock().unwrap()[0].clone();
        assert!(!slot.ready_by.is_some_and(|ready| ready <= soon), "expected in 1 s: a free worker records it");
    }

    /// A preparation that fails is not tried again by the idle workers (they spun on one forever, at
    /// ~3 cores, and kept the service from exiting): the next job like it tries once more.
    #[test]
    fn a_failed_preparation_is_not_retried_while_idle() {
        let service = idle_service(2);
        let failing = Ahead { args: vec!["--harness".into(), std::env::temp_dir().join("ree-missing-harness.luau").to_string_lossy().into_owned()], split: false, framed: false, record: true };
        *service.recent.lock().unwrap() = Some(failing.clone());
        let Some(Work::Prepare(ahead_work)) = ahead(&service, 1, None) else { panic!("an idle worker prepares the recent recording") };
        assert!(prepare_next(&service, 1, &ahead_work).is_none());
        assert_eq!(service.prepared.made.load(Ordering::SeqCst), 0);
        assert!(service.recent.lock().unwrap().is_none(), "the failed recording is no longer the one to prepare");
        assert!(ahead(&service, 0, None).is_none() && ahead(&service, 1, None).is_none(), "no idle worker tries it again");
        assert!(!service.slots.lock().unwrap().iter().any(|slot| slot.preparing || slot.key.is_some()));
    }

    /// A recording is prepared ahead only when jobs like it repeat: another one started within
    /// [`REPEATS_WITHIN`] or waits; the job's own start does not count.
    #[test]
    fn only_repeated_recordings_are_prepared() {
        let service = idle_service(2);
        let args = |unit: &str| -> Vec<String> { ["--harness", "h.luau", "--context", &format!(r#"{{"Unit":"{unit}"}}"#)].iter().map(|s| s.to_string()).collect() };
        let key = prepare::Key::of(&args("A"), true, true);
        let own = Instant::now();
        service.starts.lock().unwrap().push_back((prepare::Keys::of(&args("A"), true), own));
        assert!(!repeats(&service, &key, own), "a lone job");
        service.starts.lock().unwrap().push_back((prepare::Keys::of(&["--harness", "other.luau"].iter().map(|s| s.to_string()).collect::<Vec<_>>(), true), Instant::now()));
        assert!(!repeats(&service, &key, own), "a job of another harness");
        let earlier = Instant::now().checked_sub(REPEATS_WITHIN + Duration::from_secs(1));
        if let Some(earlier) = earlier {
            service.starts.lock().unwrap().push_back((prepare::Keys::of(&args("B"), true), earlier));
            assert!(!repeats(&service, &key, own), "a job like it long ago");
        }
        service.starts.lock().unwrap().push_back((prepare::Keys::of(&args("C"), true), Instant::now()));
        assert!(repeats(&service, &key, own), "another unit of the harness just now (its Prepare serves every context)");
        let fresh = idle_service(1);
        assert!(!repeats(&fresh, &key, own));
        let (client, _server) = {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            (client, listener.accept().unwrap().0)
        };
        fresh.queue.lock().unwrap().push_back(Job { command: "record".into(), args: args("D"), stream: client, key: Some(prepare::Keys::of(&args("D"), true)) });
        assert!(repeats(&fresh, &key, own), "a job like it waits");
    }

    /// A run that read what a prepared run cannot check (an image, a mesh) before its checkpoint is not
    /// prepared after its job: the preparation would fail alike. Read after the checkpoint, it is.
    #[test]
    fn a_run_that_read_an_image_before_its_checkpoint_is_not_prepared() {
        let root = std::env::temp_dir().join(format!("ree-serve-unchecked-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let image = root.join("missing.png").to_string_lossy().replace('\\', "/");
        let next = |before: bool| {
            let read = format!(r#"pcall(function() game:GetService("AssetService"):CreateEditableImageAsync(Content.fromUri("file://{image}")) end)"#);
            let harness = root.join(if before { "Before.luau" } else { "After.luau" });
            let (first, second) = if before { (read.as_str(), "") } else { ("", read.as_str()) };
            std::fs::write(&harness, format!("return function(Context)\n{first}\nContext.Checkpoint()\n{second}\nend")).unwrap();
            let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &root.join("out").to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.1"].iter().map(|s| s.to_string()).collect();
            let counts = PreparedCounts::default();
            let (reply, _, next) = run_job("record", &args, &mut None, &counts);
            assert_eq!(reply["code"], 0, "{reply}");
            (next.is_some(), counts.skipped.load(Ordering::SeqCst), counts.unmade.lock().unwrap().clone())
        };
        assert_eq!(next(true), (false, 1, Some("an image was read".to_string())));
        assert_eq!(next(false), (true, 0, None));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Workers run jobs side by side, sharing one store of converted bundles: each job records what it
    /// records alone.
    #[test]
    fn workers_side_by_side_record_what_one_records() {
        let root = std::env::temp_dir().join(format!("ree-serve-workers-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let harness = root.join("Spin.luau");
        std::fs::write(
            &harness,
            r#"return function(Context)
                local Part = Instance.new("Part")
                Part.Name = "Spinner" .. Context.Index
                Part.Anchored = true
                Part.Parent = workspace
                game:GetService("RunService").Heartbeat:Connect(function(Step)
                    Part.CFrame *= CFrame.Angles(0, Step * Context.Index, 0)
                end)
            end"#,
        )
        .unwrap();
        let record = |index: usize, out: &str| {
            let args: Vec<String> = ["--harness", &harness.to_string_lossy(), "--out", &root.join(out).to_string_lossy(), "--roots", &root.join("none").to_string_lossy(), "--seconds", "0.5", "--context", &format!(r#"{{"Index":{index}}}"#)]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let (reply, emulator) = job("record", &args);
            drop(emulator);
            assert_eq!(reply["code"], 0, "{reply}");
            std::fs::read(root.join(out).join("playback.json")).unwrap()
        };
        let alone: Vec<Vec<u8>> = (1..=3).map(|index| record(index, &format!("alone{index}"))).collect();
        #[cfg(feature = "bundles")]
        let store = crate::alloc::long_lived(crate::project::model_assets::BundleStore::default);
        let together: Vec<Vec<u8>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (1..=3)
                .map(|index| {
                    #[cfg(feature = "bundles")]
                    let store = store.clone();
                    let record = &record;
                    std::thread::Builder::new()
                        .stack_size(64 << 20)
                        .spawn_scoped(scope, move || {
                            #[cfg(feature = "bundles")]
                            crate::project::model_assets::keep_bundles_in(store);
                            record(index, &format!("together{index}"))
                        })
                        .unwrap()
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect()
        });
        assert_eq!(alone, together);
        assert!((1..=8).contains(&default_workers()));
        let _ = std::fs::remove_dir_all(&root);
    }
}

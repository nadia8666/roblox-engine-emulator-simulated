//! The profiling suite: `ree run|record ... --profile <kinds> [--profile-out <dir>]`, or [`Session`] from a host that
//! embeds the engine (docs/cli.md "Profiles and caches", docs/engine.md "Profiling").
//!
//! Kinds (comma-separated; `all` is every deterministic one):
//! - `work` (also `instructions`, `lines`): the VM steps every instruction and books the work clock's charges to the
//!   instruction that caused them: instructions and work per function and line (self and total), by opcode, call counts,
//!   callers and callees ([`vm`]). Exact and the same every run.
//! - `caches`: Luau's inline caches per call site (from the same stepping) and ree's own caches ([`caches`]).
//! - `alloc`: bytes and counts allocated by Luau stack and by type ([`memory`]).
//! - `heap`: snapshots of the reachable heap, what a full collection would keep, at the start, the end and each
//!   `Context.ProfilePhase` mark (by type, Instance class, the biggest by what they keep alive, the run's reachable
//!   objects by allocating stack), with the garbage the collector has not swept yet apart ([`memory::snapshot`]: nothing
//!   is collected for one), and the collector's work.
//! - `timeline`: one row a frame: work, host time, instructions, heap, allocations, collector, instances, connections,
//!   threads.
//! - `cpu`: the run thread's CPU cycles, the process's CPU time, page faults and peak memory ([`cpu`]).
//! - `hw`: the CPU's hardware counters (instructions, cycles, the cycles stalled waiting for load data, the L1 data
//!   cache's misses outstanding, branch mispredictions) per run, thread, phase and frame, and per Luau function, calling
//!   context and call site from readings around sampled calls, through the counter service ([`hw`]). Not in `all`: the
//!   stepping kinds' own work would be counted with the run's, so the counters are read in a run of their own.
//! - `phases`: where the command's wall time went (the `--profile` of before, timing.rs).
//! - `time`: host time sampled at the VM's safepoints (luau_profile.rs's ticker). Not in `all`: stepping makes the
//!   interpreter slower than the engine's Rust, so host time is read in a run of its own.
//! - `boundary`: Luau-to-engine crossings per call site (boundary.rs). Not in `all`: it walks a stack per crossing.
//!
//! Each run writes, in the profile directory: `summary.txt` (also printed), `profile.json`, folded stacks
//! (`work.folded`, `instructions.folded`, `time.folded`, `alloc.folded`, `heap.folded`), `profile.speedscope.json` and
//! `report.html` ([`report`]); `ree profile diff <before> <after>` compares two ([`diff`]).
//!
//! Off, every hook is one thread-local read (the VM's own: one per interpreter entry); with a profile on, the work clock,
//! os.clock and what scripts see are unchanged: the profile allocates nothing on the VM's heap and changes nothing the
//! VM counts.

pub mod caches;
pub mod cpu;
pub mod diff;
pub mod hw;
pub mod memory;
pub mod prices;
pub mod report;
pub mod time;
pub mod vm;
pub mod whatif;

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
    time::Instant,
};

use mlua::{Lua, ffi::lua_State};
use rustc_hash::FxHashMap;

use crate::Engine;

/// What a profile records
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kinds(u32);

impl Kinds {
    pub const PHASES: Kinds = Kinds(1);
    pub const BOUNDARY: Kinds = Kinds(1 << 1);
    pub const TIME: Kinds = Kinds(1 << 2);
    pub const WORK: Kinds = Kinds(1 << 3);
    pub const ALLOC: Kinds = Kinds(1 << 4);
    pub const HEAP: Kinds = Kinds(1 << 5);
    pub const CACHES: Kinds = Kinds(1 << 6);
    pub const TIMELINE: Kinds = Kinds(1 << 7);
    pub const CPU: Kinds = Kinds(1 << 8);
    pub const HW: Kinds = Kinds(1 << 9);
    /// Every kind that gives the same numbers every run, with the phases and the CPU's
    pub const ALL: Kinds = Kinds(Self::PHASES.0 | Self::WORK.0 | Self::ALLOC.0 | Self::HEAP.0 | Self::CACHES.0 | Self::TIMELINE.0 | Self::CPU.0);

    /// (name, kind, what it records)
    pub const NAMES: [(&'static str, Kinds, &'static str); 10] = [
        ("work", Kinds::WORK, "instructions and the work clock per function and line, by opcode, calls, callers and callees"),
        ("caches", Kinds::CACHES, "Luau's inline caches per call site and ree's own caches"),
        ("alloc", Kinds::ALLOC, "bytes and counts allocated by Luau stack and by type"),
        ("heap", Kinds::HEAP, "reachable-heap snapshots (garbage not swept yet apart) and the collector's work"),
        ("timeline", Kinds::TIMELINE, "one row a frame: work, host time, heap, allocations, instances, connections, threads"),
        ("cpu", Kinds::CPU, "the run thread's CPU cycles, the process's CPU time, page faults and peak memory"),
        ("hw", Kinds::HW, "hardware counters (cycles stalled on loads, misses outstanding) per run, thread, phase, frame and Luau function, through the counter service (not in all)"),
        ("phases", Kinds::PHASES, "where the command's wall time went"),
        ("time", Kinds::TIME, "host time sampled at the VM's safepoints (a run of its own: not in all)"),
        ("boundary", Kinds::BOUNDARY, "Luau-to-engine crossings per call site (not in all)"),
    ];

    pub fn has(self, kind: Kinds) -> bool {
        self.0 & kind.0 != 0
    }

    pub fn with(self, kind: Kinds) -> Kinds {
        Kinds(self.0 | kind.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `work,alloc`, `all`, `all,time`: the kinds of a `--profile` value. Aliases: `instructions` and `lines` are
    /// `work`, `memory` is `alloc,heap`, `gc` is `heap`. A word it does not know, or a value that names no kind, is an
    /// error that lists the kinds.
    pub fn parse(text: &str) -> Result<Kinds, String> {
        let known = || Kinds::NAMES.iter().map(|(name, _, _)| *name).collect::<Vec<_>>().join(", ");
        let mut kinds = Kinds::default();
        for word in text.split(',').map(str::trim).filter(|word| !word.is_empty()) {
            kinds = kinds.with(match word {
                "all" => Kinds::ALL,
                "instructions" | "lines" => Kinds::WORK,
                "memory" => Kinds::ALLOC.with(Kinds::HEAP),
                "gc" => Kinds::HEAP,
                _ => match Kinds::NAMES.iter().find(|(name, _, _)| *name == word) {
                    Some((_, kind, _)) => *kind,
                    None => return Err(format!("--profile: no kind {word:?} (kinds: {}, all)", known())),
                },
            });
        }
        if kinds.is_empty() {
            return Err(format!("--profile: no kind named in {text:?} (kinds: {}, all)", known()));
        }
        Ok(kinds)
    }

    pub fn names(self) -> Vec<&'static str> {
        Kinds::NAMES.iter().filter(|(_, kind, _)| self.has(*kind)).map(|(name, _, _)| *name).collect()
    }

    /// Whether the VM steps (instruction counts, work, Luau's caches)
    pub fn steps(self) -> bool {
        self.has(Kinds::WORK) || self.has(Kinds::CACHES)
    }

    /// Whether the run's host time is not its own: a kind that slows it (stepping about 5x, the allocation and heap hooks about
    /// 2x, host time's samples, the boundary's stack walks, the counters' readings). Its host time, its cycles and its CPU figures
    /// are then taken in a pass of their own (`cpu`), not from this one
    pub fn slows_the_run(self) -> bool {
        self.steps() || self.has(Kinds::ALLOC) || self.has(Kinds::HEAP) || self.has(Kinds::TIME) || self.has(Kinds::BOUNDARY) || self.has(Kinds::HW)
    }

    pub fn without(self, kind: Kinds) -> Kinds {
        Kinds(self.0 & !kind.0)
    }
}

/// A profile's options
#[derive(Clone, Debug)]
pub struct Config {
    pub kinds: Kinds,
    /// The profile directory
    pub out: PathBuf,
    /// Rows per table in the text summary
    pub top: usize,
    /// The phases the summary reports (`--profile-phase a,b` or `all`; default: `steady` when the harness named one, else
    /// every phase)
    pub phases: Option<String>,
    /// Allocation stacks sampled every this many bytes on average (1: every allocation, the default)
    pub alloc_step: u64,
    /// Heap snapshots at each `Context.ProfilePhase` mark as well as at the end
    pub snapshots_at_marks: bool,
    /// The summary is printed on stderr when the profile is written
    pub print: bool,
    /// The hardware counters `hw` reads (`--profile-counters a,b`; default [`hw::protocol::DEFAULT_COUNTERS`])
    pub counters: Vec<String>,
    /// The folder of the `cpu` pass this run's CPU figures, host time a frame and cycles come from (ran before it, as a process of
    /// its own, when `cpu` was asked with a kind that slows the run)
    pub cpu_pass: Option<PathBuf>,
}

impl Config {
    pub fn new(kinds: Kinds, out: PathBuf) -> Config {
        Config { kinds, out, top: 25, phases: None, alloc_step: 1, snapshots_at_marks: true, print: true, counters: hw::protocol::DEFAULT_COUNTERS.iter().map(|name| name.to_string()).collect(), cpu_pass: None }
    }

    /// The suite's options of a command line, None when it asks for no profile of the suite: `--profile` alone (the
    /// phases of before, timing.rs) and `--profile boundary` alone (boundary.rs) keep their own form. A value naming a
    /// kind the suite does not know is an error listing the kinds (`ree record|run` exit 2 before they start).
    ///
    /// `--profile <kinds> [--profile-out <dir>] [--profile-top <n>] [--profile-phase <a,b|all>] [--profile-alloc-step
    /// <bytes>] [--profile-counters <a,b>]`; `default_out` is the directory when `--profile-out` is not given.
    pub fn from_args(args: &[String], default_out: impl FnOnce() -> PathBuf) -> Result<Option<Config>, String> {
        let value = |key: &str| args.iter().position(|arg| arg == key).and_then(|index| args.get(index + 1)).cloned();
        let Some(text) = profile_value(args) else {
            return Self::from_env(default_out);
        };
        if text == "boundary" {
            return Ok(None);
        }
        let kinds = Kinds::parse(&text)?;
        let mut config = Config::new(kinds, value("--profile-out").map(PathBuf::from).unwrap_or_else(default_out));
        if let Some(top) = value("--profile-top") {
            config.top = top.parse().map_err(|_| "--profile-top: not a count".to_string())?;
        }
        config.phases = value("--profile-phase");
        if let Some(step) = value("--profile-alloc-step") {
            config.alloc_step = step.parse::<u64>().map_err(|_| "--profile-alloc-step: not a count of bytes".to_string())?.max(1);
        }
        if let Some(counters) = value("--profile-counters") {
            config.counters = hw::parse_counters(&counters)?;
        }
        Ok(Some(config))
    }

    /// `REE_PROFILE=<kinds>` (and `REE_PROFILE_OUT=<dir>`, `REE_PROFILE_COUNTERS=<a,b>`): a profile for a host whose
    /// command line cannot carry one
    pub fn from_env(default_out: impl FnOnce() -> PathBuf) -> Result<Option<Config>, String> {
        let Some(text) = std::env::var("REE_PROFILE").ok().filter(|text| !text.trim().is_empty()) else { return Ok(None) };
        let out = std::env::var_os("REE_PROFILE_OUT").filter(|out| !out.is_empty()).map(PathBuf::from).unwrap_or_else(default_out);
        let mut config = Config::new(Kinds::parse(&text)?, out);
        if let Some(counters) = std::env::var("REE_PROFILE_COUNTERS").ok().filter(|text| !text.trim().is_empty()) {
            config.counters = hw::parse_counters(&counters)?;
        }
        Ok(Some(config))
    }
}

/// Runs the command again as a process of its own with `--profile cpu` alone, its profile into `folder` (and its recording, if
/// any, into `folder/out`): the CPU figures, host time a frame and cycles of a run whose asked kinds slow it come from there
pub fn cpu_pass(args: &[String], command: &str, folder: &std::path::Path) -> Result<(), String> {
    // (the options the pass sets itself, and the outputs it must not overwrite)
    const VALUED: [&str; 6] = ["--profile-out", "--profile-top", "--profile-phase", "--profile-alloc-step", "--profile-counters", "--profile"];
    const OUTPUTS: [&str; 3] = ["--out", "--dir", "--dump-scene"];
    let mut child: Vec<String> = vec![command.to_string()];
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--profile" {
            index += profile_arguments(args, index);
            continue;
        }
        if VALUED.contains(&arg.as_str()) {
            index += 2;
            continue;
        }
        if OUTPUTS.contains(&arg.as_str()) {
            child.push(arg.clone());
            child.push(folder.join("out").to_string_lossy().into_owned());
            index += 2;
            continue;
        }
        child.push(arg.clone());
        index += 1;
    }
    child.extend(["--profile".to_string(), "cpu".to_string(), "--profile-out".to_string(), folder.to_string_lossy().into_owned()]);
    std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let log = std::fs::File::create(folder.join("stderr.txt")).map_err(|error| error.to_string())?;
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let status = std::process::Command::new(exe)
        .args(&child)
        .env_remove("REE_PROFILE")
        .env_remove("REE_PROFILE_OUT")
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .status()
        .map_err(|error| error.to_string())?;
    if !folder.join("profile.json").is_file() {
        return Err(format!("it wrote no profile (exit {}; {})", status.code().unwrap_or(-1), folder.join("stderr.txt").display()));
    }
    // (what the pass is for is its profile.json's CPU figures and frames: its recording, its own report page, summary and stacks go)
    let _ = std::fs::remove_dir_all(folder.join("out"));
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten().filter(|entry| entry.file_name() != "profile.json") {
            let path = entry.path();
            let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        }
    }
    Ok(())
}

/// The value of `--profile`: the word after it (`--profile` followed by another option or nothing is the phases of
/// before). Every such word is the value, so [`Kinds::parse`] refuses one that names no kind it knows: it used to be
/// taken for `--profile` alone, and a misspelt kind ran the command without a profile and exited 0.
pub fn profile_value(args: &[String]) -> Option<String> {
    let index = args.iter().position(|arg| arg == "--profile")?;
    args.get(index + 1).filter(|next| !next.starts_with("--")).cloned()
}

/// How many arguments `--profile` at `index` takes (1, or 2 with a value): for parsers that skip it
pub fn profile_arguments(args: &[String], index: usize) -> usize {
    match args.get(index + 1) {
        Some(next) if !next.starts_with("--") => 2,
        _ => 1,
    }
}

/// Whether a command line asks for the phases of before (`--profile` alone, or `phases` among its kinds)
pub fn phases_requested(args: &[String]) -> bool {
    match profile_value(args) {
        Some(text) => Kinds::parse(&text).is_ok_and(|kinds| kinds.has(Kinds::PHASES)),
        None => args.iter().any(|arg| arg == "--profile"),
    }
}

/// Whether a command runs in its own process for a profile (a service's job would profile the service)
pub fn requested(args: &[String]) -> bool {
    profile_value(args).is_some() || std::env::var("REE_PROFILE").is_ok_and(|text| !text.trim().is_empty())
}

/// A chunk's source, for the annotated listing
#[derive(Clone)]
pub struct Source {
    pub path: Option<String>,
    pub text: Rc<[u8]>,
}

/// A `Context.ProfilePhase` mark (and the first frame's `frames`)
#[derive(Clone, Debug, Default)]
pub struct Mark {
    pub phase: String,
    pub frame: u64,
    pub seconds: f64,
    pub work: f64,
    /// Host time since the profile began, in seconds, and the run thread's CPU cycles so far (QueryThreadCycleTime: reference
    /// cycles), read first thing at the mark
    pub host: f64,
    pub cycles: u64,
    /// What the VM and the profilers had counted so far, once the VM is installed: a phase's figures are the difference
    /// between its mark's and the next one's (the last phase's: the profile's end, [`State::end`])
    pub totals: Option<Totals>,
    /// What the run's scripts had asked the engine to do so far (cost::EngineOps), once the engine is installed
    pub engine: Option<crate::cost::EngineOps>,
}

/// Running counts at a mark ([`Mark::totals`])
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    /// Instructions dispatched (scripts', a native module's counted as interpreted)
    pub insns: u64,
    /// The VM's work counters (cost::VmCounters): collectable objects and their bytes a script's own instructions allocated,
    /// table storage bytes, keys rehashed, and the frame's cold first touches (tables, closures, functions, their code)
    pub objects: u64,
    pub allocated: u64,
    pub stored: u64,
    pub rehashed: u64,
    pub cold_tables: u64,
    pub cold_closures: u64,
    pub cold_protos: u64,
    pub cold_code: u64,
    /// table.sort's comparisons and its calls to a script's comparison function (the work counters' sortcompares, sortcalls)
    pub sort_compares: u64,
    pub sort_calls: u64,
    /// The allocation profile's: every allocation's bytes and the objects, and the rehash hook's rehashes and their keys, all
    /// and those a script's own instruction made: (events, keys, script events, script keys)
    /// (`alloc` or `heap`)
    pub alloc_bytes: Option<u64>,
    pub allocs: Option<u64>,
    pub rehashes: Option<(u64, u64, u64, u64)>,
    /// The collector's steps, cycles, bytes freed and host seconds (`heap` or `timeline`)
    pub gc: (u64, u64, u64, f64),
    /// The inline caches' outcomes by kind (vm::CACHE_KINDS) and the builtin calls served and declined (`caches`)
    pub caches: Option<vm::CacheTotals>,
}

/// One frame of the timeline
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub frame: u64,
    pub seconds: f64,
    pub phase: u32,
    /// The frame's host time and the run thread's cycles (None in a run a kind slows: the `cpu` pass's)
    pub host_ms: Option<f64>,
    pub work_ms: f64,
    pub instructions: u64,
    pub heap_bytes: u64,
    pub allocated_bytes: u64,
    pub allocations: u64,
    pub gc_steps: u64,
    pub gc_ms: f64,
    pub gc_freed: u64,
    /// The live instances at the frame's end: in the DataModel, alive outside it, destroyed and kept until the VM collects them
    pub instances: (u64, u64, u64),
    pub connections: u64,
    /// Threads the VM made in the frame (coroutines, task threads, handlers' threads)
    pub threads_made: u64,
    pub cycles: Option<u64>,
}

/// One frame of the `cpu` kind: its host time, the run thread's cycles and the working set at its end
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuFrame {
    pub frame: u64,
    pub phase: u32,
    pub host_ms: f64,
    pub cycles: u64,
    pub working_set: Option<u64>,
}

/// What a run of the engine was (the summary's head)
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub command: String,
    pub status: String,
    pub harness: Option<String>,
    pub frames: u64,
    pub seconds: f64,
    pub fps: f64,
    /// Script timeouts the run had (watchdog.rs), and those host time fired alone (its backstop: a profile that slows the run
    /// makes them likelier, so a run and its cpu pass are compared by them)
    pub script_timeouts: u64,
    pub host_backstop_timeouts: u64,
}

/// The engine's state the profile reads, set at [`install`]
struct Installed {
    state: *mut lua_State,
    engine: Weak<Engine>,
    /// (the VM's profile reads these cells by pointer while it steps: held here as long as it runs)
    _work: Rc<Cell<f64>>,
    _drained: Rc<Cell<f64>>,
    counters: Option<*const crate::cost::VmCounters>,
    frame_started: Option<(Instant, u64)>,
    last: FrameTotals,
    /// The threads the VM had made when the frame began
    threads_made: u64,
}

/// Running totals a frame's row is the difference of (taken when the frame begins)
#[derive(Clone, Copy, Default)]
struct FrameTotals {
    work: f64,
    instructions: u64,
    allocated_bytes: u64,
    allocations: u64,
    gc_steps: u64,
    gc_seconds: f64,
    gc_freed: u64,
}

struct State {
    config: Config,
    started: Instant,
    installed: Option<Installed>,
    sources: FxHashMap<String, Source>,
    phases: Vec<String>,
    phase: u32,
    marks: Vec<Mark>,
    rows: Vec<Row>,
    snapshots: Vec<memory::Snapshot>,
    collector: memory::Collector,
    /// Threads the VM made so far (the thread callback)
    threads_made: u64,
    /// The `cpu` kind's frames, and when the frame now running began
    cpu_frames: Vec<CpuFrame>,
    cpu_frame_started: Option<(Instant, u64)>,
    /// Host time samples (`time`)
    time: Option<time::Time>,
    /// What the VM's profile and the allocation profile held when collected
    vm: Option<vm::Results>,
    allocations: Option<memory::Allocations>,
    cpu: cpu::Counters,
    /// The hardware counters (`hw`)
    hw: Option<hw::Session>,
    warnings: Vec<String>,
    collected: bool,
    /// The work clock when the profile was collected
    end_work: f64,
    /// The profile's end as a mark (phase `end`): its counts when the profile was collected
    end: Option<Mark>,
}

impl State {
    /// The running counts now ([`Totals`]; None before the VM is installed)
    fn totals(&self) -> Option<Totals> {
        let installed = self.installed.as_ref()?;
        let kinds = self.config.kinds;
        let mut totals = Totals::default();
        if let Some(counters) = installed.counters.map(|counters| unsafe { &*counters }) {
            totals.insns = instructions(counters);
            totals.objects = counters.objects;
            totals.allocated = counters.allocated;
            totals.stored = counters.stored;
            totals.rehashed = counters.rehashed;
            totals.cold_tables = counters.coldtables;
            totals.cold_closures = counters.coldclosures;
            totals.cold_protos = counters.coldprotos;
            totals.cold_code = counters.coldcode;
            totals.sort_compares = counters.sortcompares;
            totals.sort_calls = counters.sortcalls;
        }
        if kinds.has(Kinds::ALLOC) || kinds.has(Kinds::HEAP) {
            if let Some((bytes, objects, rehashes)) = memory::with(|allocations| (allocations.bytes, allocations.objects, (allocations.rehashes.events, allocations.rehashes.keys, allocations.rehashes.script_events, allocations.rehashes.script_keys))) {
                totals.alloc_bytes = Some(bytes);
                totals.allocs = Some(objects);
                totals.rehashes = Some(rehashes);
            }
        }
        totals.gc = (self.collector.steps, self.collector.cycles, self.collector.freed, self.collector.seconds);
        if kinds.has(Kinds::CACHES) {
            totals.caches = Some(vm::cache_totals(installed.state));
        }
        Some(totals)
    }
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static STATE: RefCell<Option<Box<State>>> = const { RefCell::new(None) };
    /// The VM's resume callbacks before the counters took them (the counters call them on, and give them back at the
    /// collect); none while the counters hold none
    static RESUME_CALLBACKS: Cell<Option<(Option<ResumeCallback>, Option<ResumeCallback>)>> = const { Cell::new(None) };
}

/// lua.h `lua_Callbacks::preresume` and `postresume`
type ResumeCallback = unsafe extern "C-unwind" fn(*mut lua_State);

/// Whether a profile of the suite runs on this thread
#[inline]
pub fn active() -> bool {
    ON.with(Cell::get)
}

fn with_state<R>(body: impl FnOnce(&mut State) -> R) -> Option<R> {
    if !active() {
        return None;
    }
    STATE.with(|state| {
        let mut state = state.try_borrow_mut().ok()?;
        Some(body(state.as_mut()?))
    })
}

/// The kinds of the profile running on this thread (none when there is none)
pub fn kinds() -> Kinds {
    with_state(|state| state.config.kinds).unwrap_or_default()
}

/// A profile of the suite on this thread: [`Session::begin`] before the engine starts, [`install`] once its VM runs
/// (luau_profile::install calls it), [`collect`] before the VM goes, [`Session::finish`] to write it.
pub struct Session {
    _private: (),
}

impl Session {
    /// Starts a profile on this thread (ree's own profilers the kinds name: the phases, the boundary profiler, ree's
    /// caches; the VM's are added by [`install`]).
    pub fn begin(config: Config) -> Session {
        if config.kinds.has(Kinds::PHASES) && !crate::timing::enabled() {
            crate::timing::begin(Instant::now());
        }
        if config.kinds.has(Kinds::BOUNDARY) {
            crate::boundary::begin(crate::boundary::Config { out: config.out.join("boundary.json"), top: config.top, phase: config.phases.clone() });
        }
        if config.kinds.has(Kinds::CACHES) {
            caches::reset();
            caches::set_on(true);
        }
        let cpu = cpu::Counters::begin(config.kinds.has(Kinds::CPU));
        let mut warnings = Vec::new();
        let hw = config.kinds.has(Kinds::HW).then(|| {
            if config.kinds.steps() || config.kinds.has(Kinds::ALLOC) || config.kinds.has(Kinds::HEAP) {
                warnings.push("hw counts what the work, caches, alloc and heap profilers do too (stepping makes the interpreter several times slower): read the counters in a run of their own".to_string());
            }
            let session = hw::Session::begin(&config.counters);
            if !session.counting() {
                warnings.push(format!("hardware counters not read: {}", session.error().unwrap_or("the counter service did not start")));
            }
            session
        });
        let state = State {
            config,
            started: Instant::now(),
            installed: None,
            sources: FxHashMap::default(),
            phases: vec!["startup".into()],
            phase: 0,
            marks: Vec::new(),
            rows: Vec::new(),
            snapshots: Vec::new(),
            collector: memory::Collector::default(),
            threads_made: 0,
            cpu_frames: Vec::new(),
            cpu_frame_started: None,
            time: None,
            vm: None,
            allocations: None,
            cpu,
            hw,
            warnings,
            collected: false,
            end_work: 0.0,
            end: None,
        };
        STATE.with(|cell| *cell.borrow_mut() = Some(Box::new(state)));
        ON.with(|on| on.set(true));
        Session { _private: () }
    }

    /// Writes the profile (collecting what [`collect`] did not: a host that never called it gets no VM profile, which
    /// the summary says) and ends it: the summary text.
    pub fn finish(self, run: Run) -> Result<String, String> {
        let boundary_text = crate::boundary::finish();
        let (phases_text, phase_tree) = crate::timing::finish_tree().unzip();
        caches::set_on(false);
        ON.with(|on| on.set(false));
        let Some(mut state) = STATE.with(|cell| cell.borrow_mut().take()) else { return Err("no profile ran".into()) };
        if !state.collected && state.installed.is_some() {
            state.warnings.push("the VM's profile was not collected before the VM went (a host must call profiler::collect)".into());
        }
        let caches = caches::snapshot();
        let facts = caches::facts();
        let cpu = state.cpu.finish();
        let _ = &state.cpu_frame_started;
        let hw = state.hw.take().map(|session| session.finish(state.phase));
        let input = report::Input {
            config: &state.config,
            run: &run,
            elapsed: state.started.elapsed().as_secs_f64(),
            vm: state.vm.as_ref(),
            allocations: state.allocations.as_ref(),
            time: state.time.as_ref(),
            phases: &state.phases,
            marks: &state.marks,
            rows: &state.rows,
            snapshots: &state.snapshots,
            collector: &state.collector,
            caches: &caches,
            facts,
            cpu: &cpu,
            cpu_frames: &state.cpu_frames,
            hw: hw.as_ref(),
            sources: &state.sources,
            end_work: state.end_work,
            end: state.end.as_ref(),
            boundary: boundary_text.as_deref(),
            phase_timing: phases_text.as_deref(),
            phase_tree: phase_tree.as_deref(),
            warnings: &state.warnings,
        };
        report::write(&input)
    }
}

/// Hooks the VM of `lua` (its engine from the Lua's app data) for the running profile: the VM's stepping profile, the
/// allocation profile, the interrupt's sampling and collector events, the thread counter and a first heap snapshot.
/// Nothing when no profile runs.
pub fn install(lua: &Lua) {
    if !active() {
        return;
    }
    let Some(engine) = lua.app_data_ref::<Rc<Engine>>().map(|engine| engine.clone()) else { return };
    let state = unsafe { mlua::ffi::lua_mainthread(lua.current_thread().state()) };
    let kinds = kinds();
    let (work, drained, counters) = {
        let engine_state = engine.state.borrow();
        (engine_state.sched.work.clone(), engine_state.sched.work_drained.clone(), engine_state.sched.vm_counters)
    };
    let prices = if kinds.steps() { vm::prices() } else { None };
    if kinds.steps() && prices.is_none() {
        with_state(|s| s.warnings.push("the work clock's model is not a weighted sum of the VM's counts: work is not booked per instruction".into()));
    }
    let host = Some((work.as_ptr() as *const f64, drained.as_ptr() as *const f64));
    let profiling_vm = vm::start(state, kinds.steps(), prices.as_deref(), host);
    if !profiling_vm {
        with_state(|s| s.warnings.push("another profile of the VM already runs on this thread".into()));
    }
    // (host time names its samples without stepping: a chunk's functions and its inlined copies are registered as it loads, which
    // no other kind needs)
    if profiling_vm {
        unsafe { vm::chunks(state, kinds.has(Kinds::TIME)) };
        // (ree's tools' code is not the game's: the marks' cache totals leave it out, as the report's tables do)
        let mut tools = vec!["=rur.", "=ree."];
        let harness = format!("={}.", report::HARNESS_FOLDER);
        if std::env::var_os(report::TOOL_HARNESS_ENV).is_some_and(|value| value == "1") {
            tools.push(&harness);
        }
        unsafe { vm::tool_chunks(state, &tools) };
    }
    if kinds.has(Kinds::ALLOC) || kinds.has(Kinds::HEAP) {
        let step = with_state(|s| s.config.alloc_step).unwrap_or(1);
        memory::start(state, step, kinds.has(Kinds::HEAP), Some(memory::resolver(Rc::downgrade(&engine))));
    }
    if kinds.has(Kinds::TIMELINE) {
        // threads made: the VM's thread callback (census.rs chains the same function)
        unsafe { (*mlua::ffi::lua_callbacks(state)).userthread = Some(crate::census::userthread) };
    }
    // (the run's CPU figures start with its VM: the command's startup is not the run's)
    with_state(|s| s.cpu.start());
    if kinds.has(Kinds::TIME) {
        // (the interpreter entered and left: host time between handlers is the engine's, booked when the next one starts)
        unsafe { vm::set_vm_hook(state, Some(vm_entered), std::ptr::null_mut()) };
        // (a sample after a C function holds its time: the VM's count of C calls says whether one ran since the last safepoint)
        crate::luau_profile::watch_c_calls(counters.map_or(std::ptr::null(), |counters| unsafe { &raw const (*counters).ccalls }));
        let base = crate::luau_profile::start_suite_ticker();
        with_state(|s| s.time = Some(time::Time::new(base)));
    }
    // the counters: a reading as the VM starts, and the VM's hook around the Luau calls it samples (hw/mod.rs)
    if kinds.has(Kinds::HW) {
        let counting = with_state(|s| {
            let phase = s.phase;
            let hw = s.hw.as_mut()?;
            hw.read(hw::What::Install, phase);
            Some(hw.counting())
        })
        .flatten()
        .unwrap_or(false);
        if counting && profiling_vm {
            unsafe { vm::set_calls(state, Some(call_hook), std::ptr::null_mut()) };
            // (and where a Luau thread holding a sampled call resumes and stops, so what it runs is booked to it)
            let callbacks = unsafe { &mut *mlua::ffi::lua_callbacks(state) };
            RESUME_CALLBACKS.set(Some((callbacks.preresume, callbacks.postresume)));
            callbacks.preresume = Some(resume_hook);
            callbacks.postresume = Some(suspend_hook);
        }
    }
    let totals = FrameTotals { work: engine.state.borrow().sched.work_total(), instructions: counters.map(|c| instructions(unsafe { &*c })).unwrap_or(0), ..Default::default() };
    let (frame, seconds, ops) = {
        let engine_state = engine.state.borrow();
        let ops = engine_state.sched.touched.borrow().ops();
        (engine_state.sched.frame, engine_state.sched.now, ops)
    };
    with_state(|s| {
        s.installed = Some(Installed { state, engine: Rc::downgrade(&engine), _work: work, _drained: drained, counters, frame_started: None, last: totals, threads_made: 0 });
        let (host, cycles) = (s.started.elapsed().as_secs_f64(), cpu::thread_cycles());
        let counts = s.totals();
        s.marks.push(Mark { phase: s.phases[0].clone(), frame, seconds, work: totals.work, host, cycles, totals: counts, engine: Some(ops) });
    });
    if kinds.has(Kinds::HEAP) {
        snapshot("start");
    }
}

/// Whether the profile needs the VM's interrupt (time samples, collector events): luau_profile::install asks
pub fn wants_interrupt() -> bool {
    let kinds = kinds();
    // (the counters read around calls through the VM's own hook: hw/mod.rs)
    kinds.has(Kinds::TIME) || kinds.has(Kinds::HEAP) || kinds.has(Kinds::TIMELINE)
}

/// Whether the profile takes the VM's allocation callback
pub fn wants_allocations() -> bool {
    let kinds = kinds();
    kinds.has(Kinds::ALLOC) || kinds.has(Kinds::HEAP)
}

fn instructions(counters: &crate::cost::VmCounters) -> u64 {
    // (a native module's functions count apart: nativeops)
    counters.ops.iter().sum::<u64>() + counters.nativeops.iter().sum::<u64>()
}

/// What the profile reads from the VM, taken while it still runs: the VM's stepping profile, the allocation profile, a
/// last heap snapshot. A host calls it before its VM goes (ree's commands do, at the run's end).
pub fn collect(lua: &Lua) {
    let Some((state, kinds)) = with_state(|s| (s.installed.as_ref().map(|installed| installed.state), s.config.kinds)) else { return };
    let Some(state) = state else { return };
    if with_state(|s| s.collected).unwrap_or(true) {
        return;
    }
    let _ = lua;
    // (the run's end, before the last snapshot's walk)
    let (ended, end_cycles) = (Instant::now(), cpu::thread_cycles());
    if kinds.has(Kinds::HEAP) {
        snapshot("end");
    }
    vm::stop(state);
    if let Some((preresume, postresume)) = RESUME_CALLBACKS.take() {
        let callbacks = unsafe { &mut *mlua::ffi::lua_callbacks(state) };
        callbacks.preresume = preresume;
        callbacks.postresume = postresume;
    }
    with_state(|s| {
        let (frame, seconds, work, ops) = match s.installed.as_ref().and_then(|installed| installed.engine.upgrade()) {
            Some(engine) => {
                let engine_state = engine.state.borrow();
                let ops = engine_state.sched.touched.borrow().ops();
                (engine_state.sched.frame, engine_state.sched.now, engine_state.sched.work_total(), Some(ops))
            }
            None => (0, 0.0, 0.0, None),
        };
        let host = ended.duration_since(s.started).as_secs_f64();
        s.end = Some(Mark { phase: "end".into(), frame, seconds, work, host, cycles: end_cycles, totals: s.totals(), engine: ops });
    });
    let results = vm::results(state);
    vm::free(state);
    let allocations = if kinds.has(Kinds::ALLOC) || kinds.has(Kinds::HEAP) { memory::stop(state) } else { None };
    if kinds.has(Kinds::TIMELINE) && !crate::census::installed() {
        // (the thread callback stays while a census uses it)
        unsafe { (*mlua::ffi::lua_callbacks(state)).userthread = None };
    }
    if kinds.has(Kinds::TIME) {
        let ticks = crate::luau_profile::ticks();
        with_state(|s| {
            let phase = s.phase;
            if let Some(time) = s.time.as_mut() {
                time.finish(ticks, crate::luau_profile::in_vm(), phase);
            }
        });
        crate::luau_profile::stop_suite_ticker();
    }
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            hw.end(phase);
        }
        s.end_work = s.installed.as_ref().and_then(|installed| installed.engine.upgrade()).map_or(0.0, |engine| engine.state.borrow().sched.work_total());
        s.vm = Some(results);
        s.allocations = allocations;
        s.collected = true;
    });
}

/// A heap snapshot now (`label`: the phase mark or `start` / `end`)
fn snapshot(label: &str) {
    let Some((state, engine, frame, top)) = with_state(|s| {
        let installed = s.installed.as_ref()?;
        Some((installed.state, installed.engine.clone(), s.rows.last().map_or(0, |row| row.frame), s.config.top))
    })
    .flatten() else {
        return;
    };
    let seconds = engine.upgrade().map_or(0.0, |engine| engine.state.borrow().sched.now);
    let resolver = memory::resolver(engine);
    let snapshot = memory::snapshot(state, label, frame, seconds, Some(&*resolver), top.max(25));
    with_state(|s| s.snapshots.push(snapshot));
}

/// `Context.ProfilePhase(name)` (and the first frame's `frames`): the phase of what follows; a heap snapshot when the
/// profile takes them
pub fn phase(name: &str) {
    if !active() {
        return;
    }
    // (the mark's clocks first: what the mark itself does is not the phase before's)
    let (now, cycles) = (Instant::now(), cpu::thread_cycles());
    let Some((stepping, snapshots, state)) = with_state(|s| {
        let id = match s.phases.iter().position(|phase| phase == name) {
            Some(id) => id as u32,
            None => {
                s.phases.push(name.to_string());
                s.phases.len() as u32 - 1
            }
        };
        s.phase = id;
        if let Some(hw) = s.hw.as_mut() {
            hw.read(hw::What::Mark, id);
        }
        let (frame, seconds, work, ops) = match s.installed.as_ref().and_then(|installed| installed.engine.upgrade()) {
            Some(engine) => {
                let engine_state = engine.state.borrow();
                let ops = engine_state.sched.touched.borrow().ops();
                (engine_state.sched.frame, engine_state.sched.now, engine_state.sched.work_total(), Some(ops))
            }
            None => (0, 0.0, 0.0, None),
        };
        let host = now.duration_since(s.started).as_secs_f64();
        let totals = s.totals();
        s.marks.push(Mark { phase: name.to_string(), frame, seconds, work, host, cycles, totals, engine: ops });
        (s.config.kinds.steps(), s.config.kinds.has(Kinds::HEAP) && s.config.snapshots_at_marks && name != "frames", s.installed.as_ref().map(|installed| (installed.state, id)))
    }) else {
        return;
    };
    if let Some((vm_state, id)) = state {
        if stepping {
            vm::phase(vm_state, id);
        }
        memory::with(|allocations| allocations.phase = id);
    }
    if snapshots {
        snapshot(name);
    }
}

/// The current phase's id (allocation samples are filed under it)
pub fn current_phase() -> u32 {
    with_state(|s| s.phase).unwrap_or(0)
}

/// A frame begins (scheduler::step): its host time, cycles and totals start here (what ran between frames is not the
/// frame's: a harness's setup before the first frame is the startup phase's)
pub fn frame_begin(engine: &Engine) {
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            hw.read(hw::What::FrameBegin, phase);
        }
        if s.config.kinds.has(Kinds::CPU) {
            s.cpu_frame_started = Some((Instant::now(), cpu::thread_cycles()));
        }
        if !s.config.kinds.has(Kinds::TIMELINE) {
            return;
        }
        let collector = (s.collector.steps, s.collector.seconds, s.collector.freed);
        let threads_made = s.threads_made;
        let Some(installed) = s.installed.as_mut() else { return };
        installed.frame_started = Some((Instant::now(), cpu::thread_cycles()));
        installed.threads_made = threads_made;
        let (allocated_bytes, allocations) = memory::with(|allocations| (allocations.bytes, allocations.objects)).unwrap_or((0, 0));
        installed.last = FrameTotals {
            work: engine.state.borrow().sched.work_total(),
            instructions: installed.counters.map(|counters| instructions(unsafe { &*counters })).unwrap_or(0),
            allocated_bytes,
            allocations,
            gc_steps: collector.0,
            gc_seconds: collector.1,
            gc_freed: collector.2,
        };
    });
}

/// A frame ended (scheduler::step): its timeline row
pub fn frame_end(engine: &Engine) {
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            hw.read(hw::What::FrameEnd(engine.state.borrow().sched.frame), phase);
        }
        if let Some((started, cycles)) = s.cpu_frame_started.take() {
            let working_set = s.cpu.sample();
            s.cpu_frames.push(CpuFrame { frame: engine.state.borrow().sched.frame, phase, host_ms: started.elapsed().as_secs_f64() * 1e3, cycles: cpu::thread_cycles().saturating_sub(cycles), working_set });
        }
        if !s.config.kinds.has(Kinds::TIMELINE) {
            return;
        }
        // (a kind that slows the run makes its host time not the run's: the cpu pass takes it)
        let own_host = !s.config.kinds.slows_the_run();
        let threads_made = s.threads_made;
        let Some(installed) = s.installed.as_mut() else { return };
        let (started, cycles) = installed.frame_started.take().unwrap_or((Instant::now(), cpu::thread_cycles()));
        let engine_state = engine.state.borrow();
        let work = engine_state.sched.work_total();
        let insns = installed.counters.map(|counters| instructions(unsafe { &*counters })).unwrap_or(0);
        let (allocated_bytes, allocations) = memory::with(|allocations| {
            allocations.resolve_deferred();
            (allocations.bytes, allocations.objects)
        })
        .unwrap_or((0, 0));
        let totals = FrameTotals {
            work,
            instructions: insns,
            allocated_bytes,
            allocations,
            gc_steps: s.collector.steps,
            gc_seconds: s.collector.seconds,
            gc_freed: s.collector.freed,
        };
        let last = installed.last;
        let made = threads_made.saturating_sub(installed.threads_made);
        s.rows.push(Row {
            frame: engine_state.sched.frame,
            seconds: engine_state.sched.now,
            phase: s.phase,
            host_ms: own_host.then(|| started.elapsed().as_secs_f64() * 1e3),
            work_ms: (totals.work - last.work) * 1e3,
            instructions: totals.instructions - last.instructions,
            heap_bytes: vm::total_bytes(installed.state) as u64,
            allocated_bytes: totals.allocated_bytes - last.allocated_bytes,
            allocations: totals.allocations - last.allocations,
            gc_steps: totals.gc_steps - last.gc_steps,
            gc_ms: (totals.gc_seconds - last.gc_seconds) * 1e3,
            gc_freed: totals.gc_freed - last.gc_freed,
            instances: engine_state.dom.census(engine_state.collection.game),
            connections: engine_state.signals.counts().1 as u64,
            threads_made: made,
            cycles: own_host.then(|| cpu::thread_cycles().saturating_sub(cycles)),
        });
    });
}

#[cfg(test)]
thread_local! {
    /// The tick the safepoint's work last ran for (a test's view of it)
    pub(crate) static LAST_SAFEPOINT: Cell<u64> = const { Cell::new(0) };
}

/// The interrupt at a non-GC safepoint where the ticker moved (luau_profile.rs compares it first): a host time sample
pub fn safepoint(state: *mut lua_State, ticks: u64) {
    crate::luau_profile::set_suite_seen(ticks);
    #[cfg(test)]
    LAST_SAFEPOINT.with(|last| last.set(ticks));
    with_state(|s| {
        let phase = s.phase;
        if let Some(time) = s.time.as_mut() {
            crate::luau_profile::set_seen(ticks);
            time.safepoint(state, ticks, phase);
        }
    });
}

/// The VM's hook (lua_setprofilevm): the interpreter entered from outside it, or left back to it. Host time since the last
/// booking was the engine's or the Luau that ran last; booked when the ticker moved.
unsafe extern "C-unwind" fn vm_entered(_ud: *mut std::ffi::c_void, state: *mut lua_State, entered: std::ffi::c_int) {
    let entered = entered != 0;
    let ticks = crate::luau_profile::ticks();
    if ticks != crate::luau_profile::seen() {
        crate::luau_profile::set_seen(ticks);
        with_state(|s| {
            let phase = s.phase;
            if let Some(time) = s.time.as_mut() {
                time.vm(state, entered, ticks, phase);
            }
        });
    }
    crate::luau_profile::set_in_vm(entered);
}

/// One of the engine's own steps begins (`Some(name)`: the animator, joints, physics, the recorder) or the innermost ends
/// (luau_profile::engine_step): its host time is booked to a frame of its own
pub fn time_step(step: Option<&str>) {
    let ticks = crate::luau_profile::ticks();
    with_state(|s| {
        let phase = s.phase;
        if let Some(time) = s.time.as_mut() {
            crate::luau_profile::set_seen(ticks);
            match step {
                Some(name) => time.step_begin(name, crate::luau_profile::in_vm(), ticks, phase),
                None => time.step_end(ticks, phase),
            }
        }
    });
}

/// An Instance API call begins (`begin`) or ends, and the ticker moved (luau_profile::ApiCall): the host time since the last
/// booking was the Luau that made the call, or the call's own (booked to its line)
pub fn time_call(state: *mut lua_State, begin: bool, ticks: u64) {
    with_state(|s| {
        let phase = s.phase;
        if let Some(time) = s.time.as_mut() {
            // (the profile's own thread: another thread's Instance API calls see the switch, not this state)
            crate::luau_profile::set_seen(ticks);
            if begin {
                time.call_begin(state, ticks, phase);
            } else {
                time.call_end(state, ticks, phase);
            }
        }
    });
}

/// The VM's hook around a Luau call it sampled (vm::CALL_ENTER as its first instruction is about to run, vm::CALL_LEAVE as
/// it returns): the counters' readings, so what ran inside the call is its own (hw/mod.rs)
unsafe extern "C-unwind" fn call_hook(_ud: *mut std::ffi::c_void, state: *mut lua_State, event: std::ffi::c_int, function: std::ffi::c_int, every: std::ffi::c_int, depth: std::ffi::c_int) {
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            hw.call(state, event, function, every, depth, phase);
        }
    });
}

/// The VM's `preresume`: a Luau thread is about to run (hw/mod.rs reads there when it holds a sampled call); then the
/// callback the counters took the place of
unsafe extern "C-unwind" fn resume_hook(state: *mut lua_State) {
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            unsafe { hw.resume(state, phase) };
        }
    });
    if let Some((Some(previous), _)) = RESUME_CALLBACKS.get() {
        unsafe { previous(state) };
    }
}

/// The VM's `postresume`: a Luau thread stopped (it yielded, returned, or an error ended it)
unsafe extern "C-unwind" fn suspend_hook(state: *mut lua_State) {
    if let Some((_, Some(previous))) = RESUME_CALLBACKS.get() {
        unsafe { previous(state) };
    }
    with_state(|s| {
        let phase = s.phase;
        if let Some(hw) = s.hw.as_mut() {
            unsafe { hw.suspend(state, phase) };
        }
    });
}

/// One of the engine's own steps begins (`Some(name)`) or ends (luau_profile::engine_step): the counters' readings around
/// it, so its counts are its own; whether the counters read it
pub fn hw_step(step: Option<&str>) -> bool {
    with_state(|s| {
        let phase = s.phase;
        let hw = s.hw.as_mut().filter(|hw| hw.counting())?;
        hw.step(step.map(report::engine_step_function), phase);
        Some(())
    })
    .flatten()
    .is_some()
}

/// The interrupt at a collector step's start and end
pub fn gc_interrupt(state: *mut lua_State) {
    with_state(|s| s.collector.interrupt(state));
}

/// The VM made (`parent` set) or freed a thread (census.rs's callback): the threads made are counted (what the collector has
/// freed by a frame follows its pace, not the program's)
pub fn thread_event(made: bool) {
    if made {
        with_state(|s| s.threads_made += 1);
    }
}

/// A module chunk was compiled (lua/modules.rs): its source, for the annotated listing
pub fn source_loaded(name: &str, path: Option<&str>, text: &Rc<[u8]>) {
    with_state(|s| {
        if s.config.kinds.steps() || s.config.kinds.has(Kinds::TIME) || s.config.kinds.has(Kinds::ALLOC) {
            s.sources.insert(name.to_string(), Source { path: path.map(str::to_string), text: text.clone() });
        }
    });
}

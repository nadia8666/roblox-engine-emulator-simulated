//! Virtual-time task scheduler (port of `runtime/engine/scheduler.luau`): the deferred queue with
//! causal event depth, task / legacy timers, virtual clocks, the frame loop, display-duration
//! stepping and recorded Studio clocks. Every rule is the Luau runtime's (measured in Studio by the
//! runtime this was ported from):
//!
//! Studio edit mode: Heartbeat runs on its own ~240 Hz clock and rendering at 60 Hz, so one frame
//! is 4 x [ legacy wait/spawn/delay resumptions, task.wait/task.delay resumptions, Heartbeat ]
//! then the render step: BindToRenderStep callbacks, the TweenService step, RenderStepped,
//! PreRender, layout. The Heartbeat dts replay a fixed two-frame pattern of float32 beats whose
//! sum exceeds 1/60 by the smallest step. Running games (Mode ~= "edit") get one Heartbeat per
//! frame, as a playing client runs it (measured, Studio 0.744, tools/parity): wait / delay / spawn
//! resumptions (then a full flush), PreAnimation, Stepped / PreSimulation, PostSimulation, the due
//! task.wait / task.delay threads (then one pass over the deferred queue), Heartbeat, then the
//! render step (bindings, one pass, RenderStepped / PreRender, a full flush).
//!
//! A render is a pure function of its inputs and the hour: the wall clock starts at the current
//! whole hour and math.random at a fixed seed. A recorded Studio clock (RUR_CLOCK, written by
//! `rur compare`) replaces the ideal frame clock with the Heartbeats and render steps of that
//! Studio run, its os.clock / tick / DateTime / GetServerTimeNow values and its random seed, and
//! ends the run where Studio captured (see [`Scheduler::use_recorded_clock`]).

use std::{cmp::Ordering, collections::BinaryHeap, collections::VecDeque, rc::Rc};

use mlua::prelude::*;
use rustc_hash::FxHashMap;
use serde_json::Value as Json;

use super::{
    Engine, InstanceId, Options, profile,
    signal::{ConnectionId, SignalId},
    value::Value,
};

/// Roblox's cut of a chain of deferred work: a handler of an event fired from a deferred handler, or a thread a
/// deferred thread task.defers, runs one level deeper than it, and nothing deeper than 80 runs. One depth for both
/// (measured in a playing client, Studio 0.744: a self-firing event, a Changed ping-pong, a task.defer chain and an
/// event / task.defer alternation all stop at 80 steps; task.spawn and coroutine.wrap carry the depth along;
/// tools/parity/probes/deferred_chains.luau, deferred_points.luau).
pub const MAX_REENTRANCY: u32 = 80;

fn trace_scheduler_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("REE_TRACE_SCHEDULER").as_deref() == Ok("1"))
}

fn trace_scheduler(engine: &Engine, label: &str) {
    if !trace_scheduler_enabled() {
        return;
    }
    let (frame, now) = {
        let state = engine.state.borrow();
        (state.sched.frame, state.sched.now)
    };
    crate::output::diagnostic(&format!("ree trace frame={frame} time={now:.6} {label}"));
}

fn trace_scheduler_format(engine: &Engine, label: impl FnOnce() -> String) {
    if trace_scheduler_enabled() {
        trace_scheduler(engine, &label());
    }
}

/// A job that belongs to no signal.
pub const NO_SIGNAL: SignalId = SignalId::MAX;

/// A deferred resumption.
pub enum JobTarget {
    Thread(LuaThread),
    /// A connection's handler: a fresh thread on its callback, if still connected when it runs
    Handler(ConnectionId),
    /// Destroy's deferred signal cleanup
    DisconnectAll(InstanceId),
}

pub struct Job {
    pub target: JobTarget,
    pub args: Vec<Value>,
    pub event_depth: u32,
    /// The signal whose handler or waiting thread this is (the refusal names it), else `NO_SIGNAL`
    pub signal: SignalId,
    /// Seconds of script work the thread's resume costs before its first line (cost.rs HANDLER_THREAD for the handlers of the
    /// signals Studio's bench measured; 0 for a job whose resume is priced elsewhere)
    pub resume_cost: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimerKind {
    Wait,
    LegacyWait,
    Delay,
}

pub struct Timer {
    pub due: f64,
    pub sequence: u64,
    pub thread: LuaThread,
    pub args: Vec<Value>,
    pub started: f64,
    pub kind: TimerKind,
    /// Build progress (instances created) of a call made before the first Heartbeat while a
    /// recorded clock places the build (PlaceBuild)
    pub created_work: Option<u64>,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    // BinaryHeap is a max-heap: the earliest (due, sequence) must compare greatest
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .due
            .partial_cmp(&self.due)
            .unwrap_or(Ordering::Equal)
            .then(other.sequence.cmp(&self.sequence))
    }
}

/// Frame phases hooked by services and the layout (Scheduler.AddHook).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Simulation,
    Heartbeat,
    /// The render-step bindings
    RenderStep,
    /// The frame's layout point: after the render-step bindings, before RenderStepped (a host's lazy layout marks it here)
    Tween,
    /// RenderStepped and PreRender
    RenderEvents,
    /// TweenService's step: after the PreRender handlers on a playing client, before the Heartbeat handlers on a server
    TweenStep,
    PreRender,
    Render,
}

pub type PhaseHook = Rc<dyn Fn(&Lua, &Rc<Engine>, f64) -> LuaResult<()>>;

/// float32 rounding (Studio's dts, tween times and easing results are floats).
#[inline]
pub fn f32r(value: f64) -> f64 {
    value as f32 as f64
}

/// The next float32 above a positive float32.
fn next_f32(value: f64) -> f64 {
    f32::from_bits((value as f32).to_bits() + 1) as f64
}

/// One event of a recorded Studio clock: `{ kind, a, b }` = the JSON array `[kind, a, b]`
/// (`tonumber` of the fields; see [`Scheduler::use_recorded_clock`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedEvent {
    /// b't' / b'h' / b'r' / b'c', 0 for anything else
    pub kind: u8,
    pub a: Option<f64>,
    pub b: Option<f64>,
}

/// Luau `tonumber` of a decoded JSON value (numbers, numeric strings).
fn json_number(value: Option<&Json>) -> Option<f64> {
    match value? {
        Json::Number(n) => n.as_f64(),
        Json::String(text) => {
            let text = text.trim();
            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
            }
            text.parse::<f64>().ok().filter(|_| !text.is_empty() && !text.eq_ignore_ascii_case("nan") && !text.to_ascii_lowercase().contains("inf"))
        }
        _ => None,
    }
}

impl RecordedEvent {
    fn from_json(event: &Json) -> Self {
        let kind = match event.get(0).and_then(Json::as_str) {
            Some("t") => b't',
            Some("h") => b'h',
            Some("r") => b'r',
            Some("c") => b'c',
            _ => 0,
        };
        RecordedEvent { kind, a: json_number(event.get(1)), b: json_number(event.get(2)) }
    }
}

/// Display-duration stepping's Heartbeat cursor (StepDisplayDuration's DisplayBeatClock): the
/// edit-mode beats keep their own deadlines whatever durations the display presents.
#[derive(Clone, Debug)]
struct BeatClock {
    origin: f64,
    first_interval: u64,
    start: f64,
    interval: u64,
    /// 1-based index of the next beat in `beats`
    beat: usize,
    elapsed: f64,
    duration: f64,
    beats: Vec<f64>,
}

/// How far a recorded clock was replayed (Scheduler.ReplayStatus, diagnostics).
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayStatus {
    pub events: usize,
    pub replayed: usize,
    pub captured: bool,
    pub time: f64,
    pub heartbeats: u64,
    pub frames: u64,
}

pub struct Scheduler {
    pub now: f64,
    pub frame: u64,
    /// A playing client's or a server's output lines waiting for LogService.MessageOut: it gets them after the frame's
    /// Heartbeat handlers and the deferred work they leave ([`deliver_log`])
    pub log_queue: Vec<crate::LogLine>,
    /// While MessageOut's handlers run: a line they log reaches no handler (measured)
    pub delivering_log: bool,
    pub heartbeat_count: u64,
    pub frame_duration: f64,
    /// Heartbeats per second when Heartbeat has its own clock (edit mode)
    pub heartbeat_rate: Option<f64>,
    /// Edit-mode Heartbeat beats of a 60 Hz frame, in ms, one entry per frame, cycled
    pub heartbeat_pattern: Option<Vec<Vec<f64>>>,
    pub edit_mode: bool,
    pub legacy_minimum_wait: f64,
    pub clock_origin: f64,
    pub clock_base: f64,
    pub epoch_millis: f64,
    epoch_pinned: bool,
    pub random_seed: f64,
    pub max_deferred_per_flush: usize,
    /// Set once a recorded clock reached Studio's capture point: nothing runs after it
    pub captured: bool,
    pub clock_call_allowance: u32,
    pub clock_call_quantum: f64,
    /// Clock reads in a row that found the clock where the last one left it (this phase; with a frozen clock, this outermost
    /// resumption)
    phase_clock_calls: u32,
    /// How many scheduler resumptions are running, nested (scheduler::resume)
    resume_depth: u32,
    /// What those reads past the allowance added (this phase) and the last clock they saw, without it
    clock_drift: f64,
    last_clock: f64,
    /// Script work charged since the last clock read (seconds of Studio's time, [`crate::cost`]);
    /// shared with the Instance API's C metamethods, which charge without borrowing the state
    pub work: Rc<std::cell::Cell<f64>>,
    /// Depth of the engine's own Luau that prices itself (services.luau Priced): the Instance API's charges
    /// are dropped while it is above 0, shared with the C metamethods like `work`
    pub muted: Rc<std::cell::Cell<u32>>,
    /// What clock reads have taken out of `work` so far (the profile of the work clock adds it back:
    /// [`crate::luau_profile`])
    pub work_drained: Rc<std::cell::Cell<f64>>,
    /// The frame each instance was last touched in (cost.rs COLD_TOUCH), shared with the C metamethods like `work`
    pub touched: Rc<std::cell::RefCell<crate::cost::Touched>>,
    /// The Luau VM's work counters (lua::raw::install), charged by what they cost Studio (cost::luau)
    pub vm_counters: Option<*const crate::cost::VmCounters>,
    /// Whether the VM counts cold touches (cost::cold_tracking): on from the first frame of a VM whose scripts are interpreted
    pub cold_tracking: bool,
    /// What the VM's work had cost when last charged
    vm_charged: f64,
    /// The work clock:the virtual time plus the work charged since, never behind the frame's time
    cpu: f64,
    /// How far frames that ran over their length have put the work clock ahead of the virtual time
    stretch: f64,
    /// Where the clock had reached when the current frame began (its first read starts no earlier)
    carry: f64,
    /// Whether work is charged: from the first frame on
    charging: bool,
    /// REE_FROZEN_CLOCK=1: os.clock and elapsedTime follow the frames only, as before the first frame. Work moves no clock reading,
    /// so a code change that is faster or slower leaves what a script writes from the time it reads as it was. Nor do the reads:
    /// they count against the allowance only in a row within one resumption, so only a busy loop moves the clock, and a frame's
    /// threads that each read it (copies timed on os.clock) all read the frame's time, as the bench's frame-locked clock gives
    /// them however many there are
    pub frozen_clock: bool,
    /// Roblox's script execution timeout for this VM's resumptions (watchdog.rs; installed by `ree run` / `ree record`)
    pub timeout: Option<crate::watchdog::ScriptTimeout>,
    standard_offset: f64,
    server_offset: f64,
    elapsed_time_offset: f64,

    deferred: VecDeque<Job>,
    holding: bool,
    held: Vec<Job>,
    event_depth: u32,
    flushing: u32,
    timers: BinaryHeap<Timer>,
    legacy_timers: BinaryHeap<Timer>,
    timer_sequence: u64,
    /// Cancelled threads task.cancel could not close (the running thread and the ones it resumed), held so their
    /// address is not reused by a new thread while it is listed (a closed thread is dead: [`resume`] skips it).
    cancelled: FxHashMap<usize, LuaThread>,
    /// Threads task.defer or task.delay queued that have not resumed since, by address, with their task-library state
    /// ('deferred', 'delayed'); the queued job or timer holds each one alive while it is listed
    task_states: FxHashMap<usize, &'static str>,
    pattern_cache: Vec<(f64, Vec<Vec<f64>>)>,
    hooks: Vec<(Phase, PhaseHook)>,

    /// The Luau main thread (Scheduler.MainThread): resumptions made from it are top-level
    main_thread: Option<usize>,

    // Build-time placement (recorded clock only, see place_build)
    track_build: bool,
    build_clock: bool,
    build_start_explicit: bool,
    build_work: u64,
    build_work_start: Option<u64>,
    build_work_end: Option<u64>,
    studio_build: f64,
    studio_instances: f64,

    // Recorded clock (RUR_CLOCK)
    recorded: Option<Rc<Vec<RecordedEvent>>>,
    /// 0-based index of the next event to replay
    recorded_index: usize,
    recorded_clocks: bool,
    recorded_wait: f64,
    handlers_pending: bool,
    /// A recorded clock's seed that math.random must restart from (Luau's UseRecordedClock
    /// calls math.randomseed itself; the Rust side reseeds when it next reaches Luau)
    pub reseed_pending: bool,
    /// The seed the last unseeded Random.new() took (the next one follows from it; None: the next
    /// starts from `random_seed`)
    pub unseeded_random: Option<f64>,
    /// Every Random made so far, in the order made, while it lives (Context.ReseedRandoms)
    pub randoms: crate::lua::types::RandomRegistry,

    // Display cadence
    display_interval_index: Option<u64>,
    display_beat_clock: Option<BeatClock>,
    /// What the clock's script work is scaled by for the device the run prices (cost.rs device_scale: 1 on a desktop)
    price_scale: f64,
}

/// The run's random seed: 1, or `REE_RANDOM_SEED` (another seed shows what a run draws at random:
/// math.random and unseeded Random.new() both follow it).
fn default_random_seed() -> f64 {
    std::env::var("REE_RANDOM_SEED").ok().and_then(|text| text.trim().parse().ok()).unwrap_or(1.0)
}

/// The wall clock of a frozen-clock run: 2026-09-21 14:00 UTC (an hour boundary), in Unix milliseconds.
const FROZEN_EPOCH_MILLIS: f64 = 1_789_999_200_000.0;

fn current_hour_millis() -> f64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ((seconds / 3600) * 3600 * 1000) as f64
}

/// The zone's standard offset in seconds (smaller of January and July, as tick() uses).
pub fn standard_offset_seconds() -> f64 {
    use chrono::{Local, NaiveDate, TimeZone};
    let offset_at = |month: u32| -> Option<i32> {
        let naive = NaiveDate::from_ymd_opt(2026, month, 15)?.and_hms_opt(12, 0, 0)?;
        Local.offset_from_local_datetime(&naive).single().map(|o| o.local_minus_utc())
    };
    // scheduler.luau reads January only; datatypes.luau the smaller of January and July
    offset_at(1).unwrap_or(0) as f64
}

pub fn standard_offset_min_seconds() -> f64 {
    use chrono::{Local, NaiveDate, TimeZone};
    let offset_at = |month: u32| -> Option<i32> {
        let naive = NaiveDate::from_ymd_opt(2026, month, 15)?.and_hms_opt(12, 0, 0)?;
        Local.offset_from_local_datetime(&naive).single().map(|o| o.local_minus_utc())
    };
    match (offset_at(1), offset_at(7)) {
        (Some(a), Some(b)) => a.min(b) as f64,
        (Some(a), None) | (None, Some(a)) => a as f64,
        _ => 0.0,
    }
}

impl Scheduler {
    /// Scheduler state plus Scheduler.Configure: the mode, RUR_EPOCH and RUR_CLOCK.
    pub fn new(options: &Options) -> Self {
        let muted = Rc::new(std::cell::Cell::new(0));
        let mut scheduler = Scheduler {
            now: 0.0,
            frame: 0,
            log_queue: Vec::new(),
            delivering_log: false,
            heartbeat_count: 0,
            frame_duration: 1.0 / 60.0,
            heartbeat_rate: Some(240.0),
            heartbeat_pattern: Some(vec![vec![3.5, 4.5, 4.2], vec![3.5, 3.9, 4.6]]),
            edit_mode: true,
            legacy_minimum_wait: 0.03,
            clock_origin: 10000.0,
            clock_base: 10000.0,
            epoch_millis: current_hour_millis(),
            epoch_pinned: false,
            random_seed: default_random_seed(),
            max_deferred_per_flush: 200_000,
            captured: false,
            clock_call_allowance: 256,
            clock_call_quantum: 1e-5,
            phase_clock_calls: 0,
            resume_depth: 0,
            clock_drift: 0.0,
            last_clock: f64::NEG_INFINITY,
            work: Rc::new(std::cell::Cell::new(0.0)),
            muted: muted.clone(),
            work_drained: Rc::new(std::cell::Cell::new(0.0)),
            touched: Rc::new(std::cell::RefCell::new(crate::cost::Touched::new(muted))),
            vm_counters: None,
            cold_tracking: false,
            vm_charged: 0.0,
            cpu: f64::NEG_INFINITY,
            stretch: 0.0,
            carry: f64::NEG_INFINITY,
            charging: false,
            frozen_clock: std::env::var("REE_FROZEN_CLOCK").as_deref() == Ok("1"),
            timeout: None,
            standard_offset: standard_offset_seconds(),
            server_offset: 0.0,
            elapsed_time_offset: 0.0,
            deferred: VecDeque::new(),
            holding: false,
            held: Vec::new(),
            event_depth: 0,
            flushing: 0,
            timers: BinaryHeap::new(),
            legacy_timers: BinaryHeap::new(),
            timer_sequence: 0,
            cancelled: FxHashMap::default(),
            task_states: FxHashMap::default(),
            pattern_cache: Vec::new(),
            hooks: Vec::new(),
            main_thread: None,
            track_build: false,
            build_clock: false,
            build_start_explicit: false,
            build_work: 0,
            build_work_start: None,
            build_work_end: None,
            studio_build: 0.0,
            studio_instances: 0.0,
            recorded: None,
            recorded_index: 0,
            recorded_clocks: false,
            recorded_wait: 0.0,
            handlers_pending: false,
            reseed_pending: false,
            unseeded_random: None,
            randoms: Default::default(),
            display_interval_index: None,
            display_beat_clock: None,
            // (a timing check's second run: twice the modelled work moves os.clock within each frame, record/run.rs)
            price_scale: crate::cost::device_scale(&options.device) * if options.clock_moved { 2.0 } else { 1.0 },
        };
        scheduler.edit_mode = options.edit_mode();
        // Scheduler.Configure: RUR_EPOCH=<Unix seconds or ms> pins the wall clock's start
        if let Some(epoch) = std::env::var("RUR_EPOCH").ok().and_then(|v| v.trim().parse::<f64>().ok()) {
            scheduler.epoch_millis = if epoch < 1e11 { epoch * 1000.0 } else { epoch };
            scheduler.epoch_pinned = true;
        }
        if scheduler.frozen_clock {
            scheduler.freeze_wall_clock();
        }
        if options.clock_moved {
            scheduler.frozen_clock = false;
        }
        // RUR_CLOCK=<file.json> replays a recorded Studio frame clock instead of the ideal one
        if let Ok(path) = std::env::var("RUR_CLOCK") {
            if !path.is_empty() {
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| super::lune_json::decode(&text)) {
                    // (lua::start seeds math.random from random_seed afterwards, as Configure does)
                    Ok(clock) => {
                        scheduler.use_recorded_clock(Some(&clock));
                        scheduler.reseed_pending = false;
                    }
                    Err(error) => eprintln!("rur: RUR_CLOCK {path}: {error}"),
                }
            }
        }
        scheduler
    }

    pub fn add_hook(&mut self, phase: Phase, hook: PhaseHook) {
        self.hooks.push((phase, hook));
    }

    fn hooks_of(&self, phase: Phase) -> Vec<PhaseHook> {
        self.hooks.iter().filter(|(p, _)| *p == phase).map(|(_, h)| h.clone()).collect()
    }

    /// Scheduler.MainThread: resumptions made from `thread` count as the top level (the build of
    /// a recorded clock's build-time placement). Set by the host after `lua::start`.
    pub fn set_main_thread(&mut self, thread: &LuaThread) {
        self.main_thread = Some(thread.to_pointer() as usize);
    }

    // Clocks: within one phase the clock advances only by the script work charged in it (what the
    // engine calls a script makes cost Studio, crate::cost), and never falls behind the frame's
    // virtual time. A clock that stands still (a loop that only reads it, a clock that takes no
    // charges: before the first frame, a recorded replay) advances by `clock_call_quantum` for each
    // read past `clock_call_allowance` in a row, so a busy loop ends; a clock that moves between reads
    // needs none. A frozen clock counts the reads in a row of one outermost resumption, not of the phase
    // (frozen_clock). A recorded clock replays Studio's values and takes no charges.

    fn begin_phase(&mut self) {
        self.phase_clock_calls = 0;
        self.clock_drift = 0.0;
        self.last_clock = f64::NEG_INFINITY;
    }

    /// All script work charged so far, in seconds of Studio's time (cost.rs): the engine calls' and the VM's
    /// own, whether or not a clock read took it; the same for the same run, whatever the host's speed.
    pub fn work_total(&self) -> f64 {
        let vm = self.vm_counters.map(|counters| crate::cost::luau(unsafe { &*counters })).unwrap_or(0.0);
        self.work_drained.get() + self.work.get() + vm
    }

    /// Charges `seconds` of script work to the clock (cost.rs).
    #[inline]
    pub fn charge(&self, seconds: f64) {
        if self.muted.get() == 0 {
            crate::cost::tally_charge(seconds);
            crate::boundary::charged(seconds);
            self.work.set(self.work.get() + seconds);
        }
    }

    /// Charges `seconds` the engine takes for delivering an event to a handler (the boundary profiler keeps these apart from the
    /// script's own crossings).
    #[inline]
    pub fn charge_dispatch(&self, seconds: f64) {
        if self.muted.get() == 0 {
            crate::cost::tally_charge(seconds);
            crate::boundary::dispatched(seconds);
            self.work.set(self.work.get() + seconds);
        }
    }

    /// Whether the engine's own Luau is pricing a whole call (charges are dropped)
    pub fn is_muted(&self) -> bool {
        self.muted.get() != 0
    }

    /// Charges `seconds` whether or not the engine's Luau is pricing itself (its own total).
    pub fn charge_priced(&self, seconds: f64) {
        crate::boundary::charged(seconds);
        crate::boundary::add_cost(seconds);
        self.work.set(self.work.get() + seconds);
    }

    /// The work charged since the last take: the engine calls' and the VM's own since then, in the clock's seconds (the device's
    /// scale on it: cost.rs device_scale; the totals, work_total, stay the desktop prices').
    fn take_work(&mut self) -> f64 {
        let mut work = self.work.replace(0.0);
        self.work_drained.set(self.work_drained.get() + work);
        if let Some(counters) = self.vm_counters {
            let total = crate::cost::luau(unsafe { &*counters });
            work += total - self.vm_charged;
            self.vm_charged = total;
        }
        work * self.price_scale
    }

    /// A frame begins: its first read starts from its own time, unless the work before it ran past
    /// that; then the clock is stretched by as much from there on (Studio's frame would have lasted
    /// that long; the frames themselves stay at their fixed or replayed times), so it never runs
    /// backwards.
    /// Work before the first frame (a story's or harness's build) is not charged: Studio's build
    /// takes time ree does not model (a recorded clock's build placement does).
    fn begin_frame_work(&mut self) {
        let end = self.cpu.max(self.now + self.stretch) + self.take_work();
        if self.charging {
            self.carry = self.carry.max(end);
        }
        self.charging = true;
        self.cpu = f64::NEG_INFINITY;
    }

    /// Virtual seconds since the engine started plus the script work charged (and the busy-loop
    /// drift of the current phase); during a recorded clock's build placement, the build's progress.
    pub fn elapsed(&mut self) -> f64 {
        let work = self.take_work();
        let now = if self.build_clock {
            self.build_now()
        } else if self.recorded.is_some() || !self.charging || self.frozen_clock {
            self.now
        } else {
            let base = self.now + self.stretch;
            if self.carry > base {
                self.stretch += self.carry - base;
            }
            self.carry = f64::NEG_INFINITY;
            self.cpu = (self.cpu + work).max(self.now + self.stretch);
            self.cpu
        };
        // Only a clock that stands still drifts (a loop that reads it and nothing else, or a clock that
        // takes no charges): a read that finds it moved is a read of real work, however many a phase
        // holds. Every read past 256 billed 10 us, which made a spawn replay's os.clock brackets cost
        // 7x their Studio time.
        if now > self.last_clock {
            self.last_clock = now;
            self.phase_clock_calls = 0;
        } else {
            self.phase_clock_calls += 1;
            if self.phase_clock_calls > self.clock_call_allowance {
                self.clock_drift += self.clock_call_quantum;
            }
        }
        now + self.clock_drift
    }

    /// A frozen clock also stops the wall clock at the hour the run began (tick(), os.time(), DateTime.now(), GetServerTimeNow): two
    /// runs an hour apart read the same time unless RUR_EPOCH pinned one (differential runs start whenever they are started).
    pub fn freeze_wall_clock(&mut self) {
        self.frozen_clock = true;
        if !self.epoch_pinned {
            self.epoch_millis = FROZEN_EPOCH_MILLIS;
            self.epoch_pinned = true;
        }
    }

    /// os.clock()
    pub fn clock(&mut self) -> f64 {
        self.clock_base + self.elapsed()
    }

    /// elapsedTime()
    pub fn elapsed_time(&mut self) -> f64 {
        self.clock_base + self.elapsed_time_offset + self.elapsed()
    }

    /// Whether the wall clock this scheduler took reads the same now: its hour and the zone's offset,
    /// unless pinned (a recording run ahead of its job: record/prepare.rs).
    pub fn wall_clock_current(&self) -> bool {
        self.epoch_pinned || (self.epoch_millis == current_hour_millis() && self.standard_offset == standard_offset_seconds())
    }

    /// DateTime.now() / os.time() (whole milliseconds)
    pub fn unix_millis(&mut self) -> f64 {
        (self.epoch_millis + self.elapsed() * 1000.0).floor()
    }

    /// workspace:GetServerTimeNow()
    pub fn unix_time(&mut self) -> f64 {
        (self.epoch_millis + self.elapsed() * 1000.0) / 1000.0 + self.server_offset
    }

    /// tick()
    pub fn tick(&mut self) -> f64 {
        (self.epoch_millis + self.elapsed() * 1000.0) / 1000.0 + self.standard_offset
    }

    /// Scheduler.BeginMount (the host reseeds math.random from `random_seed` right after): the
    /// wall clock is taken again when the story mounts, and a placed build starts here.
    pub fn begin_mount(&mut self) {
        if !self.epoch_pinned {
            self.epoch_millis = current_hour_millis();
        }
        if self.track_build {
            self.build_start_explicit = true;
            self.build_work_start = Some(self.build_work);
        }
    }

    // Build-time placement (recorded clock only). Progress through the story build is counted in
    // instance creations (NoteBuildWork, from every instance creation): Studio and the offline run
    // create the same instances in the same order, so the count is a deterministic stand-in for
    // the build's elapsed time. The build is the last resumption the main thread makes outside a
    // deferred flush before the first Heartbeat, unless BeginMount marks its start. In Studio the
    // calls a story makes while it is built happen over the build (fixtures/anim-mount.luau),
    // while the offline build takes no virtual time: timers created during it are placed, once it
    // is over, at the same fraction of Studio's build as the instances created before them are of
    // the build's instances; clock reads during the build use the instances Studio's build made
    // as the total.

    /// Scheduler.NoteBuildWork: an instance was created (engine::instance::create).
    #[inline]
    pub fn note_build_work(&mut self) {
        if self.track_build {
            self.build_work += 1;
        }
    }

    /// NoteBuildWork for `count` instances at once (a model imported lazily counts all of its
    /// instances when it is imported, as making them then would).
    pub fn note_build_work_many(&mut self, count: u32) {
        if self.track_build {
            self.build_work += count as u64;
        }
    }

    /// Scheduler.TracksBuild
    pub fn tracks_build(&self) -> bool {
        self.track_build
    }

    fn build_now(&self) -> f64 {
        if let (true, Some(start)) = (self.studio_instances > 0.0, self.build_work_start) {
            let fraction = ((self.build_work as f64 - start as f64) / self.studio_instances).clamp(0.0, 1.0);
            return -(1.0 - fraction) * self.studio_build;
        }
        -self.studio_build / 2.0
    }

    fn place_build(&mut self) {
        self.track_build = false;
        self.build_clock = false;
        self.now = 0.0;
        let (Some(start), Some(finish)) = (self.build_work_start, self.build_work_end) else { return };
        if self.studio_build <= 0.0 {
            return;
        }
        let studio_build = self.studio_build;
        for heap in [&mut self.timers, &mut self.legacy_timers] {
            let mut entries = std::mem::take(heap).into_vec();
            for entry in &mut entries {
                if let Some(created) = entry.created_work.take() {
                    let fraction = if finish > start {
                        ((created as f64 - start as f64) / (finish as f64 - start as f64)).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    let call = -(1.0 - fraction) * studio_build;
                    entry.due = call + (entry.due - entry.started);
                    entry.started = call;
                }
            }
            *heap = BinaryHeap::from(entries);
        }
    }

    // Deferred queue

    /// Below Roblox's cut of a chain of deferred work: events fired from deferred handlers and task.defer calls from
    /// deferred threads share one depth, and nothing deeper than [`MAX_REENTRANCY`] runs.
    pub fn can_defer_event(&self) -> bool {
        self.event_depth < MAX_REENTRANCY
    }

    pub fn defer(&mut self, target: JobTarget, args: Vec<Value>) {
        self.defer_costing(target, args, 0.0);
    }

    /// task.defer: one level deeper than the thread that calls it (a chain of them is cut at [`MAX_REENTRANCY`]).
    pub fn defer_task(&mut self, target: JobTarget, args: Vec<Value>) {
        self.event_depth += 1;
        self.defer_costing(target, args, 0.0);
        self.event_depth -= 1;
    }

    fn defer_costing(&mut self, target: JobTarget, args: Vec<Value>, resume_cost: f64) {
        self.defer_signal(target, args, resume_cost, NO_SIGNAL);
    }

    fn defer_signal(&mut self, target: JobTarget, args: Vec<Value>, resume_cost: f64, signal: SignalId) {
        let job = Job { target, args, event_depth: self.event_depth, signal, resume_cost };
        if self.holding {
            self.held.push(job);
        } else {
            self.deferred.push_back(job);
        }
    }

    pub fn defer_event(&mut self, target: JobTarget, args: Vec<Value>) {
        self.defer_event_costing(target, args, 0.0);
    }

    /// [`Self::defer_event`] for a job whose resume costs `resume_cost` seconds of script work when it runs
    pub fn defer_event_costing(&mut self, target: JobTarget, args: Vec<Value>, resume_cost: f64) {
        self.event_depth += 1;
        self.defer_costing(target, args, resume_cost);
        self.event_depth -= 1;
    }

    /// A handler (or waiting thread) of `signal`, one level deeper than what fired it; one past the cut is refused
    /// when its turn comes (run_job), as Roblox reports it for the handler it would call
    pub fn defer_handler(&mut self, target: JobTarget, args: Vec<Value>, resume_cost: f64, signal: SignalId) {
        self.event_depth += 1;
        self.defer_signal(target, args, resume_cost, signal);
        self.event_depth -= 1;
    }

    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Queued jobs (diagnostics: census.rs)
    pub fn deferred_len(&self) -> usize {
        self.deferred.len() + self.held.len()
    }

    /// The threads the scheduler will resume, with what holds each ("timer" for task.wait, "delay", "legacy"
    /// for wait / spawn / delay, "deferred") and when it is due (diagnostics: census.rs).
    pub fn visit_waiting(&self, visit: &mut dyn FnMut(&LuaThread, &'static str, f64)) {
        for (heap, legacy) in [(&self.timers, false), (&self.legacy_timers, true)] {
            for timer in heap.iter().filter(|timer| !self.is_cancelled(&timer.thread)) {
                let kind = match (legacy, timer.kind) {
                    (true, _) => "legacy",
                    (false, TimerKind::Delay) => "delay",
                    (false, _) => "timer",
                };
                visit(&timer.thread, kind, timer.due);
            }
        }
        for job in self.deferred.iter().chain(&self.held) {
            if let JobTarget::Thread(thread) = &job.target {
                visit(thread, "deferred", self.now);
            }
        }
    }

    // Timers

    fn schedule(&mut self, legacy: bool, kind: TimerKind, delay: f64, thread: LuaThread, args: Vec<Value>) {
        self.timer_sequence += 1;
        // NaN and negative delays resume at the next Heartbeat
        let wait = if delay.is_nan() { 0.0 } else { delay.max(0.0) };
        let timer = Timer {
            due: self.now + wait,
            sequence: self.timer_sequence,
            thread,
            args,
            started: self.now,
            kind,
            created_work: (self.track_build && self.heartbeat_count == 0).then_some(self.build_work),
        };
        if legacy {
            self.legacy_timers.push(timer);
        } else {
            self.timers.push(timer);
        }
    }

    /// task.delay / task.wait
    pub fn schedule_task(&mut self, kind: TimerKind, delay: f64, thread: LuaThread, args: Vec<Value>) {
        self.schedule(false, kind, delay, thread, args);
    }

    fn legacy_delay(&self, duration: Option<f64>) -> f64 {
        let wait = duration.unwrap_or(0.0);
        let wait = if wait.is_nan() { 0.0 } else { wait };
        wait.max(self.legacy_minimum_wait)
    }

    /// wait() / spawn() / delay(): resume at the first Heartbeat at least 0.03 s after the call
    pub fn schedule_legacy(&mut self, kind: TimerKind, duration: Option<f64>, thread: LuaThread) {
        let delay = self.legacy_delay(duration);
        self.schedule(true, kind, delay, thread, Vec::new());
    }

    pub fn next_timer_due(&self) -> Option<f64> {
        match (self.timers.peek(), self.legacy_timers.peek()) {
            (Some(a), Some(b)) => Some(a.due.min(b.due)),
            (Some(a), None) => Some(a.due),
            (None, Some(b)) => Some(b.due),
            (None, None) => None,
        }
    }

    /// task.cancel of a thread it could not close: the scheduler never resumes it again. Entries go once their thread
    /// is dead, so the list holds only live threads (keyed by a bare address that was never released, a new
    /// thread allocated where a collected cancelled one had been was never resumed, and its task.defer was lost).
    pub fn cancel(&mut self, thread: &LuaThread) {
        self.cancelled.retain(|_, listed| matches!(listed.status(), LuaThreadStatus::Resumable | LuaThreadStatus::Running));
        if matches!(thread.status(), LuaThreadStatus::Resumable | LuaThreadStatus::Running) {
            self.cancelled.insert(thread.to_pointer() as usize, thread.clone());
        }
    }

    /// The task-library state of a thread task.defer or task.delay queued and that has not resumed since: Roblox
    /// refuses to queue it again, `Cannot call task.defer on a thread that is already 'deferred' in the task library`
    /// (task.spawn, task.defer and task.delay alike; measured in a playing client, Studio 0.744:
    /// tools/parity/probes/task_order.luau).
    pub fn task_state(&self, thread: &LuaThread) -> Option<&'static str> {
        if self.task_states.is_empty() {
            return None;
        }
        self.task_states.get(&(thread.to_pointer() as usize)).copied()
    }

    pub fn set_task_state(&mut self, thread: &LuaThread, state: &'static str) {
        self.task_states.insert(thread.to_pointer() as usize, state);
    }

    /// The thread resumes (or was cancelled): it may be queued again.
    pub fn clear_task_state(&mut self, thread: &LuaThread) {
        if !self.task_states.is_empty() {
            self.task_states.remove(&(thread.to_pointer() as usize));
        }
    }

    pub fn is_cancelled(&self, thread: &LuaThread) -> bool {
        !self.cancelled.is_empty() && self.cancelled.contains_key(&(thread.to_pointer() as usize))
    }

    /// The Heartbeat dts of one `delta`-long frame (FrameBeats): the pattern's beats (scaled to
    /// the frame) plus a completing beat that makes their sum strictly exceed the frame (so a
    /// `> 1/120` style accumulator such as Vide's crosses its thresholds inside the frame), or
    /// uniform 1/HeartbeatRate beats.
    fn frame_beats(&mut self, delta: f64, frame_index: u64) -> Vec<f64> {
        if let Some(pattern) = self.heartbeat_pattern.clone().filter(|p| !p.is_empty()) {
            let frames = match self.pattern_cache.iter().find(|(d, _)| *d == delta) {
                Some((_, frames)) => frames.clone(),
                None => {
                    let mut frames = Vec::new();
                    for beats in &pattern {
                        let mut dts = Vec::new();
                        let mut sum = 0.0;
                        for milliseconds in beats {
                            let dt = f32r(milliseconds / 1000.0 * delta * 60.0);
                            dts.push(dt);
                            sum += dt;
                        }
                        let mut last = f32r(delta - sum);
                        while sum + last <= delta {
                            last = next_f32(last);
                        }
                        dts.push(last);
                        frames.push(dts);
                    }
                    self.pattern_cache.push((delta, frames.clone()));
                    frames
                }
            };
            return frames[(frame_index % frames.len() as u64) as usize].clone();
        }
        let count = ((delta * self.heartbeat_rate.unwrap_or(60.0) + 0.5).floor() as usize).max(1);
        vec![f32r(delta / count as f64); count]
    }

    // Recorded clock (RUR_CLOCK): a Studio run's frame clock replaces the ideal one.
    //
    // Version 2 (`rur compare`: cli/compare.mjs records it from the story mount to the capture):
    //   { version = 2, time, seed, mount = { start, end, instances }, unix, tick, server, elapsed, events }
    // with Studio's os.clock() at the start and end of the story build (its end is virtual time 0
    // here) and the instances the build left under the target, the Unix time (ms), tick(),
    // GetServerTimeNow() and elapsedTime() read at that moment, and the events in the order
    // Studio ran them:
    //   { "t", clock }                 a Heartbeat's resumption phase (read by a task.wait() loop)
    //   { "h", dt, clock }             the Heartbeat's handlers, with its float32 dt
    //   { "r", dt, clock }             a render step (RenderStepped), with its float32 dt
    //   { "c", clock, call, waited }   the capture: Studio's `task.wait(time)` (called at `call`)
    //                                  resumed in the resumption phase just logged; the run ends there
    // Virtual time is Studio's os.clock() minus the mount's, so timers, os.clock(), tick() and
    // DateTime.now() read what they read in Studio.
    // Version 1 (tools/analysis/anim_clock.py, from a probe log): { events = { { "h" | "r", dt } } },
    // dts only; virtual time advances by the Heartbeat dts.

    /// Scheduler.UseRecordedClock (None = back to the ideal clock). A recorded seed sets
    /// `random_seed` and `reseed_pending`: callers with a Lua state reseed math.random
    /// ([`use_recorded_clock`] does).
    pub fn use_recorded_clock(&mut self, clock: Option<&Json>) {
        self.display_beat_clock = None;
        self.display_interval_index = None;
        let events = match clock {
            None | Some(Json::Null) => None,
            Some(Json::Object(map)) => Some(map.get("events").unwrap_or(&Json::Null)),
            Some(other) => Some(other),
        };
        self.recorded = events.map(|events| {
            Rc::new(match events {
                Json::Array(items) => items.iter().map(RecordedEvent::from_json).collect(),
                _ => Vec::new(),
            })
        });
        self.recorded_index = 0;
        self.handlers_pending = false;
        self.captured = false;
        let Some(Json::Object(clock)) = clock else {
            self.recorded_clocks = false;
            return;
        };
        let mount = clock.get("mount").and_then(Json::as_object);
        self.recorded_clocks = json_number(clock.get("version")) == Some(2.0) && mount.is_some();
        let Some(mount) = mount.filter(|_| self.recorded_clocks) else { return };

        // Studio's clocks at the end of the mount (virtual time 0); the build before it lasted
        // studio_build seconds, and until the offline build is over clock reads follow its
        // progress (build_now)
        let mount_end = json_number(mount.get("end")).unwrap_or(self.clock_origin);
        self.clock_base = mount_end;
        self.studio_build = (mount_end - json_number(mount.get("start")).unwrap_or(mount_end)).max(0.0);
        self.studio_instances = json_number(mount.get("instances")).unwrap_or(0.0);
        if self.heartbeat_count == 0 && self.studio_build > 0.0 {
            self.track_build = true;
            self.build_clock = true;
            self.now = -self.studio_build / 2.0;
        }
        if let Some(unix) = json_number(clock.get("unix")) {
            self.epoch_millis = unix;
            self.epoch_pinned = true;
            if let Some(tick) = json_number(clock.get("tick")) {
                self.standard_offset = tick - unix / 1000.0;
            }
            if let Some(server) = json_number(clock.get("server")) {
                self.server_offset = server - unix / 1000.0;
            }
        }
        if let Some(elapsed) = json_number(clock.get("elapsed")) {
            self.elapsed_time_offset = elapsed - mount_end;
        }
        self.recorded_wait = json_number(clock.get("time")).unwrap_or(0.0);
        if let Some(seed) = json_number(clock.get("seed")) {
            self.random_seed = seed;
            self.reseed_pending = true;
        }
    }

    /// Scheduler.HasRecordedClock
    pub fn has_recorded_clock(&self) -> bool {
        self.recorded.is_some()
    }

    /// Studio's os.clock() value `clock` as virtual time (never backwards).
    fn recorded_at(&self, clock: Option<f64>) -> f64 {
        match clock {
            Some(value) => self.now.max(value - self.clock_base),
            None => self.now,
        }
    }

    /// Scheduler.HasCapturePoint: whether the recorded clock ends at a Studio capture.
    pub fn has_capture_point(&self) -> bool {
        let Some(events) = &self.recorded else { return false };
        events.iter().rev().take(3).any(|event| event.kind == b'c')
    }

    /// Scheduler.ReplayStatus
    pub fn replay_status(&self) -> Option<ReplayStatus> {
        let events = self.recorded.as_ref()?;
        Some(ReplayStatus {
            events: events.len(),
            replayed: self.recorded_index,
            captured: self.captured,
            time: self.now,
            heartbeats: self.heartbeat_count,
            frames: self.frame,
        })
    }

    fn recorded_remaining(&self) -> bool {
        self.recorded.as_ref().is_some_and(|events| self.recorded_index < events.len())
    }
}

// Script execution (these release the state borrow while scripts run)

/// The message of a Luau error as `tostring(message)` shows it (without the traceback mlua
/// appends to errors of resumed threads; tracebacks are reported separately).
pub fn error_message(error: &LuaError) -> String {
    match error {
        LuaError::RuntimeError(message) => match message.rfind("\nstack traceback:\n") {
            Some(index) => message[..index].to_string(),
            None => message.clone(),
        },
        LuaError::CallbackError { cause, .. } => error_message(cause),
        LuaError::SyntaxError { message, .. } => message.clone(),
        LuaError::WithContext { cause, .. } => error_message(cause),
        // mlua's reference stack could not hold a callback's arguments: mlua 0.12 returns this where 0.11 panicked (lua/refs.rs)
        LuaError::StackError => super::lua::refs::message(),
        other => other.to_string(),
    }
}

/// Scheduler.Resume: resumes a suspended, uncancelled thread; errors go to the output with the
/// thread's traceback. Returns whether it ran without error.
pub fn resume(lua: &Lua, engine: &Rc<Engine>, thread: &LuaThread, args: LuaMultiValue) -> bool {
    let (top_level, _entered) = {
        let mut state = engine.state.borrow_mut();
        let sched = &mut state.sched;
        if sched.is_cancelled(thread) {
            return false;
        }
        if thread.status() != LuaThreadStatus::Resumable {
            return false;
        }
        // Build-time placement: a top-level resumption before the first Heartbeat is (part of) the
        // story build
        let top_level = sched.track_build
            && sched.flushing == 0
            && sched.heartbeat_count == 0
            && sched.main_thread.is_some_and(|main| main == lua.current_thread().to_pointer() as usize);
        if top_level && !sched.build_start_explicit {
            sched.build_work_start = Some(sched.build_work);
        }
        // (a frozen clock counts the reads in a row of one outermost resumption: Scheduler::frozen_clock)
        if sched.frozen_clock && sched.resume_depth == 0 {
            sched.phase_clock_calls = 0;
        }
        sched.resume_depth += 1;
        // (the script timeout times the outermost resumption of a nest: watchdog.rs)
        (top_level, crate::watchdog::enter(sched.timeout.as_ref()))
    };
    let log_errors = super::lua::stubs::log_error_texts();
    let resumed = {
        let _resuming = super::lua::refs::Resuming::enter();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| thread.resume::<LuaMultiValue>(args)))
    };
    engine.state.borrow_mut().sched.resume_depth -= 1;
    let ok = match resumed {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            let message = error_message(&error);
            let trace = super::lua::traceback(lua, thread);
            if super::lua::stubs::uncaught_log_error(log_errors, thread, &message) {
                engine.uncaught_log_error(&message, trace);
            } else {
                engine.output("error", message, trace);
            }
            false
        }
        // mlua's reference stack ran full under this thread (lua/refs.rs): its error, and the run goes on
        Err(panic) if super::lua::refs::is_exhaustion(&*panic) => {
            let trace = super::lua::traceback(lua, thread);
            engine.output("error", super::lua::refs::message(), trace);
            false
        }
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if top_level {
        let mut state = engine.state.borrow_mut();
        state.sched.build_work_end = Some(state.sched.build_work);
    }
    ok
}

fn job_args(lua: &Lua, engine: &Rc<Engine>, args: &[Value]) -> LuaResult<LuaMultiValue> {
    let mut values = LuaMultiValue::with_capacity(args.len());
    for value in args {
        values.push_back(super::lua::convert::to_lua(lua, engine, value)?);
    }
    Ok(values)
}

/// RunHandler's Profile.BeginCallback (only with RUR_PROFILE_CALLBACKS=1): the sample of a
/// connection handler about to run, named after its signal.
pub(crate) fn sample_handler(engine: &Engine, signal: SignalId, callback: &LuaFunction) -> Option<profile::Sample> {
    if !profile::sample_callbacks() {
        return None;
    }
    profile::begin_callback(callback, || {
        engine.state.borrow().signals.get(signal).name.to_str_lossy().into_owned()
    })
}

/// The MicroProfiler label of a connection handler: its signal and where the handler is defined
/// ("Heartbeat · ui/button:42"), cached per function
pub(crate) fn handler_label(engine: &Engine, signal: SignalId, callback: &LuaFunction) -> &'static str {
    thread_local! {
        static LABELS: std::cell::RefCell<std::collections::HashMap<(usize, u64), &'static str>> = Default::default();
    }
    let key = (callback.to_pointer() as usize, signal_key(signal));
    if let Some(label) = LABELS.with(|labels| labels.borrow().get(&key).copied()) {
        return label;
    }
    let name = engine.state.borrow().signals.get(signal).name.to_str_lossy().into_owned();
    let info = callback.info();
    let source = info.short_src.or(info.source).unwrap_or_default();
    // The last two path parts are enough to find the script
    let parts: Vec<&str> = source.split(['/', '\\', '.']).filter(|part| !part.is_empty() && *part != "luau" && *part != "lua").collect();
    let short = parts[parts.len().saturating_sub(2)..].join("/");
    let line = info.line_defined.map(|line| format!(":{line}")).unwrap_or_default();
    let label = crate::microprofile::intern(&format!("{name} · {short}{line}"));
    LABELS.with(|labels| {
        let mut labels = labels.borrow_mut();
        if labels.len() > 8192 {
            labels.clear();
        }
        labels.insert(key, label);
    });
    label
}

fn signal_key<T: std::fmt::Debug>(signal: T) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    format!("{signal:?}").hash(&mut hash);
    hash.finish()
}

/// Profile.EndCallback once the handler returned (RunHandler records it after the callback; a
/// handler that yielded is not sampled here).
pub(crate) fn end_handler_sample(sample: Option<profile::Sample>, thread: &LuaThread) {
    if sample.is_some() && thread.status() != LuaThreadStatus::Resumable {
        profile::end_callback(sample);
    }
}

fn run_job(lua: &Lua, engine: &Rc<Engine>, job: Job) -> LuaResult<()> {
    if job.event_depth > MAX_REENTRANCY && job.signal != NO_SIGNAL {
        return refuse(lua, engine, job);
    }
    if job.resume_cost != 0.0 {
        engine.state.borrow().sched.charge_dispatch(job.resume_cost);
    }
    match job.target {
        JobTarget::Thread(thread) => {
            engine.state.borrow_mut().sched.clear_task_state(&thread);
            let args = job_args(lua, engine, &job.args)?;
            let _scope = crate::microprofile::scope("Resume thread");
            resume(lua, engine, &thread, args);
        }
        JobTarget::Handler(connection) => {
            let census = crate::census::handler_site(engine, connection);
            let callback = engine.state.borrow_mut().signals.begin_handler(connection);
            if let Some((callback, signal)) = callback {
                let args = job_args(lua, engine, &job.args)?;
                let sample = sample_handler(engine, signal, &callback);
                let _scope = crate::microprofile::enabled()
                    .then(|| crate::microprofile::scope(handler_label(engine, signal, &callback)));
                let thread = lua.create_thread(callback)?;
                if let Some(site) = census {
                    crate::census::handler_ran(site, &thread);
                }
                resume(lua, engine, &thread, args);
                end_handler_sample(sample, &thread);
            }
        }
        JobTarget::DisconnectAll(id) => {
            super::instance::disconnect_all(&mut engine.state.borrow_mut(), id);
        }
    }
    Ok(())
}

/// A handler past the cut of a chain does not run; Roblox logs it as an error naming the event and the handler
/// (measured in a playing client, Studio 0.744: `Maximum event re-entrancy depth exceeded for IntValue.Changed when
/// calling anonymous function on line 12 in ReplicatedStorage.Module`, the line where the function was defined).
fn refuse(lua: &Lua, engine: &Rc<Engine>, job: Job) -> LuaResult<()> {
    let label = engine.state.borrow().signals.get(job.signal).label.to_str_lossy().into_owned();
    let mut message = format!("Maximum event re-entrancy depth exceeded for {label}");
    if let JobTarget::Handler(connection) = job.target {
        let begun = engine.state.borrow_mut().signals.begin_handler(connection);
        let Some((callback, _)) = begun else { return Ok(()) };
        let info = callback.info();
        let name = info.name.filter(|name| !name.is_empty()).unwrap_or_else(|| "anonymous function".into());
        let source = info.short_src.unwrap_or_default();
        message = format!("{message} when calling {name} on line {} in {source}", info.line_defined.unwrap_or(0));
    }
    let _ = lua;
    // (through Engine::output, so LogService.MessageOut gets it as Roblox's does: tools/parity/probes/deferred_points.luau)
    engine.output("error", message, None);
    Ok(())
}

/// Scheduler.FlushDeferred: runs queued jobs (including ones queued meanwhile) in order, each
/// with the causal event depth it was queued at.
pub fn flush_deferred(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    // Profile: the "deferred" bucket, flushes (and those that had work) and jobs run
    profile::enter("deferred");
    profile::count("deferredFlushes", 1);
    if profile::active() && engine.state.borrow().sched.has_deferred() {
        profile::count("nonemptyDeferredFlushes", 1);
    }
    let mut processed = 0usize;
    let result = flush_deferred_jobs(lua, engine, &mut processed);
    profile::count("deferredJobs", processed as u64);
    profile::leave();
    result
}

/// One pass over the deferred queue: the jobs queued before it began run; what they queue waits for the next
/// resumption point. A playing client resumes this way after its task.wait / task.delay step and after the
/// render-step bindings (measured, Studio 0.744: a task.defer chain started there runs its first step there and
/// the rest after the Heartbeat or PreRender handlers; tools/parity/probes/deferred_points.luau).
pub fn flush_deferred_once(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    profile::enter("deferred");
    profile::count("deferredFlushes", 1);
    let queued = engine.state.borrow().sched.deferred.len();
    let mut processed = 0usize;
    let result = flush_deferred_upto(lua, engine, &mut processed, Some(queued));
    profile::count("deferredJobs", processed as u64);
    profile::leave();
    result
}

fn flush_deferred_jobs(lua: &Lua, engine: &Rc<Engine>, processed: &mut usize) -> LuaResult<()> {
    flush_deferred_upto(lua, engine, processed, None)
}

fn flush_deferred_upto(lua: &Lua, engine: &Rc<Engine>, processed: &mut usize, upto: Option<usize>) -> LuaResult<()> {
    engine.state.borrow_mut().sched.flushing += 1;
    loop {
        if upto.is_some_and(|upto| *processed >= upto) {
            break;
        }
        let job = {
            let mut state = engine.state.borrow_mut();
            if state.sched.deferred.is_empty() {
                break;
            }
            if *processed >= state.sched.max_deferred_per_flush {
                let now = state.sched.now;
                state.output.add(
                    "error",
                    "Scheduler: deferred queue exceeded its per-flush budget (runaway re-entrancy?)".into(),
                    None,
                    now,
                );
                break;
            }
            state.sched.deferred.pop_front().unwrap()
        };
        *processed += 1;
        let previous = {
            let mut state = engine.state.borrow_mut();
            std::mem::replace(&mut state.sched.event_depth, job.event_depth)
        };
        trace_scheduler_format(engine, || format!("deferred {} begin", *processed));
        let result = run_job(lua, engine, job);
        trace_scheduler_format(engine, || format!("deferred {} end", *processed));
        engine.state.borrow_mut().sched.event_depth = previous;
        if let Err(error) = result {
            engine.state.borrow_mut().sched.flushing -= 1;
            return Err(error);
        }
    }
    engine.state.borrow_mut().sched.flushing -= 1;
    Ok(())
}

/// How a timer step lets the deferred work of the threads it resumed run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TimerFlush {
    /// a full flush after each resumption (edit mode, as measured in Studio's edit DataModel)
    EachResumption,
    /// every due timer first, then one pass (a playing client's task.wait / task.delay step)
    OncePass,
    /// every due timer first, then a full flush (a playing client's wait / delay / spawn step)
    AfterAll,
}

/// RunTimers: resumes timers due at this Heartbeat (only those due by `limit`); timers scheduled
/// while this runs wait for the next Heartbeat.
fn run_timers(lua: &Lua, engine: &Rc<Engine>, legacy: bool, limit: Option<f64>, flush: TimerFlush) -> LuaResult<()> {
    profile::enter("timers");
    let result = run_timers_in(lua, engine, legacy, limit, flush);
    profile::leave();
    result
}

fn run_timers_in(lua: &Lua, engine: &Rc<Engine>, legacy: bool, limit: Option<f64>, flush: TimerFlush) -> LuaResult<()> {
    let (cutoff, until) = {
        let state = engine.state.borrow();
        let mut until = state.sched.now + 1e-9;
        if let Some(limit) = limit {
            if limit + 1e-9 < until {
                until = limit + 1e-9;
            }
        }
        (state.sched.timer_sequence, until)
    };
    loop {
        let timer = {
            let mut state = engine.state.borrow_mut();
            let heap = if legacy { &mut state.sched.legacy_timers } else { &mut state.sched.timers };
            match heap.peek() {
                Some(top) if top.due <= until && top.sequence <= cutoff => heap.pop().unwrap(),
                _ => break,
            }
        };
        // (cancelled entries count too: they are popped and discarded by resume)
        profile::count("timerEntries", 1);
        let elapsed = engine.state.borrow().sched.now - timer.started;
        let args = match timer.kind {
            TimerKind::Wait => LuaMultiValue::from_iter([LuaValue::Number(elapsed)]),
            TimerKind::LegacyWait => {
                let clock = engine.state.borrow_mut().sched.clock();
                LuaMultiValue::from_iter([LuaValue::Number(elapsed), LuaValue::Number(clock)])
            }
            TimerKind::Delay => {
                engine.state.borrow_mut().sched.clear_task_state(&timer.thread);
                job_args(lua, engine, &timer.args)?
            }
        };
        trace_scheduler_format(engine, || format!("timer {:?} begin", timer.kind));
        resume(lua, engine, &timer.thread, args);
        trace_scheduler_format(engine, || format!("timer {:?} end", timer.kind));
        if flush == TimerFlush::EachResumption {
            flush_deferred(lua, engine)?;
        }
    }
    match flush {
        TimerFlush::EachResumption => Ok(()),
        TimerFlush::OncePass => flush_deferred_once(lua, engine),
        TimerFlush::AfterAll => flush_deferred(lua, engine),
    }
}

fn run_hooks(lua: &Lua, engine: &Rc<Engine>, phase: Phase, delta: f64) -> LuaResult<()> {
    // HookBuckets
    profile::enter(match phase {
        Phase::Simulation | Phase::Heartbeat => "heartbeat",
        Phase::RenderEvents | Phase::PreRender | Phase::Render => "renderEvents",
        _ => "other",
    });
    let result = (|| {
        let hooks = engine.state.borrow().sched.hooks_of(phase);
        for (index, hook) in hooks.into_iter().enumerate() {
            trace_scheduler_format(engine, || format!("phase {phase:?} hook {index} begin"));
            hook(lua, engine, delta)?;
            trace_scheduler_format(engine, || format!("phase {phase:?} hook {index} end"));
            flush_deferred(lua, engine)?;
        }
        Ok(())
    })();
    profile::leave();
    result
}

/// math.random restarts from a recorded clock's seed (UseRecordedClock's math.randomseed).
fn reseed(engine: &Rc<Engine>) -> LuaResult<()> {
    let pending = std::mem::take(&mut engine.state.borrow_mut().sched.reseed_pending);
    if pending {
        super::lua::globals::randomseed(engine)?;
    }
    Ok(())
}

/// Scheduler.UseRecordedClock from a Lua context (tests, sessions): also reseeds math.random.
pub fn use_recorded_clock(engine: &Rc<Engine>, clock: Option<&Json>) -> LuaResult<()> {
    engine.state.borrow_mut().sched.use_recorded_clock(clock);
    reseed(engine)
}

/// A Heartbeat's resumption phase at virtual time `at`: legacy wait/spawn/delay resumptions,
/// then task.wait / task.delay (measured order; only those due by `limit`, if given). `delta` is
/// the Heartbeat's dt, for the running-game simulation events that precede them.
fn heartbeat_timers(lua: &Lua, engine: &Rc<Engine>, at: f64, delta: f64, limit: Option<f64>) -> LuaResult<()> {
    let edit_mode = {
        let mut state = engine.state.borrow_mut();
        state.sched.now = at;
        state.sched.heartbeat_count += 1;
        state.sched.begin_phase();
        profile::count("heartbeats", 1);
        state.sched.edit_mode
    };
    flush_deferred(lua, engine)?;
    if !edit_mode {
        // A playing client (measured, Studio 0.744: tools/parity/probes/deferred_points.luau and a wait / spawn
        // chain probe) resumes wait / delay / spawn threads after the render step's PreRender handlers and before
        // PreAnimation, every due one first and then a full flush; then the simulation events; then every due
        // task.wait / task.delay thread, in the order they were scheduled, and one pass over the deferred queue
        // before the Heartbeat handlers.
        run_timers(lua, engine, true, None, TimerFlush::AfterAll)?;
        run_hooks(lua, engine, Phase::Simulation, delta)?;
        run_timers(lua, engine, false, limit, TimerFlush::OncePass)?;
        // A server has no render step: it steps its tweens after the task.wait resumptions and before the Heartbeat
        // handlers (measured, a Studio 0.744 server's game script: tools/parity/probes/tween.luau; a tween played at a
        // resumption point has moved by the first Heartbeat and a 0.09 s tween ends at the sixth, a frame before a
        // client's), its events delivered after the Heartbeat handlers. Not measured: where among the simulation events.
        if engine.options.server() {
            step_tweens(lua, engine, delta)?;
        }
        return Ok(());
    }
    // Edit mode has no simulation step: Motor6D.Transform changes apply where it would run
    // (world::joints; play mode applies them after PreSimulation)
    super::world::joints::step_transforms(engine);
    // (animation assets still arrive: crate::animation)
    crate::animation::arrivals(engine);
    run_timers(lua, engine, true, None, TimerFlush::EachResumption)?;
    run_timers(lua, engine, false, limit, TimerFlush::EachResumption)
}

/// A server's tween step (heartbeat_timers): the tween step, then the frame's layout point, their events queued as deferred work.
fn step_tweens(lua: &Lua, engine: &Rc<Engine>, delta: f64) -> LuaResult<()> {
    run_phase(lua, engine, Phase::TweenStep, "tweens", delta)?;
    run_phase(lua, engine, Phase::Tween, "tweens", delta)
}

/// A phase's hooks with no resumption point between them.
fn run_phase(lua: &Lua, engine: &Rc<Engine>, phase: Phase, label: &'static str, delta: f64) -> LuaResult<()> {
    profile::enter(label);
    let hooks = engine.state.borrow().sched.hooks_of(phase);
    let result = hooks.into_iter().enumerate().try_for_each(|(index, hook)| {
        trace_scheduler_format(engine, || format!("{label} {phase:?} hook {index} begin"));
        let result = hook(lua, engine, delta);
        trace_scheduler_format(engine, || format!("{label} {phase:?} hook {index} end"));
        result
    });
    profile::leave();
    result
}

/// A client's tween step (with `phases` after it: the frame's layout point in edit mode), its events held back (`held`).
fn step_tweens_held(lua: &Lua, engine: &Rc<Engine>, phases: &[Phase], delta: f64) -> LuaResult<()> {
    engine.state.borrow_mut().sched.holding = true;
    let result = phases.iter().try_for_each(|&phase| run_phase(lua, engine, phase, "tweens", delta));
    engine.state.borrow_mut().sched.holding = false;
    result
}

/// A Heartbeat's handlers (after its resumptions), with delta `delta` (float32).
fn heartbeat_handlers(lua: &Lua, engine: &Rc<Engine>, delta: f64) -> LuaResult<()> {
    run_hooks(lua, engine, Phase::Heartbeat, delta)?;
    flush_deferred(lua, engine)?;
    deliver_log(lua, engine)
}

/// LogService.MessageOut on a playing client and on a server: the lines logged since the last delivery, each with its
/// MessageType and a context table, after the frame's Heartbeat handlers and the deferred work they leave, handlers called
/// later-connected first. Measured in a playing client (Studio 0.744, tools/parity/probes/logservice.luau): a warning logged
/// at a resumption point, in a deferred thread or in a Heartbeat handler arrives after that frame's Heartbeat handlers, after
/// the deferred events queued beside it; a line a MessageOut handler logs reaches no handler. A server's game script measured
/// the same order and the same dropped line. The third argument is always a table (measured, client and server: select("#")
/// is 3 for print and warn too): a LogService method's converted context, else a new empty one.
pub fn deliver_log(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let lines = std::mem::take(&mut engine.state.borrow_mut().sched.log_queue);
    if lines.is_empty() {
        return Ok(());
    }
    let Some(log) = crate::services::find_service(engine, engine.world().game, "LogService") else { return Ok(()) };
    let signal = {
        let state = engine.state.borrow();
        engine.reflection.name_id("MessageOut").and_then(|event| state.dom.at(log).signals.as_ref().and_then(|signals| signals.event(event)))
    };
    let Some(signal) = signal else { return Ok(()) };
    engine.state.borrow_mut().sched.delivering_log = true;
    let mut result = Ok(());
    for line in lines {
        let item = engine.reflection.enum_item("MessageType", match line.kind {
            "warn" => "MessageWarning",
            "error" => "MessageError",
            "info" => "MessageInfo",
            _ => "MessageOutput",
        });
        let Some(item) = item else { continue };
        let context = match line.context {
            Some(context) => context,
            None => match lua.create_table() {
                Ok(table) => Value::Lua(Box::new(LuaValue::Table(table))),
                Err(error) => {
                    result = Err(error);
                    break;
                }
            },
        };
        result = crate::lua::instance::fire_immediate(lua, engine, signal, &[Value::String(crate::value::Str::from(line.text.as_str())), Value::EnumItem(item), context]);
        if result.is_err() {
            break;
        }
    }
    engine.state.borrow_mut().sched.delivering_log = false;
    result
}

/// One Heartbeat at virtual time `at` with delta `delta` (float32).
fn heartbeat(lua: &Lua, engine: &Rc<Engine>, at: f64, delta: f64) -> LuaResult<()> {
    heartbeat_timers(lua, engine, at, delta, None)?;
    heartbeat_handlers(lua, engine, delta)
}

/// The render step: a playing client's bindings, RenderStepped / PreRender, the tween step and one resumption point; edit mode's
/// bindings, tween step and flush, then RenderStepped / PreRender (a server's tweens step in heartbeat_timers); then the render
/// hooks (input, layout).
fn render(lua: &Lua, engine: &Rc<Engine>, delta: f64) -> LuaResult<()> {
    {
        let mut state = engine.state.borrow_mut();
        state.sched.frame += 1;
        let frame = state.sched.frame;
        state.sched.touched.borrow_mut().begin_frame(frame);
        crate::boundary::begin_frame(delta);
        state.sched.begin_phase();
        profile::count("renderSteps", 1);
    }
    // The VM's tables, closures and functions go cold with the instances (cost.rs LUAU_COLD_TABLE): a VM that interprets its
    // scripts (their Luau is charged) counts their first touch in a frame from the first frame on
    let cold = engine.state.borrow().sched.cold_tracking;
    if cold {
        crate::cost::cold_frame(lua);
    } else if engine.state.borrow().sched.vm_counters.is_some() && crate::lua::modules::interprets_scripts(engine) {
        crate::cost::cold_tracking(lua, true);
        engine.state.borrow_mut().sched.cold_tracking = true;
    }
    flush_deferred(lua, engine)?;
    run_phase(lua, engine, Phase::RenderStep, "renderBindings", delta)?;
    let server = engine.options.server();
    let edit_mode = engine.state.borrow().sched.edit_mode;
    if !server && !edit_mode {
        // A playing client (measured, roblox-sim's rendered and headless clients of Studio 0.744's engine: notes/macro
        // orderprobe.luau, bindprobe.luau): the bindings (at any priority, First to Last), RenderStepped and PreRender run with
        // no resumption point between them, then the
        // tween step (a RenderStepped or PreRender handler reads a tween's value from before it, and a tween played there or in
        // a binding takes its first step that frame), then one resumption point: the tween step's events (a tweened
        // property's Changed handlers) first, then what the bindings and the RenderStepped and PreRender handlers deferred, in
        // the order they deferred it, all seeing the stepped values. The frame's layout point stays after the bindings.
        run_phase(lua, engine, Phase::Tween, "tweens", delta)?;
        run_phase(lua, engine, Phase::RenderEvents, "renderEvents", delta)?;
        step_tweens_held(lua, engine, &[Phase::TweenStep], delta)?;
        {
            let mut state = engine.state.borrow_mut();
            let held = std::mem::take(&mut state.sched.held);
            for job in held.into_iter().rev() {
                state.sched.deferred.push_front(job);
            }
        }
        flush_deferred(lua, engine)?;
    } else {
        // A server (its tweens step before its Heartbeat handlers: heartbeat_timers) and edit mode: the tween step after the
        // bindings, its events after the RenderStepped and PreRender handlers, one pass for the bindings' deferred work (edit
        // mode: a full flush)
        if !server {
            step_tweens_held(lua, engine, &[Phase::TweenStep, Phase::Tween], delta)?;
        }
        if edit_mode {
            flush_deferred(lua, engine)?;
        } else {
            flush_deferred_once(lua, engine)?;
        }
        {
            let mut state = engine.state.borrow_mut();
            let held = std::mem::take(&mut state.sched.held);
            state.sched.deferred.extend(held);
        }
        run_hooks(lua, engine, Phase::RenderEvents, delta)?;
    }
    run_hooks(lua, engine, Phase::PreRender, delta)?;
    run_hooks(lua, engine, Phase::Render, delta)?;
    flush_deferred(lua, engine)?;
    // Frees unreachable parentless instances now and then (lua/raw.rs)
    super::lua::raw::collect_if_due(lua, engine)
}

/// Replays the capture: the resumption phase in which Studio's `task.wait(time)` resumed runs
/// the legacy resumptions and the task.wait / task.delay threads due before the capture's own due
/// time (they ran before it in Studio), then everything stops.
fn capture(lua: &Lua, engine: &Rc<Engine>, at: f64, event: RecordedEvent) -> LuaResult<()> {
    let due = {
        let state = engine.state.borrow();
        match event.b {
            Some(call) => call - state.sched.clock_base + state.sched.recorded_wait,
            None => at,
        }
    };
    heartbeat_timers(lua, engine, at, 0.0, Some(due))?;
    let mut state = engine.state.borrow_mut();
    state.sched.handlers_pending = false;
    state.sched.captured = true;
    Ok(())
}

/// Replays recorded events up to and including the next render step; false when none ran (the
/// clock ended or reached its capture).
fn step_recorded(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<bool> {
    let events = {
        let mut state = engine.state.borrow_mut();
        if state.sched.track_build {
            state.sched.place_build();
        }
        state.sched.recorded.clone().unwrap_or_default()
    };
    loop {
        let (event, next, clocks, pending) = {
            let mut state = engine.state.borrow_mut();
            let sched = &mut state.sched;
            if sched.recorded_index >= events.len() || sched.captured {
                return Ok(false);
            }
            let event = events[sched.recorded_index];
            sched.recorded_index += 1;
            (event, events.get(sched.recorded_index).copied(), sched.recorded_clocks, sched.handlers_pending)
        };
        match event.kind {
            b't' => {
                let at = engine.state.borrow().sched.recorded_at(event.a);
                if let Some(next) = next.filter(|n| n.kind == b'c') {
                    engine.state.borrow_mut().sched.recorded_index += 1;
                    capture(lua, engine, at, next)?;
                    return Ok(false);
                }
                if pending {
                    heartbeat_handlers(lua, engine, 0.0)?;
                }
                let delta = match next {
                    Some(next) if next.kind == b'h' => f32r(next.a.unwrap_or(0.0)),
                    _ => 0.0,
                };
                // (the resumption phase's time is read after the pending handlers ran)
                let at = engine.state.borrow().sched.recorded_at(event.a);
                heartbeat_timers(lua, engine, at, delta, None)?;
                engine.state.borrow_mut().sched.handlers_pending = true;
            }
            b'h' => {
                let delta = f32r(event.a.unwrap_or(0.0));
                if clocks {
                    let at = engine.state.borrow().sched.recorded_at(event.b);
                    if pending {
                        let mut state = engine.state.borrow_mut();
                        state.sched.now = at;
                        state.sched.begin_phase();
                    } else {
                        heartbeat_timers(lua, engine, at, delta, None)?;
                    }
                } else {
                    let at = engine.state.borrow().sched.now + delta;
                    heartbeat_timers(lua, engine, at, delta, None)?;
                }
                engine.state.borrow_mut().sched.handlers_pending = false;
                heartbeat_handlers(lua, engine, delta)?;
            }
            b'r' => {
                if pending {
                    heartbeat_handlers(lua, engine, 0.0)?;
                    engine.state.borrow_mut().sched.handlers_pending = false;
                }
                if clocks {
                    let mut state = engine.state.borrow_mut();
                    state.sched.now = state.sched.recorded_at(event.b);
                }
                render(lua, engine, f32r(event.a.unwrap_or(0.0)))?;
                return Ok(true);
            }
            b'c' => {
                let at = engine.state.borrow().sched.recorded_at(event.a);
                capture(lua, engine, at, event)?;
                return Ok(false);
            }
            _ => {}
        }
    }
}

/// Scheduler.Step: one rendered frame of `duration` (default FrameDuration): the frame's
/// Heartbeats (their times spread over the frame, the last at its end), then its render step. A
/// recorded clock replays its next frame instead while it has one; after its capture nothing
/// runs.
pub fn step(lua: &Lua, engine: &Rc<Engine>, duration: Option<f64>) -> LuaResult<()> {
    // (a profile's timeline: one row a frame; off, one thread-local read)
    if crate::profiler::active() {
        crate::profiler::frame_begin(engine);
        let result = step_frame(lua, engine, duration);
        crate::profiler::frame_end(engine);
        return result;
    }
    step_frame(lua, engine, duration)
}

fn step_frame(lua: &Lua, engine: &Rc<Engine>, duration: Option<f64>) -> LuaResult<()> {
    trace_scheduler(engine, "frame begin");
    {
        let state = engine.state.borrow();
        if state.sched.captured {
            return Ok(());
        }
    }
    if engine.state.borrow().sched.recorded_remaining() {
        let stepped = step_recorded(lua, engine)?;
        if stepped || engine.state.borrow().sched.captured {
            return Ok(());
        }
    }
    if engine.state.borrow().sched.handlers_pending {
        heartbeat_handlers(lua, engine, 0.0)?;
        engine.state.borrow_mut().sched.handlers_pending = false;
    }
    let (start, delta, beats, display) = {
        let mut state = engine.state.borrow_mut();
        let sched = &mut state.sched;
        sched.begin_frame_work();
        let delta = duration.unwrap_or(sched.frame_duration);
        let display = sched.display_beat_clock.is_some() && sched.recorded.is_none();
        let beats = if !display && sched.edit_mode && sched.heartbeat_rate.is_some() {
            let index = sched.display_interval_index.unwrap_or(sched.frame);
            Some(sched.frame_beats(delta, index))
        } else {
            None
        };
        (sched.now, delta, beats, display)
    };
    if display {
        step_display_duration(lua, engine, delta)?;
        return Ok(());
    }
    let target = start + delta;
    trace_scheduler(engine, "heartbeat begin");
    match beats {
        Some(beats) => {
            let mut elapsed = 0.0;
            let count = beats.len();
            for (index, dt) in beats.into_iter().enumerate() {
                elapsed += dt;
                let at = if index + 1 == count { target } else { start + elapsed };
                heartbeat(lua, engine, at, dt)?;
            }
        }
        None => heartbeat(lua, engine, target, f32r(delta))?,
    }
    trace_scheduler(engine, "heartbeat end");
    engine.state.borrow_mut().sched.now = target;
    trace_scheduler(engine, "render begin");
    render(lua, engine, f32r(delta))?;
    trace_scheduler(engine, "render end");
    let mut state = engine.state.borrow_mut();
    if let Some(index) = state.sched.display_interval_index.as_mut() {
        *index += 1;
    }
    Ok(())
}

/// Scheduler.StepDisplayDuration: presents one render step after `duration` seconds of display
/// time. Display deadlines are independent of the edit-mode Heartbeat clock: the next beat is
/// retained across calls, including calls shorter than one beat and switches back to fixed
/// stepping. Returns the render steps run (0 or 1).
pub fn step_display_duration(lua: &Lua, engine: &Rc<Engine>, duration: f64) -> LuaResult<u32> {
    {
        let state = engine.state.borrow();
        if state.sched.recorded.is_some() {
            return Err(LuaError::runtime("recorded clocks require fixed-step replay"));
        }
        if state.sched.captured || !(duration > 0.0) {
            return Ok(0);
        }
    }
    if engine.state.borrow().sched.handlers_pending {
        heartbeat_handlers(lua, engine, 0.0)?;
        engine.state.borrow_mut().sched.handlers_pending = false;
    }
    let (first, beat_clock) = {
        let mut state = engine.state.borrow_mut();
        let sched = &mut state.sched;
        let first = sched.now;
        sched.begin_frame_work();
        let clock = if sched.edit_mode && sched.heartbeat_rate.is_some() {
            Some(match sched.display_beat_clock.take() {
                Some(clock) => clock,
                None => {
                    let interval = sched.display_interval_index.unwrap_or(sched.frame);
                    let beat_duration = sched.frame_duration;
                    BeatClock {
                        origin: first,
                        first_interval: interval,
                        start: first,
                        interval,
                        beat: 1,
                        elapsed: 0.0,
                        duration: beat_duration,
                        beats: sched.frame_beats(beat_duration, interval),
                    }
                }
            })
        } else {
            None
        };
        (first, clock)
    };
    let target = first + duration;
    match beat_clock {
        Some(mut clock) => {
            // DisplayBeatClock is the scheduler's while the Heartbeats run Luau; a recorded clock
            // installed meanwhile (UseRecordedClock) drops it and the rest runs on the local cursor
            engine.state.borrow_mut().sched.display_beat_clock = Some(clock.clone());
            let mut detached = false;
            loop {
                let dt = clock.beats[clock.beat - 1];
                let last = clock.beat == clock.beats.len();
                let at = if last {
                    clock.origin + (clock.interval - clock.first_interval + 1) as f64 * clock.duration
                } else {
                    clock.start + clock.elapsed + dt
                };
                if at > target + 1e-12 {
                    break;
                }
                heartbeat(lua, engine, at, dt)?;
                detached = detached || engine.state.borrow().sched.display_beat_clock.is_none();
                if last {
                    clock.interval += 1;
                    clock.start = clock.origin + (clock.interval - clock.first_interval) as f64 * clock.duration;
                    clock.beat = 1;
                    clock.elapsed = 0.0;
                    clock.beats = engine.state.borrow_mut().sched.frame_beats(clock.duration, clock.interval);
                } else {
                    clock.elapsed += dt;
                    clock.beat += 1;
                }
            }
            let mut state = engine.state.borrow_mut();
            state.sched.display_interval_index = Some(clock.interval);
            if !detached {
                state.sched.display_beat_clock = Some(clock);
            }
        }
        None => heartbeat(lua, engine, target, f32r(duration))?,
    }
    engine.state.borrow_mut().sched.now = target;
    render(lua, engine, f32r(duration))?;
    Ok(1)
}

/// Scheduler.StepDisplay: integer-interval callers keep the fixed-step API and recorded replay
/// behaviour. Returns the render steps run.
pub fn step_display(lua: &Lua, engine: &Rc<Engine>, intervals: u32) -> LuaResult<u32> {
    if engine.state.borrow().sched.recorded.is_some() {
        let before = engine.state.borrow().sched.frame;
        for _ in 0..intervals {
            step(lua, engine, None)?;
        }
        return Ok((engine.state.borrow().sched.frame - before) as u32);
    }
    let duration = intervals as f64 * engine.state.borrow().sched.frame_duration;
    step_display_duration(lua, engine, duration)
}

/// Scheduler.RunUntil: whole frames until virtual time reaches `until` (the last frame may end up
/// to one frame later: frames are never shortened). A recorded clock with a capture point runs to
/// the capture instead, wherever that falls: it is the moment Studio showed.
pub fn run_until(lua: &Lua, engine: &Rc<Engine>, until: f64) -> LuaResult<()> {
    let replay_to_capture = {
        let state = engine.state.borrow();
        state.sched.recorded.is_some() && state.sched.recorded_clocks && state.sched.has_capture_point()
    };
    if replay_to_capture {
        loop {
            {
                let state = engine.state.borrow();
                if state.sched.captured || !state.sched.recorded_remaining() {
                    break;
                }
            }
            step_recorded(lua, engine)?;
        }
        if engine.state.borrow().sched.captured {
            return Ok(());
        }
    }
    loop {
        {
            let state = engine.state.borrow();
            if state.sched.now + 1e-9 >= until || state.sched.captured {
                return Ok(());
            }
        }
        step(lua, engine, None)?;
    }
}

// Instance references held by queued work: lua::raw::collect keeps these instances alive.
impl Scheduler {
    pub fn visit_instance_refs(&self, visit: &mut dyn FnMut(InstanceId)) {
        for job in self.deferred.iter().chain(self.held.iter()) {
            if let JobTarget::DisconnectAll(id) = job.target {
                visit(id);
            }
            for value in &job.args {
                value.visit_instances(visit);
            }
        }
        for timer in self.timers.iter().chain(self.legacy_timers.iter()) {
            for value in &timer.args {
                value.visit_instances(visit);
            }
        }
    }
}

//! Hardware counters (`--profile hw`): what the CPU counted while the run thread ran (instructions, cycles, the cycles
//! it stalled waiting for load data, the L1 data cache's misses outstanding, branch mispredictions), per run, thread,
//! phase and frame, and per Luau function, calling context and call site.
//!
//! A process on Windows cannot read the CPU's counters itself (`rdpmc` faults in user mode), and a kernel trace session,
//! which can record them, needs a process with the system's profiling right, which an account's ordinary processes never
//! hold (UAC takes it out of their token). The counter service (tools/hw-counters, installed once from an elevated
//! shell) runs such a session for the asking process only: the CPU's counters ride on every context switch, and the
//! service hands back each of the process's threads' slices (protocol.rs), never another process's.
//!
//! The run thread takes a reading (client.rs [`client::Reader`]: it makes itself switch out, about 2 us) at the
//! profile's start and end, when the VM is installed, at each frame's start and end, at each `Context.ProfilePhase` mark,
//! around each of the engine's own steps (the Animator, joints, physics), around the Luau calls it samples, and where a
//! Luau thread that holds an open sampled call resumes or stops. The counts between two readings are exact
//! (readings.rs), and a reading's own counts (the median of back-to-back ones, measured at the start) are taken out of
//! every stretch.
//!
//! Functions: the VM calls the profile's hook (patches/luau/17-profile-calls.patch) as a sampled call's first instruction
//! is about to run and as it returns or an error unwinds it, so everything counted between those two readings ran inside
//! that call. Each call is drawn on its own: every call of a function whose calls take at least the threshold, one in
//! `threshold / its mean call` of a shorter one; a call inside a sampled call of its own function on the same thread is
//! read too (up to 64 such calls a sampled call), so a recursion is read as a whole and its estimate is as steady as its
//! outermost call's (drawn each on its own, the levels' corrections multiply: four levels at one in six read 12 % off in
//! a run of 3,000 calls, simulated, against 2 % so). A function's
//! first [`MIN_MEASURED`] calls are all read to learn its length; one whose calls take under [`MIN_SAMPLED_US`] is not
//! sampled (its counts are its callers', those first calls' too), and one of its calls is still read now and then, for
//! its length alone, in case its calls grow. The threshold follows call readings alone, aiming at
//! [`READINGS_A_SECOND`] between [`MIN_THRESHOLD_US`] and [`MAX_THRESHOLD_US`].
//!
//! Booking: a stretch between two readings goes to what ran in it, as the reading that opened it left things: the
//! innermost open sampled call of the innermost running Luau thread that holds one (weighted by that call's `every`),
//! else the engine step that runs, else a frame of the profile's own (before the VM; outside the sampled calls). The VM's
//! resume callbacks say where a thread holding an open sampled call resumes and stops, and a reading is taken there; a
//! thread that holds none switches without one, since what it runs is booked where its resumer's is (a coroutine resumed
//! from a function books inside that function, a thread the engine resumes outside every call). When a sampled call
//! ends, its net (its own counts and its sampled calls' corrections) times `every - 1` comes off what it ran in, which
//! held the calls like it that were not sampled. So each function's own counts are an unbiased estimate of what was
//! counted while it ran (exact where every call is read around), its total and a call site's too, and every booking adds
//! up to the run's counts. What the readings do to the code around them is not taken out: a reading's switch costs the
//! code after it some of its caches and branch history, which a sampled call pays at its start and its caller after its
//! return, so a short sampled function reads high and its callers low (docs/cli.md "Hardware counters" has the measure).
//!
//! `REE_HW_COUNTERS=mock` counts without the service (tests): each reading takes the thread's cycle count
//! (QueryThreadCycleTime) and the other counters are fixed shares of it. `REE_HW_COUNTERS=console` reads from
//! `ree-hw-counters serve` run in a console.

pub mod client;
pub mod counters;
pub mod protocol;
pub mod readings;

use rustc_hash::FxHashMap;

use super::{memory, vm};
use protocol::{MAX_COUNTERS, Slice};
use readings::Counts;

/// Booked counts: weighted, with the readings' own taken out, so fractional and possibly below zero for a stretch
pub type Figures = [f64; MAX_COUNTERS];

/// What a reading marks
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// Back-to-back readings before the start: what a reading itself costs
    Calibrate,
    Start,
    /// The VM was installed: what ran before it is the engine's start (the mount)
    Install,
    FrameBegin,
    /// A frame ended (its number)
    FrameEnd(u64),
    /// A phase mark (the reading's phase is the new one)
    Mark,
    /// A sampled Luau call begins (its index in [`Session::calls`]), and ends (it returned or an error unwound it)
    Enter(u32),
    Leave(u32),
    /// A Luau thread (its lua_State) that holds an open sampled call runs again (`fresh`: its first run, so what it held
    /// was a thread's before it at the same address), and stops (`ended`: it returned or an error ended it, so its calls
    /// are over)
    Resume { thread: usize, fresh: bool },
    Suspend { thread: usize, ended: bool },
    /// One of the engine's own steps begins (its frame's function id), and ends
    StepBegin(i32),
    StepEnd,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// In the service's clock (QueryPerformanceCounter ticks)
    pub at: i64,
    pub what: What,
    /// The phase in effect from this reading on
    pub phase: u32,
}

/// A call the VM's hook read around
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    pub function: i32,
    /// Its callers, each at the line it called from, and the function itself (line -1): the stack its counts are booked to
    pub stack: u32,
    /// Its thread (the lua_State) and its frame's depth there
    pub thread: usize,
    pub depth: i32,
    /// It stood for `every` calls of its function (each drawn with a chance of one in `every`); 0: read for its length
    /// alone, its counts its caller's
    pub every: u32,
    /// Read while its function's first calls were measured (a function then found too short to sample keeps none of
    /// them: they are its callers')
    pub learning: bool,
}

/// Where the counts come from
enum Source {
    Service { client: client::Client, reader: client::Reader },
    /// The thread's cycle count at each reading
    Mock { cycles: Vec<u64> },
}

/// How many readings measure a reading's own cost
const CALIBRATION_READINGS: usize = 64;
/// The call readings a second the sampling aims at (each takes about 2 us of the run thread)
pub const READINGS_A_SECOND: f64 = 10_000.0;
/// Calls at least this long (on average, their function's) are all sampled while the readings stay within the aim
pub const THRESHOLD_US: f64 = 100.0;
/// The threshold never falls below this
pub const MIN_THRESHOLD_US: f64 = 20.0;
/// The threshold never rises above this, so short functions keep being sampled
pub const MAX_THRESHOLD_US: f64 = 1_000.0;
/// The rate of call readings is measured over at least this many seconds before the threshold may move
const PACING_WINDOW_SECONDS: f64 = 0.1;
/// The threshold rises when call readings come faster than [`READINGS_A_SECOND`] times [`TOO_FAST`], and falls when they
/// come slower than it times [`TOO_SLOW`]
const TOO_FAST: f64 = 1.25;
const TOO_SLOW: f64 = 0.5;
/// What one move multiplies (or divides) the threshold by
const THRESHOLD_STEP: f64 = 1.5;
/// A function whose calls take less than this on average is not sampled once its first calls measured it: a reading's
/// switch costs the call that follows it part of its caches (measured on a Ryzen 9 9950X: a call of 1.2 us read around
/// took 9,000 to 12,600 cycles where its unread calls took 6,000, and re-fetched 44 to 100 lines; calls of 300,000
/// cycles paid the same few thousand, about 1 %), which would weigh too much in a shorter call. A function sampled stops
/// being sampled only when its calls fall below [`STOP_SAMPLED`] of this, so one whose calls hover near it does not flip
pub const MIN_SAMPLED_US: f64 = 10.0;
pub const STOP_SAMPLED: f64 = 0.75;
/// Calls of a function read before its length decides how it is sampled
pub const MIN_MEASURED: u32 = 4;
/// A function too short to sample has one call read for its length after this many, then twice as many each time it
/// measures short again, up to the most
pub const REMEASURE_FIRST: u32 = 64;
pub const REMEASURE_MOST: u32 = 65_536;
/// The most calls one sample stands for
const MAX_EVERY: u32 = 1 << 20;
/// About how many of a function's calls its mean length follows (each measured call weighed by the calls it stands for,
/// one at most as much as a quarter of them: a single call is one call's length, however many it stands for, and one
/// read for its length alone moves the mean a fifth of the way to it)
const MEAN_HORIZON: f64 = 64.0;
const MOST_WEIGHT: f64 = MEAN_HORIZON / 4.0;

/// lua.h `lua_Status`: the statuses an error leaves a thread in (LUA_ERRRUN to LUA_ERRERR)
const STATUS_ERRORS: std::ops::RangeInclusive<i32> = 2..=5;

/// A function's calls as the sampling has measured them
#[derive(Clone, Copy, Debug, Default)]
struct Learned {
    /// Its calls' mean length (ticks, the readings inside taken out), each measured call weighed by the calls it stood
    /// for; the weight of its first calls; how many were measured
    mean: f64,
    weight: f64,
    measured: u32,
    /// What the VM samples it with: 1 every call, n one in n, 0 none; and with none, a call read for its length after
    /// `remeasure` calls
    every: u32,
    remeasure: u32,
    /// Its fourth measured call found it too short to sample: its first calls, read to learn that, are its callers'
    /// (decided then: how it is sampled later, as its calls grow or shrink, does not move them)
    first_short: bool,
}

/// The sampling's choices
struct Pacing {
    functions: FxHashMap<i32, Learned>,
    /// Ticks: calls this long or longer are all sampled
    threshold: f64,
    frequency: f64,
    /// How many call readings have been taken. Frame, step and thread readings are not counted: raising the threshold
    /// cannot reduce them, so they must not push it up
    call_readings: usize,
    /// (when, call readings then) the rate is measured from
    since: (i64, usize),
}

impl Pacing {
    fn new(frequency: i64) -> Pacing {
        let frequency = frequency.max(1) as f64;
        Pacing { functions: FxHashMap::default(), threshold: THRESHOLD_US * 1e-6 * frequency, frequency, call_readings: 0, since: (0, 0) }
    }

    /// Moves the threshold toward the aimed rate of call readings, within its bounds: up when they come too fast, down
    /// when too slow. Returns whether the threshold changed (`at` is the time of the latest reading, in ticks)
    fn adjust_threshold(&mut self, at: i64) -> bool {
        let (window_start, call_readings_then) = self.since;
        let seconds = (at - window_start) as f64 / self.frequency;
        if seconds < PACING_WINDOW_SECONDS {
            return false;
        }
        let rate = (self.call_readings - call_readings_then) as f64 / seconds;
        self.since = (at, self.call_readings);

        let lowest = MIN_THRESHOLD_US * 1e-6 * self.frequency;
        let highest = MAX_THRESHOLD_US * 1e-6 * self.frequency;
        let new_threshold = if rate > READINGS_A_SECOND * TOO_FAST {
            (self.threshold * THRESHOLD_STEP).min(highest)
        } else if rate < READINGS_A_SECOND * TOO_SLOW {
            (self.threshold / THRESHOLD_STEP).max(lowest)
        } else {
            return false;
        };
        // (it may already sit at a bound)
        let changed = new_threshold != self.threshold;
        self.threshold = new_threshold;
        changed
    }

    /// One in how many calls of `mean` ticks are sampled, when they are
    fn every_for(&self, mean: f64) -> u32 {
        ((self.threshold / mean.max(1.0)).round() as u32).clamp(1, MAX_EVERY)
    }

    /// How a function measured as `learned` is sampled from now on: (every, remeasure). Each of its first calls is read;
    /// then, too short, none (one read for its length after `remeasure` calls, twice as many each time it is short again).
    /// Sampled, it stops only below [`STOP_SAMPLED`] of the shortest sampled
    fn sampling(&self, learned: &Learned) -> (u32, u32) {
        if learned.measured < MIN_MEASURED {
            return (1, 0);
        }
        let shortest = MIN_SAMPLED_US * 1e-6 * self.frequency;
        let sampled = learned.every > 0 && learned.measured > MIN_MEASURED;
        let short = learned.mean < if sampled { shortest * STOP_SAMPLED } else { shortest };
        match (short, learned.every) {
            (true, 0) if learned.remeasure > 0 => (0, learned.remeasure.saturating_mul(2).min(REMEASURE_MOST)),
            (true, _) => (0, REMEASURE_FIRST),
            (false, _) => (self.every_for(learned.mean), 0),
        }
    }

    /// Whether `function` is too short to sample now (its first calls measured it)
    fn unsampled(&self, function: i32) -> bool {
        self.functions.get(&function).is_some_and(|learned| learned.measured >= MIN_MEASURED && learned.every == 0)
    }
}

/// A call read around, still running
#[derive(Clone, Copy, Debug)]
struct Open {
    call: u32,
    depth: i32,
    /// Its entry reading's time and index
    at: i64,
    reading: usize,
}

/// A profile's hardware counters while it runs
pub struct Session {
    source: Option<Source>,
    /// The counters' names (the profile's) and the profile sources the service counts for them
    pub names: Vec<String>,
    sources: Vec<String>,
    error: Option<String>,
    notes: Vec<String>,
    readings: Vec<Reading>,
    /// The calls read around, and the stacks they are booked to
    pub calls: Vec<Call>,
    pub stacks: memory::Stacks,
    /// The calls read around still open, by Luau thread
    open: FxHashMap<usize, Vec<Open>>,
    pacing: Pacing,
    /// A reading's own host time (ticks), measured at the start
    reading_ticks: f64,
    /// The OS thread the profile runs on
    thread: u32,
    frequency: i64,
    in_step: bool,
    ended: bool,
    /// Scratch for a stack's frames
    functions: Vec<i32>,
    lines: Vec<i32>,
    /// The run thread pinned to its CPU while the service counts (client.rs [`client::pin_here`])
    pinned: Option<client::Pinned>,
}

#[cfg(test)]
thread_local! {
    /// A test's `REE_HW_COUNTERS` for this thread (the variable is the process's: tests run side by side)
    pub static TEST_MODE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// `REE_HW_COUNTERS`: where the counts come from (the installed service unless it names `mock` or `console`)
fn mode() -> String {
    #[cfg(test)]
    if let Some(mode) = TEST_MODE.with(std::cell::Cell::get) {
        return mode.into();
    }
    std::env::var("REE_HW_COUNTERS").unwrap_or_default().trim().to_ascii_lowercase()
}

/// The counter a service error says the CPU does not offer ("the CPU offers no counter X (it offers A, B)": X), not the
/// ones it lists as offered
fn missing_counter_source(error: &str) -> Option<&str> {
    let after_prefix = error.strip_prefix("the CPU offers no counter ")?;
    after_prefix.split_whitespace().next()
}

/// A service error that names one of ree's own counters (an AMD event the service registers) the service does not offer
/// comes from a service installed before that counter: it says how to install the service again
fn with_reinstall_hint(error: String, sources: &[String]) -> String {
    let Some(missing_source) = missing_counter_source(&error) else { return error };
    let missing = counters::KNOWN.iter().find(|known| {
        let is_ree_counter = known.custom.is_some();
        let was_asked_for = sources.iter().any(|source| source == known.source);
        is_ree_counter && was_asked_for && known.source == missing_source
    });
    match missing {
        Some(known) => format!("{error}: the counter service was installed before ree counted {} ({}); run tools/hw-counters/setup.ps1 again (docs/cli.md \"Hardware counters\"), or name other counters with --profile-counters", known.name, known.source),
        None => error,
    }
}

/// The names of a `--profile-counters` value: at most [`MAX_COUNTERS`], none twice (an old name is read as its new one)
pub fn parse_counters(text: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = text.split(',').map(str::trim).filter(|name| !name.is_empty()).map(|name| counters::canonical(name).to_string()).collect();
    if names.is_empty() {
        return Err("--profile-counters: no counter named".into());
    }
    if names.len() > MAX_COUNTERS {
        return Err(format!("--profile-counters: {} counters, at most {MAX_COUNTERS} (the CPU counts six at once)", names.len()));
    }
    for (index, name) in names.iter().enumerate() {
        if names[..index].contains(name) {
            return Err(format!("--profile-counters: {name} twice"));
        }
    }
    Ok(names)
}

impl Session {
    /// A session that counts nothing yet
    fn new(names: &[String]) -> Session {
        let frequency = client::qpc_frequency();
        Session {
            source: None,
            names: names.to_vec(),
            sources: names.iter().map(|name| protocol::source_of(name).to_string()).collect(),
            error: None,
            notes: Vec::new(),
            readings: Vec::new(),
            calls: Vec::new(),
            stacks: memory::Stacks::default(),
            open: FxHashMap::default(),
            pacing: Pacing::new(frequency),
            reading_ticks: 0.0,
            thread: client::current_thread_id(),
            frequency,
            in_step: false,
            ended: false,
            functions: Vec::new(),
            lines: Vec::new(),
            pinned: None,
        }
    }

    /// Starts counting `names` on this thread (the run thread); what failed is reported, never fatal
    pub fn begin(names: &[String]) -> Session {
        let mut session = Session::new(names);
        let started = match mode().as_str() {
            "mock" => Ok(Source::Mock { cycles: Vec::new() }),
            mode => {
                let endpoint = if mode == "console" { client::Endpoint::Console } else { client::Endpoint::Service };
                // (the run thread pinned to its CPU and the reader's helper with it, so a reading switches between them on
                // one CPU and the run thread keeps its caches: client.rs pin_here; `REE_HW_PIN=0` leaves both free)
                let pin = if std::env::var("REE_HW_PIN").is_ok_and(|value| value.trim() == "0") { None } else { client::pin_here() };
                let helper = pin.as_ref().map(|(_, mask)| *mask);
                session.pinned = pin.map(|(pinned, _)| pinned);
                // (the reader's thread first: the service counts every thread the process has from its start on)
                client::Reader::with_helper_on(helper).and_then(|reader| {
                    let mut client = client::Client::connect(endpoint)?;
                    let started = client.start(&session.sources, &[session.thread])?;
                    session.sources = started.sources;
                    session.notes = started.notes;
                    session.frequency = (started.qpc_frequency as i64).max(1);
                    session.pacing = Pacing::new(session.frequency);
                    Ok(Source::Service { client, reader })
                })
            }
        };
        match started {
            Ok(source) => session.source = Some(source),
            Err(error) => {
                session.error = Some(with_reinstall_hint(error, &session.sources));
                session.pinned = None;
            }
        }
        for _ in 0..CALIBRATION_READINGS {
            session.read(What::Calibrate, 0);
        }
        // (a reading's own host time: the median gap of the back-to-back readings)
        let mut gaps: Vec<i64> = session.readings.windows(2).map(|pair| pair[1].at - pair[0].at).collect();
        gaps.sort_unstable();
        session.reading_ticks = gaps.get(gaps.len() / 2).copied().unwrap_or(0) as f64;
        session.read(What::Start, 0);
        session.pacing.since = (session.readings.last().map_or(0, |reading| reading.at), 0);
        session
    }

    /// Whether counting runs
    pub fn counting(&self) -> bool {
        self.source.is_some() && !self.ended
    }

    /// Why counting did not start
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Takes a reading (nothing when counting did not start or has ended)
    #[inline]
    pub fn read(&mut self, what: What, phase: u32) {
        if self.ended {
            return;
        }
        let at = match self.source.as_mut() {
            Some(Source::Service { reader, .. }) => reader.read(),
            Some(Source::Mock { cycles }) => {
                cycles.push(super::cpu::thread_cycles());
                // (the mock's slices end a tick after their reading: each reading gets a tick of its own)
                let last = self.readings.last().map_or(i64::MIN, |reading| reading.at);
                client::qpc().max(last.saturating_add(2))
            }
            None => return,
        };
        self.readings.push(Reading { at, what, phase });
        let is_call_reading = matches!(what, What::Enter(_) | What::Leave(_));
        if is_call_reading {
            self.pacing.call_readings += 1;
        }
    }

    /// One of the engine's own steps begins (`Some(frame id)`) or ends
    pub fn step(&mut self, step: Option<i32>, phase: u32) {
        match step {
            Some(id) => {
                self.in_step = true;
                self.read(What::StepBegin(id), phase);
            }
            None if self.in_step => {
                self.in_step = false;
                self.read(What::StepEnd, phase);
            }
            None => {}
        }
    }

    /// The VM's `preresume`: a Luau thread is about to run. A reading when it holds an open sampled call: what runs from
    /// here is that call's, not its resumer's
    ///
    /// # Safety
    /// `state` is a live thread of the VM this session profiles.
    pub unsafe fn resume(&mut self, state: *mut mlua::ffi::lua_State, phase: u32) {
        let thread = state as usize;
        if !self.counting() || self.open.get(&thread).is_none_or(Vec::is_empty) {
            return;
        }
        // (a thread's first run: what a thread at the same address left open, a coroutine dropped while it held a
        // sampled call, is over)
        let fresh = unsafe { vm::depth(state) } == 0;
        if fresh {
            self.open.remove(&thread);
        }
        self.read(What::Resume { thread, fresh }, phase);
    }

    /// The VM's `postresume`: a Luau thread stopped (it yielded, returned, or an error ended it). A reading when it holds
    /// an open sampled call: what runs from here is its resumer's
    ///
    /// # Safety
    /// `state` is a live thread of the VM this session profiles.
    pub unsafe fn suspend(&mut self, state: *mut mlua::ffi::lua_State, phase: u32) {
        let thread = state as usize;
        if !self.counting() || self.open.get(&thread).is_none_or(Vec::is_empty) {
            return;
        }
        let status = unsafe { vm::status(state) };
        let ended_in_error = STATUS_ERRORS.contains(&status);
        let returned = status == 0 && unsafe { vm::depth(state) } == 0;
        let ended = ended_in_error || returned;
        if ended {
            self.open.remove(&thread);
        }
        self.read(What::Suspend { thread, ended }, phase);
    }

    /// The VM's hook around a call it reads (vm::CALL_ENTER, vm::CALL_LEAVE): a reading, the stack it is booked to, and
    /// the sampling's lesson from its length
    pub fn call(&mut self, state: *mut mlua::ffi::lua_State, event: i32, function: i32, every: i32, depth: i32, phase: u32) {
        if !self.counting() {
            return;
        }
        let thread = state as usize;
        if event == vm::CALL_ENTER {
            let stack = self.call_stack(state, function);
            let index = self.calls.len() as u32;
            let learning = self.pacing.functions.get(&function).is_none_or(|learned| learned.measured < MIN_MEASURED);
            self.calls.push(Call { function, stack, thread, depth, every: every.max(0) as u32, learning });
            self.read(What::Enter(index), phase);
            let (at, reading) = (self.readings.last().map_or(0, |reading| reading.at), self.readings.len() - 1);
            let open = self.open.entry(thread).or_default();
            // (calls an error unwound outside a protected call are gone: their frames were at this depth or deeper)
            while open.last().is_some_and(|call| call.depth >= depth) {
                open.pop();
            }
            open.push(Open { call: index, depth, at, reading });
        } else {
            let open = self.open.entry(thread).or_default();
            while open.last().is_some_and(|call| call.depth > depth) {
                open.pop();
            }
            let Some(entered) = open.last().copied().filter(|call| call.depth == depth) else { return };
            open.pop();
            self.read(What::Leave(entered.call), phase);
            // (a reading's time is taken before its switch: the call's span holds its entry reading's switch and every
            // reading inside it, which are not the call's)
            let leave = self.readings.len() - 1;
            let inside = leave.saturating_sub(entered.reading + 1);
            let ticks = (self.readings[leave].at - entered.at) as f64 - (inside + 1) as f64 * self.reading_ticks;
            // (a call stands for its `every` calls; one read for its length alone, for the calls since the last)
            let every = self.calls[entered.call as usize].every;
            let stands_for = if every > 0 { every } else { self.pacing.functions.get(&function).map_or(1, |learned| learned.remeasure.max(1)) };
            self.learn(state, function, ticks.max(0.0), stands_for as f64);
        }
        self.pace(state);
    }

    /// The stack a call read around is booked to: its callers at the lines they called from, then the function itself
    /// with no line (the stretches between readings in a call span its lines)
    fn call_stack(&mut self, state: *mut mlua::ffi::lua_State, function: i32) -> u32 {
        if self.functions.len() < memory::MAX_DEPTH {
            self.functions.resize(memory::MAX_DEPTH, 0);
            self.lines.resize(memory::MAX_DEPTH, 0);
        }
        let depth = vm::stack(state, &mut self.functions, &mut self.lines);
        let mut frames: Vec<memory::Frame> = (0..depth).map(|index| (self.functions[index], self.lines[index])).collect();
        match frames.first_mut() {
            Some(top) => *top = (function, -1),
            None => frames.push((function, -1)),
        }
        self.stacks.intern(&frames)
    }

    /// A call of `function` took `ticks` and stood for `weight` calls: its function's mean (of its first calls, then
    /// over about [`MEAN_HORIZON`] calls; weighed, so the inner calls of a recursion read with their outer one do not
    /// pull it their way), and the VM's sampling of it when that moves
    fn learn(&mut self, state: *mut mlua::ffi::lua_State, function: i32, ticks: f64, weight: f64) {
        let learned = self.pacing.functions.entry(function).or_insert(Learned { mean: 0.0, weight: 0.0, measured: 0, every: 1, remeasure: 0, first_short: false });
        learned.measured += 1;
        let weight = weight.min(MOST_WEIGHT);
        if learned.measured <= MIN_MEASURED {
            learned.weight += weight;
            learned.mean += (ticks - learned.mean) * weight / learned.weight;
        } else {
            let keep = (1.0 - 1.0 / MEAN_HORIZON).powf(weight);
            learned.mean = learned.mean * keep + ticks * (1.0 - keep);
        }
        let now = *learned;
        let (every, remeasure) = self.pacing.sampling(&now);
        if now.measured == MIN_MEASURED {
            self.pacing.functions.get_mut(&function).unwrap().first_short = every == 0;
        }
        if (every, remeasure) != (now.every, now.remeasure) {
            let learned = self.pacing.functions.get_mut(&function).unwrap();
            learned.every = every;
            learned.remeasure = remeasure;
            unsafe { vm::set_call_every(state, function, every, remeasure) };
        }
    }

    /// Keeps call readings near [`READINGS_A_SECOND`], within the threshold's bounds
    fn pace(&mut self, state: *mut mlua::ffi::lua_State) {
        let Some(last) = self.readings.last() else { return };
        if !self.pacing.adjust_threshold(last.at) {
            return;
        }
        // (the functions still being learned keep every call read; those too short keep their remeasuring: the threshold
        // moves only the others)
        let changes: Vec<(i32, u32)> = self
            .pacing
            .functions
            .iter()
            .filter(|(_, learned)| learned.measured >= MIN_MEASURED && learned.every > 0)
            .filter_map(|(&function, learned)| {
                let every = self.pacing.every_for(learned.mean);
                (every != learned.every).then_some((function, every))
            })
            .collect();
        for (function, every) in changes {
            self.pacing.functions.get_mut(&function).unwrap().every = every;
            unsafe { vm::set_call_every(state, function, every, 0) };
        }
    }

    /// The profile's last reading (once); the run thread is free again
    pub fn end(&mut self, phase: u32) {
        self.read(What::End, phase);
        self.ended = true;
        self.pinned = None;
    }

    /// Whether a call read around is booked to its caller rather than as a call of its own: read for its length alone, or
    /// among its function's first calls when those found it too short to sample
    fn transparent(&self, call: &Call) -> bool {
        call.every == 0 || (call.learning && self.pacing.functions.get(&call.function).is_some_and(|learned| learned.first_short))
    }

    /// Stops counting and books what was counted: the report
    pub fn finish(mut self, phase: u32) -> Report {
        if !self.ended {
            self.end(phase);
        }
        let threshold_us = self.pacing.threshold / self.pacing.frequency * 1e6;
        let functions = self.pacing.functions.values().filter(|learned| learned.measured > 0).count();
        let unsampled = self.pacing.functions.keys().filter(|&&function| self.pacing.unsampled(function)).count();
        let transparent = self.calls.iter().filter(|call| self.transparent(call)).count();
        let switch_readings = self.readings.iter().filter(|reading| matches!(reading.what, What::Resume { .. } | What::Suspend { .. })).count();
        let mut report = Report {
            source: match self.source {
                Some(Source::Mock { .. }) => "mock",
                Some(Source::Service { .. }) => "service",
                None => "none",
            },
            names: self.names.clone(),
            sources: self.sources.clone(),
            error: self.error.take(),
            notes: std::mem::take(&mut self.notes),
            readings: self.readings.len(),
            calls: self.calls.len(),
            transparent_calls: transparent,
            switch_readings,
            sampled_functions: functions,
            unsampled_functions: unsampled,
            threshold_us,
            reading_us: self.reading_ticks / self.frequency.max(1) as f64 * 1e6,
            // (the calls' stacks: the booking adds its own frames to them)
            stacks: std::mem::take(&mut self.stacks),
            ..Report::default()
        };
        let results = match self.source.take() {
            Some(Source::Service { client, reader }) => {
                drop(reader);
                client.stop()
            }
            Some(Source::Mock { cycles }) => Ok(mock_results(&self.names, &self.readings, &cycles, self.thread)),
            None => return report,
        };
        match results {
            Ok(results) => book(&mut report, &self, results),
            Err(error) => report.error = Some(format!("the counter service's results: {error}")),
        }
        report
    }
}

/// The mock's counters for a window of `cycles` of the thread's cycles: each a fixed share of them
pub fn mock_count(name: &str, cycles: u64) -> u64 {
    match counters::canonical(name) {
        "cycles" => cycles,
        "instructions" => cycles * 2,
        "load-stall-cycles" => cycles / 4,
        "op-stall-cycles" => cycles / 3,
        "no-retire-cycles" => cycles / 2,
        "miss-buffer-occupancy" => cycles / 5,
        "miss-buffer-allocs" => cycles / 500,
        "miss-buffer-allocs-ls" => cycles / 600,
        "miss-buffer-allocs-prefetch" => cycles / 3000,
        "ls-dispatched" => cycles / 2,
        "demand-fills" => cycles / 50,
        "demand-fills-beyond-l2" => cycles / 400,
        "demand-fills-beyond-l3" => cycles / 3200,
        "branches" => cycles / 8,
        "branch-misses" => cycles / 800,
        _ => cycles / 100,
    }
}

/// The mock's results: the run thread's slices end a tick after each reading, as a reading's switch would
fn mock_results(names: &[String], readings: &[Reading], cycles: &[u64], thread: u32) -> protocol::Results {
    let mut slices = Vec::with_capacity(readings.len());
    for index in 1..readings.len().min(cycles.len()) {
        let spent = cycles[index].saturating_sub(cycles[index - 1]);
        let mut counts = [0u64; MAX_COUNTERS];
        for (count, name) in counts.iter_mut().zip(names) {
            *count = mock_count(name, spent);
        }
        slices.push(Slice { cpu: 0, start: readings[index - 1].at + 1, end: readings[index].at + 1, counts });
    }
    protocol::Results { counters: names.len() as u8, threads: vec![protocol::Thread { id: thread, registered: true, slices }], ..protocol::Results::default() }
}

/// A thread's counts over the profile
#[derive(Clone, Debug, Default)]
pub struct ThreadCounts {
    pub id: u32,
    pub name: Option<String>,
    pub run_thread: bool,
    pub slices: usize,
    pub counts: Figures,
}

/// A frame's counts, from its start to its end
#[derive(Clone, Debug, Default)]
pub struct FrameCounts {
    pub frame: u64,
    pub phase: u32,
    pub host_ms: f64,
    pub counts: Figures,
}

/// What the counters counted, booked
#[derive(Default)]
pub struct Report {
    /// `service`, `mock`, or `none` when counting did not start
    pub source: &'static str,
    pub names: Vec<String>,
    pub sources: Vec<String>,
    /// Why there are no counts
    pub error: Option<String>,
    pub notes: Vec<String>,
    /// Seconds captured before the service's slice limit; the rest of the run has no counter figures
    pub cut_short: Option<f64>,
    /// The run thread from the start reading to the end one, and the host time between them
    pub run: Figures,
    pub run_seconds: f64,
    /// A reading's own counts (the median of back-to-back ones), taken out of every stretch between readings, and its
    /// own host time
    pub per_reading: Option<Counts>,
    pub reading_us: f64,
    pub readings: usize,
    /// Readings the run thread switched out for (their counts split exactly there)
    pub matched: usize,
    /// The calls read around; of them, those booked to their callers (read for their length alone, or while learning a
    /// function then found too short to sample); the readings where a Luau thread holding a sampled call resumed or
    /// stopped
    pub calls: usize,
    pub transparent_calls: usize,
    pub switch_readings: usize,
    /// The functions whose calls were measured, and of them those too short to sample at the end; the threshold at the
    /// end (us): calls this long were all sampled
    pub sampled_functions: usize,
    pub unsampled_functions: usize,
    pub threshold_us: f64,
    pub threads: Vec<ThreadCounts>,
    /// By phase id
    pub phases: Vec<(u32, Figures)>,
    pub frames: Vec<FrameCounts>,
    /// (phase, stack) -> counts: the run's counts booked to the sampled calls' stacks (each call's function without a
    /// line, its callers at their call sites), the engine's steps and what ran outside the sampled calls
    pub by_stack: FxHashMap<(u32, u32), Figures>,
    pub stacks: memory::Stacks,
    pub switches: u64,
    pub without_counters: u64,
    pub lost_events: u32,
    pub lost_buffers: u32,
}

/// The engine's start before the VM ran, and what ran outside the sampled Luau calls: frames of their own
pub const BEFORE_VM: &str = "ree before the VM: the mount and the engine's start";
pub const OUTSIDE_CALLS: &str = "ree outside sampled Luau calls: the engine's other work and Luau calls too short to sample";
/// The outside frame's name in profiles before the calls were sampled this way (the viewer and `profile diff` read both)
pub const OUTSIDE_CALLS_BEFORE: &str = "ree outside Luau calls: the engine's own work between them";

fn add(total: &mut Figures, counts: &Figures, weight: f64) {
    for (total, count) in total.iter_mut().zip(counts) {
        *total += count * weight;
    }
}

/// What a stretch is booked to
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// A frame of the profile's own (before the VM, an engine step, outside the sampled calls)
    Stack(u32),
    /// A sampled call (its index in the calls)
    Call(u32),
}

/// What runs, each inside the one before it: a Luau thread (its lua_State), or one of the engine's steps (its frame)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Running {
    Thread(usize),
    Step(u32),
}

/// A sampled call while the booking walks the readings
#[derive(Clone, Copy, Debug)]
struct Booked {
    every: f64,
    stack: u32,
    /// What it ran in when it began: its correction goes there
    parent: Ctx,
    /// What was booked to it directly, and its own sampled calls' corrections
    net: Figures,
    open: bool,
}

/// The booking's state as it walks the readings
struct Booking<'a> {
    calls: &'a [Call],
    transparent: Vec<bool>,
    booked: FxHashMap<u32, Booked>,
    /// Each Luau thread's open sampled calls, innermost last
    open: FxHashMap<usize, Vec<u32>>,
    running: Vec<Running>,
    /// What runs outside every thread and step: before the VM, then outside the sampled calls
    base: u32,
    by_stack: FxHashMap<(u32, u32), Figures>,
}

impl Booking<'_> {
    /// What runs now is booked to: the innermost open sampled call of the innermost running thread that holds one, unless
    /// an engine step runs inside it; else the base. A thread holding none runs where its resumer did.
    fn target(&self) -> Ctx {
        for running in self.running.iter().rev() {
            match *running {
                Running::Step(stack) => return Ctx::Stack(stack),
                Running::Thread(thread) => {
                    if let Some(&call) = self.open.get(&thread).and_then(|calls| calls.last()) {
                        return Ctx::Call(call);
                    }
                }
            }
        }
        Ctx::Stack(self.base)
    }

    /// Books `counts` to `ctx` in `phase`: a frame takes them as they are; a call `every` times, into its net while it is
    /// open; a call already over (a coroutine's call outlived the call it began in) also passes on what its end would have
    /// taken off its parent, so the bookings still add up
    fn book(&mut self, mut ctx: Ctx, mut counts: Figures, phase: u32) {
        loop {
            let (stack, weight) = match ctx {
                Ctx::Stack(stack) => (stack, 1.0),
                Ctx::Call(call) => (self.booked[&call].stack, self.booked[&call].every),
            };
            if counts.iter().any(|&count| count != 0.0) {
                add(self.by_stack.entry((phase, stack)).or_insert([0.0; MAX_COUNTERS]), &counts, weight);
            }
            let Ctx::Call(call) = ctx else { return };
            let booked = self.booked.get_mut(&call).unwrap();
            if booked.open {
                add(&mut booked.net, &counts, 1.0);
                return;
            }
            if booked.every <= 1.0 {
                return;
            }
            counts = counts.map(|count| -(booked.every - 1.0) * count);
            ctx = booked.parent;
        }
    }

    /// A sampled call ends: its net, `every - 1` times, comes off what it ran in, which held the calls like it that were
    /// not sampled (a call read every time has nothing to take off)
    fn close(&mut self, call: u32, phase: u32) {
        let booked = self.booked.get_mut(&call).unwrap();
        booked.open = false;
        if booked.every > 1.0 {
            let correction = booked.net.map(|value| -(booked.every - 1.0) * value);
            let parent = booked.parent;
            self.book(parent, correction, phase);
        }
    }

    /// Closes `thread`'s innermost open calls while `over` says their depth is, innermost first
    fn close_while(&mut self, thread: usize, phase: u32, over: impl Fn(i32) -> bool) {
        while let Some(&call) = self.open.get(&thread).and_then(|calls| calls.last()) {
            if !over(self.calls[call as usize].depth) {
                break;
            }
            self.open.get_mut(&thread).unwrap().pop();
            self.close(call, phase);
        }
    }

    /// An event of `thread`'s: it runs. Threads above it stopped without a reading, which a thread does only holding no
    /// open call; a thread not running was resumed holding none, inside what runs now. (Above an engine step it runs
    /// again inside the step.)
    fn runs(&mut self, thread: usize) {
        match self.running.iter().rposition(|running| *running == Running::Thread(thread)) {
            Some(at) if !self.running[at + 1..].iter().any(|running| matches!(running, Running::Step(_))) => self.running.truncate(at + 1),
            _ => self.running.push(Running::Thread(thread)),
        }
    }
}

/// Whether the service's notes say it dropped slices at its limit for one client (the note `tools/hw-counters` writes)
fn slice_limit_reached(notes: &[String]) -> bool {
    notes.iter().any(|note| note.starts_with("the counter service keeps at most ") && note.contains(" slices a client and dropped "))
}

/// The readings that come before the end of the run thread's last saved slice: the ones whose switches the service kept
fn readings_with_saved_slices<'a>(readings: &'a [Reading], run_thread: &protocol::Thread) -> &'a [Reading] {
    let last_saved = run_thread.slices.iter().map(|slice| slice.end).max().unwrap_or(i64::MIN);
    let kept = readings.partition_point(|reading| reading.at <= last_saved);
    &readings[..kept]
}

/// Sets the report's reading and call counts to those of `readings`, the part of the run the service counted
fn count_readings(report: &mut Report, session: &Session, readings: &[Reading]) {
    report.readings = readings.len();
    report.calls = readings.iter().filter(|reading| matches!(reading.what, What::Enter(_))).count();
    report.transparent_calls = readings
        .iter()
        .filter(|reading| match reading.what {
            What::Enter(call) => session.transparent(&session.calls[call as usize]),
            _ => false,
        })
        .count();
    report.switch_readings = readings.iter().filter(|reading| matches!(reading.what, What::Resume { .. } | What::Suspend { .. })).count();
}

/// Books the service's slices to the run, its threads, phases, frames and stacks
fn book(report: &mut Report, session: &Session, results: protocol::Results) {
    let mut readings = session.readings.as_slice();
    report.switches = results.switches;
    report.without_counters = results.without_counters;
    report.lost_events = results.lost_events;
    report.lost_buffers = results.lost_buffers;
    let hit_slice_limit = slice_limit_reached(&results.notes);
    report.notes.extend(results.notes);
    let Some(run) = results.threads.iter().find(|thread| thread.id == session.thread) else {
        report.error = Some("the counter service saw no slice of the run thread".into());
        return;
    };
    if hit_slice_limit {
        // Past the limit the service saved no slices, so the readings after the last saved one have no counts: book
        // only the run before them
        readings = readings_with_saved_slices(readings, run);
        count_readings(report, session, readings);
        // (marked cut short even if no run is left in the readings; the seconds are set below when there is)
        report.cut_short = Some(0.0);
    }
    let at: Vec<i64> = readings.iter().map(|reading| reading.at).collect();
    // (a reading's switch comes microseconds after it; a millisecond is far)
    let split = readings::split(&at, &run.slices, session.frequency / 1000);
    report.matched = split.matched;
    let Some(start) = readings.iter().position(|reading| reading.what == What::Start) else { return };
    let end = readings.iter().rposition(|reading| reading.what == What::End).unwrap_or(readings.len().saturating_sub(1)).max(start);
    report.run_seconds = (readings[end].at - readings[start].at) as f64 / session.frequency as f64;
    if hit_slice_limit {
        report.cut_short = Some(report.run_seconds);
    }
    // a reading's own cost: the windows between the back-to-back readings, whose median is taken out of every window
    let calibration: Vec<&Counts> = (1..start).map(|index| &split.windows[index]).collect();
    let mut own = [0.0f64; MAX_COUNTERS];
    if !calibration.is_empty() {
        let mut median = [0u64; MAX_COUNTERS];
        for (counter, value) in median.iter_mut().enumerate() {
            let mut values: Vec<u64> = calibration.iter().map(|counts| counts[counter]).collect();
            values.sort_unstable();
            *value = values[values.len() / 2];
        }
        report.per_reading = Some(median);
        own = median.map(|value| value as f64);
    }
    let window = |index: usize| -> Figures { std::array::from_fn(|counter| split.windows[index][counter] as f64 - own[counter]) };
    // the run's other threads: what ended between the run thread's start and end switches (no readings of theirs)
    let (from, to) = (readings[start].at, readings[end].at);
    for thread in &results.threads {
        let mut counts = [0.0; MAX_COUNTERS];
        let mut slices = 0;
        if thread.id == session.thread {
            for index in start + 1..=end {
                add(&mut counts, &window(index), 1.0);
            }
            slices = run.slices.iter().filter(|slice| slice.end > from && slice.end <= to + session.frequency / 1000).count();
        } else {
            for slice in thread.slices.iter().filter(|slice| slice.end > from && slice.end <= to) {
                add(&mut counts, &slice.counts.map(|count| count as f64), 1.0);
                slices += 1;
            }
        }
        report.threads.push(ThreadCounts { id: thread.id, name: client::thread_name(thread.id), run_thread: thread.id == session.thread, slices, counts });
    }
    report.threads.sort_by(|a, b| b.run_thread.cmp(&a.run_thread).then(b.counts[0].total_cmp(&a.counts[0])));
    if let Some(run_thread) = report.threads.iter().find(|thread| thread.run_thread) {
        report.run = run_thread.counts;
    }
    // phases, frames and stacks: each window between the start and the end readings, in order
    let mut phases: FxHashMap<u32, Figures> = FxHashMap::default();
    let mut frame: Option<(i64, u32, Figures)> = None;
    let mut stacks = std::mem::take(&mut report.stacks);
    let mut synthetic: FxHashMap<i32, u32> = FxHashMap::default();
    let mut frame_of = |stacks: &mut memory::Stacks, id: i32| *synthetic.entry(id).or_insert_with(|| stacks.intern(&[(id, -1)]));
    let outside = frame_of(&mut stacks, super::report::outside_function(OUTSIDE_CALLS));
    let before_vm = frame_of(&mut stacks, super::report::outside_function(BEFORE_VM));
    let calls = &session.calls;
    let mut booking = Booking {
        calls,
        transparent: calls.iter().map(|call| session.transparent(call)).collect(),
        booked: FxHashMap::default(),
        open: FxHashMap::default(),
        running: Vec::new(),
        base: before_vm,
        by_stack: std::mem::take(&mut report.by_stack),
    };
    for index in start + 1..=end {
        let (opened, here) = (&readings[index - 1], &readings[index]);
        let counts = window(index);
        let phase = opened.phase;
        add(phases.entry(phase).or_insert([0.0; MAX_COUNTERS]), &counts, 1.0);
        if opened.what == What::FrameBegin {
            frame = Some((opened.at, opened.phase, [0.0; MAX_COUNTERS]));
        }
        if let Some((_, _, frame_counts)) = frame.as_mut() {
            add(frame_counts, &counts, 1.0);
        }
        if let What::FrameEnd(number) = here.what
            && let Some((began, frame_phase, frame_counts)) = frame.take()
        {
            report.frames.push(FrameCounts { frame: number, phase: frame_phase, host_ms: (here.at - began) as f64 * 1e3 / session.frequency as f64, counts: frame_counts });
        }
        // the stretch is what ran as the reading that opened it left things
        let target = booking.target();
        booking.book(target, counts, phase);
        // then this reading's event
        match here.what {
            What::Install => booking.base = outside,
            What::StepBegin(id) => {
                let stack = frame_of(&mut stacks, id);
                booking.running.push(Running::Step(stack));
            }
            What::StepEnd => {
                if let Some(at) = booking.running.iter().rposition(|running| matches!(running, Running::Step(_))) {
                    booking.running.truncate(at);
                }
            }
            What::Enter(call) => {
                let record = calls[call as usize];
                booking.runs(record.thread);
                // (calls an error unwound without a word: their frames were at this depth or deeper)
                booking.close_while(record.thread, phase, |depth| depth >= record.depth);
                if !booking.transparent[call as usize] {
                    let parent = booking.target();
                    booking.booked.insert(call, Booked { every: record.every.max(1) as f64, stack: record.stack, parent, net: [0.0; MAX_COUNTERS], open: true });
                    booking.open.entry(record.thread).or_default().push(call);
                }
            }
            What::Leave(call) => {
                let record = calls[call as usize];
                booking.runs(record.thread);
                booking.close_while(record.thread, phase, |depth| depth > record.depth);
                if booking.open.get(&record.thread).and_then(|calls| calls.last()) == Some(&call) {
                    booking.open.get_mut(&record.thread).unwrap().pop();
                    booking.close(call, phase);
                }
            }
            What::Resume { thread, fresh } => {
                if fresh {
                    booking.close_while(thread, phase, |_| true);
                }
                booking.running.retain(|running| *running != Running::Thread(thread));
                booking.running.push(Running::Thread(thread));
            }
            What::Suspend { thread, ended } => {
                if ended {
                    booking.close_while(thread, phase, |_| true);
                }
                if let Some(at) = booking.running.iter().rposition(|running| *running == Running::Thread(thread)) {
                    booking.running.truncate(at);
                }
            }
            What::Calibrate | What::Start | What::FrameBegin | What::FrameEnd(_) | What::Mark | What::End => {}
        }
    }
    // calls still open at the end (a coroutine left suspended) close there
    let last_phase = readings[end].phase;
    let threads: Vec<usize> = booking.open.keys().copied().collect();
    for thread in threads {
        booking.close_while(thread, last_phase, |_| true);
    }
    report.by_stack = booking.by_stack;
    report.stacks = stacks;
    let mut phases: Vec<(u32, Figures)> = phases.into_iter().collect();
    phases.sort_by_key(|(phase, _)| *phase);
    report.phases = phases;
}

/// The derived figures of a set of counts (counters.rs [`counters::figures`])
pub fn figures(names: &[String], counts: &Figures) -> Vec<counters::Figure> {
    counters::figures(names, &counts[..names.len().min(MAX_COUNTERS)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(at: i64, what: What, phase: u32) -> Reading {
        Reading { at, what, phase }
    }

    fn counts(value: u64) -> Counts {
        let mut counts = [0; MAX_COUNTERS];
        counts[0] = value;
        counts
    }

    fn call(function: i32, stack: u32, thread: usize, depth: i32, every: u32) -> Call {
        Call { function, stack, thread, depth, every, learning: false }
    }

    /// Runs the booking over made-up readings: window k (reading k-1 to k) counts `values[k]`
    fn booked(session: &mut Session, sequence: &[(What, u32)], values: &[u64]) -> Report {
        session.readings = sequence.iter().enumerate().map(|(index, &(what, phase))| reading(100 * (index as i64 + 1), what, phase)).collect();
        let slices: Vec<Slice> = (0..session.readings.len()).map(|k| Slice { cpu: 0, start: 100 * k as i64 + 2, end: 100 * (k as i64 + 1) + 1, counts: counts(values[k]) }).collect();
        let thread = session.thread;
        let results = protocol::Results { counters: 1, threads: vec![protocol::Thread { id: thread, registered: true, slices }], ..Default::default() };
        let mut report = Report { names: session.names.clone(), ..Report::default() };
        report.stacks = std::mem::take(&mut session.stacks);
        book(&mut report, session, results);
        report
    }

    /// The readings a test books: the calibration, the start and the install, then `events` each with the count of the
    /// window it closes, then the end (whose window counts `last`)
    fn run(session: &mut Session, events: &[(What, u64)], last: u64) -> Report {
        let mut sequence = vec![(What::Calibrate, 0), (What::Calibrate, 0), (What::Start, 0), (What::Install, 0)];
        let mut values = vec![0, 0, 0, 0];
        for &(what, value) in events {
            sequence.push((what, 0));
            values.push(value);
        }
        sequence.push((What::End, 0));
        values.push(last);
        booked(session, &sequence, &values)
    }

    fn stack_total(report: &Report, stack: u32) -> f64 {
        report.by_stack.iter().filter(|((_, s), _)| *s == stack).map(|(_, counts)| counts[0]).sum()
    }

    fn frame_total(report: &Report, label: &str) -> f64 {
        let id = super::super::report::outside_function(label);
        report.stacks.stacks.iter().position(|frames| frames.as_ref() == [(id, -1)]).map_or(0.0, |stack| stack_total(report, stack as u32))
    }

    fn total(report: &Report) -> f64 {
        report.by_stack.values().map(|counts| counts[0]).sum()
    }

    /// Every count of the run lands once in the run, its phase and its frame; a reading's own counts come out of each
    /// window; the sampled calls take what ran while each was the innermost open one, the engine's step its own, before
    /// the VM and outside the calls frames of their own, and the stacks add up to the run
    #[test]
    fn counts_are_booked_once_to_the_run_its_phases_frames_and_calls() {
        let mut session = Session::new(&["cycles".into()]);
        let outer = session.stacks.intern(&[(1, -1)]);
        let inner = session.stacks.intern(&[(2, -1), (1, 7)]);
        session.calls = vec![call(1, outer, 10, 1, 1), call(2, inner, 10, 2, 1)];
        let sequence = [
            (What::Calibrate, 0),
            (What::Calibrate, 0),
            (What::Calibrate, 0),
            (What::Start, 0),
            (What::Install, 0),
            (What::Mark, 1),
            (What::FrameBegin, 1),
            (What::Enter(0), 1),
            (What::Enter(1), 1),
            (What::Leave(1), 1),
            (What::Leave(0), 1),
            (What::StepBegin(-1_000_000_000), 1),
            (What::StepEnd, 1),
            (What::FrameEnd(1), 1),
            (What::End, 1),
        ];
        // a reading's own count is 1 (the calibration windows, and the one before the start); window k counts 1 + 10 k
        let values: Vec<u64> = (0..sequence.len()).map(|k| if k <= 3 { 1 } else { 1 + 10 * k as u64 }).collect();
        let report = booked(&mut session, &sequence, &values);
        let net = |k: usize| (10 * k) as f64;
        assert_eq!(report.per_reading.unwrap()[0], 1, "the median of the back-to-back windows");
        let run: f64 = (4..=14).map(net).sum();
        assert_eq!(report.run[0], run, "the run, each window's reading taken out");
        assert_eq!(report.phases, vec![(0, [net(4) + net(5), 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]), (1, std::array::from_fn(|c| if c == 0 { (6..=14).map(net).sum() } else { 0.0 }))]);
        assert_eq!(report.frames.len(), 1);
        assert_eq!(report.frames[0].counts[0], (7..=13).map(net).sum::<f64>(), "the frame: from its begin (reading 6) to its end (reading 13)");
        let step = report.stacks.stacks.iter().position(|frames| frames.as_ref() == [(-1_000_000_000, -1)]).unwrap() as u32;
        assert_eq!(frame_total(&report, BEFORE_VM), net(4), "before the VM: the start to the install");
        assert_eq!(stack_total(&report, outer), net(8) + net(10), "the outer call: its entry to the inner's, the inner's return to its own");
        assert_eq!(stack_total(&report, inner), net(9), "the inner call: its entry to its return");
        assert_eq!(stack_total(&report, step), net(12), "the engine step");
        assert_eq!(frame_total(&report, OUTSIDE_CALLS), net(5) + net(6) + net(7) + net(11) + net(13) + net(14), "outside the calls: marks, frames and the end");
        assert_eq!(total(&report), run, "the stacks add up to the run");
    }

    /// A call sampled one time in `every` stands for its function's calls that were not, and comes off the call they ran
    /// in: the estimate is right on average, and every booking still adds up to the run's counts
    #[test]
    fn a_sampled_call_stands_for_those_not_sampled_and_the_bookings_add_up() {
        // a parent call (every 2) holds two calls of a child, the second sampled (every 3); the child's net is 30, the
        // parent's own 5 + 10 (the unsampled child's 10 is in the parent's windows)
        let mut session = Session::new(&["cycles".into()]);
        let parent = session.stacks.intern(&[(1, -1)]);
        let child = session.stacks.intern(&[(2, -1), (1, 3)]);
        session.calls = vec![call(1, parent, 10, 1, 2), call(2, child, 10, 2, 3)];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 15), (What::Leave(1), 30), (What::Leave(0), 0)], 0);
        // the child: 30 times 3; the parent: (15 + child's correction -(3-1) 30) times 2 = (15 - 60) 2 = -90; the outside:
        // the parent's correction -(2-1)(15 - 60) = +45
        assert_eq!(stack_total(&report, child), 90.0);
        assert_eq!(stack_total(&report, parent), -90.0);
        assert_eq!(frame_total(&report, OUTSIDE_CALLS), 45.0);
        assert_eq!(total(&report), report.run[0], "the bookings add up to the run exactly, sampled or not");
        assert_eq!(report.run[0], 45.0);
    }

    /// The estimate is unbiased: over every way the sampling can fall, each function gets its own counts on average
    #[test]
    fn the_sampled_estimate_is_right_on_average() {
        // the example worked by hand: the parent's own 5, its child's 10; the parent sampled with every 2, the child 3
        let mut expected = [0.0f64; 3];
        for (parent_sampled, child_sampled, chance) in [(true, true, 1.0 / 6.0), (true, false, 1.0 / 3.0), (false, true, 1.0 / 6.0), (false, false, 1.0 / 3.0)] {
            let mut session = Session::new(&["cycles".into()]);
            let parent = session.stacks.intern(&[(1, -1)]);
            let child = session.stacks.intern(&[(2, -1), (1, 3)]);
            let mut events = Vec::new();
            let mut calls = Vec::new();
            if parent_sampled {
                calls.push(call(1, parent, 10, 1, 2));
                events.push((What::Enter(0), 0));
            }
            if child_sampled {
                calls.push(call(2, child, 10, 2, 3));
                let index = calls.len() as u32 - 1;
                events.push((What::Enter(index), 5));
                events.push((What::Leave(index), 10));
            }
            if parent_sampled {
                events.push((What::Leave(0), if child_sampled { 0 } else { 15 }));
            }
            // (with neither sampled, the parent's 15 runs between the install and the end)
            let last = if parent_sampled || child_sampled { 0 } else { 15 };
            session.calls = calls;
            let report = run(&mut session, &events, last);
            expected[0] += chance * stack_total(&report, parent);
            expected[1] += chance * stack_total(&report, child);
            expected[2] += chance * frame_total(&report, OUTSIDE_CALLS);
            assert!((total(&report) - 15.0).abs() < 1e-9, "every way adds up to the run: {}", total(&report));
        }
        assert!((expected[0] - 5.0).abs() < 1e-9 && (expected[1] - 10.0).abs() < 1e-9 && expected[2].abs() < 1e-9, "{expected:?}");
    }

    /// A function calling itself: each call is drawn on its own, so the nested call is as likely sampled as the outer
    /// one and the function's estimate is right on average (one draw for each run of `every` calls took exactly one of
    /// the two, and read 25 where the function had 15)
    #[test]
    fn nested_calls_of_one_function_are_each_drawn_on_their_own() {
        // f's outer call has 5 of its own, the f it calls 10; both drawn with every 2
        let mut expected = 0.0;
        for (outer_sampled, inner_sampled) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut session = Session::new(&["cycles".into()]);
            let outer = session.stacks.intern(&[(1, -1)]);
            let inner = session.stacks.intern(&[(1, -1), (1, 4)]);
            let mut events = Vec::new();
            let mut calls = Vec::new();
            if outer_sampled {
                calls.push(call(1, outer, 10, 1, 2));
                events.push((What::Enter(0), 0));
            }
            if inner_sampled {
                calls.push(call(1, inner, 10, 2, 2));
                let index = calls.len() as u32 - 1;
                events.push((What::Enter(index), 5));
                events.push((What::Leave(index), 10));
            }
            if outer_sampled {
                events.push((What::Leave(0), if inner_sampled { 0 } else { 15 }));
            }
            let last = if outer_sampled || inner_sampled { 0 } else { 15 };
            session.calls = calls;
            let report = run(&mut session, &events, last);
            let f = stack_total(&report, outer) + stack_total(&report, inner);
            expected += 0.25 * f;
            assert!((total(&report) - 15.0).abs() < 1e-9, "each way adds up to the run: {}", total(&report));
        }
        assert!((expected - 15.0).abs() < 1e-9, "f's own on average: {expected}");
    }

    /// A recursion's inner call is read with its sampled outer one (the VM tells the hook every 1), and drawn on its own
    /// when the outer one was not: the estimate is still right on average
    #[test]
    fn a_recursions_inner_call_read_with_its_outer_one_keeps_the_estimate_right() {
        // f's outer call has 5 of its own, the f it calls 10; the outer drawn with every 2
        let mut expected = 0.0;
        for (chance, outer_sampled, inner_every) in [(0.5, true, Some(1)), (0.25, false, Some(2)), (0.25, false, None)] {
            let mut session = Session::new(&["cycles".into()]);
            let outer = session.stacks.intern(&[(1, -1)]);
            let inner = session.stacks.intern(&[(1, -1), (1, 4)]);
            let mut events = Vec::new();
            let mut calls = Vec::new();
            if outer_sampled {
                calls.push(call(1, outer, 10, 1, 2));
                events.push((What::Enter(0), 0));
            }
            if let Some(every) = inner_every {
                calls.push(call(1, inner, 10, 2, every));
                let index = calls.len() as u32 - 1;
                events.push((What::Enter(index), 5));
                events.push((What::Leave(index), 10));
            }
            if outer_sampled {
                events.push((What::Leave(0), if inner_every.is_some() { 0 } else { 15 }));
            }
            let last = if outer_sampled || inner_every.is_some() { 0 } else { 15 };
            session.calls = calls;
            let report = run(&mut session, &events, last);
            expected += chance * (stack_total(&report, outer) + stack_total(&report, inner));
            assert!((total(&report) - 15.0).abs() < 1e-9, "each way adds up to the run: {}", total(&report));
        }
        assert!((expected - 15.0).abs() < 1e-9, "f's own on average: {expected}");
    }

    /// A coroutine's work is its own calls', wherever the readings around it fall: the main thread's call resumes a
    /// coroutine whose body yields, calls a function, resumes it again; each stretch the body ran goes to the body, the
    /// rest to the caller
    #[test]
    fn a_coroutine_books_what_it_ran_to_its_own_calls() {
        let (main, coroutine) = (10, 20);
        let mut session = Session::new(&["cycles".into()]);
        let chunk = session.stacks.intern(&[(1, -1)]);
        let body = session.stacks.intern(&[(2, -1)]);
        let work = session.stacks.intern(&[(3, -1), (1, 5)]);
        session.calls = vec![call(1, chunk, main, 1, 1), call(2, body, coroutine, 1, 1), call(3, work, main, 2, 1)];
        let report = run(
            &mut session,
            &[
                (What::Enter(0), 0),
                // the chunk's own, then the coroutine's first resume (no reading: it held nothing) and its body's entry
                (What::Enter(1), 1),
                // the body's loop, then its yield
                (What::Suspend { thread: coroutine, ended: false }, 100),
                (What::Enter(2), 2),
                (What::Leave(2), 100),
                (What::Resume { thread: coroutine, fresh: false }, 3),
                (What::Suspend { thread: coroutine, ended: false }, 100),
                (What::Leave(0), 4),
            ],
            0,
        );
        assert_eq!(stack_total(&report, body), 200.0, "the body: both its runs");
        assert_eq!(stack_total(&report, work), 100.0);
        assert_eq!(stack_total(&report, chunk), 10.0, "the chunk: its own between them");
        // (the body stays open at the end, suspended: it closes there, every 1, nothing to take off)
        assert_eq!(total(&report), 310.0);
    }

    /// Two threads the engine resumes in turn, each holding its loop's call: each stretch goes to the thread that ran it,
    /// not to the one whose reading closed it
    #[test]
    fn threads_switching_book_each_stretch_to_the_thread_that_ran_it() {
        let (a, b) = (10, 20);
        let mut session = Session::new(&["cycles".into()]);
        let inline = session.stacks.intern(&[(1, -1)]);
        let calling = session.stacks.intern(&[(2, -1)]);
        let compute = session.stacks.intern(&[(3, -1), (2, 4)]);
        session.calls = vec![call(1, inline, a, 1, 1), call(2, calling, b, 1, 1), call(3, compute, b, 2, 1), call(3, compute, b, 2, 1)];
        let report = run(
            &mut session,
            &[
                // a's loop, then it waits
                (What::Enter(0), 1),
                (What::Suspend { thread: a, ended: false }, 50),
                // b calls compute, then waits
                (What::Enter(1), 2),
                (What::Enter(2), 1),
                (What::Leave(2), 50),
                (What::Suspend { thread: b, ended: false }, 1),
                // a again: its loop's stretch closes at b's next reading
                (What::Resume { thread: a, fresh: false }, 3),
                (What::Suspend { thread: a, ended: false }, 50),
                (What::Resume { thread: b, fresh: false }, 4),
                (What::Enter(3), 1),
                (What::Leave(3), 50),
                (What::Suspend { thread: b, ended: false }, 1),
            ],
            5,
        );
        assert_eq!(stack_total(&report, inline), 100.0, "a's loop, both runs");
        assert_eq!(stack_total(&report, compute), 100.0);
        assert_eq!(stack_total(&report, calling), 4.0, "b's own around its calls");
        assert_eq!(frame_total(&report, OUTSIDE_CALLS), 1.0 + 2.0 + 3.0 + 4.0 + 5.0, "the engine between the threads");
    }

    /// A thread an error ended holds no call: its calls close at its stop, and what follows is its resumer's
    #[test]
    fn a_thread_an_error_ended_closes_its_calls() {
        let thread = 20;
        let mut session = Session::new(&["cycles".into()]);
        let body = session.stacks.intern(&[(2, -1)]);
        let inner = session.stacks.intern(&[(3, -1), (2, 9)]);
        session.calls = vec![call(2, body, thread, 1, 1), call(3, inner, thread, 2, 1)];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 7), (What::Suspend { thread, ended: true }, 11)], 13);
        assert_eq!(stack_total(&report, body), 7.0);
        assert_eq!(stack_total(&report, inner), 11.0);
        assert_eq!(frame_total(&report, OUTSIDE_CALLS), 13.0, "after the error, the engine's");
    }

    /// A fresh thread at an address a dropped coroutine had: what that one held is over, the new thread's work its own
    #[test]
    fn a_fresh_thread_drops_what_its_address_held() {
        let (main, thread) = (10, 20);
        let mut session = Session::new(&["cycles".into()]);
        let chunk = session.stacks.intern(&[(1, -1)]);
        let old = session.stacks.intern(&[(2, -1)]);
        session.calls = vec![call(1, chunk, main, 1, 1), call(2, old, thread, 1, 1)];
        // the chunk resumes a coroutine whose body yields and is never resumed; a new thread at its address runs
        let report = run(
            &mut session,
            &[(What::Enter(0), 0), (What::Enter(1), 1), (What::Suspend { thread, ended: false }, 20), (What::Resume { thread, fresh: true }, 2), (What::Leave(0), 30)],
            0,
        );
        assert_eq!(stack_total(&report, old), 20.0, "the old body: until its yield");
        assert_eq!(stack_total(&report, chunk), 1.0 + 2.0 + 30.0, "the new thread's work holds no call of its own: the chunk's, which resumed it");
    }

    /// Calls read for their length alone, and the first calls of a function then found too short to sample, are their
    /// callers': their stretches go to the call they ran in, and they take nothing off it
    #[test]
    fn calls_read_only_to_measure_belong_to_their_callers() {
        let mut session = Session::new(&["cycles".into()]);
        let caller = session.stacks.intern(&[(1, -1)]);
        let short = session.stacks.intern(&[(2, -1), (1, 3)]);
        // function 2 measured too short to sample: its first calls (learning) and a later one read for its length
        session.pacing.functions.insert(2, Learned { mean: 1.0, weight: 4.0, measured: MIN_MEASURED, every: 0, remeasure: REMEASURE_FIRST, first_short: true });
        let learning = Call { learning: true, ..call(2, short, 10, 2, 1) };
        session.calls = vec![call(1, caller, 10, 1, 1), learning, call(2, short, 10, 2, 0)];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 1), (What::Leave(1), 10), (What::Enter(2), 2), (What::Leave(2), 10), (What::Leave(0), 3)], 0);
        assert_eq!(stack_total(&report, short), 0.0, "nothing of the short function's own");
        assert_eq!(stack_total(&report, caller), 26.0, "its calls are in its caller");
        // a function whose first calls measured long keeps them
        let mut session = Session::new(&["cycles".into()]);
        let caller = session.stacks.intern(&[(1, -1)]);
        let long = session.stacks.intern(&[(2, -1), (1, 3)]);
        session.pacing.functions.insert(2, Learned { mean: 1e9, weight: 4.0, measured: MIN_MEASURED, every: 1, remeasure: 0, first_short: false });
        session.calls = vec![call(1, caller, 10, 1, 1), Call { learning: true, ..call(2, long, 10, 2, 1) }];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 1), (What::Leave(1), 10), (What::Leave(0), 3)], 0);
        assert_eq!(stack_total(&report, long), 10.0);
        assert_eq!(stack_total(&report, caller), 4.0);
        // and keeps them when its calls shrink later and it is not sampled at the end: its first calls were long
        let mut session = Session::new(&["cycles".into()]);
        let caller = session.stacks.intern(&[(1, -1)]);
        let build = session.stacks.intern(&[(2, -1), (1, 3)]);
        session.pacing.functions.insert(2, Learned { mean: 1.0, weight: 4.0, measured: 20_000, every: 0, remeasure: REMEASURE_MOST, first_short: false });
        session.calls = vec![call(1, caller, 10, 1, 1), Call { learning: true, ..call(2, build, 10, 2, 1) }];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 1), (What::Leave(1), 600), (What::Leave(0), 3)], 0);
        assert_eq!(stack_total(&report, build), 600.0, "its long first call is its own");
        assert_eq!(stack_total(&report, caller), 4.0);
    }

    /// A coroutine's sampled call that outlives the call it began in: its correction still reaches what that call ran
    /// in, and the bookings add up
    #[test]
    fn a_call_that_outlives_its_parent_still_adds_up() {
        let (main, coroutine) = (10, 20);
        let mut session = Session::new(&["cycles".into()]);
        let starter = session.stacks.intern(&[(1, -1)]);
        let body = session.stacks.intern(&[(2, -1)]);
        // the starter (every 2) resumes a coroutine whose body (every 3) yields; the starter returns; the engine resumes
        // the coroutine later and the body returns
        session.calls = vec![call(1, starter, main, 1, 2), call(2, body, coroutine, 1, 3)];
        let report = run(
            &mut session,
            &[
                (What::Enter(0), 0),
                (What::Enter(1), 5),
                (What::Suspend { thread: coroutine, ended: false }, 10),
                (What::Leave(0), 1),
                (What::Resume { thread: coroutine, fresh: false }, 2),
                (What::Leave(1), 20),
                (What::Suspend { thread: coroutine, ended: true }, 0),
            ],
            3,
        );
        assert!((total(&report) - 41.0).abs() < 1e-9, "the bookings add up to the run: {}", total(&report));
        assert_eq!(stack_total(&report, body), 90.0, "the body: (10 + 20) times 3");
    }

    /// A call an error unwound without a word closes at the next event as deep or deeper in its thread
    #[test]
    fn a_call_an_error_unwound_closes_at_the_next_event() {
        let mut session = Session::new(&["cycles".into()]);
        let a = session.stacks.intern(&[(1, -1)]);
        let b = session.stacks.intern(&[(2, -1)]);
        session.calls = vec![call(1, a, 10, 1, 1), call(2, b, 10, 1, 1)];
        let report = run(&mut session, &[(What::Enter(0), 0), (What::Enter(1), 7), (What::Leave(1), 11)], 0);
        assert_eq!(stack_total(&report, a), 7.0, "a's own, until b entered at a's depth");
        assert_eq!(stack_total(&report, b), 11.0);
    }

    /// A service that lacks one of ree's own counters (installed before it) says to install it again; other errors stay
    #[test]
    fn a_service_without_a_counter_says_to_install_it_again() {
        let sources: Vec<String> = ["InstructionRetired", "TotalCycles", "ReeLoadStallCycles"].iter().map(|source| source.to_string()).collect();
        let hinted = with_reinstall_hint("the CPU offers no counter ReeLoadStallCycles".into(), &sources);
        assert!(hinted.starts_with("the CPU offers no counter ReeLoadStallCycles: ") && hinted.contains("load-stall-cycles") && hinted.contains("setup.ps1 again"), "{hinted}");
        assert_eq!(with_reinstall_hint("the counter service is busy".into(), &sources), "the counter service is busy");
        assert_eq!(with_reinstall_hint("the CPU offers no counter IcacheMisses".into(), &["IcacheMisses".to_string()]), "the CPU offers no counter IcacheMisses", "a Windows source of its own: nothing to reinstall");
    }

    /// The hint names the counter the error says is missing, not one the error lists among those the service offers
    #[test]
    fn the_reinstall_hint_names_the_missing_counter_not_an_offered_one() {
        let sources = vec!["ReeL1dMisses".to_string(), "ReeMabAllocsPrefetch".to_string()];
        let error = "the CPU offers no counter ReeMabAllocsPrefetch (it offers TotalCycles, ReeL1dMisses)";
        let hinted = with_reinstall_hint(error.into(), &sources);
        assert!(hinted.contains("before ree counted miss-buffer-allocs-prefetch (ReeMabAllocsPrefetch)"), "{hinted}");
        assert!(!hinted.contains("before ree counted demand-fills"), "{hinted}");
    }

    /// The same when the missing counter is the one the other test's error lists as offered
    #[test]
    fn the_reinstall_hint_also_names_missing_demand_fills() {
        let sources = vec!["ReeL1dMisses".to_string(), "ReeMabAllocsPrefetch".to_string()];
        let error = "the CPU offers no counter ReeL1dMisses (it offers TotalCycles, ReeMabAllocsPrefetch)";
        let hinted = with_reinstall_hint(error.into(), &sources);
        assert!(hinted.contains("before ree counted demand-fills (ReeL1dMisses)"), "{hinted}");
    }

    /// Frame, engine-step and thread readings are not call readings: they come however many calls are sampled, so they
    /// cannot raise the threshold, and only call readings are counted toward its rate
    #[test]
    fn frame_step_and_thread_readings_do_not_raise_the_call_threshold() {
        let mut session = Session::new(&["cycles".into()]);
        session.source = Some(Source::Mock { cycles: Vec::new() });
        let readings_that_are_not_calls = [
            What::FrameBegin,
            What::StepBegin(-1),
            What::StepEnd,
            What::Resume { thread: 1, fresh: false },
            What::Suspend { thread: 1, ended: false },
            What::FrameEnd(1),
        ];
        for _ in 0..3000 {
            for what in readings_that_are_not_calls {
                session.read(what, 0);
            }
        }
        assert_eq!(session.pacing.call_readings, 0);

        // a second on, with no call readings in it, the threshold falls rather than rises
        session.pacing.adjust_threshold(session.frequency);
        let starting_threshold = THRESHOLD_US * 1e-6 * session.frequency as f64;
        assert!(session.pacing.threshold < starting_threshold);

        session.read(What::Enter(0), 0);
        session.read(What::Leave(0), 0);
        assert_eq!(session.pacing.call_readings, 2);
    }

    /// Call readings far too fast raise the threshold only to its upper bound, and none at all lower it only to its
    /// lower bound
    #[test]
    fn the_call_threshold_stays_within_its_bounds() {
        // (a frequency of a million ticks a second: a tick is a microsecond, as the bounds are)
        let mut pacing = Pacing::new(1_000_000);
        let window_ticks = 100_000;
        for window in 1..=1000 {
            // 20,000 call readings in a tenth of a second: 200,000 a second
            pacing.call_readings += 20_000;
            pacing.adjust_threshold(window * window_ticks);
        }
        assert!((pacing.threshold - MAX_THRESHOLD_US).abs() < 1e-9);
        for window in 1001..=2000 {
            pacing.adjust_threshold(window * window_ticks);
        }
        assert!((pacing.threshold - MIN_THRESHOLD_US).abs() < 1e-9);
    }

    /// When the service says it dropped slices at its limit, the booking ends at the last reading it kept a slice for,
    /// the report says the profile was cut short (and when), and the missing tail has no reading's cost taken out
    #[test]
    fn a_slice_limit_stops_booking_at_the_last_complete_reading() {
        let mut session = Session::new(&["cycles".into()]);
        session.frequency = 1000;
        let stack = session.stacks.intern(&[(1, -1)]);
        session.calls = vec![call(1, stack, 1, 1, 1)];
        // a reading every 100 ticks; the mark and the end begin the second phase
        let events = [What::Calibrate, What::Calibrate, What::Start, What::Install, What::Enter(0), What::Leave(0), What::Mark, What::End];
        session.readings = events
            .iter()
            .enumerate()
            .map(|(index, &what)| {
                let phase = if matches!(what, What::Mark | What::End) { 1 } else { 0 };
                reading(100 * (index as i64 + 1), what, phase)
            })
            .collect();
        // The service kept six slices, one for each window up to the leave, and dropped the two after them. The counts are
        // those of the windows: the calibration windows count 5 (a reading's own cost), the call's two windows 7 and 8, and
        // the window that ends at the leave 105
        let counts_per_window = [5, 5, 5, 7, 8, 105];
        let slices = counts_per_window
            .iter()
            .enumerate()
            .map(|(window, &value)| Slice { cpu: 0, start: window as i64 * 100 + 2, end: (window as i64 + 1) * 100 + 1, counts: counts(value) })
            .collect();
        let limit_note = "the counter service keeps at most 6 slices a client and dropped 2 more: what was counted after them is missing (count a shorter run)";
        let limit_results = protocol::Results {
            counters: 1,
            threads: vec![protocol::Thread { id: session.thread, registered: true, slices }],
            notes: vec![limit_note.into()],
            ..Default::default()
        };
        let mut results_without_note = limit_results.clone();
        results_without_note.notes.clear();

        // Without the note the booking runs over all eight readings: the two windows with no slice are empty, and each loses
        // a reading's cost of 5
        let mut uncapped = Report::default();
        for frames in &session.stacks.stacks {
            uncapped.stacks.intern(frames);
        }
        book(&mut uncapped, &session, results_without_note);
        assert_eq!(uncapped.run[0], 95.0);
        let uncapped_second_phase = uncapped.phases.iter().find(|(phase, _)| *phase == 1).unwrap().1[0];
        assert_eq!(uncapped_second_phase, -5.0);

        let mut report = Report { stacks: std::mem::take(&mut session.stacks), ..Default::default() };
        book(&mut report, &session, limit_results);
        assert_eq!(report.cut_short, Some(0.3));
        assert_eq!(report.readings, 6);
        assert_eq!(report.matched, 6);
        assert_eq!(report.run[0], 105.0, "the missing tail never has a reading's cost subtracted");
        assert_eq!(stack_total(&report, stack), 100.0);
        assert_eq!(total(&report), 105.0);
        assert_eq!(report.phases.len(), 1, "the phase after the limit has no counts");
    }

    /// `--profile-counters`: names, at most eight, none twice; an old name is its new one
    #[test]
    fn counter_lists_parse() {
        assert_eq!(parse_counters("cycles, instructions").unwrap(), vec!["cycles".to_string(), "instructions".to_string()]);
        assert_eq!(parse_counters("l1d-misses").unwrap(), vec!["demand-fills".to_string()]);
        assert!(parse_counters(" , ").is_err());
        assert!(parse_counters("a,b,c,d,e,f,g,h,i").is_err());
        assert!(parse_counters("cycles,cycles").unwrap_err().contains("twice"));
        assert!(parse_counters("l2-misses,demand-fills-beyond-l2").unwrap_err().contains("twice"));
    }

    /// The sampling: calls at least the threshold long all, shorter ones one in threshold / mean, very short none; a
    /// function sampled stops only well below the line, so one hovering near it does not flip
    #[test]
    fn the_sampling_follows_the_calls_lengths() {
        let pacing = Pacing::new(10_000_000);
        let us = |value: f64| value * 10.0;
        assert_eq!(pacing.every_for(us(500.0)), 1);
        assert_eq!(pacing.every_for(us(100.0)), 1);
        assert_eq!(pacing.every_for(us(20.0)), 5);
        let learned = |mean: f64, measured: u32, every: u32| Learned { mean, weight: 4.0, measured, every, remeasure: if every == 0 { REMEASURE_FIRST } else { 0 }, first_short: false };
        assert_eq!(pacing.sampling(&learned(us(5.0), MIN_MEASURED, 1)).0, 0, "shorter than the shortest sampled");
        assert_eq!(pacing.sampling(&learned(us(9.0), MIN_MEASURED, 1)).0, 0, "its first calls decide at the line");
        assert_eq!(pacing.sampling(&learned(us(9.0), 50, 10)).0, 11, "sampled, it stays sampled just under the line");
        assert_eq!(pacing.sampling(&learned(us(7.0), 50, 10)).0, 0, "and stops well under it");
        assert_eq!(pacing.sampling(&learned(us(9.0), 50, 0)).0, 0, "not sampled, it starts again only at the line");
        assert_eq!(pacing.sampling(&learned(us(10.0), 50, 0)).0, 10);
    }

    /// A function's first calls are all read before its length decides; one found too short is still measured now and
    /// then, less often each time it is short again, and sampled again once its calls grow
    #[test]
    fn the_sampling_needs_several_calls_and_keeps_measuring() {
        let pacing = Pacing::new(10_000_000);
        let us = |value: f64| value * 10.0;
        let learned = |mean: f64, measured: u32, every: u32, remeasure: u32| Learned { mean, weight: measured as f64, measured, every, remeasure, first_short: false };
        assert_eq!(pacing.sampling(&learned(us(1.0), 1, 1, 0)), (1, 0), "one short call decides nothing");
        assert_eq!(pacing.sampling(&learned(us(1.0), MIN_MEASURED - 1, 1, 0)), (1, 0));
        assert_eq!(pacing.sampling(&learned(us(1.0), MIN_MEASURED, 1, 0)), (0, REMEASURE_FIRST), "short after its first calls: none, but measured again");
        assert_eq!(pacing.sampling(&learned(us(1.0), 9, 0, REMEASURE_FIRST)), (0, 2 * REMEASURE_FIRST), "short again: measured half as often");
        assert_eq!(pacing.sampling(&learned(us(1.0), 99, 0, REMEASURE_MOST)), (0, REMEASURE_MOST));
        assert_eq!(pacing.sampling(&learned(us(300.0), 10, 0, 512)), (1, 0), "grown: every call again");
        assert_eq!(pacing.sampling(&learned(us(25.0), 10, 0, 512)), (4, 0));
    }
}

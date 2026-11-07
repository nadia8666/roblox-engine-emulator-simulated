//! Optional sampling profiler for the engine's Luau VM (`RUR_LUAU_PROFILE=<file>`: the native app
//! and `rur-native engine`, so every host).
//!
//! A ticker thread advances a shared counter every millisecond. The VM's existing interrupt
//! callback (called at loop back edges, calls and returns) notices a changed counter and records
//! the current Luau stack, weighted by the elapsed ticks. Time spent inside native functions is
//! attributed to the next safepoint of their Luau caller. The output uses the folded-stack format
//! (`root;caller;callee count`) read by flame graph tools, with separate `startup` and `frames`
//! roots. Without the environment variable the interrupt only performs one relaxed load.
//!
//! `RUR_LUAU_PROFILE_CLOCK=work` profiles the engine's work clock (cost.rs) instead of the host's: a
//! stack is recorded each time another microsecond of Studio time has been charged (the VM's
//! instructions, calls and allocations, a datatype operation's host call, the Instance API's reads
//! and calls), weighted by that charge, so a CFrame operation costs the 15 ns the model fitted to
//! Studio, not what the emulator's Rust takes, and a run gives the same profile every time. The
//! weights are milliseconds like the time profile's (rounded); `RUR_LUAU_PROFILE_UNIT=us` writes
//! microseconds.
//!
//! `RUR_LUAU_ALLOC_PROFILE=<file>` samples allocations instead of time. The VM's `onallocate`
//! callback sees every allocation, those of a native function included (the stack then ends in the
//! native function, `rep [C]:-1 @-1`). An allocation of `size` bytes is recorded with probability
//! `size / step` (always when it is at least `step`) and weighted by the inverse, so the folded
//! totals estimate the bytes allocated without aliasing with a program's own period.
//! `RUR_LUAU_ALLOC_STEP=<bytes>` sets the step (default 1024; 1 records every allocation). The bytes go
//! to `<file>` and the allocation counts to `<file>.counts`, both in the folded-stack format of
//! the time profile. A realloc counts the bytes it grows by (what paces the collector), a shrink
//! nothing. A realloc of a thread's own stack or call-info array calls back before the thread
//! points at the new block, so the walk there reads the old, just released block (intact in
//! practice, and it only reads); a script stack of hundreds of KB could in principle fault there.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::CStr,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// The profiles share process-wide switches (a guard's drop turns them all off): a test that starts one
/// holds this lock until its guard drops (`claim`), so two tests never run a profile at once.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
thread_local! {
    static CLAIMED: RefCell<Option<std::sync::MutexGuard<'static, ()>>> = const { RefCell::new(None) };
}

/// Starting a profile on this thread takes `TEST_LOCK` for the guard's life. A test that started one without
/// the lock (the allocation profile of record/tests.rs) turned the switches off under the work-clock profile
/// test that ran beside it: its stacks were never sampled and 'microseconds profiled' read 0.
#[cfg(test)]
fn claim() {
    CLAIMED.with(|claimed| {
        let mut claimed = claimed.borrow_mut();
        if claimed.is_none() {
            *claimed = Some(TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        }
    });
}

#[cfg(not(test))]
fn claim() {}

#[cfg(test)]
pub(crate) fn claimed() -> bool {
    CLAIMED.with(|claimed| claimed.borrow().is_some())
}

#[cfg(test)]
fn release() {
    CLAIMED.with(|claimed| claimed.borrow_mut().take());
}

#[cfg(not(test))]
fn release() {}

static TICKS: AtomicU64 = AtomicU64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static WORK_CLOCK: AtomicBool = AtomicBool::new(false);
/// The profiling suite samples host time (profiler.rs `time`): the ticker runs for it, and the interrupt samples for it
static SUITE_TIME: AtomicBool = AtomicBool::new(false);

thread_local! {
    static SUITE_TICKER: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Starts the ticker for the profiling suite's host time samples (profiler.rs): the ticks it starts from
pub(crate) fn start_suite_ticker() -> u64 {
    let stop = Arc::new(AtomicBool::new(false));
    let ticker_stop = stop.clone();
    let (base, started) = (TICKS.load(Ordering::Relaxed), Instant::now());
    let spawned = thread::Builder::new().name("ree-profile-ticker".into()).spawn(move || {
        while !ticker_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(1));
            TICKS.fetch_max(base + started.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
    });
    if spawned.is_ok() {
        SUITE_TICKER.with(|ticker| *ticker.borrow_mut() = Some(stop));
        SUITE_TIME.store(true, Ordering::Relaxed);
    }
    base
}

pub(crate) fn stop_suite_ticker() {
    SUITE_TIME.store(false, Ordering::Relaxed);
    SUITE_TICKER.with(|ticker| {
        if let Some(stop) = ticker.borrow_mut().take() {
            stop.store(true, Ordering::Relaxed);
        }
    });
}

/// The work clock's cost for a sample: a stack is recorded once this much Studio time has been charged
/// since the last one, so the samples follow the model's cost, not the host's
const WORK_QUANTUM: f64 = 1e-6;

struct Samples {
    path: PathBuf,
    seen: u64,
    phase: String,
    stacks: HashMap<String, u64>,
    stop: Arc<AtomicBool>,
    /// The work-clock profile: seconds of Studio time per stack, and the total when last sampled
    work: HashMap<String, f64>,
    work_seen: f64,
    /// Weights in microseconds instead of milliseconds (`RUR_LUAU_PROFILE_UNIT=us`)
    microseconds: bool,
}

/// Where the work clock's total comes from: the Instance API's charges, what clock reads took out of
/// them, and the VM's counters (scheduler.rs `take_work`)
struct WorkSource {
    work: Rc<Cell<f64>>,
    drained: Rc<Cell<f64>>,
    counters: *const crate::cost::VmCounters,
}

static ALLOC_ENABLED: AtomicBool = AtomicBool::new(false);

struct Allocations {
    path: PathBuf,
    step: u64,
    random: u64,
    phase: String,
    /// (bytes, allocations) per stack: one key for both files (a stack is ~1 KB of text; a HUD scene's
    /// profile samples ~15k of them)
    stacks: HashMap<String, (f64, f64)>,
}

thread_local! {
    static WORK_SOURCE: RefCell<Option<WorkSource>> = const { RefCell::new(None) };
    static SAMPLES: RefCell<Option<Samples>> = const { RefCell::new(None) };
    static ALLOCATIONS: RefCell<Option<Allocations>> = const { RefCell::new(None) };
}

/// Starts sampling on this thread when `RUR_LUAU_PROFILE` (time) or `RUR_LUAU_ALLOC_PROFILE`
/// (allocations) names an output file.
pub fn begin() -> Option<ProfileGuard> {
    let time = begin_time();
    let allocations = begin_allocations_from_env();
    (time || allocations).then_some(ProfileGuard)
}

fn begin_time() -> bool {
    let Some(path) = std::env::var_os("RUR_LUAU_PROFILE").filter(|path| !path.is_empty()) else {
        return false;
    };
    let work_clock = std::env::var("RUR_LUAU_PROFILE_CLOCK").as_deref() == Ok("work");
    let microseconds = std::env::var("RUR_LUAU_PROFILE_UNIT").as_deref() == Ok("us");
    begin_time_at(PathBuf::from(path), work_clock, microseconds)
}

/// Starts the time profile on this thread: host time from a 1 ms ticker, or the work clock's charges
/// (`work_clock`), written to `path` when a [`ProfileGuard`] drops.
pub(crate) fn begin_time_at(path: PathBuf, work_clock: bool, microseconds: bool) -> bool {
    claim();
    let stop = Arc::new(AtomicBool::new(false));
    if !work_clock {
        let ticker_stop = stop.clone();
        let (base, started) = (TICKS.load(Ordering::Relaxed), Instant::now());
        let ticker = thread::Builder::new().name("rur-luau-profile".into()).spawn(move || {
            // a tick is a millisecond since the profile began, read from the clock: a sleep of 1 ms lasts up to
            // 15.6 ms on Windows unless some process raised the timer resolution, which made 100 ms of native time
            // count 6 ticks (the host-time test failed 1 run in 4 here)
            while !ticker_stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
                TICKS.fetch_max(base + started.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        });
        if ticker.is_err() {
            release();
            return false;
        }
    }
    SAMPLES.with(|samples| {
        *samples.borrow_mut() = Some(Samples {
            path,
            seen: TICKS.load(Ordering::Relaxed),
            phase: "startup".into(),
            stacks: HashMap::new(),
            stop,
            work: HashMap::new(),
            work_seen: 0.0,
            microseconds,
        })
    });
    WORK_CLOCK.store(work_clock, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    true
}

/// Tells the work-clock profile where the engine's work clock is (lua::raw::install).
pub fn set_work_source(work: Rc<Cell<f64>>, drained: Rc<Cell<f64>>, counters: *const crate::cost::VmCounters) {
    if WORK_CLOCK.load(Ordering::Relaxed) {
        WORK_SOURCE.with(|source| *source.borrow_mut() = Some(WorkSource { work, drained, counters }));
    }
}

fn begin_allocations_from_env() -> bool {
    let Some(path) = std::env::var_os("RUR_LUAU_ALLOC_PROFILE").filter(|path| !path.is_empty()) else {
        return false;
    };
    let step = std::env::var("RUR_LUAU_ALLOC_STEP").ok().and_then(|step| step.parse().ok()).unwrap_or(1024);
    begin_allocations(PathBuf::from(path), step);
    true
}

/// Starts the allocation profile on this thread: bytes to `path`, counts to `path.counts`, one
/// sample per `step` bytes allocated (1: every allocation). Written when a [`ProfileGuard`] drops.
pub(crate) fn begin_allocations(path: PathBuf, step: u64) {
    claim();
    ALLOCATIONS.with(|allocations| {
        *allocations.borrow_mut() = Some(Allocations {
            path,
            step: step.max(1),
            random: 0x9e37_79b9_7f4a_7c15,
            phase: "startup".into(),
            stacks: HashMap::new(),
        })
    });
    ALLOC_ENABLED.store(true, Ordering::Relaxed);
}

/// Samples recorded after the first reply belong to the frame loop.
pub fn mark_frames() {
    set_phase("frames");
}

/// Names the phase the samples after this call belong to (the first segment of every stack):
/// `startup` until the harness's first reply, then `frames`, or what `Context.ProfilePhase(name)`
/// set, so a harness can read its steady state apart from its setup.
pub fn set_phase(name: &str) {
    crate::boundary::set_phase(name);
    crate::profiler::phase(name);
    SAMPLES.with(|samples| {
        if let Some(samples) = samples.borrow_mut().as_mut() {
            samples.phase = name.into();
        }
    });
    ALLOCATIONS.with(|allocations| {
        if let Some(allocations) = allocations.borrow_mut().as_mut() {
            allocations.phase = name.into();
        }
    });
}

/// Luau's `onallocate` callback: `old` bytes became `new` (0 for a new block).
unsafe extern "C-unwind" fn allocating(state: *mut mlua::ffi::lua_State, _block: *mut std::ffi::c_void, old: usize, new: usize, _memcat: u8, _tt: i32, _tag: i32) {
    if new <= old {
        return;
    }
    let size = (new - old) as u64;
    ALLOCATIONS.with(|allocations| {
        let Ok(mut allocations) = allocations.try_borrow_mut() else {
            return;
        };
        let Some(allocations) = allocations.as_mut() else {
            return;
        };
        // taken with probability size / step, weighted by its inverse
        let mut weight = 1.0;
        if size < allocations.step {
            allocations.random ^= allocations.random << 13;
            allocations.random ^= allocations.random >> 7;
            allocations.random ^= allocations.random << 17;
            if allocations.random % allocations.step >= size {
                return;
            }
            weight = allocations.step as f64 / size as f64;
        }
        let stack = folded_stack(state, &allocations.phase, true);
        let (bytes, counts) = allocations.stacks.entry(stack).or_default();
        *counts += weight;
        *bytes += size as f64 * weight;
    });
}

/// Runs a step of ree's own engine (the Animator, joint and physics steps the scheduler makes between a
/// frame's Luau handlers) and, in the host-time profile, books its ticks to `phase;(ree engine step:
/// name)` instead of to the next Luau safepoint, where they made a spring module's PreSimulation and
/// PostSimulation handlers and a Stepped handler's first native line look like 25-57 % of a frame
/// (the same time with no springs and no enemies). A step that reached a Luau safepoint
/// itself is left as it is.
pub fn engine_step<R>(name: &str, step: impl FnOnce() -> R) -> R {
    if SUITE_TIME.load(Ordering::Relaxed) {
        // (the hardware counters' readings around the step: its counts are its own)
        let counted = crate::profiler::hw_step(Some(name));
        let start = TICKS.load(Ordering::Relaxed);
        let result = step();
        if counted {
            crate::profiler::hw_step(None);
        }
        let ticks = TICKS.load(Ordering::Relaxed) - start;
        if ticks > 0 {
            crate::profiler::engine_step(name, ticks);
        }
        return result;
    }
    if !ENABLED.load(Ordering::Relaxed) || WORK_CLOCK.load(Ordering::Relaxed) {
        return step();
    }
    let start = TICKS.load(Ordering::Relaxed);
    let seen = SAMPLES.with(|samples| samples.try_borrow().ok().and_then(|samples| samples.as_ref().map(|samples| samples.seen)));
    let result = step();
    let ticks = TICKS.load(Ordering::Relaxed) - start;
    SAMPLES.with(|samples| {
        let Ok(mut samples) = samples.try_borrow_mut() else {
            return;
        };
        let Some(samples) = samples.as_mut() else {
            return;
        };
        if ticks > 0 && Some(samples.seen) == seen {
            samples.seen += ticks;
            *samples.stacks.entry(format!("{};(ree engine step: {name})", samples.phase)).or_default() += ticks;
        }
    });
    result
}

/// Called from the VM interrupt at non-GC safepoints.
#[inline]
pub fn sample(state: *mut mlua::ffi::lua_State) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if WORK_CLOCK.load(Ordering::Relaxed) {
        return sample_work(state);
    }
    let now = TICKS.load(Ordering::Relaxed);
    SAMPLES.with(|samples| {
        let Ok(mut samples) = samples.try_borrow_mut() else {
            return;
        };
        let Some(samples) = samples.as_mut() else {
            return;
        };
        if now == samples.seen {
            return;
        }
        let weight = now - samples.seen;
        samples.seen = now;
        *samples.stacks.entry(folded_stack(state, &samples.phase, false)).or_default() += weight;
    });
}

/// The work clock's profile: the stack at the safepoint where another [`WORK_QUANTUM`] of what the work
/// clock charges (cost.rs: the VM's instructions, calls and allocations, a datatype operation's host
/// call, the Instance API's reads and calls) has accrued, weighted by that charge.
fn sample_work(state: *mut mlua::ffi::lua_State) {
    WORK_SOURCE.with(|source| {
        let Ok(source) = source.try_borrow() else {
            return;
        };
        let Some(source) = source.as_ref() else {
            return;
        };
        let total = source.work.get() + source.drained.get() + crate::cost::luau(unsafe { &*source.counters });
        SAMPLES.with(|samples| {
            let Ok(mut samples) = samples.try_borrow_mut() else {
                return;
            };
            let Some(samples) = samples.as_mut() else {
                return;
            };
            let charged = total - samples.work_seen;
            if charged < WORK_QUANTUM {
                return;
            }
            samples.work_seen = total;
            *samples.work.entry(folded_stack(state, &samples.phase, false)).or_default() += charged;
        });
    });
}

/// The Luau stack of `state` as one folded line (`phase;outermost;...;innermost @line`). The innermost
/// frame carries its current line; with `line_under_native` so does the first Luau frame under native ones
/// (an allocation made inside `table.create` names the line that called it).
fn folded_stack(state: *mut mlua::ffi::lua_State, phase: &str, line_under_native: bool) -> String {
    let mut frames = Vec::new();
    unsafe {
        let mut debug: mlua::ffi::lua_Debug = std::mem::zeroed();
        let mut level = 0;
        let mut lined = false;
        while level < 200 && mlua::ffi::lua_getinfo(state, level, c"sln".as_ptr(), &mut debug) != 0 {
            let source = if debug.short_src.is_null() {
                "?".into()
            } else {
                CStr::from_ptr(debug.short_src).to_string_lossy()
            };
            let name = if debug.name.is_null() {
                "anonymous".into()
            } else {
                CStr::from_ptr(debug.name).to_string_lossy()
            };
            if level == 0 || (line_under_native && !lined && source != "[C]") {
                frames.push(format!("{name} {source}:{} @{}", debug.linedefined, debug.currentline));
            } else {
                frames.push(format!("{name} {source}:{}", debug.linedefined));
            }
            lined |= source != "[C]";
            level += 1;
        }
    }
    frames.push(phase.into());
    frames.reverse();
    frames.join(";")
}

unsafe extern "C-unwind" fn sampling_interrupt(state: *mut mlua::ffi::lua_State, gc: i32) {
    if gc < 0 {
        sample(state);
        if SUITE_TIME.load(Ordering::Relaxed) {
            crate::profiler::safepoint(state, TICKS.load(Ordering::Relaxed));
        }
        crate::boundary::safepoint(state);
        // (a profile holds the interrupt slot: the script timeout checks from here)
        unsafe { crate::watchdog::check(state) };
    } else {
        // (a collector step's start and end)
        crate::profiler::gc_interrupt(state);
    }
}

/// Makes `lua`'s interrupt sample stacks, and its allocator callback record them, while a profile
/// is active (the engine CLI; the native app's worker samples from its cancellation interrupt
/// instead). Nothing without a profile.
pub fn install(lua: &mlua::Lua) {
    let time = ENABLED.load(Ordering::Relaxed);
    let allocations = ALLOC_ENABLED.load(Ordering::Relaxed);
    let boundary = crate::boundary::active();
    // the profiling suite (profiler.rs): its interrupt work (host time samples, the collector's steps) and its own
    // allocation callback, which takes the place of this one
    let suite = crate::profiler::active();
    let suite_interrupt = suite && crate::profiler::wants_interrupt();
    let suite_allocations = suite && crate::profiler::wants_allocations();
    if !time && !allocations && !boundary && !suite {
        return;
    }
    if time || boundary || suite_interrupt {
        unsafe { (*callbacks(lua)).interrupt = Some(sampling_interrupt) };
    }
    if allocations && !suite_allocations {
        hook_allocations(lua);
    } else if allocations {
        eprintln!("RUR_LUAU_ALLOC_PROFILE is not written: the profile's own allocation profile (--profile alloc) takes the VM's allocation callback");
    }
    if suite {
        crate::profiler::install(lua);
    }
}

/// `lua`'s VM's callbacks, read straight from its state: `exec_raw` would run a protected call, which makes a closure on
/// the VM's heap, and those few bytes move where the collector's later cycles begin, so what a script reads of memory
/// (collectgarbage("count")) differed at some frames with a profile that hooks the VM this way
fn callbacks(lua: &mlua::Lua) -> *mut mlua::ffi::lua_Callbacks {
    unsafe { mlua::ffi::lua_callbacks(lua.current_thread().state()) }
}

/// Points `lua`'s `onallocate` callback at the allocation profile of the thread it runs on.
pub(crate) fn hook_allocations(lua: &mlua::Lua) {
    unsafe { (*callbacks(lua)).onallocate = Some(allocating) };
}

pub struct ProfileGuard;

/// Folded lines, largest first.
fn folded<'a>(rows: impl Iterator<Item = (&'a str, f64)>) -> Vec<(&'a str, u64)> {
    let mut rows: Vec<_> = rows.map(|(stack, weight)| (stack, weight.round() as u64)).collect();
    rows.retain(|row| row.1 > 0);
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows
}

/// Writes `stack weight` lines to `path` as they go (the profile of a long run is tens of MB: it was
/// built as one string first, at the end of the run).
fn write_folded<'a>(path: &std::path::Path, rows: impl IntoIterator<Item = (&'a str, u64)>) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::with_capacity(1 << 16, std::fs::File::create(path)?);
    for (stack, weight) in rows {
        writeln!(out, "{stack} {weight}")?;
    }
    out.flush()
}

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        ENABLED.store(false, Ordering::Relaxed);
        ALLOC_ENABLED.store(false, Ordering::Relaxed);
        let work_clock = WORK_CLOCK.swap(false, Ordering::Relaxed);
        WORK_SOURCE.with(|source| *source.borrow_mut() = None);
        SAMPLES.with(|samples| {
            if let Some(samples) = samples.borrow_mut().take() {
                samples.stop.store(true, Ordering::Relaxed);
                let written = if work_clock {
                    let unit = if samples.microseconds { 1e6 } else { 1e3 };
                    write_folded(&samples.path, folded(samples.work.iter().map(|(stack, seconds)| (stack.as_str(), seconds * unit))))
                } else {
                    let mut rows: Vec<_> = samples.stacks.iter().map(|(stack, count)| (stack.as_str(), *count)).collect();
                    rows.sort_by(|a, b| b.1.cmp(&a.1));
                    write_folded(&samples.path, rows)
                };
                if let Err(error) = written {
                    eprintln!("Writing Luau profile {}: {error}", samples.path.display());
                }
            }
        });
        ALLOCATIONS.with(|allocations| {
            if let Some(allocations) = allocations.borrow_mut().take() {
                let mut counts = allocations.path.clone().into_os_string();
                counts.push(".counts");
                let bytes = folded(allocations.stacks.iter().map(|(stack, (bytes, _))| (stack.as_str(), *bytes)));
                let counts_rows = folded(allocations.stacks.iter().map(|(stack, (_, counts))| (stack.as_str(), *counts)));
                for (path, rows) in [(allocations.path.clone(), bytes), (PathBuf::from(counts), counts_rows)] {
                    if let Err(error) = write_folded(&path, rows) {
                        eprintln!("Writing Luau allocation profile {}: {error}", path.display());
                    }
                }
            }
        });
        release();
    }
}

#[cfg(test)]
mod folded_tests {
    use super::*;

    /// A profile file lists `stack weight` lines, the largest first and equal weights by stack, rounded,
    /// without the ones that round to 0; the allocation profile writes both files from one map.
    #[test]
    fn folded_files_list_stacks_largest_first() {
        let mut stacks: HashMap<String, (f64, f64)> = HashMap::new();
        stacks.insert("b;c".into(), (10.4, 2.0));
        stacks.insert("a".into(), (9.6, 0.4));
        stacks.insert("z".into(), (3.5, 1.0));
        let path = std::env::temp_dir().join(format!("ree-folded-test-{}", std::process::id()));
        write_folded(&path, folded(stacks.iter().map(|(stack, (bytes, _))| (stack.as_str(), *bytes)))).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a 10\nb;c 10\nz 4\n");
        write_folded(&path, folded(stacks.iter().map(|(stack, (_, counts))| (stack.as_str(), *counts)))).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b;c 2\nz 1\n");
        let _ = std::fs::remove_file(&path);
    }
}

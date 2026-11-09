//! A run that cannot outlive its job. On 2026-10-24 eleven `ree run`s of automatically mutated scripts spun one core
//! each for 7-8 hours: a mutation deleted the only yield of a loop (a `runOnRunner(coroutine.yield())` in
//! `while true do`, a `counter += Heartbeat:Wait()` in a particle tween), Roblox ends such a script with its execution
//! timeout, and ree had none, while the parent processes waited on it with no timeout of their own. Three guards:
//!
//! - [`ScriptTimeout`]: Roblox's script execution timeout. A resumption the engine makes (a thread resumed, a handler, a
//!   harness) that runs [`DEFAULT_SCRIPT_TIMEOUT`] seconds without yielding errors with Roblox's [`TIMEOUT_MESSAGE`] at its
//!   next safepoint in a script's own code, and again at every later one until it yields or ends, so a `pcall` cannot
//!   swallow it. The seconds are the work clock's (what the VM's instructions, calls and allocations and the engine calls a
//!   script makes cost Studio: cost.rs), not the virtual clock's, so the timeout fires with `REE_FROZEN_CLOCK=1` too and does
//!   not depend on how busy the machine is; host time past [`HOST_BACKSTOP`] times the limit also fires it (work the clock
//!   does not count: natively compiled code, the engine's own Luau). Roblox goes on after a timeout; a run stops after [`DEFAULT_STOP_AFTER`] of them
//!   ([`ScriptTimeout::stopped`]: every script safepoint raises, the frame loop ends the run, exit [`EXIT_SCRIPT_TIMEOUTS`]),
//!   because a script that never yields is usually resumed again each frame.
//! - [`cap_run`]: a wall-clock cap per process, from `--seconds` with a generous margin; ree exits with [`EXIT_CAP`].
//! - [`watch_parent`]: ree exits with [`EXIT_ORPHANED`] when the process that started it is gone.
//!
//! The timeout costs nothing while scripts yield: the engine bumps an atomic epoch when a top-level resumption begins and
//! ends ([`enter`], from `scheduler::resume`), and a watchdog thread that finds one resumption still running after a tick
//! (0.1 s of host time) installs the VM's interrupt (Luau's REPL sets it from a signal handler the same way). The interrupt
//! then counts the work charged from then on (the first tick's work is not counted: the timeout never fires early) and
//! removes itself once the resumption is over.

use std::{
    cell::Cell,
    rc::Rc,
    ffi::{CStr, c_int},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use mlua::ffi::{lua_Callbacks, lua_State};

/// The error a script gets when it runs past the timeout without yielding (Roblox's text: Roblox's documentation and the
/// developer forum's reports of it; not measured, no Studio was open)
pub const TIMEOUT_MESSAGE: &str = "Script timeout: exhausted allowed execution time";

/// Studio's default Script Timeout Length, in seconds (Studio settings, Roblox's documentation; not measured)
pub const DEFAULT_SCRIPT_TIMEOUT: f64 = 10.0;

/// Host time that fires the timeout as well, in multiples of the limit: a loop the work clock prices below its host cost
/// still ends (a mutated effect script rebuilt NumberSequences at ~10 host seconds a work second; natively compiled code)
pub const HOST_BACKSTOP: f64 = 6.0;

/// The exit code of a run that reached its wall-clock cap (GNU timeout's)
pub const EXIT_CAP: i32 = 124;

/// The exit code of a run whose parent process is gone
pub const EXIT_ORPHANED: i32 = 125;

/// The exit code of a run stopped by its script timeouts ([`DEFAULT_STOP_AFTER`])
pub const EXIT_SCRIPT_TIMEOUTS: i32 = 126;

/// Script timeouts after which the run stops. Roblox goes on after one, and ree does too, but a script that never yields
/// is usually resumed again (a frame's handler, a pooled runner): each further one costs the full limit, so one mutated
/// script ran 10 s of every frame and its 80 s scene would have taken hours.
pub const DEFAULT_STOP_AFTER: u32 = 3;

/// How often the watchdog thread looks at the running resumptions
const TICK: Duration = Duration::from_millis(100);

/// Safepoints between two looks at a long resumption (its work clock, whether it has ended)
const CHECK_EVERY: u32 = 1024;

/// The timeout's state for one VM, shared by its thread and the watchdog thread.
pub struct Watch {
    /// This watch among all the process has made (a later VM's callbacks and Arc may reuse an earlier one's addresses)
    serial: u64,
    /// Odd while the engine is inside a top-level resumption: bumped when one begins and when it ends
    epoch: AtomicU64,
    /// Nested resumptions inside the current top-level one (the VM's thread only)
    depth: AtomicU32,
    /// The epoch of the resumption the watchdog found running long (0: none)
    due: AtomicU64,
    /// The VM's callbacks (`lua_callbacks`), null once the VM is about to go: the watchdog writes their interrupt
    callbacks: AtomicPtr<lua_Callbacks>,
    /// Serializes writes of the interrupt slot
    slot: Mutex<()>,
    /// Seconds of the work clock a resumption may run without yielding
    limit: f64,
    /// Script timeouts after which the process exits (0: never)
    stop_after: u32,
    /// Script timeouts so far
    timeouts: AtomicU32,
    /// Those of them host time fired alone ([`HOST_BACKSTOP`]: the work clock had not reached the limit), which a profile that
    /// slows the run makes likelier: its passes are compared by them
    host_timeouts: AtomicU32,
    /// The run stops: `stop_after` timeouts happened ([`ScriptTimeout::stopped`])
    stopped: AtomicBool,
    /// Where the last of them ran
    last: Mutex<String>,
}

struct Registry {
    watches: Vec<Weak<Watch>>,
    started: bool,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { watches: Vec::new(), started: false });

/// The next watch's serial
static SERIALS: AtomicU64 = AtomicU64::new(1);

/// Whether the watchdog has armed any VM's timeout yet: until then a profile's interrupt skips the check at once
static ARMED: AtomicBool = AtomicBool::new(false);

/// What the interrupt measured of the resumption it times (the VM's thread)
#[derive(Clone, Copy)]
struct Timing {
    /// The watch and the resumption it times (a later VM on this thread counts its epochs from 0 again)
    serial: u64,
    epoch: u64,
    work: f64,
    host: Option<Instant>,
    /// The resumption has had its timeout (later raises are the same timeout's)
    raised: bool,
}

thread_local! {
    static TIMING: Cell<Timing> = const { Cell::new(Timing { serial: 0, epoch: 0, work: 0.0, host: None, raised: false }) };
    /// Safepoints the timeout's check has seen on this thread
    static CALLS: Cell<u32> = const { Cell::new(0) };
    /// The check looks at every safepoint: a timeout is due, or the run stops
    static EAGER: Cell<bool> = const { Cell::new(false) };
    /// This thread's VMs with a timeout, for the interrupt (one or two: a service worker holds a prepared run beside the one
    /// it runs)
    static WATCHES: std::cell::RefCell<Vec<Local>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A VM's timeout on its own thread: its callbacks (the key), its watch and its scheduler's work clock cells
struct Local {
    callbacks: usize,
    watch: Weak<Watch>,
    /// What the engine calls charged since the last clock read, and what clock reads took out of it so far (Scheduler's
    /// `work` and `work_drained`)
    work: Rc<Cell<f64>>,
    drained: Rc<Cell<f64>>,
}

/// The script timeout of one VM: [`ScriptTimeout::install`] makes it, the engine's scheduler holds it.
#[derive(Clone)]
pub struct ScriptTimeout(Arc<Watch>);

impl ScriptTimeout {
    /// Times the resumptions of `engine`'s VM `lua` on this thread from now on, `limit` seconds of the work clock each
    /// (`None` when `limit` is not above 0: no timeout); the run stops at the `stop_after`th timeout (0: never,
    /// [`ScriptTimeout::stopped`]). The engine's scheduler must hold the result ([`crate::scheduler::Scheduler`]'s
    /// `timeout`), and [`ScriptTimeout::detach`] must run before the VM is dropped ([`crate::Emulator`] does).
    pub fn install(lua: &mlua::Lua, engine: &crate::Engine, limit: f64, stop_after: u32) -> Option<ScriptTimeout> {
        if !(limit > 0.0 && limit.is_finite()) {
            return None;
        }
        let mut callbacks = std::ptr::null_mut();
        unsafe {
            let _ = lua.exec_raw::<()>((), |state| callbacks = mlua::ffi::lua_callbacks(state));
        }
        if callbacks.is_null() {
            return None;
        }
        let watch = Arc::new(Watch {
            serial: SERIALS.fetch_add(1, Ordering::Relaxed),
            epoch: AtomicU64::new(0),
            depth: AtomicU32::new(0),
            due: AtomicU64::new(0),
            callbacks: AtomicPtr::new(callbacks),
            slot: Mutex::new(()),
            limit,
            stop_after,
            timeouts: AtomicU32::new(0),
            host_timeouts: AtomicU32::new(0),
            stopped: AtomicBool::new(false),
            last: Mutex::new(String::new()),
        });
        let (work, drained) = {
            let state = engine.state.borrow();
            (state.sched.work.clone(), state.sched.work_drained.clone())
        };
        WATCHES.with(|watches| {
            let mut watches = watches.borrow_mut();
            watches.retain(|local| local.watch.strong_count() > 0);
            watches.push(Local { callbacks: callbacks as usize, watch: Arc::downgrade(&watch), work, drained });
        });
        let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.watches.retain(|weak| weak.strong_count() > 0);
        registry.watches.push(Arc::downgrade(&watch));
        if !registry.started {
            registry.started = std::thread::Builder::new().name("ree-script-timeout".into()).spawn(watchdog).is_ok();
        }
        Some(ScriptTimeout(watch))
    }

    /// The watchdog leaves the VM alone from now on (before it is dropped: its callbacks go with it).
    pub fn detach(&self) {
        let watch = &self.0;
        let _slot = watch.slot.lock().unwrap_or_else(|poison| poison.into_inner());
        let callbacks = watch.callbacks.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !callbacks.is_null() {
            unsafe { clear_timeout_interrupt(callbacks) };
        }
        WATCHES.with(|watches| watches.borrow_mut().retain(|local| local.callbacks != callbacks as usize));
    }

    /// The seconds of the work clock a resumption may run without yielding
    pub fn limit(&self) -> f64 {
        self.0.limit
    }

    /// Script timeouts so far
    pub fn timeouts(&self) -> u32 {
        self.0.timeouts.load(Ordering::Relaxed)
    }

    /// Those of them host time fired alone, past [`HOST_BACKSTOP`] times the limit with the work clock short of it
    pub fn host_timeouts(&self) -> u32 {
        self.0.host_timeouts.load(Ordering::Relaxed)
    }

    /// Why the run must stop, once `stop_after` script timeouts happened: every script safepoint raises from then on, and the
    /// frame loop ends the run with this failure and exit code [`EXIT_SCRIPT_TIMEOUTS`].
    pub fn stopped(&self) -> Option<String> {
        let watch = &self.0;
        watch.stopped.load(Ordering::Relaxed).then(|| {
            format!(
                "{} script timeouts: the run stops (each time a script ran {} s of the work clock without yielding; --max-script-timeouts or REE_MAX_SCRIPT_TIMEOUTS sets how many stop it, 0 never). The last: {}",
                watch.timeouts.load(Ordering::Relaxed),
                watch.limit,
                watch.last.lock().unwrap_or_else(|poison| poison.into_inner())
            )
        })
    }
}

/// A resumption is running while this lives ([`enter`]). It points at the watch the scheduler holds, which outlives
/// every resumption of its VM.
pub struct Entered(*const Watch);

/// The engine begins a resumption (`scheduler::resume`): the outermost one of a nest bumps the epoch now and when it ends.
#[inline]
pub fn enter(timeout: Option<&ScriptTimeout>) -> Entered {
    let Some(timeout) = timeout else { return Entered(std::ptr::null()) };
    let watch = &*timeout.0;
    if watch.depth.fetch_add(1, Ordering::Relaxed) == 0 {
        watch.epoch.fetch_add(1, Ordering::Release);
    }
    Entered(watch)
}

impl Drop for Entered {
    #[inline]
    fn drop(&mut self) {
        if let Some(watch) = unsafe { self.0.as_ref() } {
            if watch.depth.fetch_sub(1, Ordering::Relaxed) == 1 {
                watch.epoch.fetch_add(1, Ordering::Release);
            }
        }
    }
}

/// The interrupt slot as the atomic it is written through (a function pointer or null)
unsafe fn interrupt_slot(callbacks: *mut lua_Callbacks) -> &'static AtomicUsize {
    unsafe { AtomicUsize::from_ptr(std::ptr::addr_of_mut!((*callbacks).interrupt).cast::<usize>()) }
}

/// Removes [`timeout_interrupt`] from the slot when it is there (another interrupt, a profile's or a host's, stays)
unsafe fn clear_timeout_interrupt(callbacks: *mut lua_Callbacks) {
    let slot = unsafe { interrupt_slot(callbacks) };
    let _ = slot.compare_exchange(timeout_interrupt as usize, 0, Ordering::AcqRel, Ordering::Relaxed);
}

/// The watchdog thread: arms the interrupt of a VM whose resumption has run a whole tick.
fn watchdog() {
    let mut seen: Vec<(u64, u64)> = Vec::new();
    loop {
        std::thread::sleep(TICK);
        let watches: Vec<Arc<Watch>> = {
            let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
            registry.watches.retain(|weak| weak.strong_count() > 0);
            registry.watches.iter().filter_map(Weak::upgrade).collect()
        };
        let mut next = Vec::with_capacity(watches.len());
        for watch in &watches {
            let key = watch.serial;
            let epoch = watch.epoch.load(Ordering::Acquire);
            let before = seen.iter().find(|(k, _)| *k == key).map(|(_, e)| *e);
            if epoch % 2 == 1 && before == Some(epoch) && watch.due.load(Ordering::Acquire) != epoch {
                // (under the slot's lock: the VM's thread disarms under it too, so it never clears a fresh arming)
                let _slot = watch.slot.lock().unwrap_or_else(|poison| poison.into_inner());
                watch.due.store(epoch, Ordering::Release);
                ARMED.store(true, Ordering::Release);
                let callbacks = watch.callbacks.load(Ordering::Acquire);
                if !callbacks.is_null() {
                    // (a profile's interrupt, already there, checks `due` itself: luau_profile)
                    let slot = unsafe { interrupt_slot(callbacks) };
                    let _ = slot.compare_exchange(0, timeout_interrupt as usize, Ordering::AcqRel, Ordering::Relaxed);
                }
            }
            next.push((key, epoch));
        }
        seen = next;
    }
}

unsafe extern "C-unwind" fn timeout_interrupt(state: *mut lua_State, gc: c_int) {
    // never raise from a GC interrupt (as mlua and rur's cancellation interrupt)
    if gc >= 0 {
        return;
    }
    unsafe { check(state) }
}

/// The timeout's check at a safepoint of `state` (the timeout's own interrupt, or a profile's that holds the slot):
/// raises [`TIMEOUT_MESSAGE`] when the resumption the watchdog armed it for has run past the limit.
///
/// # Safety
/// `state` is the running thread of a VM, called from its interrupt at a non-GC safepoint.
#[inline]
pub unsafe fn check(state: *mut lua_State) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    // every safepoint of an armed VM comes here (an empty loop's every step): only one in CHECK_EVERY looks further, unless
    // a timeout is due and waits for a script frame (a loop's period can keep every sampled safepoint in the engine's Luau)
    if !EAGER.with(Cell::get) {
        let calls = CALLS.with(|calls| {
            let count = calls.get().wrapping_add(1);
            calls.set(count);
            count
        });
        if calls % CHECK_EVERY != 0 {
            return;
        }
    }
    let callbacks = unsafe { mlua::ffi::lua_callbacks(state) } as usize;
    let found = WATCHES.with(|watches| {
        let watches = watches.borrow();
        let local = watches.iter().find(|local| local.callbacks == callbacks)?;
        Some((local.watch.upgrade()?, local.work.clone(), local.drained.clone()))
    });
    let Some((watch, charged, drained)) = found else {
        EAGER.with(|eager| eager.set(false));
        return;
    };
    // the work clock so far: the VM's own work and what the engine calls charged (cost.rs)
    let work = |state| work(state) + charged.get() + drained.get();
    if watch.stopped.load(Ordering::Relaxed) {
        // the run stops: every script safepoint raises until the frame loop ends it (record/run.rs)
        EAGER.with(|eager| eager.set(true));
        if !in_engine_luau(state) {
            drop(watch);
            unsafe { raise(state) }
        }
        return;
    }
    let due = watch.due.load(Ordering::Acquire);
    if due == 0 {
        EAGER.with(|eager| eager.set(false));
        return;
    }
    if watch.epoch.load(Ordering::Acquire) != due {
        // the resumption it was armed for has ended: back to no interrupt
        EAGER.with(|eager| eager.set(false));
        let _slot = watch.slot.lock().unwrap_or_else(|poison| poison.into_inner());
        let pointer = watch.callbacks.load(Ordering::Acquire);
        if watch.due.compare_exchange(due, 0, Ordering::AcqRel, Ordering::Relaxed).is_ok() && !pointer.is_null() {
            unsafe { clear_timeout_interrupt(pointer) };
        }
        return;
    }
    let timing = TIMING.with(Cell::get);
    if timing.epoch != due || timing.serial != watch.serial {
        TIMING.with(|cell| cell.set(Timing { serial: watch.serial, epoch: due, work: work(state), host: Some(Instant::now()), raised: false }));
        return;
    }
    let mut by_host = false;
    if !timing.raised {
        let over_work = work(state) - timing.work >= watch.limit;
        let over_host = timing.host.is_some_and(|host| host.elapsed().as_secs_f64() >= watch.limit * HOST_BACKSTOP);
        if !(over_work || over_host) {
            return;
        }
        by_host = !over_work;
    }
    // due (or raised already and still running: a pcall caught it): raised at the first safepoint in a script's own code
    EAGER.with(|eager| eager.set(true));
    if in_engine_luau(state) {
        return;
    }
    if !timing.raised {
        TIMING.with(|cell| cell.set(Timing { raised: true, ..timing }));
        let count = watch.timeouts.fetch_add(1, Ordering::Relaxed) + 1;
        if by_host {
            watch.host_timeouts.fetch_add(1, Ordering::Relaxed);
        }
        let place = script_stack(state);
        crate::output::diagnostic(&format!("ree: script timeout {count} at {place}"));
        if watch.stop_after > 0 && count >= watch.stop_after {
            *watch.last.lock().unwrap_or_else(|poison| poison.into_inner()) = place;
            watch.stopped.store(true, Ordering::Relaxed);
        }
    }
    drop(watch);
    unsafe { raise(state) }
}

/// Where `state` runs, innermost first: `name chunk:line` for up to 6 Luau frames
fn script_stack(state: *mut lua_State) -> String {
    let mut frames = Vec::new();
    unsafe {
        let mut debug: mlua::ffi::lua_Debug = std::mem::zeroed();
        let mut level = 0;
        while frames.len() < 6 && level < 64 && mlua::ffi::lua_getinfo(state, level, c"sln".as_ptr(), &mut debug) != 0 {
            level += 1;
            if debug.currentline < 0 {
                continue;
            }
            let source = if debug.short_src.is_null() { "?".into() } else { CStr::from_ptr(debug.short_src).to_string_lossy() };
            let name = if debug.name.is_null() { "anonymous".into() } else { CStr::from_ptr(debug.name).to_string_lossy() };
            frames.push(format!("{name} {source}:{}", debug.currentline));
        }
    }
    frames.join(" < ")
}

/// What the VM's own script work has cost so far, in Studio seconds (cost.rs `luau`)
fn work(state: *mut lua_State) -> f64 {
    unsafe { crate::cost::luau(&*crate::cost::lua_workcounters(state)) }
}

/// Whether the running function is the engine's own Luau (`=rur.*` chunks): Roblox's timeout never fires inside the engine,
/// so it waits for the script's next safepoint
fn in_engine_luau(state: *mut lua_State) -> bool {
    unsafe {
        let mut debug: mlua::ffi::lua_Debug = std::mem::zeroed();
        if mlua::ffi::lua_getinfo(state, 0, c"s".as_ptr(), &mut debug) == 0 || debug.source.is_null() {
            return false;
        }
        CStr::from_ptr(debug.source).to_bytes().starts_with(b"=rur.")
    }
}

/// Raises the timeout's error in `state` (a plain string, as Roblox's: no position)
unsafe fn raise(state: *mut lua_State) -> ! {
    unsafe {
        mlua::ffi::lua_rawcheckstack(state, 1);
        mlua::ffi::lua_pushlstring(state, TIMEOUT_MESSAGE.as_ptr().cast(), TIMEOUT_MESSAGE.len());
        mlua::ffi::lua_error(state)
    }
}

/// The script timeout a run asked for: `--script-timeout <seconds>`, else `REE_SCRIPT_TIMEOUT`, else Studio's default (0
/// turns it off).
pub fn script_timeout_seconds(flag: Option<f64>) -> f64 {
    flag.or_else(|| std::env::var("REE_SCRIPT_TIMEOUT").ok().and_then(|text| text.trim().parse().ok())).unwrap_or(DEFAULT_SCRIPT_TIMEOUT)
}

/// How many script timeouts stop a run: `--max-script-timeouts <n>`, else `REE_MAX_SCRIPT_TIMEOUTS`, else
/// [`DEFAULT_STOP_AFTER`] (0: never).
pub fn stop_after(flag: Option<u32>) -> u32 {
    flag.or_else(|| std::env::var("REE_MAX_SCRIPT_TIMEOUTS").ok().and_then(|text| text.trim().parse().ok())).unwrap_or(DEFAULT_STOP_AFTER)
}

/// The default wall-clock cap of a run of `seconds` virtual seconds: 10 minutes for the mount and the harness's setup plus
/// 20 host seconds a virtual second. The heaviest scenes measured run at 0.5-3 host seconds a virtual second.
pub fn default_cap(seconds: f64) -> f64 {
    600.0 + 20.0 * seconds.max(0.0)
}

/// The wall-clock cap a run asked for: `--max-wall-seconds <n>`, else `REE_MAX_WALL_SECONDS`, else [`default_cap`] (0 turns
/// it off).
pub fn cap_seconds(flag: Option<f64>, seconds: f64) -> f64 {
    flag.or_else(|| std::env::var("REE_MAX_WALL_SECONDS").ok().and_then(|text| text.trim().parse().ok())).unwrap_or_else(|| default_cap(seconds))
}

/// Ends the process with `code` at once after writing `message` to stderr: no destructors, atexit handlers or buffered
/// stdout (the main thread may be anywhere: in the VM, holding the heap's lock).
pub fn terminate(message: &str, code: i32) -> ! {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{message}");
    let _ = stderr.flush();
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn TerminateProcess(process: *mut std::ffi::c_void, code: u32) -> i32;
        }
        TerminateProcess(GetCurrentProcess(), code as u32);
    }
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn _exit(code: c_int) -> !;
        }
        _exit(code)
    }
    #[allow(unreachable_code)]
    std::process::exit(code)
}

/// Ends the process with [`EXIT_CAP`] once `cap` seconds of wall-clock time have passed since `started` (nothing when `cap`
/// is not above 0). `command` and `seconds` name the run in the message.
pub fn cap_run(command: &str, cap: f64, seconds: f64, started: Instant) {
    if !(cap > 0.0 && cap.is_finite()) {
        return;
    }
    let message = format!(
        "ree {command}: wall-clock cap reached after {cap:.0} s (the run asked for {seconds} virtual seconds; --max-wall-seconds or REE_MAX_WALL_SECONDS sets the cap, 0 turns it off); the run did not finish"
    );
    let deadline = started + Duration::from_secs_f64(cap);
    let _ = std::thread::Builder::new().name("ree-wall-cap".into()).spawn(move || {
        loop {
            let now = Instant::now();
            if now >= deadline {
                terminate(&message, EXIT_CAP);
            }
            std::thread::sleep(deadline - now);
        }
    });
}

/// Ends the process with [`EXIT_ORPHANED`] when the process that started it exits (a thread waits on its handle), or at
/// once when it is gone already (no process has its id, or a later one does). Nothing with `REE_PARENT_WATCH=0` (a launcher
/// that detaches on purpose), or when the parent may not be watched (access denied).
pub fn watch_parent(command: &str) {
    if std::env::var("REE_PARENT_WATCH").as_deref() == Ok("0") {
        return;
    }
    platform::watch_parent(command);
}

/// Why a `ree serve` job stopped before its run ended: its caller left ([`caller_gone`])
pub const CALLER_GONE: &str = "its caller has gone (the client that asked for this job exited or gave up waiting): the job stops";

/// How often a job's frames ask whether its caller has gone
const CALLER_EVERY: Duration = Duration::from_millis(250);

/// The caller of the job running on a thread ([`for_caller`])
struct Caller {
    gone: Box<dyn Fn() -> bool>,
    asked: Cell<Option<Instant>>,
    left: Cell<bool>,
}

thread_local! {
    static CALLER: std::cell::RefCell<Option<Rc<Caller>>> = const { std::cell::RefCell::new(None) };
}

/// Runs `run` (a `ree serve` job) for a caller that `gone` says has left: its frames stop once it has ([`EXIT_ORPHANED`])
pub fn for_caller<R>(gone: impl Fn() -> bool + 'static, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Rc<Caller>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CALLER.with(|caller| *caller.borrow_mut() = previous);
        }
    }
    let caller = Rc::new(Caller { gone: Box::new(gone), asked: Cell::new(None), left: Cell::new(false) });
    let _restore = Restore(CALLER.with(|current| current.replace(Some(caller))));
    run()
}

/// Whether the caller of this thread's job has gone (never outside [`for_caller`]), asked at most every [`CALLER_EVERY`]
pub fn caller_gone() -> bool {
    let Some(caller) = CALLER.with(|caller| caller.borrow().clone()) else { return false };
    if caller.left.get() {
        return true;
    }
    let now = Instant::now();
    if caller.asked.get().is_some_and(|asked| now.duration_since(asked) < CALLER_EVERY) {
        return false;
    }
    caller.asked.set(Some(now));
    let left = (caller.gone)();
    caller.left.set(left);
    left
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        priority_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetCurrentProcessId() -> u32;
        fn GetProcessTimes(process: *mut c_void, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const ERROR_INVALID_PARAMETER: u32 = 87;

    fn parent_id() -> Option<u32> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot as isize == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return None;
            }
            let me = GetCurrentProcessId();
            let mut entry: ProcessEntry32W = std::mem::zeroed();
            entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
            let mut found = None;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                if entry.process_id == me {
                    found = Some(entry.parent_process_id);
                    break;
                }
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            found
        }
    }

    fn created(process: *mut c_void) -> Option<u64> {
        let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
        (unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } != 0).then_some(creation)
    }

    pub fn watch_parent(command: &str) {
        let Some(parent) = parent_id().filter(|&pid| pid != 0) else { return };
        let message = format!("ree {command}: its parent process (pid {parent}) is gone; exiting");
        let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, parent) };
        if handle.is_null() {
            // no such process: the parent ended before this one looked (access denied: one we may not watch)
            if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                super::terminate(&message, super::EXIT_ORPHANED);
            }
            return;
        }
        // a process made after this one has only reused the id of a parent that is gone
        match (created(handle), created(unsafe { GetCurrentProcess() })) {
            (Some(theirs), Some(mine)) if theirs <= mine => {}
            (Some(_), Some(_)) => super::terminate(&message, super::EXIT_ORPHANED),
            _ => {
                unsafe { CloseHandle(handle) };
                return;
            }
        }
        let handle = handle as usize;
        let _ = std::thread::Builder::new().name("ree-parent-watch".into()).spawn(move || {
            if unsafe { WaitForSingleObject(handle as *mut c_void, INFINITE) } == WAIT_OBJECT_0 {
                super::terminate(&message, super::EXIT_ORPHANED);
            }
        });
    }
}

#[cfg(unix)]
mod platform {
    pub fn watch_parent(command: &str) {
        let parent = std::os::unix::process::parent_id();
        if parent <= 1 {
            return;
        }
        let message = format!("ree {command}: its parent process (pid {parent}) is gone; exiting");
        let _ = std::thread::Builder::new().name("ree-parent-watch".into()).spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if std::os::unix::process::parent_id() != parent {
                    super::terminate(&message, super::EXIT_ORPHANED);
                }
            }
        });
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn watch_parent(_command: &str) {}
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use mlua::prelude::*;

    use super::{ScriptTimeout, TIMEOUT_MESSAGE};
    use crate::{
        Engine, Options,
        lua::{self as engine_lua, globals::CallOutcome},
    };

    fn start(limit: f64, stop_after: u32) -> (Lua, Rc<Engine>, ScriptTimeout) {
        let engine = Engine::new(Options::default()).expect("engine");
        let lua = Lua::new();
        engine_lua::start(&lua, &engine).expect("start");
        let timeout = ScriptTimeout::install(&lua, &engine, limit, stop_after).expect("a timeout");
        engine.state.borrow_mut().sched.timeout = Some(timeout.clone());
        (lua, engine, timeout)
    }

    /// Runs `source` as a script on a scheduler thread; how its first resumption ended: "finished", "pending" or the error
    fn call(lua: &Lua, engine: &Rc<Engine>, source: &str) -> String {
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
        };
        let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
        let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
        match engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call").outcome().expect("outcome") {
            CallOutcome::Finished(_) => "finished".into(),
            CallOutcome::Pending => "pending".into(),
            CallOutcome::Failed(message, _) => message,
        }
    }

    fn timed_out(outcome: &str) -> bool {
        outcome.contains(TIMEOUT_MESSAGE)
    }

    fn interrupt_installed(lua: &Lua) -> bool {
        let mut installed = false;
        unsafe {
            let _ = lua.exec_raw::<()>((), |state| installed = (*mlua::ffi::lua_callbacks(state)).interrupt.is_some());
        }
        installed
    }

    #[test]
    fn a_script_that_never_yields_errors_with_robloxs_timeout_and_a_pcall_does_not_keep_it_running() {
        let (lua, engine, timeout) = start(0.02, 0);
        let outcome = call(&lua, &engine, "local ok, message = pcall(function() while true do end end) print('caught', ok, message) while true do end");
        assert!(timed_out(&outcome), "{outcome}");
        assert_eq!(timeout.timeouts(), 1);
        // the next resumption runs as any other
        let outcome = call(&lua, &engine, "local n = 0 for i = 1, 1000 do n += i end print(n)");
        assert_eq!(outcome, "finished");
        timeout.detach();
    }

    #[test]
    fn a_busy_wait_on_a_frozen_clock_ends_by_the_timeout() {
        // REE_FROZEN_CLOCK=1: work moves no clock reading, so a loop waiting for os.clock, tick or os.time to pass a time
        // only drifts: Roblox's timeout ends it
        let (lua, engine, timeout) = start(0.02, 0);
        engine.state.borrow_mut().sched.freeze_wall_clock();
        for wait in [
            "local t = os.clock() while os.clock() < t + 1e9 do end",
            "local t = tick() while tick() < t + 1e9 do end",
            "local t = os.time() while os.time() < t + 1e9 do end",
        ] {
            let outcome = call(&lua, &engine, wait);
            assert!(timed_out(&outcome), "{wait}: {outcome}");
        }
        assert_eq!(timeout.timeouts(), 3);
        timeout.detach();
    }

    #[test]
    fn scripts_that_yield_are_not_timed_out_and_the_interrupt_goes_with_the_long_resumption() {
        let (lua, engine, timeout) = start(10.0, 0);
        // ~0.3 s of host time in one resumption (the watchdog arms the interrupt) but ~0.1 s of the work clock
        let outcome = call(&lua, &engine, "local n = 0 for i = 1, 3e7 do n += i end print(n)");
        assert_eq!(outcome, "finished");
        let outcome = call(&lua, &engine, "local n = 0 for i = 1, 5000 do n += i end");
        assert_eq!(outcome, "finished");
        assert!(!interrupt_installed(&lua), "the interrupt stayed after the resumption it timed ended");
        assert_eq!(timeout.timeouts(), 0);
        timeout.detach();
    }

    /// A service worker runs VM after VM on one thread, and the allocator often gives a later VM an earlier one's callbacks
    #[test]
    fn a_later_vm_on_the_thread_is_timed_from_its_own_resumption() {
        let long = "local n = 0 for i = 1, 1.5e8 do n += i end print(n)";
        let mut earlier = None;
        for _ in 0..6 {
            let (lua, engine, timeout) = start(10.0, 0);
            let mut callbacks = 0usize;
            unsafe {
                let _ = lua.exec_raw::<()>((), |state| callbacks = mlua::ffi::lua_callbacks(state) as usize);
            }
            // the thread's timing as it stands two minutes after the earlier VM's resumption, which had this one's epoch
            super::TIMING.with(|cell| {
                let timing = cell.get();
                cell.set(super::Timing { host: timing.host.map(|host| host.checked_sub(std::time::Duration::from_secs(120)).unwrap_or(host)), ..timing });
            });
            let outcome = call(&lua, &engine, long);
            timeout.detach();
            assert_eq!(outcome, "finished", "a VM's resumption was timed from an earlier VM's (same callbacks: {})", earlier == Some(callbacks));
            assert!(super::TIMING.with(std::cell::Cell::get).host.is_some(), "the watchdog timed the long resumption");
            if earlier == Some(callbacks) {
                break;
            }
            earlier = Some(callbacks);
        }
    }

    #[test]
    fn script_timeouts_stop_the_run_after_the_limit() {
        let (lua, engine, timeout) = start(0.02, 2);
        let outcome = call(&lua, &engine, "game:GetService('RunService').Heartbeat:Connect(function() while true do end end)");
        assert_eq!(outcome, "finished");
        let mut frames = 0;
        while timeout.stopped().is_none() && frames < 40 {
            crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
            frames += 1;
        }
        let why = timeout.stopped().expect("the run stops");
        // (a step of the default edit mode holds several Heartbeats)
        assert!(frames <= 2 && timeout.timeouts() == 2, "{frames} frames: {why}");
        assert!(why.starts_with("2 script timeouts") && why.contains("Test:1"), "{why}");
        // from now on every script safepoint raises: a fresh script cannot run on
        let outcome = call(&lua, &engine, "local n = 0 for i = 1, 1e6 do n += i end");
        assert!(timed_out(&outcome), "{outcome}");
        timeout.detach();
    }
}

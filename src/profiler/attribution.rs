//! Who pays what the work clock charges besides a script's own instructions (profiler/mod.rs), while a `work` profile steps:
//! - the engine's cold and first-use charges (cost.rs Touched::cold), each kept with the instruction that paid it and whether other
//!   code touched the same object in the frame: a cut of the payer saves a charge only when nothing else touched its object then
//!   (otherwise the charge moves to the other code), so a function's saving is its hot work and its unshared cold charges;
//! - the engine's dispatch of a handler, by the site that connected it ("300 Heartbeat handlers from Listeners:8"), with each run's
//!   whole work;
//! - threads' resumptions, by the line they waited on.
//!
//! The VM keeps the same for its own cold charges (tables, C and engine functions: patches/luau/16-cold-attribution.patch); the
//! report adds the two.

use std::cell::{Cell, RefCell};

use mlua::ffi::lua_State;
use rustc_hash::{FxHashMap, FxHashSet};

use super::vm::{self, Booking, Payer};
use crate::cost::{ColdKey, MemberUse, Path};
use crate::{ClassId, reflection::NameId};

/// A cold charge in the frame now: who paid it, what it cost, whether other code touched its object since
#[derive(Clone, Copy, Debug)]
struct Record {
    function: i32,
    pc: i32,
    node: i32,
    cost: f64,
    shared: bool,
}

/// A line's os.clock reads: how many, in how many frames and runs of its function (a call, or what is left of one after its thread
/// yielded), and the clock's advance between two reads by one run (`between` seconds over `intervals`). The rest describes the last
/// read, to tell whether the next one continues its run.
#[derive(Clone, Copy, Debug, Default)]
struct ClockReads {
    reads: u64,
    frames: u64,
    runs: u64,
    intervals: u64,
    between: f64,
    /// The last read's frame, call-tree node, node calls, thread, the thread's resumptions then, and the clock it returned
    frame: u64,
    node: i32,
    calls: u64,
    thread: usize,
    resumes: u64,
    last: f64,
}

impl ClockReads {
    /// Counts a read in `frame` by the running `booking` on `thread` (resumed `resumes` times so far) that returned `clock`. A read
    /// continues the last one's run when the same call of the function made it without its thread yielding between them; the clock's
    /// advance since then is an interval. Any other read starts a run.
    fn record(&mut self, frame: u64, booking: Booking, thread: usize, resumes: u64, clock: f64) {
        self.reads += 1;
        if self.frame != frame {
            self.frames += 1;
            self.frame = frame;
        }
        let same_run = self.reads > 1 && self.node == booking.node && self.calls == booking.calls && self.thread == thread && self.resumes == resumes;
        if same_run {
            self.between += clock - self.last;
            self.intervals += 1;
        } else {
            self.runs += 1;
        }
        self.node = booking.node;
        self.calls = booking.calls;
        self.thread = thread;
        self.resumes = resumes;
        self.last = clock;
    }
}

/// A line that read os.clock ([`Results::clock_reads`]): its function and pc, reads, frames and runs of its function that read it (a
/// call, or what is left of one after its thread yielded), and the clock's advance between two reads by one run (seconds of the clock
/// as it ran, over `intervals`)
#[derive(Clone, Debug)]
pub struct ClockLine {
    pub function: i32,
    pub pc: i32,
    pub reads: u64,
    pub frames: u64,
    pub runs: u64,
    pub intervals: u64,
    pub between: f64,
}

/// What the totals name a charge by: an instance's first touch by its class, the rest as their key says
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum TotalKey {
    Instance(ClassId),
    Member(MemberUse, NameId),
    Path(Path),
    Cloned(ClassId),
    Destroyed(ClassId),
}

impl TotalKey {
    fn of(key: ColdKey) -> TotalKey {
        match key {
            ColdKey::Instance(_, class) => TotalKey::Instance(class),
            ColdKey::Member(kind, name) => TotalKey::Member(kind, name),
            ColdKey::Path(path) => TotalKey::Path(path),
            ColdKey::Cloned(class) => TotalKey::Cloned(class),
            ColdKey::Destroyed(class) => TotalKey::Destroyed(class),
        }
    }
}

/// Everything charged cold for one [`TotalKey`] over the profile
#[derive(Default, Debug)]
struct Total {
    cost: f64,
    charges: u64,
    shared: u64,
    /// The cost each instruction (function, pc) paid
    payers: FxHashMap<(i32, i32), f64>,
}

/// A connected handler's site: the instruction that connected it, and its signal's name
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Site {
    function: i32,
    pc: i32,
    signal: String,
}

/// The runs of the handlers one connect site made
#[derive(Default, Debug)]
struct Handlers {
    runs: u64,
    /// Seconds of the work clock the engine's delivery cost, and the runs' whole work (dispatch included)
    dispatch: f64,
    work: f64,
    connections: FxHashSet<u32>,
}

/// The resumptions of the threads that waited on one line
#[derive(Default, Debug)]
struct Resumes {
    count: u64,
    seconds: f64,
}

#[derive(Default)]
struct State {
    frame: FxHashMap<ColdKey, Record>,
    totals: FxHashMap<TotalKey, Total>,
    /// The engine's cold charges nothing else touched in their frame, by (function, pc, node)
    exclusive: FxHashMap<(i32, i32, i32), f64>,
    /// The site that connected each connection
    connections: FxHashMap<u32, Site>,
    handlers: FxHashMap<Site, Handlers>,
    /// Resumptions by (function, pc) of the line the thread waited on
    resumes: FxHashMap<(i32, i32), Resumes>,
    /// Fires of events no bench measured, which borrow a price (cost.rs signal_kinds)
    unmeasured_fires: u64,
    /// os.clock reads by (function, pc) (a line that reads it more than once a run of its function is a budget loop's check: its work
    /// depends on the clock, so a frozen clock changes what it does)
    clock_reads: FxHashMap<(i32, i32), ClockReads>,
    frames: u64,
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Starts keeping attributions on this thread (a `work` profile's install)
pub(super) fn begin() {
    STATE.with(|state| *state.borrow_mut() = Some(State::default()));
    ON.with(|on| on.set(true));
}

#[inline]
pub(crate) fn on() -> bool {
    ON.with(Cell::get)
}

/// Runs `body` on the profile's state; None when no profile keeps attributions on this thread
fn with<R>(body: impl FnOnce(&mut State) -> R) -> Option<R> {
    if !on() {
        return None;
    }
    STATE.with(|state| state.try_borrow_mut().ok().and_then(|mut state| state.as_mut().map(body)))
}

/// The engine charged `seconds` cold for `key`'s object, paid by the instruction running now. One operation can charge an object
/// twice (a write's first touch, then its dirtying), and the charges add up: the first payer keeps the record, and another
/// function's charge makes it shared.
pub fn engine_cold(key: ColdKey, seconds: f64) {
    let Some(payer) = vm::current() else { return };
    with(|state| match state.frame.get_mut(&key) {
        Some(record) => {
            record.cost += seconds;
            if record.function != payer.function {
                record.shared = true;
            }
        }
        None => {
            state.frame.insert(key, Record { function: payer.function, pc: payer.pc, node: payer.node, cost: seconds, shared: false });
        }
    });
}

/// Code touched `key`'s object again: when another function than the payer, the charge is shared
pub fn engine_touch(key: ColdKey) {
    if !on() {
        return;
    }
    let Some(toucher) = vm::current() else { return };
    with(|state| {
        if let Some(record) = state.frame.get_mut(&key)
            && record.function != toucher.function
        {
            record.shared = true;
        }
    });
}

/// A frame begins: the frame's charges are settled
pub fn engine_frame() {
    with(|state| {
        settle(state);
        state.frames += 1;
    });
}

/// A script on `lua`'s running thread read os.clock, which returned `clock` (seconds of the work clock): counted by the line that read
/// it, by the runs of its function that read it (a call, or what is left of one after its thread yielded), and with the clock's advance
/// between two reads by one run (a budget loop's check reads it once an iteration, so that advance is an iteration's work, its callees'
/// included; across a yield it would be whatever else ran meanwhile, so a read after one starts a run of its own)
pub fn clock_read(lua: &mlua::Lua, clock: f64) {
    let Some(booking) = vm::current() else { return };
    let thread = lua.current_thread();
    // (the running thread is live)
    let resumes = unsafe { vm::resumes(thread.state()) };
    let thread = thread.to_pointer() as usize;
    with(|state| {
        let frame = state.frames + 1;
        let line = (booking.function, booking.pc);
        state.clock_reads.entry(line).or_default().record(frame, booking, thread, resumes, clock);
    });
}

/// Moves the frame's charges into the totals; a charge nothing else shared is also the payer's exclusive saving
fn settle(state: &mut State) {
    for (key, record) in state.frame.drain() {
        let total = state.totals.entry(TotalKey::of(key)).or_default();
        total.cost += record.cost;
        total.charges += 1;
        if record.shared {
            total.shared += 1;
        }
        *total.payers.entry((record.function, record.pc)).or_default() += record.cost;
        if !record.shared {
            *state.exclusive.entry((record.function, record.pc, record.node)).or_default() += record.cost;
        }
    }
}

/// A script connected `connection` to the signal named `signal`: its site is the instruction running now
pub fn connected(connection: u32, signal: &str) {
    if !on() {
        return;
    }
    let (function, pc) = match vm::current() {
        Some(booking) => (booking.function, booking.pc),
        None => (-1, -1),
    };
    with(|state| {
        state.connections.insert(connection, Site { function, pc, signal: signal.to_string() });
    });
}

/// A handler of `connection` ran: what its dispatch cost and what its run cost in all (seconds of the work clock)
pub fn handler_ran(connection: u32, signal: &str, dispatch: f64, work: f64) {
    with(|state| {
        let site = match state.connections.get(&connection) {
            Some(site) => site.clone(),
            None => Site { function: -1, pc: -1, signal: signal.to_string() },
        };
        let handlers = state.handlers.entry(site).or_default();
        handlers.runs += 1;
        handlers.dispatch += dispatch;
        handlers.work += work;
        handlers.connections.insert(connection);
    });
}

/// What the engine charges next goes to `thread`'s current frame, or to the function on top of its stack when it has not started
/// (`main`: the VM's main thread)
///
/// # Safety
/// `main` and `thread` are live threads of the VM the profile runs on.
pub unsafe fn book_next(main: *mut lua_State, thread: *mut lua_State) {
    if on() {
        unsafe { vm::book_next(main, thread) };
    }
}

/// An event no bench measured fired with handlers (priced as a BindableEvent's: counted for the report)
pub fn unmeasured_fire() {
    with(|state| state.unmeasured_fires += 1);
}

/// A parked thread was resumed at `cost` seconds, booked to the line it waited on (the booking's target now: [`book_next`] first)
pub fn resumed(cost: f64) {
    let Some(booking) = vm::current() else { return };
    with(|state| {
        let line = state.resumes.entry((booking.function, booking.pc)).or_default();
        line.count += 1;
        line.seconds += cost;
    });
}

/// An engine cold charge's object over the profile, named
#[derive(Clone, Debug)]
pub struct ColdObject {
    /// "instance", "member", "engine path", "first clone", "first destroy"
    pub kind: &'static str,
    pub name: String,
    pub cost: f64,
    pub charges: u64,
    /// Charges whose object other code touched in the same frame
    pub shared: u64,
    /// Who paid for it, dearest first
    pub payers: Vec<Payer>,
}

/// A connect site's handlers over the profile
#[derive(Clone, Debug)]
pub struct HandlerSite {
    pub function: i32,
    pub pc: i32,
    pub signal: String,
    pub connections: usize,
    pub runs: u64,
    pub dispatch: f64,
    pub work: f64,
}

/// Cold charges for objects nothing else touched in their frame, which one instruction paid
#[derive(Clone, Copy, Debug)]
pub struct ExclusiveCharge {
    pub function: i32,
    pub pc: i32,
    /// The payer's node in the call tree
    pub node: i32,
    pub cost: f64,
}

/// The resumptions of the threads that waited on one line
#[derive(Clone, Copy, Debug)]
pub struct ResumeSite {
    pub function: i32,
    pub pc: i32,
    pub count: u64,
    pub cost: f64,
}

/// What the attributions came to, for the report
#[derive(Clone, Debug, Default)]
pub struct Results {
    pub cold: Vec<ColdObject>,
    /// The engine's cold charges nothing else touched in their frame, ordered by function, pc and node
    pub exclusive: Vec<ExclusiveCharge>,
    pub handlers: Vec<HandlerSite>,
    /// Each line threads waited on, the dearest first
    pub resumes: Vec<ResumeSite>,
    pub unmeasured_fires: u64,
    /// Each line that read os.clock
    pub clock_reads: Vec<ClockLine>,
}

/// Names the engine's classes and members for the report (without an engine, by their ids)
struct Names<'a> {
    engine: Option<&'a crate::Engine>,
}

impl Names<'_> {
    fn class(&self, class: ClassId) -> String {
        match self.engine {
            Some(engine) => engine.reflection.class(class).name.to_string(),
            None => format!("class #{class}"),
        }
    }

    fn member(&self, name: NameId) -> String {
        if name == crate::cost::CHILD_LOOKUP {
            return "(a child found by name)".to_string();
        }
        match self.engine {
            Some(engine) => engine.reflection.name(name).to_string(),
            None => format!("member #{name}"),
        }
    }

    /// What kind of object `key` is, and its name
    fn describe(&self, key: TotalKey) -> (&'static str, String) {
        match key {
            TotalKey::Instance(class) => ("instance", self.class(class)),
            TotalKey::Member(member_use, name) => {
                let verb = match member_use {
                    MemberUse::Read => "read of",
                    MemberUse::Write => "write of",
                    MemberUse::Call => "call of",
                };
                ("member", format!("{verb} {}", self.member(name)))
            }
            TotalKey::Path(path) => ("engine path", format!("{path:?}")),
            TotalKey::Cloned(class) => ("first clone", self.class(class)),
            TotalKey::Destroyed(class) => ("first destroy", self.class(class)),
        }
    }
}

/// The cold charges by object, the dearest first
fn cold_objects(totals: FxHashMap<TotalKey, Total>, names: &Names) -> Vec<ColdObject> {
    let mut objects: Vec<ColdObject> = Vec::new();
    for (key, total) in totals {
        let (kind, name) = names.describe(key);
        let mut payers: Vec<Payer> = Vec::new();
        for ((function, pc), cost) in total.payers {
            payers.push(Payer { function, pc, cost });
        }
        payers.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.function.cmp(&b.function)).then(a.pc.cmp(&b.pc)));
        objects.push(ColdObject { kind, name, cost: total.cost, charges: total.charges, shared: total.shared, payers });
    }
    objects.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.kind.cmp(b.kind)).then(a.name.cmp(&b.name)));
    objects
}

/// The handlers by connect site, the heaviest first
fn handler_sites(handlers: FxHashMap<Site, Handlers>) -> Vec<HandlerSite> {
    let mut sites: Vec<HandlerSite> = Vec::new();
    for (site, handlers) in handlers {
        sites.push(HandlerSite {
            function: site.function,
            pc: site.pc,
            signal: site.signal,
            connections: handlers.connections.len(),
            runs: handlers.runs,
            dispatch: handlers.dispatch,
            work: handlers.work,
        });
    }
    sites.sort_by(|a, b| b.work.total_cmp(&a.work).then(a.function.cmp(&b.function)).then(a.pc.cmp(&b.pc)).then(a.signal.cmp(&b.signal)));
    sites
}

/// Ends the attributions and names them (`engine`'s reflection names classes and members)
pub(super) fn finish(engine: Option<&crate::Engine>) -> Option<Results> {
    ON.with(|on| on.set(false));
    let mut state = STATE.with(|state| state.borrow_mut().take())?;
    settle(&mut state);

    let cold = cold_objects(state.totals, &Names { engine });

    let mut exclusive: Vec<ExclusiveCharge> = Vec::new();
    for ((function, pc, node), cost) in state.exclusive {
        exclusive.push(ExclusiveCharge { function, pc, node, cost });
    }
    exclusive.sort_by_key(|charge| (charge.function, charge.pc, charge.node));

    let mut resumes: Vec<ResumeSite> = Vec::new();
    for ((function, pc), line) in state.resumes {
        resumes.push(ResumeSite { function, pc, count: line.count, cost: line.seconds });
    }
    resumes.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.function.cmp(&b.function)).then(a.pc.cmp(&b.pc)));

    let mut clock_reads: Vec<ClockLine> = Vec::new();
    for ((function, pc), line) in state.clock_reads {
        clock_reads.push(ClockLine { function, pc, reads: line.reads, frames: line.frames, runs: line.runs, intervals: line.intervals, between: line.between });
    }
    clock_reads.sort_by(|a, b| b.reads.cmp(&a.reads).then(a.function.cmp(&b.function)).then(a.pc.cmp(&b.pc)));

    Some(Results { cold, exclusive, handlers: handler_sites(state.handlers), resumes, unmeasured_fires: state.unmeasured_fires, clock_reads })
}

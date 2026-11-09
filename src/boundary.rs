//! The boundary profiler (`ree run ... --profile boundary`): every crossing from a script's Luau into the engine,
//! counted per call site (script, function, line) with the cost the work clock (cost.rs) charged for it.
//!
//! Roblox's Luau-to-C++ boundary is dear (a property read is 30-300 ns, a method 50-100 ns, a first touch of an instance
//! in a frame 0.3 us more), so code that crosses it every frame for a value that did not change is the cheapest kind of
//! win to find. The crossings counted: Instance property reads and writes by Class.Member, Instance methods, event
//! connects, Instance.new, and the datatype operations the VM counts as host calls (a CFrame, Color3 or UDim2
//! constructor, method or operator). A call site is the innermost frame of the game's own Luau; the engine's Luau
//! (`rur.*`, `ree.*`) is skipped, so a call into the prelude is the game line that made it.
//!
//! Gets also feed the repeat table: the same property of the same instance read again in one frame while its value has not
//! changed since that read (no write to the instance since, by a script or by the engine: the instance's version and the
//! writes the profiler saw), split by where a cache would go: in the same call of the same function (a local holds it), in a
//! later call of that function (read it once in the caller and pass it in), or by another function. A read after a write of
//! a new value is not a repeat (a local would be stale), nor is one read per call of a function called twice a frame
//! counted as a "same call" repeat: no local spans the calls.
//!
//! Off, the Instance API pays one thread-local read. On, a crossing walks the Luau stack to its call site (cached by
//! chunk, defining line and current line), so a run is several times slower than an unprofiled one; the counts are
//! exact for the paths below and the cost is the same charge the work clock takes, so the report is the same on every
//! run. Datatype operations are counted by the VM, not per call: each safepoint (a call, return or loop edge) books the
//! host calls and nanoseconds charged since the last one to the call site of the previous safepoint, so an operator
//! (`cf * cf`, which runs no call instruction) lands on the call before it, and a constructor cannot be told from
//! another by name. `workspace` and `game` are plain globals, not crossings. NOT counted: reads inside a muted engine call
//! (the engine's Luau pricing a whole method), Vector3 members, and what the engine does on its own (events delivered,
//! the physics and animation steps).

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    ffi::CStr,
    path::PathBuf,
};

use mlua::ffi;
use rustc_hash::FxHashMap;
use serde_json::{Value as Json, json};

/// What a crossing is
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Kind {
    Get,
    Set,
    Method,
    Event,
    Create,
    Datatype,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Get => "get",
            Kind::Set => "set",
            Kind::Method => "method",
            Kind::Event => "event",
            Kind::Create => "create",
            Kind::Datatype => "datatype",
        }
    }
}

/// No instance (the repeat table skips it)
pub const NO_INSTANCE: u32 = u32::MAX;

/// Luau's RETURN opcode (Bytecode.h): an interrupt there ends the call at its depth
const OP_RETURN: u8 = 22;

/// The keyed property access opcodes' indexes in `cost::INTERPRETED_OPS`
const GETTABLEKS: usize = 15;
const SETTABLEKS: usize = 16;
/// A member the reflection has no name for (a child looked up by name)
pub const NO_MEMBER: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct Config {
    /// Where the JSON report goes (`--profile-out`; default `ree-boundary.json` in state for standalone runs, else here)
    pub out: PathBuf,
    /// How many rows the printed report shows per table
    pub top: usize,
    /// The phases to report (`--profile-phase a,b` or `all`; default `steady` when the harness named one, else the one with the most frames)
    pub phase: Option<String>,
}

/// `--profile boundary [--profile-out <file>] [--profile-top <n>] [--profile-phase <name>]`, or the environment variable
/// `RUR_BOUNDARY_PROFILE=<file>`. None when neither is given.
pub fn parse(args: &[String]) -> Option<Config> {
    let value = |key: &str| args.iter().position(|arg| arg == key).and_then(|index| args.get(index + 1)).cloned();
    let flag = args.windows(2).any(|pair| pair[0] == "--profile" && pair[1] == "boundary");
    let env = std::env::var_os("RUR_BOUNDARY_PROFILE").filter(|path| !path.is_empty());
    if !flag && env.is_none() {
        return None;
    }
    let out = value("--profile-out").map(PathBuf::from).or_else(|| env.map(PathBuf::from)).unwrap_or_else(|| crate::default_output_path("ree-boundary.json"));
    Some(Config { out, top: value("--profile-top").and_then(|top| top.parse().ok()).unwrap_or(30), phase: value("--profile-phase") })
}

/// Whether `--profile` in `args` is the timing profile (not `--profile boundary`)
pub fn timing_requested(args: &[String]) -> bool {
    args.iter().enumerate().any(|(index, arg)| arg == "--profile" && args.get(index + 1).map(String::as_str) != Some("boundary"))
}

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// The Lua thread of the metamethod now running (set by `enter`, only while profiling)
    static CURRENT: Cell<*mut ffi::lua_State> = const { Cell::new(std::ptr::null_mut()) };
    static RECORDER: RefCell<Option<Recorder>> = const { RefCell::new(None) };
}

/// Whether this thread is profiling
#[inline]
pub fn active() -> bool {
    ACTIVE.with(Cell::get)
}

/// How many threads are profiling (the VM's interrupt reads this before the thread's own switch)
static ON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[inline]
pub fn on() -> bool {
    ON.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// Called by the Instance metamethods and the engine's Lua callbacks on entry: the thread whose stack names the call site.
#[inline]
pub fn enter(state: *mut ffi::lua_State) {
    if active() {
        CURRENT.with(|current| current.set(state));
    }
}

#[derive(Default, Clone, Copy)]
struct Stat {
    count: u64,
    cost: f64,
    /// writes that changed nothing
    unchanged: u64,
    repeat_cost: f64,
    unchanged_cost: f64,
    /// reads of a value read before in the frame and unchanged since: by the same call of the same function, by a later call of
    /// it, by another function
    same_call: u64,
    later_call: u64,
    other_function: u64,
}

impl Stat {
    /// Repeats a cache in the reading function would remove (in the same call, or across its calls)
    fn repeats(&self) -> u64 {
        self.same_call + self.later_call
    }
}

#[derive(Default, Clone, Copy)]
struct MemberStat {
    reads: u64,
    /// distinct (instance, member) pairs read in a frame, summed over the frames
    distinct: u64,
    same_call: u64,
    later_call: u64,
    other_function: u64,
}

#[derive(Default, Clone)]
struct Phase {
    name: String,
    seconds: f64,
    /// the physical frames the phase ran in, and how many there are (a frame the phase was marked in the middle of counts
    /// once for each phase it ran in)
    frames: u64,
    frame_ids: BTreeSet<u64>,
    stats: FxHashMap<(u32, u32), Stat>,
    members: FxHashMap<u32, MemberStat>,
    /// seconds the Instance API charged, and the part of it a crossing accounts for
    charged: f64,
    attributed: f64,
    /// seconds charged for the engine delivering events to handlers (a handler thread resumed, a signal queued): not a crossing
    dispatched: f64,
}

impl Phase {
    /// Notes that the phase ran in this physical frame, which counts once however often the phase is marked in it.
    /// Frame 0 is the count before the first render, not a frame, so it is never recorded.
    fn ran_in_frame(&mut self, frame: u64) {
        if frame == 0 {
            return;
        }
        self.frame_ids.insert(frame);
        self.frames = self.frame_ids.len() as u64;
    }
}

struct Site {
    script: u32,
    function: u32,
    line: i32,
}

struct Function {
    script: u32,
    defined: i32,
    name: String,
}

struct Member {
    kind: Kind,
    class: String,
    member: String,
}

struct Recorder {
    config: Config,
    scripts: Vec<String>,
    script_ids: FxHashMap<usize, u32>,
    sites: Vec<Site>,
    site_ids: FxHashMap<(usize, i32, i32), u32>,
    functions: Vec<Function>,
    function_ids: FxHashMap<(u32, i32), u32>,
    members: Vec<Member>,
    member_ids: FxHashMap<(u8, u32, u32), u32>,
    member_texts: HashMap<(u8, String, String), u32>,
    phases: Vec<Phase>,
    current: usize,
    /// the render steps so far; the one in progress is not counted until it renders
    frame: u64,
    /// the scheduler step in progress, if any (see [`Frame`])
    open_step: Option<OpenStep>,
    /// reads this frame: (function, instance, member) -> the value's mark then (the instance's version and the writes the
    /// profiler saw to it) and the call that read it; (instance, member) -> the function that read it last and the mark then
    read_by_function: FxHashMap<(u32, u32, u32), (Mark, Activation)>,
    read_by_any: FxHashMap<(u32, u32), (u32, Mark)>,
    /// writes the profiler saw to an instance this frame (a script's)
    writes: FxHashMap<u32, u32>,
    /// calls ended at each (thread, depth): a RETURN at a depth ends the call there (the next frame at that depth is another)
    returns: FxHashMap<(usize, i32), u64>,
    /// the last crossing, which a follow-up charge of the same call (a whole-call price) adds its cost to
    last: Option<(u32, u32)>,
    last_saved: (bool, bool),
    accesses: Vec<Access>,
    /// the VM's host-call counters at the last safepoint, and the call site that safepoint was at
    host: Option<(u64, i64)>,
    previous: Option<u32>,
    datatype: Option<u32>,
}

/// Starts profiling on this thread. `phase` names are the luau_profile ones (`startup`, `frames`, what the harness set).
pub fn begin(config: Config) {
    RECORDER.with(|recorder| {
        *recorder.borrow_mut() = Some(Recorder {
            config,
            scripts: Vec::new(),
            script_ids: FxHashMap::default(),
            sites: Vec::new(),
            site_ids: FxHashMap::default(),
            functions: Vec::new(),
            function_ids: FxHashMap::default(),
            members: Vec::new(),
            member_ids: FxHashMap::default(),
            member_texts: HashMap::new(),
            phases: vec![Phase { name: "startup".into(), ..Phase::default() }],
            current: 0,
            frame: 0,
            open_step: None,
            read_by_function: FxHashMap::default(),
            read_by_any: FxHashMap::default(),
            writes: FxHashMap::default(),
            returns: FxHashMap::default(),
            last: None,
            last_saved: (false, false),
            accesses: Vec::new(),
            host: None,
            previous: None,
            datatype: None,
        })
    });
    if !ACTIVE.with(|active| active.replace(true)) {
        ON.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A frame begins (`delta` virtual seconds long): the repeat table starts over and the phase's clock advances.
pub fn begin_frame(delta: f64) {
    with(|recorder| {
        recorder.frame += 1;
        let phase = &mut recorder.phases[recorder.current];
        phase.seconds += delta;
        phase.ran_in_frame(recorder.frame);
        recorder.read_by_function.clear();
        recorder.read_by_any.clear();
        recorder.writes.clear();
        recorder.accesses.clear();
    });
}

/// A scheduler step that has begun and not yet ended.
struct OpenStep {
    /// the render count when the step began; the step has rendered once the count is higher
    frame_at_start: u64,
    /// the phases that were current at some moment of the step: the one it began in, and each one marked since
    phases: BTreeSet<usize>,
}

/// Held by a scheduler step from its start to its end, so that the phases marked during the step count in the
/// frame the step renders, and to no other.
///
/// A step runs the Heartbeat handlers and then renders, and a handler can mark a new phase on the way. Both the phase
/// the step began in and the phase marked later have then run in the frame the step renders, so both get it. A step
/// that does not render (a recorded replay's tail, which only resumes timers) is no frame, so its phases get none.
/// The render count only moves when the step renders, which is how the guard tells the two apart when the step ends.
pub(crate) struct Frame {
    /// whether this guard opened the step; false for a step started inside another, which the outer guard accounts for
    opened_step: bool,
}

impl Frame {
    /// Opens a step. When one is already open (a step that runs another, as `step` runs `step_recorded`), this does
    /// nothing, and the outer guard counts the phases at its end.
    pub(crate) fn begin() -> Self {
        let mut opened_step = false;
        with(|recorder| {
            if recorder.open_step.is_some() {
                return;
            }
            recorder.open_step = Some(OpenStep { frame_at_start: recorder.frame, phases: BTreeSet::from([recorder.current]) });
            opened_step = true;
        });
        Self { opened_step }
    }
}

impl Drop for Frame {
    /// Ends the step: counts the phases it held in the frame it rendered, if it rendered one
    fn drop(&mut self) {
        if !self.opened_step {
            return;
        }
        with(|recorder| {
            let Some(step) = recorder.open_step.take() else { return };
            let rendered = recorder.frame > step.frame_at_start;
            if !rendered {
                return;
            }
            for phase in step.phases {
                recorder.phases[phase].ran_in_frame(recorder.frame);
            }
        });
    }
}

/// A property read or write under way (raw.rs `meta_index`, `meta_newindex`)
struct Access {
    /// the VM's C calls, host calls and host nanoseconds as it began
    at: (u64, u64, i64),
    /// the crossing it recorded: its key, and whether it was a repeat read or an unchanged write
    crossing: Option<((u32, u32), bool, bool)>,
    /// what the reads and writes nested in it charged (seconds), not its own
    nested: f64,
}

/// A property read or write begins: what the VM's call counters read now, so its end can book what its own C and host
/// calls cost the work clock
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn access_begin(state: *mut ffi::lua_State) {
    if !active() {
        return;
    }
    let counters = unsafe { &*crate::cost::lua_workcounters(state) };
    let at = (counters.ccalls, counters.hostcalls, counters.hostnanos);
    with(|recorder| recorder.accesses.push(Access { at, crossing: None, nested: 0.0 }));
}

/// A property read or write ends: when the crossing it recorded was a repeat read or an unchanged write, what the script's
/// side of it cost is booked with it, as removing it saves that too: the keyed access (GETTABLEKS or SETTABLEKS, at its
/// interpreted price) and the C and host calls the VM charged while it ran (a getter's call is charged, the __index call
/// itself is not), the reads nested in it aside
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn access_end(state: *mut ffi::lua_State, write: bool) {
    if !active() {
        return;
    }
    let counters = unsafe { &*crate::cost::lua_workcounters(state) };
    let now = (counters.ccalls, counters.hostcalls, counters.hostnanos);
    with(|recorder| {
        let Some(access) = recorder.accesses.pop() else { return };
        // (what the VM charged for the C and host calls made while the access ran, in ns, then seconds)
        let c_calls = now.0.saturating_sub(access.at.0) as f64;
        let host_calls = now.1.saturating_sub(access.at.1) as f64;
        let host_ns = (now.2 - access.at.2).max(0) as f64;
        let calls_ns = c_calls * crate::cost::LUAU_CCALL + host_calls * crate::cost::LUAU_HOSTCALL + host_ns;
        let spent = calls_ns * 1e-9;
        if let Some(parent) = recorder.accesses.last_mut() {
            parent.nested += spent;
        }
        // (the keyed access itself: GETTABLEKS for a read, SETTABLEKS for a write)
        let keyed_access_op = if write { SETTABLEKS } else { GETTABLEKS };
        let keyed_access = crate::cost::INTERPRETED_OPS[keyed_access_op] * 1e-9;
        let own = (spent - access.nested).max(0.0) + keyed_access;
        if let Some((key, repeat, unchanged)) = access.crossing
            && let Some(stat) = recorder.phases[recorder.current].stats.get_mut(&key)
        {
            if repeat {
                stat.repeat_cost += own;
            }
            if unchanged {
                stat.unchanged_cost += own;
            }
        }
    });
}

/// Names the phase the crossings after this call belong to (luau_profile::set_phase)
pub fn set_phase(name: &str) {
    with(|recorder| {
        recorder.current = match recorder.phases.iter().position(|phase| phase.name == name) {
            Some(index) => index,
            None => {
                recorder.phases.push(Phase { name: name.into(), ..Phase::default() });
                recorder.phases.len() - 1
            }
        };
        // Only a mark inside a step is held for that step's frame. A mark outside any step (before the first one, when no
        // frame has rendered yet) ran in no frame, so it adds none.
        if let Some(step) = recorder.open_step.as_mut() {
            step.phases.insert(recorder.current);
        }
    });
}

#[inline]
fn with(body: impl FnOnce(&mut Recorder)) {
    if !active() {
        return;
    }
    RECORDER.with(|recorder| {
        if let Ok(mut recorder) = recorder.try_borrow_mut() {
            if let Some(recorder) = recorder.as_mut() {
                body(recorder);
            }
        }
    });
}

/// The Instance API charged `seconds` to the work clock (Api::charge, Scheduler::charge)
#[inline]
pub fn charged(seconds: f64) {
    if active() {
        with(|recorder| recorder.phases[recorder.current].charged += seconds);
    }
}

/// The engine charged `seconds` for delivering events to handlers (the scheduler resuming a handler's thread, a signal queued)
#[inline]
pub fn dispatched(seconds: f64) {
    if active() {
        with(|recorder| recorder.phases[recorder.current].dispatched += seconds);
    }
}

/// A read value's mark: the instance's version (every change of its own properties, by a script or the engine, moves it) and the
/// writes the profiler saw to it; a read whose mark is the earlier read's read the same value
#[derive(Clone, Copy, PartialEq, Eq)]
struct Mark {
    version: u32,
    writes: u32,
}

/// A call: its thread, its frame's depth, and the calls that ended at that depth before it
#[derive(Clone, Copy, PartialEq, Eq)]
struct Activation {
    thread: usize,
    depth: i32,
    generation: u64,
}

unsafe extern "C-unwind" {
    fn lua_stackdepth(state: *mut ffi::lua_State) -> std::ffi::c_int;
}

/// A crossing: `class` and `member` are the reflection's ids (or [`NO_MEMBER`]); `names` gives their text the first time
/// they are seen. `cost` is the charge the crossing took. `changed` is false for a write that stored what the property
/// held. `version` is a read instance's version (None when not known: no repeat is counted). Nothing is recorded when no
/// game Luau is running (the engine's own work).
#[allow(clippy::too_many_arguments)]
pub fn record(kind: Kind, class: u32, member: u32, instance: u32, version: Option<u32>, cost: f64, changed: bool, names: impl FnOnce() -> (String, String)) {
    if !active() {
        return;
    }
    let state = CURRENT.with(Cell::get);
    with(|recorder| {
        let Some((site, activation)) = recorder.site_call(state) else { return };
        let member = match recorder.member_ids.get(&(kind as u8, class, member)) {
            Some(&id) => id,
            None => {
                let (class_name, member_name) = names();
                let id = recorder.members.len() as u32;
                recorder.members.push(Member { kind, class: class_name, member: member_name });
                recorder.member_ids.insert((kind as u8, class, member), id);
                id
            }
        };
        recorder.count(kind, site, member, cost, changed);
        if kind == Kind::Set && instance != NO_INSTANCE {
            *recorder.writes.entry(instance).or_default() += 1;
        }
        if kind == Kind::Get
            && instance != NO_INSTANCE
            && let Some(version) = version
        {
            recorder.repeat(site, member, instance, version, activation, cost);
        }
    });
}

/// A crossing named by text (an event connect, a datatype): not hot enough to need ids.
pub fn record_named(kind: Kind, class: &str, member: &str, cost: f64) {
    if !active() {
        return;
    }
    let state = CURRENT.with(Cell::get);
    with(|recorder| {
        let Some(site) = recorder.site(state) else { return };
        let member = recorder.text_member(kind, class, member);
        recorder.count(kind, site, member, cost, true);
    });
}

/// More cost for the crossing just recorded (a method's whole-call price, a list's elements): added to it, not counted again.
pub fn add_cost(seconds: f64) {
    with(|recorder| {
        if let Some(key) = recorder.last {
            let phase = &mut recorder.phases[recorder.current];
            if let Some(stat) = phase.stats.get_mut(&key) {
                stat.cost += seconds;
                if recorder.last_saved.0 {
                    stat.repeat_cost += seconds;
                }
                if recorder.last_saved.1 {
                    stat.unchanged_cost += seconds;
                }
                phase.attributed += seconds;
            }
        }
    });
}

/// Called from the VM interrupt (a call, return or loop edge): books the datatype operations the VM counted since the
/// last safepoint to the call site that safepoint was at, then notes this one.
pub fn safepoint(state: *mut ffi::lua_State) {
    with(|recorder| {
        recorder.note_return(state);
        let counters = unsafe { &*crate::cost::lua_workcounters(state) };
        let now = (counters.hostcalls, counters.hostnanos);
        let before = recorder.host.replace(now);
        if let (Some((calls, nanos)), Some(site)) = (before, recorder.previous) {
            if now.0 != calls || now.1 != nanos {
                let cost = ((now.0 - calls) as f64 * crate::cost::LUAU_HOSTCALL + (now.1 - nanos) as f64) * 1e-9;
                let member = match recorder.datatype {
                    Some(member) => member,
                    None => {
                        let member = recorder.text_member(Kind::Datatype, "VM", "host call (datatype op or engine function)");
                        recorder.datatype = Some(member);
                        member
                    }
                };
                recorder.count_n(Kind::Datatype, site, member, now.0 - calls, cost, true);
            }
        }
        recorder.previous = recorder.site(state);
    });
}

impl Recorder {
    /// An interrupt at a RETURN ends the call at its depth: a read there after it is another call's
    fn note_return(&mut self, state: *mut ffi::lua_State) {
        if unsafe { crate::profiler::vm::current_op(state) } == Some(OP_RETURN) {
            let depth = unsafe { lua_stackdepth(state) };
            *self.returns.entry((state as usize, depth)).or_default() += 1;
        }
    }

    fn text_member(&mut self, kind: Kind, class: &str, member: &str) -> u32 {
        let key = (kind as u8, class.to_string(), member.to_string());
        if let Some(&id) = self.member_texts.get(&key) {
            return id;
        }
        let id = self.members.len() as u32;
        self.members.push(Member { kind, class: class.into(), member: member.into() });
        self.member_texts.insert(key, id);
        id
    }

    fn count(&mut self, kind: Kind, site: u32, member: u32, cost: f64, changed: bool) {
        self.count_n(kind, site, member, 1, cost, changed);
    }

    #[allow(clippy::too_many_arguments)]
    fn count_n(&mut self, kind: Kind, site: u32, member: u32, times: u64, cost: f64, changed: bool) {
        let phase = &mut self.phases[self.current];
        let stat = phase.stats.entry((site, member)).or_default();
        stat.count += times;
        stat.cost += cost;
        if kind != Kind::Datatype {
            phase.attributed += cost;
        }
        if kind == Kind::Set && !changed {
            stat.unchanged += 1;
            stat.unchanged_cost += cost;
        }
        let unchanged = kind == Kind::Set && !changed;
        self.last = Some((site, member));
        self.last_saved = (false, unchanged);
        if matches!(kind, Kind::Get | Kind::Set)
            && let Some(access) = self.accesses.last_mut().filter(|access| access.crossing.is_none())
        {
            access.crossing = Some(((site, member), false, unchanged));
        }
    }

    /// A read of `member` of `instance` at `site`: a repeat when its value is the one an earlier read in the frame saw (the
    /// instance's version and the writes to it unchanged since), by the same call of the reading function, by a later call of
    /// it, or by another function
    fn repeat(&mut self, site: u32, member: u32, instance: u32, version: u32, activation: Activation, cost: f64) {
        let function = self.sites[site as usize].function;
        let mark = Mark { version, writes: self.writes.get(&instance).copied().unwrap_or(0) };
        let phase = &mut self.phases[self.current];
        let extra = phase.members.entry(member).or_default();
        extra.reads += 1;
        let by_function = self.read_by_function.insert((function, instance, member), (mark, activation));
        let by_any = self.read_by_any.insert((instance, member), (function, mark));
        if by_any.is_none() {
            extra.distinct += 1;
        }
        let stat = phase.stats.entry((site, member)).or_default();
        match by_function {
            Some((seen, call)) if seen == mark => {
                stat.repeat_cost += cost;
                self.last_saved.0 = true;
                if let Some(access) = self.accesses.last_mut()
                    && let Some((key, repeat, _)) = access.crossing.as_mut()
                    && *key == (site, member)
                {
                    *repeat = true;
                }
                if call == activation {
                    stat.same_call += 1;
                    extra.same_call += 1;
                } else {
                    stat.later_call += 1;
                    extra.later_call += 1;
                }
            }
            _ => {
                if let Some((other, seen)) = by_any
                    && other != function
                    && seen == mark
                {
                    stat.other_function += 1;
                    extra.other_function += 1;
                }
            }
        }
    }

    /// The call site of `state`'s stack and the call it is in
    fn site_call(&mut self, state: *mut ffi::lua_State) -> Option<(u32, Activation)> {
        let (site, level) = self.site_level(state)?;
        let depth = unsafe { lua_stackdepth(state) } - level;
        let generation = self.returns.get(&(state as usize, depth)).copied().unwrap_or(0);
        Some((site, Activation { thread: state as usize, depth, generation }))
    }

    fn site(&mut self, state: *mut ffi::lua_State) -> Option<u32> {
        self.site_level(state).map(|(site, _)| site)
    }

    /// The call site (the innermost frame of the game's own Luau) of `state`'s stack, and the level of its frame
    fn site_level(&mut self, state: *mut ffi::lua_State) -> Option<(u32, i32)> {
        // (a thread that is not running is a stale one: the crossing came from the engine, not from Luau)
        if state.is_null() || unsafe { ffi::lua_status(state) } != ffi::LUA_OK {
            return None;
        }
        unsafe {
            let mut debug: ffi::lua_Debug = std::mem::zeroed();
            let mut level = 0;
            while level < 40 && ffi::lua_getinfo(state, level, c"sl".as_ptr(), &mut debug) != 0 {
                let here = level;
                level += 1;
                if debug.short_src.is_null() || debug.source.is_null() || debug.currentline < 0 {
                    continue;
                }
                let key = (debug.source as usize, debug.linedefined, debug.currentline);
                if let Some(&site) = self.site_ids.get(&key) {
                    return Some((site, here));
                }
                let script = match self.script_ids.get(&(debug.source as usize)) {
                    Some(&script) => script,
                    None => {
                        let text = CStr::from_ptr(debug.short_src).to_string_lossy().into_owned();
                        let engine = text == "[C]" || text.starts_with("ree.") || text.starts_with("rur.") || text.contains("prelude");
                        // (the engine's own Luau is marked with an empty name; it never becomes a site)
                        let name = if engine { String::new() } else { text.strip_prefix("[string \"").and_then(|inner| inner.strip_suffix("\"]")).unwrap_or(&text).to_string() };
                        self.scripts.push(name);
                        let id = self.scripts.len() as u32 - 1;
                        self.script_ids.insert(debug.source as usize, id);
                        id
                    }
                };
                if self.scripts[script as usize].is_empty() {
                    continue;
                }
                // (the function's name is the caller's name for it: read once per call site)
                let mut named: ffi::lua_Debug = std::mem::zeroed();
                let name = if ffi::lua_getinfo(state, here, c"n".as_ptr(), &mut named) != 0 && !named.name.is_null() {
                    CStr::from_ptr(named.name).to_string_lossy().into_owned()
                } else {
                    String::new()
                };
                let function = match self.function_ids.get(&(script, debug.linedefined)) {
                    Some(&function) => function,
                    None => {
                        self.functions.push(Function { script, defined: debug.linedefined, name: name.clone() });
                        let id = self.functions.len() as u32 - 1;
                        self.function_ids.insert((script, debug.linedefined), id);
                        id
                    }
                };
                if self.functions[function as usize].name.is_empty() {
                    self.functions[function as usize].name = name.clone();
                }
                self.sites.push(Site { script, function, line: debug.currentline });
                let site = self.sites.len() as u32 - 1;
                self.site_ids.insert(key, site);
                return Some((site, here));
            }
        }
        None
    }

    fn function_label(&self, function: u32) -> String {
        let function = &self.functions[function as usize];
        let name = if function.name.is_empty() { "anonymous" } else { function.name.as_str() };
        format!("{name}:{}", function.defined)
    }

    /// The phase to report: the ones asked for (`--profile-phase a,b`, or `all`; their crossings and seconds added), else `steady`
    /// when the harness named one, else the one with the most frames
    fn reported(&self) -> Phase {
        let named = |name: &str| self.phases.iter().find(|phase| phase.name == name && phase.frames > 0);
        let Some(wanted) = self.config.phase.as_deref() else {
            return named("steady").or_else(|| self.phases.iter().max_by_key(|phase| phase.frames)).unwrap_or(&self.phases[0]).clone();
        };
        let names: Vec<&str> = wanted.split(',').map(str::trim).collect();
        let chosen: Vec<&Phase> = self.phases.iter().filter(|phase| names.contains(&"all") || names.contains(&phase.name.as_str())).collect();
        if chosen.is_empty() {
            return self.phases.iter().max_by_key(|phase| phase.frames).unwrap_or(&self.phases[0]).clone();
        }
        let label = if names.contains(&"all") {
            "all".to_string()
        } else if names.len() > 6 {
            format!("{} phases {}..{}", names.len(), names[0], names[names.len() - 1])
        } else {
            wanted.to_string()
        };
        let mut merged = Phase { name: label, ..Phase::default() };
        for phase in chosen {
            merged.seconds += phase.seconds;
            // (a frame two of the phases ran in is one frame, not two)
            merged.frame_ids.extend(&phase.frame_ids);
            merged.frames = merged.frame_ids.len() as u64;
            merged.charged += phase.charged;
            merged.attributed += phase.attributed;
            merged.dispatched += phase.dispatched;
            for (&key, stat) in &phase.stats {
                let into = merged.stats.entry(key).or_default();
                into.count += stat.count;
                into.cost += stat.cost;
                into.unchanged += stat.unchanged;
                into.repeat_cost += stat.repeat_cost;
                into.unchanged_cost += stat.unchanged_cost;
                into.same_call += stat.same_call;
                into.later_call += stat.later_call;
                into.other_function += stat.other_function;
            }
            for (&key, stat) in &phase.members {
                let into = merged.members.entry(key).or_default();
                into.reads += stat.reads;
                into.distinct += stat.distinct;
                into.same_call += stat.same_call;
                into.later_call += stat.later_call;
                into.other_function += stat.other_function;
            }
        }
        merged
    }

    fn report(&self) -> (Json, String) {
        let phase = self.reported();
        let phase = &phase;
        // (a phase in which no virtual time passed, a boot that runs inside one frame: its figures are per run, not per second)
        let zero_time = phase.seconds <= 0.0;
        let seconds = if zero_time { 1.0 } else { phase.seconds };
        let member_name = |member: u32| {
            let member = &self.members[member as usize];
            format!("{}.{}", member.class, member.member)
        };
        let ms = |cost: f64| cost / seconds * 1e3;

        // (a member read through different classes of instance, Part.CFrame and MeshPart.CFrame, is one: BasePart.CFrame)
        let mut canonical: HashMap<(u8, &str, &str), u32> = HashMap::new();
        let canon: Vec<u32> = self.members.iter().enumerate().map(|(id, member)| *canonical.entry((member.kind as u8, &member.class, &member.member)).or_insert(id as u32)).collect();
        let mut merged: FxHashMap<(u32, u32), Stat> = FxHashMap::default();
        for (&(site, member), stat) in &phase.stats {
            let into = merged.entry((site, canon[member as usize])).or_default();
            into.count += stat.count;
            into.cost += stat.cost;
            into.unchanged += stat.unchanged;
            into.repeat_cost += stat.repeat_cost;
            into.unchanged_cost += stat.unchanged_cost;
            into.same_call += stat.same_call;
            into.later_call += stat.later_call;
            into.other_function += stat.other_function;
        }
        let mut extras: FxHashMap<u32, MemberStat> = FxHashMap::default();
        for (&member, stat) in &phase.members {
            let into = extras.entry(canon[member as usize]).or_default();
            into.reads += stat.reads;
            into.distinct += stat.distinct;
            into.same_call += stat.same_call;
            into.later_call += stat.later_call;
            into.other_function += stat.other_function;
        }

        let all_sites: Vec<_> = merged.iter().map(|(&(site, member), stat)| (site, member, *stat)).collect();
        // (the VM's host calls, datatype operations and engine functions, are ranked apart from the crossings)
        let is_host = |member: u32| self.members[member as usize].kind == Kind::Datatype;
        let mut sites: Vec<_> = all_sites.iter().filter(|row| !is_host(row.1)).cloned().collect();
        let mut host_sites: Vec<_> = all_sites.iter().filter(|row| is_host(row.1)).cloned().collect();
        sites.sort_by(|a, b| b.2.count.cmp(&a.2.count).then(a.0.cmp(&b.0)));
        host_sites.sort_by(|a, b| b.2.cost.total_cmp(&a.2.cost).then(a.0.cmp(&b.0)));
        let site_json = |(site, member, stat): &(u32, u32, Stat)| {
            let info = &self.sites[*site as usize];
            json!({
                "script": self.scripts[info.script as usize],
                "function": self.function_label(info.function),
                "line": info.line,
                "kind": self.members[*member as usize].kind.label(),
                "member": member_name(*member),
                "count": stat.count,
                "per_s": stat.count as f64 / seconds,
                "cost_ms_per_s": ms(stat.cost),
                "unchanged_writes": stat.unchanged,
                // (a value read again unchanged by the same function: in the same call, a local holds it; in a later call, the
                // caller can read it once; repeat_reads is the two)
                "repeat_reads": stat.repeats(),
                "repeat_cost_ms": stat.repeat_cost * 1e3,
                "unchanged_cost_ms": stat.unchanged_cost * 1e3,
                "repeat_reads_per_s": stat.repeats() as f64 / seconds,
                "repeat_same_call": stat.same_call,
                "repeat_later_call": stat.later_call,
                "repeat_other_function": stat.other_function,
            })
        };
        let mut by_cost = sites.clone();
        by_cost.sort_by(|a, b| b.2.cost.total_cmp(&a.2.cost).then(a.0.cmp(&b.0)));

        let mut members: FxHashMap<u32, (u64, f64, u64)> = FxHashMap::default();
        let mut scripts: FxHashMap<u32, (u64, f64)> = FxHashMap::default();
        let mut functions: FxHashMap<u32, (u64, f64)> = FxHashMap::default();
        let (mut crossings, mut priced, mut host_calls, mut host_cost) = (0u64, 0.0, 0u64, 0.0);
        for (site, member, stat) in &all_sites {
            let info = &self.sites[*site as usize];
            let entry = members.entry(*member).or_default();
            entry.0 += stat.count;
            entry.1 += stat.cost;
            entry.2 += 1;
            if !is_host(*member) {
                for (table, key) in [(&mut scripts, info.script), (&mut functions, info.function)] {
                    let entry = table.entry(key).or_default();
                    entry.0 += stat.count;
                    entry.1 += stat.cost;
                }
            }
            // (the VM's host calls include the engine functions a script calls through the Instance API, which are already
            // crossings of their own: they are reported apart, not added to the crossings)
            if self.members[*member as usize].kind == Kind::Datatype {
                host_calls += stat.count;
                host_cost += stat.cost;
            } else {
                crossings += stat.count;
                priced += stat.cost;
            }
        }
        let mut member_rows: Vec<_> = members.into_iter().collect();
        member_rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let frames = phase.frames.max(1) as f64;
        let member_json = |(member, (count, cost, call_sites)): &(u32, (u64, f64, u64))| {
            let extra = extras.get(member).copied().unwrap_or_default();
            json!({
                "member": member_name(*member),
                "kind": self.members[*member as usize].kind.label(),
                "count": count,
                "per_s": *count as f64 / seconds,
                "cost_ms_per_s": ms(*cost),
                "call_sites": call_sites,
                "reads_per_frame": extra.reads as f64 / frames,
                "distinct_per_frame": extra.distinct as f64 / frames,
                "same_call_repeats_per_frame": extra.same_call as f64 / frames,
                "later_call_repeats_per_frame": extra.later_call as f64 / frames,
                "other_function_repeats_per_frame": extra.other_function as f64 / frames,
            })
        };
        let mut script_rows: Vec<_> = scripts.into_iter().collect();
        script_rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let mut function_rows: Vec<_> = functions.into_iter().collect();
        function_rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        let mut repeats: Vec<_> = sites.iter().filter(|row| row.2.repeats() + row.2.other_function > 0).cloned().collect();
        repeats.sort_by(|a, b| (b.2.repeats() + b.2.other_function).cmp(&(a.2.repeats() + a.2.other_function)).then(a.0.cmp(&b.0)));
        let mut unchanged: Vec<_> = sites.iter().filter(|row| row.2.unchanged > 0).cloned().collect();
        unchanged.sort_by(|a, b| b.2.unchanged.cmp(&a.2.unchanged).then(a.0.cmp(&b.0)));

        let coverage = if phase.charged > 0.0 { phase.attributed / phase.charged } else { 1.0 };
        let report = json!({
            "version": 1,
            "phase": phase.name,
            "phases": self.phases.iter().map(|phase| {
                let (mut count, mut cost) = (0u64, 0.0);
                for (&(_, member), stat) in &phase.stats {
                    if self.members[member as usize].kind != Kind::Datatype {
                        count += stat.count;
                        cost += stat.cost;
                    }
                }
                let rate = |value: f64| if phase.seconds > 0.0 { json!(value / phase.seconds) } else { Json::Null };
                json!({"phase": phase.name, "seconds": phase.seconds, "frames": phase.frames, "crossings": count, "cost_ms": cost * 1e3, "crossings_per_s": rate(count as f64), "cost_ms_per_s": rate(cost * 1e3)})
            }).collect::<Vec<_>>(),
            "emulated_seconds": phase.seconds,
            "zero_time": zero_time,
            "frames": phase.frames,
            "crossings": crossings,
            "crossings_per_s": crossings as f64 / seconds,
            "cost_ms_per_s": ms(priced),
            "host_calls": host_calls,
            "host_calls_per_s": host_calls as f64 / seconds,
            "host_calls_cost_ms_per_s": ms(host_cost),
            "instance_api_charged_ms_per_s": ms(phase.charged),
            "event_dispatch_ms_per_s": ms(phase.dispatched),
            "attributed_share_of_instance_api_charges": coverage,
            "by_site_count": sites.iter().take(300).map(site_json).collect::<Vec<_>>(),
            "by_site_cost": by_cost.iter().take(300).map(site_json).collect::<Vec<_>>(),
            "by_site_host_calls": host_sites.iter().take(300).map(site_json).collect::<Vec<_>>(),
            "by_member": member_rows.iter().take(500).map(member_json).collect::<Vec<_>>(),
            "by_module": script_rows.iter().take(400).map(|(script, (count, cost))| json!({
                "script": self.scripts[*script as usize], "count": count, "per_s": *count as f64 / seconds, "cost_ms_per_s": ms(*cost),
            })).collect::<Vec<_>>(),
            "by_function": function_rows.iter().take(300).map(|(function, (count, cost))| {
                let info = &self.functions[*function as usize];
                json!({"script": self.scripts[info.script as usize], "function": self.function_label(*function), "count": count, "per_s": *count as f64 / seconds, "cost_ms_per_s": ms(*cost)})
            }).collect::<Vec<_>>(),
            "repeat_reads": repeats.iter().take(200).map(site_json).collect::<Vec<_>>(),
            "unchanged_writes": unchanged.iter().take(200).map(site_json).collect::<Vec<_>>(),
        });

        let top = self.config.top;
        let mut text = format!(
            "boundary profile: phase {} ({:.2} emulated s, {} frames{}): {} crossings ({:.0}/s), priced {:.2} ms/s; {:.0} % of the Instance API's {:.2} ms/s of charges attributed\n\
             VM host calls (datatype operations and engine functions): {:.0}/s, {:.2} ms/s; the engine delivering events to handlers: {:.2} ms/s (not a crossing)\n",
            phase.name,
            phase.seconds,
            phase.frames,
            if zero_time { "; no virtual time passed, so the rates are per run" } else { "" },
            crossings,
            crossings as f64 / seconds,
            ms(priced),
            coverage * 100.0,
            ms(phase.charged),
            host_calls as f64 / seconds,
            ms(host_cost),
            ms(phase.dispatched)
        );
        let table = |title: &str, rows: &[(u32, u32, Stat)], text: &mut String| {
            text.push_str(&format!("\n{title}\n{:>10} {:>10} {:>8}  {:<8} {:<44} {}\n", "per s", "ms/s", "repeats", "kind", "member", "script:line function"));
            for (site, member, stat) in rows.iter().take(top) {
                let info = &self.sites[*site as usize];
                text.push_str(&format!(
                    "{:>10.1} {:>10.3} {:>8.1}  {:<8} {:<44} {}:{} {}\n",
                    stat.count as f64 / seconds,
                    ms(stat.cost),
                    stat.repeats() as f64 / seconds,
                    self.members[*member as usize].kind.label(),
                    member_name(*member),
                    self.scripts[info.script as usize],
                    info.line,
                    self.function_label(info.function)
                ));
            }
        };
        table("top call sites by crossings per emulated second", &sites, &mut text);
        table("top call sites by priced cost", &by_cost, &mut text);
        table("VM host calls (datatype operations and engine functions) by priced cost", &host_sites, &mut text);
        text.push_str(&format!("\nby Class.Member\n{:>10} {:>10} {:>9} {:>9} {:>9} {:>6}  {:<8} {}\n", "per s", "ms/s", "reads/fr", "distinct", "repeat/fr", "sites", "kind", "member"));
        for row in member_rows.iter().take(top) {
            let extra = extras.get(&row.0).copied().unwrap_or_default();
            text.push_str(&format!(
                "{:>10.1} {:>10.3} {:>9.1} {:>9.1} {:>9.1} {:>6}  {:<8} {}\n",
                row.1.0 as f64 / seconds,
                ms(row.1.1),
                extra.reads as f64 / frames,
                extra.distinct as f64 / frames,
                (extra.same_call + extra.later_call + extra.other_function) as f64 / frames,
                row.1.2,
                self.members[row.0 as usize].kind.label(),
                member_name(row.0)
            ));
        }
        text.push_str(&format!("\nby module\n{:>10} {:>10}  {}\n", "per s", "ms/s", "script"));
        for (script, (count, cost)) in script_rows.iter().take(top) {
            text.push_str(&format!("{:>10.1} {:>10.3}  {}\n", *count as f64 / seconds, ms(*cost), self.scripts[*script as usize]));
        }
        text.push_str(&format!(
            "\nrepeat reads (a value read again in the frame, unchanged since: same call, a local holds it; later call, read it once in the caller and pass it; other function, another function read it)\n{:>10} {:>10} {:>10} {:>10}  {:<44} {}\n",
            "same/s", "later/s", "other/s", "of reads/s", "member", "script:line function"
        ));
        for (site, member, stat) in repeats.iter().take(top) {
            let info = &self.sites[*site as usize];
            text.push_str(&format!(
                "{:>10.1} {:>10.1} {:>10.1} {:>10.1}  {:<44} {}:{} {}\n",
                stat.same_call as f64 / seconds,
                stat.later_call as f64 / seconds,
                stat.other_function as f64 / seconds,
                stat.count as f64 / seconds,
                member_name(*member),
                self.scripts[info.script as usize],
                info.line,
                self.function_label(info.function)
            ));
        }
        text.push_str(&format!("\nunchanged writes (a write that stored what the property held)\n{:>10} {:>10}  {:<44} {}\n", "per s", "of writes/s", "member", "script:line function"));
        for (site, member, stat) in unchanged.iter().take(top) {
            let info = &self.sites[*site as usize];
            text.push_str(&format!(
                "{:>10.1} {:>10.1}  {:<44} {}:{} {}\n",
                stat.unchanged as f64 / seconds,
                stat.count as f64 / seconds,
                member_name(*member),
                self.scripts[info.script as usize],
                info.line,
                self.function_label(info.function)
            ));
        }
        (report, text)
    }
}

/// Ends profiling: the report as text, and as JSON in the configured file (its folder made when missing). None when
/// this thread was not profiling.
pub fn finish() -> Option<String> {
    if ACTIVE.with(|active| active.replace(false)) {
        ON.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    let recorder = RECORDER.with(|recorder| recorder.borrow_mut().take())?;
    let (report, text) = recorder.report();
    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            // (the profiling suite's boundary.json goes in the profile's folder, which the suite makes only after this, as
            // it writes the rest: a new folder lost the file)
            let folder = recorder.config.out.parent().filter(|folder| !folder.as_os_str().is_empty());
            let written = folder.map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&recorder.config.out, json));
            match written {
                Err(error) => eprintln!("Writing boundary profile {}: {error}", recorder.config.out.display()),
                // a standalone run's default file is in state, a folder the user never named, so say where it went
                Ok(()) if crate::is_standalone_binary() => eprintln!("boundary profile: {}", recorder.config.out.display()),
                Ok(()) => {}
            }
        }
        Err(error) => eprintln!("Encoding boundary profile: {error}"),
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_never_counts_frame_zero() {
        let mut phase = Phase::default();

        phase.ran_in_frame(0);

        assert_eq!(phase.frames, 0);
        assert!(phase.frame_ids.is_empty());
    }

    #[test]
    fn a_phase_counts_a_frame_once_however_often_it_is_marked_in_it() {
        let mut phase = Phase::default();

        phase.ran_in_frame(1);
        phase.ran_in_frame(1);
        phase.ran_in_frame(2);

        assert_eq!(phase.frames, 2);
        assert_eq!(phase.frame_ids, BTreeSet::from([1, 2]));
    }
}

//! The VM's own profile (patches/luau/12-profiling.patch, lprofile.cpp): while it steps, every instruction the interpreter dispatches
//! is counted, what the work clock charged since the previous instruction is booked to that instruction (the VM's work
//! counters priced by [`crate::cost::luau`], and the engine's own charges), a calling-context tree is kept, and the
//! instructions with an inline cache record what it did. Without stepping it only names functions for [`stack`].
//!
//! Native code keeps running natively, unseen: a natively compiled function shows its entries and the work booked while
//! it ran, never instruction counts (they are labelled, not estimated).

use std::ffi::{CStr, c_char, c_int, c_void};

use mlua::ffi::lua_State;

use crate::cost::VmCounters;

/// lua.h `LUA_PROFILE_*`
pub const EXEMPT: i32 = 1;
pub const NATIVE: i32 = 2;

/// What an instruction with an inline cache did, as the interpreter did it (lua.h `LUA_PROFILE_HIT` ...), in this order:
/// [`CACHE_OUTCOME_NAMES`]
pub const CACHE_OUTCOMES: usize = 10;
pub const HIT: usize = 0;
pub const REPREDICTED: usize = 1;
pub const ABSENT: usize = 2;
pub const INSERTED: usize = 3;
pub const REHASHED: usize = 4;
pub const METATABLE: usize = 5;
pub const METAFUNCTION: usize = 6;
pub const UNSAFE_ENV: usize = 7;
pub const NOT_TABLE: usize = 8;
pub const READONLY: usize = 9;
/// The outcomes' names in profile.json (`luau_caches.sites[].outcomes`)
pub const CACHE_OUTCOME_NAMES: [&str; CACHE_OUTCOMES] = ["hit", "repredicted", "absent", "inserted", "rehashed", "metatable_table", "metatable_function", "unsafe_env", "not_table", "readonly"];
/// Metatable walks by the metatables consulted: 1, 2, 3, 4 or more
pub const CACHE_DEPTHS: usize = 4;
/// The types whose metamethods served a site's `not_table` outcomes: three, and the others in the last
pub const CACHE_HANDLERS: usize = 4;

/// Why a builtin's fast path declined a call (lua.h `LUA_PROFILE_DECLINE_*`), in this order: [`DECLINE_NAMES`]
pub const DECLINES: usize = 6;
pub const DECLINE_ARGS: usize = 0;
pub const DECLINE_TYPES: usize = 1;
pub const DECLINE_RESULTS: usize = 2;
pub const DECLINE_GC_DEBT: usize = 3;
pub const DECLINE_UNSAFE_ENV: usize = 4;
pub const DECLINE_VALUES: usize = 5;
/// The reasons' names in profile.json (`builtins[].declines`)
pub const DECLINE_NAMES: [&str; DECLINES] = ["args", "types", "results", "gc_debt", "unsafe_env", "values"];

/// lua.h `LUA_ALLOC_*`: what the memory an allocation callback reports is for
pub const ALLOC_OTHER: i32 = 0;
pub const ALLOC_OBJECT: i32 = 1;
pub const ALLOC_TABLE: i32 = 2;
pub const ALLOC_THREAD: i32 = 3;
pub const ALLOC_STRINGS: i32 = 4;
pub const ALLOC_PROTO: i32 = 5;

/// lua.h `LUA_T*`: the type tags of the objects a heap walk or an allocation names. The value types are mlua-sys's (they
/// bind the same lua.h); Luau 0.746 put LUA_TCLASS and LUA_TOBJECT after LUA_TBUFFER and LUA_TDEADKEY before the
/// collector's own objects (profiler_tests.rs checks these against the vendored lua.h)
pub const T_STRING: i32 = mlua::ffi::LUA_TSTRING;
pub const T_TABLE: i32 = mlua::ffi::LUA_TTABLE;
pub const T_FUNCTION: i32 = mlua::ffi::LUA_TFUNCTION;
pub const T_USERDATA: i32 = mlua::ffi::LUA_TUSERDATA;
pub const T_THREAD: i32 = mlua::ffi::LUA_TTHREAD;
pub const T_BUFFER: i32 = mlua::ffi::LUA_TBUFFER;
pub const T_CLASS: i32 = 12;
pub const T_OBJECT: i32 = 13;
pub const T_PROTO: i32 = 15;
pub const T_UPVAL: i32 = 16;

#[repr(C)]
struct RawFunction {
    source: *const c_char,
    name: *const c_char,
    linedefined: c_int,
    flags: c_int,
    sizecode: c_int,
    code: *const u32,
    lines: *const c_int,
    counts: *const u64,
    work: *const f64,
    ncaches: c_int,
    caches: *const RawCacheSite,
    nbuiltins: c_int,
    builtins: *const RawBuiltinSite,
    nativeentries: u64,
    nativework: f64,
    ninlines: c_int,
    inlines: *const RawInline,
    split: *const f64,
    exclusive: *const f64,
    nphaselines: c_int,
    phaselines: *const RawPhaseLines,
    binding: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawPhaseLines {
    phase: c_int,
    counts: *const u64,
    work: *const f64,
    cold: *const f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawCacheSite {
    pc: c_int,
    op: c_int,
    key: c_int,
    phase: c_int,
    node: c_int,
    charged: f64,
    firsttouch: f64,
    presized: f64,
    outcomes: [u64; CACHE_OUTCOMES],
    depths: [u64; CACHE_DEPTHS],
    handlers: [c_int; CACHE_HANDLERS],
    handlercounts: [u64; CACHE_HANDLERS],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawBuiltinSite {
    pc: c_int,
    builtin: c_int,
    phase: c_int,
    node: c_int,
    charged: f64,
    fastequivalent: f64,
    fast: u64,
    declines: [u64; DECLINES],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInline {
    function: c_int,
    firstpc: c_int,
    lastpc: c_int,
    parent: c_int,
    entries: u64,
}

#[repr(C)]
struct RawNode {
    parent: c_int,
    function: c_int,
    callpc: c_int,
    calls: u64,
    insns: u64,
    work: f64,
    split: [f64; SPLITS],
    exclusive: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawColdPayer {
    function: c_int,
    pc: c_int,
    cost: f64,
}

#[repr(C)]
struct RawColdObject {
    kind: c_int,
    function: c_int,
    pc: c_int,
    cost: f64,
    frames: u64,
    sharedframes: u64,
    npayers: c_int,
    payers: *const RawColdPayer,
}

/// lua.h `LUA_PROFILE_SPLITS` and the kinds of work a profile keeps apart in each booking: cold and first-use charges, the collector's,
/// prices from a Studio-era measurement, borrowed (defaulted) prices, and the engine's dispatch of a handler or resumption of a thread.
/// A price is of at most one kind of each axis: what it is (cold, collector, dispatch) and where it came from (Studio-era, defaulted).
pub const SPLITS: usize = 5;
pub const COLD: usize = 0;
pub const COLLECTOR: usize = 1;
pub const STUDIO: usize = 2;
pub const DEFAULTED: usize = 3;
pub const DISPATCH: usize = 4;

/// lua.h `LUA_COLD_*`: what a cold charge's object is
pub const COLD_TABLE: i32 = 0;
pub const COLD_CFUNCTION: i32 = 1;
pub const COLD_HOSTFUNCTION: i32 = 2;

/// lua.h `lua_setprofilerehash`'s hook: (ud, the thread, the table, array slots and hash nodes before, after, the keys it holds
/// with the new one, whether a script's own instruction made it)
pub type RehashHook = unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, *mut c_void, c_int, c_int, c_int, c_int, c_int, c_int);
/// lua.h `lua_setprofilecalls`' hook: (ud, the thread, [`CALL_ENTER`] or [`CALL_LEAVE`], the function's id, the `every` its
/// call was drawn with (entries), the frame's depth)
pub type CallHook = unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, c_int, c_int, c_int, c_int);
/// lua.h `LUA_PROFILECALL_*`
pub const CALL_ENTER: c_int = 0;
pub const CALL_LEAVE: c_int = 1;
type HeapNode = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, u8, u8, usize, *const c_char);
type HeapEdge = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, *mut c_void, *const c_char);
type HeapDead = unsafe extern "C-unwind" fn(*mut c_void, u8, usize);

unsafe extern "C-unwind" {
    fn lua_profilestart(state: *mut lua_State, stepping: c_int, prices: *const f64, nprices: c_int, hostwork: *const f64, hostdrained: *const f64) -> c_int;
    fn lua_profilestop(state: *mut lua_State);
    fn lua_profilefunctions(state: *mut lua_State, each: unsafe extern "C-unwind" fn(*mut c_void, c_int, *const RawFunction), ud: *mut c_void) -> c_int;
    fn lua_profilenodes(state: *mut lua_State, each: unsafe extern "C-unwind" fn(*mut c_void, c_int, *const RawNode), ud: *mut c_void) -> c_int;
    fn lua_profilecfunction(state: *mut lua_State, index: c_int) -> *const c_char;
    fn lua_profilename(state: *mut lua_State, id: c_int) -> *const c_char;
    fn lua_profilebuiltinname(id: c_int) -> *const c_char;
    fn lua_setprofilevm(state: *mut lua_State, vm: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, c_int)>, ud: *mut c_void);
    fn lua_profileop(state: *mut lua_State) -> c_int;
    fn lua_profileengine(state: *mut lua_State) -> f64;
    fn lua_profilephase(state: *mut lua_State, phase: c_int);
    fn lua_profilefree(state: *mut lua_State);
    fn lua_profilestack(state: *mut lua_State, functions: *mut c_int, lines: *mut c_int, max: c_int) -> c_int;
    fn lua_profilepc(state: *mut lua_State, function: *mut c_int) -> c_int;
    fn lua_profileentries(state: *mut lua_State, in_c_function: *mut c_int) -> c_int;
    fn lua_profilechunks(state: *mut lua_State, on: c_int);
    fn lua_allocblock(state: *mut lua_State, kind: *mut c_int) -> *mut c_void;
    fn lua_profileobject(block: *mut c_void, tag: *mut c_int, data: *mut *mut c_void, type_name: *mut *const c_char) -> c_int;
    fn lua_profilemark(block: *mut c_void);
    fn lua_profilesplit(state: *mut lua_State, kinds: *const i8, nkinds: c_int, hostkinds: *const f64);
    fn lua_profilecoldobjects(state: *mut lua_State, each: unsafe extern "C-unwind" fn(*mut c_void, *const RawColdObject), ud: *mut c_void) -> c_int;
    fn lua_profilebooknext(state: *mut lua_State, co: *mut lua_State);
    fn lua_profilecurrent(function: *mut c_int, pc: *mut c_int, node: *mut c_int, calls: *mut u64) -> c_int;
    fn lua_profileresumes(state: *mut lua_State) -> u64;
    fn lua_profileudataoffset() -> usize;
    fn lua_setprofilefreed(state: *mut lua_State, freed: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut c_void)>, ud: *mut c_void);
    fn lua_setprofilerehash(state: *mut lua_State, rehash: Option<RehashHook>, ud: *mut c_void);
    fn lua_setprofilecalls(state: *mut lua_State, hook: Option<CallHook>, ud: *mut c_void);
    fn lua_setprofilecallevery(state: *mut lua_State, function: c_int, every: c_int, remeasure: c_int);
    fn lua_profiletable(ptr: *mut c_void, arrayslots: *mut c_int, arrayused: *mut c_int, nodes: *mut c_int, nodesused: *mut c_int) -> c_int;
    fn lua_profiletablebytes(header: *mut usize, slot: *mut usize, node: *mut usize);
    fn lua_profilecachetotals(state: *mut lua_State, out: *mut u64) -> c_int;
    fn lua_profiletoolchunks(state: *mut lua_State, prefixes: *const *const c_char, n: c_int);
    fn lua_enumheap(state: *mut lua_State, context: *mut c_void, node: HeapNode, edge: HeapEdge, dead: Option<HeapDead>);
    fn lua_gcstate(state: *mut lua_State) -> c_int;
    fn lua_totalbytes(state: *mut lua_State, category: c_int) -> usize;
    fn lua_mainthread(state: *mut lua_State) -> *mut lua_State;
}

/// The price of one count of each 8-byte slot of the VM's work counters, in seconds: [`crate::cost::luau`] of a counter
/// set holding 1 in that slot. With `linear`, the model is checked to be a weighted sum of its counts (what booking it
/// count by count needs); a model that is not gives None, and the work profile is not taken.
pub fn prices() -> Option<Vec<f64>> {
    let slots = std::mem::size_of::<VmCounters>() / 8;
    let unit = |slot: usize, value: u64| -> f64 {
        let mut counters: VmCounters = unsafe { std::mem::zeroed() };
        unsafe { (&mut counters as *mut VmCounters as *mut u64).add(slot).write(value) };
        crate::cost::luau(&counters)
    };
    let prices: Vec<f64> = (0..slots).map(|slot| unit(slot, 1)).collect();
    // (a weighted sum: the price of a mix is the sum of its parts' prices)
    let mut mixed: VmCounters = unsafe { std::mem::zeroed() };
    let mut expected = 0.0;
    for slot in 0..slots {
        let count = (slot as u64 * 7919 + 13) % 1000;
        unsafe { (&mut mixed as *mut VmCounters as *mut u64).add(slot).write(count) };
        expected += prices[slot] * count as f64;
    }
    let total = crate::cost::luau(&mixed);
    ((total - expected).abs() <= 1e-9 * total.abs().max(1e-12)).then_some(prices)
}

/// Starts the VM's profile on this thread: stepping (instructions, work, tree, caches) or only naming functions for
/// stacks. `host` is the engine's work cells (Scheduler::work and work_drained), whose charges are booked too.
///
/// # Safety
/// `state` is a live thread of the VM to profile, and `host`'s cells, when given, stay readable until [`free`].
pub unsafe fn start(state: *mut lua_State, stepping: bool, prices: Option<&[f64]>, host: Option<(*const f64, *const f64)>) -> bool {
    let (prices_ptr, count) = prices.map_or((std::ptr::null(), 0), |prices| (prices.as_ptr(), prices.len() as c_int));
    let (work, drained) = host.unwrap_or((std::ptr::null(), std::ptr::null()));
    unsafe { lua_profilestart(state, stepping as c_int, prices_ptr, count, work, drained) != 0 }
}

/// Keeps kinds of work apart in the profile's bookings ([`SPLITS`]): each 8-byte slot's kinds as a mask (bit k for kind k), and the
/// host's running totals of its own charges by kind (`hostkinds`, [`SPLITS`] cells it keeps alive while the profile runs).
///
/// # Safety
/// `state` is the VM a profile runs on, and `hostkinds` points to [`SPLITS`] cells that live until the profile stops.
pub unsafe fn split(state: *mut lua_State, kinds: &[i8], hostkinds: *const f64) {
    unsafe { lua_profilesplit(state, kinds.as_ptr(), kinds.len() as c_int, hostkinds) }
}

/// What the host charges next is booked to `co`'s current frame (a thread about to resume) or, when it has not started, to the Luau
/// function on top of its stack (a handler about to run as a thread of its own)
///
/// # Safety
/// `state` and `co` are live threads of the VM the profile runs on.
pub unsafe fn book_next(state: *mut lua_State, co: *mut lua_State) {
    unsafe { lua_profilebooknext(state, co) }
}

/// The instruction the running booking goes to
#[derive(Clone, Copy, Debug)]
pub struct Booking {
    pub function: i32,
    pub pc: i32,
    /// Its node in the call tree
    pub node: i32,
    /// The calls its tree node had begun so far: two readings with the same node and calls are in one invocation
    pub calls: u64,
}

/// The instruction the running booking goes to; None when no profile steps on this thread or no instruction is the target (the
/// engine's own work)
pub fn current() -> Option<Booking> {
    let (mut function, mut pc, mut node, mut calls): (c_int, c_int, c_int, u64) = (-1, -1, -1, 0);
    let stepping = unsafe { lua_profilecurrent(&mut function, &mut pc, &mut node, &mut calls) } != 0;
    (stepping && function >= 0).then_some(Booking { function, pc, node, calls })
}

/// How many times `state` was resumed while the profile stepped: two of its readings with a resumption between straddle a yield
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn resumes(state: *mut lua_State) -> u64 {
    unsafe { lua_profileresumes(state) }
}

/// Stops stepping (the last instruction's charges are booked); the results stay until [`free`].
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn stop(state: *mut lua_State) {
    unsafe { lua_profilestop(state) }
}

/// The phase what follows belongs to: its frames hang from a node of the phase's (function -1, callpc the phase; phase 0
/// is the root itself)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn phase(state: *mut lua_State, phase: u32) {
    unsafe { lua_profilephase(state, phase as c_int) }
}

/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn free(state: *mut lua_State) {
    unsafe { lua_profilefree(state) }
}

/// One function the profile saw: its instructions and what each did.
#[derive(Clone, Debug, Default)]
pub struct Function {
    pub source: String,
    pub name: String,
    pub line: i32,
    pub flags: i32,
    pub code: Vec<u32>,
    pub lines: Vec<i32>,
    /// Dispatches per instruction (empty without stepping)
    pub counts: Vec<u64>,
    /// Work clock seconds booked per instruction
    pub work: Vec<f64>,
    /// Its instructions with an inline cache that ran, and what each did
    pub caches: Vec<CacheSite>,
    /// Its builtin calls (FASTCALL) that ran: served by the fast path, or declined and why
    pub builtins: Vec<BuiltinSite>,
    pub native_entries: u64,
    pub native_work: f64,
    /// Copies of other functions Luau's compiler inlined into this one (`-O2`), outermost before the ones inside them
    pub inlines: Vec<Inline>,
    /// Of each instruction's work, what was of each kind ([`SPLITS`] an instruction; empty when nothing was split), and of its cold
    /// charges those for objects nothing but this function touched in the frame (empty when none)
    pub split: Vec<f64>,
    pub exclusive: Vec<f64>,
    /// Its own instructions' dispatches, work and cold work in each phase that ran it (lines per phase)
    pub phase_lines: Vec<PhaseLines>,
    /// A helper chunk of the binding (mlua's "=__mlua..."): its work is booked to the script line that called it
    pub binding: bool,
}

/// A function's instructions in one phase: dispatches, work and cold work per instruction
#[derive(Clone, Debug, Default)]
pub struct PhaseLines {
    pub phase: u32,
    pub counts: Vec<u64>,
    pub work: Vec<f64>,
    pub cold: Vec<f64>,
}

/// An instruction with an inline cache in one phase: what it did each time it ran there ([`CACHE_OUTCOMES`]), the metatable walks
/// by depth, and the types whose metamethods served its `not_table` outcomes (an instruction has a site for each phase it ran in)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CacheSite {
    pub pc: usize,
    /// The phase it ran in (the profile's phases, by index)
    pub phase: u32,
    /// The call-tree node it ran in (Results::nodes' index): an instruction has a record for each node and phase it ran in
    pub node: i32,
    /// What the work clock charged the instruction past its dispatch there, in seconds (a rehash's keys and storage, a
    /// looked-up import, an Instance member's engine price): part of its function's work
    pub charged: f64,
    /// Of `charged`, the first touch in a frame of the table the instruction reaches (its operand's, the environment's for a
    /// global), which any change to it still pays, in seconds
    pub first_touch: f64,
    /// Its inserts' rehashes: what allocating the tables' storage at the size each rehash made, up front, would have charged
    /// beyond the storage they had (a presized constructor's cost in their place), in seconds
    pub presized: f64,
    /// The opcode as it ran (a userdata form the interpreter rewrote to the table access it fell back to is that access)
    pub op: u8,
    /// The key it uses: a table key, a method, a global, an import's path (`math.floor`); empty for none
    pub key: String,
    pub outcomes: [u64; CACHE_OUTCOMES],
    pub depths: [u64; CACHE_DEPTHS],
    /// (type name, count); "other types" for the rest
    pub handlers: Vec<(String, u64)>,
}

impl CacheSite {
    pub fn runs(&self) -> u64 {
        self.outcomes.iter().sum()
    }
}

/// A builtin call in one phase: the calls its fast path served there, and the declined ones by reason ([`DECLINES`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuiltinSite {
    pub pc: usize,
    /// The phase it ran in (the profile's phases, by index)
    pub phase: u32,
    /// The call-tree node it ran in (Results::nodes' index): an instruction has a record for each node and phase it ran in
    pub node: i32,
    /// What the work clock charged its declined calls there, in seconds: their fast call's dispatch, their fallback's instructions
    /// and the function those reached (a C function's work and its cold first call, the engine's own Luau's), part of its
    /// function's work
    pub charged: f64,
    /// What it would charge the same calls on the fast path, in seconds: the fast call's dispatch and the work the builtin counts
    /// there (a new string, a number made a string, the objects and table storage it allocates), which both paths pay; so
    /// `charged - fast_equivalent` is what the fast path would save
    pub fast_equivalent: f64,
    /// The builtin (an LBF id; [`builtin_name`])
    pub builtin: i32,
    pub fast: u64,
    pub declines: [u64; DECLINES],
}

impl BuiltinSite {
    pub fn calls(&self) -> u64 {
        self.fast + self.declines.iter().sum::<u64>()
    }
}

/// A builtin's name by its id (`math.floor`), as the VM lists them
pub fn builtin_name(id: i32) -> Option<&'static str> {
    let name = unsafe { lua_profilebuiltinname(id) };
    (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("?"))
}

/// A copy of a function inlined into another: its instructions there and the calls it stood for
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    /// The inlined function (an index into [`Results::functions`])
    pub function: i32,
    pub first_pc: i32,
    pub last_pc: i32,
    /// The copy this one sits in (an index into the same `inlines`), when the copied function had it inlined too
    pub parent: i32,
    /// Times its frame stepped into it from outside it
    pub entries: u64,
}

impl Function {
    pub fn native(&self) -> bool {
        self.flags & NATIVE != 0
    }

    /// The engine's own Luau (prelude, services, datatypes): not charged as a script's work
    pub fn exempt(&self) -> bool {
        self.flags & EXEMPT != 0
    }
}

/// A node of the calling-context tree: a function reached by one path of calls.
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub parent: i32,
    /// >= 0 a [`Function`]; -1 the root; < -1 the C function `-2 - n` ([`Results::cfunctions`])
    pub function: i32,
    /// The parent's instruction that made the call (-1: a C or native caller)
    pub callpc: i32,
    pub calls: u64,
    pub insns: u64,
    pub work: f64,
    /// Of its work, what was of each kind ([`SPLITS`]), and of its cold charges those for objects nothing but its function touched
    pub split: [f64; SPLITS],
    pub exclusive: f64,
}

/// A cold or first-use charge's object over the profile (lua_profilecoldobjects): a table by the instruction that made it (`function`
/// -1: made before the profile), a C function (`function` a [`Results::cfunctions`] index) or a function of the engine's Luau
#[derive(Clone, Debug, Default)]
pub struct ColdObject {
    pub kind: i32,
    pub function: i32,
    pub pc: i32,
    pub cost: f64,
    pub frames: u64,
    /// Frames in which something besides the function that paid also touched it
    pub shared_frames: u64,
    /// Who paid it, dearest first
    pub payers: Vec<Payer>,
}

/// An instruction that paid cold charges for an object
#[derive(Clone, Copy, Debug)]
pub struct Payer {
    pub function: i32,
    pub pc: i32,
    /// Seconds of the work clock
    pub cost: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub functions: Vec<Function>,
    pub cfunctions: Vec<String>,
    pub nodes: Vec<Node>,
    /// Work charged while no instruction ran (the engine delivering events, resuming threads)
    pub engine_work: f64,
    pub cold_objects: Vec<ColdObject>,
}

unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() { String::new() } else { unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned() }
}

unsafe fn slice<T: Copy>(pointer: *const T, len: usize) -> Vec<T> {
    if pointer.is_null() || len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(pointer, len) }.to_vec() }
}

struct Collected {
    functions: Vec<Function>,
    state: *mut lua_State,
}

unsafe extern "C-unwind" fn each_function(ud: *mut c_void, _id: c_int, raw: *const RawFunction) {
    let collected = unsafe { &mut *(ud as *mut Collected) };
    let state = collected.state;
    let raw = unsafe { &*raw };
    let len = raw.sizecode.max(0) as usize;
    let name = |id: c_int| -> String {
        match id {
            -2 => "other types".to_string(),
            id if id >= 0 => unsafe { text(lua_profilename(state, id)) },
            _ => String::new(),
        }
    };
    let caches = unsafe { slice(raw.caches, raw.ncaches.max(0) as usize) }
        .into_iter()
        .map(|site| CacheSite {
            pc: site.pc.max(0) as usize,
            phase: site.phase.max(0) as u32,
            node: site.node,
            charged: site.charged,
            first_touch: site.firsttouch,
            presized: site.presized,
            op: site.op as u8,
            key: name(site.key),
            outcomes: site.outcomes,
            depths: site.depths,
            handlers: site.handlers.iter().zip(site.handlercounts.iter()).filter(|(id, count)| **id != -1 && **count > 0).map(|(id, count)| (name(*id), *count)).collect(),
        })
        .collect();
    let builtins = unsafe { slice(raw.builtins, raw.nbuiltins.max(0) as usize) }.into_iter().map(|site| BuiltinSite { pc: site.pc.max(0) as usize, phase: site.phase.max(0) as u32, node: site.node, charged: site.charged, fast_equivalent: site.fastequivalent, builtin: site.builtin, fast: site.fast, declines: site.declines }).collect();
    collected.functions.push(unsafe {
        Function {
            source: text(raw.source),
            name: text(raw.name),
            line: raw.linedefined,
            flags: raw.flags,
            code: slice(raw.code, len),
            lines: slice(raw.lines, len),
            counts: slice(raw.counts, len),
            work: slice(raw.work, len),
            caches,
            builtins,
            native_entries: raw.nativeentries,
            native_work: raw.nativework,
            inlines: slice(raw.inlines, raw.ninlines.max(0) as usize)
                .into_iter()
                .map(|raw| Inline { function: raw.function, first_pc: raw.firstpc, last_pc: raw.lastpc, parent: raw.parent, entries: raw.entries })
                .collect(),
            split: slice(raw.split, len * SPLITS),
            exclusive: slice(raw.exclusive, len),
            phase_lines: slice(raw.phaselines, raw.nphaselines.max(0) as usize)
                .into_iter()
                .map(|lines| PhaseLines { phase: lines.phase.max(0) as u32, counts: slice(lines.counts, len), work: slice(lines.work, len), cold: slice(lines.cold, len) })
                .collect(),
            binding: raw.binding != 0,
        }
    });
}

unsafe extern "C-unwind" fn each_cold(ud: *mut c_void, raw: *const RawColdObject) {
    let out = unsafe { &mut *(ud as *mut Vec<ColdObject>) };
    let raw = unsafe { &*raw };
    let mut payers: Vec<Payer> = Vec::new();
    for payer in unsafe { slice(raw.payers, raw.npayers.max(0) as usize) } {
        payers.push(Payer { function: payer.function, pc: payer.pc, cost: payer.cost });
    }
    out.push(ColdObject { kind: raw.kind, function: raw.function, pc: raw.pc, cost: raw.cost, frames: raw.frames, shared_frames: raw.sharedframes, payers });
}

unsafe extern "C-unwind" fn each_node(ud: *mut c_void, _id: c_int, raw: *const RawNode) {
    let out = unsafe { &mut *(ud as *mut Vec<Node>) };
    let raw = unsafe { &*raw };
    out.push(Node { parent: raw.parent, function: raw.function, callpc: raw.callpc, calls: raw.calls, insns: raw.insns, work: raw.work, split: raw.split, exclusive: raw.exclusive });
}

/// What the profile of `state`'s VM holds (call after [`stop`]).
///
/// # Safety
/// `state` is the VM the profile ran on, stopped and not yet freed ([`free`]).
pub unsafe fn results(state: *mut lua_State) -> Results {
    let mut collected = Collected { functions: Vec::new(), state };
    let mut nodes: Vec<Node> = Vec::new();
    let mut cold_objects: Vec<ColdObject> = Vec::new();
    unsafe {
        lua_profilefunctions(state, each_function, &mut collected as *mut _ as *mut c_void);
        lua_profilenodes(state, each_node, &mut nodes as *mut _ as *mut c_void);
        lua_profilecoldobjects(state, each_cold, &mut cold_objects as *mut _ as *mut c_void);
    }
    let functions = collected.functions;
    cold_objects.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.kind.cmp(&b.kind)).then(a.function.cmp(&b.function)).then(a.pc.cmp(&b.pc)));
    let mut cfunctions = Vec::new();
    loop {
        let name = unsafe { lua_profilecfunction(state, cfunctions.len() as c_int) };
        if name.is_null() {
            break;
        }
        cfunctions.push(unsafe { text(name) });
    }
    Results { functions, cfunctions, nodes, engine_work: unsafe { lua_profileengine(state) }, cold_objects }
}

/// The call tree as it stands, while the profile runs: `each(node, parent, function, work in seconds)` for every node, a
/// parent before its children (ids as [`Node`]'s; a node, once made, keeps its parent and function); the count. Reads
/// pointers only: nothing is copied or booked.
// (the state is the one the profile was started on, as every call here takes it)
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn walk_nodes(state: *mut lua_State, mut each: impl FnMut(usize, i32, i32, f64)) -> usize {
    unsafe extern "C-unwind" fn visit(ud: *mut c_void, id: c_int, raw: *const RawNode) {
        let each = unsafe { &mut *(ud as *mut &mut dyn FnMut(usize, i32, i32, f64)) };
        let raw = unsafe { &*raw };
        each(id.max(0) as usize, raw.parent, raw.function, raw.work);
    }
    let mut each: &mut dyn FnMut(usize, i32, i32, f64) = &mut each;
    unsafe { lua_profilenodes(state, visit, &mut each as *mut &mut dyn FnMut(usize, i32, i32, f64) as *mut c_void).max(0) as usize }
}

/// The functions the profile registered from id `from` on, in id order, while it runs: `each(id, source, flags)` (a
/// function's id never changes, so a reader keeps what it read of the ones before)
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn walk_functions(state: *mut lua_State, from: usize, mut each: impl FnMut(usize, &str, i32)) -> usize {
    struct Visit<'a> {
        from: usize,
        each: &'a mut dyn FnMut(usize, &str, i32),
    }
    unsafe extern "C-unwind" fn visit(ud: *mut c_void, id: c_int, raw: *const RawFunction) {
        let visit = unsafe { &mut *(ud as *mut Visit) };
        if (id as usize) < visit.from {
            return;
        }
        let raw = unsafe { &*raw };
        let source = if raw.source.is_null() { std::borrow::Cow::Borrowed("") } else { unsafe { CStr::from_ptr(raw.source) }.to_string_lossy() };
        (visit.each)(id as usize, &source, raw.flags);
    }
    let mut context = Visit { from, each: &mut each };
    unsafe { lua_profilefunctions(state, visit, &mut context as *mut Visit as *mut c_void).max(0) as usize }
}

/// What the work clock charged one inline-cache or builtin-call site in one calling context, cumulative over the run.
/// Reading them does not copy instruction histories.
pub struct SiteCharges {
    /// `cache` or `builtin`
    pub kind: &'static str,
    pub function: i32,
    pub pc: usize,
    /// The call-tree node of the context
    pub node: usize,
    /// All the work the clock charged at the site (seconds)
    pub charged: f64,
    /// The part of it a fix keeps paying (seconds): a cache's first touch and presized build, a builtin's fast-path work
    pub retained: f64,
    /// The builtin called, for a builtin site
    pub builtin: Option<&'static str>,
    /// The call instruction's opcode (for a builtin site; 0 where it cannot be read)
    pub op: u8,
    /// The calls the builtin's fast path declined in this context
    pub declines: u64,
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn walk_site_charges(state: *mut lua_State, mut each: impl FnMut(SiteCharges)) {
    unsafe extern "C-unwind" fn visit(ud: *mut c_void, function: c_int, raw: *const RawFunction) {
        let each = unsafe { &mut *(ud as *mut &mut dyn FnMut(SiteCharges)) };
        let raw = unsafe { &*raw };
        if !raw.caches.is_null() {
            let cache_sites = unsafe { std::slice::from_raw_parts(raw.caches, raw.ncaches.max(0) as usize) };
            for site in cache_sites {
                if site.node < 0 || site.charged == 0.0 {
                    continue;
                }
                each(SiteCharges {
                    kind: "cache",
                    function,
                    pc: site.pc.max(0) as usize,
                    node: site.node as usize,
                    charged: site.charged,
                    retained: site.firsttouch + site.presized,
                    builtin: None,
                    op: site.op as u8,
                    declines: 0,
                });
            }
        }
        if !raw.builtins.is_null() {
            let builtin_sites = unsafe { std::slice::from_raw_parts(raw.builtins, raw.nbuiltins.max(0) as usize) };
            for site in builtin_sites {
                let declines = site.declines.iter().sum();
                if site.node < 0 || declines == 0 {
                    continue;
                }
                let pc = site.pc.max(0) as usize;
                let has_code = pc < raw.sizecode.max(0) as usize && !raw.code.is_null();
                let op = if has_code { unsafe { *raw.code.add(pc) as u8 } } else { 0 };
                each(SiteCharges {
                    kind: "builtin",
                    function,
                    pc,
                    node: site.node as usize,
                    charged: site.charged,
                    retained: site.fastequivalent,
                    builtin: builtin_name(site.builtin),
                    op,
                    declines,
                });
            }
        }
    }
    let mut each: &mut dyn FnMut(SiteCharges) = &mut each;
    unsafe { lua_profilefunctions(state, visit, &mut each as *mut &mut dyn FnMut(SiteCharges) as *mut c_void) };
}

/// The frames of `state`, innermost first, as (function id, line) pairs (ids as [`Node::function`]; line -1 for C and
/// native frames), written into `out`; the count.
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn stack(state: *mut lua_State, functions: &mut [c_int], lines: &mut [c_int]) -> usize {
    let max = functions.len().min(lines.len()) as c_int;
    unsafe { lua_profilestack(state, functions.as_mut_ptr(), lines.as_mut_ptr(), max).max(0) as usize }
}

/// The instruction `state`'s innermost Luau frame is at (in an interrupt: the one about to run), with its function's id; None
/// when there is no Luau frame or it runs native code
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn pc(state: *mut lua_State) -> Option<(i32, i32)> {
    let mut function: c_int = -1;
    let pc = unsafe { lua_profilepc(state, &mut function) };
    (pc >= 0 && function >= 0).then_some((function, pc))
}

/// The interpreter's entries open (C entered it for each: a script's call of a C function that calls back into Luau is one more),
/// and whether `state`'s innermost frame is a C function's (an interrupt from inside one, as the string library's matcher sends)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn entries(state: *mut lua_State) -> (usize, bool) {
    let mut in_c: c_int = 0;
    let entries = unsafe { lua_profileentries(state, &mut in_c) };
    (entries.max(0) as usize, in_c != 0)
}

/// Whether each chunk's functions and the copies its compiler inlined are registered as it loads: a profile that names samples
/// without stepping needs them (host time's samples in inlined copies); stepping registers a chunk when it first runs one
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn chunks(state: *mut lua_State, on: bool) {
    unsafe { lua_profilechunks(state, on as c_int) }
}

/// During an allocation callback: the collectable object being allocated (null for other memory) and what the memory is
/// for (`ALLOC_*`)
///
/// # Safety
/// `state` is the thread an allocation callback was called with, during that callback.
pub unsafe fn alloc_block(state: *mut lua_State) -> (*mut c_void, i32) {
    let mut kind: c_int = 0;
    let block = unsafe { lua_allocblock(state, &mut kind) };
    (block, kind)
}

/// An object: its Luau type tag (lua.h `LUA_T*`), and for a userdata its tag, its data and its metatable's `__type`
pub struct Object {
    pub tt: i32,
    pub tag: i32,
    pub data: *mut c_void,
    pub type_name: Option<String>,
}

/// # Safety
/// `block` is a live collectable object (an allocation callback's block once its header is set).
pub unsafe fn object(block: *mut c_void) -> Object {
    let (mut tag, mut data, mut type_name): (c_int, *mut c_void, *const c_char) = (-1, std::ptr::null_mut(), std::ptr::null());
    let tt = unsafe { lua_profileobject(block, &mut tag, &mut data, &mut type_name) };
    Object { tt, tag, data, type_name: (!type_name.is_null()).then(|| unsafe { text(type_name) }) }
}

/// Where a userdata's data starts in its block (a heap walk names a userdata by its data)
pub fn udata_data_offset() -> usize {
    unsafe { lua_profileudataoffset() }
}

/// # Safety
/// `block` is a live collectable object.
pub unsafe fn mark(block: *mut c_void) {
    unsafe { lua_profilemark(block) }
}

/// # Safety
/// `state` is a live thread of the VM the profile runs on, and `ud` is what `freed` takes, valid while the hook is set.
pub unsafe fn set_freed(state: *mut lua_State, freed: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut c_void)>, ud: *mut c_void) {
    unsafe { lua_setprofilefreed(state, freed, ud) }
}

/// The hook told of each table that rehashed to fit a new key (the profile's own thread's VM), or none
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on, and `ud` is what `rehash` takes, valid while the hook is set.
pub unsafe fn set_rehash(state: *mut lua_State, rehash: Option<RehashHook>, ud: *mut c_void) {
    unsafe { lua_setprofilerehash(state, rehash, ud) }
}

/// A thread's frames on its call stack (lua.h `lua_stackdepth`: 0 before its first run and once it returned)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn depth(state: *mut lua_State) -> i32 {
    unsafe { mlua::ffi::lua_stackdepth(state) }
}

/// A thread's status (lua.h `lua_status`: 0 running or done, 1 yielded, 2 to 5 an error ended it, 6 a break)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn status(state: *mut lua_State) -> i32 {
    unsafe { mlua::ffi::lua_status(state) }
}

/// The hook called around the Luau calls the VM samples (the running profile's VM), or none (lua.h `lua_setprofilecalls`)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
/// The hook and `ud` stay valid until the hook is cleared or the profile ends.
pub unsafe fn set_calls(state: *mut lua_State, hook: Option<CallHook>, ud: *mut c_void) {
    unsafe { lua_setprofilecalls(state, hook, ud) }
}

/// Each call of function `function` is sampled with a chance of one in `every`; with `every` 0 none, and one call after
/// every `remeasure` calls is read around for its length alone (0: none)
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn set_call_every(state: *mut lua_State, function: i32, every: u32, remeasure: u32) {
    unsafe { lua_setprofilecallevery(state, function, every.min(i32::MAX as u32) as c_int, remeasure.min(i32::MAX as u32) as c_int) }
}

/// A table's shape: its array part's slots and those holding a value, its hash part's nodes (0: none) and those holding one
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TableShape {
    pub array: u32,
    pub array_used: u32,
    pub nodes: u32,
    pub nodes_used: u32,
}

/// The shape of the table at `ptr` (a heap walk's pointer for a table), None when it is not a table
///
/// # Safety
/// `ptr` is a live collectable object: a heap walk's node, read during the walk or before the VM runs again.
pub unsafe fn table_shape(ptr: *mut c_void) -> Option<TableShape> {
    let (mut array, mut array_used, mut nodes, mut nodes_used): (c_int, c_int, c_int, c_int) = (0, 0, 0, 0);
    let table = unsafe { lua_profiletable(ptr, &mut array, &mut array_used, &mut nodes, &mut nodes_used) } != 0;
    table.then(|| TableShape { array: array.max(0) as u32, array_used: array_used.max(0) as u32, nodes: nodes.max(0) as u32, nodes_used: nodes_used.max(0) as u32 })
}

/// The bytes of a table itself, of an array slot and of a hash node (a table's size is the first plus its parts)
pub fn table_bytes() -> (u64, u64, u64) {
    let (mut header, mut slot, mut node) = (0usize, 0usize, 0usize);
    unsafe { lua_profiletablebytes(&mut header, &mut slot, &mut node) };
    (header as u64, slot as u64, node as u64)
}

/// The inline caches' kinds, in [`cache_totals`]' order (the names [`cache_kind`] gives)
pub const CACHE_KINDS: [&str; 4] = ["global slot", "import", "method slot", "table key slot"];

/// What the instructions with an inline cache and the builtin calls did so far, summed over the profile's functions
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheTotals {
    /// By kind ([`CACHE_KINDS`]): [`CACHE_OUTCOMES`] each
    pub kinds: [[u64; CACHE_OUTCOMES]; 4],
    /// The builtin calls the fast path served, and the declined ones by reason ([`DECLINES`])
    pub fast: u64,
    pub declines: [u64; DECLINES],
    /// By kind: the metatable walks by the __index tables they consulted ([`CACHE_DEPTHS`], the last "or more")
    pub depths: [[u64; CACHE_DEPTHS]; 4],
}

/// What the instructions with an inline cache and the builtin calls did so far, summed over the profile's functions (the
/// engine's own Luau and ree's tools' chunks aside: [`tool_chunks`])
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn cache_totals(state: *mut lua_State) -> CacheTotals {
    const DEPTHS_AT: usize = 4 * CACHE_OUTCOMES + 1 + DECLINES;
    const TOTAL: usize = DEPTHS_AT + 4 * CACHE_DEPTHS;
    let mut out = [0u64; TOTAL];
    unsafe { lua_profilecachetotals(state, out.as_mut_ptr()) };
    CacheTotals {
        kinds: std::array::from_fn(|kind| std::array::from_fn(|outcome| out[kind * CACHE_OUTCOMES + outcome])),
        fast: out[4 * CACHE_OUTCOMES],
        declines: std::array::from_fn(|reason| out[4 * CACHE_OUTCOMES + 1 + reason]),
        depths: std::array::from_fn(|kind| std::array::from_fn(|depth| out[DEPTHS_AT + kind * CACHE_DEPTHS + depth])),
    }
}

/// The chunk names ree's tools' code begins with (`=rur.record`; the what-if runner's harness): [`cache_totals`] leaves their
/// caches and builtin calls out
///
/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn tool_chunks(state: *mut lua_State, prefixes: &[&str]) {
    let owned: Vec<std::ffi::CString> = prefixes.iter().filter_map(|prefix| std::ffi::CString::new(*prefix).ok()).collect();
    let pointers: Vec<*const c_char> = owned.iter().map(|prefix| prefix.as_ptr()).collect();
    unsafe { lua_profiletoolchunks(state, pointers.as_ptr(), pointers.len() as c_int) }
}

/// The hook told when the interpreter is entered from outside it (true) and left back to it (false), while the profile runs
pub type VmHook = unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, c_int);

/// # Safety
/// `state` is a live thread of the VM the profile runs on.
pub unsafe fn set_vm_hook(state: *mut lua_State, hook: Option<VmHook>, ud: *mut c_void) {
    unsafe { lua_setprofilevm(state, hook, ud) }
}

/// The opcode of the instruction `state`'s innermost frame is at (in an interrupt: the one about to run), None outside a Luau
/// frame
///
/// # Safety
/// `state` is a live thread of a VM (an interrupt's).
pub unsafe fn current_op(state: *mut lua_State) -> Option<u8> {
    let op = unsafe { lua_profileop(state) };
    (op >= 0).then_some(op as u8)
}

/// Walks the heap as the collector sees it: `node(ptr, tt, size, name)` for every collectable object (a userdata's ptr
/// is its data), `edge(from, to, name)` for every reference the collector follows, the collector's roots as edges from
/// 0 (the main thread, the registry, the basic types' metatables, the fixed objects), and `dead(tt, size)` for each
/// object a sweep found dead and has not freed yet (lua.h lua_enumheap).
///
/// # Safety
/// `state` is a live thread of a VM, and no other thread runs that VM during the walk.
pub unsafe fn enum_heap(state: *mut lua_State, mut node: impl FnMut(usize, u8, usize, Option<&str>), mut edge: impl FnMut(usize, usize, Option<&str>), mut dead: impl FnMut(u8, usize)) {
    struct Context<'a> {
        node: &'a mut dyn FnMut(usize, u8, usize, Option<&str>),
        edge: &'a mut dyn FnMut(usize, usize, Option<&str>),
        dead: &'a mut dyn FnMut(u8, usize),
    }
    unsafe extern "C-unwind" fn on_node(context: *mut c_void, ptr: *mut c_void, tt: u8, _memcat: u8, size: usize, name: *const c_char) {
        let context = unsafe { &mut *(context as *mut Context) };
        let name = (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }.to_string_lossy());
        (context.node)(ptr as usize, tt, size, name.as_deref());
    }
    unsafe extern "C-unwind" fn on_edge(context: *mut c_void, from: *mut c_void, to: *mut c_void, name: *const c_char) {
        let context = unsafe { &mut *(context as *mut Context) };
        let name = (!name.is_null()).then(|| unsafe { CStr::from_ptr(name) }.to_string_lossy());
        (context.edge)(from as usize, to as usize, name.as_deref());
    }
    unsafe extern "C-unwind" fn on_dead(context: *mut c_void, tt: u8, size: usize) {
        let context = unsafe { &mut *(context as *mut Context) };
        (context.dead)(tt, size);
    }
    let mut context = Context { node: &mut node, edge: &mut edge, dead: &mut dead };
    unsafe { lua_enumheap(state, &mut context as *mut Context as *mut c_void, on_node, on_edge, Some(on_dead)) };
}

/// The collector's state: 0 pause, 1 propagate, 2 propagate again, 3 atomic, 4 sweep (lgc.h)
///
/// # Safety
/// `state` is a live thread of a VM.
pub unsafe fn gc_state(state: *mut lua_State) -> i32 {
    unsafe { lua_gcstate(state) }
}

/// The VM's heap in bytes (what gcinfo() and collectgarbage("count") report in KB)
///
/// # Safety
/// `state` is a live thread of a VM.
pub unsafe fn total_bytes(state: *mut lua_State) -> usize {
    unsafe { lua_totalbytes(state, -1) }
}

/// # Safety
/// `state` is a live thread of a VM.
pub unsafe fn main_thread(state: *mut lua_State) -> *mut lua_State {
    unsafe { lua_mainthread(state) }
}

/// Luau's opcode names, in Bytecode.h's order (LOP_NOP = 0 ...; profiler_tests.rs checks them against the vendored header)
pub const OPCODES: [&str; 95] = [
    "NOP", "BREAK", "LOADNIL", "LOADB", "LOADN", "LOADK", "MOVE", "GETGLOBAL", "SETGLOBAL", "GETUPVAL", "SETUPVAL", "CLOSEUPVALS", "GETIMPORT",
    "GETTABLE", "SETTABLE", "GETTABLEKS", "SETTABLEKS", "GETTABLEN", "SETTABLEN", "NEWCLOSURE", "NAMECALL", "CALL", "RETURN", "JUMP", "JUMPBACK",
    "JUMPIF", "JUMPIFNOT", "JUMPIFEQ", "JUMPIFLE", "JUMPIFLT", "JUMPIFNOTEQ", "JUMPIFNOTLE", "JUMPIFNOTLT", "ADD", "SUB", "MUL", "DIV", "MOD",
    "POW", "ADDK", "SUBK", "MULK", "DIVK", "MODK", "POWK", "AND", "OR", "ANDK", "ORK", "CONCAT", "NOT", "MINUS", "LENGTH", "NEWTABLE",
    "DUPTABLE", "SETLIST", "FORNPREP", "FORNLOOP", "FORGLOOP", "FORGPREP_INEXT", "FASTCALL3", "FORGPREP_NEXT", "NATIVECALL", "GETVARARGS",
    "DUPCLOSURE", "PREPVARARGS", "LOADKX", "JUMPX", "FASTCALL", "COVERAGE", "CAPTURE", "SUBRK", "DIVRK", "FASTCALL1", "FASTCALL2",
    "FASTCALL2K", "FORGPREP", "JUMPXEQKNIL", "JUMPXEQKB", "JUMPXEQKN", "JUMPXEQKS", "IDIV", "IDIVK", "GETUDATAKS", "SETUDATAKS",
    "NAMECALLUDATA", "NEWCLASSMEMBER", "CALLFB", "CMPPROTO", "FASTPCALL", "NEWCLASS", "CONSTRUCT", "FINCONSTRUCT", "GETPRIVATEFIELD",
    "SETPRIVATEFIELD",
];

pub fn opcode_name(op: u8) -> &'static str {
    OPCODES.get(op as usize).copied().unwrap_or("?")
}

/// The opcodes with an inline cache the profile reads: global slot, import, table key slot, method slot (the userdata forms the
/// loader makes of the last two are the same caches)
pub fn cache_kind(op: u8) -> Option<&'static str> {
    Some(match OPCODES.get(op as usize).copied()? {
        "GETGLOBAL" | "SETGLOBAL" => "global slot",
        "GETIMPORT" => "import",
        "GETTABLEKS" | "SETTABLEKS" | "GETUDATAKS" | "SETUDATAKS" => "table key slot",
        "NAMECALL" | "NAMECALLUDATA" => "method slot",
        _ => return None,
    })
}

/// An instruction's length in code words: 2 for one with an AUX word (BytecodeUtils.h getOpLength)
pub fn op_length(op: u8) -> usize {
    match opcode_name(op) {
        "GETGLOBAL" | "SETGLOBAL" | "GETIMPORT" | "GETTABLEKS" | "SETTABLEKS" | "NAMECALL" | "JUMPIFEQ" | "JUMPIFLE" | "JUMPIFLT" | "JUMPIFNOTEQ" | "JUMPIFNOTLE"
        | "JUMPIFNOTLT" | "NEWTABLE" | "SETLIST" | "FORGLOOP" | "LOADKX" | "FASTCALL2" | "FASTCALL2K" | "FASTCALL3" | "JUMPXEQKNIL" | "JUMPXEQKB" | "JUMPXEQKN"
        | "JUMPXEQKS" | "GETUDATAKS" | "SETUDATAKS" | "NAMECALLUDATA" | "NEWCLASSMEMBER" | "CALLFB" | "CMPPROTO" | "NEWCLASS" | "CONSTRUCT" | "GETPRIVATEFIELD"
        | "SETPRIVATEFIELD" => 2,
        _ => 1,
    }
}

/// Whether the interpreter checks its interrupt at the instruction (lvmexecute.cpp VM_INTERRUPT): a call, a return, a loop's
/// edge, a long jump. Host time's samples are taken there
pub fn is_safepoint(op: u8) -> bool {
    matches!(opcode_name(op), "CALL" | "CALLFB" | "FASTPCALL" | "RETURN" | "FORNLOOP" | "FORGLOOP" | "JUMPBACK" | "JUMPX")
}

/// Where a jump back to an earlier instruction lands (a loop's edge: FORNLOOP, FORGLOOP, JUMPBACK, a JUMPX back), else None
fn back_target(code: &[u32], pc: usize) -> Option<usize> {
    let insn = code[pc];
    let offset = match opcode_name((insn & 0xff) as u8) {
        "FORNLOOP" | "FORGLOOP" | "JUMPBACK" => (insn as i32) >> 16,
        "JUMPX" => (insn as i32) >> 8,
        _ => return None,
    };
    let target = pc as i64 + 1 + offset as i64;
    (offset < 0 && target >= 0).then_some(target as usize)
}

/// The instructions a sample at the safepoint `pc` stands for, as the code reads: those since the safepoint before it (the
/// interrupt runs at every one), back to the top of the loop it is in when it reaches one, else to the function's start. Both
/// arms of a branch between them are in it. Empty when `pc` is not an instruction. And the call just before it, by pc, when the
/// safepoint before it is one: the interrupt runs before a CALL, so a C function's time (it has no safepoint of its own) lands
/// on the next sample, which then stands for more than the instructions; a Luau function's own safepoints sample it (whether a C
/// function ran since the safepoint before a sample, the VM's count of C calls says: luau_profile::c_called).
pub fn sample_segment(code: &[u32], pc: usize) -> (Vec<usize>, Option<usize>) {
    let mut starts = Vec::new();
    let mut at = 0;
    while at < code.len() {
        starts.push(at);
        at += op_length((code[at] & 0xff) as u8);
    }
    let Ok(index) = starts.binary_search(&pc) else { return (Vec::new(), None) };
    let tops: rustc_hash::FxHashSet<usize> = starts.iter().filter_map(|&start| back_target(code, start)).collect();
    let mut first = index;
    // (the sample's own loop edge goes back to its top: the body is what ran)
    let floor = back_target(code, pc).unwrap_or(0);
    while first > 0 && starts[first] > floor && !tops.contains(&starts[first]) && !is_safepoint((code[starts[first - 1]] & 0xff) as u8) {
        first -= 1;
    }
    let call = (first > 0 && matches!(opcode_name((code[starts[first - 1]] & 0xff) as u8), "CALL" | "CALLFB" | "FASTPCALL")).then(|| starts[first - 1]);
    (starts[first..=index].to_vec(), call)
}

/// The opcode a userdata form falls back to (GETUDATAKS is GETTABLEKS when no direct access serves its value), else the opcode
pub fn base_op(op: u8) -> u8 {
    match OPCODES.get(op as usize).copied() {
        Some("GETUDATAKS") => 15,
        Some("SETUDATAKS") => 16,
        Some("NAMECALLUDATA") => 20,
        _ => op,
    }
}

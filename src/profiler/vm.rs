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
/// Outcomes kept per instruction with an inline cache: hit, miss (the slot was repredicted), slow (a metatable's path, or
/// an unsafe environment), other (not a table: an Instance's or a datatype's own `__index` / `__namecall`)
pub const CACHE_OUTCOMES: usize = 4;

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
    caches: *const u64,
    nativeentries: u64,
    nativework: f64,
    ninlines: c_int,
    inlines: *const RawInline,
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
}

/// lua.h `lua_setprofilerehash`'s hook: (ud, the thread, the table, array slots and hash nodes before, after, the keys it holds
/// with the new one, whether a script's own instruction made it)
pub type RehashHook = unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, *mut c_void, c_int, c_int, c_int, c_int, c_int, c_int);
type HeapNode = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, u8, u8, usize, *const c_char);
type HeapEdge = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, *mut c_void, *const c_char);
type HeapDead = unsafe extern "C-unwind" fn(*mut c_void, u8, usize);

unsafe extern "C-unwind" {
    fn lua_profilestart(state: *mut lua_State, stepping: c_int, prices: *const f64, nprices: c_int, hostwork: *const f64, hostdrained: *const f64) -> c_int;
    fn lua_profilestop(state: *mut lua_State);
    fn lua_profilefunctions(state: *mut lua_State, each: unsafe extern "C-unwind" fn(*mut c_void, c_int, *const RawFunction), ud: *mut c_void) -> c_int;
    fn lua_profilenodes(state: *mut lua_State, each: unsafe extern "C-unwind" fn(*mut c_void, c_int, *const RawNode), ud: *mut c_void) -> c_int;
    fn lua_profilecfunction(state: *mut lua_State, index: c_int) -> *const c_char;
    fn lua_profileengine(state: *mut lua_State) -> f64;
    fn lua_profilephase(state: *mut lua_State, phase: c_int);
    fn lua_profilefree(state: *mut lua_State);
    fn lua_profilestack(state: *mut lua_State, functions: *mut c_int, lines: *mut c_int, max: c_int) -> c_int;
    fn lua_allocblock(state: *mut lua_State, kind: *mut c_int) -> *mut c_void;
    fn lua_profileobject(block: *mut c_void, tag: *mut c_int, data: *mut *mut c_void, type_name: *mut *const c_char) -> c_int;
    fn lua_profilemark(block: *mut c_void);
    fn lua_profileudataoffset() -> usize;
    fn lua_setprofilefreed(state: *mut lua_State, freed: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut c_void)>, ud: *mut c_void);
    fn lua_setprofilerehash(state: *mut lua_State, rehash: Option<RehashHook>, ud: *mut c_void);
    fn lua_profiletable(ptr: *mut c_void, arrayslots: *mut c_int, arrayused: *mut c_int, nodes: *mut c_int, nodesused: *mut c_int) -> c_int;
    fn lua_profiletablebytes(header: *mut usize, slot: *mut usize, node: *mut usize);
    fn lua_profilecachetotals(state: *mut lua_State, out: *mut u64) -> c_int;
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
pub fn start(state: *mut lua_State, stepping: bool, prices: Option<&[f64]>, host: Option<(*const f64, *const f64)>) -> bool {
    let (prices_ptr, count) = prices.map_or((std::ptr::null(), 0), |prices| (prices.as_ptr(), prices.len() as c_int));
    let (work, drained) = host.unwrap_or((std::ptr::null(), std::ptr::null()));
    unsafe { lua_profilestart(state, stepping as c_int, prices_ptr, count, work, drained) != 0 }
}

/// Stops stepping (the last instruction's charges are booked); the results stay until [`free`].
pub fn stop(state: *mut lua_State) {
    unsafe { lua_profilestop(state) }
}

/// The phase what follows belongs to: its frames hang from a node of the phase's (function -1, callpc the phase; phase 0
/// is the root itself)
pub fn phase(state: *mut lua_State, phase: u32) {
    unsafe { lua_profilephase(state, phase as c_int) }
}

pub fn free(state: *mut lua_State) {
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
    /// [`CACHE_OUTCOMES`] per instruction (empty when none of its instructions has a cache)
    pub caches: Vec<u64>,
    pub native_entries: u64,
    pub native_work: f64,
    /// Copies of other functions Luau's compiler inlined into this one (`-O2`), outermost before the ones inside them
    pub inlines: Vec<Inline>,
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
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub functions: Vec<Function>,
    pub cfunctions: Vec<String>,
    pub nodes: Vec<Node>,
    /// Work charged while no instruction ran (the engine delivering events, resuming threads)
    pub engine_work: f64,
}

unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() { String::new() } else { unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned() }
}

unsafe fn slice<T: Copy>(pointer: *const T, len: usize) -> Vec<T> {
    if pointer.is_null() || len == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(pointer, len) }.to_vec() }
}

unsafe extern "C-unwind" fn each_function(ud: *mut c_void, _id: c_int, raw: *const RawFunction) {
    let out = unsafe { &mut *(ud as *mut Vec<Function>) };
    let raw = unsafe { &*raw };
    let len = raw.sizecode.max(0) as usize;
    out.push(unsafe {
        Function {
            source: text(raw.source),
            name: text(raw.name),
            line: raw.linedefined,
            flags: raw.flags,
            code: slice(raw.code, len),
            lines: slice(raw.lines, len),
            counts: slice(raw.counts, len),
            work: slice(raw.work, len),
            caches: slice(raw.caches, len * CACHE_OUTCOMES),
            native_entries: raw.nativeentries,
            native_work: raw.nativework,
            inlines: slice(raw.inlines, raw.ninlines.max(0) as usize)
                .into_iter()
                .map(|raw| Inline { function: raw.function, first_pc: raw.firstpc, last_pc: raw.lastpc, parent: raw.parent, entries: raw.entries })
                .collect(),
        }
    });
}

unsafe extern "C-unwind" fn each_node(ud: *mut c_void, _id: c_int, raw: *const RawNode) {
    let out = unsafe { &mut *(ud as *mut Vec<Node>) };
    let raw = unsafe { &*raw };
    out.push(Node { parent: raw.parent, function: raw.function, callpc: raw.callpc, calls: raw.calls, insns: raw.insns, work: raw.work });
}

/// What the profile of `state`'s VM holds (call after [`stop`]).
pub fn results(state: *mut lua_State) -> Results {
    let mut functions: Vec<Function> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    unsafe {
        lua_profilefunctions(state, each_function, &mut functions as *mut _ as *mut c_void);
        lua_profilenodes(state, each_node, &mut nodes as *mut _ as *mut c_void);
    }
    let mut cfunctions = Vec::new();
    loop {
        let name = unsafe { lua_profilecfunction(state, cfunctions.len() as c_int) };
        if name.is_null() {
            break;
        }
        cfunctions.push(unsafe { text(name) });
    }
    Results { functions, cfunctions, nodes, engine_work: unsafe { lua_profileengine(state) } }
}

/// The frames of `state`, innermost first, as (function id, line) pairs (ids as [`Node::function`]; line -1 for C and
/// native frames), written into `out`; the count.
pub fn stack(state: *mut lua_State, functions: &mut [c_int], lines: &mut [c_int]) -> usize {
    let max = functions.len().min(lines.len()) as c_int;
    unsafe { lua_profilestack(state, functions.as_mut_ptr(), lines.as_mut_ptr(), max).max(0) as usize }
}

/// During an allocation callback: the collectable object being allocated (null for other memory) and what the memory is
/// for (`ALLOC_*`)
pub fn alloc_block(state: *mut lua_State) -> (*mut c_void, i32) {
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

pub fn set_freed(state: *mut lua_State, freed: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut c_void)>, ud: *mut c_void) {
    unsafe { lua_setprofilefreed(state, freed, ud) }
}

/// The hook told of each table that rehashed to fit a new key (the profile's own thread's VM), or none
pub fn set_rehash(state: *mut lua_State, rehash: Option<RehashHook>, ud: *mut c_void) {
    unsafe { lua_setprofilerehash(state, rehash, ud) }
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
pub const CACHE_KINDS: [&str; 5] = ["builtin fast path", "global slot", "import", "method slot", "table key slot"];

/// What the instructions with an inline cache did so far, summed over the profile's functions (the engine's own Luau aside),
/// by kind ([`CACHE_KINDS`]): hit, miss, slow, other each
pub fn cache_totals(state: *mut lua_State) -> [[u64; CACHE_OUTCOMES]; 5] {
    let mut out = [0u64; 5 * CACHE_OUTCOMES];
    unsafe { lua_profilecachetotals(state, out.as_mut_ptr()) };
    std::array::from_fn(|kind| std::array::from_fn(|outcome| out[kind * CACHE_OUTCOMES + outcome]))
}

/// Walks the heap as the collector sees it: `node(ptr, tt, size, name)` for every collectable object (a userdata's ptr
/// is its data), `edge(from, to, name)` for every reference the collector follows, the collector's roots as edges from
/// 0 (the main thread, the registry, the basic types' metatables, the fixed objects), and `dead(tt, size)` for each
/// object a sweep found dead and has not freed yet (lua.h lua_enumheap).
pub fn enum_heap(state: *mut lua_State, mut node: impl FnMut(usize, u8, usize, Option<&str>), mut edge: impl FnMut(usize, usize, Option<&str>), mut dead: impl FnMut(u8, usize)) {
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
pub fn gc_state(state: *mut lua_State) -> i32 {
    unsafe { lua_gcstate(state) }
}

/// The VM's heap in bytes (what gcinfo() and collectgarbage("count") report in KB)
pub fn total_bytes(state: *mut lua_State) -> usize {
    unsafe { lua_totalbytes(state, -1) }
}

pub fn main_thread(state: *mut lua_State) -> *mut lua_State {
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

/// The opcodes with an inline cache the profile reads: global slot, import, table key slot, method slot, fast call
pub fn cache_kind(op: u8) -> Option<&'static str> {
    Some(match OPCODES.get(op as usize).copied()? {
        "GETGLOBAL" | "SETGLOBAL" => "global slot",
        "GETIMPORT" => "import",
        "GETTABLEKS" | "SETTABLEKS" => "table key slot",
        "NAMECALL" => "method slot",
        "FASTCALL" | "FASTCALL1" | "FASTCALL2" | "FASTCALL2K" | "FASTCALL3" => "builtin fast path",
        _ => return None,
    })
}

//! Memory: what the VM allocates, by Luau stack and by type (`--profile alloc`), and what it holds at a moment (`--profile
//! heap`): a snapshot of the reachable heap, what a full collection would keep (by type, Instance userdata by class, the
//! biggest by what they keep alive, their dominated size, with the path that holds them, and the objects the run
//! allocated that are still reachable by the stack that allocated them), with the garbage the collector has not swept
//! yet counted apart; and the collector's work (steps, cycles, bytes freed, host time).
//!
//! The allocation callback sees every allocation. Its type is exact: a collectable object's header is read at the next
//! callback (once the VM set it), Instance userdata name their class, and the VM labels the other memory it allocates (a
//! table's array and hash parts, a thread's stack, the string table, a loaded function's arrays). Stacks are sampled:
//! an allocation of `size` bytes is taken with probability `size / step` and weighted by its inverse (step 1, the
//! default: every allocation); the per-type totals count every allocation. A sampled object is marked in the VM, which
//! tells the profile when it is freed, so the live objects by stack at a snapshot are exact at step 1. Nothing is
//! allocated on the VM's heap.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{c_int, c_void},
    rc::Weak,
    time::Instant,
};

use mlua::ffi::lua_State;
use rustc_hash::FxHashMap;

use super::vm;
use crate::Engine;

/// A frame of a stack: (function id as vm::Node::function, line; -1 for C and native frames)
pub type Frame = (i32, i32);

const MAX_DEPTH: usize = 96;

/// Interned stacks, innermost first
#[derive(Default)]
pub struct Stacks {
    pub stacks: Vec<Box<[Frame]>>,
    ids: FxHashMap<Box<[Frame]>, u32>,
    functions: Vec<c_int>,
    lines: Vec<c_int>,
    frames: Vec<Frame>,
}

impl Stacks {
    /// The id of `state`'s current stack
    pub fn of(&mut self, state: *mut lua_State) -> u32 {
        if self.functions.len() < MAX_DEPTH {
            self.functions.resize(MAX_DEPTH, 0);
            self.lines.resize(MAX_DEPTH, 0);
        }
        let depth = vm::stack(state, &mut self.functions, &mut self.lines);
        self.frames.clear();
        self.frames.extend((0..depth).map(|index| (self.functions[index], self.lines[index])));
        self.intern_current()
    }

    fn intern_current(&mut self) -> u32 {
        if let Some(&id) = self.ids.get(self.frames.as_slice()) {
            return id;
        }
        let stack: Box<[Frame]> = self.frames.clone().into_boxed_slice();
        let id = self.stacks.len() as u32;
        self.stacks.push(stack.clone());
        self.ids.insert(stack, id);
        id
    }

    /// The id of a stack given as frames
    pub fn intern(&mut self, frames: &[Frame]) -> u32 {
        self.frames.clear();
        self.frames.extend_from_slice(frames);
        self.intern_current()
    }
}

/// Interned type names
#[derive(Default)]
pub struct Types {
    pub names: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Types {
    pub fn id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }
}

/// Luau's type tags (lua.h, [`vm::T_STRING`] and on)
pub fn type_name(tt: i32) -> &'static str {
    match tt {
        vm::T_STRING => "string",
        vm::T_TABLE => "table",
        vm::T_FUNCTION => "function",
        vm::T_USERDATA => "userdata",
        vm::T_THREAD => "thread",
        vm::T_BUFFER => "buffer",
        vm::T_CLASS => "class",
        vm::T_OBJECT => "object",
        vm::T_PROTO => "function prototype",
        vm::T_UPVAL => "upvalue",
        _ => "other",
    }
}

/// The group a type name belongs to in summaries (a table and its storage are one)
pub fn group(name: &str) -> &str {
    match name {
        "table storage" => "table",
        "thread stack" => "thread",
        "function prototype" | "prototype arrays" => "function prototypes",
        _ if name.starts_with("userdata") => "userdata",
        _ => name,
    }
}

/// Weighted bytes and counts
#[derive(Clone, Copy, Debug, Default)]
pub struct Amount {
    pub bytes: f64,
    pub count: f64,
}

impl Amount {
    fn add(&mut self, bytes: f64, count: f64) {
        self.bytes += bytes;
        self.count += count;
    }
}

struct Pending {
    block: usize,
    size: u64,
    phase: u32,
    sampled: Option<(u32, f64)>,
}

/// An Instance's userdata whose class could not be read when it was booked (the engine's state was in use): booked as
/// `userdata: Instance` and moved to its class's type once it can be read
struct Deferred {
    index: u32,
    generation: u32,
    block: usize,
    size: u64,
    phase: u32,
    sampled: Option<(u32, f64)>,
}

/// A sampled object still alive: the stack that allocated it, its type and weight, and the phase it was allocated in
#[derive(Clone, Copy, Debug)]
pub struct Live {
    pub stack: u32,
    pub ty: u32,
    pub weight: f64,
    pub phase: u32,
}

/// Tables that rehashed to fit a new key (the VM's rehash hook, lua_setprofilerehash): a table grows (or, emptied, shrinks)
/// only there, moving the keys it holds into its new parts (for a new key that is not an integer, its hash part's: the
/// array part stays)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rehash {
    pub events: u64,
    /// Keys the rehashes moved, the new ones included (the VM's work counters count them as `rehashed` when a script's own
    /// instruction made the rehash)
    pub keys: u64,
    /// Of those, the rehashes a script's own instruction made, and their keys
    pub script_events: u64,
    pub script_keys: u64,
    /// Rehashes that left the table's parts bigger
    pub grew: u64,
    /// The parts' bytes (array slots and hash nodes; the table itself aside) before and after the rehashes
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl Rehash {
    fn add(&mut self, keys: u64, script: bool, before: u64, after: u64) {
        self.events += 1;
        self.keys += keys;
        if script {
            self.script_events += 1;
            self.script_keys += keys;
        }
        self.grew += (after > before) as u64;
        self.bytes_before += before;
        self.bytes_after += after;
    }
}

/// No stack: a table the profile did not follow (made before it began, or not sampled)
pub const NO_STACK: u32 = u32::MAX;

/// What the instance userdata's payload names (lua/raw.rs Payload: slot index, generation)
pub type Resolver = Box<dyn Fn(u32, u32) -> Option<String>>;

pub struct Allocations {
    pub step: u64,
    random: u64,
    track_live: bool,
    pub stacks: Stacks,
    pub types: Types,
    /// Every allocation, by type (exact)
    pub by_type: Vec<Amount>,
    /// Sampled allocations by (phase, stack, type), weighted
    pub by_stack: FxHashMap<(u32, u32, u32), Amount>,
    /// The phase allocations are filed under now (profiler::phase)
    pub phase: u32,
    pub bytes: u64,
    pub objects: u64,
    pending: Option<Pending>,
    deferred: Vec<Deferred>,
    pub live: FxHashMap<usize, Live>,
    resolver: Option<Resolver>,
    instance_tag: i32,
    /// Rehashes by (phase, the stack that made the table rehash, the stack that allocated the table: [`NO_STACK`] when the
    /// profile did not follow it), and their totals
    pub rehash: FxHashMap<(u32, u32, u32), Rehash>,
    pub rehashes: Rehash,
    /// The bytes of an array slot and of a hash node (vm::table_bytes)
    slot_bytes: u64,
    node_bytes: u64,
}

/// The type of the table storage the engine's own bookkeeping grows ([`bookkeeping`]): ree's cache of the Instance userdata scripts
/// hold (lua/raw.rs push_instance). A client keeps a cache like it in its Luau heap too (an Instance a script holds is a 48-byte
/// userdata in both), but its table grows in rare large steps where ree's grows a slot at a time: memory a script's
/// collectgarbage("count") sees, booked here apart from what the script's own code allocates.
pub const ENGINE_CACHE: &str = "table storage: ree's Instance cache";

/// Runs `body` with the allocations it makes booked as the engine's own bookkeeping ([`ENGINE_CACHE`]).
#[inline]
pub fn bookkeeping<R>(body: impl FnOnce() -> R) -> R {
    BOOKKEEPING.with(|flag| flag.set(true));
    let out = body();
    BOOKKEEPING.with(|flag| flag.set(false));
    out
}

thread_local! {
    static BOOKKEEPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ALLOCATIONS: RefCell<Option<Allocations>> = const { RefCell::new(None) };
}

impl Allocations {
    fn new(step: u64, track_live: bool, resolver: Option<Resolver>) -> Allocations {
        Allocations {
            step: step.max(1),
            random: 0x9e37_79b9_7f4a_7c15,
            track_live,
            stacks: Stacks::default(),
            types: Types::default(),
            by_type: Vec::new(),
            by_stack: FxHashMap::default(),
            phase: 0,
            bytes: 0,
            objects: 0,
            pending: None,
            deferred: Vec::new(),
            live: FxHashMap::default(),
            resolver,
            instance_tag: crate::lua::raw::INSTANCE_TAG,
            rehash: FxHashMap::default(),
            rehashes: Rehash::default(),
            slot_bytes: vm::table_bytes().1,
            node_bytes: vm::table_bytes().2,
        }
    }

    /// Taken with probability size / step: the weight (its inverse), None when not taken
    fn sample(&mut self, size: u64) -> Option<f64> {
        if size >= self.step {
            return Some(1.0);
        }
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random % self.step < size).then(|| self.step as f64 / size as f64)
    }

    fn book(&mut self, phase: u32, ty: u32, size: u64, sampled: Option<(u32, f64)>) {
        if self.by_type.len() <= ty as usize {
            self.by_type.resize(ty as usize + 1, Amount::default());
        }
        self.by_type[ty as usize].add(size as f64, 1.0);
        if let Some((stack, weight)) = sampled {
            self.by_stack.entry((phase, stack, ty)).or_default().add(size as f64 * weight, weight);
        }
    }

    /// The object the last callback allocated is initialized now: its type is booked (and, sampled, it is followed)
    fn resolve_pending(&mut self) {
        let Some(pending) = self.pending.take() else { return };
        let object = unsafe { vm::object(pending.block as *mut c_void) };
        let name = object_name(&object, self.instance_tag, self.resolver.as_deref());
        let ty = self.types.id(&name);
        self.book(pending.phase, ty, pending.size, pending.sampled);
        if let (true, Some((stack, weight))) = (self.track_live, pending.sampled) {
            unsafe { vm::mark(pending.block as *mut c_void) };
            self.live.insert(pending.block, Live { stack, ty, weight, phase: pending.phase });
        }
        // (the name alone is not enough: any userdata whose metatable's __type is "Instance" gets it too, and only an
        // Instance's own userdata has a slot to read)
        if name == UNRESOLVED_INSTANCE {
            if let Some((index, generation)) = instance_slot(&object, self.instance_tag) {
                self.deferred.push(Deferred { index, generation, block: pending.block, size: pending.size, phase: pending.phase, sampled: pending.sampled });
            }
        }
    }

    /// The Instances booked before their class could be read, moved to their class's type where it can be read now
    pub fn resolve_deferred(&mut self) {
        if self.deferred.is_empty() {
            return;
        }
        let Some(resolver) = self.resolver.take() else { return };
        let from = self.types.id(UNRESOLVED_INSTANCE);
        let deferred = std::mem::take(&mut self.deferred);
        for entry in deferred {
            let Some(class) = resolver(entry.index, entry.generation) else {
                self.deferred.push(entry);
                continue;
            };
            let to = self.types.id(&format!("userdata: Instance {class}"));
            if self.by_type.len() <= to as usize {
                self.by_type.resize(to as usize + 1, Amount::default());
            }
            self.by_type[from as usize].add(-(entry.size as f64), -1.0);
            self.by_type[to as usize].add(entry.size as f64, 1.0);
            if let Some((stack, weight)) = entry.sampled {
                self.by_stack.entry((entry.phase, stack, from)).or_default().add(-(entry.size as f64) * weight, -weight);
                self.by_stack.entry((entry.phase, stack, to)).or_default().add(entry.size as f64 * weight, weight);
            }
            if let Some(live) = self.live.get_mut(&entry.block) {
                live.ty = to;
            }
        }
        self.by_stack.retain(|_, amount| amount.count.abs() > 1e-9);
        self.resolver = Some(resolver);
        // (a destroyed Instance's class is gone with it: those stay as they are, and the list stays bounded)
        if self.deferred.len() > 1 << 20 {
            self.deferred.clear();
        }
    }
}

/// The type an Instance's userdata is booked under until its class can be read
pub const UNRESOLVED_INSTANCE: &str = "userdata: Instance";

/// An Instance userdata's slot and generation (lua/raw.rs Payload): None for any other object, a userdata of another
/// tag (whatever its metatable's __type says) or one without data
fn instance_slot(object: &vm::Object, instance_tag: i32) -> Option<(u32, u32)> {
    let data = object.data;
    if object.tt != vm::T_USERDATA || object.tag != instance_tag || data.is_null() {
        return None;
    }
    let words = data as *const u32;
    // SAFETY: a userdata of the Instance tag holds a Payload (lua/raw.rs), which starts with its slot and generation
    Some(unsafe { (words.read_unaligned(), words.add(1).read_unaligned()) })
}

/// An object's type as the profile names it: `userdata: Instance Part` for an Instance (its class), `userdata: CFrame`
/// for a datatype (its metatable's __type)
pub fn object_name(object: &vm::Object, instance_tag: i32, resolver: Option<&dyn Fn(u32, u32) -> Option<String>>) -> String {
    if object.tt != vm::T_USERDATA {
        return type_name(object.tt).to_string();
    }
    if let Some((index, generation)) = instance_slot(object, instance_tag) {
        return match resolver.and_then(|resolve| resolve(index, generation)) {
            Some(class) => format!("userdata: Instance {class}"),
            None => UNRESOLVED_INSTANCE.into(),
        };
    }
    match &object.type_name {
        Some(name) => format!("userdata: {name}"),
        None => "userdata".into(),
    }
}

/// An instance's class from its slot and generation, while the engine's state can be read
pub fn resolver(engine: Weak<Engine>) -> Resolver {
    Box::new(move |index, generation| {
        let engine = engine.upgrade()?;
        let state = engine.state.try_borrow().ok()?;
        let instance = state.dom.get(crate::InstanceId { index, generation })?;
        Some(engine.reflection.class(instance.class).name.to_string())
    })
}

/// Luau's `onallocate` callback (the block, memory category, type and tag it also gives are read through lua_allocblock,
/// which also says what non-object memory is for)
unsafe extern "C-unwind" fn allocating(state: *mut lua_State, _block: *mut std::ffi::c_void, old: usize, new: usize, _memcat: u8, _tt: i32, _tag: i32) {
    ALLOCATIONS.with(|cell| {
        let Ok(mut allocations) = cell.try_borrow_mut() else { return };
        let Some(allocations) = allocations.as_mut() else { return };
        allocations.resolve_pending();
        if new <= old {
            return;
        }
        let size = (new - old) as u64;
        let (block, kind) = vm::alloc_block(state);
        let sampled = allocations.sample(size).map(|weight| (allocations.stacks.of(state), weight));
        allocations.bytes += size;
        if kind == vm::ALLOC_OBJECT && !block.is_null() {
            allocations.objects += 1;
            allocations.pending = Some(Pending { block: block as usize, size, phase: allocations.phase, sampled });
        } else {
            let name = match kind {
                vm::ALLOC_TABLE if BOOKKEEPING.with(|flag| flag.get()) => ENGINE_CACHE,
                vm::ALLOC_TABLE => "table storage",
                vm::ALLOC_THREAD => "thread stack",
                vm::ALLOC_STRINGS => "string table",
                vm::ALLOC_PROTO => "prototype arrays",
                _ => "other",
            };
            let ty = allocations.types.id(name);
            let phase = allocations.phase;
            allocations.book(phase, ty, size, sampled);
        }
    });
}

/// The VM frees a marked object
unsafe extern "C-unwind" fn freed(_ud: *mut c_void, block: *mut c_void) {
    ALLOCATIONS.with(|cell| {
        if let Ok(mut allocations) = cell.try_borrow_mut() {
            if let Some(allocations) = allocations.as_mut() {
                allocations.live.remove(&(block as usize));
            }
        }
    });
}

/// The VM's rehash hook: a table rehashed to fit a new key. Booked by the stack that made it rehash and the stack that
/// allocated the table (when the profile follows it: the heap kind's live objects).
unsafe extern "C-unwind" fn rehashing(_ud: *mut c_void, state: *mut lua_State, table: *mut c_void, old_array: c_int, old_nodes: c_int, new_array: c_int, new_nodes: c_int, keys: c_int, script: c_int) {
    ALLOCATIONS.with(|cell| {
        let Ok(mut allocations) = cell.try_borrow_mut() else { return };
        let Some(allocations) = allocations.as_mut() else { return };
        // (a table made by the last allocation is booked first, so that it is followed)
        allocations.resolve_pending();
        let stack = allocations.stacks.of(state);
        let made = allocations.live.get(&(table as usize)).map_or(NO_STACK, |live| live.stack);
        let parts = |array: c_int, nodes: c_int| array.max(0) as u64 * allocations.slot_bytes + nodes.max(0) as u64 * allocations.node_bytes;
        let (before, after) = (parts(old_array, old_nodes), parts(new_array, new_nodes));
        let (keys, script) = (keys.max(0) as u64, script != 0);
        allocations.rehash.entry((allocations.phase, stack, made)).or_default().add(keys, script, before, after);
        allocations.rehashes.add(keys, script, before, after);
    });
}

/// Starts the allocation profile of `state`'s VM (the VM's profile must have started: its function ids name the stacks).
pub fn start(state: *mut lua_State, step: u64, track_live: bool, resolver: Option<Resolver>) {
    ALLOCATIONS.with(|cell| *cell.borrow_mut() = Some(Allocations::new(step, track_live, resolver)));
    if track_live {
        vm::set_freed(state, Some(freed), std::ptr::null_mut());
    }
    vm::set_rehash(state, Some(rehashing), std::ptr::null_mut());
    unsafe { (*mlua::ffi::lua_callbacks(state)).onallocate = Some(allocating) };
}

/// Ends it: the profile so far (the callback is removed)
pub fn stop(state: *mut lua_State) -> Option<Allocations> {
    unsafe { (*mlua::ffi::lua_callbacks(state)).onallocate = None };
    vm::set_freed(state, None, std::ptr::null_mut());
    vm::set_rehash(state, None, std::ptr::null_mut());
    ALLOCATIONS.with(|cell| {
        let mut allocations = cell.borrow_mut().take()?;
        allocations.resolve_pending();
        allocations.resolve_deferred();
        Some(allocations)
    })
}

/// Runs `body` with the allocation profile (a snapshot reads its live objects)
pub fn with<R>(body: impl FnOnce(&mut Allocations) -> R) -> Option<R> {
    ALLOCATIONS.with(|cell| {
        let mut allocations = cell.try_borrow_mut().ok()?;
        let allocations = allocations.as_mut()?;
        allocations.resolve_pending();
        Some(body(allocations))
    })
}

/// The collector's work, from the VM's interrupt at each step's start and end
#[derive(Clone, Debug, Default)]
pub struct Collector {
    pub steps: u64,
    pub cycles: u64,
    pub seconds: f64,
    pub freed: u64,
    /// (steps, seconds) by the state a step started in: pause, propagate, propagate again, atomic, sweep
    pub by_state: [(u64, f64); 5],
    started: Option<(Instant, usize, i32)>,
}

pub const GC_STATES: [&str; 5] = ["pause", "propagate", "propagate again", "atomic", "sweep"];

impl Collector {
    /// The interrupt with `gc` >= 0: a step begins (the first of a pair) or ends
    pub fn interrupt(&mut self, state: *mut lua_State) {
        let bytes = vm::total_bytes(state);
        let now = vm::gc_state(state);
        match self.started.take() {
            None => self.started = Some((Instant::now(), bytes, now)),
            Some((at, before, from)) => {
                let seconds = at.elapsed().as_secs_f64();
                self.steps += 1;
                self.seconds += seconds;
                self.freed += before.saturating_sub(bytes) as u64;
                if from != 0 && now == 0 {
                    self.cycles += 1;
                }
                if let Some(slot) = self.by_state.get_mut(from.clamp(0, 4) as usize) {
                    slot.0 += 1;
                    slot.1 += seconds;
                }
            }
        }
    }
}

/// A snapshot of the heap: what the collector's roots reach (what a full collection would keep) and the garbage the
/// collector has not swept yet (what that collection would free), apart
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub label: String,
    pub frame: u64,
    pub seconds: f64,
    /// The VM's own count of its heap (gcinfo() * 1024): the objects, reachable or not swept yet, and the string table
    pub total_bytes: usize,
    /// Every collectable object in the heap now, reachable or not swept yet
    pub objects: u64,
    pub object_bytes: u64,
    /// What the collector's roots reach, what scripts and the engine still hold: (count, bytes)
    pub reachable: (u64, u64),
    /// Garbage the collector has not freed yet: objects no root reaches, and objects a sweep found dead: (count, bytes)
    pub unswept: (u64, u64),
    /// The reachable objects by type: (type, count, bytes), largest first
    pub by_type: Vec<(String, u64, u64)>,
    /// The garbage not swept yet by type, largest first
    pub unswept_by_type: Vec<(String, u64, u64)>,
    pub biggest: Vec<Held>,
    /// Reachable objects the run allocated, by (stack, type), weighted: (stack, type name, count, bytes now)
    pub by_stack: Vec<(u32, String, f64, f64)>,
    /// Of the reachable objects, those the run allocated (followed since the profile began), weighted
    pub allocated_since: (f64, f64),
    /// The reachable tables and their shapes, by the site that allocated them
    pub tables: Tables,
}

/// A size over a set of tables: its total and how much of it holds a value (a part's slots), its least, median and
/// greatest, and how many tables fall in each bucket, as (upper bound, tables): powers of two for a part's slots (a part
/// of 3 slots counts under 4), each value for nesting levels
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Distribution {
    pub total: u64,
    pub used: u64,
    pub min: u32,
    pub median: u32,
    pub max: u32,
    pub hist: Vec<(u32, u64)>,
}

impl Distribution {
    /// Of (size, used) pairs; `powers` buckets sizes by the next power of two
    pub fn of(values: &mut [(u32, u32)], powers: bool) -> Distribution {
        if values.is_empty() {
            return Distribution::default();
        }
        values.sort_unstable();
        let mut hist: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
        for &(size, _) in values.iter() {
            *hist.entry(if powers && size > 0 { size.next_power_of_two() } else { size }).or_default() += 1;
        }
        Distribution {
            total: values.iter().map(|&(size, _)| size as u64).sum(),
            used: values.iter().map(|&(_, used)| used as u64).sum(),
            min: values[0].0,
            median: values[(values.len() - 1) / 2].0,
            max: values[values.len() - 1].0,
            hist: hist.into_iter().collect(),
        }
    }
}

/// A snapshot's reachable tables: their totals, and those the run allocated by the site that allocated them
#[derive(Clone, Debug, Default)]
pub struct Tables {
    pub count: u64,
    /// Each table with its array and hash parts, as the VM counts them (gcinfo), and those apart
    pub bytes: u64,
    pub header_bytes: u64,
    pub array_bytes: u64,
    pub hash_bytes: u64,
    /// The bytes of a table itself, an array slot, a hash node
    pub sizes: (u64, u64, u64),
    /// The reachable tables the profile followed, by (phase, stack) of their allocation, most bytes first
    pub sites: Vec<TableSite>,
}

/// The reachable tables one site allocated: how many, their bytes, and the distributions of their array parts, hash parts
/// and nesting levels (how many levels of tables a table is with those it holds: 1 holds no table; tables in a cycle share
/// one level; a metatable is not held). Counts and bytes are weighted as the allocation stacks are; the distributions
/// count the tables followed.
#[derive(Clone, Debug, Default)]
pub struct TableSite {
    pub phase: u32,
    pub stack: u32,
    pub count: f64,
    pub bytes: f64,
    pub header_bytes: f64,
    pub array_bytes: f64,
    pub hash_bytes: f64,
    pub array: Distribution,
    pub hash: Distribution,
    pub levels: Distribution,
}

/// Each table's nesting level (`n` tables; `successors(t)` the tables table t holds as a key or a value): 1 for a table
/// that holds none, else one more than the deepest it holds; the tables of a cycle share their level (Tarjan's strongly
/// connected components, finished after every component they reach).
pub fn table_levels(n: usize, offsets: &[u32], targets: &[u32]) -> Vec<u32> {
    const NONE: u32 = u32::MAX;
    let edges = |node: u32| offsets[node as usize] as usize..offsets[node as usize + 1] as usize;
    let (mut order, mut low, mut component) = (vec![NONE; n], vec![0u32; n], vec![NONE; n]);
    let mut on_stack = vec![false; n];
    let mut levels_of: Vec<u32> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut counter = 0u32;
    for root in 0..n as u32 {
        if order[root as usize] != NONE {
            continue;
        }
        let mut calls: Vec<(u32, usize)> = vec![(root, edges(root).start)];
        order[root as usize] = counter;
        low[root as usize] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (node, ref mut next)) = calls.last_mut() {
            if *next < edges(node).end {
                let to = targets[*next];
                *next += 1;
                if order[to as usize] == NONE {
                    order[to as usize] = counter;
                    low[to as usize] = counter;
                    counter += 1;
                    stack.push(to);
                    on_stack[to as usize] = true;
                    calls.push((to, edges(to).start));
                } else if on_stack[to as usize] {
                    low[node as usize] = low[node as usize].min(order[to as usize]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(parent, _)) = calls.last() {
                low[parent as usize] = low[parent as usize].min(low[node as usize]);
            }
            if low[node as usize] != order[node as usize] {
                continue;
            }
            let id = levels_of.len() as u32;
            let mut members: Vec<u32> = Vec::new();
            while let Some(member) = stack.pop() {
                on_stack[member as usize] = false;
                component[member as usize] = id;
                members.push(member);
                if member == node {
                    break;
                }
            }
            let mut level = 1;
            for &member in &members {
                for at in edges(member) {
                    let other = component[targets[at] as usize];
                    if other != id && other != NONE {
                        level = level.max(levels_of[other as usize] + 1);
                    }
                }
            }
            levels_of.push(level);
        }
    }
    component.iter().map(|&id| levels_of[id as usize]).collect()
}

/// One of the biggest objects: what it keeps alive (its dominated size) and the path that holds it
#[derive(Clone, Debug, Default)]
pub struct Held {
    pub ty: String,
    pub name: String,
    pub size: u64,
    pub retained: u64,
    pub path: String,
    /// The stack that allocated it, when the profile followed it
    pub stack: Option<u32>,
}

/// Walks the heap and summarizes it. `allocations` maps the live sampled objects to their stacks (ptr: their block).
///
/// The walk follows every reference the collector follows from the collector's roots (lua.h lua_enumheap), so what it
/// reaches is what a full collection would keep, and the rest, with the objects a sweep found dead and has not freed, is
/// what it would free. Nothing is collected: a snapshot changes nothing the VM does.
pub fn snapshot(state: *mut lua_State, label: &str, frame: u64, seconds: f64, resolver: Option<&dyn Fn(u32, u32) -> Option<String>>, top: usize) -> Snapshot {
    let main = vm::main_thread(state) as usize;
    let instance_tag = crate::lua::raw::INSTANCE_TAG;
    // (a userdata's pointer is its data: the offset back to its block, from a probe of the layout)
    let mut ptrs: Vec<usize> = Vec::new();
    let mut tts: Vec<u8> = Vec::new();
    let mut sizes: Vec<u64> = Vec::new();
    let mut names: Vec<Option<Box<str>>> = Vec::new();
    let mut index: FxHashMap<usize, u32> = FxHashMap::default();
    let mut edges: Vec<(u32, u32, u32)> = Vec::new();
    let mut edge_names = Types::default();
    let mut pending_edges: Vec<(usize, usize, u32)> = Vec::new();
    let mut root_edges: Vec<(usize, u32)> = Vec::new();
    let mut dead: FxHashMap<u8, (u64, u64)> = FxHashMap::default();
    // (each table's shape, read while the walk holds it: (node, shape))
    let mut shapes: Vec<(u32, vm::TableShape)> = Vec::new();
    vm::enum_heap(
        state,
        |ptr, tt, size, name| {
            if tt as i32 == vm::T_TABLE {
                if let Some(shape) = unsafe { vm::table_shape(ptr as *mut c_void) } {
                    shapes.push((ptrs.len() as u32, shape));
                }
            }
            index.insert(ptr, ptrs.len() as u32);
            ptrs.push(ptr);
            tts.push(tt);
            sizes.push(size as u64);
            names.push(name.map(|name| name.into()));
        },
        |from, to, name| {
            let name = edge_names.id(name.unwrap_or(""));
            if from == 0 {
                root_edges.push((to, name));
            } else {
                pending_edges.push((from, to, name));
            }
        },
        |tt, size| {
            let entry = dead.entry(tt).or_default();
            entry.0 += 1;
            entry.1 += size as u64;
        },
    );
    for (from, to, name) in pending_edges {
        if let (Some(&from), Some(&to)) = (index.get(&from), index.get(&to)) {
            edges.push((from, to, name));
        }
    }
    let count = ptrs.len();
    let root = count as u32;
    // successors in CSR form, the virtual root's first: the collector's roots in the walk's order (the main thread, the
    // registry, the basic types' metatables, the fixed objects), each by the name of its edge
    let mut roots: Vec<(u32, u32)> = Vec::new();
    let mut is_root = vec![false; count];
    for (ptr, name) in root_edges {
        if let Some(&node) = index.get(&ptr) {
            if !is_root[node as usize] {
                is_root[node as usize] = true;
                roots.push((node, name));
            }
        }
    }
    let mut offsets = vec![0u32; count + 2];
    for &(from, _, _) in &edges {
        offsets[from as usize + 1] += 1;
    }
    offsets[count + 1] = roots.len() as u32;
    for node in 0..count + 1 {
        offsets[node + 1] += offsets[node];
    }
    let mut targets = vec![0u32; edges.len() + roots.len()];
    let mut target_names = vec![0u32; edges.len() + roots.len()];
    let mut fill = offsets.clone();
    for &(from, to, name) in &edges {
        let at = fill[from as usize] as usize;
        targets[at] = to;
        target_names[at] = name;
        fill[from as usize] += 1;
    }
    for &(node, name) in &roots {
        let at = fill[count] as usize;
        targets[at] = node;
        target_names[at] = name;
        fill[count] += 1;
    }
    let successors = |node: u32| -> std::ops::Range<usize> { offsets[node as usize] as usize..offsets[node as usize + 1] as usize };

    // depth-first order from the root (reverse postorder numbers), and a shortest-ish holder (BFS parent) for paths
    const NONE: u32 = u32::MAX;
    let total = count + 1;
    let mut postorder: Vec<u32> = Vec::with_capacity(total);
    let mut number = vec![NONE; total];
    let mut visited = vec![false; total];
    let mut stack: Vec<(u32, usize)> = vec![(root, successors(root).start)];
    visited[root as usize] = true;
    while let Some(&mut (node, ref mut next)) = stack.last_mut() {
        let range = successors(node);
        if *next < range.end {
            let child = targets[*next];
            *next += 1;
            if !visited[child as usize] {
                visited[child as usize] = true;
                stack.push((child, successors(child).start));
            }
        } else {
            number[node as usize] = postorder.len() as u32;
            postorder.push(node);
            stack.pop();
        }
    }
    // predecessors of the reachable nodes
    let mut pred_offsets = vec![0u32; total + 1];
    for node in 0..total as u32 {
        if !visited[node as usize] {
            continue;
        }
        for at in successors(node) {
            pred_offsets[targets[at] as usize + 1] += 1;
        }
    }
    for node in 0..total {
        pred_offsets[node + 1] += pred_offsets[node];
    }
    let mut preds = vec![0u32; pred_offsets[total] as usize];
    let mut pred_fill = pred_offsets.clone();
    for node in 0..total as u32 {
        if !visited[node as usize] {
            continue;
        }
        for at in successors(node) {
            let to = targets[at] as usize;
            preds[pred_fill[to] as usize] = node;
            pred_fill[to] += 1;
        }
    }
    // dominators (Cooper, Harvey and Kennedy's iterative algorithm, on postorder numbers)
    let mut idom = vec![NONE; total];
    idom[root as usize] = root;
    let intersect = |idom: &[u32], mut a: u32, mut b: u32| -> u32 {
        while a != b {
            while number[a as usize] < number[b as usize] {
                a = idom[a as usize];
            }
            while number[b as usize] < number[a as usize] {
                b = idom[b as usize];
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &node in postorder.iter().rev() {
            if node == root {
                continue;
            }
            let mut new_idom = NONE;
            for &pred in &preds[pred_offsets[node as usize] as usize..pred_offsets[node as usize + 1] as usize] {
                if idom[pred as usize] == NONE {
                    continue;
                }
                new_idom = if new_idom == NONE { pred } else { intersect(&idom, pred, new_idom) };
            }
            if new_idom != NONE && idom[node as usize] != new_idom {
                idom[node as usize] = new_idom;
                changed = true;
            }
        }
    }
    // retained sizes: each reachable node's size added up its dominator chain (children come before parents in postorder)
    let mut retained: Vec<u64> = (0..total).map(|node| if node < count { sizes[node] } else { 0 }).collect();
    for &node in &postorder {
        if node == root {
            continue;
        }
        let dominator = idom[node as usize];
        if dominator != NONE && dominator != node {
            retained[dominator as usize] += retained[node as usize];
        }
    }

    // by type (and Instance class): what is reachable, and the garbage not swept yet (what no root reaches, and what a
    // sweep found dead: its type alone, since its references may be freed)
    let offset = vm::udata_data_offset();
    let block_of = |node: usize| -> usize { if tts[node] as i32 == vm::T_USERDATA { ptrs[node].wrapping_sub(offset) } else { ptrs[node] } };
    let resolve = |node: usize| -> String {
        if tts[node] as i32 == vm::T_USERDATA {
            // (the node's pointer is the userdata's data; its block holds the tag)
            let object = unsafe { vm::object(block_of(node) as *mut c_void) };
            return object_name(&object, instance_tag, resolver);
        }
        type_name(tts[node] as i32).to_string()
    };
    let mut by_type: HashMap<String, (u64, u64)> = HashMap::new();
    let mut garbage: HashMap<String, (u64, u64)> = HashMap::new();
    let (mut reachable, mut unswept) = ((0u64, 0u64), (0u64, 0u64));
    for node in 0..count {
        let (types, total) = if visited[node] { (&mut by_type, &mut reachable) } else { (&mut garbage, &mut unswept) };
        let entry = types.entry(resolve(node)).or_default();
        entry.0 += 1;
        entry.1 += sizes[node];
        total.0 += 1;
        total.1 += sizes[node];
    }
    for (&tt, &(found, bytes)) in &dead {
        let entry = garbage.entry(type_name(tt as i32).to_string()).or_default();
        entry.0 += found;
        entry.1 += bytes;
        unswept.0 += found;
        unswept.1 += bytes;
    }
    let sorted = |types: HashMap<String, (u64, u64)>| -> Vec<(String, u64, u64)> {
        let mut types: Vec<(String, u64, u64)> = types.into_iter().map(|(name, (count, bytes))| (name, count, bytes)).collect();
        types.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        types
    };
    let (by_type, unswept_by_type) = (sorted(by_type), sorted(garbage));
    let object_bytes = reachable.1 + unswept.1;

    // holder paths: breadth-first parents from the root
    let mut parent = vec![(NONE, NONE); total];
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root);
    let mut seen = vec![false; total];
    seen[root as usize] = true;
    while let Some(node) = queue.pop_front() {
        for at in successors(node) {
            let child = targets[at];
            if !seen[child as usize] {
                seen[child as usize] = true;
                parent[child as usize] = (node, target_names[at]);
                queue.push_back(child);
            }
        }
    }
    let label_of = |node: usize| -> String {
        match names[node].as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => resolve(node),
        }
    };
    let path_of = |node: u32| -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut current = node;
        while parent[current as usize].0 != NONE && parts.len() < 24 {
            let (up, edge) = parent[current as usize];
            let edge = edge_names.names.get(edge as usize).cloned().unwrap_or_default();
            // (a root by the name of its edge: "main thread", "registry", "metatable of string", "fixed")
            if up == root {
                parts.push(if edge.is_empty() { label_of(current as usize) } else { edge });
                break;
            }
            parts.push(if edge.is_empty() { label_of(current as usize) } else { edge });
            current = up;
        }
        parts.reverse();
        // (the registry's array part holds the references the engine keeps to Luau values: a handler, a thread it
        // will resume, a module's result)
        if parts.len() >= 2 && parts[0] == "registry" && parts[1] == "array" {
            parts.splice(0..2, ["(engine reference)".to_string()]);
        }
        parts.join(" > ")
    };

    // the biggest by what they keep alive (the roots, among them the registry and the main thread, and the globals keep
    // nearly everything: left out)
    let main_index = index.get(&main).copied();
    let mut candidates: Vec<u32> = (0..count as u32).filter(|&node| visited[node as usize] && !is_root[node as usize]).collect();
    candidates.sort_by(|&a, &b| retained[b as usize].cmp(&retained[a as usize]).then(a.cmp(&b)));
    // (the main thread's globals table keeps nearly everything too)
    let globals = main_index.and_then(|main| successors(main).find(|&at| edge_names.names.get(target_names[at] as usize).map(String::as_str) == Some("globals")).map(|at| targets[at]));
    let keep_out = |node: u32| -> bool { Some(node) == globals };
    let live = with(|allocations| allocations.live.clone()).unwrap_or_default();
    let biggest: Vec<Held> = candidates
        .iter()
        .filter(|&&node| !keep_out(node))
        .take(top)
        .map(|&node| Held {
            ty: resolve(node as usize),
            name: names[node as usize].as_deref().unwrap_or("").to_string(),
            size: sizes[node as usize],
            retained: retained[node as usize],
            path: path_of(node),
            stack: live.get(&block_of(node as usize)).map(|live| live.stack),
        })
        .collect();

    // the run's reachable objects by the stack that allocated them, at their size now (its garbage is still followed
    // until the collector frees it)
    let mut by_stack: FxHashMap<(u32, u32), (f64, f64)> = FxHashMap::default();
    let mut allocated_since = (0.0, 0.0);
    let mut type_names: Vec<String> = Vec::new();
    with(|allocations| type_names = allocations.types.names.clone());
    for node in (0..count).filter(|&node| visited[node]) {
        if let Some(followed) = live.get(&block_of(node)) {
            let entry = by_stack.entry((followed.stack, followed.ty)).or_default();
            entry.0 += followed.weight;
            entry.1 += followed.weight * sizes[node] as f64;
            allocated_since.0 += followed.weight;
            allocated_since.1 += followed.weight * sizes[node] as f64;
        }
    }
    let mut by_stack: Vec<(u32, String, f64, f64)> = by_stack.into_iter().map(|((stack, ty), (count, bytes))| (stack, type_names.get(ty as usize).cloned().unwrap_or_default(), count, bytes)).collect();
    by_stack.sort_by(|a, b| b.3.total_cmp(&a.3).then(a.0.cmp(&b.0)));

    // the reachable tables: their shapes, nesting levels (over the references between tables, a metatable's aside) and
    // the sites that allocated them
    let tables = {
        let metatable = edge_names.id("metatable");
        let reachable: Vec<(u32, vm::TableShape)> = shapes.into_iter().filter(|&(node, _)| visited[node as usize]).collect();
        let mut table_of = vec![NONE; count];
        for (at, &(node, _)) in reachable.iter().enumerate() {
            table_of[node as usize] = at as u32;
        }
        let mut table_offsets = vec![0u32; reachable.len() + 1];
        let mut table_edges: Vec<(u32, u32)> = edges
            .iter()
            .filter(|&&(from, to, name)| name != metatable && table_of[from as usize] != NONE && table_of[to as usize] != NONE)
            .map(|&(from, to, _)| (table_of[from as usize], table_of[to as usize]))
            .collect();
        table_edges.sort_unstable();
        table_edges.dedup();
        for &(from, _) in &table_edges {
            table_offsets[from as usize + 1] += 1;
        }
        for at in 0..reachable.len() {
            table_offsets[at + 1] += table_offsets[at];
        }
        let table_targets: Vec<u32> = table_edges.iter().map(|&(_, to)| to).collect();
        let levels = table_levels(reachable.len(), &table_offsets, &table_targets);
        let (header, slot, node_bytes) = vm::table_bytes();
        let mut tables = Tables { sizes: (header, slot, node_bytes), ..Tables::default() };
        type Shapes = (Vec<(u32, u32)>, Vec<(u32, u32)>, Vec<(u32, u32)>);
        let mut sites: FxHashMap<(u32, u32), (TableSite, Shapes)> = FxHashMap::default();
        for (at, &(node, shape)) in reachable.iter().enumerate() {
            let (array, hash) = (shape.array as u64 * slot, shape.nodes as u64 * node_bytes);
            let size = sizes[node as usize];
            tables.count += 1;
            tables.bytes += size;
            tables.header_bytes += size.saturating_sub(array + hash);
            tables.array_bytes += array;
            tables.hash_bytes += hash;
            let Some(followed) = live.get(&block_of(node as usize)) else { continue };
            let (site, (arrays, hashes, nests)) = sites.entry((followed.phase, followed.stack)).or_insert_with(|| (TableSite { phase: followed.phase, stack: followed.stack, ..TableSite::default() }, Shapes::default()));
            let weight = followed.weight;
            site.count += weight;
            site.bytes += weight * size as f64;
            site.header_bytes += weight * size.saturating_sub(array + hash) as f64;
            site.array_bytes += weight * array as f64;
            site.hash_bytes += weight * hash as f64;
            arrays.push((shape.array, shape.array_used));
            hashes.push((shape.nodes, shape.nodes_used));
            nests.push((levels[at], 0));
        }
        tables.sites = sites
            .into_values()
            .map(|(mut site, (mut arrays, mut hashes, mut nests))| {
                site.array = Distribution::of(&mut arrays, true);
                site.hash = Distribution::of(&mut hashes, true);
                site.levels = Distribution::of(&mut nests, false);
                site
            })
            .collect();
        tables.sites.sort_by(|a, b| b.bytes.total_cmp(&a.bytes).then(a.phase.cmp(&b.phase)).then(a.stack.cmp(&b.stack)));
        tables
    };

    Snapshot {
        label: label.to_string(),
        frame,
        seconds,
        total_bytes: vm::total_bytes(state),
        objects: reachable.0 + unswept.0,
        object_bytes,
        reachable,
        unswept,
        by_type,
        unswept_by_type,
        biggest,
        by_stack,
        allocated_since,
        tables,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a userdata of the Instance tag, with data, has a slot to read: another userdata whose metatable's __type is
    /// "Instance" (or one with no data) is named like an Instance still waiting for its class, but nothing is read from it
    #[test]
    fn only_an_instances_own_userdata_has_a_slot_read() {
        let tag = crate::lua::raw::INSTANCE_TAG;
        let impostor = vm::Object { tt: vm::T_USERDATA, tag: tag + 1, data: std::ptr::null_mut(), type_name: Some("Instance".into()) };
        assert_eq!(object_name(&impostor, tag, None), UNRESOLVED_INSTANCE);
        assert_eq!(instance_slot(&impostor, tag), None);
        let empty = vm::Object { tt: vm::T_USERDATA, tag, data: std::ptr::null_mut(), type_name: None };
        assert_eq!(instance_slot(&empty, tag), None);
        assert_eq!(instance_slot(&vm::Object { tt: vm::T_STRING, tag, data: std::ptr::null_mut(), type_name: None }, tag), None);
        let mut payload = [7u32, 3, 0, 0];
        let real = vm::Object { tt: vm::T_USERDATA, tag, data: payload.as_mut_ptr() as *mut c_void, type_name: None };
        assert_eq!(instance_slot(&real, tag), Some((7, 3)));
        let resolve = |index: u32, generation: u32| (index == 7 && generation == 3).then(|| "Part".to_string());
        assert_eq!(object_name(&real, tag, Some(&resolve)), "userdata: Instance Part");
        assert_eq!(object_name(&real, tag, None), UNRESOLVED_INSTANCE);
    }
}

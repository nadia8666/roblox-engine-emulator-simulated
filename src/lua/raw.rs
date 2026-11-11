//! Instances as raw tagged Luau userdata with C metamethods (the hot path of the script API),
//! and their lifetime.
//!
//! An Instance is a 24-byte userdata with tag [`INSTANCE_TAG`] (payload: slot index, generation,
//! creation serial and a pointer to this engine's [`Api`]); the tag's metatable (`lua_setuserdatametatable`) holds
//! C functions for `__index`, `__newindex`, `__namecall` and `__tostring`. They bypass mlua's
//! callback machinery: keys resolve through Luau string atoms (`useratom` maps a string to its
//! interned `NameId` once per string), members through a per-(class, name) dispatch cache, and
//! the common cases (plain property reads and writes of primitive / enum / instance values,
//! cached datatype values, Name / Parent / ClassName, children by name, events, the frequent
//! Instance methods) run without a Luau or mlua call. Everything else falls back to mlua
//! functions (`instance::get` / `instance::assign`, a behaviour method's protocol function)
//! called from C. Script errors are raised from the C function itself with `luaL_where(L, 1)`
//! (the script line), after every Rust value is dropped, exactly where the old Luau wrappers
//! raised them; method yields are `lua_yield` from the C function (it has a continuation, so the
//! Luau stub methods it calls may yield too).
//!
//! Lifetime: userdata are cached in a weak-valued table by slot, and anchored (a strong table)
//! while their instance is in the DataModel, so identity holds for every instance a script can
//! still reach while unreferenced parentless instances become collectable. A userdata's
//! destructor counts it out; [`collect`] (run from the frame loop) frees a parentless subtree
//! once none of its instances has a userdata, nothing in the engine references it (instance
//! properties, queued events and timers, pins) and it is not the DataModel.

use std::{
    cell::{Cell, RefCell},
    ffi::{c_char, c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{Arc, OnceLock},
};

use mlua::{ffi, prelude::*};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{
    Flow, instance as linstance,
    instance::{find_first_child, find_first_child_which_is_a, materialize},
};
use crate::{
    ClassId, Engine, InstanceId, K, NameId, Reflection, State,
    dom::flags,
    instance::{self as core, Member as Resolved},
    reflection::ScriptType,
    value::{Str, Value},
};

/// The userdata tag of Instances (mlua uses 1 and its internal destructor tag).
pub const INSTANCE_TAG: c_int = 73;
/// A tag only used to check the Udata header layout at startup.
const PROBE_TAG: c_int = 74;

#[repr(C)]
struct Payload {
    index: u32,
    generation: u32,
    /// The instance's creation serial: what the patched VM hashes an Instance key by (ltable.cpp `hashuserdata`). A slot is
    /// reused when the collector frees an instance, which depends on the heap, so a slot hash orders `pairs` differently per run.
    serial: u32,
    api: *const Api,
}

const PAYLOAD_SIZE: usize = std::mem::size_of::<Payload>();

const REGISTRY: c_int = ffi::LUA_REGISTRYINDEX;

// Udata header (lobject.h): tt, marked, memcat, tag (u8 each), len (int), metatable, data
const TAG_OFFSET: usize = 13;
const LEN_OFFSET: usize = 12;

/// The instance behind a userdata (no Lua call: reads the Udata header in front of the payload).
#[inline]
pub fn instance_of_ud(ud: &LuaAnyUserData) -> Option<InstanceId> {
    let data = ud.to_pointer() as *const u8;
    if data.is_null() {
        return None;
    }
    unsafe {
        if *data.sub(TAG_OFFSET) as c_int != INSTANCE_TAG
            || std::ptr::read_unaligned(data.sub(LEN_OFFSET) as *const i32) as usize != PAYLOAD_SIZE
        {
            return None;
        }
        let payload = &*(data as *const Payload);
        Some(InstanceId { index: payload.index, generation: payload.generation })
    }
}

/// The instance a script value is, if any.
#[inline]
pub fn instance_id(value: &LuaValue) -> Option<InstanceId> {
    match value {
        LuaValue::UserData(ud) => instance_of_ud(ud),
        _ => None,
    }
}

// Dispatch

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Read {
    /// A reflected property without a getter: the stored value or the class default
    Plain,
    Name,
    Parent,
    ClassName,
    Method,
    Event,
    /// Not a member: a child by name, else an error
    Child,
    /// Getters, callbacks: `instance::get`
    Slow,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Write {
    /// A writable reflected property without a setter (property index)
    Plain(u32),
    Parent,
    Name,
    Slow,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Base {
    FindFirstChild,
    IsA,
    GetChildren,
    GetDescendants,
    FindFirstChildOfClass,
    FindFirstChildWhichIsA,
    FindFirstAncestor,
    FindFirstAncestorOfClass,
    FindFirstAncestorWhichIsA,
    IsDescendantOf,
    IsAncestorOf,
    GetPropertyChangedSignal,
    GetAttribute,
    SetAttribute,
    GetFullName,
    WaitForChild,
    HasTag,
    /// BindableEvent:Fire (services/mod.rs): fires Event with the arguments
    FireBindable,
    /// WorldRoot:Raycast (world/query.rs raw_cast)
    Raycast,
    /// PhysicsService:CollisionGroupsAreCollidable (world/query.rs CollisionGroups)
    GroupsCollide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Call {
    /// An Instance method implemented inline (the base behaviour, not overridden)
    Base(Base),
    /// Any other method: its function (protocol or Luau) called from C
    Function,
    /// Not a method: `instance[key](instance, ...)`
    NotMethod,
}

#[derive(Clone, Copy, Debug)]
struct Member {
    read: Read,
    write: Write,
    call: Call,
    /// Slot in the functions table of the function `instance[key]` returns (0: not yet made)
    index_fn: c_int,
    /// Slot of the function a method call runs (0: not yet made)
    call_fn: c_int,
    /// What a call of it costs Studio (crate::cost), charged to the work clock
    cost: f32,
    /// The name its value is kept under: the key's own, or for a deprecated lower-case property the property it
    /// stands for (`brickColor` -> BrickColor, Engine::member_name)
    name: NameId,
}

fn base_method(name: NameId) -> Option<Base> {
    const MAP: &[(K, Base)] = &[
        (K::FindFirstChild, Base::FindFirstChild),
        (K::findFirstChild, Base::FindFirstChild),
        (K::IsA, Base::IsA),
        (K::isA, Base::IsA),
        (K::GetChildren, Base::GetChildren),
        (K::children, Base::GetChildren),
        (K::getChildren, Base::GetChildren),
        (K::GetDescendants, Base::GetDescendants),
        (K::FindFirstChildOfClass, Base::FindFirstChildOfClass),
        (K::FindFirstChildWhichIsA, Base::FindFirstChildWhichIsA),
        (K::FindFirstAncestor, Base::FindFirstAncestor),
        (K::FindFirstAncestorOfClass, Base::FindFirstAncestorOfClass),
        (K::FindFirstAncestorWhichIsA, Base::FindFirstAncestorWhichIsA),
        (K::IsDescendantOf, Base::IsDescendantOf),
        (K::isDescendantOf, Base::IsDescendantOf),
        (K::IsAncestorOf, Base::IsAncestorOf),
        (K::GetPropertyChangedSignal, Base::GetPropertyChangedSignal),
        (K::GetAttribute, Base::GetAttribute),
        (K::SetAttribute, Base::SetAttribute),
        (K::GetFullName, Base::GetFullName),
        (K::WaitForChild, Base::WaitForChild),
        (K::HasTag, Base::HasTag),
    ];
    MAP.iter().find(|(k, _)| *k as NameId == name).map(|(_, base)| *base)
}

fn compute_member(engine: &Engine, class: ClassId, key: NameId) -> Member {
    let reflection = &engine.reflection;
    let base_class = reflection.class_id("Instance").expect("Instance class");
    // (a deprecated lower-case property is the property it stands for: its value, setter and Changed)
    let name = engine.member_name(class, key);
    let mut member = Member { read: Read::Slow, write: Write::Slow, call: Call::NotMethod, index_fn: 0, call_fn: 0, cost: crate::cost::method(reflection.name(name)) as f32, name };
    match engine.resolve_member(class, name) {
        Resolved::Property { property, getter, setter } => {
            let behaviors = engine.behaviors.borrow();
            let base_getter = |n: K| {
                name == n as NameId
                    && match (behaviors.find_getter(reflection, class, name), behaviors.find_getter(reflection, base_class, name)) {
                        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                        _ => false,
                    }
            };
            member.read = if !getter {
                if property.is_some() { Read::Plain } else { Read::Slow }
            } else if base_getter(K::Name) {
                Read::Name
            } else if base_getter(K::Parent) {
                Read::Parent
            } else if base_getter(K::ClassName) {
                Read::ClassName
            } else {
                Read::Slow
            };
            let base_setter = |n: K| {
                name == n as NameId
                    && match (behaviors.find_setter(reflection, class, name), behaviors.find_setter(reflection, base_class, name)) {
                        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                        _ => false,
                    }
            };
            member.write = if setter {
                if base_setter(K::Parent) {
                    Write::Parent
                } else if base_setter(K::Name) {
                    Write::Name
                } else {
                    Write::Slow
                }
            } else {
                match property {
                    Some(index) if !reflection.property(index).read_only => Write::Plain(index),
                    _ => Write::Slow,
                }
            };
        }
        Resolved::Method { implemented } => {
            member.read = Read::Method;
            member.call = Call::Function;
            if implemented {
                let owner = engine.behaviors.borrow().find_method(reflection, class, name).map(|(owner, _)| owner);
                if owner == Some(base_class) {
                    if let Some(base) = base_method(name) {
                        member.call = Call::Base(base);
                    }
                } else if owner.is_some() && owner == reflection.class_id("BindableEvent") && name == K::Fire as NameId {
                    member.call = Call::Base(Base::FireBindable);
                } else if owner.is_some() && reflection.name(name).to_string() == "Raycast" {
                    member.call = Call::Base(Base::Raycast);
                } else if owner.is_some() && reflection.name(name).to_string() == "CollisionGroupsAreCollidable" {
                    member.call = Call::Base(Base::GroupsCollide);
                }
            }
        }
        Resolved::Event => member.read = Read::Event,
        Resolved::Callback => {}
        Resolved::None => member.read = Read::Child,
    }
    member
}

/// Converted datatype property values (UDim2, Color3, ...): one userdata per (instance,
/// property) while the instance's version holds, kept in a Luau table so C can push it.
#[derive(Default)]
struct ValueCache {
    map: FxHashMap<(InstanceId, NameId), (u32, c_int)>,
    free: Vec<c_int>,
    next: c_int,
}

/// The value-cache key of an instance's GetChildren list (version: its child_version).
const CHILDREN_KEY: NameId = NameId::MAX;

impl ValueCache {
    fn slot(&mut self, key: (InstanceId, NameId), version: u32) -> c_int {
        let slot = match self.map.get(&key) {
            Some(&(_, slot)) => slot,
            None => match self.free.pop() {
                Some(slot) => slot,
                None => {
                    self.next += 1;
                    self.next
                }
            },
        };
        self.map.insert(key, (version, slot));
        slot
    }
}

/// Userdata lifetime bookkeeping (touched by the destructor: never allocate Lua memory while
/// this is borrowed).
#[derive(Default)]
struct Gc {
    /// Live userdata per slot index (0 or 1, 2 while a collected one awaits its destructor)
    live: Vec<u32>,
    /// Whether the slot's userdata is in the anchor table
    anchored: Vec<bool>,
    /// Slots whose last userdata was freed since the last collection
    dead: Vec<u32>,
    /// Instances that were unparented since the last collection
    orphans: Vec<InstanceId>,
    /// Per parentless root a collection found held: the instance that held it (`find_garbage`)
    holders: FxHashMap<InstanceId, InstanceId>,
    /// Roots whose tree an instance property or queued work (not a Luau userdata or a pin) kept at the last
    /// collection: nothing signals the day that reference goes, so they are looked at again (`find_garbage`)
    kept: Vec<InstanceId>,
    /// Frames since the last collection
    frames: u32,
}

/// A parent change the anchor table has not seen yet.
#[derive(Clone, Copy)]
struct Moved {
    id: InstanceId,
    /// The DataModel membership of the subtree changed: re-anchor the whole subtree
    subtree: bool,
    unparented: bool,
}

/// Registry references (for C) of the Luau tables and functions the fast path uses.
#[derive(Default, Clone, Copy)]
struct Refs {
    instances: c_int,
    anchors: c_int,
    values: c_int,
    enums: c_int,
    enum_ids: c_int,
    signals: c_int,
    functions: c_int,
    slow_index: c_int,
    slow_newindex: c_int,
    resolve_call: c_int,
    bridge: c_int,
}

/// Per-engine state of the raw Instance API (one per Lua state; payloads point at it).
pub struct Api {
    engine: *const Engine,
    lua: mlua::WeakLua,
    /// Whether `useratom` maps strings to this reflection's NameIds
    atoms: bool,
    refs: Refs,
    instances: LuaTable,
    values_table: LuaTable,
    enums: LuaTable,
    enum_ids: LuaTable,
    signals: LuaTable,
    functions: LuaTable,
    /// Hands a stack value to Rust as an mlua value (slot 1)
    bridge: LuaTable,
    create: RefCell<Option<LuaFunction>>,
    members: RefCell<FxHashMap<(ClassId, NameId), Member>>,
    values: RefCell<ValueCache>,
    next_function: Cell<c_int>,
    gc: RefCell<Gc>,
    moved: Rc<RefCell<Vec<Moved>>>,
    /// The scheduler's work clock charges (Scheduler.work)
    work: Rc<Cell<f64>>,
    /// While above 0 the engine's own Luau prices itself and the charges are dropped (Scheduler.muted)
    muted: Rc<Cell<u32>>,
    touched: Rc<RefCell<crate::cost::Touched>>,
    _keys: Vec<LuaRegistryKey>,
}

impl Api {
    #[inline]
    fn engine(&self) -> &Engine {
        unsafe { &*self.engine }
    }

    /// Charges script work to the work clock (crate::cost).
    #[inline]
    fn charge(&self, seconds: f64) {
        if self.muted.get() == 0 {
            crate::cost::tally_charge(seconds);
            crate::boundary::charged(seconds);
            self.work.set(self.work.get() + seconds);
        }
    }

    /// A crossing for the boundary profiler (src/boundary.rs): nothing unless profiling, and nothing inside the engine's own
    /// Luau pricing a whole call (muted); `name` None, or `child`, is a child looked up by name.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn cross(&self, kind: crate::boundary::Kind, class: ClassId, name: Option<NameId>, child: bool, id: InstanceId, cost: f64, changed: bool) {
        if crate::boundary::active() && self.muted.get() == 0 {
            let engine = self.engine();
            let reflection = &engine.reflection;
            // (a read's stamp: its instance's version, which a change of its own properties by a script or the engine bumps, so the next
            // read is a new value; Parent's value is the parent, which a reparent (Destroy's too) changes without touching the
            // instance's own properties; a child looked up by name reads the child list, whose own version a child added or removed
            // bumps, and the children's names, which any rename changes)
            let version = (kind == crate::boundary::Kind::Get)
                .then(|| {
                    engine.state.try_borrow().ok().and_then(|state| {
                        state.dom.get(id).map(|instance| match name {
                            Some(member) if !child && member == K::Parent as NameId => instance.version ^ instance.parent.map_or(u32::MAX, |parent| parent.index).rotate_left(16),
                            Some(_) if !child => instance.version,
                            _ => instance.child_version ^ state.dom.name_version.rotate_left(16),
                        })
                    })
                })
                .flatten();
            crate::boundary::record(kind, class, name.unwrap_or(crate::boundary::NO_MEMBER), id.index, version, cost, changed, || {
                let owner = name.map_or(class, |name| reflection.declaring_class(class, name));
                (reflection.class(owner).name.to_string(), name.map_or_else(|| "[child]".to_string(), |name| reflection.name(name).to_string()))
            });
        }
    }

    #[inline]
    fn member(&self, class: ClassId, name: NameId) -> Member {
        if let Some(member) = self.members.borrow().get(&(class, name)) {
            crate::profiler::caches::hit(crate::profiler::caches::Cache::Members);
            return *member;
        }
        crate::profiler::caches::miss(crate::profiler::caches::Cache::Members);
        let member = compute_member(self.engine(), class, name);
        self.members.borrow_mut().insert((class, name), member);
        member
    }

    fn next_function_slot(&self) -> c_int {
        let slot = self.next_function.get() + 1;
        self.next_function.set(slot);
        slot
    }

    fn note_created(&self, index: u32) {
        let mut gc = self.gc.borrow_mut();
        let index = index as usize;
        if gc.live.len() <= index {
            gc.live.resize(index + 1, 0);
            gc.anchored.resize(index + 1, false);
        }
        gc.live[index] += 1;
    }

    fn value_slot(&self, key: (InstanceId, NameId), version: u32) -> c_int {
        self.values.borrow_mut().slot(key, version)
    }

    pub fn clear_members(&self) {
        let mut members = self.members.borrow_mut();
        crate::profiler::caches::evicted(crate::profiler::caches::Cache::Members, members.len() as u64);
        members.clear();
    }
}

static ATOM_REFLECTION: OnceLock<Arc<Reflection>> = OnceLock::new();

/// `useratom`: a string's atom is its member NameId (-1: not a member name, -2: look it up). The VM asks once per new string,
/// and the answer is a lookup by its bytes: the member atoms cache's miss (a later read off the string is its hit).
unsafe extern "C-unwind" fn user_atom(_state: *mut ffi::lua_State, s: *const c_char, len: usize) -> i16 {
    let Some(reflection) = ATOM_REFLECTION.get() else { return -2 };
    crate::profiler::caches::miss(crate::profiler::caches::Cache::Atoms);
    let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    match reflection.name_id_bytes(bytes) {
        Some(id) if id < i16::MAX as NameId => id as i16,
        Some(_) => -2,
        None => -1,
    }
}

/// The name id of an atom-carrying string.
#[inline]
unsafe fn name_of(api: &Api, s: *const c_char, atom: c_int) -> Option<NameId> {
    if api.atoms && atom >= 0 {
        crate::profiler::caches::hit(crate::profiler::caches::Cache::Atoms);
        return Some(atom as NameId);
    }
    if api.atoms && atom == -1 {
        crate::profiler::caches::hit(crate::profiler::caches::Cache::Atoms);
        return None;
    }
    crate::profiler::caches::miss(crate::profiler::caches::Cache::Atoms);
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    api.engine().reflection.name_id_bytes(bytes)
}

/// The engine's raw API.
pub fn api(engine: &Engine) -> Option<Rc<Api>> {
    engine.lua.borrow().raw.clone()
}

/// Whether the instance has a live Luau userdata (and whether that userdata is anchored) (diagnostics:
/// `lua::holders`).
pub fn userdata_state(engine: &Engine, id: InstanceId) -> (bool, bool) {
    let Some(api) = api(engine) else { return (false, false) };
    let gc = api.gc.borrow();
    let index = id.index as usize;
    (gc.live.get(index).is_some_and(|&n| n > 0), gc.anchored.get(index).copied().unwrap_or(false))
}

/// Queues every tree outside the DataModel for the next collection and forgets the remembered holders, so each is
/// looked at again (diagnostics: `lua::holders`; a tree is otherwise looked at only when one of its userdata dies or
/// it leaves the DataModel). Returns the trees queued.
pub fn requeue_orphans(engine: &Engine) -> usize {
    let Some(api) = api(engine) else { return 0 };
    let state = engine.state.borrow();
    let mut gc = api.gc.borrow_mut();
    let game = state.collection.game;
    let mut queued = 0;
    for index in 0..state.dom.slot_count() as u32 {
        let Some(id) = state.dom.id_at_slot(index) else { continue };
        if state.dom.at(id).parent.is_none() && Some(id) != game {
            gc.orphans.push(id);
            queued += 1;
        }
    }
    gc.holders.clear();
    queued
}

/// The registry slots of the Instance API's tables, by name (diagnostics: `lua::holders`).
pub fn registry_names(engine: &Engine) -> Vec<(c_int, &'static str)> {
    let Some(api) = api(engine) else { return Vec::new() };
    let refs = &api.refs;
    vec![
        (refs.instances, "ree instance userdata cache"),
        (refs.anchors, "ree anchors of instances in the DataModel"),
        (refs.values, "ree datatype value cache"),
        (refs.enums, "ree enums"),
        (refs.enum_ids, "ree enum ids"),
        (refs.signals, "ree signal objects"),
        (refs.functions, "ree member functions"),
        (refs.slow_index, "ree slow index"),
        (refs.slow_newindex, "ree slow newindex"),
        (refs.resolve_call, "ree resolve call"),
        (refs.bridge, "ree bridge"),
    ]
}

fn registry_table(lua: &Lua, keys: &mut Vec<LuaRegistryKey>, mode: Option<&str>) -> LuaResult<(LuaTable, c_int)> {
    let table = lua.create_table()?;
    if let Some(mode) = mode {
        let meta = lua.create_table()?;
        meta.raw_set("__mode", mode)?;
        table.set_metatable(Some(meta))?;
    }
    let key = lua.create_registry_value(table.clone())?;
    let id = key.id();
    keys.push(key);
    Ok((table, id))
}

fn registry_function(lua: &Lua, keys: &mut Vec<LuaRegistryKey>, function: LuaFunction) -> LuaResult<c_int> {
    let key = lua.create_registry_value(function)?;
    let id = key.id();
    keys.push(key);
    Ok(id)
}

/// Creates the raw API's tables, fallback functions and bookkeeping (Engine.Start, before any
/// enum item, signal or instance reaches Luau). The metatable is installed by [`set_metatable`].
pub fn install(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    // (a first touch is priced by the instance's class: cost.rs COLD_TOUCH_GUI)
    engine.state.borrow().sched.touched.borrow_mut().set_prices(&engine.reflection);
    let mut keys = Vec::new();
    let (instances, instances_ref) = registry_table(lua, &mut keys, Some("v"))?;
    let (_anchors, anchors_ref) = registry_table(lua, &mut keys, None)?;
    let (values_table, values_ref) = registry_table(lua, &mut keys, None)?;
    let (enums, enums_ref) = registry_table(lua, &mut keys, None)?;
    let (enum_ids, enum_ids_ref) = registry_table(lua, &mut keys, None)?;
    let (signals, signals_ref) = registry_table(lua, &mut keys, None)?;
    let (functions, functions_ref) = registry_table(lua, &mut keys, None)?;
    let (bridge, bridge_ref) = registry_table(lua, &mut keys, None)?;

    // Fallbacks (mlua functions called from C): (value, message, level) / (message, level)
    let e = engine.clone();
    let slow_index = lua.create_function(move |lua, (this, key): (LuaValue, LuaValue)| {
        let Some(id) = instance_id(&this) else { return Err(LuaError::runtime("invalid Instance")) };
        match linstance::get(lua, &e, id, &key) {
            Ok(value) => Ok((value, LuaValue::Nil, 0)),
            Err(Flow::Error(error)) => Ok((LuaValue::Nil, LuaValue::String(lua.create_string(&error.message)?), error.level as i64)),
            Err(Flow::Fail(error)) => Err(error),
            Err(Flow::Yield) => Err(LuaError::runtime("attempt to yield across metamethod/C-call boundary")),
        }
    })?;
    let e = engine.clone();
    let slow_newindex = lua.create_function(move |lua, (this, key, value): (LuaValue, LuaValue, LuaValue)| {
        let Some(id) = instance_id(&this) else { return Err(LuaError::runtime("invalid Instance")) };
        match linstance::assign(lua, &e, id, &key, &value) {
            Ok(()) => Ok((LuaValue::Nil, 0)),
            Err(Flow::Error(error)) => Ok((LuaValue::String(lua.create_string(&error.message)?), error.level as i64)),
            Err(Flow::Fail(error)) => Err(error),
            Err(Flow::Yield) => Err(LuaError::runtime("attempt to yield across metamethod/C-call boundary")),
        }
    })?;
    let e = engine.clone();
    let resolve_call = lua.create_function(move |lua, (this, key): (LuaValue, LuaString)| {
        let Some(id) = instance_id(&this) else { return Err(LuaError::runtime("invalid Instance")) };
        let Some(name) = e.reflection.name_id_bytes(&key.as_bytes()) else { return Ok(LuaValue::Nil) };
        let class = e.state.borrow().dom.at(id).class;
        let function = linstance::call_function(lua, &e, class, name)?;
        if let (Some(function), Some(api)) = (&function, api(&e)) {
            let slot = api.next_function_slot();
            api.functions.raw_set(slot, function.clone())?;
            let mut member = api.member(class, name);
            member.call_fn = slot;
            api.members.borrow_mut().insert((class, name), member);
        }
        Ok(function.map_or(LuaValue::Nil, LuaValue::Function))
    })?;
    let refs = Refs {
        instances: instances_ref,
        anchors: anchors_ref,
        values: values_ref,
        enums: enums_ref,
        enum_ids: enum_ids_ref,
        signals: signals_ref,
        functions: functions_ref,
        slow_index: registry_function(lua, &mut keys, slow_index)?,
        slow_newindex: registry_function(lua, &mut keys, slow_newindex)?,
        resolve_call: registry_function(lua, &mut keys, resolve_call)?,
        bridge: bridge_ref,
    };

    let atoms = Arc::ptr_eq(ATOM_REFLECTION.get_or_init(|| engine.reflection.clone()), &engine.reflection);
    let moved: Rc<RefCell<Vec<Moved>>> = Rc::default();
    let api = Rc::new(Api {
        engine: Rc::as_ptr(engine),
        lua: lua.weak(),
        atoms,
        refs,
        instances,
        values_table,
        enums,
        enum_ids,
        signals,
        functions,
        bridge,
        create: RefCell::new(None),
        members: RefCell::default(),
        values: RefCell::default(),
        next_function: Cell::new(0),
        gc: RefCell::default(),
        moved: moved.clone(),
        work: engine.state.borrow().sched.work.clone(),
        muted: engine.state.borrow().sched.muted.clone(),
        touched: engine.state.borrow().sched.touched.clone(),
        _keys: keys,
    });

    // The userdata factory for Rust callers (instance_value), with the Api as upvalue
    let api_ptr = Rc::as_ptr(&api) as *mut c_void;
    let create: LuaFunction = unsafe {
        lua.exec_raw((), |state| {
            if atoms {
                (*ffi::lua_callbacks(state)).useratom = Some(user_atom);
            }
            ffi::lua_setuserdatadtor(state, INSTANCE_TAG, Some(instance_dtor));
            let counters = crate::cost::lua_workcounters(state);
            {
                let mut engine_state = engine.state.borrow_mut();
                engine_state.sched.vm_counters = Some(counters);
                crate::luau_profile::set_work_source(
                    engine_state.sched.work.clone(),
                    engine_state.sched.work_drained.clone(),
                    counters,
                );
            }
            ffi::lua_pushlightuserdata(state, api_ptr);
            ffi::lua_pushcclosurek(state, create_instance, c"Instance".as_ptr(), 1, None);
        })?
    };
    *api.create.borrow_mut() = Some(create);
    engine.lua.borrow_mut().raw = Some(api);
    // (the collector steps by the scripts' work, not by the host's speed: at the same points in every run)
    let counters = engine.state.borrow().sched.vm_counters;
    if let Some(counters) = counters {
        crate::cost::install_gc_clock(lua, counters)?;
    }
    crate::scheduler::start_cold_tracking(lua, engine);
    // (the engine's own C functions, mlua's callback among them: the objects they make for a script are priced with their operation)
    crate::cost::mark_host_functions(lua, &[meta_index, meta_newindex, meta_namecall, meta_tostring, create_instance, instance_new, table_namecall, typeof_c])?;

    // Parent changes that move a subtree into or out of the DataModel re-anchor its userdata
    engine.hooks.borrow_mut().hierarchy_changed.push(Box::new(move |state, id, old, new| {
        let game = state.collection.game;
        let was = old.is_some_and(|parent| rooted(state, parent, game));
        let now = new.is_some_and(|parent| rooted(state, parent, game));
        if was != now || new.is_none() {
            moved.borrow_mut().push(Moved { id, subtree: was != now, unparented: new.is_none() });
        }
    }));
    Ok(())
}

/// Installs the Instance metatable (`__type`, `__metatable`, `__iter` set by the caller) with the
/// C metamethods, after checking the Udata header layout `instance_of_ud` reads.
pub fn set_metatable(lua: &Lua, metatable: LuaTable) -> LuaResult<()> {
    let key = lua.create_registry_value(metatable)?;
    let id = key.id();
    let layout_ok: bool = unsafe {
        lua.exec_raw((), |state| {
            let probe = ffi::lua_newuserdatatagged(state, PAYLOAD_SIZE, PROBE_TAG) as *const u8;
            let ok = *probe.sub(TAG_OFFSET) as c_int == PROBE_TAG
                && std::ptr::read_unaligned(probe.sub(LEN_OFFSET) as *const i32) as usize == PAYLOAD_SIZE
                && ffi::lua_userdatatag(state, -1) == PROBE_TAG;
            ffi::lua_pop(state, 1);
            ffi::lua_rawgeti_(state, REGISTRY, id);
            ffi::lua_pushcclosurek(state, meta_index, c"__index".as_ptr(), 0, None);
            ffi::lua_setfield(state, -2, c"__index".as_ptr());
            ffi::lua_pushcclosurek(state, meta_newindex, c"__newindex".as_ptr(), 0, None);
            ffi::lua_setfield(state, -2, c"__newindex".as_ptr());
            ffi::lua_pushcclosurek(state, meta_namecall, c"__namecall".as_ptr(), 0, Some(namecall_continue));
            ffi::lua_setfield(state, -2, c"__namecall".as_ptr());
            ffi::lua_pushcclosurek(state, meta_tostring, c"__tostring".as_ptr(), 0, None);
            ffi::lua_setfield(state, -2, c"__tostring".as_ptr());
            ffi::lua_setuserdatametatable(state, INSTANCE_TAG);
            ffi::lua_pushboolean(state, ok as c_int);
        })?
    };
    lua.remove_registry_value(key)?;
    if !layout_ok {
        return Err(LuaError::runtime("engine: unexpected Luau Udata layout (lua/raw.rs)"));
    }
    Ok(())
}

// Rust-side conversions

/// The userdata of an instance (cached; created on first use).
pub fn instance_value(lua: &Lua, engine: &Engine, id: InstanceId) -> LuaResult<LuaValue> {
    let _ = lua;
    let api = api(engine).expect("raw Instance API not installed");
    if let LuaValue::UserData(ud) = api.instances.raw_get::<LuaValue>(id.index as i64 + 1)? {
        if instance_of_ud(&ud) == Some(id) {
            crate::profiler::caches::hit(crate::profiler::caches::Cache::Wrappers);
            return Ok(LuaValue::UserData(ud));
        }
    }
    crate::profiler::caches::miss(crate::profiler::caches::Cache::Wrappers);
    crate::profiler::caches::instance_touched(id.index, id.generation);
    let create = api.create.borrow().clone().expect("instance factory");
    create.call::<LuaValue>((id.index, id.generation))
}

/// Mirrors an enum item's userdata into the tables C reads.
pub fn remember_enum_item(engine: &Engine, item: u32, value: &LuaAnyUserData) -> LuaResult<()> {
    if let Some(api) = api(engine) {
        api.enums.raw_set(item as i64 + 1, value.clone())?;
        api.enum_ids.raw_set(value.clone(), item)?;
    }
    Ok(())
}

/// Mirrors a signal's userdata into the table C reads.
/// The userdata of a signal (nil: none yet); `None` without the raw API.
pub fn signal_userdata(engine: &Engine, signal: u32) -> LuaResult<Option<LuaValue>> {
    match api(engine) {
        Some(api) => Ok(Some(api.signals.raw_get(signal as i64 + 1)?)),
        None => Ok(None),
    }
}

pub fn remember_signal(engine: &Engine, signal: u32, value: &LuaAnyUserData) -> LuaResult<()> {
    if let Some(api) = api(engine) {
        api.signals.raw_set(signal as i64 + 1, value.clone())?;
    }
    Ok(())
}

/// Records the function `instance[key]` returns for a method (so C returns it directly).
pub fn remember_index_function(engine: &Engine, class: ClassId, name: NameId, function: &LuaFunction) -> LuaResult<()> {
    let Some(api) = api(engine) else { return Ok(()) };
    let mut member = api.member(class, name);
    if member.index_fn != 0 {
        return Ok(());
    }
    let slot = api.next_function_slot();
    api.functions.raw_set(slot, function.clone())?;
    member.index_fn = slot;
    api.members.borrow_mut().insert((class, name), member);
    Ok(())
}

/// A cached converted datatype value of (instance, property) at `version`.
pub fn cached_value(engine: &Engine, id: InstanceId, name: NameId, version: u32) -> LuaResult<Option<LuaValue>> {
    let Some(api) = api(engine) else { return Ok(None) };
    let slot = match api.values.borrow().map.get(&(id, name)) {
        Some(&(cached, slot)) if cached == version => slot,
        _ => return Ok(None),
    };
    Ok(Some(api.values_table.raw_get(slot)?))
}

pub fn cache_value(engine: &Engine, id: InstanceId, name: NameId, version: u32, value: &LuaValue) -> LuaResult<()> {
    let Some(api) = api(engine) else { return Ok(()) };
    let slot = api.value_slot((id, name), version);
    api.values_table.raw_set(slot, value.clone())
}

// C helpers

enum Out {
    /// Return this many values from the C function
    Ret(c_int),
    /// Raise a script error (level 1: prefixed with the calling line's position)
    Raise(String, u8),
    /// Take the mlua fallback
    Slow,
    /// (namecall) call the member's function
    General,
    /// (namecall) `instance[key](instance, ...)`
    NotMethod,
}

#[inline]
fn guard(body: impl FnOnce() -> Out) -> Out {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(out) => out,
        Err(panic) if super::refs::is_exhaustion(&*panic) => Out::Raise(super::refs::message(), 0),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            Out::Raise(format!("engine error: {message}"), 0)
        }
    }
}

/// Raises a string error. Every Rust value of the caller must be dropped already (the message is
/// dropped here before `lua_error` unwinds).
unsafe fn raise(state: *mut ffi::lua_State, message: String, level: u8) -> ! {
    unsafe {
        if level > 0 {
            ffi::luaL_where(state, 1);
        }
        ffi::lua_pushlstring_(state, message.as_ptr() as *const c_char, message.len());
        drop(message);
        if level > 0 {
            ffi::lua_concat(state, 2);
        }
        ffi::lua_error(state)
    }
}

/// Raises the message on top of the stack (with the calling line's position unless level is 0).
unsafe fn raise_top(state: *mut ffi::lua_State, level: c_int) -> ! {
    unsafe {
        if level != 0 {
            ffi::luaL_where(state, 1);
            ffi::lua_insert(state, -2);
            ffi::lua_concat(state, 2);
        }
        ffi::lua_error(state)
    }
}

#[inline]
unsafe fn this<'a>(state: *mut ffi::lua_State) -> Option<(&'a Api, InstanceId)> {
    unsafe {
        let payload = ffi::lua_touserdatatagged(state, 1, INSTANCE_TAG) as *const Payload;
        if payload.is_null() {
            return None;
        }
        let payload = &*payload;
        Some((&*payload.api, InstanceId { index: payload.index, generation: payload.generation }))
    }
}

#[inline]
/// The instance the stack value at `index` is, if any.
pub(super) unsafe fn stack_instance(state: *mut ffi::lua_State, index: c_int) -> Option<InstanceId> {
    unsafe { arg_instance(state, index) }
}

unsafe fn arg_instance(state: *mut ffi::lua_State, index: c_int) -> Option<InstanceId> {
    unsafe {
        let payload = ffi::lua_touserdatatagged(state, index, INSTANCE_TAG) as *const Payload;
        if payload.is_null() {
            return None;
        }
        Some(InstanceId { index: (*payload).index, generation: (*payload).generation })
    }
}

#[inline]
unsafe fn arg_bytes<'a>(state: *mut ffi::lua_State, index: c_int) -> Option<&'a [u8]> {
    unsafe {
        if ffi::lua_type(state, index) != ffi::LUA_TSTRING {
            return None;
        }
        let mut len = 0usize;
        let data = ffi::lua_tolstring(state, index, &mut len);
        Some(std::slice::from_raw_parts(data as *const u8, len))
    }
}

#[inline]
unsafe fn push_bytes(state: *mut ffi::lua_State, bytes: &[u8]) {
    unsafe { ffi::lua_pushlstring_(state, bytes.as_ptr() as *const c_char, bytes.len()) }
}

/// Whether an instance is in the DataModel (its root is the game).
fn rooted(state: &State, id: InstanceId, game: Option<InstanceId>) -> bool {
    let Some(game) = game else { return false };
    let mut current = id;
    loop {
        match state.dom.get(current).and_then(|i| i.parent) {
            Some(parent) => current = parent,
            None => return current == game,
        }
    }
}

/// Pushes the userdata of an instance (the cached one, or a new one).
unsafe fn push_instance(state: *mut ffi::lua_State, api: &Api, dom_state: Option<&State>, id: InstanceId) {
    unsafe {
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.instances);
        let cache = ffi::lua_gettop(state);
        push_instance_with(state, api, dom_state, id, cache);
        ffi::lua_remove(state, cache);
    }
}

/// `push_instance` with the userdata cache table at stack index `cache`.
#[inline]
unsafe fn push_instance_with(state: *mut ffi::lua_State, api: &Api, dom_state: Option<&State>, id: InstanceId, cache: c_int) {
    unsafe {
        let slot = id.index as c_int + 1;
        if ffi::lua_rawgeti_(state, cache, slot) == ffi::LUA_TUSERDATA {
            let payload = ffi::lua_touserdatatagged(state, -1, INSTANCE_TAG) as *const Payload;
            if !payload.is_null() && (*payload).generation == id.generation {
                crate::profiler::caches::hit(crate::profiler::caches::Cache::Wrappers);
                return;
            }
        }
        crate::profiler::caches::miss(crate::profiler::caches::Cache::Wrappers);
        crate::profiler::caches::instance_touched(id.index, id.generation);
        ffi::lua_pop(state, 1);
        let payload = ffi::lua_newuserdatataggedwithmetatable(state, PAYLOAD_SIZE, INSTANCE_TAG) as *mut Payload;
        let serial = match dom_state {
            Some(dom_state) => dom_state.dom.get(id).map_or(0, |instance| instance.serial),
            None => api.engine().state.try_borrow().ok().and_then(|s| s.dom.get(id).map(|instance| instance.serial)).unwrap_or(0),
        };
        payload.write(Payload { index: id.index, generation: id.generation, serial, api });
        ffi::lua_pushvalue(state, -1);
        // (the cache's growth is the engine's bookkeeping, not the script's: profiler::memory ENGINE_CACHE)
        crate::profiler::memory::bookkeeping(|| ffi::lua_rawseti_(state, cache, slot));
        api.note_created(id.index);
        // In the DataModel: anchored (identity holds while the instance is reachable)
        let in_game = match dom_state {
            Some(dom_state) => Some(rooted(dom_state, id, dom_state.collection.game)),
            None => api.engine().state.try_borrow().ok().map(|s| rooted(&s, id, s.collection.game)),
        };
        match in_game {
            Some(true) => {
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.anchors);
                ffi::lua_pushvalue(state, -2);
                ffi::lua_rawseti_(state, -2, slot);
                ffi::lua_pop(state, 1);
                api.gc.borrow_mut().anchored[id.index as usize] = true;
            }
            Some(false) => {}
            None => api.moved.borrow_mut().push(Moved { id, subtree: false, unparented: false }),
        }
    }
}

/// `Instance.new(className, parent)` (InstanceModule.New) as a C function.
unsafe extern "C-unwind" fn instance_new(state: *mut ffi::lua_State) -> c_int {
    crate::boundary::enter(state);
    // (host time: the call's time is its line's)
    let _time = unsafe { crate::luau_profile::ApiCall::new(state) };
    let out = guard(|| unsafe {
        let api = &*(ffi::lua_touserdata(state, ffi::lua_upvalueindex(1)) as *const Api);
        let engine = api.engine();
        let class_name = arg_bytes(state, 1).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        let parent = arg_instance(state, 2);
        let created = {
            let hooks = engine.hooks.borrow();
            let mut dom_state = engine.state.borrow_mut();
            core::new_instance(&mut dom_state, &engine.reflection, &hooks, &class_name, parent)
        };
        match created {
            Ok(id) => {
                {
                    let dom_state = engine.state.borrow();
                    let class = dom_state.dom.at(id).class;
                    let cost = crate::cost::created(&engine.reflection, class);
                    api.charge(cost);
                    api.touched.borrow().book_kinds_with(cost, || crate::cost::created_kinds(&engine.reflection, class));
                    if crate::boundary::active() && api.muted.get() == 0 {
                        crate::boundary::record_named(crate::boundary::Kind::Create, "Instance", &format!("new({})", engine.reflection.class(class).name), cost);
                    }
                    {
                        let mut touched = api.touched.borrow_mut();
                        touched.mark(id.index); // (an instance made this frame is hot)
                        touched.count(|ops| ops.created += 1);
                    }
                    push_instance(state, api, Some(&dom_state), id);
                }
                sync_if_moved(state, api);
                Out::Ret(1)
            }
            Err(error) => Out::Raise(error.message, error.level),
        }
    });
    match out {
        Out::Ret(count) => count,
        Out::Raise(message, level) => unsafe { raise(state, message, level) },
        _ => 0,
    }
}

/// The `Instance.new` function.
pub fn instance_new_function(lua: &Lua, engine: &Engine) -> LuaResult<LuaFunction> {
    let api = api(engine).expect("raw Instance API not installed");
    let api_ptr = Rc::as_ptr(&api) as *mut c_void;
    unsafe {
        lua.exec_raw((), |state| {
            ffi::lua_pushlightuserdata(state, api_ptr);
            ffi::lua_pushcclosurek(state, instance_new, c"new".as_ptr(), 1, None);
        })
    }
}

/// `create(index, generation)`: the userdata factory Rust callers use.
unsafe extern "C-unwind" fn create_instance(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        let api = &*(ffi::lua_touserdata(state, ffi::lua_upvalueindex(1)) as *const Api);
        let index = ffi::lua_tonumberx(state, 1, std::ptr::null_mut()) as u32;
        let generation = ffi::lua_tonumberx(state, 2, std::ptr::null_mut()) as u32;
        push_instance(state, api, None, InstanceId { index, generation });
        1
    }
}

unsafe extern "C" fn instance_dtor(_state: *mut ffi::lua_State, data: *mut c_void) {
    unsafe {
        let payload = &*(data as *const Payload);
        let api = &*payload.api;
        crate::profiler::caches::evicted(crate::profiler::caches::Cache::Wrappers, 1);
        if let Ok(mut gc) = api.gc.try_borrow_mut() {
            let index = payload.index as usize;
            if let Some(count) = gc.live.get_mut(index) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    gc.dead.push(payload.index);
                }
            }
        }
    }
}

/// Pushes a stored property value if C can (primitives, instances, mirrored enum items, cached
/// datatype values); `false` leaves the stack unchanged.
unsafe fn push_value(state: *mut ffi::lua_State, api: &Api, dom_state: &State, id: InstanceId, name: NameId, value: Option<&Value>) -> bool {
    unsafe {
        match value {
            None | Some(Value::Nil) => ffi::lua_pushnil(state),
            Some(Value::Bool(b)) => ffi::lua_pushboolean(state, *b as c_int),
            Some(Value::Number(n)) => ffi::lua_pushnumber(state, *n),
            Some(Value::String(s)) => push_bytes(state, s.as_bytes()),
            Some(Value::EnumItem(item)) => {
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.enums);
                let found = ffi::lua_rawgeti_(state, -1, *item as c_int + 1) != ffi::LUA_TNIL;
                ffi::lua_remove(state, -2);
                if !found {
                    ffi::lua_pop(state, 1);
                    return false;
                }
            }
            Some(Value::Instance(target)) => {
                if dom_state.dom.get(*target).is_some() {
                    push_instance(state, api, Some(dom_state), *target);
                } else {
                    ffi::lua_pushnil(state);
                }
            }
            // (a Vector3 is the native vector type)
            Some(Value::Vector3(v)) => ffi::lua_pushvector(state, v.x, v.y, v.z),
            Some(
                Value::UDim(_)
                | Value::UDim2(_)
                | Value::Vector2(_)
                | Value::Color3(_)
                | Value::Rect(_)
                | Value::NumberRange(_)
                | Value::NumberSequence(_)
                | Value::ColorSequence(_)
                | Value::Dom(_),
            ) => {
                let version = dom_state.dom.at(id).version;
                let slot = match api.values.borrow().map.get(&(id, name)) {
                    Some(&(cached, slot)) if cached == version => slot,
                    _ => {
                        crate::profiler::caches::miss(crate::profiler::caches::Cache::Values);
                        return false;
                    }
                };
                crate::profiler::caches::hit(crate::profiler::caches::Cache::Values);
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.values);
                ffi::lua_rawgeti_(state, -1, slot);
                ffi::lua_remove(state, -2);
            }
            Some(_) => return false,
        }
        true
    }
}

// __index

unsafe extern "C-unwind" fn meta_index(state: *mut ffi::lua_State) -> c_int {
    crate::boundary::enter(state);
    crate::boundary::access_begin(state);
    // (host time: the call's time is its line's)
    let _time = unsafe { crate::luau_profile::ApiCall::new(state) };
    let out = guard(|| unsafe { index_fast(state) });
    let count = match out {
        Out::Ret(count) => count,
        Out::Raise(message, level) => unsafe { raise(state, message, level) },
        _ => unsafe { index_slow(state) },
    };
    crate::boundary::access_end(state, false);
    count
}

/// What a read's value adds to its price (cost.rs `read_value`), charged and booked to the read's boundary crossing
fn read_extra(api: &Api, seconds: f64) {
    if seconds > 0.0 {
        api.charge(seconds);
        crate::boundary::add_cost(seconds);
    }
}

unsafe fn index_fast(state: *mut ffi::lua_State) -> Out {
    unsafe {
        let Some((api, id)) = this(state) else { return Out::Slow };
        let engine = api.engine();
        let (mut class, placeholder, serial) = {
            let dom_state = engine.state.borrow();
            let instance = dom_state.dom.at(id);
            (instance.class, instance.has(flags::BUNDLE_ROOT), instance.serial)
        };
        let cost = crate::cost::READ + api.touched.borrow_mut().touch(id.index, class, serial);
        api.charge(cost);
        let mut atom: c_int = 0;
        let key = ffi::lua_tostringatom(state, 2, &mut atom);
        if key.is_null() {
            return Out::Slow;
        }
        let name = name_of(api, key, atom);
        // (a bundle's placeholder a script holds becomes the bundle's instance before anything but
        // its name or parent, which are the instance's too, is read)
        if placeholder && name != Some(K::Name as NameId) && name != Some(K::Parent as NameId) {
            linstance::reveal_bundle(engine, id);
            class = engine.state.borrow().dom.at(id).class;
        }
        crate::cost::tally_member("read", &engine.reflection.class(class).name, || std::ffi::CStr::from_ptr(key).to_string_lossy().into_owned());
        let mut name = name;
        let mut read = match name {
            Some(name) => api.member(class, name),
            None => Member { read: Read::Child, write: Write::Slow, call: Call::NotMethod, index_fn: 0, call_fn: 0, cost: 0.0, name: 0 },
        };
        // A key that names no member and no child reads the member it names with its first letter upper-cased
        // (`part.anchored`, instance.rs `get`): only after the child lookup has missed, so exact names never pay for it
        let mut child = None;
        let mut lower_first = 0.0;
        if matches!(read.read, Read::Child) {
            let bytes = arg_bytes(state, 2).unwrap_or_default();
            materialize(engine, id);
            child = engine.state.borrow().dom.find_child(id, bytes);
            if child.is_none()
                && let Some(target) = engine.reflection.upper_first_id(bytes)
            {
                let member = api.member(class, target);
                if !matches!(member.read, Read::Child) {
                    // (the child lookup that missed is the engine's code too: its first use in the frame is charged)
                    lower_first = crate::cost::LOWER_FIRST + api.touched.borrow_mut().first_use(crate::cost::MemberUse::Read, crate::cost::CHILD_LOOKUP);
                    api.charge(lower_first);
                    name = Some(target);
                    read = member;
                }
            }
        }
        // (the engine's code for the member, cold at the frame's first use: cost.rs FIRST_READ)
        let used = match name {
            Some(name) if !matches!(read.read, Read::Child) => name,
            _ => crate::cost::CHILD_LOOKUP,
        };
        let first = api.touched.borrow_mut().first_use(crate::cost::MemberUse::Read, used);
        if first > 0.0 {
            api.charge(first);
        }
        api.cross(crate::boundary::Kind::Get, class, name, matches!(read.read, Read::Child), id, cost + lower_first + first, true);
        match read.read {
            Read::Plain => {
                let name = read.name;
                let dom_state = engine.state.borrow();
                let value = dom_state.dom.raw_get(id, name);
                read_extra(api, crate::cost::read_value(value));
                if push_value(state, api, &dom_state, id, name, value) { Out::Ret(1) } else { Out::Slow }
            }
            Read::Name => {
                read_extra(api, crate::cost::READ_STRING_EXTRA);
                let dom_state = engine.state.borrow();
                let instance = dom_state.dom.at(id);
                match instance.stored(K::Name as NameId) {
                    Some(Value::String(name)) => push_bytes(state, name.as_bytes()),
                    Some(_) => push_bytes(state, b""),
                    // the class's default name, which is not always the class's own (a BoolValue is "Value", a
                    // BindableEvent "Event", a WedgePart "Wedge": Studio's defaults in data/reflection.json; measured in a
                    // playing client for BindableEvent, tools/parity/probes/instances.luau)
                    None => push_bytes(state, instance.name.as_bytes()),
                }
                Out::Ret(1)
            }
            Read::Parent => {
                let dom_state = engine.state.borrow();
                match dom_state.dom.at(id).parent {
                    Some(parent) => {
                        read_extra(api, crate::cost::READ_INSTANCE_EXTRA);
                        push_instance(state, api, Some(&dom_state), parent)
                    }
                    None => ffi::lua_pushnil(state),
                }
                Out::Ret(1)
            }
            Read::ClassName => {
                read_extra(api, crate::cost::READ_STRING_EXTRA);
                push_bytes(state, engine.reflection.class(class).name.as_bytes());
                Out::Ret(1)
            }
            Read::Method => {
                if read.index_fn == 0 {
                    return Out::Slow;
                }
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.functions);
                ffi::lua_rawgeti_(state, -1, read.index_fn);
                Out::Ret(1)
            }
            Read::Event => {
                let signal = core::event_signal(&mut engine.state.borrow_mut(), &engine.reflection, id, name.unwrap_or_default());
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.signals);
                if ffi::lua_rawgeti_(state, -1, signal as c_int + 1) == ffi::LUA_TNIL {
                    ffi::lua_pop(state, 2);
                    return Out::Slow;
                }
                Out::Ret(1)
            }
            Read::Child => {
                let Some(child) = child else { return Out::Slow };
                read_extra(api, crate::cost::READ_CHILD_EXTRA);
                linstance::reached(engine, child);
                let dom_state = engine.state.borrow();
                push_instance(state, api, Some(&dom_state), child);
                Out::Ret(1)
            }
            Read::Slow => Out::Slow,
        }
    }
}

unsafe fn index_slow(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        let Some((api, _)) = this(state) else { raise(state, "invalid Instance".into(), 0) };
        ffi::lua_settop(state, 2);
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.slow_index);
        ffi::lua_pushvalue(state, 1);
        ffi::lua_pushvalue(state, 2);
        ffi::lua_call(state, 2, 3);
        if ffi::lua_type(state, -2) == ffi::LUA_TNIL {
            ffi::lua_pop(state, 2);
            return 1;
        }
        let level = ffi::lua_tonumberx(state, -1, std::ptr::null_mut()) as c_int;
        ffi::lua_pop(state, 1);
        raise_top(state, level)
    }
}

// __newindex

unsafe extern "C-unwind" fn meta_newindex(state: *mut ffi::lua_State) -> c_int {
    crate::trace::enter(state);
    crate::boundary::enter(state);
    crate::boundary::access_begin(state);
    // (host time: the call's time is its line's)
    let _time = unsafe { crate::luau_profile::ApiCall::new(state) };
    let out = guard(|| unsafe { newindex_fast(state) });
    match out {
        Out::Ret(_) => {}
        Out::Raise(message, level) => unsafe { raise(state, message, level) },
        _ => unsafe { newindex_slow(state) },
    }
    unsafe {
        if let Some((api, _)) = this(state) {
            sync_if_moved(state, api);
        }
    }
    crate::boundary::access_end(state, true);
    0
}

unsafe fn newindex_fast(state: *mut ffi::lua_State) -> Out {
    unsafe {
        let Some((api, id)) = this(state) else { return Out::Slow };
        let mut atom: c_int = 0;
        let key = ffi::lua_tostringatom(state, 2, &mut atom);
        if key.is_null() {
            return Out::Slow;
        }
        let Some(name) = name_of(api, key, atom) else { return Out::Slow };
        let engine = api.engine();
        let (mut class, placeholder) = {
            let dom_state = engine.state.borrow();
            let instance = dom_state.dom.at(id);
            (instance.class, instance.has(flags::BUNDLE_ROOT))
        };
        if placeholder {
            linstance::reveal_bundle(engine, id);
            class = engine.state.borrow().dom.at(id).class;
        }
        let member = api.member(class, name);
        let kind = ffi::lua_type(state, 3);
        match member.write {
            Write::Plain(property) => {
                let info = engine.reflection.property(property);
                let value = match (info.script_type, kind) {
                    (ScriptType::Number, ffi::LUA_TNUMBER) => {
                        let number = ffi::lua_tonumberx(state, 3, std::ptr::null_mut());
                        Value::Number(super::convert::number_for(engine, info, number))
                    }
                    (ScriptType::Boolean, ffi::LUA_TBOOLEAN) => Value::Bool(ffi::lua_toboolean(state, 3) != 0),
                    (ScriptType::Boolean, ffi::LUA_TNIL) => Value::Bool(false),
                    (ScriptType::String, ffi::LUA_TSTRING) => Value::String(Str::new(arg_bytes(state, 3).unwrap_or_default())),
                    (ScriptType::Instance, ffi::LUA_TNIL) => Value::Nil,
                    (ScriptType::Instance, ffi::LUA_TUSERDATA) => match arg_instance(state, 3) {
                        Some(target) => Value::Instance(target),
                        None => return Out::Slow,
                    },
                    (ScriptType::EnumItem, ffi::LUA_TUSERDATA) if info.enum_name.is_some() => {
                        ffi::lua_rawgeti_(state, REGISTRY, api.refs.enum_ids);
                        ffi::lua_pushvalue(state, 3);
                        let found = ffi::lua_rawget(state, -2) == ffi::LUA_TNUMBER;
                        let item = ffi::lua_tonumberx(state, -1, std::ptr::null_mut()) as u32;
                        ffi::lua_pop(state, 2);
                        if !found || Some(engine.reflection.enum_item_info(item).enum_type) != info.enum_type {
                            return Out::Slow;
                        }
                        Value::EnumItem(item)
                    }
                    // Datatypes, strings for numbers / enums, ...: Coerce on the value as mlua
                    // sees it (its errors are left to the fallback)
                    _ => match bridged_coerce(state, api, engine, info, 3) {
                        Some(value) => value,
                        None => return Out::Slow,
                    },
                };
                linstance::script_set_property(engine, id, member.name, value);
                Out::Ret(0)
            }
            Write::Parent => {
                // (a subtree entering the DataModel starts the scripts under Actors in it: the slow path runs them)
                if super::actors::any_actor(engine) {
                    return Out::Slow;
                }
                let parent = match kind {
                    ffi::LUA_TNIL => None,
                    _ => match arg_instance(state, 3) {
                        Some(parent) => Some(parent),
                        None => return Out::Slow,
                    },
                };
                // (priced by the reparenting itself: instance.rs set_parent adds its charge to this crossing; a write of the
                // parent it has changes nothing)
                let changed = engine.state.borrow().dom.get(id).is_none_or(|instance| instance.parent != parent);
                api.cross(crate::boundary::Kind::Set, class, Some(K::Parent as NameId), false, id, 0.0, changed);
                match linstance::set_parent(engine, id, parent) {
                    Ok(()) => Out::Ret(0),
                    Err(Flow::Error(error)) => Out::Raise(error.message, error.level),
                    Err(_) => Out::Slow,
                }
            }
            Write::Name => {
                let Some(bytes) = arg_bytes(state, 3) else { return Out::Slow };
                linstance::script_set_property(engine, id, K::Name as NameId, Value::String(Str::new(bytes)));
                Out::Ret(0)
            }
            Write::Slow => Out::Slow,
        }
    }
}

/// Coerce of the value at `index` (passed to Rust through the bridge table); `None` when it
/// fails (the fallback raises the error).
unsafe fn bridged_coerce(
    state: *mut ffi::lua_State,
    api: &Api,
    engine: &Engine,
    info: &crate::reflection::PropertyInfo,
    index: c_int,
) -> Option<Value> {
    unsafe {
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.bridge);
        ffi::lua_pushvalue(state, index);
        ffi::lua_rawseti_(state, -2, 1);
        ffi::lua_pop(state, 1);
    }
    let lua = api.lua.try_upgrade()?;
    let value: LuaValue = api.bridge.raw_get(1).ok()?;
    let coerced = super::convert::coerce(&lua, engine, info, &value).ok();
    let _ = api.bridge.raw_set(1, LuaValue::Nil);
    coerced
}

unsafe fn newindex_slow(state: *mut ffi::lua_State) {
    unsafe {
        let Some((api, _)) = this(state) else { raise(state, "invalid Instance".into(), 0) };
        ffi::lua_settop(state, 3);
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.slow_newindex);
        ffi::lua_pushvalue(state, 1);
        ffi::lua_pushvalue(state, 2);
        ffi::lua_pushvalue(state, 3);
        ffi::lua_call(state, 3, 2);
        if ffi::lua_type(state, -2) == ffi::LUA_TNIL {
            ffi::lua_pop(state, 2);
            return;
        }
        let level = ffi::lua_tonumberx(state, -1, std::ptr::null_mut()) as c_int;
        ffi::lua_pop(state, 1);
        raise_top(state, level)
    }
}

// __namecall

unsafe extern "C-unwind" fn meta_namecall(state: *mut ffi::lua_State) -> c_int {
    crate::trace::enter(state);
    crate::boundary::enter(state);
    // (host time: the call's time is its line's)
    let _time = unsafe { crate::luau_profile::ApiCall::new(state) };
    let out = guard(|| unsafe { namecall_fast(state) });
    match out {
        Out::Ret(count) => count,
        Out::Raise(message, level) => unsafe { raise(state, message, level) },
        Out::General => unsafe { namecall_general(state) },
        Out::NotMethod | Out::Slow => unsafe { namecall_index(state) },
    }
}

/// After a yield: the results are the stack. A Luau method that yielded inside its call leaves them
/// above the frame's top (the VM lowered it to the call's base when it yielded), so the frame grows
/// over them first, as Luau's own continuations do (luaB_pcallcont); reading them otherwise fails
/// the VM's stack checks, which are compiled in (cc defines no NDEBUG) and break into a debugger.
unsafe extern "C-unwind" fn namecall_continue(state: *mut ffi::lua_State, _status: c_int) -> c_int {
    unsafe {
        ffi::lua_rawcheckstack(state, 1);
        namecall_finish(state, None)
    }
}

/// The member's function (made by the `resolve_call` fallback the first time), called with the
/// arguments; protocol results raise or yield from here.
unsafe fn namecall_general(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        let Some((api, id)) = this(state) else { raise(state, "invalid Instance".into(), 0) };
        let mut atom: c_int = 0;
        let key = ffi::lua_namecallatom(state, &mut atom);
        if key.is_null() {
            raise(state, "invalid namecall".into(), 0)
        }
        // (the method namecall_fast found: by the key, or by the key with its first letter upper-cased)
        let class = api.engine().state.borrow().dom.at(id).class;
        let (name, slot) = match method_member(api, class, key, name_of(api, key, atom)) {
            Some((name, member, _)) => (Some(name), member.call_fn),
            None => (None, 0),
        };
        if slot != 0 {
            ffi::lua_rawgeti_(state, REGISTRY, api.refs.functions);
            ffi::lua_rawgeti_(state, -1, slot);
            ffi::lua_remove(state, -2);
        } else {
            ffi::lua_rawgeti_(state, REGISTRY, api.refs.resolve_call);
            ffi::lua_pushvalue(state, 1);
            match name {
                Some(name) => push_bytes(state, api.engine().reflection.name(name).as_bytes()),
                None => ffi::lua_pushstring_(state, key),
            }
            ffi::lua_call(state, 2, 1);
            if ffi::lua_type(state, -1) == ffi::LUA_TNIL {
                ffi::lua_pop(state, 1);
                return namecall_index(state);
            }
        }
        ffi::lua_insert(state, 1);
        let count = ffi::lua_gettop(state) - 1;
        ffi::lua_call(state, count, ffi::LUA_MULTRET);
        if ffi::lua_status(state) != ffi::LUA_OK {
            // the Luau method yielded: namecall_continue finishes when it returns
            return -1;
        }
        namecall_finish(state, Some(api))
    }
}

/// Results of a member function (the whole stack): protocol errors and yields, else returned.
unsafe fn namecall_finish(state: *mut ffi::lua_State, api: Option<&Api>) -> c_int {
    unsafe {
        let count = ffi::lua_gettop(state);
        if count >= 1
            && ffi::lua_type(state, 1) == ffi::LUA_TLIGHTUSERDATA
            && ffi::lua_tolightuserdata(state, 1) == super::signal_marker_ptr()
        {
            let kind = ffi::lua_tonumberx(state, 2, std::ptr::null_mut()) as c_int;
            if kind == 1 {
                let level = ffi::lua_tonumberx(state, 4, std::ptr::null_mut()) as c_int;
                ffi::lua_settop(state, 3);
                raise_top(state, level)
            }
            ffi::lua_settop(state, 0);
            if let Some(api) = api {
                sync_if_moved(state, api);
            }
            return ffi::lua_yield(state, 0);
        }
        if let Some(api) = api {
            sync_if_moved(state, api);
        }
        count
    }
}

/// A method call whose key names no method of the instance, by its own name or with its first letter upper-cased: a client
/// raises `<key> is not a valid member of <Class> "<full name>"` for a property, an event, a callback or a child as for an
/// unknown key (measured: `part:Anchored()`, `part:touched()`, a child's name; tools/parity/probes/camelcase.luau).
unsafe fn namecall_index(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        let Some((api, id)) = this(state) else { raise(state, "invalid Instance".into(), 0) };
        let key = ffi::lua_namecallatom(state, std::ptr::null_mut());
        if key.is_null() {
            raise(state, "invalid namecall".into(), 0)
        }
        let engine = api.engine();
        if engine.state.borrow().dom.at(id).has(flags::BUNDLE_ROOT) {
            linstance::reveal_bundle(engine, id);
        }
        let message = linstance::not_a_member_message(engine, id, std::ffi::CStr::from_ptr(key).to_bytes());
        raise(state, message, 0)
    }
}

/// The member a method call `instance:key(...)` runs: the exact name's, else (a key that names no method of the class) the
/// method the key names with its first letter upper-cased (`part:getPropertyChangedSignal`, docs/engine.md "Member names"),
/// with what finding it that way adds to the call's price (cost.rs LOWER_FIRST); else the exact name's member that is no
/// method, if any.
unsafe fn method_member(api: &Api, class: ClassId, key: *const c_char, exact: Option<NameId>) -> Option<(NameId, Member, f64)> {
    let exact = exact.map(|name| (name, api.member(class, name)));
    if let Some((name, member)) = exact
        && !matches!(member.call, Call::NotMethod)
    {
        return Some((name, member, 0.0));
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(key) }.to_bytes();
    if let Some(target) = api.engine().reflection.upper_first_id(bytes) {
        let member = api.member(class, target);
        if !matches!(member.call, Call::NotMethod) {
            return Some((target, member, crate::cost::LOWER_FIRST));
        }
    }
    exact.map(|(name, member)| (name, member, 0.0))
}

unsafe fn namecall_fast(state: *mut ffi::lua_State) -> Out {
    unsafe {
        let Some((api, id)) = this(state) else { return Out::Raise("invalid Instance".into(), 0) };
        let mut atom: c_int = 0;
        let key = ffi::lua_namecallatom(state, &mut atom);
        if key.is_null() {
            return Out::Raise("invalid namecall".into(), 0);
        }
        let exact = name_of(api, key, atom);
        if exact.is_none() && !(*key as u8).is_ascii_lowercase() {
            return Out::NotMethod;
        }
        let engine = api.engine();
        let (mut class, placeholder) = {
            let dom_state = engine.state.borrow();
            let instance = dom_state.dom.at(id);
            (instance.class, instance.has(flags::BUNDLE_ROOT))
        };
        if placeholder {
            linstance::reveal_bundle(engine, id);
            class = engine.state.borrow().dom.at(id).class;
        }
        let Some((name, member, lower_first)) = method_member(api, class, key, exact) else { return Out::NotMethod };
        crate::cost::tally_member("call", &engine.reflection.class(class).name, || engine.reflection.name(name).to_string());
        let serial = engine.state.borrow().dom.at(id).serial;
        let cost = {
            let mut touched = api.touched.borrow_mut();
            // (where the method's own price came from: cost.rs method_kinds)
            touched.book_kinds_with(member.cost as f64, || crate::cost::method_kinds(engine.reflection.name(name)));
            // (the method's code, cold at its first call in the frame: cost.rs FIRST_CALL)
            member.cost as f64 + touched.touch(id.index, class, serial) + touched.first_use(crate::cost::MemberUse::Call, name) + lower_first
        };
        // (PivotTo: the frame's first one, and the parts it carries and what hangs on them that the frame had not touched: cost.rs
        // Path::Pivot, priced here so the engine's Luau body is unchanged)
        let cost = if api.touched.borrow().pivot_to == Some(name) { cost + linstance::pivot_cold(engine, id) } else { cost };
        api.charge(cost);
        api.cross(crate::boundary::Kind::Method, class, Some(name), false, id, cost, true);
        match member.call {
            Call::Base(base) => base_call(state, api, engine, id, class, base),
            Call::Function => Out::General,
            Call::NotMethod => Out::NotMethod,
        }
    }
}

unsafe fn push_optional_instance(state: *mut ffi::lua_State, api: &Api, engine: &Engine, id: Option<InstanceId>) -> Out {
    unsafe {
        match id {
            Some(id) => {
                let dom_state = engine.state.borrow();
                push_instance(state, api, Some(&dom_state), id);
            }
            None => ffi::lua_pushnil(state),
        }
        Out::Ret(1)
    }
}

unsafe fn push_instance_list(state: *mut ffi::lua_State, api: &Api, dom_state: &State, ids: &[InstanceId]) -> Out {
    unsafe {
        api.charge(ids.len() as f64 * crate::cost::LISTED);
        crate::boundary::add_cost(ids.len() as f64 * crate::cost::LISTED);
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.instances);
        let cache = ffi::lua_gettop(state);
        ffi::lua_createtable(state, ids.len() as c_int, 0);
        for (index, &child) in ids.iter().enumerate() {
            push_instance_with(state, api, Some(dom_state), child, cache);
            ffi::lua_rawseti_(state, -2, index as c_int + 1);
        }
        ffi::lua_remove(state, cache);
        Out::Ret(1)
    }
}

fn class_name_is(engine: &Engine, class: ClassId, wanted: Option<&[u8]>) -> bool {
    wanted == Some(engine.reflection.class(class).name.as_bytes())
}

fn is_a_bytes(engine: &Engine, class: ClassId, wanted: Option<&[u8]>) -> bool {
    match wanted.and_then(|w| std::str::from_utf8(w).ok()) {
        Some(wanted) => engine.reflection.is_a(class, wanted),
        None => false,
    }
}

/// The base Instance methods inline (same semantics as lua/instance.rs install_base).
/// The bytes of the string at `index` (nothing for any other type: a number would be converted in place).
#[inline]
unsafe fn stack_string<'a>(state: *mut ffi::lua_State, index: c_int) -> Option<&'a [u8]> {
    unsafe {
        if ffi::lua_type(state, index) != ffi::LUA_TSTRING {
            return None;
        }
        let mut length = 0usize;
        let data = ffi::lua_tolstring(state, index, &mut length);
        if data.is_null() { None } else { Some(std::slice::from_raw_parts(data as *const u8, length)) }
    }
}

/// The three components of the native vector at `index`.
#[inline]
unsafe fn stack_vector(state: *mut ffi::lua_State, index: c_int) -> Option<glam::Vec3> {
    unsafe {
        let components = ffi::lua_tovector(state, index);
        if components.is_null() { None } else { Some(glam::Vec3::new(*components, *components.add(1), *components.add(2))) }
    }
}

/// Pushes the metatable of a RaycastResult (made once, kept in the bridge table's slot 2); false: none made.
unsafe fn push_result_meta(state: *mut ffi::lua_State, api: &Api) -> bool {
    unsafe {
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.bridge);
        if ffi::lua_rawgeti_(state, -1, 2) != ffi::LUA_TTABLE {
            ffi::lua_pop(state, 2);
            let Some(lua) = api.lua.try_upgrade() else { return false };
            let Ok(meta) = lua.create_table() else { return false };
            if meta.set("__type", "RaycastResult").is_err() || meta.set("__metatable", "The metatable is locked").is_err() || api.bridge.raw_set(2, meta).is_err() {
                return false;
            }
            ffi::lua_rawgeti_(state, REGISTRY, api.refs.bridge);
            ffi::lua_rawgeti_(state, -1, 2);
        }
        ffi::lua_remove(state, -2);
        true
    }
}

unsafe fn base_call(state: *mut ffi::lua_State, api: &Api, engine: &Engine, id: InstanceId, class: ClassId, base: Base) -> Out {
    unsafe {
        match base {
            Base::FindFirstChild => {
                // (a missing name raises in the behaviour: `Argument 1 missing or nil`)
                if ffi::lua_type(state, 2) <= ffi::LUA_TNIL {
                    return Out::General;
                }
                let (recursive, mut looked) = (ffi::lua_toboolean(state, 3) != 0, 0);
                let found = linstance::find_first_child_counting(engine, id, arg_bytes(state, 2), recursive, &mut looked);
                linstance::charge_find(engine, looked, recursive);
                push_optional_instance(state, api, engine, found)
            }
            Base::IsA => {
                ffi::lua_pushboolean(state, is_a_bytes(engine, class, arg_bytes(state, 2)) as c_int);
                Out::Ret(1)
            }
            Base::GetChildren => {
                materialize(engine, id);
                let dom_state = engine.state.borrow();
                let instance = dom_state.dom.at(id);
                let key = (id, CHILDREN_KEY);
                let cached = match api.values.borrow().map.get(&key) {
                    Some(&(version, slot)) if version == instance.child_version => Some(slot),
                    _ => None,
                };
                if let Some(slot) = cached {
                    api.charge(instance.children.len() as f64 * crate::cost::LISTED);
                    crate::boundary::add_cost(instance.children.len() as f64 * crate::cost::LISTED);
                    ffi::lua_rawgeti_(state, REGISTRY, api.refs.values);
                    ffi::lua_rawgeti_(state, -1, slot);
                    lua_clonetable(state, -1);
                    return Out::Ret(1);
                }
                push_instance_list(state, api, &dom_state, &instance.children);
                // Children lists of instances in the DataModel are kept (their children are
                // anchored anyway) and cloned while the list is unchanged
                if !instance.children.is_empty() && rooted(&dom_state, id, dom_state.collection.game) {
                    let slot = api.value_slot(key, instance.child_version);
                    ffi::lua_rawgeti_(state, REGISTRY, api.refs.values);
                    ffi::lua_pushvalue(state, -2);
                    ffi::lua_rawseti_(state, -2, slot);
                    ffi::lua_pop(state, 1);
                    lua_clonetable(state, -1);
                }
                Out::Ret(1)
            }
            Base::GetDescendants => {
                crate::project::materialize_deep(engine, id);
                let dom_state = engine.state.borrow();
                let descendants = dom_state.dom.descendants(id);
                // (the frame's first GetDescendants runs its code cold: cost.rs Path::Descendants)
                let first = {
                    let mut touched = api.touched.borrow_mut();
                    touched.count(|ops| ops.queries += 1);
                    touched.first_path(crate::cost::Path::Descendants)
                };
                if first > 0.0 {
                    api.charge(first);
                    crate::boundary::add_cost(first);
                }
                push_instance_list(state, api, &dom_state, &descendants)
            }
            Base::FindFirstChildOfClass => {
                materialize(engine, id);
                let wanted = arg_bytes(state, 2);
                let found = linstance::find_child_where(engine, id, |dom_state, child| class_name_is(engine, dom_state.dom.at(child).class, wanted));
                push_optional_instance(state, api, engine, found)
            }
            Base::FindFirstChildWhichIsA => {
                let found = find_first_child_which_is_a(engine, id, arg_bytes(state, 2), ffi::lua_toboolean(state, 3) != 0);
                push_optional_instance(state, api, engine, found)
            }
            Base::FindFirstAncestor => {
                let wanted = arg_bytes(state, 2);
                let found = {
                    let dom_state = engine.state.borrow();
                    let mut current = dom_state.dom.at(id).parent;
                    while let Some(node) = current {
                        if wanted == Some(dom_state.dom.at(node).name.as_bytes()) {
                            break;
                        }
                        current = dom_state.dom.at(node).parent;
                    }
                    current
                };
                push_optional_instance(state, api, engine, found)
            }
            Base::FindFirstAncestorOfClass => {
                let wanted = arg_bytes(state, 2);
                let found = {
                    let dom_state = engine.state.borrow();
                    let mut current = dom_state.dom.at(id).parent;
                    while let Some(node) = current {
                        if class_name_is(engine, dom_state.dom.at(node).class, wanted) {
                            break;
                        }
                        current = dom_state.dom.at(node).parent;
                    }
                    current
                };
                push_optional_instance(state, api, engine, found)
            }
            Base::FindFirstAncestorWhichIsA => {
                let wanted = arg_bytes(state, 2);
                let found = {
                    let dom_state = engine.state.borrow();
                    let mut current = dom_state.dom.at(id).parent;
                    while let Some(node) = current {
                        if is_a_bytes(engine, dom_state.dom.at(node).class, wanted) {
                            break;
                        }
                        current = dom_state.dom.at(node).parent;
                    }
                    current
                };
                push_optional_instance(state, api, engine, found)
            }
            Base::IsDescendantOf => {
                let result = match arg_instance(state, 2) {
                    Some(ancestor) => engine.state.borrow().dom.is_descendant_of(id, ancestor),
                    // (every instance is a descendant of nil, as in Roblox: lua/instance.rs)
                    None => ffi::lua_type(state, 2) <= ffi::LUA_TNIL,
                };
                ffi::lua_pushboolean(state, result as c_int);
                Out::Ret(1)
            }
            Base::IsAncestorOf => {
                let result = match arg_instance(state, 2) {
                    Some(descendant) => engine.state.borrow().dom.is_descendant_of(descendant, id),
                    None => false,
                };
                ffi::lua_pushboolean(state, result as c_int);
                Out::Ret(1)
            }
            Base::GetPropertyChangedSignal => {
                api.charge(crate::cost::PROPERTY_SIGNAL);
                let Some(name) = arg_bytes(state, 2).and_then(|b| engine.reflection.name_id_bytes(b)) else {
                    return Out::General;
                };
                if !matches!(engine.resolve_member(class, name), Resolved::Property { .. }) {
                    return Out::General;
                }
                let signal = core::property_signal(&mut engine.state.borrow_mut(), &engine.reflection, id, name);
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.signals);
                if ffi::lua_rawgeti_(state, -1, signal as c_int + 1) == ffi::LUA_TNIL {
                    ffi::lua_pop(state, 2);
                    return Out::General;
                }
                Out::Ret(1)
            }
            Base::GetAttribute => {
                let Some(name) = arg_bytes(state, 2) else {
                    // (a number names the attribute of its text, as SetAttribute takes it: the behaviour)
                    if ffi::lua_type(state, 2) == ffi::LUA_TNUMBER {
                        return Out::General;
                    }
                    ffi::lua_pushnil(state);
                    return Out::Ret(1);
                };
                let dom_state = engine.state.borrow();
                let value = dom_state.dom.at(id).attribute(name);
                match value {
                    None | Some(Value::Nil | Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
                        push_value(state, api, &dom_state, id, 0, value);
                        Out::Ret(1)
                    }
                    Some(_) => Out::General,
                }
            }
            Base::SetAttribute => {
                // Primitive values under a string name (others: typeof checks, coercion)
                let Some(name) = arg_bytes(state, 2) else { return Out::General };
                // (a name Roblox refuses raises in the behaviour: lua/instance.rs attribute_name_error)
                if linstance::attribute_name_error(name).is_some() {
                    return Out::General;
                }
                let value = match ffi::lua_type(state, 3) {
                    ffi::LUA_TNIL | ffi::LUA_TNONE => Value::Nil,
                    ffi::LUA_TBOOLEAN => Value::Bool(ffi::lua_toboolean(state, 3) != 0),
                    ffi::LUA_TNUMBER => Value::Number(ffi::lua_tonumberx(state, 3, std::ptr::null_mut())),
                    ffi::LUA_TSTRING => Value::String(Str::new(arg_bytes(state, 3).unwrap_or_default())),
                    _ => return Out::General,
                };
                linstance::set_attribute(engine, id, Str::new(name), value);
                Out::Ret(0)
            }
            Base::FireBindable => {
                // Primitive and instance arguments (others: the behaviour's from_lua conversion)
                let count = ffi::lua_gettop(state);
                let mut arguments = Vec::with_capacity(count.max(1) as usize - 1);
                for index in 2..=count {
                    arguments.push(match ffi::lua_type(state, index) {
                        ffi::LUA_TNIL => Value::Nil,
                        ffi::LUA_TBOOLEAN => Value::Bool(ffi::lua_toboolean(state, index) != 0),
                        ffi::LUA_TNUMBER => Value::Number(ffi::lua_tonumberx(state, index, std::ptr::null_mut())),
                        ffi::LUA_TSTRING => Value::String(Str::new(arg_bytes(state, index).unwrap_or_default())),
                        ffi::LUA_TUSERDATA => match arg_instance(state, index) {
                            Some(target) => Value::Instance(target),
                            None => return Out::General,
                        },
                        _ => return Out::General,
                    });
                }
                core::fire_event_measured(&mut engine.state.borrow_mut(), id, K::Event as NameId, arguments, crate::cost::SignalKind::Bindable);
                Out::Ret(0)
            }
            Base::GroupsCollide => {
                let (Some(first), Some(second)) = (stack_string(state, 2), stack_string(state, 3)) else { return Out::General };
                let groups = crate::world::query::collision_groups(engine).borrow();
                let (Some(a), Some(b)) = (groups.id(first), groups.id(second)) else { return Out::General };
                ffi::lua_pushboolean(state, groups.collidable(a, b) as c_int);
                Out::Ret(1)
            }
            Base::Raycast => {
                let (Some(origin), Some(direction)) = (stack_vector(state, 2), stack_vector(state, 3)) else { return Out::General };
                let Some(method) = engine.reflection.name_id("Raycast") else { return Out::General };
                let params = match ffi::lua_type(state, 4) {
                    ffi::LUA_TNONE | ffi::LUA_TNIL => None,
                    ffi::LUA_TUSERDATA | ffi::LUA_TTABLE => {
                        ffi::lua_rawgeti_(state, REGISTRY, api.refs.bridge);
                        ffi::lua_pushvalue(state, 4);
                        ffi::lua_rawseti_(state, -2, 1);
                        ffi::lua_pop(state, 1);
                        match api.bridge.raw_get::<LuaValue>(1) {
                            Ok(value) => Some(value),
                            Err(_) => return Out::General,
                        }
                    }
                    _ => return Out::General,
                };
                crate::project::before_method(&mut engine.state.borrow_mut(), &engine.reflection, id, method);
                let hit = crate::world::query::raw_cast(engine, origin, direction, params.as_ref());
                if params.is_some() {
                    let _ = api.bridge.raw_set(1, LuaValue::Nil);
                }
                let Some((part, position, normal, distance, material)) = hit else {
                    ffi::lua_pushnil(state);
                    return Out::Ret(1);
                };
                ffi::lua_createtable(state, 0, 5);
                let table = ffi::lua_gettop(state);
                push_instance(state, api, None, part);
                ffi::lua_setfield(state, table, c"Instance".as_ptr());
                ffi::lua_pushvector(state, position.x, position.y, position.z);
                ffi::lua_setfield(state, table, c"Position".as_ptr());
                ffi::lua_pushvector(state, normal.x, normal.y, normal.z);
                ffi::lua_setfield(state, table, c"Normal".as_ptr());
                ffi::lua_pushnumber(state, distance as f64);
                ffi::lua_setfield(state, table, c"Distance".as_ptr());
                if let Some(item) = material {
                    ffi::lua_rawgeti_(state, REGISTRY, api.refs.enums);
                    if ffi::lua_rawgeti_(state, -1, item as c_int + 1) != ffi::LUA_TNIL {
                        ffi::lua_remove(state, -2);
                        ffi::lua_setfield(state, table, c"Material".as_ptr());
                    } else {
                        ffi::lua_pop(state, 2);
                    }
                }
                if push_result_meta(state, api) {
                    ffi::lua_setmetatable(state, table);
                    ffi::lua_setreadonly(state, table, 1);
                }
                Out::Ret(1)
            }
            Base::GetFullName => {
                let name = engine.state.borrow().dom.full_name(&engine.reflection, id);
                push_bytes(state, name.as_bytes());
                Out::Ret(1)
            }
            Base::WaitForChild => {
                let Some(name) = arg_bytes(state, 2) else { return Out::General };
                match find_first_child(engine, id, Some(name), false) {
                    Some(child) => push_optional_instance(state, api, engine, Some(child)),
                    None => Out::General,
                }
            }
            Base::HasTag => {
                let has = arg_bytes(state, 2).is_some_and(|tag| engine.state.borrow().dom.at(id).tags.iter().any(|t| t.as_bytes() == tag));
                ffi::lua_pushboolean(state, has as c_int);
                Out::Ret(1)
            }
        }
    }
}

// Other engine userdata: methods from a table

/// `__namecall` of engine userdata whose members are methods in a table (RBXScriptSignal,
/// RBXScriptConnection). Upvalues: 1 = methods (name -> function; protocol results are checked),
/// 2 = the type name, 3 = properties (name -> the Luau type name of its value).
unsafe extern "C-unwind" fn table_namecall(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        let key = ffi::lua_namecallatom(state, std::ptr::null_mut());
        if key.is_null() {
            raise(state, "invalid namecall".into(), 0)
        }
        ffi::lua_pushstring_(state, key);
        if ffi::lua_rawget(state, ffi::lua_upvalueindex(1)) == ffi::LUA_TFUNCTION {
            ffi::lua_insert(state, 1);
            let count = ffi::lua_gettop(state) - 1;
            ffi::lua_call(state, count, ffi::LUA_MULTRET);
            if ffi::lua_status(state) != ffi::LUA_OK {
                return -1;
            }
            return namecall_finish(state, None);
        }
        ffi::lua_pop(state, 1);
        let name = std::ffi::CStr::from_ptr(key).to_string_lossy().into_owned();
        ffi::lua_pushstring_(state, key);
        let (message, level) = if ffi::lua_rawget(state, ffi::lua_upvalueindex(3)) == ffi::LUA_TSTRING {
            (format!("attempt to call a {} value", arg_bytes(state, -1).map(String::from_utf8_lossy).unwrap_or_default()), 1)
        } else {
            // (a member error: Roblox raises it without the script's position)
            let type_name = arg_bytes(state, ffi::lua_upvalueindex(2)).map(String::from_utf8_lossy).unwrap_or_default();
            (format!("{name} is not a valid member of {type_name}"), 0)
        };
        drop(name);
        ffi::lua_pop(state, 1);
        raise(state, message, level)
    }
}

/// Gives the metatable of `sample`'s type a table-dispatched `__namecall` (see `table_namecall`).
pub fn set_table_namecall(lua: &Lua, sample: &LuaAnyUserData, methods: LuaTable, type_name: &str, properties: LuaTable) -> LuaResult<()> {
    unsafe {
        lua.exec_raw::<()>((sample, methods, type_name, properties), |state| {
            // stack: sample, methods, type name, properties
            if ffi::lua_getmetatable(state, 1) != 0 {
                ffi::lua_pushvalue(state, 2);
                ffi::lua_pushvalue(state, 3);
                ffi::lua_pushvalue(state, 4);
                ffi::lua_pushcclosurek(state, table_namecall, c"__namecall".as_ptr(), 3, Some(namecall_continue));
                ffi::lua_setfield(state, -2, c"__namecall".as_ptr());
                ffi::lua_pop(state, 1);
            }
        })
    }
}

// typeof

unsafe extern "C-unwind" {
    fn lua_clonetable(state: *mut ffi::lua_State, index: c_int);
    fn lua_settypeofnames(state: *mut ffi::lua_State, index: c_int, vectorname: *const c_char);
}

const UTAG_PROXY: c_int = ffi::LUA_UTAG_LIMIT + 1;

/// `typeof` (prelude.luau Typeof): a registered table / proxy type, Vector3 for native vectors,
/// else Luau's typeof (a userdata's `__type`, except for newproxy proxies). Upvalues: the registry,
/// "Instance".
unsafe extern "C-unwind" fn typeof_c(state: *mut ffi::lua_State) -> c_int {
    unsafe {
        // `typeof()` raises "missing argument #1", as type() does, without a position (measured in a playing client, Studio
        // 0.744: tools/parity/probes/values.luau)
        if ffi::lua_gettop(state) == 0 {
            raise(state, "missing argument #1".into(), 0);
        }
        ffi::lua_settop(state, 1);
        let kind = ffi::lua_type(state, 1);
        if kind == ffi::LUA_TUSERDATA && ffi::lua_userdatatag(state, 1) == INSTANCE_TAG {
            ffi::lua_pushvalue(state, ffi::lua_upvalueindex(2));
            return 1;
        }
        if kind == ffi::LUA_TUSERDATA || kind == ffi::LUA_TTABLE {
            ffi::lua_pushvalue(state, 1);
            if ffi::lua_rawget(state, ffi::lua_upvalueindex(1)) == ffi::LUA_TSTRING {
                return 1;
            }
            ffi::lua_pop(state, 1);
            if kind == ffi::LUA_TUSERDATA && ffi::lua_userdatatag(state, 1) != UTAG_PROXY {
                if ffi::luaL_getmetafield(state, 1, c"__type".as_ptr()) == ffi::LUA_TSTRING {
                    return 1;
                }
                ffi::lua_settop(state, 1);
            }
        }
        if kind == ffi::LUA_TVECTOR {
            push_bytes(state, b"Vector3");
            return 1;
        }
        ffi::lua_pushstring_(state, ffi::lua_typename(state, kind.max(ffi::LUA_TNIL)));
        1
    }
}

/// [`typeof_c`] as the `typeof` global; the VM's builtin typeof, which a script's `typeof(x)` calls directly in a safe
/// environment (a fast call, as in Roblox), is given the same names: the registry, and Vector3 for vectors (Instances and the
/// engine's other userdata carry their `__type`)
pub fn typeof_function(lua: &Lua, registry: LuaTable) -> LuaResult<LuaFunction> {
    unsafe {
        lua.exec_raw((registry, "Instance"), |state| {
            lua_settypeofnames(state, -2, c"Vector3".as_ptr());
            ffi::lua_pushcclosurek(state, typeof_c, c"typeof".as_ptr(), 2, None);
        })
    }
}

// __tostring

unsafe extern "C-unwind" fn meta_tostring(state: *mut ffi::lua_State) -> c_int {
    let out = guard(|| unsafe {
        let Some((api, id)) = this(state) else { return Out::Raise("invalid Instance".into(), 0) };
        let engine = api.engine();
        let dom_state = engine.state.borrow();
        push_bytes(state, dom_state.dom.at(id).name.as_bytes());
        Out::Ret(1)
    });
    match out {
        Out::Ret(count) => count,
        Out::Raise(message, level) => unsafe { raise(state, message, level) },
        _ => 0,
    }
}

// Anchoring and collection

unsafe fn sync_if_moved(state: *mut ffi::lua_State, api: &Api) {
    if api.moved.borrow().is_empty() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe { sync(state, api) }));
}

/// Applies queued parent changes to the anchor table: userdata of instances that entered the
/// DataModel are anchored, those that left it are released (and their roots become collection
/// candidates).
unsafe fn sync(state: *mut ffi::lua_State, api: &Api) {
    unsafe {
        let moved = std::mem::take(&mut *api.moved.borrow_mut());
        let engine = api.engine();
        let Ok(dom_state) = engine.state.try_borrow() else {
            api.moved.borrow_mut().extend(moved);
            return;
        };
        let game = dom_state.collection.game;
        for entry in moved {
            if dom_state.dom.get(entry.id).is_none() {
                continue;
            }
            if entry.unparented && dom_state.dom.at(entry.id).parent.is_none() {
                api.gc.borrow_mut().orphans.push(entry.id);
            }
            let in_game = rooted(&dom_state, entry.id, game);
            if entry.subtree && !in_game {
                release_children_lists(state, api, &dom_state, entry.id);
            }
            let nodes = if entry.subtree {
                let mut nodes = vec![entry.id];
                nodes.extend(dom_state.dom.descendants(entry.id));
                nodes
            } else {
                vec![entry.id]
            };
            for node in nodes {
                let index = node.index as usize;
                let (live, anchored) = {
                    let gc = api.gc.borrow();
                    (gc.live.get(index).copied().unwrap_or(0), gc.anchored.get(index).copied().unwrap_or(false))
                };
                if live == 0 || anchored == in_game {
                    continue;
                }
                let slot = node.index as c_int + 1;
                ffi::lua_rawgeti_(state, REGISTRY, api.refs.anchors);
                if in_game {
                    ffi::lua_rawgeti_(state, REGISTRY, api.refs.instances);
                    ffi::lua_rawgeti_(state, -1, slot);
                    ffi::lua_remove(state, -2);
                    if ffi::lua_type(state, -1) != ffi::LUA_TUSERDATA {
                        ffi::lua_pop(state, 2);
                        continue;
                    }
                } else {
                    ffi::lua_pushnil(state);
                }
                ffi::lua_rawseti_(state, -2, slot);
                ffi::lua_pop(state, 1);
                api.gc.borrow_mut().anchored[index] = in_game;
            }
        }
    }
}

/// Drops the cached GetChildren lists of a subtree (they hold its children's userdata).
unsafe fn release_children_lists(state: *mut ffi::lua_State, api: &Api, dom_state: &State, root: InstanceId) {
    let mut slots = Vec::new();
    {
        let mut cache = api.values.borrow_mut();
        let mut nodes = vec![root];
        nodes.extend(dom_state.dom.descendants(root));
        for node in nodes {
            if let Some((_, slot)) = cache.map.remove(&(node, CHILDREN_KEY)) {
                cache.free.push(slot);
                slots.push(slot);
            }
        }
    }
    if slots.is_empty() {
        return;
    }
    unsafe {
        ffi::lua_rawgeti_(state, REGISTRY, api.refs.values);
        for slot in slots {
            ffi::lua_pushnil(state);
            ffi::lua_rawseti_(state, -2, slot);
        }
        ffi::lua_pop(state, 1);
    }
}

/// Frames between looks at the trees a property or queued work kept (a tween that ends, an ObjectValue that is
/// cleared): about two seconds at 60 fps.
const KEPT_RECHECK_FRAMES: u32 = 120;

/// Collects when enough garbage candidates piled up (called once per rendered frame).
pub fn collect_if_due(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let Some(api) = api(engine) else { return Ok(()) };
    let due = {
        let mut gc = api.gc.borrow_mut();
        gc.frames += 1;
        let unpinned = engine.state.try_borrow().map_or(0, |state| state.dom.unpinned_len());
        let pending = gc.dead.len() + gc.orphans.len() + api.moved.borrow().len() + unpinned;
        pending >= 512 || (pending > 0 && gc.frames >= 30) || (!gc.kept.is_empty() && gc.frames >= KEPT_RECHECK_FRAMES)
    };
    if due {
        collect(lua, engine)?;
    }
    Ok(())
}

/// Drops cached GetChildren lists that are out of date (they would keep removed children's
/// userdata alive).
fn prune_children_lists(api: &Api, engine: &Engine) -> LuaResult<()> {
    let stale: Vec<c_int> = {
        let state = engine.state.borrow();
        let mut cache = api.values.borrow_mut();
        let mut stale = Vec::new();
        cache.map.retain(|&(id, name), &mut (version, slot)| {
            if name != CHILDREN_KEY {
                return true;
            }
            let current = state.dom.get(id).is_some_and(|i| i.child_version == version);
            if !current {
                stale.push(slot);
            }
            current
        });
        cache.free.extend(stale.iter().copied());
        stale
    };
    for slot in stale {
        api.values_table.raw_set(slot, LuaValue::Nil)?;
    }
    Ok(())
}

/// Frees unreachable instances: parentless subtrees (not the DataModel, not pinned, not
/// Studio-only placeholders) none of whose instances has a Luau userdata or is referenced by
/// another instance's property or by queued work. Must run where no engine borrow is held.
pub fn collect(lua: &Lua, engine: &Engine) -> LuaResult<usize> {
    let Some(api) = api(engine) else { return Ok(0) };
    if !api.moved.borrow().is_empty() {
        unsafe { lua.exec_raw::<()>((), |state| sync(state, &api))? };
    }
    prune_children_lists(&api, engine)?;
    let (dead, orphans) = {
        let mut gc = api.gc.borrow_mut();
        gc.frames = 0;
        let kept = std::mem::take(&mut gc.kept);
        let mut orphans = std::mem::take(&mut gc.orphans);
        orphans.extend(kept);
        orphans.extend(engine.state.borrow_mut().dom.take_unpinned());
        (std::mem::take(&mut gc.dead), orphans)
    };
    if dead.is_empty() && orphans.is_empty() {
        return Ok(0);
    }
    let freed = {
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        let garbage = find_garbage(engine, &api, &state, &dead, &orphans);
        for &id in &garbage {
            core::disconnect_all(&mut state, id);
            for hook in &hooks.instance_freed {
                hook(&mut state, id);
            }
        }
        let mut signals = Vec::new();
        for &id in &garbage {
            if let Some(instance) = state.dom.free(id) {
                if let Some(instance_signals) = &instance.signals {
                    signals.extend(instance_signals.all());
                }
            }
        }
        // (their slots are reused; a script still holding one of these signals holds a dead one)
        for &signal in &signals {
            state.signals.free(signal);
        }
        // (with the signals the hooks freed)
        (garbage, state.signals.take_freed())
    };
    let (garbage, signals) = freed;
    forget(&api, engine, &garbage, &signals)?;
    Ok(garbage.len())
}

fn find_garbage(engine: &Engine, api: &Api, state: &State, dead: &[u32], orphans: &[InstanceId]) -> Vec<InstanceId> {
    let game = state.collection.game;
    let mut starts: Vec<InstanceId> = orphans.to_vec();
    starts.extend(dead.iter().filter_map(|&index| state.dom.id_at_slot(index)));
    // Candidate roots: parentless, not the DataModel. (A walk up stops at an instance an earlier
    // walk passed: its root is already listed or skipped. Thousands of userdata die together, most
    // of them siblings.)
    let mut roots: Vec<InstanceId> = Vec::new();
    let mut seen_roots: FxHashSet<InstanceId> = FxHashSet::default();
    let mut walked: FxHashSet<u32> = FxHashSet::default();
    for start in starts {
        if state.dom.get(start).is_none() {
            continue;
        }
        let mut root = start;
        let mut known = !walked.insert(root.index);
        while !known {
            match state.dom.at(root).parent {
                Some(parent) => {
                    root = parent;
                    known = !walked.insert(root.index);
                }
                None => break,
            }
        }
        if known || Some(root) == game || !seen_roots.insert(root) {
            continue;
        }
        roots.push(root);
    }
    // Subtrees without userdata, pins or Studio-only placeholders. A subtree is looked at again
    // whenever one of its userdata dies; a tree a script keeps (a removed UI whose connections
    // still reference it) is held by the same instances every time, so the one that held it last
    // time is asked first, and a walk stops at the first holder.
    let mut candidate_root: FxHashMap<InstanceId, InstanceId> = FxHashMap::default();
    let mut subtrees: FxHashMap<InstanceId, Vec<InstanceId>> = FxHashMap::default();
    let mut holders = std::mem::take(&mut api.gc.borrow_mut().holders);
    {
        let gc = api.gc.borrow();
        let holds = |node: InstanceId| {
            gc.live.get(node.index as usize).is_some_and(|&n| n > 0)
                || state.dom.is_pinned(node)
                || state.dom.at(node).has(flags::STUDIO_ONLY)
        };
        for root in roots {
            if let Some(&holder) = holders.get(&root) {
                if state.dom.get(holder).is_some() {
                    let mut top = holder;
                    while let Some(parent) = state.dom.at(top).parent {
                        top = parent;
                    }
                    if top == root && holds(holder) {
                        continue;
                    }
                }
                holders.remove(&root);
            }
            // (the root, then its descendants depth-first: `Dom::descendants`' order)
            let mut nodes = Vec::new();
            let mut stack = vec![root];
            let mut holder = None;
            while let Some(node) = stack.pop() {
                if holds(node) {
                    holder = Some(node);
                    break;
                }
                nodes.push(node);
                stack.extend(state.dom.at(node).children.iter().rev());
            }
            if let Some(holder) = holder {
                holders.insert(root, holder);
                continue;
            }
            for &node in &nodes {
                candidate_root.insert(node, root);
            }
            subtrees.insert(root, nodes);
        }
    }
    // (holders of trees that were freed or joined another tree are dropped when their root is next
    // looked at; the map is also cut back when it outgrows the instances that could be roots)
    if holders.len() > 4096 {
        holders.retain(|root, _| state.dom.get(*root).is_some_and(|instance| instance.parent.is_none()));
    }
    api.gc.borrow_mut().holders = holders;
    if candidate_root.is_empty() {
        return Vec::new();
    }
    // References from outside keep a candidate subtree (and what it references) alive
    let mut pending: Vec<InstanceId> = Vec::new();
    let mut kept_roots: Vec<InstanceId> = Vec::new();
    let mut keep = |target: InstanceId, candidate_root: &mut FxHashMap<InstanceId, InstanceId>, pending: &mut Vec<InstanceId>| {
        if let Some(root) = candidate_root.get(&target).copied() {
            if let Some(nodes) = subtrees.remove(&root) {
                kept_roots.push(root);
                for node in &nodes {
                    candidate_root.remove(node);
                }
                pending.extend(nodes);
            }
        }
    };
    let world = engine.world();
    for id in [Some(world.game), Some(world.workspace), Some(world.camera), world.local_player, state.project.client_gui]
        .into_iter()
        .flatten()
    {
        keep(id, &mut candidate_root, &mut pending);
    }
    let mut referenced = Vec::new();
    state.sched.visit_instance_refs(&mut |id| referenced.push(id));
    for id in referenced {
        keep(id, &mut candidate_root, &mut pending);
    }
    for index in 0..state.dom.slot_count() as u32 {
        let Some(id) = state.dom.id_at_slot(index) else { continue };
        // (only instances that ever stored a reference can hold one)
        if !state.dom.at(id).has(flags::HOLDS_REFS) || candidate_root.contains_key(&id) {
            continue;
        }
        let mut targets = Vec::new();
        for (_, value) in state.dom.at(id).props.iter() {
            value.visit_instances(&mut |target| targets.push(target));
        }
        // (an attribute holding an Instance keeps it too)
        for (_, value) in state.dom.at(id).attributes.iter() {
            value.visit_instances(&mut |target| targets.push(target));
        }
        for target in targets {
            keep(target, &mut candidate_root, &mut pending);
        }
    }
    while let Some(id) = pending.pop() {
        let mut targets = Vec::new();
        for (_, value) in state.dom.at(id).props.iter() {
            value.visit_instances(&mut |target| targets.push(target));
        }
        for (_, value) in state.dom.at(id).attributes.iter() {
            value.visit_instances(&mut |target| targets.push(target));
        }
        for target in targets {
            keep(target, &mut candidate_root, &mut pending);
        }
    }
    // (a tree kept by a property or queued work has no userdata and no pin to signal the day that goes:
    // `collect` offers it again)
    api.gc.borrow_mut().kept = kept_roots;
    candidate_root.into_keys().collect()
}

/// Drops the Luau-side caches of freed instances (converted values, signal userdata).
fn forget(api: &Api, engine: &Engine, garbage: &[InstanceId], signals: &[u32]) -> LuaResult<()> {
    let freed: FxHashSet<InstanceId> = garbage.iter().copied().collect();
    let slots: Vec<c_int> = {
        let mut cache = api.values.borrow_mut();
        let mut slots = Vec::new();
        cache.map.retain(|(id, _), &mut (_, slot)| {
            if freed.contains(id) {
                slots.push(slot);
                false
            } else {
                true
            }
        });
        cache.free.extend(slots.iter().copied());
        slots
    };
    for slot in slots {
        api.values_table.raw_set(slot, LuaValue::Nil)?;
    }
    {
        let mut gc = api.gc.borrow_mut();
        for id in garbage {
            if let Some(anchored) = gc.anchored.get_mut(id.index as usize) {
                *anchored = false;
            }
        }
    }
    let mut refs = engine.lua.borrow_mut();
    for &signal in signals {
        if let Some(entry) = refs.signals.get_mut(signal as usize) {
            *entry = None;
        }
        api.signals.raw_set(signal as i64 + 1, LuaValue::Nil)?;
    }
    Ok(())
}

/// Entries of the converted-value cache (diagnostics and tests).
pub fn value_cache_len(engine: &Engine) -> usize {
    api(engine).map_or(0, |api| api.values.borrow().map.len())
}

/// Live userdata / anchored / pending counts (diagnostics and tests).
pub fn stats(engine: &Engine) -> (usize, usize, usize) {
    let Some(api) = api(engine) else { return (0, 0, 0) };
    let gc = api.gc.borrow();
    let live = gc.live.iter().filter(|&&n| n > 0).count();
    let anchored = gc.anchored.iter().filter(|&&a| a).count();
    (live, anchored, gc.dead.len() + gc.orphans.len())
}

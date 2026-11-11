//! The Luau-facing engine API: Instance userdata, engine datatypes, globals, per-script
//! environments and require (ports of `runtime/engine/globals.luau`, `modules.luau`, the
//! metatable half of `instance.luau` and `init.luau`'s Engine.Start).
//!
//! Engine functions reach scripts through the prelude's call protocol (`prelude.luau`): a Rust
//! function returns its values, or a marker that makes the Luau wrapper raise a string error at
//! the right level or yield the calling coroutine. Scripts therefore see Roblox's string errors
//! (never mlua's userdata errors), and yields never cross a Rust frame.

pub mod actors;
pub mod bindable;
pub mod convert;
pub mod curve_keys;
pub mod globals;
pub mod holders;
pub mod instance;
pub mod leaks;
pub mod modules;
pub mod raw;
pub mod refs;
pub mod stubs;
pub mod types;

#[cfg(test)]
mod datatype_tests;

use std::rc::Rc;

use mlua::prelude::*;
use rustc_hash::FxHashMap;

use super::{
    ClassId, Engine, InstanceId, NameId, ScriptError,
    reflection::{EnumId, EnumItemId},
    signal::SignalId,
};

static SIGNAL_MARKER: u8 = 0;

/// The protocol marker (a light userdata no script can forge).
pub fn signal_marker() -> LuaValue {
    LuaValue::LightUserData(LuaLightUserData(signal_marker_ptr()))
}

#[inline]
pub fn signal_marker_ptr() -> *mut std::ffi::c_void {
    &SIGNAL_MARKER as *const u8 as *mut std::ffi::c_void
}

/// How an engine function ended besides returning values.
#[derive(Debug)]
pub enum Flow {
    /// Raise a string error in the calling script
    Error(ScriptError),
    /// Yield the calling coroutine (its resumption is already registered)
    Yield,
    /// An internal failure (surfaces as an mlua error)
    Fail(LuaError),
}

impl From<LuaError> for Flow {
    fn from(error: LuaError) -> Self {
        Flow::Fail(error)
    }
}

impl From<ScriptError> for Flow {
    fn from(error: ScriptError) -> Self {
        Flow::Error(error)
    }
}

pub type Ret = Result<LuaMultiValue, Flow>;

/// Encodes an engine function's outcome for the prelude's `Pass`.
pub fn encode(lua: &Lua, outcome: Ret) -> LuaResult<LuaMultiValue> {
    match outcome {
        Ok(values) => Ok(values),
        Err(Flow::Error(error)) => Ok(LuaMultiValue::from_iter([
            signal_marker(),
            LuaValue::Integer(1),
            LuaValue::String(lua.create_string(&error.message)?),
            LuaValue::Integer(error.level as _),
        ])),
        Err(Flow::Yield) => Ok(LuaMultiValue::from_iter([signal_marker(), LuaValue::Integer(2)])),
        Err(Flow::Fail(error)) => Err(error),
    }
}

/// An error of the engine's own members (properties, methods, events, services): Roblox raises them without the
/// script's position (`NotAMember is not a valid member of Part "Part"`, `Unable to assign property ClassName.
/// Property is read only`, `'X' is not a valid Service name`; measured in a playing client, Studio 0.744:
/// tools/parity/probes/errors.luau).
pub fn raise<T>(message: impl Into<String>) -> Result<T, Flow> {
    Err(Flow::Error(ScriptError::plain(message)))
}

pub fn raise_plain<T>(message: impl Into<String>) -> Result<T, Flow> {
    Err(Flow::Error(ScriptError::plain(message)))
}

/// An error Luau's own checks would raise, at the calling script line: `invalid argument #2 (string expected, got
/// number)`, `attempt to iterate over a Instance value` (Roblox's are positioned like the VM's).
pub fn raise_at_caller<T>(message: impl Into<String>) -> Result<T, Flow> {
    Err(Flow::Error(ScriptError::at_caller(message)))
}

pub fn values<const N: usize>(items: [LuaValue; N]) -> LuaMultiValue {
    LuaMultiValue::from_iter(items)
}

pub fn none() -> LuaMultiValue {
    LuaMultiValue::new()
}

/// Functions the prelude chunk returns.
pub struct Prelude {
    pub wrap: LuaFunction,
    pub typeof_fn: LuaFunction,
    pub register: LuaFunction,
    pub traceback: LuaFunction,
    pub exports: LuaTable,
}

/// Luau-side caches: one userdata per enum item / signal (identity matters: they are table keys
/// and compared with ==), and one function per method implementation. Instance userdata live in
/// the raw API's weak / anchor tables (`raw.rs`).
#[derive(Default)]
pub struct LuaRefs {
    /// The raw Instance API (userdata, dispatch cache, lifetime)
    pub raw: Option<Rc<raw::Api>>,
    pub enum_items: Vec<Option<LuaAnyUserData>>,
    pub enums: Vec<Option<LuaAnyUserData>>,
    pub enums_global: Option<LuaAnyUserData>,
    pub signals: Vec<Option<LuaAnyUserData>>,
    /// Behaviour methods by the (class, name) that defines them
    pub methods: FxHashMap<(ClassId, NameId), LuaFunction>,
    /// Behaviour methods as namecall runs them (protocol results, no Luau wrapper)
    pub call_methods: FxHashMap<(ClassId, NameId), LuaFunction>,
    /// Stand-ins for reflected methods without a behaviour (report + return nil)
    pub unimplemented: FxHashMap<(ClassId, NameId), LuaFunction>,
    pub prelude: Option<Rc<Prelude>>,
    /// The shared global table every script environment inherits
    pub shared: Option<LuaTable>,
    /// Signal / connection member functions
    pub signal_members: Option<LuaTable>,
    pub connection_members: Option<LuaTable>,
    pub enum_members: Option<LuaTable>,
    pub enum_item_is_a: Option<LuaFunction>,
    /// Lune's `coroutine.close` (task.cancel)
    pub coroutine_close: Option<LuaFunction>,
    /// `math.randomseed`
    pub randomseed: Option<LuaFunction>,
    /// Library tables of engine datatypes (Font, Content, ...)
    pub libraries: FxHashMap<&'static str, LuaTable>,
    /// The slots the VM holds for scripts (refs.rs)
    pub gauge: Option<refs::Gauge>,
}

impl Engine {
    pub fn prelude(&self) -> Rc<Prelude> {
        self.lua.borrow().prelude.clone().expect("prelude not loaded")
    }
}

/// Wraps a Rust engine function (protocol results) into a script-callable function.
pub fn wrapped<F>(lua: &Lua, engine: &Rc<Engine>, function: F) -> LuaResult<LuaFunction>
where
    F: Fn(&Lua, LuaMultiValue) -> Ret + 'static,
{
    let raw = lua.create_function(move |lua, args: LuaMultiValue| encode(lua, function(lua, args)))?;
    engine.prelude().wrap.call::<LuaFunction>(raw)
}

/// `debug.traceback(thread)` (diagnostics only).
pub fn traceback(lua: &Lua, thread: &LuaThread) -> Option<String> {
    let engine = lua.app_data_ref::<Rc<Engine>>()?.clone();
    let prelude = engine.lua.borrow().prelude.clone()?;
    prelude.traceback.call::<Option<String>>(thread.clone()).ok().flatten()
}

/// typeof as scripts see it (registered table types, Vector3 for native vectors).
pub fn typeof_value(engine: &Engine, value: &LuaValue) -> String {
    match value {
        LuaValue::Nil => "nil".into(),
        LuaValue::Boolean(_) => "boolean".into(),
        LuaValue::Integer(_) | LuaValue::Number(_) => "number".into(),
        LuaValue::String(_) => "string".into(),
        LuaValue::Function(_) => "function".into(),
        LuaValue::Thread(_) => "thread".into(),
        LuaValue::Vector(_) => "Vector3".into(),
        LuaValue::UserData(ud) if raw::instance_of_ud(ud).is_some() => "Instance".into(),
        _ => {
            let prelude = engine.lua.borrow().prelude.clone();
            match prelude {
                Some(prelude) => prelude.typeof_fn.call::<String>(value.clone()).unwrap_or_else(|_| "userdata".into()),
                None => value.type_name().into(),
            }
        }
    }
}

/// The (cached) userdata of an instance.
#[inline]
pub fn instance_value(lua: &Lua, engine: &Engine, id: InstanceId) -> LuaResult<LuaValue> {
    raw::instance_value(lua, engine, id)
}

/// The instance a script value is, if any (no Lua call).
#[inline]
pub fn instance_id(value: &LuaValue) -> Option<InstanceId> {
    raw::instance_id(value)
}

pub fn enum_item_value(lua: &Lua, engine: &Engine, item: EnumItemId) -> LuaResult<LuaValue> {
    let index = item as usize;
    {
        let refs = engine.lua.borrow();
        if let Some(Some(existing)) = refs.enum_items.get(index) {
            return Ok(LuaValue::UserData(existing.clone()));
        }
    }
    let userdata = lua.create_any_userdata(types::LEnumItem(item))?;
    {
        let mut refs = engine.lua.borrow_mut();
        if refs.enum_items.len() <= index {
            refs.enum_items.resize(engine.reflection.enum_items.len().max(index + 1), None);
        }
        refs.enum_items[index] = Some(userdata.clone());
    }
    raw::remember_enum_item(engine, item, &userdata)?;
    Ok(LuaValue::UserData(userdata))
}

pub fn enum_value(lua: &Lua, engine: &Engine, id: EnumId) -> LuaResult<LuaValue> {
    let index = id as usize;
    {
        let refs = engine.lua.borrow();
        if let Some(Some(existing)) = refs.enums.get(index) {
            return Ok(LuaValue::UserData(existing.clone()));
        }
    }
    let userdata = lua.create_any_userdata(types::LEnum(id))?;
    let mut refs = engine.lua.borrow_mut();
    if refs.enums.len() <= index {
        refs.enums.resize(engine.reflection.enums.len().max(index + 1), None);
    }
    refs.enums[index] = Some(userdata.clone());
    Ok(LuaValue::UserData(userdata))
}

pub fn signal_value(lua: &Lua, engine: &Engine, signal: SignalId) -> LuaResult<LuaValue> {
    // With the raw Instance API, the signal userdata are kept in its Luau table only (the C `__index`
    // reads them there): a Rust-side handle each would take a slot of the VM's reference stack per
    // signal a script ever touched
    if let Some(existing) = raw::signal_userdata(engine, signal)? {
        if !existing.is_nil() {
            return Ok(existing);
        }
        let userdata = lua.create_any_userdata(types::LSignal(signal, engine.state.borrow().signals.generation(signal)))?;
        raw::remember_signal(engine, signal, &userdata)?;
        return Ok(LuaValue::UserData(userdata));
    }
    let index = signal as usize;
    {
        let refs = engine.lua.borrow();
        if let Some(Some(existing)) = refs.signals.get(index) {
            return Ok(LuaValue::UserData(existing.clone()));
        }
    }
    let userdata = lua.create_any_userdata(types::LSignal(signal, engine.state.borrow().signals.generation(signal)))?;
    {
        let mut refs = engine.lua.borrow_mut();
        if refs.signals.len() <= index {
            refs.signals.resize(index + 1, None);
        }
        refs.signals[index] = Some(userdata.clone());
    }
    raw::remember_signal(engine, signal, &userdata)?;
    Ok(LuaValue::UserData(userdata))
}

/// Replaces mlua's `__metatable = false` with Roblox's locked-metatable string for a registered
/// userdata type (the metatable is shared by every value of the type).
pub fn lock_metatable(lua: &Lua, sample: &LuaAnyUserData) -> LuaResult<()> {
    unsafe {
        lua.exec_raw::<()>(sample, |state| {
            use mlua::ffi;
            if ffi::lua_getmetatable(state, -1) != 0 {
                ffi::lua_pushstring(state, c"The metatable is locked".as_ptr());
                ffi::lua_setfield(state, -2, c"__metatable".as_ptr());
                ffi::lua_pop(state, 1);
            }
        })
    }
}

/// Argument helpers (engine functions parse their own arguments: conversion failures must be
/// Roblox errors, not mlua's).
pub struct Args(pub LuaMultiValue);

impl Args {
    #[inline]
    pub fn new(args: LuaMultiValue) -> Self {
        Self(args)
    }

    #[inline]
    pub fn get(&self, index: usize) -> &LuaValue {
        self.0.get(index).unwrap_or(&LuaValue::Nil)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn number(&self, index: usize) -> Option<f64> {
        match self.get(index) {
            LuaValue::Number(n) => Some(*n),
            LuaValue::Integer(n) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn string(&self, index: usize) -> Option<LuaString> {
        match self.get(index) {
            LuaValue::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn boolean(&self, index: usize) -> bool {
        !matches!(self.get(index), LuaValue::Nil | LuaValue::Boolean(false))
    }

    #[inline]
    pub fn instance(&self, index: usize) -> Option<InstanceId> {
        raw::instance_id(self.get(index))
    }

    pub fn rest(&self, from: usize) -> LuaMultiValue {
        LuaMultiValue::from_iter(self.0.iter().skip(from).cloned())
    }
}

pub use globals::start;

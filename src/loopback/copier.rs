//! The wire's copier on Luau's C API (wire.rs's rules, without mlua's typed values per entry): a captured table is walked
//! with `lua_rawgeti` / `lua_next` in one protected call, and a rebuilt one is made presized with `lua_createtable` and
//! filled with raw sets. Only the values mlua must make (instances, datatypes, a Random, a TweenInfo, a DateTime) cross
//! through it: a capture keeps their stack slots in a side table and converts them after; a rebuild makes them first and
//! pushes them from a sequence. The results are the typed copier's (tools/parity/probes/loopback/remote_values.luau and
//! remote_tables.luau, run by tests/parity.rs; loopback_tests).

use std::ffi::{c_int, c_void};

use mlua::{ffi, prelude::*};

use super::wire::{Key, Wire};
use crate::{Engine, InstanceId};

/// A value whose capture mlua finishes (an object's slot in the side table)
const PENDING: u32 = u32::MAX;

/// Captures the tables among `values` (the rest as wire.rs captures them) into wire values, tables nested at most `depth`
pub fn capture_all(lua: &Lua, engine: &Engine, values: &[LuaValue], depth: usize) -> Result<Box<[Wire]>, String> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(match value {
            LuaValue::Table(table) => capture_table(lua, engine, table, depth)?,
            other => super::wire::capture(lua, engine, other, &mut Vec::new(), depth)?,
        });
    }
    Ok(out.into_boxed_slice())
}

/// What a raw walk found: the tree with objects left as `Wire::Pending`-like placeholders, and those objects' order
struct Walk {
    error: Option<String>,
    objects: u32,
}

fn capture_table(lua: &Lua, engine: &Engine, table: &LuaTable, depth: usize) -> Result<Wire, String> {
    let mut walk = Walk { error: None, objects: 0 };
    let mut tree: Option<Wire> = None;
    // (the side table: the objects' values in the order the walk met them, finished by mlua below)
    let objects: LuaTable = unsafe {
        lua.exec_raw(table, |state| {
            ffi::lua_createtable(state, 0, 0);
            let side = ffi::lua_gettop(state);
            let mut ancestors: Vec<*const c_void> = Vec::new();
            match walk_table(state, 1, side, &mut ancestors, depth, &mut walk) {
                Ok(wire) => tree = Some(wire),
                Err(message) => walk.error = Some(message),
            }
            // (the side table returned: a walk that stopped early leaves its key and value above it)
            ffi::lua_pushvalue(state, side);
            ffi::lua_replace(state, 1);
            ffi::lua_settop(state, 1);
        })
    }
    .map_err(|error| error.to_string())?;
    if let Some(message) = walk.error {
        if message == FALLBACK {
            return super::wire::capture(lua, engine, &LuaValue::Table(table.clone()), &mut Vec::new(), depth);
        }
        return Err(message);
    }
    let mut tree = tree.unwrap_or(Wire::Nil);
    if walk.objects > 0 {
        let mut finished = Vec::with_capacity(walk.objects as usize);
        for index in 1..=walk.objects {
            let value: LuaValue = objects.raw_get(index).map_err(|error| error.to_string())?;
            finished.push(super::wire::capture(lua, engine, &value, &mut Vec::new(), depth)?);
        }
        let mut next = finished.into_iter();
        fill_objects(&mut tree, &mut next);
    }
    Ok(tree)
}

/// Replaces the walk's placeholders (in the walk's order) with the objects mlua captured
fn fill_objects(wire: &mut Wire, objects: &mut std::vec::IntoIter<Wire>) {
    match wire {
        Wire::Instance(id) if id.index == PENDING && id.generation == PENDING => *wire = objects.next().unwrap_or(Wire::Nil),
        Wire::Table(entries) => {
            for (_, item) in entries.iter_mut() {
                fill_objects(item, objects);
            }
        }
        _ => {}
    }
}

const PLACEHOLDER: Wire = Wire::Instance(InstanceId { index: PENDING, generation: PENDING });

/// The table at stack slot `index` as a wire table (wire.rs `capture_entries`' rules)
unsafe fn walk_table(state: *mut ffi::lua_State, index: c_int, side: c_int, ancestors: &mut Vec<*const c_void>, depth: usize, walk: &mut Walk) -> Result<Wire, String> {
    unsafe {
        let pointer = ffi::lua_topointer(state, index);
        if ancestors.contains(&pointer) {
            return Err("tables cannot be cyclic".into());
        }
        if ancestors.len() >= depth {
            return Err("Max lua stack size reached".into());
        }
        if ffi::lua_checkstack(state, 4) == 0 {
            return Err("Max lua stack size reached".into());
        }
        ancestors.push(pointer);
        let length = ffi::lua_objlen(state, index) as usize;
        let mut entries: Vec<(Key, Wire)> = Vec::new();
        if length > 0 {
            entries.reserve_exact(length);
            // (each item at the slot above the table's walk; lua_rawgeti gives its type)
            let top = ffi::lua_gettop(state) + 1;
            for item in 1..=length {
                let kind = ffi::lua_rawgeti_(state, index, item as c_int);
                let wire = walk_typed(state, top, kind, side, ancestors, depth, walk);
                ffi::lua_settop(state, top - 1);
                let wire = wire?;
                if !matches!(wire, Wire::Nil) {
                    entries.push((Key::Index(item as u32), wire));
                }
            }
        } else {
            // (only a number key can name the same text as another key: string keys are unique)
            let mut numbered = false;
            ffi::lua_pushnil(state);
            while ffi::lua_next(state, index) != 0 {
                let top = ffi::lua_gettop(state);
                numbered |= ffi::lua_type(state, top - 1) == ffi::LUA_TNUMBER;
                let key = match key_name(state, top - 1) {
                    Some(key) => key,
                    None => {
                        // (other keys are named as bindable.rs names them: through mlua, rare)
                        slow_key(state, top - 1)?
                    }
                };
                let wire = walk_value(state, top, side, ancestors, depth, walk);
                ffi::lua_settop(state, top - 1);
                let wire = wire?;
                if !matches!(wire, Wire::Nil) {
                    entries.push((Key::Name(key), wire));
                }
            }
            if numbered {
                keep_later(&mut entries);
            }
        }
        ancestors.pop();
        Ok(Wire::Table(entries.into_boxed_slice()))
    }
}

/// Of entries whose keys name the same text, the later one, in its own place (as wire.rs `capture_entries`
/// leaves them: a table's set keeps the later value)
fn keep_later(entries: &mut Vec<(Key, Wire)>) {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut kept: Vec<(Key, Wire)> = Vec::with_capacity(entries.len());
    for (key, wire) in entries.drain(..).rev() {
        let name = match &key {
            Key::Name(name) => name.clone(),
            Key::Index(_) => {
                kept.push((key, wire));
                continue;
            }
        };
        if seen.insert(name) {
            kept.push((key, wire));
        }
    }
    kept.reverse();
    *entries = kept;
}

/// A string or number key's text without mlua (a number truncated to a 64-bit integer, as bindable.rs `key_name`)
unsafe fn key_name(state: *mut ffi::lua_State, index: c_int) -> Option<Box<[u8]>> {
    unsafe {
        match ffi::lua_type(state, index) {
            ffi::LUA_TSTRING => {
                let mut length = 0usize;
                let text = ffi::lua_tolstring(state, index, &mut length) as *const u8;
                Some(std::slice::from_raw_parts(text, length).into())
            }
            ffi::LUA_TNUMBER => {
                let number = ffi::lua_tonumber(state, index);
                let integer = if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number) { number as i64 } else { i64::MIN };
                Some(integer.to_string().into_bytes().into_boxed_slice())
            }
            _ => None,
        }
    }
}

/// What a key other than a string or a number gives: a boolean's or thread's error, else [`FALLBACK`] (its text is named
/// through mlua, as bindable.rs names it: the typed copier takes the whole value)
unsafe fn slow_key(state: *mut ffi::lua_State, index: c_int) -> Result<Box<[u8]>, String> {
    unsafe {
        match ffi::lua_type(state, index) {
            ffi::LUA_TBOOLEAN | ffi::LUA_TTHREAD => Err("Invalid table key type used".into()),
            _ => Err(FALLBACK.into()),
        }
    }
}

/// The walk met a key the typed copier names
const FALLBACK: &str = "\u{0}fallback";

/// The value at stack slot `index` (wire.rs `capture`'s rules): an object other than an instance goes to the side table
unsafe fn walk_value(state: *mut ffi::lua_State, index: c_int, side: c_int, ancestors: &mut Vec<*const c_void>, depth: usize, walk: &mut Walk) -> Result<Wire, String> {
    unsafe { walk_typed(state, index, ffi::lua_type(state, index), side, ancestors, depth, walk) }
}

/// [`walk_value`] of a value whose type is `kind`
#[inline]
unsafe fn walk_typed(state: *mut ffi::lua_State, index: c_int, kind: c_int, side: c_int, ancestors: &mut Vec<*const c_void>, depth: usize, walk: &mut Walk) -> Result<Wire, String> {
    unsafe {
        Ok(match kind {
            ffi::LUA_TNIL => Wire::Nil,
            ffi::LUA_TBOOLEAN => Wire::Bool(ffi::lua_toboolean(state, index) != 0),
            ffi::LUA_TNUMBER => {
                let number = ffi::lua_tonumber(state, index);
                Wire::Number(if number != 0.0 && number.is_finite() && number.abs() < f64::MIN_POSITIVE { 0.0 } else { number })
            }
            ffi::LUA_TSTRING => {
                let mut length = 0usize;
                let text = ffi::lua_tolstring(state, index, &mut length) as *const u8;
                Wire::String(std::slice::from_raw_parts(text, length).into())
            }
            ffi::LUA_TVECTOR => {
                let vector = ffi::lua_tovector(state, index);
                Wire::Vector([*vector, *vector.add(1), *vector.add(2)])
            }
            ffi::LUA_TBUFFER => {
                let mut length = 0usize;
                let data = ffi::lua_tobuffer(state, index, &mut length) as *const u8;
                Wire::Buffer(std::slice::from_raw_parts(data, length).into())
            }
            ffi::LUA_TTABLE => walk_table(state, index, side, ancestors, depth, walk)?,
            ffi::LUA_TUSERDATA => match super::super::lua::raw::stack_instance(state, index) {
                Some(id) => Wire::Instance(id),
                None => {
                    walk.objects += 1;
                    ffi::lua_pushvalue(state, index);
                    ffi::lua_rawseti(state, side, walk.objects as _);
                    PLACEHOLDER
                }
            },
            // functions, threads, light userdata: nil
            _ => Wire::Nil,
        })
    }
}

/// A wire table rebuilt in the receiver's VM, instances mapped by `map`: the objects made by mlua first, then the table
/// with raw sets
pub fn rebuild_table(lua: &Lua, engine: &Engine, wire: &Wire, map: &dyn Fn(InstanceId) -> Option<InstanceId>) -> LuaResult<LuaValue> {
    let mut objects: Vec<LuaValue> = Vec::new();
    collect_objects(lua, engine, wire, map, &mut objects)?;
    let side = if objects.is_empty() { LuaValue::Nil } else { LuaValue::Table(lua.create_sequence_from(objects)?) };
    unsafe {
        lua.exec_raw(side, |state| {
            let mut next = 1;
            push_wire(state, wire, &mut next);
            ffi::lua_replace(state, 1);
            ffi::lua_settop(state, 1);
        })
    }
}

/// The values of a wire tree mlua makes (in [`push_wire`]'s order)
fn collect_objects(lua: &Lua, engine: &Engine, wire: &Wire, map: &dyn Fn(InstanceId) -> Option<InstanceId>, out: &mut Vec<LuaValue>) -> LuaResult<()> {
    match wire {
        Wire::Table(entries) => {
            for (_, item) in entries.iter() {
                collect_objects(lua, engine, item, map, out)?;
            }
        }
        Wire::Instance(id) => out.push(match map(*id) {
            Some(mapped) if engine.state.borrow().dom.get(mapped).is_some() => crate::lua::instance_value(lua, engine, mapped)?,
            _ => LuaValue::Boolean(false),
        }),
        Wire::Value(_) | Wire::Random(_) | Wire::TweenInfo { .. } | Wire::DateTime(_) => out.push(super::wire::rebuilt(lua, engine, wire, map)?),
        _ => {}
    }
    Ok(())
}

/// Pushes a wire value; `next` is the next object's slot in the side table at stack slot 1. An instance the receiver lacks
/// was made `false`: it pushes nil
unsafe fn push_wire(state: *mut ffi::lua_State, wire: &Wire, next: &mut i64) {
    unsafe {
        match wire {
            Wire::Nil => ffi::lua_pushnil(state),
            Wire::Bool(flag) => ffi::lua_pushboolean(state, *flag as c_int),
            Wire::Number(number) => ffi::lua_pushnumber(state, *number),
            Wire::Vector([x, y, z]) => ffi::lua_pushvector(state, *x, *y, *z),
            Wire::String(text) => {
                ffi::lua_pushlstring(state, text.as_ptr() as *const _, text.len());
            }
            Wire::Buffer(bytes) => {
                let data = ffi::lua_newbuffer(state, bytes.len()) as *mut u8;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            }
            Wire::Table(entries) => {
                let indexed = entries.iter().filter(|(key, _)| matches!(key, Key::Index(_))).count();
                // (room for the table, a key and a value: each nested table checks its own)
                ffi::lua_checkstack(state, 4);
                ffi::lua_createtable(state, indexed as c_int, (entries.len() - indexed) as c_int);
                let table = ffi::lua_gettop(state);
                for (key, item) in entries.iter() {
                    push_wire(state, item, next);
                    if ffi::lua_type(state, -1) == ffi::LUA_TNIL {
                        ffi::lua_pop(state, 1);
                        continue;
                    }
                    match key {
                        Key::Index(index) => ffi::lua_rawseti_(state, table, *index as c_int),
                        Key::Name(name) => {
                            ffi::lua_pushlstring(state, name.as_ptr() as *const _, name.len());
                            ffi::lua_insert(state, -2);
                            ffi::lua_rawset(state, table);
                        }
                    }
                }
            }
            Wire::Instance(_) | Wire::Value(_) | Wire::Random(_) | Wire::TweenInfo { .. } | Wire::DateTime(_) => {
                ffi::lua_rawgeti(state, 1, *next as _);
                *next += 1;
                if matches!(wire, Wire::Instance(_)) && ffi::lua_type(state, -1) == ffi::LUA_TBOOLEAN {
                    ffi::lua_pop(state, 1);
                    ffi::lua_pushnil(state);
                }
            }
        }
    }
}

//! What a remote call's arguments (and a RemoteFunction's results) are while they cross from one VM to the other: plain
//! Rust values, captured at the call (a change the sender makes afterwards does not arrive) and rebuilt in the receiver's
//! VM, a table and a buffer anew for each handler. Measured in a playing session's two sides, each way alike (Studio
//! 0.747, tools/parity/probes/loopback/remote_values.luau, remote_tables.luau, remote_depth.luau):
//!
//! - The count of arguments, trailing nils included, arrives. Numbers arrive as they are, NaN, infinities and -0 too, but a
//!   subnormal double arrives as 0; strings and buffers byte for byte; a vector as a Vector3 (floats).
//! - A function, a thread, a newproxy userdata, an Enum and a RaycastParams arrive as nil. CFrame, Color3, UDim, UDim2,
//!   Vector2, BrickColor, NumberRange, NumberSequence, ColorSequence, Rect, Ray, Font, DateTime, PhysicalProperties,
//!   Random, TweenInfo and EnumItems arrive equal.
//! - An Instance arrives as the receiver's own when it has it (mapped at delivery, after the batch's earlier creations), else
//!   as nil: a server-only instance, a parentless one, a client-made one.
//! - Tables as BindableEvent copies them (lua/bindable.rs: the border decides array or dictionary, a dictionary's keys
//!   become strings, `Invalid table key type used`, `tables cannot be cyclic`), except that a function or an instance the
//!   receiver lacks leaves a hole in an array, a function's dictionary entry is dropped, and a table nested deeper than
//!   [`MAX_DEPTH`] is refused at the sender.

use std::rc::Rc;

use lune_roblox::datatypes::types as lune;
use mlua::prelude::*;
use rbx_dom_weak::types::{self as dom, Variant};

use crate::{Engine, InstanceId, lua::bindable, value::Value};

/// A sent value.
#[derive(Clone, Debug)]
pub enum Wire {
    Nil,
    Bool(bool),
    Number(f64),
    Vector([f32; 3]),
    String(Box<[u8]>),
    Buffer(Box<[u8]>),
    /// The sender's instance: the receiver's copy of it, or nil
    Instance(InstanceId),
    /// A datatype or EnumItem (never `Value::Lua`)
    Value(Value),
    Random(crate::lua::types::RandomState),
    TweenInfo { time: f64, style: crate::reflection::EnumItemId, direction: crate::reflection::EnumItemId, repeats: f64, reverses: bool, delay: f64 },
    DateTime(f64),
    Table(Box<[(Key, Wire)]>),
}

#[derive(Clone, Debug)]
pub enum Key {
    Index(u32),
    Name(Box<[u8]>),
}

/// The deepest a table sent by FireServer or InvokeServer may nest: one nested this deep is sent, one more is refused at the
/// sender with `Max lua stack size reached`. FireClient and InvokeClient, which take the player first, send one level less
/// ([`MAX_DEPTH_AFTER_PLAYER`]). Bisected each way, three captures alike (Studio 0.747:
/// tools/parity/probes/loopback/remote_depth.luau); FireAllClients and a callback's results are taken as FireServer's (not
/// measured: a callback's 1000-deep result raised the text at the caller).
pub const MAX_DEPTH: usize = 299;
pub const MAX_DEPTH_AFTER_PLAYER: usize = 298;

/// The arguments of a call, captured, its tables nested at most `depth` deep: an error's text when one cannot be sent.
pub fn capture_all(lua: &Lua, engine: &Engine, values: &[LuaValue], depth: usize) -> Result<Box<[Wire]>, String> {
    // (tables through the C API's copier: copier.rs)
    super::copier::capture_all(lua, engine, values, depth)
}

/// A received table each handler rebuilds (copier.rs), its instances already the receiver's
pub struct Delivered(pub Rc<Wire>);

impl LuaUserData for Delivered {}

/// `wire`'s instances made the receiver's (`map`), an instance it lacks nil
fn map_instances(wire: &mut Wire, map: &dyn Fn(InstanceId) -> Option<InstanceId>) {
    if let Wire::Table(entries) = wire {
        for (_, item) in entries.iter_mut() {
            match item {
                Wire::Instance(id) => *item = map(*id).map_or(Wire::Nil, Wire::Instance),
                Wire::Table(_) => map_instances(item, map),
                _ => {}
            }
        }
    }
}

/// A value as it is sent. `ancestors`: the tables it is inside of.
pub fn capture(lua: &Lua, engine: &Engine, value: &LuaValue, ancestors: &mut Vec<*const std::ffi::c_void>, depth: usize) -> Result<Wire, String> {
    Ok(match value {
        LuaValue::Nil => Wire::Nil,
        LuaValue::Boolean(flag) => Wire::Bool(*flag),
        LuaValue::Integer(number) => Wire::Number(*number as f64),
        LuaValue::Number(number) => Wire::Number(number_sent(*number)),
        LuaValue::String(text) => Wire::String(text.as_bytes().to_vec().into_boxed_slice()),
        LuaValue::Buffer(buffer) => Wire::Buffer(buffer.to_vec().into_boxed_slice()),
        LuaValue::Vector(vector) => Wire::Vector([vector.x(), vector.y(), vector.z()]),
        LuaValue::Table(table) => capture_table(lua, engine, table, ancestors, depth)?,
        LuaValue::UserData(userdata) => capture_userdata(userdata),
        // functions, threads, light userdata, errors: nil
        _ => Wire::Nil,
    })
}

/// A number as it arrives: a subnormal one as 0 (measured), the rest as they are
#[inline]
fn number_sent(number: f64) -> f64 {
    if number != 0.0 && number.is_finite() && number.abs() < f64::MIN_POSITIVE { 0.0 } else { number }
}

/// A stored property or attribute value as the other VM can hold it: a datatype a script made (kept by the engine as its VM's
/// userdata: a part's CustomPhysicalProperties, say) as an engine value; a table, function or other VM value as None
pub fn portable(value: &Value) -> Option<Value> {
    match value {
        Value::Lua(lua_value) => match &**lua_value {
            LuaValue::UserData(userdata) => match capture_userdata(userdata) {
                Wire::Value(value) => Some(value),
                _ => None,
            },
            LuaValue::Vector(vector) => Some(Value::Vector3(dom::Vector3::new(vector.x(), vector.y(), vector.z()))),
            _ => None,
        },
        other => Some(other.clone()),
    }
}

fn capture_userdata(userdata: &LuaAnyUserData) -> Wire {
    use crate::lua::types::{LDateTime, LRandom, LTweenInfo};
    if let Some(id) = crate::lua::raw::instance_of_ud(userdata) {
        return Wire::Instance(id);
    }
    if let Ok(random) = userdata.borrow::<LRandom>() {
        return Wire::Random(random.0.get());
    }
    if let Ok(info) = userdata.borrow::<LTweenInfo>() {
        return Wire::TweenInfo { time: info.time(), style: info.easing_style, direction: info.easing_direction, repeats: info.repeats(), reverses: info.reverses, delay: info.delay() };
    }
    if let Ok(time) = userdata.borrow::<LDateTime>() {
        return Wire::DateTime(time.0);
    }
    let dom = |variant: Variant| Wire::Value(Value::Dom(Rc::new(variant)));
    if let Ok(v) = userdata.borrow::<lune::Ray>() {
        return dom(Variant::Ray(dom::Ray::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::PhysicalProperties>() {
        return dom(Variant::PhysicalProperties(dom::PhysicalProperties::Custom(dom::CustomPhysicalProperties::from(*v))));
    }
    if let Ok(v) = userdata.borrow::<lune::Faces>() {
        return dom(Variant::Faces(dom::Faces::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::Axes>() {
        return dom(Variant::Axes(dom::Axes::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::Region3>() {
        return dom(Variant::Region3(dom::Region3::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::Region3int16>() {
        return dom(Variant::Region3int16(dom::Region3int16::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::Vector2int16>() {
        return dom(Variant::Vector2int16(dom::Vector2int16::from(*v)));
    }
    if let Ok(v) = userdata.borrow::<lune::Vector3int16>() {
        return dom(Variant::Vector3int16(dom::Vector3int16::from(*v)));
    }
    match crate::lua::convert::from_lua(&LuaValue::UserData(userdata.clone())) {
        // (EnumItems, Font, Content and the datatypes the engine stores: VM-independent values)
        Value::Lua(_) | Value::Instance(_) => Wire::Nil,
        value => Wire::Value(value),
    }
}

fn capture_table(lua: &Lua, engine: &Engine, table: &LuaTable, ancestors: &mut Vec<*const std::ffi::c_void>, depth: usize) -> Result<Wire, String> {
    let pointer = table.to_pointer();
    if ancestors.contains(&pointer) {
        return Err("tables cannot be cyclic".into());
    }
    if ancestors.len() >= depth {
        return Err("Max lua stack size reached".into());
    }
    ancestors.push(pointer);
    let entries = capture_entries(lua, engine, table, ancestors, depth);
    ancestors.pop();
    Ok(Wire::Table(entries?.into_boxed_slice()))
}

fn capture_entries(lua: &Lua, engine: &Engine, table: &LuaTable, ancestors: &mut Vec<*const std::ffi::c_void>, depth: usize) -> Result<Vec<(Key, Wire)>, String> {
    let length = table.raw_len();
    if length > 0 {
        // an array: indices 1 to the border, a value that does not arrive leaving a hole
        let mut entries = Vec::with_capacity(length);
        for index in 1..=length {
            let item: LuaValue = table.raw_get(index).map_err(|error| error.to_string())?;
            let wire = capture(lua, engine, &item, ancestors, depth)?;
            if !matches!(wire, Wire::Nil) {
                entries.push((Key::Index(index as u32), wire));
            }
        }
        return Ok(entries);
    }
    let mut entries = Vec::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, item) = pair.map_err(|error| error.to_string())?;
        let key = bindable::key_name(lua, engine, &key)?;
        let wire = capture(lua, engine, &item, ancestors, depth)?;
        if !matches!(wire, Wire::Nil) {
            // (keys that end up equal keep the later one, as a table's set does)
            entries.retain(|(existing, _): &(Key, Wire)| !matches!(existing, Key::Name(name) if **name == *key));
            entries.push((Key::Name(key.into_boxed_slice()), wire));
        }
    }
    Ok(entries)
}

/// A value for the receiver's handlers: a table or buffer as a copy each handler rebuilds ([`Delivered`], copier.rs),
/// an instance as the receiver's own (`map`: the sender's id to the receiver's) or nil.
pub fn delivered(lua: &Lua, engine: &Engine, wire: Wire, map: &dyn Fn(InstanceId) -> Option<InstanceId>) -> LuaResult<Value> {
    Ok(match wire {
        Wire::Nil => Value::Nil,
        Wire::Bool(flag) => Value::Bool(flag),
        Wire::Number(number) => Value::Number(number),
        Wire::Vector([x, y, z]) => Value::Vector3(dom::Vector3::new(x, y, z)),
        Wire::String(text) => Value::String(crate::value::Str::new(&text)),
        Wire::Instance(id) => map(id).map_or(Value::Nil, Value::Instance),
        Wire::Value(value) => value,
        // (the message's own copy, kept: each handler rebuilds its table or buffer from it, copier.rs)
        mut kept @ (Wire::Table(_) | Wire::Buffer(_)) => {
            map_instances(&mut kept, map);
            Value::Lua(Box::new(LuaValue::UserData(lua.create_userdata(Delivered(Rc::new(kept)))?)))
        }
        wire @ (Wire::Random(_) | Wire::TweenInfo { .. } | Wire::DateTime(_)) => Value::Lua(Box::new(object(lua, engine, &wire)?)),
    })
}

/// A value rebuilt now in the receiver's VM (a RemoteFunction's results and arguments go straight to their thread)
pub fn rebuilt(lua: &Lua, engine: &Engine, wire: &Wire, map: &dyn Fn(InstanceId) -> Option<InstanceId>) -> LuaResult<LuaValue> {
    match wire {
        Wire::Table(_) => super::copier::rebuild_table(lua, engine, wire, map),
        Wire::Buffer(bytes) => lua.create_buffer(&bytes[..]).map(LuaValue::Buffer),
        Wire::Nil => Ok(LuaValue::Nil),
        Wire::Bool(flag) => Ok(LuaValue::Boolean(*flag)),
        Wire::Number(number) => Ok(LuaValue::Number(*number)),
        Wire::String(text) => lua.create_string(&**text).map(LuaValue::String),
        other => {
            let value = delivered(lua, engine, other.clone(), map)?;
            crate::lua::convert::to_lua(lua, engine, &value)
        }
    }
}

/// A Random, TweenInfo or DateTime made in the receiver's VM (a value of its own: no handler shares another's)
fn object(lua: &Lua, engine: &Engine, wire: &Wire) -> LuaResult<LuaValue> {
    use crate::lua::types::{LDateTime, LTweenInfo};
    Ok(match wire {
        Wire::Random(state) => LuaValue::UserData(lua.create_any_userdata(crate::lua::types::random_with_state(engine, *state))?),
        Wire::TweenInfo { time, style, direction, repeats, reverses, delay } => LuaValue::UserData(lua.create_any_userdata(LTweenInfo {
            time: LuaValue::Number(*time),
            easing_style: *style,
            easing_direction: *direction,
            repeat_count: LuaValue::Number(*repeats),
            reverses: *reverses,
            delay_time: LuaValue::Number(*delay),
        })?),
        Wire::DateTime(millis) => LuaValue::UserData(lua.create_any_userdata(LDateTime(*millis))?),
        _ => LuaValue::Nil,
    })
}

/// What a call's arguments come to on the network, for UnreliableRemoteEvent's ceiling ([`UNRELIABLE_LIMIT`]). Measured
/// (tools/parity/probes/loopback/remote_limits.luau, a sweep across the cutoff): a string costs 1 + its length's varint + its
/// bytes, a buffer one byte more, a number 9, a table 1 + its count's varint + its entries (a dictionary's keys as
/// strings), an instance reference 6. Not measured, taken from those: nil and booleans 1 and 2, a vector 13, other values
/// their datatype's float count times 4 plus 1. A buffer that compresses well can pass a real client where ree drops it
/// (a client compresses buffers).
pub fn encoded_size(values: &[Wire]) -> usize {
    values.iter().map(size_of).sum()
}

/// The UnreliableRemoteEvent ceiling: 1000 bytes of arguments (Roblox's documentation, and the measured cutoffs)
pub const UNRELIABLE_LIMIT: usize = 1000;

fn varint(value: usize) -> usize {
    let mut bytes = 1;
    let mut rest = value >> 7;
    while rest > 0 {
        bytes += 1;
        rest >>= 7;
    }
    bytes
}

fn size_of(wire: &Wire) -> usize {
    match wire {
        Wire::Nil => 1,
        Wire::Bool(_) => 2,
        Wire::Number(_) => 9,
        Wire::Vector(_) => 13,
        Wire::String(text) => 1 + varint(text.len()) + text.len(),
        Wire::Buffer(bytes) => 2 + varint(bytes.len()) + bytes.len(),
        Wire::Instance(_) => 6,
        Wire::Value(value) => 1 + 4 * floats_of(value),
        Wire::Random(_) => 17,
        Wire::TweenInfo { .. } => 1 + 8 * 4 + 3,
        Wire::DateTime(_) => 9,
        Wire::Table(entries) => {
            1 + varint(entries.len())
                + entries
                    .iter()
                    .map(|(key, item)| match key {
                        Key::Index(_) => size_of(item),
                        Key::Name(name) => 1 + varint(name.len()) + name.len() + size_of(item),
                    })
                    .sum::<usize>()
        }
    }
}

/// How many float32s a datatype holds (its encoding's size: not measured)
fn floats_of(value: &Value) -> usize {
    match value {
        Value::EnumItem(_) => 1,
        Value::UDim(_) => 2,
        Value::UDim2(_) => 4,
        Value::Vector2(_) => 2,
        Value::Vector3(_) | Value::Color3(_) => 3,
        Value::Rect(_) => 4,
        Value::NumberRange(_) => 2,
        Value::NumberSequence(sequence) => 3 * sequence.keypoints.len() + 1,
        Value::ColorSequence(sequence) => 4 * sequence.keypoints.len() + 1,
        Value::Dom(variant) => match &**variant {
            Variant::CFrame(_) => 12,
            Variant::Ray(_) => 6,
            _ => 4,
        },
        _ => 4,
    }
}

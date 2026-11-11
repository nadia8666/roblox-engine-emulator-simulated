//! FloatCurveKey and RotationCurveKey: the keys scripts hand to and get from a FloatCurve and a
//! RotationCurve (crate::animation::curves). Measured in Studio 0.747 (2026-11-11):
//!
//! - `new(time, value, interpolation?)` takes two or three arguments ("FloatCurveKey.new expects 2
//!   or 3 input arguments"); without the third the key is Cubic. A time or value that is not a
//!   number (or a string of one) is 0; a RotationCurveKey's value that is not a CFrame is the
//!   identity. The third must be an Enum.KeyInterpolationMode item: another enum's item raises
//!   "Invalid type 'Material' for enum 'KeyInterpolationMode'", anything else (a name, a number,
//!   nil) "Invalid value for enum 'KeyInterpolationMode'".
//! - Time, Value, Interpolation, LeftTangent and RightTangent read and write; numbers are float32. A
//!   Time or Value written as something that is no number reads nil afterwards. A FloatCurveKey's
//!   tangents start unset (nil) and writing nil leaves a set one as it is; a RotationCurveKey's
//!   start at 0 and writing nil unsets one. Writing RightTangent on a key that is not Cubic raises;
//!   making a key not Cubic unsets its right tangent.
//! - A RotationCurveKey keeps only its CFrame's rotation, as a unit quaternion.
//! - tostring is `{Linear, 0, 1, nil, nil}` (mode, time, value, tangents) and, for a rotation, the
//!   CFrame's twelve components in the value's place; numbers print with nine significant digits
//!   (`0.100000001`, `1e+09`, `NAN`, `INF`). Two keys with the same fields are equal, unless a Time
//!   or Value among them reads nil (such a key does not equal itself).

use std::rc::Rc;

use glam::{Affine3A, Quat};
use mlua::prelude::*;

use super::{Args, Flow, Ret, convert, enum_item_value, lock_metatable, none, raise, types::LEnumItem, values, wrapped};
use crate::{
    Engine,
    animation::curve::{CUBIC, FloatKey, RotationKey, tick_of, time_of},
    value::Value,
    world::frame,
};

#[derive(Clone, Copy, PartialEq)]
pub struct LFloatCurveKey {
    pub time: Option<f32>,
    pub value: Option<f32>,
    pub mode: u8,
    pub left: Option<f32>,
    pub right: Option<f32>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct LRotationCurveKey {
    pub time: Option<f32>,
    pub rotation: Quat,
    pub mode: u8,
    pub left: Option<f32>,
    pub right: Option<f32>,
}

impl LFloatCurveKey {
    /// The key as a curve stores it, with the time it was given.
    pub fn stored(&self) -> (f32, FloatKey) {
        let time = self.time.unwrap_or(0.0);
        (time, FloatKey { tick: tick_of(time), value: self.value.unwrap_or(0.0), mode: self.mode, left: self.left, right: self.right })
    }
}

impl LRotationCurveKey {
    pub fn stored(&self) -> (f32, RotationKey) {
        let time = self.time.unwrap_or(0.0);
        (time, RotationKey { tick: tick_of(time), rotation: self.rotation, mode: self.mode, left: self.left, right: self.right })
    }
}

pub fn float_key_value(lua: &Lua, key: &FloatKey) -> LuaResult<LuaValue> {
    let key = LFloatCurveKey { time: Some(time_of(key.tick)), value: Some(key.value), mode: key.mode, left: key.left, right: key.right };
    let userdata = lua.create_any_userdata(key)?;
    lock_metatable(lua, &userdata)?;
    Ok(LuaValue::UserData(userdata))
}

pub fn rotation_key_value(lua: &Lua, key: &RotationKey) -> LuaResult<LuaValue> {
    let key = LRotationCurveKey { time: Some(time_of(key.tick)), rotation: key.rotation, mode: key.mode, left: key.left, right: key.right };
    let userdata = lua.create_any_userdata(key)?;
    lock_metatable(lua, &userdata)?;
    Ok(LuaValue::UserData(userdata))
}

/// A number as the keys take one: a number or a string of one.
fn number_of(lua: &Lua, value: &LuaValue) -> Option<f32> {
    match value {
        LuaValue::Number(n) => Some(*n as f32),
        LuaValue::Integer(n) => Some(*n as f32),
        LuaValue::String(_) => lua.coerce_number(value.clone()).ok().flatten().map(|n| n as f32),
        _ => None,
    }
}

fn mode_of(engine: &Engine, value: &LuaValue) -> Result<u8, Flow> {
    let item = match value {
        LuaValue::UserData(ud) => ud.borrow::<LEnumItem>().ok().map(|item| item.0),
        _ => None,
    };
    let Some(item) = item else { return raise("Invalid value for enum 'KeyInterpolationMode'") };
    let info = engine.reflection.enum_item_info(item);
    let enum_name = &engine.reflection.enums[info.enum_type as usize].name;
    if &**enum_name != "KeyInterpolationMode" {
        return raise(format!("Invalid type '{enum_name}' for enum 'KeyInterpolationMode'"));
    }
    Ok(info.value as u8)
}

fn mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "Constant",
        1 => "Linear",
        _ => "Cubic",
    }
}

fn mode_value(lua: &Lua, engine: &Engine, mode: u8) -> LuaResult<LuaValue> {
    match engine.reflection.enum_item("KeyInterpolationMode", mode_name(mode)) {
        Some(item) => enum_item_value(lua, engine, item),
        None => Ok(LuaValue::Nil),
    }
}

fn optional(value: Option<f32>) -> LuaValue {
    value.map_or(LuaValue::Nil, |value| LuaValue::Number(value as f64))
}

/// A float32 with nine significant digits, as the keys print.
fn digits(value: f32) -> String {
    if value.is_nan() {
        return "NAN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-INF".into() } else { "INF".into() };
    }
    lune_roblox::datatypes::types::format_g(value as f64, 9)
}

fn optional_digits(value: Option<f32>) -> String {
    value.map_or_else(|| "nil".into(), digits)
}

/// A CFrame's rotation as a unit quaternion; the identity for anything else.
pub fn rotation_of(value: &LuaValue) -> Quat {
    let stored = convert::from_lua(value);
    if !matches!(&stored, Value::Dom(variant) if matches!(&**variant, rbx_dom_weak::types::Variant::CFrame(_))) {
        return Quat::IDENTITY;
    }
    let rotation = Quat::from_mat3a(&frame::affine_of(Some(&stored)).matrix3);
    if rotation.is_finite() && rotation.length_squared() > 1e-12 { rotation.normalize() } else { Quat::IDENTITY }
}

pub fn rotation_value(lua: &Lua, engine: &Engine, rotation: Quat) -> LuaResult<LuaValue> {
    convert::to_lua(lua, engine, &frame::cframe_value(&Affine3A::from_quat(rotation)))
}

fn member(key: &LuaValue) -> String {
    match key {
        LuaValue::String(name) => name.to_string_lossy(),
        other => other.type_name().into(),
    }
}

/// `new`'s time, its value argument and the mode: two or three arguments.
fn new_arguments<'a>(lua: &Lua, engine: &Engine, type_name: &str, args: &'a Args) -> Result<(f32, &'a LuaValue, u8), Flow> {
    if args.len() != 2 && args.len() != 3 {
        return raise(format!("{type_name}.new expects 2 or 3 input arguments"));
    }
    let mode = if args.len() == 3 { mode_of(engine, args.get(2))? } else { CUBIC };
    Ok((number_of(lua, args.get(0)).unwrap_or(0.0), args.get(1), mode))
}

/// Writes one of the fields both key types share; false when `name` is not one of them.
#[allow(clippy::too_many_arguments)]
fn write_shared(
    lua: &Lua,
    engine: &Engine,
    name: &[u8],
    value: &LuaValue,
    time: &mut Option<f32>,
    mode: &mut u8,
    left: &mut Option<f32>,
    right: &mut Option<f32>,
    nil_unsets: bool,
    not_cubic: &str,
) -> Result<bool, Flow> {
    let tangent = |slot: &mut Option<f32>| match number_of(lua, value) {
        Some(number) => *slot = Some(number),
        None if nil_unsets => *slot = None,
        None => {}
    };
    match name {
        b"Time" => *time = number_of(lua, value),
        b"Interpolation" => {
            *mode = mode_of(engine, value)?;
            if *mode != CUBIC {
                *right = None;
            }
        }
        b"LeftTangent" => tangent(left),
        b"RightTangent" => {
            if *mode != CUBIC && number_of(lua, value).is_some() {
                return raise(not_cubic);
            }
            tangent(right);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn library(lua: &Lua, engine: &Rc<Engine>, name: &'static str, new: impl Fn(&Lua, LuaMultiValue) -> Ret + 'static) -> LuaResult<()> {
    let library = lua.create_table()?;
    library.set("new", wrapped(lua, engine, new)?)?;
    library.set_readonly(true);
    engine.lua.borrow_mut().libraries.insert(name, library);
    Ok(())
}

fn register_float(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let e = engine.clone();
    let index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid FloatCurveKey") };
        let key = *ud.borrow::<LFloatCurveKey>()?;
        if let LuaValue::String(name) = args.get(1) {
            match &*name.as_bytes() {
                b"Time" => return Ok(values([optional(key.time)])),
                b"Value" => return Ok(values([optional(key.value)])),
                b"Interpolation" => return Ok(values([mode_value(lua, &e, key.mode)?])),
                b"LeftTangent" => return Ok(values([optional(key.left)])),
                b"RightTangent" => return Ok(values([optional(key.right)])),
                _ => {}
            }
        }
        raise(format!("'{}' is not a valid member of FloatCurveKey", member(args.get(1))))
    })?;
    let e = engine.clone();
    let newindex = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid FloatCurveKey") };
        let mut key = ud.borrow_mut::<LFloatCurveKey>()?;
        let key = &mut *key;
        let name = match args.get(1) {
            LuaValue::String(name) => name.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let value = args.get(2);
        if name == b"Value" {
            key.value = number_of(lua, value);
            return Ok(none());
        }
        let not_cubic = "Attempting to set a right tangent value on a curve key for which the interpolation mode is not Cubic";
        if write_shared(lua, &e, &name, value, &mut key.time, &mut key.mode, &mut key.left, &mut key.right, false, not_cubic)? {
            return Ok(none());
        }
        raise(format!("{} is not a valid member of FloatCurveKey", member(args.get(1))))
    })?;
    let tostring = lua.create_function(|_, this: LuaAnyUserData| {
        let key = this.borrow::<LFloatCurveKey>()?;
        Ok(format!(
            "{{{}, {}, {}, {}, {}}}",
            mode_name(key.mode),
            optional_digits(key.time),
            optional_digits(key.value),
            optional_digits(key.left),
            optional_digits(key.right)
        ))
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| {
        let (LuaValue::UserData(a), LuaValue::UserData(b)) = (a, b) else { return Ok(false) };
        let (Ok(a), Ok(b)) = (a.borrow::<LFloatCurveKey>(), b.borrow::<LFloatCurveKey>()) else { return Ok(false) };
        Ok(*a == *b && a.time.is_some() && a.value.is_some())
    })?;
    lua.register_userdata_type::<LFloatCurveKey>(|registry| {
        registry.add_meta_field("__type", "FloatCurveKey");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;
    let e = engine.clone();
    library(lua, engine, "FloatCurveKey", move |lua, args| {
        let args = Args::new(args);
        let (time, value, mode) = new_arguments(lua, &e, "FloatCurveKey", &args)?;
        let key = LFloatCurveKey { time: Some(time), value: Some(number_of(lua, value).unwrap_or(0.0)), mode, left: None, right: None };
        let userdata = lua.create_any_userdata(key)?;
        lock_metatable(lua, &userdata)?;
        Ok(values([LuaValue::UserData(userdata)]))
    })
}

fn register_rotation(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let e = engine.clone();
    let index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid RotationCurveKey") };
        let key = *ud.borrow::<LRotationCurveKey>()?;
        if let LuaValue::String(name) = args.get(1) {
            match &*name.as_bytes() {
                b"Time" => return Ok(values([optional(key.time)])),
                b"Value" => return Ok(values([rotation_value(lua, &e, key.rotation)?])),
                b"Interpolation" => return Ok(values([mode_value(lua, &e, key.mode)?])),
                b"LeftTangent" => return Ok(values([optional(key.left)])),
                b"RightTangent" => return Ok(values([optional(key.right)])),
                _ => {}
            }
        }
        raise(format!("'{}' is not a valid member of RotationCurveKey", member(args.get(1))))
    })?;
    let e = engine.clone();
    let newindex = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid RotationCurveKey") };
        let mut key = ud.borrow_mut::<LRotationCurveKey>()?;
        let key = &mut *key;
        let name = match args.get(1) {
            LuaValue::String(name) => name.as_bytes().to_vec(),
            _ => Vec::new(),
        };
        let value = args.get(2);
        if name == b"Value" {
            key.rotation = rotation_of(value);
            return Ok(none());
        }
        let not_cubic = "Attempting to set a right tangent value on a curve key but the interpolation mode is not Cubic";
        if write_shared(lua, &e, &name, value, &mut key.time, &mut key.mode, &mut key.left, &mut key.right, true, not_cubic)? {
            return Ok(none());
        }
        raise(format!("{} is not a valid member of RotationCurveKey", member(args.get(1))))
    })?;
    let tostring = lua.create_function(|_, this: LuaAnyUserData| {
        let key = this.borrow::<LRotationCurveKey>()?;
        let rows = frame::from_affine(&Affine3A::from_quat(key.rotation)).orientation;
        let mut parts = vec![mode_name(key.mode).to_string(), optional_digits(key.time), "0".into(), "0".into(), "0".into()];
        for row in [rows.x, rows.y, rows.z] {
            parts.extend([digits(row.x), digits(row.y), digits(row.z)]);
        }
        parts.extend([optional_digits(key.left), optional_digits(key.right)]);
        Ok(format!("{{{}}}", parts.join(", ")))
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| {
        let (LuaValue::UserData(a), LuaValue::UserData(b)) = (a, b) else { return Ok(false) };
        let (Ok(a), Ok(b)) = (a.borrow::<LRotationCurveKey>(), b.borrow::<LRotationCurveKey>()) else { return Ok(false) };
        Ok(*a == *b && a.time.is_some())
    })?;
    lua.register_userdata_type::<LRotationCurveKey>(|registry| {
        registry.add_meta_field("__type", "RotationCurveKey");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;
    let e = engine.clone();
    library(lua, engine, "RotationCurveKey", move |lua, args| {
        let args = Args::new(args);
        let (time, value, mode) = new_arguments(lua, &e, "RotationCurveKey", &args)?;
        let key = LRotationCurveKey { time: Some(time), rotation: rotation_of(value), mode, left: Some(0.0), right: Some(0.0) };
        let userdata = lua.create_any_userdata(key)?;
        lock_metatable(lua, &userdata)?;
        Ok(values([LuaValue::UserData(userdata)]))
    })
}

pub fn register(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    register_float(lua, engine)?;
    register_rotation(lua, engine)
}

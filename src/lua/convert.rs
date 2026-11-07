//! Conversions between engine [`Value`]s and Luau values, and property coercion (port of
//! instance.luau Coerce). Lune's datatype userdata are the script representation of rbx-dom
//! datatypes, as in the Luau runtime.

use std::rc::Rc;

use lune_roblox::datatypes::types as lune;
use mlua::prelude::*;
use rbx_dom_weak::types::{self as dom, Variant};

use super::{
    Flow, enum_item_value, instance_value, raw::instance_of_ud, typeof_value,
    types::{LContent, LEnumItem, LFont},
};
use crate::{
    Engine, ScriptError,
    reflection::{EngineType, PropertyInfo, ScriptType},
    value::{ContentValue, Str, Value},
};

/// An engine value as scripts see it (instances and enum items keep their identity; Font values
/// are fresh copies, as CopyIfMutable returns).
pub fn to_lua(lua: &Lua, engine: &Engine, value: &Value) -> LuaResult<LuaValue> {
    Ok(match value {
        Value::Nil => LuaValue::Nil,
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::Number(n) => LuaValue::Number(*n),
        Value::String(s) => LuaValue::String(lua.create_string(s.as_bytes())?),
        Value::EnumItem(item) => enum_item_value(lua, engine, *item)?,
        Value::Instance(id) => {
            if engine.state.borrow().dom.get(*id).is_some() {
                instance_value(lua, engine, *id)?
            } else {
                LuaValue::Nil
            }
        }
        Value::UDim(v) => LuaValue::UserData(lua.create_userdata(lune::UDim::from(*v))?),
        Value::UDim2(v) => LuaValue::UserData(lua.create_userdata(lune::UDim2::from(*v))?),
        Value::Vector2(v) => LuaValue::UserData(lua.create_userdata(lune::Vector2::from(*v))?),
        // (the native vector type, as in Roblox: prelude.luau "Vector3")
        Value::Vector3(v) => LuaValue::Vector(mlua::Vector::new(v.x, v.y, v.z)),
        Value::Color3(v) => LuaValue::UserData(lua.create_userdata(lune::Color3::from(*v))?),
        Value::Rect(v) => LuaValue::UserData(lua.create_userdata(lune::Rect::from(*v))?),
        Value::NumberRange(v) => LuaValue::UserData(lua.create_userdata(lune::NumberRange::from(*v))?),
        Value::NumberSequence(v) => {
            LuaValue::UserData(lua.create_userdata(lune::NumberSequence::from((**v).clone()))?)
        }
        Value::ColorSequence(v) => {
            LuaValue::UserData(lua.create_userdata(lune::ColorSequence::from((**v).clone()))?)
        }
        Value::Font(font) => LuaValue::UserData(lua.create_any_userdata(LFont(std::cell::RefCell::new((**font).clone())))?),
        Value::Content(content) => LuaValue::UserData(lua.create_any_userdata(LContent((**content).clone()))?),
        Value::Dom(variant) => dom_to_lua(lua, variant)?,
        Value::Lua(value) => {
            // (a table or buffer BindableEvent:Fire sent: every receiver gets a copy of its own, lua/bindable.rs)
            if let LuaValue::UserData(userdata) = &**value {
                if let Ok(copied) = userdata.borrow::<super::bindable::Copied>() {
                    return super::bindable::rebuild(lua, &copied.0);
                }
            }
            (**value).clone()
        }
    })
}

/// Other rbx-dom datatypes as Lune userdata.
pub fn dom_to_lua(lua: &Lua, variant: &Variant) -> LuaResult<LuaValue> {
    macro_rules! ud {
        ($value:expr) => {
            LuaValue::UserData(lua.create_userdata($value)?)
        };
    }
    Ok(match variant {
        Variant::Axes(v) => ud!(lune::Axes::from(*v)),
        Variant::BrickColor(v) => ud!(lune::BrickColor::from(*v)),
        Variant::CFrame(v) => ud!(lune::CFrame::from(*v)),
        Variant::Faces(v) => ud!(lune::Faces::from(*v)),
        Variant::Ray(v) => ud!(lune::Ray::from(*v)),
        Variant::Region3(v) => ud!(lune::Region3::from(*v)),
        Variant::Region3int16(v) => ud!(lune::Region3int16::from(*v)),
        Variant::UniqueId(v) => ud!(lune::UniqueId::from(*v)),
        Variant::Vector2int16(v) => ud!(lune::Vector2int16::from(*v)),
        Variant::Vector3int16(v) => ud!(lune::Vector3int16::from(*v)),
        Variant::PhysicalProperties(dom::PhysicalProperties::Custom(v)) => {
            ud!(lune::PhysicalProperties::from(*v))
        }
        _ => LuaValue::Nil,
    })
}

/// A script value stored as given (attributes, `any`-typed properties, event arguments): engine
/// objects and Lune datatypes become typed values, anything else stays a Luau reference.
pub fn from_lua(value: &LuaValue) -> Value {
    match value {
        LuaValue::Nil => Value::Nil,
        LuaValue::Boolean(b) => Value::Bool(*b),
        LuaValue::Integer(n) => Value::Number(*n as f64),
        LuaValue::Number(n) => Value::Number(*n),
        LuaValue::String(s) => Value::String(Str::new(&s.as_bytes())),
        LuaValue::Vector(v) => Value::Vector3(dom::Vector3::new(v.x(), v.y(), v.z())),
        LuaValue::UserData(ud) => userdata_value(ud).unwrap_or_else(|| Value::Lua(Box::new(value.clone()))),
        other => Value::Lua(Box::new(other.clone())),
    }
}

/// A script's Vector3 as a property stores it: its float32 components as they are. (lune's conversion rounds each
/// fractional part to 1/65536, which is for the files it writes: a playing client keeps the float, measured with
/// notes/macro/bodyprobe.luau.)
fn vector3_exact(v: &lune::Vector3) -> dom::Vector3 {
    dom::Vector3::new(v.0.x, v.0.y, v.0.z)
}

/// A script's CFrame as a property stores it: position and rotation rows as the float32s the script made (lune's
/// conversion rounds each to 1/65536: CFrame.Angles(0.001, 0, 0) read back sin 0.001007; a client reads 0.001000).
fn cframe_exact(v: &lune::CFrame) -> dom::CFrame {
    let (x, y, z, w) = (v.0.x_axis, v.0.y_axis, v.0.z_axis, v.0.w_axis);
    dom::CFrame::new(
        dom::Vector3::new(w.x, w.y, w.z),
        dom::Matrix3::new(dom::Vector3::new(x.x, y.x, z.x), dom::Vector3::new(x.y, y.y, z.y), dom::Vector3::new(x.z, y.z, z.z)),
    )
}

fn userdata_value(ud: &LuaAnyUserData) -> Option<Value> {
    if let Some(instance) = instance_of_ud(ud) {
        return Some(Value::Instance(instance));
    }
    if let Ok(item) = ud.borrow::<LEnumItem>() {
        return Some(Value::EnumItem(item.0));
    }
    if let Ok(v) = ud.borrow::<lune::UDim2>() {
        return Some(Value::UDim2(dom::UDim2::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::Color3>() {
        return Some(Value::Color3(dom::Color3::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::Vector2>() {
        return Some(Value::Vector2(dom::Vector2::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::UDim>() {
        return Some(Value::UDim(dom::UDim::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::Vector3>() {
        return Some(Value::Vector3(vector3_exact(&v)));
    }
    if let Ok(font) = ud.borrow::<LFont>() {
        return Some(Value::Font(Rc::new(font.0.borrow().clone())));
    }
    if let Ok(content) = ud.borrow::<LContent>() {
        return Some(Value::Content(Rc::new(content.0.clone())));
    }
    if let Ok(v) = ud.borrow::<lune::Rect>() {
        return Some(Value::Rect(dom::Rect::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::NumberRange>() {
        return Some(Value::NumberRange(dom::NumberRange::from(*v)));
    }
    if let Ok(v) = ud.borrow::<lune::NumberSequence>() {
        return Some(Value::NumberSequence(Rc::new(dom::NumberSequence::from(v.clone()))));
    }
    if let Ok(v) = ud.borrow::<lune::ColorSequence>() {
        return Some(Value::ColorSequence(Rc::new(dom::ColorSequence::from(v.clone()))));
    }
    if let Ok(v) = ud.borrow::<lune::CFrame>() {
        return Some(Value::Dom(Rc::new(Variant::CFrame(cframe_exact(&v)))));
    }
    if let Ok(v) = ud.borrow::<lune::BrickColor>() {
        return Some(Value::Dom(Rc::new(Variant::BrickColor(dom::BrickColor::from(*v)))));
    }
    None
}

fn type_error(info: &PropertyInfo, engine: &Engine, reflection_name: &str, expected: &str, value: &LuaValue) -> Flow {
    let _ = reflection_name;
    // (Roblox names a CFrame property's type CoordinateFrame here: measured in a playing client, Studio 0.744,
    // tools/parity/probes/coercion.luau)
    let expected = if expected == "CFrame" { "CoordinateFrame" } else { expected };
    Flow::Error(ScriptError::plain(format!(
        "Unable to assign property {}. {expected} expected, got {}",
        engine.reflection.name(info.name),
        typeof_value(engine, value)
    )))
}

/// Text objects keep TextSize as a whole number from 1 to 100 (measured: 10.61 and 10.99 read
/// back as 10, 0.5 and -3 as 1, 150 as 100).
fn whole_text_size_owner(engine: &Engine, info: &PropertyInfo) -> bool {
    let owner = &*engine.reflection.class(info.owner).name;
    matches!(owner, "TextLabel" | "TextButton" | "TextBox")
}

/// BasePart.Size (a part of any class).
fn part_size_owner(engine: &Engine, info: &PropertyInfo) -> bool {
    engine.reflection.class_id("BasePart").is_some_and(|base| engine.reflection.class(info.owner).is_a(base))
}

/// UIScale.Scale stays within [float32(1e-7), 1000] (measured, tools/studio/layout-probe-zeroscale2:
/// 0, 1e-9, -1 and -inf read back as 1.0000000116860974e-07, inf and 1e30 as 1000, NaN stays NaN).
const MIN_UI_SCALE: f64 = 1e-7_f32 as f64;
const MAX_UI_SCALE: f64 = 1000.0;

/// An `int` property's value for an assigned number, as a C cast to a 32-bit integer gives it: toward zero, and the
/// minimum for a number out of range or not finite (measured in a playing client, Studio 0.744,
/// tools/parity/probes/coercion.luau: ZIndex = 2.7, -2.7, 2^31, 1e20, NaN, inf read 2, -2, then -2147483648).
#[inline]
fn int32_for(number: f64) -> f64 {
    // (+ 0.0: an integer has no negative zero; -0.5 reads 0)
    let truncated = number.trunc() + 0.0;
    if truncated >= i32::MIN as f64 && truncated <= i32::MAX as f64 { truncated } else { i32::MIN as f64 }
}

/// An `int64` property's value for an assigned number: rounded half away from zero, zero for a number out of range or
/// not finite (measured likewise: IntValue.Value = 2.5, -2.5, 0.5, 1e20, NaN read 3, -3, 1, 0, 0).
#[inline]
fn int64_for(number: f64) -> f64 {
    let rounded = number.round() + 0.0;
    if rounded >= -9.223372036854775808e18 && rounded < 9.223372036854775808e18 { rounded } else { 0.0 }
}

/// The number a numeric property stores for an assigned number (`int` properties truncate, `int64` ones round,
/// float properties round to float32, TextSize of text objects is whole and clamped, BorderSizePixel clamped,
/// UIScale.Scale clamped).
#[inline]
pub fn number_for(engine: &Engine, info: &PropertyInfo, number: f64) -> f64 {
    if info.engine_type == EngineType::Integer {
        let whole = int32_for(number);
        // GuiObject.BorderSizePixel stays within [0, 100] (measured: -2.7, 2^31 and "1e3" read 0, 0 and 100)
        if info.name == crate::K::BorderSizePixel as crate::NameId { whole.clamp(0.0, 100.0) } else { whole }
    } else if info.engine_type == EngineType::Int64 {
        int64_for(number)
    } else if info.engine_type == EngineType::Float {
        if info.name == crate::K::TextSize as crate::NameId && whole_text_size_owner(engine, info) {
            // a whole number as an int property takes it, then 1 to 100 (2^31 and NaN read 1)
            int32_for(number).clamp(1.0, 100.0)
        } else if info.name == crate::K::Scale as crate::NameId
            && !number.is_nan()
            && &*engine.reflection.class(info.owner).name == "UIScale"
        {
            (number as f32 as f64).clamp(MIN_UI_SCALE, MAX_UI_SCALE)
        } else {
            number as f32 as f64
        }
    } else {
        number
    }
}

/// Coerce: converts an assigned value to what the property stores, or the assignment error.
pub fn coerce(lua: &Lua, engine: &Engine, info: &PropertyInfo, value: &LuaValue) -> Result<Value, Flow> {
    let name = engine.reflection.name(info.name);
    match info.script_type {
        ScriptType::Number => {
            // Any value goes: a string as Luau reads a number, anything else (and a string that is no number) as 0
            // (measured in a playing client, Studio 0.744, tools/parity/probes/coercion.luau: "0x10" reads 16,
            // "five", true, {}, a Vector3 and nil read 0)
            let number = match value {
                LuaValue::Number(n) => *n,
                LuaValue::Integer(n) => *n as f64,
                LuaValue::String(_) => lua.coerce_number(value.clone())?.unwrap_or(0.0),
                _ => 0.0,
            };
            Ok(Value::Number(number_for(engine, info, number)))
        }
        ScriptType::String => match value {
            LuaValue::String(s) => Ok(Value::String(Str::new(&s.as_bytes()))),
            LuaValue::Number(_) | LuaValue::Integer(_) => {
                let text = lua.coerce_string(value.clone())?.map(|s| Str::new(&s.as_bytes())).unwrap_or_default();
                Ok(Value::String(text))
            }
            LuaValue::UserData(ud) if info.engine_type == EngineType::ContentId && ud.is::<LContent>() => {
                let content = ud.borrow::<LContent>()?;
                Ok(Value::String(content.0.uri().cloned().unwrap_or_default()))
            }
            _ => Err(type_error(info, engine, name, "string", value)),
        },
        // Lua truth: only nil and false are false (measured likewise: 0, "" and {} read true)
        ScriptType::Boolean => Ok(Value::Bool(!matches!(value, LuaValue::Nil | LuaValue::Boolean(false)))),
        ScriptType::EnumItem => {
            let Some(enum_name) = &info.enum_name else {
                return Ok(from_lua(value));
            };
            // Beside an item of the enum, its name and its value: a value as a numeric string or with a fraction is
            // truncated ("512" and 512.5 are Wood's 512), and a string that is neither raises `Invalid value "wood" for enum
            // Material` (measured in a playing client, Studio 0.744: tools/parity/probes/values.luau, coercion.luau)
            let coerced = info.enum_type.and_then(|id| {
                coerce_enum(lua, engine, id, value).or_else(|| {
                    let number = match value {
                        LuaValue::Number(n) => Some(*n),
                        LuaValue::String(_) => lua.coerce_number(value.clone()).ok().flatten(),
                        _ => None,
                    }?;
                    let info = &engine.reflection.enums[id as usize];
                    number.is_finite().then(|| info.by_value.get(&(number.trunc() as i64)).copied()).flatten()
                })
            });
            // What the rest raises, a playing client and a server alike (Studio 0.744, tools/parity/probes/enum_assignment.luau):
            // a number, or a numeric string, no item has `Invalid value 256 for enum Font` (the value truncated: 512.5 shows
            // 512); any other string `Invalid value "Wood" for enum Font`; an item of another enum `EnumItem of type Font
            // expected, got an EnumItem of type SortOrder`; anything else `EnumItem, number, or string expected, got nil`.
            if coerced.is_none() {
                let number = match value {
                    LuaValue::Number(n) => Some(*n),
                    LuaValue::Integer(n) => Some(*n as f64),
                    LuaValue::String(_) => lua.coerce_number(value.clone()).ok().flatten(),
                    _ => None,
                };
                if let Some(number) = number {
                    let shown = if number.is_finite() {
                        format!("{}", number.trunc() as i64)
                    } else {
                        lua.coerce_string(LuaValue::Number(number)).ok().flatten().map(|s| s.to_string_lossy()).unwrap_or_default()
                    };
                    return Err(Flow::Error(ScriptError::plain(format!(
                        "Unable to assign property {name}. Invalid value {shown} for enum {enum_name}"
                    ))));
                }
                if let LuaValue::String(text) = value {
                    return Err(Flow::Error(ScriptError::plain(format!(
                        "Unable to assign property {name}. Invalid value \"{}\" for enum {enum_name}",
                        text.to_string_lossy()
                    ))));
                }
            }
            match coerced {
                Some(item) => Ok(Value::EnumItem(item)),
                None => Err(Flow::Error(ScriptError::plain(match value {
                    LuaValue::UserData(ud) if ud.is::<LEnumItem>() => {
                        let item = ud.borrow::<LEnumItem>()?.0;
                        let enum_type = engine.reflection.enum_item_info(item).enum_type;
                        format!(
                            "Unable to assign property {name}. EnumItem of type {enum_name} expected, got an EnumItem of type {}",
                            engine.reflection.enums[enum_type as usize].name
                        )
                    }
                    _ => format!("Unable to assign property {name}. EnumItem, number, or string expected, got {}", typeof_value(engine, value)),
                }))),
            }
        }
        ScriptType::Instance => match value {
            LuaValue::Nil => Ok(Value::Nil),
            LuaValue::UserData(ud) if instance_of_ud(ud).is_some() => Ok(Value::Instance(instance_of_ud(ud).unwrap())),
            _ => Err(type_error(info, engine, name, "Instance", value)),
        },
        ScriptType::Content => match value {
            LuaValue::String(s) => Ok(Value::Content(Rc::new(ContentValue::Uri(Str::new(&s.as_bytes()))))),
            LuaValue::Nil => Ok(Value::Content(Rc::new(ContentValue::None))),
            LuaValue::UserData(ud) if ud.is::<LContent>() => Ok(Value::Content(Rc::new(ud.borrow::<LContent>()?.0.clone()))),
            _ => Err(type_error(info, engine, name, "Content", value)),
        },
        ScriptType::Any => Ok(from_lua(value)),
        _ => {
            // Native vectors are Vector3 values in Roblox; properties store the datatype form
            if let (ScriptType::Vector3, LuaValue::Vector(v)) = (info.script_type, value) {
                if info.name == crate::K::Size as crate::NameId && part_size_owner(engine, info) {
                    // A part is at least 0.001 on every axis (measured in a playing client, Studio 0.744,
                    // tools/parity/probes/changed.luau: Size = (0, 0, 0) reads 0.00100000005 each; negative axes
                    // not measured)
                    let least = |axis: f32| axis.max(0.001);
                    return Ok(Value::Vector3(dom::Vector3::new(least(v.x()), least(v.y()), least(v.z()))));
                }
                return Ok(Value::Vector3(dom::Vector3::new(v.x(), v.y(), v.z())));
            }
            let converted = from_lua(value);
            let matches = match (info.script_type, &converted) {
                (ScriptType::UDim2, Value::UDim2(_))
                | (ScriptType::UDim, Value::UDim(_))
                | (ScriptType::Vector2, Value::Vector2(_))
                | (ScriptType::Vector3, Value::Vector3(_))
                | (ScriptType::Color3, Value::Color3(_))
                | (ScriptType::Rect, Value::Rect(_))
                | (ScriptType::NumberRange, Value::NumberRange(_))
                | (ScriptType::NumberSequence, Value::NumberSequence(_))
                | (ScriptType::ColorSequence, Value::ColorSequence(_))
                | (ScriptType::Font, Value::Font(_)) => true,
                (ScriptType::CFrame, Value::Dom(v)) => matches!(**v, Variant::CFrame(_)),
                (ScriptType::BrickColor, Value::Dom(v)) => matches!(**v, Variant::BrickColor(_)),
                _ => false,
            };
            if matches {
                if let Value::Vector3(v) = &converted {
                    if info.name == crate::K::Size as crate::NameId && part_size_owner(engine, info) {
                        return Ok(Value::Vector3(dom::Vector3::new(v.x.max(0.001), v.y.max(0.001), v.z.max(0.001))));
                    }
                }
                return Ok(converted);
            }
            // nil clears custom physical properties, another type raises (measured in Studio 2026-10-12:
            // BasePart.CustomPhysicalProperties = nil is accepted, also over a set value, and reads back nil; = 5 raises)
            let physical = &*info.engine_type_name == "PhysicalProperties";
            if value.is_nil() && physical {
                return Ok(Value::Nil);
            }
            let actual = typeof_value(engine, value);
            if actual != &*info.script_type_name {
                if value.is_nil() || info.script_type.strict() || physical {
                    return Err(type_error(info, engine, name, &info.script_type_name, value));
                }
            }
            Ok(converted)
        }
    }
}

/// Enums.Coerce: an EnumItem of the type, an item name or an item value.
pub fn coerce_enum(lua: &Lua, engine: &Engine, enum_id: u32, value: &LuaValue) -> Option<u32> {
    let _ = lua;
    let info = &engine.reflection.enums[enum_id as usize];
    match value {
        LuaValue::UserData(ud) => {
            let item = ud.borrow::<LEnumItem>().ok()?.0;
            (engine.reflection.enum_item_info(item).enum_type == enum_id).then_some(item)
        }
        LuaValue::String(s) => info.by_name.get(&*s.to_str().ok()?).copied(),
        LuaValue::Number(n) => (n.fract() == 0.0).then(|| info.by_value.get(&(*n as i64)).copied()).flatten(),
        LuaValue::Integer(n) => info.by_value.get(&(*n as i64)).copied(),
        _ => None,
    }
}

//! Property and attribute values held by the engine (no Luau conversion here: see
//! `engine::lua::convert`). The datatypes layout and export read natively are typed variants over
//! rbx-dom's structs (the same single-precision fields Lune's userdata carry); rarer datatypes
//! are kept as rbx-dom variants, and any other script value (tables, functions, other userdata)
//! as a Luau reference. Port of the value side of `runtime/engine/datatypes.luau`.

use std::{borrow::Cow, fmt, rc::Rc};

use rbx_dom_weak::types::{self as dom, Variant};

use super::{
    dom::InstanceId,
    reflection::{EnumItemId, RawDefault, Reflection, ScriptType},
};

/// An immutable byte string (Roblox strings are bytes; almost always UTF-8).
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct Str(Rc<[u8]>);

impl Str {
    pub fn new(bytes: &[u8]) -> Self {
        Self(Rc::from(bytes))
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_str_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for Str {
    fn from(value: &str) -> Self {
        Self::new(value.as_bytes())
    }
}

impl From<String> for Str {
    fn from(value: String) -> Self {
        Self::new(value.as_bytes())
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.to_str_lossy(), f)
    }
}

impl fmt::Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_str_lossy())
    }
}

/// `Font` (datatypes.luau FontInfo): family URI plus interned FontWeight / FontStyle items.
#[derive(Clone, Debug, PartialEq)]
pub struct FontValue {
    pub family: Str,
    pub weight: EnumItemId,
    pub style: EnumItemId,
}

/// `Content` (datatypes.luau ContentInfo).
#[derive(Clone, Debug, PartialEq)]
pub enum ContentValue {
    None,
    Uri(Str),
    Object(InstanceId),
    /// An Opaque Content (AssetService:CreateDataModelContentAsync): a copy of an object's data as it was, which scripts
    /// see no Uri or Object of; here the file holding the copy (its `file://` URI) and a serial (each call's Content is
    /// its own: two of the same mesh are not equal)
    Opaque(u64, Str),
}

impl ContentValue {
    /// The Uri a script reads.
    pub fn uri(&self) -> Option<&Str> {
        match self {
            Self::Uri(uri) => Some(uri),
            _ => None,
        }
    }

    /// Where the content's data is: a Uri's URI, an Opaque Content's copy (what loads it, and what a recording names).
    pub fn location(&self) -> Option<&Str> {
        match self {
            Self::Uri(uri) | Self::Opaque(_, uri) => Some(uri),
            _ => None,
        }
    }
}

/// A stored property / attribute value.
#[derive(Clone, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    String(Str),
    EnumItem(EnumItemId),
    Instance(InstanceId),
    UDim(dom::UDim),
    UDim2(dom::UDim2),
    Vector2(dom::Vector2),
    Vector3(dom::Vector3),
    Color3(dom::Color3),
    Rect(dom::Rect),
    NumberRange(dom::NumberRange),
    NumberSequence(Rc<dom::NumberSequence>),
    ColorSequence(Rc<dom::ColorSequence>),
    Font(Rc<FontValue>),
    Content(Rc<ContentValue>),
    /// Other rbx-dom datatypes (CFrame, BrickColor, Ray, Faces, ...), shown as Lune userdata
    Dom(Rc<Variant>),
    /// Any other script value (tables, functions, threads, other userdata); boxed, so a Value is 24
    /// bytes and not 40 (every stored property and attribute is one)
    Lua(Box<mlua::Value>),
}

impl Default for Value {
    fn default() -> Self {
        Value::Nil
    }
}

impl Value {
    #[inline]
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// The instances this value references (instance values, Content objects): what keeps them
    /// from being collected (lua::raw::collect).
    #[inline]
    pub fn visit_instances(&self, visit: &mut dyn FnMut(InstanceId)) {
        // (references_instance: the same values)
        match self {
            Value::Instance(id) => visit(*id),
            Value::Content(content) => {
                if let ContentValue::Object(id) = **content {
                    visit(id);
                }
            }
            _ => {}
        }
    }

    /// Whether [`Value::visit_instances`] visits anything.
    #[inline]
    pub fn references_instance(&self) -> bool {
        match self {
            Value::Instance(_) => true,
            Value::Content(content) => matches!(**content, ContentValue::Object(_)),
            _ => false,
        }
    }

    #[inline]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    #[inline]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[inline]
    pub fn as_str(&self) -> Option<&Str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    #[inline]
    pub fn as_enum(&self) -> Option<EnumItemId> {
        match self {
            Value::EnumItem(item) => Some(*item),
            _ => None,
        }
    }

    #[inline]
    pub fn as_instance(&self) -> Option<InstanceId> {
        match self {
            Value::Instance(id) => Some(*id),
            _ => None,
        }
    }

    #[inline]
    pub fn as_udim2(&self) -> Option<dom::UDim2> {
        match self {
            Value::UDim2(v) => Some(*v),
            _ => None,
        }
    }

    #[inline]
    pub fn as_udim(&self) -> Option<dom::UDim> {
        match self {
            Value::UDim(v) => Some(*v),
            _ => None,
        }
    }

    #[inline]
    pub fn as_vector2(&self) -> Option<dom::Vector2> {
        match self {
            Value::Vector2(v) => Some(*v),
            _ => None,
        }
    }

    #[inline]
    pub fn as_color3(&self) -> Option<dom::Color3> {
        match self {
            Value::Color3(v) => Some(*v),
            _ => None,
        }
    }

    #[inline]
    pub fn as_rect(&self) -> Option<dom::Rect> {
        match self {
            Value::Rect(v) => Some(*v),
            _ => None,
        }
    }

    /// Luau `==` as Roblox compares old and new values to decide whether a write changed anything
    /// (userdata by value through their `__eq`, interned enums and instances by identity, tables by
    /// reference). A NaN is never the same: writing NaN over NaN fires Changed again, for a property,
    /// a NumberValue and an attribute alike (measured in a playing client, Studio 0.744:
    /// tools/parity/probes/changed.luau).
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::EnumItem(a), Value::EnumItem(b)) => a == b,
            (Value::Instance(a), Value::Instance(b)) => a == b,
            (Value::UDim(a), Value::UDim(b)) => a == b,
            (Value::UDim2(a), Value::UDim2(b)) => a == b,
            (Value::Vector2(a), Value::Vector2(b)) => a == b,
            (Value::Vector3(a), Value::Vector3(b)) => a == b,
            (Value::Color3(a), Value::Color3(b)) => a == b,
            (Value::Rect(a), Value::Rect(b)) => a == b,
            (Value::NumberRange(a), Value::NumberRange(b)) => a == b,
            (Value::NumberSequence(a), Value::NumberSequence(b)) => a == b,
            (Value::ColorSequence(a), Value::ColorSequence(b)) => a == b,
            (Value::Font(a), Value::Font(b)) => a == b,
            (Value::Content(a), Value::Content(b)) => a == b,
            (Value::Dom(a), Value::Dom(b)) => a == b,
            (Value::Lua(a), Value::Lua(b)) => a == b,
            _ => false,
        }
    }
}

// Decoding of the JSON value encoding used by data/reflection.json (datatypes.luau Decode).

/// Non-finite numbers: `{ "$t": "number", s = "inf" }` at the top level and
/// `{ t = "numeric", v = "inf" }` inside vectors.
fn decode_number(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::Object(map) => {
            let text = if map.get("t").and_then(|t| t.as_str()) == Some("numeric") {
                map.get("v")
            } else {
                map.get("s")
            };
            match text.and_then(|t| t.as_str()) {
                Some("inf" | "+inf") => f64::INFINITY,
                Some("-inf") => f64::NEG_INFINITY,
                Some("nan" | "-nan") => f64::NAN,
                Some(other) => other.parse().unwrap_or(0.0),
                None => 0.0,
            }
        }
        serde_json::Value::String(s) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn numbers(value: Option<&serde_json::Value>) -> Vec<f64> {
    match value {
        Some(serde_json::Value::Array(items)) => items.iter().map(decode_number).collect(),
        Some(other) => vec![decode_number(other)],
        None => Vec::new(),
    }
}

fn at(values: &[f64], index: usize) -> f64 {
    values.get(index).copied().unwrap_or(0.0)
}

/// Lune truncates constructor offsets to i32 (`as` saturates; NaN is 0).
fn int32(value: f64) -> i32 {
    value as i32
}

pub fn decode_json(reflection: &Reflection, value: &serde_json::Value) -> Option<Value> {
    use serde_json::Value as J;
    let map = match value {
        J::Null => return None,
        J::Bool(b) => return Some(Value::Bool(*b)),
        J::Number(n) => return Some(Value::Number(n.as_f64().unwrap_or(0.0))),
        J::String(s) => return Some(Value::String(Str::from(s.as_str()))),
        J::Array(_) => return None,
        J::Object(map) => map,
    };
    let kind = map.get("$t")?.as_str()?;
    let raw = map.get("v");
    let v = || numbers(raw);
    Some(match kind {
        "number" => match map.get("s").and_then(|s| s.as_str()) {
            Some("nan") => Value::Number(f64::NAN),
            Some("inf") => Value::Number(f64::INFINITY),
            _ => Value::Number(f64::NEG_INFINITY),
        },
        "EnumItem" => {
            let enum_name = map.get("e")?.as_str()?;
            let item_name = map.get("n")?.as_str()?;
            Value::EnumItem(reflection.enum_item(enum_name, item_name)?)
        }
        "UDim" => {
            let v = v();
            Value::UDim(dom::UDim::new(at(&v, 0) as f32, int32(at(&v, 1))))
        }
        "UDim2" => {
            let v = v();
            Value::UDim2(dom::UDim2::new(
                dom::UDim::new(at(&v, 0) as f32, int32(at(&v, 1))),
                dom::UDim::new(at(&v, 2) as f32, int32(at(&v, 3))),
            ))
        }
        "Vector2" => {
            let v = v();
            Value::Vector2(dom::Vector2::new(at(&v, 0) as f32, at(&v, 1) as f32))
        }
        "Vector2int16" => {
            let v = v();
            Value::Dom(Rc::new(Variant::Vector2int16(dom::Vector2int16::new(
                at(&v, 0) as i16,
                at(&v, 1) as i16,
            ))))
        }
        "Vector3" => {
            let v = v();
            Value::Vector3(dom::Vector3::new(at(&v, 0) as f32, at(&v, 1) as f32, at(&v, 2) as f32))
        }
        "Vector3int16" => {
            let v = v();
            Value::Dom(Rc::new(Variant::Vector3int16(dom::Vector3int16::new(
                at(&v, 0) as i16,
                at(&v, 1) as i16,
                at(&v, 2) as i16,
            ))))
        }
        "Color3" => {
            let v = v();
            Value::Color3(dom::Color3::new(at(&v, 0) as f32, at(&v, 1) as f32, at(&v, 2) as f32))
        }
        "BrickColor" => {
            let number = at(&v(), 0);
            let color = dom::BrickColor::from_number(number as u16)
                .unwrap_or(dom::BrickColor::MediumStoneGrey);
            Value::Dom(Rc::new(Variant::BrickColor(color)))
        }
        "CFrame" => {
            let v = v();
            let f = |i| at(&v, i) as f32;
            let cframe = if v.len() >= 12 {
                dom::CFrame::new(
                    dom::Vector3::new(f(0), f(1), f(2)),
                    dom::Matrix3::new(
                        dom::Vector3::new(f(3), f(4), f(5)),
                        dom::Vector3::new(f(6), f(7), f(8)),
                        dom::Vector3::new(f(9), f(10), f(11)),
                    ),
                )
            } else {
                dom::CFrame::new(dom::Vector3::new(f(0), f(1), f(2)), dom::Matrix3::identity())
            };
            Value::Dom(Rc::new(Variant::CFrame(cframe)))
        }
        "Rect" => {
            let v = v();
            Value::Rect(dom::Rect::new(
                dom::Vector2::new(at(&v, 0) as f32, at(&v, 1) as f32),
                dom::Vector2::new(at(&v, 2) as f32, at(&v, 3) as f32),
            ))
        }
        "NumberRange" => {
            let v = v();
            Value::NumberRange(dom::NumberRange::new(at(&v, 0) as f32, at(&v, 1) as f32))
        }
        "NumberSequence" => {
            let keypoints = match raw {
                Some(J::Array(items)) => items
                    .iter()
                    .map(|item| {
                        let k = numbers(Some(item));
                        dom::NumberSequenceKeypoint::new(
                            at(&k, 0) as f32,
                            at(&k, 1) as f32,
                            at(&k, 2) as f32,
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Value::NumberSequence(Rc::new(dom::NumberSequence { keypoints }))
        }
        "ColorSequence" => {
            let keypoints = match raw {
                Some(J::Array(items)) => items
                    .iter()
                    .map(|item| {
                        let k = numbers(Some(item));
                        dom::ColorSequenceKeypoint::new(
                            at(&k, 0) as f32,
                            dom::Color3::new(at(&k, 1) as f32, at(&k, 2) as f32, at(&k, 3) as f32),
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Value::ColorSequence(Rc::new(dom::ColorSequence { keypoints }))
        }
        "Font" => {
            let parts = match raw {
                Some(J::Array(items)) => items,
                _ => return None,
            };
            let text = |i: usize| parts.get(i).and_then(|p| p.as_str()).unwrap_or("");
            Value::Font(Rc::new(FontValue {
                family: Str::from(text(0)),
                weight: reflection.enum_item("FontWeight", text(1))?,
                style: reflection.enum_item("FontStyle", text(2))?,
            }))
        }
        "Content" => match raw {
            Some(J::String(uri)) if !uri.is_empty() => {
                Value::Content(Rc::new(ContentValue::Uri(Str::from(uri.as_str()))))
            }
            _ => Value::Content(Rc::new(ContentValue::None)),
        },
        _ => return None,
    })
}

fn font_weight_name(weight: dom::FontWeight) -> &'static str {
    match weight {
        dom::FontWeight::Thin => "Thin",
        dom::FontWeight::ExtraLight => "ExtraLight",
        dom::FontWeight::Light => "Light",
        dom::FontWeight::Regular => "Regular",
        dom::FontWeight::Medium => "Medium",
        dom::FontWeight::SemiBold => "SemiBold",
        dom::FontWeight::Bold => "Bold",
        dom::FontWeight::ExtraBold => "ExtraBold",
        dom::FontWeight::Heavy => "Heavy",
        _ => "Regular",
    }
}

fn font_style_name(style: dom::FontStyle) -> &'static str {
    match style {
        dom::FontStyle::Normal => "Normal",
        dom::FontStyle::Italic => "Italic",
        _ => "Normal",
    }
}

/// An rbx-dom value (Lune default or snapshot property) as the engine stores it
/// (datatypes.luau FromLune): enums interned by name, Font / Content as engine values,
/// primitives unwrapped, other datatypes kept as rbx-dom variants. `None` where Lune would
/// produce no value (null refs, missing optional values, unsupported types).
pub fn from_dom(reflection: &Reflection, value: &Variant) -> Option<Value> {
    Some(match value {
        Variant::Bool(b) => Value::Bool(*b),
        Variant::Int32(n) => Value::Number(*n as f64),
        Variant::Int64(n) => Value::Number(*n as f64),
        Variant::Float32(n) => Value::Number(*n as f64),
        Variant::Float64(n) => Value::Number(*n),
        Variant::String(s) => Value::String(Str::from(s.as_str())),
        Variant::BinaryString(s) => Value::String(Str::new(s.as_ref())),
        Variant::ContentId(s) => Value::String(Str::from(s.as_str())),
        Variant::Content(content) => Value::Content(Rc::new(match content.as_uri() {
            Some(uri) if !uri.is_empty() => ContentValue::Uri(Str::from(uri)),
            _ => ContentValue::None,
        })),
        Variant::Font(font) => Value::Font(Rc::new(FontValue {
            family: Str::from(font.family.as_str()),
            weight: reflection.enum_item("FontWeight", font_weight_name(font.weight))?,
            style: reflection.enum_item("FontStyle", font_style_name(font.style))?,
        })),
        Variant::UDim(v) => Value::UDim(*v),
        Variant::UDim2(v) => Value::UDim2(*v),
        Variant::Vector2(v) => Value::Vector2(*v),
        Variant::Vector3(v) => Value::Vector3(*v),
        Variant::Color3(v) => Value::Color3(*v),
        Variant::Color3uint8(v) => Value::Color3(dom::Color3::new(
            v.r as f32 / 255.0,
            v.g as f32 / 255.0,
            v.b as f32 / 255.0,
        )),
        Variant::Rect(v) => Value::Rect(*v),
        Variant::NumberRange(v) => Value::NumberRange(*v),
        Variant::NumberSequence(v) => Value::NumberSequence(Rc::new(v.clone())),
        Variant::ColorSequence(v) => Value::ColorSequence(Rc::new(v.clone())),
        Variant::OptionalCFrame(None) => return None,
        Variant::OptionalCFrame(Some(cframe)) => Value::Dom(Rc::new(Variant::CFrame(*cframe))),
        Variant::PhysicalProperties(dom::PhysicalProperties::Default) => return None,
        Variant::Axes(_)
        | Variant::BrickColor(_)
        | Variant::CFrame(_)
        | Variant::Faces(_)
        | Variant::Ray(_)
        | Variant::Region3(_)
        | Variant::Region3int16(_)
        | Variant::UniqueId(_)
        | Variant::Vector2int16(_)
        | Variant::Vector3int16(_)
        | Variant::PhysicalProperties(_) => Value::Dom(Rc::new(value.clone())),
        _ => return None,
    })
}

/// A recorded default as a value (Reflection.Default's decode step).
pub fn decode_default(reflection: &Reflection, raw: &RawDefault) -> Option<Value> {
    match raw {
        RawDefault::Json(json) => decode_json(reflection, json),
        RawDefault::Dom(variant) => from_dom(reflection, variant),
        RawDefault::EnumItem(item) => Some(Value::EnumItem(*item)),
    }
}

/// Default ("zero") value of a property type with no recorded default (datatypes.luau ZeroValue).
pub fn zero_value(
    reflection: &Reflection,
    script_type: ScriptType,
    script_type_name: &str,
    enum_type: Option<u32>,
) -> Option<Value> {
    Some(match script_type {
        ScriptType::Number => Value::Number(0.0),
        ScriptType::String => Value::String(Str::default()),
        ScriptType::Boolean => Value::Bool(false),
        ScriptType::EnumItem => {
            let id = enum_type?;
            Value::EnumItem(*reflection.enums[id as usize].items.first()?)
        }
        ScriptType::UDim => Value::UDim(dom::UDim::new(0.0, 0)),
        ScriptType::UDim2 => Value::UDim2(dom::UDim2::new(dom::UDim::new(0.0, 0), dom::UDim::new(0.0, 0))),
        ScriptType::Vector2 => Value::Vector2(dom::Vector2::new(0.0, 0.0)),
        ScriptType::Vector3 => Value::Vector3(dom::Vector3::new(0.0, 0.0, 0.0)),
        ScriptType::Color3 => Value::Color3(dom::Color3::new(0.0, 0.0, 0.0)),
        ScriptType::CFrame => Value::Dom(Rc::new(Variant::CFrame(dom::CFrame::new(
            dom::Vector3::new(0.0, 0.0, 0.0),
            dom::Matrix3::identity(),
        )))),
        ScriptType::Rect => Value::Rect(dom::Rect::new(dom::Vector2::new(0.0, 0.0), dom::Vector2::new(0.0, 0.0))),
        ScriptType::NumberRange => Value::NumberRange(dom::NumberRange::new(0.0, 0.0)),
        ScriptType::Font => Value::Font(Rc::new(FontValue {
            family: Str::from("rbxasset://fonts/families/LegacyArial.json"),
            weight: reflection.enum_item("FontWeight", "Regular")?,
            style: reflection.enum_item("FontStyle", "Normal")?,
        })),
        ScriptType::Content => Value::Content(Rc::new(ContentValue::None)),
        _ => {
            let _ = script_type_name;
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflection::{K, NameId};

    #[test]
    fn decodes_dump_defaults() {
        let reflection = Reflection::shared().unwrap();
        let frame = reflection.class_by_name("Frame").unwrap();
        let defaults = frame.raw_defaults.as_ref().unwrap();
        let color = decode_default(&reflection, &defaults[&(K::BackgroundColor3 as NameId)]).unwrap();
        let Value::Color3(color) = color else { panic!() };
        assert_eq!(color.r, 0.6392157077789307f64 as f32);
        let mode = decode_default(&reflection, &defaults[&(K::BorderMode as NameId)]).unwrap();
        assert_eq!(mode.as_enum(), reflection.enum_item("BorderMode", "Outline"));
        let size = decode_default(&reflection, &defaults[&(K::Size as NameId)]).unwrap();
        assert!(size.as_udim2().is_some());
        // (writing NaN over NaN fires Changed again in Roblox: tools/parity/probes/changed.luau)
        assert!(!Value::Number(f64::NAN).same(&Value::Number(f64::NAN)));
        assert!(Value::Number(0.0).same(&Value::Number(-0.0)));
        assert!(!Value::Number(0.0).same(&Value::Bool(false)));
    }
}

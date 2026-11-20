//! Engine values as a recording writes them: playback JSON (ror's README "Playback": numbers,
//! booleans, strings and content URIs, `[x, y, z]` vectors and colours, 12-number CFrames,
//! `[min, max]` ranges, `[[t, v, envelope]...]` / `[[t, r, g, b]...]` sequences, `[xs, xo, ys, yo]`
//! UDim2s, `[scale, offset]` UDims, `[minX, minY, maxX, maxY]` Rects, enum items by value), and
//! rbx-dom variants for the scene file (by the type rbx-dom's reflection gives the property).

use std::borrow::Cow;

use glam::Affine3A;
use rbx_dom_weak::types::{self as dom, Variant, VariantType};
use rbx_reflection::DataType;

use serde_json::{Value as Json, json};

use crate::{
    Engine, InstanceId,
    value::Value,
    world::frame,
};

/// A playback number: five decimals, as the recorder this one replaced wrote them (and no negative zero).
pub fn round(value: f64) -> f64 {
    let rounded = (value * 1e5).round() / 1e5;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn number(value: f64) -> Json {
    json!(round(value))
}

/// A CFrame as playback numbers: `CFrame.new(x, y, z, R00, R01, R02, ...)`'s twelve.
pub fn cframe_json(transform: &Affine3A) -> Json {
    let cframe = frame::from_affine(transform);
    let (p, o) = (cframe.position, cframe.orientation);
    Json::Array([p.x, p.y, p.z, o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z].iter().map(|&v| number(v as f64)).collect())
}

/// A content URI naming a relative local file (`checker.png` the project holds, `./x.png`, `output/x.mesh`,
/// `file://output/x.mesh`), as the absolute `file://` URI of the file the engine read for it (`Options::local_file`: the
/// project's folder), `%` escaped as ror's file URIs are read, a `#` fragment kept; None for anything else (an asset, an
/// absolute path or file URI, nothing). A recording names the file so: ror reads a relative path from the folder it runs
/// in, which is not the project's.
pub fn local_uri(engine: &Engine, uri: &str) -> Option<String> {
    let text = uri.trim();
    let (path, fragment) = match text.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (text, None),
    };
    let named = if path.get(..7).is_some_and(|head| head.eq_ignore_ascii_case("file://")) { &path[7..] } else { path };
    let bytes = named.as_bytes();
    let drive = |b: &[u8]| b.len() > 1 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if named.is_empty() || drive(bytes) || named.starts_with(['/', '\\']) || std::path::Path::new(named).is_absolute() {
        return None;
    }
    let file = engine.options.local_file(path)?;
    let mut resolved = format!("file://{}", file.display().to_string().replace('%', "%25"));
    if let Some(fragment) = fragment {
        resolved.push('#');
        resolved.push_str(fragment);
    }
    Some(resolved)
}

/// A Content or content ID `variant` naming a relative local file, made to name it by [`local_uri`].
pub fn localize(engine: &Engine, variant: &mut Variant) {
    let resolved = match &*variant {
        Variant::Content(content) => match content.value() {
            dom::ContentType::Uri(uri) => local_uri(engine, uri).map(|uri| Variant::Content(dom::Content::from_uri(uri))),
            _ => None,
        },
        Variant::ContentId(id) => local_uri(engine, id.as_str()).map(|uri| Variant::ContentId(uri.into())),
        _ => None,
    };
    if let Some(resolved) = resolved {
        *variant = resolved;
    }
}

/// `value` as a playback value; None when playback cannot set it (instances, fonts, script values)
/// or it holds a NaN or an infinity (Roblox draws nothing then).
pub fn to_json(engine: &Engine, value: &Value) -> Option<Json> {
    let encoded = match value {
        Value::Bool(b) => json!(b),
        Value::Number(n) => number(*n),
        Value::String(s) => json!(s.to_str_lossy()),
        Value::EnumItem(item) => json!(engine.reflection.enum_item_info(*item).value),
        Value::Vector3(v) => json!([round(v.x as f64), round(v.y as f64), round(v.z as f64)]),
        Value::Vector2(v) => json!([round(v.x as f64), round(v.y as f64)]),
        Value::Color3(c) => json!([round(c.r as f64), round(c.g as f64), round(c.b as f64)]),
        Value::UDim(u) => json!([round(u.scale as f64), u.offset]),
        Value::UDim2(u) => json!([round(u.x.scale as f64), u.x.offset, round(u.y.scale as f64), u.y.offset]),
        Value::Rect(r) => json!([round(r.min.x as f64), round(r.min.y as f64), round(r.max.x as f64), round(r.max.y as f64)]),
        Value::NumberRange(r) => json!([round(r.min as f64), round(r.max as f64)]),
        Value::NumberSequence(s) => Json::Array(s.keypoints.iter().map(|k| json!([round(k.time as f64), round(k.value as f64), round(k.envelope as f64)])).collect()),
        Value::ColorSequence(s) => {
            Json::Array(s.keypoints.iter().map(|k| json!([round(k.time as f64), round(k.color.r as f64), round(k.color.g as f64), round(k.color.b as f64)])).collect())
        }
        Value::Content(content) => match content.location() {
            Some(uri) => json!(uri.to_str_lossy()),
            None => json!(""),
        },
        Value::Dom(variant) => match &**variant {
            Variant::CFrame(cframe) => cframe_json(&frame::to_affine(cframe)),
            Variant::BrickColor(brick) => {
                let c = brick.to_color3uint8();
                json!([round(c.r as f64 / 255.0), round(c.g as f64 / 255.0), round(c.b as f64 / 255.0)])
            }
            _ => return None,
        },
        _ => return None,
    };
    finite(&encoded).then_some(encoded)
}

fn finite(value: &Json) -> bool {
    match value {
        Json::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        Json::Array(items) => items.iter().all(finite),
        _ => true,
    }
}

/// The numbers of a playback value (None for strings and booleans): two values interpolate when
/// both have the same count.
pub fn numbers(value: &Json) -> Option<Vec<f64>> {
    fn flatten(value: &Json, out: &mut Vec<f64>) -> bool {
        match value {
            Json::Number(n) => {
                out.push(n.as_f64().unwrap_or_default());
                true
            }
            Json::Array(items) => items.iter().all(|item| flatten(item, out)),
            _ => false,
        }
    }
    let mut out = Vec::new();
    flatten(value, &mut out).then_some(out)
}

/// rbx-dom's descriptor of `class.property` (walking up the superclasses).
pub fn dom_property(class_name: &str, property: &str) -> Option<&'static rbx_reflection::PropertyDescriptor<'static>> {
    let database = rbx_reflection_database::get().ok()?;
    let mut current = database.classes.get(class_name);
    while let Some(class) = current {
        if let Some(descriptor) = class.properties.get(property) {
            return Some(descriptor);
        }
        current = class.superclass.as_ref().and_then(|name| database.classes.get(name.as_ref()));
    }
    None
}

/// `value` as the variant rbx-dom stores `data_type` in; `refs` maps recorded instances to their
/// copies (other instance references become none).
pub fn to_variant(engine: &Engine, value: &Value, data_type: &DataType, refs: &dyn Fn(InstanceId) -> Option<dom::Ref>) -> Option<Variant> {
    let DataType::Value(kind) = data_type else {
        // an enum property
        return match value {
            Value::EnumItem(item) => Some(Variant::Enum(dom::Enum::from_u32(engine.reflection.enum_item_info(*item).value as u32))),
            Value::Number(n) => Some(Variant::Enum(dom::Enum::from_u32(*n as u32))),
            _ => None,
        };
    };
    Some(match (kind, value) {
        (VariantType::Bool, Value::Bool(b)) => Variant::Bool(*b),
        (VariantType::Float32, Value::Number(n)) => Variant::Float32(*n as f32),
        (VariantType::Float64, Value::Number(n)) => Variant::Float64(*n),
        (VariantType::Int32, Value::Number(n)) => Variant::Int32(*n as i32),
        (VariantType::Int64, Value::Number(n)) => Variant::Int64(*n as i64),
        (VariantType::String, Value::String(s)) => Variant::String(s.to_str_lossy().into_owned()),
        (VariantType::BinaryString, Value::String(s)) => Variant::BinaryString(s.as_bytes().to_vec().into()),
        (VariantType::ContentId, Value::String(s)) => Variant::ContentId(s.to_str_lossy().as_ref().into()),
        (VariantType::ContentId, Value::Content(c)) => Variant::ContentId(c.uri().map(|u| u.to_str_lossy().into_owned()).unwrap_or_default().into()),
        (VariantType::Content, Value::Content(c)) => Variant::Content(match c.location() {
            Some(uri) if !uri.is_empty() => dom::Content::from_uri(uri.to_str_lossy().into_owned()),
            _ => dom::Content::none(),
        }),
        (VariantType::Content, Value::String(s)) => Variant::Content(if s.is_empty() { dom::Content::none() } else { dom::Content::from_uri(s.to_str_lossy().into_owned()) }),
        (VariantType::Vector3, Value::Vector3(v)) => Variant::Vector3(*v),
        (VariantType::Vector2, Value::Vector2(v)) => Variant::Vector2(*v),
        (VariantType::Color3, Value::Color3(c)) => Variant::Color3(*c),
        (VariantType::Color3uint8, Value::Color3(c)) => Variant::Color3uint8(dom::Color3uint8::new(
            (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        )),
        (VariantType::UDim, Value::UDim(u)) => Variant::UDim(*u),
        (VariantType::UDim2, Value::UDim2(u)) => Variant::UDim2(*u),
        (VariantType::Rect, Value::Rect(r)) => Variant::Rect(*r),
        (VariantType::NumberRange, Value::NumberRange(r)) => Variant::NumberRange(*r),
        (VariantType::NumberSequence, Value::NumberSequence(s)) => Variant::NumberSequence((**s).clone()),
        (VariantType::ColorSequence, Value::ColorSequence(s)) => Variant::ColorSequence((**s).clone()),
        (VariantType::Ref, Value::Instance(id)) => Variant::Ref(refs(*id).unwrap_or_else(dom::Ref::none)),
        (VariantType::Ref, Value::Nil) => Variant::Ref(dom::Ref::none()),
        (VariantType::Font, Value::Font(font)) => {
            let weight = engine.reflection.enum_item_info(font.weight).value;
            let style = engine.reflection.enum_item_info(font.style).value;
            Variant::Font(dom::Font {
                family: font.family.to_str_lossy().into_owned(),
                weight: dom::FontWeight::from_u16(weight as u16).unwrap_or_default(),
                style: dom::FontStyle::from_u8(style as u8).unwrap_or_default(),
                cached_face_id: None,
            })
        }
        (VariantType::CFrame, Value::Dom(v)) | (VariantType::OptionalCFrame, Value::Dom(v)) => match (&**v, kind) {
            (Variant::CFrame(c), VariantType::OptionalCFrame) => Variant::OptionalCFrame(Some(*c)),
            (Variant::OptionalCFrame(c), VariantType::OptionalCFrame) => Variant::OptionalCFrame(*c),
            (Variant::CFrame(c), _) => Variant::CFrame(*c),
            _ => return None,
        },
        (_, Value::Dom(v)) if v.ty() == *kind => (**v).clone(),
        _ => return None,
    })
}

/// `value` as a variant of its own type, for a property rbx-dom's database does not know (numbers
/// by the engine type the dump gives them).
pub fn untyped_variant(engine: &Engine, value: &Value, engine_type: crate::reflection::EngineType) -> Option<Variant> {
    use crate::reflection::EngineType;
    Some(match value {
        Value::Bool(b) => Variant::Bool(*b),
        Value::Number(n) => match engine_type {
            EngineType::Integer | EngineType::Int64 => Variant::Int32(*n as i32),
            EngineType::Float => Variant::Float32(*n as f32),
            _ => Variant::Float64(*n),
        },
        Value::String(s) if engine_type == EngineType::ContentId => Variant::ContentId(s.to_str_lossy().as_ref().into()),
        Value::String(s) => Variant::String(s.to_str_lossy().into_owned()),
        Value::EnumItem(item) => Variant::Enum(dom::Enum::from_u32(engine.reflection.enum_item_info(*item).value as u32)),
        Value::UDim(u) => Variant::UDim(*u),
        Value::UDim2(u) => Variant::UDim2(*u),
        Value::Vector2(v) => Variant::Vector2(*v),
        Value::Vector3(v) => Variant::Vector3(*v),
        Value::Color3(c) => Variant::Color3(*c),
        Value::Rect(r) => Variant::Rect(*r),
        Value::NumberRange(r) => Variant::NumberRange(*r),
        Value::NumberSequence(s) => Variant::NumberSequence((**s).clone()),
        Value::ColorSequence(s) => Variant::ColorSequence((**s).clone()),
        Value::Content(c) => Variant::Content(match c.location() {
            Some(uri) if !uri.is_empty() => dom::Content::from_uri(uri.to_str_lossy().into_owned()),
            _ => dom::Content::none(),
        }),
        Value::Dom(v) => (**v).clone(),
        _ => return None,
    })
}

/// The canonical, serialized properties of a class as rbx-dom lists them: (name, type).
pub fn serialized_properties(class_name: &str) -> Vec<(Cow<'static, str>, &'static DataType<'static>)> {
    let Ok(database) = rbx_reflection_database::get() else { return Vec::new() };
    let mut result = Vec::new();
    let mut current = database.classes.get(class_name);
    while let Some(class) = current {
        for (name, descriptor) in &class.properties {
            let serializes = matches!(
                &descriptor.kind,
                rbx_reflection::PropertyKind::Canonical { serialization: rbx_reflection::PropertySerialization::Serializes | rbx_reflection::PropertySerialization::SerializesAs(_) }
            );
            if serializes && !result.iter().any(|(n, _): &(Cow<'static, str>, _)| n == name) {
                result.push((name.clone(), &descriptor.data_type));
            }
        }
        current = class.superclass.as_ref().and_then(|name| database.classes.get(name.as_ref()));
    }
    result
}

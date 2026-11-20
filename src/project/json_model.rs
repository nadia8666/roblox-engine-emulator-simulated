//! Rojo's JSON models (`*.model.json`, https://rojo.space/docs/v7/sync-details/#json-models): an
//! instance tree written by hand, which Rojo puts in the place as one instance named after the file
//! (a project RemoteEvent is often just `{"className": "RemoteEvent"}`).
//!
//! The format is Rojo 7's (its src/snapshot_middleware/json_model.rs and src/resolution.rs):
//! `className` (or `ClassName`); `name` / `Name`, a child's name, its class name when absent (the top
//! instance takes the file's name and its own is ignored, as since Rojo 6); `properties` /
//! `Properties`; `attributes`; `children` / `Children`. A property value is explicit, rbx-dom's form
//! of a value (`{"Vector3": [1, 2, 3]}`), or implicit, read by the property's type in rbx-dom's
//! reflection database: a string for a string, Content or enum item name, a number, a bool, `[x, y]` /
//! `[x, y, z]` for vectors and colours, twelve numbers for a CFrame, an array of strings for Tags, a
//! Font object. A property rbx-dom's database does not have (it is older than the engine's Studio
//! dump: InputBinding.Type, SurfaceAppearance.ResampleMode) is read by the engine's reflection, and
//! an enum item name by the engine's enum (KeyCode.MouseWheel), as the Rojo release that knows them
//! reads them. An attribute is explicit, a string, a number or a bool. A file holding only white
//! space makes nothing; one Rojo would reject (not a model, an unknown property given implicitly, a
//! value of the wrong type) makes nothing either and is reported.
//!
//! The tree then goes through rbx_binary and back, as `rojo build` writes it into a place: a property
//! that does not serialize is dropped (a part's Position: its CFrame holds it), a part's Color is kept
//! as Color3uint8, and the result converts as any model file does ([`super::ModelPlan`]).

use std::collections::BTreeMap;

use rbx_dom_weak::{
    InstanceBuilder, WeakDom,
    types::{Attributes, CFrame, Color3, Content, ContentId, Enum, Font, MaterialColors, Matrix3, Tags, Variant, VariantType, Vector2, Vector3},
};
use rbx_reflection::DataType;
use serde::Deserialize;

use crate::Reflection;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonModel {
    #[serde(alias = "Name")]
    name: Option<String>,
    #[serde(alias = "ClassName")]
    class_name: String,
    #[serde(alias = "Children", default)]
    children: Vec<JsonModel>,
    #[serde(alias = "Properties", default)]
    properties: BTreeMap<String, Unresolved>,
    #[serde(default)]
    attributes: BTreeMap<String, Unresolved>,
}

/// A value as written: explicit (rbx-dom's tagged form) or one whose type the property decides.
#[derive(Deserialize)]
#[serde(untagged)]
enum Unresolved {
    Explicit(Variant),
    Implicit(Implicit),
}

/// The shapes an implicit value can take, tried in Rojo's order (its AmbiguousValue).
#[derive(Deserialize)]
#[serde(untagged)]
enum Implicit {
    Bool(bool),
    String(String),
    StringArray(Vec<String>),
    Number(f64),
    Array2([f64; 2]),
    Array3([f64; 3]),
    // (no property takes four numbers: read so that one given is reported as such, as Rojo does)
    #[allow(dead_code)]
    Array4([f64; 4]),
    Array12([f64; 12]),
    Attributes(Attributes),
    Font(Font),
    MaterialColors(MaterialColors),
}

impl Implicit {
    fn describe(&self) -> &'static str {
        match self {
            Implicit::Bool(_) => "a bool",
            Implicit::String(_) => "a string",
            Implicit::StringArray(_) => "an array of strings",
            Implicit::Number(_) => "a number",
            Implicit::Array2(_) => "an array of two numbers",
            Implicit::Array3(_) => "an array of three numbers",
            Implicit::Array4(_) => "an array of four numbers",
            Implicit::Array12(_) => "an array of twelve numbers",
            Implicit::Attributes(_) => "an object of attributes",
            Implicit::Font(_) => "a Font object",
            Implicit::MaterialColors(_) => "a MaterialColors object",
        }
    }

    /// The value of `class_name.property` this stands for, by the property's type.
    fn resolve(self, class_name: &str, property: &str) -> Result<Variant, String> {
        match property_type(class_name, property).ok_or_else(|| format!("unknown property {class_name}.{property} (an explicit value names its type)"))? {
            PropertyType::Enum(enum_name) => match self {
                Implicit::String(item) => enum_item_value(&enum_name, &item).map(|value| Enum::from_u32(value).into()).ok_or_else(|| format!("{class_name}.{property}: {item} is not an item of Enum.{enum_name}")),
                other => Err(format!("{class_name}.{property} takes the name of an Enum.{enum_name} item, not {}", other.describe())),
            },
            PropertyType::Unknown => Err(format!("{class_name}.{property}: a type this reader does not know")),
            PropertyType::Value(kind) => match (kind, self) {
                (VariantType::Bool, Implicit::Bool(value)) => Ok(value.into()),
                (VariantType::Float32, Implicit::Number(value)) => Ok((value as f32).into()),
                (VariantType::Float64, Implicit::Number(value)) => Ok(value.into()),
                (VariantType::Int32, Implicit::Number(value)) => Ok((value as i32).into()),
                (VariantType::Int64, Implicit::Number(value)) => Ok((value as i64).into()),
                (VariantType::String, Implicit::String(value)) => Ok(value.into()),
                (VariantType::Tags, Implicit::StringArray(value)) => Ok(Tags::from(value).into()),
                (VariantType::Content, Implicit::String(value)) => Ok(Content::from(value).into()),
                (VariantType::ContentId, Implicit::String(value)) => Ok(ContentId::from(value).into()),
                (VariantType::Vector2, Implicit::Array2(v)) => Ok(Vector2::new(v[0] as f32, v[1] as f32).into()),
                (VariantType::Vector3, Implicit::Array3(v)) => Ok(Vector3::new(v[0] as f32, v[1] as f32, v[2] as f32).into()),
                (VariantType::Color3, Implicit::Array3(v)) => Ok(Color3::new(v[0] as f32, v[1] as f32, v[2] as f32).into()),
                (VariantType::CFrame, Implicit::Array12(v)) => {
                    let v = v.map(|n| n as f32);
                    let axes = Matrix3::new(Vector3::new(v[3], v[4], v[5]), Vector3::new(v[6], v[7], v[8]), Vector3::new(v[9], v[10], v[11]));
                    Ok(CFrame::new(Vector3::new(v[0], v[1], v[2]), axes).into())
                }
                (VariantType::Attributes, Implicit::Attributes(value)) => Ok(value.into()),
                (VariantType::Font, Implicit::Font(value)) => Ok(value.into()),
                (VariantType::MaterialColors, Implicit::MaterialColors(value)) => Ok(value.into()),
                (kind, other) => Err(format!("{class_name}.{property} takes a {kind:?}, not {}", other.describe())),
            },
        }
    }
}

/// What an implicit value of a property is read as.
enum PropertyType {
    Enum(String),
    Value(VariantType),
    Unknown,
}

/// `class_name.property`'s type: rbx-dom's descriptor, as Rojo resolves it, else the engine's reflection for a property
/// rbx-dom's (older) database does not have.
fn property_type(class_name: &str, property: &str) -> Option<PropertyType> {
    if let Some(descriptor) = crate::record::encode::dom_property(class_name, property) {
        return Some(match &descriptor.data_type {
            DataType::Enum(enum_name) => PropertyType::Enum(enum_name.to_string()),
            DataType::Value(kind) => PropertyType::Value(*kind),
            _ => PropertyType::Unknown,
        });
    }
    let reflection = Reflection::shared().ok()?;
    let info = reflection.class_property(reflection.class_id(class_name)?, reflection.name_id(property)?)?;
    if let Some(enum_name) = &info.enum_name {
        return Some(PropertyType::Enum(enum_name.to_string()));
    }
    Some(match &*info.engine_type_name {
        "bool" => PropertyType::Value(VariantType::Bool),
        "float" => PropertyType::Value(VariantType::Float32),
        "double" => PropertyType::Value(VariantType::Float64),
        "int" => PropertyType::Value(VariantType::Int32),
        "int64" => PropertyType::Value(VariantType::Int64),
        "string" => PropertyType::Value(VariantType::String),
        "Content" => PropertyType::Value(VariantType::Content),
        "ContentId" => PropertyType::Value(VariantType::ContentId),
        "Vector2" => PropertyType::Value(VariantType::Vector2),
        "Vector3" => PropertyType::Value(VariantType::Vector3),
        "Color3" => PropertyType::Value(VariantType::Color3),
        "CoordinateFrame" => PropertyType::Value(VariantType::CFrame),
        "Font" => PropertyType::Value(VariantType::Font),
        _ => PropertyType::Unknown,
    })
}

/// The value of the item named `item` of `enum_name`: the engine's enum first (it has the items a newer Studio added),
/// then rbx-dom's.
fn enum_item_value(enum_name: &str, item: &str) -> Option<u32> {
    if let Ok(reflection) = Reflection::shared() {
        if let Some(id) = reflection.enum_item(enum_name, item) {
            return Some(reflection.enum_item_info(id).value as u32);
        }
    }
    let database = rbx_reflection_database::get().ok()?;
    database.enums.get(enum_name)?.items.get(item).copied()
}

impl Unresolved {
    fn resolve(self, class_name: &str, property: &str) -> Result<Variant, String> {
        match self {
            Unresolved::Explicit(value) => Ok(value),
            Unresolved::Implicit(value) => value.resolve(class_name, property),
        }
    }

    /// An attribute's value: explicit, or a string, a number (Float64) or a bool.
    fn resolve_attribute(self, name: &str) -> Result<Variant, String> {
        match self {
            Unresolved::Explicit(value) => Ok(value),
            Unresolved::Implicit(Implicit::Bool(value)) => Ok(value.into()),
            Unresolved::Implicit(Implicit::Number(value)) => Ok(value.into()),
            Unresolved::Implicit(Implicit::String(value)) => Ok(value.into()),
            Unresolved::Implicit(other) => Err(format!("attribute {name}: {} needs its type named", other.describe())),
        }
    }
}

/// The instance `model` describes, named `name`.
fn build(model: JsonModel, name: String) -> Result<InstanceBuilder, String> {
    let JsonModel { class_name, children, properties, attributes, .. } = model;
    let mut builder = InstanceBuilder::new(class_name.as_str()).with_name(name);
    for (property, value) in properties {
        let value = value.resolve(&class_name, &property)?;
        builder.add_property(property.as_str(), value);
    }
    if !attributes.is_empty() {
        let mut list = Attributes::new();
        for (attribute, value) in attributes {
            let value = value.resolve_attribute(&attribute)?;
            list.insert(attribute, value);
        }
        builder.add_property("Attributes", list);
    }
    for child in children {
        let child_name = child.name.clone().unwrap_or_else(|| child.class_name.clone());
        builder.add_child(build(child, child_name)?);
    }
    Ok(builder)
}

/// A `.model.json` file's instance tree (its top instance named `name`) as a decoded model whose root
/// holds it; None for a file of white space alone.
pub fn decode(bytes: &[u8], name: &str) -> Result<Option<WeakDom>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    // (a byte order mark, as some Windows editors write one, is not part of the JSON)
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Ok(None);
    }
    let model: JsonModel = serde_json::from_str(text).map_err(|error| format!("not a JSON model: {error}"))?;
    let top = build(model, name.to_string())?;
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let top = dom.insert(dom.root_ref(), top);
    let mut file = Vec::new();
    rbx_binary::to_writer(&mut file, &dom, &[top]).map_err(|error| error.to_string())?;
    super::decode_model(&file).map(Some)
}

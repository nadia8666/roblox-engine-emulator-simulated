//! Class database: the live engine's ReflectionService dump (`data/reflection.json`) merged over
//! rbx-dom's database for classes the plugin security context cannot see (e.g. Player). Port of
//! `runtime/engine/reflection.luau` and the interned enums of `runtime/engine/enums.luau`.
//!
//! Member names are interned once per process into [`NameId`]s; the names every engine subsystem
//! reads natively (layout, export, behaviours) are pre-interned at fixed ids ([`K`]) so hot paths
//! compare integers, never strings.

use std::sync::{Arc, OnceLock};

use rbx_dom_weak::types::Variant;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Deserialize;

pub type ClassId = u32;
pub type NameId = u32;
pub type EnumId = u32;
pub type EnumItemId = u32;

/// Declares the pre-interned member names: `K::Size as NameId` is the id of "Size".
macro_rules! known_names {
    ($($name:ident),* $(,)?) => {
        #[repr(u32)]
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum K { $($name),* }
        /// Names of [`K`], in id order.
        pub const KNOWN_NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

known_names!(
    // Instance
    Name, Parent, ClassName, Archivable, Changed, ChildAdded, ChildRemoved, DescendantAdded,
    DescendantRemoving, AncestryChanged, Destroying, AttributeChanged, Source, Value,
    // Instance methods (behaviours)
    GetChildren, GetDescendants, FindFirstChild, FindFirstChildOfClass, FindFirstChildWhichIsA,
    FindFirstDescendant, FindFirstAncestor, FindFirstAncestorOfClass, FindFirstAncestorWhichIsA,
    IsA, IsDescendantOf, IsAncestorOf, GetFullName, Clone, Destroy, Remove, ClearAllChildren,
    WaitForChild, GetPropertyChangedSignal, GetAttribute, GetAttributes, SetAttribute,
    GetAttributeChangedSignal, AddTag, RemoveTag, HasTag, GetTags, GetActor, IsPropertyModified,
    ResetPropertyToDefault, GetDebugId, GetStyled, children, clone, destroy, remove,
    findFirstChild, getChildren, isA, isDescendantOf,
    // DataModel / services
    GetService, service, FindService, IsLoaded, Workspace, BindToClose, GetJobsInfo, PlaceId,
    GameId, JobId, CreatorId, CreatorType, PlaceVersion, PrivateServerId, PrivateServerOwnerId,
    CurrentCamera, ViewportSize, DistributedGameTime, GetServerTimeNow, GetRealPhysicsFPS,
    LocalPlayer, localPlayer, GetPlayers, GetPlayerByUserId, DisplayName, UserId,
    IsStudio, IsClient, IsServer, IsRunning, IsRunMode, IsEdit, BindToRenderStep,
    UnbindFromRenderStep, Heartbeat, RenderStepped, PreRender, PreAnimation, Stepped,
    PreSimulation, PostSimulation, Fire, Event, Invoke, OnInvoke,
    // GuiBase2d / GuiObject / LayerCollector (layout + export)
    AbsolutePosition, AbsoluteSize, AbsoluteRotation, AnchorPoint, Position, Size, Rotation,
    Visible, ZIndex, LayoutOrder, BackgroundColor3, BackgroundTransparency, BorderColor3,
    BorderSizePixel, BorderMode, ClipsDescendants, AutomaticSize, SizeConstraint, Enabled,
    DisplayOrder, ZIndexBehavior, IgnoreGuiInset, ResetOnSpawn, Transparency,
    // Text / image
    Text, RichText, TextScaled, TextWrapped, TextSize, TextColor3, TextTransparency,
    TextXAlignment, TextYAlignment, FontFace, Font, TextBounds, Image, ImageContent,
    ImageColor3, ImageTransparency, ImageRectOffset, ImageRectSize, ScaleType, SliceCenter,
    SliceScale, TileSize, ResampleMode,
    // Decorations
    CornerRadius, TopLeftRadius, TopRightRadius, BottomRightRadius, BottomLeftRadius, Thickness,
    Color, LineJoinMode, ApplyStrokeMode, BorderStrokePosition, StrokeSizingMode, BorderOffset,
    Offset, BlurRadius, Spread, Inset, ShowBehindParent, Mode, GroupColor3, GroupTransparency,
    // ScrollingFrame
    CanvasPosition, CanvasSize, ScrollBarImageColor3, ScrollBarImageTransparency, TopImage,
    MidImage, BottomImage, ScrollingEnabled, VerticalScrollBarPosition, ScrollingDirection,
    ScrollBarThickness,
    // UIScale
    Scale,
);

/// A property's script-visible type as `Coerce` distinguishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptType {
    Number,
    String,
    Boolean,
    EnumItem,
    Instance,
    Content,
    /// `any`, `Variant`, empty, `Tuple`, `Dictionary`, `Array`: stored as given
    Any,
    Vector3,
    // Types whose mismatch is an assignment error
    Font,
    UDim2,
    UDim,
    Vector2,
    Color3,
    Rect,
    ColorSequence,
    NumberSequence,
    NumberRange,
    CFrame,
    BrickColor,
    /// Any other type name: mismatches are stored anyway (as the Luau runtime does)
    Other,
}

impl ScriptType {
    fn parse(name: &str) -> Self {
        match name {
            "number" => Self::Number,
            "string" => Self::String,
            "boolean" => Self::Boolean,
            "EnumItem" => Self::EnumItem,
            "Instance" => Self::Instance,
            "Content" => Self::Content,
            "any" | "Variant" | "" | "Tuple" | "Dictionary" | "Array" => Self::Any,
            "Vector3" => Self::Vector3,
            "Font" => Self::Font,
            "UDim2" => Self::UDim2,
            "UDim" => Self::UDim,
            "Vector2" => Self::Vector2,
            "Color3" => Self::Color3,
            "Rect" => Self::Rect,
            "ColorSequence" => Self::ColorSequence,
            "NumberSequence" => Self::NumberSequence,
            "NumberRange" => Self::NumberRange,
            "CFrame" => Self::CFrame,
            "BrickColor" => Self::BrickColor,
            _ => Self::Other,
        }
    }

    /// Types whose mismatched assignments raise ("Unable to assign property X. T expected").
    pub fn strict(self) -> bool {
        matches!(
            self,
            Self::Font
                | Self::UDim2
                | Self::UDim
                | Self::Vector2
                | Self::Color3
                | Self::Vector3
                | Self::Rect
                | Self::ColorSequence
                | Self::NumberSequence
                | Self::NumberRange
                | Self::CFrame
                | Self::BrickColor
        )
    }
}

/// Engine storage kinds `Coerce` treats specially.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineType {
    /// int, int32, Int32: truncated toward zero, out of range (or not finite) the minimum
    Integer,
    /// int64, Int64: rounded half away from zero, out of range (or not finite) zero
    Int64,
    /// float: single precision (TextSize of text objects: whole 1..100)
    Float,
    ContentId,
    Other,
}

#[derive(Clone, Debug)]
pub struct PropertyInfo {
    pub name: NameId,
    pub script_type: ScriptType,
    /// The script type as named in the database (error messages, typeof comparisons)
    pub script_type_name: Box<str>,
    pub engine_type: EngineType,
    pub engine_type_name: Box<str>,
    /// The enum named by the dump (may name an enum that does not exist)
    pub enum_name: Option<Box<str>>,
    pub enum_type: Option<EnumId>,
    pub read_only: bool,
    /// Scripts can read it: the dump permits Read (NotScriptable properties such as PVInstance.Origin
    /// and Model.WorldPivotData permit nothing, so `part.Origin` finds a child named Origin)
    pub scriptable: bool,
    pub serialized: bool,
    pub owner: ClassId,
}

/// A default as recorded: the dump's JSON encoding or an rbx-dom value (Lune supplement).
#[derive(Clone, Debug)]
pub enum RawDefault {
    Json(serde_json::Value),
    Dom(Variant),
    /// An rbx-dom enum default resolved to the interned item
    EnumItem(EnumItemId),
}

#[derive(Debug)]
pub struct ClassInfo {
    pub id: ClassId,
    pub name: Box<str>,
    pub superclass: Option<ClassId>,
    pub creatable: bool,
    pub service: bool,
    ancestors: Box<[u64]>,
    /// Resolved (inherited) members
    pub properties: FxHashMap<NameId, u32>,
    pub events: FxHashSet<NameId>,
    pub methods: FxHashSet<NameId>,
    pub callbacks: FxHashSet<NameId>,
    /// Own recorded defaults (`None`: the class has no default table at all)
    pub raw_defaults: Option<FxHashMap<NameId, RawDefault>>,
    /// The dump lists it: ReflectionService (GetClass, GetClasses) reports it. The classes only rbx-dom describes are the ones a
    /// script cannot get there (Player, CoreGui, UserGameSettings: GetClass gives nil for them in a playing client)
    pub reflected: bool,
    /// The dump's `serialized` (ReflectedClass.Serialized)
    pub serialized: bool,
    /// The dump's `display` as (key, text) pairs (ReflectedClass.Display: Category, and DeprecationMessage where the class has one)
    pub display: Box<[(Box<str>, Box<str>)]>,
    /// Every class whose superclass this is, rbx-dom's included, in name order (ReflectedClass.Subclasses)
    pub subclasses: Box<[ClassId]>,
    /// The dump's own deprecated lower-case members (inherited) and the member each stands for, by kind
    /// ([`Reflection::property_alias`], [`Reflection::method_alias`]): `brickColor` is BrickColor, `getService` GetService
    property_aliases: FxHashMap<NameId, NameId>,
    method_aliases: FxHashMap<NameId, NameId>,
    own_properties: Vec<u32>,
    own_events: Vec<NameId>,
    own_methods: Vec<NameId>,
    own_callbacks: Vec<NameId>,
}

impl ClassInfo {
    /// `Class.Ancestors[Name]` (the class itself included).
    #[inline]
    pub fn is_a(&self, class: ClassId) -> bool {
        let word = (class / 64) as usize;
        word < self.ancestors.len() && self.ancestors[word] & (1u64 << (class % 64)) != 0
    }
}

#[derive(Debug)]
pub struct EnumInfo {
    pub id: EnumId,
    pub name: Box<str>,
    /// Enumerated items (GetEnumItems order; aliases excluded)
    pub items: Vec<EnumItemId>,
    pub by_name: FxHashMap<Box<str>, EnumItemId>,
    pub by_value: FxHashMap<i64, EnumItemId>,
}

#[derive(Debug)]
pub struct EnumItemInfo {
    pub name: Box<str>,
    pub value: f64,
    pub enum_type: EnumId,
}

#[derive(Debug, Default)]
pub struct Reflection {
    pub version: String,
    pub classes: Vec<ClassInfo>,
    pub properties: Vec<PropertyInfo>,
    class_ids: FxHashMap<Box<str>, ClassId>,
    names: Vec<Box<str>>,
    name_ids: FxHashMap<Box<str>, NameId>,
    pub enums: Vec<EnumInfo>,
    pub enum_items: Vec<EnumItemInfo>,
    enum_ids: FxHashMap<Box<str>, EnumId>,
    /// data/font-enums.json `fromEnum`: Enum.Font name -> (family, weight, style)
    pub font_enum_faces: FxHashMap<Box<str>, (Box<str>, Box<str>, Box<str>)>,
    /// The dump's deprecated event names that fire with ChildAdded and with a value object's Changed (lua's
    /// `instance.childAdded`, `IntValue.changed`): signals of their own the engine fires beside the others
    pub child_added_alias: Option<NameId>,
    pub changed_alias: Option<NameId>,
}

/// Members services.luau / query.luau give behaviours that the Studio dump does not list (Player
/// methods are not in it), interned at load so behaviours can register them.
const BEHAVIOR_ONLY_NAMES: &[&str] = &[
    "DistanceFromCharacter", "GetFriendsOnline", "GetJoinData", "GetMouse", "GetNetworkPing", "GetRankInGroup",
    "GetRoleInGroup", "HasAppearanceLoaded", "IsFriendsWith", "IsInGroup", "IsVerified", "LoadCharacter", "LoadCharacterAsync",
    "RequestStreamAroundAsync", "Kick", "Move", "AddReplicationFocus", "RemoveReplicationFocus",
];

// Content properties whose string asset id has another name: (declaring class, Content property,
// string property). Measured in Studio (2026-10-12): setting the Content property
// reads back through the string one and the reverse.
const RENAMED_CONTENT: &[(&str, &str, &str)] = &[
    ("Sound", "AudioContent", "SoundId"),
    ("ShirtGraphic", "TextureContent", "Graphic"),
    ("Sky", "SkyboxBackContent", "SkyboxBk"),
    ("Sky", "SkyboxDownContent", "SkyboxDn"),
    ("Sky", "SkyboxFrontContent", "SkyboxFt"),
    ("Sky", "SkyboxLeftContent", "SkyboxLf"),
    ("Sky", "SkyboxRightContent", "SkyboxRt"),
    // Roblox's documentation (not measured): Decal.ColorMapContent is TextureContent renamed, the colour
    // map Texture holds (a Texture's too)
    ("Decal", "ColorMapContent", "Texture"),
];

// Measured deprecated item names that resolve but are not enumerated (enums.luau ItemAliases).
const ITEM_ALIASES: &[(&str, &[(&str, &str)])] = &[
    (
        "Font",
        &[
            ("GothamSemibold", "GothamMedium"),
            ("Montserrat", "Gotham"),
            ("MontserratMedium", "GothamMedium"),
            ("MontserratBold", "GothamBold"),
            ("MontserratBlack", "GothamBlack"),
        ],
    ),
    ("RaycastFilterType", &[("Blacklist", "Exclude"), ("Whitelist", "Include")]),
    ("KeyCode", &[("Unknown", "None")]),
];

// Events of classes only rbx-dom describes (reflection.luau EventSupplement).
const EVENT_SUPPLEMENT: &[(&str, &[&str])] = &[
    ("RunService", &["Heartbeat", "RenderStepped", "PreRender", "PreAnimation", "Stepped", "PreSimulation", "PostSimulation"]),
    (
        "Player",
        &[
            "CharacterAdded",
            "CharacterRemoving",
            "CharacterAppearanceLoaded",
            "Chatted",
            "Idled",
            "OnTeleport",
        ],
    ),
    ("UserGameSettings", &["FullscreenChanged", "StudioModeChanged"]),
];

// Methods of classes only rbx-dom describes (it lists properties alone): UserGameSettings's, which
// Roblox's PlayerModule calls (behaviours in lua/services.luau)
const METHOD_SUPPLEMENT: &[(&str, &[&str])] = &[(
    "UserGameSettings",
    &[
        "GetCameraYInvert",
        "GetCameraYInvertValue",
        "GetOnboardingCompleted",
        "InFullScreen",
        "InStudioMode",
        "SetCameraYInvertVisible",
        "SetGamepadCameraSensitivityVisible",
        "SetOnboardingCompleted",
    ],
)];

// Properties the dump leaves out: (class, name, script type, engine type, scriptable, serialized). Path2D keeps its
// control points in PropertiesSerialize, a BinaryString only files see, and a Transparency that only the engine's own
// scripts reach, like its selection (path2d.rs; measured in Studio 0.747 and a playing client, Studio 0.744).
const PROPERTY_SUPPLEMENT: &[(&str, &str, &str, &str, bool, bool)] = &[
    ("Path2D", "PropertiesSerialize", "any", "BinaryString", false, true),
    ("Path2D", "Transparency", "number", "float", true, true),
    ("Path2D", "SelectedControlPoint", "number", "int", true, false),
    ("Path2D", "SelectedControlPointData", "any", "", true, false),
    ("GuiService", "CoreGuiNavigationEnabled", "boolean", "bool", true, false),
];

// Callbacks the dump leaves out (it lists none for any class): every callback Roblox's API dump lets
// a game script assign (Security None; MaximumADHD's Roblox-Client-Tracker API-Dump.json, read
// 2026-10-24). A script assigns them, the engine keeps the function (`Instance::callbacks`) and
// BindableFunction:Invoke runs it (lua/services.luau); nothing calls the others offline (no server
// invokes a remote, no purchase or chat message arrives).
const CALLBACK_SUPPLEMENT: &[(&str, &[&str])] = &[
    ("BindableFunction", &["OnInvoke"]),
    ("RemoteFunction", &["OnClientInvoke", "OnServerInvoke"]),
    ("MarketplaceService", &["ProcessReceipt"]),
    ("SocialService", &["OnCallInviteInvoked"]),
    ("TextChatService", &["OnBubbleAdded", "OnChatWindowAdded", "OnIncomingMessage"]),
    ("TextChannel", &["OnIncomingMessage", "ShouldDeliverCallback"]),
    ("AdGui", &["OnAdEvent"]),
    ("DataModel", &["OnClose"]),
];

/// The dump as written by the Studio plugin: empty tables may be `[]` or `{}` anywhere.
fn list(value: Option<&serde_json::Value>) -> &[serde_json::Value] {
    match value {
        Some(serde_json::Value::Array(items)) => items,
        _ => &[],
    }
}

fn has_key(value: Option<&serde_json::Value>, key: &str) -> bool {
    value.and_then(|v| v.as_object()).is_some_and(|map| map.contains_key(key))
}

fn text<'a>(value: Option<&'a serde_json::Value>) -> Option<&'a str> {
    value.and_then(|v| v.as_str())
}

struct DumpEnumItem {
    name: String,
    value: f64,
}

#[derive(Deserialize)]
struct FontEnums {
    #[serde(rename = "fromEnum")]
    from_enum: FxHashMap<String, (String, String, String)>,
}

fn engine_type(name: &str) -> EngineType {
    match name {
        "int" | "int32" | "Int32" => EngineType::Integer,
        "int64" | "Int64" => EngineType::Int64,
        "float" => EngineType::Float,
        "ContentId" => EngineType::ContentId,
        _ => EngineType::Other,
    }
}

/// rbx-dom datatype names as Lune reports them (`Datatype`), and reflection.luau's LuneTypeMap.
fn lune_datatype_name(data_type: &rbx_reflection::DataType) -> Option<String> {
    use rbx_dom_weak::types::VariantType as T;
    Some(match data_type {
        rbx_reflection::DataType::Enum(name) => format!("Enum.{name}"),
        rbx_reflection::DataType::Value(value) => match value {
            T::Attributes => "Attributes",
            T::Axes => "Axes",
            T::BinaryString => "BinaryString",
            T::Bool => "Bool",
            T::BrickColor => "BrickColor",
            T::CFrame => "CFrame",
            T::Color3 => "Color3",
            T::Color3uint8 => "Color3uint8",
            T::ColorSequence => "ColorSequence",
            T::Content => "Content",
            T::ContentId => "ContentId",
            T::Enum => "Enum",
            T::EnumItem => "EnumItem",
            T::Faces => "Faces",
            T::Float32 => "Float32",
            T::Float64 => "Float64",
            T::Font => "Font",
            T::Int32 => "Int32",
            T::Int64 => "Int64",
            T::MaterialColors => "MaterialColors",
            T::NumberRange => "NumberRange",
            T::NumberSequence => "NumberSequence",
            T::PhysicalProperties => "PhysicalProperties",
            T::Ray => "Ray",
            T::Rect => "Rect",
            T::Ref => "Ref",
            T::Region3 => "Region3",
            T::Region3int16 => "Region3int16",
            T::SharedString => "SharedString",
            T::String => "String",
            T::Tags => "Tags",
            T::UDim => "UDim",
            T::UDim2 => "UDim2",
            T::UniqueId => "UniqueId",
            T::Vector2 => "Vector2",
            T::Vector2int16 => "Vector2int16",
            T::Vector3 => "Vector3",
            T::Vector3int16 => "Vector3int16",
            T::OptionalCFrame => "OptionalCFrame",
            T::SecurityCapabilities => "SecurityCapabilities",
            T::NetAssetRef => "NetAssetRef",
            _ => return None,
        }
        .to_string(),
        _ => return None,
    })
}

fn lune_script_type(datatype: &str) -> &str {
    match datatype {
        "Bool" => "boolean",
        "String" | "Content" | "ContentId" | "BinaryString" | "SharedString" => "string",
        "Float32" | "Float64" | "Int32" | "Int64" => "number",
        "Ref" => "Instance",
        "Enum" => "EnumItem",
        other => other,
    }
}

/// Properties the engine stores that the dump gives no class and no script reaches: a MeshPart's
/// hidden InitialSize, which an EditableMesh's parts and ApplyMesh set (crate::meshes) and ror
/// scales a mesh by (the recorder tracks it like a class property).
pub const ENGINE_ONLY: &[&str] = &["InitialSize"];

impl Reflection {
    /// The compiled-in database (crate::data), loaded once per process.
    pub fn shared() -> Result<Arc<Reflection>, String> {
        static SHARED: OnceLock<Result<Arc<Reflection>, String>> = OnceLock::new();
        SHARED.get_or_init(|| Self::load().map(Arc::new)).clone()
    }

    /// The compiled-in database: Studio's dump and the Enum.Font faces of `data/`.
    pub fn load() -> Result<Reflection, String> {
        let mut reflection = Self::from_dump(crate::data::REFLECTION, Some(crate::data::FONT_ENUMS))?;
        // Names the engine stores that no script reaches (interned last, so every other id stays)
        for &name in ENGINE_ONLY {
            if reflection.name_id(name).is_none() {
                reflection.intern(name);
            }
        }
        Ok(reflection)
    }

    /// A database from a ReflectionService dump (the JSON tools/dump-reflection.luau writes) and an
    /// optional `font-enums.json`.
    pub fn from_dump(bytes: &[u8], font_enums: Option<&[u8]>) -> Result<Reflection, String> {
        let dump: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parsing reflection.json: {error}"))?;
        let mut reflection = Reflection::default();
        for name in KNOWN_NAMES {
            reflection.intern(name);
        }
        // (services port) Behaviour members the dump does not list: the Luau runtime finds
        // behaviours by string, so they need a name id to be registered and found here
        for name in BEHAVIOR_ONLY_NAMES {
            reflection.intern(name);
        }
        reflection.version = text(dump.get("version")).unwrap_or_default().to_string();

        // Enums (enums.luau Define + LoadFromReflection)
        for entry in list(dump.get("enums")) {
            let items: Vec<DumpEnumItem> = list(entry.get("items"))
                .iter()
                .filter_map(|item| {
                    Some(DumpEnumItem { name: text(item.get("name"))?.to_string(), value: item.get("value")?.as_f64()? })
                })
                .collect();
            if let Some(name) = text(entry.get("name")) {
                reflection.define_enum(name, &items);
            }
        }
        for (enum_name, aliases) in ITEM_ALIASES {
            if let Some(&id) = reflection.enum_ids.get(*enum_name) {
                let info = &mut reflection.enums[id as usize];
                for (alias, target) in *aliases {
                    if !info.by_name.contains_key(*alias) {
                        if let Some(&item) = info.by_name.get(*target) {
                            info.by_name.insert((*alias).into(), item);
                        }
                    }
                }
            }
        }

        // Studio dump classes
        let mut superclass_names: Vec<Option<String>> = Vec::new();
        for entry in list(dump.get("classes")) {
            let Some(class_name) = text(entry.get("name")) else { continue };
            let id = reflection.new_class(class_name);
            superclass_names.push(text(entry.get("superclass")).filter(|name| !name.is_empty()).map(str::to_string));
            let permits = entry.get("permits");
            let mut own_properties = Vec::new();
            for property in list(entry.get("properties")) {
                let Some(name) = text(property.get("name")) else { continue };
                let kind = property.get("type");
                let script_name = text(kind.and_then(|k| k.get("script"))).unwrap_or("any").to_string();
                let engine_name = text(kind.and_then(|k| k.get("engine"))).unwrap_or("").to_string();
                let enum_name = text(kind.and_then(|k| k.get("enum"))).map(str::to_string);
                let read_only = !has_key(property.get("permits"), "Write");
                let scriptable = has_key(property.get("permits"), "Read");
                let serialized = property.get("serialized").and_then(|v| v.as_bool()) == Some(true);
                let index = reflection.add_property(
                    name,
                    &script_name,
                    &engine_name,
                    enum_name.as_deref(),
                    read_only,
                    scriptable,
                    serialized,
                    id,
                );
                own_properties.push(index);
            }
            let names = |reflection: &mut Reflection, key: &str| -> Vec<NameId> {
                list(entry.get(key)).iter().filter_map(|e| text(e.get("name"))).map(|n| reflection.intern(n)).collect()
            };
            let own_events = names(&mut reflection, "events");
            let own_methods = names(&mut reflection, "methods");
            let own_callbacks = names(&mut reflection, "callbacks");
            let raw_defaults = match entry.get("defaults") {
                Some(serde_json::Value::Object(defaults)) => Some(
                    defaults
                        .iter()
                        .map(|(name, value)| (reflection.intern(name), RawDefault::Json(value.clone())))
                        .collect(),
                ),
                Some(serde_json::Value::Array(_)) => Some(FxHashMap::default()),
                _ => None,
            };
            let display: Box<[(Box<str>, Box<str>)]> = match entry.get("display") {
                Some(serde_json::Value::Object(display)) => {
                    display.iter().filter_map(|(key, value)| Some((key.as_str().into(), value.as_str()?.into()))).collect()
                }
                _ => Box::new([]),
            };
            let class = &mut reflection.classes[id as usize];
            class.reflected = true;
            class.serialized = entry.get("serialized").and_then(|value| value.as_bool()) == Some(true);
            class.display = display;
            class.creatable = has_key(permits, "New");
            class.service = has_key(permits, "GetService");
            class.own_properties = own_properties;
            class.own_events = own_events;
            class.own_methods = own_methods;
            class.own_callbacks = own_callbacks;
            class.raw_defaults = raw_defaults;
        }

        // rbx-dom supplement (LoadLuneSupplement)
        reflection.load_lune_supplement(&mut superclass_names);

        for (class, name, script_type, engine_type, scriptable, serialized) in PROPERTY_SUPPLEMENT {
            if let Some(&id) = reflection.class_ids.get(*class) {
                let known = reflection.classes[id as usize].own_properties.iter().any(|&index| reflection.names[reflection.properties[index as usize].name as usize].as_ref() == *name);
                if !known {
                    let index = reflection.add_property(name, script_type, engine_type, None, false, *scriptable, *serialized, id);
                    reflection.classes[id as usize].own_properties.push(index);
                }
            }
        }
        for (class, events) in EVENT_SUPPLEMENT {
            if let Some(&id) = reflection.class_ids.get(*class) {
                for event in *events {
                    let name = reflection.intern(event);
                    let own = &mut reflection.classes[id as usize].own_events;
                    if !own.contains(&name) {
                        own.push(name);
                    }
                }
            }
        }
        for (class, methods) in METHOD_SUPPLEMENT {
            if let Some(&id) = reflection.class_ids.get(*class) {
                for method in *methods {
                    let name = reflection.intern(method);
                    let own = &mut reflection.classes[id as usize].own_methods;
                    if !own.contains(&name) {
                        own.push(name);
                    }
                }
            }
        }

        for (class, callbacks) in CALLBACK_SUPPLEMENT {
            if let Some(&id) = reflection.class_ids.get(*class) {
                for callback in *callbacks {
                    let name = reflection.intern(callback);
                    let own = &mut reflection.classes[id as usize].own_callbacks;
                    if !own.contains(&name) {
                        own.push(name);
                    }
                }
            }
        }


        for (index, superclass) in superclass_names.iter().enumerate() {
            reflection.classes[index].superclass =
                superclass.as_ref().and_then(|name| reflection.class_ids.get(name.as_str()).copied());
        }
        let count = reflection.classes.len();
        let mut resolved = vec![false; count];
        for id in 0..count {
            reflection.resolve(id as ClassId, &mut resolved);
        }
        // ReflectedClass.Subclasses names every class whose superclass it is, the ones a script cannot get included (measured in
        // a playing client, Studio 0.744: BasePlayerGui's are CoreGui, PlayerGui and StarterGui, and GetClass("CoreGui") is nil)
        let mut subclasses: Vec<Vec<ClassId>> = vec![Vec::new(); count];
        for class in &reflection.classes {
            if let Some(superclass) = class.superclass {
                subclasses[superclass as usize].push(class.id);
            }
        }
        for (id, mut list) in subclasses.into_iter().enumerate() {
            list.sort_unstable_by(|&a, &b| reflection.classes[a as usize].name.cmp(&reflection.classes[b as usize].name));
            reflection.classes[id].subclasses = list.into_boxed_slice();
        }
        reflection.link_aliases();

        if let Some(bytes) = font_enums {
            if let Ok(fonts) = serde_json::from_slice::<FontEnums>(bytes) {
                reflection.font_enum_faces = fonts
                    .from_enum
                    .into_iter()
                    .map(|(name, (family, weight, style))| {
                        (name.into(), (family.into(), weight.into(), style.into()))
                    })
                    .collect();
            }
        }
        Ok(reflection)
    }

    fn define_enum(&mut self, name: &str, items: &[DumpEnumItem]) {
        let id = match self.enum_ids.get(name) {
            Some(&id) => id,
            None => {
                let id = self.enums.len() as EnumId;
                self.enums.push(EnumInfo {
                    id,
                    name: name.into(),
                    items: Vec::new(),
                    by_name: FxHashMap::default(),
                    by_value: FxHashMap::default(),
                });
                self.enum_ids.insert(name.into(), id);
                id
            }
        };
        for item in items {
            if self.enums[id as usize].by_name.contains_key(item.name.as_str()) {
                continue;
            }
            let item_id = self.enum_items.len() as EnumItemId;
            self.enum_items.push(EnumItemInfo {
                name: item.name.as_str().into(),
                value: item.value,
                enum_type: id,
            });
            let info = &mut self.enums[id as usize];
            info.items.push(item_id);
            info.by_name.insert(item.name.as_str().into(), item_id);
            if item.value.fract() == 0.0 {
                info.by_value.entry(item.value as i64).or_insert(item_id);
            }
        }
    }

    fn new_class(&mut self, name: &str) -> ClassId {
        let id = self.classes.len() as ClassId;
        self.classes.push(ClassInfo {
            id,
            name: name.into(),
            superclass: None,
            creatable: false,
            service: false,
            ancestors: Box::new([]),
            properties: FxHashMap::default(),
            events: FxHashSet::default(),
            methods: FxHashSet::default(),
            callbacks: FxHashSet::default(),
            raw_defaults: None,
            reflected: false,
            serialized: false,
            display: Box::new([]),
            subclasses: Box::new([]),
            property_aliases: FxHashMap::default(),
            method_aliases: FxHashMap::default(),
            own_properties: Vec::new(),
            own_events: Vec::new(),
            own_methods: Vec::new(),
            own_callbacks: Vec::new(),
        });
        self.class_ids.insert(name.into(), id);
        id
    }

    /// Links the dump's own deprecated lower-case members to the members they stand for: a lower-case-first property a
    /// script can read whose name with the first letter upper-cased is a property of the class too (brickColor, maxHealth,
    /// archivable, workspace), and such a method (getService, getChildren, takeDamage), plus the two Players methods
    /// named otherwise. A client treats each as the member itself: an alias property reads and writes the other's value
    /// and its Changed names the other; an alias method does the other's work through a function of its own (measured in
    /// a playing client, tools/parity/probes/aliases.luau). Events keep signals of their own (lua/instance.rs fires them).
    fn link_aliases(&mut self) {
        const NAMED_OTHERWISE: &[(&str, &str, &str)] =
            &[("Players", "players", "GetPlayers"), ("Players", "playerFromCharacter", "GetPlayerFromCharacter")];
        let upper = |names: &Vec<Box<str>>, ids: &FxHashMap<Box<str>, NameId>, name: NameId| -> Option<NameId> {
            let text = &names[name as usize];
            let first = text.chars().next()?;
            if !first.is_ascii_lowercase() {
                return None;
            }
            ids.get(format!("{}{}", first.to_ascii_uppercase(), &text[1..]).as_str()).copied()
        };
        for id in 0..self.classes.len() {
            let class = &self.classes[id];
            let mut properties = FxHashMap::default();
            for (&name, &index) in &class.properties {
                if let Some(twin) = upper(&self.names, &self.name_ids, name)
                    && self.properties[index as usize].scriptable
                    && class.properties.get(&twin).is_some_and(|&other| self.properties[other as usize].scriptable)
                {
                    properties.insert(name, twin);
                }
            }
            let mut methods = FxHashMap::default();
            for &name in &class.methods {
                if let Some(twin) = upper(&self.names, &self.name_ids, name).filter(|twin| class.methods.contains(twin)) {
                    methods.insert(name, twin);
                }
            }
            for &(class_name, alias, twin) in NAMED_OTHERWISE {
                if let (Some(owner), Some(alias), Some(twin)) = (self.class_ids.get(class_name), self.name_ids.get(alias), self.name_ids.get(twin))
                    && class.is_a(*owner)
                    && class.methods.contains(alias)
                {
                    methods.insert(*alias, *twin);
                }
            }
            let class = &mut self.classes[id];
            class.property_aliases = properties;
            class.method_aliases = methods;
        }
        self.child_added_alias = self.name_ids.get("childAdded").copied();
        self.changed_alias = self.name_ids.get("changed").copied();
    }

    /// The property a deprecated lower-case property of `class` is ([`Self::link_aliases`]): `brickColor` -> BrickColor.
    #[inline]
    pub fn property_alias(&self, class: ClassId, name: NameId) -> Option<NameId> {
        let aliases = &self.classes[class as usize].property_aliases;
        if aliases.is_empty() { None } else { aliases.get(&name).copied() }
    }

    /// The method whose work a deprecated lower-case method of `class` does ([`Self::link_aliases`]): `getService` -> GetService.
    #[inline]
    pub fn method_alias(&self, class: ClassId, name: NameId) -> Option<NameId> {
        let aliases = &self.classes[class as usize].method_aliases;
        if aliases.is_empty() { None } else { aliases.get(&name).copied() }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_property(
        &mut self,
        name: &str,
        script_type: &str,
        engine_type_name: &str,
        enum_name: Option<&str>,
        read_only: bool,
        scriptable: bool,
        serialized: bool,
        owner: ClassId,
    ) -> u32 {
        let name = self.intern(name);
        let index = self.properties.len() as u32;
        self.properties.push(PropertyInfo {
            name,
            script_type: ScriptType::parse(script_type),
            script_type_name: script_type.into(),
            engine_type: engine_type(engine_type_name),
            engine_type_name: engine_type_name.into(),
            enum_name: enum_name.map(Into::into),
            enum_type: enum_name.and_then(|name| self.enum_ids.get(name).copied()),
            read_only,
            scriptable,
            serialized,
            owner,
        });
        index
    }

    fn load_lune_supplement(&mut self, superclass_names: &mut Vec<Option<String>>) {
        let Ok(database) = rbx_reflection_database::get() else { return };
        let mut names: Vec<&str> = database.classes.keys().map(|name| name.as_ref()).collect();
        // rbx-dom's map order is arbitrary; sort so class ids are stable across processes
        names.sort_unstable();
        for name in names {
            let descriptor = &database.classes[name];
            let (id, is_new) = match self.class_ids.get(name) {
                Some(&id) => (id, false),
                None => {
                    let id = self.new_class(name);
                    superclass_names.push(descriptor.superclass.as_ref().map(|s| s.to_string()));
                    let tags = &descriptor.tags;
                    let service = tags.contains(&rbx_reflection::ClassTag::Service);
                    let class = &mut self.classes[id as usize];
                    class.service = service;
                    class.creatable =
                        !tags.contains(&rbx_reflection::ClassTag::NotCreatable) && !service;
                    (id, true)
                }
            };

            if is_new {
                let mut properties: Vec<_> = descriptor.properties.iter().collect();
                properties.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (property_name, property) in properties {
                    let Some(datatype) = lune_datatype_name(&property.data_type) else {
                        continue;
                    };
                    let read_only = property.tags.contains(&rbx_reflection::PropertyTag::ReadOnly);
                    let scriptable = !matches!(property.scriptability, rbx_reflection::Scriptability::None);
                    // an enum property keeps its enum (its items coerce, and its zero default is
                    // the enum's first item, as the dump's classes have)
                    let enum_name = match &property.data_type {
                        rbx_reflection::DataType::Enum(name) => Some(name.as_ref()),
                        _ => None,
                    };
                    let script_type = if enum_name.is_some() { "EnumItem" } else { lune_script_type(&datatype) };
                    let index = self.add_property(
                        property_name,
                        script_type,
                        &datatype,
                        enum_name,
                        read_only,
                        scriptable,
                        true,
                        id,
                    );
                    self.classes[id as usize].own_properties.push(index);
                }
            }

            if is_new || self.classes[id as usize].raw_defaults.is_none() {
                let mut defaults = FxHashMap::default();
                for (property_name, value) in &descriptor.default_properties {
                    let raw = match value {
                        Variant::Enum(item) => {
                            // Lune: the enum named by the class's own property descriptor
                            let enum_name = descriptor
                                .properties
                                .get(property_name.as_ref())
                                .and_then(|property| match &property.data_type {
                                    rbx_reflection::DataType::Enum(name) => Some(name.as_ref()),
                                    _ => None,
                                });
                            let Some(item) = enum_name.and_then(|enum_name| {
                                let descriptor = database.enums.get(enum_name)?;
                                let (item_name, _) = descriptor
                                    .items
                                    .iter()
                                    .find(|(_, value)| **value == item.to_u32())?;
                                self.enum_item(enum_name, item_name)
                            }) else {
                                continue;
                            };
                            RawDefault::EnumItem(item)
                        }
                        other => RawDefault::Dom(other.clone()),
                    };
                    defaults.insert(self.intern(property_name), raw);
                }
                self.classes[id as usize].raw_defaults = Some(defaults);
            }
        }
    }

    fn resolve(&mut self, id: ClassId, resolved: &mut Vec<bool>) {
        if resolved[id as usize] {
            return;
        }
        resolved[id as usize] = true;
        let words = self.classes.len().div_ceil(64);
        let mut ancestors = vec![0u64; words];
        let mut properties = FxHashMap::default();
        let mut events = FxHashSet::default();
        let mut methods = FxHashSet::default();
        let mut callbacks = FxHashSet::default();
        if let Some(superclass) = self.classes[id as usize].superclass {
            self.resolve(superclass, resolved);
            let parent = &self.classes[superclass as usize];
            ancestors.copy_from_slice(&parent.ancestors);
            properties.clone_from(&parent.properties);
            events.clone_from(&parent.events);
            methods.clone_from(&parent.methods);
            callbacks.clone_from(&parent.callbacks);
        }
        ancestors[(id / 64) as usize] |= 1u64 << (id % 64);
        let class = &mut self.classes[id as usize];
        for &index in &class.own_properties {
            properties.insert(self.properties[index as usize].name, index);
        }
        events.extend(class.own_events.iter().copied());
        methods.extend(class.own_methods.iter().copied());
        callbacks.extend(class.own_callbacks.iter().copied());
        class.ancestors = ancestors.into_boxed_slice();
        class.properties = properties;
        class.events = events;
        class.methods = methods;
        class.callbacks = callbacks;
    }

    /// Interns a member name (only during load; lookups use [`Self::name_id`]).
    fn intern(&mut self, name: &str) -> NameId {
        if let Some(&id) = self.name_ids.get(name) {
            return id;
        }
        let id = self.names.len() as NameId;
        self.names.push(name.into());
        self.name_ids.insert(name.into(), id);
        id
    }

    /// The id of a member name, if any class knows it.
    #[inline]
    pub fn name_id(&self, name: &str) -> Option<NameId> {
        self.name_ids.get(name).copied()
    }

    #[inline]
    pub fn name_id_bytes(&self, name: &[u8]) -> Option<NameId> {
        std::str::from_utf8(name).ok().and_then(|name| self.name_id(name))
    }

    /// The id of the member name `key` spells with its first letter upper-cased, when that letter is a lower-case ASCII
    /// one (`getPropertyChangedSignal` -> GetPropertyChangedSignal): the member an instance's read or method call
    /// reaches by a key that names no member of the class itself (lua/instance.rs `get`, docs/engine.md "Member names").
    pub fn upper_first_id(&self, key: &[u8]) -> Option<NameId> {
        let (&first, rest) = key.split_first()?;
        if !first.is_ascii_lowercase() {
            return None;
        }
        let mut buffer = [0u8; 64];
        if key.len() <= buffer.len() {
            buffer[0] = first.to_ascii_uppercase();
            buffer[1..key.len()].copy_from_slice(rest);
            self.name_id_bytes(&buffer[..key.len()])
        } else {
            let mut upper = key.to_vec();
            upper[0] = first.to_ascii_uppercase();
            self.name_id_bytes(&upper)
        }
    }

    #[inline]
    pub fn name(&self, id: NameId) -> &str {
        &self.names[id as usize]
    }

    #[inline]
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.class_ids.get(name).copied()
    }

    #[inline]
    pub fn class(&self, id: ClassId) -> &ClassInfo {
        &self.classes[id as usize]
    }

    pub fn class_by_name(&self, name: &str) -> Option<&ClassInfo> {
        self.class_id(name).map(|id| self.class(id))
    }

    /// `Class.Ancestors[Name]` by name (false for unknown class names).
    pub fn is_a(&self, class: ClassId, ancestor: &str) -> bool {
        self.class_id(ancestor).is_some_and(|id| self.class(class).is_a(id))
    }

    #[inline]
    pub fn property(&self, index: u32) -> &PropertyInfo {
        &self.properties[index as usize]
    }

    /// The class that declares member `name` of `class`: a property's owner, else the nearest ancestor with the method, event or
    /// callback (`class` itself when it has none), so `Part.CFrame` and `MeshPart.CFrame` are both `BasePart.CFrame`.
    pub fn declaring_class(&self, class: ClassId, name: NameId) -> ClassId {
        if let Some(info) = self.class_property(class, name) {
            return info.owner;
        }
        let mut current = Some(class);
        while let Some(id) = current {
            let info = &self.classes[id as usize];
            if info.own_methods.contains(&name) || info.own_events.contains(&name) || info.own_callbacks.contains(&name) {
                return id;
            }
            current = info.superclass;
        }
        class
    }

    /// The property `name` of `class` (inherited ones included).
    #[inline]
    pub fn class_property(&self, class: ClassId, name: NameId) -> Option<&PropertyInfo> {
        self.classes[class as usize]
            .properties
            .get(&name)
            .map(|&index| &self.properties[index as usize])
    }

    /// The string asset id property a Content property is a view of (Studio keeps the two as one
    /// value, each settable and readable through the other): a measured rename (RENAMED_CONTENT),
    /// else `<base>`, `<base>Id` or `<base>ID` of the same class (Decal.TextureContent: Texture,
    /// Animation.AnimationContent: AnimationId, MeshPart.TextureContent: TextureID).
    pub fn content_sibling(&self, class: ClassId, content: NameId) -> Option<NameId> {
        let info = self.class_property(class, content)?;
        if info.script_type != ScriptType::Content {
            return None;
        }
        let name = self.name(content);
        let owner = &*self.class(info.owner).name;
        let string = |candidate: &str| self.name_id(candidate).filter(|&id| self.class_property(class, id).is_some_and(|p| p.script_type == ScriptType::String));
        if let Some(&(_, _, renamed)) = RENAMED_CONTENT.iter().find(|&&(c, n, _)| c == owner && n == name) {
            return string(renamed);
        }
        let base = name.strip_suffix("Content").filter(|base| !base.is_empty())?;
        string(base).or_else(|| string(&format!("{base}Id"))).or_else(|| string(&format!("{base}ID")))
    }

    /// Every (string asset id, Content view) pair of `class`, inherited ones included.
    pub fn content_siblings(&self, class: ClassId) -> Vec<(NameId, NameId)> {
        let mut pairs: Vec<(NameId, NameId)> = self.classes[class as usize]
            .properties
            .iter()
            .filter(|&(_, &index)| self.properties[index as usize].script_type == ScriptType::Content)
            .filter_map(|(&content, _)| self.content_sibling(class, content).map(|string| (string, content)))
            .collect();
        pairs.sort();
        pairs
    }

    pub fn enum_id(&self, name: &str) -> Option<EnumId> {
        self.enum_ids.get(name).copied()
    }

    /// `Enums.Get(EnumName, ItemName)`
    pub fn enum_item(&self, enum_name: &str, item_name: &str) -> Option<EnumItemId> {
        let id = self.enum_id(enum_name)?;
        self.enums[id as usize].by_name.get(item_name).copied()
    }

    pub fn enum_item_info(&self, item: EnumItemId) -> &EnumItemInfo {
        &self.enum_items[item as usize]
    }

    /// `Enum.<Type>.<Item>` of an item.
    pub fn enum_item_path(&self, item: EnumItemId) -> String {
        let info = &self.enum_items[item as usize];
        format!("Enum.{}.{}", self.enums[info.enum_type as usize].name, info.name)
    }

    pub fn enum_item_type_name(&self, item: EnumItemId) -> &str {
        &self.enums[self.enum_items[item as usize].enum_type as usize].name
    }

    /// Number of classes (ids are `0..count`).
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_dump_and_supplement() {
        let reflection = Reflection::shared().unwrap();
        for (index, name) in KNOWN_NAMES.iter().enumerate() {
            assert_eq!(reflection.name_id(name), Some(index as NameId));
        }
        let frame = reflection.class_by_name("Frame").unwrap();
        assert!(frame.creatable && !frame.service);
        assert!(reflection.is_a(frame.id, "GuiObject") && reflection.is_a(frame.id, "Instance"));
        assert!(!reflection.is_a(frame.id, "ScreenGui") && !reflection.is_a(frame.id, "Nope"));
        let size = reflection.class_property(frame.id, K::Size as NameId).unwrap();
        assert_eq!(size.script_type, ScriptType::UDim2);
        let transparency = reflection
            .class_property(frame.id, K::BackgroundTransparency as NameId)
            .unwrap();
        assert_eq!(transparency.engine_type, EngineType::Float);
        let absolute = reflection.class_property(frame.id, K::AbsoluteSize as NameId).unwrap();
        assert!(absolute.read_only);
        assert!(frame.events.contains(&(K::Changed as NameId)));
        assert!(frame.methods.contains(&(K::FindFirstChild as NameId)));
        // Player exists only in rbx-dom's database; its events come from the supplement
        let player = reflection.class_by_name("Player").unwrap();
        assert!(player.events.contains(&reflection.name_id("CharacterAdded").unwrap()));
        assert!(player.properties.contains_key(&(K::UserId as NameId)));
        let run_service = reflection.class_by_name("RunService").unwrap();
        for event in ["Heartbeat", "RenderStepped", "PreRender", "PreAnimation", "Stepped", "PreSimulation", "PostSimulation"] {
            assert!(run_service.events.contains(&reflection.name_id(event).unwrap()));
        }
        let user_game_settings = reflection.class_by_name("UserGameSettings").unwrap();
        assert!(user_game_settings.methods.contains(&reflection.name_id("GetCameraYInvertValue").unwrap()));
        let workspace = reflection.class_by_name("Workspace").unwrap();
        assert!(workspace.service);
        // Aliases resolve but are not enumerated
        let semibold = reflection.enum_item("Font", "GothamSemibold").unwrap();
        assert_eq!(Some(semibold), reflection.enum_item("Font", "GothamMedium"));
        let font = &reflection.enums[reflection.enum_id("Font").unwrap() as usize];
        assert!(!font.items.iter().any(|&item| &*reflection.enum_item_info(item).name == "GothamSemibold"));
        assert_eq!(reflection.enum_item_path(semibold), "Enum.Font.GothamMedium");
        assert!(reflection.font_enum_faces.contains_key("Gotham"));
    }
}

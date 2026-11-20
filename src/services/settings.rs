//! The two settings roots outside the DataModel, `settings()` (GlobalSettings) and `UserSettings()` (UserSettings, made in
//! services::user_settings), and their settings services, as Studio 0.747.0.7471481 answers them in roblox-sim
//! (tools/parity/probes/global_settings.luau, global_settings_tree.luau and global_settings_services.luau; 2026-11-20).
//!
//! - `settings()` is refused to a game script (script identity 2, a playing client's and a server's): "The current thread
//!   cannot call 'settings' (lacking capability Plugin)", whatever it is called with, without a position (lua/globals.rs). A
//!   plugin's code has the Plugin capability (the probes run at identity 4, the command bar's): there it is one GlobalSettings
//!   object, "Global Settings", the same at every call, which ree makes on the first call in edit mode (its run context for a
//!   plugin's code), with its eight settings services in a client's order and names ([`GLOBAL_SERVICES`]): a run that never
//!   calls it numbers its instances as before. GlobalSettings is one object a process: a server's identity 4 reads the same.
//! - Each settings service holds its measured values ([`VALUES`]); a live reading of the process or the machine (instance and
//!   player counts, free memory, the thread pool) is not emulated: a read reports itself (Output `missing`) and answers the
//!   sim client's reading ([`READINGS`]). Studio's own settings service (the sim runs Studio's engine) is there and none of its
//!   members is emulated: each read reports itself. RenderSettings' writes store without Changed (12 of its 15 settings
//!   measured), the others' fire it; GameSettings.VideoCaptureEnabled drops a write, as every settings service does a Name
//!   write (the roots rename); NetworkSettings' proxy settings refuse one (lacking capability RobloxScript), as reflection.rs
//!   CAPABILITY_ONLY's PhysicsSettings ones refuse a read too.
//! - GetService on a settings root ([`get_service`]) answers its own services; another root's it refuses as its child
//!   ("Invalid parent for Service. RenderSettings cannot be parented to GlobalBasicSettings ."), or answers nil where a game
//!   script lacks the service's capability ([`PLUGIN_SETTINGS`]); a service of the DataModel it refuses ([`REFUSED`]: "Not
//!   allowed to add that under settings") or answers nil, and it makes none. The DataModel's GetService answers the settings
//!   services the same way ([`data_model_service`]). The roots and their services keep their parents, neither clones, and
//!   nothing can be added under them (instance.rs set_parent and begin_destroy, lua/instance.rs Clone).

use std::rc::Rc;

use mlua::prelude::*;

use super::{Behaviors, find_service, getter, method};
use crate::{
    ClassId, Engine, InstanceId, K, NameId, Reflection,
    dom::flags,
    instance as core,
    lua::{Args, Flow, Ret, convert, instance_value, none, raise, values},
    value::{Str, Value},
};

/// settings()'s services: (class, name), in the order a client holds them (all eight from the start, Studio's among them).
pub const GLOBAL_SERVICES: &[(&str, &str)] = &[
    ("GameSettings", "Game Options"),
    ("LuaSettings", "Lua"),
    ("DebugSettings", "Diagnostics"),
    ("PhysicsSettings", "Physics"),
    ("Studio", "Studio"),
    ("NetworkSettings", "Network"),
    ("RenderSettings", "Rendering"),
    ("TaskScheduler", "Task Scheduler"),
];

/// UserSettings()' service (services::get_service names it).
const USER_SERVICES: &[&str] = &["UserGameSettings"];

/// The settings services a game script cannot get: another provider's GetService answers nil for them at identity 2, and refuses
/// them at identity 4 as it refuses the others.
const PLUGIN_SETTINGS: &[&str] = &["GameSettings", "DebugSettings", "PhysicsSettings", "RenderSettings", "Studio"];

/// The classes a settings root and the DataModel answer nil for, as for no service of theirs (GetService and FindService alike).
const ROOT_CLASSES: &[&str] = &["GlobalSettings", "UserSettings", "GenericSettings", "ServiceProvider"];

/// The services a settings root refuses as its child, "Not allowed to add that under settings", where it answers nil for the
/// other services (identity 4, both roots alike: global_settings_services.luau, each service class rbx-dom describes).
const REFUSED: &[&str] = &[
    "AdService", "AnalyticsService", "AnimationFromVideoCreatorService", "AssetDeliveryProxy", "AvatarCreationService",
    "AvatarEditorService", "BadgeService", "CaptureService", "ChangeHistoryService", "Chat", "CommerceService",
    "ConfigService", "CreatorStoreService", "DataStoreService", "DraftsService", "DraggerService", "EncodingService",
    "ExperienceNotificationService", "FeatureRestrictionManager", "FriendService", "GamePassService", "GamepadService",
    "GenerationService", "GenericChallengeService", "Geometry", "GeometryService", "GroupService", "HapticService",
    "HeapProfilerService", "HeightmapImporterService", "HttpRbxApiService", "HttpService", "IncrementalPatchBuilder",
    "InsertService", "InstanceExtensionsService", "Lighting", "LocalizationService", "LogService", "MLService",
    "MarketplaceService", "MatchmakingService", "MaterialService", "MemStorageService", "MemoryStoreService",
    "MessagingService", "MicroProfilerService", "ModerationService", "MouseService", "NetworkClient", "OpenCloudService",
    "PackageService", "PatchBundlerFileWatch", "PathfindingService", "PlacesService", "PlayerDataService",
    "PlayerViewService", "PluginManagementService", "PointsService", "PolicyService", "ProximityPromptService",
    "PublishService", "RecommendationService", "ReflectionService", "RemoteCommandService", "ReplicatedFirst",
    "ReplicatedStorage", "ScriptContext", "ScriptEditorService", "ScriptProfilerService", "SelectionHighlightManager",
    "SerializationService", "ServerScriptService", "ServerStorage", "ServiceVisibilityService", "SessionCheckService",
    "SharedTableRegistry", "SlimContentProvider", "SocialService", "SoundService", "StarterPlayer", "StartupMessageService",
    "StudioService", "StudioTestService", "StylingService", "TeamCreateData", "TeamCreateService", "Teams", "TestService",
    "TextBoxService", "TextChatService", "TimerService", "UGCValidationService", "UIDragDetectorService",
    "UniqueIdLookupService", "UserInputService", "UserService", "VRService", "VRStatusService", "VersionControlService",
    "VideoCaptureService", "VideoScreenCaptureService", "VideoService", "VirtualInputManager", "VirtualUser",
    "VisibilityCheckDispatcher", "Visit", "VisualizationModeService", "VoiceChatInternal", "VoiceChatService",
];

/// Of [`REFUSED`], the ones a game script's GetService answers nil for (UserSettings() at identity 2: global_settings.luau);
/// NetworkClient raises "lacking capability Plugin" there instead.
const PLUGIN_REFUSED: &[&str] = &[
    "DraftsService", "DraggerService", "ScriptEditorService", "SerializationService", "StudioService", "TeamCreateData",
    "TeamCreateService", "VersionControlService",
];

/// A measured value of a setting.
enum Setting {
    Bool(bool),
    Number(f64),
    Text(&'static str),
    /// (enum, item)
    Item(&'static str, &'static str),
}

use Setting::{Bool, Item, Number, Text};

/// What the settings services hold (a client's, at identity 4; a server's are the same). QualityLevel follows the user's
/// choice in a client: Automatic is what the sim's client holds, as a client does until its user picks a level.
const VALUES: &[(&str, &str, Setting)] = &[
    ("RenderSettings", "AutoFRMLevel", Number(1.0)),
    ("RenderSettings", "EagerBulkExecution", Bool(false)),
    ("RenderSettings", "EditQualityLevel", Item("QualityLevel", "Automatic")),
    ("RenderSettings", "Enable VR Mode", Bool(false)),
    ("RenderSettings", "EnableFRM", Bool(true)),
    ("RenderSettings", "ExportMergeByMaterial", Bool(false)),
    ("RenderSettings", "FrameRateManager", Item("FramerateManagerMode", "Automatic")),
    ("RenderSettings", "GraphicsMode", Item("GraphicsMode", "Automatic")),
    ("RenderSettings", "MeshCacheSize", Number(33554432.0)),
    ("RenderSettings", "MeshPartDetailLevel", Item("MeshPartDetailLevel", "DistanceBased")),
    ("RenderSettings", "QualityLevel", Item("QualityLevel", "Automatic")),
    ("RenderSettings", "ReloadAssets", Bool(false)),
    ("RenderSettings", "RenderCSGTrianglesDebug", Bool(false)),
    ("RenderSettings", "ShowBoundingBoxes", Bool(false)),
    ("RenderSettings", "ViewMode", Item("ViewMode", "None")),
    ("GameSettings", "VideoCaptureEnabled", Bool(true)),
    ("DebugSettings", "IsScriptStackTracingEnabled", Bool(true)),
    ("DebugSettings", "ReportSoundWarnings", Bool(false)),
    ("DebugSettings", "TickCountPreciseOverride", Item("TickCountSampleMethod", "Precise")),
    // (PhysicsSettings' other settings read false; reflection.rs CAPABILITY_ONLY has the ones a plugin may not read)
    ("PhysicsSettings", "AllowSleep", Bool(true)),
    ("PhysicsSettings", "ForceCSGv2", Bool(true)),
    ("PhysicsSettings", "PhysicsEnvironmentalThrottle", Item("EnviromentalPhysicsThrottle", "DefaultAuto")),
    ("PhysicsSettings", "ThrottleAdjustTime", Number(1.0)),
    ("NetworkSettings", "HttpProxyEnabled", Bool(false)),
    ("NetworkSettings", "HttpProxyURL", Text("")),
    ("NetworkSettings", "IncomingReplicationLag", Number(0.0)),
    ("NetworkSettings", "PrintJoinSizeBreakdown", Bool(false)),
    ("NetworkSettings", "PrintPhysicsErrors", Bool(false)),
    ("NetworkSettings", "PrintStreamInstanceQuota", Bool(false)),
    ("NetworkSettings", "RandomizeJoinInstanceOrder", Bool(true)),
    ("NetworkSettings", "RenderStreamedRegions", Bool(false)),
    ("NetworkSettings", "ShowActiveAnimationAsset", Bool(false)),
    ("TaskScheduler", "ThreadPoolConfig", Item("ThreadPoolConfig", "Auto")),
];

/// Live readings of the process and the machine, which ree does not emulate: a read reports itself and answers what the sim's
/// client read (its session's counts; the machine's total memory and half its logical processors).
const READINGS: &[(&str, &str, Setting)] = &[
    ("DebugSettings", "DataModel", Number(1.0)),
    ("DebugSettings", "InstanceCount", Number(1362.0)),
    ("DebugSettings", "JobCount", Number(0.0)),
    ("DebugSettings", "PlayerCount", Number(1.0)),
    ("DebugSettings", "RobloxVersion", Text("?")),
    ("NetworkSettings", "EmulatedTotalMemoryInMB", Number(63115.0)),
    ("NetworkSettings", "FreeMemoryMBytes", Number(31029.57421875)),
    ("TaskScheduler", "SchedulerDutyCycle", Number(0.0)),
    ("TaskScheduler", "SchedulerRate", Number(0.0)),
    ("TaskScheduler", "ThreadPoolSize", Number(16.0)),
];

/// The highest quality level RenderSettings:GetMaxQualityLevel answers (the sim's client).
const MAX_QUALITY_LEVEL: f64 = 22.0;

impl Setting {
    fn value(&self, reflection: &Reflection) -> Option<Value> {
        Some(match *self {
            Bool(value) => Value::Bool(value),
            Number(value) => Value::Number(value),
            Text(text) => Value::String(Str::from(text)),
            Item(enum_name, item) => Value::EnumItem(reflection.enum_item(enum_name, item)?),
        })
    }
}

/// A settings root (GlobalSettings, UserSettings): a GenericSettings, the class of both.
pub fn is_root(reflection: &Reflection, class: ClassId) -> bool {
    reflection.class_id("GenericSettings").is_some_and(|generic| reflection.class(class).is_a(generic))
}

/// The name a settings root's Parent write names: "The current thread cannot set 'settings parent' (lacking capability
/// RobloxScript)", 'UserSettings parent' for UserSettings().
pub fn parent_label(reflection: &Reflection, class: ClassId) -> &str {
    match &*reflection.class(class).name {
        "GlobalSettings" => "settings",
        other => other,
    }
}

/// The GlobalSettings object with its settings services (settings()'s first call in edit mode; lua/globals.rs keeps it). None when
/// the reflection has no GlobalSettings class.
pub fn global_settings(engine: &Engine) -> Option<InstanceId> {
    let reflection = &engine.reflection;
    let class = reflection.class_id("GlobalSettings")?;
    let mut state = engine.state.borrow_mut();
    let root = core::create_named(&mut state, reflection, class, "Global Settings");
    state.dom.at_mut(root).set_flag(flags::PARENT_LOCKED, true);
    state.dom.pin(root);
    for &(class_name, name) in GLOBAL_SERVICES {
        add_service(&mut state, reflection, root, class_name, name);
    }
    Some(root)
}

/// One of settings()'s services, with its values, as the root's child (parent-locked, made without events as a service is).
fn add_service(state: &mut crate::State, reflection: &Reflection, root: InstanceId, class_name: &str, name: &str) -> Option<InstanceId> {
    let class = reflection.class_id(class_name)?;
    let service = core::create_named(state, reflection, class, name);
    for (owner, property, setting) in VALUES.iter().chain(READINGS) {
        if *owner == class_name
            && let (Some(property), Some(value)) = (reflection.name_id(property), setting.value(reflection))
        {
            core::raw_set_silent(state, reflection, service, property, value);
        }
    }
    state.dom.attach(service, root);
    state.dom.at_mut(service).set_flag(flags::PARENT_LOCKED, true);
    Some(service)
}

fn service_name(args: &Args) -> String {
    args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default()
}

/// A settings root's GetService.
fn get_service(lua: &Lua, engine: &Rc<Engine>, root: InstanceId, name: &str) -> Ret {
    let reflection = &engine.reflection;
    if reflection.class_id(name).is_none() {
        return raise(format!("'{name}' is not a valid Service name"));
    }
    if let Some(found) = find_service(engine, root, name) {
        return Ok(values([instance_value(lua, engine, found)?]));
    }
    let (root_class, root_name) = {
        let state = engine.state.borrow();
        let instance = state.dom.at(root);
        (instance.class, instance.name.to_str_lossy().into_owned())
    };
    // (both roots make their services with themselves; one of its own that is not there is made again)
    let made = match &*reflection.class(root_class).name {
        "GlobalSettings" => match GLOBAL_SERVICES.iter().find(|&&(service, _)| service == name) {
            Some(&(class_name, child)) => add_service(&mut engine.state.borrow_mut(), reflection, root, class_name, child),
            None => None,
        },
        _ if USER_SERVICES.contains(&name) => Some(super::get_service(engine, root, name)?),
        _ => None,
    };
    if let Some(service) = made {
        return Ok(values([instance_value(lua, engine, service)?]));
    }
    let plugin = engine.options.edit_mode();
    if GLOBAL_SERVICES.iter().any(|&(service, _)| service == name) || USER_SERVICES.contains(&name) {
        if !plugin && PLUGIN_SETTINGS.contains(&name) {
            return Ok(none());
        }
        return raise(format!("Invalid parent for Service. {name} cannot be parented to {root_name} ."));
    }
    match name {
        "NetworkServer" => raise("The current thread cannot create 'NetworkServer' (lacking capability RobloxEngine)"),
        "NetworkClient" if !plugin => raise("The current thread cannot create 'NetworkClient' (lacking capability Plugin)"),
        _ if REFUSED.contains(&name) && (plugin || !PLUGIN_REFUSED.contains(&name)) => raise("Not allowed to add that under settings"),
        // (another class, a service or not, is nil and nothing is made)
        _ => Ok(none()),
    }
}

/// The DataModel's GetService of a settings service or a settings root's class: Some(Ok) answers nil, Some(Err) refuses; None for
/// any other name. Measured with the place's name in the text ("Invalid parent for Service. LuaSettings cannot be parented to
/// <place> ."): at identity 2 the plugin's settings services are nil, at identity 4 every one is refused.
pub fn data_model_service(engine: &Engine, game: InstanceId, name: &str) -> Option<Result<(), Flow>> {
    if ROOT_CLASSES.contains(&name) {
        return Some(Ok(()));
    }
    if !GLOBAL_SERVICES.iter().any(|&(service, _)| service == name) && !USER_SERVICES.contains(&name) {
        return None;
    }
    if !engine.options.edit_mode() && PLUGIN_SETTINGS.contains(&name) {
        return Some(Ok(()));
    }
    let place = engine.state.borrow().dom.at(game).name.to_str_lossy().into_owned();
    Some(raise(format!("Invalid parent for Service. {name} cannot be parented to {place} .")))
}

/// Whether the DataModel's FindService answers nil for a name that is no service's class: a settings root's class.
pub fn found_nowhere(name: &str) -> bool {
    ROOT_CLASSES.contains(&name)
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let class = |name: &str| reflection.class_id(name);
    let n = |name: &str| reflection.name_id(name);

    if let Some(generic) = class("GenericSettings") {
        behaviors.add_method(generic, K::GetService as NameId, method(|lua, engine, id, args| get_service(lua, engine, id, &service_name(args))));
        behaviors.add_method(generic, K::FindService as NameId, method(|lua, engine, id, args| {
            let name = service_name(args);
            if engine.reflection.class_id(&name).is_none() {
                return raise(format!("'{name}' is not a valid Service name"));
            }
            Ok(values([match find_service(engine, id, &name) {
                Some(service) => instance_value(lua, engine, service)?,
                None => LuaValue::Nil,
            }]))
        }));
    }

    // a settings service keeps its name: a write reads back the old one, with no error (each of settings()' eight and
    // UserSettings()' GameSettings; the roots rename)
    for class_name in GLOBAL_SERVICES.iter().map(|&(class_name, _)| class_name).chain(USER_SERVICES.iter().copied()) {
        let Some(owner) = class(class_name) else { continue };
        behaviors.add_setter(owner, K::Name as NameId, Rc::new(move |lua, engine, id, value| {
            let class = engine.state.borrow().dom.at(id).class;
            if let Some(info) = engine.reflection.class_property(class, K::Name as NameId).cloned() {
                convert::coerce(lua, engine, &info, &value)?;
            }
            Ok(())
        }));
    }
    // RenderSettings' writes store the value and fire no Changed (Changed and GetPropertyChangedSignal, 12 of its 15 settings
    // measured: all but GraphicsMode, Enable VR Mode and ReloadAssets, which a probe should not switch)
    if let Some(render) = class("RenderSettings") {
        let properties: Vec<NameId> = reflection.class(render).properties.iter().filter(|&(_, &index)| reflection.property(index).owner == render).map(|(&name, _)| name).collect();
        for name in properties {
            behaviors.add_setter(render, name, Rc::new(move |lua, engine, id, value| {
                let class = engine.state.borrow().dom.at(id).class;
                let Some(info) = engine.reflection.class_property(class, name).cloned() else { return Ok(()) };
                let value = convert::coerce(lua, engine, &info, &value)?;
                core::raw_set_silent(&mut engine.state.borrow_mut(), &engine.reflection, id, name, value);
                Ok(())
            }));
        }
        if let Some(name) = n("GetMaxQualityLevel") {
            behaviors.add_method(render, name, method(|_, _, _, _| Ok(values([LuaValue::Number(MAX_QUALITY_LEVEL)]))));
        }
    }
    // GameSettings.VideoCaptureEnabled (deprecated) keeps its value: a write of false reads true, with no error and no Changed
    if let (Some(game_settings), Some(name)) = (class("GameSettings"), n("VideoCaptureEnabled")) {
        behaviors.add_setter(game_settings, name, Rc::new(move |lua, engine, id, value| {
            let class = engine.state.borrow().dom.at(id).class;
            if let Some(info) = engine.reflection.class_property(class, name).cloned() {
                convert::coerce(lua, engine, &info, &value)?;
            }
            Ok(())
        }));
    }
    // the proxy settings may be read and not written
    if let Some(network) = class("NetworkSettings") {
        for member in ["HttpProxyEnabled", "HttpProxyURL"] {
            let Some(name) = n(member) else { continue };
            let message = format!("The current thread cannot write '{member}' (lacking capability RobloxScript)");
            behaviors.add_setter(network, name, Rc::new(move |_, _, _, _| raise(message.clone())));
        }
    }
    // the live readings report themselves and answer the sim's reading
    for &(class_name, member, _) in READINGS {
        let (Some(owner), Some(name)) = (class(class_name), n(member)) else { continue };
        behaviors.add_getter(owner, name, getter(move |lua, engine, id| {
            engine.missing(Some(lua), class_name, member);
            stored(lua, engine, id, name)
        }));
    }
    // Studio's settings: none is emulated (its theme, editor and camera settings are Studio's own); each read reports itself and
    // answers what the property holds (its type's zero, or what a script wrote)
    if let Some(studio) = class("Studio") {
        let properties: Vec<(NameId, String)> = reflection
            .class(studio)
            .properties
            .iter()
            .filter(|&(_, &index)| reflection.property(index).owner == studio && reflection.property(index).scriptable)
            .map(|(&name, _)| (name, reflection.name(name).to_string()))
            .collect();
        for (name, member) in properties {
            behaviors.add_getter(studio, name, getter(move |lua, engine, id| {
                engine.missing(Some(lua), "Studio", &member);
                stored(lua, engine, id, name)
            }));
        }
    }
}

/// What the instance holds for a property (its default when nothing was written).
fn stored(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, name: NameId) -> Result<LuaValue, Flow> {
    let value = engine.state.borrow().dom.raw_get(id, name).cloned();
    Ok(match value {
        Some(value) => convert::to_lua(lua, engine, &value)?,
        None => LuaValue::Nil,
    })
}

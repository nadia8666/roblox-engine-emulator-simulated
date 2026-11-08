//! Per-class behaviours (port of `runtime/engine/services.luau`): methods, getters and setters
//! that override or implement reflected members, looked up through the superclass chain as
//! instance.luau's FindBehaviorMember does.
//!
//! Rust: DataModel (GetService & co.), Workspace, Camera, Players, RunService (frame events,
//! BindToRenderStep), CollectionService, GuiService, UserInputService (the static answers; runtime/
//! input's live state is phase 5), StarterGui, ContextActionService, Debris, Sound, Bindable* /
//! Remote*, HttpService (GUIDs, URL / JSON encoding, disabled requests); TweenService (tween.rs),
//! QueryDescendants (query.rs); derived property views are engine::derived. GUI behaviours that
//! need layout or text measurement (Absolute*, TextBounds, TextService:GetTextSize, ContentText)
//! and live input are a host layer's (crate::Layer; roblox-ui-renderer installs them). The script-data stubs (Player methods, 3D model helpers,
//! humanoid stand-ins (AnimationTracks: crate::animation), Path2D, EditableImage, ContentProvider, marketplace / policy /
//! social / localization mocks) are Luau (lua/services.luau). The UI Labs package is the Luau
//! runtime's own file (runtime/engine/packages/ui-labs.luau, mounted by project.rs).

pub mod encoding;
pub mod json;
pub mod query;
pub mod reflection_service;
pub mod tween;

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rbx_dom_weak::types as dom;
use rustc_hash::FxHashMap;

use super::{
    ClassId, Engine, InstanceId, K, NameId, Reflection, State,
    dom::flags,
    instance as core,
    lua::{
        Args, Flow, Ret, convert, enum_item_value, instance::fire_immediate_costing, instance::set_property,
        instance_value, none, raise, signal_value, values,
    },
    scheduler::{Phase, TimerKind},
    signal::SignalId,
    value::{Str, Value},
};

/// What Roblox's server puts in a Player when it joins (a client's own player also has PlayerScripts)
const JOINED_PLAYER_CONTAINERS: [&str; 3] = ["Backpack", "StarterGear", "PlayerGui"];

pub type MethodFn = Rc<dyn Fn(&Lua, &Rc<Engine>, InstanceId, &Args) -> Ret>;
pub type GetterFn = Rc<dyn Fn(&Lua, &Rc<Engine>, InstanceId) -> Result<LuaValue, Flow>>;
pub type SetterFn = Rc<dyn Fn(&Lua, &Rc<Engine>, InstanceId, LuaValue) -> Result<(), Flow>>;

/// A behaviour method: Rust (through the call protocol) or a Luau function returned as is (the
/// Luau service stubs: their errors and yields are the script's own).
#[derive(Clone)]
pub enum Method {
    Rust(MethodFn),
    Lua(LuaFunction),
}

#[derive(Default)]
pub struct Behaviors {
    methods: FxHashMap<(ClassId, NameId), Method>,
    getters: FxHashMap<(ClassId, NameId), GetterFn>,
    setters: FxHashMap<(ClassId, NameId), SetterFn>,
}

fn find<'a, T>(
    map: &'a FxHashMap<(ClassId, NameId), T>,
    reflection: &Reflection,
    class: ClassId,
    name: NameId,
) -> Option<(ClassId, &'a T)> {
    let mut current = Some(class);
    while let Some(id) = current {
        if let Some(found) = map.get(&(id, name)) {
            return Some((id, found));
        }
        current = reflection.class(id).superclass;
    }
    None
}

impl Behaviors {
    pub fn add_method(&mut self, class: ClassId, name: NameId, method: MethodFn) {
        self.methods.insert((class, name), Method::Rust(method));
    }

    pub fn add_lua_method(&mut self, class: ClassId, name: NameId, function: LuaFunction) {
        self.methods.insert((class, name), Method::Lua(function));
    }

    pub fn add_getter(&mut self, class: ClassId, name: NameId, getter: GetterFn) {
        self.getters.insert((class, name), getter);
    }

    pub fn add_setter(&mut self, class: ClassId, name: NameId, setter: SetterFn) {
        self.setters.insert((class, name), setter);
    }

    pub fn find_method(&self, reflection: &Reflection, class: ClassId, name: NameId) -> Option<(ClassId, &Method)> {
        find(&self.methods, reflection, class, name)
    }

    /// [`Self::find_method`], and for a deprecated lower-case method with no behaviour of its own the behaviour of the method
    /// it stands for (Reflection::method_alias: `getService` does GetService's work, through a function of its own).
    pub fn find_method_or_alias(&self, reflection: &Reflection, class: ClassId, name: NameId) -> Option<(ClassId, &Method)> {
        find(&self.methods, reflection, class, name)
            .or_else(|| reflection.method_alias(class, name).and_then(|twin| find(&self.methods, reflection, class, twin)))
    }

    pub fn find_getter(&self, reflection: &Reflection, class: ClassId, name: NameId) -> Option<&GetterFn> {
        find(&self.getters, reflection, class, name).map(|(_, f)| f)
    }

    pub fn find_setter(&self, reflection: &Reflection, class: ClassId, name: NameId) -> Option<&SetterFn> {
        find(&self.setters, reflection, class, name).map(|(_, f)| f)
    }
}

/// CollectionService's per-tag signals.
///
/// As in Studio (0.740, measured with a tagged tree moved in, around and out of the DataModel): a
/// tag counts only while its instance is in the DataModel. GetInstanceAddedSignal fires when a tag
/// is added to an instance there and when a tagged instance enters it (for every tagged instance of
/// the entering subtree, in GetDescendants' order, an instance's tags in the order they were
/// added); GetInstanceRemovedSignal when the tag is removed there or the instance leaves (Parent =
/// nil, Destroy, ClearAllChildren). Moves inside the DataModel and changes outside it fire nothing,
/// and Destroy keeps the instance's tags.
#[derive(Default)]
pub struct CollectionState {
    pub game: Option<InstanceId>,
    tags: FxHashMap<Str, (SignalId, SignalId)>,
    /// Whether any instance was ever given a tag (parent changes skip the tag walk until then)
    pub tagged: bool,
    /// Every instance ever given each tag ([`note_tag`]; freed and untagged ones are skipped when
    /// read): GetTagged looks at these instead of the whole DataModel
    holders: FxHashMap<Str, Vec<InstanceId>>,
}

/// Records that `id` was given `tag` (whatever gave it: AddTag, Clone, a model import).
pub fn note_tag(state: &mut State, id: InstanceId, tag: &Str) {
    state.collection.tagged = true;
    match state.collection.holders.get_mut(tag) {
        Some(holders) => holders.push(id),
        None => {
            state.collection.holders.insert(tag.clone(), vec![id]);
        }
    }
}

/// The descendants of `root` with `tag`, in GetDescendants' order: those of the tag's holders
/// that are still alive, tagged and below `root`, ordered by their place in the tree.
pub fn tagged_below(state: &mut State, root: InstanceId, tag: &Str) -> Vec<InstanceId> {
    let Some(holders) = state.collection.holders.get_mut(tag) else { return Vec::new() };
    let dom = &state.dom;
    holders.retain(|&id| dom.get(id).is_some_and(|instance| instance.tags.contains(tag)));
    let mut seen = rustc_hash::FxHashSet::default();
    let mut found: Vec<(Vec<u32>, InstanceId)> = holders
        .iter()
        .filter(|&&id| seen.insert(id))
        .filter_map(|&id| {
            // (the child index at each level from `root` down: preorder is their order)
            let mut path = Vec::new();
            let mut node = id;
            while node != root {
                let parent = dom.at(node).parent?;
                path.push(dom.at(parent).children.iter().position(|&child| child == node)? as u32);
                node = parent;
            }
            path.reverse();
            (!path.is_empty()).then_some((path, id))
        })
        .collect();
    found.sort_unstable();
    found.into_iter().map(|(_, id)| id).collect()
}

/// Whether `id` is the DataModel or inside it.
pub fn in_data_model(state: &State, id: InstanceId) -> bool {
    state.collection.game.is_some_and(|game| id == game || state.dom.is_descendant_of(id, game))
}

/// A subtree entered (`entered`) or is about to leave the DataModel: its tagged instances fire
/// their tags' added / removed signals.
pub fn subtree_moved(state: &mut State, root: InstanceId, entered: bool) {
    if state.collection.tags.is_empty() {
        return;
    }
    let mut nodes = vec![root];
    nodes.extend(state.dom.descendants(root));
    for node in nodes {
        if state.dom.at(node).tags.is_empty() {
            continue;
        }
        for tag in state.dom.at(node).tags.clone() {
            if let Some(&(added, removed)) = state.collection.tags.get(&tag) {
                core::fire(state, if entered { added } else { removed }, vec![Value::Instance(node)]);
            }
        }
    }
}

fn tag_signals(state: &mut State, tag: &Str) -> (SignalId, SignalId) {
    if let Some(&signals) = state.collection.tags.get(tag) {
        return signals;
    }
    let added = state.signals.create(Str::from("InstanceAdded"));
    let removed = state.signals.create(Str::from("InstanceRemoved"));
    state.collection.tags.insert(tag.clone(), (added, removed));
    (added, removed)
}

/// Hooks.TagAdded: GetInstanceAddedSignal fires for instances in the DataModel.
pub fn tag_added(state: &mut State, id: InstanceId, tag: &Str) {
    note_tag(state, id, tag);
    if !in_data_model(state, id) {
        return;
    }
    if let Some(&(added, _)) = state.collection.tags.get(tag) {
        core::fire(state, added, vec![Value::Instance(id)]);
    }
}

/// Hooks.TagRemoved: GetInstanceRemovedSignal fires for instances in the DataModel.
pub fn tag_removed(state: &mut State, id: InstanceId, tag: &Str) {
    if !in_data_model(state, id) {
        return;
    }
    if let Some(&(_, removed)) = state.collection.tags.get(tag) {
        core::fire(state, removed, vec![Value::Instance(id)]);
    }
}

fn class(reflection: &Reflection, name: &str) -> Option<ClassId> {
    reflection.class_id(name)
}

fn name(reflection: &Reflection, name: &str) -> Option<NameId> {
    reflection.name_id(name)
}

/// Players:GetPlayers: the local player, then every other Player a harness parented to Players (ree has one
/// joined player of its own; a lobby harness adds the others), in child order.
fn joined_players(engine: &Rc<Engine>, players: InstanceId) -> Vec<InstanceId> {
    let local = engine.world().local_player;
    let state = engine.state.borrow();
    let others = state
        .dom
        .children_of(players)
        .iter()
        .copied()
        .filter(|&child| Some(child) != local && engine.reflection.is_a(state.dom.at(child).class, "Player"));
    local.into_iter().chain(others).collect()
}

/// A behaviour method from a closure (a layer's behaviours: [`Behaviors::add_method`]).
pub fn method(f: impl Fn(&Lua, &Rc<Engine>, InstanceId, &Args) -> Ret + 'static) -> MethodFn {
    Rc::new(f)
}

/// A behaviour getter from a closure ([`Behaviors::add_getter`]).
pub fn getter(f: impl Fn(&Lua, &Rc<Engine>, InstanceId) -> Result<LuaValue, Flow> + 'static) -> GetterFn {
    Rc::new(f)
}

fn constant(value: impl Fn(&Lua) -> LuaResult<LuaValue> + 'static) -> MethodFn {
    Rc::new(move |lua, _, _, _| Ok(values([value(lua)?])))
}

fn string_value(lua: &Lua, text: &str) -> LuaResult<LuaValue> {
    Ok(LuaValue::String(lua.create_string(text)?))
}

fn enum_item(lua: &Lua, engine: &Engine, enum_name: &str, item: &str) -> LuaResult<LuaValue> {
    match engine.reflection.enum_item(enum_name, item) {
        Some(item) => enum_item_value(lua, engine, item),
        None => Ok(LuaValue::Nil),
    }
}

fn vector2(lua: &Lua, x: f64, y: f64) -> LuaResult<LuaValue> {
    let vector = lune_roblox::datatypes::types::Vector2::from(dom::Vector2::new(x as f32, y as f32));
    Ok(LuaValue::UserData(lua.create_userdata(vector)?))
}

/// GetService: the existing service child, or a new one (named, parented without events and
/// parent-locked).
pub fn get_service(engine: &Engine, game: InstanceId, service_name: &str) -> Result<InstanceId, Flow> {
    let reflection = &engine.reflection;
    let mut state = engine.state.borrow_mut();
    let found = state
        .dom
        .at(game)
        .children
        .iter()
        .copied()
        .find(|&child| &*reflection.class(state.dom.at(child).class).name == service_name);
    if let Some(found) = found {
        return Ok(found);
    }
    let Some(class) = reflection.class_id(service_name).filter(|&c| reflection.class(c).service) else {
        return raise(format!("'{service_name}' is not a valid Service name"));
    };
    // Roblox's names that are not the class's: scripts index `game["Run Service"]` and
    // `UserSettings().GameSettings`
    let instance_name = match service_name {
        "RunService" => "Run Service",
        "UserGameSettings" => "GameSettings",
        other => other,
    };
    let service = core::create_named(&mut state, reflection, class, instance_name);
    state.dom.attach(service, game);
    state.dom.at_mut(service).set_flag(flags::PARENT_LOCKED, true);
    Ok(service)
}

/// A UserSettings object (outside the DataModel, as in Roblox): `UserSettings()` makes it on its first
/// call and returns it from then on. None when the reflection has no UserSettings class.
pub fn user_settings(engine: &Engine) -> Option<InstanceId> {
    let class = engine.reflection.class_id("UserSettings")?;
    let id = {
        let mut state = engine.state.borrow_mut();
        let id = core::create_named(&mut state, &engine.reflection, class, "UserSettings");
        state.dom.at_mut(id).set_flag(flags::PARENT_LOCKED, true);
        state.dom.pin(id);
        id
    };
    // GameSettings is there from the start: scripts read `UserSettings().GameSettings` (Roblox's own
    // PlayerModule among them) before any GetService
    let _ = get_service(engine, id, "UserGameSettings");
    Some(id)
}

pub fn find_service(engine: &Engine, game: InstanceId, service_name: &str) -> Option<InstanceId> {
    let state = engine.state.borrow();
    state
        .dom
        .at(game)
        .children
        .iter()
        .copied()
        .find(|&child| &*engine.reflection.class(state.dom.at(child).class).name == service_name)
}

struct RenderStep {
    name: String,
    priority: f64,
    callback: LuaFunction,
    sequence: u64,
}

/// Installs the service behaviours (Services.Install) and the RunService frame hooks.
pub fn install(engine: &Rc<Engine>) {
    let reflection = engine.reflection.clone();
    let mut behaviors = engine.behaviors.borrow_mut();
    let options = engine.options.clone();

    // DataModel
    if let Some(data_model) = class(&reflection, "DataModel") {
        let get = method(|lua, engine, id, args| {
            let service_name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
            let service = get_service(engine, id, &service_name)?;
            Ok(values([instance_value(lua, engine, service)?]))
        });
        behaviors.add_method(data_model, K::GetService as NameId, get.clone());
        behaviors.add_method(data_model, K::service as NameId, get);
        behaviors.add_method(data_model, K::FindService as NameId, method(|lua, engine, id, args| {
            let service_name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
            Ok(values([match find_service(engine, id, &service_name) {
                Some(service) => instance_value(lua, engine, service)?,
                None => LuaValue::Nil,
            }]))
        }));
        behaviors.add_method(data_model, K::IsLoaded as NameId, constant(|_| Ok(LuaValue::Boolean(true))));
        behaviors.add_getter(data_model, K::Workspace as NameId, getter(|lua, engine, id| {
            let service = get_service(engine, id, "Workspace")?;
            Ok(instance_value(lua, engine, service)?)
        }));
        // (the dump's deprecated `game.lighting` is the Lighting service: measured, tools/parity/probes/aliases.luau;
        // `game.workspace` reads Workspace through Reflection::property_alias)
        if let Some(lighting) = reflection.name_id("lighting") {
            behaviors.add_getter(data_model, lighting, getter(|lua, engine, id| {
                let service = get_service(engine, id, "Lighting")?;
                Ok(instance_value(lua, engine, service)?)
            }));
        }
        behaviors.add_method(data_model, K::BindToClose as NameId, method(|_, _, _, _| Ok(none())));
        behaviors.add_method(data_model, K::GetJobsInfo as NameId, constant(|lua| Ok(LuaValue::Table(lua.create_table()?))));
        let place_id = options.place_id;
        behaviors.add_getter(data_model, K::PlaceId as NameId, getter(move |_, _, _| Ok(LuaValue::Number(place_id))));
        let game_id = options.game_id;
        behaviors.add_getter(data_model, K::GameId as NameId, getter(move |_, _, _| Ok(LuaValue::Number(game_id))));
        behaviors.add_getter(data_model, K::JobId as NameId, getter(|lua, _, _| Ok(string_value(lua, "")?)));
        let creator_id = options.creator_id;
        behaviors.add_getter(data_model, K::CreatorId as NameId, getter(move |_, _, _| Ok(LuaValue::Number(creator_id))));
        behaviors.add_getter(data_model, K::CreatorType as NameId, getter(|lua, engine, _| {
            Ok(enum_item(lua, engine, "CreatorType", "Group")?)
        }));
        behaviors.add_getter(data_model, K::PlaceVersion as NameId, getter(|_, _, _| Ok(LuaValue::Number(0.0))));
        behaviors.add_getter(data_model, K::PrivateServerId as NameId, getter(|lua, _, _| Ok(string_value(lua, "")?)));
        behaviors.add_getter(data_model, K::PrivateServerOwnerId as NameId, getter(|_, _, _| Ok(LuaValue::Number(0.0))));
    }

    // UserSettings(): GetService / FindService give its settings services (children made on first
    // use, like the DataModel's)
    if let Some(settings) = class(&reflection, "UserSettings") {
        let get = method(|lua, engine, id, args| {
            let service_name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
            let service = get_service(engine, id, &service_name)?;
            Ok(values([instance_value(lua, engine, service)?]))
        });
        behaviors.add_method(settings, K::GetService as NameId, get);
        behaviors.add_method(settings, K::FindService as NameId, method(|lua, engine, id, args| {
            let service_name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
            Ok(values([match find_service(engine, id, &service_name) {
                Some(service) => instance_value(lua, engine, service)?,
                None => LuaValue::Nil,
            }]))
        }));
    }

    // Workspace / Camera
    if let Some(workspace) = class(&reflection, "Workspace") {
        let edit = options.edit_mode();
        behaviors.add_getter(workspace, K::DistributedGameTime as NameId, getter(move |_, engine, _| {
            // Measured: 0 in edit mode (the game never started running), like time()
            Ok(LuaValue::Number(if edit { 0.0 } else { engine.state.borrow().sched.now }))
        }));
        behaviors.add_method(workspace, K::GetServerTimeNow as NameId, method(|_, engine, _, _| {
            let time = engine.state.borrow_mut().sched.unix_time();
            Ok(values([LuaValue::Number(time)]))
        }));
        behaviors.add_method(workspace, K::GetRealPhysicsFPS as NameId, constant(|_| Ok(LuaValue::Number(60.0))));
        behaviors.add_getter(workspace, K::CurrentCamera as NameId, getter(|lua, engine, id| {
            let stored = engine.state.borrow().dom.at(id).stored(K::CurrentCamera as NameId).cloned();
            let camera = match stored {
                Some(Value::Instance(camera)) => camera,
                _ => engine.world().camera,
            };
            Ok(instance_value(lua, engine, camera)?)
        }));
    }
    if let Some(camera) = class(&reflection, "Camera") {
        let width = options.camera_width.unwrap_or(options.viewport_width);
        let height = options.camera_height.unwrap_or(options.viewport_height);
        behaviors.add_getter(camera, K::ViewportSize as NameId, getter(move |lua, _, _| Ok(vector2(lua, width, height)?)));
    }

    // Players / Player
    if let Some(players) = class(&reflection, "Players") {
        let local_player = getter(|lua, engine, _| match engine.world().local_player {
            Some(player) => Ok(instance_value(lua, engine, player)?),
            None => Ok(LuaValue::Nil),
        });
        behaviors.add_getter(players, K::LocalPlayer as NameId, local_player.clone());
        behaviors.add_getter(players, K::localPlayer as NameId, local_player);
        // The local player, then the Player instances a harness parented to Players (other players of a lobby)
        behaviors.add_method(players, K::GetPlayers as NameId, method(|lua, engine, this, _| {
            let table = lua.create_table()?;
            for player in joined_players(engine, this) {
                table.raw_push(instance_value(lua, engine, player)?)?;
            }
            Ok(values([LuaValue::Table(table)]))
        }));
        behaviors.add_method(players, K::GetPlayerByUserId as NameId, method(|lua, engine, this, args| {
            let wanted = args.number(1);
            for player in joined_players(engine, this) {
                let user_id = engine.state.borrow().dom.number(player, K::UserId as NameId);
                if user_id.is_some() && user_id == wanted {
                    return Ok(values([instance_value(lua, engine, player)?]));
                }
            }
            Ok(values([LuaValue::Nil]))
        }));
        if let Some(n) = name(&reflection, "GetPlayerFromCharacter") {
            let character = name(&reflection, "Character");
            behaviors.add_method(players, n, method(move |lua, engine, this, args| {
                let (Some(character), Some(model)) = (character, args.instance(1)) else { return Ok(values([LuaValue::Nil])) };
                for player in joined_players(engine, this) {
                    let held = engine.state.borrow().dom.at(player).stored(character).cloned();
                    if matches!(held, Some(Value::Instance(held)) if held == model) {
                        return Ok(values([instance_value(lua, engine, player)?]));
                    }
                }
                Ok(values([LuaValue::Nil]))
            }));
        }
        if let Some(n) = name(&reflection, "GetUserIdFromNameAsync") {
            behaviors.add_method(players, n, constant(|_| Ok(LuaValue::Number(1.0))));
        }
        // Players a harness adds after the run started: PlayerAdded when a Player is parented to Players,
        // PlayerRemoving when it leaves, and Player.CharacterAdded when its Character is set to a model
        // (the local player joined before the run, and CharacterRemoving is not fired: a changed property
        // carries no old value). On a server a joining player gets the containers Roblox's server gives
        // it (Backpack, StarterGear, PlayerGui) before PlayerAdded, unless the harness made them.
        if let (Some(player_class), Some(added), Some(removing), Some(character), Some(character_added)) = (
            class(&reflection, "Player"),
            name(&reflection, "PlayerAdded"),
            name(&reflection, "PlayerRemoving"),
            name(&reflection, "Character"),
            name(&reflection, "CharacterAdded"),
        ) {
            let weak = Rc::downgrade(engine);
            let reflection_for_parent = engine.reflection.clone();
            let server = options.server();
            let loopback = options.loopback;
            let mut hooks = engine.hooks.borrow_mut();
            hooks.hierarchy_changed.push(Box::new(move |state, id, old, new| {
                let Some(engine) = weak.upgrade() else { return };
                if !reflection_for_parent.is_a(state.dom.at(id).class, "Player") || (engine.world().local_player == Some(id) && !loopback) {
                    return;
                }
                let is_players = |state: &State, parent: Option<InstanceId>| {
                    parent.is_some_and(|parent| state.dom.at(parent).class == players)
                };
                if is_players(state, old) {
                    core::fire_event(state, old.unwrap(), removing, vec![Value::Instance(id)]);
                }
                if is_players(state, new) {
                    if (server || loopback) && !is_players(state, old) {
                        for name in JOINED_PLAYER_CONTAINERS {
                            if state.dom.find_child(id, name.as_bytes()).is_some() {
                                continue;
                            }
                            if let Some(class) = reflection_for_parent.class_id(name) {
                                let child = core::create_named(state, &reflection_for_parent, class, name);
                                state.dom.attach(child, id);
                            }
                        }
                    }
                    core::fire_event(state, new.unwrap(), added, vec![Value::Instance(id)]);
                }
            }));
            hooks.property_changed.push(Box::new(move |state, id, changed| {
                if changed != character || state.dom.at(id).class != player_class {
                    return;
                }
                if let Some(Value::Instance(model)) = state.dom.at(id).stored(character).cloned() {
                    core::fire_event(state, id, character_added, vec![Value::Instance(model)]);
                }
            }));
        }
    }
    // Player methods and the other script-data stubs are Luau (lua/services.luau)

    // RunService: frame events (measured in Studio). Edit mode: Heartbeat ~240 Hz;
    // per render step the BindToRenderStep callbacks, the tween step, then RenderStepped before
    // PreRender. PreAnimation / Stepped / PreSimulation / PostSimulation never fire in edit mode.
    // A server (--run-context server) answers as Roblox's RunService documentation's table gives a Play-mode
    // server: IsServer, IsStudio and IsRunning true, IsClient and IsRunMode false (IsEdit is refused to game scripts:
    // below). It renders nothing: BindToRenderStep callbacks and PreRender never run, and RenderStepped refuses a
    // connection (measured in a server's game script, Studio 0.744: tools/parity/probes/run_service_sides.luau).
    // `--studio false` answers IsStudio as a live game does (false), on either side.
    if let Some(run_service) = class(&reflection, "RunService") {
        let mode_edit = options.edit_mode();
        let server = options.server();
        let loopback = options.loopback;
        let studio = options.studio;
        behaviors.add_method(run_service, K::IsStudio as NameId, constant(move |_| Ok(LuaValue::Boolean(studio))));
        behaviors.add_method(run_service, K::IsClient as NameId, constant(move |_| Ok(LuaValue::Boolean(!server))));
        behaviors.add_method(run_service, K::IsServer as NameId, constant(move |_| Ok(LuaValue::Boolean(server || loopback))));
        behaviors.add_method(run_service, K::IsRunning as NameId, constant(move |_| Ok(LuaValue::Boolean(!mode_edit))));
        behaviors.add_method(run_service, K::IsRunMode as NameId, constant(|_| Ok(LuaValue::Boolean(false))));
        // (a game script may not ask: "The current thread cannot call 'IsEdit' (lacking capability PluginOrOpenCloud)" on a
        // playing client and on a server, measured, Studio 0.744: tools/parity/probes/run_service_sides.luau; edit mode, where
        // a plugin's story runs, answers)
        behaviors.add_method(run_service, K::IsEdit as NameId, method(move |_, _, _, _| {
            if !mode_edit {
                return raise("The current thread cannot call 'IsEdit' (lacking capability PluginOrOpenCloud)");
            }
            Ok(values([LuaValue::Boolean(true)]))
        }));

        let steps: Rc<RefCell<Vec<Rc<RenderStep>>>> = Rc::default();
        let sequence = Rc::new(std::cell::Cell::new(0u64));
        let bind_steps = steps.clone();
        behaviors.add_method(run_service, K::BindToRenderStep as NameId, method(move |lua, _, _, args| {
            let LuaValue::Function(callback) = args.get(3).clone() else {
                return raise("Argument 3 missing or nil");
            };
            // (a priority that is no number raises "Unable to cast string to int": measured in a playing client, Studio 0.744,
            // tools/parity/probes/frame_phases.luau, with "high"; a numeric string is read as its number)
            let level = match args.get(2) {
                LuaValue::String(_) => match lua.coerce_number(args.get(2).clone())? {
                    Some(level) => level,
                    None => return raise("Unable to cast string to int"),
                },
                _ => args.number(2).unwrap_or(0.0),
            };
            sequence.set(sequence.get() + 1);
            let level = if level >= 0.0 { level.floor() } else { level.ceil() };
            let name = lua.coerce_string(args.get(1).clone())?.map(|s| s.to_string_lossy()).unwrap_or_else(|| "nil".into());
            let mut list = bind_steps.borrow_mut();
            list.push(Rc::new(RenderStep { name, priority: level, callback, sequence: sequence.get() }));
            list.sort_by(|a, b| a.priority.partial_cmp(&b.priority).unwrap_or(std::cmp::Ordering::Equal).then(a.sequence.cmp(&b.sequence)));
            Ok(none())
        }));
        let unbind_steps = steps.clone();
        behaviors.add_method(run_service, K::UnbindFromRenderStep as NameId, method(move |lua, _, _, args| {
            let name = lua.coerce_string(args.get(1).clone())?.map(|s| s.to_string_lossy());
            unbind_steps.borrow_mut().retain(|step| Some(&step.name) != name.as_ref());
            Ok(none())
        }));
        let mut state = engine.state.borrow_mut();
        let hook_steps = steps;
        state.sched.add_hook(Phase::RenderStep, Rc::new(move |lua, engine, delta| {
            if server {
                return Ok(());
            }
            let snapshot: Vec<Rc<RenderStep>> = hook_steps.borrow().clone();
            for step in snapshot {
                // Unbound by an earlier callback of this render step: skipped
                if hook_steps.borrow().iter().any(|s| Rc::ptr_eq(s, &step)) {
                    // (a binding's call: cost.rs RENDER_STEP_DISPATCH)
                    engine.state.borrow().sched.charge_dispatch(crate::cost::RENDER_STEP_DISPATCH);
                    let thread = lua.create_thread(step.callback.clone())?;
                    super::scheduler::resume(lua, engine, &thread, LuaMultiValue::from_iter([LuaValue::Number(delta)]));
                }
            }
            Ok(())
        }));
        state.sched.add_hook(Phase::RenderEvents, Rc::new(move |lua, engine, delta| {
            if server {
                return Ok(());
            }
            fire_frame_event(lua, engine, K::RenderStepped as NameId, &[Value::Number(delta)])?;
            fire_frame_event(lua, engine, K::PreRender as NameId, &[Value::Number(delta)])
        }));
        state.sched.add_hook(Phase::Simulation, Rc::new(|lua, engine, _| crate::lua::actors::start_scripts(lua, engine)));
        state.sched.add_hook(Phase::Simulation, Rc::new(|lua, engine, delta| {
            let now = engine.state.borrow().sched.now;
            fire_frame_event(lua, engine, K::PreAnimation as NameId, &[Value::Number(delta)])?;
            // The Animator step: AnimationTracks pose their rigs' joints (crate::animation)
            crate::luau_profile::engine_step("animation", || crate::animation::step(engine, delta));
            super::scheduler::flush_deferred(lua, engine)?;
            fire_frame_event(lua, engine, K::Stepped as NameId, &[Value::Number(now), Value::Number(delta)])?;
            fire_frame_event(lua, engine, K::PreSimulation as NameId, &[Value::Number(delta)])?;
            // The deferred handlers of what Stepped and PreSimulation handlers did (a Changed or ChildAdded handler of a write)
            // run here, before the engine's step and PostSimulation: Studio's client orders a PreSimulation write's Changed,
            // AttributeChanged and ChildAdded handlers between PreSimulation and PostSimulation, and a Motor6D.Transform one of
            // them writes is applied by the batch below the same frame (a deferred_phase probe)
            super::scheduler::flush_deferred(lua, engine)?;
            // Motor6D.Transform: applied in a batch after PreSimulation (Roblox's Motor6D docs)
            crate::luau_profile::engine_step("joints", || crate::world::joints::step_transforms(engine));
            // The physics step: free assemblies fall (world::gravity)
            crate::luau_profile::engine_step("physics", || crate::world::gravity::step(engine, delta));
            fire_frame_event(lua, engine, K::PostSimulation as NameId, &[Value::Number(delta)])
        }));
        state.sched.add_hook(Phase::Heartbeat, Rc::new(|lua, engine, delta| {
            fire_frame_event(lua, engine, K::Heartbeat as NameId, &[Value::Number(delta)])
        }));
    }

    // TweenService, Instance:QueryDescendants, AnimationTracks
    tween::install(engine, &mut behaviors);
    reflection_service::install(engine, &mut behaviors);
    crate::animation::install(engine, &mut behaviors);
    crate::meshes::install(engine, &mut behaviors);
    query::install(engine, &mut behaviors);
    // (a client keeps EncodingService unemulated, as it was: its output does not change)
    if options.server() {
        encoding::install(engine, &mut behaviors);
    }

    // CollectionService
    if let Some(collection) = class(&reflection, "CollectionService") {
        if let Some(n) = name(&reflection, "GetTagged") {
            behaviors.add_method(collection, n, method(|lua, engine, _, args| {
                let tag = args.string(1).map(|s| Str::new(&s.as_bytes())).unwrap_or_default();
                let game = engine.world().game;
                let tagged = tagged_below(&mut engine.state.borrow_mut(), game, &tag);
                // (an instance it lists: cost.rs TAGGED_LISTED)
                let listed = tagged.len() as f64 * crate::cost::TAGGED_LISTED;
                engine.state.borrow().sched.charge(listed);
                crate::boundary::add_cost(listed);
                let table = lua.create_table()?;
                for id in tagged {
                    table.raw_push(instance_value(lua, engine, id)?)?;
                }
                Ok(values([LuaValue::Table(table)]))
            }));
        }
        // GetAllTags: every tag in the DataModel, in first-seen order of a depth-first walk
        if let Some(n) = name(&reflection, "GetAllTags") {
            behaviors.add_method(collection, n, method(|lua, engine, _, _| {
                let game = engine.world().game;
                let mut tags: Vec<Str> = Vec::new();
                {
                    let state = engine.state.borrow();
                    for id in state.dom.descendants_where(game, |instance| !instance.tags.is_empty()) {
                        for tag in &state.dom.at(id).tags {
                            if !tags.contains(tag) {
                                tags.push(tag.clone());
                            }
                        }
                    }
                }
                let table = lua.create_table()?;
                for tag in tags {
                    table.raw_push(lua.create_string(tag.as_bytes())?)?;
                }
                Ok(values([LuaValue::Table(table)]))
            }));
        }
        for (member, added) in [("GetInstanceAddedSignal", true), ("GetInstanceRemovedSignal", false)] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(collection, n, method(move |lua, engine, _, args| {
                    let tag = args.string(1).map(|s| Str::new(&s.as_bytes())).unwrap_or_default();
                    let (a, r) = tag_signals(&mut engine.state.borrow_mut(), &tag);
                    Ok(values([signal_value(lua, engine, if added { a } else { r })?]))
                }));
            }
        }
        for member in ["AddTag", "RemoveTag", "HasTag", "GetTags"] {
            if let Some(n) = name(&reflection, member) {
                let forward = n;
                behaviors.add_method(collection, n, method(move |lua, engine, _, args| {
                    let Some(target) = args.instance(1) else { return raise("Argument 1 missing or nil") };
                    let target_class = engine.state.borrow().dom.at(target).class;
                    let Some((_, f)) = engine.behaviors.borrow().find_method(&engine.reflection, target_class, forward).map(|(c, f)| (c, f.clone())) else {
                        return Ok(none());
                    };
                    // CollectionService:AddTag(Target, Tag) is Target:AddTag(Tag)
                    let forwarded = Args(args.0.iter().skip(1).cloned().collect());
                    match f {
                        Method::Rust(f) => f(lua, engine, target, &forwarded),
                        Method::Lua(f) => Ok(f.call::<LuaMultiValue>(forwarded.0)?),
                    }
                }));
            }
        }
    }

    // GuiService / UserInputService
    if let Some(gui) = class(&reflection, "GuiService") {
        let inset = options.topbar_inset;
        if let Some(n) = name(&reflection, "GetGuiInset") {
            behaviors.add_method(gui, n, method(move |lua, _, _, _| Ok(values([vector2(lua, 0.0, inset)?, vector2(lua, 0.0, 0.0)?]))));
        }
        for member in ["IsTenFootInterface", "GetEmotesMenuOpen", "GetInspectMenuEnabled", "GetGameplayPausedNotificationEnabled"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(gui, n, constant(|_| Ok(LuaValue::Boolean(false))));
            }
        }
        for member in ["CloseInspectMenu", "SetEmotesMenuOpen", "Select", "AddSelectionParent", "RemoveSelectionGroup"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(gui, n, method(|_, _, _, _| Ok(none())));
            }
        }
    }
    if let Some(input) = class(&reflection, "UserInputService") {
        let device = options.device.clone();
        // (a phone is a touch device: cost.rs device_scale gives it its prices too)
        let touch = device == "touch" || device == "phone";
        let pad = device == "gamepad";
        for (member, value) in [
            ("TouchEnabled", touch),
            ("KeyboardEnabled", !touch && !pad),
            ("MouseEnabled", !touch && !pad),
            ("GamepadEnabled", pad),
        ] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_getter(input, n, getter(move |_, _, _| Ok(LuaValue::Boolean(value))));
            }
        }
        if let Some(n) = name(&reflection, "GetMouseLocation") {
            behaviors.add_method(input, n, method(|lua, _, _, _| Ok(values([vector2(lua, -1000.0, -1000.0)?]))));
        }
        let edit = options.edit_mode();
        if let Some(n) = name(&reflection, "GetLastInputType") {
            behaviors.add_method(input, n, method(move |lua, engine, _, _| {
                let item = if touch {
                    "Touch"
                } else if pad {
                    "Gamepad1"
                } else if edit {
                    // Measured: Studio in edit mode reports Focus (the window focus event)
                    "Focus"
                } else {
                    "MouseMovement"
                };
                Ok(values([enum_item(lua, engine, "UserInputType", item)?]))
            }));
        }
        for member in ["IsKeyDown", "IsMouseButtonPressed", "IsGamepadButtonDown", "GetGamepadConnected", "GamepadSupports"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(input, n, constant(|_| Ok(LuaValue::Boolean(false))));
            }
        }
        for member in ["GetKeysPressed", "GetMouseButtonsPressed", "GetConnectedGamepads", "GetNavigationGamepads", "GetSupportedGamepadKeyCodes", "GetGamepadState"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(input, n, constant(|lua| Ok(LuaValue::Table(lua.create_table()?))));
            }
        }
        if let Some(n) = name(&reflection, "GetFocusedTextBox") {
            behaviors.add_method(input, n, constant(|_| Ok(LuaValue::Nil)));
        }
        if let Some(n) = name(&reflection, "GetMouseDelta") {
            behaviors.add_method(input, n, constant(|lua| vector2(lua, 0.0, 0.0)));
        }
        if let Some(n) = name(&reflection, "GetImageForKeyCode") {
            behaviors.add_method(input, n, constant(|lua| string_value(lua, "")));
        }
        if let Some(n) = name(&reflection, "GetStringForKeyCode") {
            behaviors.add_method(input, n, method(|lua, engine, _, args| {
                let item = match args.get(1) {
                    LuaValue::UserData(ud) => ud.borrow::<super::lua::types::LEnumItem>().ok().map(|i| i.0),
                    _ => None,
                };
                let text = item.map(|i| engine.reflection.enum_item_info(i).name.to_string()).unwrap_or_default();
                Ok(values([string_value(lua, &text)?]))
            }));
        }
    }

    // StarterGui / ContextActionService / Debris / Sound
    if let Some(starter) = class(&reflection, "StarterGui") {
        for member in ["SetCore", "SetCoreGuiEnabled"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(starter, n, method(|_, _, _, _| Ok(none())));
            }
        }
        if let Some(n) = name(&reflection, "GetCore") {
            behaviors.add_method(starter, n, method(|_, _, _, args| {
                let name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
                let active = matches!(name.as_str(), "ChatActive" | "PointsNotificationsActive" | "BadgesNotificationsActive");
                Ok(values([if active { LuaValue::Boolean(true) } else { LuaValue::Nil }]))
            }));
        }
        if let Some(n) = name(&reflection, "GetCoreGuiEnabled") {
            behaviors.add_method(starter, n, constant(|_| Ok(LuaValue::Boolean(true))));
        }
    }
    if let Some(actions) = class(&reflection, "ContextActionService") {
        for member in ["BindAction", "BindActionAtPriority", "UnbindAction", "UnbindAllActions", "SetTitle", "SetImage", "SetDescription", "SetPosition", "BindActivate", "UnbindActivate"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(actions, n, method(|_, _, _, _| Ok(none())));
            }
        }
        if let Some(n) = name(&reflection, "GetButton") {
            behaviors.add_method(actions, n, constant(|_| Ok(LuaValue::Nil)));
        }
        for member in ["GetAllBoundActionInfo", "GetBoundActionInfo"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(actions, n, constant(|lua| Ok(LuaValue::Table(lua.create_table()?))));
            }
        }
    }
    if let (Some(debris), Some(n)) = (class(&reflection, "Debris"), name(&reflection, "AddItem")) {
        behaviors.add_method(debris, n, method(|lua, engine, _, args| {
            // Measured in a playing client (Studio 0.744, tools/parity/probes/lifetime.luau): an item that is no Instance
            // raises "Unable to cast value to Object" (nil is ignored), a lifetime that is no number "Unable to cast string to
            // double", and a lifetime of 0 or less removes the item before the next Heartbeat (here at once; whether the
            // client does it in the call is not measured)
            let item_value = args.get(1);
            if !item_value.is_nil() && args.instance(1).is_none() {
                return raise("Unable to cast value to Object");
            }
            let lifetime = match args.get(2) {
                LuaValue::String(_) => match lua.coerce_number(args.get(2).clone())? {
                    Some(lifetime) => lifetime,
                    None => return raise("Unable to cast string to double"),
                },
                _ => args.number(2).unwrap_or(10.0),
            };
            if let (Some(item), true) = (args.instance(1), lifetime <= 0.0) {
                super::lua::instance::destroy(lua, engine, item)?;
                return Ok(none());
            }
            if let Some(item) = args.instance(1) {
                super::lua::refs::admit(engine, super::lua::refs::Hold::Work, "AddItem")?;
                // The item rides along as the timer's argument: queued work keeps it alive
                // (lua/raw.rs collection) until Debris destroys it
                let e = engine.clone();
                let body = lua.create_function(move |lua, item: LuaValue| {
                    if let Some(item) = super::lua::instance_id(&item) {
                        // (a client's Debris destroys the item in the engine's own step, outside any script: no script pays for it)
                        let muted = e.state.borrow().sched.muted.clone();
                        muted.set(muted.get() + 1);
                        let _ = super::lua::instance::destroy(lua, &e, item);
                        muted.set(muted.get() - 1);
                    }
                    Ok(())
                })?;
                let thread = lua.create_thread(body)?;
                engine.state.borrow_mut().sched.schedule_task(TimerKind::Delay, lifetime, thread, vec![Value::Instance(item)]);
            }
            Ok(none())
        }));
    }
    if let Some(sound) = class(&reflection, "Sound") {
        // (Play / Stop / Pause / Resume are engine::world's: they set Playing and report the play)
        if let Some(n) = name(&reflection, "IsLoaded") {
            behaviors.add_getter(sound, n, getter(|_, _, _| Ok(LuaValue::Boolean(true))));
        }
        if let Some(n) = name(&reflection, "TimeLength") {
            behaviors.add_getter(sound, n, getter(|_, _, _| Ok(LuaValue::Number(1.0))));
        }
    }

    // Scripts' Source reads their text, as Studio's command bar and plugins read it (the mounted
    // file of a project script, else the stored property)
    if let Some(container) = class(&reflection, "LuaSourceContainer") {
        behaviors.add_getter(container, K::Source as NameId, getter(|lua, engine, id| {
            let text = super::lua::modules::source_of(engine, id).unwrap_or_default();
            Ok(LuaValue::String(lua.create_string(&text)?))
        }));
    }

    // Remotes never reach a server offline. With `--loopback` one VM plays both sides: FireServer reaches OnServerEvent with the
    // local player in front, FireClient (to the local player) and FireAllClients reach OnClientEvent; arguments are copied as
    // Bindable's are. Bindables fire locally
    let loopback = options.loopback;
    for remote in ["RemoteEvent", "UnreliableRemoteEvent"] {
        let Some(c) = class(&reflection, remote) else { continue };
        for (member, event, from_player) in [("FireServer", "OnServerEvent", true), ("FireClient", "OnClientEvent", false), ("FireAllClients", "OnClientEvent", false)] {
            let (Some(n), Some(event)) = (name(&reflection, member), name(&reflection, event)) else { continue };
            if !loopback && member != "FireServer" {
                continue;
            }
            behaviors.add_method(c, n, method(move |lua, engine, id, args| {
                if !loopback {
                    return Ok(none());
                }
                let local = engine.world().local_player;
                let mut arguments: Vec<Value> = Vec::new();
                if from_player {
                    let Some(player) = local else { return Ok(none()) };
                    arguments.push(Value::Instance(player));
                }
                if member == "FireClient" && args.instance(1) != local {
                    return Ok(none());
                }
                let skip = if member == "FireClient" { 2 } else { 1 };
                for argument in args.0.iter().skip(skip) {
                    match super::lua::bindable::sent_value(lua, engine, argument) {
                        Ok(value) => arguments.push(value),
                        Err(message) => return super::lua::raise(message),
                    }
                }
                core::fire_event(&mut engine.state.borrow_mut(), id, event, arguments);
                Ok(none())
            }));
        }
    }
    if let Some(bindable) = class(&reflection, "BindableEvent") {
        behaviors.add_method(bindable, K::Fire as NameId, method(|lua, engine, id, args| {
            // (raw.rs fires one without table, function or thread arguments, which hold nothing for later; tables and
            // buffers are copied as Roblox copies them, threads arrive as nil: lua/bindable.rs)
            super::lua::refs::admit(engine, super::lua::refs::Hold::Work, "Fire")?;
            let mut arguments: Vec<Value> = Vec::with_capacity(args.0.len().saturating_sub(1));
            for argument in args.0.iter().skip(1) {
                match super::lua::bindable::sent_value(lua, engine, argument) {
                    Ok(value) => arguments.push(value),
                    Err(message) => return super::lua::raise(message),
                }
            }
            core::fire_event_measured(&mut engine.state.borrow_mut(), id, K::Event as NameId, arguments, crate::cost::SignalKind::Bindable);
            Ok(none())
        }));
    }
    // HttpService (GUIDs and URL encoding; JSON and requests are phase 4)
    if let Some(http) = class(&reflection, "HttpService") {
        let counter = Rc::new(std::cell::Cell::new(0u32));
        if let Some(n) = name(&reflection, "GenerateGUID") {
            behaviors.add_method(http, n, method(move |lua, _, _, args| {
                counter.set(counter.get() + 1);
                let hex = format!("{:08X}", counter.get());
                let guid = format!("{hex}-0000-4000-8000-00000000{}", &hex[4..8]);
                let text = if matches!(args.get(1), LuaValue::Boolean(false)) { guid } else { format!("{{{guid}}}") };
                Ok(values([string_value(lua, &text)?]))
            }));
        }
        // JSONEncode / JSONDecode as a playing client runs them (json.rs), and the disabled requests; their errors carry
        // no position, as Roblox raises them
        if let Some(n) = name(&reflection, "JSONEncode") {
            behaviors.add_method(http, n, method(|lua, engine, _, args| match json::encode(lua, engine, args.get(1)) {
                Ok(text) => Ok(values([string_value(lua, &text)?])),
                Err(message) => raise(message),
            }));
        }
        if let Some(n) = name(&reflection, "JSONDecode") {
            behaviors.add_method(http, n, method(|lua, _, _, args| {
                // a number or boolean is read as its text (12.5, true; measured); nil is a missing argument
                let text = match args.get(1) {
                    LuaValue::String(text) => text.as_bytes().to_vec(),
                    LuaValue::Nil => return raise("Argument 1 missing or nil"),
                    number @ (LuaValue::Number(_) | LuaValue::Integer(_)) => lua.coerce_string(number.clone())?.map(|text| text.as_bytes().to_vec()).unwrap_or_default(),
                    LuaValue::Boolean(flag) => if *flag { b"true".to_vec() } else { b"false".to_vec() },
                    // a table, array or dictionary, decodes to a new empty table (json_limits.luau)
                    LuaValue::Table(_) => return Ok(values([LuaValue::Table(lua.create_table()?)])),
                    _ => return raise("Can't parse JSON"),
                };
                match json::decode(lua, &text) {
                    Ok(value) => Ok(values([value])),
                    Err(message) => raise(message),
                }
            }));
        }
        for member in ["GetAsync", "PostAsync", "RequestAsync"] {
            if let Some(n) = name(&reflection, member) {
                behaviors.add_method(http, n, method(|_, _, _, _| raise("Http requests are not enabled. Enable via game settings")));
            }
        }
        if let Some(n) = name(&reflection, "UrlEncode") {
            behaviors.add_method(http, n, method(|lua, _, _, args| {
                let text = args.string(1).map(|s| s.as_bytes().to_vec()).unwrap_or_default();
                let mut encoded = String::new();
                for byte in text {
                    // (`~` is encoded too: "%7E", measured in a playing client, Studio 0.744)
                    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                        encoded.push(byte as char);
                    } else {
                        encoded.push_str(&format!("%{byte:02X}"));
                    }
                }
                Ok(values([string_value(lua, &encoded)?]))
            }));
        }
    }
    drop(behaviors);
    engine.clear_member_cache();
}

/// Tables JSONEncode nests at most. Each level is native stack in the encoder (this copy, mlua's serde, serde_json): a
/// 1 MiB stack, a library host's main thread, held about 700 before it overflowed and ended the process. Roblox's limit is
/// not measured (a Studio probe of nesting up to 10,000 and of a cycle, on 2026-10-30, closed Studio); 256 is ree's.
const JSON_DEPTH: usize = 256;

/// Services.JsonSafe: engine values become JSON-encodable data as HttpService makes them (non-JSON types are dropped).
/// `path` holds the tables being copied: a table inside itself (that copy never ended, and overflowed the stack) and
/// nesting past [`JSON_DEPTH`] are the script's errors.
pub(crate) fn json_safe(lua: &Lua, value: &LuaValue, path: &mut Vec<*const std::ffi::c_void>) -> Result<LuaValue, Flow> {
    Ok(match value {
        LuaValue::Table(table) => {
            let pointer = table.to_pointer();
            if path.contains(&pointer) {
                // (Roblox's message as remembered, not measured: see JSON_DEPTH)
                return raise("tables cannot be cyclic");
            }
            if path.len() >= JSON_DEPTH {
                return raise(format!("tables nested more than {JSON_DEPTH} levels deep cannot be encoded"));
            }
            path.push(pointer);
            let result = lua.create_table()?;
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, item) = pair?;
                result.raw_set(key, json_safe(lua, &item, path)?)?;
            }
            path.pop();
            LuaValue::Table(result)
        }
        LuaValue::Number(_) | LuaValue::Integer(_) | LuaValue::String(_) | LuaValue::Boolean(_) => value.clone(),
        _ => LuaValue::Nil,
    })
}

/// FireFrameEvent: RunService's event, if a script ever created it, with immediate handlers.
fn fire_frame_event(lua: &Lua, engine: &Rc<Engine>, event: NameId, args: &[Value]) -> LuaResult<()> {
    let game = engine.world().game;
    let Some(run_service) = find_service(engine, game, "RunService") else { return Ok(()) };
    let signal = engine.state.borrow().dom.at(run_service).signals.as_ref().and_then(|s| s.event(event));
    if let Some(signal) = signal {
        fire_immediate_costing(lua, engine, signal, args, crate::cost::FRAME_HANDLER)?;
    }
    Ok(())
}

/// Script-visible GUI behaviours that need no layout (runtime/layout/install.luau's property rules:
/// TextScaled / TextWrapped, UICorner radii, Tween* methods, the static GuiState and TextBox
/// answers). The geometry parts (Absolute* / TextBounds getters, CanvasPosition clamping,
/// ApplyLayout, UIPageLayout navigation, the layout pass and its invalidation hooks), ContentText,
/// TextService's measurements and live input are a host layer's (crate::Layer).
pub fn install_gui_behaviors(engine: &Rc<Engine>) {
    let reflection = engine.reflection.clone();
    let mut behaviors = engine.behaviors.borrow_mut();
    let n = |name: &str| reflection.name_id(name);
    // (a host layer's TextService:GetTextSize, measuring with real fonts, replaces this one)
    crate::text_measure::install(engine, &mut behaviors);

    for class_name in ["TextLabel", "TextButton", "TextBox"] {
        let Some(class) = class(&reflection, class_name) else { continue };
        // (ContentText needs rich-text parsing: a host layer's)
        if let Some(name) = n("LocalizedText") {
            behaviors.add_getter(class, name, getter(|lua, engine, id| {
                let text = engine.state.borrow().dom.raw_get(id, K::Text as NameId).cloned().unwrap_or(Value::String(Str::default()));
                Ok(convert::to_lua(lua, engine, &text)?)
            }));
        }
        // Measured: assigning TextScaled = true also sets TextWrapped = true, and assigning
        // TextWrapped = false also clears TextScaled (TextScaled = false leaves TextWrapped alone).
        if let (Some(scaled), Some(wrapped)) = (n("TextScaled"), n("TextWrapped")) {
            behaviors.add_setter(class, scaled, Rc::new(move |lua, engine, id, value| {
                let class = engine.state.borrow().dom.at(id).class;
                let on = match engine.reflection.class_property(class, scaled).cloned() {
                    Some(info) => convert::coerce(lua, engine, &info, &value)?,
                    None => Value::Bool(matches!(value, LuaValue::Boolean(true))),
                };
                let flag = matches!(on, Value::Bool(true));
                set_property(engine, id, scaled, on);
                if flag {
                    set_property(engine, id, wrapped, Value::Bool(true));
                }
                Ok(())
            }));
            behaviors.add_setter(class, wrapped, Rc::new(move |lua, engine, id, value| {
                let class = engine.state.borrow().dom.at(id).class;
                let on = match engine.reflection.class_property(class, wrapped).cloned() {
                    Some(info) => convert::coerce(lua, engine, &info, &value)?,
                    None => Value::Bool(matches!(value, LuaValue::Boolean(true))),
                };
                let flag = matches!(on, Value::Bool(true));
                set_property(engine, id, wrapped, on);
                if !flag {
                    set_property(engine, id, scaled, Value::Bool(false));
                }
                Ok(())
            }));
        }
    }
    if let Some(text_box) = class(&reflection, "TextBox") {
        for member in ["CaptureFocus", "ReleaseFocus"] {
            if let Some(name) = n(member) {
                behaviors.add_method(text_box, name, method(|_, _, _, _| Ok(none())));
            }
        }
        if let Some(name) = n("IsFocused") {
            behaviors.add_method(text_box, name, constant(|_| Ok(LuaValue::Boolean(false))));
        }
    }
    for class_name in ["ImageLabel", "ImageButton"] {
        if let (Some(class), Some(name)) = (class(&reflection, class_name), n("IsLoaded")) {
            behaviors.add_getter(class, name, getter(|_, _, _| Ok(LuaValue::Boolean(true))));
        }
    }
    if let Some(gui_object) = class(&reflection, "GuiObject") {
        if let Some(name) = n("GuiState") {
            behaviors.add_getter(gui_object, name, getter(|lua, engine, _| Ok(enum_item(lua, engine, "GuiState", "Idle")?)));
        }
        // Tween* methods apply the end values at once and report Completed at the next resumption
        let tweens: [(&str, &[&str]); 3] = [
            ("TweenPosition", &["Position"]),
            ("TweenSize", &["Size"]),
            ("TweenSizeAndPosition", &["Size", "Position"]),
        ];
        for (member, fields) in tweens {
            let Some(name) = n(member) else { continue };
            let fields: Vec<NameId> = fields.iter().filter_map(|f| n(f)).collect();
            behaviors.add_method(gui_object, name, method(move |lua, engine, id, args| {
                let class = engine.state.borrow().dom.at(id).class;
                for (index, field) in fields.iter().enumerate() {
                    if let Some(info) = engine.reflection.class_property(class, *field).cloned() {
                        let value = convert::coerce(lua, engine, &info, args.get(1 + index))?;
                        set_property(engine, id, *field, value);
                    }
                }
                let callback_index = 1 + fields.len() + 4;
                if let LuaValue::Function(callback) = args.get(callback_index).clone() {
                    let thread = lua.create_thread(callback)?;
                    let status = engine.reflection.enum_item("TweenStatus", "Completed").map_or(Value::Nil, Value::EnumItem);
                    super::lua::instance::defer_thread(engine, thread, vec![status]);
                }
                Ok(values([LuaValue::Boolean(true)]))
            }));
        }
    }
    // (ApplyLayout, UIPageLayout navigation, the Absolute* / TextBounds getters and the
    // CanvasPosition setter are a layout layer's)
    // Measured: CornerRadius writes all four per-corner radii and reads back TopLeftRadius.
    if let (Some(corner), Some(radius)) = (class(&reflection, "UICorner"), n("CornerRadius")) {
        behaviors.add_getter(corner, radius, getter(|lua, engine, id| {
            let value = engine.state.borrow().dom.raw_get(id, K::TopLeftRadius as NameId).cloned().unwrap_or(Value::Nil);
            Ok(convert::to_lua(lua, engine, &value)?)
        }));
        behaviors.add_setter(corner, radius, Rc::new(move |_, engine, id, value| {
            let converted = convert::from_lua(&value);
            if !matches!(converted, Value::UDim(_)) {
                return raise(format!(
                    "Unable to assign property CornerRadius. UDim expected, got {}",
                    super::lua::typeof_value(engine, &value)
                ));
            }
            for name in [K::TopLeftRadius, K::TopRightRadius, K::BottomRightRadius, K::BottomLeftRadius] {
                set_property(engine, id, name as NameId, converted.clone());
            }
            let hooks = engine.hooks.borrow();
            let mut state = engine.state.borrow_mut();
            core::notify_changed(&mut state, &engine.reflection, &hooks, id, radius, None);
            Ok(())
        }));
    }
    drop(behaviors);
    engine.clear_member_cache();
}

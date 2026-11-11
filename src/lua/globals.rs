//! Engine.Start (port of `runtime/engine/init.luau`) and the shared global environment scripts
//! run in (port of `runtime/engine/globals.luau`).

use std::rc::Rc;

use mlua::prelude::*;

use super::{
    actors, Args, Flow, LuaRefs, Prelude, enum_item_value, instance, instance_value, lock_metatable, modules, none, raise_plain,
    refs::{self, Hold}, types, values, wrapped,
};
use crate::{
    Engine, InstanceId, K, NameId, World,
    dom::flags,
    instance as core,
    scheduler::{self, TimerKind},
    services,
    value::{Str, Value},
};

const PRELUDE: &str = include_str!("prelude.luau");

/// Services Engine.Start creates, in its order.
const START_SERVICES: &[&str] = &[
    "ReplicatedStorage",
    "ReplicatedFirst",
    "StarterGui",
    "StarterPlayer",
    "Lighting",
    "CoreGui",
    "RunService",
    "TweenService",
    "UserInputService",
    "GuiService",
    "TextService",
    "HttpService",
    "SoundService",
    "CollectionService",
];

fn primitive<F>(lua: &Lua, engine: &Rc<Engine>, f: F) -> LuaResult<LuaFunction>
where
    F: Fn(&Lua, &Rc<Engine>, LuaMultiValue) -> LuaResult<LuaMultiValue> + 'static,
{
    let engine = engine.clone();
    lua.create_function(move |lua, args| f(lua, &engine, args))
}

/// The VM a prelude call names: nil for the main VM, else an Actor's key (modules::vm_key).
fn vm_of(value: &LuaValue) -> modules::Vm {
    match value {
        LuaValue::Integer(key) => Some(*key as u64),
        LuaValue::Number(key) => Some(*key as u64),
        _ => None,
    }
}

/// The thread task.spawn / defer / delay (`call`) runs: a thread as it is, a function in a new thread. Roblox's errors
/// (measured in a playing client, Studio 0.744, tools/parity/probes/task_order.luau; raised without a position):
/// `invalid argument #1 to 'spawn' (function or thread expected)` (#2 for delay), and for a thread an earlier
/// task.defer or task.delay queued that has not resumed yet, `Cannot call task.spawn on a thread that is already
/// 'deferred' in the task library`.
fn to_thread(lua: &Lua, engine: &Engine, value: &LuaValue, index: usize, call: &str) -> Result<LuaThread, Flow> {
    match value {
        LuaValue::Thread(thread) => {
            if let Some(state) = engine.state.borrow().sched.task_state(thread) {
                return raise_plain(format!("Cannot call task.{call} on a thread that is already '{state}' in the task library"));
            }
            Ok(thread.clone())
        }
        LuaValue::Function(function) => Ok(lua.create_thread(function.clone())?),
        _ => raise_plain(format!("invalid argument #{index} to '{call}' (function or thread expected)")),
    }
}

/// The thread the legacy spawn / delay run (their errors were not measured).
fn legacy_thread(lua: &Lua, value: &LuaValue) -> Result<LuaThread, Flow> {
    match value {
        LuaValue::Thread(thread) => Ok(thread.clone()),
        LuaValue::Function(function) => Ok(lua.create_thread(function.clone())?),
        _ => raise_plain("invalid argument #1 (function or thread expected)"),
    }
}

/// task.delay's and task.wait's seconds: nil is none; a number, or a string Luau reads as one, goes; anything else is
/// `invalid argument #1 to 'delay' (number expected, got string)` (measured likewise; raised without a position).
fn task_seconds(lua: &Lua, engine: &Engine, value: &LuaValue, call: &str) -> Result<Option<f64>, Flow> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Number(number) => Ok(Some(*number)),
        LuaValue::Integer(number) => Ok(Some(*number as f64)),
        LuaValue::String(_) => match lua.coerce_number(value.clone())? {
            Some(number) => Ok(Some(number)),
            None => raise_plain(format!("invalid argument #1 to '{call}' (number expected, got string)")),
        },
        other => raise_plain(format!("invalid argument #1 to '{call}' (number expected, got {})", super::typeof_value(engine, other))),
    }
}

/// A function handed to task.spawn / defer / delay gets a thread of its own (cost::THREAD); a thread is used as it is.
fn charge_thread(lua: &Lua, engine: &Engine, value: &LuaValue, call: &str) {
    // (a thread handed over costs a client what a function's new thread does: cost.rs THREAD)
    if matches!(value, LuaValue::Function(_) | LuaValue::Thread(_)) {
        let state = engine.state.borrow();
        state.sched.charge(crate::cost::THREAD);
        if crate::boundary::active() && !state.sched.is_muted() {
            crate::boundary::enter(lua.current_thread().state());
            crate::boundary::record_named(crate::boundary::Kind::Method, "task", call, crate::cost::THREAD);
        }
    }
}

fn load_prelude(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let r = lua.create_table()?;
    r.set("SIGNAL", super::signal_marker())?;
    // (the prelude's thread-making functions charge a script as task.spawn does)
    r.set("ThreadCost", crate::cost::THREAD)?;
    r.set("SharedIncrementCost", crate::cost::SHARED_INCREMENT)?;
    r.set("VectorLerpCost", crate::cost::VECTOR_LERP)?;
    r.set("VectorFuzzyEqCost", crate::cost::VECTOR_FUZZY_EQ)?;
    // Vector3:Angle's atan2 in floats (the C runtime's atan2f), as a playing client takes it (Studio 0.744,
    // tools/parity/probes/cframe_order.luau and datatypes.luau: math.atan2's doubles differ in the last digits)
    r.set("Atan2f", lua.create_function(|_, (y, x): (f64, f64)| Ok(f64::from((y as f32).atan2(x as f32))))?)?;
    let e = engine.clone();
    r.set(
        "Charge",
        lua.create_function(move |_, seconds: f64| {
            let state = e.state.borrow();
            state.sched.charge(seconds);
            if !state.sched.is_muted() {
                crate::boundary::add_cost(seconds);
            }
            Ok(())
        })?,
    )?;
    r.set(
        "EnumItemName",
        lua.create_function(|lua, value: LuaValue| {
            let engine = lua.app_data_ref::<Rc<Engine>>().expect("engine").clone();
            Ok(match &value {
                LuaValue::UserData(ud) => match ud.borrow::<types::LEnumItem>() {
                    Ok(item) => Some(engine.reflection.enum_item_info(item.0).name.to_string()),
                    Err(_) => None,
                },
                _ => None,
            })
        })?,
    )?;
    r.set(
        "EnumItemType",
        lua.create_function(|lua, value: LuaValue| {
            let engine = lua.app_data_ref::<Rc<Engine>>().expect("engine").clone();
            Ok(match &value {
                LuaValue::UserData(ud) => match ud.borrow::<types::LEnumItem>() {
                    Ok(item) => Some(engine.reflection.enum_item_type_name(item.0).to_string()),
                    Err(_) => None,
                },
                _ => None,
            })
        })?,
    )?;
    r.set(
        "GetEnumItem",
        lua.create_function(|lua, (enum_name, item_name): (String, String)| {
            let engine = lua.app_data_ref::<Rc<Engine>>().expect("engine").clone();
            match engine.reflection.enum_item(&enum_name, &item_name) {
                Some(item) => enum_item_value(lua, &engine, item),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    r.set(
        "Output",
        primitive(lua, engine, |_, engine, args| {
            let args = Args::new(args);
            let kind = match args.string(0).map(|s| s.to_string_lossy()).as_deref() {
                Some("warn") => "warn",
                Some("error") => "error",
                _ => "print",
            };
            let message = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
            engine.output(kind, message, None);
            Ok(none())
        })?,
    )?;
    // task library primitives (scheduler.luau Task)
    r.set(
        "TaskSpawn",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                let thread = to_thread(lua, engine, args.get(0), 1, "spawn")?;
                refs::admit(engine, Hold::Work, "task.spawn")?;
                charge_thread(lua, engine, args.get(0), "spawn");
                scheduler::resume(lua, engine, &thread, args.rest(1));
                Ok(values([LuaValue::Thread(thread)]))
            })())
        })?,
    )?;
    r.set(
        "TaskDefer",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                let thread = to_thread(lua, engine, args.get(0), 1, "defer")?;
                refs::admit(engine, Hold::Work, "task.defer")?;
                charge_thread(lua, engine, args.get(0), "defer");
                let rest: Vec<Value> = args.0.iter().skip(1).map(super::convert::from_lua).collect();
                if engine.state.borrow().sched.can_defer_event() {
                    let mut state = engine.state.borrow_mut();
                    state.sched.defer_task(scheduler::JobTarget::Thread(thread.clone()), rest);
                    state.sched.set_task_state(&thread, "deferred");
                } else {
                    // Past the cut of a chain the thread is not queued; the call returns and Roblox logs an error
                    // (measured in a playing client, Studio 0.744: pcall(task.defer, f) is ok, the chain stops)
                    let message = format!("Maximum re-entrancy depth ({}) exceeded calling task.defer", scheduler::MAX_REENTRANCY);
                    engine.output("error", message, None);
                }
                Ok(values([LuaValue::Thread(thread)]))
            })())
        })?,
    )?;
    r.set(
        "TaskDelay",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                let delay = task_seconds(lua, engine, args.get(0), "delay")?.unwrap_or(0.0);
                let thread = to_thread(lua, engine, args.get(1), 2, "delay")?;
                refs::admit(engine, Hold::Work, "task.delay")?;
                charge_thread(lua, engine, args.get(1), "delay");
                // (its timer, a thread or a function's: cost.rs DELAYED)
                engine.state.borrow().sched.charge(crate::cost::DELAYED);
                crate::boundary::add_cost(crate::cost::DELAYED);
                let rest: Vec<Value> = args.0.iter().skip(2).map(super::convert::from_lua).collect();
                let mut state = engine.state.borrow_mut();
                state.sched.schedule_task(TimerKind::Delay, delay, thread.clone(), rest);
                state.sched.set_task_state(&thread, "delayed");
                Ok(values([LuaValue::Thread(thread)]))
            })())
        })?,
    )?;
    // (the prelude's Pass raises a refusal and yields after: no values, the thread is registered)
    r.set(
        "ScheduleWait",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                let delay = task_seconds(lua, engine, args.get(0), "wait")?.unwrap_or(0.0);
                refs::admit(engine, Hold::Wait, "task.wait")?;
                engine.state.borrow_mut().sched.schedule_task(TimerKind::Wait, delay, lua.current_thread(), Vec::new());
                Ok(none())
            })())
        })?,
    )?;
    r.set(
        "ScheduleLegacyWait",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                let duration = args.number(0);
                refs::admit(engine, Hold::Wait, "wait")?;
                engine.state.borrow_mut().sched.schedule_legacy(TimerKind::LegacyWait, duration, lua.current_thread());
                Ok(none())
            })())
        })?,
    )?;
    r.set(
        "TaskCancel",
        primitive(lua, engine, |lua, engine, args| {
            super::encode(lua, (|| {
                let args = Args::new(args);
                // (the running thread and one that resumed it raise `cannot cancel thread` in the prelude's task.cancel)
                let LuaValue::Thread(thread) = args.get(0) else {
                    let got = super::typeof_value(engine, args.get(0));
                    return raise_plain(format!("invalid argument #1 to 'cancel' (thread expected, got {got})"));
                };
                if thread.status() == LuaThreadStatus::Resumable {
                    let close = engine.lua.borrow().coroutine_close.clone();
                    if let Some(close) = close {
                        let _ = close.call::<LuaMultiValue>(thread.clone());
                    }
                }
                // (after the close: only a thread that is still alive is listed)
                let mut state = engine.state.borrow_mut();
                state.sched.cancel(thread);
                state.sched.clear_task_state(thread);
                Ok(none())
            })())
        })?,
    )?;
    r.set(
        "RequireBegin",
        primitive(lua, engine, |lua, engine, args| {
            let args = Args::new(args);
            modules::require_begin(lua, engine, (args.get(0).clone(), args.get(1).clone(), vm_of(args.get(2))))
        })?,
    )?;
    // task.synchronize / task.desynchronize of a parallel handler's thread: whether to yield (actors.rs)
    r.set(
        "ParallelPark",
        primitive(lua, engine, |lua, engine, args| {
            let args = Args::new(args);
            let to_serial = matches!(args.get(0), LuaValue::Boolean(true));
            Ok(values([LuaValue::Boolean(actors::park(lua, engine, to_serial))]))
        })?,
    )?;
    r.set(
        "RequireFinish",
        primitive(lua, engine, |lua, engine, args| {
            let args = Args::new(args);
            let LuaValue::Table(results) = args.get(1).clone() else {
                return Err(LuaError::runtime("RequireFinish: results expected"));
            };
            modules::require_finish(lua, engine, (args.get(0).clone(), results, vm_of(args.get(2))))
        })?,
    )?;

    // Vector3 is the native vector type, as in Roblox (vector3.luau, loaded by the prelude): vectors
    // get their members through the type's metatable, and Lune's CFrame / Ray / Region3 convert,
    // through their own metatables reached here (they are locked for scripts)
    r.set(
        "Vector3",
        crate::cost::engine_luau(
            lua,
            lua.load(include_str!("vector3.luau")).set_name("=rur.vector3").set_compiler(modules::engine_compiler()).into_function()?,
        )?,
    )?;
    r.set(
        "RawMetatable",
        lua.create_function(|lua, value: LuaAnyUserData| unsafe {
            lua.exec_raw::<LuaValue>(value, |state| {
                if mlua::ffi::lua_getmetatable(state, -1) == 0 {
                    mlua::ffi::lua_pushnil(state);
                }
                mlua::ffi::lua_remove(state, -2);
            })
        })?,
    )?;
    r.set(
        "SetVectorMetatable",
        lua.create_function(|lua, metatable: LuaTable| {
            lua.set_type_metatable::<mlua::Vector>(Some(metatable));
            Ok(())
        })?,
    )?;

    let roblox = lune_roblox::module(lua.clone())?;
    let prelude = lua.load(PRELUDE).set_name("=rur.prelude").set_compiler(modules::engine_compiler()).into_function()?;
    let exports: LuaTable = crate::cost::engine_luau(lua, prelude)?.call((r, roblox))?;
    let prelude = Prelude {
        wrap: exports.get("Wrap")?,
        // typeof as a C function over the same registry (lua/raw.rs)
        typeof_fn: super::raw::typeof_function(lua, exports.get("TypeRegistry")?)?,
        register: exports.get("Register")?,
        traceback: exports.get("Traceback")?,
        exports,
    };
    let globals = lua.globals();
    let coroutine: LuaTable = globals.get("coroutine")?;
    let math: LuaTable = globals.get("math")?;
    let mut refs = engine.lua.borrow_mut();
    refs.coroutine_close = Some(coroutine.get("close")?);
    refs.randomseed = Some(math.get("randomseed")?);
    refs.prelude = Some(Rc::new(prelude));
    Ok(())
}

/// Hooks.MissingChild (edit mode): the running client's PlayerGui a script waits for or indexes
/// is created on first use, and PlayerScripts children are copied from StarterPlayerScripts as a
/// starting client's would be. FindFirstChild never asks. Indexing (`__index`) cannot yield, so
/// the client gui mount (rur.json clientGui) waits for a lookup that may: see
/// [`missing_child_yieldable`].
pub fn missing_child(lua: &Lua, engine: &Rc<Engine>, parent: InstanceId, name: &[u8]) -> LuaResult<Option<InstanceId>> {
    missing_child_in(lua, engine, parent, name, false)
}

/// Hooks.MissingChild from a lookup that may yield (WaitForChild): a pending client gui mount
/// (init.luau MountClient) runs first. (Added for the services port: the one caller is
/// lua/instance.rs wait_for_child.)
pub fn missing_child_yieldable(lua: &Lua, engine: &Rc<Engine>, parent: InstanceId, name: &[u8]) -> LuaResult<Option<InstanceId>> {
    missing_child_in(lua, engine, parent, name, true)
}

/// init.luau MountClient: Engine.ClientGuiMount once, from a yieldable lookup. It runs as a
/// plain call here (a Luau yield inside it cannot cross the Rust frame: the mount then fails with
/// a warning where the Luau runtime would wait).
fn mount_client(lua: &Lua, engine: &Rc<Engine>, yieldable: bool) -> LuaResult<()> {
    if !yieldable {
        return Ok(());
    }
    let mount = {
        let mut state = engine.state.borrow_mut();
        if !state.project.client_mount_pending {
            return Ok(());
        }
        state.project.client_mount_pending = false;
        state.project.client_gui_mount.clone()
    };
    if let Some(mount) = mount {
        if let Err(error) = mount.call::<()>(()) {
            engine.warn(format!("rur.json clientGui: {}", scheduler::error_message(&error)));
        }
        let _ = lua;
    }
    Ok(())
}

/// StarterGui's child `name` copied into `gui` (FindFirstChild on StarterGui: Studio-only children
/// materialize first, so a gui that exists only in the place is copied too).
fn copy_starter_gui(engine: &Rc<Engine>, gui: InstanceId, name: &[u8]) -> Option<InstanceId> {
    let starter = {
        let state = engine.state.borrow();
        state.dom.find_child_of_class(engine.world().game, engine.reflection.class_id("StarterGui").unwrap_or(u32::MAX))
    };
    let template = starter.and_then(|s| instance::find_first_child(engine, s, Some(name), false))?;
    let copy = instance::clone(engine, template)?;
    instance::set_parent(engine, copy, Some(gui)).ok();
    Some(copy)
}

fn missing_child_in(lua: &Lua, engine: &Rc<Engine>, parent: InstanceId, name: &[u8], yieldable: bool) -> LuaResult<Option<InstanceId>> {
    if !engine.options.edit_mode() {
        // A playing client's PlayerGui holds StarterGui's copies (Roblox copies them when the local
        // player spawns): copied when first looked up, as the edit-mode PlayerGui below. What the
        // client's own boot builds there (rur.json clientGui) is not mounted: that is the whole client.
        let Some(player) = engine.world().local_player else { return Ok(None) };
        let is_player_gui = {
            let state = engine.state.borrow();
            let instance = state.dom.at(parent);
            instance.parent == Some(player) && Some(instance.class) == engine.reflection.class_id("PlayerGui")
        };
        return Ok(if is_player_gui { copy_starter_gui(engine, parent, name) } else { None });
    }
    let Some(player) = engine.world().local_player else { return Ok(None) };
    let reflection = &engine.reflection;
    if parent == player && name == b"PlayerGui" {
        let existing = engine.state.borrow().project.client_gui;
        if let Some(gui) = existing {
            mount_client(lua, engine, yieldable)?;
            return Ok(Some(gui));
        }
        let gui = {
            let hooks = engine.hooks.borrow();
            let mut state = engine.state.borrow_mut();
            let class = reflection.class_id("PlayerGui").expect("PlayerGui");
            let gui = core::create_named(&mut state, reflection, class, "PlayerGui");
            let _ = core::set_parent(&mut state, reflection, &hooks, gui, Some(player));
            state.project.client_gui = Some(gui);
            state.project.client_mount_pending = state.project.client_gui_mount.is_some();
            gui
        };
        engine.warn(
            "edit mode has no PlayerGui; created one as a play test would (in Studio this story previews only while playing)",
        );
        mount_client(lua, engine, yieldable)?;
        return Ok(Some(gui));
    }
    let parent_class = engine.state.borrow().dom.at(parent).class;
    let player_scripts = reflection.class_id("PlayerScripts");
    if Some(parent_class) == player_scripts && engine.state.borrow().dom.at(parent).parent == Some(player) {
        // FindFirstChild, as the reference does: materializes Studio-only children first
        let starter = {
            let state = engine.state.borrow();
            state.dom.find_child_of_class(engine.world().game, reflection.class_id("StarterPlayer").unwrap_or(u32::MAX))
        };
        let scripts = starter.and_then(|s| instance::find_first_child(engine, s, Some(b"StarterPlayerScripts"), false));
        let template = scripts.and_then(|s| instance::find_first_child(engine, s, Some(name), false));
        if let Some(template) = template {
            if let Some(copy) = instance::clone(engine, template) {
                instance::set_parent(engine, copy, Some(parent)).ok();
                return Ok(Some(copy));
            }
        }
        return Ok(None);
    }
    let client_gui = engine.state.borrow().project.client_gui;
    if client_gui == Some(parent) {
        mount_client(lua, engine, yieldable)?;
        let built = instance::find_first_child(engine, parent, Some(name), false);
        if let Some(built) = built {
            return Ok(Some(built));
        }
        // StarterGui:FindFirstChild(Name), as the reference does: it materializes StarterGui's
        // Studio-only children first, so a gui that exists only in the place (the game place's
        // StarterGui.Windows) is copied into PlayerGui. A raw child lookup missed it and
        // PlayerGui:WaitForChild("Windows") yielded forever.
        if let Some(copy) = copy_starter_gui(engine, parent, name) {
            return Ok(Some(copy));
        }
    }
    Ok(None)
}

/// Engine.Start: reflection-backed types, behaviours, the DataModel with its services, the local
/// player, camera and mouse, and the shared script environment.
pub fn start(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    lua.set_app_data(engine.clone());
    lua.set_compiler(modules::compiler());
    // The string metatable is locked: getmetatable("") is a string in a playing client (measured, Studio 0.744:
    // tools/parity/probes/values.luau; the text is the lock text Roblox's other metatables give, not measured here)
    lua.load(r#"getmetatable("").__metatable = "The metatable is locked""#).set_name("=ree.strings").exec()?;
    // Luau native code generation (feature `jit`; a WebAssembly build has none: an interpreting VM),
    // unless the VM interprets everything (modules::interpret_all)
    #[cfg(feature = "jit")]
    lua.enable_jit(!modules::interprets_all(engine));
    // Instances are raw tagged userdata with C metamethods (lua/raw.rs)
    super::raw::install(lua, engine)?;
    // Connection handlers live in one Luau table (signal.rs)
    engine.state.borrow_mut().signals.hold_in(lua.create_table()?);
    // What the VM holds for scripts has a ceiling: past it a script errors, as for an exhausted resource (refs.rs)
    engine.lua.borrow_mut().gauge = refs::Gauge::find(lua);
    refs::quiet_panics();
    load_prelude(lua, engine)?;
    types::register(lua, engine)?;
    instance::register(lua, engine)?;
    services::install(engine);
    crate::derived::install(engine);
    // The 3D world's views, queries and reported calls (engine::world; after the derived views)
    crate::world::install(engine);
    services::install_gui_behaviors(engine);
    // The host's layers (roblox-ui-renderer: text measurement, layout, input), in their order
    for layer in engine.layers() {
        layer.install(engine);
        engine.clear_member_cache();
    }

    let reflection = engine.reflection.clone();
    let options = engine.options.clone();
    let class = |name: &str| reflection.class_id(name).unwrap_or_else(|| panic!("class {name}"));

    let game = core::create_named(&mut engine.state.borrow_mut(), &reflection, class("DataModel"), &options.place_name);
    engine.state.borrow_mut().collection.game = Some(game);
    // A provisional world so services can be reached while the rest is built
    engine.set_world(World { game, workspace: game, camera: game, local_player: None });
    let workspace = services::get_service(engine, game, "Workspace").map_err(flow_error)?;
    let camera = {
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        let camera = core::create_named(&mut state, &reflection, class("Camera"), "Camera");
        core::set_parent(&mut state, &reflection, &hooks, camera, Some(workspace)).map_err(|e| LuaError::runtime(e.message))?;
        core::raw_set_silent(&mut state, &reflection, workspace, K::CurrentCamera as NameId, Value::Instance(camera));
        camera
    };
    engine.set_world(World { game, workspace, camera, local_player: None });
    // The place file's streaming (scripts cannot set it): Workspace.StreamingEnabled and its radius
    if let Some(radius) = options.streaming_target_radius {
        let mut state = engine.state.borrow_mut();
        for (name, value) in [("StreamingEnabled", Value::Bool(true)), ("StreamingTargetRadius", Value::Number(radius))] {
            if let Some(name) = reflection.name_id(name) {
                core::raw_set_silent(&mut state, &reflection, workspace, name, value);
            }
        }
    }

    let players = services::get_service(engine, game, "Players").map_err(flow_error)?;
    let mut local_player = None;
    if let Some(player_name) = &options.local_player_name {
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        let player = core::create_named(&mut state, &reflection, class("Player"), player_name);
        let display = options.local_player_display_name.clone().unwrap_or_else(|| player_name.clone());
        core::raw_set_silent(&mut state, &reflection, player, K::DisplayName as NameId, Value::String(Str::from(display)));
        core::raw_set_silent(&mut state, &reflection, player, K::UserId as NameId, Value::Number(options.local_player_user_id));
        // Measured: in edit mode the local player has PlayerScripts and Backpack but no PlayerGui
        let mut containers = vec!["PlayerScripts", "Backpack"];
        if !options.edit_mode() {
            containers.insert(0, "PlayerGui");
        }
        for name in containers {
            let child = core::create_named(&mut state, &reflection, class(name), name);
            core::set_parent(&mut state, &reflection, &hooks, child, Some(player)).map_err(|e| LuaError::runtime(e.message))?;
        }
        core::set_parent(&mut state, &reflection, &hooks, player, Some(players)).map_err(|e| LuaError::runtime(e.message))?;
        local_player = Some(player);
    }
    engine.set_world(World { game, workspace, camera, local_player });

    let mouse = make_mouse(lua, engine)?;
    for name in START_SERVICES {
        services::get_service(engine, game, name).map_err(flow_error)?;
    }
    instance::lock(lua, engine, game)?;

    let shared = build_shared(lua, engine)?;
    shared.raw_set("__rurMouse", mouse)?;
    engine.lua.borrow_mut().shared = Some(shared);
    super::stubs::load(lua, engine)?;

    // Scheduler.Configure: math.random starts from the run's seed
    randomseed(engine)?;
    Ok(())
}

fn flow_error(flow: Flow) -> LuaError {
    match flow {
        Flow::Error(error) => LuaError::runtime(error.message),
        Flow::Fail(error) => error,
        Flow::Yield => LuaError::runtime("unexpected yield"),
    }
}

/// math.randomseed(Scheduler.RandomSeed)
pub fn randomseed(engine: &Engine) -> LuaResult<()> {
    let seed = engine.state.borrow().sched.random_seed;
    let function = engine.lua.borrow().randomseed.clone();
    if let Some(function) = function {
        function.call::<()>(seed)?;
    }
    Ok(())
}

/// Engine.JoinLocalPlayer: a joining player takes these from StarterPlayer (after the project,
/// which may set them, is mounted).
pub fn join_local_player(engine: &Rc<Engine>) {
    let Some(player) = engine.world().local_player else { return };
    let game = engine.world().game;
    let Ok(starter) = services::get_service(engine, game, "StarterPlayer") else { return };
    let reflection = &engine.reflection;
    let pairs = [
        ("CameraMaxZoomDistance", "CameraMaxZoomDistance"),
        ("CameraMinZoomDistance", "CameraMinZoomDistance"),
        ("CameraMode", "CameraMode"),
        ("DevCameraOcclusionMode", "DevCameraOcclusionMode"),
        ("DevComputerCameraMode", "DevComputerCameraMovementMode"),
        ("DevComputerMovementMode", "DevComputerMovementMode"),
        ("DevTouchCameraMode", "DevTouchCameraMovementMode"),
        ("DevTouchMovementMode", "DevTouchMovementMode"),
        ("DevEnableMouseLock", "EnableMouseLockOption"),
        ("HealthDisplayDistance", "HealthDisplayDistance"),
        ("NameDisplayDistance", "NameDisplayDistance"),
        ("AutoJumpEnabled", "AutoJumpEnabled"),
    ];
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    for (player_name, starter_name) in pairs {
        let (Some(player_prop), Some(starter_prop)) = (reflection.name_id(player_name), reflection.name_id(starter_name)) else {
            continue;
        };
        let value = state.dom.raw_get(starter, starter_prop).cloned().unwrap_or(Value::Nil);
        core::raw_set(&mut state, reflection, &hooks, player, player_prop, value);
    }
}

fn make_mouse(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<LuaTable> {
    let mouse = lua.create_table()?;
    mouse.set("X", -1000)?;
    mouse.set("Y", -1000)?;
    let roblox_cframe: LuaTable = engine.prelude().exports.get::<LuaTable>("Datatypes")?.get("CFrame")?;
    let new_cframe: LuaFunction = roblox_cframe.get("new")?;
    mouse.set("Hit", new_cframe.call::<LuaValue>(())?)?;
    mouse.set("Origin", new_cframe.call::<LuaValue>(())?)?;
    mouse.set("Icon", "")?;
    mouse.set("ViewSizeX", 0)?;
    mouse.set("ViewSizeY", 0)?;
    let datatypes: LuaTable = engine.prelude().exports.get("Datatypes")?;
    let ray: LuaTable = datatypes.get("Ray")?;
    let vector3: LuaTable = datatypes.get("Vector3")?;
    let zero: LuaValue = vector3.get("zero")?;
    let down: LuaValue = vector3.get::<LuaFunction>("new")?.call((0, 0, -1))?;
    mouse.set("UnitRay", ray.get::<LuaFunction>("new")?.call::<LuaValue>((zero, down))?)?;
    for name in ["Button1Down", "Button1Up", "Button2Down", "Button2Up", "Move", "Idle", "WheelForward", "WheelBackward", "KeyDown", "KeyUp"] {
        let signal = types::new_signal(engine, name);
        mouse.set(name, super::signal_value(lua, engine, signal)?)?;
    }
    engine.prelude().register.call::<()>((mouse.clone(), "Instance"))?;
    Ok(mouse)
}

fn build_shared(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<LuaTable> {
    let prelude = engine.prelude();
    let exports = &prelude.exports;
    let globals = lua.globals();
    let shared = lua.create_table()?;
    for name in [
        "assert", "error", "getmetatable", "ipairs", "next", "pairs", "pcall", "rawequal", "rawget", "rawlen", "rawset",
        "select", "setmetatable", "tonumber", "tostring", "type", "xpcall", "gcinfo", "newproxy", "getfenv", "setfenv",
        "string", "table", "math", "bit32", "buffer", "vector",
    ] {
        shared.raw_set(name, globals.get::<LuaValue>(name)?)?;
    }
    let table: LuaTable = globals.get("table")?;
    shared.raw_set("unpack", table.get::<LuaValue>("unpack")?)?;
    shared.raw_set("ypcall", globals.get::<LuaValue>("pcall")?)?;
    let gcinfo: LuaFunction = globals.get("gcinfo")?;
    shared.raw_set(
        "collectgarbage",
        lua.create_function(move |_, option: Option<String>| {
            if option.as_deref() == Some("count") {
                return gcinfo.call::<LuaValue>(());
            }
            Ok(LuaValue::Integer(0))
        })?,
    )?;
    shared.raw_set("coroutine", exports.get::<LuaValue>("Coroutine")?)?;
    shared.raw_set("utf8", exports.get::<LuaValue>("Utf8")?)?;
    shared.raw_set("debug", exports.get::<LuaValue>("Debug")?)?;

    // Virtual clocks (scheduler.luau): os.clock() and elapsedTime() stand still within a frame
    // phase; time() and DistributedGameTime are 0 in edit mode; tick() is Unix time plus the
    // standard zone offset; the wall clock starts at the current whole hour.
    let os: LuaTable = exports.get("Os")?;
    let native_os: LuaTable = globals.get("os")?;
    let e = engine.clone();
    os.raw_set(
        "clock",
        lua.create_function(move |lua, ()| {
            let clock = e.state.borrow_mut().sched.clock();
            // (a profile counts the reads by line and the clock's advance between them: a loop that checks a budget reads it once an
            // iteration)
            if crate::profiler::attribution::on() {
                crate::profiler::attribution::clock_read(lua, clock);
            }
            Ok(clock)
        })?,
    )?;
    if std::env::var("RUR_REAL_CLOCK").as_deref() == Ok("1") {
        let start = std::time::Instant::now();
        os.raw_set("realclock", lua.create_function(move |_, ()| Ok(start.elapsed().as_secs_f64()))?)?;
    }
    if std::env::var("REE_WORK_COUNTERS").as_deref() == Ok("1") {
        // calibration (cost.rs, tools/cost-parity): the VM's work counters, { ops = {[opcode + 1] = n}, nativeops, allocated, ccalls, ... }
        let e = engine.clone();
        os.raw_set(
            "workcounters",
            lua.create_function(move |lua, ()| {
                let Some(pointer) = e.state.borrow().sched.vm_counters else { return Ok(LuaValue::Nil) };
                let counters = unsafe { &*pointer };
                let ops = lua.create_table()?;
                for (op, count) in counters.ops.iter().enumerate() {
                    if *count > 0 {
                        ops.raw_set(op + 1, *count as f64)?;
                    }
                }
                let nativeops = lua.create_table()?;
                for (op, count) in counters.nativeops.iter().enumerate() {
                    if *count > 0 {
                        nativeops.raw_set(op + 1, *count as f64)?;
                    }
                }
                let result = lua.create_table()?;
                result.raw_set("ops", ops)?;
                result.raw_set("nativeops", nativeops)?;
                result.raw_set("allocated", counters.allocated as f64)?;
                result.raw_set("ccalls", counters.ccalls as f64)?;
                result.raw_set("tmcalls", counters.tmcalls as f64)?;
                result.raw_set("hostcalls", counters.hostcalls as f64)?;
                result.raw_set("hostnanos", counters.hostnanos as f64)?;
                result.raw_set("objects", counters.objects as f64)?;
                result.raw_set("stored", counters.stored as f64)?;
                result.raw_set("rehashed", counters.rehashed as f64)?;
                result.raw_set("hostops", crate::cost::host_instructions(counters) as f64)?;
                for (key, value) in [
                    ("vectormagnitudes", counters.vectormagnitudes),
                    ("vectorunits", counters.vectorunits),
                    ("vectormethods", counters.vectormethods),
                    ("numtostr", counters.numtostr),
                    ("sortcompares", counters.sortcompares),
                    ("sortcalls", counters.sortcalls),
                    ("createitems", counters.createitems),
                    ("cloneitems", counters.cloneitems),
                    ("concatitems", counters.concatitems),
                    ("splitparts", counters.splitparts),
                    ("formats", counters.formats),
                    ("formatspecs", counters.formatspecs),
                    ("newstrings", counters.newstrings),
                    ("slowimports", counters.slowimports),
                    ("coldtables", counters.coldtables),
                    ("coldclosures", counters.coldclosures),
                    ("coldprotos", counters.coldprotos),
                    ("coldcode", counters.coldcode),
                    ("scanned", counters.hostops[crate::cost::SCANNED_SLOT]),
                    ("moved", counters.hostops[crate::cost::MOVED_SLOT]),
                    ("strbytes", counters.hostops[crate::cost::STRBYTES_SLOT]),
                    ("strscanned", counters.hostops[crate::cost::STRSCANNED_SLOT]),
                    ("matchsteps", counters.hostops[crate::cost::MATCHSTEPS_SLOT]),
                    ("bufbytes", counters.hostops[crate::cost::BUFBYTES_SLOT]),
                    ("allocbytes", counters.hostops[crate::cost::ALLOCBYTES_SLOT]),
                    ("libobjects", counters.hostops[crate::cost::LIBOBJECTS_SLOT]),
                    ("casebytes", counters.hostops[crate::cost::CASEBYTES_SLOT]),
                    ("utf8bytes", counters.hostops[crate::cost::UTF8BYTES_SLOT]),
                    ("utf8chars", counters.hostops[crate::cost::UTF8CHARS_SLOT]),
                    ("strchars", counters.hostops[crate::cost::STRCHARS_SLOT]),
                    ("bytevalues", counters.hostops[crate::cost::BYTEVALUES_SLOT]),
                    ("buffers", counters.hostops[crate::cost::BUFFERS_SLOT]),
                    ("matchclasses", counters.hostops[crate::cost::MATCHCLASSES_SLOT]),
                    ("hashrehashed", counters.hostops[crate::cost::HASHREHASHED_SLOT]),
                    ("largeblocks", counters.hostops[crate::cost::LARGEBLOCKS_SLOT]),
                    ("buffreshbytes", counters.hostops[crate::cost::BUFFRESH_SLOT]),
                    ("bufbigcopybytes", counters.hostops[crate::cost::BUFBIGCOPY_SLOT]),
                    ("coldcfuncs", counters.hostops[crate::cost::COLD_CFUNCS_SLOT]),
                    ("coldhostfuncs", counters.hostops[crate::cost::COLD_HOSTFUNCS_SLOT]),
                    ("rareprotos", counters.hostops[crate::cost::RARE_PROTOS_SLOT]),
                ] {
                    result.raw_set(key, value as f64)?;
                }
                // (the engine's own counts of what a script asked it for by size: JSON text and values)
                let json = crate::services::json::counts();
                result.raw_set("jsonbytes", json.bytes as f64)?;
                result.raw_set("jsonvalues", json.values as f64)?;
                Ok(LuaValue::Table(result))
            })?,
        )?;
    }
    // Luau's own os.time and os.date on the virtual clock; their errors are Luau's plain messages (`invalid argument #1
    // to 'time' (table expected, got number)`; Studio 0.744, tools/parity/probes/datetime.luau)
    let native_time: LuaFunction = native_os.get("time")?;
    let e = engine.clone();
    os.raw_set(
        "time",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            if !args.get(0).is_nil() {
                return match native_time.call::<LuaValue>(args.get(0).clone()) {
                    Ok(time) => Ok(values([time])),
                    Err(error) => raise_plain(scheduler::error_message(&error)),
                };
            }
            let millis = e.state.borrow_mut().sched.unix_millis();
            Ok(values([LuaValue::Number((millis / 1000.0).floor())]))
        })?,
    )?;
    let native_date: LuaFunction = native_os.get("date")?;
    let e = engine.clone();
    os.raw_set(
        "date",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            let time = match args.get(1) {
                LuaValue::Nil => LuaValue::Number((e.state.borrow_mut().sched.unix_millis() / 1000.0).floor()),
                time => time.clone(),
            };
            match native_date.call::<LuaValue>((args.get(0).clone(), time)) {
                Ok(date) => Ok(values([date])),
                Err(error) => raise_plain(scheduler::error_message(&error)),
            }
        })?,
    )?;
    shared.raw_set("os", os)?;
    // (a string in a playing client's and a server's scripts, measured: tools/parity/probes/errors.luau; "Luau" is Luau's own
    // base library value, the text is not measured)
    shared.raw_set("_VERSION", "Luau")?;

    let game = engine.world().game;
    let workspace = engine.world().workspace;
    shared.raw_set("game", instance_value(lua, engine, game)?)?;
    shared.raw_set("Game", instance_value(lua, engine, game)?)?;
    shared.raw_set("workspace", instance_value(lua, engine, workspace)?)?;
    shared.raw_set("Workspace", instance_value(lua, engine, workspace)?)?;
    shared.raw_set("Enum", engine.lua.borrow().enums_global.clone().expect("Enum global"))?;

    let instance_global = lua.create_table()?;
    // Instance.new: a C function (lua/raw.rs; InstanceModule.New)
    instance_global.raw_set("new", super::raw::instance_new_function(lua, engine)?)?;
    let e = engine.clone();
    instance_global.raw_set(
        "fromExisting",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let Some(existing) = args.instance(0) else { return raise_plain("Instance expected") };
            let Some(copy) = instance::clone(&e, existing) else { return Ok(values([LuaValue::Nil])) };
            let children = e.state.borrow().dom.at(copy).children.clone();
            for child in children {
                instance::destroy(lua, &e, child)?;
            }
            Ok(values([instance_value(lua, &e, copy)?]))
        })?,
    )?;
    shared.raw_set("Instance", instance_global)?;
    shared.raw_set("typeof", prelude.typeof_fn.clone())?;
    shared.raw_set("print", exports.get::<LuaValue>("Print")?)?;
    shared.raw_set("warn", exports.get::<LuaValue>("Warn")?)?;

    let e = engine.clone();
    shared.raw_set("tick", lua.create_function(move |_, ()| Ok(e.state.borrow_mut().sched.tick()))?)?;
    let e = engine.clone();
    let edit = engine.options.edit_mode();
    shared.raw_set(
        "time",
        lua.create_function(move |_, ()| Ok(if edit { 0.0 } else { e.state.borrow().sched.now }))?,
    )?;
    let e = engine.clone();
    let elapsed = lua.create_function(move |_, ()| Ok(e.state.borrow_mut().sched.elapsed_time()))?;
    shared.raw_set("elapsedTime", elapsed.clone())?;
    shared.raw_set("ElapsedTime", elapsed)?;
    let legacy_wait: LuaValue = exports.get("LegacyWait")?;
    shared.raw_set("wait", legacy_wait.clone())?;
    shared.raw_set("Wait", legacy_wait)?;
    let e = engine.clone();
    let legacy_spawn = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let thread = legacy_thread(lua, args.get(0))?;
        refs::admit(&e, Hold::Work, "spawn")?;
        e.state.borrow_mut().sched.schedule_legacy(TimerKind::Delay, Some(0.0), thread);
        Ok(none())
    })?;
    shared.raw_set("spawn", legacy_spawn.clone())?;
    shared.raw_set("Spawn", legacy_spawn)?;
    let e = engine.clone();
    let legacy_delay = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let duration = args.number(0);
        let thread = legacy_thread(lua, args.get(1))?;
        refs::admit(&e, Hold::Work, "delay")?;
        e.state.borrow_mut().sched.schedule_legacy(TimerKind::Delay, duration, thread);
        Ok(none())
    })?;
    shared.raw_set("delay", legacy_delay.clone())?;
    shared.raw_set("Delay", legacy_delay)?;
    shared.raw_set("task", exports.get::<LuaValue>("Task")?)?;

    let make_stub: LuaFunction = exports.get("MakeStubTree")?;
    let settings: LuaValue = make_stub.call("settings")?;
    // UserSettings(): the UserSettings object (its GetService gives UserGameSettings & co. at their
    // defaults: SavedQualityLevel Automatic, ...), made on the first call (an engine that made it at
    // start would number every later instance differently), else an inert stub without the class
    let stub_settings: LuaValue = make_stub.call("UserSettings")?;
    let user_settings: std::cell::RefCell<Option<LuaValue>> = std::cell::RefCell::new(None);
    shared.raw_set("settings", lua.create_function(move |_, ()| Ok(settings.clone()))?)?;
    let e = engine.clone();
    shared.raw_set(
        "UserSettings",
        lua.create_function(move |lua, ()| {
            if let Some(existing) = user_settings.borrow().as_ref() {
                return Ok(existing.clone());
            }
            let made = match services::user_settings(&e) {
                Some(id) => instance_value(lua, &e, id)?,
                None => stub_settings.clone(),
            };
            *user_settings.borrow_mut() = Some(made.clone());
            Ok(made)
        })?,
    )?;
    let version = engine.reflection.version.clone();
    let version = if version.is_empty() { "0.0.0.0".to_string() } else { version };
    let version_fn = lua.create_function(move |_, ()| Ok(version.clone()))?;
    shared.raw_set("version", version_fn.clone())?;
    shared.raw_set("Version", version_fn)?;
    let e = engine.clone();
    let stats = lua.create_function(move |lua, ()| {
        let game = e.world().game;
        let stats = services::get_service(&e, game, "Stats").map_err(flow_error)?;
        instance_value(lua, &e, stats)
    })?;
    shared.raw_set("stats", stats.clone())?;
    shared.raw_set("Stats", stats)?;
    shared.raw_set("shared", lua.create_table()?)?;
    shared.raw_set("SharedTable", exports.get::<LuaValue>("SharedTable")?)?;
    shared.raw_set("PluginManager", lua.create_function(|_, ()| Ok(LuaValue::Nil))?)?;
    let print: LuaFunction = exports.get("Print")?;
    shared.raw_set(
        "printidentity",
        lua.create_function(move |_, prefix: Option<String>| {
            print.call::<()>(format!("{} 2", prefix.unwrap_or_else(|| "Current identity is".into())))
        })?,
    )?;
    shared.raw_set(
        "loadstring",
        lua.create_function(|_, _: LuaMultiValue| Ok((LuaValue::Nil, "loadstring() is not available")))?,
    )?;
    let inert: LuaFunction = exports.get("InertLibrary")?;
    for name in ["DockWidgetPluginGuiInfo", "PluginDrag", "CellId"] {
        shared.raw_set(name, inert.call::<LuaValue>(name)?)?;
    }

    // Datatypes: engine types, prelude proxies, then Lune-backed script libraries
    for name in ["Content", "DateTime", "FloatCurveKey", "Font", "Random", "RotationCurveKey", "TweenInfo"] {
        let library = engine.lua.borrow().libraries.get(name).cloned().expect("library");
        shared.raw_set(name, library)?;
    }
    for name in ["OverlapParams", "RaycastParams", "CatalogSearchParams", "PathWaypoint", "Path2DControlPoint"] {
        shared.raw_set(name, exports.get::<LuaValue>(name)?)?;
    }
    let script_library: LuaFunction = exports.get("ScriptLibrary")?;
    for name in [
        "Axes", "BrickColor", "CFrame", "Color3", "ColorSequence", "ColorSequenceKeypoint", "Faces", "NumberRange",
        "NumberSequence", "NumberSequenceKeypoint", "PhysicalProperties", "Ray", "Rect", "Region3", "Region3int16",
        "UDim", "UDim2", "Vector2", "Vector2int16", "Vector3", "Vector3int16",
    ] {
        shared.raw_set(name, script_library.call::<LuaValue>(name)?)?;
    }
    shared.raw_set("_G", lua.create_table()?)?;
    Ok(shared)
}

/// Engine.Call: runs `function` on a fresh scheduler thread (so it may yield) under xpcall and
/// flushes the deferred queue.
pub fn call(lua: &Lua, engine: &Rc<Engine>, function: LuaFunction, args: LuaMultiValue) -> LuaResult<Call> {
    call_then(lua, engine, function, args, || true)
}

/// [`call`], flushing the deferred queue after the first resumption only when `flush` says so (a
/// recording parked at its harness's checkpoint flushes when it goes on: record/run.rs).
pub fn call_then(lua: &Lua, engine: &Rc<Engine>, function: LuaFunction, args: LuaMultiValue, flush: impl FnOnce() -> bool) -> LuaResult<Call> {
    let body: LuaFunction = engine.prelude().exports.get("CallBody")?;
    let outcome = lua.create_table()?;
    let thread = lua.create_thread(body)?;
    let mut arguments = LuaMultiValue::from_iter([LuaValue::Table(outcome.clone()), LuaValue::Function(function)]);
    arguments.extend(args);
    scheduler::resume(lua, engine, &thread, arguments);
    if flush() {
        scheduler::flush_deferred(lua, engine)?;
    }
    Ok(Call { thread, outcome })
}

/// A pending or finished Engine.Call.
pub struct Call {
    pub thread: LuaThread,
    outcome: LuaTable,
}

pub enum CallOutcome {
    Finished(LuaMultiValue),
    Pending,
    Failed(String, Option<String>),
}

impl Call {
    /// How the call ended so far (Outcome()).
    pub fn outcome(&self) -> LuaResult<CallOutcome> {
        if let Some(results) = self.outcome.get::<Option<LuaTable>>("Results")? {
            let count: i64 = results.get("n")?;
            let mut values = LuaMultiValue::new();
            for index in 2..=count {
                values.push_back(results.get(index)?);
            }
            return Ok(CallOutcome::Finished(values));
        }
        if let Some(failure) = self.outcome.get::<Option<LuaTable>>("Failure")? {
            let message: String = self.outcome.get("Message")?;
            let trace: Option<String> = failure.get("Trace")?;
            return Ok(CallOutcome::Failed(message, trace));
        }
        Ok(CallOutcome::Pending)
    }
}

impl LuaRefs {
    /// The shared environment table (after `start`).
    pub fn shared_globals(&self) -> Option<LuaTable> {
        self.shared.clone()
    }
}

/// Unused marker to keep the lock helper referenced for types built lazily.
#[allow(dead_code)]
fn _lock(lua: &Lua, ud: &LuaAnyUserData) -> LuaResult<()> {
    lock_metatable(lua, ud)
}

pub fn studio_only(engine: &Engine, id: InstanceId) -> bool {
    engine.state.borrow().dom.at(id).has(flags::STUDIO_ONLY)
}

//! Loads the Luau service stubs (`services.luau`) with the engine primitives they use.

use std::rc::Rc;

use mlua::prelude::*;

use super::{
    Flow, convert, enum_item_value,
    instance::{set_parent, set_property},
    instance_value, modules, raw, signal_value, types,
};
use crate::{Engine, instance as core};

const SERVICES: &str = include_str!("services.luau");
const SERVER: &str = include_str!("server.luau");

/// The named registry value holding server.luau's R.SetJoinData (a server run's only): what
/// Context.SetJoinData calls to give a player the join data Player:GetJoinData returns
pub const SET_JOIN_DATA: &str = "ree.setjoindata";

/// LogService error tables turned into text (their `__tostring`, services.luau ErrorText): how many so far, and the last
/// one's (thread that asked, text). A thread that ends on such a table has mlua convert it into text on its resumer's
/// thread before the scheduler sees the error ([`uncaught_log_error`]).
#[derive(Default)]
struct LogErrorTexts {
    count: u64,
    thread: usize,
    text: String,
}

thread_local! {
    static LOG_ERROR_TEXTS: std::cell::RefCell<LogErrorTexts> = std::cell::RefCell::new(LogErrorTexts::default());
}

/// How many LogService error tables have been turned into text so far (taken before a thread is resumed).
pub fn log_error_texts() -> u64 {
    LOG_ERROR_TEXTS.with(|texts| texts.borrow().count)
}

/// Whether `thread`, which ended on the error `message`, ended on a LogService error table no script caught: one was turned
/// into text since `since`, last, on another thread than `thread` (mlua converts the ended thread's error value on the
/// thread that resumed it; a script's own `tostring` of the table runs on the script's thread), giving `message`.
pub fn uncaught_log_error(since: u64, thread: &LuaThread, message: &str) -> bool {
    LOG_ERROR_TEXTS.with(|texts| {
        let texts = texts.borrow();
        texts.count > since && texts.thread != thread.to_pointer() as usize && texts.text == message
    })
}

fn instance_of(value: &LuaValue) -> LuaResult<crate::InstanceId> {
    match value {
        LuaValue::UserData(ud) => raw::instance_of_ud(ud).ok_or_else(|| LuaError::runtime("Instance expected")),
        _ => Err(LuaError::runtime("Instance expected")),
    }
}

/// Where data stores are kept as files (`--datastore-dir`), unless `--fresh-datastore` (a run that starts empty and saves nothing)
fn datastore_root(options: &crate::Options) -> Option<std::path::PathBuf> {
    if options.fresh_datastore {
        return None;
    }
    options.datastore_dir.clone()
}

/// A store's folder: its name (an ordered store's with `.ordered` after it)
fn store_directory(kind: &str, name: &str) -> String {
    let name = path_component(name);
    if kind == "Ordered" { format!("{name}.ordered") } else { name }
}

/// A name as one folder name: letters, digits, `_` and `-` as they are (and `.` inside a name), the rest as `%XX` bytes
fn path_component(text: &str) -> String {
    let last = text.chars().count().saturating_sub(1);
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' || (character == '.' && index != 0 && index != last) {
            out.push(character);
        } else {
            let mut buffer = [0u8; 4];
            for byte in character.encode_utf8(&mut buffer).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

fn path_component_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = (bytes[at] == b'%').then(|| bytes.get(at + 1..at + 3)).flatten().and_then(|pair| std::str::from_utf8(pair).ok()).and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn load(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let r = lua.create_table()?;
    let e = engine.clone();
    r.set(
        "Method",
        lua.create_function(move |_, (class_name, name, function): (String, String, LuaFunction)| {
            let (Some(class), Some(name)) = (e.reflection.class_id(&class_name), e.reflection.name_id(&name)) else {
                return Ok(());
            };
            e.behaviors.borrow_mut().add_lua_method(class, name, function);
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "Getter",
        lua.create_function(move |_, (class_name, name, function): (String, String, LuaFunction)| {
            let (Some(class), Some(name)) = (e.reflection.class_id(&class_name), e.reflection.name_id(&name)) else {
                return Ok(());
            };
            e.behaviors.borrow_mut().add_getter(
                class,
                name,
                Rc::new(move |lua, engine, id| Ok(function.call::<LuaValue>(instance_value(lua, engine, id)?)?)),
            );
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        // a setter that first passes the assigned value through `filter(instance, value)` (which may
        // swap it), then assigns what it returns as the property's own setter does (a derived view's,
        // engine::derived: a Content view of a string asset id), else as a plain assignment (coerced,
        // notified)
        "ContentSetter",
        lua.create_function(move |_, (class_name, name, filter): (String, String, LuaFunction)| {
            let (Some(class), Some(name)) = (e.reflection.class_id(&class_name), e.reflection.name_id(&name)) else {
                return Ok(());
            };
            let Some(info) = e.reflection.class_property(class, name).cloned() else { return Ok(()) };
            let previous = e.behaviors.borrow().find_setter(&e.reflection, class, name).cloned();
            e.behaviors.borrow_mut().add_setter(
                class,
                name,
                Rc::new(move |lua, engine, id, value| {
                    let value = filter.call::<LuaValue>((instance_value(lua, engine, id)?, value))?;
                    if let Some(previous) = &previous {
                        return previous(lua, engine, id, value);
                    }
                    let coerced = convert::coerce(lua, engine, &info, &value)?;
                    set_property(engine, id, name, coerced);
                    Ok(())
                }),
            );
            // (a member resolved before now cached it without the setter)
            e.clear_member_cache();
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "Create",
        lua.create_function(move |lua, class_name: String| {
            let class = e.reflection.class_id(&class_name).ok_or_else(|| LuaError::runtime(format!("Unable to create an Instance of type \"{class_name}\"")))?;
            let id = core::create(&mut e.state.borrow_mut(), &e.reflection, class);
            instance_value(lua, &e, id)
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "RawSet",
        lua.create_function(move |_, (target, name, value, silent): (LuaValue, String, LuaValue, bool)| {
            let id = instance_of(&target)?;
            let Some(name) = e.reflection.name_id(&name) else { return Ok(false) };
            let value = convert::from_lua(&value);
            let hooks = e.hooks.borrow();
            let mut state = e.state.borrow_mut();
            Ok(if silent {
                core::raw_set_silent(&mut state, &e.reflection, id, name, value)
            } else {
                core::raw_set(&mut state, &e.reflection, &hooks, id, name, value)
            })
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "RawGet",
        // (`stored_only`: nil unless the instance holds a value, never the class default)
        lua.create_function(move |lua, (target, name, stored_only): (LuaValue, String, Option<bool>)| {
            let id = instance_of(&target)?;
            let Some(name) = e.reflection.name_id(&name) else { return Ok(LuaValue::Nil) };
            let value = {
                let state = e.state.borrow();
                if stored_only == Some(true) {
                    state.dom.at(id).stored(name).filter(|value| !matches!(value, crate::value::Value::Nil)).cloned()
                } else {
                    state.dom.raw_get(id, name).cloned()
                }
            };
            match value {
                Some(value) => convert::to_lua(lua, &e, &value),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "Children",
        lua.create_function(move |lua, target: LuaValue| {
            let id = instance_of(&target)?;
            // (a lazily imported bundle instance's children are made first, as GetChildren makes them)
            if e.state.borrow().dom.at(id).has(crate::dom::flags::LAZY_PENDING) {
                crate::project::materialize_lazy(&mut e.state.borrow_mut(), &e.reflection, id);
            }
            let children = e.state.borrow().dom.at(id).children.clone();
            let table = lua.create_table_with_capacity(children.len(), 0)?;
            for child in children {
                table.raw_push(instance_value(lua, &e, child)?)?;
            }
            Ok(table)
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "SetParent",
        lua.create_function(move |_, (child, parent): (LuaValue, LuaValue)| {
            let child = instance_of(&child)?;
            let parent = if parent.is_nil() { None } else { Some(instance_of(&parent)?) };
            set_parent(&e, child, parent).map_err(|_| LuaError::runtime("SetParent failed"))
        })?,
    )?;
    // The engine's Luau that prices its own methods (services.luau Priced): Mute(true) drops the Instance API's
    // charges until Mute(false), Charge adds the method's own cost
    let e = engine.clone();
    r.set(
        "Mute",
        lua.create_function(move |lua, on: bool| {
            crate::cost::mute_host_nanos(lua, on);
            let muted = e.state.borrow().sched.muted.clone();
            muted.set(if on { muted.get() + 1 } else { muted.get().saturating_sub(1) });
            Ok(())
        })?,
    )?;
    let costs = lua.create_table()?;
    for (name, seconds) in [
        ("ActorInvoke", crate::cost::ACTOR_INVOKE),
        ("SharedIncrement", crate::cost::SHARED_INCREMENT),
        ("ModelQuery", crate::cost::MODEL_QUERY),
        ("LogCall", crate::cost::LOG_CALL),
        ("Thread", crate::cost::THREAD),
        ("PivotSingle", crate::cost::PIVOT_SINGLE),
        ("PivotPart", crate::cost::PIVOT_PART),
        ("PivotBase", crate::cost::PIVOT_BASE),
        ("PivotLoose", crate::cost::PIVOT_LOOSE),
        ("ScaleScanned", crate::cost::SCALE_SCANNED),
        ("ScalePartPlaced", crate::cost::SCALE_PART_PLACED),
        ("ScalePartLoose", crate::cost::SCALE_PART_LOOSE),
        ("ScaleAttachment", crate::cost::SCALE_ATTACHMENT),
        ("ScaleJoint", crate::cost::SCALE_JOINT),
    ] {
        costs.set(name, seconds)?;
    }
    r.set("Cost", costs)?;
    let e = engine.clone();
    r.set(
        "Charge",
        lua.create_function(move |_, seconds: f64| {
            e.state.borrow().sched.charge_priced(seconds);
            Ok(())
        })?,
    )?;
    // Model:PivotTo / ScaleTo: many part moves as one step (world::joints batches)
    let e = engine.clone();
    r.set(
        "BeginMoves",
        lua.create_function(move |_, ()| {
            crate::world::joints::begin_batch(&mut e.state.borrow_mut());
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "EndMoves",
        lua.create_function(move |_, ()| {
            let hooks = e.hooks.borrow();
            let mut state = e.state.borrow_mut();
            crate::world::joints::end_batch(&mut state, &e.reflection, &hooks);
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "Missing",
        lua.create_function(move |_, (class_name, member): (String, String)| {
            e.state.borrow_mut().output.missing(&class_name, &member);
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set("LogHistory", lua.create_function(move |lua, ()| {
        let state = e.state.borrow();
        let history = lua.create_table_with_capacity(state.log_history.len(), 0)?;
        for (index, line) in state.log_history.iter().enumerate() {
            let entry = lua.create_table_with_capacity(0, 3)?;
            entry.raw_set("message", line.text.as_str())?;
            let name = match line.kind {
                "warn" => "MessageWarning",
                "error" => "MessageError",
                "info" => "MessageInfo",
                _ => "MessageOutput",
            };
            let item = e.reflection.enum_item("MessageType", name).unwrap();
            entry.raw_set("messageType", enum_item_value(lua, &e, item)?)?;
            entry.raw_set("timestamp", line.timestamp)?;
            history.raw_set(index + 1, entry)?;
        }
        Ok(history)
    })?)?;
    // LogService's logging methods (services.luau): a line of a kind with the converted context MessageOut passes on
    let e = engine.clone();
    r.set(
        "LogLine",
        lua.create_function(move |_, (kind, message, context): (LuaString, LuaString, Option<LuaTable>)| {
            let kind = match &*kind.as_bytes() {
                b"info" => "info",
                b"warn" => "warn",
                b"error" => "error",
                _ => "print",
            };
            let context = context.map(|table| crate::value::Value::Lua(Box::new(LuaValue::Table(table))));
            e.output_with_context(kind, message.to_string_lossy(), None, context);
            Ok(())
        })?,
    )?;
    // an engine warning about the calling script's arguments, with its stack (a client's MessageOut text carries the stack lines)
    let e = engine.clone();
    r.set(
        "LogWarn",
        lua.create_function(move |_, (message, trace): (LuaString, Option<LuaString>)| {
            e.output("warn", message.to_string_lossy(), trace.map(|trace| trace.to_string_lossy()));
            Ok(())
        })?,
    )?;
    // a LogService error's `stack`: "Stack Begin", a line per frame of the script's own code, "Stack End"
    r.set(
        "LogStack",
        lua.create_function(|lua, trace: LuaString| {
            let mut lines = vec!["Stack Begin".to_string()];
            lines.extend(crate::stack_frames(&trace.to_string_lossy()));
            lines.push("Stack End".to_string());
            lua.create_string(lines.join("\n"))
        })?,
    )?;
    // a LogService error table turned into text ([`uncaught_log_error`])
    r.set(
        "LogErrorText",
        lua.create_function(|lua, text: LuaString| {
            let thread = lua.current_thread().to_pointer() as usize;
            LOG_ERROR_TEXTS.with(|texts| {
                let mut texts = texts.borrow_mut();
                texts.count += 1;
                texts.thread = thread;
                texts.text = text.to_string_lossy();
            });
            Ok(())
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "LocalPlayer",
        lua.create_function(move |lua, ()| match e.world().local_player {
            Some(player) => instance_value(lua, &e, player),
            None => Ok(LuaValue::Nil),
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "GetEnumItem",
        lua.create_function(move |lua, (enum_name, item_name): (String, String)| match e.reflection.enum_item(&enum_name, &item_name) {
            Some(item) => enum_item_value(lua, &e, item),
            None => Ok(LuaValue::Nil),
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "NewSignal",
        lua.create_function(move |lua, name: String| {
            let signal = types::new_signal(&e, &name);
            signal_value(lua, &e, signal)
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "GetCallback",
        lua.create_function(move |_, (target, name): (LuaValue, String)| {
            let id = instance_of(&target)?;
            let Some(name) = e.reflection.name_id(&name) else { return Ok(LuaValue::Nil) };
            Ok(e.state.borrow().dom.at(id).callback(name).cloned().unwrap_or(LuaValue::Nil))
        })?,
    )?;
    // BindableFunction:Invoke's copies of its arguments and results (lua/bindable.rs)
    let e = engine.clone();
    r.set("Copied", super::wrapped(lua, engine, move |lua, values| super::bindable::copied(lua, &e, values))?)?;
    // KeyframeSequenceProvider:GetKeyframeSequenceAsync's copy of an animation, else nil and the error
    let e = engine.clone();
    r.set(
        "KeyframeSequence",
        lua.create_function(move |lua, uri: String| match crate::animation::source::sequence(&e, &uri) {
            Ok(sequence) => Ok((instance_value(lua, &e, sequence)?, None)),
            Err(error) => Ok((LuaValue::Nil, Some(format!("Failed to load animation {uri}: {error}")))),
        })?,
    )?;
    // AnimationClipProvider:GetAnimationClipAsync's clip, else nil and the error, or false for an id that never answers
    let e = engine.clone();
    r.set(
        "AnimationClip",
        lua.create_function(move |lua, uri: String| match crate::animation::source::animation_clip(&e, &uri) {
            Ok(clip) => Ok((instance_value(lua, &e, clip)?, None)),
            Err(error) if error == crate::animation::source::NEVER => Ok((LuaValue::Boolean(false), None)),
            Err(error) => Ok((LuaValue::Nil, Some(format!("Failed to load animation {uri}: {error}")))),
        })?,
    )?;
    // ContentProvider:PreloadAsync's animations: the seconds until the ones it asks for arrive
    let e = engine.clone();
    r.set(
        "PreloadAnimations",
        lua.create_function(move |_, items: LuaTable| {
            let mut ids = Vec::new();
            for item in items.sequence_values::<LuaValue>() {
                if let Ok(id) = instance_of(&item?) {
                    ids.push(id);
                }
            }
            Ok(crate::animation::preload(&e, &ids))
        })?,
    )?;
    // EditableImage pixels (crate::images): an asset's or file's as (width, height, buffer), else nil
    // and the error; a snapshot of a drawn image written as a PNG, its path
    let e = engine.clone();
    r.set(
        "LoadImage",
        lua.create_function(move |lua, uri: String| match crate::images::load(&e, &uri) {
            Ok(pixels) => Ok((LuaValue::Number(pixels.width as f64), LuaValue::Number(pixels.height as f64), LuaValue::Buffer(lua.create_buffer(pixels.rgba)?), None)),
            Err(error) => Ok((LuaValue::Nil, LuaValue::Nil, LuaValue::Nil, Some(error))),
        })?,
    )?;
    let e = engine.clone();
    r.set(
        "SaveImage",
        lua.create_function(move |_, (width, height, pixels): (u32, u32, mlua::Buffer)| match crate::images::save(&e, width, height, &pixels.to_vec()) {
            Ok(path) => Ok((Some(path.to_string_lossy().replace('\\', "/")), None)),
            Err(error) => Ok((None, Some(error))),
        })?,
    )?;
    // HttpService JSON: Lune's serde (sorted keys, empty tables as `{}`, integral numbers as integers)
    r.set(
        "JsonEncode",
        lua.create_function(|lua, value: LuaValue| {
            let safe = crate::services::json_safe(lua, &value, &mut Vec::new()).map_err(|flow| match flow {
                Flow::Error(error) => LuaError::runtime(error.message),
                Flow::Fail(error) => error,
                Flow::Yield => LuaError::runtime("JsonEncode yielded"),
            })?;
            let json: serde_json::Value = lua.from_value_with(
                safe,
                LuaDeserializeOptions::new().sort_keys(true).deny_recursive_tables(false).deny_unsupported_types(true),
            )?;
            serde_json::to_string(&json).map_err(LuaError::external)
        })?,
    )?;
    r.set(
        "JsonDecode",
        lua.create_function(|lua, text: LuaValue| {
            let LuaValue::String(text) = text else { return Ok((false, LuaValue::Nil)) };
            // (services port) Lune's serde.decode number semantics (engine::lune_json)
            let decoded = std::str::from_utf8(&text.as_bytes()).map_err(|e| e.to_string()).and_then(crate::lune_json::decode);
            match decoded {
                Ok(value) => Ok((
                    true,
                    lua.to_value_with(
                        &value,
                        LuaSerializeOptions::new().set_array_metatable(false).serialize_none_to_null(false).serialize_unit_to_null(false),
                    )?,
                )),
                Err(_) => Ok((false, LuaValue::Nil)),
            }
        })?,
    )?;
    // Data stores kept as files (--datastore-dir/<store>/<scope>/<key>/data.json); --fresh-datastore reads and writes none
    if let Some(root) = datastore_root(&engine.options) {
        let load_root = root.clone();
        r.set(
            "DataStoreLoad",
            lua.create_function(move |lua, (kind, name, scope): (String, String, String)| {
                let directory = load_root.join(store_directory(&kind, &name)).join(path_component(&scope));
                let list = lua.create_table()?;
                if let Ok(read) = std::fs::read_dir(&directory) {
                    for entry in read.flatten() {
                        if let Ok(text) = std::fs::read_to_string(entry.path().join("data.json")) {
                            let item = lua.create_table()?;
                            item.set("key", path_component_decoded(&entry.file_name().to_string_lossy()))?;
                            item.set("json", text)?;
                            list.push(item)?;
                        }
                    }
                }
                Ok(list)
            })?,
        )?;
        r.set(
            "DataStoreSave",
            lua.create_function(move |_, (kind, name, scope, key, json): (String, String, String, String, Option<String>)| {
                let directory = root.join(store_directory(&kind, &name)).join(path_component(&scope)).join(path_component(&key));
                match json {
                    Some(text) => {
                        let pretty = serde_json::from_str::<serde_json::Value>(&text).and_then(|value| serde_json::to_string_pretty(&value)).unwrap_or(text);
                        if let Err(error) = std::fs::create_dir_all(&directory).and_then(|_| std::fs::write(directory.join("data.json"), pretty)) {
                            eprintln!("ree: could not save data store key {}: {error}", directory.display());
                        }
                    }
                    None => {
                        let _ = std::fs::remove_dir_all(&directory);
                    }
                }
                Ok(())
            })?,
        )?;
    }
    let options = lua.create_table()?;
    options.set("TopbarInset", engine.options.topbar_inset)?;
    options.set("ViewportWidth", engine.options.viewport_width)?;
    options.set("ViewportHeight", engine.options.viewport_height)?;
    r.set("Options", options)?;
    let mouse: LuaValue = engine.lua.borrow().shared.as_ref().map(|s| s.raw_get("__rurMouse")).transpose()?.unwrap_or(LuaValue::Nil);
    r.set("Mouse", mouse)?;
    r.set("Loopback", engine.options.loopback)?;

    let shared = engine.lua.borrow().shared.clone().ok_or_else(|| LuaError::runtime("shared globals"))?;
    let services = lua.load(SERVICES).set_name("=rur.services").set_environment(shared.clone()).set_compiler(modules::engine_compiler()).into_function()?;
    crate::cost::engine_luau(lua, services)?.call::<()>(r.clone())?;
    // A server's own services (DataStores, MemoryStores, MessagingService, the server side of remotes)
    if engine.options.server_services() {
        let server = lua.load(SERVER).set_name("=rur.server").set_environment(shared).set_compiler(modules::engine_compiler()).into_function()?;
        crate::cost::engine_luau(lua, server)?.call::<()>(r.clone())?;
        if let LuaValue::Function(set_join_data) = r.get::<LuaValue>("SetJoinData")? {
            lua.set_named_registry_value(SET_JOIN_DATA, set_join_data)?;
        }
    }
    engine.clear_member_cache();
    Ok(())
}

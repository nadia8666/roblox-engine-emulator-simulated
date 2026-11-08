//! Instance userdata: `__index` / `__newindex` (port of instance.luau InstanceMeta), the base
//! Instance behaviours (Methods, Getters, Setters of class Instance) and the operations that run
//! scripts (Destroy, WaitForChild).

use std::{cell::Cell, rc::Rc};

use mlua::prelude::*;

use super::{
    Args, Flow, Ret, convert, encode, instance_id, instance_value, none, raise, raise_at_caller, raw, signal_value, typeof_value,
    values, wrapped,
};
use crate::{
    ClassId, Engine, InstanceId, K, NameId, State,
    dom::flags,
    instance::{self as core, Member},
    project,
    scheduler::{self, JobTarget, TimerKind},
    services::{Behaviors, GetterFn, MethodFn, SetterFn},
    signal::{Handler, SignalId},
    value::{Str, Value},
};

fn key_text(key: &LuaValue) -> String {
    match key {
        LuaValue::String(s) => s.to_string_lossy(),
        LuaValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Boolean(b) => b.to_string(),
        LuaValue::Nil => "nil".into(),
        other => other.type_name().into(),
    }
}

pub fn class_name(engine: &Engine, id: InstanceId) -> String {
    let class = engine.state.borrow().dom.at(id).class;
    engine.reflection.class(class).name.to_string()
}

fn full_name(engine: &Engine, id: InstanceId) -> String {
    engine.state.borrow().dom.full_name(&engine.reflection, id)
}

fn not_a_member(engine: &Engine, id: InstanceId, key: &LuaValue) -> Flow {
    Flow::Error(crate::ScriptError::plain(format!(
        "{} is not a valid member of {} \"{}\"",
        key_text(key),
        class_name(engine, id),
        full_name(engine, id)
    )))
}

/// The (cached) script function of a behaviour method (shared by every class inheriting it).
fn method_function(lua: &Lua, engine: &Rc<Engine>, class: ClassId, name: NameId) -> LuaResult<LuaValue> {
    let function = method_function_uncached(lua, engine, class, name)?;
    if let LuaValue::Function(function) = &function {
        raw::remember_index_function(engine, class, name, function)?;
    }
    Ok(function)
}

/// The function a method call (`instance:Name(...)`) runs: a Rust behaviour's protocol function
/// (the raw namecall checks its results), a Luau behaviour, or the unimplemented stand-in.
/// `None`: not a method.
pub fn call_function(lua: &Lua, engine: &Rc<Engine>, class: ClassId, name: NameId) -> LuaResult<Option<LuaFunction>> {
    if !matches!(engine.resolve_member(class, name), Member::Method { .. }) {
        return Ok(None);
    }
    let found = engine.behaviors.borrow().find_method_or_alias(&engine.reflection, class, name).map(|(owner, f)| (owner, f.clone()));
    let Some((owner, method)) = found else {
        return match unimplemented_function(lua, engine, class, name)? {
            LuaValue::Function(function) => Ok(Some(function)),
            _ => Ok(None),
        };
    };
    let method = match method {
        crate::services::Method::Lua(function) => return Ok(Some(function)),
        crate::services::Method::Rust(method) => method,
    };
    if let Some(existing) = engine.lua.borrow().call_methods.get(&(owner, name)) {
        return Ok(Some(existing.clone()));
    }
    let e = engine.clone();
    let method_name = engine.reflection.name(name).to_string();
    let function = lua.create_function(move |lua, args: LuaMultiValue| {
        let args = Args::new(args);
        let outcome = match args.instance(0) {
            Some(id) => {
                project::before_method(&mut e.state.borrow_mut(), &e.reflection, id, name);
                method(lua, &e, id, &args)
            }
            None => raise(format!("Expected ':' not '.' calling member function {method_name}")),
        };
        encode(lua, outcome)
    })?;
    engine.lua.borrow_mut().call_methods.insert((owner, name), function.clone());
    Ok(Some(function))
}

fn method_function_uncached(lua: &Lua, engine: &Rc<Engine>, class: ClassId, name: NameId) -> LuaResult<LuaValue> {
    let found = engine.behaviors.borrow().find_method_or_alias(&engine.reflection, class, name).map(|(owner, f)| (owner, f.clone()));
    let Some((owner, method)) = found else {
        return unimplemented_function(lua, engine, class, name);
    };
    let method = match method {
        crate::services::Method::Lua(function) => return Ok(LuaValue::Function(function)),
        crate::services::Method::Rust(method) => method,
    };
    if let Some(existing) = engine.lua.borrow().methods.get(&(owner, name)) {
        return Ok(LuaValue::Function(existing.clone()));
    }
    let e = engine.clone();
    let method_name = engine.reflection.name(name).to_string();
    let function = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let Some(id) = args.instance(0) else {
            return raise(format!("Expected ':' not '.' calling member function {method_name}"));
        };
        project::before_method(&mut e.state.borrow_mut(), &e.reflection, id, name);
        method(lua, &e, id, &args)
    })?;
    engine.lua.borrow_mut().methods.insert((owner, name), function.clone());
    Ok(LuaValue::Function(function))
}

/// A reflected method no behaviour implements: records the call and returns nil.
fn unimplemented_function(lua: &Lua, engine: &Rc<Engine>, class: ClassId, name: NameId) -> LuaResult<LuaValue> {
    if let Some(existing) = engine.lua.borrow().unimplemented.get(&(class, name)) {
        return Ok(LuaValue::Function(existing.clone()));
    }
    let e = engine.clone();
    let class_label = engine.reflection.class(class).name.to_string();
    let member = engine.reflection.name(name).to_string();
    let function = lua.create_function(move |_, _: LuaMultiValue| {
        e.state.borrow_mut().output.missing(&class_label, &member);
        Ok(())
    })?;
    engine.lua.borrow_mut().unimplemented.insert((class, name), function.clone());
    Ok(LuaValue::Function(function))
}

/// Hooks.Materialize (Studio-only children, once per instance) before children are looked up.
pub fn materialize(engine: &Engine, id: InstanceId) {
    #[cfg(feature = "bundles")]
    project::model_assets::materialize(engine, id);
    let mut state = engine.state.borrow_mut();
    project::materialize(&mut state, &engine.reflection, id);
}

/// A manifest bundle's placeholder ([`flags::BUNDLE_ROOT`]) a script is about to find, test or touch
/// becomes the bundle's instance first (project::model_assets::reveal). Call it with the state
/// unborrowed, for an instance whose flag is set.
pub fn reveal_bundle(engine: &Engine, id: InstanceId) {
    #[cfg(feature = "bundles")]
    project::model_assets::reveal(engine, id);
    #[cfg(not(feature = "bundles"))]
    let _ = (engine, id);
}

/// A child a script looked up by name and did not find that a bundle mount makes when asked for: a manifest alias's copy
/// (project::model_assets::requested).
fn made_on_request(engine: &Engine, id: InstanceId, name: &[u8]) -> Option<InstanceId> {
    #[cfg(feature = "bundles")]
    return project::model_assets::requested(engine, id, name);
    #[cfg(not(feature = "bundles"))]
    {
        let _ = (engine, id, name);
        None
    }
}

/// Reached: a script found `child` by name (a bundle's placeholder becomes the bundle's instance;
/// script access to Studio-only placeholders is noted).
pub(super) fn reached(engine: &Engine, child: InstanceId) -> InstanceId {
    if engine.state.borrow().dom.at(child).has(flags::BUNDLE_ROOT) {
        reveal_bundle(engine, child);
    }
    let mut state = engine.state.borrow_mut();
    if state.dom.at(child).has(flags::STUDIO_ONLY) {
        project::placeholder_used(&mut state, &engine.reflection, child);
    }
    child
}

/// The first child of `id` (made: [`materialize`] first) that `matches`, each bundle placeholder
/// among them made its bundle's instance before it is tested (a class or value test sees the
/// instance the game has there).
pub fn find_child_where(engine: &Engine, id: InstanceId, matches: impl Fn(&State, InstanceId) -> bool) -> Option<InstanceId> {
    let mut from = 0;
    loop {
        let (index, placeholder) = {
            let state = engine.state.borrow();
            let mut found = None;
            for (index, &child) in state.dom.at(id).children.iter().enumerate().skip(from) {
                if state.dom.at(child).has(flags::BUNDLE_ROOT) {
                    found = Some((index, child));
                    break;
                }
                if matches(&state, child) {
                    return Some(child);
                }
            }
            found?
        };
        reveal_bundle(engine, placeholder);
        if matches(&engine.state.borrow(), placeholder) {
            return Some(placeholder);
        }
        from = index + 1;
    }
}

/// `instance[key]` (InstanceMeta.__index): a member by its exact name, else a child by that name, else (a client's rule,
/// docs/engine.md "Member names") the member the key names with its first letter upper-cased.
pub fn get(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, key: &LuaValue) -> Result<LuaValue, Flow> {
    let LuaValue::String(key_string) = key else {
        return raise_at_caller(format!("invalid argument #2 (string expected, got {})", typeof_value(engine, key)));
    };
    let key_bytes = key_string.as_bytes();
    if engine.state.borrow().dom.at(id).has(flags::BUNDLE_ROOT) {
        reveal_bundle(engine, id);
    }
    let class = engine.state.borrow().dom.at(id).class;
    if let Some(name) = engine.reflection.name_id_bytes(&key_bytes)
        && let Some(value) = get_member(lua, engine, id, class, name)
    {
        return value;
    }
    materialize(engine, id);
    let child = engine.state.borrow().dom.find_child(id, &key_bytes);
    if let Some(child) = child {
        reached(engine, child);
        return Ok(instance_value(lua, engine, child)?);
    }
    if let Some(child) = made_on_request(engine, id, &key_bytes) {
        return Ok(instance_value(lua, engine, child)?);
    }
    if let Some(child) = super::globals::missing_child(lua, engine, id, &key_bytes)? {
        return Ok(instance_value(lua, engine, child)?);
    }
    if let Some(name) = engine.reflection.upper_first_id(&key_bytes)
        && let Some(value) = get_member(lua, engine, id, class, name)
    {
        return value;
    }
    Err(not_a_member(engine, id, key))
}

/// A read of member `name` of an instance of `class`; `None` when the class has no such member a script can read.
fn get_member(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, class: ClassId, name: NameId) -> Option<Result<LuaValue, Flow>> {
    let member = engine.resolve_member(class, name);
    // (a deprecated lower-case property reads the property it stands for)
    let name = engine.member_name(class, name);
    Some(match member {
        Member::Property { getter: true, .. } => {
            let getter = engine.behaviors.borrow().find_getter(&engine.reflection, class, name).cloned()?;
            getter(lua, engine, id)
        }
        Member::Property { .. } => read_property(lua, engine, id, name),
        Member::Method { .. } => method_function(lua, engine, class, name).map_err(Flow::from),
        Member::Event => {
            let signal = core::event_signal(&mut engine.state.borrow_mut(), &engine.reflection, id, name);
            signal_value(lua, engine, signal).map_err(Flow::from)
        }
        // (the member's own name, also when the key spelled it with a lower-case first letter: measured, `onInvoke`)
        Member::Callback => raise(format!(
            "{} is a callback member of {}; you can only set the callback value, get is not available",
            engine.reflection.name(name),
            class_name(engine, id)
        )),
        Member::None => return None,
    })
}

/// The error a client raises for a key that names nothing on an instance: no member, no child (and for a method call, no
/// method), with the key as the script wrote it.
pub fn not_a_member_message(engine: &Engine, id: InstanceId, key: &[u8]) -> String {
    format!("{} is not a valid member of {} \"{}\"", String::from_utf8_lossy(key), class_name(engine, id), full_name(engine, id))
}

/// A property read. Immutable datatype values (UDim2, Color3, ...) keep one userdata while the
/// instance is unchanged, as the Luau runtime returns the stored value (and to spare an allocation);
/// the raw `__index` returns them from the same cache.
fn read_property(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, name: NameId) -> Result<LuaValue, Flow> {
    let (value, version) = {
        let state = engine.state.borrow();
        (state.dom.raw_get(id, name).cloned(), state.dom.at(id).version)
    };
    let Some(value) = value else { return Ok(LuaValue::Nil) };
    let cacheable = matches!(
        value,
        Value::UDim(_)
            | Value::UDim2(_)
            | Value::Vector2(_)
            | Value::Vector3(_)
            | Value::Color3(_)
            | Value::Rect(_)
            | Value::NumberRange(_)
            | Value::NumberSequence(_)
            | Value::ColorSequence(_)
            | Value::Dom(_)
    );
    if !cacheable {
        return Ok(convert::to_lua(lua, engine, &value)?);
    }
    if let Some(cached) = raw::cached_value(engine, id, name, version)? {
        return Ok(cached);
    }
    let converted = convert::to_lua(lua, engine, &value)?;
    raw::cache_value(engine, id, name, version, &converted)?;
    Ok(converted)
}

/// `instance[key] = value` (InstanceMeta.__newindex).
pub fn assign(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, key: &LuaValue, value: &LuaValue) -> Result<(), Flow> {
    if engine.state.borrow().dom.at(id).has(flags::BUNDLE_ROOT) {
        reveal_bundle(engine, id);
    }
    let class = engine.state.borrow().dom.at(id).class;
    let name = match key {
        LuaValue::String(s) => engine.reflection.name_id_bytes(&s.as_bytes()),
        _ => None,
    };
    let member = name.map_or(Member::None, |name| engine.resolve_member(class, name));
    let name = match (member, name) {
        (Member::None, _) | (_, None) => return Err(not_a_member(engine, id, key)),
        // (a deprecated lower-case property writes the property it stands for)
        (_, Some(name)) => engine.member_name(class, name),
    };
    match member {
        Member::Callback => {
            let mut state = engine.state.borrow_mut();
            let callbacks = state.dom.at_mut(id).callbacks.vec_mut();
            match callbacks.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = value.clone(),
                None => callbacks.push((name, value.clone())),
            }
            return Ok(());
        }
        Member::Property { property, setter, .. } => {
            if setter {
                let setter = engine.behaviors.borrow().find_setter(&engine.reflection, class, name).cloned();
                if let Some(setter) = setter {
                    setter(lua, engine, id, value.clone())?;
                    // (a behaviour's setter stores through the engine: priced as a write that changed)
                    let state = engine.state.borrow();
                    let placed = engine.reflection.is_a(class, "BasePart") && state.dom.is_descendant_of(id, engine.world().workspace);
                    let price = crate::cost::write(&engine.reflection, class, engine.reflection.name(name), true, placed) + write_touch(&state, id, name, engine.reflection.name(name), true);
                    state.sched.charge(price);
                    cross_set(engine, &state, id, name, price, true);
                    return Ok(());
                }
            }
            let Some(property) = property else {
                return raise(format!("Unable to assign property {}. Property is read only", key_text(key)));
            };
            let info = engine.reflection.property(property).clone();
            if info.read_only {
                return raise(format!("Unable to assign property {}. Property is read only", key_text(key)));
            }
            let coerced = convert::coerce(lua, engine, &info, value)?;
            script_set_property(engine, id, name, coerced);
            Ok(())
        }
        _ => Err(not_a_member(engine, id, key)),
    }
}

/// RawSet with notifications (signals queue; hooks run) on an engine handle.
pub fn set_property(engine: &Engine, id: InstanceId, name: NameId, value: Value) -> bool {
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    core::raw_set(&mut state, &engine.reflection, &hooks, id, name, value)
}

/// What a script's write adds to its price for the cache the engine's write path runs in (crate::cost): the first touch of the
/// instance in the frame (a read's miss), and for a changed write the property's share of a dirtied instance on top of it. A write
/// that changed nothing pays only the first touch. The property's first write in the frame runs the engine's code for it cold
/// (cost.rs FIRST_WRITE), changed or not.
fn write_touch(state: &State, id: InstanceId, name: NameId, property: &str, changed: bool) -> f64 {
    let (class, serial) = (state.dom.at(id).class, state.dom.at(id).serial);
    let mut touched = state.sched.touched.borrow_mut();
    let first = touched.first_use(crate::cost::MemberUse::Write, name);
    let touch = touched.touch(id.index, class, serial);
    if changed && touch > 0.0 {
        first + touch + crate::cost::cold_write_extra(touched.kind(class), property)
    } else {
        first + touch
    }
}

/// A CFrame a script writes to a part's CFrame or a joint's C0 / C1, as a client stores it: its rotation orthonormalized once it
/// is off by more than the client lets pass (world::frame::stored_rotation). Other values, and CFrames written elsewhere (a
/// model's WorldPivot keeps a scaled rotation, measured), pass as they are.
fn cframe_as_stored(engine: &Engine, id: InstanceId, name: NameId, value: Value) -> Value {
    use rbx_dom_weak::types::{self as dom, Variant};
    let Value::Dom(variant) = &value else { return value };
    let Variant::CFrame(cframe) = &**variant else { return value };
    let class = engine.state.borrow().dom.at(id).class;
    let property = engine.reflection.name(name);
    let held = (name == crate::world::cframe_name(engine) && engine.reflection.is_a(class, "BasePart"))
        || (matches!(property, "C0" | "C1") && engine.reflection.is_a(class, "JointInstance"));
    if !held {
        return value;
    }
    let o = &cframe.orientation;
    let rows = [[o.x.x, o.x.y, o.x.z], [o.y.x, o.y.y, o.y.z], [o.z.x, o.z.y, o.z.z]];
    let stored = crate::world::frame::stored_rotation(rows);
    if stored == rows {
        return value;
    }
    let row = |r: [f32; 3]| dom::Vector3::new(r[0], r[1], r[2]);
    Value::Dom(Rc::new(Variant::CFrame(dom::CFrame::new(cframe.position, dom::Matrix3::new(row(stored[0]), row(stored[1]), row(stored[2]))))))
}

/// A script's property write: [`set_property`], charged to the work clock by what Studio spends on it
/// (crate::cost::write: the property, whether it changed, whether the instance is in the workspace).
pub fn script_set_property(engine: &Engine, id: InstanceId, name: NameId, value: Value) -> bool {
    let value = cframe_as_stored(engine, id, name, value);
    // (a client looks a Text write up for translation: AutoLocalize on and a letter in it)
    let client = !engine.options.edit_mode() && !engine.options.server();
    let looked_up = match &value {
        Value::String(text) if crate::cost::text_localization_in(&engine.options.place_prices, engine.options.place_id, 1, client).is_some() && engine.reflection.name(name) == "Text" => {
            let text = String::from_utf8_lossy(text.as_bytes());
            text.chars().any(char::is_alphabetic).then(|| text.chars().count())
        }
        _ => None,
    };
    let changed = set_property(engine, id, name, value);
    let state = engine.state.borrow();
    let class = state.dom.at(id).class;
    let placed = changed && engine.reflection.is_a(class, "BasePart") && state.dom.is_descendant_of(id, engine.world().workspace);
    let price = crate::cost::write(&engine.reflection, class, engine.reflection.name(name), changed, placed)
        * crate::cost::place_write_scale(&engine.options.place_prices, engine.options.place_id, engine.reflection.name(name), placed);
    let mut paid = price + write_touch(&state, id, name, engine.reflection.name(name), changed);
    state.sched.touched.borrow_mut().count(|ops| {
        ops.writes += 1;
        ops.changed += u64::from(changed);
    });
    // (the frame's first geometry write on a part in the Workspace, and its first switch of an effect, run the engine's code for
    // them cold; an effect switched cold reads more: cost.rs Path::Geometry, Path::Toggle, TOGGLE_COLD)
    if changed {
        let property = engine.reflection.name(name);
        let mut touched = state.sched.touched.borrow_mut();
        let cold = touched.take_last_cold();
        if placed && matches!(property, "CFrame" | "Position" | "Orientation" | "Rotation" | "Size") {
            touched.count(|ops| ops.geometry += 1);
            paid += touched.first_path(crate::cost::Path::Geometry);
        } else if property == "Enabled" && ["ParticleEmitter", "Beam", "Trail"].iter().any(|kind| engine.reflection.is_a(class, kind)) {
            touched.count(|ops| ops.toggles += 1);
            paid += touched.first_path(crate::cost::Path::Toggle) + if cold { crate::cost::TOGGLE_COLD } else { 0.0 };
        }
    }
    state.sched.charge(paid);
    cross_set(engine, &state, id, name, paid, changed);
    crate::cost::tally_write(&engine.reflection.class(class).name, engine.reflection.name(name), changed, placed, price);
    if let (true, Some(characters)) = (changed, looked_up) {
        let auto_localize = engine.reflection.name_id("AutoLocalize").and_then(|auto| state.dom.boolean(id, auto)).unwrap_or(true);
        if let (true, Some(lookup)) = (auto_localize, crate::cost::text_localization_in(&engine.options.place_prices, engine.options.place_id, characters, client)) {
            state.sched.charge(lookup);
            crate::boundary::add_cost(lookup);
        }
    }
    changed
}

/// A script's property write, for the boundary profiler (src/boundary.rs): nothing unless profiling, and nothing inside the
/// engine's own Luau pricing a whole call.
fn cross_set(engine: &Engine, state: &State, id: InstanceId, name: NameId, cost: f64, changed: bool) {
    if crate::boundary::active() && !state.sched.is_muted() {
        let class = state.dom.at(id).class;
        let reflection = &engine.reflection;
        crate::boundary::record(crate::boundary::Kind::Set, class, name, id.index, cost, changed, || {
            (reflection.class(reflection.declaring_class(class, name)).name.to_string(), reflection.name(name).to_string())
        });
    }
}

/// SetAttribute on an engine handle (after type validation): stores the value, then runs the attribute hooks
/// when it changed.
pub fn set_attribute(engine: &Engine, id: InstanceId, name: Str, value: Value) {
    let mut state = engine.state.borrow_mut();
    let version = state.dom.at(id).version;
    core::set_attribute(&mut state, &engine.reflection, id, name.clone(), value);
    // (the version moves only when the attribute changed)
    if state.dom.at(id).version != version {
        // (the namecall that got here touched the instance first: a cold one pays the dirtied instance's extra, as a property write does)
        let cold = {
            let mut touched = state.sched.touched.borrow_mut();
            if touched.take_last_cold() { crate::cost::cold_write_extra(touched.kind(state.dom.at(id).class), "SetAttribute") } else { 0.0 }
        };
        state.sched.charge(crate::cost::ATTRIBUTE_CHANGED + cold);
        let hooks = engine.hooks.borrow();
        if !hooks.attribute_changed.is_empty() {
            let text = String::from_utf8_lossy(name.as_bytes()).into_owned();
            for hook in &hooks.attribute_changed {
                hook(&mut state, id, &text);
            }
        }
    } else {
        state.sched.charge(crate::cost::ATTRIBUTE_SAME);
    }
}

/// SetParent on an engine handle.
pub fn set_parent(engine: &Engine, id: InstanceId, parent: Option<InstanceId>) -> Result<(), Flow> {
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    core::set_parent(&mut state, &engine.reflection, &hooks, id, parent).map_err(Flow::Error)
}

/// Signal.FireImmediate: runs handlers now, each on a fresh thread (engine callbacks that do not
/// defer: RunService frame events).
pub fn fire_immediate(lua: &Lua, engine: &Rc<Engine>, signal: SignalId, args: &[Value]) -> LuaResult<()> {
    fire_immediate_costing(lua, engine, signal, args, 0.0)
}

/// [`fire_immediate`], charging the clock `per_handler` seconds before each handler after the first (cost.rs `FRAME_HANDLER`)
pub fn fire_immediate_costing(lua: &Lua, engine: &Rc<Engine>, signal: SignalId, args: &[Value], per_handler: f64) -> LuaResult<()> {
    let handlers = {
        let mut state = engine.state.borrow_mut();
        if !state.signals.has_handlers(signal) {
            return Ok(());
        }
        state.signals.take_handlers(signal)
    };
    let mut parallel = Vec::new();
    for (index, handler) in handlers.into_iter().enumerate() {
        if index > 0 && per_handler > 0.0 {
            engine.state.borrow_mut().sched.charge_dispatch(per_handler);
        }
        let mut values = LuaMultiValue::with_capacity(args.len());
        for value in args {
            values.push_back(convert::to_lua(lua, engine, value)?);
        }
        match handler {
            Handler::Thread(thread) => {
                scheduler::resume(lua, engine, &thread, values);
            }
            Handler::Connection(connection) => {
                let census = crate::census::handler_site(engine, connection);
                let is_parallel = engine.state.borrow().signals.is_parallel(connection);
                let callback = engine.state.borrow_mut().signals.begin_handler(connection);
                if let Some((callback, signal)) = callback {
                    if is_parallel {
                        parallel.push((lua.create_thread(callback)?, values));
                        continue;
                    }
                    let sample = scheduler::sample_handler(engine, signal, &callback);
                    let _scope = crate::microprofile::enabled()
                        .then(|| crate::microprofile::scope(scheduler::handler_label(engine, signal, &callback)));
                    let thread = lua.create_thread(callback)?;
                    if let Some(site) = census {
                        crate::census::handler_ran(site, &thread);
                    }
                    scheduler::resume(lua, engine, &thread, values);
                    scheduler::end_handler_sample(sample, &thread);
                }
            }
        }
    }
    if !parallel.is_empty() {
        super::actors::parallel_phase(lua, engine, parallel);
    }
    Ok(())
}

/// InstanceModule.Destroy.
pub fn destroy(lua: &Lua, engine: &Rc<Engine>, id: InstanceId) -> Result<(), Flow> {
    // (the frame's first destroy, and of each class, runs the engine's code for it cold; an instance in the Workspace since an
    // earlier frame has render, physics and particle state to undo: cost.rs Path::Destroy, first_destroyed, destroy_live. Priced
    // for the whole subtree before it leaves.)
    {
        let state = engine.state.borrow();
        if state.dom.at(id).has(flags::DESTROYED) {
            return Ok(());
        }
        let mut touched = state.sched.touched.borrow_mut();
        let mut cold = touched.first_path(crate::cost::Path::Destroy);
        for node in std::iter::once(id).chain(state.dom.descendants(id)) {
            cold += touched.destroyed(node.index, state.dom.at(node).class);
            touched.count(|ops| ops.destroyed += 1);
        }
        drop(touched);
        state.sched.charge(cold);
        crate::boundary::add_cost(cold);
    }
    destroy_subtree(lua, engine, id)
}

fn destroy_subtree(lua: &Lua, engine: &Rc<Engine>, id: InstanceId) -> Result<(), Flow> {
    let destroying = {
        let state = engine.state.borrow();
        let instance = state.dom.at(id);
        if instance.has(flags::DESTROYED) {
            return Ok(());
        }
        instance.signals.as_ref().and_then(|s| s.event(K::Destroying as NameId))
    };
    // (a GUI object or a part in the DataModel pays more to destroy than its leaving: cost.rs DESTROYED_GUI, DESTROYED_PART)
    let extra = {
        let state = engine.state.borrow();
        let class = state.dom.at(id).class;
        let placed = state.dom.at(id).parent.is_some_and(|parent| crate::services::in_data_model(&state, parent));
        match placed {
            true if engine.reflection.is_a(class, "GuiObject") => crate::cost::DESTROYED_GUI,
            true if engine.reflection.is_a(class, "BasePart") => crate::cost::DESTROYED_PART,
            _ => 0.0,
        }
    };
    if let Some(signal) = destroying {
        // Deferred like every other event (measured in a playing client, Studio 0.744, SignalBehavior Deferred:
        // tools/parity/probes/hierarchy.luau): the handlers run after Destroy returned and see the instance
        // unparented; each instance's are queued just before its own removal events, a child's after its parent's.
        core::fire(&mut engine.state.borrow_mut(), signal, Vec::new());
    }
    let children = {
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        core::begin_destroy(&mut state, &engine.reflection, &hooks, id)?
    };
    for child in children {
        destroy_subtree(lua, engine, child)?;
    }
    let mut state = engine.state.borrow_mut();
    state.sched.charge(crate::cost::DESTROYED + extra);
    crate::boundary::add_cost(crate::cost::DESTROYED + extra);
    core::finish_destroy(&mut state, id);
    Ok(())
}

/// What a PivotTo of `id` adds for the engine's code and the instances it carries being cold (cost.rs Path::Pivot, PIVOT_COLD_PART,
/// PIVOT_COLD_ATTACHED): nothing out of the Workspace. Called right after the call's own touch of `id`, which says whether it was cold.
pub fn pivot_cold(engine: &Engine, id: InstanceId) -> f64 {
    let state = engine.state.borrow();
    let mut touched = state.sched.touched.borrow_mut();
    let root_cold = touched.take_last_cold();
    if !touched.workspace.is_some_and(|workspace| state.dom.is_descendant_of(id, workspace)) {
        return 0.0;
    }
    touched.count(|ops| ops.pivots += 1);
    let mut seconds = touched.first_path(crate::cost::Path::Pivot);
    // (the call touched the root: cold then, and not moved the frame before either)
    if root_cold && !touched.last_cold_was_recent() {
        seconds += touched.moved_cold(state.dom.at(id).class, crate::cost::PIVOT_COLD_PART, crate::cost::PIVOT_COLD_ATTACHED);
    }
    for node in state.dom.descendants(id) {
        if touched.mark_moved(node.index) {
            seconds += touched.moved_cold(state.dom.at(node).class, crate::cost::PIVOT_COLD_PART, crate::cost::PIVOT_COLD_ATTACHED);
        }
    }
    seconds
}

/// InstanceModule.Clone (materializing the subtree first).
pub fn clone(engine: &Engine, id: InstanceId) -> Option<InstanceId> {
    if matches!(engine.state.borrow().dom.raw_get(id, K::Archivable as NameId), Some(Value::Bool(false))) {
        return None;
    }
    // (the template's root was touched by the call itself: whether that touch found it cold)
    let root_cold = engine.state.borrow().sched.touched.borrow_mut().take_last_cold();
    project::materialize_deep(engine, id);
    let mut state = engine.state.borrow_mut();
    let copy = core::clone_instance(&mut state, &engine.reflection, id);
    if let Some(copy) = copy {
        let mut work: f64 = std::iter::once(copy).chain(state.dom.descendants(copy)).map(|node| crate::cost::cloned(&engine.reflection, state.dom.at(node).class)).sum();
        // (the frame's first clone, and of each class it holds, runs the engine's code for it cold; a template no code touched this
        // frame is read cold: cost.rs Path::Clone, first_cloned, clone_cold. The copy is made now, so it is hot.)
        {
            let mut touched = state.sched.touched.borrow_mut();
            work += touched.first_path(crate::cost::Path::Clone);
            work += touched.cloned_source(id.index, state.dom.at(id).class, Some(root_cold));
            for node in state.dom.descendants(id) {
                work += touched.cloned_source(node.index, state.dom.at(node).class, None);
            }
            for node in std::iter::once(copy).chain(state.dom.descendants(copy)) {
                touched.mark(node.index);
                touched.count(|ops| ops.cloned += 1);
            }
        }
        state.sched.charge(work);
        crate::boundary::add_cost(work);
    }
    copy
}

fn name_equals(state: &State, id: InstanceId, name: Option<&[u8]>) -> bool {
    name == Some(state.dom.at(id).name.as_bytes())
}

/// A string argument (other types match no name).
fn name_arg(value: &LuaValue) -> Option<LuaString> {
    match value {
        LuaValue::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// FindFirstChild (recursive: each child's subtree in turn, after the direct children).
pub fn find_first_child(engine: &Engine, id: InstanceId, name: Option<&[u8]>, recursive: bool) -> Option<InstanceId> {
    find_first_child_counting(engine, id, name, recursive, &mut 0)
}

/// [`find_first_child`], adding to `looked` the instances whose names it compared (a script's find pays for them: [`charge_find`])
pub fn find_first_child_counting(engine: &Engine, id: InstanceId, name: Option<&[u8]>, recursive: bool, looked: &mut usize) -> Option<InstanceId> {
    if recursive {
        // (a lazily imported instance whose children are not made: searched in its plan)
        let unmade = project::find_unmade(&mut engine.state.borrow_mut(), &engine.reflection, id, |child, _| Some(child) == name);
        if let Some(found) = unmade {
            return found.map(|child| reached(engine, child));
        }
    }
    materialize(engine, id);
    let found = {
        let state = engine.state.borrow();
        let children = &state.dom.at(id).children;
        let index = children.iter().position(|&child| name_equals(&state, child, name));
        *looked += index.map_or(children.len(), |index| index + 1);
        index.map(|index| children[index])
    };
    if let Some(child) = found {
        return Some(reached(engine, child));
    }
    if !recursive && let Some(name) = name && let Some(made) = made_on_request(engine, id, name) {
        return Some(made);
    }
    if recursive {
        let children = engine.state.borrow().dom.at(id).children.clone();
        for child in children {
            if let Some(found) = find_first_child_counting(engine, child, name, true, looked) {
                return Some(found);
            }
        }
    }
    None
}

/// What a script's FindFirstChild pays for the instances it looked at beyond the call's price (cost.rs FIND_CHILD past the first
/// child, FIND_NODE for each node of a recursive find)
pub fn charge_find(engine: &Engine, looked: usize, recursive: bool) {
    let seconds = if recursive { looked as f64 * crate::cost::FIND_NODE } else { looked.saturating_sub(1) as f64 * crate::cost::FIND_CHILD };
    if seconds > 0.0 {
        engine.state.borrow().sched.charge(seconds);
        crate::boundary::add_cost(seconds);
    }
}

fn class_is_a(engine: &Engine, class: ClassId, name: Option<&[u8]>) -> bool {
    match name.and_then(|n| std::str::from_utf8(n).ok()) {
        Some(name) => engine.reflection.is_a(class, name),
        None => false,
    }
}

pub fn find_first_child_which_is_a(engine: &Engine, id: InstanceId, name: Option<&[u8]>, recursive: bool) -> Option<InstanceId> {
    if recursive {
        let unmade = project::find_unmade(&mut engine.state.borrow_mut(), &engine.reflection, id, |_, class| class_is_a(engine, class, name));
        if let Some(found) = unmade {
            return found;
        }
    }
    materialize(engine, id);
    let found = find_child_where(engine, id, |state, child| class_is_a(engine, state.dom.at(child).class, name));
    if found.is_some() {
        return found;
    }
    if recursive {
        let children = engine.state.borrow().dom.at(id).children.clone();
        for child in children {
            if let Some(found) = find_first_child_which_is_a(engine, child, name, true) {
                return Some(found);
            }
        }
    }
    None
}

fn optional_instance(lua: &Lua, engine: &Engine, id: Option<InstanceId>) -> Ret {
    Ok(values([match id {
        Some(id) => instance_value(lua, engine, id)?,
        None => LuaValue::Nil,
    }]))
}

fn instance_list(lua: &Lua, engine: &Engine, ids: &[InstanceId]) -> Ret {
    let table = lua.create_table_with_capacity(ids.len(), 0)?;
    for &id in ids {
        table.raw_push(instance_value(lua, engine, id)?)?;
    }
    Ok(values([LuaValue::Table(table)]))
}

fn string_arg(args: &Args, index: usize) -> Option<Str> {
    args.string(index).map(|s| Str::new(&s.as_bytes()))
}

/// Why SetAttribute refuses an attribute name, as Roblox words it (measured in a playing client, Studio 0.744,
/// tools/parity/probes/instances.luau, raised without a position): empty, over 100 characters, a byte other than
/// letters, digits and `_ - . /` (a space, `$`, the first byte of `Ü`), or the `RBX` prefix (case matters: `rbxname`
/// goes). Not measured: which check comes first for a name that fails several.
pub fn attribute_name_error(name: &[u8]) -> Option<String> {
    let shown = String::from_utf8_lossy(name);
    if name.is_empty() {
        return Some("Attribute name cannot be empty (\"\")".into());
    }
    if name.len() > 100 {
        return Some(format!("Attribute name exceeds 100 character limit (\"{shown}\")"));
    }
    let legal = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    if let Some(illegal) = shown.chars().find(|&c| !legal(c)) {
        // (Roblox prints the first illegal byte, a lone lead byte for `Ü`; the message here is UTF-8, so it holds the
        // whole character)
        return Some(format!("Attribute name contains illegal character '{illegal}' (\"{shown}\")"));
    }
    if name.starts_with(b"RBX") {
        return Some(format!("Attempt to assign attribute {shown}. CoreScript permission required"));
    }
    None
}

/// The attribute datatypes SetAttribute accepts (by typeof).
const ATTRIBUTE_TYPES: &[&str] = &[
    "string", "boolean", "number", "UDim", "UDim2", "BrickColor", "Color3", "Vector2", "Vector3", "CFrame", "EnumItem",
    "NumberSequence", "ColorSequence", "NumberRange", "Rect", "Font", "Instance",
];

/// An attribute's value as a script reads it: an Instance as an InstanceHandle (types.rs), anything else as itself
fn attribute_to_lua(lua: &Lua, engine: &Engine, value: &Value) -> LuaResult<LuaValue> {
    match value {
        Value::Instance(target) => super::types::instance_handle(lua, engine, *target),
        other => convert::to_lua(lua, engine, other),
    }
}

/// WaitForChild: the existing child, or yields until a child with the name is added (a
/// ChildAdded connection resumes the thread), the timeout passes, or forever (with the
/// "Infinite yield possible" warning after 5 s).
fn wait_for_child(lua: &Lua, engine: &Rc<Engine>, id: InstanceId, args: &Args) -> Ret {
    let name = args.get(1).clone();
    let name_string = name_arg(&name);
    let name_bytes = name_string.as_ref().map(|s| s.as_bytes());
    if let Some(existing) = find_first_child(engine, id, name_bytes.as_deref(), false) {
        return optional_instance(lua, engine, Some(existing));
    }
    if let LuaValue::String(s) = &name {
        // (services port: the yieldable lookup runs a pending rur.json clientGui mount first)
        if let Some(child) = super::globals::missing_child_yieldable(lua, engine, id, &s.as_bytes())? {
            return optional_instance(lua, engine, Some(child));
        }
    }
    super::refs::admit(engine, super::refs::Hold::Wait, "WaitForChild")?;
    let thread = lua.current_thread();
    let done = Rc::new(Cell::new(false));
    let connection_slot = Rc::new(Cell::new(None));
    let callback = {
        let engine = engine.clone();
        let done = done.clone();
        let connection_slot = connection_slot.clone();
        let thread = thread.clone();
        let name = name.clone();
        lua.create_function(move |lua, child: LuaValue| {
            let Some(child_id) = instance_id(&child) else {
                return Ok(());
            };
            let wanted = name_arg(&name);
            let wanted = wanted.as_ref().map(|s| s.as_bytes());
            if done.get() || !name_equals(&engine.state.borrow(), child_id, wanted.as_deref()) {
                return Ok(());
            }
            done.set(true);
            if let Some(connection) = connection_slot.get() {
                engine.state.borrow_mut().signals.disconnect(connection);
            }
            scheduler::resume(lua, &engine, &thread, LuaMultiValue::from_iter([child]));
            Ok(())
        })?
    };
    let signal = core::event_signal(&mut engine.state.borrow_mut(), &engine.reflection, id, K::ChildAdded as NameId);
    let noted = crate::census::active(engine).then(|| callback.clone());
    let connection = engine.state.borrow_mut().signals.connect(signal, callback, false);
    if let Some(callback) = noted {
        crate::census::connected(lua, connection, &callback);
    }
    connection_slot.set(Some(connection));
    match args.number(2) {
        Some(timeout) => {
            let engine_ref = engine.clone();
            let done = done.clone();
            let thread = thread.clone();
            let body = lua.create_function(move |lua, ()| {
                if done.get() {
                    return Ok(());
                }
                done.set(true);
                engine_ref.state.borrow_mut().signals.disconnect(connection);
                scheduler::resume(lua, &engine_ref, &thread, LuaMultiValue::from_iter([LuaValue::Nil]));
                Ok(())
            })?;
            let timer = lua.create_thread(body)?;
            engine.state.borrow_mut().sched.schedule_task(TimerKind::Delay, timeout, timer, Vec::new());
        }
        None => {
            let label = format!(
                "Infinite yield possible on '{}:WaitForChild(\"{}\")'",
                full_name(engine, id),
                key_text(&name)
            );
            let engine_ref = engine.clone();
            let body = lua.create_function(move |_, ()| {
                if !done.get() {
                    engine_ref.warn(label.clone());
                }
                Ok(())
            })?;
            let timer = lua.create_thread(body)?;
            engine.state.borrow_mut().sched.schedule_task(TimerKind::Delay, 5.0, timer, Vec::new());
        }
    }
    Err(Flow::Yield)
}

/// Registers the Instance userdata type (raw tagged userdata with C metamethods, lua/raw.rs) and
/// the base Instance behaviours.
pub fn register(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let iter = wrapped(lua, engine, |_, _| raise_at_caller("attempt to iterate over a Instance value"))?;
    let metatable = lua.create_table()?;
    metatable.raw_set("__type", "Instance")?;
    metatable.raw_set("__metatable", "The metatable is locked")?;
    metatable.raw_set("__iter", iter)?;
    raw::set_metatable(lua, metatable)?;
    install_base(&mut engine.behaviors.borrow_mut(), &engine.reflection);
    Ok(())
}

/// The Instance metatable is locked when it is installed (kept for Engine.Start's call order).
pub fn lock(_lua: &Lua, _engine: &Engine, _sample: InstanceId) -> LuaResult<()> {
    Ok(())
}

fn method(f: impl Fn(&Lua, &Rc<Engine>, InstanceId, &Args) -> Ret + 'static) -> MethodFn {
    Rc::new(f)
}

fn install_base(behaviors: &mut Behaviors, reflection: &crate::Reflection) {
    let base = reflection.class_id("Instance").expect("Instance class");

    // Getters / setters
    let getter = |f: GetterFn| f;
    let class_name = getter(Rc::new(|lua, engine, id| {
        let class = engine.state.borrow().dom.at(id).class;
        Ok(LuaValue::String(lua.create_string(&*engine.reflection.class(class).name)?))
    }));
    behaviors.add_getter(base, K::ClassName as NameId, class_name.clone());
    behaviors.add_getter(base, K::Parent as NameId, getter(Rc::new(|lua, engine, id| {
        let parent = engine.state.borrow().dom.at(id).parent;
        match parent {
            Some(parent) => instance_value(lua, engine, parent).map_err(Flow::from),
            None => Ok(LuaValue::Nil),
        }
    })));
    let parent_setter: SetterFn = Rc::new(|lua, engine, id, value| {
        let parent = match &value {
            LuaValue::Nil => None,
            LuaValue::UserData(_) if instance_id(&value).is_some() => instance_id(&value),
            _ => {
                // Measured in a playing client (Studio 0.744, tools/parity/probes/hierarchy.luau): Luau's own
                // argument error of the __newindex call, at the script line.
                return raise_at_caller(format!("invalid argument #3 (Instance expected, got {})", typeof_value(engine, &value)));
            }
        };
        set_parent(engine, id, parent)?;
        if parent.is_some() {
            super::actors::start_under(lua, engine, id)?;
        }
        Ok(())
    });
    behaviors.add_setter(base, K::Parent as NameId, parent_setter);
    // A script under an Actor starts when it is enabled, if it is in the DataModel by then
    if let (Some(script), Some(enabled)) = (reflection.class_id("BaseScript"), reflection.name_id("Enabled")) {
        behaviors.add_setter(script, enabled, Rc::new(move |lua, engine, id, value| {
            let LuaValue::Boolean(on) = value else {
                return raise(format!("Unable to assign property Enabled. boolean expected, got {}", typeof_value(engine, &value)));
            };
            set_property(engine, id, enabled, Value::Bool(on));
            if on {
                super::actors::start_under(lua, engine, id)?;
            }
            Ok(())
        }));
    }
    // Script.Source: Studio lets a plugin write it (a game's scripts cannot), and ree has no plugin or game boundary, so a
    // script may. It replaces the source file the instance was mounted from: the next require or start reads the text.
    if let Some(container) = reflection.class_id("LuaSourceContainer") {
        behaviors.add_setter(container, K::Source as NameId, Rc::new(|_, engine, id, value| {
            let LuaValue::String(text) = &value else {
                return raise(format!("Unable to assign property Source. string expected, got {}", typeof_value(engine, &value)));
            };
            let bytes = text.as_bytes().to_vec();
            {
                let mut state = engine.state.borrow_mut();
                let instance = state.dom.at_mut(id);
                instance.set_source(Some(Rc::from(bytes.as_slice())));
                instance.set_source_path(None);
            }
            engine.modules.borrow_mut().forget_bytecode(id);
            set_property(engine, id, K::Source as NameId, Value::String(Str::new(&bytes)));
            Ok(())
        }));
    }
    // Script.RunContext is plugin-writable in Studio as Source is
    if let (Some(script), Some(run_context)) = (reflection.class_id("BaseScript"), reflection.name_id("RunContext")) {
        behaviors.add_setter(script, run_context, Rc::new(move |lua, engine, id, value| {
            let class = engine.state.borrow().dom.at(id).class;
            if let Member::Property { property: Some(property), .. } = engine.resolve_member(class, run_context) {
                let info = engine.reflection.property(property).clone();
                let coerced = convert::coerce(lua, engine, &info, &value)?;
                set_property(engine, id, run_context, coerced);
            }
            Ok(())
        }));
    }
    behaviors.add_getter(base, K::Name as NameId, getter(Rc::new(|lua, engine, id| {
        let state = engine.state.borrow();
        let instance = state.dom.at(id);
        let name = match instance.stored(K::Name as NameId) {
            Some(Value::String(name)) => name.as_bytes().to_vec(),
            Some(_) => Vec::new(),
            None => engine.reflection.class(instance.class).name.as_bytes().to_vec(),
        };
        drop(state);
        Ok(LuaValue::String(lua.create_string(name)?))
    })));
    behaviors.add_setter(base, K::Name as NameId, Rc::new(|lua, engine, id, value| {
        let text = match &value {
            LuaValue::String(s) => Str::new(&s.as_bytes()),
            LuaValue::Number(_) | LuaValue::Integer(_) => {
                Str::new(&lua.coerce_string(value.clone())?.map(|s| s.as_bytes().to_vec()).unwrap_or_default())
            }
            _ => {
                return raise(format!(
                    "Unable to assign property Name. string expected, got {}",
                    typeof_value(engine, &value)
                ));
            }
        };
        set_property(engine, id, K::Name as NameId, Value::String(text));
        Ok(())
    }));

    // Methods
    let get_children = method(|lua, engine, id, _| {
        materialize(engine, id);
        let children = engine.state.borrow().dom.at(id).children.clone();
        instance_list(lua, engine, &children)
    });
    behaviors.add_method(base, K::GetChildren as NameId, get_children.clone());
    behaviors.add_method(base, K::children as NameId, get_children.clone());
    behaviors.add_method(base, K::getChildren as NameId, get_children);
    behaviors.add_method(base, K::GetDescendants as NameId, method(|lua, engine, id, _| {
        project::materialize_deep(engine, id);
        let descendants = engine.state.borrow().dom.descendants(id);
        // (the frame's first GetDescendants runs its code cold: cost.rs Path::Descendants)
        {
            let state = engine.state.borrow();
            let first = {
                let mut touched = state.sched.touched.borrow_mut();
                touched.count(|ops| ops.queries += 1);
                touched.first_path(crate::cost::Path::Descendants)
            };
            if first > 0.0 {
                state.sched.charge(first);
                crate::boundary::add_cost(first);
            }
        }
        instance_list(lua, engine, &descendants)
    }));
    let find_first_child_method = method(|lua, engine, id, args| {
        // A missing name raises (measured in a playing client, Studio 0.744, tools/parity/probes/instances.luau; no
        // position): FindFirstChild(nil) is a script's bug in Roblox, not a miss
        if args.get(1).is_nil() {
            return raise("Argument 1 missing or nil");
        }
        let name = name_arg(args.get(1));
        let (recursive, mut looked) = (args.boolean(2), 0);
        let found = find_first_child_counting(engine, id, name.as_ref().map(|s| s.as_bytes()).as_deref(), recursive, &mut looked);
        charge_find(engine, looked, recursive);
        optional_instance(lua, engine, found)
    });
    behaviors.add_method(base, K::FindFirstChild as NameId, find_first_child_method.clone());
    behaviors.add_method(base, K::findFirstChild as NameId, find_first_child_method);
    behaviors.add_method(base, K::FindFirstChildOfClass as NameId, method(|lua, engine, id, args| {
        materialize(engine, id);
        let wanted = args.string(1).map(|s| s.to_string_lossy());
        let found = find_child_where(engine, id, |state, child| wanted.as_deref() == Some(&*engine.reflection.class(state.dom.at(child).class).name));
        optional_instance(lua, engine, found)
    }));
    behaviors.add_method(base, K::FindFirstChildWhichIsA as NameId, method(|lua, engine, id, args| {
        let name = name_arg(args.get(1));
        let found = find_first_child_which_is_a(engine, id, name.as_ref().map(|s| s.as_bytes()).as_deref(), args.boolean(2));
        optional_instance(lua, engine, found)
    }));
    // (not enabled in Roblox: measured likewise, `FindFirstDescendant is not enabled`)
    behaviors.add_method(base, K::FindFirstDescendant as NameId, method(|_, _, _, _| raise("FindFirstDescendant is not enabled")));
    behaviors.add_method(base, K::FindFirstAncestor as NameId, method(|lua, engine, id, args| {
        let found = {
            let state = engine.state.borrow();
            let mut current = state.dom.at(id).parent;
            let wanted = name_arg(args.get(1));
            let wanted = wanted.as_ref().map(|s| s.as_bytes());
            while let Some(node) = current {
                if name_equals(&state, node, wanted.as_deref()) {
                    break;
                }
                current = state.dom.at(node).parent;
            }
            current
        };
        optional_instance(lua, engine, found)
    }));
    behaviors.add_method(base, K::FindFirstAncestorOfClass as NameId, method(|lua, engine, id, args| {
        let found = {
            let state = engine.state.borrow();
            let wanted = args.string(1).map(|s| s.to_string_lossy());
            let mut current = state.dom.at(id).parent;
            while let Some(node) = current {
                if wanted.as_deref() == Some(&*engine.reflection.class(state.dom.at(node).class).name) {
                    break;
                }
                current = state.dom.at(node).parent;
            }
            current
        };
        optional_instance(lua, engine, found)
    }));
    behaviors.add_method(base, K::FindFirstAncestorWhichIsA as NameId, method(|lua, engine, id, args| {
        let found = {
            let state = engine.state.borrow();
            let mut current = state.dom.at(id).parent;
            let wanted = name_arg(args.get(1));
            let wanted = wanted.as_ref().map(|s| s.as_bytes());
            while let Some(node) = current {
                if class_is_a(engine, state.dom.at(node).class, wanted.as_deref()) {
                    break;
                }
                current = state.dom.at(node).parent;
            }
            current
        };
        optional_instance(lua, engine, found)
    }));
    let is_a = method(|_, engine, id, args| {
        let class = engine.state.borrow().dom.at(id).class;
        let wanted = name_arg(args.get(1));
        let wanted = wanted.as_ref().map(|s| s.as_bytes());
        Ok(values([LuaValue::Boolean(class_is_a(engine, class, wanted.as_deref()))]))
    });
    behaviors.add_method(base, K::IsA as NameId, is_a.clone());
    behaviors.add_method(base, K::isA as NameId, is_a.clone());
    // Roblox's Object, Instance's superclass and EditableImage's and EditableMesh's, has ClassName
    // and IsA itself: an EditableImage answers them as any instance does (Instance keeps its own, found
    // first for its classes)
    if let Some(object) = reflection.class_id("Object") {
        behaviors.add_getter(object, K::ClassName as NameId, class_name);
        behaviors.add_method(object, K::IsA as NameId, is_a.clone());
        behaviors.add_method(object, K::isA as NameId, is_a);
    }
    let is_descendant_of = method(|_, engine, id, args| {
        let result = match args.instance(1) {
            Some(ancestor) => engine.state.borrow().dom.is_descendant_of(id, ancestor),
            // every instance is a descendant of nil (measured likewise: IsDescendantOf(nil) is true)
            None => args.get(1).is_nil(),
        };
        Ok(values([LuaValue::Boolean(result)]))
    });
    behaviors.add_method(base, K::IsDescendantOf as NameId, is_descendant_of.clone());
    behaviors.add_method(base, K::isDescendantOf as NameId, is_descendant_of);
    behaviors.add_method(base, K::IsAncestorOf as NameId, method(|_, engine, id, args| {
        let result = match args.instance(1) {
            Some(descendant) => engine.state.borrow().dom.is_descendant_of(descendant, id),
            None => false,
        };
        Ok(values([LuaValue::Boolean(result)]))
    }));
    behaviors.add_method(base, K::GetFullName as NameId, method(|lua, engine, id, _| {
        Ok(values([LuaValue::String(lua.create_string(full_name(engine, id))?)]))
    }));
    let clone_method = method(|lua, engine, id, _| {
        // A service cannot be cloned (measured likewise: `Lighting cannot be cloned`, `Players cannot be cloned`)
        let (class, name) = {
            let state = engine.state.borrow();
            let instance = state.dom.at(id);
            (instance.class, instance.name.to_str_lossy().into_owned())
        };
        if engine.reflection.class(class).service {
            return raise(format!("{name} cannot be cloned"));
        }
        optional_instance(lua, engine, clone(engine, id))
    });
    behaviors.add_method(base, K::Clone as NameId, clone_method.clone());
    behaviors.add_method(base, K::clone as NameId, clone_method);
    let destroy_method = method(|lua, engine, id, _| {
        destroy(lua, engine, id)?;
        Ok(none())
    });
    behaviors.add_method(base, K::Destroy as NameId, destroy_method.clone());
    behaviors.add_method(base, K::destroy as NameId, destroy_method);
    let remove = method(|_, engine, id, _| {
        set_parent(engine, id, None)?;
        Ok(none())
    });
    behaviors.add_method(base, K::Remove as NameId, remove.clone());
    behaviors.add_method(base, K::remove as NameId, remove);
    behaviors.add_method(base, K::ClearAllChildren as NameId, method(|lua, engine, id, _| {
        // Last child first (measured in a playing client, Studio 0.744: tools/parity/probes/hierarchy.luau)
        let children = engine.state.borrow().dom.at(id).children.clone();
        for child in children.into_iter().rev() {
            destroy(lua, engine, child)?;
        }
        Ok(none())
    }));
    behaviors.add_method(base, K::WaitForChild as NameId, method(wait_for_child));
    behaviors.add_method(base, K::GetPropertyChangedSignal as NameId, method(|lua, engine, id, args| {
        let class = engine.state.borrow().dom.at(id).class;
        let name = args.string(1).and_then(|s| engine.reflection.name_id_bytes(&s.as_bytes()));
        let member = name.map_or(Member::None, |name| engine.resolve_member(class, name));
        let (Member::Property { .. }, Some(name)) = (member, name) else {
            return raise(format!("{} is not a valid property name.", key_text(args.get(1))));
        };
        let signal = core::property_signal(&mut engine.state.borrow_mut(), &engine.reflection, id, name);
        Ok(values([signal_value(lua, engine, signal)?]))
    }));
    behaviors.add_method(base, K::GetAttribute as NameId, method(|lua, engine, id, args| {
        // (a number names the attribute of its text, as SetAttribute takes it: measured likewise, GetAttribute(5) reads
        // what SetAttribute(5, 1) set)
        let name = match args.get(1) {
            LuaValue::Number(_) | LuaValue::Integer(_) => lua.coerce_string(args.get(1).clone())?.map(|s| Str::new(&s.as_bytes())),
            _ => string_arg(args, 1),
        };
        let Some(name) = name else { return Ok(values([LuaValue::Nil])) };
        let value = engine.state.borrow().dom.at(id).attribute(name.as_bytes()).cloned();
        Ok(values([match value {
            Some(value) => attribute_to_lua(lua, engine, &value)?,
            None => LuaValue::Nil,
        }]))
    }));
    behaviors.add_method(base, K::GetAttributes as NameId, method(|lua, engine, id, _| {
        let attributes = engine.state.borrow().dom.at(id).attributes.as_slice().to_vec();
        let listed = attributes.len() as f64 * crate::cost::ATTRIBUTE_LISTED;
        engine.state.borrow().sched.charge(listed);
        crate::boundary::add_cost(listed);
        let table = lua.create_table()?;
        for (name, value) in attributes {
            table.raw_set(lua.create_string(name.as_bytes())?, attribute_to_lua(lua, engine, &value)?)?;
        }
        Ok(values([LuaValue::Table(table)]))
    }));
    behaviors.add_method(base, K::SetAttribute as NameId, method(|lua, engine, id, args| {
        let value = args.get(2);
        // (an Instance, or the InstanceHandle a read gave, is kept by reference: Studio 0.744 takes both)
        let handle = super::types::instance_handle_of(value);
        if !value.is_nil() && handle.is_none() {
            let kind = typeof_value(engine, value);
            if !ATTRIBUTE_TYPES.contains(&kind.as_str()) {
                // (Roblox names a table an Array and a function a Function: measured, {} and print; other values by
                // typeof, buffer in lower case. Not measured: a table with string keys, here a Dictionary)
                let kind = match value {
                    LuaValue::Table(table) if table.raw_len() > 0 || table.clone().pairs::<LuaValue, LuaValue>().next().is_none() => "Array".to_string(),
                    LuaValue::Table(_) => "Dictionary".to_string(),
                    LuaValue::Function(_) => "Function".to_string(),
                    _ => kind,
                };
                return raise(format!("{kind} is not a supported attribute type"));
            }
        }
        let name = match args.get(1) {
            LuaValue::String(s) => Str::new(&s.as_bytes()),
            other => Str::from(lua.coerce_string(other.clone())?.map(|s| s.to_string_lossy()).unwrap_or_default()),
        };
        if let Some(message) = attribute_name_error(name.as_bytes()) {
            return raise(message);
        }
        let converted = match handle {
            Some(target) => Value::Instance(target),
            None => convert::from_lua(value),
        };
        set_attribute(engine, id, name, converted);
        Ok(none())
    }));
    behaviors.add_method(base, K::GetAttributeChangedSignal as NameId, method(|lua, engine, id, args| {
        let name = string_arg(args, 1).unwrap_or_default();
        let signal = core::attribute_signal(&mut engine.state.borrow_mut(), id, &name);
        Ok(values([signal_value(lua, engine, signal)?]))
    }));
    behaviors.add_method(base, K::AddTag as NameId, method(|_, engine, id, args| {
        if let Some(tag) = string_arg(args, 1) {
            core::add_tag(&mut engine.state.borrow_mut(), id, tag);
        }
        Ok(none())
    }));
    behaviors.add_method(base, K::RemoveTag as NameId, method(|_, engine, id, args| {
        if let Some(tag) = string_arg(args, 1) {
            core::remove_tag(&mut engine.state.borrow_mut(), id, &tag);
        }
        Ok(none())
    }));
    behaviors.add_method(base, K::HasTag as NameId, method(|_, engine, id, args| {
        let has = string_arg(args, 1).is_some_and(|tag| engine.state.borrow().dom.at(id).tags.contains(&tag));
        Ok(values([LuaValue::Boolean(has)]))
    }));
    behaviors.add_method(base, K::GetTags as NameId, method(|lua, engine, id, _| {
        let tags = engine.state.borrow().dom.at(id).tags.clone();
        let table = lua.create_table()?;
        for tag in tags {
            table.raw_push(lua.create_string(tag.as_bytes())?)?;
        }
        Ok(values([LuaValue::Table(table)]))
    }));
    behaviors.add_method(base, K::GetActor as NameId, method(|lua, engine, id, _| {
        let actor = super::actors::actor_of(engine, id);
        optional_instance(lua, engine, actor)
    }));
    behaviors.add_method(base, K::IsPropertyModified as NameId, method(|_, engine, id, args| {
        let name = args.string(1).and_then(|s| engine.reflection.name_id_bytes(&s.as_bytes()));
        let modified = name.is_some_and(|name| engine.state.borrow().dom.at(id).stored(name).is_some());
        Ok(values([LuaValue::Boolean(modified)]))
    }));
    behaviors.add_method(base, K::ResetPropertyToDefault as NameId, method(|_, engine, id, args| {
        let Some(name) = args.string(1).and_then(|s| engine.reflection.name_id_bytes(&s.as_bytes())) else {
            return Ok(none());
        };
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        if state.dom.at_mut(id).unstore(name).is_some() {
            if name == K::Name as NameId {
                state.dom.refresh_name(&engine.reflection, id);
            }
            state.dom.bump_version(id);
            core::notify_changed(&mut state, &engine.reflection, &hooks, id, name, None);
        }
        Ok(none())
    }));
    behaviors.add_method(base, K::GetDebugId as NameId, method(|lua, engine, id, _| {
        let serial = engine.state.borrow().dom.at(id).serial;
        Ok(values([LuaValue::String(lua.create_string(format!("RUR_{serial}"))?)]))
    }));
    behaviors.add_method(base, K::GetStyled as NameId, method(|lua, engine, id, args| {
        let name = args.string(1).and_then(|s| engine.reflection.name_id_bytes(&s.as_bytes()));
        let value = name.and_then(|name| engine.state.borrow().dom.raw_get(id, name).cloned());
        Ok(values([match value {
            Some(value) => convert::to_lua(lua, engine, &value)?,
            None => LuaValue::Nil,
        }]))
    }));
}

/// A deferred resumption of `thread` with `args` (task.defer, module waiters).
pub fn defer_thread(engine: &Engine, thread: LuaThread, args: Vec<Value>) {
    engine.state.borrow_mut().sched.defer(JobTarget::Thread(thread), args);
}

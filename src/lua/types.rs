//! Engine datatypes as userdata (ports of `runtime/engine/enums.luau`, `signal.luau` and the
//! Font / Content / TweenInfo / Random / DateTime parts of `datatypes.luau`): interned
//! Enum / EnumItem, RBXScriptSignal / RBXScriptConnection and the values Lune does not provide.
//! Every type has Roblox's locked metatable and string errors.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use mlua::prelude::*;

use super::{
    Args, Flow, Ret, enum_item_value, enum_value, lock_metatable, none, raise, raise_plain,
    refs::{self, Hold}, signal_value, typeof_value, values, wrapped,
};
use crate::{
    Engine, ScriptError,
    reflection::{EnumId, EnumItemId},
    scheduler,
    signal::{ConnectionId, SignalId},
    value::{ContentValue, FontValue, Str},
};

pub struct LEnumItem(pub EnumItemId);
pub struct LEnum(pub EnumId);
pub struct LEnums;
/// A script's RBXScriptSignal: the signal's slot and the slot's generation when the value was made
/// (a freed instance's signal slots are reused: `signal_of`)
pub struct LSignal(pub SignalId, pub u32);
pub struct LConnection(pub ConnectionId);
pub struct LFont(pub RefCell<FontValue>);
pub struct LContent(pub ContentValue);

pub struct LTweenInfo {
    pub time: LuaValue,
    pub easing_style: EnumItemId,
    pub easing_direction: EnumItemId,
    pub repeat_count: LuaValue,
    pub reverses: bool,
    pub delay_time: LuaValue,
}

impl LTweenInfo {
    pub fn time(&self) -> f64 {
        number_of(&self.time)
    }

    pub fn delay(&self) -> f64 {
        number_of(&self.delay_time)
    }

    pub fn repeats(&self) -> f64 {
        number_of(&self.repeat_count)
    }
}

fn number_of(value: &LuaValue) -> f64 {
    match value {
        LuaValue::Number(n) => *n,
        LuaValue::Integer(n) => *n as f64,
        _ => 0.0,
    }
}

pub struct LDateTime(pub f64);

#[derive(Clone, Copy)]
pub struct RandomState {
    state: u64,
    /// A non-finite seed made the Luau runtime's state NaN: every draw is 0
    dead: bool,
}

pub struct LRandom(pub Rc<Cell<RandomState>>);

/// Every Random a run made, in the order made, while it lives: Context.ReseedRandoms restarts each from
/// the run's seed and its place in that order (a Random made before a checkpoint is every run's).
#[derive(Default)]
pub struct RandomRegistry {
    made: u64,
    live: Vec<(u64, std::rc::Weak<Cell<RandomState>>)>,
}

impl RandomRegistry {
    fn add(&mut self, state: &Rc<Cell<RandomState>>) {
        if self.live.len() >= 64 && self.live.len().is_power_of_two() {
            self.live.retain(|(_, weak)| weak.strong_count() > 0);
        }
        self.live.push((self.made, Rc::downgrade(state)));
        self.made += 1;
    }

    /// Restarts every live Random from `seed` and the order it was made in (as Random.new(n) starts:
    /// `n` mixes both into 52 bits); how many it restarted
    pub fn reseed(&mut self, seed: f64) -> usize {
        self.live.retain(|(_, weak)| weak.strong_count() > 0);
        let base = seed.to_bits();
        for (made, weak) in &self.live {
            if let Some(state) = weak.upgrade() {
                state.set(seed_random(mix_seed(base, *made)));
            }
        }
        self.live.len()
    }
}

/// splitmix64 of the seed's bits and an ordinal, as a whole number below 2^52
fn mix_seed(base: u64, ordinal: u64) -> f64 {
    let mut z = base ^ ordinal.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 12) as f64
}

fn new_random(engine: &Engine, state: RandomState) -> LRandom {
    let cell = Rc::new(Cell::new(state));
    engine.state.borrow_mut().sched.randoms.add(&cell);
    LRandom(cell)
}

fn key_text(key: &LuaValue) -> String {
    match key {
        LuaValue::String(s) => s.to_string_lossy(),
        LuaValue::Number(n) => n.to_string(),
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Boolean(b) => b.to_string(),
        LuaValue::Nil => "nil".into(),
        other => other.type_name().into(),
    }
}

fn read_only(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<LuaFunction> {
    wrapped(lua, engine, |_, args| {
        let args = Args::new(args);
        raise(format!("{} cannot be assigned to", key_text(args.get(1))))
    })
}

fn engine_of(lua: &Lua) -> Rc<Engine> {
    lua.app_data_ref::<Rc<Engine>>().expect("engine app data").clone()
}

fn string_of(lua: &Lua, value: &LuaValue) -> LuaResult<String> {
    Ok(match lua.coerce_string(value.clone())? {
        Some(s) => s.to_string_lossy(),
        None => match value {
            LuaValue::Nil => "nil".into(),
            LuaValue::Boolean(b) => b.to_string(),
            other => format!("{}", other.to_string().unwrap_or_default()),
        },
    })
}

/// Registers every engine datatype's metatable and returns the script libraries.
pub fn register(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    register_enums(lua, engine)?;
    register_signals(lua, engine)?;
    register_font(lua, engine)?;
    register_content(lua, engine)?;
    register_tween_info(lua, engine)?;
    register_random(lua, engine)?;
    register_date_time(lua, engine)?;
    register_instance_handle(lua, engine)?;
    Ok(())
}

// InstanceHandle

/// An attribute's Instance as GetAttribute and GetAttributes hand it back (Studio 0.744, measured in a playing client:
/// tools/parity/probes/instance_attributes.luau): a userdata of type InstanceHandle, a new one at every read (never
/// rawequal to another, so a table key of one is not found by the next), that prints the instance's current Name (after
/// a rename, an unparenting and a Destroy alike), has no members (`Name is not a valid member of InstanceHandle`, for
/// methods too) and equals a handle of the same instance but never the instance. It keeps its instance's userdata as its
/// user value, so the instance lives while a script keeps the handle.
pub struct LInstanceHandle(pub crate::InstanceId);

pub fn instance_handle(lua: &Lua, engine: &Engine, id: crate::InstanceId) -> LuaResult<LuaValue> {
    if engine.state.borrow().dom.get(id).is_none() {
        return Ok(LuaValue::Nil);
    }
    let handle = lua.create_any_userdata(LInstanceHandle(id))?;
    handle.set_user_value(super::instance_value(lua, engine, id)?)?;
    Ok(LuaValue::UserData(handle))
}

/// The instance an InstanceHandle refers to
pub fn instance_handle_of(value: &LuaValue) -> Option<crate::InstanceId> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LInstanceHandle>().ok().map(|handle| handle.0),
        _ => None,
    }
}

fn register_instance_handle(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    // (an assignment raises the same text: not measured)
    let no_member = |lua: &Lua, engine: &Rc<Engine>| {
        wrapped(lua, engine, |_, args| {
            let args = Args::new(args);
            raise(format!("{} is not a valid member of InstanceHandle", key_text(args.get(1))))
        })
    };
    let index = no_member(lua, engine)?;
    let newindex = no_member(lua, engine)?;
    let e = engine.clone();
    let tostring = lua.create_function(move |_, this: LuaValue| {
        let state = e.state.borrow();
        let name = instance_handle_of(&this).and_then(|id| state.dom.get(id)).map(|instance| instance.name.to_str_lossy().into_owned());
        Ok(name.unwrap_or_default())
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| {
        Ok(matches!((instance_handle_of(&a), instance_handle_of(&b)), (Some(a), Some(b)) if a == b))
    })?;
    lua.register_userdata_type::<LInstanceHandle>(|registry| {
        registry.add_meta_field("__type", "InstanceHandle");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;
    let sample = lua.create_any_userdata(LInstanceHandle(crate::InstanceId { index: u32::MAX, generation: u32::MAX }))?;
    lock_metatable(lua, &sample)
}

// Enums

fn enum_item_of(value: &LuaValue) -> Option<EnumItemId> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LEnumItem>().ok().map(|item| item.0),
        _ => None,
    }
}

fn register_enums(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    // EnumItem
    let is_a = lua.create_function(|lua, (item, name): (LuaValue, LuaValue)| {
        let engine = engine_of(lua);
        let Some(item) = enum_item_of(&item) else { return Ok(false) };
        Ok(match name {
            LuaValue::String(name) => engine.reflection.enum_item_type_name(item).as_bytes() == &*name.as_bytes(),
            _ => false,
        })
    })?;
    engine.lua.borrow_mut().enum_item_is_a = Some(is_a.clone());
    let e = engine.clone();
    let item_index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let Some(item) = enum_item_of(args.get(0)) else { return raise("invalid EnumItem") };
        let info = e.reflection.enum_item_info(item);
        let key = args.get(1);
        if let LuaValue::String(key) = key {
            match &*key.as_bytes() {
                b"Name" => return Ok(values([LuaValue::String(lua.create_string(&*info.name)?)])),
                b"Value" => return Ok(values([LuaValue::Number(info.value)])),
                b"EnumType" => return Ok(values([enum_value(lua, &e, info.enum_type)?])),
                b"IsA" => return Ok(values([LuaValue::Function(is_a.clone())])),
                _ => {}
            }
        }
        raise(format!(
            "{} is not a valid member of \"{}\"",
            key_text(key),
            e.reflection.enum_item_path(item)
        ))
    })?;
    let item_tostring = lua.create_function(|lua, item: LuaAnyUserData| {
        let engine = engine_of(lua);
        Ok(engine.reflection.enum_item_path(item.borrow::<LEnumItem>()?.0))
    })?;
    let newindex = read_only(lua, engine)?;
    lua.register_userdata_type::<LEnumItem>(|registry| {
        registry.add_meta_field("__type", "EnumItem");
        registry.add_meta_field("__index", item_index);
        registry.add_meta_field("__newindex", newindex.clone());
        registry.add_meta_field("__tostring", item_tostring);
    })?;

    // Enum
    let members = lua.create_table()?;
    members.set(
        "GetEnumItems",
        lua.create_function(|lua, this: LuaValue| {
            let engine = engine_of(lua);
            let LuaValue::UserData(ud) = this else { return Err(LuaError::runtime("Expected ':' not '.' calling member function GetEnumItems")) };
            let id = ud.borrow::<LEnum>()?.0;
            let items = engine.reflection.enums[id as usize].items.clone();
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for item in items {
                table.raw_push(enum_item_value(lua, &engine, item)?)?;
            }
            Ok(table)
        })?,
    )?;
    members.set(
        "FromName",
        lua.create_function(|lua, (this, name): (LuaAnyUserData, LuaValue)| {
            let engine = engine_of(lua);
            let id = this.borrow::<LEnum>()?.0;
            let LuaValue::String(name) = name else { return Ok(LuaValue::Nil) };
            match engine.reflection.enums[id as usize].by_name.get(&*name.to_string_lossy()) {
                Some(&item) => enum_item_value(lua, &engine, item),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    members.set(
        "FromValue",
        lua.create_function(|lua, (this, value): (LuaAnyUserData, LuaValue)| {
            let engine = engine_of(lua);
            let id = this.borrow::<LEnum>()?.0;
            let number = number_of(&value);
            if number.fract() != 0.0 {
                return Ok(LuaValue::Nil);
            }
            match engine.reflection.enums[id as usize].by_value.get(&(number as i64)) {
                Some(&item) => enum_item_value(lua, &engine, item),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    engine.lua.borrow_mut().enum_members = Some(members.clone());
    let e = engine.clone();
    let enum_index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid Enum") };
        let id = ud.borrow::<LEnum>()?.0;
        let info = &e.reflection.enums[id as usize];
        if let LuaValue::String(key) = args.get(1) {
            if let Some(&item) = info.by_name.get(&*key.to_string_lossy()) {
                return Ok(values([enum_item_value(lua, &e, item)?]));
            }
            let member: LuaValue = members.raw_get(key.clone())?;
            if !member.is_nil() {
                return Ok(values([member]));
            }
        }
        raise(format!("{} is not a valid member of \"Enum.{}\"", key_text(args.get(1)), info.name))
    })?;
    let enum_tostring = lua.create_function(|lua, this: LuaAnyUserData| {
        let engine = engine_of(lua);
        Ok(engine.reflection.enums[this.borrow::<LEnum>()?.0 as usize].name.to_string())
    })?;
    lua.register_userdata_type::<LEnum>(|registry| {
        registry.add_meta_field("__type", "Enum");
        registry.add_meta_field("__index", enum_index);
        registry.add_meta_field("__newindex", newindex.clone());
        registry.add_meta_field("__tostring", enum_tostring);
    })?;

    // The Enum global
    let get_enums = lua.create_function(|lua, ()| {
        let engine = engine_of(lua);
        let table = lua.create_table()?;
        for id in 0..engine.reflection.enums.len() {
            table.raw_push(enum_value(lua, &engine, id as EnumId)?)?;
        }
        Ok(table)
    })?;
    let e = engine.clone();
    let global_index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        if let LuaValue::String(key) = args.get(1) {
            if &*key.as_bytes() == b"GetEnums" {
                return Ok(values([LuaValue::Function(get_enums.clone())]));
            }
            if let Some(id) = e.reflection.enum_id(&key.to_string_lossy()) {
                return Ok(values([enum_value(lua, &e, id)?]));
            }
        }
        raise(format!("{} is not a valid member of \"Enum\"", key_text(args.get(1))))
    })?;
    let global_tostring = lua.create_function(|_, _: LuaValue| Ok("Enums"))?;
    lua.register_userdata_type::<LEnums>(|registry| {
        registry.add_meta_field("__type", "Enums");
        registry.add_meta_field("__index", global_index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", global_tostring);
    })?;

    let global = lua.create_any_userdata(LEnums)?;
    lock_metatable(lua, &global)?;
    engine.lua.borrow_mut().enums_global = Some(global);
    if let Some(&first) = engine.reflection.enum_items.first().map(|_| &0) {
        if let LuaValue::UserData(item) = enum_item_value(lua, engine, first)? {
            lock_metatable(lua, &item)?;
        }
        if let LuaValue::UserData(item) = enum_value(lua, engine, 0)? {
            lock_metatable(lua, &item)?;
        }
    }
    Ok(())
}

// Signals

/// The signal of a script's RBXScriptSignal value (None: not one); `Some(None)` when the signal's
/// instance was freed since (nothing fires it any more).
fn signal_of(engine: &Engine, value: &LuaValue) -> Option<Option<SignalId>> {
    match value {
        LuaValue::UserData(ud) => {
            let signal = ud.borrow::<LSignal>().ok()?;
            Some(engine.state.borrow().signals.is_current(signal.0, signal.1).then_some(signal.0))
        }
        _ => None,
    }
}

/// `Signal.Wait()`: "missing argument #1 to 'Wait' (RBXScriptSignal expected)" at the caller (measured,
/// tools/parity/probes/errors.luau); with a value that is no signal, the invalid-argument text Connect gives (not measured)
fn wait_misuse<T>(engine: &Engine, value: &LuaValue) -> Result<T, Flow> {
    if value.is_nil() {
        return super::raise_at_caller("missing argument #1 to 'Wait' (RBXScriptSignal expected)");
    }
    super::raise_at_caller(format!("invalid argument #1 to 'Wait' (RBXScriptSignal expected, got {})", typeof_value(engine, value)))
}

fn connect(lua: &Lua, engine: &Rc<Engine>, args: LuaMultiValue, once: bool) -> Ret {
    connect_as(lua, engine, args, once, false)
}

/// Connect, Once and ConnectParallel (`parallel`: a handler of the signal's parallel phase, which only an Actor's script may add).
fn connect_as(lua: &Lua, engine: &Rc<Engine>, args: LuaMultiValue, once: bool, parallel: bool) -> Ret {
    let args = Args::new(args);
    let Some(signal) = signal_of(engine, args.get(0)) else {
        // `Signal.Connect(f)`: "invalid argument #1 to 'Connect' (RBXScriptSignal expected, got function)" at the caller
        // (measured, tools/parity/probes/errors.luau)
        let verb = if parallel { "ConnectParallel" } else if once { "Once" } else { "Connect" };
        return super::raise_at_caller(format!(
            "invalid argument #1 to '{verb}' (RBXScriptSignal expected, got {})",
            typeof_value(engine, args.get(0))
        ));
    };
    // A server's scripts may read RunService.RenderStepped but not connect to it (Connect and Once measured, a Studio 0.744
    // server's game script: tools/parity/probes/run_service_sides.luau; ConnectParallel takes the same path, Wait is not
    // measured and waits). PreRender and BindToRenderStep are taken there and never run, as here.
    if let Some(id) = signal {
        if engine.options.server() && engine.state.borrow().signals.get(id).label.as_bytes() == b"RunService.RenderStepped" {
            return raise("RenderStepped event can only be used from local scripts");
        }
    }
    let LuaValue::Function(callback) = args.get(1).clone() else {
        // Roblox raises nothing: it logs the message and returns a connection that is not connected, which firing the
        // signal skips (measured in a playing client, Studio 0.744: tools/parity/probes/signals.luau)
        engine.output("error", "Attempt to connect failed: Passed value is not a function", None);
        let userdata = lua.create_any_userdata(LConnection(ConnectionId::DISCONNECTED))?;
        return Ok(values([LuaValue::UserData(userdata)]));
    };
    if parallel && !super::actors::runs_in_actor(&callback) {
        // (Studio's message for a call from the mux, which is no script: "Function Event.ConnectParallel can only be used inside scripts";
        // what a script outside an Actor gets was not measured)
        return raise("Function Event.ConnectParallel can only be used inside scripts");
    }
    let noted = crate::census::active(engine).then(|| callback.clone());
    engine.state.borrow().sched.charge(crate::cost::CONNECT);
    if crate::boundary::active() && !engine.state.borrow().sched.is_muted() {
        crate::boundary::enter(lua.current_thread().state());
        let name = signal.map_or_else(|| "signal".to_string(), |signal| engine.state.borrow().signals.get(signal).name.to_str_lossy().into_owned());
        let verb = if parallel { "ConnectParallel" } else if once { "Once" } else { "Connect" };
        crate::boundary::record_named(crate::boundary::Kind::Event, "RBXScriptSignal", &format!("{verb}({name})"), crate::cost::CONNECT);
    }
    let connection = match signal {
        Some(signal) => engine.state.borrow_mut().signals.connect_with(signal, callback, once, parallel),
        None => ConnectionId::DISCONNECTED,
    };
    if let Some(callback) = noted {
        crate::census::connected(lua, connection, &callback);
    }
    // (a profile keeps where it was connected: its handlers' dispatch and runs are reported by that site)
    if crate::profiler::attribution::on()
        && connection != ConnectionId::DISCONNECTED
        && let Some(signal) = signal
    {
        let label = engine.state.borrow().signals.get(signal).label.to_str_lossy().into_owned();
        crate::profiler::attribution::connected(connection.slot(), &label);
    }
    let userdata = lua.create_any_userdata(LConnection(connection))?;
    Ok(values([LuaValue::UserData(userdata)]))
}

fn register_signals(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let members = lua.create_table()?;
    let e = engine.clone();
    let connect_fn = wrapped(lua, engine, move |lua, args| connect(lua, &e, args, false))?;
    let e = engine.clone();
    let once_fn = wrapped(lua, engine, move |lua, args| connect(lua, &e, args, true))?;
    let e = engine.clone();
    let wait_fn = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let Some(signal) = signal_of(&e, args.get(0)) else {
            return wait_misuse(&e, args.get(0));
        };
        if let Some(signal) = signal {
            refs::admit(&e, Hold::Wait, "Wait")?;
            e.state.borrow_mut().signals.wait(signal, lua.current_thread());
        }
        Err(Flow::Yield)
    })?;
    members.set("Connect", connect_fn.clone())?;
    let e = engine.clone();
    let connect_parallel_fn = wrapped(lua, engine, move |lua, args| connect_as(lua, &e, args, false, true))?;
    members.set("ConnectParallel", connect_parallel_fn.clone())?;
    members.set("Once", once_fn)?;
    members.set("Wait", wait_fn.clone())?;
    // The lowercase names a client answers (measured: connect, wait and connectParallel, not once;
    // tools/parity/probes/camelcase.luau)
    members.set("connect", connect_fn)?;
    members.set("wait", wait_fn)?;
    members.set("connectParallel", connect_parallel_fn)?;
    engine.lua.borrow_mut().signal_members = Some(members.clone());
    let index = wrapped(lua, engine, move |_, args| {
        let args = Args::new(args);
        let member: LuaValue = match args.get(1) {
            LuaValue::String(key) => members.raw_get(key.clone())?,
            _ => LuaValue::Nil,
        };
        if member.is_nil() {
            return raise(format!("{} is not a valid member of RBXScriptSignal", key_text(args.get(1))));
        }
        Ok(values([member]))
    })?;
    let tostring = lua.create_function(|lua, this: LuaAnyUserData| {
        let engine = engine_of(lua);
        let signal = this.borrow::<LSignal>()?;
        let state = engine.state.borrow();
        Ok(match state.signals.is_current(signal.0, signal.1) {
            true => format!("Signal {}", state.signals.get(signal.0).name),
            false => "Signal".to_string(),
        })
    })?;
    let newindex = read_only(lua, engine)?;
    lua.register_userdata_type::<LSignal>(|registry| {
        registry.add_meta_field("__type", "RBXScriptSignal");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex.clone());
        registry.add_meta_field("__tostring", tostring);
    })?;

    let connection_members = lua.create_table()?;
    let disconnect = lua.create_function(|lua, this: LuaValue| {
        let engine = engine_of(lua);
        if let LuaValue::UserData(ud) = this {
            if let Ok(connection) = ud.borrow::<LConnection>() {
                let mut state = engine.state.borrow_mut();
                state.sched.charge(crate::cost::DISCONNECT);
                if crate::boundary::active() && !state.sched.is_muted() {
                    crate::boundary::enter(lua.current_thread().state());
                    crate::boundary::record_named(crate::boundary::Kind::Event, "RBXScriptConnection", "Disconnect", crate::cost::DISCONNECT);
                }
                state.signals.disconnect(connection.0);
            }
        }
        Ok(())
    })?;
    connection_members.set("Disconnect", disconnect.clone())?;
    connection_members.set("disconnect", disconnect)?;
    engine.lua.borrow_mut().connection_members = Some(connection_members.clone());
    let e = engine.clone();
    let connection_index = wrapped(lua, engine, move |_, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid connection") };
        let connection = ud.borrow::<LConnection>()?.0;
        if let LuaValue::String(key) = args.get(1) {
            // (and the lowercase `connected` a client answers: tools/parity/probes/camelcase.luau)
            if matches!(&*key.as_bytes(), b"Connected" | b"connected") {
                let connected = e.state.borrow().signals.is_connected(connection);
                return Ok(values([LuaValue::Boolean(connected)]));
            }
            let member: LuaValue = connection_members.raw_get(key.clone())?;
            if !member.is_nil() {
                return Ok(values([member]));
            }
        }
        raise(format!("{} is not a valid member of RBXScriptConnection", key_text(args.get(1))))
    })?;
    let connection_tostring = lua.create_function(|_, _: LuaValue| Ok("Connection"))?;
    lua.register_userdata_type::<LConnection>(|registry| {
        registry.add_meta_field("__type", "RBXScriptConnection");
        registry.add_meta_field("__index", connection_index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", connection_tostring);
    })?;
    // Method calls (`signal:Connect(f)`, `connection:Disconnect()`) dispatch in C to the unwrapped
    // functions (lua/raw.rs table_namecall); indexing keeps the wrapped ones.
    let signal_calls = lua.create_table()?;
    let e = engine.clone();
    let raw_connect = lua.create_function(move |lua, args| super::encode(lua, connect(lua, &e, args, false)))?;
    let e = engine.clone();
    let raw_once = lua.create_function(move |lua, args| super::encode(lua, connect(lua, &e, args, true)))?;
    let e = engine.clone();
    let raw_wait = lua.create_function(move |lua, args: LuaMultiValue| {
        let args = Args::new(args);
        let outcome = match signal_of(&e, args.get(0)) {
            Some(Some(signal)) => refs::admit(&e, Hold::Wait, "Wait").and_then(|()| {
                e.state.borrow_mut().signals.wait(signal, lua.current_thread());
                Err(Flow::Yield)
            }),
            Some(None) => Err(Flow::Yield),
            None => wait_misuse(&e, args.get(0)),
        };
        super::encode(lua, outcome)
    })?;
    let e = engine.clone();
    let raw_connect_parallel = lua.create_function(move |lua, args| super::encode(lua, connect_as(lua, &e, args, false, true)))?;
    for (name, function) in [
        ("Connect", &raw_connect),
        ("ConnectParallel", &raw_connect_parallel),
        ("connectParallel", &raw_connect_parallel),
        ("connect", &raw_connect),
        ("Once", &raw_once),
        ("Wait", &raw_wait),
        ("wait", &raw_wait),
    ] {
        signal_calls.raw_set(name, function.clone())?;
    }
    let connection_calls = lua.create_table()?;
    let members = engine.lua.borrow().connection_members.clone().expect("connection members");
    connection_calls.raw_set("Disconnect", members.raw_get::<LuaValue>("Disconnect")?)?;
    connection_calls.raw_set("disconnect", members.raw_get::<LuaValue>("disconnect")?)?;
    let connection_properties = lua.create_table()?;
    connection_properties.raw_set("Connected", "boolean")?;
    connection_properties.raw_set("connected", "boolean")?;

    // Lock both metatables
    let signal = engine.state.borrow_mut().signals.create(Str::from("Probe"));
    if let LuaValue::UserData(sample) = signal_value(lua, engine, signal)? {
        lock_metatable(lua, &sample)?;
        super::raw::set_table_namecall(lua, &sample, signal_calls, "RBXScriptSignal", lua.create_table()?)?;
    }
    let probe = lua.create_function(|_, ()| Ok(()))?;
    let connection = engine.state.borrow_mut().signals.connect(signal, probe, false);
    let sample = lua.create_any_userdata(LConnection(connection))?;
    lock_metatable(lua, &sample)?;
    super::raw::set_table_namecall(lua, &sample, connection_calls, "RBXScriptConnection", connection_properties)?;
    engine.state.borrow_mut().signals.disconnect(connection);
    Ok(())
}

/// A standalone signal (Mouse events, BindableEvent.Event before an instance owns it).
pub fn new_signal(engine: &Engine, name: &str) -> SignalId {
    engine.state.borrow_mut().signals.create(Str::from(name))
}

// Font

fn font_of(value: &LuaValue) -> Option<FontValue> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LFont>().ok().map(|f| f.0.borrow().clone()),
        _ => None,
    }
}

fn coerce_named_enum(lua: &Lua, engine: &Engine, enum_name: &str, value: &LuaValue) -> Option<EnumItemId> {
    let id = engine.reflection.enum_id(enum_name)?;
    super::convert::coerce_enum(lua, engine, id, value)
}

fn make_font(lua: &Lua, font: FontValue) -> LuaResult<LuaValue> {
    Ok(LuaValue::UserData(lua.create_any_userdata(LFont(RefCell::new(font)))?))
}

fn register_font(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let e = engine.clone();
    let index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let Some(font) = font_of(args.get(0)) else { return raise("invalid Font") };
        if let LuaValue::String(key) = args.get(1) {
            match &*key.as_bytes() {
                b"Family" => return Ok(values([LuaValue::String(lua.create_string(font.family.as_bytes())?)])),
                b"Weight" => return Ok(values([enum_item_value(lua, &e, font.weight)?])),
                b"Style" => return Ok(values([enum_item_value(lua, &e, font.style)?])),
                b"Bold" => {
                    return Ok(values([LuaValue::Boolean(e.reflection.enum_item_info(font.weight).value >= 600.0)]));
                }
                _ => {}
            }
        }
        raise(format!("{} is not a valid member of Font", key_text(args.get(1))))
    })?;
    let e = engine.clone();
    let newindex = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid Font") };
        let font = ud.borrow::<LFont>()?;
        let value = args.get(2);
        let key = match args.get(1) {
            LuaValue::String(key) => key.to_string_lossy(),
            other => key_text(other),
        };
        let mut data = font.0.borrow_mut();
        match (key.as_str(), value) {
            ("Family", LuaValue::String(family)) => data.family = Str::new(&family.as_bytes()),
            ("Weight", _) => {
                if let Some(item) = coerce_named_enum(lua, &e, "FontWeight", value) {
                    data.weight = item;
                }
            }
            ("Style", _) => {
                if let Some(item) = coerce_named_enum(lua, &e, "FontStyle", value) {
                    data.style = item;
                }
            }
            ("Bold", _) => {
                let name = if matches!(value, LuaValue::Nil | LuaValue::Boolean(false)) { "Regular" } else { "Bold" };
                if let Some(item) = e.reflection.enum_item("FontWeight", name) {
                    data.weight = item;
                }
            }
            _ => return raise(format!("{key} is not a valid member of Font")),
        }
        Ok(none())
    })?;
    let tostring = lua.create_function(|lua, this: LuaAnyUserData| {
        let engine = engine_of(lua);
        let font = this.borrow::<LFont>()?;
        let font = font.0.borrow();
        Ok(format!(
            "Font {{ Family = {}, Weight = {}, Style = {} }}",
            font.family,
            engine.reflection.enum_item_info(font.weight).name,
            engine.reflection.enum_item_info(font.style).name
        ))
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| Ok(matches!((font_of(&a), font_of(&b)), (Some(a), Some(b)) if a == b)))?;
    lua.register_userdata_type::<LFont>(|registry| {
        registry.add_meta_field("__type", "Font");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;

    let library = lua.create_table()?;
    let weight = |lua: &Lua, engine: &Engine, value: &LuaValue, function: &str| -> Result<EnumItemId, Flow> {
        if value.is_nil() {
            return Ok(engine.reflection.enum_item("FontWeight", "Regular").unwrap_or(0));
        }
        coerce_named_enum(lua, engine, "FontWeight", value).ok_or_else(|| {
            Flow::Error(ScriptError::at_caller(format!(
                "invalid argument #2 to '{function}' (Enum.FontWeight expected, got {})",
                typeof_value(engine, value)
            )))
        })
    };
    let style = |lua: &Lua, engine: &Engine, value: &LuaValue, function: &str| -> Result<EnumItemId, Flow> {
        if value.is_nil() {
            return Ok(engine.reflection.enum_item("FontStyle", "Normal").unwrap_or(0));
        }
        coerce_named_enum(lua, engine, "FontStyle", value).ok_or_else(|| {
            Flow::Error(ScriptError::at_caller(format!(
                "invalid argument #3 to '{function}' (Enum.FontStyle expected, got {})",
                typeof_value(engine, value)
            )))
        })
    };
    let e = engine.clone();
    library.set(
        "new",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let Some(family) = args.string(0) else {
                return raise_plain("invalid argument #1 to 'new' (string expected)");
            };
            let font = FontValue {
                family: Str::new(&family.as_bytes()),
                weight: weight(lua, &e, args.get(1), "new")?,
                style: style(lua, &e, args.get(2), "new")?,
            };
            Ok(values([make_font(lua, font)?]))
        })?,
    )?;
    let e = engine.clone();
    library.set(
        "fromEnum",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let Some(item) = coerce_named_enum(lua, &e, "Font", args.get(0)) else {
                return raise_plain("invalid argument #1 to 'fromEnum' (Enum.Font expected)");
            };
            // Roblox's documentation: "Throws an error when called with Enum.Font.Unknown" (the
            // message is not measured in Studio)
            if &*e.reflection.enum_item_info(item).name == "Unknown" {
                return raise_plain("invalid argument #1 to 'fromEnum' (Enum.Font.Unknown has no font face)");
            }
            Ok(values([make_font(lua, font_from_enum(&e, item))?]))
        })?,
    )?;
    let e = engine.clone();
    library.set(
        "fromName",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let name = string_of(lua, args.get(0))?;
            let font = FontValue {
                family: Str::from(format!("rbxasset://fonts/families/{name}.json")),
                weight: weight(lua, &e, args.get(1), "fromName")?,
                style: style(lua, &e, args.get(2), "fromName")?,
            };
            Ok(values([make_font(lua, font)?]))
        })?,
    )?;
    let e = engine.clone();
    library.set(
        "fromId",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let id = string_of(lua, args.get(0))?;
            let font = FontValue {
                family: Str::from(format!("rbxassetid://{id}")),
                weight: weight(lua, &e, args.get(1), "fromId")?,
                style: style(lua, &e, args.get(2), "fromId")?,
            };
            Ok(values([make_font(lua, font)?]))
        })?,
    )?;
    engine.lua.borrow_mut().libraries.insert("Font", library);
    if let LuaValue::UserData(sample) = make_font(lua, FontValue { family: Str::default(), weight: 0, style: 0 })? {
        lock_metatable(lua, &sample)?;
    }
    Ok(())
}

/// Font.fromEnum: the face data/font-enums.json records for an Enum.Font item (LegacyArial
/// otherwise).
pub fn font_from_enum(engine: &Engine, item: EnumItemId) -> FontValue {
    let reflection = &engine.reflection;
    let name = &reflection.enum_item_info(item).name;
    match reflection.font_enum_faces.get(&**name) {
        Some((family, weight, style)) => FontValue {
            family: Str::from(&**family),
            weight: reflection.enum_item("FontWeight", weight).unwrap_or(0),
            style: reflection.enum_item("FontStyle", style).unwrap_or(0),
        },
        None => FontValue {
            family: Str::from("rbxasset://fonts/families/LegacyArial.json"),
            weight: reflection.enum_item("FontWeight", "Regular").unwrap_or(0),
            style: reflection.enum_item("FontStyle", "Normal").unwrap_or(0),
        },
    }
}

/// FontInternal.ToEnum: the Enum.Font whose face equals `font`, or Enum.Font.Unknown.
pub fn font_to_enum(engine: &Engine, font: &FontValue) -> Option<EnumItemId> {
    let reflection = &engine.reflection;
    let weight = &*reflection.enum_item_info(font.weight).name;
    let style = &*reflection.enum_item_info(font.style).name;
    let mut matches: Vec<&str> = reflection
        .font_enum_faces
        .iter()
        .filter(|(_, (family, w, s))| family.as_bytes() == font.family.as_bytes() && &**w == weight && &**s == style)
        .map(|(name, _)| &**name)
        .collect();
    // The Luau runtime takes the first in table order; prefer a stable choice
    matches.sort_unstable();
    match matches.first() {
        Some(name) => reflection.enum_item("Font", name),
        None => reflection.enum_item("Font", "Unknown"),
    }
}

// Content

fn content_of(value: &LuaValue) -> Option<ContentValue> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LContent>().ok().map(|c| c.0.clone()),
        _ => None,
    }
}

fn register_content(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let e = engine.clone();
    let index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let Some(content) = content_of(args.get(0)) else { return raise("invalid Content") };
        if let LuaValue::String(key) = args.get(1) {
            match &*key.as_bytes() {
                b"Uri" => {
                    return Ok(values([match &content {
                        ContentValue::Uri(uri) => LuaValue::String(lua.create_string(uri.as_bytes())?),
                        _ => LuaValue::Nil,
                    }]));
                }
                b"Object" => {
                    return Ok(values([match content {
                        ContentValue::Object(id) => super::instance_value(lua, &e, id)?,
                        _ => LuaValue::Nil,
                    }]));
                }
                b"SourceType" => {
                    let name = match content {
                        ContentValue::Uri(_) => "Uri",
                        ContentValue::Object(_) => "Object",
                        ContentValue::None => "None",
                    };
                    return Ok(values([match e.reflection.enum_item("ContentSourceType", name) {
                        Some(item) => enum_item_value(lua, &e, item)?,
                        None => LuaValue::Nil,
                    }]));
                }
                _ => {}
            }
        }
        raise(format!("{} is not a valid member of Content", key_text(args.get(1))))
    })?;
    let tostring = lua.create_function(|lua, this: LuaAnyUserData| {
        let engine = engine_of(lua);
        Ok(match &this.borrow::<LContent>()?.0 {
            ContentValue::Uri(uri) => format!("Content{{SourceType=Uri, Uri={uri}}}"),
            ContentValue::Object(id) => {
                let name = engine.state.borrow().dom.get(*id).map(|i| i.name.to_string()).unwrap_or_default();
                format!("Content{{SourceType=Object, Object={name}}}")
            }
            ContentValue::None => "Content{SourceType=None}".into(),
        })
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| Ok(matches!((content_of(&a), content_of(&b)), (Some(a), Some(b)) if a == b)))?;
    let newindex = read_only(lua, engine)?;
    lua.register_userdata_type::<LContent>(|registry| {
        registry.add_meta_field("__type", "Content");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;
    let library = lua.create_table()?;
    library.set(
        "fromUri",
        lua.create_function(|lua, uri: LuaValue| {
            let text = string_of(lua, &uri)?;
            lua.create_any_userdata(LContent(ContentValue::Uri(Str::from(text))))
        })?,
    )?;
    let none_value = lua.create_any_userdata(LContent(ContentValue::None))?;
    lock_metatable(lua, &none_value)?;
    // Roblox's documentation: fromAssetId(0) is Content.none, a non-finite id and fromObject(nil)
    // throw (the messages are not measured in Studio)
    let none = none_value.clone();
    library.set(
        "fromAssetId",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let id = args.get(0);
            if let LuaValue::Number(number) = id {
                if !number.is_finite() {
                    return raise_plain("invalid argument #1 to 'fromAssetId' (finite number expected)");
                }
            }
            if matches!(id, LuaValue::Integer(0)) || matches!(id, LuaValue::Number(n) if *n == 0.0) {
                return Ok(values([LuaValue::UserData(none.clone())]));
            }
            let text = string_of(lua, id)?;
            Ok(values([LuaValue::UserData(
                lua.create_any_userdata(LContent(ContentValue::Uri(Str::from(format!("rbxassetid://{text}")))))?,
            )]))
        })?,
    )?;
    library.set(
        "fromObject",
        wrapped(lua, engine, |lua, args| {
            let args = Args::new(args);
            let content = match args.get(0) {
                LuaValue::Nil => return raise_plain("invalid argument #1 to 'fromObject' (Object expected, got nil)"),
                LuaValue::UserData(ud) => match super::raw::instance_of_ud(ud) {
                    Some(instance) => ContentValue::Object(instance),
                    None => ContentValue::None,
                },
                _ => ContentValue::None,
            };
            Ok(values([LuaValue::UserData(lua.create_any_userdata(LContent(content))?)]))
        })?,
    )?;
    library.set("none", none_value)?;
    engine.lua.borrow_mut().libraries.insert("Content", library);
    Ok(())
}

// TweenInfo

fn register_tween_info(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let e = engine.clone();
    let index = wrapped(lua, engine, move |lua, args| {
        let args = Args::new(args);
        let LuaValue::UserData(ud) = args.get(0) else { return raise("invalid TweenInfo") };
        let info = ud.borrow::<LTweenInfo>()?;
        if let LuaValue::String(key) = args.get(1) {
            match &*key.as_bytes() {
                b"Time" => return Ok(values([info.time.clone()])),
                b"EasingStyle" => return Ok(values([enum_item_value(lua, &e, info.easing_style)?])),
                b"EasingDirection" => return Ok(values([enum_item_value(lua, &e, info.easing_direction)?])),
                b"RepeatCount" => return Ok(values([info.repeat_count.clone()])),
                b"Reverses" => return Ok(values([LuaValue::Boolean(info.reverses)])),
                b"DelayTime" => return Ok(values([info.delay_time.clone()])),
                _ => {}
            }
        }
        raise(format!("{} is not a valid member of TweenInfo", key_text(args.get(1))))
    })?;
    // "Time:0.1 DelayTime:0.333333 RepeatCount:-1 Reverses:True ..." (Studio 0.744): the times with 6 significant digits,
    // the flag capitalized
    let tostring = lua.create_function(|lua, this: LuaAnyUserData| {
        let engine = engine_of(lua);
        let info = this.borrow::<LTweenInfo>()?;
        let time = |value: &LuaValue| match value {
            LuaValue::Number(n) => Ok(lune_roblox::datatypes::types::format_g(*n, 6)),
            LuaValue::Integer(n) => Ok(lune_roblox::datatypes::types::format_g(*n as f64, 6)),
            other => string_of(lua, other),
        };
        Ok(format!(
            "Time:{} DelayTime:{} RepeatCount:{} Reverses:{} EasingDirection:{} EasingStyle:{}",
            time(&info.time)?,
            time(&info.delay_time)?,
            string_of(lua, &info.repeat_count)?,
            if info.reverses { "True" } else { "False" },
            engine.reflection.enum_item_info(info.easing_direction).name,
            engine.reflection.enum_item_info(info.easing_style).name,
        ))
    })?;
    let eq = lua.create_function(|_, (a, b): (LuaValue, LuaValue)| {
        let (LuaValue::UserData(a), LuaValue::UserData(b)) = (a, b) else { return Ok(false) };
        let (Ok(a), Ok(b)) = (a.borrow::<LTweenInfo>(), b.borrow::<LTweenInfo>()) else { return Ok(false) };
        Ok(a.time == b.time
            && a.easing_style == b.easing_style
            && a.easing_direction == b.easing_direction
            && a.repeat_count == b.repeat_count
            && a.reverses == b.reverses
            && a.delay_time == b.delay_time)
    })?;
    let newindex = read_only(lua, engine)?;
    lua.register_userdata_type::<LTweenInfo>(|registry| {
        registry.add_meta_field("__type", "TweenInfo");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
    })?;
    let library = lua.create_table()?;
    let e = engine.clone();
    library.set(
        "new",
        lua.create_function(move |lua, args: LuaMultiValue| {
            let args = Args::new(args);
            let or = |value: &LuaValue, default: f64| match value {
                LuaValue::Nil | LuaValue::Boolean(false) => LuaValue::Number(default),
                other => other.clone(),
            };
            let style = if args.get(1).is_nil() { None } else { coerce_named_enum(lua, &e, "EasingStyle", args.get(1)) };
            let direction =
                if args.get(2).is_nil() { None } else { coerce_named_enum(lua, &e, "EasingDirection", args.get(2)) };
            let info = LTweenInfo {
                time: or(args.get(0), 1.0),
                easing_style: style.or_else(|| e.reflection.enum_item("EasingStyle", "Quad")).unwrap_or(0),
                easing_direction: direction.or_else(|| e.reflection.enum_item("EasingDirection", "Out")).unwrap_or(0),
                repeat_count: or(args.get(3), 0.0),
                reverses: matches!(args.get(4), LuaValue::Boolean(true)),
                delay_time: or(args.get(5), 0.0),
            };
            let userdata = lua.create_any_userdata(info)?;
            lock_metatable(lua, &userdata)?;
            Ok(userdata)
        })?,
    )?;
    engine.lua.borrow_mut().libraries.insert("TweenInfo", library);
    Ok(())
}

// Random: Luau's math.random generator (PCG32), one state per object (measured: Random.new(7)
// NextNumber 0.87370859248613331, NextInteger(1, 100) 2, ...).

const MULTIPLIER: u64 = 6364136223846793005;
const INCREMENT: u64 = 105;
const TWO32: f64 = 4294967296.0;

impl RandomState {
    fn next_u32(&mut self) -> u32 {
        if self.dead {
            return 0;
        }
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        xorshifted.rotate_right((old >> 59) as u32)
    }

    /// ldexp(lo | hi << 32, -64) from two draws (lo first): the double conversion rounds once.
    fn next_unit(&mut self) -> f64 {
        let low = self.next_u32() as f64;
        let high = self.next_u32() as f64;
        (high * TWO32 + low) / 18446744073709551616.0
    }

    /// NextNumber: the bounds in order, then `min * (1 - u) + max * u` (measured in a playing client, Studio 0.744,
    /// tools/parity/probes/random.luau: 30 of 30 draws, where `min + (max - min) * u` matched 22; NextNumber(5, 1)
    /// draws from [1, 5]).
    fn next_number(&mut self, min: f64, max: f64) -> f64 {
        let (low, high) = if min > max { (max, min) } else { (min, max) };
        let unit = self.next_unit();
        low * (1.0 - unit) + high * unit
    }

    /// NextInteger: the floored bounds as 64-bit integers (a C cast on x86-64: out of range, the least one), in order;
    /// a range under 2^32 draws once, as math.random does, a wider one is `floor(min * (1 - u) + max * u)` (measured
    /// likewise: NextInteger(-2^40, 2^40) for 10 seeds, where a 64-bit multiply gave one more for 7; NextInteger(-2^63,
    /// 2^63) is -2^63. Not measured: where between 2^32 and 2^41 the wider draw starts).
    fn next_integer(&mut self, min: f64, max: f64) -> f64 {
        let (mut low, mut high) = (int64_cast(min.floor()), int64_cast(max.floor()));
        if low > high {
            std::mem::swap(&mut low, &mut high);
        }
        let range = high as i128 - low as i128 + 1;
        if range < 1i128 << 32 {
            let product = (range as u64) * (self.next_u32() as u64);
            return (low as i128 + (product >> 32) as i128) as f64;
        }
        let unit = self.next_unit();
        (low as f64 * (1.0 - unit) + high as f64 * unit).floor()
    }
}

/// A double as a C cast to a 64-bit integer makes it on x86-64: toward zero, the least integer when out of range.
fn int64_cast(value: f64) -> i64 {
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
        value as i64
    } else {
        i64::MIN
    }
}

/// Luau's `%` and `//` on doubles, for the seeding arithmetic.
fn luau_mod(a: f64, b: f64) -> f64 {
    a - (a / b).floor() * b
}

/// Luau's bit32 argument conversion (non-finite and huge values become 0).
fn luau_u32(value: f64) -> u32 {
    if !value.is_finite() || value.abs() >= 9.2e18 {
        0
    } else {
        (value as i64) as u32
    }
}

/// Measured seeding (pcg32_seed): s = 0; step; s += seed as int64 two's complement (fractional
/// seeds truncate toward zero); step.
pub fn seed_random(seed: f64) -> RandomState {
    let value = if seed < 0.0 { seed.ceil() } else { seed.floor() };
    let (seed_hi, seed_lo) = if value >= 0.0 {
        (luau_mod((value / TWO32).floor(), TWO32), luau_mod(value, TWO32))
    } else {
        let n = -value - 1.0;
        (!luau_u32(luau_mod((n / TWO32).floor(), TWO32)) as f64, !luau_u32(luau_mod(n, TWO32)) as f64)
    };
    let lo = 105.0 + seed_lo;
    let hi = seed_hi + (lo / TWO32).floor();
    let (hi, lo) = (luau_mod(hi, TWO32), luau_mod(lo, TWO32));
    if hi.is_nan() || lo.is_nan() {
        return RandomState { state: 0, dead: true };
    }
    let mut state = RandomState { state: ((hi as u64) << 32) | lo as u64, dead: false };
    state.next_u32();
    state
}

fn random_of(value: &LuaValue) -> Option<RandomState> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LRandom>().ok().map(|r| r.0.get()),
        _ => None,
    }
}

fn with_random<T>(value: &LuaValue, f: impl FnOnce(&mut RandomState) -> T) -> Option<T> {
    let LuaValue::UserData(ud) = value else { return None };
    let random = ud.borrow::<LRandom>().ok()?;
    let mut state = random.0.get();
    let result = f(&mut state);
    random.0.set(state);
    Some(result)
}

fn register_random(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let methods = lua.create_table()?;
    let arg_number = |args: &Args, index: usize, default: f64, name: &str| -> Result<f64, Flow> {
        match args.get(index) {
            LuaValue::Nil => Ok(default),
            LuaValue::Number(n) => Ok(*n),
            LuaValue::Integer(n) => Ok(*n as f64),
            other => raise_plain(format!(
                "invalid argument #{} to '{name}' (number expected, got {})",
                index,
                other.type_name()
            )),
        }
    };
    methods.set(
        "NextNumber",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            let min = arg_number(&args, 1, 0.0, "NextNumber")?;
            let max = arg_number(&args, 2, 1.0, "NextNumber")?;
            let value = with_random(args.get(0), |r| r.next_number(min, max));
            match value {
                Some(value) => Ok(values([LuaValue::Number(value)])),
                None => raise("Expected ':' not '.' calling member function NextNumber"),
            }
        })?,
    )?;
    methods.set(
        "NextInteger",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            let (Some(min), Some(max)) = (args.number(1), args.number(2)) else {
                return raise_plain("invalid argument #2 to 'NextInteger' (number expected)");
            };
            match with_random(args.get(0), |r| r.next_integer(min, max)) {
                Some(value) => Ok(values([LuaValue::Number(value)])),
                None => raise("Expected ':' not '.' calling member function NextInteger"),
            }
        })?,
    )?;
    methods.set(
        "NextUnitVector",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            // Two 32-bit draws in float arithmetic: the angle from the first, z from the second (measured in a playing
            // client, Studio 0.744, tools/parity/probes/random.luau: every component for 10 seeds; ree drew two
            // 64-bit numbers, which also moved every draw after it)
            let Some((first, second)) = with_random(args.get(0), |r| (r.next_u32(), r.next_u32())) else {
                return raise("Expected ':' not '.' calling member function NextUnitVector");
            };
            let (u, v) = ((first as f64 / TWO32) as f32, (second as f64 / TWO32) as f32);
            let angle = std::f32::consts::TAU * u;
            let z = 2.0f32 * v - 1.0;
            let radius = (1.0f32 - z * z).max(0.0).sqrt();
            let (x, y) = (radius * ((angle as f64).cos() as f32), radius * ((angle as f64).sin() as f32));
            Ok(values([LuaValue::Vector(mlua::Vector::new(x, y, z))]))
        })?,
    )?;
    methods.set(
        "Shuffle",
        wrapped(lua, engine, move |_, args| {
            let args = Args::new(args);
            let LuaValue::Table(list) = args.get(1).clone() else {
                return raise_plain("invalid argument #1 to 'Shuffle' (table expected)");
            };
            let length = list.len()? as i64;
            // (measured likewise: Shuffle({1, nil, 3}) raises this, without a position, and draws nothing)
            for index in 1..=length {
                if list.get::<LuaValue>(index)?.is_nil() {
                    return raise_plain("Attempted to shuffle table containing nil values. This is not allowed because the table length may change.");
                }
            }
            let mut index = length;
            while index >= 2 {
                let Some(other) = with_random(args.get(0), |r| r.next_integer(1.0, index as f64)) else {
                    return raise("Expected ':' not '.' calling member function Shuffle");
                };
                let other = other as i64;
                let a: LuaValue = list.get(index)?;
                let b: LuaValue = list.get(other)?;
                list.set(index, b)?;
                list.set(other, a)?;
                index -= 1;
            }
            Ok(none())
        })?,
    )?;
    let cloning = engine.clone();
    methods.set(
        "Clone",
        lua.create_function(move |lua, this: LuaValue| {
            let state = random_of(&this).ok_or_else(|| LuaError::runtime("Expected ':' not '.' calling member function Clone"))?;
            lua.create_any_userdata(new_random(&cloning, state))
        })?,
    )?;
    let index = wrapped(lua, engine, move |_, args| {
        let args = Args::new(args);
        let member: LuaValue = match args.get(1) {
            LuaValue::String(key) => methods.raw_get(key.clone())?,
            _ => LuaValue::Nil,
        };
        if member.is_nil() {
            return raise(format!("{} is not a valid member of Random", key_text(args.get(1))));
        }
        Ok(values([member]))
    })?;
    let newindex = read_only(lua, engine)?;
    let tostring = lua.create_function(|_, _: LuaValue| Ok("Random"))?;
    lua.register_userdata_type::<LRandom>(|registry| {
        registry.add_meta_field("__type", "Random");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
    })?;
    let sample = lua.create_any_userdata(LRandom(Rc::new(Cell::new(seed_random(0.0)))))?;
    lock_metatable(lua, &sample)?;

    // Random.new() without a seed: Studio seeds from entropy; offline runs stay deterministic, from
    // the run's seed (the default seed 1 keeps the sequence it always had); the sequence's state is the
    // scheduler's, so a harness's Context.SetRandomSeed restarts it
    let seeded = engine.clone();
    let library = lua.create_table()?;
    library.set(
        "new",
        wrapped(lua, engine, move |lua, args| {
            let args = Args::new(args);
            let seed = match args.get(0) {
                LuaValue::Nil => {
                    let mut state = seeded.state.borrow_mut();
                    let run = state.sched.random_seed;
                    let start = state.sched.unseeded_random.unwrap_or_else(|| luau_u32(0x9E3779B9u32 as f64 + (run - 1.0) * 2654435761.0) as f64);
                    let next = start * 1103515245.0 + 12345.0;
                    let next = luau_u32(next) as f64;
                    state.sched.unseeded_random = Some(next);
                    next
                }
                LuaValue::Number(n) => *n,
                LuaValue::Integer(n) => *n as f64,
                other => {
                    return raise_plain(format!(
                        "invalid argument #1 to 'new' (number expected, got {})",
                        other.type_name()
                    ));
                }
            };
            let userdata = lua.create_any_userdata(new_random(&seeded, seed_random(seed)))?;
            Ok(values([LuaValue::UserData(userdata)]))
        })?,
    )?;
    engine.lua.borrow_mut().libraries.insert("Random", library);
    Ok(())
}

// DateTime (UTC calendar maths; the clock itself comes from the virtual scheduler)

fn days_from_civil(year: f64, month: f64, day: f64) -> f64 {
    let year = year - if month <= 2.0 { 1.0 } else { 0.0 };
    let era = (if year >= 0.0 { year } else { year - 399.0 } / 400.0).floor();
    let year_of_era = year - era * 400.0;
    let day_of_year = ((153.0 * (month + if month > 2.0 { -3.0 } else { 9.0 }) + 2.0) / 5.0).floor() + day - 1.0;
    let day_of_era = year_of_era * 365.0 + (year_of_era / 4.0).floor() - (year_of_era / 100.0).floor() + day_of_year;
    era * 146097.0 + day_of_era - 719468.0
}

fn civil_from_days(days: f64) -> (f64, f64, f64) {
    let days = days + 719468.0;
    let era = (if days >= 0.0 { days } else { days - 146096.0 } / 146097.0).floor();
    let day_of_era = days - era * 146097.0;
    let year_of_era = ((day_of_era - (day_of_era / 1460.0).floor() + (day_of_era / 36524.0).floor()
        - (day_of_era / 146096.0).floor())
        / 365.0)
        .floor();
    let year = year_of_era + era * 400.0;
    let day_of_year = day_of_era - (365.0 * year_of_era + (year_of_era / 4.0).floor() - (year_of_era / 100.0).floor());
    let month_prime = ((5.0 * day_of_year + 2.0) / 153.0).floor();
    let day = day_of_year - ((153.0 * month_prime + 2.0) / 5.0).floor() + 1.0;
    let month = month_prime + if month_prime < 10.0 { 3.0 } else { -9.0 };
    (year + if month <= 2.0 { 1.0 } else { 0.0 }, month, day)
}

struct Parts {
    year: f64,
    month: f64,
    day: f64,
    hour: f64,
    minute: f64,
    second: f64,
    millisecond: f64,
}

fn universal_parts(millis: f64) -> Parts {
    let days = (millis / 86400000.0).floor();
    let remainder = millis - days * 86400000.0;
    let (year, month, day) = civil_from_days(days);
    Parts {
        year,
        month,
        day,
        hour: (remainder / 3600000.0).floor(),
        minute: luau_mod((remainder / 60000.0).floor(), 60.0),
        second: luau_mod((remainder / 1000.0).floor(), 60.0),
        millisecond: luau_mod(remainder, 1000.0),
    }
}

/// Local time with the machine's zone and daylight saving (as Studio's ToLocalTime measures).
fn local_parts(millis: f64) -> Parts {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let seconds = (millis / 1000.0).floor() as i64;
    match Local.timestamp_opt(seconds, 0).single() {
        Some(time) => Parts {
            year: time.year() as f64,
            month: time.month() as f64,
            day: time.day() as f64,
            hour: time.hour() as f64,
            minute: time.minute() as f64,
            second: time.second() as f64,
            millisecond: luau_mod(millis, 1000.0),
        },
        None => universal_parts(millis),
    }
}

const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const DAY_NAMES: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];

/// FormatUniversalTime / FormatLocalTime as a playing client formats in en-us (Studio 0.744,
/// tools/parity/probes/datetime.luau): the longest token at each place, `[...]` copied without its brackets, any other
/// character as it is. There are no ordinal tokens (`Do` is the day and a literal o, `14o`), and `Y`, `Q`, `w`, `W`,
/// `e`, `E`, `g`, `G`, `k`, `Z`, `X`, `x` are literals too. `DDD` / `DDDD` are the day of the year, `d` the weekday
/// (0 Sunday), `dd` its first two letters, `S` / `SS` / `SSS` tenths, hundredths and milliseconds (`SSSS` is `SSS` then
/// `S`). Other locales are not emulated (the en-us text is returned).
fn format_parts(t: &Parts, format: &str) -> String {
    const LOCALE_FORMATS: [(&str, &str); 10] = [
        ("LLLL", "dddd, MMMM D, YYYY h:mm A"),
        ("llll", "ddd, MMM D, YYYY h:mm A"),
        ("LTS", "h:mm:ss A"),
        ("LLL", "MMMM D, YYYY h:mm A"),
        ("lll", "MMM D, YYYY h:mm A"),
        ("LL", "MMMM D, YYYY"),
        ("ll", "MMM D, YYYY"),
        ("LT", "h:mm A"),
        ("L", "MM/DD/YYYY"),
        ("l", "MM/DD/YYYY"),
    ];
    const TOKENS: [&str; 27] = [
        "YYYY", "MMMM", "DDDD", "dddd", "MMM", "DDD", "ddd", "SSS", "YY", "MM", "DD", "dd", "HH", "hh", "mm", "ss", "SS", "M",
        "D", "d", "H", "h", "m", "s", "S", "A", "a",
    ];
    let days = days_from_civil(t.year, t.month, t.day);
    let weekday_index = luau_mod(days + 4.0, 7.0); // 1970-01-01 was a Thursday; 0 is Sunday
    let weekday = DAY_NAMES[luau_mod(days, 7.0) as usize];
    let day_of_year = days - days_from_civil(t.year, 1.0, 1.0) + 1.0;
    let hour12 = if luau_mod(t.hour, 12.0) == 0.0 { 12.0 } else { luau_mod(t.hour, 12.0) };
    let month = MONTH_NAMES[(t.month as usize).clamp(1, 12) - 1];
    let value = |token: &str| -> String {
        match token {
            "YYYY" => format!("{:04}", t.year as i64),
            "YY" => format!("{:02}", (t.year as i64).rem_euclid(100)),
            "MMMM" => month.to_string(),
            "MMM" => month[..3].to_string(),
            "MM" => format!("{:02}", t.month as i64),
            "M" => format!("{}", t.month as i64),
            "DDDD" => format!("{:03}", day_of_year as i64),
            "DDD" => format!("{}", day_of_year as i64),
            "DD" => format!("{:02}", t.day as i64),
            "D" => format!("{}", t.day as i64),
            "dddd" => weekday.to_string(),
            "ddd" => weekday[..3].to_string(),
            "dd" => weekday[..2].to_string(),
            "d" => format!("{}", weekday_index as i64),
            "HH" => format!("{:02}", t.hour as i64),
            "H" => format!("{}", t.hour as i64),
            "hh" => format!("{:02}", hour12 as i64),
            "h" => format!("{}", hour12 as i64),
            "mm" => format!("{:02}", t.minute as i64),
            "m" => format!("{}", t.minute as i64),
            "ss" => format!("{:02}", t.second as i64),
            "s" => format!("{}", t.second as i64),
            "SSS" => format!("{:03}", t.millisecond as i64),
            "SS" => format!("{:02}", (t.millisecond as i64) / 10),
            "S" => format!("{}", (t.millisecond as i64) / 100),
            "A" => if t.hour < 12.0 { "AM" } else { "PM" }.to_string(),
            "a" => if t.hour < 12.0 { "am" } else { "pm" }.to_string(),
            _ => String::new(),
        }
    };
    let simple = |format: &str, output: &mut String| {
        let mut rest = format;
        while !rest.is_empty() {
            if let Some(token) = TOKENS.iter().find(|token| rest.starts_with(**token)) {
                output.push_str(&value(token));
                rest = &rest[token.len()..];
            } else {
                let character = rest.chars().next().expect("a character");
                output.push(character);
                rest = &rest[character.len_utf8()..];
            }
        }
    };
    let mut output = String::new();
    let mut rest = format;
    while !rest.is_empty() {
        if let Some(escaped) = rest.strip_prefix('[') {
            if let Some(end) = escaped.find(']') {
                output.push_str(&escaped[..end]);
                rest = &escaped[end + 1..];
                continue;
            }
        }
        if let Some((token, expansion)) = LOCALE_FORMATS.iter().find(|(token, _)| rest.starts_with(*token)) {
            simple(expansion, &mut output);
            rest = &rest[token.len()..];
            continue;
        }
        if let Some(token) = TOKENS.iter().find(|token| rest.starts_with(**token)) {
            output.push_str(&value(token));
            rest = &rest[token.len()..];
            continue;
        }
        let character = rest.chars().next().expect("a character");
        output.push(character);
        rest = &rest[character.len_utf8()..];
    }
    output
}

fn parts_table(lua: &Lua, t: &Parts) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("Year", t.year)?;
    table.set("Month", t.month)?;
    table.set("Day", t.day)?;
    table.set("Hour", t.hour)?;
    table.set("Minute", t.minute)?;
    table.set("Second", t.second)?;
    table.set("Millisecond", t.millisecond)?;
    Ok(table)
}

fn iso_date(millis: f64) -> String {
    let t = universal_parts(millis);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year as i64, t.month as i64, t.day as i64, t.hour as i64, t.minute as i64, t.second as i64
    )
}

fn date_time_of(value: &LuaValue) -> Option<f64> {
    match value {
        LuaValue::UserData(ud) => ud.borrow::<LDateTime>().ok().map(|d| d.0),
        _ => None,
    }
}

/// The instants a DateTime can hold: Unix timestamps from -17,987,443,200 to 253,402,300,799 seconds, years 1400 to 9999
/// (the client's own message gives the bounds; Roblox's documentation the years).
const DATE_TIME_MIN_MILLIS: f64 = -17_987_443_200_000.0;
const DATE_TIME_MAX_MILLIS: f64 = 253_402_300_799_999.0;
/// The messages a playing client raises (Studio 0.744, tools/parity/probes/datetime.luau)
const TIMESTAMP_OUT_OF_RANGE: &str =
    "Error interpreting timestamp as a precise point in time. Fields out of range. UnixTimestamp should be between -17987443200 and 253402300799.";
const FIELDS_OUT_OF_RANGE: &str = "Error interpreting year, month, day, hour, minute, second as a precise point in time. Fields out of range.";
const OVER_THE_MAX: &str = "Over the max DateTime supported: 9999-12-31T23:59:59Z";

/// A DateTime constructor's numeric argument: nil is absent, a number or numeric string is read.
fn date_time_number(lua: &Lua, args: &Args, index: usize, name: &str) -> Result<Option<f64>, Flow> {
    match args.get(index) {
        LuaValue::Nil => Ok(None),
        LuaValue::Number(n) => Ok(Some(*n)),
        LuaValue::Integer(n) => Ok(Some(*n as f64)),
        other => match lua.coerce_number(other.clone()).ok().flatten() {
            Some(n) => Ok(Some(n)),
            None => raise_plain(format!(
                "invalid argument #{} to '{name}' (number expected, got {})",
                index + 1,
                other.type_name()
            )),
        },
    }
}

/// A DateTime method's string argument as Luau's checks read it: a string, or a number in its tostring form
/// (`FormatUniversalTime(5, "en-us")` is "5"); anything else raises (Studio 0.744, tools/parity/probes/datetime.luau:
/// `invalid argument #3 to 'FormatUniversalTime' (string expected, got nil)`).
fn date_time_string(lua: &Lua, args: &Args, index: usize, name: &str) -> Result<String, Flow> {
    let value = args.get(index);
    let text = match value {
        LuaValue::String(text) => Some(text.clone()),
        LuaValue::Number(_) | LuaValue::Integer(_) => lua.coerce_string(value.clone()).ok().flatten(),
        _ => None,
    };
    match text {
        Some(text) => Ok(text.to_string_lossy()),
        None => raise_plain(format!(
            "invalid argument #{} to '{name}' (string expected, got {})",
            index + 1,
            value.type_name()
        )),
    }
}

/// fromUniversalTime's instant for (year, month, day, hour, minute, second, millisecond) as a playing client reads them
/// (Studio 0.744, tools/parity/probes/datetime.luau): each unit truncated toward zero (2021.5, 1.5, 1.5 is 2021-01-01),
/// absent units 1970, 1, 1, 0, 0, 0, 0; time units roll over into the date (hour 24, minute 60, second 60, millisecond
/// 1000 and hour -1 measured). A month or day that names no date, or a year before 1400 (measured with -1), raises
/// FIELDS_OUT_OF_RANGE; a year after 9999 OVER_THE_MAX (measured with 10000). Not measured: an instant rolled over past
/// 9999-12-31T23:59:59.999Z (OVER_THE_MAX here) or before the year 1400 (FIELDS_OUT_OF_RANGE here).
fn date_time_millis(fields: [Option<f64>; 7]) -> Result<f64, &'static str> {
    let [year, month, day, hour, minute, second, millisecond] = fields.map(|field| field.map(f64::trunc));
    let (year, month, day) = (year.unwrap_or(1970.0), month.unwrap_or(1.0), day.unwrap_or(1.0));
    if year > 9999.0 {
        return Err(OVER_THE_MAX);
    }
    if !(year >= 1400.0) || !(1.0..=12.0).contains(&month) {
        return Err(FIELDS_OUT_OF_RANGE);
    }
    let leap = year % 4.0 == 0.0 && (year % 100.0 != 0.0 || year % 400.0 == 0.0);
    let month_days = match month as u32 {
        2 if leap => 29.0,
        2 => 28.0,
        4 | 6 | 9 | 11 => 30.0,
        _ => 31.0,
    };
    if !(1.0..=month_days).contains(&day) {
        return Err(FIELDS_OUT_OF_RANGE);
    }
    let millis = days_from_civil(year, month, day) * 86400000.0
        + hour.unwrap_or(0.0) * 3600000.0
        + minute.unwrap_or(0.0) * 60000.0
        + second.unwrap_or(0.0) * 1000.0
        + millisecond.unwrap_or(0.0);
    if millis > DATE_TIME_MAX_MILLIS {
        return Err(OVER_THE_MAX);
    }
    if !(millis >= DATE_TIME_MIN_MILLIS) {
        return Err(FIELDS_OUT_OF_RANGE);
    }
    Ok(millis)
}

/// fromIsoDate as a playing client parses (Studio 0.744, tools/parity/probes/datetime.luau): leading white space
/// skipped, then `YYYY-MM-DD`, optionally followed by `T`, `t` or a space and `hh:mm`, then `:ss` and a fraction of a
/// second (its first three digits are the milliseconds, the rest ignored), then `Z`, `z` or an offset `+hh:mm` /
/// `-hh:mm` (`+0200` is nil; no designator is universal time). A date that does not exist is nil; hour 24 and second 60
/// roll over into the next day and minute; the basic format (`20200229T123456Z`) and a signed year are nil. Not
/// measured: trailing text, an hour past 24, a minute past 59 or a second past 60 (all nil here).
fn parse_iso_date(text: &str) -> Option<f64> {
    let bytes = text.trim_start().as_bytes();
    let digits_value = |digits: &[u8]| digits.iter().fold(0.0, |value, digit| value * 10.0 + f64::from(digit - b'0'));
    let number = |at: &mut usize, count: usize| -> Option<f64> {
        let digits = bytes.get(*at..*at + count)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *at += count;
        Some(digits_value(digits))
    };
    let expect = |at: &mut usize, byte: u8| -> Option<()> {
        (bytes.get(*at) == Some(&byte)).then(|| *at += 1)
    };
    let mut at = 0;
    let year = number(&mut at, 4)?;
    expect(&mut at, b'-')?;
    let month = number(&mut at, 2)?;
    expect(&mut at, b'-')?;
    let day = number(&mut at, 2)?;
    let (mut hour, mut minute, mut second, mut millisecond, mut offset) = (0.0, 0.0, 0.0, 0.0, 0.0);
    if let Some(b'T' | b't' | b' ') = bytes.get(at) {
        at += 1;
        hour = number(&mut at, 2)?;
        expect(&mut at, b':')?;
        minute = number(&mut at, 2)?;
        if expect(&mut at, b':').is_some() {
            second = number(&mut at, 2)?;
            if expect(&mut at, b'.').is_some() {
                let start = at;
                while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
                if at == start {
                    return None;
                }
                let digits = &bytes[start..at.min(start + 3)];
                millisecond = digits_value(digits) * 10f64.powi(3 - digits.len() as i32);
            }
        }
        match bytes.get(at) {
            Some(b'Z' | b'z') => at += 1,
            Some(&sign @ (b'+' | b'-')) => {
                at += 1;
                let hours = number(&mut at, 2)?;
                expect(&mut at, b':')?;
                let minutes = number(&mut at, 2)?;
                offset = (hours * 3600000.0 + minutes * 60000.0) * if sign == b'+' { 1.0 } else { -1.0 };
            }
            _ => {}
        }
    }
    if at != bytes.len() || hour > 24.0 || minute > 59.0 || second > 60.0 {
        return None;
    }
    let fields = [Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second), Some(millisecond)];
    Some(date_time_millis(fields).ok()? - offset)
}

/// A DateTime at whole milliseconds (-0 is 0: `fromUnixTimestampMillis(-0.5)` prints 0).
fn make_date_time(lua: &Lua, millis: f64) -> LuaResult<LuaValue> {
    Ok(LuaValue::UserData(lua.create_any_userdata(LDateTime(millis.floor() + 0.0))?))
}

fn register_date_time(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    let methods = lua.create_table()?;
    let this_millis = |args: &Args| match date_time_of(args.get(0)) {
        Some(millis) => Ok(millis),
        None => raise_plain("Expected ':' not '.' calling member function"),
    };
    methods.set(
        "ToUniversalTime",
        wrapped(lua, engine, move |lua, args| {
            Ok(values([LuaValue::Table(parts_table(lua, &universal_parts(this_millis(&Args::new(args))?))?)]))
        })?,
    )?;
    methods.set(
        "ToLocalTime",
        wrapped(lua, engine, move |lua, args| {
            Ok(values([LuaValue::Table(parts_table(lua, &local_parts(this_millis(&Args::new(args))?))?)]))
        })?,
    )?;
    methods.set(
        "ToIsoDate",
        wrapped(lua, engine, move |lua, args| {
            Ok(values([LuaValue::String(lua.create_string(iso_date(this_millis(&Args::new(args))?))?)]))
        })?,
    )?;
    // The locale must be a string; other locales than en-us are not emulated (Studio 0.744 formats `LL` in en-gb as
    // "14 November 2023"; an unknown locale or "" is en-us)
    let format = |name: &'static str, parts: fn(f64) -> Parts| {
        move |lua: &Lua, args: LuaMultiValue| -> Ret {
            let args = Args::new(args);
            let millis = this_millis(&args)?;
            let format = date_time_string(lua, &args, 1, name)?;
            date_time_string(lua, &args, 2, name)?;
            Ok(values([LuaValue::String(lua.create_string(format_parts(&parts(millis), &format))?)]))
        }
    };
    methods.set("FormatUniversalTime", wrapped(lua, engine, format("FormatUniversalTime", universal_parts))?)?;
    methods.set("FormatLocalTime", wrapped(lua, engine, format("FormatLocalTime", local_parts))?)?;
    let index = wrapped(lua, engine, move |_, args| {
        let args = Args::new(args);
        let millis = date_time_of(args.get(0)).unwrap_or(0.0);
        if let LuaValue::String(key) = args.get(1) {
            match &*key.as_bytes() {
                b"UnixTimestampMillis" => return Ok(values([LuaValue::Number(millis)])),
                // truncated toward zero: -1 ms is 0 (Studio 0.744, tools/parity/probes/datetime.luau)
                b"UnixTimestamp" => return Ok(values([LuaValue::Number((millis / 1000.0).trunc() + 0.0)])),
                _ => {
                    let member: LuaValue = methods.raw_get(key.clone())?;
                    if !member.is_nil() {
                        return Ok(values([member]));
                    }
                }
            }
        }
        raise(format!("{} is not a valid member of DateTime", key_text(args.get(1))))
    })?;
    let newindex = read_only(lua, engine)?;
    let compare = |f: fn(f64, f64) -> bool| {
        move |_: &Lua, (a, b): (LuaValue, LuaValue)| -> LuaResult<bool> {
            Ok(match (date_time_of(&a), date_time_of(&b)) {
                (Some(a), Some(b)) => f(a, b),
                _ => false,
            })
        }
    };
    let eq = lua.create_function(compare(|a, b| a == b))?;
    let lt = lua.create_function(compare(|a, b| a < b))?;
    let le = lua.create_function(compare(|a, b| a <= b))?;
    // tostring is the instant's UnixTimestampMillis ("1700000000123", "-1500"; Studio 0.744)
    let tostring = lua.create_function(|_, this: LuaValue| Ok(format!("{}", date_time_of(&this).unwrap_or(0.0) as i64)))?;
    lua.register_userdata_type::<LDateTime>(|registry| {
        registry.add_meta_field("__type", "DateTime");
        registry.add_meta_field("__index", index);
        registry.add_meta_field("__newindex", newindex);
        registry.add_meta_field("__tostring", tostring);
        registry.add_meta_field("__eq", eq);
        registry.add_meta_field("__lt", lt);
        registry.add_meta_field("__le", le);
    })?;
    if let LuaValue::UserData(sample) = make_date_time(lua, 0.0)? {
        lock_metatable(lua, &sample)?;
    }

    let standard_offset_millis = scheduler::standard_offset_min_seconds() * 1000.0;
    let library = lua.create_table()?;
    let e = engine.clone();
    library.set(
        "now",
        lua.create_function(move |lua, ()| {
            let millis = e.state.borrow_mut().sched.unix_millis();
            make_date_time(lua, millis)
        })?,
    )?;
    // fromUnixTimestamp truncates its seconds toward zero before scaling (0.5 and -0.5 are 0, 1.999 is 1000 ms, -1.5
    // -1000 ms) and raises TIMESTAMP_OUT_OF_RANGE for a second outside the range, NaN and the infinities included;
    // fromUnixTimestampMillis truncates its milliseconds (1.5 is 1, -1.5 -1, -0.5 0) (Studio 0.744,
    // tools/parity/probes/datetime.luau). fromUnixTimestampMillis's out-of-range message is not measured (the seconds'
    // message here).
    let timestamp = |name: &'static str, scale: f64| {
        move |lua: &Lua, args: LuaMultiValue| -> Ret {
            let args = Args::new(args);
            let value = date_time_number(lua, &args, 0, name)?.unwrap_or(0.0).trunc();
            let millis = value * scale;
            if !(DATE_TIME_MIN_MILLIS..=DATE_TIME_MAX_MILLIS).contains(&millis) {
                return raise_plain(TIMESTAMP_OUT_OF_RANGE);
            }
            Ok(values([make_date_time(lua, millis)?]))
        }
    };
    library.set("fromUnixTimestamp", wrapped(lua, engine, timestamp("fromUnixTimestamp", 1000.0))?)?;
    library.set("fromUnixTimestampMillis", wrapped(lua, engine, timestamp("fromUnixTimestampMillis", 1.0))?)?;
    let from_units = |name: &'static str, offset_millis: f64| {
        move |lua: &Lua, args: LuaMultiValue| -> Ret {
            let args = Args::new(args);
            let mut fields = [None; 7];
            for (index, field) in fields.iter_mut().enumerate() {
                *field = date_time_number(lua, &args, index, name)?;
            }
            match date_time_millis(fields) {
                Ok(millis) => Ok(values([make_date_time(lua, millis - offset_millis)?])),
                Err(message) => raise_plain(message),
            }
        }
    };
    library.set("fromUniversalTime", wrapped(lua, engine, from_units("fromUniversalTime", 0.0))?)?;
    library.set("fromLocalTime", wrapped(lua, engine, from_units("fromLocalTime", standard_offset_millis))?)?;
    library.set(
        "fromIsoDate",
        wrapped(lua, engine, |lua, args| {
            let text = date_time_string(lua, &Args::new(args), 0, "fromIsoDate")?;
            Ok(values([match parse_iso_date(&text) {
                Some(millis) => make_date_time(lua, millis)?,
                None => LuaValue::Nil,
            }]))
        })?,
    )?;
    engine.lua.borrow_mut().libraries.insert("DateTime", library);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_matches_the_luau_oracle() {
        // runtime/tests/random.luau vectors; the wide NextInteger as measured in a playing client (Studio 0.744:
        // floor(min * (1 - u) + max * u), one less than that oracle's 64-bit multiply for seeds 7, -1.9 and -2^53 + 1)
        let vectors: &[(f64, [f64; 3], f64, f64)] = &[
            (0.0, [0.5514556904749074, 0.01723102436113014, 0.883376920906403], 73.0, 72384586732.0),
            (7.0, [0.8737085924861333, 0.4321572161309693, 0.3089922198934871], 5.0, 482964445446.0),
            (-3.0, [0.23554798859579634, 0.32381286642224644, 0.14705557445875167], 28.0, -493285887950.0),
            (1.9, [0.9484344206192203, 0.061285879733742145, 0.8770159548057006], 9.0, -497511649161.0),
            (-1.9, [0.0037155688242635435, 0.5469862230236278, 0.31011551315193603], 67.0, 1062967531246.0),
            (4294967303.0, [0.9971430285879401, 0.5584106571948971, 0.05136057646030647], 58.0, -943718980273.0),
            (9007199254740991.0, [0.027684032073451197, 0.09103798432822455, 0.9869885996197573], 53.0, 502533245394.0),
            (-9007199254740991.0, [0.22788703655698744, 0.9425859688069043, 0.06775640206431996], 38.0, -518359285893.0),
        ];
        for (seed, doubles, small, large) in vectors {
            let mut random = seed_random(*seed);
            for expected in doubles {
                assert_eq!(random.next_number(0.0, 1.0), *expected, "seed {seed}");
            }
            assert_eq!(random.next_integer(1.0, 100.0), *small);
            assert_eq!(random.next_integer(-1099511627776.0, 1099511628776.0), *large);
        }
        assert_eq!(seed_random(f64::INFINITY).next_unit(), 0.0);
        assert_eq!(seed_random(f64::NEG_INFINITY).next_unit(), 0.0037155688242635435);
        assert_eq!(seed_random(f64::NAN).next_unit(), 0.0037155688242635435);
        assert_eq!(seed_random(-0.0).next_unit(), seed_random(0.0).next_unit());
        // Unseeded sequence
        let mut counter = 0x9E3779B9u32 as f64;
        for expected in [0.15903319831126075, 0.6648808275887605, 0.7537480905786614] {
            counter = luau_u32(counter * 1103515245.0 + 12345.0) as f64;
            assert_eq!(seed_random(counter).next_unit(), expected);
        }
    }

    #[test]
    fn calendar_round_trips() {
        let days = days_from_civil(2026.0, 9.0, 21.0);
        assert_eq!(civil_from_days(days), (2026.0, 9.0, 21.0));
        assert_eq!(iso_date(1767225600000.0), "2026-01-01T00:00:00Z");
        let parts = universal_parts(1767225600000.0 + 13.0 * 3600000.0 + 5.0 * 60000.0);
        assert_eq!(format_parts(&parts, "LLLL"), "Thursday, January 1, 2026 1:05 PM");
    }
}

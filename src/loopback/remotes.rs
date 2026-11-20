//! Remotes in a loopback run: RemoteEvent, UnreliableRemoteEvent and RemoteFunction carry calls between the two sides.
//! Measured in a play session's two sides (Studio 0.747, tools/parity/probes/loopback; docs/engine.md, Log 2026-11-19):
//!
//! - Each side calls only its own members: on a client `FireClient can only be called from the server` (before the
//!   player argument is looked at), `FireAllClients ...`, `InvokeClient ...` (after it: `Argument 1 is invalid: expected
//!   'Player' instance type`), `OnServerEvent can only be used on the server` at Connect (the member reads), `OnServerInvoke
//!   can only be implemented on the server`; a server's are server.luau's (FireServer, InvokeServer) and `OnClientEvent can
//!   only be used on the client`, `OnClientInvoke can only be implemented on the client`. A parentless remote refuses
//!   nothing (and sends nothing).
//! - An event reaches the other side's handlers at its receive point (deferred: after the legacy wait() resumptions);
//!   OnServerEvent gets the player first. A remote the receiver lacks (a client-made one, one the server keeps in
//!   ServerStorage) takes nothing and logs nothing. Events reaching a remote nothing listens to wait on it: the first
//!   connection made gets them all, in order, a later one none; 256 wait, and past them each is dropped with the error
//!   `Remote event invocation queue exhausted for <full name>; did you forget to implement OnClientEvent? (N events
//!   dropped)` logged at 1, 2, 4, ... dropped.
//! - UnreliableRemoteEvent: arguments over 1000 encoded bytes (wire.rs `encoded_size`) are dropped with the sender's
//!   warning `Unreliable Event dropped as its payload is too large. Consider reducing the total size of the arguments.
//!   Payload went over by: N bytes`; its events arrive after the frame's reliable messages. ree delivers every one, in
//!   order (no loss is simulated).
//! - RemoteFunction: the caller yields; the callback runs on a thread of its own when the call arrives (called, not
//!   deferred: before the legacy wait() resumptions), or once it is set; its results go back as the arguments do, its
//!   error as a string with its script and line (`error(x, 0)` without, a table as an empty string). A call to a remote
//!   the other side lacks waits for good. InvokeClient of a harness's fake player waits for that player to leave, as in a
//!   server run.

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rustc_hash::FxHashMap;

use super::{Endpoint, Side, bridge::Msg, wire::{self, Wire}};
use crate::{
    Engine, InstanceId, NameId,
    lua::{Args, Flow, Ret, instance_value, none, raise, values},
    scheduler::{DispatchKind, JobTarget},
    signal::{ConnectionId, SignalId},
    value::Value,
};

const LOOPBACK: &str = include_str!("loopback.luau");

/// An invoke waiting for its remote's callback: (call id, arguments)
type WaitingInvoke = (u32, Box<[Wire]>);

/// Events waiting on a remote nothing listens to
#[derive(Default)]
struct Queue {
    events: Vec<Vec<Value>>,
    dropped: u64,
}

/// One side's remote state: its callers waiting for an answer, the events waiting for a handler, the invokes waiting for
/// their callback. Its Lua values are its own VM's ([`Remotes::release`] lets them go before the VM).
#[derive(Default)]
pub struct Remotes {
    /// Threads waiting for an answer, by call id (a slot freed when answered)
    calls: RefCell<Vec<Option<LuaThread>>>,
    free: RefCell<Vec<u32>>,
    /// Events waiting for a first connection, by the remote's event signal
    queued: RefCell<FxHashMap<SignalId, Queue>>,
    /// Invokes waiting for the callback to be set, by remote
    waiting: RefCell<FxHashMap<InstanceId, Vec<WaitingInvoke>>>,
    /// loopback.luau's Run and the Done it calls
    run: RefCell<Option<(LuaFunction, LuaFunction)>>,
}

impl Remotes {
    /// Lets go of every Lua value held (the waiting callers, the queued events), as the side's VM goes or the player leaves
    pub fn release(&self) {
        self.calls.borrow_mut().clear();
        self.free.borrow_mut().clear();
        self.queued.borrow_mut().clear();
        self.waiting.borrow_mut().clear();
        *self.run.borrow_mut() = None;
    }

    fn hold(&self, thread: LuaThread) -> u32 {
        let mut calls = self.calls.borrow_mut();
        match self.free.borrow_mut().pop() {
            Some(slot) => {
                calls[slot as usize] = Some(thread);
                slot
            }
            None => {
                calls.push(Some(thread));
                (calls.len() - 1) as u32
            }
        }
    }

    fn take(&self, call: u32) -> Option<LuaThread> {
        let thread = self.calls.borrow_mut().get_mut(call as usize)?.take()?;
        self.free.borrow_mut().push(call);
        Some(thread)
    }

    /// Callers waiting (tests: none are left when a run ends)
    pub fn waiting_calls(&self) -> usize {
        self.calls.borrow().iter().filter(|call| call.is_some()).count()
    }
}

fn parentless(engine: &Engine, id: InstanceId) -> bool {
    engine.state.borrow().dom.at(id).parent.is_none()
}

fn is_player(engine: &Engine, value: &LuaValue) -> Option<InstanceId> {
    let id = crate::lua::instance_id(value)?;
    engine.reflection.is_a(engine.state.borrow().dom.at(id).class, "Player").then_some(id)
}

/// The receiving side's handlers of `remote` get `arguments`: queued on the remote when nothing listens
fn event_name(side: Side) -> &'static str {
    match side {
        Side::Server => "OnServerEvent",
        Side::Client => "OnClientEvent",
    }
}

/// Registers a linked engine's remote members (after server.luau's, so they win)
pub fn install(lua: &Lua, engine: &Rc<Engine>, side: Side) -> LuaResult<()> {
    let reflection = engine.reflection.clone();
    let name = |text: &str| reflection.name_id(text);
    let classes: Vec<_> = ["RemoteEvent", "UnreliableRemoteEvent"].iter().filter_map(|class| reflection.class_id(class).map(|id| (*class == "UnreliableRemoteEvent", id))).collect();
    let mut behaviors = engine.behaviors.borrow_mut();
    for &(unreliable, class) in &classes {
        if let Some(fire_server) = name("FireServer") {
            behaviors.add_method(class, fire_server, crate::services::method(move |lua, engine, id, args| {
                if parentless(engine, id) {
                    return Ok(none());
                }
                if side == Side::Server {
                    return raise("FireServer can only be called from the client");
                }
                send_event(lua, engine, id, &args.0.iter().skip(1).cloned().collect::<Vec<_>>(), unreliable, wire::MAX_DEPTH)
            }));
        }
        if let Some(fire_client) = name("FireClient") {
            behaviors.add_method(class, fire_client, crate::services::method(move |lua, engine, id, args| {
                if side == Side::Client {
                    if parentless(engine, id) {
                        return Ok(none());
                    }
                    return raise("FireClient can only be called from the server");
                }
                let Some(player) = is_player(engine, args.get(1)) else {
                    return raise("FireClient: player argument must be a Player object");
                };
                if parentless(engine, id) || !is_loopback_player(engine, player) {
                    return Ok(none());
                }
                send_event(lua, engine, id, &args.0.iter().skip(2).cloned().collect::<Vec<_>>(), unreliable, wire::MAX_DEPTH_AFTER_PLAYER)
            }));
        }
        if let Some(fire_all) = name("FireAllClients") {
            behaviors.add_method(class, fire_all, crate::services::method(move |lua, engine, id, args| {
                if parentless(engine, id) {
                    return Ok(none());
                }
                if side == Side::Client {
                    return raise("FireAllClients can only be called from the server");
                }
                let joined = engine.loopback().is_some_and(|end| end.bridge.joined.get() && !end.bridge.left.get());
                if !joined {
                    return Ok(none());
                }
                send_event(lua, engine, id, &args.0.iter().skip(1).cloned().collect::<Vec<_>>(), unreliable, wire::MAX_DEPTH)
            }));
        }
    }
    drop(behaviors);
    // RemoteFunction: the Luau wrappers raise a callback's error in the caller
    let call = crate::lua::wrapped(lua, engine, {
        let engine = engine.clone();
        move |lua, args| invoke(lua, &engine, side, Args::new(args))
    })?;
    let players = crate::services::get_service(engine, engine.world().game, "Players").map_err(|_| LuaError::runtime("no Players"))?;
    let chunk = crate::lua::modules::load_engine_luau(lua, LOOPBACK, "=rur.loopback", None)?;
    let exports: LuaTable = crate::cost::engine_luau(lua, chunk)?.call((call, instance_value(lua, engine, players)?))?;
    if let Some(function) = reflection.class_id("RemoteFunction") {
        let mut behaviors = engine.behaviors.borrow_mut();
        for member in ["InvokeServer", "InvokeClient"] {
            if let Some(member_name) = name(member) {
                behaviors.add_lua_method(function, member_name, exports.get(member)?);
            }
        }
    }
    let done = {
        let engine = engine.clone();
        lua.create_function(move |lua, (call, ok, results): (u32, bool, LuaMultiValue)| answered(lua, &engine, call, ok, results))?
    };
    if let Some(endpoint) = engine.loopback() {
        *endpoint.remotes.run.borrow_mut() = Some((exports.get("Run")?, done));
    }
    engine.clear_member_cache();
    Ok(())
}

fn is_loopback_player(engine: &Engine, player: InstanceId) -> bool {
    engine.loopback().is_some_and(|end| end.bridge.player.get().is_some_and(|(server, _)| server == player) && end.bridge.joined.get() && !end.bridge.left.get())
}

/// FireServer / FireClient / FireAllClients: the arguments as the call left them (wire.rs), into the side's stream; an
/// unreliable call over its ceiling is dropped with the sender's warning
fn send_event(lua: &Lua, engine: &Rc<Engine>, remote: InstanceId, arguments: &[LuaValue], unreliable: bool, depth: usize) -> Ret {
    let Some(endpoint) = engine.loopback() else { return Ok(none()) };
    let args = match wire::capture_all(lua, engine, arguments, depth) {
        Ok(args) => args,
        Err(message) => return raise(message),
    };
    if unreliable {
        let size = wire::encoded_size(&args);
        if size > wire::UNRELIABLE_LIMIT {
            engine.warn(format!(
                "Unreliable Event dropped as its payload is too large. Consider reducing the total size of the arguments. Payload went over by: {} bytes",
                size - wire::UNRELIABLE_LIMIT
            ));
            return Ok(none());
        }
        let message = Msg::Event { remote, args };
        super::lazy::pair_sent(engine, &endpoint, &message);
        endpoint.bridge.send_unreliable(endpoint.side, message);
        return Ok(none());
    }
    send(engine, &endpoint, Msg::Event { remote, args });
    Ok(none())
}

/// A reliable message from a side: a server's in its place among its frame's changes (replicate.rs flush), a client's
/// straight into its stream
fn send(engine: &Engine, endpoint: &Rc<Endpoint>, message: Msg) {
    match endpoint.side {
        Side::Server => {
            if let Some(log) = engine.state.borrow_mut().loopback_log.as_mut() {
                log.push(super::replicate::Change::Message(message));
            }
        }
        Side::Client => {
            super::lazy::pair_sent(engine, endpoint, &message);
            endpoint.bridge.send(Side::Client, message)
        }
    }
}

/// loopback.luau's Call(remote, toServer, player, ...): InvokeServer or InvokeClient; the call goes out and the caller yields
/// until its answer; ("fake") for a harness's player
fn invoke(lua: &Lua, engine: &Rc<Engine>, side: Side, args: Args) -> Ret {
    let Some(endpoint) = engine.loopback() else { return raise("not a loopback run") };
    let Some(remote) = crate::lua::instance_id(args.get(0)) else { return raise("Expected ':' not '.' calling member function InvokeServer") };
    let client_call = matches!(args.get(1), LuaValue::Boolean(true));
    let (skip, depth) = if client_call { (3, wire::MAX_DEPTH) } else { (3, wire::MAX_DEPTH_AFTER_PLAYER) };
    if client_call {
        if side == Side::Server {
            return raise("InvokeServer can only be called from the client");
        }
    } else {
        let Some(player) = is_player(engine, args.get(2)) else {
            return raise("Argument 1 is invalid: expected 'Player' instance type");
        };
        if side == Side::Client {
            return raise("InvokeClient can only be called from the server");
        }
        if !is_loopback_player(engine, player) {
            return Ok(values([LuaValue::String(lua.create_string("fake")?)]));
        }
    }
    let arguments: Vec<LuaValue> = args.0.iter().skip(skip).cloned().collect();
    let captured = match wire::capture_all(lua, engine, &arguments, depth) {
        Ok(captured) => captured,
        Err(message) => return raise(message),
    };
    let call = endpoint.remotes.hold(lua.current_thread());
    send(engine, &endpoint, Msg::Invoke { remote, call, args: captured });
    Err(Flow::Yield)
}

/// The instance ids of the other side's messages, as this side's
fn mapper<'a>(engine: &'a Engine, endpoint: &'a Endpoint) -> impl Fn(InstanceId) -> Option<InstanceId> + 'a {
    move |id| match endpoint.side {
        // (a client pairs the server's Studio-only instances as it meets them: lazy.rs)
        Side::Client => super::lazy::to_client(engine, endpoint, id),
        Side::Server => endpoint.bridge.ids.borrow().map(Side::Server, id),
    }
}

/// An event at the receive point: the receiver's handlers get it (deferred), or the remote keeps it until a first
/// connection; a remote this side lacks takes nothing
pub fn deliver_event(lua: &Lua, engine: &Rc<Engine>, endpoint: &Rc<Endpoint>, remote: InstanceId, args: Box<[Wire]>) -> LuaResult<()> {
    let map = mapper(engine, endpoint);
    let Some(remote) = map(remote) else { return Ok(()) };
    if engine.state.borrow().dom.get(remote).is_none() {
        return Ok(());
    }
    let mut values = Vec::with_capacity(args.len() + 1);
    if endpoint.side == Side::Server {
        let player = endpoint.bridge.player.get().map(|(server, _)| server);
        values.push(player.map_or(Value::Nil, Value::Instance));
    }
    for arg in args.into_vec() {
        values.push(wire::delivered(lua, engine, arg, &map)?);
    }
    let Some(event) = engine.reflection.name_id(event_name(endpoint.side)) else { return Ok(()) };
    let mut state = engine.state.borrow_mut();
    let signal = crate::instance::event_signal(&mut state, &engine.reflection, remote, event);
    if state.signals.has_handlers(signal) {
        crate::instance::fire_event_measured(&mut state, remote, event, values, crate::cost::SignalKind::Remote);
        return Ok(());
    }
    // nothing listens: the remote keeps it for its first connection (256 at most)
    let full_name = state.dom.full_name(&engine.reflection, remote);
    drop(state);
    let mut queued = endpoint.remotes.queued.borrow_mut();
    let queue = queued.entry(signal).or_default();
    if queue.events.len() < 256 {
        queue.events.push(values);
        return Ok(());
    }
    queue.dropped += 1;
    let dropped = queue.dropped;
    drop(queued);
    if dropped.is_power_of_two() {
        engine.output(
            "error",
            format!("Remote event invocation queue exhausted for {full_name}; did you forget to implement {}? ({dropped} events dropped)", event_name(endpoint.side)),
            None,
        );
    }
    Ok(())
}

/// A connection was made to `signal` (lua/types.rs Connect): when it is a remote's event with events waiting, they go to
/// this connection, in order. On the wrong side Connect is refused first (`OnServerEvent can only be used on the server`).
pub fn connecting(engine: &Engine, signal: SignalId) -> Result<(), Flow> {
    let Some(endpoint) = engine.loopback() else { return Ok(()) };
    let label = engine.state.borrow().signals.get(signal).label.clone();
    let label = label.as_bytes();
    let (server_only, client_only) = (label.ends_with(b".OnServerEvent"), label.ends_with(b".OnClientEvent"));
    if server_only && endpoint.side == Side::Client {
        return raise("OnServerEvent can only be used on the server");
    }
    if client_only && endpoint.side == Side::Server {
        return raise("OnClientEvent can only be used on the client");
    }
    Ok(())
}

/// After a connection to `signal` was made: the events waiting on it go to it
pub fn connected(engine: &Engine, signal: SignalId, connection: ConnectionId) {
    let Some(endpoint) = engine.loopback() else { return };
    let Some(queue) = endpoint.remotes.queued.borrow_mut().remove(&signal) else { return };
    let (_, resume) = crate::cost::signal_costs(crate::cost::SignalKind::Remote);
    let mut state = engine.state.borrow_mut();
    for values in queue.events {
        state.sched.defer_handler(JobTarget::Handler { connection, kind: DispatchKind::Signal(crate::cost::SignalKind::Remote) }, values, resume, signal);
    }
}

/// A callback member is being set (lua/instance.rs): refused on the wrong side
pub fn setting_callback(engine: &Engine, name: NameId) -> Result<(), Flow> {
    let Some(endpoint) = engine.loopback() else { return Ok(()) };
    match (engine.reflection.name(name), endpoint.side) {
        ("OnServerInvoke", Side::Client) => raise("OnServerInvoke can only be implemented on the server"),
        ("OnClientInvoke", Side::Server) => raise("OnClientInvoke can only be implemented on the client"),
        _ => Ok(()),
    }
}

/// A callback member was set: the invokes waiting for it run now
pub fn callback_set(lua: &Lua, engine: &Rc<Engine>, remote: InstanceId) -> LuaResult<()> {
    let Some(endpoint) = engine.loopback() else { return Ok(()) };
    let waiting = endpoint.remotes.waiting.borrow_mut().remove(&remote);
    for (call, args) in waiting.unwrap_or_default() {
        run_callback(lua, engine, &endpoint, remote, call, &args)?;
    }
    Ok(())
}

/// An invoke at the receive point: its callback runs now on a thread of its own, or once it is set
pub fn deliver_invoke(lua: &Lua, engine: &Rc<Engine>, endpoint: &Rc<Endpoint>, remote: InstanceId, call: u32, args: Box<[Wire]>) -> LuaResult<()> {
    let Some(remote) = mapper(engine, endpoint)(remote) else { return Ok(()) };
    if engine.state.borrow().dom.get(remote).is_none() {
        return Ok(());
    }
    let name = callback_name(endpoint.side);
    let has_callback = engine.reflection.name_id(name).is_some_and(|name| matches!(engine.state.borrow().dom.at(remote).callback(name), Some(LuaValue::Function(_))));
    if !has_callback {
        endpoint.remotes.waiting.borrow_mut().entry(remote).or_default().push((call, args));
        return Ok(());
    }
    run_callback(lua, engine, endpoint, remote, call, &args)
}

fn callback_name(side: Side) -> &'static str {
    match side {
        Side::Server => "OnServerInvoke",
        Side::Client => "OnClientInvoke",
    }
}

fn run_callback(lua: &Lua, engine: &Rc<Engine>, endpoint: &Rc<Endpoint>, remote: InstanceId, call: u32, args: &[Wire]) -> LuaResult<()> {
    let Some(name) = engine.reflection.name_id(callback_name(endpoint.side)) else { return Ok(()) };
    let Some(LuaValue::Function(callback)) = engine.state.borrow().dom.at(remote).callback(name).cloned() else { return Ok(()) };
    let Some((run, done)) = endpoint.remotes.run.borrow().clone() else { return Ok(()) };
    let map = mapper(engine, endpoint);
    let mut arguments = LuaMultiValue::with_capacity(args.len() + 4);
    arguments.push_back(LuaValue::Function(callback));
    arguments.push_back(LuaValue::Function(done));
    arguments.push_back(LuaValue::Integer(i64::from(call)));
    if endpoint.side == Side::Server {
        let player = endpoint.bridge.player.get().map(|(server, _)| server);
        arguments.push_back(match player {
            Some(player) => instance_value(lua, engine, player)?,
            None => LuaValue::Nil,
        });
    }
    for arg in args {
        arguments.push_back(wire::rebuilt(lua, engine, arg, &map)?);
    }
    let thread = lua.create_thread(run)?;
    crate::scheduler::resume(lua, engine, &thread, arguments);
    Ok(())
}

/// loopback.luau's Done: a callback's outcome goes back to its caller (results as arguments go, an error as its text)
fn answered(lua: &Lua, engine: &Rc<Engine>, call: u32, ok: bool, mut results: LuaMultiValue) -> LuaResult<()> {
    let Some(endpoint) = engine.loopback() else { return Ok(()) };
    let result = if ok {
        wire::capture_all(lua, engine, results.make_contiguous(), wire::MAX_DEPTH)
    } else {
        match results.into_iter().next().unwrap_or(LuaValue::Nil) {
            LuaValue::String(text) => Err(text.to_string_lossy()),
            LuaValue::Integer(number) => Err(number.to_string()),
            LuaValue::Number(number) => Err(number.to_string()),
            // (a table or other value thrown arrives as an empty text: measured)
            _ => Err(String::new()),
        }
    };
    send(engine, &endpoint, Msg::Return { call, result });
    Ok(())
}

/// An answer at the receive point: the caller resumes now with the results, or raises the error
pub fn deliver_return(lua: &Lua, engine: &Rc<Engine>, endpoint: &Rc<Endpoint>, call: u32, result: Result<Box<[Wire]>, String>) -> LuaResult<()> {
    let Some(thread) = endpoint.remotes.take(call) else { return Ok(()) };
    if !crate::scheduler::can_resume(engine, &thread) {
        return Ok(());
    }
    let map = mapper(engine, endpoint);
    let mut answer = LuaMultiValue::new();
    match result {
        Ok(results) => {
            answer.push_back(LuaValue::Boolean(true));
            for item in results.iter() {
                answer.push_back(wire::rebuilt(lua, engine, item, &map)?);
            }
        }
        Err(message) => {
            answer.push_back(LuaValue::Boolean(false));
            answer.push_back(LuaValue::String(lua.create_string(message)?));
        }
    }
    crate::scheduler::resume(lua, engine, &thread, answer);
    Ok(())
}

/// The player left: the server's InvokeClient calls waiting on it raise, as a server's do when the player leaves
pub fn player_left(lua: &Lua, engine: &Rc<Engine>, endpoint: &Rc<Endpoint>) -> LuaResult<()> {
    let threads: Vec<(u32, LuaThread)> = {
        let mut calls = endpoint.remotes.calls.borrow_mut();
        calls.iter_mut().enumerate().filter_map(|(index, slot)| slot.take().map(|thread| (index as u32, thread))).collect()
    };
    let name = endpoint.bridge.player.get().map(|(server, _)| engine.state.borrow().dom.at(server).name.to_string()).unwrap_or_default();
    for (call, thread) in threads {
        endpoint.remotes.free.borrow_mut().push(call);
        if crate::scheduler::can_resume(engine, &thread) {
            let message = format!("InvokeClient: {name} left the game before RemoteFunction returned");
            crate::scheduler::resume(lua, engine, &thread, LuaMultiValue::from_iter([LuaValue::Boolean(false), LuaValue::String(lua.create_string(message)?)]));
        }
    }
    Ok(())
}

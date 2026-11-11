//! RBXScriptSignal / RBXScriptConnection state with Roblox's deferred event semantics (port of
//! `runtime/engine/signal.luau`). Firing queues one fresh thread per connected handler (and
//! resumes threads blocked in :Wait()) at the next resumption point. Measured in Studio: handlers
//! run newest first; :Once() and :Wait() are part of the same chain (with Connect A, B, C, then
//! Once, then Wait, a fire resumes Wait, Once, C, B, A).
//!
//! Storage is reused: a disconnected connection's slot and a freed instance's signal slots go
//! back to a free list, so a session that builds and destroys UIs for hours holds what its live
//! signals and connections need, not one entry per signal ever made. What can outlive a slot
//! carries its generation: a connection id (script-held RBXScriptConnection values, queued handler
//! jobs) and a script-held RBXScriptSignal ([`Signals::generation`]). A stale one is a
//! disconnected connection / a signal nothing fires.

use super::value::Str;

/// A signal's slot. Ids held by the engine (an instance's signals, the services') are live by
/// construction; a script's RBXScriptSignal value also keeps the slot's generation.
pub type SignalId = u32;

/// A connection: its slot and the slot's generation when it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    index: u32,
    generation: u32,
}

impl ConnectionId {
    /// A connection that was never connected (Connect on a signal whose instance was freed)
    pub const DISCONNECTED: ConnectionId = ConnectionId { index: u32::MAX, generation: 0 };

    /// The connection's slot (reused once it is disconnected; diagnostics: census.rs)
    #[inline]
    pub fn slot(&self) -> u32 {
        self.index
    }
}

/// A registered handler: a connection, or a thread blocked in :Wait().
#[derive(Clone, Debug)]
pub enum Handler {
    Connection(ConnectionId),
    Thread(mlua::Thread),
}

#[derive(Debug)]
pub struct SignalData {
    /// Signal name (tostring "Signal <Name>")
    pub name: Str,
    /// The name Roblox's diagnostics give it: the declaring class and the event (`IntValue.Changed`,
    /// `BindableEvent.Event`), `Object.<Property>Changed` for a property's signal, `Instance.<Name>AttributeChanged`
    pub label: Str,
    pub handlers: Vec<Handler>,
    generation: u32,
}

#[derive(Debug)]
pub struct ConnectionData {
    pub connected: bool,
    /// The handler while connected, when it is not in the handler table (`Signals::handlers`)
    callback: Option<mlua::Function>,
    pub once: bool,
    /// A :Once() connection whose signal fired: it reads disconnected and left the signal's handlers, and its queued
    /// handler still runs once (then the slot goes)
    fired: bool,
    /// Connected with ConnectParallel: runs in the parallel phase, after the signal's serial handlers (actors.rs)
    pub parallel: bool,
    pub signal: SignalId,
    generation: u32,
}

#[derive(Default, Debug)]
pub struct Signals {
    signals: Vec<SignalData>,
    free_signals: Vec<SignalId>,
    /// The signals freed since the collector last asked ([`Signals::take_freed`])
    freed: Vec<SignalId>,
    connections: Vec<ConnectionData>,
    free_connections: Vec<u32>,
    /// The handlers of connected connections, by connection slot + 1, in one Luau table (`hold_in`):
    /// a Rust-side handle per handler takes a slot of the VM's reference stack each, and a story
    /// that rebuilds a large UI and keeps the old ones connected ran out of them (a million) after
    /// a few hundred rebuilds.
    ///
    /// A disconnected connection lets go of its handler: the closure and the instances it captures
    /// must be collectable, as in Roblox. (Kept, a UI a story removed stayed alive through the
    /// handlers it had disconnected.)
    handlers: Option<mlua::Table>,
    /// How many connected connections are ConnectParallel ones
    parallel_connections: usize,
}

impl Signals {
    /// Keeps handlers in `table` from now on (the engine's Luau state gives one at start).
    pub fn hold_in(&mut self, table: mlua::Table) {
        self.handlers = Some(table);
    }

    pub fn create(&mut self, name: Str) -> SignalId {
        let label = name.clone();
        self.create_labelled(name, label)
    }

    /// [`Self::create`] with the name Roblox's diagnostics give the signal
    pub fn create_labelled(&mut self, name: Str, label: Str) -> SignalId {
        if let Some(id) = self.free_signals.pop() {
            self.signals[id as usize].name = name;
            self.signals[id as usize].label = label;
            return id;
        }
        self.signals.push(SignalData { name, label, handlers: Vec::new(), generation: 0 });
        (self.signals.len() - 1) as SignalId
    }

    #[inline]
    pub fn get(&self, id: SignalId) -> &SignalData {
        &self.signals[id as usize]
    }

    /// The generation of a signal's slot: what a script's RBXScriptSignal value remembers, to
    /// know later whether the slot is still its signal (`is_current`).
    #[inline]
    pub fn generation(&self, id: SignalId) -> u32 {
        self.signals[id as usize].generation
    }

    #[inline]
    pub fn is_current(&self, id: SignalId, generation: u32) -> bool {
        self.signals.get(id as usize).is_some_and(|signal| signal.generation == generation)
    }

    /// Frees a signal whose owner is gone (a freed instance): its connections are disconnected,
    /// waiting threads dropped, and the slot is reused by a later `create`.
    pub fn free(&mut self, id: SignalId) {
        self.disconnect_all(id);
        let signal = &mut self.signals[id as usize];
        signal.name = Str::default();
        signal.handlers = Vec::new();
        signal.generation = signal.generation.wrapping_add(1);
        self.free_signals.push(id);
        self.freed.push(id);
    }

    /// The signals freed since this was last called: the collector's own and those a hook of a freed instance let go
    /// of (a track's marker signals). Their slots are reused, so what scripts were handed for them must be forgotten
    /// with them (lua::raw): a script asking for the slot's next signal got the dead one's userdata, and its handlers
    /// connected to nothing.
    pub fn take_freed(&mut self) -> Vec<SignalId> {
        std::mem::take(&mut self.freed)
    }

    /// (live signals, live connections) (diagnostics and tests)
    pub fn counts(&self) -> (usize, usize) {
        (self.signals.len() - self.free_signals.len(), self.connections.len() - self.free_connections.len())
    }

    /// (signal slots, connection slots) ever allocated (diagnostics and tests)
    pub fn capacity(&self) -> (usize, usize) {
        (self.signals.len(), self.connections.len())
    }

    /// The connection `id` names, while it is connected.
    #[inline]
    pub fn connection(&self, id: ConnectionId) -> Option<&ConnectionData> {
        self.connections.get(id.index as usize).filter(|data| data.generation == id.generation && data.connected)
    }

    /// RBXScriptConnection.Connected: false from the fire on for a :Once() connection (measured in a playing client,
    /// Studio 0.744: tools/parity/probes/signals.luau), while its queued handler is still to run.
    pub fn is_connected(&self, id: ConnectionId) -> bool {
        self.connection(id).is_some_and(|data| !data.fired)
    }

    pub fn connect(&mut self, signal: SignalId, callback: mlua::Function, once: bool) -> ConnectionId {
        self.connect_with(signal, callback, once, false)
    }

    pub fn connect_with(&mut self, signal: SignalId, callback: mlua::Function, once: bool, parallel: bool) -> ConnectionId {
        let index = match self.free_connections.pop() {
            Some(index) => index,
            None => {
                self.connections.push(ConnectionData { connected: false, callback: None, once: false, fired: false, parallel: false, signal: 0, generation: 0 });
                (self.connections.len() - 1) as u32
            }
        };
        let callback = match &self.handlers {
            Some(table) if table.raw_set(index as i64 + 1, &callback).is_ok() => None,
            _ => Some(callback),
        };
        let data = &mut self.connections[index as usize];
        (data.connected, data.callback, data.once, data.parallel, data.signal) = (true, callback, once, parallel, signal);
        data.fired = false;
        self.parallel_connections += usize::from(parallel);
        let id = ConnectionId { index, generation: data.generation };
        self.signals[signal as usize].handlers.push(Handler::Connection(id));
        id
    }

    /// Lets a connection's slot go (it was just disconnected): its handler is dropped and ids of
    /// it become stale.
    fn release(&mut self, index: u32) {
        let data = &mut self.connections[index as usize];
        self.parallel_connections -= usize::from(data.parallel && data.connected);
        data.connected = false;
        data.fired = false;
        data.generation = data.generation.wrapping_add(1);
        if data.callback.take().is_none() {
            if let Some(table) = &self.handlers {
                let _ = table.raw_set(index as i64 + 1, mlua::Value::Nil);
            }
        }
        self.free_connections.push(index);
        crate::census::released(index);
    }

    /// Registers a thread blocked in :Wait() (resumed once by the next fire).
    pub fn wait(&mut self, signal: SignalId, thread: mlua::Thread) {
        self.signals[signal as usize].handlers.push(Handler::Thread(thread));
    }

    pub fn disconnect(&mut self, connection: ConnectionId) {
        let Some(data) = self.connection(connection) else {
            return;
        };
        // (a :Once() connection that fired is disconnected already; its queued handler still runs. Not measured: a
        // Disconnect between the fire and that run)
        if data.fired {
            return;
        }
        let signal = data.signal;
        let handlers = &mut self.signals[signal as usize].handlers;
        if let Some(index) = handlers
            .iter()
            .position(|h| matches!(h, Handler::Connection(c) if *c == connection))
        {
            handlers.remove(index);
        }
        self.release(connection.index);
    }

    #[inline]
    pub fn has_handlers(&self, signal: SignalId) -> bool {
        !self.signals[signal as usize].handlers.is_empty()
    }

    /// The handlers of one fire, newest first, the ConnectParallel ones (also newest first) after the others; waiting
    /// threads leave the chain, and so do :Once() connections (they read disconnected from the fire on, and a second
    /// fire before their handler runs does not reach them: measured, Studio 0.744, tools/parity/probes/signals.luau).
    pub fn take_handlers(&mut self, signal: SignalId) -> Vec<Handler> {
        let connections = &mut self.connections;
        let handlers = &mut self.signals[signal as usize].handlers;
        let mut result: Vec<Handler> = handlers.iter().rev().cloned().collect();
        handlers.retain(|handler| match handler {
            Handler::Connection(id) => match connections.get_mut(id.index as usize) {
                Some(data) if data.once && data.connected && data.generation == id.generation => {
                    data.fired = true;
                    false
                }
                _ => true,
            },
            Handler::Thread(_) => false,
        });
        if self.parallel_connections > 0 {
            let parallel = |handler: &Handler| matches!(handler, Handler::Connection(id) if self.connection(*id).is_some_and(|data| data.parallel));
            if result.iter().any(parallel) {
                result.sort_by_key(parallel);
            }
        }
        result
    }

    /// Whether `connection` was made with ConnectParallel.
    pub fn is_parallel(&self, connection: ConnectionId) -> bool {
        self.connection(connection).is_some_and(|data| data.parallel)
    }

    /// Disconnects every connection and drops waiting threads.
    pub fn disconnect_all(&mut self, signal: SignalId) {
        let handlers = std::mem::take(&mut self.signals[signal as usize].handlers);
        for handler in handlers {
            if let Handler::Connection(id) = handler {
                if self.connection(id).is_some() {
                    self.release(id.index);
                }
            }
        }
    }

    /// For a connection handler about to run: whether it is still connected (a :Once()
    /// connection disconnects here, as RunHandler does), its callback and its signal.
    pub fn begin_handler(&mut self, connection: ConnectionId) -> Option<(mlua::Function, SignalId)> {
        let data = self.connection(connection)?;
        let (once, fired, held, signal) = (data.once, data.fired, data.callback.clone(), data.signal);
        let callback = held.or_else(|| self.handlers.as_ref()?.raw_get::<Option<mlua::Function>>(connection.index as i64 + 1).ok()?);
        if fired {
            // (it left the signal's handlers when the signal fired)
            self.release(connection.index);
        } else if once {
            self.disconnect(connection);
        }
        callback.map(|callback| (callback, signal))
    }

    /// Threads blocked in :Wait() on any signal (diagnostics: census.rs).
    pub fn visit_waiting(&self, visit: &mut dyn FnMut(&mlua::Thread)) {
        for signal in &self.signals {
            for handler in &signal.handlers {
                if let Handler::Thread(thread) = handler {
                    visit(thread);
                }
            }
        }
    }

    /// Every connected connection with its signal (diagnostics: census.rs).
    pub fn visit_connections(&self, visit: &mut dyn FnMut(SignalId, ConnectionId)) {
        for (index, signal) in self.signals.iter().enumerate() {
            for handler in &signal.handlers {
                if let Handler::Connection(id) = handler {
                    if self.connection(*id).is_some() {
                        visit(index as SignalId, *id);
                    }
                }
            }
        }
    }

    /// The connected handlers of a signal with their callbacks (diagnostics: `lua::leaks`).
    pub fn connected(&self, signal: SignalId) -> Vec<mlua::Function> {
        self.signals[signal as usize]
            .handlers
            .iter()
            .filter_map(|handler| match handler {
                Handler::Connection(id) => {
                    let data = self.connection(*id)?;
                    data.callback.clone().or_else(|| self.handlers.as_ref()?.raw_get::<Option<mlua::Function>>(id.index as i64 + 1).ok()?)
                }
                Handler::Thread(_) => None,
            })
            .collect()
    }
}

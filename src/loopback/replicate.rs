//! Replication, server to client: the copy of the replicated containers at the join, then the server's changes there, one
//! frame's at a time, in the stream with its remote traffic. Measured in a play session's two sides (Studio 0.747,
//! tools/parity/probes/loopback; docs/engine.md, Log 2026-11-19):
//!
//! - What reaches the client: the children of [`CONTAINERS`] (ReplicatedFirst's only at the join), not ServerStorage's
//!   or ServerScriptService's; the server's own Camera and the harness's RURFixtures stay the server's.
//! - Of an instance: its class, Name, every property but those rbx-dom marks NotReplicated (LocalTransparencyModifier among
//!   them, measured), its attributes and tags; a property holding an instance the client lacks arrives as nil; a server
//!   Script without its source (one whose RunContext is Client with it), a LocalScript or ModuleScript with it.
//! - When: a frame's changes go out when the frame ends, each as its value then, in the order first made: an instance made
//!   and written in the frame arrives whole, three writes to one property give one Changed with the last value, a write
//!   and its revert give none, two properties' Changed come in the order first written, and remote events keep their
//!   place among them (an instance made, parented and passed to FireClient arrives before the event).
//! - Hierarchy: an instance the server unparents or moves out of the containers keeps its copy on the client with Parent
//!   nil (it comes back as the same instance); a destroyed one is destroyed there.
//!
//! The client applies them at its receive point through the engine's setters, so its events fire, deferred, as for a
//! script's change; a physics move arrives without Changed. Nothing the client does reaches the server.

use std::rc::Rc;

use mlua::prelude::*;
use rustc_hash::FxHashSet;

use super::{Endpoint, Side, bridge::{Bridge, Msg, Node}};
use crate::{ClassId, Engine, InstanceId, K, NameId, Reflection, State, dom::flags, instance as core, value::{Str, Value}};

/// The services whose children reach the client (measured: a marker Folder in each service the server can parent to,
/// before and after the join, tools/parity/probes/loopback/replicated_services.luau). ReplicatedFirst's only at the join.
pub const CONTAINERS: [&str; 19] = [
    "Workspace",
    "ReplicatedStorage",
    "ReplicatedFirst",
    "Lighting",
    "Players",
    "StarterGui",
    "StarterPack",
    "StarterPlayer",
    "Teams",
    "SoundService",
    "Chat",
    "TextChatService",
    "LocalizationService",
    "MaterialService",
    "TestService",
    "InsertService",
    "MarketplaceService",
    "ProximityPromptService",
    "PolicyService",
];

/// The properties whose values stay on the server, by (class, name; None: any class). Measured (Studio 0.747,
/// tools/parity/probes/loopback/replicated_properties.luau: 70 properties of 25 classes written on the server, before and
/// after the join): every one arrives but LocalTransparencyModifier. rbx-dom's NotReplicated tag is not used: it marks a
/// property not sent as itself (a script's Enabled, sent as Disabled; a part's Color, Size and Position), whose value arrives
/// all the same. A Workspace's CurrentCamera is each side's own; Parent is the hierarchy's.
const NOT_REPLICATED: [(Option<&str>, &str); 2] = [(None, "LocalTransparencyModifier"), (Some("Workspace"), "CurrentCamera")];

/// What the server did this frame that the client may have to hear of, in the order first done (sealed at the frame's
/// end by [`flush`]: values are read then).
pub enum Change {
    Moved(InstanceId),
    Prop(InstanceId, NameId, bool),
    Attr(InstanceId, Str),
    Tag(InstanceId, Str),
    /// A remote message, in its place among the changes
    Message(Msg),
}

/// Which properties replicate ([`NOT_REPLICATED`])
#[derive(Default)]
pub struct Filter {
    names: Vec<NameId>,
    pairs: Vec<(ClassId, NameId)>,
}

impl Filter {
    pub fn new(reflection: &Reflection) -> Filter {
        let mut filter = Filter::default();
        for (class, name) in NOT_REPLICATED {
            let Some(name) = reflection.name_id(name) else { continue };
            match class {
                None => filter.names.push(name),
                Some(class) => {
                    if let Some(class) = reflection.class_id(class) {
                        filter.pairs.push((class, name));
                    }
                }
            }
        }
        filter
    }

    /// Whether property `name` of an instance of `class` replicates
    #[inline]
    pub fn replicates(&self, reflection: &Reflection, class: ClassId, name: NameId) -> bool {
        if name == K::Parent as NameId || self.names.contains(&name) {
            return false;
        }
        !self.pairs.iter().any(|&(owner, denied)| denied == name && reflection.class(class).is_a(owner))
    }
}

/// The services of `engine` that replicate, by name, as they are now (found, not made)
fn containers(engine: &Engine) -> Vec<(&'static str, InstanceId)> {
    let game = engine.world().game;
    CONTAINERS.iter().filter_map(|&name| crate::services::find_service(engine, game, name).map(|id| (name, id))).collect()
}

/// Makes what waits below `root` that only the server can make (project.rs `lazy_beyond_studio`), the made instances' own
/// Studio-only children too
fn make_lazy_regions(server: &Engine, root: InstanceId, excluded: &[InstanceId]) {
    let mut waiting = Vec::new();
    {
        let state = server.state.borrow();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if excluded.contains(&node) {
                continue;
            }
            if crate::project::lazy_beyond_studio(&state, node) {
                waiting.push(node);
                continue;
            }
            stack.extend_from_slice(&state.dom.at(node).children);
        }
    }
    for node in waiting {
        if server.state.borrow().dom.get(node).is_some() {
            crate::project::materialize_deep(server, node);
        }
    }
}

/// What a copy keeps of its original's place in the Studio snapshot: the node (none for a script's instance) and whether
/// its Studio children were made (lazy.rs)
#[inline]
fn studio_of(instance: &crate::dom::Instance) -> (crate::dom::StudioNode, u16) {
    let node = if instance.flags & flags::SCRIPT_CREATED != 0 { crate::dom::StudioNode::None } else { instance.studio_node };
    (node, instance.flags & (flags::STUDIO_ONLY | flags::MATERIALIZED))
}

/// The server instances that never replicate: its Camera, the harness's RURFixtures
fn excluded(server: &Engine) -> Vec<InstanceId> {
    let world = server.world();
    let mut out = vec![world.camera];
    let state = server.state.borrow();
    if let Some(storage) = crate::services::find_service(server, world.game, "ReplicatedStorage")
        && let Some(fixtures) = state.dom.find_child(storage, b"RURFixtures")
    {
        out.push(fixtures);
    }
    out
}

/// The join's copy of the server's replicated containers into the client, and the hooks that log the server's changes
/// from then on. The server's lazily made regions are made first (the copy is of instances). The loopback player pairs
/// with the client's local player, and the containers a server makes for it (Backpack, PlayerGui, StarterGear) with the
/// client's of their class, made where the client has none. No event fires for the copy: no client script runs yet.
pub fn join(server_lua: &Lua, server: &Rc<Engine>, client_lua: &Lua, client: &Rc<Engine>, bridge: &Rc<Bridge>, server_player: InstanceId, client_player: InstanceId) -> Result<(), String> {
    let _ = (server_lua, client_lua);
    let filter = Rc::new(Filter::new(&server.reflection));
    let excluded = excluded(server);
    // the client's services, made where it has none (with no state borrowed: services run code as they are made)
    let mut pairs = Vec::new();
    for (name, service) in containers(server) {
        // (what only the server can make, made now: a lazily imported plan's or a bundle's instances; the Studio snapshot's
        // stay lazy, each side making its own: lazy.rs)
        crate::timing::time("loopback: join: make the server's lazy regions", || make_lazy_regions(server, service, &excluded));
        let game = client.world().game;
        let copy = crate::services::get_service(client, game, name).map_err(|_| format!("the client cannot make {name}"))?;
        pairs.push((name, service, copy));
    }
    let copying = crate::timing::start("loopback: join: copy");
    let mut server_state = server.state.borrow_mut();
    let mut client_state = client.state.borrow_mut();
    crate::project::share_studio(&server_state, &mut client_state, client.world().game);
    let reflection = &client.reflection;
    let mut ids = bridge.ids.borrow_mut();
    let mut refs: Vec<(InstanceId, NameId, InstanceId)> = Vec::new();
    let mut created = 0u64;
    for &(name, service, copy) in &pairs {
        ids.pair(service, copy);
        copy_values(&server_state, &mut client_state, reflection, &filter, service, copy, false, &mut refs);
        server_state.dom.at_mut(service).flags |= flags::SENT;
        let mut stack: Vec<(InstanceId, InstanceId)> = server_state.dom.at(service).children.iter().rev().map(|&child| (child, copy)).collect();
        while let Some((node, parent)) = stack.pop() {
            if excluded.contains(&node) {
                continue;
            }
            let instance = server_state.dom.at(node);
            let (class, parent_of_node) = (instance.class, instance.parent);
            // the loopback player is the client's local one; its containers the client's of their class
            let paired = if node == server_player {
                Some(client_player)
            } else if parent_of_node == Some(server_player) {
                client_state.dom.at(client_player).children.iter().copied().find(|&child| client_state.dom.at(child).class == class && ids.to_server.get(child).is_none())
            } else {
                None
            };
            let made = match paired {
                Some(existing) => existing,
                None => {
                    created += 1;
                    let made = core::create(&mut client_state, reflection, class);
                    client_state.dom.attach(made, parent);
                    made
                }
            };
            ids.pair(node, made);
            copy_values(&server_state, &mut client_state, reflection, &filter, node, made, paired.is_none(), &mut refs);
            server_state.dom.at_mut(node).flags |= flags::SENT;
            stack.extend(server_state.dom.at(node).children.iter().rev().map(|&child| (child, made)));
        }
        let _ = name;
    }
    // instance values, once every copy is made: the client's copy, or nil for one it lacks
    for (copy, name, target) in refs {
        let value = ids.to_client.get(target).map_or(Value::Nil, Value::Instance);
        client_state.dom.at_mut(copy).store(name, value);
    }
    bridge.stats.borrow_mut().created += created;
    copying.stop();
    drop(ids);
    drop(client_state);
    drop(server_state);
    // the server's replicated instances from now on, and the hooks that log their changes (ReplicatedFirst replicates no
    // more after the join)
    {
        let mut state = server.state.borrow_mut();
        for &(name, service, _) in &pairs {
            if name == "ReplicatedFirst" {
                continue;
            }
            mark(&mut state, service, true, &excluded);
        }
    }
    server.state.borrow_mut().loopback_log = Some(Vec::new());
    install(server, bridge, filter, excluded);
    install_client(client, bridge);
    Ok(())
}

/// Copies an instance's name, replicated properties, attributes, tags and script source into its client copy
/// (properties holding instances are noted in `refs`, set once every copy is made)
/// (a `fresh` copy, made for it, takes them as one vector in the server's name order, as Clone gives a copy its own; a
/// paired one of the client's keeps what it has, a service's camera say)
#[allow(clippy::too_many_arguments)]
fn copy_values(server: &State, client: &mut State, reflection: &Reflection, filter: &Filter, from: InstanceId, to: InstanceId, fresh: bool, refs: &mut Vec<(InstanceId, NameId, InstanceId)>) {
    let source = server.dom.at(from);
    let class = source.class;
    let script = script_source(reflection, source);
    let mut props: Vec<(NameId, Value)> = Vec::with_capacity(source.props.len());
    for (name, value) in source.props.iter() {
        if !filter.replicates(reflection, class, name) {
            continue;
        }
        if let Some(target) = value.as_instance() {
            refs.push((to, name, target));
            continue;
        }
        if let Some(value) = super::wire::portable(value) {
            props.push((name, value));
        }
    }
    let attributes: crate::dom::Attributes = source.attributes.iter().filter_map(|(name, value)| super::wire::portable(value).map(|value| (name.clone(), value))).collect::<Vec<_>>().into();
    let tags = source.tags.clone();
    let json = source.flags & flags::JSON_MODULE;
    let (studio, studio_flags) = studio_of(source);
    let target = client.dom.at_mut(to);
    target.studio_node = studio;
    target.flags |= studio_flags;
    if fresh {
        target.props = crate::dom::Props::from(props);
    } else {
        for (name, value) in props {
            target.store(name, value);
        }
    }
    target.attributes = attributes;
    for tag in tags.iter() {
        if !target.tags.contains(tag) {
            target.tags.push(tag.clone());
        }
    }
    target.flags |= json;
    if let Some(script) = script {
        target.script = Some(Box::new(script));
    }
    client.dom.refresh_name(reflection, to);
    for tag in tags.iter() {
        crate::services::tag_added(client, to, tag);
    }
}

/// What source a client copy of a script gets: a LocalScript's, a ModuleScript's and a Client-context Script's, not a server
/// Script's
fn script_source(reflection: &Reflection, instance: &crate::dom::Instance) -> Option<crate::dom::ScriptSource> {
    let script = instance.script.as_deref()?;
    let class = &*reflection.class(instance.class).name;
    let runs_on_client = match class {
        "LocalScript" | "ModuleScript" => true,
        "Script" => {
            let client = reflection.enum_item("RunContext", "Client");
            reflection.name_id("RunContext").and_then(|name| instance.props.get(name)).and_then(Value::as_enum).is_some_and(|item| Some(item) == client)
        }
        _ => false,
    };
    runs_on_client.then(|| script.clone())
}

/// Sets or clears [`flags::REPLICATED`] on `root` and what is below it (an excluded instance and its subtree stay out)
pub(super) fn mark(state: &mut State, root: InstanceId, on: bool, excluded: &[InstanceId]) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if on && excluded.contains(&node) {
            continue;
        }
        let instance = state.dom.at_mut(node);
        if on {
            instance.flags |= flags::REPLICATED;
        } else {
            if instance.flags & flags::SENT != 0 {
                instance.flags |= flags::STALE;
            }
            instance.flags &= !flags::REPLICATED;
        }
        stack.extend_from_slice(&state.dom.at(node).children);
    }
}

/// The server's hooks: a moved, written, attributed or tagged instance that replicates is logged for the frame's flush
/// ([`flush`]); each tests the instance's flag first, so a change elsewhere costs one test.
fn install(server: &Rc<Engine>, bridge: &Rc<Bridge>, filter: Rc<Filter>, excluded: Vec<InstanceId>) {
    *bridge.filter.borrow_mut() = Some(filter);
    let mut hooks = server.hooks.borrow_mut();
    hooks.hierarchy_changed.push(Box::new(move |state, id, _old, new| {
        let was = state.dom.at(id).flags & flags::REPLICATED != 0;
        let now = new.is_some_and(|parent| state.dom.at(parent).flags & flags::REPLICATED != 0) && !excluded.contains(&id);
        if was != now {
            mark(state, id, now, &excluded);
        }
        if (was || now || state.dom.at(id).flags & flags::SENT != 0)
            && let Some(log) = state.loopback_log.as_mut()
        {
            log.push(Change::Moved(id));
        }
    }));
    hooks.property_changed.push(Box::new(move |state, id, name| {
        if state.dom.at(id).flags & flags::REPLICATED == 0 || name == K::Parent as NameId {
            return;
        }
        let quiet = state.quiet_write;
        if let Some(log) = state.loopback_log.as_mut() {
            log.push(Change::Prop(id, name, quiet));
        }
    }));
    hooks.attribute_changed.push(Box::new(move |state, id, name| {
        if state.dom.at(id).flags & flags::REPLICATED == 0 {
            return;
        }
        if let Some(log) = state.loopback_log.as_mut() {
            log.push(Change::Attr(id, Str::from(name)));
        }
    }));
    let log = bridge.clone();
    hooks.instance_freed.push(Box::new(move |_, id| {
        log.ids.borrow_mut().to_client.remove(id);
    }));
}

/// The client's hook: a copy it frees forgets its server instance
pub fn install_client(client: &Rc<Engine>, bridge: &Rc<Bridge>) {
    let ids = bridge.clone();
    client.hooks.borrow_mut().instance_freed.push(Box::new(move |_, id| {
        ids.ids.borrow_mut().to_server.remove(id);
    }));
}

/// The server's frame ended: its logged changes become messages, each read now, in the order first made, with its remote
/// messages in their places ([`Change`]); then the batch is sealed (bridge.rs).
pub fn flush(server: &Engine, bridge: &Bridge) {
    let mut state = server.state.borrow_mut();
    let Some(log) = state.loopback_log.as_mut().filter(|log| !log.is_empty()).map(std::mem::take) else { return };
    let Some(filter) = bridge.filter.borrow().clone() else { return };
    let reflection = &server.reflection;
    let mut out: Vec<Msg> = Vec::with_capacity(log.len());
    let mut moved: FxHashSet<InstanceId> = FxHashSet::default();
    let mut written: FxHashSet<(InstanceId, NameId)> = FxHashSet::default();
    let mut attributed: FxHashSet<(InstanceId, Str)> = FxHashSet::default();
    let mut tagged: FxHashSet<(InstanceId, Str)> = FxHashSet::default();
    // instances whose whole state went out this flush (made, or brought back): their later changes are in it
    let mut whole: FxHashSet<InstanceId> = FxHashSet::default();
    let mut writes = 0u64;
    let mut created = 0u64;
    for change in log {
        match change {
            Change::Message(message) => out.push(message),
            Change::Moved(id) => {
                if moved.insert(id) {
                    created += flush_move(&mut state, reflection, &filter, id, &mut out, &mut whole);
                }
            }
            Change::Prop(id, name, quiet) => {
                if whole.contains(&id) || !written.insert((id, name)) || !sent_and_replicated(&state, id) {
                    continue;
                }
                let class = state.dom.at(id).class;
                if !filter.replicates(reflection, class, name) {
                    continue;
                }
                let value = state.dom.raw_get(id, name).and_then(super::wire::portable).unwrap_or(Value::Nil);
                writes += 1;
                out.push(Msg::Prop { id, name, value, quiet });
            }
            Change::Attr(id, name) => {
                if whole.contains(&id) || !sent_and_replicated(&state, id) || !attributed.insert((id, name.clone())) {
                    continue;
                }
                let value = state.dom.at(id).attribute(name.as_bytes()).and_then(super::wire::portable).unwrap_or(Value::Nil);
                out.push(Msg::Attr { id, name, value });
            }
            Change::Tag(id, tag) => {
                if whole.contains(&id) || !sent_and_replicated(&state, id) || !tagged.insert((id, tag.clone())) {
                    continue;
                }
                let added = state.dom.at(id).tags.contains(&tag);
                out.push(Msg::Tag { id, tag, added });
            }
        }
    }
    {
        let mut stats = bridge.stats.borrow_mut();
        stats.writes += writes;
        stats.created += created;
    }
    drop(state);
    bridge.outgoing(Side::Server).borrow_mut().open.extend(out);
}

/// An instance the client has a copy of (or will, from a message already sealed) that replicates now
#[inline]
fn sent_and_replicated(state: &State, id: InstanceId) -> bool {
    state.dom.get(id).is_some_and(|instance| instance.flags & (flags::SENT | flags::REPLICATED) == flags::SENT | flags::REPLICATED)
}

/// What a moved instance comes to: a tree the client makes (a new instance entering), a move of the client's copy (in or
/// out of the containers), a destroy, or nothing (never sent and not replicated). Returns how many instances it made.
fn flush_move(state: &mut State, reflection: &Reflection, filter: &Filter, id: InstanceId, out: &mut Vec<Msg>, whole: &mut FxHashSet<InstanceId>) -> u64 {
    let Some(instance) = state.dom.get(id) else { return 0 };
    let sent = instance.flags & flags::SENT != 0;
    if instance.flags & flags::DESTROYED != 0 {
        if sent {
            out.push(Msg::Destroy { id });
        }
        return 0;
    }
    let now = instance.flags & flags::REPLICATED != 0;
    let parent = instance.parent;
    match (now, sent) {
        (true, false) => {
            let Some(parent) = parent else { return 0 };
            let nodes = tree(state, reflection, filter, id, whole);
            let count = nodes.len() as u64;
            out.push(Msg::Tree { parent, nodes });
            count
        }
        (true, true) => {
            out.push(Msg::Parent { id, parent });
            // back in the containers after time out of them: what changed meanwhile, and children made meanwhile
            let mut made = 0;
            let mut stack = vec![id];
            while let Some(node) = stack.pop() {
                let flags_now = state.dom.at(node).flags;
                if flags_now & flags::REPLICATED == 0 {
                    continue;
                }
                if flags_now & flags::SENT == 0 {
                    let Some(parent) = state.dom.at(node).parent else { continue };
                    let nodes = tree(state, reflection, filter, node, whole);
                    made += nodes.len() as u64;
                    out.push(Msg::Tree { parent, nodes });
                    continue;
                }
                if flags_now & flags::STALE != 0 {
                    state.dom.at_mut(node).flags &= !flags::STALE;
                    whole.insert(node);
                    let class = state.dom.at(node).class;
                    for (name, value) in state.dom.at(node).props.iter() {
                        if filter.replicates(reflection, class, name)
                            && let Some(value) = super::wire::portable(value)
                        {
                            out.push(Msg::Prop { id: node, name, value, quiet: false });
                        }
                    }
                    for (name, value) in state.dom.at(node).attributes.iter() {
                        if let Some(value) = super::wire::portable(value) {
                            out.push(Msg::Attr { id: node, name: name.clone(), value });
                        }
                    }
                }
                stack.extend(state.dom.at(node).children.iter().rev());
            }
            made
        }
        (false, true) => {
            out.push(Msg::Parent { id, parent: None });
            0
        }
        (false, false) => 0,
    }
}

/// `root` and its replicated descendants as the client makes them (marked sent), in GetDescendants' order
fn tree(state: &mut State, reflection: &Reflection, filter: &Filter, root: InstanceId, whole: &mut FxHashSet<InstanceId>) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<(InstanceId, Option<u32>)> = vec![(root, None)];
    while let Some((id, parent)) = stack.pop() {
        let instance = state.dom.at(id);
        if instance.flags & flags::REPLICATED == 0 {
            continue;
        }
        let class = instance.class;
        // (in name order, as the instance stores them: the client's copy takes the vector as it is)
        let mut props: Vec<(NameId, Value)> = Vec::with_capacity(instance.props.len());
        for (name, value) in instance.props.iter() {
            if filter.replicates(reflection, class, name)
                && let Some(value) = super::wire::portable(value)
            {
                props.push((name, value));
            }
        }
        let (studio, studio_flags) = studio_of(instance);
        let node = Node {
            id,
            class,
            parent,
            props,
            attributes: instance.attributes.iter().filter_map(|(name, value)| super::wire::portable(value).map(|value| (name.clone(), value))).collect(),
            tags: instance.tags.iter().cloned().collect(),
            source: script_source(reflection, instance),
            json: instance.flags & flags::JSON_MODULE != 0,
            studio,
            studio_flags,
        };
        let index = nodes.len() as u32;
        nodes.push(node);
        stack.extend(instance.children.iter().rev().map(|&child| (child, Some(index))));
        let target = state.dom.at_mut(id);
        target.flags |= flags::SENT;
        target.flags &= !flags::STALE;
        whole.insert(id);
    }
    nodes
}

/// A replication message at the client's receive point: through the engine's setters, so its events queue as for a
/// script's change (deferred: they run at the legacy step's flush)
pub fn apply(lua: &Lua, client: &Rc<Engine>, endpoint: &Rc<Endpoint>, message: Msg) -> LuaResult<()> {
    let bridge = &endpoint.bridge;
    // (the server's Studio-only instances are paired as they are met: lazy.rs)
    let map = |id: InstanceId| super::lazy::to_client(client, endpoint, id);
    let live = |id: InstanceId| -> Option<InstanceId> {
        let copy = map(id)?;
        let state = client.state.borrow();
        state.dom.get(copy).filter(|instance| instance.flags & flags::DESTROYED == 0).map(|_| copy)
    };
    let mapped = |value: Value| -> Value {
        match value {
            Value::Instance(target) => map(target).map_or(Value::Nil, Value::Instance),
            other => other,
        }
    };
    match message {
        Msg::Tree { parent, mut nodes } => {
            let Some(parent) = live(parent) else { return Ok(()) };
            let reflection = &client.reflection;
            let mut made: Vec<InstanceId> = Vec::with_capacity(nodes.len());
            let mut refs: Vec<(InstanceId, NameId, InstanceId)> = Vec::new();
            {
                let mut state = client.state.borrow_mut();
                let mut ids = bridge.ids.borrow_mut();
                for node in nodes.iter_mut() {
                    let copy = core::create(&mut state, reflection, node.class);
                    {
                        // (the server's values in name order, as Clone gives a copy its vector; instance values once
                        // every copy is made)
                        let mut props = std::mem::take(&mut node.props);
                        props.retain(|(name, value)| match value.as_instance() {
                            Some(instance) => {
                                refs.push((copy, *name, instance));
                                false
                            }
                            None => true,
                        });
                        let target = state.dom.at_mut(copy);
                        target.props = crate::dom::Props::from(props);
                        target.attributes = std::mem::take(&mut node.attributes).into();
                        target.tags = node.tags.clone().into();
                        if node.json {
                            target.flags |= flags::JSON_MODULE;
                        }
                        if let Some(source) = &node.source {
                            target.script = Some(Box::new(source.clone()));
                        }
                        target.studio_node = node.studio;
                        target.flags |= node.studio_flags;
                    }
                    state.dom.refresh_name(reflection, copy);
                    if let Some(index) = node.parent {
                        state.dom.attach(copy, made[index as usize]);
                    }
                    ids.pair(node.id, copy);
                    made.push(copy);
                }
                for (node, &copy) in nodes.iter().zip(&made) {
                    for tag in &node.tags {
                        crate::services::tag_added(&mut state, copy, tag);
                    }
                }
            }
            // instance values, every copy made (one the client makes of the snapshot is paired now: nothing borrowed)
            for (copy, name, target) in refs {
                let value = map(target).map_or(Value::Nil, Value::Instance);
                client.state.borrow_mut().dom.at_mut(copy).store(name, value);
            }
            // the tree enters at once: its DescendantAdded, ChildAdded, the tagged instances' added signals
            if let Some(&root) = made.first() {
                let _ = crate::lua::instance::set_parent(client, root, Some(parent));
            }
        }
        Msg::Parent { id, parent } => {
            let Some(copy) = live(id) else { return Ok(()) };
            let parent = parent.and_then(live);
            // (a copy the server took out of the containers stays the server's instance: kept while it is out)
            {
                let mut state = client.state.borrow_mut();
                if parent.is_none() {
                    state.dom.pin(copy);
                } else if state.dom.is_pinned(copy) {
                    state.dom.unpin(copy);
                }
            }
            let _ = crate::lua::instance::set_parent(client, copy, parent);
        }
        Msg::Prop { id, name, value, quiet } => {
            let Some(copy) = live(id) else { return Ok(()) };
            let value = mapped(value);
            let hooks = client.hooks.borrow();
            let mut state = client.state.borrow_mut();
            if quiet {
                core::raw_set_unsignalled(&mut state, &client.reflection, &hooks, copy, name, value);
            } else {
                core::raw_set(&mut state, &client.reflection, &hooks, copy, name, value);
            }
        }
        Msg::Attr { id, name, value } => {
            let Some(copy) = live(id) else { return Ok(()) };
            let value = mapped(value);
            let mut state = client.state.borrow_mut();
            core::set_attribute(&mut state, &client.reflection, copy, name, value);
        }
        Msg::Tag { id, tag, added } => {
            let Some(copy) = live(id) else { return Ok(()) };
            let mut state = client.state.borrow_mut();
            if added {
                core::add_tag(&mut state, copy, tag);
            } else {
                core::remove_tag(&mut state, copy, &tag);
            }
        }
        Msg::Destroy { id } => {
            let Some(copy) = live(id) else { return Ok(()) };
            if client.state.borrow().dom.is_pinned(copy) {
                client.state.borrow_mut().dom.unpin(copy);
            }
            let _ = crate::lua::instance::destroy(lua, client, copy);
        }
        Msg::Event { .. } | Msg::Invoke { .. } | Msg::Return { .. } => {}
    }
    Ok(())
}

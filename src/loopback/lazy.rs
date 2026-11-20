//! The place's Studio-only instances (project.rs `materialize`: the placeholders a Studio hierarchy snapshot adds below an
//! instance once something looks at its children) stay lazy on both sides of a loopback pair. The join copies what the
//! server has made, each copy keeping its hierarchy node and whether its Studio children were made; each side then makes
//! a copy's Studio children on its own when its scripts look, from the same snapshot, so the two sides' are the same
//! instances. The two are paired when a message first names one: by the instance it was made under (paired itself, or
//! paired the same way) and its node in the snapshot, the other side making that instance's Studio children first if it
//! has not. A server instance made this way under a sent one is replicated (its changes go to the client) and counts as
//! sent; one the client cannot make itself (a plan's or a bundle's) is sent as a script's new instance is.

use std::rc::Rc;

use rustc_hash::FxHashMap;

use super::{Endpoint, Side, bridge::Msg, replicate::Change, wire::Wire};
use crate::{
    Engine, InstanceId,
    dom::{StudioNode, flags},
};

/// What one side made of the snapshot after the join
#[derive(Default)]
pub struct Lazy {
    /// A made instance: the instance it was made under, its node
    origin: FxHashMap<InstanceId, (InstanceId, u32)>,
    /// (the instance made under, the node) to the made instance
    made: FxHashMap<(InstanceId, u32), InstanceId>,
}

impl Lazy {
    /// How many instances this side made of the snapshot since the join
    pub fn made(&self) -> usize {
        self.made.len()
    }
}

/// After `id`'s children were made (lua/instance.rs `materialize`) on a linked engine: the children past `before` are new.
/// A server's new children of a replicated instance replicate: a Studio-only one under a sent instance is the client's to
/// make (lazily paired), any other is sent as a new instance.
pub fn materialized(engine: &Engine, id: InstanceId, before: usize) {
    let Some(endpoint) = engine.loopback.borrow().clone() else { return };
    let mut state = engine.state.borrow_mut();
    let Some(instance) = state.dom.get(id) else { return };
    if instance.children.len() <= before {
        return;
    }
    let made: Vec<InstanceId> = instance.children[before..].to_vec();
    let parent_flags = instance.flags;
    let server = endpoint.side == Side::Server;
    if server && parent_flags & flags::REPLICATED == 0 {
        return;
    }
    let shared = !server || parent_flags & flags::SENT != 0;
    let mut lazy = endpoint.lazy.borrow_mut();
    for child in made {
        let node = match state.dom.at(child).studio_node {
            StudioNode::Node(node) if state.dom.at(child).flags & flags::STUDIO_ONLY != 0 => Some(node),
            _ => None,
        };
        match (shared, node) {
            (true, Some(node)) => {
                lazy.origin.insert(child, (id, node));
                lazy.made.insert((id, node), child);
                if server {
                    state.dom.at_mut(child).flags |= flags::REPLICATED | flags::SENT;
                }
            }
            _ if server => {
                // (a parent not sent yet takes it along; otherwise it goes as a new instance)
                super::replicate::mark(&mut state, child, true, &[]);
                if parent_flags & flags::SENT != 0
                    && let Some(log) = state.loopback_log.as_mut()
                {
                    log.push(Change::Moved(child));
                }
            }
            _ => {}
        }
    }
}

/// The client's instance for the server's `id`: the paired one, or the one the client makes of the snapshot where the
/// server made `id` (paired now). None for an instance the client has no copy of.
pub fn to_client(client: &Engine, endpoint: &Endpoint, id: InstanceId) -> Option<InstanceId> {
    if let Some(copy) = endpoint.bridge.ids.borrow().to_client.get(id) {
        return Some(copy);
    }
    let (_, server) = client.peer_server()?;
    let server_end = server.loopback()?;
    pair_lazily(&server, &server_end, client, endpoint, id, Side::Client)
}

/// The server's instance for the client's `id`: the paired one, or the one the server makes of the snapshot where the
/// client made `id` (paired now)
pub fn to_server(client: &Engine, endpoint: &Endpoint, id: InstanceId) -> Option<InstanceId> {
    if let Some(original) = endpoint.bridge.ids.borrow().to_server.get(id) {
        return Some(original);
    }
    let (_, server) = client.peer_server()?;
    let server_end = server.loopback()?;
    pair_lazily(client, endpoint, &server, &server_end, id, Side::Server)
}

/// `id` of `from`, made of the snapshot, paired with `to`'s instance of the same node under the counterpart of what it was
/// made under (`to` makes that one's Studio children first)
fn pair_lazily(from: &Engine, from_end: &Endpoint, to: &Engine, to_end: &Endpoint, id: InstanceId, side: Side) -> Option<InstanceId> {
    let (under, node) = from_end.lazy.borrow().origin.get(&id).copied()?;
    let under = match side {
        Side::Client => to_client(to, to_end, under)?,
        Side::Server => to_server(from, from_end, under)?,
    };
    crate::lua::instance::materialize(to, under);
    let made = to_end.lazy.borrow().made.get(&(under, node)).copied()?;
    to.state.borrow().dom.get(made)?;
    let mut ids = from_end.bridge.ids.borrow_mut();
    match side {
        Side::Client => ids.pair(id, made),
        Side::Server => ids.pair(made, id),
    }
    Some(made)
}

/// A client's message about to go: every instance it names that the client made of the snapshot is paired with the
/// server's now (the server's mapping reads only the pairs)
pub fn pair_sent(client: &Engine, endpoint: &Rc<Endpoint>, message: &Msg) {
    if endpoint.side != Side::Client || endpoint.lazy.borrow().origin.is_empty() {
        return;
    }
    let mut pair = |id: InstanceId| {
        let _ = to_server(client, endpoint, id);
    };
    match message {
        Msg::Event { remote, args } | Msg::Invoke { remote, args, .. } => {
            pair(*remote);
            args.iter().for_each(|wire| each_instance(wire, &mut pair));
        }
        Msg::Return { result: Ok(results), .. } => results.iter().for_each(|wire| each_instance(wire, &mut pair)),
        _ => {}
    }
}

fn each_instance(wire: &Wire, f: &mut dyn FnMut(InstanceId)) {
    match wire {
        Wire::Instance(id) => f(*id),
        Wire::Table(entries) => entries.iter().for_each(|(_, item)| each_instance(item, f)),
        _ => {}
    }
}

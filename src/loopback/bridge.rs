//! What links a loopback pair's two engines: one ordered stream of messages each way, the id maps between the two
//! DataModels, the invokes waiting for an answer, and the counts a run's summary reports. Plain Rust data: no Lua value
//! crosses from one VM to the other (a waiting caller's thread is held only by its own side's table of calls).
//!
//! A message a side sends during its frame goes into that side's open batch; the batch is sealed when the frame ends and
//! delivered at the receiver's next frame start (its receive point, `loopback::receive`), or `latency - 1` frames after
//! that. Measured (tools/parity/probes/loopback/remote_latency.luau): an event echoed by the other side comes back at the
//! start of the sender's next frame, from either side.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use crate::{InstanceId, NameId, value::{Str, Value}};


use super::{Side, wire::Wire};

/// One message of a stream.
pub enum Msg {
    /// Instances the server made, or that entered the replicated containers, as they are when the batch is sealed: a tree
    /// the client makes and puts under `parent` at once (its first node the root, in GetDescendants' order)
    Tree { parent: InstanceId, nodes: Vec<Node> },
    /// An instance the client has moved: under `parent` (mapped at delivery), or out of the replicated containers (None)
    Parent { id: InstanceId, parent: Option<InstanceId> },
    /// A property's value when its batch was sealed (instance values mapped at delivery); `quiet`: a physics move, which
    /// fires no Changed
    Prop { id: InstanceId, name: NameId, value: Value, quiet: bool },
    /// An attribute's value when its batch was sealed (nil: removed)
    Attr { id: InstanceId, name: Str, value: Value },
    /// A tag added or removed
    Tag { id: InstanceId, tag: Str, added: bool },
    /// An instance the server destroyed
    Destroy { id: InstanceId },
    /// RemoteEvent / UnreliableRemoteEvent: OnClientEvent or OnServerEvent with these arguments (instance ids are the
    /// sender's, mapped at delivery)
    Event { remote: InstanceId, args: Box<[Wire]> },
    /// RemoteFunction: the callback with these arguments; `call` names the caller's wait
    Invoke { remote: InstanceId, call: u32, args: Box<[Wire]> },
    /// The answer to an `Invoke`: the callback's results, or its error's text
    Return { call: u32, result: Result<Box<[Wire]>, String> },
}

/// One instance of a [`Msg::Tree`]: the server's id, its class, its parent's index in the tree (none: the root), its
/// replicated properties (instance values the server's ids), attributes, tags and the source a client copy gets
pub struct Node {
    pub id: InstanceId,
    pub class: crate::ClassId,
    pub parent: Option<u32>,
    pub props: Vec<(NameId, Value)>,
    pub attributes: Vec<(Str, Value)>,
    pub tags: Vec<Str>,
    pub source: Option<crate::dom::ScriptSource>,
    pub json: bool,
    /// Its place in the Studio snapshot and whether its Studio children were made (lazy.rs): the client makes them itself
    pub studio: crate::dom::StudioNode,
    pub studio_flags: u16,
}

/// Messages sealed at the end of a sender's frame, delivered at the receiver's frame `due`.
struct Batch {
    due: u64,
    messages: Vec<Msg>,
}

/// One direction: the sender's open batch and the sealed batches on their way.
#[derive(Default)]
pub struct Stream {
    /// What the sender's frame sent so far, in order (a server's changes are sealed into messages at the frame's end:
    /// replicate.rs)
    pub open: Vec<Msg>,
    /// Unreliable events of the open frame: they arrive after the frame's reliable messages (measured,
    /// tools/parity/probes/loopback/replication_order.luau)
    pub unreliable: Vec<Msg>,
    sealed: VecDeque<Batch>,
}

impl Stream {
    /// Seals the open messages as a batch due at the receiver's frame `due` (nothing when there are none: no allocation)
    fn seal(&mut self, due: u64) {
        if self.open.is_empty() && self.unreliable.is_empty() {
            return;
        }
        let mut messages = std::mem::take(&mut self.open);
        messages.append(&mut self.unreliable);
        self.sealed.push_back(Batch { due, messages });
    }

    /// The batches due by the receiver's frame `frame`, oldest first
    fn take_due(&mut self, frame: u64) -> Option<Vec<Msg>> {
        if self.sealed.front().is_none_or(|batch| batch.due > frame) {
            return None;
        }
        let mut out = self.sealed.pop_front().map(|batch| batch.messages).unwrap_or_default();
        while self.sealed.front().is_some_and(|batch| batch.due <= frame) {
            if let Some(mut batch) = self.sealed.pop_front() {
                out.append(&mut batch.messages);
            }
        }
        Some(out)
    }

    pub fn pending(&self) -> usize {
        self.sealed.iter().map(|batch| batch.messages.len()).sum::<usize>() + self.open.len() + self.unreliable.len()
    }
}

/// Instance ids of one DataModel by the other's slot: an entry keeps the generation it was made for, so a reused slot
/// finds nothing (a lookup is one array load and a compare).
#[derive(Default)]
pub struct IdMap {
    slots: Vec<(u32, InstanceId)>,
}

const NONE: (u32, InstanceId) = (u32::MAX, InstanceId { index: u32::MAX, generation: u32::MAX });

impl IdMap {
    #[inline]
    pub fn get(&self, from: InstanceId) -> Option<InstanceId> {
        let (generation, to) = *self.slots.get(from.index as usize)?;
        (generation == from.generation).then_some(to)
    }

    pub fn set(&mut self, from: InstanceId, to: InstanceId) {
        let index = from.index as usize;
        if self.slots.len() <= index {
            self.slots.resize(index + 1, NONE);
        }
        self.slots[index] = (from.generation, to);
    }

    pub fn remove(&mut self, from: InstanceId) {
        if let Some(slot) = self.slots.get_mut(from.index as usize)
            && slot.0 == from.generation
        {
            *slot = NONE;
        }
    }

    /// Bytes held (the bench's memory row)
    pub fn bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<(u32, InstanceId)>()
    }
}

/// Server instances and their client copies, both ways.
#[derive(Default)]
pub struct Ids {
    pub to_client: IdMap,
    pub to_server: IdMap,
}

impl Ids {
    pub fn pair(&mut self, server: InstanceId, client: InstanceId) {
        self.to_client.set(server, client);
        self.to_server.set(client, server);
    }

    /// The instance on `to` side of one on the other side
    #[inline]
    pub fn map(&self, to: Side, id: InstanceId) -> Option<InstanceId> {
        match to {
            Side::Client => self.to_client.get(id),
            Side::Server => self.to_server.get(id),
        }
    }
}

/// What a run's summary reports of the link (`summary.loopback`)
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub messages: [u64; 2],
    pub created: u64,
    pub writes: u64,
}

pub struct Bridge {
    /// Frames each way: a batch sealed at a frame's end is delivered at the receiver's frame start this many frames on (1:
    /// its next frame, the measured minimum of a local session)
    pub latency: u32,
    /// [server to client, client to server]
    streams: [RefCell<Stream>; 2],
    /// Frames each side started (its receive points so far)
    frames: [Cell<u64>; 2],
    pub ids: RefCell<Ids>,
    /// The loopback player: (the server's, the client's)
    pub player: Cell<Option<(InstanceId, InstanceId)>>,
    /// The player has joined (the server's Player is in Players and the client steps)
    pub joined: Cell<bool>,
    /// The server's harness asked to hold the join (Context.HoldJoin) / to join now (Context.Join)
    pub hold_join: Cell<bool>,
    pub join_asked: Cell<bool>,
    /// The player left (Context.Leave): the client steps no more
    pub left: Cell<bool>,
    pub leave_asked: Cell<bool>,
    /// The frame the player joined at (the pair's frames stepped before)
    pub joined_at: Cell<u64>,
    /// A recording samples the server's world
    pub record_server: Cell<bool>,
    pub stats: RefCell<Stats>,
    /// Which properties replicate (made at the join)
    pub filter: RefCell<Option<std::rc::Rc<super::replicate::Filter>>>,
}

impl Bridge {
    pub fn new(latency: u32) -> Bridge {
        Bridge {
            latency: latency.max(1),
            streams: Default::default(),
            frames: Default::default(),
            ids: Default::default(),
            player: Cell::new(None),
            joined: Cell::new(false),
            hold_join: Cell::new(false),
            join_asked: Cell::new(false),
            left: Cell::new(false),
            leave_asked: Cell::new(false),
            joined_at: Cell::new(0),
            record_server: Cell::new(false),
            stats: Default::default(),
            filter: Default::default(),
        }
    }

    /// The stream `from` sends on
    #[inline]
    pub fn outgoing(&self, from: Side) -> &RefCell<Stream> {
        &self.streams[from.index()]
    }

    /// Queues a reliable message `from` a side, in order with its others
    pub fn send(&self, from: Side, message: Msg) {
        self.streams[from.index()].borrow_mut().open.push(message);
    }

    /// Queues an unreliable event: after the frame's reliable messages
    pub fn send_unreliable(&self, from: Side, message: Msg) {
        self.streams[from.index()].borrow_mut().unreliable.push(message);
    }

    /// The sender's frame ended: what it sent goes out, due at the other side's frame `latency` frames after its last
    /// started one
    pub fn seal(&self, from: Side) {
        let receiver = from.other();
        let due = self.frames[receiver.index()].get() + u64::from(self.latency);
        let mut stream = self.streams[from.index()].borrow_mut();
        let count = stream.open.len() + stream.unreliable.len();
        if count > 0 {
            self.stats.borrow_mut().messages[from.index()] += count as u64;
        }
        stream.seal(due);
    }

    /// `side` starts a frame: the messages due for it now, in order (none: one compare, no allocation)
    pub fn begin_frame(&self, side: Side) -> Option<Vec<Msg>> {
        let frame = self.frames[side.index()].get() + 1;
        self.frames[side.index()].set(frame);
        self.streams[side.other().index()].borrow_mut().take_due(frame)
    }

    /// Messages on their way, both ways (tests)
    pub fn in_flight(&self) -> usize {
        self.streams.iter().map(|stream| stream.borrow().pending()).sum()
    }
}

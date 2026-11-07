//! The instance arena: generational ids, class, name, parent, ordered children, properties by
//! interned name, attributes, tags, flags and change versions. Pure data and tree queries; the
//! script-visible semantics (signals, hooks, errors) live in `engine::instance`.

use std::rc::Rc;

use rustc_hash::FxHashMap;

use super::{
    reflection::{ClassId, K, NameId, Reflection},
    signal::SignalId,
    value::{Str, Value},
};

/// A generational instance handle. `index` addresses the arena slot; `generation` guards a
/// reused slot: `lua::raw::collect` frees unreachable parentless instances (their slot's
/// generation is bumped and the slot reused), so `Dom::get` of a stale id is `None`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct InstanceId {
    pub index: u32,
    pub generation: u32,
}

pub mod flags {
    pub const DESTROYED: u16 = 1;
    pub const PARENT_LOCKED: u16 = 1 << 1;
    /// Made by a script (Instance.new): never stands for a Studio-only instance of the place
    pub const SCRIPT_CREATED: u16 = 1 << 2;
    /// A Studio-only placeholder from the place's hierarchy snapshot
    pub const STUDIO_ONLY: u16 = 1 << 3;
    /// Studio-only children were added (Hooks.Materialize ran)
    pub const MATERIALIZED: u16 = 1 << 4;
    /// A Rojo JSON module: `require` returns the decoded file
    pub const JSON_MODULE: u16 = 1 << 5;
    /// An instance reference was stored among its properties at some point (set by every write
    /// path, never cleared): the instance collector looks for references only in these
    pub const HOLDS_REFS: u16 = 1 << 6;
    /// Made from a model plan imported lazily (project::LazyImport): its subtree is made as it is
    /// looked at
    pub const LAZY_REGION: u16 = 1 << 7;
    /// Its plan children are not made yet (they are made before anything lists or finds them)
    pub const LAZY_PENDING: u16 = 1 << 8;
    /// A lazily imported plan's roots are below it (a bundle folder or one of its ancestors)
    pub const LAZY_ABOVE: u16 = 1 << 9;
    /// Stands where a manifest bundle's root goes and is not that root yet: it becomes it (class,
    /// properties, attributes, tags) before a script finds, tests, touches or looks inside it
    /// (project::model_assets::reveal)
    pub const BUNDLE_ROOT: u16 = 1 << 10;
}

/// Signals created for an instance (events by name, property / attribute change signals).
#[derive(Default, Debug)]
pub struct InstanceSignals {
    pub events: Vec<(NameId, SignalId)>,
    pub properties: Vec<(NameId, SignalId)>,
    pub attributes: Vec<(Str, SignalId)>,
}

impl InstanceSignals {
    pub fn event(&self, name: NameId) -> Option<SignalId> {
        self.events.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }

    pub fn property(&self, name: NameId) -> Option<SignalId> {
        self.properties.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }

    pub fn attribute(&self, name: &[u8]) -> Option<SignalId> {
        self.attributes.iter().find(|(n, _)| n.as_bytes() == name).map(|(_, s)| *s)
    }

    pub fn all(&self) -> impl Iterator<Item = SignalId> + '_ {
        self.events
            .iter()
            .map(|(_, s)| *s)
            .chain(self.properties.iter().map(|(_, s)| *s))
            .chain(self.attributes.iter().map(|(_, s)| *s))
    }
}

/// Where a mounted instance sits in the place's Studio hierarchy snapshot (project.luau
/// StudioNodeOf: cached per instance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StudioNode {
    #[default]
    Unknown,
    None,
    Node(u32),
}

#[derive(Debug)]
pub struct Instance {
    pub class: ClassId,
    /// Creation serial (the Luau runtime's State.Id): the exported node id
    pub serial: u32,
    /// Effective name (RawGet(Name)): the stored Name, else the class's recorded default
    pub name: Str,
    pub parent: Option<InstanceId>,
    pub children: Vec<InstanceId>,
    /// Explicitly stored properties, sorted by name; `Value::Nil` is a stored nil
    pub props: Props,
    /// Attributes in insertion order (AttributeOrder)
    pub attributes: Attributes,
    /// Tags in the order they were added; a tag given other than through `instance::add_tag` is
    /// recorded with `services::note_tag`
    pub tags: ThinVec<Str>,
    pub flags: u16,
    /// Where a script's or module's source comes from (none for every other instance: boxed, so an
    /// instance holds one pointer for it, not 48 bytes)
    pub script: Option<Box<ScriptSource>>,
    pub studio_node: StudioNode,
    pub signals: Option<Box<InstanceSignals>>,
    /// Callback members assigned by scripts (OnInvoke, OnClientInvoke, ...)
    pub callbacks: ThinVec<(NameId, mlua::Value)>,
    /// Properties a model file held that the engine does not model (a MeshPart's InitialSize and
    /// physics data, ...), kept as read so the instance can be written back out (engine::record)
    pub unmodeled: Option<crate::project::Unmodeled>,
    /// Bumped on every own property / attribute / tag change
    pub version: u32,
    /// Bumped when the child list changes
    pub child_version: u32,
}

/// A script's source, the file it is read from and the stand-in package it comes from ([`Instance::script`]).
#[derive(Clone, Debug, Default)]
pub struct ScriptSource {
    /// Script source (ModuleScript / Script) when held in memory
    pub source: Option<Rc<[u8]>>,
    /// File the source is read from (project scripts, JSON modules)
    pub source_path: Option<Rc<str>>,
    /// Offline stand-in package file this module's source comes from
    pub stand_in: Option<Rc<str>>,
}

impl Instance {
    /// Its source when held in memory.
    #[inline]
    pub fn source(&self) -> Option<&Rc<[u8]>> {
        self.script.as_ref()?.source.as_ref()
    }

    /// The file its source is read from.
    #[inline]
    pub fn source_path(&self) -> Option<&Rc<str>> {
        self.script.as_ref()?.source_path.as_ref()
    }

    /// The stand-in package file its source comes from.
    #[inline]
    pub fn stand_in(&self) -> Option<&Rc<str>> {
        self.script.as_ref()?.stand_in.as_ref()
    }

    /// Its [`ScriptSource`], made when it has none, to change.
    pub fn script_mut(&mut self) -> &mut ScriptSource {
        self.script.get_or_insert_with(Default::default)
    }

    pub fn set_source(&mut self, source: Option<Rc<[u8]>>) {
        if source.is_some() || self.script.is_some() {
            self.script_mut().source = source;
        }
    }

    pub fn set_source_path(&mut self, path: Option<Rc<str>>) {
        if path.is_some() || self.script.is_some() {
            self.script_mut().source_path = path;
        }
    }

    pub fn set_stand_in(&mut self, stand_in: Option<Rc<str>>) {
        if stand_in.is_some() || self.script.is_some() {
            self.script_mut().stand_in = stand_in;
        }
    }

    #[inline]
    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }

    #[inline]
    pub fn set_flag(&mut self, flag: u16, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }

    /// The explicitly stored value of a property (`Some(Value::Nil)` for a stored nil).
    #[inline]
    pub fn stored(&self, name: NameId) -> Option<&Value> {
        self.props.get(name)
    }

    /// Stores a property value; returns the previously stored one.
    pub fn store(&mut self, name: NameId, value: Value) -> Option<Value> {
        if value.references_instance() {
            self.flags |= flags::HOLDS_REFS;
        }
        let props = self.props.own_mut();
        match props.binary_search_by_key(&name, |(n, _)| *n) {
            Ok(index) => Some(std::mem::replace(&mut props[index].1, value)),
            Err(index) => {
                props.insert(index, (name, value));
                None
            }
        }
    }

    pub fn unstore(&mut self, name: NameId) -> Option<Value> {
        self.props.get(name)?;
        let props = self.props.own_mut();
        match props.binary_search_by_key(&name, |(n, _)| *n) {
            Ok(index) => Some(props.remove(index).1),
            Err(_) => None,
        }
    }

    pub fn attribute(&self, name: &[u8]) -> Option<&Value> {
        self.attributes.get(name)
    }

    pub fn callback(&self, name: NameId) -> Option<&mlua::Value> {
        self.callbacks.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }
}

/// An instance's explicitly stored properties, sorted by name: its own, or, for an instance made of a
/// lazily imported model plan's node and its unchanged copies, the node's, read through the import
/// ([`crate::project::PlanProps`]) until the first write copies them (a bundle's made instances held a
/// vector of ~15-25 values each: ~0.44 GB for the 581k instances the mesh effects scene walks).
pub enum Props {
    Own(Vec<(NameId, Value)>),
    Plan(Rc<crate::project::PlanProps>, u32),
}

impl Default for Props {
    fn default() -> Self {
        Props::Own(Vec::new())
    }
}

impl From<Vec<(NameId, Value)>> for Props {
    fn from(props: Vec<(NameId, Value)>) -> Self {
        Props::Own(props)
    }
}

impl std::fmt::Debug for Props {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Props {
    /// The stored value of property `name`.
    #[inline]
    pub fn get(&self, name: NameId) -> Option<&Value> {
        match self {
            Props::Own(props) => props.binary_search_by_key(&name, |(n, _)| *n).ok().map(|index| &props[index].1),
            Props::Plan(table, node) => table.get(*node, name),
        }
    }

    /// (name, value) of each, sorted by name.
    pub fn iter(&self) -> PropsIter<'_> {
        match self {
            Props::Own(props) => PropsIter::Own(props.iter()),
            Props::Plan(table, node) => PropsIter::Plan(table.iter(*node)),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Props::Own(props) => props.len(),
            Props::Plan(..) => self.iter().count(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The values this instance holds a vector of (none while they are read through a plan).
    pub fn capacity(&self) -> usize {
        match self {
            Props::Own(props) => props.capacity(),
            Props::Plan(..) => 0,
        }
    }

    /// The properties as a vector of its own, to change (a plan node's are copied first).
    pub fn own_mut(&mut self) -> &mut Vec<(NameId, Value)> {
        if let Props::Plan(..) = self {
            let own: Vec<(NameId, Value)> = self.iter().map(|(name, value)| (name, value.clone())).collect();
            *self = Props::Own(own);
        }
        match self {
            Props::Own(props) => props,
            Props::Plan(..) => unreachable!(),
        }
    }

    /// What a copy of the instance stores: every property but `skip` (a plan node's are shared on).
    pub fn copied_without(&self, skip: NameId) -> Props {
        match self {
            Props::Plan(table, node) if self.get(skip).is_none() => Props::Plan(table.clone(), *node),
            _ => Props::Own(self.iter().filter(|(name, _)| *name != skip).map(|(name, value)| (name, value.clone())).collect()),
        }
    }
}

impl<'a> IntoIterator for &'a Props {
    type Item = (NameId, &'a Value);
    type IntoIter = PropsIter<'a>;

    fn into_iter(self) -> PropsIter<'a> {
        self.iter()
    }
}

/// [`Props::iter`]
pub enum PropsIter<'a> {
    Own(std::slice::Iter<'a, (NameId, Value)>),
    Plan(crate::project::PlanPropsIter<'a>),
}

impl<'a> Iterator for PropsIter<'a> {
    type Item = (NameId, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            PropsIter::Own(iter) => iter.next().map(|(name, value)| (*name, value)),
            PropsIter::Plan(iter) => iter.next(),
        }
    }
}

/// A vector that is one pointer while empty (most instances have no tags or callbacks): 8 bytes in
/// the instance instead of 24, and a block of its own only once it holds something.
pub struct ThinVec<T>(Option<Box<Vec<T>>>);

impl<T> ThinVec<T> {
    /// The vector, made when there is none, to change.
    pub fn vec_mut(&mut self) -> &mut Vec<T> {
        self.0.get_or_insert_with(Default::default)
    }

    pub fn push(&mut self, item: T) {
        self.vec_mut().push(item);
    }

    pub fn remove(&mut self, index: usize) -> T {
        let item = self.vec_mut().remove(index);
        if self.0.as_ref().is_some_and(|items| items.is_empty()) {
            self.0 = None;
        }
        item
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        if let Some(items) = &mut self.0 {
            items.retain(keep);
            if items.is_empty() {
                self.0 = None;
            }
        }
    }
}

impl<T> Default for ThinVec<T> {
    fn default() -> Self {
        ThinVec(None)
    }
}

impl<T> std::ops::Deref for ThinVec<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }
}

impl<T> std::ops::DerefMut for ThinVec<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.0.as_deref_mut().map_or(&mut [], Vec::as_mut_slice)
    }
}

impl<T> From<Vec<T>> for ThinVec<T> {
    fn from(items: Vec<T>) -> Self {
        ThinVec((!items.is_empty()).then(|| Box::new(items)))
    }
}

impl<T: Clone> Clone for ThinVec<T> {
    fn clone(&self) -> Self {
        ThinVec(self.0.clone())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ThinVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T> IntoIterator for ThinVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.map(|items| *items).unwrap_or_default().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a ThinVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An instance's attributes in insertion order: its own, or, for an instance made of a lazily imported
/// plan node without instance attributes and its unchanged copies, the node's, made from the plan
/// when first read (`project::PlanProps::attributes`) and copied by the first change (a bundle's ~1.1M
/// attributes in the mesh effects scene were converted for every made instance, read or not).
pub enum Attributes {
    Own(ThinVec<(Str, Value)>),
    Plan(Rc<crate::project::PlanProps>, u32),
}

impl Default for Attributes {
    fn default() -> Self {
        Attributes::Own(ThinVec::default())
    }
}

impl From<Vec<(Str, Value)>> for Attributes {
    fn from(items: Vec<(Str, Value)>) -> Self {
        Attributes::Own(items.into())
    }
}

impl std::fmt::Debug for Attributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Attributes {
    /// The (name, value) pairs in insertion order.
    #[inline]
    pub fn as_slice(&self) -> &[(Str, Value)] {
        match self {
            Attributes::Own(items) => items,
            Attributes::Plan(table, node) => table.attributes(*node),
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (Str, Value)> {
        self.as_slice().iter()
    }

    pub fn get(&self, name: &[u8]) -> Option<&Value> {
        self.iter().find(|(n, _)| n.as_bytes() == name).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        match self {
            Attributes::Own(items) => items.len(),
            Attributes::Plan(table, node) => table.attribute_count(*node),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The attributes as a vector of its own, to change (a plan node's are copied first).
    pub fn own_mut(&mut self) -> &mut ThinVec<(Str, Value)> {
        if let Attributes::Plan(..) = self {
            *self = Attributes::Own(self.as_slice().to_vec().into());
        }
        match self {
            Attributes::Own(items) => items,
            Attributes::Plan(..) => unreachable!(),
        }
    }

    /// What a copy of the instance holds (a plan node's are shared on).
    pub fn copied(&self) -> Attributes {
        match self {
            Attributes::Own(items) => Attributes::Own(items.clone()),
            Attributes::Plan(table, node) => Attributes::Plan(table.clone(), *node),
        }
    }
}

impl<'a> IntoIterator for &'a Attributes {
    type Item = &'a (Str, Value);
    type IntoIter = std::slice::Iter<'a, (Str, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

struct Slot {
    generation: u32,
    instance: Option<Instance>,
}

/// Per-class default values (Reflection.Default), computed when a class is first instantiated.
#[derive(Default)]
pub struct Defaults {
    per_class: Vec<Option<Box<FxHashMap<NameId, Value>>>>,
}

impl Defaults {
    /// Reflection.Default(Class, Name) for every name the class records or declares.
    pub fn ensure(&mut self, reflection: &Reflection, class: ClassId) {
        if self.per_class.len() < reflection.class_count() {
            self.per_class.resize_with(reflection.class_count(), || None);
        }
        if self.per_class[class as usize].is_some() {
            return;
        }
        let mut names: Vec<NameId> = Vec::new();
        let mut current = Some(class);
        while let Some(id) = current {
            let info = reflection.class(id);
            if let Some(raw) = &info.raw_defaults {
                names.extend(raw.keys().copied());
            }
            current = info.superclass;
        }
        names.extend(reflection.class(class).properties.keys().copied());
        names.sort_unstable();
        names.dedup();
        let mut table = FxHashMap::default();
        for name in names {
            if let Some(value) = compute_default(reflection, class, name) {
                table.insert(name, value);
            }
        }
        self.per_class[class as usize] = Some(Box::new(table));
    }

    #[inline]
    pub fn get(&self, class: ClassId, name: NameId) -> Option<&Value> {
        self.per_class.get(class as usize)?.as_ref()?.get(&name)
    }
}

fn compute_default(reflection: &Reflection, class: ClassId, name: NameId) -> Option<Value> {
    let info = reflection.class(class);
    let mut value = info
        .raw_defaults
        .as_ref()
        .and_then(|raw| raw.get(&name))
        .and_then(|raw| super::value::decode_default(reflection, raw));
    if value.is_none() {
        let mut ancestor = info.superclass;
        while let (Some(id), None) = (ancestor, &value) {
            let ancestor_info = reflection.class(id);
            if ancestor_info.raw_defaults.as_ref().is_some_and(|raw| raw.contains_key(&name)) {
                value = compute_default(reflection, id, name);
            }
            ancestor = ancestor_info.superclass;
        }
    }
    // (a class whose defaults record no Name is named after itself, not the empty string a string property's zero is:
    // Player, the services and the abstract classes)
    if value.is_none() && name == K::Name as NameId {
        return Some(Value::String(Str::from(&*info.name)));
    }
    if value.is_none() {
        if let Some(property) = reflection.class_property(class, name) {
            value = super::value::zero_value(
                reflection,
                property.script_type,
                &property.script_type_name,
                property.enum_type,
            );
        }
    }
    value
}

/// Unpinned instances remembered between collections (an engine without the Instance API's collector never drains
/// them)
const UNPINNED_CAP: usize = 1 << 16;

#[derive(Default)]
pub struct Dom {
    slots: Vec<Slot>,
    /// Freed slots, reused by `create` (their generation was bumped)
    free_slots: Vec<u32>,
    /// Engine-held references that keep an instance (and its subtree root) from being collected
    pins: FxHashMap<InstanceId, u32>,
    /// Instances whose last pin was released since `take_unpinned` (lua::raw::collect looks at their trees again:
    /// nothing else signals that a tween ended on an instance already outside the DataModel)
    unpinned: Vec<InstanceId>,
    next_serial: u32,
    pub defaults: Defaults,
    /// Bumped on every parent change anywhere (structure version)
    pub structure_version: u64,
    /// Bumped on every property, attribute or tag change anywhere (what the world's query cache
    /// checks, with the structure version)
    pub write_version: u64,
    /// Models whose rig animation caches (animation::pose), with a version a change inside one bumps: a child
    /// attached or detached below it, or a Name / Part0 / Part1 / Scale written on it or below it
    rig_watch: FxHashMap<InstanceId, u32>,
    rig_props: [NameId; 4],
    /// Live slots of the classes someone asked to enumerate cheaply (`instances_of_class`), in
    /// slot order
    tracked: FxHashMap<ClassId, std::collections::BTreeSet<u32>>,
}

impl Dom {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an unparented instance of `class` with the next serial (InstanceModule.Create).
    pub fn create(&mut self, reflection: &Reflection, class: ClassId) -> InstanceId {
        self.next_serial += 1;
        self.create_with_serial(reflection, class, self.next_serial)
    }

    /// Reserves `count` serials for instances made later ([`Dom::create_with_serial`]), as if they
    /// were made now; returns the first.
    pub fn reserve_serials(&mut self, count: u32) -> u32 {
        let first = self.next_serial + 1;
        self.next_serial += count;
        first
    }

    /// Creates an unparented instance of `class` with a serial [`Dom::reserve_serials`] reserved.
    pub fn create_with_serial(&mut self, reflection: &Reflection, class: ClassId, serial: u32) -> InstanceId {
        self.defaults.ensure(reflection, class);
        let name = match self.defaults.get(class, K::Name as NameId) {
            Some(Value::String(name)) => name.clone(),
            _ => Str::from(&*reflection.class(class).name),
        };
        let instance = Instance {
            class,
            serial,
            name,
            parent: None,
            children: Vec::new(),
            props: Props::default(),
            attributes: Attributes::default(),
            tags: ThinVec::default(),
            flags: 0,
            script: None,
            studio_node: StudioNode::Unknown,
            signals: None,
            callbacks: ThinVec::default(),
            unmodeled: None,
            version: 0,
            child_version: 0,
        };
        if let Some(index) = self.free_slots.pop() {
            let slot = &mut self.slots[index as usize];
            slot.instance = Some(instance);
            if let Some(set) = self.tracked.get_mut(&class) {
                set.insert(index);
            }
            return InstanceId { index, generation: slot.generation };
        }
        let index = self.slots.len() as u32;
        self.slots.push(Slot { generation: 0, instance: Some(instance) });
        if let Some(set) = self.tracked.get_mut(&class) {
            set.insert(index);
        }
        InstanceId { index, generation: 0 }
    }

    /// The live instances of exactly `class`, in slot order (the order of a walk over every
    /// slot). The first call for a class scans the arena; later ones read the index `create` /
    /// `free` keep for it.
    pub fn instances_of_class(&mut self, class: ClassId) -> Vec<InstanceId> {
        self.track_class(class);
        self.tracked[&class].iter().filter_map(|&index| self.id_at_slot(index)).collect()
    }

    /// How many live instances of exactly `class` there are (the index [`Dom::instances_of_class`] keeps).
    pub fn count_of_class(&mut self, class: ClassId) -> usize {
        self.track_class(class);
        self.tracked[&class].len()
    }

    /// Gives a live instance another class in place (a bundle's placeholder becoming the bundle's root:
    /// project::plan), keeping its id, serial, stored values and children: the class index and the
    /// class's defaults follow, and the change counts as one to its properties and the tree.
    pub fn set_class(&mut self, reflection: &Reflection, id: InstanceId, class: ClassId) {
        let old = self.at(id).class;
        if old == class {
            return;
        }
        self.defaults.ensure(reflection, class);
        if let Some(set) = self.tracked.get_mut(&old) {
            set.remove(&id.index);
        }
        if let Some(set) = self.tracked.get_mut(&class) {
            set.insert(id.index);
        }
        let default_name = match self.defaults.get(class, K::Name as NameId) {
            Some(Value::String(name)) => name.clone(),
            _ => Str::from(&*reflection.class(class).name),
        };
        let instance = self.at_mut(id);
        instance.class = class;
        if instance.stored(K::Name as NameId).is_none() {
            instance.name = default_name;
        }
        instance.version = instance.version.wrapping_add(1);
        self.structure_version += 1;
        self.write_version += 1;
    }

    fn track_class(&mut self, class: ClassId) {
        if !self.tracked.contains_key(&class) {
            let set = (0..self.slots.len() as u32)
                .filter(|&index| self.slots[index as usize].instance.as_ref().is_some_and(|i| i.class == class))
                .collect();
            self.tracked.insert(class, set);
        }
    }

    /// Frees an instance's slot (garbage collection: the caller made sure nothing references
    /// it). The slot's generation is bumped, so the id and every copy of it become stale.
    pub fn free(&mut self, id: InstanceId) -> Option<Instance> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let instance = slot.instance.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        if let Some(set) = self.tracked.get_mut(&instance.class) {
            set.remove(&id.index);
        }
        self.free_slots.push(id.index);
        self.pins.remove(&id);
        self.rig_watch.remove(&id);
        Some(instance)
    }

    /// Keeps `id` alive while the engine holds it outside the tree (a playing tween, a tween's
    /// target, ...). Pins count: each `pin` needs one `unpin`.
    pub fn pin(&mut self, id: InstanceId) {
        if self.get(id).is_some() {
            *self.pins.entry(id).or_default() += 1;
        }
    }

    pub fn unpin(&mut self, id: InstanceId) {
        if let Some(count) = self.pins.get_mut(&id) {
            *count -= 1;
            if *count == 0 {
                self.pins.remove(&id);
                if self.unpinned.len() < UNPINNED_CAP {
                    self.unpinned.push(id);
                }
            }
        }
    }

    /// The instances unpinned since the last call.
    pub fn take_unpinned(&mut self) -> Vec<InstanceId> {
        std::mem::take(&mut self.unpinned)
    }

    pub fn unpinned_len(&self) -> usize {
        self.unpinned.len()
    }

    #[inline]
    pub fn is_pinned(&self, id: InstanceId) -> bool {
        self.pins.contains_key(&id)
    }

    /// (property values stored, their vectors' capacity, child ids' capacity, attributes, instances
    /// with signals) over every live instance (diagnostics)
    pub fn storage(&self) -> (usize, usize, usize, usize, usize) {
        let mut total = (0, 0, 0, 0, 0);
        for instance in self.slots.iter().filter_map(|slot| slot.instance.as_ref()) {
            total.0 += instance.props.len();
            total.1 += instance.props.capacity();
            total.2 += instance.children.capacity();
            total.3 += instance.attributes.len();
            total.4 += instance.signals.is_some() as usize;
        }
        total
    }

    /// Number of live instances.
    pub fn live_count(&self) -> usize {
        self.slots.len() - self.free_slots.len()
    }

    /// Number of instances ever created (the last serial).
    pub fn created(&self) -> u32 {
        self.next_serial
    }

    #[inline]
    pub fn get(&self, id: InstanceId) -> Option<&Instance> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.instance.as_ref()
    }

    #[inline]
    pub fn get_mut(&mut self, id: InstanceId) -> Option<&mut Instance> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.instance.as_mut()
    }

    /// The instance behind a live id (panics on a stale id: an engine bug).
    #[inline]
    pub fn at(&self, id: InstanceId) -> &Instance {
        self.get(id).expect("stale instance id")
    }

    #[inline]
    pub fn at_mut(&mut self, id: InstanceId) -> &mut Instance {
        self.get_mut(id).expect("stale instance id")
    }

    /// The id stored in slot `index` (for walks keyed by slot).
    pub fn id_at_slot(&self, index: u32) -> Option<InstanceId> {
        let slot = self.slots.get(index as usize)?;
        slot.instance.as_ref()?;
        Some(InstanceId { index, generation: slot.generation })
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// RawGet: the stored value, else the class default (`None`: nil).
    #[inline]
    pub fn raw_get(&self, id: InstanceId, name: NameId) -> Option<&Value> {
        let instance = self.at(id);
        match instance.stored(name) {
            Some(Value::Nil) => None,
            Some(value) => Some(value),
            None => self.defaults.get(instance.class, name),
        }
    }

    /// RawGet as a number (0 for nil or other types).
    #[inline]
    pub fn number(&self, id: InstanceId, name: NameId) -> Option<f64> {
        self.raw_get(id, name).and_then(Value::as_number)
    }

    #[inline]
    pub fn boolean(&self, id: InstanceId, name: NameId) -> Option<bool> {
        self.raw_get(id, name).and_then(Value::as_bool)
    }

    #[inline]
    pub fn class_of(&self, id: InstanceId) -> ClassId {
        self.at(id).class
    }

    #[inline]
    pub fn parent_of(&self, id: InstanceId) -> Option<InstanceId> {
        self.at(id).parent
    }

    #[inline]
    pub fn children_of(&self, id: InstanceId) -> &[InstanceId] {
        &self.at(id).children
    }

    /// Starts watching `model` for the rig cache (the properties that change a rig are `props`); its version now.
    pub fn watch_rig(&mut self, model: InstanceId, props: [NameId; 4]) -> u32 {
        self.rig_props = props;
        *self.rig_watch.entry(model).or_insert(0)
    }

    /// `from` and its ancestors that are watched rigs: something below them changed.
    fn touch_rigs(&mut self, from: InstanceId) {
        if self.rig_watch.is_empty() {
            return;
        }
        let mut current = Some(from);
        while let Some(id) = current {
            if let Some(version) = self.rig_watch.get_mut(&id) {
                *version = version.wrapping_add(1);
            }
            current = self.get(id).and_then(|instance| instance.parent);
        }
    }

    /// A property of `id` was written: a watched rig changes when it is one of those that make a rig.
    pub fn touch_rigs_for_property(&mut self, id: InstanceId, name: NameId) {
        if !self.rig_watch.is_empty() && self.rig_props.contains(&name) {
            self.touch_rigs(id);
        }
    }

    /// Appends `child` to `parent`'s children and sets its parent (no events: RawParent).
    pub fn attach(&mut self, child: InstanceId, parent: InstanceId) {
        self.touch_rigs(parent);
        self.at_mut(child).parent = Some(parent);
        let parent = self.at_mut(parent);
        parent.children.push(child);
        parent.child_version = parent.child_version.wrapping_add(1);
        self.structure_version += 1;
    }

    /// Removes `child` from its parent's children and clears its parent (no events).
    pub fn detach(&mut self, child: InstanceId) {
        let Some(parent) = self.at(child).parent else {
            return;
        };
        self.touch_rigs(parent);
        let parent_instance = self.at_mut(parent);
        if let Some(index) = parent_instance.children.iter().position(|c| *c == child) {
            parent_instance.children.remove(index);
        }
        parent_instance.child_version = parent_instance.child_version.wrapping_add(1);
        self.at_mut(child).parent = None;
        self.structure_version += 1;
    }

    /// Replaces `old` by `new` in the same child slot of `parent` (project EnsureChild).
    pub fn replace_child(&mut self, parent: InstanceId, old: InstanceId, new: InstanceId) {
        self.touch_rigs(parent);
        let parent_instance = self.at_mut(parent);
        if let Some(index) = parent_instance.children.iter().position(|c| *c == old) {
            parent_instance.children[index] = new;
        }
        parent_instance.child_version = parent_instance.child_version.wrapping_add(1);
        self.at_mut(old).parent = None;
        self.at_mut(new).parent = Some(parent);
        self.structure_version += 1;
    }

    /// Whether `ancestor` is a (strict) ancestor of `id`.
    pub fn is_descendant_of(&self, id: InstanceId, ancestor: InstanceId) -> bool {
        let mut current = self.at(id).parent;
        while let Some(node) = current {
            if node == ancestor {
                return true;
            }
            current = self.at(node).parent;
        }
        false
    }

    /// Descendants in preorder (GetDescendants' order).
    pub fn descendants(&self, id: InstanceId) -> Vec<InstanceId> {
        let mut result = Vec::new();
        self.collect_descendants(id, &mut result);
        result
    }

    fn collect_descendants(&self, id: InstanceId, result: &mut Vec<InstanceId>) {
        for &child in &self.at(id).children {
            result.push(child);
            self.collect_descendants(child, result);
        }
    }

    /// The descendants of `id` for which `keep` holds, in [`Dom::descendants`]' order, without
    /// listing the others.
    pub fn descendants_where(&self, id: InstanceId, keep: impl Fn(&Instance) -> bool) -> Vec<InstanceId> {
        let mut result = Vec::new();
        let mut stack: Vec<InstanceId> = self.at(id).children.iter().rev().copied().collect();
        while let Some(node) = stack.pop() {
            let instance = self.at(node);
            if keep(instance) {
                result.push(node);
            }
            stack.extend(instance.children.iter().rev());
        }
        result
    }

    /// Root plus descendants breadth-first (the order FireDescendantEvents fires in).
    pub fn subtree_breadth_first(&self, root: InstanceId) -> Vec<InstanceId> {
        let mut list = vec![root];
        let mut index = 0;
        while index < list.len() {
            let node = list[index];
            list.extend_from_slice(&self.at(node).children);
            index += 1;
        }
        list
    }

    /// The subtree with every instance after its children (children in order): FireAncestryChanged's order.
    pub fn subtree_post_order(&self, root: InstanceId) -> Vec<InstanceId> {
        fn visit(dom: &Dom, node: InstanceId, result: &mut Vec<InstanceId>) {
            for &child in &dom.at(node).children {
                visit(dom, child, result);
            }
            result.push(node);
        }
        let mut result = Vec::new();
        visit(self, root, &mut result);
        result
    }

    /// Root plus descendants in an old FireAncestryChanged's order (a stack: last child first).
    pub fn subtree_stack_order(&self, root: InstanceId) -> Vec<InstanceId> {
        let mut result = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            result.push(node);
            stack.extend_from_slice(&self.at(node).children);
        }
        result
    }

    /// GetFullName: names from below the DataModel down to `id`, dot separated.
    pub fn full_name(&self, reflection: &Reflection, id: InstanceId) -> String {
        let data_model = reflection.class_id("DataModel");
        // the DataModel's own full name is its Name (`game:GetFullName() == game.Name` in a playing client, Studio 0.744:
        // tools/parity/probes/instances.luau); below it, paths start at the service
        if Some(self.at(id).class) == data_model {
            return self.at(id).name.to_str_lossy().into_owned();
        }
        let mut parts: Vec<&Str> = Vec::new();
        let mut current = Some(id);
        while let Some(node) = current {
            let instance = self.at(node);
            if Some(instance.class) == data_model {
                break;
            }
            parts.push(&instance.name);
            current = instance.parent;
        }
        let mut result = String::new();
        for (index, part) in parts.iter().rev().enumerate() {
            if index > 0 {
                result.push('.');
            }
            result.push_str(&part.to_str_lossy());
        }
        result
    }

    /// First child named `name`.
    pub fn find_child(&self, parent: InstanceId, name: &[u8]) -> Option<InstanceId> {
        self.at(parent)
            .children
            .iter()
            .copied()
            .find(|&child| self.at(child).name.as_bytes() == name)
    }

    pub fn find_child_of_class(&self, parent: InstanceId, class: ClassId) -> Option<InstanceId> {
        self.at(parent).children.iter().copied().find(|&child| self.at(child).class == class)
    }

    /// Sets the effective name after the Name property changed.
    pub fn refresh_name(&mut self, reflection: &Reflection, id: InstanceId) {
        let instance = self.at(id);
        let name = match instance.stored(K::Name as NameId) {
            Some(Value::String(name)) => name.clone(),
            Some(Value::Nil) => Str::default(),
            Some(_) | None => match self.defaults.get(instance.class, K::Name as NameId) {
                Some(Value::String(name)) => name.clone(),
                _ => Str::from(&*reflection.class(instance.class).name),
            },
        };
        self.at_mut(id).name = name;
    }

    /// Stores `value` as RawSet does with one property lookup: (whether RawGet's value changed,
    /// the previous RawGet value when it did (nil when it was unset without a default); nil
    /// otherwise).
    pub fn exchange(&mut self, id: InstanceId, name: NameId, value: Value) -> (bool, Value) {
        let slot = &mut self.slots[id.index as usize];
        assert!(slot.generation == id.generation, "stale instance id");
        let instance = slot.instance.as_mut().expect("stale instance id");
        if value.references_instance() {
            instance.flags |= flags::HOLDS_REFS;
        }
        let props = instance.props.own_mut();
        match props.binary_search_by_key(&name, |(n, _)| *n) {
            Ok(index) => {
                let stored = &mut props[index].1;
                // (a stored nil reads as nil: RawGet does not fall back to the default)
                let same = match &*stored {
                    Value::Nil => value.is_nil(),
                    old => old.same(&value),
                };
                let old = std::mem::replace(stored, value);
                (!same, if same { Value::Nil } else { old })
            }
            Err(index) => {
                let default = self.defaults.get(instance.class, name);
                let same = match default {
                    Some(old) => old.same(&value),
                    None => value.is_nil(),
                };
                let old = if same { Value::Nil } else { default.cloned().unwrap_or(Value::Nil) };
                props.insert(index, (name, value));
                (!same, old)
            }
        }
    }

    pub fn bump_version(&mut self, id: InstanceId) {
        let instance = self.at_mut(id);
        instance.version = instance.version.wrapping_add(1);
        self.write_version = self.write_version.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every instance holds the rarely set parts (a script's source, tags, callbacks) as one pointer
    /// each, and its properties and attributes in 24 bytes: 168 bytes an instance (it was 248).
    #[test]
    fn an_instance_is_168_bytes() {
        assert_eq!(std::mem::size_of::<Instance>(), 168);
    }

    #[test]
    fn tree_queries_and_defaults() {
        let reflection = Reflection::shared().unwrap();
        let mut dom = Dom::new();
        let frame_class = reflection.class_id("Frame").unwrap();
        let root = dom.create(&reflection, reflection.class_id("Folder").unwrap());
        let a = dom.create(&reflection, frame_class);
        let b = dom.create(&reflection, frame_class);
        let c = dom.create(&reflection, frame_class);
        assert_eq!(dom.at(a).serial, 2);
        dom.attach(a, root);
        dom.attach(b, root);
        dom.attach(c, a);
        assert_eq!(dom.descendants(root), vec![a, c, b]);
        assert_eq!(dom.subtree_breadth_first(root), vec![root, a, b, c]);
        assert_eq!(dom.subtree_stack_order(root), vec![root, b, a, c]);
        assert!(dom.is_descendant_of(c, root) && !dom.is_descendant_of(root, c));
        assert_eq!(dom.full_name(&reflection, c), "Folder.Frame.Frame");
        dom.detach(a);
        assert_eq!(dom.children_of(root), &[b]);
        assert_eq!(dom.parent_of(a), None);
        // Defaults: the dump's Frame defaults
        let visible = dom.raw_get(a, K::Visible as NameId).unwrap();
        assert_eq!(visible.as_bool(), Some(true));
        assert_eq!(dom.number(a, K::ZIndex as NameId), Some(1.0));
        dom.at_mut(a).store(K::Visible as NameId, Value::Nil);
        assert!(dom.raw_get(a, K::Visible as NameId).is_none());
        // BindableFunction's recorded Name default differs from its class name
        let function = dom.create(&reflection, reflection.class_id("BindableFunction").unwrap());
        assert_eq!(dom.at(function).name.as_bytes(), b"Function");
    }
}

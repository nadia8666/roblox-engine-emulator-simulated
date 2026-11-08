//! Script-visible instance semantics that never run Luau code (port of the state side of
//! `runtime/engine/instance.luau`): creation, parenting with its event order, raw property access
//! with change notification, clone, attributes, tags and signal firing. Operations that run
//! scripts (Destroy, WaitForChild) are in `engine::lua`.

use crate::cost::SignalKind;
use super::{
    Engine, Hooks, InstanceId, ScriptError, ScriptResult, State,
    dom::{InstanceSignals, flags},
    reflection::{ClassId, K, NameId, Reflection},
    scheduler::JobTarget,
    signal::{Handler, SignalId},
    value::{Str, Value},
};

/// What a key means on a class (instance.luau ResolveMember), cached per (class, key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Member {
    Property { property: Option<u32>, getter: bool, setter: bool },
    Method { implemented: bool },
    Event,
    Callback,
    None,
}

impl Engine {
    /// ResolveMember: property (reflection), behaviour method, reflected method, event, callback,
    /// or a getter-only behaviour property.
    pub fn resolve_member(&self, class: ClassId, name: NameId) -> Member {
        if let Some(member) = self.member_cache.borrow().get(&(class, name)) {
            crate::profiler::caches::hit(crate::profiler::caches::Cache::Resolve);
            return *member;
        }
        crate::profiler::caches::miss(crate::profiler::caches::Cache::Resolve);
        let reflection = &self.reflection;
        // (a deprecated lower-case property is the property it stands for: `brickColor` is BrickColor)
        if let Some(twin) = reflection.property_alias(class, name) {
            let member = self.resolve_member(class, twin);
            self.member_cache.borrow_mut().insert((class, name), member);
            return member;
        }
        let behaviors = self.behaviors.borrow();
        let info = reflection.class(class);
        let getter = behaviors.find_getter(reflection, class, name).is_some();
        let method = behaviors.find_method_or_alias(reflection, class, name).is_some();
        // a property scripts cannot read (NotScriptable: the dump permits nothing) is no member, as in
        // Roblox, unless a behaviour gives it a getter: `part.Origin` finds the part's child named
        // Origin, not PVInstance.Origin (an effect that emits from `Glow.Inner.Origin`, an Attachment)
        let scriptable = |property: u32| reflection.property(property).scriptable || getter;
        let member = if let Some(&property) = info.properties.get(&name).filter(|&&property| scriptable(property)) {
            Member::Property {
                property: Some(property),
                getter,
                setter: behaviors.find_setter(reflection, class, name).is_some(),
            }
        } else if method {
            Member::Method { implemented: true }
        } else if info.methods.contains(&name) {
            Member::Method { implemented: false }
        } else if info.events.contains(&name) {
            Member::Event
        } else if info.callbacks.contains(&name) {
            Member::Callback
        } else if getter {
            Member::Property {
                property: None,
                getter: true,
                setter: behaviors.find_setter(reflection, class, name).is_some(),
            }
        } else {
            Member::None
        };
        self.member_cache.borrow_mut().insert((class, name), member);
        member
    }

    /// The name a member of `class` keeps its value under: a deprecated lower-case property's is the property it stands for
    /// (`brickColor` -> BrickColor: one value, and Changed names BrickColor), any other member's its own.
    #[inline]
    pub fn member_name(&self, class: ClassId, name: NameId) -> NameId {
        self.reflection.property_alias(class, name).unwrap_or(name)
    }

    /// Drops cached member resolutions (after behaviours change).
    pub fn clear_member_cache(&self) {
        self.member_cache.borrow_mut().clear();
        // and the raw Instance API's dispatch entries built on it (lua/raw.rs)
        if let Some(api) = self.lua.try_borrow().ok().and_then(|refs| refs.raw.clone()) {
            api.clear_members();
        }
    }
}

/// InstanceModule.Create: a fresh unparented instance.
pub fn create(state: &mut State, reflection: &Reflection, class: ClassId) -> InstanceId {
    // Scheduler.NoteBuildWork (InstanceModule.Hooks.Created): a recorded clock's build placement
    state.sched.note_build_work();
    state.joints.note_created(class);
    state.dom.create(reflection, class)
}

/// Creates an instance with a stored Name (RawSet silent), as the engine builds services.
pub fn create_named(state: &mut State, reflection: &Reflection, class: ClassId, name: &str) -> InstanceId {
    state.sched.note_build_work();
    state.joints.note_created(class);
    let id = state.dom.create(reflection, class);
    raw_set_silent(state, reflection, id, K::Name as NameId, Value::String(Str::from(name)));
    id
}

/// [`create_named`] with a name the caller holds already.
pub fn create_named_str(state: &mut State, reflection: &Reflection, class: ClassId, name: Str) -> InstanceId {
    state.sched.note_build_work();
    state.joints.note_created(class);
    let id = state.dom.create(reflection, class);
    raw_set_silent(state, reflection, id, K::Name as NameId, Value::String(name));
    id
}

/// InstanceModule.New: `Instance.new(ClassName, Parent)`.
pub fn new_instance(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    class_name: &str,
    parent: Option<InstanceId>,
) -> ScriptResult<InstanceId> {
    let class = reflection
        .class_id(class_name)
        .filter(|&class| reflection.class(class).creatable)
        .ok_or_else(|| ScriptError::plain(format!("Unable to create an Instance of type \"{class_name}\"")))?;
    let id = create(state, reflection, class);
    state.dom.at_mut(id).set_flag(flags::SCRIPT_CREATED, true);
    if let Some(parent) = parent {
        set_parent(state, reflection, hooks, id, Some(parent))?;
    }
    Ok(id)
}

// Signals

/// The event signal `name` of an instance, created on first use (GetSignal).
pub fn event_signal(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId) -> SignalId {
    if let Some(signal) = state.dom.at(id).signals.as_ref().and_then(|s| s.event(name)) {
        return signal;
    }
    let declaring = reflection.declaring_class(state.dom.at(id).class, name);
    let label = format!("{}.{}", reflection.class(declaring).name, reflection.name(name));
    let signal = state.signals.create_labelled(Str::from(reflection.name(name)), Str::from(label.as_str()));
    state
        .dom
        .at_mut(id)
        .signals
        .get_or_insert_with(Box::default)
        .events
        .push((name, signal));
    signal
}

/// GetPropertyChangedSignal's signal for `name` (created on first use).
pub fn property_signal(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId) -> SignalId {
    if let Some(signal) = state.dom.at(id).signals.as_ref().and_then(|s| s.property(name)) {
        return signal;
    }
    // tostring reads "Signal SizeChanged" and diagnostics "Object.SizeChanged" (measured in a playing client, Studio 0.744)
    let changed = format!("{}Changed", reflection.name(name));
    let signal = state.signals.create_labelled(Str::from(changed.as_str()), Str::from(format!("Object.{changed}").as_str()));
    state
        .dom
        .at_mut(id)
        .signals
        .get_or_insert_with(Box::default)
        .properties
        .push((name, signal));
    signal
}

pub fn attribute_signal(state: &mut State, id: InstanceId, name: &Str) -> SignalId {
    if let Some(signal) = state.dom.at(id).signals.as_ref().and_then(|s| s.attribute(name.as_bytes())) {
        return signal;
    }
    let label = format!("Instance.{}AttributeChanged", name.to_str_lossy());
    let signal = state.signals.create_labelled(name.clone(), Str::from(label.as_str()));
    state
        .dom
        .at_mut(id)
        .signals
        .get_or_insert_with(Box::default)
        .attributes
        .push((name.clone(), signal));
    signal
}

#[inline]
fn existing_event(state: &State, id: InstanceId, name: NameId) -> Option<SignalId> {
    state.dom.at(id).signals.as_ref().and_then(|s| s.event(name))
}

/// Signal.Fire: queues every handler (deferred) and wakes threads blocked in :Wait().
pub fn fire(state: &mut State, signal: SignalId, args: Vec<Value>) {
    fire_costing(state, signal, args, 0.0, 0.0);
}

/// [`fire`], charging the firer `queue` seconds for a signal with handlers and each handler's thread `resume` seconds when it runs.
fn fire_costing(state: &mut State, signal: SignalId, args: Vec<Value>, queue: f64, resume: f64) {
    if !state.signals.has_handlers(signal) {
        return;
    }
    state.sched.charge_dispatch(queue);
    let handlers = state.signals.take_handlers(signal);
    // (a measured kind: a handler beyond the first queues and resumes for less than a lone one, cost.rs EXTRA_HANDLER_*)
    if resume > 0.0 && handlers.len() > 1 {
        state.sched.charge_dispatch((handlers.len() - 1) as f64 * crate::cost::EXTRA_HANDLER_QUEUED);
    }
    for (index, handler) in handlers.into_iter().enumerate() {
        let target = match handler {
            Handler::Thread(thread) => JobTarget::Thread(thread),
            Handler::Connection(connection) => JobTarget::Handler(connection),
        };
        let thread = if index == 0 { resume } else { resume.min(crate::cost::EXTRA_HANDLER_THREAD) };
        state.sched.defer_handler(target, args.clone(), thread, signal);
    }
}

/// FireSignal: fires an instance event if it was ever created.
pub fn fire_event(state: &mut State, id: InstanceId, name: NameId, args: Vec<Value>) {
    if let Some(signal) = existing_event(state, id, name) {
        fire(state, signal, args);
    }
}

/// [`fire_event`] for an event of the kinds Studio's bench measured (cost.rs `signal_costs`)
pub fn fire_event_measured(state: &mut State, id: InstanceId, name: NameId, args: Vec<Value>, kind: SignalKind) {
    if let Some(signal) = existing_event(state, id, name) {
        fire_measured(state, signal, args, kind);
    }
}

/// [`fire`] for an event of the kinds Studio's bench measured (cost.rs `SIGNAL_QUEUED`, `HANDLER_THREAD`)
fn fire_measured(state: &mut State, signal: SignalId, args: Vec<Value>, kind: SignalKind) {
    let (queue, resume) = crate::cost::signal_costs(kind);
    fire_costing(state, signal, args, queue, resume);
}

// Properties

/// RawSet(Silent = true): stores without notifications. Returns whether the value changed.
pub fn raw_set_silent(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId, value: Value) -> bool {
    store(state, reflection, id, name, value).0
}

/// RawSet's store (one property lookup): (whether the value changed, the previous value when it
/// did).
fn store(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId, value: Value) -> (bool, Value) {
    let (changed, old) = state.dom.exchange(id, name, value);
    if name == K::Name as NameId {
        state.dom.refresh_name(reflection, id);
    }
    if changed {
        state.dom.touch_rigs_for_property(id, name);
    }
    if changed {
        state.dom.bump_version(id);
    }
    (changed, old)
}

/// RawSet: stores a value and fires change notifications when it differs.
pub fn raw_set(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
    name: NameId,
    value: Value,
) -> bool {
    let (changed, old) = store(state, reflection, id, name, value);
    if changed {
        notify_changed(state, reflection, hooks, id, name, Some(old));
    } else {
        // Model:PivotTo writing a part where it is still moves its assembly (world/joints.rs)
        super::world::joints::written_unchanged(state, reflection, id, name);
        for hook in &hooks.property_unchanged {
            hook(state, id, name);
        }
    }
    changed
}

/// Stores a value the engine computed, as physics moves parts in Roblox: no Changed events (scripts
/// see none), but the PropertyChanged hooks run (a recorder sees it). Returns whether it changed.
pub fn raw_set_unsignalled(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
    name: NameId,
    value: Value,
) -> bool {
    let (changed, _) = store(state, reflection, id, name, value);
    if changed {
        for hook in &hooks.property_changed {
            hook(state, id, name);
        }
    }
    changed
}

/// NotifyChanged: Changed / GetPropertyChangedSignal events (measured order: other instances fire
/// Changed(name) before the property signal; ValueBase objects fire the property signal first and
/// Changed(value) only for Value), then the PropertyChanged hooks.
pub fn notify_changed(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
    name: NameId,
    old: Option<Value>,
) {
    notify_changed_events(state, reflection, hooks, id, name, old.as_ref());
    notify_changed_hooks(state, reflection, hooks, id, name, old);
}

/// NotifyChanged's events (Changed and the property signals, with the derived views').
fn notify_changed_events(state: &mut State, reflection: &Reflection, hooks: &Hooks, id: InstanceId, name: NameId, old: Option<&Value>) {
    // Derived views (Font of FontFace, Transparency of BackgroundTransparency, ...): those someone
    // listens to fire when their value changed, before or after the source's events; a part's companion events
    // (derived::Companions: Position, Orientation and Rotation beside CFrame, AssemblyMass beside Size, ...) fire
    // whatever their values
    let companions = hooks.derived.companions_for(reflection, state.dom.at(id).class, name);
    let (before, after) = super::derived::changed_views(state, reflection, hooks, id, name, old, companions.as_deref());
    if let Some(companions) = &companions {
        for &companion in &companions.before {
            fire_changed(state, reflection, id, companion);
        }
    }
    for view in before {
        fire_changed(state, reflection, id, view);
    }
    fire_changed(state, reflection, id, name);
    if let Some(companions) = &companions {
        for &companion in &companions.after {
            fire_changed(state, reflection, id, companion);
        }
    }
    for view in after {
        fire_changed(state, reflection, id, view);
    }
}

/// NotifyChanged's hooks (the PropertyChanged hooks, rigid assemblies).
fn notify_changed_hooks(state: &mut State, reflection: &Reflection, hooks: &Hooks, id: InstanceId, name: NameId, old: Option<Value>) {
    for hook in &hooks.property_changed {
        hook(state, id, name);
    }
    // Rigid assemblies: a part's CFrame moves its assembly, a joint's change settles it
    super::world::joints::property_changed(state, reflection, hooks, id, name, old.as_ref());
}

fn fire_changed(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId) {
    let Some(signals) = state.dom.at(id).signals.as_deref() else {
        return;
    };
    let changed = signals.event(K::Changed as NameId);
    let property = signals.property(name);
    let class = state.dom.at(id).class;
    let value_base = reflection.class_id("ValueBase").is_some_and(|base| reflection.class(class).is_a(base));
    if value_base {
        // (a value object's deprecated `changed` is a signal of its own that fires with Changed, the new value too: measured,
        // tools/parity/probes/aliases.luau)
        let changed_alias = reflection.changed_alias.and_then(|alias| signals.event(alias));
        if let Some(signal) = property {
            fire_measured(state, signal, Vec::new(), SignalKind::Property);
        }
        if name == K::Value as NameId {
            for signal in [changed, changed_alias].into_iter().flatten() {
                let value = state.dom.raw_get(id, K::Value as NameId).cloned().unwrap_or(Value::Nil);
                fire_measured(state, signal, vec![value], SignalKind::Changed);
            }
        }
        return;
    }
    if let Some(signal) = changed {
        fire_measured(state, signal, vec![Value::String(Str::from(reflection.name(name)))], SignalKind::Changed);
    }
    if let Some(signal) = property {
        fire_measured(state, signal, Vec::new(), SignalKind::Property);
    }
}

// Hierarchy

/// `from` and its ancestors, nearest first.
fn ancestry(state: &State, from: Option<InstanceId>) -> Vec<InstanceId> {
    let mut chain = Vec::new();
    let mut current = from;
    while let Some(node) = current {
        chain.push(node);
        current = state.dom.at(node).parent;
    }
    chain
}

/// FireDescendantEvents (DescendantAdded, DescendantRemoving), in the order a Studio client and the edit DataModel
/// both fire them (an arrival probe, 2026-10-23): for each instance of `root`'s subtree in
/// GetDescendants' order (the root first, depth first), every ancestor in `ancestors` (nearest first) that has the event.
/// An ancestor the subtree stays under (a move inside one folder) fires nothing: the caller leaves it out.
fn fire_descendant_events(state: &mut State, ancestors: &[InstanceId], root: InstanceId, event: NameId) {
    let listeners: Vec<SignalId> = ancestors.iter().filter_map(|&node| existing_event(state, node, event)).collect();
    if listeners.is_empty() {
        return;
    }
    let mut nodes = vec![root];
    nodes.extend(state.dom.descendants(root));
    let kind = if event == K::DescendantAdded as NameId {
        SignalKind::DescendantAdded
    } else if event == K::DescendantRemoving as NameId {
        SignalKind::DescendantRemoving
    } else {
        SignalKind::Unmeasured
    };
    for node in nodes {
        for &signal in &listeners {
            fire_measured(state, signal, vec![Value::Instance(node)], kind);
        }
    }
}

/// AncestryChanged of every instance of the moved subtree, children before the instance (post-order, measured in Studio
/// with the others above: A1, A2, A, B1, B, C, U for U(A(A1, A2), B(B1), C)).
fn fire_ancestry_changed(state: &mut State, root: InstanceId, new_parent: Option<InstanceId>) {
    let parent = new_parent.map_or(Value::Nil, Value::Instance);
    for node in state.dom.subtree_post_order(root) {
        if let Some(signal) = existing_event(state, node, K::AncestryChanged as NameId) {
            fire_measured(state, signal, vec![Value::Instance(root), parent.clone()], SignalKind::AncestryChanged);
        }
    }
}

/// InstanceModule.SetParent.
pub fn set_parent(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
    new_parent: Option<InstanceId>,
) -> ScriptResult<()> {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/hierarchy.luau): a service refuses with "Cannot change
    // Parent of type Lighting"; the DataModel and a destroyed instance with "The Parent property of X is locked, current
    // parent: NULL, new parent Y"; an instance made its own parent with "Attempt to set X as its own parent", and its
    // descendant with the circular-reference text.
    let is_data_model = Some(state.dom.at(id).class) == reflection.class_id("DataModel");
    if state.dom.at(id).has(flags::PARENT_LOCKED) || is_data_model {
        if is_service(state, reflection, id) {
            return Err(ScriptError::plain(format!("Cannot change Parent of type {}", reflection.class(state.dom.at(id).class).name)));
        }
        let new_name = match new_parent {
            Some(parent) => state.dom.at(parent).name.to_string(),
            None => "NULL".into(),
        };
        return Err(ScriptError::plain(format!(
            "The Parent property of {} is locked, current parent: NULL, new parent {new_name}",
            state.dom.at(id).name
        )));
    }
    let old_parent = state.dom.at(id).parent;
    if old_parent == new_parent {
        return Ok(());
    }
    if let Some(parent) = new_parent {
        if parent == id {
            return Err(ScriptError::plain(format!("Attempt to set {} as its own parent", state.dom.full_name(reflection, id))));
        }
        if state.dom.is_descendant_of(parent, id) {
            return Err(ScriptError::plain(format!(
                "Attempt to set parent of {} to {} would result in circular reference",
                state.dom.full_name(reflection, id),
                state.dom.full_name(reflection, parent)
            )));
        }
    }
    // Lazily imported instances: a moved subtree is made whole first (its descendants fire their
    // events, and what references them within their model exists), and a new parent's own children
    // come before the new child
    if state.dom.at(id).flags & (flags::LAZY_REGION | flags::LAZY_ABOVE) != 0 {
        super::project::materialize_region(state, reflection, id);
    }
    if let Some(parent) = new_parent.filter(|&parent| state.dom.at(parent).has(flags::LAZY_PENDING)) {
        super::project::materialize_lazy(state, reflection, parent);
    }
    // CollectionService: tagged instances entering or leaving the DataModel (services/mod.rs
    // CollectionState; Studio fires the removed signals with DescendantRemoving, before ChildRemoved
    // and AncestryChanged, and the added ones with DescendantAdded)
    let (was_inside, now_inside) = if state.collection.tagged {
        (
            old_parent.is_some_and(|parent| super::services::in_data_model(state, parent)),
            new_parent.is_some_and(|parent| super::services::in_data_model(state, parent)),
        )
    } else {
        (false, false)
    };
    // the work clock (crate::cost): Studio's cost of a subtree entering or leaving the DataModel
    let inside = |state: &State, parent: Option<InstanceId>| parent.is_some_and(|parent| super::services::in_data_model(state, parent));
    if inside(state, old_parent) != inside(state, new_parent) {
        let work: f64 = crate::cost::MOVE_BASE
            + std::iter::once(id).chain(state.dom.descendants(id)).map(|node| crate::cost::moved(reflection, state.dom.at(node).class)).sum::<f64>();
        state.sched.charge(work);
        crate::boundary::add_cost(work);
    } else {
        let loose = if inside(state, new_parent) { crate::cost::REPARENTED } else { crate::cost::REPARENTED_LOOSE };
        state.sched.charge(loose);
        crate::boundary::add_cost(loose);
    }
    // (entering the Workspace: the frame's first entry runs the engine's code for it cold, the first part's more (cost.rs Path::Enter,
    // EnterPart); each instance's entry is noted, so a destroy knows the engine took it into its frame work: destroy_live)
    let workspace = state.sched.touched.borrow().workspace;
    if let Some(workspace) = workspace {
        let placed = |state: &State, parent: Option<InstanceId>| parent.is_some_and(|parent| parent == workspace || state.dom.is_descendant_of(parent, workspace));
        let (was_placed, now_placed) = (placed(state, old_parent), placed(state, new_parent));
        if was_placed != now_placed {
            let mut touched = state.sched.touched.borrow_mut();
            let mut first = 0.0;
            let mut part = false;
            for node in std::iter::once(id).chain(state.dom.descendants(id)) {
                touched.set_entered(node.index, now_placed);
                touched.count(|ops| ops.entered += u64::from(now_placed));
                part = part || touched.kind(state.dom.at(node).class) == crate::cost::ColdKind::Part;
            }
            if now_placed {
                first += touched.first_path(crate::cost::Path::Enter);
                if part {
                    first += touched.first_path(crate::cost::Path::EnterPart);
                }
            }
            drop(touched);
            if first > 0.0 {
                state.sched.charge(first);
                crate::boundary::add_cost(first);
            }
        }
    }
    // (an ancestor the subtree keeps is told nothing: Root.DescendantAdded / DescendantRemoving stay quiet when a model moves
    // from Root.Units to Root.Other)
    let old_chain = ancestry(state, old_parent);
    let new_chain = ancestry(state, new_parent);
    if old_parent.is_some() {
        let leaving: Vec<InstanceId> = old_chain.iter().copied().filter(|node| !new_chain.contains(node)).collect();
        fire_descendant_events(state, &leaving, id, K::DescendantRemoving as NameId);
        if was_inside && !now_inside {
            super::services::subtree_moved(state, id, false);
        }
        state.dom.detach(id);
    }
    if let Some(parent) = new_parent {
        state.dom.attach(id, parent);
    }
    notify_changed_hooks(state, reflection, hooks, id, K::Parent as NameId, None);
    if let Some(old) = old_parent {
        fire_event_measured(state, old, K::ChildRemoved as NameId, vec![Value::Instance(id)], SignalKind::ChildRemoved);
    }
    if let Some(parent) = new_parent {
        // (Studio: every DescendantAdded of the subtree, then ChildAdded)
        let arriving: Vec<InstanceId> = new_chain.iter().copied().filter(|node| !old_chain.contains(node)).collect();
        fire_descendant_events(state, &arriving, id, K::DescendantAdded as NameId);
        if let Some(signal) = existing_event(state, parent, K::ChildAdded as NameId) {
            fire_measured(state, signal, vec![Value::Instance(id)], SignalKind::ChildAdded);
        }
        // (the dump's deprecated childAdded is a signal of its own that fires with ChildAdded: measured,
        // tools/parity/probes/aliases.luau)
        if let Some(alias) = reflection.child_added_alias
            && let Some(signal) = existing_event(state, parent, alias)
        {
            fire_measured(state, signal, vec![Value::Instance(id)], SignalKind::ChildAdded);
        }
        if now_inside && !was_inside {
            super::services::subtree_moved(state, id, true);
        }
    }
    fire_ancestry_changed(state, id, new_parent);
    // (the moved instance's own Changed("Parent") and Parent property signal come last: measured with the others)
    notify_changed_events(state, reflection, hooks, id, K::Parent as NameId, None);
    for hook in &hooks.hierarchy_changed {
        hook(state, id, old_parent, new_parent);
    }
    // Joints and parts entering a WorldRoot link up (world::joints)
    super::world::joints::reparented(state, reflection, hooks, id, old_parent, new_parent);
    Ok(())
}

// Clone

fn archivable(state: &State, id: InstanceId) -> bool {
    !matches!(state.dom.raw_get(id, K::Archivable as NameId), Some(Value::Bool(false)))
}

/// InstanceModule.Clone (after the caller materialized the subtree): copies properties (except
/// Parent), attributes, tags, sources and markers; instance-valued properties pointing inside
/// the copied tree are remapped to the copies. `None` when the root is not Archivable.
pub fn clone_instance(state: &mut State, reflection: &Reflection, root: InstanceId) -> Option<InstanceId> {
    if !archivable(state, root) {
        return None;
    }
    let mut map: Vec<(InstanceId, InstanceId)> = Vec::new();
    let result = copy_tree(state, reflection, root, &mut map)?;
    let copies: rustc_hash::FxHashMap<InstanceId, InstanceId> = map.iter().copied().collect();
    for &(_, copy) in &map {
        if !state.dom.at(copy).has(flags::HOLDS_REFS) {
            continue;
        }
        let remapped: Vec<(usize, InstanceId)> = state
            .dom
            .at(copy)
            .props
            .iter()
            .enumerate()
            .filter_map(|(index, (_, value))| copies.get(&value.as_instance()?).map(|c| (index, *c)))
            .collect();
        if remapped.is_empty() {
            continue;
        }
        let props = state.dom.at_mut(copy).props.own_mut();
        for (index, target) in remapped {
            props[index].1 = Value::Instance(target);
        }
    }
    for &(_, copy) in &map {
        let tags = state.dom.at(copy).tags.clone();
        for tag in tags {
            super::services::tag_added(state, copy, &tag);
        }
    }
    Some(result)
}

fn copy_tree(
    state: &mut State,
    reflection: &Reflection,
    original: InstanceId,
    map: &mut Vec<(InstanceId, InstanceId)>,
) -> Option<InstanceId> {
    if !archivable(state, original) {
        return None;
    }
    let source = state.dom.at(original);
    let class = source.class;
    let props = source.props.copied_without(K::Parent as NameId);
    let attributes = source.attributes.copied();
    let tags = source.tags.clone();
    let script = source.script.clone();
    let copied_flags = source.flags
        & (flags::SCRIPT_CREATED | flags::STUDIO_ONLY | flags::MATERIALIZED | flags::JSON_MODULE | flags::HOLDS_REFS);
    let studio_node = source.studio_node;
    let callbacks = source.callbacks.clone();
    let unmodeled = source.unmodeled.clone();
    let children = source.children.clone();

    let copy = create(state, reflection, class);
    {
        let target = state.dom.at_mut(copy);
        target.props = props;
        target.attributes = attributes;
        target.tags = tags;
        target.script = script;
        target.flags |= copied_flags;
        target.studio_node = studio_node;
        target.callbacks = callbacks;
        target.unmodeled = unmodeled;
    }
    state.dom.refresh_name(reflection, copy);
    map.push((original, copy));
    for child in children {
        if let Some(child_copy) = copy_tree(state, reflection, child, map) {
            state.dom.attach(child_copy, copy);
        }
    }
    Some(copy)
}

// Attributes and tags

/// SetAttribute after type validation: stores (or removes) and fires AttributeChanged plus the
/// attribute's own signal when the value changed.
pub fn set_attribute(state: &mut State, reflection: &Reflection, id: InstanceId, name: Str, value: Value) {
    let instance = state.dom.at_mut(id);
    // (an Instance attribute keeps its instance as an instance property does: lua::raw's collector walks both)
    if value.references_instance() {
        instance.flags |= flags::HOLDS_REFS;
    }
    let index = instance.attributes.iter().position(|(n, _)| *n == name);
    match (index, value.is_nil()) {
        (Some(index), _) if instance.attributes.as_slice()[index].1.same(&value) => return,
        (None, true) => return,
        (Some(index), true) => {
            instance.attributes.own_mut().remove(index);
        }
        (Some(index), false) => instance.attributes.own_mut()[index].1 = value,
        (None, false) => instance.attributes.own_mut().push((name.clone(), value)),
    }
    instance.version = instance.version.wrapping_add(1);
    let _ = reflection;
    if let Some(signal) = existing_event(state, id, K::AttributeChanged as NameId) {
        fire_measured(state, signal, vec![Value::String(name.clone())], SignalKind::Attribute);
    }
    if let Some(signal) = state.dom.at(id).signals.as_ref().and_then(|s| s.attribute(name.as_bytes())) {
        fire_measured(state, signal, Vec::new(), SignalKind::Attribute);
    }
}

pub fn add_tag(state: &mut State, id: InstanceId, tag: Str) {
    if state.dom.at(id).tags.contains(&tag) {
        return;
    }
    state.dom.at_mut(id).tags.push(tag.clone());
    state.dom.bump_version(id);
    super::services::tag_added(state, id, &tag);
}

pub fn remove_tag(state: &mut State, id: InstanceId, tag: &Str) {
    let tags = &mut state.dom.at_mut(id).tags;
    let Some(index) = tags.iter().position(|t| t == tag) else {
        return;
    };
    tags.remove(index);
    state.dom.bump_version(id);
    super::services::tag_removed(state, id, tag);
}

/// A service: a parent-locked instance of a service class under the DataModel.
fn is_service(state: &State, reflection: &Reflection, id: InstanceId) -> bool {
    let instance = state.dom.at(id);
    instance.has(flags::PARENT_LOCKED)
        && reflection.class(instance.class).service
        && instance.parent.is_some_and(|parent| Some(state.dom.at(parent).class) == reflection.class_id("DataModel"))
}

/// The first half of Destroy (after its Destroying handlers were queued): marks the instance destroyed,
/// unparents it and locks its parent. Returns the children to destroy next.
pub fn begin_destroy(
    state: &mut State,
    reflection: &Reflection,
    hooks: &Hooks,
    id: InstanceId,
) -> ScriptResult<Vec<InstanceId>> {
    // a service stays as it was: "The Parent property of Lighting is locked" (measured, tools/parity/probes/hierarchy.luau)
    if is_service(state, reflection, id) {
        return Err(ScriptError::plain(format!("The Parent property of {} is locked", state.dom.at(id).name)));
    }
    state.dom.at_mut(id).set_flag(flags::DESTROYED, true);
    if state.dom.at(id).parent.is_some() {
        set_parent(state, reflection, hooks, id, None)?;
    }
    // A destroyed Model lets go of its PrimaryPart (measured in Studio, 5 of 5; unparenting keeps it): the
    // part it named stays collectable once the Model is the last thing that referenced it
    if reflection.is_a(state.dom.at(id).class, "Model")
        && let Some(primary) = reflection.name_id("PrimaryPart")
        && matches!(state.dom.raw_get(id, primary), Some(Value::Instance(_)))
    {
        raw_set(state, reflection, hooks, id, primary, Value::Nil);
    }
    state.dom.at_mut(id).set_flag(flags::PARENT_LOCKED, true);
    Ok(state.dom.at(id).children.clone())
}

/// The rest of Destroy: signals disconnect at the next resumption point. (The instance keeps its
/// tags, as in Studio: CollectionService's removed signals fired when it left the DataModel.)
pub fn finish_destroy(state: &mut State, id: InstanceId) {
    state.sched.defer(JobTarget::DisconnectAll(id), Vec::new());
}

/// Disconnects every signal of an instance (Destroy's deferred cleanup).
pub fn disconnect_all(state: &mut State, id: InstanceId) {
    let Some(signals) = state.dom.at(id).signals.as_deref() else {
        return;
    };
    let ids: Vec<SignalId> = signals.all().collect();
    for signal in ids {
        state.signals.disconnect_all(signal);
    }
}

/// The signals created for an instance (for invalidation-aware ports).
pub fn instance_signals(state: &State, id: InstanceId) -> Option<&InstanceSignals> {
    state.dom.at(id).signals.as_deref()
}

//! The Input Action System: InputContext, InputAction and InputBinding. Each action keeps its state (GetState), which
//! InputAction:Fire, a Scriptable InputBinding's Fire and device input change, raising Pressed / Released and
//! StateChanged; the core never reads a device: a host that replays device input (a Layer) calls [`deliver`] for a key or
//! mouse button going down or up, and the core finds the actions bound to it.
//!
//! Measured in a 0.747 playing client (tools/parity/probes/input_actions.luau and input_action_resets.luau for the script
//! side; the device side with the client's own key and mouse input):
//!
//! - State: Bool false, Direction1D 0 (a number kept as a float), Direction2D (0, 0), Direction3D (0, 0, 0),
//!   ViewportPosition (-1, -1) until set. A value equal to the state changes nothing. A change queues, in order, Pressed or
//!   Released (Bool: false to true, true to false) and StateChanged(state), deferred, from the call that made it: GetState
//!   answers the new state at once. No value is scaled, clamped or checked against thresholds by Fire.
//! - Fire(state) acts on an enabled action whose nearest InputContext ancestor is enabled, or, with no context, one in
//!   the DataModel; elsewhere it does nothing. The state must be the type's (a bool, a number, a Vector2 or a Vector3):
//!   "Action:Fire() on InputActionType.Bool should be called with a bool state"; nil is "Argument 1 missing or nil".
//!   An InputBinding's Fire needs Type Scriptable ("InputBinding:Fire() can only be called when Type is Scriptable") and
//!   acts on its nearest InputAction ancestor as Fire does (its errors say "InputBinding:Fire()"); one with no action
//!   does nothing.
//! - Resets: an action whose context is disabled, which is disabled itself, or whose place in the tree changes (the
//!   action, its context or an ancestor moved, a context destroyed) goes back to its type's default (Released and
//!   StateChanged for a pressed Bool); a change of Type gives the new type's default with StateChanged alone. Priority
//!   and Sink changes keep it.
//! - Devices ([`deliver`]): a key or mouse button (KeyCode.MouseLeftButton, MouseRightButton, MouseMiddleButton)
//!   reaches the enabled actions in the DataModel, not under StarterGui, StarterPack or StarterPlayer, whose nearest
//!   context is enabled (an outer context does not matter; an action without one gets it too), through their
//!   Automatic bindings that are their direct children. A binding with a KeyCode answers that key: down (with each of
//!   its PrimaryModifier and SecondaryModifier already down; other keys held do not matter) makes a Bool true and a
//!   Direction1D its Scale; its key or a modifier going up gives false or 0; on other types it does nothing. A binding
//!   without a KeyCode is a composite: Up / Down make a Direction1D +-Scale; Up, Down, Left, Right (and Forward -Z,
//!   Backward +Z for a Direction3D) sum to a vector, made one long when longer and ClampMagnitudeToOne is on, then
//!   multiplied by Vector2Scale (Vector3Scale) and Scale. Each input sets the state its binding gives (the last input
//!   wins: of two keys bound to one Bool, releasing either releases it). Contexts are tried in their delivery order:
//!   by Priority, highest first; among equal priorities the one that entered the DataModel last first, kept in place
//!   by a Priority change (a stable re-sort); an action without a context after them. Once a priority level held a
//!   context with Sink on and an action that took the input, lower ones do not get it; a key going up reaches every
//!   binding it holds. A key held when its context is enabled does not press; a binding whose keys change while it
//!   holds its action releases it. The UserInputService events of the input come first (the host fires them before it
//!   calls [`deliver`]).

use std::{
    cell::{OnceCell, RefCell},
    rc::{Rc, Weak},
};

use mlua::prelude::*;
use rbx_dom_weak::types as dom;
use rustc_hash::FxHashMap;

use super::{Behaviors, method, values};
use crate::{
    ClassId, Engine, InstanceId, NameId, State,
    instance as core,
    lua::{Args, Flow, convert, none, raise},
    reflection::EnumItemId,
    value::Value,
};

/// An action's state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ActionState {
    Bool(bool),
    Number(f32),
    Vector2([f32; 2]),
    Vector3([f32; 3]),
}

impl ActionState {
    fn value(self) -> Value {
        match self {
            ActionState::Bool(on) => Value::Bool(on),
            ActionState::Number(n) => Value::Number(n as f64),
            ActionState::Vector2(v) => Value::Vector2(dom::Vector2::new(v[0], v[1])),
            ActionState::Vector3(v) => Value::Vector3(dom::Vector3::new(v[0], v[1], v[2])),
        }
    }
}

/// InputActionType.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Bool,
    Direction1D,
    Direction2D,
    Direction3D,
    ViewportPosition,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Bool => "Bool",
            Kind::Direction1D => "Direction1D",
            Kind::Direction2D => "Direction2D",
            Kind::Direction3D => "Direction3D",
            Kind::ViewportPosition => "ViewportPosition",
        }
    }

    fn default_state(self) -> ActionState {
        match self {
            Kind::Bool => ActionState::Bool(false),
            Kind::Direction1D => ActionState::Number(0.0),
            Kind::Direction2D => ActionState::Vector2([0.0; 2]),
            Kind::Direction3D => ActionState::Vector3([0.0; 3]),
            Kind::ViewportPosition => ActionState::Vector2([-1.0; 2]),
        }
    }

    /// What Fire takes, by name (its error) and as a state.
    fn expects(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Direction1D => "number",
            Kind::Direction2D | Kind::ViewportPosition => "Vector2",
            Kind::Direction3D => "Vector3",
        }
    }

    fn read(self, value: &LuaValue) -> Option<ActionState> {
        match (self, value) {
            (Kind::Bool, LuaValue::Boolean(on)) => Some(ActionState::Bool(*on)),
            (Kind::Direction1D, LuaValue::Number(n)) => Some(ActionState::Number(*n as f32)),
            (Kind::Direction1D, LuaValue::Integer(n)) => Some(ActionState::Number(*n as f32)),
            (Kind::Direction2D | Kind::ViewportPosition, value) => match convert::from_lua(value) {
                Value::Vector2(v) => Some(ActionState::Vector2([v.x, v.y])),
                _ => None,
            },
            (Kind::Direction3D, value) => match convert::from_lua(value) {
                Value::Vector3(v) => Some(ActionState::Vector3([v.x, v.y, v.z])),
                _ => None,
            },
            _ => None,
        }
    }
}

struct Names {
    context: ClassId,
    action: ClassId,
    binding: ClassId,
    /// StarterGui, StarterPack, StarterPlayer: device input does not reach what is below them
    starters: Vec<ClassId>,
    enabled: NameId,
    kind: NameId,
    priority: NameId,
    sink: NameId,
    key_code: NameId,
    modifiers: [NameId; 2],
    /// Up, Down, Left, Right, Forward, Backward
    directions: [NameId; 6],
    scale: NameId,
    vector2_scale: NameId,
    vector3_scale: NameId,
    clamp: NameId,
    pressed: NameId,
    released: NameId,
    state_changed: NameId,
    kinds: [(EnumItemId, Kind); 5],
    scriptable: EnumItemId,
    no_key: EnumItemId,
}

/// A binding's device input holding its action: the keys it holds (its KeyCode, or its composite's directions held).
struct Hold {
    action: InstanceId,
    binding: InstanceId,
    keys: Vec<EnumItemId>,
}

#[derive(Default)]
struct InputActions {
    names: OnceCell<Option<Rc<Names>>>,
    /// The actions whose state is not their type's default
    states: RefCell<FxHashMap<InstanceId, ActionState>>,
    /// Device input holding actions, in the order it came
    holds: RefCell<Vec<Hold>>,
    /// The contexts in the DataModel, in delivery order
    order: RefCell<Vec<InstanceId>>,
    /// The keys and buttons down, as delivered
    down: RefCell<Vec<EnumItemId>>,
}

impl crate::Extension for InputActions {}

fn extension(engine: &Engine) -> &InputActions {
    engine.extension_or_insert_with(InputActions::default)
}

fn names(engine: &Engine) -> Option<Rc<Names>> {
    extension(engine)
        .names
        .get_or_init(|| {
            let r = &engine.reflection;
            let n = |name: &str| r.name_id(name);
            let kind = |name: &str, kind: Kind| r.enum_item("InputActionType", name).map(|item| (item, kind));
            Some(Rc::new(Names {
                context: r.class_id("InputContext")?,
                action: r.class_id("InputAction")?,
                binding: r.class_id("InputBinding")?,
                starters: ["StarterGui", "StarterPack", "StarterPlayer"].iter().filter_map(|name| r.class_id(name)).collect(),
                enabled: n("Enabled")?,
                kind: n("Type")?,
                priority: n("Priority")?,
                sink: n("Sink")?,
                key_code: n("KeyCode")?,
                modifiers: [n("PrimaryModifier")?, n("SecondaryModifier")?],
                directions: [n("Up")?, n("Down")?, n("Left")?, n("Right")?, n("Forward")?, n("Backward")?],
                scale: n("Scale")?,
                vector2_scale: n("Vector2Scale")?,
                vector3_scale: n("Vector3Scale")?,
                clamp: n("ClampMagnitudeToOne")?,
                pressed: n("Pressed")?,
                released: n("Released")?,
                state_changed: n("StateChanged")?,
                kinds: [
                    kind("Bool", Kind::Bool)?,
                    kind("Direction1D", Kind::Direction1D)?,
                    kind("Direction2D", Kind::Direction2D)?,
                    kind("Direction3D", Kind::Direction3D)?,
                    kind("ViewportPosition", Kind::ViewportPosition)?,
                ],
                scriptable: r.enum_item("InputBindingType", "Scriptable")?,
                no_key: r.enum_item("KeyCode", "None")?,
            }))
        })
        .clone()
}

fn class_of(state: &State, id: InstanceId) -> Option<ClassId> {
    state.dom.get(id).map(|instance| instance.class)
}

fn kind_of(state: &State, names: &Names, action: InstanceId) -> Kind {
    match state.dom.raw_get(action, names.kind) {
        Some(Value::EnumItem(item)) => names.kinds.iter().find(|(id, _)| id == item).map_or(Kind::Bool, |(_, kind)| *kind),
        _ => Kind::Bool,
    }
}

fn flag(state: &State, id: InstanceId, name: NameId) -> bool {
    state.dom.boolean(id, name).unwrap_or(true)
}

fn number(state: &State, id: InstanceId, name: NameId, default: f32) -> f32 {
    state.dom.number(id, name).map_or(default, |n| n as f32)
}

fn key(state: &State, id: InstanceId, name: NameId) -> Option<EnumItemId> {
    match state.dom.raw_get(id, name) {
        Some(Value::EnumItem(item)) => Some(*item),
        _ => None,
    }
}

/// The nearest InputContext above `id`.
fn context_of(state: &State, names: &Names, id: InstanceId) -> Option<InstanceId> {
    let mut at = state.dom.parent_of(id);
    while let Some(parent) = at {
        if class_of(state, parent) == Some(names.context) {
            return Some(parent);
        }
        at = state.dom.parent_of(parent);
    }
    None
}

/// The nearest InputAction above `id`.
fn action_of(state: &State, names: &Names, id: InstanceId) -> Option<InstanceId> {
    let mut at = state.dom.parent_of(id);
    while let Some(parent) = at {
        if class_of(state, parent) == Some(names.action) {
            return Some(parent);
        }
        at = state.dom.parent_of(parent);
    }
    None
}

fn is_below(state: &State, id: InstanceId, ancestor: InstanceId) -> bool {
    let mut at = Some(id);
    while let Some(here) = at {
        if here == ancestor {
            return true;
        }
        at = state.dom.parent_of(here);
    }
    false
}

fn in_data_model(state: &State, id: InstanceId) -> bool {
    state.collection.game.is_some_and(|game| is_below(state, id, game))
}

/// Whether Fire acts on `action` (measured): enabled, with its nearest context enabled, or with none and in the DataModel.
fn fire_reaches(state: &State, names: &Names, action: InstanceId) -> bool {
    if !flag(state, action, names.enabled) {
        return false;
    }
    match context_of(state, names, action) {
        Some(context) => flag(state, context, names.enabled),
        None => in_data_model(state, action),
    }
}

/// Sets `action`'s state, queuing Pressed / Released and StateChanged when it changed.
fn set_state(state: &mut State, input: &InputActions, names: &Names, action: InstanceId, kind: Kind, new: ActionState) {
    let default = kind.default_state();
    let old = input.states.borrow().get(&action).copied().unwrap_or(default);
    if old == new {
        return;
    }
    if new == default {
        input.states.borrow_mut().remove(&action);
    } else {
        input.states.borrow_mut().insert(action, new);
    }
    if let (ActionState::Bool(was), ActionState::Bool(now)) = (old, new) {
        if now && !was {
            core::fire_event(state, action, names.pressed, Vec::new());
        } else if was && !now {
            core::fire_event(state, action, names.released, Vec::new());
        }
    }
    core::fire_event(state, action, names.state_changed, vec![new.value()]);
}

/// `action` back to its type's default (its device holds forgotten), with the events a change gives.
fn reset(state: &mut State, input: &InputActions, names: &Names, action: InstanceId) {
    input.holds.borrow_mut().retain(|hold| hold.action != action);
    if input.states.borrow().contains_key(&action) {
        let kind = kind_of(state, names, action);
        set_state(state, input, names, action, kind, kind.default_state());
    }
}

/// Action:Fire / InputBinding:Fire on `action`.
fn fire(engine: &Engine, names: &Names, action: InstanceId, value: &LuaValue, caller: &str) -> Result<(), Flow> {
    let input = extension(engine);
    let mut state = engine.state.borrow_mut();
    let kind = kind_of(&state, names, action);
    let Some(new) = kind.read(value) else {
        return raise(format!("{caller}:Fire() on InputActionType.{} should be called with a {} state", kind.name(), kind.expects()));
    };
    if fire_reaches(&state, names, action) {
        set_state(&mut state, input, names, action, kind, new);
    }
    Ok(())
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let Some(n) = names(engine) else { return };
    let reflection = engine.reflection.clone();
    let name = |member: &str| reflection.name_id(member);
    if let Some(fire_name) = name("Fire") {
        behaviors.add_method(n.action, fire_name, method(|_, engine, id, args| {
            let names = names(engine).expect("installed");
            if args.get(1).is_nil() {
                return raise("Argument 1 missing or nil");
            }
            fire(engine, &names, id, args.get(1), "Action")?;
            Ok(none())
        }));
        behaviors.add_method(n.binding, fire_name, method(|_, engine, id, args| {
            let names = names(engine).expect("installed");
            if args.get(1).is_nil() {
                return raise("Argument 1 missing or nil");
            }
            let (scriptable, action) = {
                let state = engine.state.borrow();
                (key(&state, id, names.kind) == Some(names.scriptable), action_of(&state, &names, id))
            };
            if !scriptable {
                return raise("InputBinding:Fire() can only be called when Type is Scriptable");
            }
            if let Some(action) = action {
                fire(engine, &names, action, args.get(1), "InputBinding")?;
            }
            Ok(none())
        }));
    }
    if let Some(get_state) = name("GetState") {
        behaviors.add_method(n.action, get_state, method(|lua, engine, id, _: &Args| {
            let names = names(engine).expect("installed");
            let value = {
                let state = engine.state.borrow();
                let kind = kind_of(&state, &names, id);
                extension(engine).states.borrow().get(&id).copied().unwrap_or(kind.default_state()).value()
            };
            Ok(values([convert::to_lua(lua, engine, &value)?]))
        }));
    }
    // (measured: a game script may not call them)
    for (class, member) in [(n.context, "GetInputActions"), (n.action, "GetInputBindings")] {
        if let Some(id) = name(member) {
            let message = format!("The current thread cannot call '{member}' (lacking capability RobloxScript)");
            behaviors.add_method(class, id, method(move |_, _, _, _| raise(message.clone())));
        }
    }

    let weak = Rc::downgrade(engine);
    let mut hooks = engine.hooks.borrow_mut();
    let on_move: Weak<Engine> = weak.clone();
    hooks.hierarchy_changed.push(Box::new(move |state, id, _, new_parent| {
        let Some(engine) = on_move.upgrade() else { return };
        let (Some(names), input) = (names(&engine), extension(&engine)) else { return };
        moved(state, input, &names, id, new_parent);
    }));
    let on_change = weak.clone();
    hooks.property_changed.push(Box::new(move |state, id, name| {
        let Some(engine) = on_change.upgrade() else { return };
        let (Some(names), input) = (names(&engine), extension(&engine)) else { return };
        changed(state, input, &names, id, name);
    }));
    let on_free = weak;
    hooks.instance_freed.push(Box::new(move |_, id| {
        let Some(engine) = on_free.upgrade() else { return };
        let input = extension(&engine);
        input.states.borrow_mut().remove(&id);
        input.holds.borrow_mut().retain(|hold| hold.action != id && hold.binding != id);
        input.order.borrow_mut().retain(|&context| context != id);
    }));
}

/// An instance's parent changed: a context entering or leaving the DataModel joins or leaves the delivery order, and an
/// action it moved (the action, its context, an ancestor) or whose holding binding it moved is reset.
fn moved(state: &mut State, input: &InputActions, names: &Names, id: InstanceId, new_parent: Option<InstanceId>) {
    if class_of(state, id) == Some(names.context) {
        let inside = new_parent.is_some_and(|parent| in_data_model(state, parent));
        let known = input.order.borrow().contains(&id);
        if inside && !known {
            register(state, input, names, id);
        } else if !inside && known {
            input.order.borrow_mut().retain(|&context| context != id);
        }
    }
    if input.states.borrow().is_empty() && input.holds.borrow().is_empty() {
        return;
    }
    let mut affected: Vec<InstanceId> = input.states.borrow().keys().copied().filter(|&action| is_below(state, action, id)).collect();
    for hold in input.holds.borrow().iter() {
        if (is_below(state, hold.action, id) || is_below(state, hold.binding, id)) && !affected.contains(&hold.action) {
            affected.push(hold.action);
        }
    }
    // (in a fixed order: an action's events queue as it resets)
    affected.sort_unstable_by_key(|action| (action.index, action.generation));
    for action in affected {
        reset(state, input, names, action);
    }
}

/// A context joins the delivery order at the front of its priority (measured: the one that entered last is tried first).
fn register(state: &State, input: &InputActions, names: &Names, context: InstanceId) {
    let priority = number(state, context, names.priority, 1000.0);
    let mut order = input.order.borrow_mut();
    let at = order.iter().position(|&other| number(state, other, names.priority, 1000.0) <= priority).unwrap_or(order.len());
    order.insert(at, context);
}

fn changed(state: &mut State, input: &InputActions, names: &Names, id: InstanceId, name: NameId) {
    let Some(class) = class_of(state, id) else { return };
    if class == names.context {
        if name == names.enabled && !flag(state, id, names.enabled) {
            let mut actions: Vec<InstanceId> = input.states.borrow().keys().copied().collect();
            actions.extend(input.holds.borrow().iter().map(|hold| hold.action));
            actions.sort_unstable_by_key(|action| (action.index, action.generation));
            actions.dedup();
            for action in actions {
                if context_of(state, names, action) == Some(id) {
                    reset(state, input, names, action);
                }
            }
        } else if name == names.priority {
            // (a stable re-sort: among equal priorities the context keeps its place)
            input.order.borrow_mut().sort_by(|&a, &b| number(state, b, names.priority, 1000.0).total_cmp(&number(state, a, names.priority, 1000.0)));
        }
    } else if class == names.action {
        if name == names.enabled && !flag(state, id, names.enabled) {
            reset(state, input, names, id);
        } else if name == names.kind {
            // the new type's default, StateChanged alone (measured: a pressed Bool made Direction1D reads 0, no Released)
            input.holds.borrow_mut().retain(|hold| hold.action != id);
            if input.states.borrow_mut().remove(&id).is_some() {
                let kind = kind_of(state, names, id);
                core::fire_event(state, id, names.state_changed, vec![kind.default_state().value()]);
            }
        }
    } else if class == names.binding && (name == names.key_code || name == names.kind || names.modifiers.contains(&name) || names.directions.contains(&name)) {
        let held: Vec<InstanceId> = input.holds.borrow().iter().filter(|hold| hold.binding == id).map(|hold| hold.action).collect();
        for action in held {
            reset(state, input, names, action);
        }
    }
}

/// A device's key or mouse button (`key`, a KeyCode item: MouseLeftButton, MouseRightButton and MouseMiddleButton for the
/// buttons) went down or up: the actions bound to it take it as Roblox delivers it (the module's comment). A host calls it
/// with no engine borrow held, after it fired UserInputService.InputBegan / InputEnded for the input, and not for input a
/// focused TextBox takes (measured: it never reaches an action).
pub fn deliver(engine: &Engine, key: EnumItemId, down: bool) {
    let Some(names) = names(engine) else { return };
    let input = extension(engine);
    {
        let mut keys = input.down.borrow_mut();
        keys.retain(|&k| k != key);
        if down {
            keys.push(key);
        }
    }
    let mut state = engine.state.borrow_mut();
    if down {
        press(&mut state, input, &names, key);
    } else {
        release(&mut state, input, &names, key);
    }
}

/// The value a composite binding's held directions give (`held`: the direction keys down), if its action's type has one.
fn composite(state: &State, names: &Names, binding: InstanceId, kind: Kind, held: &[EnumItemId]) -> Option<ActionState> {
    let scale = number(state, binding, names.scale, 1.0);
    let mut sum = [0f32; 3];
    // Up +Y, Down -Y, Left -X, Right +X, Forward -Z, Backward +Z
    let axes: [(usize, f32); 6] = [(1, 1.0), (1, -1.0), (0, -1.0), (0, 1.0), (2, -1.0), (2, 1.0)];
    for (&direction, &(axis, sign)) in names.directions.iter().zip(&axes) {
        if key(state, binding, direction).is_some_and(|k| k != names.no_key && held.contains(&k)) {
            sum[axis] += sign;
        }
    }
    let clamp = |v: glam::Vec3| if flag(state, binding, names.clamp) && v.length() > 1.0 { v.normalize() } else { v };
    match kind {
        Kind::Direction1D => Some(ActionState::Number(sum[1] * scale)),
        Kind::Direction2D => {
            let v = clamp(glam::Vec3::new(sum[0], sum[1], 0.0));
            let s = match state.dom.raw_get(binding, names.vector2_scale) {
                Some(Value::Vector2(s)) => [s.x, s.y],
                _ => [1.0, 1.0],
            };
            Some(ActionState::Vector2([v.x * s[0] * scale, v.y * s[1] * scale]))
        }
        Kind::Direction3D => {
            let v = clamp(glam::Vec3::new(sum[0], sum[1], sum[2]));
            let s = match state.dom.raw_get(binding, names.vector3_scale) {
                Some(Value::Vector3(s)) => [s.x, s.y, s.z],
                _ => [1.0; 3],
            };
            Some(ActionState::Vector3([v.x * s[0] * scale, v.y * s[1] * scale, v.z * s[2] * scale]))
        }
        Kind::Bool | Kind::ViewportPosition => None,
    }
}

/// The actions device input reaches, grouped by context in delivery order: (context, its actions), an action without a
/// context under None last.
fn reachable(state: &State, input: &InputActions, names: &Names) -> Vec<(Option<InstanceId>, Vec<InstanceId>)> {
    let Some(game) = state.collection.game else { return Vec::new() };
    let mut found: Vec<(Option<InstanceId>, InstanceId)> = Vec::new();
    let mut stack: Vec<(InstanceId, Option<InstanceId>)> = state.dom.children_of(game).iter().rev().map(|&child| (child, None)).collect();
    while let Some((id, context)) = stack.pop() {
        let Some(class) = class_of(state, id) else { continue };
        if names.starters.contains(&class) {
            continue;
        }
        let inner = if class == names.context { Some(id) } else { context };
        if class == names.action {
            found.push((inner, id));
        }
        stack.extend(state.dom.children_of(id).iter().rev().map(|&child| (child, inner)));
    }
    if found.is_empty() {
        return Vec::new();
    }
    // contexts the hooks did not see enter (a model's, one inside something parented) join the order now
    for &(context, _) in &found {
        if let Some(context) = context {
            if !input.order.borrow().contains(&context) {
                register(state, input, names, context);
            }
        }
    }
    let order = input.order.borrow();
    let mut groups: Vec<(Option<InstanceId>, Vec<InstanceId>)> = order.iter().map(|&context| (Some(context), Vec::new())).collect();
    groups.push((None, Vec::new()));
    for (context, action) in found {
        let at = match context {
            Some(context) => order.iter().position(|&c| c == context).unwrap_or(groups.len() - 1),
            None => groups.len() - 1,
        };
        groups[at].1.push(action);
    }
    groups.retain(|(_, actions)| !actions.is_empty());
    groups
}

fn press(state: &mut State, input: &InputActions, names: &Names, pressed: EnumItemId) {
    let groups = reachable(state, input, names);
    let mut level: Option<f32> = None;
    let mut sunk = false;
    for (context, actions) in groups {
        let priority = context.map_or(1000.0, |c| number(state, c, names.priority, 1000.0));
        if level != Some(priority) {
            if sunk {
                break;
            }
            level = Some(priority);
        }
        if context.is_some_and(|c| !flag(state, c, names.enabled)) {
            continue;
        }
        let sinks = context.is_some_and(|c| state.dom.boolean(c, names.sink).unwrap_or(false));
        for action in actions {
            if !flag(state, action, names.enabled) {
                continue;
            }
            let kind = kind_of(state, names, action);
            let bindings: Vec<InstanceId> = state.dom.children_of(action).iter().copied().filter(|&b| class_of(state, b) == Some(names.binding)).collect();
            for binding in bindings {
                if key(state, binding, names.kind) == Some(names.scriptable) {
                    continue;
                }
                let key_code = key(state, binding, names.key_code).filter(|&k| k != names.no_key);
                let taken = match key_code {
                    Some(code) => {
                        let modifiers_down = names.modifiers.iter().all(|&m| match key(state, binding, m) {
                            Some(modifier) if modifier != names.no_key => modifier != pressed && input.down.borrow().contains(&modifier),
                            _ => true,
                        });
                        let value = match kind {
                            Kind::Bool => Some(ActionState::Bool(true)),
                            Kind::Direction1D => Some(ActionState::Number(number(state, binding, names.scale, 1.0))),
                            _ => None,
                        };
                        match value {
                            Some(value) if code == pressed && modifiers_down => {
                                let mut holds = input.holds.borrow_mut();
                                holds.retain(|hold| hold.binding != binding);
                                holds.push(Hold { action, binding, keys: vec![pressed] });
                                drop(holds);
                                set_state(state, input, names, action, kind, value);
                                true
                            }
                            _ => false,
                        }
                    }
                    None => {
                        let is_direction = names.directions.iter().any(|&d| key(state, binding, d) == Some(pressed));
                        if !is_direction || matches!(kind, Kind::Bool | Kind::ViewportPosition) {
                            false
                        } else {
                            let held = {
                                let mut holds = input.holds.borrow_mut();
                                match holds.iter_mut().find(|hold| hold.binding == binding) {
                                    Some(hold) => {
                                        if !hold.keys.contains(&pressed) {
                                            hold.keys.push(pressed);
                                        }
                                        hold.keys.clone()
                                    }
                                    None => {
                                        holds.push(Hold { action, binding, keys: vec![pressed] });
                                        vec![pressed]
                                    }
                                }
                            };
                            if let Some(value) = composite(state, names, binding, kind, &held) {
                                set_state(state, input, names, action, kind, value);
                            }
                            true
                        }
                    }
                };
                if taken && sinks {
                    sunk = true;
                }
            }
        }
    }
}

fn release(state: &mut State, input: &InputActions, names: &Names, released: EnumItemId) {
    let holds: Vec<(InstanceId, InstanceId, Vec<EnumItemId>)> = input.holds.borrow().iter().map(|hold| (hold.action, hold.binding, hold.keys.clone())).collect();
    for (action, binding, keys) in holds {
        if state.dom.get(action).is_none() || state.dom.get(binding).is_none() {
            continue;
        }
        let kind = kind_of(state, names, action);
        let key_code = key(state, binding, names.key_code).filter(|&k| k != names.no_key);
        match key_code {
            Some(code) => {
                let modifier = names.modifiers.iter().any(|&m| key(state, binding, m) == Some(released));
                if code == released || modifier {
                    input.holds.borrow_mut().retain(|hold| hold.binding != binding);
                    let value = match kind {
                        Kind::Bool => ActionState::Bool(false),
                        Kind::Direction1D => ActionState::Number(0.0),
                        _ => continue,
                    };
                    set_state(state, input, names, action, kind, value);
                }
            }
            None => {
                if !keys.contains(&released) {
                    continue;
                }
                let held: Vec<EnumItemId> = keys.iter().copied().filter(|&k| k != released).collect();
                {
                    let mut holds = input.holds.borrow_mut();
                    if held.is_empty() {
                        holds.retain(|hold| hold.binding != binding);
                    } else if let Some(hold) = holds.iter_mut().find(|hold| hold.binding == binding) {
                        hold.keys = held.clone();
                    }
                }
                if let Some(value) = composite(state, names, binding, kind, &held) {
                    set_state(state, input, names, action, kind, value);
                }
            }
        }
    }
}

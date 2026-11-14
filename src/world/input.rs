//! Simulated player input: [`Input`]s a harness (`context.Input`) or a host sends are queued with a frame number and
//! delivered in the frame's input phase ([`Phase::Input`]: after Heartbeat, before the render-step bindings) to
//! UserInputService's events and queries and to ContextActionService's bound actions.
//!
//! A real input source forwards through [`send`] (next frame) or [`send_at`] (a frame of its choosing).

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rbx_dom_weak::types as dom;

use crate::{
    Engine, InstanceId,
    instance as core,
    lua::{
        Args, Flow, Ret, convert, enum_item_value, instance::fire_immediate_costing, instance_value, none, raise,
        types::LEnumItem, values,
    },
    reflection::EnumItemId,
    scheduler::{self, Phase},
    services,
    signal::SignalId,
    value::{Str, Value},
};

const DEFAULT_ACTION_PRIORITY: i32 = 2000;
const MOUSE_UNKNOWN: (f64, f64) = (-1000.0, -1000.0);
const MAX_PER_FRAME: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Begin,
    Change,
    End,
}

#[derive(Clone, Debug)]
pub enum Input {
    Key { code: EnumItemId, down: bool },
    MouseButton { button: EnumItemId, down: bool },
    MouseMove { x: f64, y: f64 },
    MouseDelta { dx: f64, dy: f64 },
    Scroll { dy: f64 },
    Touch { id: u32, x: f64, y: f64, step: Step },
    GamepadButton { code: EnumItemId, down: bool },
    Axis { code: EnumItemId, x: f64, y: f64, z: f64 },
}

enum What {
    Input(Input),
    Call(LuaFunction),
}

struct Entry {
    frame: u64,
    order: u64,
    what: What,
}

struct Action {
    name: Str,
    callback: LuaFunction,
    priority: i32,
    inputs: Vec<EnumItemId>,
    order: u64,
}

#[derive(Default)]
struct Inner {
    queue: Vec<Entry>,
    order: u64,
    delivering: Option<u64>,
    keys: Vec<EnumItemId>,
    buttons: Vec<EnumItemId>,
    pad_buttons: Vec<EnumItemId>,
    axes: Vec<(EnumItemId, [f64; 3])>,
    touches: Vec<(u32, (f64, f64))>,
    mouse: Option<(f64, f64)>,
    frame_delta: (f64, f64),
    last_type: Option<EnumItemId>,
    actions: Vec<Action>,
}

#[derive(Default)]
pub struct InputState(RefCell<Inner>);

impl crate::Extension for InputState {
    fn teardown(&self) {
        let mut inner = self.0.borrow_mut();
        inner.queue.clear();
        inner.actions.clear();
    }
}

impl InputState {
    fn push(&self, frame: u64, what: What) {
        let mut inner = self.0.borrow_mut();
        inner.order += 1;
        let order = inner.order;
        inner.queue.push(Entry { frame, order, what });
    }
}

fn state_of(engine: &Engine) -> &InputState {
    engine.extension_or_insert_with(InputState::default)
}

pub fn send(engine: &Engine, input: Input) {
    let state = state_of(engine);
    let frame = state.0.borrow().delivering.unwrap_or_else(|| engine.state.borrow().sched.frame + 1);
    state.push(frame, What::Input(input));
}

pub fn send_at(engine: &Engine, frame: u64, input: Input) {
    state_of(engine).push(frame, What::Input(input));
}

fn call_at(engine: &Engine, frame: u64, function: LuaFunction) {
    state_of(engine).push(frame, What::Call(function));
}

fn item(engine: &Engine, enum_name: &str, name: &str) -> Option<EnumItemId> {
    engine.reflection.enum_item(enum_name, name)
}

fn item_arg(engine: &Engine, value: &LuaValue, enum_name: &str) -> Option<EnumItemId> {
    match value {
        LuaValue::UserData(data) => {
            data.borrow::<LEnumItem>().ok().map(|found| found.0).filter(|found| engine.reflection.enum_item_type_name(*found) == enum_name)
        }
        LuaValue::String(text) => item(engine, enum_name, &text.to_string_lossy().to_string()),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Lock {
    Free,
    Center,
    Current,
}

fn lock_of(engine: &Engine) -> Lock {
    let game = engine.world().game;
    let Some(service) = services::find_service(engine, game, "UserInputService") else { return Lock::Free };
    let Some(name) = engine.reflection.name_id("MouseBehavior") else { return Lock::Free };
    let held = engine.state.borrow().dom.raw_get(service, name).and_then(Value::as_enum);
    match held {
        Some(found) if Some(found) == item(engine, "MouseBehavior", "LockCenter") => Lock::Center,
        Some(found) if Some(found) == item(engine, "MouseBehavior", "LockCurrentPosition") => Lock::Current,
        _ => Lock::Free,
    }
}

fn center(engine: &Engine) -> (f64, f64) {
    (engine.options.viewport_width / 2.0, engine.options.viewport_height / 2.0)
}

fn screen(engine: &Engine, inner: &Inner, lock: Lock) -> (f64, f64) {
    match lock {
        Lock::Center => center(engine),
        _ => inner.mouse.unwrap_or(MOUSE_UNKNOWN),
    }
}

struct Event {
    kind: EnumItemId,
    code: EnumItemId,
    step: Step,
    position: [f64; 3],
    delta: [f64; 3],
    touch: bool,
    last_changed: bool,
}

fn toggle(held: &mut Vec<EnumItemId>, code: EnumItemId, down: bool) -> bool {
    match (held.iter().position(|found| *found == code), down) {
        (None, true) => {
            held.push(code);
            true
        }
        (Some(index), false) => {
            held.remove(index);
            true
        }
        _ => false,
    }
}

fn edge(down: bool) -> Step {
    if down { Step::Begin } else { Step::End }
}

fn update(engine: &Engine, input: Input) -> Option<Event> {
    let lock = lock_of(engine);
    let inset = engine.options.topbar_inset;
    let unknown = item(engine, "KeyCode", "Unknown")?;
    let mut inner = state_of(engine).0.borrow_mut();
    let (kind, code, step, position, delta, touch) = match input {
        Input::Key { code, down } => {
            if !toggle(&mut inner.keys, code, down) {
                return None;
            }
            (item(engine, "UserInputType", "Keyboard")?, code, edge(down), [0.0; 3], [0.0; 3], false)
        }
        Input::MouseButton { button, down } => {
            if !toggle(&mut inner.buttons, button, down) {
                return None;
            }
            let (x, y) = screen(engine, &inner, lock);
            (button, unknown, edge(down), [x, y - inset, 0.0], [0.0; 3], false)
        }
        Input::MouseMove { x, y } => {
            let fresh = inner.mouse.is_none();
            let old = screen(engine, &inner, lock);
            let delta = if fresh && lock == Lock::Free { (0.0, 0.0) } else { (x - old.0, y - old.1) };
            if lock == Lock::Free {
                inner.mouse = Some((x, y));
            }
            if delta == (0.0, 0.0) && !fresh {
                return None;
            }
            inner.frame_delta = (inner.frame_delta.0 + delta.0, inner.frame_delta.1 + delta.1);
            let (px, py) = screen(engine, &inner, lock);
            (item(engine, "UserInputType", "MouseMovement")?, unknown, Step::Change, [px, py - inset, 0.0], [delta.0, delta.1, 0.0], false)
        }
        Input::MouseDelta { dx, dy } => {
            if (dx, dy) == (0.0, 0.0) {
                return None;
            }
            if lock == Lock::Free {
                let old = inner.mouse.unwrap_or(MOUSE_UNKNOWN);
                inner.mouse = Some((old.0 + dx, old.1 + dy));
            }
            inner.frame_delta = (inner.frame_delta.0 + dx, inner.frame_delta.1 + dy);
            let (px, py) = screen(engine, &inner, lock);
            (item(engine, "UserInputType", "MouseMovement")?, unknown, Step::Change, [px, py - inset, 0.0], [dx, dy, 0.0], false)
        }
        Input::Scroll { dy } => {
            let (px, py) = screen(engine, &inner, lock);
            (item(engine, "UserInputType", "MouseWheel")?, unknown, Step::Change, [px, py - inset, dy], [0.0; 3], false)
        }
        Input::Touch { id, x, y, step } => {
            let at = inner.touches.iter().position(|(found, _)| *found == id);
            let delta = match (step, at) {
                (Step::Begin, None) => {
                    inner.touches.push((id, (x, y)));
                    (0.0, 0.0)
                }
                (Step::Change, Some(index)) => {
                    let old = inner.touches[index].1;
                    inner.touches[index].1 = (x, y);
                    (x - old.0, y - old.1)
                }
                (Step::End, Some(index)) => {
                    let old = inner.touches.remove(index).1;
                    (x - old.0, y - old.1)
                }
                _ => return None,
            };
            (item(engine, "UserInputType", "Touch")?, unknown, step, [x, y - inset, 0.0], [delta.0, delta.1, 0.0], true)
        }
        Input::GamepadButton { code, down } => {
            if !toggle(&mut inner.pad_buttons, code, down) {
                return None;
            }
            (item(engine, "UserInputType", "Gamepad1")?, code, edge(down), [0.0; 3], [0.0; 3], false)
        }
        Input::Axis { code, x, y, z } => {
            let at = inner.axes.iter().position(|(found, _)| *found == code);
            let old = at.map(|index| inner.axes[index].1).unwrap_or([0.0; 3]);
            let new = [x, y, z];
            if old == new {
                return None;
            }
            match at {
                Some(index) => inner.axes[index].1 = new,
                None => inner.axes.push((code, new)),
            }
            let delta = [new[0] - old[0], new[1] - old[1], new[2] - old[2]];
            (item(engine, "UserInputType", "Gamepad1")?, code, Step::Change, new, delta, false)
        }
    };
    let last_changed = inner.last_type != Some(kind);
    inner.last_type = Some(kind);
    Some(Event { kind, code, step, position, delta, touch, last_changed })
}

fn step_item(engine: &Engine, step: Step) -> Option<EnumItemId> {
    item(engine, "UserInputState", match step {
        Step::Begin => "Begin",
        Step::Change => "Change",
        Step::End => "End",
    })
}

fn make_object(engine: &Engine, kind: EnumItemId, code: EnumItemId, step: EnumItemId, position: [f64; 3], delta: [f64; 3]) -> Option<InstanceId> {
    let reflection = &engine.reflection;
    let class = reflection.class_id("InputObject")?;
    let mut state = engine.state.borrow_mut();
    let id = core::create(&mut state, reflection, class);
    let vector = |v: [f64; 3]| Value::Vector3(dom::Vector3::new(v[0] as f32, v[1] as f32, v[2] as f32));
    for (name, value) in [
        ("KeyCode", Value::EnumItem(code)),
        ("UserInputType", Value::EnumItem(kind)),
        ("UserInputState", Value::EnumItem(step)),
        ("Position", vector(position)),
        ("Delta", vector(delta)),
    ] {
        if let Some(name) = reflection.name_id(name) {
            core::raw_set_silent(&mut state, reflection, id, name, value);
        }
    }
    Some(id)
}

fn signal_of(engine: &Engine, event: &str) -> Option<SignalId> {
    let game = engine.world().game;
    let service = services::find_service(engine, game, "UserInputService")?;
    let name = engine.reflection.name_id(event)?;
    engine.state.borrow().dom.at(service).signals.as_ref().and_then(|signals| signals.event(name))
}

fn fire(lua: &Lua, engine: &Rc<Engine>, event: &str, args: &[Value]) -> LuaResult<()> {
    if let Some(signal) = signal_of(engine, event) {
        fire_immediate_costing(lua, engine, signal, args, crate::cost::FRAME_HANDLER)?;
    }
    Ok(())
}

fn matching(engine: &Engine, event: &Event) -> Vec<(Str, LuaFunction)> {
    let inner = state_of(engine).0.borrow();
    let mut found: Vec<&Action> = inner.actions.iter().filter(|action| action.inputs.contains(&event.code) || action.inputs.contains(&event.kind)).collect();
    found.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.order.cmp(&a.order)));
    found.into_iter().map(|action| (action.name.clone(), action.callback.clone())).collect()
}

fn run_actions(lua: &Lua, engine: &Rc<Engine>, actions: Vec<(Str, LuaFunction)>, step: EnumItemId, object: InstanceId) -> LuaResult<bool> {
    let pass = item(engine, "ContextActionResult", "Pass");
    for (name, callback) in actions {
        let thread = lua.create_thread(callback)?;
        scheduler::charge_dispatch_to(lua, engine, &thread, crate::cost::FRAME_HANDLER, 0);
        let args = LuaMultiValue::from_iter([
            LuaValue::String(lua.create_string(name.as_bytes())?),
            enum_item_value(lua, engine, step)?,
            instance_value(lua, engine, object)?,
        ]);
        let Some(returned) = scheduler::resume_returning(lua, engine, &thread, args) else { continue };
        let passed = match returned.iter().next() {
            Some(LuaValue::UserData(data)) => data.borrow::<LEnumItem>().ok().is_some_and(|found| Some(found.0) == pass),
            _ => false,
        };
        if !passed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply(lua: &Lua, engine: &Rc<Engine>, input: Input) -> LuaResult<()> {
    let Some(event) = update(engine, input) else { return Ok(()) };
    if event.last_changed {
        fire(lua, engine, "LastInputTypeChanged", &[Value::EnumItem(event.kind)])?;
    }
    let (main, extra) = match event.step {
        Step::Begin => ("InputBegan", "TouchStarted"),
        Step::Change => ("InputChanged", "TouchMoved"),
        Step::End => ("InputEnded", "TouchEnded"),
    };
    let actions = matching(engine, &event);
    let heard = !actions.is_empty() || signal_of(engine, main).is_some() || (event.touch && signal_of(engine, extra).is_some());
    if !heard {
        return Ok(());
    }
    let Some(step) = step_item(engine, event.step) else { return Ok(()) };
    let Some(object) = make_object(engine, event.kind, event.code, step, event.position, event.delta) else { return Ok(()) };
    let processed = run_actions(lua, engine, actions, step, object)?;
    let args = [Value::Instance(object), Value::Bool(processed)];
    fire(lua, engine, main, &args)?;
    if event.touch {
        fire(lua, engine, extra, &args)?;
    }
    Ok(())
}

fn deliver(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<()> {
    if engine.options.server() {
        return Ok(());
    }
    let frame = engine.state.borrow().sched.frame;
    let input = state_of(engine);
    {
        let mut inner = input.0.borrow_mut();
        inner.frame_delta = (0.0, 0.0);
        inner.delivering = Some(frame);
    }
    let result = drain(lua, engine, input, frame);
    input.0.borrow_mut().delivering = None;
    result
}

fn drain(lua: &Lua, engine: &Rc<Engine>, input: &InputState, frame: u64) -> LuaResult<()> {
    for handled in 0usize.. {
        let next = {
            let mut inner = input.0.borrow_mut();
            let due = inner
                .queue
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.frame <= frame)
                .min_by_key(|(_, entry)| (entry.frame, entry.order))
                .map(|(index, _)| index);
            due.map(|index| inner.queue.remove(index))
        };
        let Some(entry) = next else { return Ok(()) };
        if handled >= MAX_PER_FRAME {
            engine.warn(format!("input: more than {MAX_PER_FRAME} inputs in frame {frame}, the rest wait for the next frame"));
            input.push(frame + 1, entry.what);
            return Ok(());
        }
        match entry.what {
            What::Input(input) => apply(lua, engine, input)?,
            What::Call(function) => {
                let thread = lua.create_thread(function)?;
                scheduler::resume(lua, engine, &thread, LuaMultiValue::new());
            }
        }
    }
    Ok(())
}

fn location(engine: &Engine) -> (f64, f64) {
    let lock = lock_of(engine);
    screen(engine, &state_of(engine).0.borrow(), lock)
}

fn vector2(lua: &Lua, engine: &Rc<Engine>, x: f64, y: f64) -> LuaResult<LuaValue> {
    convert::to_lua(lua, engine, &Value::Vector2(dom::Vector2::new(x as f32, y as f32)))
}

fn listing(lua: &Lua, engine: &Rc<Engine>, entries: Vec<(EnumItemId, EnumItemId, [f64; 3], &'static str)>) -> Ret {
    let table = lua.create_table()?;
    for (kind, code, position, step) in entries {
        let Some(step) = item(engine, "UserInputState", step) else { continue };
        if let Some(id) = make_object(engine, kind, code, step, position, [0.0; 3]) {
            table.raw_push(instance_value(lua, engine, id)?)?;
        }
    }
    Ok(values([LuaValue::Table(table)]))
}

fn bind(engine: &Engine, args: &Args, at_priority: bool) -> Result<(), Flow> {
    let Some(name) = args.string(1) else { return raise("Argument 1 missing or nil") };
    let LuaValue::Function(callback) = args.get(2).clone() else { return raise("Argument 2 missing or nil") };
    let (priority, first) = if at_priority {
        (args.number(4).unwrap_or(DEFAULT_ACTION_PRIORITY as f64) as i32, 5)
    } else {
        (DEFAULT_ACTION_PRIORITY, 4)
    };
    let inputs = (first..args.len())
        .filter_map(|index| item_arg(engine, args.get(index), "KeyCode").or_else(|| item_arg(engine, args.get(index), "UserInputType")))
        .collect();
    let name = Str::new(&name.as_bytes());
    let mut inner = state_of(engine).0.borrow_mut();
    inner.actions.retain(|action| action.name.as_bytes() != name.as_bytes());
    inner.order += 1;
    let order = inner.order;
    inner.actions.push(Action { name, callback, priority, inputs, order });
    Ok(())
}

pub fn install(engine: &Rc<Engine>) {
    let reflection = engine.reflection.clone();
    let device = engine.options.device.clone();
    let fallback = item(
        engine,
        "UserInputType",
        if device == "touch" || device == "phone" {
            "Touch"
        } else if device == "gamepad" {
            "Gamepad1"
        } else if engine.options.edit_mode() {
            "Focus"
        } else {
            "MouseMovement"
        },
    );
    {
        let mut behaviors = engine.behaviors.borrow_mut();
        let mut add = |class: crate::ClassId, name: &str, function: services::MethodFn| {
            if let Some(name) = reflection.name_id(name) {
                behaviors.add_method(class, name, function);
            }
        };
        if let Some(input) = reflection.class_id("UserInputService") {
            add(input, "IsKeyDown", services::method(|_, engine, _, args| {
                let down = item_arg(engine, args.get(1), "KeyCode").is_some_and(|code| state_of(engine).0.borrow().keys.contains(&code));
                Ok(values([LuaValue::Boolean(down)]))
            }));
            add(input, "IsMouseButtonPressed", services::method(|_, engine, _, args| {
                let down = item_arg(engine, args.get(1), "UserInputType").is_some_and(|button| state_of(engine).0.borrow().buttons.contains(&button));
                Ok(values([LuaValue::Boolean(down)]))
            }));
            add(input, "IsGamepadButtonDown", services::method(|_, engine, _, args| {
                let down = item_arg(engine, args.get(2), "KeyCode").is_some_and(|code| state_of(engine).0.borrow().pad_buttons.contains(&code));
                Ok(values([LuaValue::Boolean(down)]))
            }));
            add(input, "GetKeysPressed", services::method(|lua, engine, _, _| {
                let held = state_of(engine).0.borrow().keys.clone();
                let Some(kind) = item(engine, "UserInputType", "Keyboard") else { return listing(lua, engine, Vec::new()) };
                listing(lua, engine, held.into_iter().map(|code| (kind, code, [0.0; 3], "Begin")).collect())
            }));
            add(input, "GetMouseButtonsPressed", services::method(|lua, engine, _, _| {
                let held = state_of(engine).0.borrow().buttons.clone();
                let Some(unknown) = item(engine, "KeyCode", "Unknown") else { return listing(lua, engine, Vec::new()) };
                let (x, y) = location(engine);
                let y = y - engine.options.topbar_inset;
                listing(lua, engine, held.into_iter().map(|button| (button, unknown, [x, y, 0.0], "Begin")).collect())
            }));
            add(input, "GetGamepadState", services::method(|lua, engine, _, _| {
                let Some(kind) = item(engine, "UserInputType", "Gamepad1") else { return listing(lua, engine, Vec::new()) };
                let inner = state_of(engine).0.borrow();
                let mut entries: Vec<_> = inner.pad_buttons.iter().map(|code| (kind, *code, [0.0; 3], "Begin")).collect();
                entries.extend(inner.axes.iter().map(|(code, position)| (kind, *code, *position, "Change")));
                drop(inner);
                listing(lua, engine, entries)
            }));
            add(input, "GetMouseLocation", services::method(|lua, engine, _, _| {
                let (x, y) = location(engine);
                Ok(values([vector2(lua, engine, x, y)?]))
            }));
            add(input, "GetMouseDelta", services::method(|lua, engine, _, _| {
                let (x, y) = if lock_of(engine) == Lock::Free { (0.0, 0.0) } else { state_of(engine).0.borrow().frame_delta };
                Ok(values([vector2(lua, engine, x, y)?]))
            }));
            add(input, "GetLastInputType", services::method(move |lua, engine, _, _| {
                let last = state_of(engine).0.borrow().last_type.or(fallback);
                Ok(values([match last {
                    Some(found) => enum_item_value(lua, engine, found)?,
                    None => LuaValue::Nil,
                }]))
            }));
        }
        if let Some(actions) = reflection.class_id("ContextActionService") {
            add(actions, "BindAction", services::method(|_, engine, _, args| {
                bind(engine, args, false)?;
                Ok(none())
            }));
            add(actions, "BindActionAtPriority", services::method(|_, engine, _, args| {
                bind(engine, args, true)?;
                Ok(none())
            }));
            add(actions, "UnbindAction", services::method(|_, engine, _, args| {
                if let Some(name) = args.string(1) {
                    let name = name.as_bytes().to_vec();
                    state_of(engine).0.borrow_mut().actions.retain(|action| action.name.as_bytes() != name.as_slice());
                }
                Ok(none())
            }));
            add(actions, "UnbindAllActions", services::method(|_, engine, _, _| {
                state_of(engine).0.borrow_mut().actions.clear();
                Ok(none())
            }));
        }
    }
    engine.state.borrow_mut().sched.add_hook(Phase::Input, Rc::new(|lua, engine, _| deliver(lua, engine)));
}

fn button_arg(engine: &Engine, value: &LuaValue) -> Option<EnumItemId> {
    match value {
        LuaValue::Integer(number) => item(engine, "UserInputType", &format!("MouseButton{number}")),
        LuaValue::Number(number) => item(engine, "UserInputType", &format!("MouseButton{}", *number as i64)),
        other => item_arg(engine, other, "UserInputType"),
    }
}

fn stick_arg(engine: &Engine, value: &LuaValue) -> Option<EnumItemId> {
    match value {
        LuaValue::Integer(number) => item(engine, "KeyCode", &format!("Thumbstick{number}")),
        LuaValue::Number(number) => item(engine, "KeyCode", &format!("Thumbstick{}", *number as i64)),
        other => item_arg(engine, other, "KeyCode"),
    }
}

fn step_arg(engine: &Engine, value: &LuaValue) -> Option<Step> {
    let name = match value {
        LuaValue::String(text) => text.to_string_lossy().to_string(),
        other => engine.reflection.enum_item_info(item_arg(engine, other, "UserInputState")?).name.to_string(),
    };
    match name.as_str() {
        "Begin" => Some(Step::Begin),
        "Change" => Some(Step::Change),
        "End" => Some(Step::End),
        _ => None,
    }
}

/// `context.Input`: the harness's way to be the player. Each call queues for the next frame's input phase.
pub fn context_table(lua: &Lua, engine: &Rc<Engine>) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    let e = engine.clone();
    table.set("Key", lua.create_function(move |_, (code, down): (LuaValue, Option<bool>)| {
        let Some(code) = item_arg(&e, &code, "KeyCode") else { return Err(LuaError::runtime("Context.Input.Key: a KeyCode expected")) };
        send(&e, Input::Key { code, down: down.unwrap_or(true) });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("MouseMove", lua.create_function(move |_, (x, y): (f64, f64)| {
        send(&e, Input::MouseMove { x, y });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("MouseDelta", lua.create_function(move |_, (dx, dy): (f64, f64)| {
        send(&e, Input::MouseDelta { dx, dy });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("MouseButton", lua.create_function(move |_, (button, down): (LuaValue, Option<bool>)| {
        let Some(button) = button_arg(&e, &button) else { return Err(LuaError::runtime("Context.Input.MouseButton: a UserInputType (MouseButton1 to 3) or 1 to 3 expected")) };
        send(&e, Input::MouseButton { button, down: down.unwrap_or(true) });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("Scroll", lua.create_function(move |_, dy: f64| {
        send(&e, Input::Scroll { dy });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("Touch", lua.create_function(move |_, (id, x, y, step): (u32, f64, f64, LuaValue)| {
        let Some(step) = step_arg(&e, &step) else { return Err(LuaError::runtime("Context.Input.Touch: a UserInputState (Begin, Change, End) expected")) };
        send(&e, Input::Touch { id, x, y, step });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("Gamepad", lua.create_function(move |_, (code, down): (LuaValue, Option<bool>)| {
        let Some(code) = item_arg(&e, &code, "KeyCode") else { return Err(LuaError::runtime("Context.Input.Gamepad: a KeyCode (ButtonA ...) expected")) };
        send(&e, Input::GamepadButton { code, down: down.unwrap_or(true) });
        Ok(())
    })?)?;
    let e = engine.clone();
    table.set("Thumbstick", lua.create_function(move |_, (stick, x, y): (LuaValue, f64, f64)| {
        let Some(code) = stick_arg(&e, &stick) else { return Err(LuaError::runtime("Context.Input.Thumbstick: Thumbstick1, Thumbstick2, 1 or 2 expected")) };
        send(&e, Input::Axis { code, x, y, z: 0.0 });
        Ok(())
    })?)?;
    table.set("Text", lua.create_function(|_, _text: LuaValue| -> LuaResult<()> {
        Err(LuaError::runtime("Context.Input.Text: no TextBox can take focus yet (GUI input is M4)"))
    })?)?;
    let e = engine.clone();
    table.set("Frame", lua.create_function(move |_, ()| Ok(e.state.borrow().sched.frame))?)?;
    let e = engine.clone();
    table.set("At", lua.create_function(move |_, (frame, function): (f64, LuaFunction)| {
        call_at(&e, frame.max(0.0) as u64, function);
        Ok(())
    })?)?;
    Ok(table)
}

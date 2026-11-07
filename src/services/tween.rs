//! TweenService (port of services.luau InstallTweens, Ease, Lerpers, SmoothDamp). Every rule is
//! the Luau runtime's, measured in Studio (tween, easing and lerp probes; the stepper was checked frame by
//! frame against a model of Studio's tween steps).
//!
//! Easing (GetValue, bit-exact on a 26k-point Studio grid): alpha is a float32; <= 0 gives 0 and
//! >= 1 gives 1; Out is 1 - In(1 - t) and InOut the two halves, in float32. The In curves run in
//! double from the float32 alpha and round to float32, except Back and Circular (float32);
//! Exponential and Elastic take 10 * (t - 1) and Bounce 1 - t in float32.
//!
//! Interpolation by property type: floats and float-backed components lerp as
//! a + f32(f32(b - a) * t); doubles in double; integers and UDim offsets round the double lerp half
//! away from zero; booleans take the goal once the lerp of 0/1 is non-zero; CFrames slerp.

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rbx_dom_weak::types as dom;
use rustc_hash::FxHashMap;

use super::{Behaviors, method, values};
use crate::{
    Engine, InstanceId, NameId,
    instance as core,
    lua::{
        Flow, convert, instance::assign, instance::get, instance_value, none, raise_plain, typeof_value,
        types::LTweenInfo,
    },
    reflection::{EngineType, Reflection, ScriptType},
    scheduler::{Phase, f32r},
    value::Value,
};

const HALF_PI: f64 = std::f64::consts::PI / 2.0;

fn bounce_out(mut t: f64) -> f64 {
    if t < 1.0 / 2.75 {
        7.5625 * t * t
    } else if t < 2.0 / 2.75 {
        t -= 1.5 / 2.75;
        7.5625 * t * t + 0.75
    } else if t < 2.5 / 2.75 {
        t -= 2.25 / 2.75;
        7.5625 * t * t + 0.9375
    } else {
        t -= 2.625 / 2.75;
        7.5625 * t * t + 0.984375
    }
}

fn ease_in(style: &str, t: f64) -> f64 {
    let overshoot = f32r(1.70158);
    let overshoot_plus_one = f32r(overshoot + 1.0);
    match style {
        "Linear" => t,
        "Cubic" => f32r(t * t * t),
        "Quart" => f32r(t * t * t * t),
        "Quint" => f32r(t * t * t * t * t),
        "Sine" => f32r(1.0 - (t * HALF_PI).cos()),
        "Exponential" => f32r(2f64.powf(f32r(f32r(t - 1.0) * 10.0))),
        "Circular" => f32r(1.0 - f32r(f32r(1.0 - f32r(t * t)).sqrt())),
        "Back" => f32r(f32r(t * t) * f32r(f32r(overshoot_plus_one * t) - overshoot)),
        "Elastic" => f32r((13.0 * HALF_PI * t).sin() * 2f64.powf(f32r(f32r(t - 1.0) * 10.0))),
        "Bounce" => f32r(1.0 - bounce_out(f32r(1.0 - t))),
        // Quad, and unknown styles
        _ => f32r(t * t),
    }
}

/// Services.Ease (TweenService:GetValue for style / direction names).
pub fn ease(alpha: f64, style: &str, direction: &str) -> f64 {
    let t = f32r(alpha);
    if t <= 0.0 {
        return 0.0;
    } else if t >= 1.0 {
        return 1.0;
    }
    match direction {
        "In" => ease_in(style, t),
        "Out" => f32r(1.0 - ease_in(style, f32r(1.0 - t))),
        _ if t < 0.5 => f32r(ease_in(style, f32r(t * 2.0)) * 0.5),
        _ => f32r(1.0 - f32r(ease_in(style, f32r(f32r(1.0 - t) * 2.0)) * 0.5)),
    }
}

fn lerp_float(a: f64, b: f64, t: f64) -> f64 {
    let from = f32r(a);
    f32r(from + f32r(f32r(f32r(b) - from) * t))
}

fn round_half_away(value: f64) -> f64 {
    if value >= 0.0 { (value + 0.5).floor() } else { -(-value + 0.5).floor() }
}

fn lerp_offset(a: i32, b: i32, t: f64) -> i32 {
    round_half_away(a as f64 + (b as f64 - a as f64) * t) as i32
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Number,
    Boolean,
    Color3,
    Vector2,
    Vector3,
    UDim,
    UDim2,
    Rect,
    CFrame,
}

impl Kind {
    fn of(script_type: ScriptType, name: &str) -> Option<Self> {
        Some(match script_type {
            ScriptType::Number => Self::Number,
            ScriptType::Boolean => Self::Boolean,
            ScriptType::Color3 => Self::Color3,
            ScriptType::Vector2 => Self::Vector2,
            ScriptType::Vector3 => Self::Vector3,
            ScriptType::UDim => Self::UDim,
            ScriptType::UDim2 => Self::UDim2,
            ScriptType::Rect => Self::Rect,
            ScriptType::CFrame => Self::CFrame,
            _ => {
                let _ = name;
                return None;
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberKind {
    Double,
    Integer,
    Float,
}

struct Property {
    name: NameId,
    goal: LuaValue,
    kind: Kind,
    script_type_name: String,
    number: NumberKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TweenPhase {
    Delay,
    Play,
}

struct TweenData {
    target: InstanceId,
    properties: Vec<Property>,
    style: String,
    direction: String,
    time: f64,
    delay: f64,
    repeat_count: f64,
    reverses: bool,
    phase: Option<TweenPhase>,
    delay_elapsed: f64,
    elapsed: f64,
    iteration: f64,
    start: Option<Vec<LuaValue>>,
}

#[derive(Default)]
struct Tweens {
    data: FxHashMap<InstanceId, TweenData>,
    /// Playing, delayed and paused tweens (stepped from the end; swap-remove on finish)
    active: Vec<InstanceId>,
    /// (target, property) -> the tween that last took it with Play()
    owners: FxHashMap<(InstanceId, NameId), InstanceId>,
    /// Tweens that lost a property to a later Play(), cancelled before the next step if they own
    /// none of their properties any more
    displaced: Vec<InstanceId>,
}

type Shared = Rc<RefCell<Tweens>>;

fn lerp(_lua: &Lua, kind: Kind, number: NumberKind, a: &LuaValue, b: &LuaValue, t: f64) -> LuaResult<Value> {
    let (va, vb) = (convert::from_lua(a), convert::from_lua(b));
    Ok(match (kind, &va, &vb) {
        (Kind::Number, Value::Number(a), Value::Number(b)) => Value::Number(match number {
            NumberKind::Double => a + (b - a) * t,
            NumberKind::Integer => round_half_away(a + (b - a) * t),
            NumberKind::Float => lerp_float(*a, *b, t),
        }),
        (Kind::Boolean, Value::Bool(a), Value::Bool(b)) => {
            let (from, to) = (if *a { 1.0 } else { 0.0 }, if *b { 1.0 } else { 0.0 });
            Value::Bool(from + (to - from) * t != 0.0)
        }
        (Kind::Color3, Value::Color3(a), Value::Color3(b)) => Value::Color3(dom::Color3::new(
            lerp_float(a.r as f64, b.r as f64, t) as f32,
            lerp_float(a.g as f64, b.g as f64, t) as f32,
            lerp_float(a.b as f64, b.b as f64, t) as f32,
        )),
        (Kind::Vector2, Value::Vector2(a), Value::Vector2(b)) => Value::Vector2(dom::Vector2::new(
            lerp_float(a.x as f64, b.x as f64, t) as f32,
            lerp_float(a.y as f64, b.y as f64, t) as f32,
        )),
        (Kind::Vector3, Value::Vector3(a), Value::Vector3(b)) => Value::Vector3(dom::Vector3::new(
            lerp_float(a.x as f64, b.x as f64, t) as f32,
            lerp_float(a.y as f64, b.y as f64, t) as f32,
            lerp_float(a.z as f64, b.z as f64, t) as f32,
        )),
        (Kind::UDim, Value::UDim(a), Value::UDim(b)) => Value::UDim(dom::UDim::new(
            lerp_float(a.scale as f64, b.scale as f64, t) as f32,
            lerp_offset(a.offset, b.offset, t),
        )),
        (Kind::UDim2, Value::UDim2(a), Value::UDim2(b)) => Value::UDim2(dom::UDim2::new(
            dom::UDim::new(lerp_float(a.x.scale as f64, b.x.scale as f64, t) as f32, lerp_offset(a.x.offset, b.x.offset, t)),
            dom::UDim::new(lerp_float(a.y.scale as f64, b.y.scale as f64, t) as f32, lerp_offset(a.y.offset, b.y.offset, t)),
        )),
        (Kind::Rect, Value::Rect(a), Value::Rect(b)) => Value::Rect(dom::Rect::new(
            dom::Vector2::new(lerp_float(a.min.x as f64, b.min.x as f64, t) as f32, lerp_float(a.min.y as f64, b.min.y as f64, t) as f32),
            dom::Vector2::new(lerp_float(a.max.x as f64, b.max.x as f64, t) as f32, lerp_float(a.max.y as f64, b.max.y as f64, t) as f32),
        )),
        (Kind::CFrame, _, _) => match a {
            LuaValue::UserData(ud) => convert::from_lua(&ud.call_method::<LuaValue>("Lerp", (b.clone(), t))?),
            _ => vb.clone(),
        },
        _ => vb.clone(),
    })
}

fn tween_of(args: &super::Args) -> Result<InstanceId, Flow> {
    args.instance(0).ok_or_else(|| Flow::Error(crate::ScriptError::plain("Expected ':' not '.' calling member function")))
}

fn state_name(engine: &Engine, tween: InstanceId) -> String {
    let playback = engine.reflection.name_id("PlaybackState");
    let item = playback.and_then(|p| engine.state.borrow().dom.raw_get(tween, p).and_then(Value::as_enum));
    item.map_or_else(|| "Begin".to_string(), |item| engine.reflection.enum_item_info(item).name.to_string())
}

fn set_state(engine: &Engine, tween: InstanceId, name: &str) {
    let (Some(playback), Some(item)) = (engine.reflection.name_id("PlaybackState"), engine.reflection.enum_item("PlaybackState", name)) else {
        return;
    };
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    core::raw_set(&mut state, &engine.reflection, &hooks, tween, playback, Value::EnumItem(item));
}

/// PlaybackState changes first, then Completed (both deferred events).
fn finish(engine: &Engine, tweens: &Shared, tween: InstanceId, name: &str) {
    set_state(engine, tween, name);
    if let (Some(completed), Some(item)) = (engine.reflection.name_id("Completed"), engine.reflection.enum_item("PlaybackState", name)) {
        core::fire_event(&mut engine.state.borrow_mut(), tween, completed, vec![Value::EnumItem(item)]);
    }
    let mut shared = tweens.borrow_mut();
    if let Some(index) = shared.active.iter().position(|t| *t == tween) {
        shared.active.swap_remove(index);
        engine.state.borrow_mut().dom.unpin(tween);
    }
}

fn owns(shared: &Tweens, tween: InstanceId, data: &TweenData, name: NameId) -> bool {
    shared.owners.get(&(data.target, name)) == Some(&tween)
}

fn cast_name(engine: &Engine, value: &LuaValue) -> String {
    match typeof_value(engine, value).as_str() {
        "number" => "double".into(),
        "boolean" => "bool".into(),
        other => other.into(),
    }
}

fn create(lua: &Lua, engine: &Rc<Engine>, tweens: &Shared, args: &super::Args) -> Result<LuaMultiValue, Flow> {
    let target = args.get(1);
    let info = args.get(2);
    let goals = args.get(3);
    let target = match target {
        LuaValue::Nil => return raise_plain("TweenService:Create failed because Instance is null"),
        LuaValue::UserData(ud) if crate::lua::raw::instance_of_ud(ud).is_some() => {
            crate::lua::raw::instance_of_ud(ud).unwrap()
        }
        _ => return raise_plain("Unable to cast value to Object"),
    };
    let info_ud = match info {
        LuaValue::Nil => return raise_plain("Argument 2 missing or nil"),
        LuaValue::UserData(ud) if ud.is::<LTweenInfo>() => ud.clone(),
        other => return raise_plain(format!("Unable to cast {} to TweenInfo", cast_name(engine, other))),
    };
    let LuaValue::Table(goals) = goals else {
        return raise_plain(format!("Unable to cast {} to Dictionary", cast_name(engine, goals)));
    };
    let reflection: &Reflection = &engine.reflection;
    let class = engine.state.borrow().dom.at(target).class;
    let object_name = engine.state.borrow().dom.at(target).name.to_string();
    let mut properties = Vec::new();
    for pair in goals.pairs::<LuaValue, LuaValue>() {
        let (key, goal) = pair?;
        let key_text = match &key {
            LuaValue::String(s) => s.to_string_lossy(),
            other => lua.coerce_string(other.clone())?.map(|s| s.to_string_lossy()).unwrap_or_else(|| other.type_name().into()),
        };
        let info = match &key {
            LuaValue::String(s) => reflection.name_id_bytes(&s.as_bytes()).and_then(|n| reflection.class_property(class, n)),
            _ => None,
        };
        let Some(info) = info else {
            return raise_plain(format!("TweenService:Create no property named '{key_text}' for object '{object_name}'"));
        };
        if info.read_only {
            return raise_plain(format!("TweenService:Create property named '{key_text}' on object '{object_name}' is read only"));
        }
        let Some(kind) = Kind::of(info.script_type, &info.script_type_name) else {
            return raise_plain(format!(
                "TweenService:Create property named '{key_text}' on object '{object_name}' is not a data type that can be tweened"
            ));
        };
        let goal_type = typeof_value(engine, &goal);
        if goal_type != &*info.script_type_name {
            return raise_plain(format!(
                "TweenService:Create property named '{key_text}' cannot be tweened due to type mismatch (property is a '{}', but given type is '{goal_type}')",
                info.engine_type_name
            ));
        }
        let number = match (&*info.engine_type_name, info.engine_type) {
            ("double", _) => NumberKind::Double,
            (_, EngineType::Integer) => NumberKind::Integer,
            _ => NumberKind::Float,
        };
        properties.push(Property { name: info.name, goal, kind, script_type_name: info.script_type_name.to_string(), number });
    }
    let data = {
        let info = info_ud.borrow::<LTweenInfo>()?;
        TweenData {
            target,
            properties,
            style: reflection.enum_item_info(info.easing_style).name.to_string(),
            direction: reflection.enum_item_info(info.easing_direction).name.to_string(),
            time: f32r(info.time()),
            delay: f32r(info.delay()),
            repeat_count: info.repeats(),
            reverses: info.reverses,
            phase: None,
            delay_elapsed: 0.0,
            elapsed: 0.0,
            iteration: 0.0,
            start: None,
        }
    };
    let tween = {
        let mut state = engine.state.borrow_mut();
        let class = reflection.class_id("Tween").expect("Tween class");
        let tween = core::create(&mut state, reflection, class);
        if let Some(name) = reflection.name_id("Instance") {
            core::raw_set_silent(&mut state, reflection, tween, name, Value::Instance(target));
        }
        if let Some(name) = reflection.name_id("TweenInfo") {
            core::raw_set_silent(&mut state, reflection, tween, name, Value::Lua(Box::new(LuaValue::UserData(info_ud.clone()))));
        }
        if let (Some(name), Some(item)) = (reflection.name_id("PlaybackState"), reflection.enum_item("PlaybackState", "Begin")) {
            core::raw_set_silent(&mut state, reflection, tween, name, Value::EnumItem(item));
        }
        tween
    };
    // A tween keeps its target alive (lua/raw.rs collection) until the tween itself is freed
    engine.state.borrow_mut().dom.pin(data.target);
    tweens.borrow_mut().data.insert(tween, data);
    Ok(values([instance_value(lua, engine, tween)?]))
}

/// Captures the start values (read when the playing phase begins).
fn capture(lua: &Lua, engine: &Rc<Engine>, tweens: &Shared, tween: InstanceId) -> LuaResult<()> {
    let (target, entries): (InstanceId, Vec<(NameId, String, LuaValue)>) = {
        let shared = tweens.borrow();
        let data = &shared.data[&tween];
        (data.target, data.properties.iter().map(|p| (p.name, p.script_type_name.clone(), p.goal.clone())).collect())
    };
    let mut start = Vec::new();
    for (name, kind, goal) in entries {
        let key = LuaValue::String(lua.create_string(engine.reflection.name(name))?);
        let value = match get(lua, engine, target, &key) {
            Ok(value) if typeof_value(engine, &value) == kind => value,
            _ => goal,
        };
        start.push(value);
    }
    tweens.borrow_mut().data.get_mut(&tween).unwrap().start = Some(start);
    Ok(())
}

enum End {
    Goal,
    Start,
}

/// Writes the owned properties at `alpha`, or at the end of an iteration. Destroyed targets keep
/// being written, like Studio's.
fn write(lua: &Lua, engine: &Rc<Engine>, tweens: &Shared, tween: InstanceId, owned: &[usize], alpha: Option<f64>, end: Option<End>) -> LuaResult<()> {
    let (target, eased, entries) = {
        let shared = tweens.borrow();
        let data = &shared.data[&tween];
        let eased = match end {
            Some(End::Goal) => 1.0,
            Some(End::Start) => 0.0,
            None => ease(alpha.unwrap_or(0.0), &data.style, &data.direction),
        };
        let start = data.start.clone().unwrap_or_default();
        let entries: Vec<(NameId, Kind, NumberKind, LuaValue, LuaValue)> = owned
            .iter()
            .map(|&index| {
                let p = &data.properties[index];
                (p.name, p.kind, p.number, start.get(index).cloned().unwrap_or(LuaValue::Nil), p.goal.clone())
            })
            .collect();
        (data.target, eased, entries)
    };
    for (name, kind, number, from, goal) in entries {
        let value = lerp(lua, kind, number, &from, &goal, eased)?;
        let value = convert::to_lua(lua, engine, &value)?;
        let key = LuaValue::String(lua.create_string(engine.reflection.name(name))?);
        match assign(lua, engine, target, &key, &value) {
            Ok(()) => {}
            Err(Flow::Error(error)) => engine.warn(format!("Tween: {}", error.message)),
            Err(Flow::Fail(error)) => engine.warn(format!("Tween: {}", crate::scheduler::error_message(&error))),
            Err(Flow::Yield) => {}
        }
    }
    Ok(())
}

fn end_iteration(engine: &Engine, tweens: &Shared, tween: InstanceId) {
    let (repeat, delayed) = {
        let mut shared = tweens.borrow_mut();
        let data = shared.data.get_mut(&tween).unwrap();
        data.iteration += 1.0;
        let repeat = data.repeat_count < 0.0 || data.iteration <= data.repeat_count;
        let mut delayed = false;
        if repeat {
            data.elapsed = 0.0;
            if data.delay > 0.0 {
                data.phase = Some(TweenPhase::Delay);
                data.delay_elapsed = 0.0;
                delayed = true;
            }
        }
        (repeat, delayed)
    };
    if !repeat {
        finish(engine, tweens, tween, "Completed");
    } else if delayed {
        set_state(engine, tween, "Delayed");
    }
}

fn step_tween(lua: &Lua, engine: &Rc<Engine>, tweens: &Shared, tween: InstanceId, delta: f64) -> LuaResult<()> {
    let (owned, count) = {
        let shared = tweens.borrow();
        let Some(data) = shared.data.get(&tween) else { return Ok(()) };
        let owned: Vec<usize> = (0..data.properties.len()).filter(|&i| owns(&shared, tween, data, data.properties[i].name)).collect();
        (owned, data.properties.len())
    };
    if owned.is_empty() && count > 0 {
        finish(engine, tweens, tween, "Cancelled");
        return Ok(());
    }
    let current = state_name(engine, tween);
    if current == "Paused" {
        return Ok(());
    }
    // The delay's leftover time is dropped; this step writes nothing
    let in_delay = tweens.borrow().data[&tween].phase == Some(TweenPhase::Delay);
    if in_delay {
        let (done, needs_start) = {
            let mut shared = tweens.borrow_mut();
            let data = shared.data.get_mut(&tween).unwrap();
            data.delay_elapsed = f32r(data.delay_elapsed + delta);
            let done = data.delay_elapsed >= data.delay;
            if done {
                data.phase = Some(TweenPhase::Play);
                data.elapsed = 0.0;
            }
            (done, data.start.is_none())
        };
        if done {
            if needs_start {
                capture(lua, engine, tweens, tween)?;
            }
            if current == "Delayed" {
                set_state(engine, tween, "Playing");
            }
        }
        return Ok(());
    }
    if tweens.borrow().data[&tween].start.is_none() {
        capture(lua, engine, tweens, tween)?;
    }
    let (elapsed, time, reverses) = {
        let mut shared = tweens.borrow_mut();
        let data = shared.data.get_mut(&tween).unwrap();
        data.elapsed = f32r(data.elapsed + delta);
        (data.elapsed, data.time, data.reverses)
    };
    if reverses {
        // Time carries through the turn: the way back is the same curve traced backwards
        if time <= 0.0 || elapsed >= f32r(time * 2.0) {
            write(lua, engine, tweens, tween, &owned, None, Some(End::Start))?;
            end_iteration(engine, tweens, tween);
        } else if elapsed >= time {
            write(lua, engine, tweens, tween, &owned, Some(f32r(2.0 - f32r(elapsed / time))), None)?;
        } else {
            write(lua, engine, tweens, tween, &owned, Some(f32r(elapsed / time)), None)?;
        }
    } else if time <= 0.0 || elapsed >= time {
        write(lua, engine, tweens, tween, &owned, None, Some(End::Goal))?;
        end_iteration(engine, tweens, tween);
    } else {
        write(lua, engine, tweens, tween, &owned, Some(f32r(elapsed / time)), None)?;
    }
    Ok(())
}

/// Once per render frame (the Tween phase): every tween's elapsed time grows by the frame's dt.
fn step_tweens(lua: &Lua, engine: &Rc<Engine>, tweens: &Shared, render_delta: f64) -> LuaResult<()> {
    let delta = f32r(render_delta);
    let displaced = std::mem::take(&mut tweens.borrow_mut().displaced);
    for tween in displaced {
        let cancel = {
            let shared = tweens.borrow();
            let active = shared.active.contains(&tween);
            let data = &shared.data[&tween];
            let owns_any = data.properties.is_empty() || data.properties.iter().any(|p| owns(&shared, tween, data, p.name));
            active && !owns_any
        };
        if cancel {
            finish(engine, tweens, tween, "Cancelled");
        }
    }
    let mut index = tweens.borrow().active.len() as i64;
    while index >= 1 {
        let len = tweens.borrow().active.len() as i64;
        if index > len {
            index = len;
            continue;
        }
        let tween = tweens.borrow().active[(index - 1) as usize];
        index -= 1;
        step_tween(lua, engine, tweens, tween, delta)?;
    }
    Ok(())
}

/// TweenService:SmoothDamp's decay (Unity's critically damped smoothing in float32).
fn smooth_damp_decay(smooth_time: f64, dt: f64) -> (f64, f64) {
    let omega = f32r(2.0 / smooth_time);
    let x = f32r(omega * dt);
    let polynomial = f32r(f32r(f32r(f32r(f32r(f32r(0.235) * x) + f32r(0.48)) * x) + 1.0) * x) + 1.0;
    (omega, f32r(1.0 / f32r(polynomial)))
}

fn smooth_damp_number(current: f64, target: f64, velocity: f64, smooth_time: f64, max_speed: f64, dt: f64) -> (f64, f64) {
    let (current, target, velocity, smooth_time, max_speed, dt) =
        (f32r(current), f32r(target), f32r(velocity), f32r(smooth_time), f32r(max_speed), f32r(dt));
    if smooth_time <= 0.0 {
        return (target, 0.0);
    }
    let (omega, decay) = smooth_damp_decay(smooth_time, dt);
    let max_change = f32r(max_speed * smooth_time);
    let change = f32r(current - target).clamp(-max_change, max_change);
    let moved = f32r(current - change);
    let temp = f32r(f32r(velocity + f32r(omega * change)) * dt);
    let mut new_velocity = f32r(f32r(velocity - f32r(omega * temp)) * decay);
    let mut output = f32r(moved + f32r(f32r(change + temp) * decay));
    if (target - current > 0.0) == (output > target) {
        output = target;
        new_velocity = 0.0;
    }
    (output, new_velocity)
}

fn smooth_damp_vector(current: &[f64], target: &[f64], velocity: &[f64], smooth_time: f64, max_speed: f64, dt: f64) -> (Vec<f64>, Vec<f64>) {
    let count = current.len();
    let (smooth_time, max_speed, dt) = (f32r(smooth_time), f32r(max_speed), f32r(dt));
    if smooth_time <= 0.0 {
        return (target.to_vec(), vec![0.0; count]);
    }
    let (omega, decay) = smooth_damp_decay(smooth_time, dt);
    let mut change: Vec<f64> = (0..count).map(|i| f32r(f32r(current[i]) - f32r(target[i]))).collect();
    let length: f64 = change.iter().map(|c| c * c).sum();
    let max_change = f32r(max_speed * smooth_time);
    if length > max_change * max_change {
        let scale = max_change / length.sqrt();
        for c in &mut change {
            *c = f32r(*c * scale);
        }
    }
    let mut output = vec![0.0; count];
    let mut new_velocity = vec![0.0; count];
    let mut overshoot = 0.0;
    for i in 0..count {
        let moved = f32r(f32r(current[i]) - change[i]);
        let temp = f32r(f32r(f32r(velocity[i]) + f32r(omega * change[i])) * dt);
        new_velocity[i] = f32r(f32r(f32r(velocity[i]) - f32r(omega * temp)) * decay);
        output[i] = f32r(moved + f32r(f32r(change[i] + temp) * decay));
        overshoot += (target[i] - current[i]) * (output[i] - target[i]);
    }
    if overshoot > 0.0 {
        for i in 0..count {
            output[i] = f32r(target[i]);
            new_velocity[i] = 0.0;
        }
    }
    (output, new_velocity)
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let (Some(service), Some(tween_class)) = (reflection.class_id("TweenService"), reflection.class_id("Tween")) else {
        return;
    };
    let tweens: Shared = Rc::default();
    let n = |name: &str| reflection.name_id(name);
    // A freed (unreachable, inactive) tween releases its target
    let shared = tweens.clone();
    engine.hooks.borrow_mut().instance_freed.push(Box::new(move |state, id| {
        let mut tweens = shared.borrow_mut();
        if let Some(data) = tweens.data.remove(&id) {
            state.dom.unpin(data.target);
            tweens.owners.retain(|_, owner| *owner != id);
            tweens.displaced.retain(|tween| *tween != id);
        }
    }));

    if let Some(name) = n("Create") {
        let shared = tweens.clone();
        behaviors.add_method(service, name, method(move |lua, engine, _, args| create(lua, engine, &shared, args)));
    }
    if let Some(name) = n("GetValue") {
        behaviors.add_method(service, name, method(|lua, engine, _, args| {
            let style = engine.reflection.enum_id("EasingStyle").and_then(|id| convert::coerce_enum(lua, engine, id, args.get(2)));
            let direction = engine.reflection.enum_id("EasingDirection").and_then(|id| convert::coerce_enum(lua, engine, id, args.get(3)));
            let Some(alpha) = args.number(1) else { return raise_plain("Argument 1 missing or nil") };
            let Some(style) = style else { return raise_plain("Unable to cast value to EasingStyle") };
            let Some(direction) = direction else { return raise_plain("Unable to cast value to EasingDirection") };
            let reflection = &engine.reflection;
            let value = ease(alpha, &reflection.enum_item_info(style).name, &reflection.enum_item_info(direction).name);
            Ok(values([LuaValue::Number(value)]))
        }));
    }
    if let Some(name) = n("SmoothDamp") {
        behaviors.add_method(service, name, method(|lua, engine, _, args| {
            let speed = args.number(5).unwrap_or(f64::INFINITY);
            let delta = args.number(6).unwrap_or_else(|| engine.state.borrow().sched.frame_duration);
            let smooth_time = args.number(4).unwrap_or(0.0);
            let (current, target, velocity) = (convert::from_lua(args.get(1)), convert::from_lua(args.get(2)), convert::from_lua(args.get(3)));
            let parts = |v: &Value| -> Option<Vec<f64>> {
                match v {
                    Value::Vector2(v) => Some(vec![v.x as f64, v.y as f64]),
                    Value::Vector3(v) => Some(vec![v.x as f64, v.y as f64, v.z as f64]),
                    _ => None,
                }
            };
            match &current {
                Value::Number(c) => {
                    let (output, speed_out) = smooth_damp_number(*c, target.as_number().unwrap_or(0.0), velocity.as_number().unwrap_or(0.0), smooth_time, speed, delta);
                    Ok(values([LuaValue::Number(output), LuaValue::Number(speed_out)]))
                }
                Value::Vector2(_) | Value::Vector3(_) => {
                    let c = parts(&current).unwrap();
                    let t = parts(&target).unwrap_or_else(|| vec![0.0; c.len()]);
                    let v = parts(&velocity).unwrap_or_else(|| vec![0.0; c.len()]);
                    let (moved, speeds) = smooth_damp_vector(&c, &t, &v, smooth_time, speed, delta);
                    let make = |p: &[f64]| -> Value {
                        if p.len() == 2 {
                            Value::Vector2(dom::Vector2::new(p[0] as f32, p[1] as f32))
                        } else {
                            Value::Vector3(dom::Vector3::new(p[0] as f32, p[1] as f32, p[2] as f32))
                        }
                    };
                    Ok(values([convert::to_lua(lua, engine, &make(&moved))?, convert::to_lua(lua, engine, &make(&speeds))?]))
                }
                _ => raise_plain(format!("SmoothDamp does not support {}", typeof_value(engine, args.get(1)))),
            }
        }));
    }
    // Play from Begin reports Delayed when there is a DelayTime; from Completed / Cancelled it
    // reports Playing even though the delay still runs (measured). Play on a paused tween resumes.
    if let Some(name) = n("Play") {
        let shared = tweens.clone();
        behaviors.add_method(tween_class, name, method(move |_, engine, _, args| {
            let tween = tween_of(args)?;
            if !shared.borrow().data.contains_key(&tween) {
                return Ok(none());
            }
            let current = state_name(engine, tween);
            if current == "Playing" || current == "Delayed" {
                return Ok(none());
            } else if current == "Paused" {
                set_state(engine, tween, "Playing");
                return Ok(none());
            }
            let delay = shared.borrow().data[&tween].delay;
            set_state(engine, tween, if current == "Begin" && delay > 0.0 { "Delayed" } else { "Playing" });
            let mut guard = shared.borrow_mut();
            let tweens = &mut *guard;
            let data = tweens.data.get_mut(&tween).unwrap();
            data.phase = Some(if delay > 0.0 { TweenPhase::Delay } else { TweenPhase::Play });
            data.delay_elapsed = 0.0;
            data.elapsed = 0.0;
            data.iteration = 0.0;
            data.start = None;
            let target = data.target;
            let names: Vec<NameId> = data.properties.iter().map(|p| p.name).collect();
            for name in names {
                if let Some(previous) = tweens.owners.insert((target, name), tween) {
                    if previous != tween && !tweens.displaced.contains(&previous) {
                        tweens.displaced.push(previous);
                    }
                }
            }
            if !tweens.active.contains(&tween) {
                tweens.active.push(tween);
                // A playing tween stays alive without script references
                engine.state.borrow_mut().dom.pin(tween);
            }
            Ok(none())
        }));
    }
    // Only a playing tween pauses (Pause during Delayed or Begin does nothing)
    if let Some(name) = n("Pause") {
        behaviors.add_method(tween_class, name, method(|_, engine, _, args| {
            let tween = tween_of(args)?;
            if state_name(engine, tween) == "Playing" {
                set_state(engine, tween, "Paused");
            }
            Ok(none())
        }));
    }
    // Cancel leaves the properties where they are; Cancel on an idle tween does nothing
    if let Some(name) = n("Cancel") {
        let shared = tweens.clone();
        behaviors.add_method(tween_class, name, method(move |_, engine, _, args| {
            let tween = tween_of(args)?;
            let current = state_name(engine, tween);
            if matches!(current.as_str(), "Playing" | "Delayed" | "Paused") {
                finish(engine, &shared, tween, "Cancelled");
            }
            Ok(none())
        }));
    }
    let shared = tweens;
    engine.state.borrow_mut().sched.add_hook(Phase::TweenStep, Rc::new(move |lua, engine, delta| step_tweens(lua, engine, &shared, delta)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_matches_the_luau_curves() {
        assert_eq!(ease(0.0, "Quad", "Out"), 0.0);
        assert_eq!(ease(1.0, "Quad", "Out"), 1.0);
        assert_eq!(ease(0.5, "Linear", "In"), 0.5);
        assert_eq!(ease(0.5, "Quad", "Out"), 0.75);
        assert_eq!(ease(0.25, "Quad", "InOut"), 0.125);
        assert_eq!(round_half_away(-2.5), -3.0);
        assert_eq!(lerp_offset(0, 5, 0.5), 3);
    }
}

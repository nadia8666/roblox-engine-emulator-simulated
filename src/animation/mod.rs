//! AnimationTracks: Animator:LoadAnimation (and the Humanoid's and AnimationController's), the
//! track's playback members and events, and the Animator's step, which writes the blended pose into
//! the rig's Motor6D.Transform and Bone.Transform each frame. Roblox's documentation, not measured
//! in Studio (docs/engine.md "Animation" lists the assumptions):
//!
//! - The step runs after RunService.PreAnimation and before Stepped (the task scheduler's order), in
//!   play mode only (an Animator does not run in edit mode); the Motor6D batch after PreSimulation
//!   then poses the parts in the same frame.
//! - A track's weight moves linearly from WeightCurrent to WeightTarget over the fade time (Play,
//!   Stop, AdjustWeight). Per joint, priorities from the highest down each take their tracks' summed
//!   weight (at most 1) of what is left; tracks of one priority blend by their weights; whatever no
//!   track takes stays at the rest pose (Transform identity).
//! - Stop() makes IsPlaying false and fires Stopped; Ended fires once the fade-out reached weight 0.
//!   A non-looped track reaching its end does the same with a 0.1 s fade-out, holding its last pose.
//!   A looped one fires DidLoop on each wrap. KeyframeReached (keyframes not named "Keyframe") and
//!   GetMarkerReachedSignal(name) (KeyframeMarkers, with their Value) fire for the keyframes the
//!   step passed, the start included on the first step after Play. Events are deferred.
//! - Translations scale with the rig model's Scale (Model:ScaleTo scales the animations it plays).

pub mod clip;
pub mod source;
#[cfg(test)]
mod tests;

use std::{cell::RefCell, rc::Rc, sync::Arc};

use glam::{Affine3A, Quat, Vec3A, Vec4};
use mlua::prelude::*;
use rustc_hash::FxHashMap;

use crate::{
    ClassId, Engine, InstanceId, NameId, Reflection, ScriptError, State,
    instance as core,
    lua::{Args, Flow, instance_value, none, signal_value, values},
    services::{Behaviors, method},
    signal::SignalId,
    value::{Str, Value},
};
use clip::{Clip, priority_rank};
pub use source::{Loading, enable};

/// Roblox's default fade time (Play, Stop, AdjustWeight): 0.100000001.
pub const DEFAULT_FADE: f64 = 0.1;

struct Track {
    animator: InstanceId,
    clip: Option<Arc<Clip>>,
    /// IsPlaying
    playing: bool,
    /// Stepped and posing: playing, or fading out after it stopped
    active: bool,
    /// TimePosition as the step last wrote it (a different stored value was set by a script)
    time: f64,
    speed: f64,
    weight: f64,
    weight_from: f64,
    weight_target: f64,
    fade: f64,
    fade_elapsed: f64,
    /// The first step since Play: keyframes at the start time fire
    fresh: bool,
    /// When it was last played (GetPlayingAnimationTracks' order)
    order: u64,
    markers: Vec<(String, SignalId)>,
    /// Its asset on the way: when it arrives and the clip it holds (until then Length 0, no pose)
    pending: Option<(f64, Arc<Clip>)>,
    /// Looped / Priority written by a script (the asset's own do not replace them when it arrives)
    set_looped: bool,
    set_priority: bool,
}

#[derive(Default)]
struct Animations {
    tracks: FxHashMap<InstanceId, Track>,
    /// Active tracks in the order they were played
    active: Vec<InstanceId>,
    /// Tracks whose asset is on the way
    pending: Vec<InstanceId>,
    /// The joints each animator posed last step (put back at rest when no track poses them)
    posed: FxHashMap<InstanceId, Vec<InstanceId>>,
    /// Each animator's rig maps with the model and the version they were built for (dom.watch_rig): rebuilt only when
    /// something inside the model changed, not every step
    rigs: FxHashMap<InstanceId, (InstanceId, u32, Rc<Rig>)>,
    played: u64,
}

type Shared = Rc<RefCell<Animations>>;

/// Names and classes resolved once.
struct Names {
    animation: NameId,
    animation_id: NameId,
    animation_content: NameId,
    is_playing: NameId,
    length: NameId,
    looped: NameId,
    priority: NameId,
    speed: NameId,
    time_position: NameId,
    weight_current: NameId,
    weight_target: NameId,
    transform: NameId,
    part0: NameId,
    part1: NameId,
    scale: NameId,
    did_loop: NameId,
    ended: NameId,
    keyframe_reached: NameId,
    stopped: NameId,
    animation_played: NameId,
    track_class: ClassId,
    animator_class: ClassId,
    animation_class: ClassId,
    motor6d: ClassId,
    bone: ClassId,
    model: ClassId,
    humanoid: ClassId,
    animation_controller: ClassId,
    /// Enum.AnimationPriority items by value
    priorities: Vec<(u32, crate::reflection::EnumItemId)>,
}

impl Names {
    fn new(reflection: &Reflection) -> Option<Names> {
        let n = |name: &str| reflection.name_id(name);
        let c = |name: &str| reflection.class_id(name);
        let priorities = ["Idle", "Movement", "Action", "Action2", "Action3", "Action4", "Core"]
            .iter()
            .filter_map(|name| reflection.enum_item("AnimationPriority", name))
            .map(|item| (reflection.enum_item_info(item).value as u32, item))
            .collect();
        Some(Names {
            animation: n("Animation")?,
            animation_id: n("AnimationId")?,
            animation_content: n("AnimationContent")?,
            is_playing: n("IsPlaying")?,
            length: n("Length")?,
            looped: n("Looped")?,
            priority: n("Priority")?,
            speed: n("Speed")?,
            time_position: n("TimePosition")?,
            weight_current: n("WeightCurrent")?,
            weight_target: n("WeightTarget")?,
            transform: n("Transform")?,
            part0: n("Part0")?,
            part1: n("Part1")?,
            scale: n("Scale")?,
            did_loop: n("DidLoop")?,
            ended: n("Ended")?,
            keyframe_reached: n("KeyframeReached")?,
            stopped: n("Stopped")?,
            animation_played: n("AnimationPlayed")?,
            track_class: c("AnimationTrack")?,
            animator_class: c("Animator")?,
            animation_class: c("Animation")?,
            motor6d: c("Motor6D")?,
            bone: c("Bone")?,
            model: c("Model")?,
            humanoid: c("Humanoid")?,
            animation_controller: c("AnimationController")?,
            priorities,
        })
    }

    fn priority_item(&self, value: u32) -> Option<crate::reflection::EnumItemId> {
        self.priorities.iter().find(|(v, _)| *v == value).map(|(_, item)| *item)
    }
}

fn number_arg(args: &Args, index: usize, default: f64) -> f64 {
    args.number(index).filter(|n| n.is_finite()).unwrap_or(default)
}

fn set(state: &mut State, reflection: &Reflection, id: InstanceId, name: NameId, value: Value) {
    core::raw_set_silent(state, reflection, id, name, value);
}

fn track_of(args: &Args) -> Result<InstanceId, Flow> {
    args.instance(0).ok_or_else(|| Flow::Error(ScriptError::plain("Expected ':' not '.' calling member function")))
}

/// The Animator of a Humanoid or AnimationController: its Animator child, else a new one (as
/// Humanoid:LoadAnimation makes it).
fn animator_of(engine: &Engine, names: &Names, owner: InstanceId, make: bool) -> Result<Option<InstanceId>, Flow> {
    let mut state = engine.state.borrow_mut();
    if state.dom.at(owner).class == names.animator_class {
        return Ok(Some(owner));
    }
    let found = state.dom.children_of(owner).iter().copied().find(|&child| state.dom.at(child).class == names.animator_class);
    if found.is_some() || !make {
        return Ok(found);
    }
    let hooks = engine.hooks.borrow();
    let animator = core::new_instance(&mut state, &engine.reflection, &hooks, "Animator", Some(owner)).map_err(Flow::Error)?;
    Ok(Some(animator))
}

/// The asset an Animation names: its AnimationId, else AnimationContent's URI (bundles store only the
/// Content form).
fn animation_uri(state: &State, names: &Names, animation: InstanceId) -> String {
    let id = state.dom.raw_get(animation, names.animation_id).and_then(Value::as_str).map(|s| s.to_str_lossy().into_owned()).unwrap_or_default();
    if !id.trim().is_empty() {
        return id;
    }
    match state.dom.raw_get(animation, names.animation_content) {
        Some(Value::Content(content)) => match &**content {
            crate::value::ContentValue::Uri(uri) => uri.to_str_lossy().into_owned(),
            _ => id,
        },
        _ => id,
    }
}

/// ContentProvider:PreloadAsync's animations: every Animation among `items` and their descendants
/// starts its asset's fetch (cost::ANIMATION_FETCH); the seconds until the last of them arrives
/// (0 when all are here). Measured: PreloadAsync waits for them, then LoadAnimation has Length at once.
pub fn preload(engine: &Rc<Engine>, items: &[InstanceId]) -> f64 {
    let Some((_, names)) = extension(engine).state.borrow().clone() else { return 0.0 };
    let uris: Vec<String> = {
        let state = engine.state.borrow();
        let mut uris = Vec::new();
        for &item in items {
            if state.dom.get(item).is_none() {
                continue;
            }
            for id in std::iter::once(item).chain(state.dom.descendants(item)) {
                if state.dom.at(id).class == names.animation_class {
                    uris.push(animation_uri(&state, &names, id));
                }
            }
        }
        uris
    };
    let now = engine.state.borrow().sched.now;
    let mut wait: f64 = 0.0;
    for uri in uris {
        if source::fetched_from_file(engine, &uri) {
            wait = wait.max(source::arrival(engine, &uri) - now);
        }
    }
    wait
}

fn load(lua: &Lua, engine: &Rc<Engine>, shared: &Shared, names: &Names, owner: InstanceId, args: &Args) -> Result<LuaMultiValue, Flow> {
    let animation = args.instance(1).filter(|&id| engine.state.borrow().dom.at(id).class == names.animation_class);
    let Some(animation) = animation else {
        return Err(Flow::Error(ScriptError::plain("LoadAnimation requires an Animation object")));
    };
    let animator = animator_of(engine, names, owner, true)?.expect("made");
    let (uri, animation_name) = {
        let state = engine.state.borrow();
        (animation_uri(&state, names, animation), state.dom.at(animation).name.clone())
    };
    let clip = match source::resolve(engine, &uri) {
        Ok(clip) => clip,
        Err(error) => {
            if !error.contains("manifest bundles need an asset worker") && source::first_failure(engine, &uri) {
                engine.warn(format!("Failed to load animation {uri}: {error} (its tracks play without keyframes)"));
            }
            None
        }
    };
    // An asset not fetched yet arrives later (cost::ANIMATION_FETCH); a registered sequence is here
    let registered = source::loader(engine).borrow().registered.contains_key(uri.trim());
    let mut pending = None;
    let clip = match clip {
        Some(found) if !registered => {
            let arrives = source::arrival(engine, &uri);
            if arrives > engine.state.borrow().sched.now + 1e-9 {
                pending = Some((arrives, found));
                None
            } else {
                Some(found)
            }
        }
        other => other,
    };
    let reflection = &engine.reflection;
    let track = {
        let mut state = engine.state.borrow_mut();
        let track = core::create(&mut state, reflection, names.track_class);
        set(&mut state, reflection, track, crate::K::Name as NameId, Value::String(animation_name));
        set(&mut state, reflection, track, names.animation, Value::Instance(animation));
        let (length, looped, priority) = clip.as_ref().map_or((0.0, false, clip::PRIORITY_ACTION), |clip| (clip.length as f64, clip.looped, clip.priority));
        set(&mut state, reflection, track, names.length, Value::Number(length));
        set(&mut state, reflection, track, names.looped, Value::Bool(looped));
        if let Some(item) = names.priority_item(priority) {
            set(&mut state, reflection, track, names.priority, Value::EnumItem(item));
        }
        set(&mut state, reflection, track, names.is_playing, Value::Bool(false));
        set(&mut state, reflection, track, names.speed, Value::Number(1.0));
        set(&mut state, reflection, track, names.time_position, Value::Number(0.0));
        set(&mut state, reflection, track, names.weight_current, Value::Number(0.0));
        set(&mut state, reflection, track, names.weight_target, Value::Number(1.0));
        track
    };
    let mut animations = shared.borrow_mut();
    if pending.is_some() {
        animations.pending.push(track);
    }
    animations.tracks.insert(track, Track {
        animator,
        clip,
        pending,
        set_looped: false,
        set_priority: false,
        playing: false,
        active: false,
        time: 0.0,
        speed: 1.0,
        weight: 0.0,
        weight_from: 0.0,
        weight_target: 1.0,
        fade: 0.0,
        fade_elapsed: 0.0,
        fresh: false,
        order: 0,
        markers: Vec::new(),
    });
    drop(animations);
    Ok(values([instance_value(lua, engine, track)?]))
}

fn playing_tracks(lua: &Lua, engine: &Rc<Engine>, shared: &Shared, names: &Names, owner: InstanceId) -> Result<LuaMultiValue, Flow> {
    let table = lua.create_table()?;
    if let Some(animator) = animator_of(engine, names, owner, false)? {
        let mut found: Vec<(u64, InstanceId)> = shared
            .borrow()
            .tracks
            .iter()
            .filter(|(_, track)| track.animator == animator && track.playing)
            .map(|(&id, track)| (track.order, id))
            .collect();
        found.sort();
        for (_, id) in found {
            table.raw_push(instance_value(lua, engine, id)?)?;
        }
    }
    Ok(values([LuaValue::Table(table)]))
}

/// Starts a fade of `track`'s weight to `target`.
fn fade_to(track: &mut Track, target: f64, fade: f64) {
    track.weight_from = track.weight;
    track.weight_target = target;
    track.fade = fade.max(0.0);
    track.fade_elapsed = 0.0;
    if track.fade <= 0.0 {
        track.weight = target;
    }
}

fn play(engine: &Engine, shared: &Shared, names: &Names, id: InstanceId, args: &Args) {
    let fade = number_arg(args, 1, DEFAULT_FADE);
    let weight = number_arg(args, 2, 1.0);
    let speed = number_arg(args, 3, 1.0);
    let animator = {
        let mut guard = shared.borrow_mut();
        let animations = &mut *guard;
        animations.played += 1;
        let order = animations.played;
        let Some(track) = animations.tracks.get_mut(&id) else { return };
        if !track.active {
            track.weight = 0.0;
        }
        fade_to(track, weight, fade);
        if track.pending.is_some() {
            // (its fade starts when the asset arrives)
            track.weight = 0.0;
        }
        track.playing = true;
        track.time = 0.0;
        track.speed = speed;
        track.fresh = true;
        track.order = order;
        let animator = track.animator;
        if !track.active {
            track.active = true;
            animations.active.push(id);
            // A playing track stays alive without script references
            engine.state.borrow_mut().dom.pin(id);
        }
        animator
    };
    let reflection = &engine.reflection;
    let mut state = engine.state.borrow_mut();
    set(&mut state, reflection, id, names.is_playing, Value::Bool(true));
    set(&mut state, reflection, id, names.time_position, Value::Number(0.0));
    set(&mut state, reflection, id, names.speed, Value::Number(speed));
    set(&mut state, reflection, id, names.weight_target, Value::Number(weight));
    let current = shared.borrow().tracks[&id].weight;
    set(&mut state, reflection, id, names.weight_current, Value::Number(current));
    if state.dom.get(animator).is_some() {
        core::fire_event(&mut state, animator, names.animation_played, vec![Value::Instance(id)]);
    }
}

/// Stops a playing track: IsPlaying false and Stopped now, the fade-out from here.
fn stop(engine: &Engine, shared: &Shared, names: &Names, id: InstanceId, fade: f64) {
    {
        let mut animations = shared.borrow_mut();
        let Some(track) = animations.tracks.get_mut(&id) else { return };
        if !track.playing {
            return;
        }
        track.playing = false;
        fade_to(track, 0.0, fade);
    }
    let reflection = &engine.reflection;
    let mut state = engine.state.borrow_mut();
    set(&mut state, reflection, id, names.is_playing, Value::Bool(false));
    set(&mut state, reflection, id, names.weight_target, Value::Number(0.0));
    core::fire_event(&mut state, id, names.stopped, Vec::new());
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let Some(names) = Names::new(&engine.reflection) else { return };
    let names = Rc::new(names);
    let shared: Shared = Rc::default();

    // A freed track (unreachable and not playing) drops its state and marker signals
    let freed = shared.clone();
    engine.hooks.borrow_mut().instance_freed.push(Box::new(move |state, id| {
        let mut animations = freed.borrow_mut();
        if let Some(track) = animations.tracks.remove(&id) {
            for (_, signal) in track.markers {
                state.signals.free(signal);
            }
            animations.active.retain(|&active| active != id);
            animations.pending.retain(|&pending| pending != id);
        }
        animations.posed.remove(&id);
    }));

    // Looped and Priority a script writes hold when the asset arrives (measured)
    for (property, looped) in [(names.looped, true), (names.priority, false)] {
        let s = shared.clone();
        let setter: crate::services::SetterFn = Rc::new(move |lua, engine, id, value| {
            let class = engine.state.borrow().dom.at(id).class;
            let Some(info) = engine.reflection.class_property(class, property).cloned() else { return Ok(()) };
            let coerced = crate::lua::convert::coerce(lua, engine, &info, &value)?;
            if let Some(track) = s.borrow_mut().tracks.get_mut(&id) {
                if looped { track.set_looped = true } else { track.set_priority = true }
            }
            crate::lua::instance::set_property(engine, id, property, coerced);
            Ok(())
        });
        behaviors.add_setter(names.track_class, property, setter);
    }

    let reflection = engine.reflection.clone();
    let add = |behaviors: &mut Behaviors, class: &str, name: &str, function: crate::services::MethodFn| {
        if let (Some(class), Some(name)) = (reflection.class_id(class), reflection.name_id(name)) {
            behaviors.add_method(class, name, function);
        }
    };

    for class in ["Animator", "Humanoid", "AnimationController"] {
        let (s, n) = (shared.clone(), names.clone());
        add(behaviors, class, "LoadAnimation", method(move |lua, engine, id, args| load(lua, engine, &s, &n, id, args)));
        let (s, n) = (shared.clone(), names.clone());
        add(behaviors, class, "GetPlayingAnimationTracks", method(move |lua, engine, id, _| playing_tracks(lua, engine, &s, &n, id)));
    }

    let (s, n) = (shared.clone(), names.clone());
    add(behaviors, "AnimationTrack", "Play", method(move |_, engine, _, args| {
        play(engine, &s, &n, track_of(args)?, args);
        Ok(none())
    }));
    let (s, n) = (shared.clone(), names.clone());
    add(behaviors, "AnimationTrack", "Stop", method(move |_, engine, _, args| {
        stop(engine, &s, &n, track_of(args)?, number_arg(args, 1, DEFAULT_FADE));
        Ok(none())
    }));
    let (s, n) = (shared.clone(), names.clone());
    add(behaviors, "AnimationTrack", "AdjustSpeed", method(move |_, engine, _, args| {
        let id = track_of(args)?;
        let speed = number_arg(args, 1, 1.0);
        if let Some(track) = s.borrow_mut().tracks.get_mut(&id) {
            track.speed = speed;
        }
        set(&mut engine.state.borrow_mut(), &engine.reflection, id, n.speed, Value::Number(speed));
        Ok(none())
    }));
    let (s, n) = (shared.clone(), names.clone());
    add(behaviors, "AnimationTrack", "AdjustWeight", method(move |_, engine, _, args| {
        let id = track_of(args)?;
        let weight = number_arg(args, 1, 1.0);
        let fade = number_arg(args, 2, DEFAULT_FADE);
        let current = {
            let mut animations = s.borrow_mut();
            let Some(track) = animations.tracks.get_mut(&id) else { return Ok(none()) };
            fade_to(track, weight, fade);
            track.weight
        };
        let mut state = engine.state.borrow_mut();
        set(&mut state, &engine.reflection, id, n.weight_target, Value::Number(weight));
        set(&mut state, &engine.reflection, id, n.weight_current, Value::Number(current));
        Ok(none())
    }));
    let s = shared.clone();
    add(behaviors, "AnimationTrack", "GetMarkerReachedSignal", method(move |lua, engine, _, args| {
        let id = track_of(args)?;
        let name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
        let existing = s.borrow().tracks.get(&id).and_then(|track| track.markers.iter().find(|(n, _)| *n == name).map(|(_, signal)| *signal));
        let signal = match existing {
            Some(signal) => signal,
            None => {
                let signal = engine.state.borrow_mut().signals.create(Str::from(name.as_str()));
                if let Some(track) = s.borrow_mut().tracks.get_mut(&id) {
                    track.markers.push((name, signal));
                }
                signal
            }
        };
        Ok(values([signal_value(lua, engine, signal)?]))
    }));
    let s = shared.clone();
    add(behaviors, "AnimationTrack", "GetTimeOfKeyframe", method(move |_, _, _, args| {
        let id = track_of(args)?;
        let name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
        let clip = s.borrow().tracks.get(&id).and_then(|track| track.clip.clone());
        // Documented: it errors before the animation loaded and for a name no keyframe has
        // (messages not measured)
        let Some(clip) = clip.filter(|clip| clip.length > 0.0 || !clip.names.is_empty()) else {
            return Err(Flow::Error(ScriptError::plain("GetTimeOfKeyframe: the animation has not loaded")));
        };
        match clip.time_of(&name) {
            Some(time) => Ok(values([LuaValue::Number(time as f64)])),
            None => Err(Flow::Error(ScriptError::plain(format!("Could not find a keyframe by the name of {name}")))),
        }
    }));

    // KeyframeSequenceProvider:RegisterKeyframeSequence: a temporary id for a sequence of this run
    // (GetKeyframeSequenceAsync yields: a Luau stub, lua/services.luau, over source::sequence)
    add(behaviors, "KeyframeSequenceProvider", "RegisterKeyframeSequence", method(|lua, engine, _, args| {
        let Some(sequence) = args.instance(1).filter(|&id| {
            let state = engine.state.borrow();
            engine.reflection.class(state.dom.at(id).class).name.as_ref() == "KeyframeSequence"
        }) else {
            return Err(Flow::Error(ScriptError::plain("RegisterKeyframeSequence requires a KeyframeSequence")));
        };
        let uri = {
            let mut loader = source::loader(engine).borrow_mut();
            loader.next_registered += 1;
            let uri = format!("rbxtemp://{}", loader.next_registered);
            loader.registered.insert(uri.clone(), sequence);
            uri
        };
        engine.state.borrow_mut().dom.pin(sequence);
        Ok(values([LuaValue::String(lua.create_string(&uri)?)]))
    }));

    *extension(engine).state.borrow_mut() = Some((shared, names));
}

/// An engine's animation state: the tracks (for [`step`]) and the keyframe loader.
#[derive(Default)]
pub(crate) struct AnimationExtension {
    pub(crate) loader: RefCell<source::Loader>,
    state: RefCell<Option<(Shared, Rc<Names>)>>,
}

impl crate::Extension for AnimationExtension {
    fn teardown(&self) {
        drop(self.state.borrow_mut().take());
        drop(std::mem::take(&mut *self.loader.borrow_mut()));
    }
}

pub(crate) fn extension(engine: &Engine) -> &AnimationExtension {
    engine.extension_or_insert_with(AnimationExtension::default)
}

/// Events a track's step fires, in order.
enum Fire {
    Keyframe(String),
    Marker(String, String),
    DidLoop,
    Stopped,
    Ended,
}

/// The keyframes and markers of `clip` from `from` to `to` (forward), the start included when
/// `inclusive`.
fn crossings(clip: &Clip, from: f64, to: f64, inclusive: bool, out: &mut Vec<Fire>) {
    let passed = |time: f32| {
        let time = time as f64;
        (time > from || inclusive && time >= from) && time <= to
    };
    let mut events: Vec<(f32, Fire)> = Vec::new();
    for (time, name) in &clip.keyframes {
        if passed(*time) {
            events.push((*time, Fire::Keyframe(name.clone())));
        }
    }
    for marker in &clip.markers {
        if passed(marker.time) {
            events.push((marker.time, Fire::Marker(marker.name.clone(), marker.value.clone())));
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.extend(events.into_iter().map(|(_, fire)| fire));
}

/// The same backwards (negative speed): from `from` down to `to`.
fn crossings_back(clip: &Clip, from: f64, to: f64, inclusive: bool, out: &mut Vec<Fire>) {
    let mut forward = Vec::new();
    crossings(clip, to, from, true, &mut forward);
    let mut events: Vec<Fire> = forward.into_iter().rev().collect();
    if !inclusive {
        // (the start itself was reached by the step before)
        let start = clip.keyframes.iter().map(|k| k.0).chain(clip.markers.iter().map(|m| m.time)).filter(|&t| t as f64 == from).count();
        events.drain(..start.min(events.len()));
    }
    out.extend(events);
}

/// Advances a playing track by `delta` seconds of the clock: its new time and what it fired.
/// `looped` is the track's Looped. Ends the track (playing false, a fade-out) at a non-looped end.
fn advance(track: &mut Track, delta: f64, looped: bool, out: &mut Vec<Fire>) {
    let Some(clip) = track.clip.clone() else { return };
    let length = clip.length as f64;
    let inclusive = std::mem::take(&mut track.fresh);
    if length <= 0.0 {
        if inclusive {
            crossings(&clip, 0.0, 0.0, true, out);
        }
        if !looped {
            end(track, 0.0, out);
        }
        return;
    }
    let from = track.time;
    let to = from + delta * track.speed;
    if track.speed >= 0.0 {
        if to < length {
            crossings(&clip, from, to, inclusive, out);
            track.time = to;
        } else if looped {
            crossings(&clip, from, length, inclusive, out);
            let wrapped = to.rem_euclid(length);
            crossings(&clip, 0.0, wrapped, true, out);
            out.push(Fire::DidLoop);
            track.time = wrapped;
        } else {
            crossings(&clip, from, length, inclusive, out);
            end(track, length, out);
        }
    } else if to > 0.0 {
        crossings_back(&clip, from, to, inclusive, out);
        track.time = to;
    } else if looped {
        // Negative speed: past the first keyframe it restarts at the last
        crossings_back(&clip, from, 0.0, inclusive, out);
        let wrapped = to.rem_euclid(length);
        crossings_back(&clip, length, wrapped, true, out);
        out.push(Fire::DidLoop);
        track.time = wrapped;
    } else {
        crossings_back(&clip, from, 0.0, inclusive, out);
        end(track, 0.0, out);
    }
}

fn end(track: &mut Track, at: f64, out: &mut Vec<Fire>) {
    track.time = at;
    track.playing = false;
    fade_to(track, 0.0, DEFAULT_FADE);
    out.push(Fire::Stopped);
}

/// Moves the weight `delta` seconds along its fade.
fn fade_step(track: &mut Track, delta: f64) {
    if track.fade <= 0.0 || track.fade_elapsed >= track.fade {
        track.weight = track.weight_target;
        return;
    }
    track.fade_elapsed += delta;
    let progress = (track.fade_elapsed / track.fade).min(1.0);
    track.weight = track.weight_from + (track.weight_target - track.weight_from) * progress;
}

/// Tracks whose asset has arrived take its clip: Length, and Looped / Priority unless a script set
/// them; a playing one starts from its TimePosition and its fade from weight 0 (measured).
fn arrive(engine: &Rc<Engine>, shared: &Shared, names: &Names) {
    let now = engine.state.borrow().sched.now;
    let arrived: Vec<(InstanceId, Arc<Clip>, bool, bool)> = {
        let mut guard = shared.borrow_mut();
        let animations = &mut *guard;
        let mut arrived = Vec::new();
        animations.pending.retain(|id| {
            let Some(track) = animations.tracks.get_mut(id) else { return false };
            let Some((_, clip)) = track.pending.take_if(|(at, _)| *at <= now + 1e-9) else { return track.pending.is_some() };
            track.clip = Some(clip.clone());
            if track.playing {
                let (target, fade) = (track.weight_target, track.fade);
                track.weight = 0.0;
                fade_to(track, target, fade);
            }
            arrived.push((*id, clip, track.set_looped, track.set_priority));
            false
        });
        arrived
    };
    let reflection = &engine.reflection;
    let mut state = engine.state.borrow_mut();
    for (id, clip, set_looped, set_priority) in arrived {
        if state.dom.get(id).is_none() {
            continue;
        }
        set(&mut state, reflection, id, names.length, Value::Number(clip.length as f64));
        if !set_looped {
            set(&mut state, reflection, id, names.looped, Value::Bool(clip.looped));
        }
        if let Some(item) = names.priority_item(clip.priority).filter(|_| !set_priority) {
            set(&mut state, reflection, id, names.priority, Value::EnumItem(item));
        }
    }
}

/// Animation assets arriving where no Animator steps (edit mode).
pub fn arrivals(engine: &Rc<Engine>) {
    let Some((shared, names)) = engine.extension::<AnimationExtension>().and_then(|found| found.state.borrow().clone()) else { return };
    if !shared.borrow().pending.is_empty() {
        arrive(engine, &shared, &names);
    }
}

/// The Animator step (play mode, after PreAnimation): every active track advances by `delta`, then
/// each animator writes its rig's blended pose.
pub fn step(engine: &Rc<Engine>, delta: f64) {
    let Some((shared, names)) = engine.extension::<AnimationExtension>().and_then(|found| found.state.borrow().clone()) else { return };
    if !shared.borrow().pending.is_empty() {
        arrive(engine, &shared, &names);
    }
    if shared.borrow().active.is_empty() && shared.borrow().posed.is_empty() {
        return;
    }
    let active: Vec<InstanceId> = shared.borrow().active.clone();
    // (posed animators in creation order, not the map's: a joint two animators pose is written by the later one, and a slot is
    // reused when the collector frees an instance, so slot order differs between two runs of one scene)
    let mut animators: Vec<InstanceId> = shared.borrow().posed.keys().copied().collect();
    {
        let state = engine.state.borrow();
        animators.sort_by_key(|&id| state.dom.get(id).map_or(0, |instance| instance.serial));
    }
    let mut seen: rustc_hash::FxHashSet<InstanceId> = animators.iter().copied().collect();
    for id in active {
        let mut fired = Vec::new();
        let (alive, looped, set_time) = {
            let state = engine.state.borrow();
            let track = &shared.borrow().tracks[&id];
            // (a destroyed rig or track stops playing)
            let alive = |id| state.dom.get(id).is_some_and(|instance| !instance.has(crate::dom::flags::DESTROYED));
            let alive = alive(track.animator) && alive(id);
            let looped = state.dom.boolean(id, names.looped).unwrap_or(false);
            let set_time = state.dom.number(id, names.time_position).filter(|&t| t != track.time);
            (alive, looped, set_time)
        };
        let (animator, finished, time, weight, playing, waiting) = {
            let mut animations = shared.borrow_mut();
            let track = animations.tracks.get_mut(&id).expect("active track");
            // (a track whose asset is on the way holds: TimePosition as set, weight 0)
            let waiting = track.pending.is_some();
            if !waiting {
                if let Some(time) = set_time {
                    // A script set TimePosition: play on from there (the keyframes skipped do not fire)
                    let length = track.clip.as_ref().map_or(0.0, |clip| clip.length as f64);
                    track.time = time.clamp(0.0, length);
                }
                fade_step(track, delta);
                if track.playing && alive {
                    advance(track, delta, looped, &mut fired);
                }
            }
            let finished = !alive || !track.playing && track.weight <= 0.0;
            (track.animator, finished, track.time, track.weight, track.playing, waiting)
        };
        if seen.insert(animator) {
            animators.push(animator);
        }
        if finished {
            {
                let mut animations = shared.borrow_mut();
                let track = animations.tracks.get_mut(&id).expect("active track");
                track.active = false;
                track.weight = 0.0;
                if track.playing {
                    // (it or its rig was destroyed)
                    track.playing = false;
                    fired.push(Fire::Stopped);
                }
                animations.active.retain(|&active| active != id);
            }
            fired.push(Fire::Ended);
        }
        let markers = shared.borrow().tracks[&id].markers.clone();
        let reflection = &engine.reflection;
        let mut state = engine.state.borrow_mut();
        if state.dom.get(id).is_none() {
            continue;
        }
        if !waiting {
            set(&mut state, reflection, id, names.time_position, Value::Number(time));
        }
        set(&mut state, reflection, id, names.weight_current, Value::Number(if finished { 0.0 } else { weight }));
        set(&mut state, reflection, id, names.is_playing, Value::Bool(playing && !finished));
        for fire in fired {
            match fire {
                Fire::Keyframe(name) => core::fire_event(&mut state, id, names.keyframe_reached, vec![Value::String(Str::from(name.as_str()))]),
                Fire::Marker(name, value) => {
                    if let Some((_, signal)) = markers.iter().find(|(n, _)| *n == name) {
                        core::fire(&mut state, *signal, vec![Value::String(Str::from(value.as_str()))]);
                    }
                }
                Fire::DidLoop => core::fire_event(&mut state, id, names.did_loop, Vec::new()),
                Fire::Stopped => {
                    set(&mut state, reflection, id, names.weight_target, Value::Number(0.0));
                    core::fire_event(&mut state, id, names.stopped, Vec::new());
                }
                Fire::Ended => core::fire_event(&mut state, id, names.ended, Vec::new()),
            }
        }
        if finished {
            state.dom.unpin(id);
        }
    }
    // Each animator's playing tracks, gathered in one pass over the active ones (every animator looked through all of them: with 60
    // enemies of two tracks each 7,000 track lookups and clip clones a step)
    let mut tracks: FxHashMap<InstanceId, Vec<PosingTrack>> = FxHashMap::default();
    {
        let animations = shared.borrow();
        let state = engine.state.borrow();
        for id in &animations.active {
            let track = &animations.tracks[id];
            let Some(clip) = track.clip.clone() else { continue };
            if track.weight <= 0.0 {
                continue;
            }
            let priority = state.dom.raw_get(*id, names.priority).and_then(Value::as_enum).map_or(clip::PRIORITY_ACTION, |item| engine.reflection.enum_item_info(item).value as u32);
            tracks.entry(track.animator).or_default().push((clip, track.time as f32, track.weight as f32, priority_rank(priority)));
        }
    }
    for animator in animators {
        pose(engine, &shared, &names, animator, tracks.remove(&animator).unwrap_or_default());
    }
}

/// A playing track as `pose` blends it: its clip, time, weight and priority rank.
type PosingTrack = (Arc<Clip>, f32, f32, u32);

/// A rig's joints by the names an animation's Poses give them.
struct Rig {
    motors: FxHashMap<(Str, Str), InstanceId>,
    bones: FxHashMap<(Str, Str), InstanceId>,
    scale: f32,
}

#[cfg(test)]
thread_local! {
    /// How many rigs `rig_of` has built on this thread (tests)
    pub(crate) static RIG_BUILDS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// `animator`'s rig maps: kept while nothing inside its model changed (the walk over every descendant ran per animator per
/// step: 0.046 host ms a frame for 60 enemies).
fn cached_rig(shared: &Shared, state: &mut State, reflection: &Reflection, names: &Names, animator: InstanceId) -> Option<Rc<Rig>> {
    let owner = state.dom.get(animator)?.parent?;
    let model = state.dom.get(owner)?.parent?;
    let version = state.dom.watch_rig(model, [crate::K::Name as NameId, names.part0, names.part1, names.scale]);
    if let Some((cached_model, cached_version, rig)) = shared.borrow().rigs.get(&animator) {
        if *cached_model == model && *cached_version == version {
            return Some(rig.clone());
        }
    }
    let rig = Rc::new(rig_of(state, reflection, names, animator)?);
    let mut animations = shared.borrow_mut();
    // (animators that are gone leave their entry behind: swept now and then)
    if animations.rigs.len() >= 512 && animations.rigs.len().is_power_of_two() {
        animations.rigs.retain(|id, _| state.dom.get(*id).is_some());
    }
    animations.rigs.insert(animator, (model, version, rig.clone()));
    Some(rig)
}

fn rig_of(state: &State, reflection: &Reflection, names: &Names, animator: InstanceId) -> Option<Rig> {
    #[cfg(test)]
    RIG_BUILDS.with(|builds| builds.set(builds.get() + 1));
    let owner = state.dom.get(animator)?.parent?;
    let model = state.dom.get(owner)?.parent?;
    let scale = if reflection.class(state.dom.at(model).class).is_a(names.model) {
        state.dom.number(model, names.scale).filter(|scale| *scale > 0.0).unwrap_or(1.0) as f32
    } else {
        1.0
    };
    let mut rig = Rig { motors: FxHashMap::default(), bones: FxHashMap::default(), scale };
    let descendants = state.dom.descendants(model);
    // A model inside this one with its own Humanoid or AnimationController is a rig of its own: its joints are not this rig's,
    // however they are named
    let carried: Vec<InstanceId> = descendants
        .iter()
        .filter(|id| {
            let class = reflection.class(state.dom.at(**id).class);
            class.is_a(names.humanoid) || class.is_a(names.animation_controller)
        })
        .filter_map(|id| state.dom.parent_of(*id))
        .filter(|parent| *parent != model)
        .collect();
    let in_carried_rig = |id: InstanceId| {
        let mut at = state.dom.parent_of(id);
        while let Some(parent) = at.filter(|parent| *parent != model) {
            if carried.contains(&parent) {
                return true;
            }
            at = state.dom.parent_of(parent);
        }
        false
    };
    for id in descendants {
        let class = reflection.class(state.dom.at(id).class);
        if !(class.is_a(names.motor6d) || class.is_a(names.bone)) || in_carried_rig(id) {
            continue;
        }
        if class.is_a(names.motor6d) {
            let part = |name| state.dom.raw_get(id, name).and_then(Value::as_instance).and_then(|part| state.dom.get(part)).map(|part| part.name.clone());
            if let (Some(part0), Some(part1)) = (part(names.part0), part(names.part1)) {
                rig.motors.entry((part0, part1)).or_insert(id);
            }
        } else if class.is_a(names.bone) {
            if let Some(parent) = state.dom.parent_of(id).and_then(|parent| state.dom.get(parent)) {
                rig.bones.entry((parent.name.clone(), state.dom.at(id).name.clone())).or_insert(id);
            }
        }
    }
    Some(rig)
}

/// One track's sample of a joint: (priority rank, weight, rotation, translation).
pub type Layer = (u32, f32, Quat, Vec3A);

/// The blended offset of a joint: priorities from the highest down each take their summed weight
/// (at most 1) of what is left, blending their tracks by weight; the rest stays at the rest pose.
pub fn blend(layers: &mut [Layer]) -> (Quat, Vec3A) {
    layers.sort_by(|a, b| b.0.cmp(&a.0));
    let align = |q: Quat| if q.w < 0.0 { -q } else { q };
    let mut remaining = 1.0f32;
    let mut rotation = Vec4::ZERO;
    let mut translation = Vec3A::ZERO;
    let mut start = 0;
    while start < layers.len() && remaining > 0.0 {
        let rank = layers[start].0;
        let end = start + layers[start..].iter().take_while(|layer| layer.0 == rank).count();
        let group = &layers[start..end];
        start = end;
        let total: f32 = group.iter().map(|layer| layer.1.max(0.0)).sum();
        if total <= 0.0 {
            continue;
        }
        let mut level_rotation = Vec4::ZERO;
        let mut level_translation = Vec3A::ZERO;
        for &(_, weight, q, t) in group {
            let weight = weight.max(0.0) / total;
            level_rotation += Vec4::from(align(q)) * weight;
            level_translation += t * weight;
        }
        let share = remaining * total.min(1.0);
        rotation += level_rotation.normalize_or_zero() * share;
        translation += level_translation * share;
        remaining -= share;
    }
    rotation += Vec4::from(Quat::IDENTITY) * remaining.max(0.0);
    let rotation = Quat::from_vec4(rotation.normalize_or_zero());
    (if rotation.length_squared() > 0.0 { rotation } else { Quat::IDENTITY }, translation)
}

/// Writes `animator`'s blended pose into its rig's joints; joints it posed before that no track
/// poses now go back to rest.
fn pose(engine: &Rc<Engine>, shared: &Shared, names: &Names, animator: InstanceId, tracks: Vec<PosingTrack>) {
    let reflection = &engine.reflection;
    // (joints in the order the tracks first pose them: hash order follows slots, which differ between two runs of one scene)
    let mut joints: Vec<(InstanceId, Vec<Layer>)> = Vec::new();
    let mut joint_at: FxHashMap<InstanceId, usize> = FxHashMap::default();
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    let rig = if tracks.is_empty() { None } else { cached_rig(shared, &mut state, reflection, names, animator) };
    let mut scale = 1.0;
    if let Some(rig) = &rig {
        scale = rig.scale;
        for &(ref clip, time, weight, rank) in &tracks {
            for joint in &clip.joints {
                let key = (Str::from(joint.parent.as_str()), Str::from(joint.name.as_str()));
                let target = rig.motors.get(&key).or_else(|| rig.bones.get(&key)).copied();
                if let Some(target) = target {
                    let (q, t) = clip::sample(&joint.keys, time);
                    let at = *joint_at.entry(target).or_insert_with(|| {
                        joints.push((target, Vec::new()));
                        joints.len() - 1
                    });
                    joints[at].1.push((rank, weight, q, t));
                }
            }
        }
    }
    let before = shared.borrow_mut().posed.remove(&animator).unwrap_or_default();
    let mut posed: Vec<InstanceId> = Vec::with_capacity(joints.len());
    for (joint, mut layers) in joints {
        let (rotation, translation) = blend(&mut layers);
        write_transform(&mut state, reflection, &hooks, names, joint, Affine3A::from_rotation_translation(rotation, (translation * scale).into()));
        posed.push(joint);
    }
    for joint in before {
        if !posed.contains(&joint) && state.dom.get(joint).is_some() {
            write_transform(&mut state, reflection, &hooks, names, joint, Affine3A::IDENTITY);
        }
    }
    if !posed.is_empty() {
        posed.sort();
        shared.borrow_mut().posed.insert(animator, posed);
    }
}

/// Sets a joint's Transform as an Animator does: no Changed event (not measured), a Motor6D's
/// joining the batch applied after PreSimulation.
fn write_transform(state: &mut State, reflection: &Reflection, hooks: &crate::Hooks, names: &Names, joint: InstanceId, transform: Affine3A) {
    let value = crate::world::frame::cframe_value(&transform);
    if core::raw_set_unsignalled(state, reflection, hooks, joint, names.transform, value) {
        crate::world::joints::property_changed(state, reflection, hooks, joint, names.transform, None);
    }
}

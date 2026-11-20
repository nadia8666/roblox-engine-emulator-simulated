//! AnimationTracks: Animator:LoadAnimation (and the Humanoid's and AnimationController's), the
//! track's playback members and events, and the Animator's step, which writes the blended pose into
//! the rig's Motor6D.Transform and Bone.Transform each frame. A track plays a KeyframeSequence
//! ([`clip`]) or a CurveAnimation ([`curve_clip`], measured in Studio; the curve instances' own
//! members are in `curves`). A track of either steps as Studio 0.747 steps one (measured with
//! Animator:StepAnimations in an edit session, docs/engine.md "Animation curves", Stepping; golden scripts in
//! `curve_golden/`):
//!
//! - The step runs after RunService.PreAnimation and before Stepped (the task scheduler's order), in
//!   play mode only (an Animator does not run in edit mode; Animator:StepAnimations steps one there); the
//!   Motor6D batch after PreSimulation then poses the parts in the same frame. That order is Roblox's
//!   documentation.
//! - TimePosition, WeightCurrent and the fade are float32 ([`advance`], [`fade_step`]). A fade (Play, Stop,
//!   AdjustWeight) starts from the weight the track has and takes effect with the next step, a fade of 0 too.
//!   A track whose fade is over and that is stopped, or whose target weight is 0, ends with the next step:
//!   Stopped if it was playing, then Ended, and it is stepped no more (its WeightCurrent stays as it was).
//! - Per joint, priorities from the highest down each take the smaller of their tracks' summed weight and
//!   what is left; tracks of one priority blend by their weights; whatever no track takes stays at the rest
//!   pose. A clip's FaceControls values blend the same way into the rig's FaceControls instance ([`pose`]).
//!   A weight below 0.001 poses nothing. While an animator has a track that is stepped it writes every joint
//!   of its rig each step (the rest pose into those no track poses); with none it writes nothing, so the
//!   pose of a track stopped with a fade of 0 stays.
//! - A track that reaches its end without looping fires Stopped and fades out over 0.3 s. KeyframeReached
//!   (a KeyframeSequence's keyframes not named "Keyframe"; "End" for a CurveAnimation) and
//!   GetMarkerReachedSignal(name) fire for what the step passed. Events are deferred.
//! - Translations scale with the rig model's Scale (Model:ScaleTo scales the animations it plays).
//! - A track of an `active://` id follows its clip instance as scripts change it ([`clip_changed`]).

pub mod clip;
pub mod curve;
pub mod curve_clip;
mod curves;
pub mod source;
#[cfg(test)]
mod curve_tests;
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
use clip::{Clip, Tree, priority_rank};
pub use source::{Loading, enable};

/// Roblox's default fade time (Play, Stop, AdjustWeight): 0.100000001.
pub const DEFAULT_FADE: f64 = 0.1;

/// How long a track that reached its end fades out (measured, Studio 0.747: a sixth of the pose is left 0.25 s after
/// the end, none after 0.3 s; a KeyframeSequence's the same).
const END_FADE: f64 = 0.3;

/// The weight below which a track poses nothing (measured: 0.00099 leaves the joint at rest, 0.00101 poses).
const POSING_WEIGHT: f32 = 0.001;

/// A target weight this near 0 ends the track once its fade is over (measured: 1e-5 does, 2e-5 does not, -1 does not).
const IDLE_WEIGHT: f32 = 1e-5;

struct Track {
    animator: InstanceId,
    clip: Option<Arc<Clip>>,
    /// IsPlaying
    playing: bool,
    /// Stepped and posing: playing, or fading out after it stopped
    active: bool,
    /// TimePosition as the step last wrote it, a float32 (a different stored value was set by a script)
    time: f64,
    speed: f64,
    /// WeightCurrent
    weight: f32,
    weight_target: f32,
    /// What is left of the fade to `weight_target`, in seconds
    fade_left: f32,
    /// When it was last played (GetPlayingAnimationTracks' order)
    order: u64,
    markers: Vec<(String, SignalId)>,
    /// Its asset on the way: when it arrives and the clip it holds (until then Length 0, no pose)
    pending: Option<(f64, Arc<Clip>)>,
    /// Looped / Priority written by a script (the asset's own do not replace them when it arrives)
    set_looped: bool,
    set_priority: bool,
    /// The clip instance an `active://` track follows ([`clip_changed`])
    live: Option<InstanceId>,
    /// The instance changed since `clip` was read from it
    stale: bool,
}

#[derive(Default)]
struct Animations {
    tracks: FxHashMap<InstanceId, Track>,
    /// Active tracks in the order they were played
    active: Vec<InstanceId>,
    /// Tracks whose asset is on the way
    pending: Vec<InstanceId>,
    /// The tracks of `active://` ids by the clip instance they follow
    live: FxHashMap<InstanceId, Vec<InstanceId>>,
    /// Whether the hooks that tell of a followed clip's changes are in
    following: bool,
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
    face_controls: Option<ClassId>,
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
            face_controls: c("FaceControls"),
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
    // (measured: an `active://` id that is none, or names no registered clip, raises here)
    let live = match source::active_clip(engine, &uri) {
        Some(found) => Some(found.map_err(|problem| Flow::Error(ScriptError::plain(problem)))?),
        None => None,
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
    let registered = source::is_registered(engine, &uri);
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
        // (measured: WeightTarget is 0 until the track is played)
        set(&mut state, reflection, track, names.weight_current, Value::Number(0.0));
        set(&mut state, reflection, track, names.weight_target, Value::Number(0.0));
        track
    };
    if live.is_some() {
        follow(engine, shared, names);
    }
    let mut animations = shared.borrow_mut();
    if pending.is_some() {
        animations.pending.push(track);
    }
    if let Some(live) = live {
        animations.live.entry(live).or_default().push(track);
    }
    animations.tracks.insert(track, Track {
        animator,
        clip,
        pending,
        live,
        stale: false,
        set_looped: false,
        set_priority: false,
        playing: false,
        active: false,
        time: 0.0,
        speed: 1.0,
        weight: 0.0,
        weight_target: 0.0,
        fade_left: 0.0,
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

/// A followed clip changed: `changed` is the clip instance or something inside it (anything else is nothing to
/// this). Its tracks take the clip's Length now, and its Loop and Priority unless a script wrote the track's own,
/// without a Changed event (measured in Studio 0.747 for a track of AnimationClipProvider:RegisterActiveAnimationClip's
/// id: after a key is inserted at 2 s the track's Length reads 2 before any step, and a signal on its Length did not
/// fire); the poses, joints and markers are read again with the next step ([`refresh`]).
fn clip_changed(animations: &mut Animations, names: &Names, reflection: &Reflection, state: &mut State, changed: InstanceId) {
    let mut at = Some(changed);
    while let Some(id) = at {
        if let Some(tracks) = animations.live.get(&id) {
            let header = {
                let tree = clip::EngineTree { state, reflection };
                let curve = tree.class(id) == "CurveAnimation";
                (state.dom.get(id).is_some() && (curve || tree.class(id) == "KeyframeSequence")).then(|| clip::header(&tree, id, curve))
            };
            let (length, looped, priority) = header.unwrap_or((0.0, false, clip::PRIORITY_ACTION));
            for &track in tracks {
                let Some(kept) = animations.tracks.get_mut(&track) else { continue };
                kept.stale = true;
                if state.dom.get(track).is_none() {
                    continue;
                }
                set(state, reflection, track, names.length, Value::Number(length as f64));
                if header.is_some() && !kept.set_looped {
                    set(state, reflection, track, names.looped, Value::Bool(looped));
                }
                if let Some(item) = names.priority_item(priority).filter(|_| header.is_some() && !kept.set_priority) {
                    set(state, reflection, track, names.priority, Value::EnumItem(item));
                }
            }
        }
        at = state.dom.get(id).and_then(|instance| instance.parent);
    }
}

/// A curve's keys were written (curves.rs stores them without a hook): [`clip_changed`] for it.
pub(super) fn keys_changed(engine: &Engine, curve: InstanceId) {
    let Some((shared, names)) = engine.extension::<AnimationExtension>().and_then(|found| found.state.borrow().clone()) else { return };
    if shared.borrow().live.is_empty() {
        return;
    }
    clip_changed(&mut shared.borrow_mut(), &names, &engine.reflection, &mut engine.state.borrow_mut(), curve);
}

/// Puts in the hooks that tell [`clip_changed`] of property and hierarchy changes, with the first `active://` track
/// of an engine (an engine without one runs none of this).
fn follow(engine: &Engine, shared: &Shared, names: &Names) {
    if std::mem::replace(&mut shared.borrow_mut().following, true) {
        return;
    }
    let mut hooks = engine.hooks.borrow_mut();
    // (the engine and the names are reached through the extension: a hook that held them would keep the engine alive)
    let told = |shared: &Shared, names: &Rc<Names>, reflection: &Arc<Reflection>, state: &mut State, id: InstanceId| {
        if let Ok(mut animations) = shared.try_borrow_mut() {
            if !animations.live.is_empty() {
                clip_changed(&mut animations, names, reflection, state, id);
            }
        }
    };
    let Some((_, names_rc)) = extension(engine).state.borrow().clone() else { return };
    let _ = names;
    let reflection = engine.reflection.clone();
    let (s, n, r) = (Rc::downgrade(shared), names_rc.clone(), reflection.clone());
    hooks.property_changed.push(Box::new(move |state, id, _| {
        if let Some(shared) = s.upgrade() {
            told(&shared, &n, &r, state, id);
        }
    }));
    let (s, n, r) = (Rc::downgrade(shared), names_rc, reflection);
    hooks.hierarchy_changed.push(Box::new(move |state, id, old, new| {
        if let Some(shared) = s.upgrade() {
            for changed in [old, new.or(Some(id))].into_iter().flatten() {
                told(&shared, &n, &r, state, changed);
            }
        }
    }));
}

/// Reads a followed clip again for the track that plays it, when it changed since.
fn refresh(engine: &Engine, shared: &Shared, id: InstanceId) {
    let live = match shared.borrow().tracks.get(&id) {
        Some(track) if track.stale => track.live,
        _ => return,
    };
    let Some(live) = live else { return };
    let clip = {
        let state = engine.state.borrow();
        let tree = clip::EngineTree { state: &state, reflection: &engine.reflection };
        match tree.class(live) {
            "CurveAnimation" => Clip::of(&tree, live, true),
            "KeyframeSequence" => Clip::of(&tree, live, false),
            // (the instance is gone)
            _ => Clip::default(),
        }
    };
    if let Some(track) = shared.borrow_mut().tracks.get_mut(&id) {
        track.clip = Some(Arc::new(clip));
        track.stale = false;
    }
}

/// Starts a fade of `track`'s weight to `target` over `fade` seconds (one below 0 is 0). The weight itself moves with
/// the next step, also for a fade of 0 (measured: AdjustWeight(0.5, 0) leaves WeightCurrent at 1 until a step, of 0 s
/// even, and Play(0) leaves a new track's at 0).
fn fade_to(track: &mut Track, target: f64, fade: f64) {
    track.weight_target = target as f32;
    track.fade_left = fade.max(0.0) as f32;
}

fn play(engine: &Engine, shared: &Shared, names: &Names, id: InstanceId, args: &Args) {
    let fade = number_arg(args, 1, DEFAULT_FADE);
    let weight = number_arg(args, 2, 1.0);
    let speed = number_arg(args, 3, 1.0);
    refresh(engine, shared, id);
    let animator = {
        let mut guard = shared.borrow_mut();
        let animations = &mut *guard;
        animations.played += 1;
        let order = animations.played;
        let Some(track) = animations.tracks.get_mut(&id) else { return };
        // (the fade starts from the weight the track has: 0 for a new one or one that faded out, and whatever a track
        // stopped with a fade of 0 was left with, measured 1)
        fade_to(track, weight, fade);
        if track.pending.is_some() {
            // (its fade starts when the asset arrives)
            track.weight = 0.0;
        }
        track.playing = true;
        // (measured: a track played backwards starts at the last key)
        track.time = track.clip.as_ref().filter(|_| speed < 0.0).map_or(0.0, |clip| clip.length as f64);
        track.speed = speed;
        track.order = order;
        let animator = track.animator;
        if !track.active {
            track.active = true;
            animations.active.push(id);
            // A playing track stays alive without script references
            engine.state.borrow_mut().dom.pin(id);
        }
        (animator, track.time)
    };
    let (animator, time) = animator;
    let reflection = &engine.reflection;
    let mut state = engine.state.borrow_mut();
    set(&mut state, reflection, id, names.is_playing, Value::Bool(true));
    set(&mut state, reflection, id, names.time_position, Value::Number(time));
    set(&mut state, reflection, id, names.speed, Value::Number(speed));
    set(&mut state, reflection, id, names.weight_target, Value::Number(weight as f32 as f64));
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
            if let Some(live) = track.live {
                if let Some(tracks) = animations.live.get_mut(&live) {
                    tracks.retain(|&kept| kept != id);
                    if tracks.is_empty() {
                        animations.live.remove(&live);
                    }
                }
            }
        }
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

    // Animator:StepAnimations(delta): this animator's tracks advance and pose its rig, as the frame's step does for all.
    // Plugin security (Roblox's API reference): a game's script may not call it, the command bar and plugins of an edit
    // session may (measured there, Studio 0.747).
    let edit = engine.options.edit_mode();
    add(behaviors, "Animator", "StepAnimations", method(move |_, engine, id, args| {
        if !edit {
            return Err(Flow::Error(ScriptError::plain("The current thread cannot call 'StepAnimations' (lacking capability PluginOrOpenCloud)")));
        }
        // (measured: a number, or a string of one. A delta below 0, or one that is no number, steps like any other:
        // `advance` and `fade_step` say what comes of it)
        let delta = match args.get(1) {
            LuaValue::Nil => return Err(Flow::Error(ScriptError::plain("Argument 1 missing or nil"))),
            LuaValue::Number(n) => *n,
            LuaValue::Integer(n) => *n as f64,
            LuaValue::Boolean(flag) => *flag as u8 as f64,
            LuaValue::String(text) => match text.to_string_lossy().trim().parse::<f64>() {
                Ok(number) => number,
                Err(_) => return Err(Flow::Error(ScriptError::plain("Unable to cast string to float"))),
            },
            LuaValue::Table(_) => return Err(Flow::Error(ScriptError::plain("Unable to cast Array to float"))),
            other => return Err(Flow::Error(ScriptError::plain(format!("Unable to cast {} to float", crate::lua::typeof_value(engine, other))))),
        };
        step_of(engine, delta, Some(id));
        Ok(none())
    }));

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
        {
            let mut animations = s.borrow_mut();
            let Some(track) = animations.tracks.get_mut(&id) else { return Ok(none()) };
            // (measured: a track that is not playing takes no weight: after Stop(0.3), AdjustWeight(0.5, 0) leaves
            // WeightTarget at 0 and the fade-out as it was)
            if !track.playing {
                return Ok(none());
            }
            fade_to(track, weight, fade);
        }
        set(&mut engine.state.borrow_mut(), &engine.reflection, id, n.weight_target, Value::Number(weight as f32 as f64));
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
        // Measured (Studio 0.747): a CurveAnimation's track knows no keyframe names
        if clip.as_ref().is_some_and(|clip| clip.curve) {
            return Err(Flow::Error(ScriptError::plain("Invalid operation for this animation format.")));
        }
        // Documented: it errors before the animation loaded (message not measured) and for a name no keyframe has
        // (measured, Studio 0.747: "Could not find a keyframe by that name!")
        let Some(clip) = clip.filter(|clip| clip.length > 0.0 || !clip.names.is_empty()) else {
            return Err(Flow::Error(ScriptError::plain("GetTimeOfKeyframe: the animation has not loaded")));
        };
        match clip.time_of(&name) {
            Some(time) => Ok(values([LuaValue::Number(time as f64)])),
            None => Err(Flow::Error(ScriptError::plain("Could not find a keyframe by that name!"))),
        }
    }));

    // Registering a clip of this run: a temporary id for it (GetKeyframeSequenceAsync and GetAnimationClipAsync yield: Luau
    // stubs, lua/services.luau, over source::sequence and source::animation_clip). Measured in Studio 0.747:
    // KeyframeSequenceProvider:RegisterKeyframeSequence takes a CurveAnimation as it takes a KeyframeSequence, and does
    // not look inside it; it takes any other instance too, a Folder say, whose id plays nothing (source::register).
    // AnimationClipProvider's two refuse a CurveAnimation whose Position or Rotation child is of another class
    // (curve_clip::validate) and anything that is not a clip; the Active ones give "active://N", counted from 0 across
    // each service (source::active_clip), and KeyframeSequenceProvider's takes a KeyframeSequence only. The two that
    // are not Active keep the clip as it is when registered and give the id that content has (source::register).
    let register = |active: bool, curves: bool, checked: bool, refusal: &'static str| {
        method(move |lua, engine, _, args| {
            if args.get(1).is_nil() {
                return Err(Flow::Error(ScriptError::plain("Argument 1 missing or nil")));
            }
            if !active && args.instance(1).is_none() {
                return Err(Flow::Error(ScriptError::plain("Unable to cast value to Object")));
            }
            let class = args.instance(1).map(|id| engine.reflection.class(engine.state.borrow().dom.at(id).class).name.clone());
            // (whether it is a CurveAnimation; None for what is no clip)
            let (instance, curve) = match (args.instance(1), class.as_deref()) {
                (Some(id), Some("KeyframeSequence")) => (id, Some(false)),
                (Some(id), Some("CurveAnimation")) if curves => (id, Some(true)),
                (Some(id), _) if !active && !checked => (id, None),
                _ => return Err(Flow::Error(ScriptError::plain(refusal))),
            };
            if checked && curve == Some(true) {
                let state = engine.state.borrow();
                let tree = clip::EngineTree { state: &state, reflection: &engine.reflection };
                curve_clip::validate(&tree, instance).map_err(|problem| Flow::Error(ScriptError::plain(problem)))?;
            }
            let uri = if active {
                let mut loader = source::loader(engine).borrow_mut();
                // (AnimationClipProvider's list is the one that checks its clips)
                let list = if checked { &mut loader.active_clips } else { &mut loader.active_sequences };
                list.push(instance);
                let uri = format!("active://{}", list.len() - 1);
                drop(loader);
                engine.state.borrow_mut().dom.pin(instance);
                uri
            } else {
                source::register(engine, instance, curve)
            };
            Ok(values([LuaValue::String(lua.create_string(&uri)?)]))
        })
    };
    add(behaviors, "KeyframeSequenceProvider", "RegisterKeyframeSequence", register(false, true, false, "RegisterKeyframeSequence requires a KeyframeSequence"));
    add(behaviors, "KeyframeSequenceProvider", "RegisterActiveKeyframeSequence", register(true, false, false, "Argument must be a 'KeyframeSequence' object"));
    add(behaviors, "AnimationClipProvider", "RegisterAnimationClip", register(false, true, true, "Invalid AnimationClip instance passed to registerAnimationClip"));
    add(behaviors, "AnimationClipProvider", "RegisterActiveAnimationClip", register(true, true, true, "Argument must be an 'AnimationClip' object"));

    curves::install(engine, behaviors);

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
    Marker(String, Vec<u8>),
    DidLoop,
    Stopped,
    Ended,
}

/// A stretch of a clip a step passes: from the lower time to the higher, each end counting when its flag says so.
type Stretch = (f32, f32, bool, bool);

/// The markers, then the named keyframes, of `clip` inside `stretches`, a stretch after the other, each from its low
/// end up or (`back`) from its high end down. Within a stretch a CurveAnimation's MarkerCurves fire one after the
/// other in the clip's order, each its own markers in the step's direction (measured: curves a, b, c with one
/// marker each at 0, 0.25 and 0.5 s fire b then c going backwards from 0.7 to 0.2 s, where one curve holding both
/// fires the later first).
fn passed(clip: &Clip, stretches: &[Stretch], back: bool, out: &mut Vec<Fire>) {
    let inside = |time: f32, &(low, high, low_counts, high_counts): &Stretch| {
        (time > low || low_counts && time == low) && (time < high || high_counts && time == high)
    };
    if !clip.markers.is_empty() {
        for stretch in stretches {
            let mut found: Vec<&clip::Marker> = clip.markers.iter().filter(|marker| inside(marker.time, stretch)).collect();
            if back {
                found.reverse();
            }
            found.sort_by_key(|marker| marker.curve);
            out.extend(found.into_iter().map(|marker| Fire::Marker(marker.name.clone(), marker.value.clone())));
        }
    }
    if !clip.keyframes.is_empty() {
        for stretch in stretches {
            let found = clip.keyframes.iter().filter(|(time, _)| inside(*time, stretch)).map(|(_, name)| Fire::Keyframe(name.clone()));
            if back { out.extend(found.rev()) } else { out.extend(found) }
        }
    }
}

/// Advances a playing track by `delta` seconds of the clock, as Studio 0.747 steps one (Animator:StepAnimations in
/// an edit session; docs/engine.md "Animation curves", Stepping): its new time and what it fired. `looped` is the
/// track's Looped.
///
/// The time is a float32 that each step adds `delta * speed` to; the direction is the speed's sign, whatever the
/// delta's. A looped track's new time is taken into the clip (the last key's time is 0 s); one that does not loop
/// is held inside it and ends when it reaches the last key forwards or 0 s backwards: Stopped, and a fade-out over
/// 0.3 s. DidLoop fires whenever the sum left the clip, looped or not (landing on an end exactly fires none).
///
/// A step fires the markers and named keyframes from where it starts up to, but not at, where it stops, so one at
/// the time a step lands on fires with the next step that moves and one at the start time with the first. When the
/// new time lies behind the old one in the track's direction, the track is taken to have gone round: the rest of
/// the clip to its end, the end's own, and from the other end to the new time. A CurveAnimation's track does so
/// looped or not and fires KeyframeReached("End") after the markers; a KeyframeSequence's does so only when
/// looped (not looping, such a step fires nothing) and has no "End". A step of several rounds lands where the
/// remainder puts it and fires only what lies between the old time and the new. That covers a delta below 0 (a
/// track at 0.35 s stepped by -0.2 s is at 0.15 s having fired everything from 0.35 s round to 0.15 s) and one
/// that is no number (the time is no number from then on; a KeyframeSequence's track fires the rest of the clip
/// once, a CurveAnimation's nothing).
///
/// A clip of Length 0 has no end: its time runs on, below 0 too; a CurveAnimation's markers fire as it passes
/// their times, a KeyframeSequence's never.
fn advance(track: &mut Track, clip: &Clip, delta: f64, looped: bool, out: &mut Vec<Fire>) {
    let length = clip.length;
    let from = track.time as f32;
    let raw = from + (delta * track.speed) as f32;
    let back = track.speed < 0.0;
    if length <= 0.0 {
        if clip.curve {
            passed(clip, &[if back { (raw, from, false, true) } else { (from, raw, true, false) }], back, out);
        }
        track.time = raw as f64;
        return;
    }
    let beyond = raw > length || raw < 0.0;
    let ends = !looped && if back { raw <= 0.0 } else { raw >= length };
    let to = if ends {
        if back { 0.0 } else { length }
    } else if !looped {
        raw.clamp(0.0, length)
    } else if raw == length {
        0.0
    } else {
        raw.rem_euclid(length)
    };
    // (a time that is no number compares as neither: a CurveAnimation then fires nothing, a looped KeyframeSequence goes round)
    let behind = if back { to > from } else { to < from };
    let ahead = if back { to <= from } else { to >= from };
    let round = !ends && if clip.curve { behind } else { looped && !ahead && !from.is_nan() };
    let mut stretches: [Stretch; 3] = [(0.0, 0.0, false, false); 3];
    let count = if ends || round {
        if back {
            stretches[0] = (0.0, from, false, true);
            stretches[1] = (0.0, 0.0, true, true);
            stretches[2] = (to, length, false, true);
        } else {
            stretches[0] = (from, length, true, false);
            stretches[1] = (length, length, true, true);
            stretches[2] = (0.0, to, true, false);
        }
        if ends { 2 } else { 3 }
    } else if ahead {
        stretches[0] = if back { (to, from, false, true) } else { (from, to, true, false) };
        1
    } else {
        0
    };
    passed(clip, &stretches[..count], back, out);
    if clip.curve && (ends || round) {
        out.push(Fire::Keyframe("End".to_string()));
    }
    track.time = to as f64;
    if ends {
        track.playing = false;
        fade_to(track, 0.0, END_FADE);
        out.push(Fire::Stopped);
    }
    if beyond {
        out.push(Fire::DidLoop);
    }
}

/// A track that stopped and is fading out still moves (measured: one set to 0.25 s after its end is at 0.375 s
/// after a step of 0.125 s, backwards at 0.125 s), firing nothing. It is held inside the clip, or goes round a
/// looped one (not measured).
fn drift(track: &mut Track, clip: &Clip, delta: f64, looped: bool) {
    let length = clip.length;
    let raw = track.time as f32 + (delta * track.speed) as f32;
    track.time = if length <= 0.0 {
        raw
    } else if looped {
        raw.rem_euclid(length)
    } else {
        raw.clamp(0.0, length)
    } as f64;
}

/// Moves the weight `delta` seconds along its fade, in float32 as Studio does (62 measured weights of seven fades
/// agree to the last digit): what is left of the fade shrinks by `delta`, and the distance to the target shrinks
/// by the same ratio; with nothing left the weight is the target. So Play(0.3) stepped by 0.05 s reads 0.166666687,
/// 0.333333313, 0.5, and a delta below 0 on a finished fade divides by 0: the weight is no number from then on.
fn fade_step(track: &mut Track, delta: f64) {
    let left = track.fade_left - delta as f32;
    if left > 0.0 {
        track.weight = track.weight_target + (track.weight - track.weight_target) * (left / track.fade_left);
        track.fade_left = left;
    } else {
        track.weight = track.weight_target;
        track.fade_left = 0.0;
    }
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
                track.weight = 0.0;
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
    step_of(engine, delta, None);
}

/// [`step`] for every animator, or for `only` (Animator:StepAnimations).
fn step_of(engine: &Rc<Engine>, delta: f64, only: Option<InstanceId>) {
    let Some((shared, names)) = engine.extension::<AnimationExtension>().and_then(|found| found.state.borrow().clone()) else { return };
    if !shared.borrow().pending.is_empty() {
        arrive(engine, &shared, &names);
    }
    if shared.borrow().active.is_empty() {
        return;
    }
    let active: Vec<InstanceId> = {
        let animations = shared.borrow();
        animations.active.iter().copied().filter(|id| only.is_none_or(|only| animations.tracks[id].animator == only)).collect()
    };
    for id in active {
        refresh(engine, &shared, id);
        let mut fired = Vec::new();
        let (alive, looped, set_time) = {
            let state = engine.state.borrow();
            let track = &shared.borrow().tracks[&id];
            // (a destroyed rig stops its tracks; a destroyed track plays on, measured: its pose stays and moves)
            let alive = state.dom.get(track.animator).is_some_and(|instance| !instance.has(crate::dom::flags::DESTROYED)) && state.dom.get(id).is_some();
            let looped = state.dom.boolean(id, names.looped).unwrap_or(false);
            let set_time = state.dom.number(id, names.time_position).filter(|&t| t != track.time && !(t.is_nan() && track.time.is_nan()));
            (alive, looped, set_time)
        };
        let (finished, time, weight, playing, waiting) = {
            let mut animations = shared.borrow_mut();
            let track = animations.tracks.get_mut(&id).expect("active track");
            // (a track whose asset is on the way holds: TimePosition as set, weight 0)
            let waiting = track.pending.is_some();
            let mut finished = !alive;
            // (measured: a track whose weight is no number, after a delta below 0 on a finished fade, is stepped no
            // more: its time, weight and IsPlaying stay)
            if alive && !waiting && track.weight.is_finite() {
                if track.fade_left <= 0.0 && (!track.playing || track.weight_target.abs() <= IDLE_WEIGHT) {
                    // Its fade is over and it is stopped or was faded to nothing: it ends before anything else moves
                    finished = true;
                } else {
                    let clip = track.clip.clone();
                    if let (Some(time), Some(clip)) = (set_time, clip.as_ref()) {
                        // A script set TimePosition: play on from there (what was skipped does not fire). Measured: a clip of
                        // Length 0 takes any time; a looped track takes the time into the clip (5 or 1 of a 1 s clip read
                        // 0, 7.25 reads 0.25, -0.3 reads 0.7, -2.5 reads 0.5), and a CurveAnimation's that lands on 0 s from
                        // below ends a pass (-1 fires "End"); one that does not loop is held inside the clip
                        let (time, length) = (time as f32, clip.length);
                        track.time = if length <= 0.0 {
                            time
                        } else if looped {
                            let inside = time.rem_euclid(length);
                            if clip.curve && time < 0.0 && inside == 0.0 {
                                fired.push(Fire::Keyframe("End".to_string()));
                            }
                            inside
                        } else {
                            time.clamp(0.0, length)
                        } as f64;
                    }
                    fade_step(track, delta);
                    if let Some(clip) = clip {
                        if track.playing { advance(track, &clip, delta, looped, &mut fired) } else { drift(track, &clip, delta, looped) }
                    }
                }
            }
            (finished, track.time, track.weight, track.playing, waiting)
        };
        if finished {
            {
                let mut animations = shared.borrow_mut();
                let track = animations.tracks.get_mut(&id).expect("active track");
                track.active = false;
                if track.playing {
                    // (it was faded to nothing, or its rig was destroyed)
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
        if !finished {
            if !waiting {
                set(&mut state, reflection, id, names.time_position, Value::Number(time));
            }
            set(&mut state, reflection, id, names.weight_current, Value::Number(weight as f64));
        }
        set(&mut state, reflection, id, names.is_playing, Value::Bool(playing && !finished));
        for fire in fired {
            match fire {
                Fire::Keyframe(name) => core::fire_event(&mut state, id, names.keyframe_reached, vec![Value::String(Str::from(name.as_str()))]),
                Fire::Marker(name, value) => {
                    if let Some((_, signal)) = markers.iter().find(|(n, _)| *n == name) {
                        core::fire(&mut state, *signal, vec![Value::String(Str::new(&value))]);
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
    // Each animator's tracks that are still stepped, gathered in one pass over the active ones (every animator looked through
    // all of them: with 60 enemies of two tracks each 7,000 track lookups and clip clones a step), the animators in
    // creation order, not the map's: a joint two animators pose is written by the later one, and a slot is reused when
    // the collector frees an instance, so slot order differs between two runs of one scene
    let mut animators: Vec<(InstanceId, Vec<PosingTrack>)> = Vec::new();
    {
        let animations = shared.borrow();
        let state = engine.state.borrow();
        let mut at: FxHashMap<InstanceId, usize> = FxHashMap::default();
        for id in &animations.active {
            let track = &animations.tracks[id];
            if only.is_some_and(|only| track.animator != only) {
                continue;
            }
            let index = *at.entry(track.animator).or_insert_with(|| {
                animators.push((track.animator, Vec::new()));
                animators.len() - 1
            });
            let Some(clip) = track.clip.clone() else { continue };
            // (a weight that is no number poses nothing either, nor does a CurveAnimation at a time that is none)
            if !(track.weight >= POSING_WEIGHT) || clip.curve && track.time.is_nan() {
                continue;
            }
            let priority = state.dom.raw_get(*id, names.priority).and_then(Value::as_enum).map_or(clip::PRIORITY_ACTION, |item| engine.reflection.enum_item_info(item).value as u32);
            animators[index].1.push((clip, track.time as f32, track.weight, priority_rank(priority)));
        }
        animators.sort_by_key(|(id, _)| state.dom.get(*id).map_or(0, |instance| instance.serial));
    }
    for (animator, tracks) in animators {
        pose(engine, &shared, &names, animator, tracks);
    }
}

/// A playing track as `pose` blends it: its clip, time, weight and priority rank.
type PosingTrack = (Arc<Clip>, f32, f32, u32);

/// A rig's joints by the names an animation's Poses give them.
struct Rig {
    motors: FxHashMap<(Str, Str), InstanceId>,
    bones: FxHashMap<(Str, Str), InstanceId>,
    /// The joints by the one name a CurveAnimation gives them: a Motor6D by its Part1's name, else a Bone by its own
    named: FxHashMap<Str, InstanceId>,
    /// Every Motor6D and Bone of the rig, in the model's order
    all: Vec<InstanceId>,
    /// The rig's FaceControls: the first instance of that class and name in the model
    face: Option<InstanceId>,
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
    let mut rig = Rig { motors: FxHashMap::default(), bones: FxHashMap::default(), named: FxHashMap::default(), all: Vec::new(), face: None, scale };
    let mut named_bones: Vec<(Str, InstanceId)> = Vec::new();
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
        if rig.face.is_none() && Some(state.dom.at(id).class) == names.face_controls && state.dom.at(id).name.as_bytes() == b"FaceControls" && !in_carried_rig(id) {
            rig.face = Some(id);
        }
        if !(class.is_a(names.motor6d) || class.is_a(names.bone)) || in_carried_rig(id) {
            continue;
        }
        rig.all.push(id);
        if class.is_a(names.motor6d) {
            let part = |name| state.dom.raw_get(id, name).and_then(Value::as_instance).and_then(|part| state.dom.get(part)).map(|part| part.name.clone());
            if let (Some(part0), Some(part1)) = (part(names.part0), part(names.part1)) {
                rig.named.entry(part1.clone()).or_insert(id);
                rig.motors.entry((part0, part1)).or_insert(id);
            }
        } else if class.is_a(names.bone) {
            named_bones.push((state.dom.at(id).name.clone(), id));
            if let Some(parent) = state.dom.parent_of(id).and_then(|parent| state.dom.get(parent)) {
                rig.bones.entry((parent.name.clone(), state.dom.at(id).name.clone())).or_insert(id);
            }
        }
    }
    for (name, id) in named_bones {
        rig.named.entry(name).or_insert(id);
    }
    Some(rig)
}

/// One track's sample of a joint: (priority rank, weight, rotation, translation).
pub type Layer = (u32, f32, Quat, Vec3A);

/// The blended offset of a joint: priorities from the highest down each take their summed weight, at most what is
/// left of 1, blending their tracks by weight; the rest stays at the rest pose. Measured in Studio 0.747 (13 mixes of
/// five tracks at four priorities, a KeyframeSequence's and a CurveAnimation's alike): two Action tracks of weight
/// 0.25 at 8 and 2 studs under an Action2 track of weight 0.5 at 4 studs give 4.5, the lower level taking all of the
/// half that is left, not half of it.
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
        // (in float32 as measured: the weighted sum, over the level's weight, times its share: 8 studs at 0.4 with 2 at
        // 0.8 give 4, and with 2 at 0.2 give 3.60000014)
        for &(_, weight, q, t) in group {
            let weight = weight.max(0.0);
            level_rotation += Vec4::from(align(q)) * weight;
            level_translation += t * weight;
        }
        let share = total.min(remaining);
        rotation += level_rotation.normalize_or_zero() * share;
        translation += level_translation / total * share;
        remaining -= share;
    }
    rotation += Vec4::from(Quat::IDENTITY) * remaining.max(0.0);
    let rotation = Quat::from_vec4(rotation.normalize_or_zero());
    (if rotation.length_squared() > 0.0 { rotation } else { Quat::IDENTITY }, translation)
}

/// Writes `animator`'s blended pose into its rig's joints, and the rest pose into every other joint of the rig
/// (measured: while a track of the animator is stepped, a Transform a script wrote to a joint no track poses is back
/// at the identity after the step, a Bone's too; `tracks` are the ones that pose, possibly none).
fn pose(engine: &Rc<Engine>, shared: &Shared, names: &Names, animator: InstanceId, tracks: Vec<PosingTrack>) {
    let reflection = &engine.reflection;
    // (joints in the order the tracks first pose them: hash order follows slots, which differ between two runs of one scene)
    // (with each joint, the layers of the tracks that turn it, once a track that does not has posed it: a CurveAnimation's
    // joint without a Rotation curve)
    let mut joints: Vec<(InstanceId, Vec<Layer>, Option<Vec<Layer>>)> = Vec::new();
    let mut joint_at: FxHashMap<InstanceId, usize> = FxHashMap::default();
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    let Some(rig) = cached_rig(shared, &mut state, reflection, names, animator) else { return };
    let scale = rig.scale;
    for &(ref clip, time, weight, rank) in &tracks {
        for joint in &clip.joints {
            let target = if clip.curve {
                rig.named.get(&Str::from(joint.name.as_str())).copied()
            } else {
                let key = (Str::from(joint.parent.as_str()), Str::from(joint.name.as_str()));
                rig.motors.get(&key).or_else(|| rig.bones.get(&key)).copied()
            };
            if let Some(target) = target {
                let (q, t) = clip::sample(&joint.keys, time, clip.even);
                let at = *joint_at.entry(target).or_insert_with(|| {
                    joints.push((target, Vec::new(), None));
                    joints.len() - 1
                });
                let (_, layers, turning) = &mut joints[at];
                if !joint.rotation && turning.is_none() {
                    *turning = Some(layers.clone());
                }
                if let Some(turning) = turning.as_mut().filter(|_| joint.rotation) {
                    turning.push((rank, weight, q, t));
                }
                layers.push((rank, weight, q, t));
            }
        }
    }
    for (joint, mut layers, turning) in joints {
        let (mut rotation, translation) = blend(&mut layers);
        if let Some(mut turning) = turning {
            rotation = blend(&mut turning).0;
        }
        write_transform(&mut state, reflection, &hooks, names, joint, Affine3A::from_rotation_translation(rotation, (translation * scale).into()));
    }
    for &joint in &rig.all {
        if !joint_at.contains_key(&joint) && state.dom.get(joint).is_some() {
            write_transform(&mut state, reflection, &hooks, names, joint, Affine3A::IDENTITY);
        }
    }
    // The rig's FaceControls: each property a posing track has a channel for takes the tracks' blend, held in 0 to 1.
    // A property no posing track has is not written, so unlike a joint it keeps what it last had (measured: 0.9
    // fading out over 0.3 s reads 0.6, 0.3 and then stays 0.3).
    let Some(face) = rig.face.filter(|&face| state.dom.get(face).is_some()) else { return };
    let class = state.dom.at(face).class;
    let mut values: Vec<(NameId, Vec<(u32, f32, f32)>)> = Vec::new();
    for &(ref clip, time, weight, rank) in &tracks {
        for channel in &clip.face {
            let Some(name) = reflection.name_id(&channel.name) else { continue };
            // (the class's own properties, all numbers: a curve named Name or after nothing drives nothing)
            if !reflection.class_property(class, name).is_some_and(|info| info.owner == class && !info.read_only) {
                continue;
            }
            let value = clip::sample_value(&channel.keys, time, clip.even);
            match values.iter_mut().find(|(kept, _)| *kept == name) {
                Some((_, layers)) => layers.push((rank, weight, value)),
                None => values.push((name, vec![(rank, weight, value)])),
            }
        }
    }
    for (name, mut layers) in values {
        let value = blend_values(&mut layers).clamp(0.0, 1.0);
        core::raw_set_unsignalled(&mut state, reflection, &hooks, face, name, Value::Number(value as f64));
    }
}

/// [`blend`] for a number whose rest is 0 (a FaceControls property; measured with the same mixes: 0.8 and 0.2 at
/// weight 0.25 each under a higher 0.4 at weight 0.5 give 0.45).
pub fn blend_values(layers: &mut [(u32, f32, f32)]) -> f32 {
    layers.sort_by(|a, b| b.0.cmp(&a.0));
    let mut remaining = 1.0f32;
    let mut value = 0.0f32;
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
        let level: f32 = group.iter().map(|&(_, weight, value)| value * weight.max(0.0)).sum();
        let share = total.min(remaining);
        value += level / total * share;
        remaining -= share;
    }
    value
}

/// Sets a joint's Transform as an Animator does: no Changed event (not measured), a Motor6D's
/// joining the batch applied after PreSimulation.
fn write_transform(state: &mut State, reflection: &Reflection, hooks: &crate::Hooks, names: &Names, joint: InstanceId, transform: Affine3A) {
    let value = crate::world::frame::cframe_value(&transform);
    if core::raw_set_unsignalled(state, reflection, hooks, joint, names.transform, value) {
        crate::world::joints::property_changed(state, reflection, hooks, joint, names.transform, None);
    }
}

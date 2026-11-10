//! AnimationTracks: sampling, easing and blending (Rust), and the script API in a running game:
//! playback, fades, priorities, looping, markers, Stopped / Ended, loading from files.

use std::rc::Rc;

use glam::{Quat, Vec3A};
use mlua::prelude::*;

use super::clip::{self, Key, ease, sample};
use super::{Loading, blend, enable};
use crate::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
};

fn key(time: f32, x: f32, style: u32, direction: u32) -> Key {
    Key { time, translation: Vec3A::new(x, 0.0, 0.0), rotation: Quat::IDENTITY, style, direction }
}

#[test]
fn easing_ends_and_legacy_directions() {
    for style in 0..=5 {
        for direction in 0..=2 {
            if style == 1 {
                continue;
            }
            assert!(ease(style, direction, 0.0).abs() < 1e-4, "{style} {direction}");
            assert!((ease(style, direction, 1.0) - 1.0).abs() < 1e-4, "{style} {direction}");
        }
    }
    // Constant holds the earlier key; legacy Cubic "In" eases out, CubicV2 "In" eases in
    assert_eq!(ease(1, 0, 0.9), 0.0);
    assert!(ease(3, 0, 0.25) > 0.25);
    assert!(ease(5, 0, 0.25) < 0.25);
}

#[test]
fn sampling_holds_and_interpolates() {
    let keys = [key(0.5, 1.0, 0, 0), key(1.5, 3.0, 0, 0)];
    assert_eq!(sample(&keys, 0.0).1.x, 1.0);
    assert_eq!(sample(&keys, 1.0).1.x, 2.0);
    assert_eq!(sample(&keys, 9.0).1.x, 3.0);
    let constant = [key(0.0, 0.0, 1, 0), key(1.0, 5.0, 0, 0)];
    assert_eq!(sample(&constant, 0.99).1.x, 0.0);
    assert_eq!(sample(&constant, 1.0).1.x, 5.0);
    let rotated = [
        Key { rotation: Quat::IDENTITY, ..key(0.0, 0.0, 0, 0) },
        Key { rotation: Quat::from_rotation_x(1.0), ..key(1.0, 0.0, 0, 0) },
    ];
    assert!((sample(&rotated, 0.5).0.angle_between(Quat::from_rotation_x(0.5))).abs() < 1e-5);
}

#[test]
fn blending_by_priority_then_weight() {
    let x = |angle: f32| Quat::from_rotation_x(angle);
    let angle = |(q, _): (Quat, Vec3A)| q.angle_between(Quat::IDENTITY) * q.x.signum();
    // a higher priority at full weight overrides
    assert!((angle(blend(&mut [(1, 1.0, x(1.0), Vec3A::ZERO), (3, 1.0, x(0.2), Vec3A::ZERO)])) - 0.2).abs() < 1e-5);
    // one priority blends by weight
    let (q, t) = blend(&mut [(3, 1.0, x(0.0), Vec3A::X), (3, 3.0, x(0.8), Vec3A::ZERO)]);
    assert!((q.angle_between(Quat::IDENTITY) - 0.6).abs() < 2e-2, "{q:?}");
    assert!((t.x - 0.25).abs() < 1e-5);
    // a fading higher priority leaves the rest to the lower one
    assert!((angle(blend(&mut [(1, 1.0, x(1.0), Vec3A::ZERO), (3, 0.5, x(0.0), Vec3A::ZERO)])) - 0.5).abs() < 2e-2);
    // weight below 1 with nothing under it blends toward the rest pose
    let (_, t) = blend(&mut [(2, 0.25, Quat::IDENTITY, Vec3A::new(4.0, 0.0, 0.0))]);
    assert!((t.x - 1.0).abs() < 1e-5);
    assert_eq!(clip::priority_rank(1000), 0);
    assert!(clip::priority_rank(0) < clip::priority_rank(5));
}

fn start(mode: &str) -> (Lua, Rc<Engine>) {
    let engine = Engine::new(Options { mode: mode.into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    (lua, engine)
}

/// Runs `source` for `seconds` of play; panics with the script's error. Returns the output lines.
fn run(lua: &Lua, engine: &Rc<Engine>, source: &str, seconds: f64) -> Vec<String> {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    let function = lua.load(format!("{RIG}\n{source}")).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    crate::scheduler::run_until(lua, engine, seconds).expect("run");
    match call.outcome().unwrap() {
        CallOutcome::Failed(message, trace) => panic!("test script failed: {message}\n{}", trace.unwrap_or_default()),
        CallOutcome::Pending => panic!("test script did not finish"),
        CallOutcome::Finished(_) => {}
    }
    engine.state.borrow().output.entries.iter().map(|line| format!("{}: {}", line.kind, line.message)).collect()
}

fn check(source: &str, seconds: f64) {
    let (lua, engine) = start("play");
    run(&lua, &engine, source, seconds);
}

/// A rig (root, torso, arm joined by Motor6Ds), and a sequence builder registered with
/// KeyframeSequenceProvider: `Sequence({ {Time, ArmAngle, Name?, Markers?} ... }, Loop, Priority)`.
const RIG: &str = r#"
local function near(a, b, tolerance) return math.abs(a - b) < (tolerance or 1e-3) end
local function angle(cframe) local x = cframe:ToEulerAnglesXYZ() return x end
local Provider = game:GetService("KeyframeSequenceProvider")
local function Rig()
	local Model = Instance.new("Model")
	local Root = Instance.new("Part")
	Root.Name = "HumanoidRootPart"
	Root.Anchored = true
	Root.CFrame = CFrame.new(0, 3, 0)
	Root.Parent = Model
	local Torso = Instance.new("Part")
	Torso.Name = "Torso"
	Torso.Parent = Model
	local Arm = Instance.new("Part")
	Arm.Name = "Right Arm"
	Arm.Size = Vector3.new(1, 2, 1)
	Arm.Parent = Model
	local RootJoint = Instance.new("Motor6D")
	RootJoint.Name = "RootJoint"
	RootJoint.Part0, RootJoint.Part1 = Root, Torso
	RootJoint.Parent = Root
	local Shoulder = Instance.new("Motor6D")
	Shoulder.Name = "Right Shoulder"
	Shoulder.Part0, Shoulder.Part1 = Torso, Arm
	Shoulder.C0 = CFrame.new(1.5, 0.5, 0)
	Shoulder.C1 = CFrame.new(0, 0.5, 0)
	Shoulder.Parent = Torso
	local Humanoid = Instance.new("Humanoid")
	Humanoid.Parent = Model
	local Animator = Instance.new("Animator")
	Animator.Parent = Humanoid
	Model.Parent = workspace
	return Model, Animator, Shoulder, Arm
end
local function Sequence(Keys, Loop, Priority)
	local Sequence = Instance.new("KeyframeSequence")
	Sequence.Loop = Loop == true
	Sequence.Priority = Priority or Enum.AnimationPriority.Action
	for _, Entry in Keys do
		local Keyframe = Instance.new("Keyframe")
		Keyframe.Time = Entry[1]
		if Entry[3] then Keyframe.Name = Entry[3] end
		local Root = Instance.new("Pose")
		Root.Name = "HumanoidRootPart"
		Root.Weight = 0
		Root.Parent = Keyframe
		local Torso = Instance.new("Pose")
		Torso.Name = "Torso"
		Torso.Weight = 0
		Torso.Parent = Root
		local Arm = Instance.new("Pose")
		Arm.Name = "Right Arm"
		Arm.CFrame = CFrame.Angles(Entry[2], 0, 0)
		Arm.Parent = Torso
		for Name, Value in Entry[4] or {} do
			local Marker = Instance.new("KeyframeMarker")
			Marker.Name = Name
			Marker.Value = Value
			Marker.Parent = Keyframe
		end
		Keyframe.Parent = Sequence
	end
	local Animation = Instance.new("Animation")
	Animation.Name = "Swing"
	Animation.AnimationId = Provider:RegisterKeyframeSequence(Sequence)
	return Animation
end
"#;

#[test]
fn a_track_poses_its_rig_over_time() {
    check(
        r#"
        local Model, Animator, Shoulder, Arm = Rig()
        local Track = Animator:LoadAnimation(Sequence({ {0, 0}, {1, 1} }))
        assert(Track:IsA("AnimationTrack") and Track.Name == "Swing", "a track named after its animation")
        assert(near(Track.Length, 1) and not Track.IsPlaying and Track.Looped == false, "Length / IsPlaying / Looped")
        assert(Track.Priority == Enum.AnimationPriority.Action, tostring(Track.Priority))
        assert(Track.Animation and Track.Speed == 1 and Track.WeightTarget == 1 and Track.WeightCurrent == 0)
        local Rest = Arm.CFrame
        Track:Play(0)
        assert(Track.IsPlaying and Track.TimePosition == 0)
        assert(#Animator:GetPlayingAnimationTracks() == 1 and Model.Humanoid:GetPlayingAnimationTracks()[1] == Track)
        task.wait(0.5)
        assert(near(Track.TimePosition, 0.5, 0.03), "TimePosition " .. Track.TimePosition)
        assert(near(angle(Shoulder.Transform), Track.TimePosition, 0.02), "Transform " .. angle(Shoulder.Transform) .. " at " .. Track.TimePosition)
        local Posed = Model.Torso.CFrame * Shoulder.C0 * Shoulder.Transform * Shoulder.C1:Inverse()
        assert((Arm.Position - Posed.Position).Magnitude < 1e-3, "the arm follows its Transform")
        assert((Arm.Position - Rest.Position).Magnitude > 0.1, "the arm moved")
        -- frozen, then a jump
        Track:AdjustSpeed(0)
        local Held = Track.TimePosition
        task.wait(0.2)
        assert(Track.TimePosition == Held and Track.Speed == 0, "AdjustSpeed(0) holds")
        Track.TimePosition = 0.9
        task.wait()
        assert(near(angle(Shoulder.Transform), 0.9, 0.02), "TimePosition jumps")
        Track:AdjustSpeed(1)
        "#,
        2.0,
    );
}

/// A model can carry other rigs inside it, each with its own Humanoid or AnimationController and joints named like the
/// outer rig's. A track poses the joints of the rig its animator belongs to, even when a carried rig comes first in the tree.
#[test]
fn a_track_poses_its_own_rig_and_not_a_rig_carried_inside_it() {
    check(
        r#"
        local Carried, _, CarriedShoulder = Rig()
        Carried.Name = "Carried"
        Carried.Humanoid:Destroy()
        local Controller = Instance.new("AnimationController")
        Controller.Parent = Carried
        local CarriedAnimator = Instance.new("Animator")
        CarriedAnimator.Parent = Controller
        local Model, Animator, Shoulder = Rig()
        Carried.Parent = Model
        for _, Child in Model:GetChildren() do
            if Child ~= Carried then
                Child.Parent = nil
                Child.Parent = Model
            end
        end
        assert(Model:GetChildren()[1] == Carried, "the carried rig comes first")
        local Track = Animator:LoadAnimation(Sequence({ {0, 0}, {1, 1} }))
        Track:Play(0)
        task.wait(0.5)
        assert(near(angle(Shoulder.Transform), Track.TimePosition, 0.02), "the outer rig's shoulder " .. angle(Shoulder.Transform))
        assert(near(angle(CarriedShoulder.Transform), 0), "the carried rig's shoulder " .. angle(CarriedShoulder.Transform))
        Track:Stop(0)
        local CarriedTrack = CarriedAnimator:LoadAnimation(Sequence({ {0, 0}, {1, 1} }))
        CarriedTrack:Play(0)
        task.wait(0.5)
        assert(near(angle(CarriedShoulder.Transform), CarriedTrack.TimePosition, 0.02), "its own track poses the carried rig " .. angle(CarriedShoulder.Transform))
        "#,
        2.0,
    );
}

/// `pose` rebuilt each posed rig's joint maps (a walk over every descendant of the model) every step: 0.046 host ms a frame for 60
/// enemies in ree's `(ree engine step: animation)` row. They are kept per animator while nothing inside the model
/// changed, and rebuilt when a Name, Part0, Part1 or Scale is written inside it or a child is added or removed: renaming the arm's
/// part stops the track posing the shoulder, naming it back poses it again.
#[test]
fn a_rigs_joint_maps_are_built_once_and_follow_changes_inside_it() {
    let (lua, engine) = start("play");
    super::RIG_BUILDS.with(|builds| builds.set(0));
    run(
        &lua,
        &engine,
        r#"
        local Model, Animator, Shoulder, Arm = Rig()
        local Track = Animator:LoadAnimation(Sequence({ {0, 0}, {1, 1} }, true))
        Track:Play(0)
        task.wait(0.5)
        assert(angle(Shoulder.Transform) > 0.3, "posed " .. angle(Shoulder.Transform))
        Arm.Name = "Left Arm"
        task.wait(0.1)
        assert(near(angle(Shoulder.Transform), 0, 1e-3), "a part the animation does not name is not posed: " .. angle(Shoulder.Transform))
        Arm.Name = "Right Arm"
        task.wait(0.2)
        assert(angle(Shoulder.Transform) > 0.05, "named back, posed again: " .. angle(Shoulder.Transform))
        print("follows")
        "#,
        1.5,
    );
    let builds = super::RIG_BUILDS.with(|builds| builds.get());
    assert!((3..=5).contains(&builds), "{builds} rig builds over 50 posed frames and two renames (about 50 without the cache)");
}

/// A Pose moves a Bone only when the bone's parent has the parent Pose's name, as a Motor6D needs both names. Measured in the
/// real engine (docs/engine.md, Log 2026-11-09): a clip's root bone keyed under its part's Pose stayed at rest once the part
/// was renamed, while the bones under it, whose parent Pose is that root bone, still played.
#[test]
fn a_bone_is_posed_only_under_its_parent_poses_name() {
    check(
        r#"
        local function BoneRig(PartName)
            local Model = Instance.new("Model")
            local Body = Instance.new("Part")
            Body.Name = PartName
            Body.Anchored = true
            Body.Parent = Model
            local Root = Instance.new("Bone")
            Root.Name = "Root"
            Root.Parent = Body
            local Tip = Instance.new("Bone")
            Tip.Name = "Tip"
            Tip.Position = Vector3.new(0, 1, 0)
            Tip.Parent = Root
            local Controller = Instance.new("AnimationController")
            Controller.Parent = Model
            local Animator = Instance.new("Animator")
            Animator.Parent = Controller
            Model.Parent = workspace
            return Animator, Body, Root, Tip
        end
        local Sequence = Instance.new("KeyframeSequence")
        Sequence.Loop = true
        for _, Time in { 0, 1 } do
            local Keyframe = Instance.new("Keyframe")
            Keyframe.Time = Time
            local Part = Instance.new("Pose")
            Part.Name = "Body"
            Part.Weight = 0
            Part.Parent = Keyframe
            local Root = Instance.new("Pose")
            Root.Name = "Root"
            Root.CFrame = CFrame.Angles(0.6, 0, 0)
            Root.Parent = Part
            local Tip = Instance.new("Pose")
            Tip.Name = "Tip"
            Tip.CFrame = CFrame.Angles(0.3, 0, 0)
            Tip.Parent = Root
            Keyframe.Parent = Sequence
        end
        local Animation = Instance.new("Animation")
        Animation.AnimationId = Provider:RegisterKeyframeSequence(Sequence)
        local Named, _, NamedRoot, NamedTip = BoneRig("Body")
        local Renamed, RenamedBody, RenamedRoot, RenamedTip = BoneRig("Mesh")
        Named:LoadAnimation(Animation):Play(0)
        Renamed:LoadAnimation(Animation):Play(0)
        task.wait(0.2)
        assert(near(angle(NamedRoot.Transform), 0.6) and near(angle(NamedTip.Transform), 0.3), "under the Pose's part both bones play")
        assert(near(angle(RenamedRoot.Transform), 0), "a bone under a part the Pose does not name stays at rest: " .. angle(RenamedRoot.Transform))
        assert(near(angle(RenamedTip.Transform), 0.3), "its child bone, under the named root bone, still plays: " .. angle(RenamedTip.Transform))
        RenamedBody.Name = "Body"
        task.wait(0.2)
        assert(near(angle(RenamedRoot.Transform), 0.6), "named back, the root bone plays again: " .. angle(RenamedRoot.Transform))
        "#,
        1.0,
    );
}

/// Two Animators on one model pose its joints, and the one stepped later leaves its pose (a character's cape bones had
/// two). The order is the Animators' creation, not their slots: a slot is reused when the collector frees an instance, which differs
/// between two runs of one scene, and the map of posed Animators visited them by slot hash. Both engines make 300 instances before the
/// scene; the second lets them go, so the scene's Animators take other slots (the serials stay equal).
#[test]
fn animators_pose_in_creation_order_whatever_their_slots() {
    const SCENE: &str = r#"
        local Shoulders = {}
        for Index = 1, 24 do
            local Model, First, Shoulder = Rig()
            local Controller = Instance.new("AnimationController")
            Controller.Parent = Model
            local Second = Instance.new("Animator")
            Second.Parent = Controller
            First:LoadAnimation(Sequence({ {0, 0.3}, {1, 0.3} }, true)):Play(0)
            Second:LoadAnimation(Sequence({ {0, 0.6}, {1, 0.6} }, true)):Play(0)
            Shoulders[Index] = Shoulder
        end
        task.wait(0.3)
        local Winners = {}
        for Index, Shoulder in Shoulders do table.insert(Winners, near(angle(Shoulder.Transform), 0.6, 0.02) and "second" or "first") end
        print("winners " .. table.concat(Winners, ","))
    "#;
    let outputs: Vec<Vec<String>> = [false, true]
        .into_iter()
        .map(|release| {
            let (lua, engine) = start("play");
            run(&lua, &engine, r#"_G.Held = {} for _ = 1, 300 do table.insert(_G.Held, Instance.new("Folder")) end"#, 0.1);
            if release {
                run(&lua, &engine, "_G.Held = nil", 0.2);
            }
            lua.gc_collect().unwrap();
            lua.gc_collect().unwrap();
            let freed = engine_lua::raw::collect(&lua, &engine).unwrap();
            assert_eq!(freed >= 300, release, "freed {freed}");
            run(&lua, &engine, SCENE, 1.0).into_iter().filter(|line| line.starts_with("print: winners")).collect()
        })
        .collect();
    let expected = vec![format!("print: winners {}", vec!["second"; 24].join(","))];
    assert_eq!(outputs[0], expected, "the later Animator of each model leaves its pose");
    assert_eq!(outputs[1], expected, "the scene's Animators took other slots and were stepped in another order");
}

#[test]
fn a_script_made_rig_takes_its_poses_translation() {
    check(
        r#"
        local Model, Animator = Rig()
        assert(Model:GetScale() == 1, "a new model reports scale 1")
        local Sequence = Instance.new("KeyframeSequence")
        for _, Time in { 0, 1 } do
            local Keyframe = Instance.new("Keyframe")
            Keyframe.Time = Time
            local Root = Instance.new("Pose")
            Root.Name = "HumanoidRootPart"
            Root.Weight = 0
            Root.Parent = Keyframe
            local Torso = Instance.new("Pose")
            Torso.Name = "Torso"
            Torso.CFrame = CFrame.new(0, 0, 2)
            Torso.Parent = Root
            Keyframe.Parent = Sequence
        end
        local Animation = Instance.new("Animation")
        Animation.AnimationId = Provider:RegisterKeyframeSequence(Sequence)
        local Track = Animator:LoadAnimation(Animation)
        Track:Play(0)
        task.wait(0.5)
        local Joint = Model.HumanoidRootPart.RootJoint
        assert(near(Joint.Transform.Position.Z, 2, 1e-3), "the pose's translation reaches the Transform: " .. tostring(Joint.Transform.Position))
        assert(near((Model.Torso.Position - Model.HumanoidRootPart.Position).Magnitude, 2, 1e-3), "the torso moves with it")
        "#,
        1.0,
    );
}

#[test]
fn fades_stops_and_ends() {
    check(
        r#"
        local Model, Animator, Shoulder = Rig()
        local Track = Animator:LoadAnimation(Sequence({ {0, 1}, {0.5, 1} }, true))
        local Events = {}
        Track.Stopped:Connect(function() table.insert(Events, "Stopped " .. tostring(Track.IsPlaying)) end)
        Track.Ended:Connect(function() table.insert(Events, "Ended " .. Track.WeightCurrent) end)
        Track:Play(0.4)
        task.wait(0.2)
        assert(near(Track.WeightCurrent, 0.5, 0.06), "fade in " .. Track.WeightCurrent)
        assert(near(angle(Shoulder.Transform), 0.5, 0.06), "half way to the pose " .. angle(Shoulder.Transform))
        task.wait(0.4)
        assert(near(Track.WeightCurrent, 1) and near(angle(Shoulder.Transform), 1, 0.01))
        Track:Stop(0.2)
        assert(not Track.IsPlaying and Track.WeightTarget == 0, "Stop: IsPlaying false at once")
        assert(#Animator:GetPlayingAnimationTracks() == 0)
        task.wait(0.1)
        assert(Events[1] == "Stopped false" and Events[2] == nil, table.concat(Events, ","))
        assert(near(angle(Shoulder.Transform), 0.5, 0.1), "fading out " .. angle(Shoulder.Transform))
        task.wait(0.2)
        assert(Events[2] == "Ended 0", table.concat(Events, ","))
        assert(near(angle(Shoulder.Transform), 0), "back at rest")
        -- a track that is not looped ends by itself, holding its last pose while it fades
        local Once = Animator:LoadAnimation(Sequence({ {0, 0}, {0.3, 1} }))
        local Order = {}
        Once.Stopped:Connect(function() table.insert(Order, "Stopped") end)
        Once.Ended:Connect(function() table.insert(Order, "Ended") end)
        Once.DidLoop:Connect(function() table.insert(Order, "DidLoop") end)
        Once:Play(0)
        task.wait(0.35)
        assert(not Once.IsPlaying and near(Once.TimePosition, 0.3), "ended at its length")
        assert(Order[1] == "Stopped", table.concat(Order, ","))
        task.wait(0.2)
        assert(table.concat(Order, ",") == "Stopped,Ended", table.concat(Order, ","))
        "#,
        2.0,
    );
}

#[test]
fn looping_markers_and_keyframes() {
    check(
        r#"
        local Model, Animator, Shoulder = Rig()
        local Track = Animator:LoadAnimation(Sequence({ {0, 0, "Start"}, {0.25, 0.5, "Hit", { Impact = "big" }}, {0.5, 0} }, true))
        assert(Track.Looped, "Looped comes from the sequence")
        assert(Track:GetTimeOfKeyframe("Hit") == 0.25)
        assert(not pcall(Track.GetTimeOfKeyframe, Track, "Nope"), "an unknown keyframe errors")
        local Seen = {}
        Track.KeyframeReached:Connect(function(Name) table.insert(Seen, Name) end)
        Track:GetMarkerReachedSignal("Impact"):Connect(function(Value) table.insert(Seen, "Impact=" .. Value) end)
        local Loops = 0
        Track.DidLoop:Connect(function() Loops += 1 end)
        Track:Play(0)
        task.wait(1.1)
        assert(Loops == 2, "DidLoop " .. Loops)
        assert(Track.IsPlaying and Track.TimePosition < 0.5)
        assert(table.concat(Seen, ","):sub(1, 23) == "Start,Hit,Impact=big,St", table.concat(Seen, ","))
        local Hits = 0
        for _, Name in Seen do if Name == "Hit" then Hits += 1 end end
        assert(Hits == 2, "Hit fired each loop: " .. table.concat(Seen, ","))
        -- Looped off from a script: it ends at the next end
        Track.Looped = false
        task.wait(0.6)
        assert(not Track.IsPlaying, "ended once Looped was turned off")
        "#,
        2.0,
    );
}

#[test]
fn higher_priorities_override_and_equal_ones_blend() {
    check(
        r#"
        local Model, Animator, Shoulder = Rig()
        local Idle = Animator:LoadAnimation(Sequence({ {0, 0.2}, {1, 0.2} }, true, Enum.AnimationPriority.Idle))
        local Action = Animator:LoadAnimation(Sequence({ {0, 1}, {1, 1} }, true, Enum.AnimationPriority.Action))
        Idle:Play(0)
        Action:Play(0)
        task.wait(0.1)
        assert(near(angle(Shoulder.Transform), 1, 0.01), "Action over Idle " .. angle(Shoulder.Transform))
        Action.Priority = Enum.AnimationPriority.Idle
        Action:AdjustWeight(1, 0)
        task.wait()
        assert(near(angle(Shoulder.Transform), 0.6, 0.02), "equal priorities blend " .. angle(Shoulder.Transform))
        Action:AdjustWeight(3, 0)
        task.wait()
        assert(near(angle(Shoulder.Transform), 0.8, 0.02), "by weight " .. angle(Shoulder.Transform))
        Action:Stop(0)
        Idle:Stop(0)
        task.wait()
        assert(near(angle(Shoulder.Transform), 0), "rest once nothing plays")
        "#,
        1.0,
    );
}

#[test]
fn humanoids_make_their_animator_and_bad_arguments_raise() {
    check(
        r#"
        local Model, Animator = Rig()
        Animator:Destroy()
        local Track = Model.Humanoid:LoadAnimation(Sequence({ {0, 0}, {1, 1} }))
        assert(Model.Humanoid:FindFirstChildOfClass("Animator"), "Humanoid:LoadAnimation made an Animator")
        local Ok, Message = pcall(function() Model.Humanoid.Animator:LoadAnimation(nil) end)
        assert(not Ok and Message:find("LoadAnimation requires an Animation object"), Message)
        local Played
        Model.Humanoid.Animator.AnimationPlayed:Connect(function(Playing) Played = Playing end)
        Track:Play()
        task.wait()
        assert(Played == Track, "AnimationPlayed")
        -- a destroyed rig stops its tracks
        local Stopped = false
        Track.Stopped:Connect(function() Stopped = true end)
        Model:Destroy()
        task.wait()
        task.wait()
        assert(not Track.IsPlaying and Stopped, "the track stopped with its rig")
        "#,
        0.5,
    );
}

#[test]
fn edit_mode_does_not_animate() {
    let (lua, engine) = start("edit");
    run(
        &lua,
        &engine,
        r#"
        local Model, Animator, Shoulder = Rig()
        local Track = Animator:LoadAnimation(Sequence({ {0, 1}, {1, 1} }))
        Track:Play(0)
        task.wait(0.3)
        assert(Track.TimePosition == 0 and near(angle(Shoulder.Transform), 0), "an Animator does not run in edit mode")
        "#,
        0.5,
    );
}

/// A KeyframeSequence file holding a two-key arm swing.
fn write_sequence(path: &std::path::Path) {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{CFrame, Enum, Matrix3, Variant, Vector3}};
    let pose = |name: &str, angle: f32, weight: f32| {
        let (s, c) = angle.sin_cos();
        let rotation = Matrix3::new(Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, c, -s), Vector3::new(0.0, s, c));
        InstanceBuilder::new("Pose")
            .with_name(name)
            .with_property("Weight", Variant::Float32(weight))
            .with_property("CFrame", Variant::CFrame(CFrame::new(Vector3::new(0.0, 0.0, 0.0), rotation)))
            .with_property("EasingStyle", Variant::Enum(Enum::from_u32(0)))
            .with_property("EasingDirection", Variant::Enum(Enum::from_u32(0)))
    };
    let keyframe = |time: f32, angle: f32| {
        InstanceBuilder::new("Keyframe")
            .with_name("Keyframe")
            .with_property("Time", Variant::Float32(time))
            .with_child(pose("HumanoidRootPart", 0.0, 0.0).with_child(pose("Torso", 0.0, 0.0).with_child(pose("Right Arm", angle, 1.0))))
    };
    let sequence = InstanceBuilder::new("KeyframeSequence")
        .with_name("Swing")
        .with_property("Loop", Variant::Bool(false))
        .with_property("Priority", Variant::Enum(Enum::from_u32(4)))
        .with_child(keyframe(0.0, 0.0))
        .with_child(keyframe(0.8, 1.0));
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let id = dom.insert(root, sequence);
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, &dom, &[id]).expect("write");
    std::fs::write(path, bytes).expect("write file");
}

#[test]
fn sequences_load_from_mounted_files_and_missing_ids_warn_once() {
    let directory = std::env::temp_dir().join(format!("ree-animations-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    write_sequence(&directory.join("4242.rbxm"));
    let (lua, engine) = start("play");
    enable(&engine, Loading { directories: vec![directory.clone()], download: false });
    // (the state dir's cache is the checkout's; nothing is cached under 999999999 there)
    let lines = run(
        &lua,
        &engine,
        r#"
        local Model, Animator, Shoulder = Rig()
        local Animation = Instance.new("Animation")
        Animation.AnimationId = "rbxassetid://4242"
        local Track = Animator:LoadAnimation(Animation)
        repeat task.wait() until Track.Length > 0
        assert(near(Track.Length, 0.8) and Track.Looped == false, "from the file: " .. Track.Length)
        assert(Track.Priority == Enum.AnimationPriority.Action3, tostring(Track.Priority))
        Track:Play(0)
        task.wait(0.4)
        assert(near(angle(Shoulder.Transform), 0.5, 0.05), "posed from the file " .. angle(Shoulder.Transform))
        local Missing = Instance.new("Animation")
        Missing.AnimationId = "rbxassetid://999999999"
        local A = Animator:LoadAnimation(Missing)
        local B = Animator:LoadAnimation(Missing)
        assert(A.Length == 0 and B.Length == 0)
        A:Play()
        task.wait(0.1)
        assert(A.IsPlaying, "a track without keyframes plays on")
        "#,
        1.0,
    );
    let _ = std::fs::remove_dir_all(&directory);
    let warnings: Vec<&String> = lines.iter().filter(|line| line.contains("999999999")).collect();
    assert_eq!(warnings.len(), 1, "{lines:?}");
}

#[test]
fn an_asset_arrives_after_its_first_fetch() {
    let directory = std::env::temp_dir().join(format!("ree-animation-fetch-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    write_sequence(&directory.join("4444.rbxm"));
    write_sequence(&directory.join("4545.rbxm"));
    let (lua, engine) = start("play");
    enable(&engine, Loading { directories: vec![directory.clone()], download: false });
    run(
        &lua,
        &engine,
        r#"
        local Model, Animator, Shoulder = Rig()
        local function Animation(Id)
            local Animation = Instance.new("Animation")
            Animation.AnimationId = Id
            return Animation
        end
        -- on its way: Length 0, the defaults, IsPlaying when played, no pose, TimePosition as set
        local Track = Animator:LoadAnimation(Animation("rbxassetid://4444"))
        assert(Track.Length == 0 and Track.Looped == false and Track.Priority == Enum.AnimationPriority.Action, "on its way")
        Track:Play()
        local Held = Animator:LoadAnimation(Animation("http://www.roblox.com/asset/?id=4444"))
        Held.Priority = Enum.AnimationPriority.Movement
        Held.Looped = false
        Held:Play(0)
        Held.TimePosition = 0.4
        local Waited = task.wait(0.2)
        assert(Track.IsPlaying and Track.TimePosition == 0 and Track.WeightCurrent == 0, "it waits")
        assert(near(Held.TimePosition, 0.4, 1e-6) and angle(Shoulder.Transform) == 0, "no pose before it arrives")
        repeat Waited += task.wait() until Track.Length > 0
        assert(near(Waited, 0.45, 0.04), "the first fetch " .. Waited)
        assert(near(Track.Length, 0.8) and Track.Priority == Enum.AnimationPriority.Action3, "the asset's own")
        assert(near(Held.Length, 0.8) and Held.Priority == Enum.AnimationPriority.Movement, "a track of the same asset shares its fetch; what a script set holds")
        -- a playing track starts there: its time from where it was, its fade from 0
        task.wait(0.05)
        assert(Track.TimePosition > 0 and Track.TimePosition < 0.1 and Track.WeightCurrent > 0 and Track.WeightCurrent < 1, "from its start " .. Track.TimePosition .. " " .. Track.WeightCurrent)
        assert(Held.TimePosition > 0.4 and Held.TimePosition < 0.5, "from the TimePosition set " .. Held.TimePosition)
        -- fetched: at once
        assert(near(Animator:LoadAnimation(Animation("4444")).Length, 0.8), "an asset fetched before loads at once")
        -- PreloadAsync waits for the animations among its items and their descendants
        local Folder = Instance.new("Folder")
        Animation("rbxassetid://4545").Parent = Folder
        local Start = os.clock()
        game:GetService("ContentProvider"):PreloadAsync({ Folder, "rbxassetid://4545" })
        assert(near(os.clock() - Start, 0.45, 0.05), "PreloadAsync waits " .. os.clock() - Start)
        assert(near(Animator:LoadAnimation(Animation("rbxassetid://4545")).Length, 0.8), "preloaded: at once")
        Start = os.clock()
        game:GetService("ContentProvider"):PreloadAsync({ Folder })
        assert(os.clock() - Start < 0.05, "a fetched asset's preload is one frame")
        "#,
        2.0,
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// `Describe(Sequence)`: a KeyframeSequence's tree as text (classes, names, nesting, the values a
/// clip reads), for comparing copies.
const DESCRIBE: &str = r#"
local function Describe(Sequence)
	local Lines = { `{Sequence.ClassName} {Sequence.Name} Loop={Sequence.Loop} {Sequence.Priority.Name}` }
	local function Visit(Node, Depth)
		for _, Child in Node:GetChildren() do
			local Line = `{string.rep(" ", Depth)}{Child.ClassName} {Child.Name}`
			if Child:IsA("Keyframe") then
				Line ..= ` Time={Child.Time}`
			elseif Child:IsA("Pose") then
				Line ..= ` Weight={Child.Weight} X={string.format("%.4f", angle(Child.CFrame))} {Child.EasingStyle.Name} {Child.EasingDirection.Name}`
			elseif Child:IsA("KeyframeMarker") then
				Line ..= ` Value={Child.Value}`
			end
			table.insert(Lines, Line)
			Visit(Child, Depth + 1)
		end
	end
	Visit(Sequence, 1)
	return table.concat(Lines, "\n")
end
"#;

#[test]
fn get_keyframe_sequence_async_copies_a_registered_sequence() {
    check(
        &format!(
            "{DESCRIBE}{}",
            r#"
        local Sequence = Instance.new("KeyframeSequence")
        Sequence.Name = "Wave"
        Sequence.Loop = true
        Sequence.Priority = Enum.AnimationPriority.Movement
        for Index, Time in { 0, 0.5 } do
            local Keyframe = Instance.new("Keyframe")
            Keyframe.Time = Time
            if Index == 1 then Keyframe.Name = "Start" end
            local Root = Instance.new("Pose")
            Root.Name = "HumanoidRootPart"
            Root.Weight = 0
            Root.Parent = Keyframe
            local Torso = Instance.new("Pose")
            Torso.Name = "Torso"
            Torso.Weight = 0.5
            Torso.CFrame = CFrame.Angles(Time, 0, 0)
            Torso.EasingStyle = Enum.PoseEasingStyle.Cubic
            Torso.EasingDirection = Enum.PoseEasingDirection.Out
            Torso.Parent = Root
            local Marker = Instance.new("KeyframeMarker")
            Marker.Name = "Step"
            Marker.Value = tostring(Index)
            Marker.Parent = Keyframe
            Keyframe.Parent = Sequence
        end
        local Id = Provider:RegisterKeyframeSequence(Sequence)
        -- (a `:` call returns through the namecall continuation, a `.` call straight from the stub)
        local Order = {}
        local A
        task.spawn(function()
            A = Provider:GetKeyframeSequenceAsync(Id)
            table.insert(Order, "returned")
        end)
        table.insert(Order, "spawned")
        local B = Provider.GetKeyframeSequenceAsync(Provider, Id)
        assert(table.concat(Order, ",") == "spawned,returned", "the call yields: " .. table.concat(Order, ","))
        assert(typeof(A) == "Instance" and A:IsA("KeyframeSequence") and A.Parent == nil, "an unparented KeyframeSequence")
        assert(A ~= Sequence and B ~= Sequence and A ~= B, "a new copy each call")
        local Original = Describe(Sequence)
        assert(Describe(A) == Original, Describe(A) .. "\n--\n" .. Original)
        assert(Describe(B) == Original, Describe(B))
        local Root = A:FindFirstChild("Start").HumanoidRootPart
        assert(Root:IsA("Pose") and Root.Weight == 0 and Root.Torso.Weight == 0.5, "the weight-0 parent pose and its child")
        Root.Torso.Weight = 1
        assert(Describe(Sequence) == Original and Describe(B) == Original, "copies are their own")
        -- an id that loads nothing raises (this engine loads no files), after the yield
        local Failed = {}
        task.spawn(function()
            local Ok, Message = pcall(function()
                return Provider:GetKeyframeSequenceAsync("rbxassetid://999999999")
            end)
            table.insert(Failed, Ok)
            table.insert(Failed, Message)
        end)
        assert(#Failed == 0, "it yields before it raises")
        local Ok, Message = pcall(Provider.GetKeyframeSequenceAsync, Provider, "rbxassetid://999999999")
        assert(not Ok and Message:find("Failed to load animation rbxassetid://999999999", 1, true) == 1, tostring(Message))
        assert(Failed[1] == false and Failed[2] == Message, tostring(Failed[2]))
        local Missing, Why = pcall(function() return Provider:GetKeyframeSequenceAsync() end)
        assert(not Missing and Why:find("Argument 1 missing or nil", 1, true), tostring(Why))
        "#
        ),
        1.0,
    );
}

#[test]
fn get_keyframe_sequence_async_makes_a_files_sequence() {
    let directory = std::env::temp_dir().join(format!("ree-keyframe-sequences-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    write_sequence(&directory.join("4343.rbxm"));
    let (lua, engine) = start("play");
    enable(&engine, Loading { directories: vec![directory.clone()], download: false });
    run(
        &lua,
        &engine,
        &format!(
            "{DESCRIBE}{}",
            r#"
        local First
        for _, Id in { "rbxassetid://4343", "http://www.roblox.com/asset/?id=4343", "4343" } do
            local Sequence = Provider:GetKeyframeSequenceAsync(Id)
            assert(Sequence:IsA("KeyframeSequence") and Sequence.Name == "Swing" and Sequence.Parent == nil, Id)
            assert(Sequence.Loop == false and Sequence.Priority == Enum.AnimationPriority.Action3, tostring(Sequence.Priority))
            local Keyframes = Sequence:GetChildren()
            assert(#Keyframes == 2 and near(Keyframes[2].Time, 0.8), "the file's keyframes")
            local Root = Keyframes[2].HumanoidRootPart
            assert(Root.Weight == 0 and Root.Torso.Weight == 0, "weight-0 parent poses")
            local Arm = Root.Torso["Right Arm"]
            assert(Arm.Weight == 1 and near(angle(Arm.CFrame), 1), tostring(Arm.CFrame))
            assert(Arm.EasingStyle == Enum.PoseEasingStyle.Linear and Arm.EasingDirection == Enum.PoseEasingDirection.In)
            assert(Sequence ~= First, "a new copy each call")
            if First then
                assert(Describe(Sequence) == Describe(First), Describe(Sequence))
            end
            First = Sequence
        end
        local Ok, Message = pcall(function()
            return Provider:GetKeyframeSequenceAsync("rbxassetid://999999999")
        end)
        assert(not Ok and Message:find("Failed to load animation rbxassetid://999999999", 1, true) == 1, tostring(Message))
        "#
        ),
        1.0,
    );
    let _ = std::fs::remove_dir_all(&directory);
}

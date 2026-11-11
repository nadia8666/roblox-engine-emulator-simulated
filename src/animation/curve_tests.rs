//! Animation curves against Roblox Studio 0.747 (2026-11-11): the curve API (curve_golden/, scripts
//! that run unchanged in Studio, with what Studio returned), the stored form (bytes Studio wrote of
//! curves built for this), and a CurveAnimation playing on a small rig (poses, events, blending and
//! the provider calls Studio gave for the same clips).

use std::rc::Rc;

use glam::Quat;

use super::curve::{self, CUBIC, FloatKey, LINEAR, MarkerKey, RotationKey};
use super::tests::{check, run, start};
use super::{Loading, enable};

/// Runs a golden script (a chunk that returns its text) and returns the lines.
fn golden(source: &str) -> Vec<String> {
    golden_in("play", source, 1.0)
}

/// A golden script's lines in an engine of `mode`, run for `seconds`.
fn golden_in(mode: &str, source: &str, seconds: f64) -> Vec<String> {
    let (lua, engine) = start(mode);
    let lines = run(&lua, &engine, &format!("print((function()\n{source}\nend)())"), seconds);
    let printed: Vec<&String> = lines.iter().filter(|line| line.starts_with("print: ")).collect();
    assert_eq!(printed.len(), 1, "{lines:?}");
    printed[0]["print: ".len()..].lines().map(str::to_string).collect()
}

/// Compares a golden script's lines with Studio's. A token written `~1.25` is a number compared
/// within `tolerance` (rotations: Studio's own float32 arithmetic leaves the seventh digit open).
fn assert_golden(got: &[String], expected: &str, tolerance: f64) {
    let expected: Vec<&str> = expected.lines().collect();
    for (index, (got, want)) in got.iter().zip(&expected).enumerate() {
        if got == want {
            continue;
        }
        let (ours, theirs): (Vec<&str>, Vec<&str>) = (got.split(' ').collect(), want.split(' ').collect());
        assert_eq!(ours.len(), theirs.len(), "line {}:\n ree    {got}\n studio {want}", index + 1);
        for (a, b) in ours.iter().zip(&theirs) {
            let close = match (a.strip_prefix('~').and_then(|a| a.parse::<f64>().ok()), b.strip_prefix('~').and_then(|b| b.parse::<f64>().ok())) {
                (Some(a), Some(b)) => (a - b).abs() <= tolerance,
                _ => a == b,
            };
            assert!(close, "line {}: {a} against Studio's {b}\n ree    {got}\n studio {want}", index + 1);
        }
    }
    assert_eq!(got.len(), expected.len(), "the number of lines");
}

#[test]
fn curve_keys_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/keys.luau")), include_str!("curve_golden/keys.txt"), 0.0);
}

/// Every value to the last bit: Constant, Linear and Cubic with set and unset tangents, the key times' ticks, SetKeys'
/// spacing rule and the members' errors.
#[test]
fn float_curves_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/float_curve.luau")), include_str!("curve_golden/float_curve.txt"), 0.0);
}

#[test]
fn the_other_curves_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/curves.luau")), include_str!("curve_golden/curves.txt"), 2e-6);
}

/// 90 curves of three and five Cubic keys with no tangent set, their steps from equal to a thousand times apart,
/// with peaks, dips and level steps among them: every value is Studio's to the last digit.
#[test]
fn unset_tangents_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/tangents.luau")), include_str!("curve_golden/tangents.txt"), 0.0);
}

/// Which FloatCurve is a Vector3Curve's or EulerRotationCurve's X, Y or Z as children come, go and are renamed.
#[test]
fn channels_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/channels.luau")), include_str!("curve_golden/channels.txt"), 0.0);
}

/// What a marker's value that is no string becomes, and the 64 bytes it may be.
#[test]
fn marker_values_match_studio() {
    assert_golden(&golden(include_str!("curve_golden/markers.luau")), include_str!("curve_golden/markers.txt"), 0.0);
}

/// A CurveAnimation's track stepped with Animator:StepAnimations in an edit session, as the script ran in Studio:
/// times, events and ends, forwards and backwards, looped and not.
#[test]
fn stepped_tracks_match_studio() {
    assert_golden(&golden_in("edit", include_str!("curve_golden/playback_steps.luau"), 60.0), include_str!("curve_golden/playback_steps.txt"), 0.0);
}

/// Keys before 0 s, clips of Length 0, ids of what is no clip and what a registered id stands for.
#[test]
fn clips_at_the_edges_match_studio() {
    assert_golden(&golden_in("edit", include_str!("curve_golden/playback_clips.luau"), 60.0), include_str!("curve_golden/playback_clips.txt"), 0.0);
}

/// Animator:StepAnimations has Plugin security (Roblox's API reference): a game's script may not call it.
#[test]
fn a_games_script_may_not_step_animations() {
    let (lua, engine) = start("play");
    let lines = run(&lua, &engine, r#"print(pcall(function() Instance.new("Animator"):StepAnimations(0.1) end))"#, 0.1);
    assert!(lines.iter().any(|line| line.ends_with("false The current thread cannot call 'StepAnimations' (lacking capability Plugin)")), "{lines:?}");
}

fn float_key(time: f32, value: f32) -> (f32, FloatKey) {
    (time, FloatKey { tick: curve::tick_of(time), value, mode: CUBIC, left: None, right: None })
}

/// A middle key's unset tangent where one segment's step is a hundredth of the other's: Studio returned 4.97512341
/// and 4.95049 at 0.5 s (the two values an earlier form of the rule had one float32 step off).
#[test]
fn an_unset_tangent_between_very_unequal_steps_is_studios() {
    for (last, time, studio) in [(10.1f32, 3.0f32, 4.975_123_4_f32), (10.2, 3.0, 4.950_49)] {
        let keys = curve::set(vec![float_key(0.0, 0.0), float_key(1.0, 10.0), float_key(time, last)]);
        let value = curve::float_at(&keys, 0.5).unwrap();
        assert_eq!(value.to_bits(), studio.to_bits(), "{value} against {studio}");
    }
}

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap()).collect()
}

/// `ValuesAndTimes` as Studio 0.747 wrote it (SerializationService:SerializeInstancesAsync of curves built by script).
#[test]
fn the_stored_form_reads_what_studio_wrote() {
    // FloatCurve: (0 s, 1, Cubic, right 0.25), (1 s, 2, Cubic, left 0.5), (2 s, 3, Cubic, left 0.75, right 1.25),
    // (3 s, 4, Linear, left 2.5)
    let floats = curve::decode_floats(&bytes(
        "020000000400000002020000803f0000803e0000803e0201000000400000003f0000003f0203000040400000403f0000a03f010100008040000020400000204001000000040000000000000060090000c0120000201c0000",
    ));
    let key = |tick, value, mode, left, right| FloatKey { tick, value, mode, left, right };
    assert_eq!(floats, vec![
        key(0, 1.0, CUBIC, None, Some(0.25)),
        key(2400, 2.0, CUBIC, Some(0.5), None),
        key(4800, 3.0, CUBIC, Some(0.75), Some(1.25)),
        key(7200, 4.0, LINEAR, Some(2.5), None),
    ]);
    assert_eq!(curve::decode_floats(&curve::encode_floats(&floats)), floats);
    assert_eq!(curve::decode_floats(&bytes("02000000000000000100000000000000")), vec![]);
    // RotationCurve: (0 s, 0.25 rad about Z, Cubic, tangents 0 and 0.5), (2 s, identity, Cubic, left 1.5),
    // (3 s, identity, Cubic, no tangent)
    let rotations = curve::decode_rotations(&bytes(
        "01000000030000000e00000000000000007955ff3daa007e3f000000000000003f060000000000000000000000000000803f0000c03f00000000020000000000000000000000000000803f0000000000000000010000000300000000000000c0120000201c0000",
    ));
    assert_eq!(rotations.iter().map(|key| (key.tick, key.mode, key.left, key.right)).collect::<Vec<_>>(), vec![
        (0, CUBIC, Some(0.0), Some(0.5)),
        (4800, CUBIC, Some(1.5), None),
        (7200, CUBIC, None, None),
    ]);
    assert!(rotations[0].rotation.angle_between(Quat::from_rotation_z(0.25)) < 1e-6);
    assert_eq!(curve::decode_rotations(&curve::encode_rotations(&rotations)), rotations);
    // MarkerCurve: "half" at 0.5 s, "a longer value" at 1 s, "" at 2 s
    let markers = curve::decode_markers(&bytes("020000000300000068616c660061206c6f6e6765722076616c756500000100000003000000b004000060090000c0120000"));
    let marker = |tick, value: &str| MarkerKey { tick, value: value.as_bytes().to_vec() };
    assert_eq!(markers, vec![marker(1200, "half"), marker(2400, "a longer value"), marker(4800, "")]);
    assert_eq!(curve::decode_markers(&curve::encode_markers(&markers)), markers);
    // Bytes that are not a curve hold no keys (and no count is trusted past the bytes there are)
    for junk in ["", "02", "0200000005000000", "02000000ffffffff01000000ffffffff"] {
        assert!(curve::decode_floats(&bytes(junk)).is_empty() && curve::decode_rotations(&bytes(junk)).is_empty() && curve::decode_markers(&bytes(junk)).is_empty());
    }
    let _: Vec<RotationKey> = rotations;
}

/// The rig and the clips of the Studio session: Root, Arm and Hand joined by Motor6Ds (a Bone "Tail" in the hand),
/// an AnimationController, and builders for the clip's parts. `Pose(Track, Time)` holds a paused track at a time.
const SCENE: &str = r#"
local L, C, K = Enum.KeyInterpolationMode.Linear, Enum.KeyInterpolationMode.Constant, Enum.KeyInterpolationMode.Cubic
local Clips, Sequences = game:GetService("AnimationClipProvider"), game:GetService("KeyframeSequenceProvider")
local function FK(t, v, m) return FloatCurveKey.new(t, v, m) end
local function Float(name, keys, parent) local f = Instance.new("FloatCurve") f.Name = name f:SetKeys(keys) f.Parent = parent return f end
local function Folder(name, parent) local f = Instance.new("Folder") f.Name = name f.Parent = parent return f end
local function PositionX(parent, keys) local p = Instance.new("Vector3Curve") p.Name = "Position" p.Parent = parent Float("X", keys, p) return p end
local function EulerX(parent, keys) local r = Instance.new("EulerRotationCurve") r.Name = "Rotation" r.Parent = parent Float("X", keys, r) return r end
local function Scene()
	local Model = Instance.new("Model")
	local Root = Instance.new("Part") Root.Name = "Root" Root.Anchored = true Root.Parent = Model
	local Arm = Instance.new("Part") Arm.Name = "Arm" Arm.Parent = Model
	local Hand = Instance.new("Part") Hand.Name = "Hand" Hand.Parent = Model
	local Shoulder = Instance.new("Motor6D") Shoulder.Name = "Shoulder" Shoulder.Part0 = Root Shoulder.Part1 = Arm Shoulder.Parent = Root
	local Wrist = Instance.new("Motor6D") Wrist.Name = "Wrist" Wrist.Part0 = Arm Wrist.Part1 = Hand Wrist.Parent = Arm
	local Tail = Instance.new("Bone") Tail.Name = "Tail" Tail.Parent = Hand
	local Controller = Instance.new("AnimationController") Controller.Parent = Model
	local Animator = Instance.new("Animator") Animator.Parent = Controller
	Model.Parent = workspace
	return Animator, Shoulder, Wrist, Tail
end
local function Load(Animator, Clip, Register)
	local Animation = Instance.new("Animation")
	Animation.AnimationId = if Register then Register(Clip) else Clips:RegisterAnimationClip(Clip)
	return Animator:LoadAnimation(Animation), Animation.AnimationId
end
local function Pose(Track, Time)
	if not Track.IsPlaying then Track:Play(0, 1, 0) end
	Track.TimePosition = Time
	task.wait()
end
local function Near(a, b, tolerance) return math.abs(a - b) <= (tolerance or 1e-4) end
-- A CFrame against twelve numbers
local function Is(cframe, expected, tolerance, what)
	local got = { cframe:GetComponents() }
	for index = 1, 12 do
		if not Near(got[index], expected[index], tolerance) then
			error(`{what}: component {index} is {got[index]}, Studio's {expected[index]} ({cframe})`, 0)
		end
	end
end
local function AngleX(cframe) local _, _, _, _, _, _, _, m11, _, _, m21 = cframe:GetComponents() return math.atan2(m21, m11) end
"#;

fn scene(source: &str) {
    check(&format!("{SCENE}\n{source}"), 3.0);
}

/// The clip measured in Studio: the arm's Position (X Linear, Y Cubic, Z Constant then Linear) and an Euler Rotation
/// in YZX order (Z Cubic through three keys), the hand's RotationCurve (Linear, then Cubic). Studio's Transforms at
/// ten times, the track paused; between two samples Studio's own rotations are off the curve by up to 1e-4.
#[test]
fn a_curve_animation_poses_a_rig_as_studio_does() {
    scene(
        r#"
        local Animator, Shoulder, Wrist = Scene()
        local Clip = Instance.new("CurveAnimation")
        local Arm = Folder("Arm", Folder("Root", Clip))
        local Hand = Folder("Hand", Arm)
        local Position = Instance.new("Vector3Curve") Position.Name = "Position" Position.Parent = Arm
        Float("X", { FK(0, 0, L), FK(1, 2, L) }, Position) Float("Y", { FK(0, 1, K), FK(2, 3, K) }, Position) Float("Z", { FK(0.5, -1, C), FK(1.5, 4, L) }, Position)
        local Rotation = Instance.new("EulerRotationCurve") Rotation.Name = "Rotation" Rotation.RotationOrder = Enum.RotationOrder.YZX Rotation.Parent = Arm
        Float("X", { FK(0, 0.3, L), FK(2, 0.9, L) }, Rotation) Float("Y", { FK(0, 0.5, L) }, Rotation) Float("Z", { FK(0, 0.7, K), FK(1, -0.2, K), FK(2, 0.4, K) }, Rotation)
        local Turn = Instance.new("RotationCurve") Turn.Name = "Rotation" Turn.Parent = Hand
        Turn:InsertKey(RotationCurveKey.new(0, CFrame.Angles(0.2, 0, 0), L)) Turn:InsertKey(RotationCurveKey.new(1, CFrame.Angles(0, 1.1, 0.3), K)) Turn:InsertKey(RotationCurveKey.new(2.5, CFrame.Angles(-0.4, 0, 0.9), L))
        assert(Clip.Length == 2.5 and Clip.Loop == true and Clip.Priority == Enum.AnimationPriority.Action, "the clip's own members")
        local Track = Load(Animator, Clip)
        assert(Track.Length == 2.5 and Track.Looped == true and Track.Priority == Enum.AnimationPriority.Action, `the track: {Track.Length}`)
        local Studio = {
            { 0, { 0,1,-1,0.67118758,-0.398428559,0.625109494,0.644241095,0.730661511,-0.226024702,-0.366688788,0.554426193,0.747094989 }, { 0,0,0,1,0,0,0,0.980068207,-0.198661417,0,0.198661417,0.980068207 } },
            { 0.25, { 0.5,1.08624995,-1,0.744005442,-0.257485181,0.616568923,0.530357718,0.788849056,-0.310544431,-0.406419247,0.558048844,0.723467231 }, { 0,0,0,0.960876882,-0.0395823233,0.274132997,0.0923622847,0.978876114,-0.1824027,-0.261122316,0.200586081,0.944235325 } },
            { 0.5, { 1,1.3125,-1,0.850294709,0.0130340904,0.526145399,0.247411087,0.872449279,-0.42144987,-0.464528412,0.488530785,0.738614202 }, { 0,0,0,0.846560359,-0.0770040452,0.526693344,0.176611245,0.974062562,-0.141458616,-0.502139449,0.212773219,0.838202596 } },
            { 0.75, { 1.5,1.63291669,-1,0.87607193,0.285059869,0.388894439,-0.0589645952,0.863815248,-0.500346243,-0.47856158,0.415408283,0.773572743 }, { 0,0,0,0.665984094,-0.109335653,0.737909853,0.246151596,0.966004252,-0.079026401,-0.704183638,0.23426801,0.670256615 } },
            { 1, { 2,2,-1,0.860083461,0.414610267,0.297245502,-0.198670208,0.808875799,-0.553398788,-0.469879448,0.416915327,0.778071344 }, { 0,0,0,0.433289289,-0.134045362,0.891230762,0.295532346,0.955332756,7.77840614e-06,-0.851422966,0.263384134,0.45355022 } },
            { 1.3, { 2,2.43650031,-1,0.875401735,0.352794528,0.330466062,-0.0703386515,0.769322634,-0.634976566,-0.478251249,0.532615006,0.698281407 }, { 0,0,0,0.489941299,-0.23122777,0.840530336,0.328938603,0.941943765,0.0673895478,-0.807314694,0.24346593,0.537556887 } },
            { 1.75, { 2,2.91374993,4,0.836788476,0.172806621,0.519541085,0.30126828,0.647012472,-0.700437188,-0.457189739,0.742639065,0.489351451 }, { 0,0,0,0.633379936,-0.555632174,0.538611829,0.489145339,0.826805234,0.277722716,-0.59963876,0.0875554755,0.795466781 } },
            { 2, { 2,3,4,0.808291078,0.163134098,0.565732062,0.389443994,0.572505176,-0.721506178,-0.441586792,0.803507984,0.399219215 }, { 0,0,0,0.655710518,-0.696282804,0.291948527,0.601094902,0.71541512,0.356182754,-0.456868321,-0.0580640286,0.887637258 } },
            { 2.4, { 2,3,4,0.808291078,0.163134098,0.565732062,0.389443994,0.572505176,-0.721506178,-0.441586792,0.803507984,0.399219215 }, { 0,0,0,0.624640822,-0.780776083,0.0145796984,0.715764225,0.579895616,0.389104933,-0.312258542,-0.232615203,0.921078086 } },
        }
        for _, Row in Studio do
            Pose(Track, Row[1])
            Is(Shoulder.Transform, Row[2], 2e-4, `the arm at {Row[1]}`)
            Is(Wrist.Transform, Row[3], 2e-4, `the hand at {Row[1]}`)
        end
        -- the samples are 1/30 s apart, not the curve: at 0.75 s the curve's Y is 1.6328125 and the pose's 1.63291669
        Pose(Track, 0.75)
        assert(Near(Shoulder.Transform.Y, 1.63291669, 2e-6), `between two samples: {Shoulder.Transform.Y}`)
        local Ok, Message = pcall(Track.GetTimeOfKeyframe, Track, "End")
        assert(not Ok and Message == "Invalid operation for this animation format.", tostring(Message))
        "#,
    );
}

/// Studio plays the line between the curves' values at multiples of 1/30 s: a Constant step climbs over the sample
/// before it, the samples start at 0 s whatever the first key's time, and the last lies at or past Length.
#[test]
fn a_track_plays_thirty_samples_a_second() {
    scene(
        r#"
        local Animator, Shoulder = Scene()
        local function ArmClip(Build) local Clip = Instance.new("CurveAnimation") Clip.Loop = false Build(Folder("Arm", Folder("Root", Clip))) return Clip end
        local function Sweep(Track, Times, Expected, Read, Tolerance)
            for Index, Time in Times do
                Pose(Track, Time)
                local Got = Read(Shoulder.Transform)
                assert(Near(Got, Expected[Index], Tolerance or 1e-4), `at {Time}: {Got}, Studio's {Expected[Index]}`)
            end
            Track:Stop(0)
            task.wait()
        end
        local function X(cframe) return cframe.X end
        -- a step from 0 to 30 at 0.5 s, then 60 at 1 s
        local Step = Load(Animator, ArmClip(function(Arm) PositionX(Arm, { FK(0, 0, C), FK(0.5, 30, C), FK(1, 60, L) }) end))
        assert(Step.Length == 1)
        Sweep(Step, { 0.45, 0.4666666, 0.4708333, 0.475, 0.4833333, 0.4958333, 0.5, 0.55, 0.95, 0.9708333, 0.9875 }, { 0, 0, 3.75, 7.5, 15, 26.25, 30, 30, 30, 33.75, 48.75 }, X)
        -- a Cubic 0 to 100 over 1 s: 0.0814815 at 1/120 s (the curve itself is 0.0207)
        local Cubic = Load(Animator, ArmClip(function(Arm) PositionX(Arm, { FK(0, 0, K), FK(1, 100, K) }) end))
        Sweep(Cubic, { 1 / 120, 2 / 120, 4 / 120, 5 / 120, 0.5 }, { 0.081482, 0.162964, 0.325926, 0.562962, 50 }, X, 2e-4)
        -- 0.26 s long: the last sample is at 8/30 s, where the curve holds its last key
        local Odd = Load(Animator, ArmClip(function(Arm) PositionX(Arm, { FK(0, 0, L), FK(0.26, 26, L) }) end))
        assert(Near(Odd.Length, 0.26, 1e-6), Odd.Length)
        Sweep(Odd, { 0.2, 0.25, 0.255, 0.2599 }, { 20, 24.6666679, 25.0666656, 25.4586678 }, X)
        -- a first key at 0.11 s: the samples still start at 0 s
        local Late = Load(Animator, ArmClip(function(Arm) PositionX(Arm, { FK(0.11, 11, L), FK(0.31, 31, L) }) end))
        Sweep(Late, { 0, 0.1, 0.11, 0.12, 0.3 }, { 11, 11, 11.7, 12.4, 30 }, X, 2e-4)
        -- rotations between two samples are the normalized mix of the quaternions: 0.0249710 rad an eighth of the way
        -- to 0.2 rad (a slerp gives 0.025)
        local Turn = Load(Animator, ArmClip(function(Arm)
            local Curve = Instance.new("RotationCurve") Curve.Name = "Rotation" Curve.Parent = Arm
            Curve:InsertKey(RotationCurveKey.new(0, CFrame.Angles(0, 0, 0), L)) Curve:InsertKey(RotationCurveKey.new(0.5, CFrame.Angles(3, 0, 0), L))
        end))
        Sweep(Turn, { 1 / 240, 2 / 240, 4 / 240, 8 / 240, 12 / 240 }, { 0.0249709857, 0.049967233, 0.0999951447, 0.199992063, 0.299989897 }, AngleX, 1.2e-5)
        -- an Euler channel is sampled as a rotation: 9 rad over 0.5 s turns on past pi
        local Spin = Load(Animator, ArmClip(function(Arm) EulerX(Arm, { FK(0, 0, L), FK(0.5, 9, L) }) end))
        Sweep(Spin, { 1 / 60, 1 / 30, 10 / 30, 0.2 + 1 / 120 }, { 0.299989926, 0.600001381, -0.283175392, -2.53401591 }, AngleX, 1.2e-4)
        "#,
    );
}

/// A Folder is a joint by its own name alone; curves of other names and Folders that are not Folders pose nothing; the
/// later of two Rotation children counts.
#[test]
fn a_folder_names_its_joint_wherever_it_is() {
    scene(
        r#"
        local Animator, Shoulder, Wrist, Tail = Scene()
        local function Posed(Build)
            local Clip = Instance.new("CurveAnimation")
            Build(Clip)
            local Track = Load(Animator, Clip)
            Pose(Track, 0.5)
            local Result = { Shoulder.Transform, Wrist.Transform, Tail.Transform, Track.Length }
            Track:Stop(0)
            task.wait()
            return Result
        end
        local Keys = { FK(0, 1, L), FK(1, 3, L) }
        local Identity = CFrame.new()
        for Name, Build in {
            ["under its parent"] = function(Clip) PositionX(Folder("Arm", Folder("Root", Clip)), Keys) end,
            ["straight under the clip"] = function(Clip) PositionX(Folder("Arm", Clip), Keys) end,
            ["under another name"] = function(Clip) PositionX(Folder("Arm", Folder("Nope", Clip)), Keys) end,
            ["deeper"] = function(Clip) PositionX(Folder("Arm", Folder("Root", Folder("Extra", Clip))), Keys) end,
        } do
            local Result = Posed(Build)
            assert(Result[1] == CFrame.new(2, 0, 0) and Result[2] == Identity and Result[4] == 1, `{Name}: {Result[1]}`)
        end
        local Hand = Posed(function(Clip) PositionX(Folder("Hand", Folder("Root", Clip)), Keys) end)
        assert(Hand[1] == Identity and Hand[2] == CFrame.new(2, 0, 0), `the hand under the root's folder: {Hand[2]}`)
        local Bone = Posed(function(Clip) PositionX(Folder("Tail", Clip), Keys) end)
        assert(Bone[3] == CFrame.new(2, 0, 0) and Bone[2] == Identity, `a bone by its name: {Bone[3]}`)
        local Renamed = Posed(function(Clip)
            local Arm = Folder("Arm", Folder("Root", Clip))
            PositionX(Arm, Keys).Name = "Pos"
            EulerX(Arm, Keys).Name = "Rot"
        end)
        assert(Renamed[1] == Identity and Renamed[4] == 1, `curves of other names pose nothing and still count for Length: {Renamed[1]}`)
        local Models = Posed(function(Clip)
            local Root = Instance.new("Model") Root.Name = "Root" Root.Parent = Clip
            local Arm = Instance.new("Configuration") Arm.Name = "Arm" Arm.Parent = Root
            PositionX(Arm, Keys)
        end)
        assert(Models[1] == Identity and Models[4] == 0, `only Folders are read: {Models[1]} {Models[4]}`)
        local RootOnly = Posed(function(Clip) PositionX(Folder("Root", Clip), Keys) end)
        assert(RootOnly[1] == Identity and RootOnly[2] == Identity and RootOnly[4] == 1, "a folder no joint has the name of")
        local Two = Posed(function(Clip)
            local Arm = Folder("Arm", Folder("Root", Clip))
            EulerX(Arm, { FK(0, 1, L) })
            local Later = Instance.new("RotationCurve") Later.Name = "Rotation" Later.Parent = Arm
            Later:InsertKey(RotationCurveKey.new(0, CFrame.Angles(0, 0.5, 0), L))
        end)
        Is(Two[1], { 0, 0, 0, 0.877585113, 0, 0.47942093, 0, 1, 0, -0.47942093, 0, 0.877585113 }, 1e-4, "the later Rotation child")
        local Channel = Posed(function(Clip)
            local Rotation = Instance.new("EulerRotationCurve") Rotation.Name = "Rotation" Rotation.Parent = Folder("Arm", Folder("Root", Clip))
            Float("Y", { FK(1, 0.5, L), FK(2, 1, L) }, Rotation)
        end)
        Is(Channel[1], { 0, 0, 0, 0.877585113, 0, 0.4794209, 0, 1, 0, -0.4794209, 0, 0.877585113 }, 1e-4, "one channel, held before its first key")
        assert(Channel[4] == 2)
        "#,
    );
}

/// MarkerCurves fire GetMarkerReachedSignal(their name) with the marker's value, KeyframeReached says "End" once at
/// the end, a looped clip loops and a clip played once stops there. Studio's order (curve_golden/playback_steps.luau
/// has it step by step).
#[test]
fn markers_the_end_and_loops_fire_as_in_studio() {
    scene(
        r#"
        local Animator, Shoulder = Scene()
        local Clip = Instance.new("CurveAnimation")
        PositionX(Folder("Arm", Folder("Root", Clip)), { FK(0, 0, L), FK(1, 2, L) })
        local Steps = Instance.new("MarkerCurve") Steps.Name = "Step" Steps.Parent = Clip
        Steps:InsertMarkerAtTime(0.25, "left") Steps:InsertMarkerAtTime(0.75, "right")
        local Deep = Instance.new("MarkerCurve") Deep.Name = "Deep" Deep.Parent = Clip.Root.Arm
        Deep:InsertMarkerAtTime(0.5, "in a folder")
        local Track = Load(Animator, Clip)
        assert(Track.Length == 1 and Track.Looped)
        local Fired = {}
        Track:GetMarkerReachedSignal("Step"):Connect(function(Value) table.insert(Fired, "Step:" .. Value) end)
        Track:GetMarkerReachedSignal("Deep"):Connect(function(Value) table.insert(Fired, "Deep:" .. Value) end)
        Track:GetMarkerReachedSignal("left"):Connect(function(Value) table.insert(Fired, "left:" .. Value) end)
        Track.KeyframeReached:Connect(function(Name) table.insert(Fired, "KeyframeReached:" .. Name) end)
        Track.DidLoop:Connect(function() table.insert(Fired, "DidLoop") end)
        Track.Stopped:Connect(function() table.insert(Fired, "Stopped") end)
        Track:Play(0)
        task.wait(1.4)
        Track:Stop(0)
        task.wait()
        local Order = table.concat(Fired, " ")
        assert(Order == "Step:left Deep:in a folder Step:right KeyframeReached:End DidLoop Step:left Stopped", Order)
        -- played once: the end, then Stopped, holding the last pose while it fades
        local Once = Instance.new("CurveAnimation") Once.Loop = false Once.Priority = Enum.AnimationPriority.Idle
        PositionX(Folder("Arm", Once), { FK(0, 1, L), FK(0.5, 3, L) })
        local Single = Load(Animator, Once)
        assert(Single.Length == 0.5 and Single.Looped == false and Single.Priority == Enum.AnimationPriority.Idle)
        table.clear(Fired)
        Single.KeyframeReached:Connect(function(Name) table.insert(Fired, "KeyframeReached:" .. Name) end)
        Single.DidLoop:Connect(function() table.insert(Fired, "DidLoop") end)
        Single.Stopped:Connect(function() table.insert(Fired, "Stopped") end)
        Single.Ended:Connect(function() table.insert(Fired, "Ended") end)
        Single:Play(0)
        task.wait(0.3)
        assert(Single.IsPlaying and Shoulder.Transform.X > 1.5)
        task.wait(0.7)
        -- (DidLoop fires at an end the step went past, of a track that does not loop too; the track fades out over
        -- 0.3 s and Ended comes a step after that: measured)
        assert(table.concat(Fired, " ") == "KeyframeReached:End Stopped DidLoop Ended", table.concat(Fired, " "))
        assert(Single.TimePosition == 0.5 and not Single.IsPlaying)
        "#,
    );
}

/// A joint without a Rotation curve leaves the rotation to the other tracks; one without a Position still counts as a
/// translation of 0. Studio: 10 studs alone, 5 studs and the full radian with a rotation-only track beside it.
#[test]
fn a_joint_without_a_rotation_curve_leaves_the_rotation_to_other_tracks() {
    scene(
        r#"
        local Animator, Shoulder = Scene()
        local Moves = Instance.new("CurveAnimation") PositionX(Folder("Arm", Folder("Root", Moves)), { FK(0, 10, L), FK(1, 10, L) })
        local Turns = Instance.new("CurveAnimation") EulerX(Folder("Arm", Folder("Root", Turns)), { FK(0, 1, L), FK(1, 1, L) })
        local A, B = Load(Animator, Moves), Load(Animator, Turns)
        Pose(A, 0.5)
        assert(Near(Shoulder.Transform.X, 10) and Near(AngleX(Shoulder.Transform), 0), `alone: {Shoulder.Transform}`)
        Pose(B, 0.5)
        assert(Near(Shoulder.Transform.X, 5) and Near(AngleX(Shoulder.Transform), 1), `both: {Shoulder.Transform}`)
        A:Stop(0)
        task.wait()
        assert(Near(Shoulder.Transform.X, 0) and Near(AngleX(Shoulder.Transform), 1), `the rotation alone: {Shoulder.Transform}`)
        -- a KeyframeSequence's pose turns the joint as before: equal weights halve the radian
        local Sequence = Instance.new("KeyframeSequence")
        for _, Time in { 0, 1 } do
            local Keyframe = Instance.new("Keyframe") Keyframe.Time = Time Keyframe.Parent = Sequence
            local Root = Instance.new("Pose") Root.Name = "Root" Root.Weight = 0 Root.Parent = Keyframe
            local Arm = Instance.new("Pose") Arm.Name = "Arm" Arm.Parent = Root
        end
        local Still = Load(Animator, Sequence)
        Pose(Still, 0.5)
        assert(Near(AngleX(Shoulder.Transform), 0.5, 2e-2), `a sequence's pose counts: {AngleX(Shoulder.Transform)}`)
        "#,
    );
}

/// What each provider takes and gives, as Studio does (the ids themselves are this engine's).
#[test]
fn the_providers_register_and_return_curve_animations() {
    scene(
        r#"
        local function Raises(Call, Message)
            local Ok, Got = pcall(Call)
            assert(not Ok and Got == Message, `{Got} (expected {Message})`)
        end
        local Clip = Instance.new("CurveAnimation") Clip.Name = "Shape" Clip.Loop = false Clip.Priority = Enum.AnimationPriority.Movement
        local Arm = Folder("Arm", Folder("Root", Clip))
        EulerX(Arm, { FK(0.11, 0.5, L), FK(0.26, 1, L) })
        PositionX(Folder("Hand", Arm), { FK(0, 1, L), FK(0.2, 3, L) })
        local Late = Instance.new("MarkerCurve") Late.Name = "Late" Late.Parent = Clip
        Late:InsertMarkerAtTime(0.5, "past") Late:InsertMarkerAtTime(0, "start")
        local Sequence = Instance.new("KeyframeSequence")
        local Other = Instance.new("Folder")
        -- registering
        local Id = Clips:RegisterAnimationClip(Clip)
        assert(type(Id) == "string" and type(Sequences:RegisterKeyframeSequence(Clip)) == "string", "both services take a CurveAnimation")
        assert(type(Clips:RegisterAnimationClip(Sequence)) == "string", "and a KeyframeSequence")
        Raises(function() Clips:RegisterAnimationClip(Other) end, "Invalid AnimationClip instance passed to registerAnimationClip")
        Raises(function() Clips:RegisterAnimationClip() end, "Argument 1 missing or nil")
        Raises(function() Clips:RegisterActiveAnimationClip(Other) end, "Argument must be an 'AnimationClip' object")
        Raises(function() Clips:RegisterActiveAnimationClip() end, "Argument 1 missing or nil")
        Raises(function() Sequences:RegisterActiveKeyframeSequence(Clip) end, "Argument must be a 'KeyframeSequence' object")
        Raises(function() Sequences:RegisterActiveKeyframeSequence() end, "Argument 1 missing or nil")
        local First = Sequences:RegisterActiveKeyframeSequence(Sequence)
        local Active = Clips:RegisterActiveAnimationClip(Clip)
        assert(First == "active://0" and Active == "active://1", `{First} {Active}`)
        -- a Position or Rotation child of another class: AnimationClipProvider refuses, KeyframeSequenceProvider does not look
        local Bad = Instance.new("CurveAnimation")
        Float("Rotation", { FK(0, 1, L) }, Folder("Arm", Folder("Root", Bad)))
        Raises(function() Clips:RegisterAnimationClip(Bad) end, "CurveAnimation (Instance)'s descendant (Arm) has an invalid Rotation channel. Only EulerRotationCurve or RotationCurve is supported")
        Raises(function() Clips:RegisterActiveAnimationClip(Bad) end, "CurveAnimation (Instance)'s descendant (Arm) has an invalid Rotation channel. Only EulerRotationCurve or RotationCurve is supported")
        assert(type(Sequences:RegisterKeyframeSequence(Bad)) == "string")
        local Worse = Instance.new("CurveAnimation") Worse.Name = "Worse"
        local Turn = Instance.new("RotationCurve") Turn.Name = "Position" Turn.Parent = Folder("Arm", Worse)
        Raises(function() Clips:RegisterAnimationClip(Worse) end, "CurveAnimation (Worse)'s descendant (Arm) has an invalid Position channel. Only Vector3Curve is supported")
        -- GetAnimationClipAsync: a copy of the clip as it is; the instance itself for an active id
        local Copy = Clips:GetAnimationClipAsync(Id)
        assert(Copy.ClassName == "CurveAnimation" and Copy ~= Clip and Copy.Name == "Shape" and Copy.Parent == nil and #Copy:GetDescendants() == #Clip:GetDescendants())
        assert(Copy.Length == Clip.Length and Copy.Root.Arm.Rotation:X():GetKeyAtIndex(2).Value == 1, "its curves hold the keys")
        assert(Clips:GetAnimationClipAsync(Active) == Clip, "an active clip is the instance")
        assert(Clips:GetAnimationClipAsync(Clips:RegisterAnimationClip(Sequence)).ClassName == "KeyframeSequence")
        Raises(function() Clips:GetAnimationClipAsync() end, "Argument 1 missing or nil")
        Raises(function() Clips:GetAnimationClipAsync(5) end, "Unable to cast double to ContentId")
        local Ok, Message = pcall(function() return Sequences:GetKeyframeSequenceAsync(Active) end)
        assert(not Ok and string.find(Message, "Unknown 'active' animation: 'active://1'", 1, true), tostring(Message))
        -- GetKeyframeSequenceAsync: the KeyframeSequence Studio makes of it
        local Made = Sequences:GetKeyframeSequenceAsync(Id)
        assert(Made.ClassName == "KeyframeSequence" and Made.Name == "KeyframeSequence" and Made.Loop == true and Made.Priority == Enum.AnimationPriority.Action, "its own name, Loop and Priority")
        local Keyframes = Made:GetChildren()
        local Studio = {
            { 0, 0.500000015, 1, "start" }, { 0.0333333351, 0.500000015, 1.33333325 }, { 0.0666666701, 0.500000015, 1.66666663 }, { 0.100000001, 0.500000015, 2 },
            { 0.13333334, 0.577777816, 2.33333325 }, { 0.166666672, 0.688888879, 2.66666651 }, { 0.200000003, 0.800000079, 3 }, { 0.233333334, 0.911111135, 3 }, { 0.25999999, 1.00000001, 3 },
        }
        assert(#Keyframes == 10, #Keyframes)
        for Index, Row in Studio do
            local Keyframe = Keyframes[Index]
            assert(Keyframe.Name == "Keyframe" and Near(Keyframe.Time, Row[1], 1e-6), `keyframe {Index} at {Keyframe.Time}`)
            local Root = Keyframe.Root
            assert(Root.Weight == 0 and Root.Arm.Weight == 1 and Root.Arm.Hand.Weight == 1, "a folder without curves is a weight 0 pose")
            assert(Root.Arm.EasingStyle == Enum.PoseEasingStyle.Linear and Root.Arm.EasingDirection == Enum.PoseEasingDirection.In)
            assert(Near(AngleX(Root.Arm.CFrame), Row[2], 2e-6) and Near(Root.Arm.Hand.CFrame.X, Row[3], 2e-6), `keyframe {Index}: {AngleX(Root.Arm.CFrame)} {Root.Arm.Hand.CFrame.X}`)
            local Marker = Keyframe:FindFirstChildOfClass("KeyframeMarker")
            assert((Marker and Marker.Name == "Late" and Marker.Value) == Row[4], `keyframe {Index}'s marker`)
        end
        local Last = Keyframes[10]
        assert(Last.Time == 0.5 and #Last:GetChildren() == 1 and Last.Late.Value == "past", "a marker past the end has a keyframe of its own")
        "#,
    );
}

/// A CurveAnimation in a model file: an asset loads and plays, GetAnimationClipAsync makes the clip and
/// GetKeyframeSequenceAsync the sequence made of it.
#[test]
fn a_curve_animation_loads_from_a_file() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{BinaryString, Enum, Variant}};
    let directory = std::env::temp_dir().join(format!("ree-curve-animations-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let float = |name: &str, keys: &[(f32, f32)]| {
        let keys: Vec<FloatKey> = keys.iter().map(|&(time, value)| FloatKey { tick: curve::tick_of(time), value, mode: LINEAR, left: None, right: None }).collect();
        InstanceBuilder::new("FloatCurve").with_name(name).with_property("ValuesAndTimes", Variant::BinaryString(BinaryString::from(curve::encode_floats(&keys))))
    };
    let markers = curve::encode_markers(&[MarkerKey { tick: 600, value: b"now".to_vec() }]);
    let clip = InstanceBuilder::new("CurveAnimation")
        .with_name("Wave")
        .with_property("Loop", Variant::Bool(false))
        .with_property("Priority", Variant::Enum(Enum::from_u32(1)))
        .with_child(InstanceBuilder::new("MarkerCurve").with_name("Hit").with_property("ValuesAndTimes", Variant::BinaryString(BinaryString::from(markers))))
        .with_child(
            InstanceBuilder::new("Folder").with_name("Root").with_child(
                InstanceBuilder::new("Folder")
                    .with_name("Arm")
                    .with_child(InstanceBuilder::new("Vector3Curve").with_name("Position").with_child(float("X", &[(0.0, 0.0), (1.0, 4.0)])))
                    .with_child(
                        InstanceBuilder::new("EulerRotationCurve")
                            .with_name("Rotation")
                            .with_property("RotationOrder", Variant::Enum(Enum::from_u32(3)))
                            .with_child(float("X", &[(0.0, 0.0), (1.0, 1.0)])),
                    ),
            ),
        );
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let id = dom.insert(root, clip);
    let mut file = Vec::new();
    rbx_binary::to_writer(&mut file, &dom, &[id]).expect("write");
    std::fs::write(directory.join("5151.rbxm"), file).expect("write file");
    let (lua, engine) = start("play");
    enable(&engine, Loading { directories: vec![directory.clone()], download: false });
    let lines = run(
        &lua,
        &engine,
        &format!(
            "{SCENE}{}",
            r#"
        local Animator, Shoulder = Scene()
        local Animation = Instance.new("Animation")
        Animation.AnimationId = "rbxassetid://5151"
        local Track = Animator:LoadAnimation(Animation)
        repeat task.wait() until Track.Length > 0
        assert(Track.Length == 1 and Track.Looped == false and Track.Priority == Enum.AnimationPriority.Movement, `{Track.Length} {Track.Priority}`)
        local Hits = {}
        Track:GetMarkerReachedSignal("Hit"):Connect(function(Value) table.insert(Hits, Value) end)
        Track:Play(0)
        task.wait(0.5)
        assert(Near(Shoulder.Transform.X, 2, 0.15) and Near(AngleX(Shoulder.Transform), 0.5, 0.05), `posed from the file: {Shoulder.Transform}`)
        assert(#Hits == 1 and Hits[1] == "now", "the file's marker")
        local Clip = Clips:GetAnimationClipAsync("rbxassetid://5151")
        assert(Clip.ClassName == "CurveAnimation" and Clip.Name == "Wave" and Clip.Parent == nil and Clip.Length == 1 and Clip.Loop == false)
        local X = Clip.Root.Arm.Position:X()
        assert(X.Length == 2 and X:GetValueAtTime(0.25) == 1 and Clip.Hit:GetMarkerAtIndex(1).Value == "now", "the file's curves hold their keys")
        assert(Clip.Root.Arm.Rotation.RotationOrder == Enum.RotationOrder.YXZ)
        assert(Clips:GetAnimationClipAsync("rbxassetid://5151") ~= Clip, "a new copy each call")
        local Sequence = Sequences:GetKeyframeSequenceAsync("rbxassetid://5151")
        assert(Sequence.ClassName == "KeyframeSequence" and #Sequence:GetChildren() == 32, #Sequence:GetChildren())
        assert(Near(Sequence:GetChildren()[16].Root.Arm.CFrame.X, 2, 1e-5), "the sequence made of the file's clip")
        "#
        ),
        3.0,
    );
    let _ = std::fs::remove_dir_all(&directory);
    assert!(!lines.iter().any(|line| line.contains("Failed to load")), "{lines:?}");
    let _: Rc<crate::Engine> = engine;
}

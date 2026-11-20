//! An animation clip as an AnimationTrack plays it: each joint's keys, the named keyframes and the
//! markers. Read from a model file (a downloaded animation asset, rbx-dom) or from the engine's own
//! instances (KeyframeSequenceProvider:RegisterKeyframeSequence), through [`Tree`]: a
//! KeyframeSequence here, a CurveAnimation in [`super::curve_clip`].
//!
//! A Pose names the joint it moves by its own name and its parent Pose's: a Motor6D whose Part0 and
//! Part1 have those names, else a Bone of the Pose's name whose parent has the parent Pose's name
//! (a bone under a renamed part is not posed). A Pose of weight 0 only holds the
//! hierarchy together (the Animation Editor writes them for parents without a key of their own).
//! Between two keys of a joint the value moves by the earlier key's easing; before the first key
//! and after the last it holds. The easing (legacy Cubic, Elastic and Bounce run In and Out the
//! other way round from TweenService's; CubicV2 fixed it for Cubic) is ported from an earlier
//! renderer of ours that poses idle animations as Roblox plays them.

use glam::{Affine3A, Quat, Vec3A};

/// Enum.AnimationPriority values (Core is the lowest priority despite its value).
pub const PRIORITY_ACTION: u32 = 2;

/// A priority's rank, lowest first: Core, Idle, Movement, Action, Action2, Action3, Action4.
pub fn priority_rank(value: u32) -> u32 {
    match value {
        1000 => 0,
        value => value + 1,
    }
}

// Enum.PoseEasingStyle
const LINEAR: u32 = 0;
const CONSTANT: u32 = 1;
const ELASTIC: u32 = 2;
const CUBIC: u32 = 3;
const BOUNCE: u32 = 4;
const CUBIC_V2: u32 = 5;
/// Not a PoseEasingStyle: a key sampled from a CurveAnimation's curves ([`super::curve_clip`]), one of 30 a second
/// from 0 s, found by its index ([`sampled`]).
pub const SAMPLED: u32 = u32::MAX;
/// A sample of a clip too long for 30 a second: found by its time, the translation moving in a line to the next
/// and the rotation as the normalized mix of the two quaternions.
pub const SAMPLED_COARSE: u32 = u32::MAX - 1;
/// A key of a packed KeyframeSequence ([`pack`]): it goes to the next in a line.
pub const PACKED: u32 = u32::MAX - 2;
// Enum.PoseEasingDirection
const IN: u32 = 0;
const OUT: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub time: f32,
    pub translation: Vec3A,
    pub rotation: Quat,
    pub style: u32,
    pub direction: u32,
}

/// One joint's keys, sorted by time.
#[derive(Clone, Debug)]
pub struct JointTrack {
    /// The parent Pose's name ("" for a top-level Pose)
    pub parent: String,
    pub name: String,
    pub keys: Vec<Key>,
    /// Whether the track turns the joint. A CurveAnimation's joint without a Rotation curve does
    /// not: it leaves the joint's rotation to the other tracks (its translation counts all the same).
    pub rotation: bool,
}

/// A number a clip drives that is no joint's: a FaceControls property, by its name.
#[derive(Clone, Debug)]
pub struct ValueTrack {
    pub name: String,
    pub keys: Vec<ValueKey>,
}

#[derive(Clone, Copy, Debug)]
pub struct ValueKey {
    pub time: f32,
    pub value: f32,
    pub style: u32,
    pub direction: u32,
}

/// A KeyframeMarker: its Keyframe's time, its name and Value.
#[derive(Clone, Debug)]
pub struct Marker {
    pub time: f32,
    pub name: String,
    /// The Value's bytes (Studio keeps any, a zero byte too)
    pub value: Vec<u8>,
    /// Which MarkerCurve of a CurveAnimation it is in, in the order the clip holds them (0 for a KeyframeMarker): a
    /// step fires one curve's markers before the next curve's
    pub curve: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Clip {
    /// The last keyframe's time
    pub length: f32,
    /// KeyframeSequence.Loop
    pub looped: bool,
    /// KeyframeSequence.Priority (an Enum.AnimationPriority value)
    pub priority: u32,
    pub joints: Vec<JointTrack>,
    /// Keyframes not named "Keyframe" (KeyframeReached), by time
    pub keyframes: Vec<(f32, String)>,
    /// Every keyframe by name and time (GetTimeOfKeyframe)
    pub names: Vec<(f32, String)>,
    pub markers: Vec<Marker>,
    /// Read from a CurveAnimation: a joint is found by its own name alone, and the track knows no
    /// keyframe names (GetTimeOfKeyframe refuses)
    pub curve: bool,
    /// A KeyframeSequence that plays from its packed form ([`evenly_keyed`], [`pack`])
    pub even: bool,
    /// The FaceControls properties it drives: a CurveAnimation's FloatCurves in a Folder named FaceControls, wherever
    /// that is in the clip, each named after its property; a KeyframeSequence's NumberPoses in such a Folder in a
    /// Pose (measured, Studio 0.747: the rig's FaceControls instance, the one of that name, takes them)
    pub face: Vec<ValueTrack>,
}

/// What a clip is read from: a model file's instances or the engine's.
pub trait Tree {
    type Node: Copy;
    fn class(&self, node: Self::Node) -> &str;
    fn name(&self, node: Self::Node) -> String;
    fn children(&self, node: Self::Node) -> Vec<Self::Node>;
    fn number(&self, node: Self::Node, property: &str) -> Option<f64>;
    fn boolean(&self, node: Self::Node, property: &str) -> Option<bool>;
    fn cframe(&self, node: Self::Node, property: &str) -> Option<Affine3A>;
    /// An enum property's value
    fn enumeration(&self, node: Self::Node, property: &str) -> Option<u32>;
    fn string(&self, node: Self::Node, property: &str) -> Option<String>;
    /// A string property's bytes as they are
    fn text(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        self.string(node, property).map(String::into_bytes)
    }
    /// A binary string property's bytes (a curve's `ValuesAndTimes`)
    fn bytes(&self, _node: Self::Node, _property: &str) -> Option<Vec<u8>> {
        None
    }
}

/// The first animation clip at or below `root`, a KeyframeSequence or a CurveAnimation (true).
pub fn clip_node<T: Tree>(tree: &T, root: T::Node) -> Result<(T::Node, bool), String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match tree.class(node) {
            "KeyframeSequence" => return Ok((node, false)),
            "CurveAnimation" => return Ok((node, true)),
            _ => stack.extend(tree.children(node).into_iter().rev()),
        }
    }
    Err("the asset holds no KeyframeSequence or CurveAnimation".into())
}

/// The first animation clip at or below `root` when it is a KeyframeSequence; an error names what
/// was found instead.
pub fn sequence_node<T: Tree>(tree: &T, root: T::Node) -> Result<T::Node, String> {
    match clip_node(tree, root)? {
        (node, false) => Ok(node),
        (node, true) => Err(format!("{:?} is a CurveAnimation, not a KeyframeSequence", tree.name(node))),
    }
}

impl Clip {
    /// The clip of the first KeyframeSequence or CurveAnimation at or below `root` ([`clip_node`]).
    pub fn find<T: Tree>(tree: &T, root: T::Node) -> Result<Clip, String> {
        clip_node(tree, root).map(|(node, curve)| Self::of(tree, node, curve))
    }

    /// The clip of `node`, a CurveAnimation (`curve`) or a KeyframeSequence.
    pub fn of<T: Tree>(tree: &T, node: T::Node, curve: bool) -> Clip {
        if curve { super::curve_clip::clip(tree, node) } else { Self::from_sequence(tree, node) }
    }

    pub fn from_sequence<T: Tree>(tree: &T, sequence: T::Node) -> Clip {
        let mut clip = Clip {
            looped: tree.boolean(sequence, "Loop").unwrap_or(true),
            priority: tree.enumeration(sequence, "Priority").unwrap_or(PRIORITY_ACTION),
            ..Clip::default()
        };
        let mut joints: Vec<JointTrack> = Vec::new();
        for keyframe in tree.children(sequence) {
            if tree.class(keyframe) != "Keyframe" {
                continue;
            }
            let time = tree.number(keyframe, "Time").unwrap_or(0.0) as f32;
            clip.length = clip.length.max(time);
            let name = tree.name(keyframe);
            if name != "Keyframe" {
                clip.keyframes.push((time, name.clone()));
            }
            clip.names.push((time, name));
            let mut stack: Vec<(T::Node, String)> = Vec::new();
            for child in tree.children(keyframe) {
                match tree.class(child) {
                    "Pose" => stack.push((child, String::new())),
                    "KeyframeMarker" => clip.markers.push(Marker {
                        time,
                        name: tree.name(child),
                        value: tree.text(child, "Value").unwrap_or_default(),
                        curve: 0,
                    }),
                    _ => {}
                }
            }
            let number_poses = |clip: &mut Clip, folder: T::Node| {
                for number in tree.children(folder) {
                    if tree.class(number) != "NumberPose" {
                        continue;
                    }
                    let key = ValueKey {
                        time,
                        value: tree.number(number, "Value").unwrap_or(0.0) as f32,
                        style: tree.enumeration(number, "EasingStyle").unwrap_or(LINEAR),
                        direction: tree.enumeration(number, "EasingDirection").unwrap_or(IN),
                    };
                    let name = tree.name(number);
                    match clip.face.iter_mut().find(|track| track.name == name) {
                        Some(track) => track.keys.push(key),
                        None => clip.face.push(ValueTrack { name, keys: vec![key] }),
                    }
                }
            };
            for child in tree.children(keyframe) {
                if tree.class(child) == "Folder" && tree.name(child) == "FaceControls" {
                    number_poses(&mut clip, child);
                }
            }
            while let Some((pose, parent)) = stack.pop() {
                let name = tree.name(pose);
                for child in tree.children(pose) {
                    match tree.class(child) {
                        "Pose" => stack.push((child, name.clone())),
                        "Folder" if tree.name(child) == "FaceControls" => number_poses(&mut clip, child),
                        _ => {}
                    }
                }
                if tree.number(pose, "Weight").unwrap_or(1.0) <= 0.0 {
                    continue;
                }
                let cframe = tree.cframe(pose, "CFrame").unwrap_or(Affine3A::IDENTITY);
                let key = Key {
                    time,
                    translation: cframe.translation,
                    rotation: Quat::from_mat3a(&cframe.matrix3).normalize(),
                    style: tree.enumeration(pose, "EasingStyle").unwrap_or(LINEAR),
                    direction: tree.enumeration(pose, "EasingDirection").unwrap_or(IN),
                };
                match joints.iter_mut().find(|joint| joint.name == name && joint.parent == parent) {
                    Some(joint) => joint.keys.push(key),
                    None => joints.push(JointTrack { parent, name, keys: vec![key], rotation: true }),
                }
            }
        }
        for joint in &mut joints {
            joint.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
        clip.even = evenly_keyed(&clip.names, &joints);
        clip.joints = joints;
        for track in &mut clip.face {
            track.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
        clip.keyframes.sort_by(|a, b| a.0.total_cmp(&b.0));
        clip.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        if clip.even {
            pack(&mut clip);
        }
        clip
    }

    /// The earliest keyframe named `name`.
    pub fn time_of(&self, name: &str) -> Option<f32> {
        self.names.iter().filter(|(_, n)| n == name).map(|(t, _)| *t).reduce(f32::min)
    }
}

/// Whether Studio 0.747 plays a KeyframeSequence from a packed form ([`pack`]) and not from its keys as they are.
/// Measured with a joint 1000 studs out, whose packed 4 studs read 3.9979248: it does when every gap between two
/// keyframes is within a tenth of the first gap (in float32: gaps of 1 and 1.1 s are, of 1 and 1.105 s are not, of
/// 1.1, 1 and 1 s are, seven of 1 s and then 8.1 - 7 are not), every posed joint has a Pose with a Weight in every
/// keyframe, and each of those Poses eases Linear (the EasingDirection is nothing to it, nor a Pose of Weight 0). One
/// keyframe is packed too; two at one time are not.
fn evenly_keyed(keyframes: &[(f32, String)], joints: &[JointTrack]) -> bool {
    let mut times: Vec<f32> = keyframes.iter().map(|(time, _)| *time).collect();
    times.sort_by(f32::total_cmp);
    let first = times.get(1).map_or(0.0, |second| second - times[0]);
    times.windows(2).all(|pair| (pair[1] - pair[0] - first).abs() <= first * 0.1 && pair[1] > pair[0])
        && joints.iter().all(|joint| joint.keys.len() == times.len() && joint.keys.iter().all(|key| key.style == LINEAR))
}

/// How many steps a packed translation has each side of 0, and a packed quaternion component.
const PACKED_TRANSLATION: f32 = 32767.0;
const PACKED_ROTATION: f32 = 16383.0;

/// Makes `clip` the packed form Studio 0.747 plays an evenly keyed KeyframeSequence from (measured; 452 rotations and
/// 150 translations agree to float32's last digits):
/// - keyframe `i` of `n` moves to `i * Length / (n - 1)` s, its name and markers with it, so keyframes at 0, 0.5 and
///   1.05 s play at 0, 0.525 and 1.05 s (GetTimeOfKeyframe gives 0.524999976) and keyframes at 0.1, 0.6 and 1.1 s at
///   0, 0.55 and 1.1 s;
/// - a joint's translations are rounded down to steps of `M / 32767`, `M` the largest size of any of its translations'
///   components in the clip: with 1000 studs on one axis 3 studs on another play as 2.99081397, -3 as -3.0213325 and
///   500 as 499.984741;
/// - a rotation keeps its three smaller quaternion components, each cut towards 0 to steps of `sqrt(2) / 32766`, and
///   takes the largest from them: 0.7 rad about X plays as 0.699953356 rad.
/// Between two keys such a clip mixes the quaternions and normalizes ([`sample`]).
fn pack(clip: &mut Clip) {
    let mut times: Vec<f32> = clip.names.iter().map(|(time, _)| *time).collect();
    times.sort_by(f32::total_cmp);
    if times.len() > 1 {
        let step = clip.length / (times.len() - 1) as f32;
        let moved = |time: f32| times.partition_point(|&earlier| earlier < time) as f32 * step;
        for joint in &mut clip.joints {
            joint.keys.iter_mut().for_each(|key| key.time = moved(key.time));
        }
        for track in &mut clip.face {
            track.keys.iter_mut().for_each(|key| key.time = moved(key.time));
        }
        clip.keyframes.iter_mut().for_each(|(time, _)| *time = moved(*time));
        clip.names.iter_mut().for_each(|(time, _)| *time = moved(*time));
        clip.markers.iter_mut().for_each(|marker| marker.time = moved(marker.time));
    }
    for joint in &mut clip.joints {
        let most = joint.keys.iter().fold(0f32, |most, key| most.max(key.translation.abs().max_element()));
        for key in &mut joint.keys {
            key.style = PACKED;
            if most > 0.0 {
                // (5000 studs come back as 5000.00049: the step count times M, then over 32767, in float32)
                key.translation = (key.translation / most * PACKED_TRANSLATION).floor() * most / PACKED_TRANSLATION;
            }
            let step = std::f32::consts::SQRT_2 / (2.0 * PACKED_ROTATION);
            let mut q = key.rotation.to_array();
            let largest = (0..4).fold(0, |largest, index| if q[index].abs() > q[largest].abs() { index } else { largest });
            let sign = if q[largest] < 0.0 { -1.0 } else { 1.0 };
            let mut rest = 0.0;
            for index in (0..4).filter(|&index| index != largest) {
                q[index] = (q[index] * sign / step).trunc() * step;
                rest += q[index] * q[index];
            }
            q[largest] = (1.0 - rest).max(0.0).sqrt();
            key.rotation = Quat::from_array(q);
        }
    }
}

/// The first coefficients of Abramowitz and Stegun's 4.4.45, `acos(x) = sqrt(1 - x) * (a0 + a1 x + a2 x^2 + a3 x^3)`,
/// good to 7e-5 rad.
const ARC_COSINE: [f32; 4] = [1.570_728_8, -0.212_114_4, 0.074_261_0, -0.018_729_3];

/// A sampled rotation as a CurveAnimation's track gives it to the joint (measured, Studio 0.747): turned into an axis
/// and an angle and back, the angle by [`ARC_COSINE`] and the axis by dividing through `sqrt(1 - w^2)`. So a
/// constant 0.6 rad about X plays as 0.60000138 rad and 2.85 rad as 2.850123 rad, for an EulerRotationCurve and a
/// RotationCurve alike, and a Pose of 1 rad in a KeyframeSequence as 1.0000431 rad; the curves' own members do not
/// show it. 120 measured
/// rotations about several axes agree with this within 4e-7 per matrix entry (the last one or two digits follow
/// Studio's own float32 sine and cosine, which this does not copy).
pub fn through_angle(rotation: Quat) -> Quat {
    let q = if rotation.w < 0.0 { -rotation } else { rotation };
    let w = q.w.min(1.0);
    let sine = (1.0 - w * w).sqrt();
    if !(sine > 0.0) {
        return q;
    }
    let [a0, a1, a2, a3] = ARC_COSINE;
    let half = (((a3 * w + a2) * w + a1) * w + a0) * (1.0 - w).sqrt();
    let (turned, cosine) = (half.sin(), half.cos());
    Quat::from_xyzw(q.x / sine * turned, q.y / sine * turned, q.z / sine * turned, cosine)
}

/// A CurveAnimation's joint at `time` seconds from its 30 samples a second, as Studio 0.747 plays one (1814 measured
/// translations of clips from 0.5 s to 10 s agree to the last digit): sample `k` is the curves' value at
/// float32(k * float32(1/30)) s, held at the clip's Length; the place between two is float32(time * 30), whose
/// whole part is the earlier sample and whose fraction `s`, taken as 0 below 1e-5, mixes the two as
/// `a * (1 - s) + b * s`. So a line from 0 to 10 over 1 s reads 7.00000048 at 0.7 s, and a constant 1000 reads
/// 999.999939 at 0.03 s. Rotations mix the same way as quaternions, normalized, then go through [`through_angle`].
fn sampled(keys: &[Key], time: f32) -> (Quat, Vec3A) {
    let place = time * super::curve_clip::SAMPLE_RATE;
    let whole = place.floor();
    let last = keys.len() - 1;
    if !(whole >= 0.0) {
        return (through_angle(keys[0].rotation), keys[0].translation);
    }
    let mut mix = place - whole;
    if mix < 1e-5 {
        mix = 0.0;
    }
    let index = (whole as usize).min(last);
    let (a, b) = (&keys[index], &keys[(index + 1).min(last)]);
    let translation = a.translation * (1.0 - mix) + b.translation * mix;
    let towards = if a.rotation.dot(b.rotation) < 0.0 { -b.rotation } else { b.rotation };
    let rotation = if mix == 0.0 { a.rotation } else { (a.rotation * (1.0 - mix) + towards * mix).normalize() };
    (through_angle(rotation), translation)
}

/// A [`ValueTrack`]'s value at `time` seconds, found as a joint's translation is ([`sample`], [`sampled`]).
pub fn sample_value(keys: &[ValueKey], time: f32, even: bool) -> f32 {
    let last = keys.len() - 1;
    if keys[0].style == SAMPLED {
        let place = time * super::curve_clip::SAMPLE_RATE;
        let whole = place.floor();
        if !(whole >= 0.0) {
            return keys[0].value;
        }
        let mut mix = place - whole;
        if mix < 1e-5 {
            mix = 0.0;
        }
        let index = (whole as usize).min(last);
        return keys[index].value * (1.0 - mix) + keys[(index + 1).min(last)].value * mix;
    }
    let next = keys.partition_point(|key| key.time <= time);
    if next == 0 {
        return keys[0].value;
    }
    if next == keys.len() || time.is_nan() {
        return keys[last].value;
    }
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.time - a.time;
    if span <= 0.0 {
        return b.value;
    }
    let fraction = if even || a.style == SAMPLED_COARSE { (time - a.time) / span } else { 1.0 - (b.time - time) / span };
    a.value + (b.value - a.value) * ease(a.style, a.direction, fraction.clamp(0.0, 1.0))
}

/// A joint's offset (rotation, translation) at `time` seconds; `even` is the clip's ([`Clip::even`]). A time that
/// is no number gives a KeyframeSequence's last key (measured).
pub fn sample(keys: &[Key], time: f32, even: bool) -> (Quat, Vec3A) {
    if keys[0].style == SAMPLED {
        return sampled(keys, time);
    }
    let coarse = keys[0].style == SAMPLED_COARSE;
    let turned = |rotation: Quat| if coarse { rotation } else { through_angle(rotation) };
    if time.is_nan() {
        let last = &keys[keys.len() - 1];
        return (turned(last.rotation), last.translation);
    }
    let next = keys.partition_point(|key| key.time <= time);
    if next == 0 {
        return (turned(keys[0].rotation), keys[0].translation);
    }
    if next == keys.len() {
        let last = &keys[keys.len() - 1];
        return (turned(last.rotation), last.translation);
    }
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.time - a.time;
    if span <= 0.0 {
        return (turned(b.rotation), b.translation);
    }
    if a.style == SAMPLED_COARSE {
        let alpha = ((time - a.time) / span).clamp(0.0, 1.0);
        let towards = if a.rotation.dot(b.rotation) < 0.0 { -b.rotation } else { b.rotation };
        return ((a.rotation * (1.0 - alpha) + towards * alpha).normalize(), a.translation.lerp(b.translation, alpha));
    }
    // (measured in float32, Studio 0.747: the translation is the earlier key's plus the fraction's share of the
    // difference. In a clip that is not evenly keyed the fraction is 1 less what is left of the span: 88 of 90 poses
    // of two such clips agree to the last digit, so 0.1 s of a line from 0 to 2.5 over 0.25 s reads 0.99999994. In an
    // even one it is the time passed over the span: 45 of 45 poses of a line from 0 to 10 over 1 s)
    // The rotation goes the short way round at an even turning rate, or in a packed clip as the normalized mix of
    // the two quaternions (2 rad about X over 1 s reads 0.46675 rad at 0.25 s packed), and then through
    // [`through_angle`] as a curve's does.
    if even {
        let alpha = ((time - a.time) / span).clamp(0.0, 1.0);
        let towards = if a.rotation.dot(b.rotation) < 0.0 { -b.rotation } else { b.rotation };
        let rotation = if alpha == 0.0 { a.rotation } else { (a.rotation * (1.0 - alpha) + towards * alpha).normalize() };
        return (through_angle(rotation), a.translation + (b.translation - a.translation) * alpha);
    }
    let alpha = ease(a.style, a.direction, (1.0 - (b.time - time) / span).clamp(0.0, 1.0));
    (through_angle(a.rotation.slerp(b.rotation, alpha)), a.translation + (b.translation - a.translation) * alpha)
}

/// A Pose's easing from its key to the next (measured, Studio 0.747, at a quarter and at 0.6 of the way for each
/// style and direction). Cubic's directions are the other way round: its In starts fast. Constant holds the later
/// key's pose from the start with In, the earlier key's to the end with Out, and changes half way with InOut.
pub fn ease(style: u32, direction: u32, alpha: f32) -> f32 {
    let flipped = match direction {
        IN => OUT,
        OUT => IN,
        other => other,
    };
    match style {
        LINEAR => alpha,
        CONSTANT => match direction {
            IN => (alpha > 0.0) as u32 as f32,
            OUT => (alpha >= 1.0) as u32 as f32,
            _ => (alpha >= 0.5) as u32 as f32,
        },
        CUBIC => tween(cubic_in, flipped, alpha),
        CUBIC_V2 => tween(cubic_in, direction, alpha),
        // (InOut is its own curve for these two: Elastic's has a period half as long again, and Bounce's second
        // half is its first again from half way up: 0.6 of the way reads 0.53)
        ELASTIC if direction != IN && direction != OUT => {
            let (period, along) = (0.45, alpha * 2.0 - 1.0);
            let wave = |power: f32| 2f32.powf(power * along) * ((along - period / 4.0) * std::f32::consts::TAU / period).sin();
            if alpha <= 0.0 || alpha >= 1.0 {
                alpha.clamp(0.0, 1.0)
            } else if along < 0.0 {
                -0.5 * wave(10.0)
            } else {
                0.5 * wave(-10.0) + 1.0
            }
        }
        BOUNCE if direction != IN && direction != OUT => {
            if alpha < 0.5 { bounce_in(alpha * 2.0) * 0.5 } else { 0.5 + bounce_in(alpha * 2.0 - 1.0) * 0.5 }
        }
        ELASTIC => tween(elastic_in, direction, alpha),
        BOUNCE => tween(bounce_in, direction, alpha),
        _ => alpha,
    }
}

/// An In easing turned into the direction asked for.
fn tween(ease_in: fn(f32) -> f32, direction: u32, alpha: f32) -> f32 {
    match direction {
        IN => ease_in(alpha),
        OUT => 1.0 - ease_in(1.0 - alpha),
        _ => {
            if alpha < 0.5 {
                ease_in(alpha * 2.0) * 0.5
            } else {
                1.0 - ease_in((1.0 - alpha) * 2.0) * 0.5
            }
        }
    }
}

fn cubic_in(alpha: f32) -> f32 {
    alpha * alpha * alpha
}

fn elastic_in(alpha: f32) -> f32 {
    if alpha <= 0.0 || alpha >= 1.0 {
        return alpha.clamp(0.0, 1.0);
    }
    let period = 0.3;
    -(2f32.powf(10.0 * (alpha - 1.0))) * ((alpha - 1.0 - period / 4.0) * std::f32::consts::TAU / period).sin()
}

fn bounce_in(alpha: f32) -> f32 {
    1.0 - bounce_out(1.0 - alpha)
}

fn bounce_out(alpha: f32) -> f32 {
    let (n, d) = (7.5625, 2.75);
    if alpha < 1.0 / d {
        n * alpha * alpha
    } else if alpha < 2.0 / d {
        let x = alpha - 1.5 / d;
        n * x * x + 0.75
    } else if alpha < 2.5 / d {
        let x = alpha - 2.25 / d;
        n * x * x + 0.9375
    } else {
        let x = alpha - 2.625 / d;
        n * x * x + 0.984375
    }
}

// Model files (rbx-dom)

pub struct FileTree<'a>(pub &'a rbx_dom_weak::WeakDom);

impl<'a> FileTree<'a> {
    fn property(&self, node: rbx_dom_weak::types::Ref, property: &str) -> Option<&'a rbx_dom_weak::types::Variant> {
        self.0.get_by_ref(node)?.properties.get(&rbx_dom_weak::ustr(property))
    }
}

impl Tree for FileTree<'_> {
    type Node = rbx_dom_weak::types::Ref;

    fn class(&self, node: Self::Node) -> &str {
        self.0.get_by_ref(node).map_or("", |instance| instance.class.as_str())
    }

    fn name(&self, node: Self::Node) -> String {
        self.0.get_by_ref(node).map_or_else(String::new, |instance| instance.name.clone())
    }

    fn children(&self, node: Self::Node) -> Vec<Self::Node> {
        self.0.get_by_ref(node).map_or_else(Vec::new, |instance| instance.children().to_vec())
    }

    fn number(&self, node: Self::Node, property: &str) -> Option<f64> {
        use rbx_dom_weak::types::Variant;
        match self.property(node, property)? {
            Variant::Float32(value) => Some(*value as f64),
            Variant::Float64(value) => Some(*value),
            Variant::Int32(value) => Some(*value as f64),
            Variant::Int64(value) => Some(*value as f64),
            _ => None,
        }
    }

    fn boolean(&self, node: Self::Node, property: &str) -> Option<bool> {
        match self.property(node, property)? {
            rbx_dom_weak::types::Variant::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn cframe(&self, node: Self::Node, property: &str) -> Option<Affine3A> {
        match self.property(node, property)? {
            rbx_dom_weak::types::Variant::CFrame(cframe) => Some(crate::world::frame::to_affine(cframe)),
            _ => None,
        }
    }

    fn enumeration(&self, node: Self::Node, property: &str) -> Option<u32> {
        match self.property(node, property)? {
            rbx_dom_weak::types::Variant::Enum(item) => Some(item.to_u32()),
            _ => None,
        }
    }

    fn string(&self, node: Self::Node, property: &str) -> Option<String> {
        match self.property(node, property)? {
            rbx_dom_weak::types::Variant::String(value) => Some(value.clone()),
            _ => None,
        }
    }

    fn bytes(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        use rbx_dom_weak::types::Variant;
        match self.property(node, property)? {
            Variant::BinaryString(value) => Some(AsRef::<[u8]>::as_ref(value).to_vec()),
            Variant::SharedString(value) => Some(value.data().to_vec()),
            Variant::String(value) => Some(value.as_bytes().to_vec()),
            _ => None,
        }
    }
}

/// How long a clip is, whether it loops and its priority, without reading its poses.
pub fn header<T: Tree>(tree: &T, node: T::Node, curve: bool) -> (f32, bool, u32) {
    let length = if curve {
        super::curve_clip::length(tree, node)
    } else {
        tree.children(node).into_iter().filter(|&child| tree.class(child) == "Keyframe").map(|child| tree.number(child, "Time").unwrap_or(0.0) as f32).fold(0.0, f32::max)
    };
    (length, tree.boolean(node, "Loop").unwrap_or(true), tree.enumeration(node, "Priority").unwrap_or(PRIORITY_ACTION))
}

// An instance tree kept as data (a clip as it was when registered)

pub struct KeptView<'a> {
    pub tree: &'a crate::instance::KeptTree,
    pub reflection: &'a crate::Reflection,
}

impl KeptView<'_> {
    fn value(&self, node: u32, property: &str) -> Option<&crate::value::Value> {
        let name = self.reflection.name_id(property)?;
        self.tree.nodes()[node as usize].props.get(name)
    }
}

impl Tree for KeptView<'_> {
    type Node = u32;

    fn class(&self, node: Self::Node) -> &str {
        self.reflection.class(self.tree.nodes()[node as usize].class).name.as_ref()
    }

    fn name(&self, node: Self::Node) -> String {
        self.tree.nodes()[node as usize].name.to_str_lossy().into_owned()
    }

    fn children(&self, node: Self::Node) -> Vec<Self::Node> {
        self.tree.nodes()[node as usize].children.clone()
    }

    fn number(&self, node: Self::Node, property: &str) -> Option<f64> {
        self.value(node, property)?.as_number()
    }

    fn boolean(&self, node: Self::Node, property: &str) -> Option<bool> {
        self.value(node, property)?.as_bool()
    }

    fn cframe(&self, node: Self::Node, property: &str) -> Option<Affine3A> {
        Some(crate::world::frame::affine_of(Some(self.value(node, property)?)))
    }

    fn enumeration(&self, node: Self::Node, property: &str) -> Option<u32> {
        let item = self.value(node, property)?.as_enum()?;
        Some(self.reflection.enum_item_info(item).value as u32)
    }

    fn string(&self, node: Self::Node, property: &str) -> Option<String> {
        Some(self.value(node, property)?.as_str()?.to_str_lossy().into_owned())
    }

    fn text(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        Some(self.value(node, property)?.as_str()?.as_bytes().to_vec())
    }

    fn bytes(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        Some(self.value(node, property)?.as_str()?.as_bytes().to_vec())
    }
}

// The engine's instances

pub struct EngineTree<'a> {
    pub state: &'a crate::State,
    pub reflection: &'a crate::Reflection,
}

impl EngineTree<'_> {
    fn value(&self, node: crate::InstanceId, property: &str) -> Option<&crate::value::Value> {
        let name = self.reflection.name_id(property)?;
        self.state.dom.raw_get(node, name)
    }
}

impl Tree for EngineTree<'_> {
    type Node = crate::InstanceId;

    fn class(&self, node: Self::Node) -> &str {
        self.state.dom.get(node).map_or("", |instance| self.reflection.class(instance.class).name.as_ref())
    }

    fn name(&self, node: Self::Node) -> String {
        self.state.dom.get(node).map_or_else(String::new, |instance| instance.name.to_str_lossy().into_owned())
    }

    fn children(&self, node: Self::Node) -> Vec<Self::Node> {
        self.state.dom.children_of(node).to_vec()
    }

    fn number(&self, node: Self::Node, property: &str) -> Option<f64> {
        self.value(node, property)?.as_number()
    }

    fn boolean(&self, node: Self::Node, property: &str) -> Option<bool> {
        self.value(node, property)?.as_bool()
    }

    fn cframe(&self, node: Self::Node, property: &str) -> Option<Affine3A> {
        let value = self.value(node, property)?;
        Some(crate::world::frame::affine_of(Some(value)))
    }

    fn enumeration(&self, node: Self::Node, property: &str) -> Option<u32> {
        let item = self.value(node, property)?.as_enum()?;
        Some(self.reflection.enum_item_info(item).value as u32)
    }

    fn string(&self, node: Self::Node, property: &str) -> Option<String> {
        Some(self.value(node, property)?.as_str()?.to_str_lossy().into_owned())
    }

    fn text(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        Some(self.value(node, property)?.as_str()?.as_bytes().to_vec())
    }

    fn bytes(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        Some(self.value(node, property)?.as_str()?.as_bytes().to_vec())
    }
}

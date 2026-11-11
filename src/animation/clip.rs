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
/// Not a PoseEasingStyle: a key sampled from a CurveAnimation's curves ([`super::curve_clip`]). To the
/// next one the translation moves in a line and the rotation as the normalized mix of the two
/// quaternions.
pub const SAMPLED: u32 = u32::MAX;
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

/// A KeyframeMarker: its Keyframe's time, its name and Value.
#[derive(Clone, Debug)]
pub struct Marker {
    pub time: f32,
    pub name: String,
    pub value: String,
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
                        value: tree.string(child, "Value").unwrap_or_default(),
                    }),
                    _ => {}
                }
            }
            while let Some((pose, parent)) = stack.pop() {
                let name = tree.name(pose);
                for child in tree.children(pose) {
                    if tree.class(child) == "Pose" {
                        stack.push((child, name.clone()));
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
        clip.joints = joints;
        clip.keyframes.sort_by(|a, b| a.0.total_cmp(&b.0));
        clip.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        clip
    }

    /// The earliest keyframe named `name`.
    pub fn time_of(&self, name: &str) -> Option<f32> {
        self.names.iter().filter(|(_, n)| n == name).map(|(t, _)| *t).reduce(f32::min)
    }
}

/// A joint's offset (rotation, translation) at `time` seconds.
pub fn sample(keys: &[Key], time: f32) -> (Quat, Vec3A) {
    let next = keys.partition_point(|key| key.time <= time);
    if next == 0 {
        return (keys[0].rotation, keys[0].translation);
    }
    if next == keys.len() {
        let last = &keys[keys.len() - 1];
        return (last.rotation, last.translation);
    }
    let (a, b) = (&keys[next - 1], &keys[next]);
    let span = b.time - a.time;
    if span <= 0.0 {
        return (b.rotation, b.translation);
    }
    if a.style == SAMPLED {
        let alpha = ((time - a.time) / span).clamp(0.0, 1.0);
        let towards = if a.rotation.dot(b.rotation) < 0.0 { -b.rotation } else { b.rotation };
        return ((a.rotation * (1.0 - alpha) + towards * alpha).normalize(), a.translation.lerp(b.translation, alpha));
    }
    let alpha = ease(a.style, a.direction, ((time - a.time) / span).clamp(0.0, 1.0));
    (a.rotation.slerp(b.rotation, alpha), a.translation.lerp(b.translation, alpha))
}

/// A Pose's easing from its key to the next.
pub fn ease(style: u32, direction: u32, alpha: f32) -> f32 {
    let flipped = match direction {
        IN => OUT,
        OUT => IN,
        other => other,
    };
    match style {
        LINEAR => alpha,
        CONSTANT => 0.0,
        CUBIC => tween(cubic_in, flipped, alpha),
        CUBIC_V2 => tween(cubic_in, direction, alpha),
        ELASTIC => tween(elastic_in, flipped, alpha),
        BOUNCE => tween(bounce_in, flipped, alpha),
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

    fn bytes(&self, node: Self::Node, property: &str) -> Option<Vec<u8>> {
        Some(self.value(node, property)?.as_str()?.as_bytes().to_vec())
    }
}

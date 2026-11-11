//! A CurveAnimation as an AnimationTrack plays it, read through [`Tree`] like a KeyframeSequence.
//! Measured in Studio 0.747 (2026-11-11) on a three-part rig with clips built by script and
//! registered with AnimationClipProvider:RegisterAnimationClip, the tracks stepped with
//! Animator:StepAnimations (docs/engine.md "Animation curves"):
//!
//! - A Folder anywhere below the clip, reached through Folders only, is a joint: the Motor6D whose
//!   Part1 has the Folder's name, or the Bone of that name. The Folder above it does not matter
//!   (`Arm` poses the arm under `Root`, under `Nope`, and straight under the clip); a Model or a
//!   Configuration in a Folder's place is not read.
//! - Its child named `Position` is a Vector3Curve (FloatCurves `X`, `Y`, `Z`; one that is missing or
//!   has no keys gives 0) and its child named `Rotation` an EulerRotationCurve (the same three
//!   channels as angles, in its RotationOrder) or a RotationCurve; with two Rotation children the
//!   later one counts. Curves of other names pose nothing. A joint without a Rotation child leaves
//!   the rotation to the other playing tracks; one without a Position child still counts as a
//!   translation of 0 (a second track of only a rotation halves the first one's 10 studs to 5).
//! - Length is the last key's time among the curves inside the Folders (FloatCurves wherever they
//!   are in one, RotationCurves and MarkerCurves too); a curve straight under the clip adds nothing.
//! - Every MarkerCurve fires GetMarkerReachedSignal(its Name) with the marker's Value, under the
//!   clip or inside a Folder. KeyframeReached fires "End" when a pass ends, looped or not, and a
//!   clip of Length 0 has no end (the step: animation::advance_curve); GetTimeOfKeyframe raises
//!   "Invalid operation for this animation format.".
//! - **Tracks play samples, not the curves.** The pose at time `t` is the line between the curves'
//!   values at the two nearest multiples of 1/30 s: a Constant step from 0 to 30 at 0.5 s climbs
//!   from 0 at 14/30 s (3.75 at 0.47083 s, 15 at 0.48333 s), and a Cubic 0 to 100 over 1 s reads
//!   0.0814815 at 1/120 s where the curve itself is 0.0207. The samples start at 0 s whatever the
//!   first key's time, and the last one lies at or past Length (a clip of 0.26 s reads 25.4667 at
//!   its end, on the way from 23.333 at 7/30 s to 26 at 8/30 s). Rotations go between samples as
//!   the normalized mix of the two quaternions (measured 0.0249710 rad an eighth of the way to
//!   0.2 rad; a slerp gives 0.025). Studio also stores the samples' rotations rounded (up to 1e-4
//!   off the curve at a sample); this engine keeps them as computed.
//! - KeyframeSequenceProvider:GetKeyframeSequenceAsync of a CurveAnimation gives a KeyframeSequence
//!   made of it ([`sequence`]): named "KeyframeSequence", Loop true and Priority Action whatever the
//!   clip's are, a Keyframe "Keyframe" at each multiple of 1/30 s before Length and one at Length,
//!   in each a Pose per Folder (Weight 1 with a curve of its own, else 0; Linear, In) holding the
//!   curves' values at that time, a KeyframeMarker for each marker at a keyframe's time, and after
//!   them a Keyframe without poses for each marker time between or past them.
//!
//! Not measured: what a clip with keys before 0 s plays there (the samples here start at 0).

use glam::{Affine3A, Quat, Vec3A};
use rbx_dom_weak::{
    InstanceBuilder, WeakDom,
    types::{Enum, Ref, Variant},
};

use super::clip::{Clip, JointTrack, Key, Marker, PRIORITY_ACTION, SAMPLED, Tree};
use super::curve::{self, FloatKey, RotationKey};

/// The samples a second a track plays a CurveAnimation from.
pub const SAMPLE_RATE: f32 = 30.0;

/// The most samples a joint is given: a clip longer than 20 minutes is sampled more coarsely (a
/// file can name any length).
const MAX_SAMPLES: usize = 36_000;

enum Rotation {
    Euler([Vec<FloatKey>; 3], u32),
    Quaternion(Vec<RotationKey>),
}

/// A Folder of the clip: a joint when it has a channel.
struct Folder {
    name: String,
    /// The Folder it is in (None straight under the clip)
    parent: Option<usize>,
    position: Option<[Vec<FloatKey>; 3]>,
    rotation: Option<Rotation>,
}

impl Folder {
    fn posed(&self) -> bool {
        self.position.is_some() || self.rotation.is_some()
    }

    fn pose(&self, time: f32) -> (Quat, Vec3A) {
        let translation = self.position.as_ref().map_or(Vec3A::ZERO, |channels| {
            let at = |index: usize| curve::float_at(&channels[index], time).unwrap_or(0.0);
            Vec3A::new(at(0), at(1), at(2))
        });
        let rotation = match &self.rotation {
            Some(Rotation::Euler(channels, order)) => {
                let at = |index: usize| curve::float_at(&channels[index], time).unwrap_or(0.0);
                curve::euler(at(0), at(1), at(2), *order)
            }
            Some(Rotation::Quaternion(keys)) => curve::rotation_at(keys, time).unwrap_or(Quat::IDENTITY),
            None => Quat::IDENTITY,
        };
        (rotation, translation)
    }
}

#[derive(Default)]
struct Read {
    folders: Vec<Folder>,
    markers: Vec<Marker>,
    length: f32,
}

fn float_keys<T: Tree>(tree: &T, node: T::Node) -> Vec<FloatKey> {
    tree.bytes(node, "ValuesAndTimes").map_or_else(Vec::new, |bytes| curve::decode_floats(&bytes))
}

/// The X, Y and Z channels of a Vector3Curve or EulerRotationCurve (the first FloatCurve of each
/// name), and the last key's time among all its FloatCurves.
fn channels<T: Tree>(tree: &T, node: T::Node, length: &mut f32) -> [Vec<FloatKey>; 3] {
    let mut found: [Option<Vec<FloatKey>>; 3] = [None, None, None];
    for child in tree.children(node) {
        if tree.class(child) != "FloatCurve" {
            continue;
        }
        let keys = float_keys(tree, child);
        if let Some(last) = keys.last() {
            *length = length.max(curve::time_of(last.tick));
        }
        let slot = match tree.name(child).as_str() {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            _ => continue,
        };
        found[slot].get_or_insert(keys);
    }
    found.map(Option::unwrap_or_default)
}

fn visit<T: Tree>(tree: &T, node: T::Node, folder: Option<usize>, read: &mut Read) {
    for child in tree.children(node) {
        let class = tree.class(child);
        if class == "Folder" {
            read.folders.push(Folder { name: tree.name(child), parent: folder, position: None, rotation: None });
            visit(tree, child, Some(read.folders.len() - 1), read);
            continue;
        }
        if class == "MarkerCurve" {
            let name = tree.name(child);
            for marker in tree.bytes(child, "ValuesAndTimes").map_or_else(Vec::new, |bytes| curve::decode_markers(&bytes)) {
                let time = curve::time_of(marker.tick);
                if folder.is_some() {
                    read.length = read.length.max(time);
                }
                read.markers.push(Marker { time, name: name.clone(), value: String::from_utf8_lossy(&marker.value).into_owned() });
            }
            continue;
        }
        // (curves straight under the clip are not the clip's)
        let Some(folder) = folder else { continue };
        let name = tree.name(child);
        match class {
            "FloatCurve" => {
                if let Some(last) = float_keys(tree, child).last() {
                    read.length = read.length.max(curve::time_of(last.tick));
                }
            }
            "RotationCurve" => {
                let keys = tree.bytes(child, "ValuesAndTimes").map_or_else(Vec::new, |bytes| curve::decode_rotations(&bytes));
                if let Some(last) = keys.last() {
                    read.length = read.length.max(curve::time_of(last.tick));
                }
                if name == "Rotation" {
                    read.folders[folder].rotation = Some(Rotation::Quaternion(keys));
                }
            }
            "Vector3Curve" => {
                let found = channels(tree, child, &mut read.length);
                if name == "Position" {
                    read.folders[folder].position = Some(found);
                }
            }
            "EulerRotationCurve" => {
                let found = channels(tree, child, &mut read.length);
                if name == "Rotation" {
                    read.folders[folder].rotation = Some(Rotation::Euler(found, tree.enumeration(child, "RotationOrder").unwrap_or(0)));
                }
            }
            _ => {}
        }
    }
}

fn read<T: Tree>(tree: &T, clip: T::Node) -> Read {
    let mut read = Read::default();
    visit(tree, clip, None, &mut read);
    read.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    read
}

/// CurveAnimation.Length: the last key's time among the curves inside the clip's Folders.
pub fn length<T: Tree>(tree: &T, clip: T::Node) -> f32 {
    read(tree, clip).length
}

/// What AnimationClipProvider:RegisterAnimationClip refuses a clip for: a Folder's `Position` that
/// is not a Vector3Curve, or its `Rotation` that is not an EulerRotationCurve or a RotationCurve.
pub fn validate<T: Tree>(tree: &T, clip: T::Node) -> Result<(), String> {
    let clip_name = tree.name(clip);
    let mut stack: Vec<T::Node> = tree.children(clip).into_iter().rev().collect();
    while let Some(folder) = stack.pop() {
        if tree.class(folder) != "Folder" {
            continue;
        }
        for child in tree.children(folder) {
            let problem = match (tree.name(child).as_str(), tree.class(child)) {
                ("Position", class) if class != "Vector3Curve" => "Position channel. Only Vector3Curve is supported",
                ("Rotation", class) if class != "EulerRotationCurve" && class != "RotationCurve" => {
                    "Rotation channel. Only EulerRotationCurve or RotationCurve is supported"
                }
                _ => continue,
            };
            return Err(format!("CurveAnimation ({clip_name})'s descendant ({}) has an invalid {problem}", tree.name(folder)));
        }
        stack.extend(tree.children(folder).into_iter().rev());
    }
    Ok(())
}

/// The clip of CurveAnimation `node`: its joints sampled 30 times a second.
pub fn clip<T: Tree>(tree: &T, node: T::Node) -> Clip {
    let read = read(tree, node);
    let length = read.length;
    // The samples: every 1/30 s from 0 to the first one at or past the end
    let steps = length * SAMPLE_RATE;
    let last = if (steps - steps.round()).abs() < 1e-3 { steps.round() } else { steps.ceil() }.max(0.0) as usize;
    let times: Vec<f32> = if last <= MAX_SAMPLES {
        (0..=last).map(|step| step as f32 / SAMPLE_RATE).collect()
    } else {
        (0..=MAX_SAMPLES).map(|step| length * (step as f32 / MAX_SAMPLES as f32)).collect()
    };
    let parent_name = |folder: &Folder| folder.parent.map_or_else(String::new, |parent| read.folders[parent].name.clone());
    let joints = read
        .folders
        .iter()
        .filter(|folder| folder.posed())
        .map(|folder| JointTrack {
            parent: parent_name(folder),
            name: folder.name.clone(),
            keys: times
                .iter()
                .map(|&time| {
                    let (rotation, translation) = folder.pose(time);
                    Key { time, translation, rotation, style: SAMPLED, direction: 0 }
                })
                .collect(),
            rotation: folder.rotation.is_some(),
        })
        .collect();
    Clip {
        length,
        looped: tree.boolean(node, "Loop").unwrap_or(true),
        priority: tree.enumeration(node, "Priority").unwrap_or(PRIORITY_ACTION),
        joints,
        // (KeyframeReached's "End" is no keyframe at a time: the step fires it when a pass ends, animation::advance_curve)
        keyframes: Vec::new(),
        names: Vec::new(),
        markers: read.markers,
        curve: true,
    }
}

/// The KeyframeSequence KeyframeSequenceProvider:GetKeyframeSequenceAsync makes of CurveAnimation
/// `node`, as a model of its own.
pub fn sequence<T: Tree>(tree: &T, node: T::Node) -> (WeakDom, Ref) {
    let read = read(tree, node);
    let length = read.length;
    let mut times: Vec<f32> = (0..MAX_SAMPLES).map(|step| step as f32 / SAMPLE_RATE).take_while(|&time| time < length - 1e-5).collect();
    times.push(length);
    fn pose(folders: &[Folder], index: usize, time: f32) -> InstanceBuilder {
        let folder = &folders[index];
        let (rotation, translation) = folder.pose(time);
        let cframe = crate::world::frame::from_affine(&Affine3A::from_rotation_translation(rotation, translation.into()));
        InstanceBuilder::new("Pose")
            .with_name(folder.name.as_str())
            .with_property("Weight", Variant::Float32(if folder.posed() { 1.0 } else { 0.0 }))
            .with_property("CFrame", Variant::CFrame(cframe))
            .with_property("EasingStyle", Variant::Enum(Enum::from_u32(0)))
            .with_property("EasingDirection", Variant::Enum(Enum::from_u32(0)))
            .with_children((0..folders.len()).filter(|&child| folders[child].parent == Some(index)).map(|child| pose(folders, child, time)))
    }
    let keyframe = |time: f32| InstanceBuilder::new("Keyframe").with_name("Keyframe").with_property("Time", Variant::Float32(time));
    let marker = |marker: &Marker| {
        InstanceBuilder::new("KeyframeMarker").with_name(marker.name.as_str()).with_property("Value", Variant::String(marker.value.clone()))
    };
    let mut keyframes: Vec<(f32, InstanceBuilder)> = times
        .iter()
        .map(|&time| {
            let top = (0..read.folders.len()).filter(|&index| read.folders[index].parent.is_none());
            let mut made = keyframe(time).with_children(top.map(|index| pose(&read.folders, index, time)));
            for found in read.markers.iter().filter(|marker| marker.time == time) {
                made = made.with_child(marker(found));
            }
            (time, made)
        })
        .collect();
    // Markers between or past the pose keyframes: a keyframe of their own each time, after the others
    let posed = keyframes.len();
    for found in read.markers.iter().filter(|marker| !times.contains(&marker.time)) {
        match keyframes[posed..].iter().position(|(time, _)| *time == found.time) {
            Some(index) => {
                let (time, made) = keyframes.remove(posed + index);
                keyframes.insert(posed + index, (time, made.with_child(marker(found))));
            }
            None => keyframes.push((found.time, keyframe(found.time).with_child(marker(found)))),
        }
    }
    let sequence = InstanceBuilder::new("KeyframeSequence")
        .with_name("KeyframeSequence")
        .with_property("Loop", Variant::Bool(true))
        .with_property("Priority", Variant::Enum(Enum::from_u32(PRIORITY_ACTION)))
        .with_children(keyframes.into_iter().map(|(_, made)| made));
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let id = dom.insert(root, sequence);
    (dom, id)
}

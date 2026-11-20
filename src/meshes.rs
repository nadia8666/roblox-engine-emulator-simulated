//! EditableMesh offline: meshes scripts build or load from mesh assets, and the MeshParts that show
//! them. A MeshPart bound to an EditableMesh (CreateMeshPartAsync, ApplyMesh) points at a FileMesh
//! 4.00 snapshot of it under `<state dir>/editable-meshes/`, written again (named by its content)
//! before each frame the recorder samples after the mesh changed, so ror draws what the script made
//! as Roblox shows it: the mesh's own coordinates from the part's origin, scaled by Size / MeshSize
//! (InitialSize keeps MeshSize: ror scales by it), and skinned by its bones.
//!
//! Measured in Studio (2026-10-16): ids carry their kind in the high 32 bits (vertex 1, normal 2,
//! colour 3, uv 4, face 5, bone 6) over a slot from 1; a clone keeps them. A removed element's slot
//! is handed out again, the last freed first, with a generation over the kind's 3 bits (a face slot's
//! first reuse reads `13:3`), and the Get* lists are dense arrays a removed element's place in is
//! taken by the last (measured in a 0.747 client, tools/parity/probes/editable_mesh_vertex_batch.luau,
//! as is all below unless said). A vertex's faces and attributes (GetVertexFaces, BatchGetVertexAttributes)
//! list one entry per corner over it, the newest first: a corner is new when its face is made, its
//! face's vertices are set or its normal, uv or colour is set to another one. A new face's corner
//! (AddTriangle, BatchAdd) takes the normal, uv and colour the newest corner over its vertex uses,
//! else a new automatic normal, a white opaque colour and a (0, 0) uv
//! (tools/parity/probes/editable_mesh_batch.luau). An automatic normal is the sum, over the corners
//! that use it in the faces' order, of the corner's unit normal weighed by 1 - cos^2 of its angle
//! (the cosine of its unit edges), normalised, in single precision as that client computes it (bit
//! for bit on 17 of 17 shared normals); one no face uses reads (0, 0, 0). BatchRemove removes faces
//! only; on a fixed-size mesh (CreateEditableMeshAsync's default) every add and remove is refused.
//! Bone weights keep 1/255 steps. A mesh asset loads one vertex
//! per file vertex (none merged), each with its own normal, uv and colour, the file's bones and bind
//! CFrames, and four bone slots a skinned vertex (zero weights included). MeshSize is the mesh's box
//! when the part is made (at least 0.001) and stays when the mesh changes; ApplyMesh copies MeshSize
//! and keeps Size. RemoveUnused returns the ids it removed.

use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
    path::PathBuf,
    rc::Rc,
};

use glam::Vec3;
use mlua::prelude::*;
use rbx_dom_weak::types::{self as dom, Variant};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

use crate::{
    Engine, InstanceId, NameId, ScriptError, State, files,
    instance as core,
    lua::{Args, Flow, Ret, convert, instance_value, none, values},
    services::{Behaviors, method},
    value::{ContentValue, Str, Value},
};

const VERTEX: u64 = 1;
const NORMAL: u64 = 2;
const COLOR: u64 = 3;
const UV: u64 = 4;
const FACE: u64 = 5;
const BONE: u64 = 6;

/// The id of slot `slot` of `kind` in its `generation`: the high 32 bits are the generation over the kind's 3 bits, the
/// low ones the slot from 1 (measured: a face slot's first reuse reads `13:3`, its second `21:3`).
fn make_id(kind: u64, generation: u32, slot: u32) -> f64 {
    (((((generation as u64) << 3) | kind) << 32) | (slot as u64 + 1)) as f64
}

/// An id's kind (0 and 7 name none), generation and slot from 1 (0: none).
fn parts(id: i64) -> (u64, u32, u32) {
    let id = id as u64;
    let high = id >> 32;
    (high & 7, (high >> 3) as u32, id as u32)
}

/// A number read as an id as the client reads one: truncated toward zero, NaN and numbers past the int64 range the
/// integer indefinite (a C cast on x86). Measured: an id plus 0.5 reads the id, NaN reads as no id (not as 0).
fn id_of_number(n: f64) -> i64 {
    if n.is_nan() || n >= 9_223_372_036_854_775_808.0 || n < -9_223_372_036_854_775_808.0 { i64::MIN } else { n as i64 }
}

const KIND_NAMES: [&str; 8] = ["", "Vertex", "Normal", "Color", "UV", "Face", "Bone", "Unknown"];

/// The client's error for `id` where an id of `kind` was expected (measured: kind 7 is "Unknown", kind 0 no id at all).
fn wrong_kind(kind: u64, id: i64) -> Flow {
    let got = match parts(id).0 {
        0 => "value that doesn't appear to be an id".to_string(),
        other => format!("{} id", KIND_NAMES[other as usize]),
    };
    err(format!("Unexpected EditableMesh id type. Expected {} id, received {got}", KIND_NAMES[kind as usize]))
}

/// Items addressed by slot, as the client keeps a mesh's vertices, normals, colours, uvs, faces and bones (measured in a
/// 0.747 client, tools/parity/probes/editable_mesh_vertex_batch.luau; bones by analogy): a removed item's slot is handed
/// out again, the last freed first, under its next generation, and the Get* lists read a dense array where a new item
/// goes at the end and a removed one's place is taken by the last one.
#[derive(Clone, Debug)]
struct Slots<T> {
    items: Vec<Option<T>>,
    /// Per slot, how often it was freed: the generation its ids carry
    generations: Vec<u32>,
    /// Slots to hand out again, the last pushed first
    free: Vec<u32>,
    /// The live slots in listing order
    listed: Vec<u32>,
    /// Per slot, its place in `listed` while it is live
    places: Vec<u32>,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self { items: Vec::new(), generations: Vec::new(), free: Vec::new(), listed: Vec::new(), places: Vec::new() }
    }
}

impl<T> Slots<T> {
    fn grow(&mut self) -> u32 {
        self.items.push(None);
        self.generations.push(0);
        self.places.push(0);
        (self.items.len() - 1) as u32
    }
    /// A slot for a new item: the last one freed, else a new one.
    fn take(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| self.grow())
    }
    /// Makes `count` slots free, new ones pushed after those already free (BatchAdd's faces take them so: measured, a
    /// batch of three on a mesh with one free slot reads two new ids backwards, then the free one).
    fn reserve(&mut self, count: usize) {
        while self.free.len() < count {
            let slot = self.grow();
            self.free.push(slot);
        }
    }
    /// Puts `item` in `slot` (one [`Slots::take`] gave) at the end of the list.
    fn put(&mut self, slot: u32, item: T) {
        self.items[slot as usize] = Some(item);
        self.places[slot as usize] = self.listed.len() as u32;
        self.listed.push(slot);
    }
    fn add(&mut self, item: T) -> u32 {
        let slot = self.take();
        self.put(slot, item);
        slot
    }
    fn get(&self, slot: u32) -> Option<&T> {
        self.items.get(slot as usize).and_then(Option::as_ref)
    }
    fn get_mut(&mut self, slot: u32) -> Option<&mut T> {
        self.items.get_mut(slot as usize).and_then(Option::as_mut)
    }
    /// Frees `slot` for reuse under its next generation, leaving it listed until [`Slots::unlist`].
    fn release(&mut self, slot: u32) -> Option<T> {
        let item = self.items.get_mut(slot as usize)?.take()?;
        let generation = &mut self.generations[slot as usize];
        *generation = (*generation + 1) & 0x1FFF_FFFF;
        self.free.push(slot);
        Some(item)
    }
    /// Takes `slot` out of the list, the last listed item into its place.
    fn unlist(&mut self, slot: u32) {
        let place = self.places[slot as usize] as usize;
        self.listed.swap_remove(place);
        if let Some(&moved) = self.listed.get(place) {
            self.places[moved as usize] = place as u32;
        }
    }
    fn remove(&mut self, slot: u32) -> Option<T> {
        let item = self.release(slot)?;
        self.unlist(slot);
        Some(item)
    }
    /// Where `slot` is in the list.
    fn place(&self, slot: u32) -> u32 {
        self.places[slot as usize]
    }
    /// The live items in listing order (the Get* lists').
    fn live(&self) -> impl Iterator<Item = (u32, &T)> {
        self.listed.iter().map(|&slot| (slot, self.items[slot as usize].as_ref().expect("listed slots are live")))
    }
    /// The live items in slot order (a snapshot's).
    fn by_slot(&self) -> impl Iterator<Item = (u32, &T)> {
        self.items.iter().enumerate().filter_map(|(slot, item)| item.as_ref().map(|item| (slot as u32, item)))
    }
    fn count(&self) -> usize {
        self.listed.len()
    }
    /// The id of live slot `slot`.
    fn id(&self, kind: u64, slot: u32) -> f64 {
        make_id(kind, self.generations.get(slot as usize).copied().unwrap_or(0), slot)
    }
    /// The live slot `id` names, when it is of `kind` and of the slot's current generation.
    fn find(&self, kind: u64, id: i64) -> Option<u32> {
        let (k, generation, low) = parts(id);
        if k != kind || low == 0 {
            return None;
        }
        let slot = low - 1;
        (self.get(slot).is_some() && self.generations[slot as usize] == generation).then_some(slot)
    }
    /// [`Slots::find`], else the client's "Invalid id".
    fn at(&self, kind: u64, id: i64) -> Result<u32, Flow> {
        self.find(kind, id).ok_or_else(|| err("Invalid id"))
    }
}

#[derive(Clone, Copy, Debug)]
struct Face {
    vertices: [u32; 3],
    normals: [u32; 3],
    colors: [u32; 3],
    uvs: [u32; 3],
    /// Per corner, when it last came to the front of its vertex's corners (higher is newer): see [`Mesh::corners_of`]
    stamps: [u32; 3],
}

#[derive(Clone, Debug)]
struct Bone {
    name: String,
    /// CFrame components: position then the 3x3 rotation row by row (CFrame.new's order)
    cframe: [f32; 12],
    parent: Option<u32>,
    virtual_bone: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    positions: Slots<Vec3>,
    /// None: automatic (the faces' normal)
    normals: Slots<Option<Vec3>>,
    colors: Slots<([f32; 3], f32)>,
    uvs: Slots<[f32; 2]>,
    faces: Slots<Face>,
    bones: Slots<Bone>,
    /// Per vertex index: its bones (indices) and weights, when it has any
    skin: FxHashMap<u32, (Vec<u32>, Vec<f32>)>,
    /// The next corner stamp
    next_stamp: u32,
    /// Per vertex index, what the corner at the front of its corners uses: (its stamp, [normal, uv, colour]); kept up to
    /// date as faces are added or their corners set; None after a change that can take a vertex's front corner away (made
    /// again from the faces when next needed)
    corners: Option<FxHashMap<u32, (u32, [u32; 3])>>,
}

impl Mesh {
    fn face_normal(&self, face: &Face) -> Vec3 {
        let p = |i: usize| self.positions.get(face.vertices[i]).copied().unwrap_or(Vec3::ZERO);
        (p(1) - p(0)).cross(p(2) - p(0))
    }

    /// What corner `corner` of `face` adds to an automatic normal it uses: the corner's unit normal times 1 - cos^2 of its
    /// angle, with the operations in the client's order (glam's normalise multiplies by the length's reciprocal, as it does).
    fn corner_weight(&self, face: &Face, corner: usize) -> Vec3 {
        let p = |i: usize| self.positions.get(face.vertices[i % 3]).copied().unwrap_or(Vec3::ZERO);
        let at = p(corner);
        let (e1, e2) = (p(corner + 1) - at, p(corner + 2) - at);
        let cos = e1.normalize_or_zero().dot(e2.normalize_or_zero());
        e1.cross(e2).normalize_or_zero() * (1.0 - cos * cos)
    }

    /// The live faces in listing order (the sums below add in it: the order they were made while none was removed, which
    /// the client's sums follow bit for bit).
    fn faces_in_order(&self) -> impl Iterator<Item = &Face> {
        self.faces.live().map(|(_, face)| face)
    }

    /// Every automatic normal's value: the normalised sum of its corners' weights; (0, 0, 0) when no face uses it.
    fn automatic_normals(&self) -> FxHashMap<u32, Vec3> {
        let mut sums: FxHashMap<u32, Vec3> = FxHashMap::default();
        for face in self.faces_in_order() {
            for (corner, &n) in face.normals.iter().enumerate() {
                if matches!(self.normals.get(n), Some(None)) {
                    *sums.entry(n).or_insert(Vec3::ZERO) += self.corner_weight(face, corner);
                }
            }
        }
        sums.into_iter().map(|(n, sum)| (n, sum.normalize_or_zero())).collect()
    }

    fn normal_value(&self, index: u32) -> Option<Vec3> {
        match self.normals.get(index)? {
            Some(normal) => Some(*normal),
            None => {
                let mut sum = Vec3::ZERO;
                for face in self.faces_in_order() {
                    for (corner, &n) in face.normals.iter().enumerate() {
                        if n == index {
                            sum += self.corner_weight(face, corner);
                        }
                    }
                }
                Some(sum.normalize_or_zero())
            }
        }
    }

    /// The corners over vertex `v`, front first: (face slot, corner). Measured in a 0.747 client
    /// (tools/parity/probes/editable_mesh_vertex_batch.luau): a vertex's faces and attributes list one entry per corner
    /// (a face over it twice, twice), and a corner comes to the front when its face is made (its corners in order, so a
    /// face over a vertex twice lists its second corner first), when its face's vertices are set (all three, the same ones
    /// too), or when its normal, uv or colour is set to another one; a removed face's corners leave the rest in order.
    fn corners_of(&self, v: u32) -> Vec<(u32, usize)> {
        let mut found: Vec<(u32, u32, usize)> = Vec::new();
        for (f, face) in self.faces.live() {
            for corner in 0..3 {
                if face.vertices[corner] == v {
                    found.push((face.stamps[corner], f, corner));
                }
            }
        }
        found.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
        found.into_iter().map(|(_, f, corner)| (f, corner)).collect()
    }

    /// [`Mesh::corners_of`] for several vertices at once (one pass over the faces).
    fn corners_of_all(&self, vertices: &[u32]) -> FxHashMap<u32, Vec<(u32, usize)>> {
        let mut found: FxHashMap<u32, Vec<(u32, u32, usize)>> = vertices.iter().map(|&v| (v, Vec::new())).collect();
        for (f, face) in self.faces.live() {
            for corner in 0..3 {
                if let Some(list) = found.get_mut(&face.vertices[corner]) {
                    list.push((face.stamps[corner], f, corner));
                }
            }
        }
        found
            .into_iter()
            .map(|(v, mut list)| {
                list.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
                (v, list.into_iter().map(|(_, f, corner)| (f, corner)).collect())
            })
            .collect()
    }

    /// The normal, uv and colour the corner at the front of vertex `v`'s corners uses, if any face uses `v`.
    fn corner_of(&mut self, v: u32) -> Option<[u32; 3]> {
        if self.corners.is_none() {
            let mut corners: FxHashMap<u32, (u32, [u32; 3])> = FxHashMap::default();
            for (_, face) in self.faces.live() {
                for corner in 0..3 {
                    let here = (face.stamps[corner], [face.normals[corner], face.uvs[corner], face.colors[corner]]);
                    let entry = corners.entry(face.vertices[corner]).or_insert(here);
                    if here.0 >= entry.0 {
                        *entry = here;
                    }
                }
            }
            self.corners = Some(corners);
        }
        self.corners.as_ref().and_then(|corners| corners.get(&v)).map(|(_, attributes)| *attributes)
    }

    fn stamp(&mut self) -> u32 {
        self.next_stamp += 1;
        self.next_stamp
    }

    /// Brings corner `corner` of face `f` to the front of its vertex's corners (see [`Mesh::corners_of`]).
    fn touch(&mut self, f: u32, corner: usize) {
        let stamp = self.stamp();
        let Some(face) = self.faces.get_mut(f) else { return };
        face.stamps[corner] = stamp;
        let face = *face;
        if let Some(corners) = self.corners.as_mut() {
            corners.insert(face.vertices[corner], (stamp, [face.normals[corner], face.uvs[corner], face.colors[corner]]));
        }
    }

    /// Sets attribute `which` (1 normal, 2 colour, 3 uv) of corner `corner` of face `f` to `slot`; a corner given another
    /// one comes to the front of its vertex's corners.
    fn set_corner(&mut self, f: u32, corner: usize, which: u32, slot: u32) {
        let Some(face) = self.faces.get_mut(f) else { return };
        let target = match which {
            1 => &mut face.normals[corner],
            2 => &mut face.colors[corner],
            _ => &mut face.uvs[corner],
        };
        if *target != slot {
            *target = slot;
            self.touch(f, corner);
        }
    }

    /// Sets face `f`'s vertices: all three corners come to the front of their (new) vertices' corners.
    fn set_face_vertices(&mut self, f: u32, vertices: [u32; 3]) {
        let stamps = [self.stamp(), self.stamp(), self.stamp()];
        if let Some(face) = self.faces.get_mut(f) {
            face.vertices = vertices;
            face.stamps = stamps;
        }
        self.faces_moved();
    }

    /// After a change that can take a vertex's front corner away (a face removed, a face's vertices set).
    fn faces_moved(&mut self) {
        self.corners = None;
    }

    /// A new face over `vertices`: each corner takes what the corner at the front of its vertex's corners uses (a vertex
    /// twice in the face shares it), else a new automatic normal, a (0, 0) uv and a white opaque colour, made corner by
    /// corner.
    fn new_face(&mut self, vertices: [u32; 3]) -> Face {
        let mut attributes = [[0u32; 3]; 3];
        for corner in 0..3 {
            let v = vertices[corner];
            attributes[corner] = match vertices[..corner].iter().position(|&earlier| earlier == v) {
                Some(earlier) => attributes[earlier],
                None => match self.corner_of(v) {
                    Some(found) => found,
                    None => [self.normals.add(None), self.uvs.add([0.0; 2]), self.colors.add(([1.0; 3], 1.0))],
                },
            };
        }
        let stamps = [self.stamp(), self.stamp(), self.stamp()];
        if let Some(corners) = self.corners.as_mut() {
            for corner in 0..3 {
                corners.insert(vertices[corner], (stamps[corner], attributes[corner]));
            }
        }
        Face {
            vertices,
            normals: attributes.map(|a| a[0]),
            uvs: attributes.map(|a| a[1]),
            colors: attributes.map(|a| a[2]),
            stamps,
        }
    }

    /// Removes faces (BatchRemove, RemoveFace): measured, in the order of their places in the list from the last, each one's
    /// slot freed and its place taken by the last listed face.
    fn remove_faces(&mut self, faces: &[u32]) {
        let mut order: Vec<u32> = faces.to_vec();
        order.sort_unstable_by_key(|&f| std::cmp::Reverse(self.faces.place(f)));
        order.dedup();
        for f in order {
            self.faces.remove(f);
        }
        self.faces_moved();
    }

    fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut out: Option<(Vec3, Vec3)> = None;
        for (_, &p) in self.positions.live() {
            out = Some(match out {
                Some((min, max)) => (min.min(p), max.max(p)),
                None => (p, p),
            });
        }
        out
    }

    /// The mesh's box size as MeshSize reads it (each axis at least 0.001).
    pub fn size(&self) -> Vec3 {
        self.bounds().map_or(Vec3::ZERO, |(min, max)| max - min).max(Vec3::splat(0.001))
    }

    /// Whether a part made from it is skinned (HasSkinnedMesh): some vertex weighs a bone above 0
    /// (measured: bones that no vertex weighs leave it false).
    pub fn skinned(&self) -> bool {
        self.skin.iter().any(|(&vertex, (bones, weights))| {
            self.positions.get(vertex).is_some()
                && bones.iter().zip(weights).any(|(&bone, &weight)| weight > 0.0 && self.bones.get(bone).is_some())
        })
    }

    /// Removes what no face uses: vertices, normals, colours, uvs (Studio's RemoveUnused); the ids, each kind in listing
    /// order. Measured in a 0.747 client: the slots are freed in that order (the last of them handed out first) while the
    /// list is closed up from its end (each removed place taken by the last listed item, the last place first).
    fn remove_unused(&mut self) -> Vec<f64> {
        fn sweep<T>(slots: &mut Slots<T>, kind: u64, used: &FxHashSet<u32>, removed: &mut Vec<f64>) -> Vec<u32> {
            let unused: Vec<u32> = slots.live().map(|(slot, _)| slot).filter(|slot| !used.contains(slot)).collect();
            removed.extend(unused.iter().map(|&slot| slots.id(kind, slot)));
            for &slot in &unused {
                slots.release(slot);
            }
            for &slot in unused.iter().rev() {
                slots.unlist(slot);
            }
            unused
        }
        let mut used = [FxHashSet::default(), FxHashSet::default(), FxHashSet::default(), FxHashSet::default()];
        for (_, face) in self.faces.live() {
            used[0].extend(face.vertices);
            used[1].extend(face.normals);
            used[2].extend(face.colors);
            used[3].extend(face.uvs);
        }
        let mut removed = Vec::new();
        for vertex in sweep(&mut self.positions, VERTEX, &used[0], &mut removed) {
            self.skin.remove(&vertex);
        }
        sweep(&mut self.normals, NORMAL, &used[1], &mut removed);
        sweep(&mut self.colors, COLOR, &used[2], &mut removed);
        sweep(&mut self.uvs, UV, &used[3], &mut removed);
        removed
    }

    /// A mesh from a decoded FileMesh: its first LOD's faces over every file vertex.
    #[cfg(feature = "meshes")]
    fn from_file(file: &ror_mesh::Mesh) -> Mesh {
        let mut mesh = Mesh::default();
        let count = file.positions.len();
        for index in 0..count {
            mesh.positions.add(Vec3::from(file.positions[index]));
            mesh.normals.add(Some(file.normals.get(index).map_or(Vec3::ZERO, |n| Vec3::from(*n))));
            let color = file.colors.get(index).copied().unwrap_or([255; 4]);
            mesh.colors.add(([color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0], color[3] as f32 / 255.0));
            mesh.uvs.add(file.uvs.get(index).copied().unwrap_or([0.0; 2]));
        }
        let range = file.lods.first().cloned().unwrap_or(0..file.indices.len() as u32);
        for corners in file.indices[range.start as usize..range.end as usize].chunks_exact(3) {
            let c = [corners[0], corners[1], corners[2]];
            if c.iter().all(|&i| (i as usize) < count) {
                let stamps = [mesh.stamp(), mesh.stamp(), mesh.stamp()];
                mesh.faces.add(Face { vertices: c, normals: c, colors: c, uvs: c, stamps });
            }
        }
        if let Some(skinning) = &file.skinning {
            for bone in &skinning.bones {
                let r = bone.rotation;
                let p = bone.position;
                mesh.bones.add(Bone {
                    name: bone.name.clone(),
                    cframe: [p[0], p[1], p[2], r[0][0], r[0][1], r[0][2], r[1][0], r[1][1], r[1][2], r[2][0], r[2][1], r[2][2]],
                    parent: bone.parent.map(u32::from),
                    virtual_bone: false,
                });
            }
            for (vertex, (joints, weights)) in skinning.joints.iter().zip(&skinning.weights).enumerate() {
                if weights.iter().any(|&w| w > 0.0) {
                    mesh.skin.insert(vertex as u32, (joints.iter().map(|&j| j as u32).collect(), weights.to_vec()));
                }
            }
        }
        mesh
    }
}

/// A FileMesh 4.00 of `mesh` (Roblox's layout: ror-mesh's filemesh.rs and skinning.rs read it):
/// one file vertex per distinct (vertex, normal, uv, colour) corner, skinned meshes in subsets of
/// at most 26 bones.
pub fn encode(mesh: &Mesh) -> Vec<u8> {
    let automatic = mesh.automatic_normals();
    // (faces and bones in slot order, as snapshots have always had them)
    let bone_order: Vec<u32> = mesh.bones.by_slot().map(|(i, _)| i).collect();
    let bone_slot: FxHashMap<u32, u16> = bone_order.iter().enumerate().map(|(slot, &i)| (i, slot as u16)).collect();
    let skinned = !bone_order.is_empty();
    let faces: Vec<Face> = mesh.faces.by_slot().map(|(_, f)| *f).collect();

    // Subsets: faces in order, a new subset when a face's bones would pass 26
    let vertex_bones = |vertex: u32| -> Vec<u16> {
        mesh.skin.get(&vertex).map_or(Vec::new(), |(bones, weights)| {
            bones.iter().zip(weights).filter(|(_, w)| **w > 0.0).filter_map(|(b, _)| bone_slot.get(b).copied()).collect()
        })
    };
    let mut groups: Vec<(Vec<usize>, Vec<u16>)> = vec![(Vec::new(), Vec::new())];
    for (index, face) in faces.iter().enumerate() {
        let mut needed: Vec<u16> = Vec::new();
        if skinned {
            for &v in &face.vertices {
                for bone in vertex_bones(v) {
                    if !needed.contains(&bone) {
                        needed.push(bone);
                    }
                }
            }
        }
        let group = groups.last_mut().unwrap();
        let extra = needed.iter().filter(|b| !group.1.contains(b)).count();
        if group.1.len() + extra > 26 && !group.0.is_empty() {
            groups.push((Vec::new(), Vec::new()));
        }
        let group = groups.last_mut().unwrap();
        for bone in needed {
            if !group.1.contains(&bone) {
                group.1.push(bone);
            }
        }
        group.0.push(index);
    }

    let mut vertices: Vec<u8> = Vec::new();
    let mut envelopes: Vec<u8> = Vec::new();
    let mut indices: Vec<u32> = Vec::with_capacity(faces.len() * 3);
    let mut subsets: Vec<[u32; 4]> = Vec::new();
    let mut culling = vec![0f32; bone_order.len()];
    let mut vertex_count = 0u32;
    for (group_faces, group_bones) in &groups {
        let first_vertex = vertex_count;
        let first_face = (indices.len() / 3) as u32;
        let mut seen: FxHashMap<[u32; 4], u32> = FxHashMap::default();
        for &face_index in group_faces {
            let face = faces[face_index];
            for corner in 0..3 {
                let key = [face.vertices[corner], face.normals[corner], face.uvs[corner], face.colors[corner]];
                let index = *seen.entry(key).or_insert_with(|| {
                    let position = mesh.positions.get(key[0]).copied().unwrap_or(Vec3::ZERO);
                    let normal = match mesh.normals.get(key[1]) {
                        Some(Some(normal)) => *normal,
                        Some(None) => automatic.get(&key[1]).copied().unwrap_or(Vec3::ZERO),
                        None => mesh.face_normal(&face).normalize_or_zero(),
                    };
                    let uv = mesh.uvs.get(key[2]).copied().unwrap_or([0.0; 2]);
                    let (rgb, alpha) = mesh.colors.get(key[3]).copied().unwrap_or(([1.0; 3], 1.0));
                    for value in [position.x, position.y, position.z, normal.x, normal.y, normal.z, uv[0], uv[1]] {
                        vertices.extend_from_slice(&value.to_le_bytes());
                    }
                    vertices.extend_from_slice(&[127, 127, 127, 127]);
                    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    vertices.extend_from_slice(&[byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), byte(alpha)]);
                    if skinned {
                        let mut slots = [0u8; 4];
                        let mut bytes = [0u8; 4];
                        if let Some((bones, weights)) = mesh.skin.get(&key[0]) {
                            let pairs: Vec<(u16, f32)> = bones.iter().zip(weights).filter(|(_, w)| **w > 0.0).filter_map(|(b, w)| bone_slot.get(b).map(|s| (*s, *w))).take(4).collect();
                            let total: f32 = pairs.iter().map(|(_, w)| w).sum();
                            if total > 0.0 {
                                let mut left = 255i32;
                                for (slot, &(bone, weight)) in pairs.iter().enumerate() {
                                    let value = if slot + 1 == pairs.len() { left } else { ((weight / total) * 255.0).round() as i32 };
                                    let value = value.clamp(0, left);
                                    left -= value;
                                    slots[slot] = group_bones.iter().position(|b| *b == bone).unwrap_or(0) as u8;
                                    bytes[slot] = value as u8;
                                    let reach = (position - bone_position(&mesh.bones, bone_order[bone as usize])).length();
                                    culling[bone as usize] = culling[bone as usize].max(reach);
                                }
                            }
                        }
                        envelopes.extend_from_slice(&slots);
                        envelopes.extend_from_slice(&bytes);
                    }
                    vertex_count += 1;
                    vertex_count - 1
                });
                indices.push(index);
            }
        }
        subsets.push([first_face, (indices.len() / 3) as u32 - first_face, first_vertex, vertex_count - first_vertex]);
    }

    let mut names: Vec<u8> = Vec::new();
    let mut bones: Vec<u8> = Vec::new();
    for (slot, &index) in bone_order.iter().enumerate() {
        let bone = mesh.bones.get(index).unwrap();
        let offset = names.len() as u32;
        names.extend_from_slice(bone.name.as_bytes());
        names.push(0);
        let parent = bone.parent.and_then(|p| bone_slot.get(&p).copied()).unwrap_or(0xFFFF);
        bones.extend_from_slice(&offset.to_le_bytes());
        bones.extend_from_slice(&parent.to_le_bytes());
        bones.extend_from_slice(&parent.to_le_bytes());
        bones.extend_from_slice(&culling[slot].to_le_bytes());
        for value in &bone.cframe[3..12] {
            bones.extend_from_slice(&value.to_le_bytes());
        }
        for value in &bone.cframe[0..3] {
            bones.extend_from_slice(&value.to_le_bytes());
        }
    }

    let face_count = (indices.len() / 3) as u32;
    let subset_count = if skinned { subsets.len() as u16 } else { 0 };
    let mut out = Vec::with_capacity(13 + 24 + vertices.len() + envelopes.len() + indices.len() * 4 + 8 + bones.len() + names.len() + subsets.len() * 72);
    out.extend_from_slice(b"version 4.00\n");
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&vertex_count.to_le_bytes());
    out.extend_from_slice(&face_count.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(bone_order.len() as u16).to_le_bytes());
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    out.extend_from_slice(&subset_count.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&vertices);
    out.extend_from_slice(&envelopes);
    for index in &indices {
        out.extend_from_slice(&index.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&face_count.to_le_bytes());
    out.extend_from_slice(&bones);
    out.extend_from_slice(&names);
    if skinned {
        for (subset, (_, group_bones)) in subsets.iter().zip(&groups) {
            for value in subset {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&(group_bones.len() as u32).to_le_bytes());
            for slot in 0..26 {
                out.extend_from_slice(&group_bones.get(slot).copied().unwrap_or(0xFFFF).to_le_bytes());
            }
        }
    }
    out
}

fn bone_position(bones: &Slots<Bone>, index: u32) -> Vec3 {
    bones.get(index).map_or(Vec3::ZERO, |b| Vec3::new(b.cframe[0], b.cframe[1], b.cframe[2]))
}

/// The engine's meshes, the parts bound to them and the meshes changed since their last snapshot.
#[derive(Default)]
pub(crate) struct MeshExtension {
    meshes: RefCell<FxHashMap<InstanceId, Mesh>>,
    users: RefCell<FxHashMap<InstanceId, Vec<InstanceId>>>,
    /// Each bound part's mesh
    bound: RefCell<FxHashMap<InstanceId, InstanceId>>,
    dirty: RefCell<FxHashSet<InstanceId>>,
    names: RefCell<Option<Rc<Names>>>,
    /// Decoded asset meshes by uri (a warm service decodes each once per engine)
    #[cfg_attr(not(feature = "meshes"), allow(dead_code))]
    assets: RefCell<FxHashMap<String, Rc<Mesh>>>,
}

impl crate::Extension for MeshExtension {
    fn teardown(&self) {
        self.meshes.borrow_mut().clear();
        self.users.borrow_mut().clear();
        self.bound.borrow_mut().clear();
        self.dirty.borrow_mut().clear();
        self.assets.borrow_mut().clear();
    }
}

fn extension(engine: &Engine) -> &MeshExtension {
    engine.extension_or_insert_with(MeshExtension::default)
}

struct Names {
    mesh_id: NameId,
    mesh_size: NameId,
    initial_size: NameId,
    size: NameId,
    fixed_size: NameId,
    render_fidelity: NameId,
    collision_fidelity: NameId,
    has_skinned_mesh: NameId,
    editable_mesh: crate::ClassId,
    mesh_part: crate::ClassId,
}

fn names(engine: &Engine) -> Option<Rc<Names>> {
    extension(engine).names.borrow().clone()
}

/// A colour's alpha as a mesh keeps it (measured: 1/255 steps, 0.25 reads 64/255).
fn alpha(value: f64) -> f32 {
    ((value.clamp(0.0, 1.0) * 255.0).round() / 255.0) as f32
}

fn err(message: impl Into<String>) -> Flow {
    // (an engine method's error: no script position, as Roblox raises them)
    Flow::Error(ScriptError::plain(message))
}

fn vec3_of(value: &LuaValue) -> Option<Vec3> {
    match convert::from_lua(value) {
        Value::Vector3(v) => Some(Vec3::new(v.x, v.y, v.z)),
        _ => None,
    }
}

fn vec2_of(value: &LuaValue) -> Option<[f32; 2]> {
    match convert::from_lua(value) {
        Value::Vector2(v) => Some([v.x, v.y]),
        _ => None,
    }
}

fn color_of(value: &LuaValue) -> Option<[f32; 3]> {
    match convert::from_lua(value) {
        Value::Color3(c) => Some([c.r, c.g, c.b]),
        _ => None,
    }
}

fn cframe_of(value: &LuaValue) -> Option<[f32; 12]> {
    match convert::from_lua(value) {
        Value::Dom(variant) => match &*variant {
            Variant::CFrame(c) => {
                let o = c.orientation;
                Some([c.position.x, c.position.y, c.position.z, o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z])
            }
            _ => None,
        },
        _ => None,
    }
}

fn vector_value(lua: &Lua, engine: &Engine, v: Vec3) -> LuaResult<LuaValue> {
    convert::to_lua(lua, engine, &Value::Vector3(dom::Vector3::new(v.x, v.y, v.z)))
}

fn ids_value(lua: &Lua, ids: impl Iterator<Item = f64>) -> LuaResult<LuaValue> {
    Ok(LuaValue::Table(lua.create_sequence_from(ids)?))
}

/// The array part of `table`, 1 to `#table` (a hole is nil).
fn sequence(table: &LuaTable) -> Vec<LuaValue> {
    (1..=table.raw_len()).map(|index| table.raw_get::<LuaValue>(index).unwrap_or(LuaValue::Nil)).collect()
}

/// The vertex a BatchAdd face names (measured: the client's errors for an id of another kind, a number that is no id,
/// and a vertex that is gone).
fn face_vertex(m: &Mesh, value: &LuaValue) -> Result<u32, Flow> {
    let id = match value {
        LuaValue::Number(n) => id_of_number(*n),
        LuaValue::Integer(n) => *n,
        _ => 0,
    };
    if parts(id).0 != VERTEX {
        return Err(wrong_kind(VERTEX, id));
    }
    m.positions.at(VERTEX, id)
}

/// A table's array part as numbers (other values dropped).
fn numbers_of(value: &LuaValue) -> Vec<f64> {
    match value {
        LuaValue::Table(table) => table.sequence_values::<f64>().filter_map(Result::ok).collect(),
        _ => Vec::new(),
    }
}

/// A table's array part as ids, unreadable values dropped.
fn ids_of(value: &LuaValue) -> Vec<i64> {
    numbers_of(value).into_iter().map(id_of_number).collect()
}

/// The slots of the live ids of `kind` among `ids`.
fn slots_of<T>(slots: &Slots<T>, kind: u64, ids: &[i64]) -> Vec<u32> {
    ids.iter().filter_map(|&id| slots.find(kind, id)).collect()
}

/// An int64 argument as the client casts one (measured: a number rounds half away from zero, a numeric string reads as
/// its number and a boolean as 1 or 0; another string is "Unable to cast string to int64", nil "Argument 2 missing or
/// nil"; other values unmeasured, cast errors naming their type).
fn int64_arg(lua: &Lua, engine: &Engine, args: &Args, at: usize) -> Result<i64, Flow> {
    let value = args.get(at);
    let number = match value {
        LuaValue::Nil => return Err(err(format!("Argument {at} missing or nil"))),
        LuaValue::Integer(n) => return Ok(*n),
        LuaValue::Number(n) => *n,
        LuaValue::Boolean(b) => return Ok(*b as i64),
        LuaValue::String(_) => lua.coerce_number(value.clone()).ok().flatten().ok_or_else(|| err("Unable to cast string to int64"))?,
        other => return Err(err(format!("Unable to cast {} to int64", crate::lua::typeof_value(engine, other)))),
    };
    Ok(id_of_number(number.round()))
}

/// Argument `at`, an id of `kind` (the client's cast, then its checks: 0 is "Invalid id" and an id of another kind is
/// named; whether its slot is live is the caller's [`Slots::at`]).
fn id_arg(lua: &Lua, engine: &Engine, args: &Args, at: usize, kind: u64) -> Result<i64, Flow> {
    let id = int64_arg(lua, engine, args, at)?;
    check_kind(id, kind)?;
    Ok(id)
}

/// The client's first checks of an id it reads: 0 is "Invalid id", an id of another kind is named.
fn check_kind(id: i64, kind: u64) -> Result<(), Flow> {
    if id == 0 {
        return Err(err("Invalid id"));
    }
    if parts(id).0 != kind {
        return Err(wrong_kind(kind, id));
    }
    Ok(())
}

/// Element `index` (from 1) of an id array a per-vertex batch reads: a number, a numeric string, else "<method>: <name>
/// at index <index> is not a number" (measured; an id plus 0.5 reads the id).
fn element_id(lua: &Lua, value: &LuaValue, method: &str, name: &str, index: usize) -> Result<i64, Flow> {
    let number = match value {
        LuaValue::Integer(n) => return Ok(*n),
        LuaValue::Number(n) => Some(*n),
        LuaValue::String(_) => lua.coerce_number(value.clone()).ok().flatten(),
        _ => None,
    };
    number.map(id_of_number).ok_or_else(|| err(format!("{method}: {name} at index {index} is not a number")))
}

/// The refusal of every add and remove on a fixed-size mesh (measured in a 0.747 client: AddVertex, AddNormal, AddUV,
/// AddColor, AddTriangle, AddBone, BatchAdd, RemoveFace, BatchRemove, RemoveUnused and RemoveBone, each before its other
/// checks, BatchAdd and BatchRemove of an empty array too).
fn refuse_fixed(engine: &Engine, id: InstanceId) -> Result<(), Flow> {
    if fixed(engine, id) {
        return Err(err("EditableMesh is fixed-size, adding or removing elements is not allowed in-experience"));
    }
    Ok(())
}

/// A MeshAttribute argument (measured: an item of another enum is "Invalid type 'KeyCode' for enum 'MeshAttribute'",
/// anything else, its name as a string included, "Invalid value for enum 'MeshAttribute'"): Vertex 0, Normal 1, Color 2,
/// UV 3, Face 4.
fn mesh_attribute(engine: &Engine, value: &LuaValue) -> Result<u32, Flow> {
    match convert::from_lua(value) {
        Value::EnumItem(item) => {
            let kind = engine.reflection.enum_item_type_name(item);
            if kind == "MeshAttribute" {
                Ok(engine.reflection.enum_item_info(item).value as u32)
            } else {
                Err(err(format!("Invalid type '{kind}' for enum 'MeshAttribute'")))
            }
        }
        _ => Err(err("Invalid value for enum 'MeshAttribute'")),
    }
}

/// Runs `f` on the mesh `id`; `changed` marks it for a new snapshot, also when `f` raises (a batch that raised part way
/// keeps what it set before).
fn with_mesh<T>(engine: &Engine, id: InstanceId, changed: bool, f: impl FnOnce(&mut Mesh) -> Result<T, Flow>) -> Result<T, Flow> {
    let ext = extension(engine);
    let result = {
        let mut meshes = ext.meshes.borrow_mut();
        let mesh = meshes.get_mut(&id).ok_or_else(|| err("This EditableMesh has been destroyed"))?;
        f(mesh)
    };
    if changed {
        ext.dirty.borrow_mut().insert(id);
    }
    result
}

fn fixed(engine: &Engine, id: InstanceId) -> bool {
    let Some(names) = names(engine) else { return false };
    engine.state.borrow().dom.boolean(id, names.fixed_size).unwrap_or(false)
}

fn new_mesh(lua: &Lua, engine: &Rc<Engine>, mesh: Mesh, fixed_size: bool) -> Ret {
    let id = new_mesh_id(engine, mesh, fixed_size)?;
    Ok(values([instance_value(lua, engine, id)?]))
}

fn new_mesh_id(engine: &Rc<Engine>, mesh: Mesh, fixed_size: bool) -> Result<InstanceId, Flow> {
    let names = names(engine).ok_or_else(|| err("EditableMesh is not available"))?;
    let id = {
        let mut state = engine.state.borrow_mut();
        let id = core::create(&mut state, &engine.reflection, names.editable_mesh);
        core::raw_set_silent(&mut state, &engine.reflection, id, names.fixed_size, Value::Bool(fixed_size));
        id
    };
    extension(engine).meshes.borrow_mut().insert(id, mesh);
    Ok(id)
}

/// `options.FixedSize` of a CreateEditableMeshAsync options table: true unless it says false (measured in a 0.747 client
/// for an EditableMesh's Content: no options make a fixed-size copy). CreateEditableMesh takes none: a mesh it makes is
/// never fixed-size (measured: `{ FixedSize = true }` reads false and takes vertices).
fn fixed_option(value: &LuaValue) -> bool {
    match value {
        LuaValue::Table(table) => table.get::<Option<bool>>("FixedSize").ok().flatten().unwrap_or(true),
        _ => true,
    }
}

/// The EditableMesh a Content holds (Content.fromObject), if any.
fn content_object(engine: &Engine, value: &LuaValue) -> Option<InstanceId> {
    match convert::from_lua(value) {
        Value::Content(content) => match &*content {
            ContentValue::Object(id) => extension(engine).meshes.borrow().contains_key(id).then_some(*id),
            _ => None,
        },
        _ => None,
    }
}

fn content_uri(value: &LuaValue) -> Option<String> {
    match convert::from_lua(value) {
        // (an Opaque Content's copy: CreateDataModelContentAsync)
        Value::Content(content) => content.location().map(|uri| uri.to_string()),
        Value::String(uri) => Some(uri.to_string()),
        Value::Number(id) if id > 0.0 => Some(format!("rbxassetid://{}", id as u64)),
        _ => None,
    }
}

/// The mesh asset `uri`, decoded (cached per engine).
#[cfg(feature = "meshes")]
fn load_asset(engine: &Engine, uri: &str) -> Result<Rc<Mesh>, String> {
    if let Some(mesh) = extension(engine).assets.borrow().get(uri) {
        return Ok(mesh.clone());
    }
    files::note_unchecked("a mesh was read");
    // a local file (a relative one the project's), else an asset downloaded once
    let path = match engine.options.local_file(uri) {
        Some(path) => path,
        None => {
            let id = crate::animation::source::asset_id(uri).ok_or_else(|| format!("{uri}: not an asset id"))?;
            let cache = crate::state_dir(&engine.options.repo).join("meshes");
            let cached = cache.join(id.to_string());
            if !files::is_file(&cached) {
                crate::animation::source::fetch(engine, uri, &cache, &cached)?;
            }
            cached
        }
    };
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let file = ror_mesh::decode_mesh(&bytes).map_err(|error| format!("{uri}: {error}"))?;
    let mesh = Rc::new(Mesh::from_file(&file));
    extension(engine).assets.borrow_mut().insert(uri.to_string(), mesh.clone());
    Ok(mesh)
}

#[cfg(not(feature = "meshes"))]
fn load_asset(_: &Engine, uri: &str) -> Result<Rc<Mesh>, String> {
    Err(format!("{uri}: this build decodes no mesh assets (feature `meshes`)"))
}

/// Writes `mesh`'s snapshot (named by its content) and returns its path.
fn snapshot(engine: &Engine, mesh: &Mesh) -> Result<PathBuf, String> {
    files::note_unchecked("a mesh was written");
    let bytes = encode(mesh);
    let mut hasher = FxHasher::default();
    bytes.hash(&mut hasher);
    let directory = crate::state_dir(&engine.options.repo).join("editable-meshes");
    let path = directory.join(format!("{:016x}-{}.mesh", hasher.finish(), bytes.len()));
    if !files::is_file(&path) {
        std::fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;
        std::fs::write(&path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(path)
}

fn content_of(path: &std::path::Path) -> Value {
    Value::String(Str::from(format!("file://{}", path.display()).as_str()))
}

fn size_value(v: Vec3) -> Value {
    Value::Vector3(dom::Vector3::new(v.x, v.y, v.z))
}

/// Binds `part` to `mesh` (its later snapshots go to the part) and points it at `content`.
fn bind(engine: &Engine, part: InstanceId, mesh: Option<InstanceId>) {
    let ext = extension(engine);
    if let Some(old) = ext.bound.borrow_mut().remove(&part) {
        if let Some(users) = ext.users.borrow_mut().get_mut(&old) {
            users.retain(|&u| u != part);
        }
    }
    if let Some(mesh) = mesh {
        ext.bound.borrow_mut().insert(part, mesh);
        let mut users = ext.users.borrow_mut();
        let list = users.entry(mesh).or_default();
        if !list.contains(&part) {
            list.push(part);
        }
    }
}

/// Points every part bound to a mesh changed since its last snapshot at a new snapshot. The recorder
/// calls it before each sample.
pub fn flush(engine: &Engine) {
    let ext = extension(engine);
    let dirty: Vec<InstanceId> = ext.dirty.borrow_mut().drain().collect();
    if dirty.is_empty() {
        return;
    }
    let Some(names) = names(engine) else { return };
    for mesh_id in dirty {
        let users: Vec<InstanceId> = ext.users.borrow().get(&mesh_id).cloned().unwrap_or_default();
        let users: Vec<InstanceId> = {
            let state = engine.state.borrow();
            users.into_iter().filter(|&u| state.dom.get(u).is_some()).collect()
        };
        if users.is_empty() {
            continue;
        }
        let path = {
            let meshes = ext.meshes.borrow();
            let Some(mesh) = meshes.get(&mesh_id) else { continue };
            snapshot(engine, mesh)
        };
        match path {
            Ok(path) => {
                let hooks = engine.hooks.borrow();
                let mut state = engine.state.borrow_mut();
                for part in users {
                    core::raw_set(&mut state, &engine.reflection, &hooks, part, names.mesh_id, content_of(&path));
                }
            }
            Err(problem) => engine.warn(format!("EditableMesh: its snapshot could not be written ({problem})")),
        }
    }
}

/// AssetService:CreateDataModelContentAsync of an EditableMesh's Content: a FileMesh copy of the mesh as it is now (the
/// snapshot file, which later edits do not reach), its `file://` URI. A mesh without faces is refused as a 0.747 client
/// refuses an empty one.
pub fn data_model_copy(engine: &Engine, id: InstanceId) -> Result<String, String> {
    let ext = extension(engine);
    let meshes = ext.meshes.borrow();
    let mesh = meshes.get(&id).ok_or_else(|| "This EditableMesh has been destroyed".to_string())?;
    if mesh.faces.live().next().is_none() {
        return Err("Error creating FileMeshData: Empty Result".into());
    }
    let path = snapshot(engine, mesh)?;
    Ok(format!("file://{}", path.display()))
}

/// A new MeshPart showing `mesh` (CreateMeshPartAsync of an EditableMesh's Content).
fn mesh_part(lua: &Lua, engine: &Rc<Engine>, mesh_id: InstanceId, options: &LuaValue) -> Ret {
    let names = names(engine).ok_or_else(|| err("MeshPart is not available"))?;
    let (path, size, skinned) = {
        let ext = extension(engine);
        let meshes = ext.meshes.borrow();
        let mesh = meshes.get(&mesh_id).ok_or_else(|| err("This EditableMesh has been destroyed"))?;
        (snapshot(engine, mesh).map_err(|problem| err(format!("CreateMeshPartAsync: {problem}")))?, mesh.size(), mesh.skinned())
    };
    let part = {
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        let reflection = &engine.reflection;
        let part = core::create(&mut state, reflection, names.mesh_part);
        core::raw_set(&mut state, reflection, &hooks, part, names.mesh_id, content_of(&path));
        for name in [names.mesh_size, names.initial_size, names.size] {
            core::raw_set(&mut state, reflection, &hooks, part, name, size_value(size));
        }
        core::raw_set(&mut state, reflection, &hooks, part, names.has_skinned_mesh, Value::Bool(skinned));
        if let LuaValue::Table(table) = options {
            for (key, name) in [("RenderFidelity", names.render_fidelity), ("CollisionFidelity", names.collision_fidelity)] {
                if let Ok(value) = table.get::<LuaValue>(key) {
                    if let Value::EnumItem(item) = convert::from_lua(&value) {
                        core::raw_set(&mut state, reflection, &hooks, part, name, Value::EnumItem(item));
                    }
                }
            }
        }
        part
    };
    bind(engine, part, Some(mesh_id));
    Ok(values([instance_value(lua, engine, part)?]))
}

/// MeshPart.MeshSize: what it was given, else the mesh's size a file stores as InitialSize (files
/// keep no MeshSize; Roblox reads it from the mesh), else Size
fn mesh_size_of(state: &State, names: &Names, id: InstanceId) -> Value {
    let vector = |value: Option<&Value>| match value {
        Some(Value::Vector3(v)) if v.x > 0.0 || v.y > 0.0 || v.z > 0.0 => Some(Value::Vector3(*v)),
        _ => None,
    };
    let from_file = || {
        state.dom.at(id).unmodeled.as_ref().and_then(|unmodeled| {
            unmodeled.iter().find(|(name, _)| *name == "InitialSize").and_then(|(_, value)| match value {
                Variant::Vector3(v) => Some(Value::Vector3(v)),
                _ => None,
            })
        })
    };
    vector(state.dom.raw_get(id, names.mesh_size))
        .or_else(|| vector(state.dom.raw_get(id, names.initial_size)))
        .or_else(from_file)
        .or_else(|| state.dom.raw_get(id, names.size).cloned())
        .unwrap_or(Value::Vector3(dom::Vector3::new(0.0, 0.0, 0.0)))
}

/// The id of what corner `corner` of `face` uses of `kind` (VERTEX, NORMAL, COLOR or UV).
fn corner_id(m: &Mesh, face: &Face, corner: usize, kind: u64) -> f64 {
    match kind {
        VERTEX => m.positions.id(kind, face.vertices[corner]),
        NORMAL => m.normals.id(kind, face.normals[corner]),
        COLOR => m.colors.id(kind, face.colors[corner]),
        _ => m.uvs.id(kind, face.uvs[corner]),
    }
}

/// For a corner over a vertex (face slot `f`, corner `corner`): its face's id for FACE, else what it uses of `kind`.
fn vertex_corner_id(m: &Mesh, f: u32, corner: usize, kind: u64) -> f64 {
    if kind == FACE {
        return m.faces.id(FACE, f);
    }
    corner_id(m, m.faces.get(f).expect("live"), corner, kind)
}

/// The live normal, colour or uv slot `id` names ("Invalid id" else).
fn attribute_slot(m: &Mesh, kind: u64, id: i64) -> Result<u32, Flow> {
    match kind {
        NORMAL => m.normals.at(kind, id),
        COLOR => m.colors.at(kind, id),
        _ => m.uvs.at(kind, id),
    }
}

/// The corner of face `f` over vertex `v` a single-item corner method names: the face checked (0 and gone "Invalid id",
/// another kind named), then its first corner over the vertex, the vertex id never checked (measured).
fn face_corner(m: &Mesh, v: i64, f: i64) -> Result<(u32, usize), Flow> {
    let f = m.faces.at(FACE, f)?;
    let vertex = m.positions.find(VERTEX, v);
    let corner = m.faces.get(f).expect("live").vertices.iter().position(|&x| Some(x) == vertex);
    Ok((f, corner.ok_or_else(|| err("vertexId not found on requested face"))?))
}

/// Pair `index` (from 1) of a per-vertex batch's vertexIds and faceIds, read as numbers (measured: both are read before
/// either is checked).
fn read_pair(lua: &Lua, method: &str, index: usize, vertex: &LuaValue, face: &LuaValue) -> Result<(i64, i64), Flow> {
    Ok((element_id(lua, vertex, method, "vertexId", index)?, element_id(lua, face, method, "faceId", index)?))
}

/// The corner a read pair names: the vertex checked (0 and gone "Invalid id", another kind named), then the face, then its
/// first corner over the vertex ("<method>: vertexId not found on requested face at index <index>"), as measured.
fn pair_corner(m: &Mesh, method: &str, index: usize, v: i64, f: i64) -> Result<(u32, usize), Flow> {
    check_kind(v, VERTEX)?;
    let v = m.positions.at(VERTEX, v)?;
    check_kind(f, FACE)?;
    let f = m.faces.at(FACE, f)?;
    let corner = m.faces.get(f).expect("live").vertices.iter().position(|&x| x == v);
    Ok((f, corner.ok_or_else(|| err(format!("{method}: vertexId not found on requested face at index {index}")))?))
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let n = |name: &str| reflection.name_id(name);
    let (Some(mesh_id), Some(mesh_size), Some(initial_size), Some(size), Some(fixed_size), Some(render_fidelity), Some(collision_fidelity), Some(has_skinned_mesh), Some(editable_mesh), Some(mesh_part_class)) = (
        n("MeshId"),
        n("MeshSize"),
        n("InitialSize"),
        n("Size"),
        n("FixedSize"),
        n("RenderFidelity"),
        n("CollisionFidelity"),
        n("HasSkinnedMesh"),
        reflection.class_id("EditableMesh"),
        reflection.class_id("MeshPart"),
    ) else {
        return;
    };
    *extension(engine).names.borrow_mut() = Some(Rc::new(Names {
        mesh_id,
        mesh_size,
        initial_size,
        size,
        fixed_size,
        render_fidelity,
        collision_fidelity,
        has_skinned_mesh,
        editable_mesh,
        mesh_part: mesh_part_class,
    }));

    // A freed mesh or part drops its data and bindings
    let freed = Rc::downgrade(engine);
    engine.hooks.borrow_mut().instance_freed.push(Box::new(move |_, id| {
        let Some(engine) = freed.upgrade() else { return };
        let ext = extension(&engine);
        if ext.meshes.borrow_mut().remove(&id).is_some() {
            ext.users.borrow_mut().remove(&id);
            ext.dirty.borrow_mut().remove(&id);
        }
        if let Some(mesh) = ext.bound.borrow_mut().remove(&id) {
            if let Some(users) = ext.users.borrow_mut().get_mut(&mesh) {
                users.retain(|&u| u != id);
            }
        }
    }));

    behaviors.add_getter(mesh_part_class, mesh_size, crate::services::getter(move |lua, engine, id| {
        let names = names(engine).ok_or_else(|| err("MeshPart is not available"))?;
        let value = mesh_size_of(&engine.state.borrow(), &names, id);
        Ok(convert::to_lua(lua, engine, &value)?)
    }));

    let mut add = |class: &str, name: &str, f: crate::services::MethodFn| {
        if let (Some(class), Some(name)) = (reflection.class_id(class), reflection.name_id(name)) {
            behaviors.add_method(class, name, f);
        }
    };

    // AssetService
    add("AssetService", "CreateEditableMesh", method(|lua, engine, _, _| new_mesh(lua, engine, Mesh::default(), false)));
    add("AssetService", "CreateEditableMeshAsync", method(|lua, engine, _, args| {
        let fixed_size = fixed_option(args.get(2));
        if let Some(source) = content_object(engine, args.get(1)) {
            let mesh = extension(engine).meshes.borrow().get(&source).cloned().unwrap_or_default();
            engine.state.borrow().sched.charge(mesh.positions.count() as f64 * crate::cost::MESH_COPY_VERTEX);
            return new_mesh(lua, engine, mesh, fixed_size);
        }
        let Some(uri) = content_uri(args.get(1)) else {
            return Err(err("CreateEditableMeshAsync: expected a mesh Content"));
        };
        // Studio's wait (crate::cost): the asset's first fetch in the session, else its decode
        let first = !extension(engine).assets.borrow().contains_key(&uri);
        let mesh = match load_asset(engine, &uri) {
            Ok(mesh) => (*mesh).clone(),
            Err(problem) => {
                engine.warn(format!("EditableMesh: {problem}; it starts empty"));
                Mesh::default()
            }
        };
        let wait = if first { crate::cost::MESH_FETCH } else { mesh.positions.count() as f64 * crate::cost::MESH_DECODE_VERTEX };
        let id = new_mesh_id(engine, mesh, fixed_size)?;
        engine.state.borrow_mut().sched.schedule_task(crate::scheduler::TimerKind::Delay, wait, lua.current_thread(), vec![Value::Instance(id)]);
        Err(Flow::Yield)
    }));
    add("AssetService", "CreateMeshPartAsync", method(|lua, engine, _, args| {
        if let Some(source) = content_object(engine, args.get(1)) {
            let vertices = extension(engine).meshes.borrow().get(&source).map_or(0, |mesh| mesh.positions.count());
            engine.state.borrow().sched.charge(vertices as f64 * crate::cost::MESH_PART_VERTEX);
            return mesh_part(lua, engine, source, args.get(2));
        }
        let Some(uri) = content_uri(args.get(1)) else {
            return Err(err("CreateMeshPartAsync: expected a mesh Content"));
        };
        let names = names(engine).ok_or_else(|| err("MeshPart is not available"))?;
        // Studio's error for content it cannot load (measured 2026-11-03), with the reason after it
        let (size, skinned) = match load_asset(engine, &uri) {
            Ok(mesh) => (mesh.size(), mesh.skinned()),
            Err(problem) => return Err(Flow::Error(ScriptError::plain(format!("Failed to load mesh asset. {problem}")))),
        };
        let part = {
            let hooks = engine.hooks.borrow();
            let mut state = engine.state.borrow_mut();
            let part = core::create(&mut state, &engine.reflection, names.mesh_part);
            core::raw_set(&mut state, &engine.reflection, &hooks, part, names.mesh_id, Value::String(Str::from(uri.as_str())));
            for name in [names.mesh_size, names.initial_size, names.size] {
                core::raw_set(&mut state, &engine.reflection, &hooks, part, name, size_value(size));
            }
            core::raw_set(&mut state, &engine.reflection, &hooks, part, names.has_skinned_mesh, Value::Bool(skinned));
            part
        };
        Ok(values([instance_value(lua, engine, part)?]))
    }));

    // MeshPart:ApplyMesh: the other part's mesh (and its binding) with MeshSize and HasSkinnedMesh
    // (measured: a skinned part's onto a fresh one makes it true, an unskinned one's onto a skinned
    // one false); Size stays
    add("MeshPart", "ApplyMesh", method(|_, engine, id, args| {
        let names = names(engine).ok_or_else(|| err("MeshPart is not available"))?;
        let source = args.instance(1).ok_or_else(|| err("ApplyMesh: expected a MeshPart"))?;
        let mesh = extension(engine).bound.borrow().get(&source).copied();
        {
            let hooks = engine.hooks.borrow();
            let mut state = engine.state.borrow_mut();
            let content = state.dom.raw_get(source, names.mesh_id).cloned().unwrap_or(Value::Nil);
            // as MeshSize reads it: a part from a file holds its mesh's size as the file's InitialSize
            let mesh_size = mesh_size_of(&state, &names, source);
            let skinned = matches!(state.dom.raw_get(source, names.has_skinned_mesh), Some(Value::Bool(true)));
            core::raw_set(&mut state, &engine.reflection, &hooks, id, names.mesh_id, content);
            core::raw_set(&mut state, &engine.reflection, &hooks, id, names.mesh_size, mesh_size.clone());
            core::raw_set(&mut state, &engine.reflection, &hooks, id, names.initial_size, mesh_size);
            core::raw_set(&mut state, &engine.reflection, &hooks, id, names.has_skinned_mesh, Value::Bool(skinned));
        }
        bind(engine, id, mesh);
        Ok(none())
    }));

    let mut add = |name: &str, f: crate::services::MethodFn| {
        if let (Some(class), Some(name)) = (reflection.class_id("EditableMesh"), reflection.name_id(name)) {
            behaviors.add_method(class, name, f);
        }
    };

    // Adding (each refused on a fixed-size mesh after its arguments' casts, measured)
    add("AddVertex", method(|_, engine, id, args| {
        let p = vec3_of(args.get(1)).ok_or_else(|| err("AddVertex: Vector3 expected"))?;
        refuse_fixed(engine, id)?;
        with_mesh(engine, id, true, |m| {
            let slot = m.positions.add(p);
            Ok(values([LuaValue::Number(m.positions.id(VERTEX, slot))]))
        })
    }));
    add("AddNormal", method(|_, engine, id, args| {
        let n = vec3_of(args.get(1));
        refuse_fixed(engine, id)?;
        with_mesh(engine, id, true, |m| {
            let slot = m.normals.add(n);
            Ok(values([LuaValue::Number(m.normals.id(NORMAL, slot))]))
        })
    }));
    add("AddUV", method(|_, engine, id, args| {
        let uv = vec2_of(args.get(1)).ok_or_else(|| err("AddUV: Vector2 expected"))?;
        refuse_fixed(engine, id)?;
        with_mesh(engine, id, true, |m| {
            let slot = m.uvs.add(uv);
            Ok(values([LuaValue::Number(m.uvs.id(UV, slot))]))
        })
    }));
    add("AddColor", method(|_, engine, id, args| {
        let c = color_of(args.get(1)).ok_or_else(|| err("AddColor: Color3 expected"))?;
        let a = alpha(args.number(2).unwrap_or(1.0));
        refuse_fixed(engine, id)?;
        with_mesh(engine, id, true, |m| {
            let slot = m.colors.add((c, a));
            Ok(values([LuaValue::Number(m.colors.id(COLOR, slot))]))
        })
    }));
    // (measured: a vertex that is gone is "Unable to add new triangle, vertex id <id> is invalid"; an id of another kind
    // is named as BatchAdd's faces name it, by analogy)
    add("AddTriangle", method(|lua, engine, id, args| {
        let ids = [int64_arg(lua, engine, args, 1)?, int64_arg(lua, engine, args, 2)?, int64_arg(lua, engine, args, 3)?];
        refuse_fixed(engine, id)?;
        with_mesh(engine, id, true, |m| {
            let mut v = [0u32; 3];
            for (slot, &vertex) in v.iter_mut().zip(&ids) {
                if parts(vertex).0 != VERTEX {
                    return Err(wrong_kind(VERTEX, vertex));
                }
                *slot = m.positions.find(VERTEX, vertex).ok_or_else(|| err(format!("Unable to add new triangle, vertex id {vertex} is invalid")))?;
            }
            let face = m.new_face(v);
            let slot = m.faces.add(face);
            Ok(values([LuaValue::Number(m.faces.id(FACE, slot))]))
        })
    }));
    // (measured: Name, CFrame and Virtual are each required, checked in that order; a Name of another type is "Variant cast
    // failed", a CFrame or Virtual of another type by analogy)
    add("AddBone", method(|_, engine, id, args| {
        let LuaValue::Table(params) = args.get(1) else { return Err(err("AddBone: table expected")) };
        refuse_fixed(engine, id)?;
        let field = |name: &str| params.get::<LuaValue>(name).unwrap_or(LuaValue::Nil);
        let missing = |name: &str| err(format!("Missing bone {name} value"));
        let name = match field("Name") {
            LuaValue::Nil => return Err(missing("Name")),
            LuaValue::String(text) => text.to_string_lossy(),
            _ => return Err(err("Variant cast failed")),
        };
        let cframe = match field("CFrame") {
            LuaValue::Nil => return Err(missing("CFrame")),
            value => cframe_of(&value).ok_or_else(|| err("Variant cast failed"))?,
        };
        let virtual_bone = match field("Virtual") {
            LuaValue::Nil => return Err(missing("Virtual")),
            LuaValue::Boolean(value) => value,
            _ => return Err(err("Variant cast failed")),
        };
        with_mesh(engine, id, true, |m| {
            let slot = m.bones.add(Bone { name, cframe, parent: None, virtual_bone });
            Ok(values([LuaValue::Number(m.bones.id(BONE, slot))]))
        })
    }));

    // Listing (in the order of the client's dense lists: see Slots)
    for (name, kind) in [("GetVertices", VERTEX), ("GetNormals", NORMAL), ("GetColors", COLOR), ("GetUVs", UV), ("GetFaces", FACE), ("GetBones", BONE)] {
        add(name, method(move |lua, engine, id, _| {
            with_mesh(engine, id, false, |m| {
                let ids: Vec<f64> = match kind {
                    VERTEX => m.positions.live().map(|(i, _)| m.positions.id(kind, i)).collect(),
                    NORMAL => m.normals.live().map(|(i, _)| m.normals.id(kind, i)).collect(),
                    COLOR => m.colors.live().map(|(i, _)| m.colors.id(kind, i)).collect(),
                    UV => m.uvs.live().map(|(i, _)| m.uvs.id(kind, i)).collect(),
                    FACE => m.faces.live().map(|(i, _)| m.faces.id(kind, i)).collect(),
                    _ => m.bones.live().map(|(i, _)| m.bones.id(kind, i)).collect(),
                };
                let each = if kind == VERTEX { crate::cost::LISTED_VERTEX } else { crate::cost::LISTED };
                engine.state.borrow().sched.charge(ids.len() as f64 * each);
                Ok(values([ids_value(lua, ids.into_iter())?]))
            })
        }));
    }

    // Values
    add("GetPosition", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        let p = with_mesh(engine, id, false, |m| Ok(m.positions.get(m.positions.at(VERTEX, v)?).copied().unwrap_or(Vec3::ZERO)))?;
        Ok(values([vector_value(lua, engine, p)?]))
    }));
    add("SetPosition", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        let p = vec3_of(args.get(2)).ok_or_else(|| err("SetPosition: Vector3 expected"))?;
        with_mesh(engine, id, true, |m| {
            let slot = m.positions.at(VERTEX, v)?;
            *m.positions.get_mut(slot).expect("live") = p;
            Ok(none())
        })
    }));
    add("GetNormal", method(|lua, engine, id, args| {
        let n = id_arg(lua, engine, args, 1, NORMAL)?;
        let value = with_mesh(engine, id, false, |m| Ok(m.normal_value(m.normals.at(NORMAL, n)?).unwrap_or(Vec3::ZERO)))?;
        Ok(values([vector_value(lua, engine, value)?]))
    }));
    add("SetNormal", method(|lua, engine, id, args| {
        let n = id_arg(lua, engine, args, 1, NORMAL)?;
        let value = vec3_of(args.get(2));
        with_mesh(engine, id, true, |m| {
            let slot = m.normals.at(NORMAL, n)?;
            *m.normals.get_mut(slot).expect("live") = value;
            Ok(none())
        })
    }));
    add("GetUV", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, UV)?;
        let uv = with_mesh(engine, id, false, |m| Ok(m.uvs.get(m.uvs.at(UV, i)?).copied().unwrap_or([0.0; 2])))?;
        Ok(values([convert::to_lua(lua, engine, &Value::Vector2(dom::Vector2::new(uv[0], uv[1])))?]))
    }));
    add("SetUV", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, UV)?;
        let uv = vec2_of(args.get(2)).ok_or_else(|| err("SetUV: Vector2 expected"))?;
        with_mesh(engine, id, true, |m| {
            let slot = m.uvs.at(UV, i)?;
            *m.uvs.get_mut(slot).expect("live") = uv;
            Ok(none())
        })
    }));
    add("GetColor", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, COLOR)?;
        let (c, _) = with_mesh(engine, id, false, |m| Ok(m.colors.get(m.colors.at(COLOR, i)?).copied().unwrap_or(([1.0; 3], 1.0))))?;
        Ok(values([convert::to_lua(lua, engine, &Value::Color3(dom::Color3::new(c[0], c[1], c[2])))?]))
    }));
    add("SetColor", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, COLOR)?;
        let c = color_of(args.get(2)).ok_or_else(|| err("SetColor: Color3 expected"))?;
        with_mesh(engine, id, true, |m| {
            let slot = m.colors.at(COLOR, i)?;
            m.colors.get_mut(slot).expect("live").0 = c;
            Ok(none())
        })
    }));
    add("GetColorAlpha", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, COLOR)?;
        let (_, a) = with_mesh(engine, id, false, |m| Ok(m.colors.get(m.colors.at(COLOR, i)?).copied().unwrap_or(([1.0; 3], 1.0))))?;
        Ok(values([LuaValue::Number(a as f64)]))
    }));
    add("SetColorAlpha", method(|lua, engine, id, args| {
        let i = id_arg(lua, engine, args, 1, COLOR)?;
        let a = alpha(args.number(2).unwrap_or(1.0));
        with_mesh(engine, id, true, |m| {
            let slot = m.colors.at(COLOR, i)?;
            m.colors.get_mut(slot).expect("live").1 = a;
            Ok(none())
        })
    }));

    // Faces
    for (name, kind) in [("GetFaceVertices", VERTEX), ("GetFaceNormals", NORMAL), ("GetFaceColors", COLOR), ("GetFaceUVs", UV)] {
        add(name, method(move |lua, engine, id, args| {
            let f = id_arg(lua, engine, args, 1, FACE)?;
            with_mesh(engine, id, false, |m| {
                let face = *m.faces.get(m.faces.at(FACE, f)?).expect("live");
                Ok(values([ids_value(lua, (0..3).map(|corner| corner_id(m, &face, corner, kind)))?]))
            })
        }));
    }
    // (a corner given another vertex, normal, uv or colour comes to the front of its vertex's corners: Mesh::corners_of)
    for (name, kind) in [("SetFaceVertices", VERTEX), ("SetFaceNormals", NORMAL), ("SetFaceColors", COLOR), ("SetFaceUVs", UV)] {
        add(name, method(move |lua, engine, id, args| {
            let f = id_arg(lua, engine, args, 1, FACE)?;
            let ids = ids_of(args.get(2));
            with_mesh(engine, id, true, |m| {
                let f = m.faces.at(FACE, f)?;
                let slots = match kind {
                    VERTEX => slots_of(&m.positions, kind, &ids),
                    NORMAL => slots_of(&m.normals, kind, &ids),
                    COLOR => slots_of(&m.colors, kind, &ids),
                    _ => slots_of(&m.uvs, kind, &ids),
                };
                if slots.len() != 3 || ids.len() != 3 {
                    return Err(err(format!("{name}: three ids of the right kind expected")));
                }
                if kind == VERTEX {
                    m.set_face_vertices(f, [slots[0], slots[1], slots[2]]);
                } else {
                    for (corner, &slot) in slots.iter().enumerate() {
                        m.set_corner(f, corner, (kind - 1) as u32, slot);
                    }
                }
                Ok(none())
            })
        }));
    }
    // (measured: a face that is gone is "Invalid faceId", 0 is named as no id; it removes the face only, what it used
    // staying until RemoveUnused)
    add("RemoveFace", method(|lua, engine, id, args| {
        let f = int64_arg(lua, engine, args, 1)?;
        refuse_fixed(engine, id)?;
        if parts(f).0 != FACE {
            return Err(wrong_kind(FACE, f));
        }
        with_mesh(engine, id, true, |m| {
            let f = m.faces.find(FACE, f).ok_or_else(|| err("Invalid faceId"))?;
            m.remove_faces(&[f]);
            Ok(none())
        })
    }));
    add("RemoveUnused", method(|lua, engine, id, _| {
        refuse_fixed(engine, id)?;
        let (removed, vertices) = with_mesh(engine, id, true, |m| {
            let vertices = m.positions.count();
            Ok((m.remove_unused(), vertices))
        })?;
        engine.state.borrow().sched.charge(vertices as f64 * crate::cost::REMOVE_UNUSED_VERTEX);
        Ok(values([ids_value(lua, removed.into_iter())?]))
    }));
    add("GetSize", method(|lua, engine, id, _| {
        let size = with_mesh(engine, id, false, |m| Ok(m.bounds().map_or(Vec3::ZERO, |(min, max)| max - min)))?;
        Ok(values([vector_value(lua, engine, size)?]))
    }));
    add("GetCenter", method(|lua, engine, id, _| {
        let center = with_mesh(engine, id, false, |m| Ok(m.bounds().map_or(Vec3::ZERO, |(min, max)| (min + max) * 0.5)))?;
        Ok(values([vector_value(lua, engine, center)?]))
    }));

    // Bones
    add("GetBoneName", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let name = with_mesh(engine, id, false, |m| Ok(m.bones.get(m.bones.at(BONE, b)?).map(|b| b.name.clone()).unwrap_or_default()))?;
        Ok(values([LuaValue::String(lua.create_string(&name)?)]))
    }));
    add("SetBoneName", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let name = args.string(2).map(|s| s.to_string_lossy()).unwrap_or_default();
        with_mesh(engine, id, true, |m| {
            let slot = m.bones.at(BONE, b)?;
            m.bones.get_mut(slot).expect("live").name = name;
            Ok(none())
        })
    }));
    add("GetBoneParent", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let parent = with_mesh(engine, id, false, |m| {
            let parent = m.bones.get(m.bones.at(BONE, b)?).and_then(|b| b.parent);
            Ok(parent.map_or(0.0, |p| m.bones.id(BONE, p)))
        })?;
        Ok(values([LuaValue::Number(parent)]))
    }));
    add("SetBoneParent", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let parent = args.number(2).map(id_of_number);
        with_mesh(engine, id, true, |m| {
            let slot = m.bones.at(BONE, b)?;
            let parent = parent.and_then(|p| m.bones.find(BONE, p));
            m.bones.get_mut(slot).expect("live").parent = parent;
            Ok(none())
        })
    }));
    add("GetBoneCFrame", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let c = with_mesh(engine, id, false, |m| Ok(m.bones.get(m.bones.at(BONE, b)?).map(|b| b.cframe).expect("live")))?;
        let cframe = dom::CFrame::new(dom::Vector3::new(c[0], c[1], c[2]), dom::Matrix3::new(dom::Vector3::new(c[3], c[4], c[5]), dom::Vector3::new(c[6], c[7], c[8]), dom::Vector3::new(c[9], c[10], c[11])));
        Ok(values([convert::to_lua(lua, engine, &Value::Dom(Rc::new(Variant::CFrame(cframe))))?]))
    }));
    add("SetBoneCFrame", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let c = cframe_of(args.get(2)).ok_or_else(|| err("SetBoneCFrame: CFrame expected"))?;
        with_mesh(engine, id, true, |m| {
            let slot = m.bones.at(BONE, b)?;
            m.bones.get_mut(slot).expect("live").cframe = c;
            Ok(none())
        })
    }));
    add("GetBoneIsVirtual", method(|lua, engine, id, args| {
        let b = id_arg(lua, engine, args, 1, BONE)?;
        let v = with_mesh(engine, id, false, |m| Ok(m.bones.get(m.bones.at(BONE, b)?).map(|b| b.virtual_bone).expect("live")))?;
        Ok(values([LuaValue::Boolean(v)]))
    }));
    add("GetVertexBones", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        with_mesh(engine, id, false, |m| {
            let v = m.positions.at(VERTEX, v)?;
            let bones: Vec<f64> = m.skin.get(&v).map_or(Vec::new(), |(b, _)| b.iter().map(|&i| m.bones.id(BONE, i)).collect());
            Ok(values([ids_value(lua, bones.into_iter())?]))
        })
    }));
    add("GetVertexBoneWeights", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        with_mesh(engine, id, false, |m| {
            let v = m.positions.at(VERTEX, v)?;
            let weights = m.skin.get(&v).map_or(Vec::new(), |(_, w)| w.iter().map(|&w| w as f64).collect());
            Ok(values([ids_value(lua, weights.into_iter())?]))
        })
    }));
    add("SetVertexBones", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        let ids = ids_of(args.get(2));
        with_mesh(engine, id, true, |m| {
            let v = m.positions.at(VERTEX, v)?;
            let bones = slots_of(&m.bones, BONE, &ids);
            let entry = m.skin.entry(v).or_default();
            entry.1.resize(bones.len(), 0.0);
            entry.0 = bones;
            Ok(none())
        })
    }));
    add("SetVertexBoneWeights", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        // (measured: weights keep 1/255 steps)
        let weights: Vec<f32> = numbers_of(args.get(2)).into_iter().map(|w| ((w.clamp(0.0, 1.0) * 255.0).round() / 255.0) as f32).collect();
        with_mesh(engine, id, true, |m| {
            let v = m.positions.at(VERTEX, v)?;
            let entry = m.skin.entry(v).or_default();
            entry.1 = weights;
            Ok(none())
        })
    }));

    // BatchSetValues / BatchGetValues: positions, normals, uvs, colours by their ids
    add("BatchSetValues", method(|_, engine, id, args| {
        let LuaValue::Table(ids) = args.get(1) else { return Err(err("BatchSetValues: table of ids expected")) };
        let LuaValue::Table(items) = args.get(2) else { return Err(err("BatchSetValues: table of values expected")) };
        let ids: Vec<i64> = ids.sequence_values::<f64>().filter_map(Result::ok).map(id_of_number).collect();
        let items: Vec<LuaValue> = items.sequence_values::<LuaValue>().filter_map(Result::ok).collect();
        with_mesh(engine, id, true, |m| {
            for (&id, item) in ids.iter().zip(&items) {
                match parts(id).0 {
                    VERTEX => {
                        if let (Some(slot), Some(p)) = (m.positions.find(VERTEX, id), vec3_of(item)) {
                            *m.positions.get_mut(slot).expect("live") = p;
                        }
                    }
                    NORMAL => {
                        if let Some(slot) = m.normals.find(NORMAL, id) {
                            *m.normals.get_mut(slot).expect("live") = vec3_of(item);
                        }
                    }
                    UV => {
                        if let (Some(slot), Some(uv)) = (m.uvs.find(UV, id), vec2_of(item)) {
                            *m.uvs.get_mut(slot).expect("live") = uv;
                        }
                    }
                    // (measured: a number sets a colour's alpha, a Color3 its colour)
                    COLOR => {
                        if let Some(slot) = m.colors.find(COLOR, id).and_then(|slot| m.colors.get_mut(slot)) {
                            match (color_of(item), item) {
                                (Some(c), _) => slot.0 = c,
                                (None, LuaValue::Number(n)) => slot.1 = alpha(*n),
                                (None, LuaValue::Integer(n)) => slot.1 = alpha(*n as f64),
                                _ => {}
                            }
                        }
                    }
                    _ => return Err(err("BatchSetValues: invalid id")),
                }
            }
            Ok(none())
        })
    }));
    add("BatchGetValues", method(|lua, engine, id, args| {
        let ids = ids_of(args.get(1));
        let out = lua.create_table()?;
        with_mesh(engine, id, false, |m| {
            for (index, &id) in ids.iter().enumerate() {
                let value = match parts(id).0 {
                    VERTEX => m.positions.find(VERTEX, id).and_then(|i| m.positions.get(i)).map(|p| Value::Vector3(dom::Vector3::new(p.x, p.y, p.z))),
                    NORMAL => m.normals.find(NORMAL, id).and_then(|i| m.normal_value(i)).map(|p| Value::Vector3(dom::Vector3::new(p.x, p.y, p.z))),
                    UV => m.uvs.find(UV, id).and_then(|i| m.uvs.get(i)).map(|uv| Value::Vector2(dom::Vector2::new(uv[0], uv[1]))),
                    COLOR => m.colors.find(COLOR, id).and_then(|i| m.colors.get(i)).map(|(c, _)| Value::Color3(dom::Color3::new(c[0], c[1], c[2]))),
                    _ => None,
                };
                let value = value.ok_or_else(|| err("BatchGetValues: invalid id"))?;
                out.raw_set(index + 1, convert::to_lua(lua, engine, &value)?)?;
            }
            Ok(())
        })?;
        Ok(values([LuaValue::Table(out)]))
    }));

    // BatchAdd(attribute, values[, alphas]): vertices, normals, uvs, colours (with their alphas) or faces at once, their
    // new ids in the values' order (measured in a 0.747 client: tools/parity/probes/editable_mesh_batch.luau). Vertices,
    // normals, uvs and colours are all made first, then given their values in order, so a value of the wrong type raises
    // with the ones before it set and the rest left at their defaults; a nil normal is an automatic one. Faces are checked
    // whole first, made in order (each corner as AddTriangle makes it) and numbered backwards: the first face gets the
    // batch's last id (with slots free, the batch's new slots backwards, then the free ones, last freed first; measured in
    // editable_mesh_vertex_batch.luau). A face of four vertices (a quad, which this engine's faces cannot hold) is made as
    // two triangles, the second on a slot of its own, and reported (Output `missing`).
    add("BatchAdd", method(|lua, engine, id, args| {
        refuse_fixed(engine, id)?;
        if args.len() < 3 {
            return Err(err("BatchAdd: expects at least 2 arguments (attr, data)"));
        }
        let attribute = mesh_attribute(engine, args.get(1))?;
        let (label, expects, wanted) = match attribute {
            0 => ("Vertex", "1 argument (positions)", 1),
            1 => ("Normal", "1 argument (normals)", 1),
            2 => ("Color", "2 arguments (colors, alphas)", 2),
            3 => ("UV", "1 argument (uvs)", 1),
            _ => ("Face", "1 argument (vertexIds)", 1),
        };
        let prefix = format!("BatchAdd(Enum.MeshAttribute.{label}, ...)");
        if args.len() - 2 != wanted {
            return Err(err(format!("{prefix}: expects {expects}")));
        }
        let array = |at: usize, name: &str| match args.get(at) {
            LuaValue::Table(table) => Ok(sequence(table)),
            _ => Err(err(format!("{prefix}: {name} must be an array"))),
        };
        // (the client's own argument numbers: -1 for a vector, 4 for a uv, 5 for a colour, wherever it is in the array)
        let wrong = |expected: &str, number: i32, value: &LuaValue| {
            Flow::Error(ScriptError::at_caller(format!("invalid argument #{number} ({expected} expected, got {})", crate::lua::typeof_value(engine, value))))
        };
        let fill = |slots: &[u32], items: &[LuaValue], set: &mut dyn FnMut(u32, &LuaValue) -> Result<(), Flow>| -> Result<(), Flow> {
            for (&slot, item) in slots.iter().zip(items) {
                set(slot, item)?;
            }
            Ok(())
        };
        let mut quads = 0;
        let ids: Vec<f64> = match attribute {
            0 => {
                let items = array(2, "positions")?;
                with_mesh(engine, id, true, |m| {
                    let slots: Vec<u32> = items.iter().map(|_| m.positions.add(Vec3::ZERO)).collect();
                    fill(&slots, &items, &mut |slot, item| {
                        *m.positions.get_mut(slot).expect("made above") = vec3_of(item).ok_or_else(|| wrong("Vector3", -1, item))?;
                        Ok(())
                    })?;
                    Ok(slots.iter().map(|&slot| m.positions.id(VERTEX, slot)).collect())
                })?
            }
            1 => {
                let items = array(2, "normals")?;
                with_mesh(engine, id, true, |m| {
                    let slots: Vec<u32> = items.iter().map(|_| m.normals.add(None)).collect();
                    fill(&slots, &items, &mut |slot, item| {
                        if !item.is_nil() {
                            *m.normals.get_mut(slot).expect("made above") = Some(vec3_of(item).ok_or_else(|| wrong("Vector3", -1, item))?);
                        }
                        Ok(())
                    })?;
                    Ok(slots.iter().map(|&slot| m.normals.id(NORMAL, slot)).collect())
                })?
            }
            2 => {
                let colors = array(2, "colors")?;
                let alphas = array(3, "alphas")?;
                if colors.len() != alphas.len() {
                    return Err(err(format!("{prefix}: colors and alphas arrays must be the same length")));
                }
                with_mesh(engine, id, true, |m| {
                    let slots: Vec<u32> = colors.iter().map(|_| m.colors.add(([1.0; 3], 1.0))).collect();
                    for ((&slot, color), opacity) in slots.iter().zip(&colors).zip(&alphas) {
                        let rgb = color_of(color).ok_or_else(|| wrong("Color3", 5, color))?;
                        // (as a number reads it: anything else is 0, unmeasured beyond a string being taken)
                        let opacity = match opacity {
                            LuaValue::Number(n) => *n,
                            LuaValue::Integer(n) => *n as f64,
                            LuaValue::String(text) => text.to_str().ok().and_then(|text| text.trim().parse::<f64>().ok()).unwrap_or(0.0),
                            _ => 0.0,
                        };
                        *m.colors.get_mut(slot).expect("made above") = (rgb, alpha(opacity));
                    }
                    Ok(slots.iter().map(|&slot| m.colors.id(COLOR, slot)).collect())
                })?
            }
            3 => {
                let items = array(2, "uvs")?;
                with_mesh(engine, id, true, |m| {
                    let slots: Vec<u32> = items.iter().map(|_| m.uvs.add([0.0; 2])).collect();
                    fill(&slots, &items, &mut |slot, item| {
                        *m.uvs.get_mut(slot).expect("made above") = vec2_of(item).ok_or_else(|| wrong("Vector2", 4, item))?;
                        Ok(())
                    })?;
                    Ok(slots.iter().map(|&slot| m.uvs.id(UV, slot)).collect())
                })?
            }
            _ => {
                let LuaValue::Table(table) = args.get(2) else {
                    return Err(err(format!("{prefix}: vertexIds must be a 2D array")));
                };
                let items = sequence(table);
                let made = with_mesh(engine, id, true, |m| {
                    let mut faces: Vec<Vec<u32>> = Vec::with_capacity(items.len());
                    for (index, item) in items.iter().enumerate() {
                        let LuaValue::Table(ids) = item else {
                            return Err(err(format!("{prefix}: element {} must be an array of 3 or 4 vertex ids", index + 1)));
                        };
                        let ids = sequence(ids);
                        if !(3..=4).contains(&ids.len()) {
                            return Err(err(format!("{prefix}: element {} must have 3 or 4 vertex ids, got {}", index + 1, ids.len())));
                        }
                        faces.push(ids.iter().map(|value| face_vertex(m, value)).collect::<Result<_, _>>()?);
                    }
                    // (the slots as the client hands a batch of faces them: made free after those already free, then
                    // taken last first)
                    m.faces.reserve(faces.len());
                    let slots: Vec<u32> = faces.iter().map(|_| m.faces.take()).collect();
                    let mut quads = 0;
                    for (vertices, &slot) in faces.iter().zip(&slots) {
                        let face = m.new_face([vertices[0], vertices[1], vertices[2]]);
                        m.faces.put(slot, face);
                        if vertices.len() == 4 {
                            let second = m.new_face([vertices[0], vertices[2], vertices[3]]);
                            m.faces.add(second);
                            quads += 1;
                        }
                    }
                    Ok((slots.iter().map(|&slot| m.faces.id(FACE, slot)).collect(), quads))
                })?;
                quads = made.1;
                made.0
            }
        };
        if quads > 0 {
            engine.missing(Some(lua), "EditableMesh", "BatchAdd of a four-vertex face");
        }
        Ok(values([ids_value(lua, ids.into_iter())?]))
    }));

    // Per-face attribute batches (Enum.MeshAttribute: Vertex 0, Normal 1, Color 2, UV 3, Face 4; an id's kind is the
    // value plus 1)
    fn attribute(engine: &Engine, value: &LuaValue) -> Option<u32> {
        match convert::from_lua(value) {
            Value::EnumItem(item) => Some(engine.reflection.enum_item_info(item).value as u32),
            _ => None,
        }
    }
    add("BatchGetFaceAttributes", method(|lua, engine, id, args| {
        let which = attribute(engine, args.get(1)).ok_or_else(|| err("BatchGetFaceAttributes: MeshAttribute expected"))?;
        let faces = ids_of(args.get(2));
        let out = lua.create_table_with_capacity(faces.len(), 0)?;
        with_mesh(engine, id, false, |m| {
            for (index, &face) in faces.iter().enumerate() {
                let face = *m.faces.find(FACE, face).and_then(|f| m.faces.get(f)).ok_or_else(|| err("Invalid face id"))?;
                if which > 3 {
                    return Err(err("BatchGetFaceAttributes: not a face attribute"));
                }
                out.raw_set(index + 1, ids_value(lua, (0..3).map(|corner| corner_id(m, &face, corner, which as u64 + 1)))?)?;
            }
            Ok(())
        })?;
        Ok(values([LuaValue::Table(out)]))
    }));
    add("BatchSetFaceAttributes", method(|_, engine, id, args| {
        let faces = ids_of(args.get(1));
        let LuaValue::Table(arrays) = args.get(2) else { return Err(err("BatchSetFaceAttributes: table expected")) };
        let arrays: Vec<Vec<i64>> = arrays.sequence_values::<LuaValue>().filter_map(Result::ok).map(|v| ids_of(&v)).collect();
        with_mesh(engine, id, true, |m| {
            for (&face, ids) in faces.iter().zip(&arrays) {
                let f = m.faces.find(FACE, face).ok_or_else(|| err("Invalid face id"))?;
                let kind = ids.first().map_or(0, |&id| parts(id).0);
                let slots = match kind {
                    VERTEX => slots_of(&m.positions, kind, ids),
                    NORMAL => slots_of(&m.normals, kind, ids),
                    COLOR => slots_of(&m.colors, kind, ids),
                    UV => slots_of(&m.uvs, kind, ids),
                    _ => return Err(err("BatchSetFaceAttributes: not a face attribute")),
                };
                if slots.len() != 3 || ids.len() != 3 {
                    return Err(err("BatchSetFaceAttributes: three ids of one kind expected"));
                }
                if kind == VERTEX {
                    m.set_face_vertices(f, [slots[0], slots[1], slots[2]]);
                } else {
                    for (corner, &slot) in slots.iter().enumerate() {
                        m.set_corner(f, corner, (kind - 1) as u32, slot);
                    }
                }
            }
            Ok(none())
        })
    }));

    // A vertex's faces and the normals / colours / uvs its corners use: one per corner, front first (Mesh::corners_of;
    // measured in a 0.747 client, tools/parity/probes/editable_mesh_vertex_batch.luau)
    for (name, kind) in [("GetVertexFaces", FACE), ("GetVertexNormals", NORMAL), ("GetVertexColors", COLOR), ("GetVertexUVs", UV)] {
        add(name, method(move |lua, engine, id, args| {
            let v = id_arg(lua, engine, args, 1, VERTEX)?;
            with_mesh(engine, id, false, |m| {
                let v = m.positions.at(VERTEX, v)?;
                let ids: Vec<f64> = m.corners_of(v).into_iter().map(|(f, corner)| vertex_corner_id(m, f, corner, kind)).collect();
                Ok(values([ids_value(lua, ids.into_iter())?]))
            })
        }));
    }
    // BatchGetVertexAttributes(attribute, vertexIds): per vertex, what GetVertexFaces / GetVertexNormals / GetVertexColors /
    // GetVertexUVs list (measured: Vertex is not supported; each id is checked in turn, a number or a numeric string, 0 and
    // an id that is gone "Invalid id", one of another kind named; the client's argument errors)
    add("BatchGetVertexAttributes", method(|lua, engine, id, args| {
        const METHOD: &str = "BatchGetVertexAttributes";
        if args.len() != 3 {
            return Err(err(format!("{METHOD}: expects 2 arguments (attr, vertexIds)")));
        }
        let which = mesh_attribute(engine, args.get(1))?;
        if which == 0 {
            return Err(err(format!("{METHOD}: Enum.MeshAttribute.Vertex is not supported")));
        }
        let LuaValue::Table(table) = args.get(2) else { return Err(err(format!("{METHOD}: ids must be an array"))) };
        let items = sequence(table);
        let out = with_mesh(engine, id, false, |m| {
            let mut vertices = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let v = element_id(lua, item, METHOD, "id", index + 1)?;
                check_kind(v, VERTEX)?;
                vertices.push(m.positions.at(VERTEX, v)?);
            }
            let corners = m.corners_of_all(&vertices);
            let out = lua.create_table_with_capacity(vertices.len(), 0)?;
            for (index, v) in vertices.iter().enumerate() {
                let ids = corners[v].iter().map(|&(f, corner)| vertex_corner_id(m, f, corner, which as u64 + 1));
                out.raw_set(index + 1, ids_value(lua, ids)?)?;
            }
            Ok(out)
        })?;
        Ok(values([LuaValue::Table(out)]))
    }));
    // GetAdjacentVertices: per corner over the vertex, front first, the corner before it, then the one after it unless some
    // corner over the vertex has that one before it (measured on five vertices of a three-face mesh in a 0.747 client)
    add("GetAdjacentVertices", method(|lua, engine, id, args| {
        let v = id_arg(lua, engine, args, 1, VERTEX)?;
        with_mesh(engine, id, false, |m| {
            let v = m.positions.at(VERTEX, v)?;
            let corners = m.corners_of(v);
            let around = |f: u32, corner: usize, step: usize| m.faces.get(f).expect("live").vertices[(corner + step) % 3];
            let before: Vec<u32> = corners.iter().map(|&(f, corner)| around(f, corner, 2)).collect();
            let mut out: Vec<f64> = Vec::new();
            for (&(f, corner), &previous) in corners.iter().zip(&before) {
                out.push(m.positions.id(VERTEX, previous));
                let next = around(f, corner, 1);
                if !before.contains(&next) {
                    out.push(m.positions.id(VERTEX, next));
                }
            }
            Ok(values([ids_value(lua, out.into_iter())?]))
        })
    }));
    add("GetAdjacentFaces", method(|lua, engine, id, args| {
        let f = id_arg(lua, engine, args, 1, FACE)?;
        with_mesh(engine, id, false, |m| {
            let f = m.faces.at(FACE, f)?;
            let face = *m.faces.get(f).expect("live");
            let edges = [(face.vertices[0], face.vertices[1]), (face.vertices[1], face.vertices[2]), (face.vertices[2], face.vertices[0])];
            let shares = |other: &Face| {
                let mine = [(other.vertices[0], other.vertices[1]), (other.vertices[1], other.vertices[2]), (other.vertices[2], other.vertices[0])];
                mine.iter().any(|&(a, b)| edges.iter().any(|&(c, d)| (a == c && b == d) || (a == d && b == c)))
            };
            let out: Vec<f64> = m.faces.by_slot().filter(|(i, other)| *i != f && shares(other)).map(|(i, _)| m.faces.id(FACE, i)).collect();
            Ok(values([ids_value(lua, out.into_iter())?]))
        })
    }));

    // The attribute at one corner: GetVertexFaceNormal / UV / Color(vertexId, faceId) and the setters (measured in a 0.747
    // client: the face is checked, then the corner found, the first in the face over the vertex, the vertex id itself
    // never checked, so one that is no vertex of the face is "vertexId not found on requested face"; a setter's id is
    // checked after; a corner given another one comes to the front of its vertex's corners)
    for (name, kind) in [("GetVertexFaceNormal", NORMAL), ("GetVertexFaceColor", COLOR), ("GetVertexFaceUV", UV)] {
        add(name, method(move |lua, engine, id, args| {
            let (v, f) = (int64_arg(lua, engine, args, 1)?, int64_arg(lua, engine, args, 2)?);
            check_kind(f, FACE)?;
            let found = with_mesh(engine, id, false, |m| {
                let (f, corner) = face_corner(m, v, f)?;
                Ok(vertex_corner_id(m, f, corner, kind))
            })?;
            Ok(values([LuaValue::Number(found)]))
        }));
    }
    for (name, kind) in [("SetVertexFaceNormal", NORMAL), ("SetVertexFaceColor", COLOR), ("SetVertexFaceUV", UV)] {
        add(name, method(move |lua, engine, id, args| {
            let (v, f, a) = (int64_arg(lua, engine, args, 1)?, int64_arg(lua, engine, args, 2)?, int64_arg(lua, engine, args, 3)?);
            check_kind(f, FACE)?;
            with_mesh(engine, id, true, |m| {
                let (f, corner) = face_corner(m, v, f)?;
                check_kind(a, kind)?;
                let slot = attribute_slot(m, kind, a)?;
                m.set_corner(f, corner, (kind - 1) as u32, slot);
                Ok(none())
            })
        }));
    }
    // BatchGetVertexFaceAttributes(attribute, vertexIds, faceIds): the normal, colour or uv id at each vertex's corner of
    // its face (measured: Vertex and Face are not supported; each pair is read, then checked, in turn)
    add("BatchGetVertexFaceAttributes", method(|lua, engine, id, args| {
        const METHOD: &str = "BatchGetVertexFaceAttributes";
        if args.len() != 4 {
            return Err(err(format!("{METHOD}: expects 3 arguments (attr, vertexIds, faceIds)")));
        }
        let which = mesh_attribute(engine, args.get(1))?;
        if which == 0 || which == 4 {
            return Err(err(format!("{METHOD}: Enum.MeshAttribute.{} is not supported", if which == 0 { "Vertex" } else { "Face" })));
        }
        let LuaValue::Table(vertices) = args.get(2) else { return Err(err(format!("{METHOD}: vertexIds must be an array"))) };
        let LuaValue::Table(faces) = args.get(3) else { return Err(err(format!("{METHOD}: faceIds must be an array"))) };
        let (vertices, faces) = (sequence(vertices), sequence(faces));
        if vertices.len() != faces.len() {
            return Err(err(format!("{METHOD}: vertexIds and faceIds arrays must be the same length")));
        }
        let found = with_mesh(engine, id, false, |m| {
            let mut found = Vec::with_capacity(vertices.len());
            for (index, (vertex, face)) in vertices.iter().zip(&faces).enumerate() {
                let (v, f) = read_pair(lua, METHOD, index + 1, vertex, face)?;
                let (f, corner) = pair_corner(m, METHOD, index + 1, v, f)?;
                found.push(vertex_corner_id(m, f, corner, which as u64 + 1));
            }
            Ok(found)
        })?;
        Ok(values([ids_value(lua, found.into_iter())?]))
    }));
    // BatchSetVertexFaceAttributes(vertexIds, faceIds, attributeIds): sets each vertex's corner of its face to a normal, a
    // colour or a uv, all of the kind of the first id (measured: pairs set in turn, so an error leaves the ones before it
    // set; a corner given another one comes to the front of its vertex's corners; allowed on a fixed-size mesh)
    add("BatchSetVertexFaceAttributes", method(|lua, engine, id, args| {
        const METHOD: &str = "BatchSetVertexFaceAttributes";
        if args.len() != 4 {
            return Err(err(format!("{METHOD}: expects 3 arguments (vertexIds, faceIds, attributeIds)")));
        }
        let LuaValue::Table(vertices) = args.get(1) else { return Err(err(format!("{METHOD}: vertexIds must be an array"))) };
        let vertices = sequence(vertices);
        let faces = match args.get(2) {
            LuaValue::Table(faces) if faces.raw_len() == vertices.len() => sequence(faces),
            _ => return Err(err(format!("{METHOD}: faceIds must be an array the same size as vertexIds"))),
        };
        let attributes = match args.get(3) {
            LuaValue::Table(ids) if ids.raw_len() == vertices.len() => sequence(ids),
            _ => return Err(err(format!("{METHOD}: attributeIds must be an array the same size as vertexIds"))),
        };
        let Some(first) = attributes.first() else { return Ok(none()) };
        let kind = parts(element_id(lua, first, METHOD, "attributeId", 1)?).0;
        if !matches!(kind, NORMAL | COLOR | UV) {
            return Err(err(format!("{METHOD}: unsupported attribute ID type {kind}")));
        }
        with_mesh(engine, id, true, |m| {
            for (index, ((vertex, face), attribute)) in vertices.iter().zip(&faces).zip(&attributes).enumerate() {
                let (v, f) = read_pair(lua, METHOD, index + 1, vertex, face)?;
                let a = element_id(lua, attribute, METHOD, "attributeId", index + 1)?;
                let (f, corner) = pair_corner(m, METHOD, index + 1, v, f)?;
                check_kind(a, kind)?;
                let slot = attribute_slot(m, kind, a)?;
                m.set_corner(f, corner, (kind - 1) as u32, slot);
            }
            Ok(none())
        })
    }));
    // BatchRemove(faceIds): removes faces, nothing else (measured in a 0.747 client: every id is checked first, so a bad one
    // removes none; another kind, 0 and a value that is no number are named as ids of no kind, a face that is gone is
    // "Invalid faceId"; a face named twice goes once; what the faces used stays until RemoveUnused; refused on a
    // fixed-size mesh; the slots are freed from the last listed face back, as Mesh::remove_faces says)
    add("BatchRemove", method(|lua, engine, id, args| {
        refuse_fixed(engine, id)?;
        if args.len() != 2 {
            return Err(err("BatchRemove: expects 1 argument (faceIds)"));
        }
        let LuaValue::Table(table) = args.get(1) else { return Err(err("BatchRemove: faceIds must be an array")) };
        let items = sequence(table);
        with_mesh(engine, id, true, |m| {
            let mut faces = Vec::with_capacity(items.len());
            for item in &items {
                let number = match item {
                    LuaValue::Integer(n) => *n as f64,
                    LuaValue::Number(n) => *n,
                    LuaValue::String(_) => lua.coerce_number(item.clone()).ok().flatten().unwrap_or(0.0),
                    _ => 0.0,
                };
                let f = id_of_number(number);
                if parts(f).0 != FACE {
                    return Err(wrong_kind(FACE, f));
                }
                faces.push(m.faces.find(FACE, f).ok_or_else(|| err("Invalid faceId"))?);
            }
            m.remove_faces(&faces);
            Ok(none())
        })
    }));

    add("GetBoneByName", method(|_, engine, id, args| {
        let name = args.string(1).map(|s| s.to_string_lossy()).unwrap_or_default();
        let found = with_mesh(engine, id, false, |m| Ok(m.bones.live().find(|(_, b)| b.name == name).map(|(i, _)| m.bones.id(BONE, i))))?;
        Ok(values([LuaValue::Number(found.unwrap_or(0.0))]))
    }));
    add("RemoveBone", method(|lua, engine, id, args| {
        let b = int64_arg(lua, engine, args, 1)?;
        refuse_fixed(engine, id)?;
        check_kind(b, BONE)?;
        with_mesh(engine, id, true, |m| {
            let b = m.bones.at(BONE, b)?;
            m.bones.remove(b);
            for bone in m.bones.items.iter_mut().flatten() {
                if bone.parent == Some(b) {
                    bone.parent = None;
                }
            }
            for (bones, weights) in m.skin.values_mut() {
                while let Some(at) = bones.iter().position(|&x| x == b) {
                    bones.remove(at);
                    if at < weights.len() {
                        weights.remove(at);
                    }
                }
            }
            Ok(none())
        })
    }));
    add("ResetNormal", method(|lua, engine, id, args| {
        let n = id_arg(lua, engine, args, 1, NORMAL)?;
        with_mesh(engine, id, true, |m| {
            let slot = m.normals.at(NORMAL, n)?;
            *m.normals.get_mut(slot).expect("live") = None;
            Ok(none())
        })
    }));
    add("Clear", method(|_, engine, id, _| {
        with_mesh(engine, id, true, |m| {
            *m = Mesh::default();
            Ok(none())
        })
    }));

    add("Destroy", method(|_, engine, id, _| {
        let ext = extension(engine);
        ext.meshes.borrow_mut().remove(&id);
        ext.dirty.borrow_mut().remove(&id);
        Ok(none())
    }));

}

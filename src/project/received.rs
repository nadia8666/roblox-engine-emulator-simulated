//! A model as a client receives it from the server. Models, snapshots and bundles reach a playing client through
//! replication: the server loads them and sends their instances. So a client script reads each stored rotation as the
//! server's load stores it and the network carries it, not as the file holds it.
//!
//! Measured in a playing client (roblox-sim, Studio 0.746, 2026-11-07). The sample was 2,347 rotations of 23 of the
//! game's bundles: the server loaded them from a place file, and a client read them after they replicated.
//! - **The server's load.** A part's CFrame more than 1e-5 off orthonormal is stored as `CFrame:Orthonormalize()` gives
//!   it, as a script's write is (`world::frame::stored_rotation`). A joint's C0 and an attachment's or bone's CFrame are
//!   orthonormalized even within that tolerance. A bone's last bits differ from Gram-Schmidt's in about half of them.
//!   A part's PivotOffset and a model's pivot keep the file's rotation.
//! - **Replication.** A rotation that is not axis-aligned crosses as a quaternion:
//!   - the largest component is dropped, with the sign made positive;
//!   - the other three, times sqrt 2, are rounded to steps of 1/16383 (15 bits);
//!   - the client rebuilds the largest from the others and makes a matrix of the quaternion ([`replicated`]).
//!
//!   So the client's rotation is orthonormal to float precision, and within about 1e-4 of the server's. Positions and
//!   axis-aligned rotations cross exactly.
//!
//! From the file's value, 2,328 of the 2,347 come out to the bit. The others are 18 bones, within 5e-7 (an ulp or two),
//! and one pivot that is 1.7e-4 off. That pivot's quaternion had four components all within 1e-5 of 0.5, and the
//! network dropped another one than the largest.
//!
//! Instances a client makes itself (`Instance.new`, `Clone`, which copies a rotation exactly) are not received this
//! way. A property not listed here (a camera's CFrame, say) keeps the file's value.

use rbx_dom_weak::{
    Ustr, WeakDom,
    types::{self as dom, Variant},
};
use rustc_hash::FxHashMap;

type Rot = [[f32; 3]; 3];

/// What a property of an instance is, for its load
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Part,
    Joint,
    Attachment,
    Model,
    Other,
}

/// Every rotation of `dom` that a client would receive, as it receives it ([`received`] for each)
pub fn as_received(dom: &mut WeakDom) {
    let database = rbx_reflection_database::get().ok();
    // (each class looked up once, by its interned name)
    let mut by_class: FxHashMap<Ustr, Kind> = FxHashMap::default();
    let mut kinds: Vec<(dom::Ref, Kind)> = Vec::new();
    for instance in dom.descendants() {
        let kind = *by_class.entry(instance.class).or_insert_with(|| kind_of(database, instance.class.as_str()));
        if kind != Kind::Other {
            kinds.push((instance.referent(), kind));
        }
    }
    for (referent, kind) in kinds {
        let Some(instance) = dom.get_by_ref_mut(referent) else { continue };
        for (name, value) in instance.properties.iter_mut() {
            let property = name.as_str();
            let load = match (kind, property) {
                (Kind::Part, "CFrame") => Load::Part,
                (Kind::Part, "PivotOffset") | (Kind::Model, "WorldPivotData") => Load::Kept,
                (Kind::Joint, "C0" | "C1") | (Kind::Attachment, "CFrame") => Load::Orthonormalized,
                _ => continue,
            };
            match value {
                Variant::CFrame(cframe) => *cframe = received_cframe(cframe, load),
                Variant::OptionalCFrame(Some(cframe)) => *cframe = received_cframe(cframe, load),
                _ => {}
            }
        }
    }
}

/// How the server's load stores a rotation
#[derive(Clone, Copy)]
pub enum Load {
    /// A part's CFrame: orthonormalized past 1e-5
    Part,
    /// A joint's C0 / C1, an attachment's or bone's CFrame: orthonormalized
    Orthonormalized,
    /// A part's PivotOffset, a model's pivot: as stored
    Kept,
}

fn received_cframe(cframe: &dom::CFrame, load: Load) -> dom::CFrame {
    let o = &cframe.orientation;
    // (rbx-dom's Matrix3 holds rows)
    let rows = [[o.x.x, o.x.y, o.x.z], [o.y.x, o.y.y, o.y.z], [o.z.x, o.z.y, o.z.z]];
    let out = received(rows, load);
    if out == rows {
        return *cframe;
    }
    let row = |r: [f32; 3]| dom::Vector3::new(r[0], r[1], r[2]);
    dom::CFrame::new(cframe.position, dom::Matrix3::new(row(out[0]), row(out[1]), row(out[2])))
}

/// A stored rotation as a client reads it: the server's load, then replication
pub fn received(rows: Rot, load: Load) -> Rot {
    use lune_roblox::datatypes::types::float32;
    let loaded = match load {
        Load::Part => crate::world::frame::stored_rotation(rows),
        Load::Orthonormalized if !axis_aligned(&rows) => {
            let normal = float32::orthonormalize(&rows);
            if normal.iter().flatten().all(|value| value.is_finite()) { normal } else { float32::IDENTITY }
        }
        Load::Orthonormalized | Load::Kept => rows,
    };
    replicated(loaded)
}

/// Every entry 0 or 1 or -1: one of the rotations that cross exactly
fn axis_aligned(rows: &Rot) -> bool {
    rows.iter().flatten().all(|&value| value == 0.0 || value.abs() == 1.0)
}

/// A rotation as it crosses the network: as is when axis-aligned (or not finite), else as a 15-bit quaternion. The
/// largest component is dropped and the other three, made positive with it and times sqrt 2, are rounded to steps of
/// 1/16383. The client takes each as `(n / 16383) * (1 / sqrt 2)`, the largest as `sqrt(1 - ((a² + b²) + c²))`, and
/// makes the matrix ([`rot_from_quat`]). All in single precision, as measured to the bit.
pub fn replicated(rows: Rot) -> Rot {
    if axis_aligned(&rows) || !rows.iter().flatten().all(|value| value.is_finite()) {
        return rows;
    }
    let mut q = quat_from_rot(&rows);
    // (the first of equal largest)
    let mut big = 0;
    for i in 1..4 {
        if q[i].abs() > q[big].abs() {
            big = i;
        }
    }
    if q[big] < 0.0 {
        q = q.map(|value| -value);
    }
    const STEPS: f32 = 16383.0;
    let scale = STEPS * std::f32::consts::SQRT_2;
    let mut out = [0.0f32; 4];
    let mut squares = 0.0f32;
    let mut first = true;
    for i in 0..4 {
        if i == big {
            continue;
        }
        let steps = (q[i] * scale + 0.5).floor();
        out[i] = (steps / STEPS) * std::f32::consts::FRAC_1_SQRT_2;
        // ((a² + b²) + c², in index order)
        squares = if first { out[i] * out[i] } else { squares + out[i] * out[i] };
        first = false;
    }
    out[big] = (1.0 - squares).max(0.0).sqrt();
    let received = rot_from_quat(out);
    if received.iter().flatten().all(|value| value.is_finite()) { received } else { rows }
}

/// A rotation's quaternion (x, y, z, w), as the engine takes it: the trace branch, else the largest diagonal term's,
/// subtracting the other two diagonal terms in index order (lune-roblox's float32 port, measured through CFrame:Lerp)
fn quat_from_rot(r: &Rot) -> [f32; 4] {
    let trace = (r[0][0] + r[1][1]) + r[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt();
        let k = 0.5 / s;
        return [(r[2][1] - r[1][2]) * k, (r[0][2] - r[2][0]) * k, (r[1][0] - r[0][1]) * k, s * 0.5];
    }
    let mut i = if r[1][1] > r[0][0] { 1 } else { 0 };
    if r[2][2] > r[i][i] {
        i = 2;
    }
    let (j, k3) = ((i + 1) % 3, (i + 2) % 3);
    let (first, second) = (j.min(k3), j.max(k3));
    let s = (((1.0 + r[i][i]) - r[first][first]) - r[second][second]).sqrt();
    let mut q = [0.0f32; 4];
    q[i] = s * 0.5;
    let k = 0.5 / s;
    q[3] = (r[k3][j] - r[j][k3]) * k;
    q[j] = (r[j][i] + r[i][j]) * k;
    q[k3] = (r[k3][i] + r[i][k3]) * k;
    q
}

/// A quaternion's rotation, normalized first (lune-roblox's float32 port)
fn rot_from_quat(q: [f32; 4]) -> Rot {
    let dot = (q[0] * q[0] + q[1] * q[1]) + (q[2] * q[2] + q[3] * q[3]);
    let k = 1.0 / dot.sqrt();
    let [x, y, z, w] = q.map(|c| c * k);
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [[1.0 - (yy + zz), xy - wz, xz + wy], [xy + wz, 1.0 - (xx + zz), yz - wx], [xz - wy, yz + wx, 1.0 - (xx + yy)]]
}

/// What a class's stored rotations are: a part's, a joint's, an attachment's (bones too) or a model's
fn kind_of(database: Option<&rbx_reflection::ReflectionDatabase<'_>>, class: &str) -> Kind {
    let Some(database) = database else { return Kind::Other };
    let mut current = Some(class.to_string());
    while let Some(name) = current {
        match name.as_str() {
            "BasePart" => return Kind::Part,
            "JointInstance" => return Kind::Joint,
            "Attachment" => return Kind::Attachment,
            "Model" => return Kind::Model,
            _ => {}
        }
        current = database.classes.get(name.as_str()).and_then(|descriptor| descriptor.superclass.as_ref().map(|s| s.to_string()));
    }
    Kind::Other
}


#[cfg(test)]
mod tests {
    use rbx_dom_weak::{InstanceBuilder, ustr};

    use super::*;

    fn rot(v: &[f32; 9]) -> Rot {
        [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]
    }

    /// Rotations stored in the game's bundles, and what a playing client read of them once the server had loaded and
    /// sent them (roblox-sim, Studio 0.746, 2026-11-07). They are one case of each property, each side of the 1e-5
    /// tolerance and each branch of the quaternion, from the probe's 2,347 (2,328 of which come out to the bit).
    #[test]
    fn a_received_rotation_is_what_a_client_read() {
        let cases: &[(&str, Load, [f32; 9], [f32; 9])] = &[
            ("Part.PivotOffset within 1e-5, aligned", Load::Kept, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("Part.CFrame within 1e-5, aligned", Load::Part, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("MeshPart.PivotOffset within 1e-5, aligned", Load::Kept, [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0]),
            ("MeshPart.CFrame within 1e-5, aligned", Load::Part, [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0]),
            ("Model.WorldPivotData within 1e-5, trace", Load::Kept, [1.0000036, -4.3711548e-08, 1.5293252e-06, 4.3711548e-08, 1.0000036, 6.697148e-14, 1.3544795e-06, -1.2238457e-16, 1.0], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("Part.CFrame within 1e-5, trace", Load::Part, [-1.7302833e-06, 3.977943e-20, 1.0000043, -7.5633185e-14, 1.0000043, 3.977943e-20, -1.0, 7.5633185e-14, 1.7302833e-06], [-1.1920929e-07, 0.0, 1.0000001, 0.0, 1.0, 0.0, -1.0000001, 0.0, -1.1920929e-07]),
            ("MeshPart.CFrame within 1e-5, trace", Load::Part, [-1.9051296e-06, -1.6292138e-07, 1.0000043, -7.551072e-14, 1.0000043, 1.6292138e-07, -1.0, -1.7765733e-13, 1.5554377e-06], [-1.1920929e-07, 0.0, 1.0000001, 0.0, 1.0, 0.0, -1.0000001, 0.0, -1.1920929e-07]),
            ("Part.CFrame within 1e-5, y", Load::Part, [-2.2726538e-06, -0.99999994, 1.6917397e-07, -1.0000049, -2.257308e-06, 5.96051e-08, 6.079698e-06, 1.6918005e-07, -1.0000025], [-1.1920929e-07, -1.0000001, 0.0, -1.0000001, -1.1920929e-07, -0.0, 0.0, 0.0, -1.0000002]),
            ("MeshPart.CFrame within 1e-5, y", Load::Part, [-0.99999976, -3.1936645e-12, 1.1535215e-06, 3.5198437e-12, 1.0000024, -4.0134046e-07, 1.1535215e-06, -1.6292097e-07, -1.0000024], [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0]),
            ("Model.WorldPivotData past 1e-5, trace", Load::Kept, [-0.38257182, -0.59908104, -0.7033804, 0.13209905, 0.7180127, -0.68338513, 0.91443366, -0.35436055, -0.19554947], [-0.38259804, -0.5990837, -0.70336145, 0.13211401, 0.7179862, -0.6834045, 0.9144203, -0.35439318, -0.19555283]),
            ("MeshPart.CFrame within 1e-5, x", Load::Part, [2.4662108e-07, -1.0, -3.084651e-08, -0.05233595, -4.3711434e-08, 0.9986302, -0.9986302, 3.320915e-07, -0.05233595], [9.417534e-06, -1.0, -8.910894e-06, -0.05238864, -9.417534e-06, 0.9986268, -0.9986268, -8.910894e-06, -0.052388668]),
            ("MeshPart.CFrame within 1e-5, z", Load::Part, [1.5724592e-07, -1.37468755e-14, -1.0, -4.371142e-08, -1.0000007, -1.8337555e-14, -1.0000007, 4.371142e-08, 4.1951435e-07], [-1.1920929e-07, -0.0, -1.0000001, 0.0, -1.0000002, 0.0, -1.0000001, 0.0, -1.1920929e-07]),
            ("Model.WorldPivotData past 1e-5, x", Load::Kept, [2.3537157e-07, 0.999981, 3.382643e-05, 1.0000284, 5.40194e-08, -0.00022615946, -0.00020939222, 3.8824455e-06, -1.0000455], [-1.1920929e-07, 1.0000001, 6.103889e-05, 1.0000001, -1.1920929e-07, -0.00018311667, -0.00018311667, 6.103889e-05, -1.0000002]),
            ("Model.WorldPivotData past 1e-5, y", Load::Kept, [-1.0, 0.0, 4.3257132e-06, 0.0, 1.0000107, 0.0, 4.325713e-06, 0.0, -1.0000107], [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0]),
            ("Part.PivotOffset past 1e-5, trace", Load::Kept, [1.0000064, -6.7800283e-06, 2.603978e-06, 7.376075e-06, 1.0000004, 4.2347237e-06, -2.8014183e-06, -4.340429e-06, 1.0000031], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("Part.PivotOffset within 1e-5, trace", Load::Kept, [1.0000033, -6.7055225e-06, 2.0861626e-06, 7.033348e-06, 1.0, 4.2077154e-06, -1.5646219e-06, -4.2170286e-06, 1.0000018], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("MeshPart.PivotOffset past 1e-5, trace", Load::Kept, [1.0, 1.2212513e-14, -5.7676425e-06, 1.2212513e-14, 1.0000143, 1.01566586e-14, -5.7676425e-06, 1.01566586e-14, 1.0000143], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            ("Part.CFrame within 1e-5, z", Load::Part, [-0.01906173, -0.18858126, 0.9818732, 0.8104693, -0.5779824, -0.09527477, 0.58547175, 0.7939608, 0.16385646], [-0.019019604, -0.1886166, 0.98186654, 0.81046295, -0.57798076, -0.095330805, 0.585481, 0.7939533, 0.16385978]),
            ("Bone.CFrame within 1e-5, trace", Load::Orthonormalized, [1.0, 0.0, 0.0, 0.0, 0.9805145, -0.19642639, -0.0, 0.19642639, 0.9805145], [1.0, 0.0, 0.0, 0.0, 0.980513, -0.19645438, 0.0, 0.19645438, 0.980513]),
            ("Part.CFrame past 1e-5, trace", Load::Part, [0.98480225, 0.17364502, 0.0, -0.17364502, 0.98480225, 0.0, 0.0, 0.0, 1.0], [0.9848125, 0.17362115, 0.0, -0.17362115, 0.9848125, -0.0, -0.0, 0.0, 1.0]),
            ("Weld.C0 past 1e-5, trace", Load::Orthonormalized, [0.9848175, -0.17364502, 0.0, 0.17364502, 0.9848175, 0.0, 0.0, 0.0, 1.0], [0.9848125, -0.17362115, 0.0, 0.17362115, 0.9848125, 0.0, 0.0, 0.0, 1.0]),
            ("MeshPart.CFrame past 1e-5, z", Load::Part, [-0.028747559, 0.016601562, 0.9994507, -0.5, -0.86602783, 0.0, 0.8655548, -0.49972534, 0.033203125], [-0.02873671, 0.016608536, 0.999449, -0.49998397, -0.86603475, 1.5690923e-05, 0.8655578, -0.49970806, 0.033190966]),
            ("Motor6D.C0 within 1e-5, x", Load::Orthonormalized, [-0.0, 0.0, 1.0, -0.5, -0.86602783, -0.0, 0.86602783, -0.5, 0.0], [-2.0623207e-05, 5.528331e-06, 0.99999994, -0.49997455, -0.86604, -5.528331e-06, 0.86604005, -0.49997455, 2.0623207e-05]),
            ("Bone.CFrame within 1e-5, x", Load::Orthonormalized, [1.0, 0.0, 0.0, -0.0, -0.99812317, 0.061187744, 0.0, -0.061187744, -0.99812317], [1.0, 0.0, 0.0, 0.0, -0.9981272, 0.061173644, 0.0, -0.061173644, -0.9981272]),
            ("Bone.CFrame past 1e-5, trace", Load::Orthonormalized, [1.0, -0.0, -0.0, 0.0, 0.9921417, 0.12516785, -0.0, -0.12516785, 0.9921417], [1.0, -0.0, -0.0, 0.0, 0.99213415, 0.12517901, 0.0, -0.12517901, 0.99213415]),
            ("MeshPart.CFrame past 1e-5, y", Load::Part, [-0.848053, -0.52919006, -0.027740479, 0.0, 0.052337646, -0.9986267, 0.5299225, -0.8468933, -0.044387817], [-0.8480631, -0.5291705, -0.027701318, -1.642108e-05, 0.052303195, -0.99863124, 0.52989507, -0.8469019, -0.04436505]),
            ("MeshPart.CFrame past 1e-5, trace", Load::Part, [1.0, 0.0, 0.0, 0.0, -0.34202576, -0.93969727, 0.0, 0.93969727, -0.34202576], [1.0, 0.0, 0.0, 0.0, -0.34204268, -0.9396844, 0.0, 0.9396844, -0.34204268]),
            ("MeshPart.CFrame past 1e-5, x", Load::Part, [0.309021, 0.8937073, -0.32528687, 0.0, -0.34202576, -0.93969727, -0.9510498, 0.29037476, -0.10569763], [0.30899334, 0.8936955, -0.32531744, -2.2441149e-05, -0.34204936, -0.93968195, -0.95106417, 0.29036278, -0.10567093]),
            ("Part.CFrame past 1e-5, y", Load::Part, [-0.96765137, 0.0, -0.25231934, 0.0, 1.0, 0.0, 0.25231934, 0.0, -0.96765137], [-0.96764266, 0.0, -0.25232473, 0.0, 1.0, 0.0, 0.25232473, 0.0, -0.96764266]),
        ];
        for (name, load, stored, read) in cases {
            assert_eq!(received(rot(stored), *load), rot(read), "{name}");
        }
    }

    /// A decoded model's rotations become what a client reads: a part's CFrame (orthonormalized past 1e-5) and
    /// PivotOffset, a joint's C0, a bone's CFrame and a model's pivot. Positions, axis-aligned rotations and other
    /// classes' CFrames stay as stored.
    #[test]
    fn a_model_is_received_as_a_client_reads_it() {
        let stored = rot(&[0.98480225, 0.17364502, 0.0, -0.17364502, 0.98480225, 0.0, 0.0, 0.0, 1.0]);
        let row = |r: [f32; 3]| dom::Vector3::new(r[0], r[1], r[2]);
        let skewed = dom::CFrame::new(dom::Vector3::new(1.5, -2.25, 3.0), dom::Matrix3::new(row(stored[0]), row(stored[1]), row(stored[2])));
        let aligned = dom::CFrame::new(dom::Vector3::new(0.25, 0.5, 0.75), dom::Matrix3::identity());
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let model = dom.insert(root, InstanceBuilder::new("Model").with_property("WorldPivotData", Variant::OptionalCFrame(Some(skewed))));
        let part = dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", skewed).with_property("PivotOffset", skewed));
        let motor = dom.insert(part, InstanceBuilder::new("Motor6D").with_property("C0", skewed).with_property("C1", aligned));
        let bone = dom.insert(part, InstanceBuilder::new("Bone").with_property("CFrame", skewed));
        let camera = dom.insert(model, InstanceBuilder::new("Camera").with_property("CFrame", skewed));
        as_received(&mut dom);
        let read = |referent, name: &str| match dom.get_by_ref(referent).unwrap().properties.get(&ustr(name)) {
            Some(Variant::CFrame(cframe)) | Some(Variant::OptionalCFrame(Some(cframe))) => *cframe,
            other => panic!("{name}: {other:?}"),
        };
        let rows = |cframe: dom::CFrame| {
            let o = cframe.orientation;
            [[o.x.x, o.x.y, o.x.z], [o.y.x, o.y.y, o.y.z], [o.z.x, o.z.y, o.z.z]]
        };
        let expect = |referent, name: &str, load: Load| {
            let cframe = read(referent, name);
            assert_eq!(rows(cframe), received(stored, load), "{name}");
            assert_eq!(cframe.position, skewed.position, "{name}'s position");
        };
        expect(part, "CFrame", Load::Part);
        expect(part, "PivotOffset", Load::Kept);
        expect(motor, "C0", Load::Orthonormalized);
        expect(bone, "CFrame", Load::Orthonormalized);
        expect(model, "WorldPivotData", Load::Kept);
        assert_ne!(rows(read(part, "CFrame")), stored, "orthonormalized and sent");
        assert_eq!(read(motor, "C1"), aligned);
        assert_eq!(read(camera, "CFrame"), skewed, "not a received rotation");
    }
}

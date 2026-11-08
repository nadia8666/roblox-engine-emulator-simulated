//! CFrame arithmetic for the 3D world (single precision, as Roblox's CFrame): conversions between
//! the engine's stored `dom::CFrame` and glam transforms, the Euler angle forms Roblox reads
//! (Orientation: `CFrame:ToOrientation()`, YXZ; Rotation: `CFrame:ToEulerAnglesXYZ()`, XYZ), and
//! the values scripts see.

use std::rc::Rc;

use glam::{Affine3A, Mat3, Vec3, Vec3A};
use rbx_dom_weak::types::{self as dom, Variant};

use crate::value::Value;

pub fn to_affine(cframe: &dom::CFrame) -> Affine3A {
    let o = &cframe.orientation;
    // rbx-dom's Matrix3 holds rows
    let matrix = Mat3::from_cols(Vec3::new(o.x.x, o.y.x, o.z.x), Vec3::new(o.x.y, o.y.y, o.z.y), Vec3::new(o.x.z, o.y.z, o.z.z));
    Affine3A::from_mat3_translation(matrix, Vec3::new(cframe.position.x, cframe.position.y, cframe.position.z))
}

pub fn from_affine(transform: &Affine3A) -> dom::CFrame {
    let m = transform.matrix3;
    let t = transform.translation;
    dom::CFrame::new(
        dom::Vector3::new(t.x, t.y, t.z),
        dom::Matrix3::new(
            dom::Vector3::new(m.x_axis.x, m.y_axis.x, m.z_axis.x),
            dom::Vector3::new(m.x_axis.y, m.y_axis.y, m.z_axis.y),
            dom::Vector3::new(m.x_axis.z, m.y_axis.z, m.z_axis.z),
        ),
    )
}

/// A stored CFrame property value as a transform (identity for nil or another type).
pub fn affine_of(value: Option<&Value>) -> Affine3A {
    match value {
        Some(Value::Dom(variant)) => match &**variant {
            Variant::CFrame(cframe) => to_affine(cframe),
            Variant::OptionalCFrame(Some(cframe)) => to_affine(cframe),
            _ => Affine3A::IDENTITY,
        },
        _ => Affine3A::IDENTITY,
    }
}

pub fn cframe_value(transform: &Affine3A) -> Value {
    Value::Dom(Rc::new(Variant::CFrame(from_affine(transform))))
}

/// A rotation (rows) as a client stores it in a part's CFrame or a joint's C0 / C1: as written while every entry of RᵀR - I is
/// within 1e-5, else orthonormalized as `CFrame:Orthonormalize()` does it (lune_roblox's float32 Gram-Schmidt), and identity where
/// that is not a number (a zero column). Measured in a playing client (roblox-sim, Studio 0.746, 2026-11-07): a column
/// stretched by 2e-6 or leaning 1e-5 toward another reads back as written, by 5e-6 or 2e-5 orthonormalized; a skewed
/// rotation reads back to the bit as Orthonormalize gives it, written to a part, through BulkMoveTo or as a Motor6D's C0; a
/// mirror stays one; a rotation with a zero column reads identity. (An Attachment's CFrame is orthonormalized too, but its
/// last bits differ from Gram-Schmidt's: not modelled.)
pub fn stored_rotation(rows: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    use lune_roblox::datatypes::types::float32;
    let column = |i: usize| [rows[0][i], rows[1][i], rows[2][i]];
    let (c0, c1, c2) = (column(0), column(1), column(2));
    let off = |a: [f32; 3], b: [f32; 3], want: f32| (float32::dot3(a, b) - want).abs() > 1e-5;
    if !(off(c0, c0, 1.0) || off(c1, c1, 1.0) || off(c2, c2, 1.0) || off(c0, c1, 0.0) || off(c0, c2, 0.0) || off(c1, c2, 0.0)) {
        return rows;
    }
    let normal = float32::orthonormalize(&rows);
    if normal.iter().flatten().all(|value| value.is_finite()) { normal } else { float32::IDENTITY }
}

/// A transform's rotation as [`stored_rotation`] stores it
pub fn stored_transform(transform: &Affine3A) -> Affine3A {
    let m = transform.matrix3;
    let rows = [[m.x_axis.x, m.y_axis.x, m.z_axis.x], [m.x_axis.y, m.y_axis.y, m.z_axis.y], [m.x_axis.z, m.y_axis.z, m.z_axis.z]];
    let stored = stored_rotation(rows);
    if stored == rows {
        return *transform;
    }
    let matrix = Mat3::from_cols(
        Vec3::new(stored[0][0], stored[1][0], stored[2][0]),
        Vec3::new(stored[0][1], stored[1][1], stored[2][1]),
        Vec3::new(stored[0][2], stored[1][2], stored[2][2]),
    );
    Affine3A::from_mat3_translation(matrix, transform.translation.into())
}

pub fn vector_value(vector: Vec3) -> Value {
    Value::Vector3(dom::Vector3::new(vector.x, vector.y, vector.z))
}

pub fn vector_of(value: &Value) -> Option<Vec3> {
    match value {
        Value::Vector3(v) => Some(Vec3::new(v.x, v.y, v.z)),
        _ => None,
    }
}

/// Where an Euler decomposition loses an axis (its middle angle at +-90 degrees): the last angle
/// is taken as 0 there.
const GIMBAL: f32 = 0.99999;

/// `CFrame:ToOrientation()` in degrees (Ry * Rx * Rz).
pub fn orientation(matrix: &Mat3) -> Vec3 {
    // m[row][col]: glam is column-major
    let r = |row: usize, col: usize| matrix.col(col)[row];
    let x = (-r(1, 2)).clamp(-1.0, 1.0).asin();
    let (y, z) = if r(1, 2).abs() < GIMBAL { (r(0, 2).atan2(r(2, 2)), r(1, 0).atan2(r(1, 1))) } else { ((-r(2, 0)).atan2(r(0, 0)), 0.0) };
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}

/// `CFrame.fromOrientation` of degrees.
pub fn from_orientation(degrees: Vec3) -> Mat3 {
    let radians = degrees * (std::f32::consts::PI / 180.0);
    Mat3::from_rotation_y(radians.y) * Mat3::from_rotation_x(radians.x) * Mat3::from_rotation_z(radians.z)
}

/// `CFrame:ToEulerAnglesXYZ()` in degrees (Rx * Ry * Rz).
pub fn rotation_xyz(matrix: &Mat3) -> Vec3 {
    let r = |row: usize, col: usize| matrix.col(col)[row];
    let y = r(0, 2).clamp(-1.0, 1.0).asin();
    let (x, z) = if r(0, 2).abs() < GIMBAL { ((-r(1, 2)).atan2(r(2, 2)), (-r(0, 1)).atan2(r(0, 0))) } else { (r(2, 1).atan2(r(1, 1)), 0.0) };
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}

/// `CFrame.Angles` of degrees.
pub fn from_rotation_xyz(degrees: Vec3) -> Mat3 {
    let radians = degrees * (std::f32::consts::PI / 180.0);
    Mat3::from_rotation_x(radians.x) * Mat3::from_rotation_y(radians.y) * Mat3::from_rotation_z(radians.z)
}

/// `CFrame.fromMatrix(position, right, up)`: the back vector completes them, as Roblox does.
pub fn from_axes(position: Vec3, right: Vec3, up: Vec3) -> Affine3A {
    let right = right.normalize_or(Vec3::X);
    let back = right.cross(up).normalize_or(Vec3::Z);
    let up = back.cross(right);
    Affine3A::from_mat3_translation(Mat3::from_cols(right, up, back), position)
}

pub fn translation(transform: &Affine3A) -> Vec3 {
    Vec3::from(transform.translation)
}

pub fn with_translation(transform: &Affine3A, position: Vec3) -> Affine3A {
    Affine3A { matrix3: transform.matrix3, translation: Vec3A::from(position) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-3
    }

    #[test]
    fn euler_forms_round_trip() {
        // each form's middle angle within +-90 degrees, the others all round
        for degrees in [Vec3::new(10.0, 20.0, 30.0), Vec3::new(-45.0, 170.0, 5.0), Vec3::new(60.0, -120.0, 150.0)] {
            assert!(close(orientation(&from_orientation(degrees)), degrees), "{degrees}");
        }
        for degrees in [Vec3::new(10.0, 20.0, 30.0), Vec3::new(170.0, -45.0, 5.0), Vec3::new(-120.0, 60.0, 150.0)] {
            assert!(close(rotation_xyz(&from_rotation_xyz(degrees)), degrees), "{degrees}");
        }
        // gimbal lock: the last angle reads 0, the rotation is the same
        for degrees in [Vec3::new(30.0, 90.0, 0.0), Vec3::new(0.0, -90.0, 0.0)] {
            assert!(close(rotation_xyz(&from_rotation_xyz(degrees)), degrees), "{degrees}");
        }
        assert!(close(orientation(&from_orientation(Vec3::new(90.0, 40.0, 0.0))), Vec3::new(90.0, 40.0, 0.0)));
        // CFrame.Angles(0, math.rad(90), 0) turns -Z (the look vector) to -X
        let turned = from_rotation_xyz(Vec3::new(0.0, 90.0, 0.0)) * Vec3::NEG_Z;
        assert!(close(turned, Vec3::NEG_X), "{turned}");
    }

    #[test]
    fn dom_cframes_keep_rows() {
        let transform = Affine3A::from_mat3_translation(from_orientation(Vec3::new(10.0, 40.0, -20.0)), Vec3::new(1.0, 2.0, 3.0));
        let back = to_affine(&from_affine(&transform));
        assert!(back.abs_diff_eq(transform, 1e-6));
        // rbx-dom rows: R00 R01 R02 is the first row
        let cframe = from_affine(&Affine3A::from_mat3(Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2)));
        assert!((cframe.orientation.x.y - -1.0).abs() < 1e-6, "{:?}", cframe.orientation);
    }
}

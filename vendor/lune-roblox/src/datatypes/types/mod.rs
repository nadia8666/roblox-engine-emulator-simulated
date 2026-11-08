mod axes;
mod brick_color;
mod cframe;
mod color3;
mod color_sequence;
mod color_sequence_keypoint;
mod content;
mod r#enum;
mod r#enum_item;
mod r#enums;
mod faces;
mod font;
mod number_range;
mod number_sequence;
mod number_sequence_keypoint;
mod physical_properties;
mod ray;
mod rect;
mod region3;
mod region3int16;
mod udim;
mod udim2;
mod unique_id;
mod vector2;
mod vector2int16;
mod vector3;
mod vector3int16;

pub use axes::Axes;
pub use brick_color::BrickColor;
pub use cframe::CFrame;
pub use color_sequence::ColorSequence;
pub use color_sequence_keypoint::ColorSequenceKeypoint;
pub use color3::Color3;
pub use content::Content;
pub use r#enum::Enum;
pub use r#enum_item::EnumItem;
pub use r#enums::Enums;
pub use faces::Faces;
pub use font::Font;
pub use number_range::NumberRange;
pub use number_sequence::NumberSequence;
pub use number_sequence_keypoint::NumberSequenceKeypoint;
pub use physical_properties::PhysicalProperties;
pub use ray::Ray;
pub use rect::Rect;
pub use region3::Region3;
pub use region3int16::Region3int16;
pub use udim::UDim;
pub use udim2::UDim2;
pub use unique_id::UniqueId;
pub use vector2::Vector2;
pub use vector2int16::Vector2int16;
pub use vector3::{Vector3, VectorArg, charge_host_calls, charge_host_nanos, datatype_price, native_vector};
pub use vector3int16::Vector3int16;

/// A number as C's `printf("%.*g", precision, value)` writes it, the way Roblox's datatypes print their components in
/// `tostring`: `precision` significant digits rounded from the exact value, trailing zeros dropped, the exponent form
/// (`1.00000002e+20`, `1e-07`, two exponent digits at least) when the exponent is below -4 or at least `precision`,
/// and `-0` for negative zero. Measured in a playing client (Studio 0.744): CFrame, Vector2, UDim, Rect, Ray and
/// PhysicalProperties print 9 digits (`0.333333343`), Color3, NumberRange, the sequences and TweenInfo's times 6
/// (`0.333333`). Not-a-number and infinities print as Luau's tostring does.
pub fn format_g(value: f64, precision: usize) -> String {
    let precision = precision.max(1);
    if value.is_nan() {
        return if value.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    // the exponent of the value rounded to `precision` digits decides the style, as C's %g does
    let scientific = format!("{:.*e}", precision - 1, value);
    let (mantissa, exponent) = scientific.split_once('e').expect("an exponent");
    let exponent: i32 = exponent.parse().expect("a decimal exponent");
    if exponent < -4 || exponent >= precision as i32 {
        let mantissa = trim_fraction(mantissa);
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exponent.unsigned_abs());
    }
    let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
    trim_fraction(&format!("{value:.decimals$}")).to_string()
}

fn trim_fraction(text: &str) -> &str {
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { text }
}

/// Roblox's single-precision arithmetic for CFrame, Vector2 and Color3, in a playing client's order of operations
/// (fitted to 5,764 values a Studio 0.744 playing client printed: tools/parity/probes/datatypes.luau and
/// cframe_order.luau): every step is a float operation, a three-term sum is grouped left to right, the trigonometry is
/// the C runtime's float functions (sinf, cosf, asinf, acosf, atan2f, which Rust's f32 methods call on Windows), and a
/// unit vector is `v * (1 / sqrtf(dot(v, v)))`.
pub mod float32 {
    /// A vector as three floats
    pub type Vec3 = [f32; 3];
    /// A rotation matrix by rows (GetComponents' R00 R01 R02 / R10 R11 R12 / R20 R21 R22)
    pub type Rot = [[f32; 3]; 3];

    pub const IDENTITY: Rot = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    pub fn dot3(a: Vec3, b: Vec3) -> f32 {
        (a[0] * b[0] + a[1] * b[1]) + a[2] * b[2]
    }

    pub fn cross3(a: Vec3, b: Vec3) -> Vec3 {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }

    /// Vector3.Unit: `v * (1 / sqrtf((x*x + y*y) + z*z))` (a zero vector gives not-a-number, as in Roblox)
    pub fn unit3(v: Vec3) -> Vec3 {
        let reciprocal = 1.0 / dot3(v, v).sqrt();
        [v[0] * reciprocal, v[1] * reciprocal, v[2] * reciprocal]
    }

    /// `a + (b - a) * t`: Vector3, Vector2, Color3 and UDim2 Lerp and a CFrame's position
    pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    pub fn column(r: &Rot, j: usize) -> Vec3 {
        [r[0][j], r[1][j], r[2][j]]
    }

    pub fn from_columns(x: Vec3, y: Vec3, z: Vec3) -> Rot {
        [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
    }

    pub fn transpose(r: &Rot) -> Rot {
        from_columns(r[0], r[1], r[2])
    }

    /// `R v`: each row dotted with v
    pub fn mat_vec(r: &Rot, v: Vec3) -> Vec3 {
        [dot3(r[0], v), dot3(r[1], v), dot3(r[2], v)]
    }

    /// `R^T v`: each column dotted with v
    pub fn mat_t_vec(r: &Rot, v: Vec3) -> Vec3 {
        [dot3(column(r, 0), v), dot3(column(r, 1), v), dot3(column(r, 2), v)]
    }

    pub fn mat_mul(a: &Rot, b: &Rot) -> Rot {
        let mut out = [[0.0; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, value) in row.iter_mut().enumerate() {
                *value = dot3(a[i], column(b, j));
            }
        }
        out
    }

    /// One factor of an Euler product: an exact 1, an exact 0, or a computed value. The client multiplies only the
    /// non-zero terms and takes a 1 as it is, so `(R_A R_B) R_C` keeps the products and sums those terms make.
    #[derive(Clone, Copy)]
    enum Term {
        Zero,
        One,
        Value(f32),
    }

    fn elementary(axis: u8, s: f32, c: f32) -> [[Term; 3]; 3] {
        use Term::{One, Value as V, Zero};
        match axis {
            b'X' => [[One, Zero, Zero], [Zero, V(c), V(-s)], [Zero, V(s), V(c)]],
            b'Y' => [[V(c), Zero, V(s)], [Zero, One, Zero], [V(-s), Zero, V(c)]],
            _ => [[V(c), V(-s), Zero], [V(s), V(c), Zero], [Zero, Zero, One]],
        }
    }

    fn sparse_mul(a: &[[Term; 3]; 3], b: &[[Term; 3]; 3]) -> [[Term; 3]; 3] {
        let mut out = [[Term::Zero; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let mut total: Option<Term> = None;
                for k in 0..3 {
                    let term = match (a[i][k], b[k][j]) {
                        (Term::Zero, _) | (_, Term::Zero) => continue,
                        (p, Term::One) => p,
                        (Term::One, q) => q,
                        (Term::Value(p), Term::Value(q)) => Term::Value(p * q),
                    };
                    total = Some(match total {
                        None => term,
                        Some(sum) => Term::Value(term_value(sum) + term_value(term)),
                    });
                }
                out[i][j] = total.unwrap_or(Term::Zero);
            }
        }
        out
    }

    fn term_value(term: Term) -> f32 {
        match term {
            Term::Zero => 0.0,
            Term::One => 1.0,
            Term::Value(v) => v,
        }
    }

    /// fromEulerAngles(x, y, z, order): `(R_A R_B) R_C` for the order's axes A B C, sines and cosines from sinf and
    /// cosf (CFrame.Angles and fromEulerAnglesXYZ are "XYZ", fromEulerAnglesYXZ and fromOrientation "YXZ")
    pub fn from_euler(order: &str, x: f32, y: f32, z: f32) -> Rot {
        let factor = |axis: u8| {
            let angle = match axis {
                b'X' => x,
                b'Y' => y,
                _ => z,
            };
            elementary(axis, angle.sin(), angle.cos())
        };
        let axes = order.as_bytes();
        let product = sparse_mul(&sparse_mul(&factor(axes[0]), &factor(axes[1])), &factor(axes[2]));
        product.map(|row| row.map(term_value))
    }

    /// ToEulerAngles(order) as (x, y, z): asinf of the middle axis's term and atan2f for the other two. A term at 1 or
    /// beyond (gimbal lock) is taken as 1 (not measured: the capture has no such rotation).
    pub fn to_euler(order: &str, r: &Rot) -> (f32, f32, f32) {
        let asin = |v: f32| v.clamp(-1.0, 1.0).asin();
        match order {
            "XYZ" => ((-r[1][2]).atan2(r[2][2]), asin(r[0][2]), (-r[0][1]).atan2(r[0][0])),
            "YXZ" => (asin(-r[1][2]), r[0][2].atan2(r[2][2]), r[1][0].atan2(r[1][1])),
            "ZXY" => (asin(r[2][1]), (-r[2][0]).atan2(r[2][2]), (-r[0][1]).atan2(r[1][1])),
            "XZY" => (r[2][1].atan2(r[1][1]), r[0][2].atan2(r[0][0]), asin(-r[0][1])),
            "YZX" => ((-r[1][2]).atan2(r[1][1]), (-r[2][0]).atan2(r[0][0]), asin(r[1][0])),
            _ => (r[2][1].atan2(r[2][2]), asin(-r[2][0]), r[1][0].atan2(r[0][0])),
        }
    }

    /// fromAxisAngle: the axis made a unit vector again, `t = 1 - cosf`, `R = t * (axis outer axis) + cos I + sin [axis]x`
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Rot {
        let [x, y, z] = unit3(axis);
        let (s, c) = (angle.sin(), angle.cos());
        let t = 1.0 - c;
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (xs, ys, zs) = (x * s, y * s, z * s);
        [
            [t * xx + c, t * xy - zs, t * xz + ys],
            [t * xy + zs, t * yy + c, t * yz - xs],
            [t * xz - ys, t * yz + xs, t * zz + c],
        ]
    }

    /// ToAxisAngle: `angle = acosf(0.5 * (((R00 + R11) + R22) - 1))`, `axis = (R21 - R12, R02 - R20, R10 - R01).Unit`.
    /// At angle 0 the axis is X (not measured: G3D's and glam's choice, and ree's before).
    pub fn to_axis_angle(r: &Rot) -> (Vec3, f32) {
        let cos = (0.5 * (((r[0][0] + r[1][1]) + r[2][2]) - 1.0)).clamp(-1.0, 1.0);
        let angle = cos.acos();
        if angle == 0.0 {
            return ([1.0, 0.0, 0.0], 0.0);
        }
        (unit3([r[2][1] - r[1][2], r[0][2] - r[2][0], r[1][0] - r[0][1]]), angle)
    }

    /// lookAt / lookAlong: `look = dir.Unit`, `right = (look x up).Unit`, or `(look x X).Unit` when `|look x up|^2`
    /// is below 1e-12 (measured to lie in (1e-14, 1e-12]), `up = right x look`; the columns right, up, -look. A zero
    /// direction gives not-a-number, as in Roblox. (Measured with the default up, Y.)
    pub fn look_along(direction: Vec3, up: Vec3) -> Rot {
        let look = unit3(direction);
        let mut c = cross3(look, up);
        if dot3(c, c) < 1e-12 {
            c = cross3(look, [1.0, 0.0, 0.0]);
        }
        let right = unit3(c);
        let up = cross3(right, look);
        from_columns(right, up, [-look[0], -look[1], -look[2]])
    }

    /// CFrame.new(position, target): G3D's CoordinateFrame lookAt (`up` is X when `|look . Y| > 0.99`; measured to start
    /// in (0.98894, 0.99034]): `u = (up - look * (look . up)).Unit`, `z = -look`, `x = -(z x u).Unit`, `y = z x x`.
    pub fn look_g3d(direction: Vec3) -> Rot {
        let look = unit3(direction);
        let mut up = [0.0, 1.0, 0.0];
        let mut d = dot3(look, up);
        if d.abs() > 0.99 {
            up = [1.0, 0.0, 0.0];
            d = dot3(look, up);
        }
        let u = unit3([up[0] - look[0] * d, up[1] - look[1] * d, up[2] - look[2] * d]);
        let z = [-look[0], -look[1], -look[2]];
        let c = cross3(z, u);
        let x = unit3([-c[0], -c[1], -c[2]]);
        let y = cross3(z, x);
        from_columns(x, y, z)
    }

    /// Orthonormalize: Gram-Schmidt over the columns, `q2 = (c2 - (d0 q0 + d1 q1)).Unit` with both dots taken on c2
    pub fn orthonormalize(r: &Rot) -> Rot {
        let (c0, c1, c2) = (column(r, 0), column(r, 1), column(r, 2));
        let q0 = unit3(c0);
        let d = dot3(q0, c1);
        let q1 = unit3([c1[0] - d * q0[0], c1[1] - d * q0[1], c1[2] - d * q0[2]]);
        let (d0, d1) = (dot3(q0, c2), dot3(q1, c2));
        let q2 = unit3([
            c2[0] - (d0 * q0[0] + d1 * q1[0]),
            c2[1] - (d0 * q0[1] + d1 * q1[1]),
            c2[2] - (d0 * q0[2] + d1 * q1[2]),
        ]);
        from_columns(q0, q1, q2)
    }

    type Quat = [f32; 4];

    /// A rotation's quaternion (x, y, z, w): the trace branch, else the largest diagonal term's, subtracting the other
    /// two diagonal terms in index order
    fn quat_from_rot(r: &Rot) -> Quat {
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

    fn quat_dot(a: Quat, b: Quat) -> f32 {
        (a[0] * b[0] + a[1] * b[1]) + (a[2] * b[2] + a[3] * b[3])
    }

    fn rot_from_quat(q: Quat) -> Rot {
        let k = 1.0 / quat_dot(q, q).sqrt();
        let [x, y, z, w] = q.map(|c| c * k);
        let (x2, y2, z2) = (x + x, y + y, z + z);
        let (xx, xy, xz) = (x * x2, x * y2, x * z2);
        let (yy, yz, zz) = (y * y2, y * z2, z * z2);
        let (wx, wy, wz) = (w * x2, w * y2, w * z2);
        [
            [1.0 - (yy + zz), xy - wz, xz + wy],
            [xy + wz, 1.0 - (xx + zz), yz - wx],
            [xz - wy, yz + wx, 1.0 - (xx + yy)],
        ]
    }

    /// CFrame:Lerp blends two rotations' quaternions when their dot is above this, rotations within about 2.2 degrees
    /// (measured in Studio 0.746: a dot of 0.99981022 blended, 0.99980998 slerped)
    const BLEND_ABOVE: f32 = 0.99981;

    /// CFrame:Lerp's rotation: the first at t == 0 and the goal at t == 1 exactly, else from their quaternions (the
    /// goal's negated when the dot c is negative): above BLEND_ABOVE the blend `q0 (1 - t) + q1 t` made a unit quaternion
    /// (`q * (1 / sqrtf(dot(q, q)))`), else the slerp (`sinf((1 - t) w) / sin w` and `sinf(t w) / sin w`, sin w as
    /// `sqrtf(1 - c*c)`), made a matrix again. Equal rotations blend: their dot can round to a float step past 1, where
    /// the slerp's sin w is not-a-number.
    pub fn lerp_rotation(a: &Rot, b: &Rot, t: f32) -> Rot {
        if t == 0.0 {
            return *a;
        }
        if t == 1.0 {
            return *b;
        }
        let q0 = quat_from_rot(a);
        let mut q1 = quat_from_rot(b);
        let mut c = quat_dot(q0, q1);
        if c < 0.0 {
            q1 = q1.map(|v| -v);
            c = -c;
        }
        if c > BLEND_ABOVE {
            let s = 1.0 - t;
            let blend = [0, 1, 2, 3].map(|i| q0[i] * s + q1[i] * t);
            let k = 1.0 / quat_dot(blend, blend).sqrt();
            return rot_from_quat(blend.map(|v| v * k));
        }
        let sin_omega = (1.0 - c * c).sqrt();
        let omega = c.acos();
        let k = 1.0 / sin_omega;
        let w0 = ((1.0 - t) * omega).sin() * k;
        let w1 = (t * omega).sin() * k;
        rot_from_quat([0, 1, 2, 3].map(|i| q0[i] * w0 + q1[i] * w1))
    }

    /// Color3:ToHSV: G3D's (Foley and van Dam), the hue's sixths divided at the end (pure red's hue is 1)
    pub fn to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let v = r.max(g).max(b);
        if v == 0.0 {
            return (0.0, 0.0, v);
        }
        let x = r.min(g).min(b);
        let s = (v - x) / v;
        if s == 0.0 {
            return (0.0, s, v);
        }
        let d = v - x;
        let (rn, gn, bn) = ((v - r) / d, (v - g) / d, (v - b) / d);
        let h = if r == v {
            if g == x { 5.0 + bn } else { 1.0 - gn }
        } else if g == v {
            if b == x { 1.0 + rn } else { 3.0 - bn }
        } else if r == x {
            3.0 + gn
        } else {
            5.0 - rn
        };
        (h / 6.0, s, v)
    }
}

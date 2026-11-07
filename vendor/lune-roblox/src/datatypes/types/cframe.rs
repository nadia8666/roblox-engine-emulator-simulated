#![allow(clippy::items_after_statements)]

use core::fmt;
use std::{ops, sync::Arc};

use glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use mlua::{Variadic, prelude::*};
use rbx_dom_weak::types::{CFrame as DomCFrame, Matrix3 as DomMatrix3, Vector3 as DomVector3};

use lune_utils::TableBuilder;

use crate::exports::LuaExportsTable;

use super::{super::*, Vector3, VectorArg, charge_host_nanos, float32 as f32m, native_vector};

/**
    An implementation of the [CFrame](https://create.roblox.com/docs/reference/engine/datatypes/CFrame)
    Roblox datatype, backed by [`glam::Mat4`].

    This implements most documented properties, methods & constructors of the `CFrame` class as of October 2025,
    notably missing `CFrame.fromRotationBetweenVectors` and `CFrame:AngleBetween()`.
*/
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CFrame(pub Mat4);

/// What Studio spends on each CFrame operation beyond the instructions that reach it (the loads, the
/// call, the loop around it), in nanoseconds, so that the work clock's cost for a CFrame operation in a
/// loop is Studio's (an op_cost probe, one body run in a lobby's playtest client and
/// in ree). Negative where the call alone already counts more than Studio spent on all of it.
pub mod price {
    //! Nanoseconds beyond what the instructions that reach the operation count (the loads, the call, the loop
    //! around it). Each is the lobby playtest client's median of four runs (the op_cost probe's
    //! `datatype` rows, in its client DataModel) less what ree counts for the same
    //! loop with no price (the VM's own charge for the call: 1.6 for a member, 15.1 for a method, 28-31 for a
    //! constructor the prelude wraps). A CFrame the operation returns is a userdata the garbage collector
    //! sweeps, which a client with a big heap pays for: the same operations in an empty test place cost
    //! 40-50 ns less (`*` 20 against 62, Inverse 20 against 69), and the allocating rows vary 2-3x between
    //! runs (Lerp 85-140, `*` 30-78, new(Vector3) 15-78), so these are good to about +-30 %.
    //!
    //! `*`, Inverse and `==` are the exception: they are priced as a handler meets them, a few times a
    //! frame on a CFrame taken from an instance (a bridge_ops probe, 28 rigs, five motors each, the median
    //! of 210 frames; an empty test place's client and a match place's client, three runs, six readings each): Inverse
    //! 18-42 ns (median 24.5), `* CFrame.identity` 20-39 (24.6), `== CFrame.identity` 15-26 (22.2); the same
    //! operations in the op_cost loop of the lobby client read 69, 62 and (not probed). A cut that removes them
    //! from per-frame code (a rig's per-frame cuts) is priced from the first set; a loop of thousands of them reads
    //! about 40 ns low per operation in ree, as the test place's loop does.
    //!
    //! `:ToObjectSpace`, `:Lerp`, `.Rotation`, `CFrame.Angles` and `CFrame.fromEulerAngles*` follow: the same
    //! rig-frame bench (a match place's client, three runs, six readings each, medians in ns): ToObjectSpace 28.9 (24-64),
    //! Lerp 66.8 (44-88), Rotation 30.3 (9-66), Angles 28.6 (18-36), fromEulerAnglesXYZ 27.0 (16-34). The op_cost loop of
    //! the lobby client reads 71, 110, 63, 87 for the first four, 1.5-3x higher.
    //!
    //! `:ToWorldSpace`, `:PointToWorldSpace` and `:ToAxisAngle` follow (the same bench, a match place's client, six
    //! runs of six readings, medians in ns): ToWorldSpace 26.6 (16-121; the twin of ToObjectSpace, which reads 28.5),
    //! PointToWorldSpace 24.4 with the vector hoisted out of the loop, ToAxisAngle 22.2 here and 26.3 in the earlier three runs.
    //!
    //! Refitted (2026-11-07) where the parity bench (tools/cost-parity: a rendered client of a game's match place, nine sessions of
    //! the whole corpus, the median of 9 rounds of a loop each) has a row outside its held-out third, ns whole: GetComponents 22.0 (all
    //! twelve values taken 26.9), `cf + v` 22.1, Rotation 18.1, Angles 41.3, the twelve-number constructor 35.5, Inverse 36.3, Lerp
    //! 75.7, PointToObjectSpace 17.9, ToEulerAnglesXYZ 22.1, ToObjectSpace 20.2, `cf * v` 12.3, X / LookVector / RightVector 3.3-3.7.
    //! Those rows read 0.41-5.9x before (GetComponents 131 ns). `*`, `==`, lookAt and the three-number constructor keep their
    //! prices: their rows are held out (`*` 41.8, lookAt 43.3, new 18.8).
    pub const MEMBER: i32 = 1;
    pub const ROTATION: i32 = 17;
    pub const MUL_CFRAME: i32 = 21;
    /// `CFrame == CFrame` is a metamethod call in Studio (22.2 ns median in the rig frame), where the VM counts only the
    /// instruction
    pub const EQ: i32 = 17;
    pub const MUL_VECTOR: i32 = 11;
    pub const ADD_VECTOR: i32 = 21;
    pub const INVERSE: i32 = 26;
    pub const LERP: i32 = 66;
    pub const TO_WORLD: i32 = 10;
    pub const TO_OBJECT: i32 = 10;
    pub const POINT_TO_WORLD: i32 = 8;
    pub const POINT_TO_OBJECT: i32 = 7;
    pub const VECTOR_TO_WORLD: i32 = -4;
    pub const VECTOR_TO_OBJECT: i32 = 0;
    pub const COMPONENTS: i32 = 14;
    pub const EULER: i32 = 12;
    pub const AXIS_ANGLE: i32 = 10;
    /// (not measured)
    pub const ORTHONORMALIZE: i32 = 0;
    pub const NEW_VECTOR: i32 = -3;
    pub const NEW_XYZ: i32 = -2;
    pub const NEW_LOOK: i32 = 55;
    /// (not measured)
    pub const NEW_QUAT: i32 = 0;
    /// (the twelve-number constructor)
    pub const NEW_MATRIX: i32 = 8;
    /// (a Studio playtest client read Angles at 27-29 ns, below what the call counts here; a client of the match place 49.6)
    pub const ANGLES: i32 = 18;
    pub const FROM_AXIS_ANGLE: i32 = 48;
    pub const LOOK_AT: i32 = 55;
    /// `:FuzzyEq` comparing both parts, and answering from the position alone (a client stops at a position that differs). The
    /// parity bench's rows (a rendered client of a game's match place, six sessions of the whole corpus on a busy machine, each
    /// row scaled by its session's median ree / client over the datatype family's fit rows, 0.55-0.69, to the reference sessions'
    /// speed): a pair a micro-stud apart 26.5 ns whole (25.0-28.5), a pair a stud apart 15.5 (12.3-17.5), where the call counts
    /// 15.9 here; held out, two CFrames apart in both with an eps given 17.7 (ree 16.4).
    pub const FUZZY_EQ: i32 = 11;
    pub const FUZZY_EQ_MOVED: i32 = 0;
}

/// `CFrame:FuzzyEq(other, eps = 1e-5)` as a client answers it (measured in a playing client, Studio 0.744: 16,784 pairs at the
/// rotation threshold and 8,000 at the position one, every one decided alike; tools/parity/probes/cframe_fuzzyeq.luau keeps the
/// rules). Everything is single precision: eps is taken as a float and its absolute value is used (a negative eps behaves as its
/// size, NaN fails every comparison). Position, component by component: self's `a` and the other's `b` are equal, or
/// `|a - b| <= |eps| * (|a| + 1)`, the tolerance growing with self's component alone (`|eps|` once `|a| + 1` is infinite).
/// Rotation: the relative rotation `C = Ra^T Rb` (column dot products, as `:ToObjectSpace` makes them, summed in order without a
/// fused multiply-add), then `sqrt(sum of (C - I)^2, row by row) / sqrt(2) <= |eps|`: the angle between the two for small ones,
/// and the matrices' plain distance for ones that are not rotations (a stretched axis passes up to `sqrt(2) * eps`).
fn fuzzy_eq_position(a: &CFrame, b: &CFrame, eps: f32) -> bool {
    let eps = eps.abs();
    let (pa, pb) = (a.position(), b.position());
    for axis in 0..3 {
        let (x, y) = (pa[axis], pb[axis]);
        if x == y {
            continue;
        }
        let scale = x.abs() + 1.0;
        let tolerance = if scale.is_infinite() { eps } else { eps * scale };
        if !((x - y).abs() <= tolerance) {
            return false;
        }
    }
    true
}

/// The rotation half of [`fuzzy_eq_position`]'s rule
fn fuzzy_eq_rotation(a: &CFrame, b: &CFrame, eps: f32) -> bool {
    let eps = eps.abs();
    let (ra, rb) = (a.orientation(), b.orientation());
    let mut sum = 0.0f32;
    for i in 0..3 {
        let u = ra.col(i);
        for j in 0..3 {
            let v = rb.col(j);
            // (written out: the order of the sum and the absence of a fused multiply-add are the client's)
            let dot = u.x * v.x + u.y * v.y + u.z * v.z;
            let residual = dot - if i == j { 1.0 } else { 0.0 };
            sum += residual * residual;
        }
    }
    sum.sqrt() / std::f32::consts::SQRT_2 <= eps
}

/// The script-facing `FuzzyEq`: a C function, so that its errors are the client's plain strings raised from the calling line
/// (an mlua callback raises its error as a userdata with a traceback). Its upvalue is [`fuzzy_eq_checked`], which checks the
/// arguments as the client does and answers `(0, result)` or `(n, nil)`: argument `n` is not what it should be (1-3), or the
/// eps is out of range (4).
struct FuzzyEqMethod;

impl IntoLua for FuzzyEqMethod {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let checked = lua.create_function(fuzzy_eq_checked)?;
        let method = unsafe {
            lua.exec_raw::<LuaFunction>(checked, |state| {
                mlua::ffi::lua_pushcclosurek(state, fuzzy_eq_c, c"FuzzyEq".as_ptr(), 1, None);
            })?
        };
        Ok(LuaValue::Function(method))
    }
}

fn fuzzy_eq_checked(lua: &Lua, args: LuaMultiValue) -> LuaResult<(i64, LuaValue)> {
    let cframe = |value: Option<&LuaValue>| match value {
        Some(LuaValue::UserData(ud)) => ud.borrow::<CFrame>().ok().map(|cf| *cf),
        _ => None,
    };
    let Some(this) = cframe(args.front()) else { return Ok((1, LuaValue::Nil)) };
    let Some(other) = cframe(args.get(1)) else { return Ok((2, LuaValue::Nil)) };
    let eps = match args.get(2) {
        None => 1e-5f32,
        Some(value) => match lua.coerce_number(value.clone())? {
            Some(number) => number as f32,
            None => return Ok((3, LuaValue::Nil)),
        },
    };
    // (Roblox's message for an eps whose size, as a float, is over 0.1; NaN passes it)
    if eps.abs() > 0.1 {
        return Ok((4, LuaValue::Nil));
    }
    let close = if fuzzy_eq_position(&this, &other, eps) {
        charge_host_nanos(lua, price::FUZZY_EQ);
        fuzzy_eq_rotation(&this, &other, eps)
    } else {
        charge_host_nanos(lua, price::FUZZY_EQ_MOVED);
        false
    };
    Ok((0, LuaValue::Boolean(close)))
}

/// A script's `typeof` of the argument at `index`, for an error message: a userdata's `__type` (not a newproxy's), Vector3
/// for a vector, else the basic type's name.
unsafe fn fuzzy_eq_type_name(state: *mut mlua::ffi::lua_State, index: std::ffi::c_int) -> String {
    use mlua::ffi;
    unsafe {
        let kind = ffi::lua_type(state, index);
        if kind == ffi::LUA_TVECTOR {
            return "Vector3".to_string();
        }
        if kind == ffi::LUA_TUSERDATA
            && ffi::lua_userdatatag(state, index) != ffi::LUA_UTAG_LIMIT + 1
            && ffi::luaL_getmetafield(state, index, c"__type".as_ptr()) != 0
        {
            let name = if ffi::lua_type(state, -1) == ffi::LUA_TSTRING {
                let mut length = 0;
                let text = ffi::lua_tolstring(state, -1, &mut length);
                Some(String::from_utf8_lossy(std::slice::from_raw_parts(text.cast::<u8>(), length)).into_owned())
            } else {
                None
            };
            ffi::lua_pop(state, 1);
            if let Some(name) = name {
                return name;
            }
        }
        std::ffi::CStr::from_ptr(ffi::lua_typename(state, kind)).to_string_lossy().into_owned()
    }
}

unsafe extern "C-unwind" fn fuzzy_eq_c(state: *mut mlua::ffi::lua_State) -> std::ffi::c_int {
    use mlua::ffi;
    unsafe {
        let count = ffi::lua_gettop(state);
        if count < 2 {
            let message = format!("Invalid number of arguments: {count}, CFrame.FuzzyEq expects 2 or 3.");
            ffi::lua_pushlstring(state, message.as_ptr().cast(), message.len());
            drop(message);
            ffi::lua_error(state);
        }
        let given = count.min(3);
        ffi::lua_pushvalue(state, ffi::lua_upvalueindex(1));
        for index in 1..=given {
            ffi::lua_pushvalue(state, index);
        }
        ffi::lua_call(state, given, 2);
        let status = ffi::lua_tointeger(state, -2);
        if status == 0 {
            return 1;
        }
        let message = match status {
            1 => format!("invalid argument #1 to 'FuzzyEq' (CFrame expected, got {})", fuzzy_eq_type_name(state, 1)),
            2 => format!("invalid argument #2 (CFrame expected, got {})", fuzzy_eq_type_name(state, 2)),
            3 => format!("invalid argument #3 (number expected, got {})", fuzzy_eq_type_name(state, 3)),
            _ => String::from("The eps value provided to FuzzyEq should be a small positive value <= 0.1"),
        };
        ffi::lua_pushlstring(state, message.as_ptr().cast(), message.len());
        drop(message);
        if status != 4 {
            ffi::luaL_where(state, 1);
            ffi::lua_insert(state, -2);
            ffi::lua_concat(state, 2);
        }
        ffi::lua_error(state)
    }
}

/// A method under each of its names, charged `price` nanoseconds
macro_rules! aliased {
    ($methods:ident, $price:expr, [$($name:literal),+], $function:expr) => {
        $($methods.add_method($name, charged_method($price, $function));)+
    };
}

fn charged_method<A, R>(
    price: i32,
    function: impl Fn(&Lua, &CFrame, A) -> LuaResult<R> + 'static,
) -> impl Fn(&Lua, &CFrame, A) -> LuaResult<R> + 'static {
    move |lua, this, args| {
        charge_host_nanos(lua, price);
        function(lua, this, args)
    }
}

/// An Enum.RotationOrder argument (the host's Enum items print as "Enum.RotationOrder.XYZ"; nil is XYZ):
/// the order's name. Angles go by the letters of the name, x y z.
fn rotation_order(order: Option<&LuaValue>, function: &str) -> LuaResult<&'static str> {
    let text = match order {
        None | Some(LuaValue::Nil) => String::from("Enum.RotationOrder.XYZ"),
        Some(item) => item.to_string()?,
    };
    Ok(match text.strip_prefix("Enum.RotationOrder.") {
        Some("XYZ") => "XYZ",
        Some("XZY") => "XZY",
        Some("YXZ") => "YXZ",
        Some("YZX") => "YZX",
        Some("ZXY") => "ZXY",
        Some("ZYX") => "ZYX",
        // (Studio's message for a string: "Invalid value for enum 'RotationOrder'")
        _ => {
            let _ = function;
            return Err(LuaError::RuntimeError("Invalid value for enum 'RotationOrder'".to_string()));
        }
    })
}

fn array(v: Vec3) -> f32m::Vec3 {
    [v.x, v.y, v.z]
}

/// CFrame's operations run in a playing client's float order (types::float32): products and spaces take each row dot
/// a column left to right and add the translation last, the inverse's position is `(-r0 p0 + -r1 p1) + -r2 p2` per
/// column, PointToObjectSpace is `R^T (v - p)` (not the inverse times v), ToObjectSpace the inverse times the frame.
impl CFrame {
    pub const IDENTITY: Self = Self(Mat4::IDENTITY);

    fn position(&self) -> Vec3 {
        self.0.w_axis.truncate()
    }

    fn orientation(&self) -> Mat3 {
        Mat3::from_cols(
            self.0.x_axis.truncate(),
            self.0.y_axis.truncate(),
            self.0.z_axis.truncate(),
        )
    }

    /// The rotation by rows
    fn rot(&self) -> f32m::Rot {
        let (x, y, z) = (self.0.x_axis, self.0.y_axis, self.0.z_axis);
        [[x.x, y.x, z.x], [x.y, y.y, z.y], [x.z, y.z, z.z]]
    }

    fn pos(&self) -> f32m::Vec3 {
        array(self.position())
    }

    fn from_parts(p: f32m::Vec3, r: f32m::Rot) -> Self {
        CFrame(Mat4::from_cols(
            Vec4::new(r[0][0], r[1][0], r[2][0], 0.0),
            Vec4::new(r[0][1], r[1][1], r[2][1], 0.0),
            Vec4::new(r[0][2], r[1][2], r[2][2], 0.0),
            Vec4::new(p[0], p[1], p[2], 1.0),
        ))
    }

    fn inverse(&self) -> Self {
        let (p, t) = (self.pos(), f32m::transpose(&self.rot()));
        let position = [0, 1, 2].map(|i| (-t[i][0] * p[0] + -t[i][1] * p[1]) + -t[i][2] * p[2]);
        Self::from_parts(position, t)
    }

    fn point_to_world(&self, v: Vec3) -> Vec3 {
        let (p, rv) = (self.pos(), f32m::mat_vec(&self.rot(), array(v)));
        Vec3::new(rv[0] + p[0], rv[1] + p[1], rv[2] + p[2])
    }

    fn point_to_object(&self, v: Vec3) -> Vec3 {
        let p = self.pos();
        Vec3::from_array(f32m::mat_t_vec(&self.rot(), [v.x - p[0], v.y - p[1], v.z - p[2]]))
    }

    fn vector_to_world(&self, v: Vec3) -> Vec3 {
        Vec3::from_array(f32m::mat_vec(&self.rot(), array(v)))
    }

    fn vector_to_object(&self, v: Vec3) -> Vec3 {
        Vec3::from_array(f32m::mat_t_vec(&self.rot(), array(v)))
    }
}

impl LuaExportsTable for CFrame {
    const EXPORT_NAME: &'static str = "CFrame";

    #[allow(clippy::too_many_lines)]
    fn create_exports_table(lua: Lua) -> LuaResult<LuaTable> {
        let cframe_angles = |lua: &Lua, (rx, ry, rz): (f32, f32, f32)| {
            charge_host_nanos(lua, price::ANGLES);
            Ok(CFrame::from_parts([0.0; 3], f32m::from_euler("XYZ", rx, ry, rz)))
        };

        let cframe_from_axis_angle = |lua: &Lua, (v, r): (VectorArg, f32)| {
            charge_host_nanos(lua, price::FROM_AXIS_ANGLE);
            Ok(CFrame::from_parts([0.0; 3], f32m::from_axis_angle(array(v.0), r)))
        };

        let cframe_from_euler_angles_xyz = |lua: &Lua, (rx, ry, rz): (f32, f32, f32)| {
            charge_host_nanos(lua, price::ANGLES);
            Ok(CFrame::from_parts([0.0; 3], f32m::from_euler("XYZ", rx, ry, rz)))
        };

        let cframe_from_euler_angles = |lua: &Lua, (rx, ry, rz, order): (f32, f32, f32, Option<LuaValue>)| {
            charge_host_nanos(lua, price::ANGLES);
            let name = rotation_order(order.as_ref(), "fromEulerAngles")?;
            Ok(CFrame::from_parts([0.0; 3], f32m::from_euler(name, rx, ry, rz)))
        };

        let cframe_from_euler_angles_yxz = |lua: &Lua, (rx, ry, rz): (f32, f32, f32)| {
            charge_host_nanos(lua, price::ANGLES);
            Ok(CFrame::from_parts([0.0; 3], f32m::from_euler("YXZ", rx, ry, rz)))
        };

        let cframe_from_matrix = |lua: &Lua,
                                  (pos, rx, ry, rz): (
            VectorArg,
            VectorArg,
            VectorArg,
            Option<VectorArg>,
        )| {
            charge_host_nanos(lua, price::NEW_MATRIX);
            // (a missing Z column is X cross Y made a unit vector; not measured)
            let z = rz.map_or_else(|| f32m::unit3(f32m::cross3(array(rx.0), array(ry.0))), |r| array(r.0));
            Ok(CFrame::from_parts(array(pos.0), f32m::from_columns(array(rx.0), array(ry.0), z)))
        };

        let cframe_from_orientation = |lua: &Lua, (rx, ry, rz): (f32, f32, f32)| {
            charge_host_nanos(lua, price::ANGLES);
            Ok(CFrame::from_parts([0.0; 3], f32m::from_euler("YXZ", rx, ry, rz)))
        };

        let cframe_look_at = |lua: &Lua, (from, to, up): (VectorArg, VectorArg, Option<VectorArg>)| {
            charge_host_nanos(lua, price::LOOK_AT);
            Ok(look_at(from.0, to.0, up.map_or(Vec3::Y, |up| up.0)))
        };

        // lookAlong takes the direction itself: far from the origin it is not lookAt(at, at + direction), whose sum
        // rounds (Studio 0.744, tools/parity/probes/cframe_order.luau)
        let cframe_look_along = |lua: &Lua, (at, direction, up): (VectorArg, VectorArg, Option<VectorArg>)| {
            charge_host_nanos(lua, price::LOOK_AT);
            Ok(CFrame::from_parts(
                array(at.0),
                f32m::look_along(array(direction.0), array(up.map_or(Vec3::Y, |up| up.0))),
            ))
        };

        // Dynamic args constructor
        type ArgsPos = VectorArg;
        type ArgsLook = (VectorArg, VectorArg, Option<VectorArg>);

        type ArgsPosXYZ = (f32, f32, f32);
        type ArgsPosXYZQuat = (f32, f32, f32, f32, f32, f32, f32);
        type ArgsMatrix = (f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32, f32);

        let cframe_new = |lua: &Lua, args: LuaMultiValue| {
            charge_host_nanos(lua, match (args.len(), args.front()) {
                (1, _) => price::NEW_VECTOR,
                (2, _) => price::NEW_LOOK,
                (3, Some(LuaValue::Number(_) | LuaValue::Integer(_))) => price::NEW_XYZ,
                (3, _) => price::NEW_LOOK,
                (7, _) => price::NEW_QUAT,
                (12, _) => price::NEW_MATRIX,
                _ => 0,
            });
            match args.len() {
            0 => Ok(CFrame(Mat4::IDENTITY)),

            1 => match ArgsPos::from_lua_multi(args, lua) {
                Ok(pos) => Ok(CFrame(Mat4::from_translation(pos.0))),
                Err(err) => Err(err),
            },

            // Roblox's CFrame.new(from, to): looking at `to`, as G3D's CoordinateFrame looks (types::float32)
            2 => {
                let vector = |index: usize| {
                    ArgsPos::from_lua(args[index].clone(), lua).map_err(|cause| {
                        LuaError::BadArgument {
                            to: Some("new".to_string()),
                            pos: index + 1,
                            name: None,
                            cause: Arc::new(cause),
                        }
                    })
                };
                let (from, to) = (vector(0)?.0, vector(1)?.0);
                Ok(CFrame::from_parts(array(from), f32m::look_g3d(array(to - from))))
            }

            3 => {
                // the position as plain numbers, without the argument conversions below
                let number = |index: usize| match args[index] {
                    LuaValue::Number(n) => Some(n as f32),
                    LuaValue::Integer(n) => Some(n as f32),
                    _ => None,
                };
                if let (Some(x), Some(y), Some(z)) = (number(0), number(1), number(2)) {
                    Ok(CFrame(Mat4::from_translation(Vec3::new(x, y, z))))
                } else if let Ok((from, to, up)) = ArgsLook::from_lua_multi(args.clone(), lua) {
                    Ok(look_at(from.0, to.0, up.map_or(Vec3::Y, |up| up.0)))
                } else if let Ok((x, y, z)) = ArgsPosXYZ::from_lua_multi(args, lua) {
                    Ok(CFrame(Mat4::from_translation(Vec3::new(x, y, z))))
                } else {
                    // TODO: Make this error message better
                    Err(LuaError::RuntimeError(
                        "Invalid arguments to constructor".to_string(),
                    ))
                }
            }

            7 => match ArgsPosXYZQuat::from_lua_multi(args, lua) {
                Ok((x, y, z, qx, qy, qz, qw)) => Ok(CFrame(Mat4::from_rotation_translation(
                    Quat::from_array([qx, qy, qz, qw]),
                    Vec3::new(x, y, z),
                ))),
                Err(err) => Err(err),
            },

            12 => match ArgsMatrix::from_lua_multi(args, lua) {
                Ok((x, y, z, r00, r01, r02, r10, r11, r12, r20, r21, r22)) => {
                    Ok(CFrame(Mat4::from_cols_array_2d(&[
                        [r00, r10, r20, 0.0],
                        [r01, r11, r21, 0.0],
                        [r02, r12, r22, 0.0],
                        [x, y, z, 1.0],
                    ])))
                }
                Err(err) => Err(err),
            },

            _ => Err(LuaError::RuntimeError(format!(
                "Invalid number of arguments: expected 0, 1, 3, 7, or 12, got {}",
                args.len()
            ))),
            }
        };

        TableBuilder::new(lua)?
            .with_function("Angles", cframe_angles)?
            .with_value("identity", CFrame(Mat4::IDENTITY))?
            .with_function("fromAxisAngle", cframe_from_axis_angle)?
            .with_function("fromEulerAngles", cframe_from_euler_angles)?
            .with_function("fromEulerAnglesXYZ", cframe_from_euler_angles_xyz)?
            .with_function("fromEulerAnglesYXZ", cframe_from_euler_angles_yxz)?
            .with_function("fromMatrix", cframe_from_matrix)?
            .with_function("fromOrientation", cframe_from_orientation)?
            .with_function("lookAlong", cframe_look_along)?
            .with_function("lookAt", cframe_look_at)?
            .with_function("new", cframe_new)?
            .build_readonly()
    }
}

impl LuaUserData for CFrame {
    fn add_fields<F: LuaUserDataFields<Self>>(fields: &mut F) {
        // Vectors come back as Luau's native vector, which is what a script's Vector3 is; each read is
        // charged to the script as the one Luau call the engine's prelude spent on it before it was native
        fields.add_field_method_get("Position", |lua, this| {
            charge_host_nanos(lua, price::MEMBER);
            Ok(native_vector(this.position()))
        });
        // (Roblox still answers its deprecated lowercase members)
        fields.add_field_method_get("p", |lua, this| {
            charge_host_nanos(lua, price::MEMBER);
            Ok(native_vector(this.position()))
        });
        fields.add_field_method_get("Rotation", |lua, this| {
            charge_host_nanos(lua, price::ROTATION);
            Ok(CFrame(Mat4::from_cols(
                this.0.x_axis,
                this.0.y_axis,
                this.0.z_axis,
                Vec3::ZERO.extend(1.0),
            )))
        });
        // (and the lowercase x / y / z, measured in a client: ree's tools/parity/probes/camelcase.luau)
        for name in ["X", "x"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(this.position().x)
            });
        }
        for name in ["Y", "y"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(this.position().y)
            });
        }
        for name in ["Z", "z"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(this.position().z)
            });
        }
        for name in ["XVector", "RightVector", "rightVector"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(native_vector(this.orientation().x_axis))
            });
        }
        for name in ["YVector", "UpVector", "upVector"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(native_vector(this.orientation().y_axis))
            });
        }
        fields.add_field_method_get("ZVector", |lua, this| {
            charge_host_nanos(lua, price::MEMBER);
            Ok(native_vector(this.orientation().z_axis))
        });
        for name in ["LookVector", "lookVector"] {
            fields.add_field_method_get(name, |lua, this| {
                charge_host_nanos(lua, price::MEMBER);
                Ok(native_vector(-this.orientation().z_axis))
            });
        }
        // (a C function in the methods table: see FuzzyEqMethod)
        fields.add_field("FuzzyEq", FuzzyEqMethod);
    }

    #[allow(clippy::too_many_lines)]
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // Methods, and the deprecated lowercase names Roblox still answers
        aliased!(methods, price::INVERSE, ["Inverse", "inverse"], |_, this, ()| Ok(this.inverse()));
        aliased!(
            methods,
            price::LERP,
            ["Lerp", "lerp"],
            |_, this, (goal, alpha): (LuaUserDataRef<CFrame>, f32)| {
                let (a, b) = (this.pos(), goal.pos());
                let position = [0, 1, 2].map(|i| f32m::lerp(a[i], b[i], alpha));
                Ok(CFrame::from_parts(position, f32m::lerp_rotation(&this.rot(), &goal.rot(), alpha)))
            }
        );
        methods.add_method("Orthonormalize", charged_method(price::ORTHONORMALIZE, |_, this, ()| {
            Ok(CFrame::from_parts(this.pos(), f32m::orthonormalize(&this.rot())))
        }));
        aliased!(
            methods,
            price::TO_WORLD,
            ["ToWorldSpace", "toWorldSpace"],
            |_, this, rhs: Variadic<LuaUserDataRef<CFrame>>| {
                Ok(rhs
                    .into_iter()
                    .map(|cf| *this * *cf)
                    .collect::<Variadic<_>>())
            }
        );
        aliased!(
            methods,
            price::TO_OBJECT,
            ["ToObjectSpace", "toObjectSpace"],
            |_, this, rhs: Variadic<LuaUserDataRef<CFrame>>| {
                let inverse = this.inverse();
                Ok(rhs
                    .into_iter()
                    .map(|cf| inverse * *cf)
                    .collect::<Variadic<_>>())
            }
        );
        aliased!(
            methods,
            price::POINT_TO_WORLD,
            ["PointToWorldSpace", "pointToWorldSpace"],
            |_, this, rhs: Variadic<VectorArg>| {
                Ok(rhs
                    .into_iter()
                    .map(|v3| native_vector(this.point_to_world(v3.0)))
                    .collect::<Variadic<_>>())
            }
        );
        aliased!(
            methods,
            price::POINT_TO_OBJECT,
            ["PointToObjectSpace", "pointToObjectSpace"],
            |_, this, rhs: Variadic<VectorArg>| {
                Ok(rhs
                    .into_iter()
                    .map(|v3| native_vector(this.point_to_object(v3.0)))
                    .collect::<Variadic<_>>())
            }
        );
        aliased!(
            methods,
            price::VECTOR_TO_WORLD,
            ["VectorToWorldSpace", "vectorToWorldSpace"],
            |_, this, rhs: Variadic<VectorArg>| {
                Ok(rhs
                    .into_iter()
                    .map(|v3| native_vector(this.vector_to_world(v3.0)))
                    .collect::<Variadic<_>>())
            }
        );
        aliased!(
            methods,
            price::VECTOR_TO_OBJECT,
            ["VectorToObjectSpace", "vectorToObjectSpace"],
            |_, this, rhs: Variadic<VectorArg>| {
                Ok(rhs
                    .into_iter()
                    .map(|v3| native_vector(this.vector_to_object(v3.0)))
                    .collect::<Variadic<_>>())
            }
        );
        #[rustfmt::skip]
        aliased!(methods, price::COMPONENTS, ["GetComponents", "components"], |_, this, ()| {
            let pos = this.position();
            let transposed = this.orientation().transpose();
            Ok((
                pos.x,               pos.y,                 pos.z,
                transposed.x_axis.x, transposed.x_axis.y,   transposed.x_axis.z,
                transposed.y_axis.x, transposed.y_axis.y,   transposed.y_axis.z,
                transposed.z_axis.x, transposed.z_axis.y,   transposed.z_axis.z,
            ))
        });
        aliased!(methods, price::EULER, ["ToEulerAnglesXYZ", "toEulerAnglesXYZ"], |_, this, ()| {
            Ok(f32m::to_euler("XYZ", &this.rot()))
        });
        // (and their lowercase names, measured in a client: ree's tools/parity/probes/camelcase.luau)
        aliased!(methods, price::EULER, ["ToEulerAnglesYXZ", "toEulerAnglesYXZ"], |_, this, ()| {
            Ok(f32m::to_euler("YXZ", &this.rot()))
        });
        aliased!(methods, price::EULER, ["ToOrientation", "toOrientation"], |_, this, ()| {
            Ok(f32m::to_euler("YXZ", &this.rot()))
        });
        // ToEulerAngles(order = Enum.RotationOrder.XYZ): the three angles in x, y, z order for the rotation order named
        aliased!(methods, price::EULER, ["ToEulerAngles", "toEulerAngles"], |_, this, order: Option<LuaValue>| {
            let name = rotation_order(order.as_ref(), "ToEulerAngles")?;
            Ok(f32m::to_euler(name, &this.rot()))
        });
        aliased!(methods, price::AXIS_ANGLE, ["ToAxisAngle", "toAxisAngle"], |_, this, ()| {
            let (axis, angle) = f32m::to_axis_angle(&this.rot());
            Ok((native_vector(Vec3::from_array(axis)), angle))
        });
        // Metamethods
        methods.add_meta_method(LuaMetaMethod::Eq, |lua, this, other: LuaValue| {
            charge_host_nanos(lua, price::EQ);
            userdata_impl_eq(lua, this, other)
        });
        methods.add_meta_method(LuaMetaMethod::ToString, userdata_impl_to_string);
        methods.add_meta_method(LuaMetaMethod::Mul, |lua, this, rhs: LuaValue| {
            match &rhs {
                LuaValue::Vector(v) => {
                    charge_host_nanos(lua, price::MUL_VECTOR);
                    return Ok(native_vector(
                        (*this * Vector3(Vec3::new(v.x(), v.y(), v.z()))).0,
                    ));
                }
                LuaValue::UserData(ud) => {
                    if let Ok(cf) = ud.borrow::<CFrame>() {
                        charge_host_nanos(lua, price::MUL_CFRAME);
                        return Ok(LuaValue::UserData(lua.create_userdata(*this * *cf)?));
                    } else if let Ok(vec) = ud.borrow::<Vector3>() {
                        charge_host_nanos(lua, price::MUL_VECTOR);
                        return Ok(native_vector((*this * *vec).0));
                    }
                }
                _ => {}
            }
            Err(LuaError::FromLuaConversionError {
                from: rhs.type_name(),
                to: "userdata".to_string(),
                message: Some(format!(
                    "Expected CFrame or Vector3, got {}",
                    rhs.type_name()
                )),
            })
        });
        methods.add_meta_method(
            LuaMetaMethod::Add,
            |lua, this, vec: VectorArg| {
                charge_host_nanos(lua, price::ADD_VECTOR);
                Ok(*this + Vector3(vec.0))
            },
        );
        methods.add_meta_method(
            LuaMetaMethod::Sub,
            |lua, this, vec: VectorArg| {
                charge_host_nanos(lua, price::ADD_VECTOR);
                Ok(*this - Vector3(vec.0))
            },
        );
    }
}

impl fmt::Display for CFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the twelve components in GetComponents' order, 9 significant digits each ("0, 0, 0, -4.37113883e-08, ...";
        // Studio 0.744)
        let pos = self.position();
        let transposed = self.orientation().transpose();
        let mut first = true;
        for value in [pos, transposed.x_axis, transposed.y_axis, transposed.z_axis].iter().flat_map(|v| [v.x, v.y, v.z]) {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            f.write_str(&super::format_g(f64::from(value), 9))?;
        }
        Ok(())
    }
}

impl ops::Mul for CFrame {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        let (p, r) = (self.pos(), self.rot());
        let rv = f32m::mat_vec(&r, rhs.pos());
        CFrame::from_parts([rv[0] + p[0], rv[1] + p[1], rv[2] + p[2]], f32m::mat_mul(&r, &rhs.rot()))
    }
}

impl ops::Mul<Vector3> for CFrame {
    type Output = Vector3;
    fn mul(self, rhs: Vector3) -> Self::Output {
        Vector3(self.point_to_world(rhs.0))
    }
}

impl ops::Add<Vector3> for CFrame {
    type Output = Self;
    fn add(self, rhs: Vector3) -> Self::Output {
        CFrame(Mat4::from_cols(
            self.0.x_axis,
            self.0.y_axis,
            self.0.z_axis,
            self.0.w_axis + rhs.0.extend(0.0),
        ))
    }
}

impl ops::Sub<Vector3> for CFrame {
    type Output = Self;
    fn sub(self, rhs: Vector3) -> Self::Output {
        CFrame(Mat4::from_cols(
            self.0.x_axis,
            self.0.y_axis,
            self.0.z_axis,
            self.0.w_axis - rhs.0.extend(0.0),
        ))
    }
}

impl From<DomCFrame> for CFrame {
    fn from(v: DomCFrame) -> Self {
        let transposed = v.orientation.transpose();
        CFrame(Mat4::from_cols(
            Vector3::from(transposed.x).0.extend(0.0),
            Vector3::from(transposed.y).0.extend(0.0),
            Vector3::from(transposed.z).0.extend(0.0),
            Vector3::from(v.position).0.extend(1.0),
        ))
    }
}

impl From<CFrame> for DomCFrame {
    fn from(v: CFrame) -> Self {
        let transposed = v.orientation().transpose();
        DomCFrame {
            position: DomVector3::from(Vector3(v.position())),
            orientation: DomMatrix3::new(
                DomVector3::from(Vector3(transposed.x_axis)),
                DomVector3::from(Vector3(transposed.y_axis)),
                DomVector3::from(Vector3(transposed.z_axis)),
            ),
        }
    }
}

/// CFrame.lookAt(from, to, up): at `from`, looking along `to - from` (types::float32 `look_along`; looking at itself
/// is not-a-number, as in Roblox)
fn look_at(from: Vec3, to: Vec3, up: Vec3) -> CFrame {
    CFrame::from_parts(array(from), f32m::look_along(array(to - from), array(up)))
}

#[cfg(test)]
mod cframe_test {
    use glam::{Mat4, Vec3};
    use rbx_dom_weak::types::{CFrame as DomCFrame, Matrix3 as DomMatrix3, Vector3 as DomVector3};

    use super::CFrame;

    #[test]
    fn dom_cframe_from_cframe() {
        let dom_cframe = DomCFrame::new(
            DomVector3::new(1.0, 2.0, 3.0),
            DomMatrix3::new(
                DomVector3::new(1.0, 2.0, 3.0),
                DomVector3::new(1.0, 2.0, 3.0),
                DomVector3::new(1.0, 2.0, 3.0),
            ),
        );

        let cframe = CFrame(Mat4::from_cols(
            Vec3::new(1.0, 1.0, 1.0).extend(0.0),
            Vec3::new(2.0, 2.0, 2.0).extend(0.0),
            Vec3::new(3.0, 3.0, 3.0).extend(0.0),
            Vec3::new(1.0, 2.0, 3.0).extend(1.0),
        ));

        assert_eq!(CFrame::from(dom_cframe), cframe);
    }

    #[test]
    fn cframe_from_dom_cframe() {
        let cframe = CFrame(Mat4::from_cols(
            Vec3::new(1.0, 2.0, 3.0).extend(0.0),
            Vec3::new(1.0, 2.0, 3.0).extend(0.0),
            Vec3::new(1.0, 2.0, 3.0).extend(0.0),
            Vec3::new(1.0, 2.0, 3.0).extend(1.0),
        ));

        let dom_cframe = DomCFrame::new(
            DomVector3::new(1.0, 2.0, 3.0),
            DomMatrix3::new(
                DomVector3::new(1.0, 1.0, 1.0),
                DomVector3::new(2.0, 2.0, 2.0),
                DomVector3::new(3.0, 3.0, 3.0),
            ),
        );

        assert_eq!(DomCFrame::from(cframe), dom_cframe);
    }
}

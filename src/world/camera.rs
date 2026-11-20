//! The Camera's projections: WorldToViewportPoint / WorldToScreenPoint and ViewportPointToRay /
//! ScreenPointToRay, from the camera's CFrame, its vertical FieldOfView and the ViewportSize the
//! Camera reports. A perspective camera as Roblox documents it; not Studio-measured. The screen forms
//! are measured from below GuiService's top inset, where GUI positions are (AbsolutePosition,
//! InputObject.Position): WorldToScreenPoint's Y is WorldToViewportPoint's less the inset, and
//! ScreenPointToRay(x, y) is ViewportPointToRay(x, y + inset).

use std::rc::Rc;

use glam::{Affine3A, Vec3};
use lune_roblox::datatypes::types as lune;
use mlua::prelude::*;
use rbx_dom_weak::types as dom;

use super::{cframe_name, frame, method, vector_arg};
use crate::{
    Engine, InstanceId,
    lua::{Args, Flow, raise},
};

struct View {
    transform: Affine3A,
    tan_y: f32,
    width: f32,
    height: f32,
}

fn view(engine: &Engine, camera: InstanceId) -> View {
    let state = engine.state.borrow();
    let transform = frame::affine_of(state.dom.raw_get(camera, cframe_name(engine)));
    let fov = engine.reflection.name_id("FieldOfView").and_then(|n| state.dom.number(camera, n)).unwrap_or(70.0);
    let width = engine.options.camera_width.unwrap_or(engine.options.viewport_width).max(1.0);
    let height = engine.options.camera_height.unwrap_or(engine.options.viewport_height).max(1.0);
    View { transform, tan_y: (fov.to_radians() / 2.0).tan() as f32, width: width as f32, height: height as f32 }
}

impl View {
    /// The point in viewport pixels (x right, y down) and its depth in studs in front of the camera,
    /// and whether it is in front of the camera and inside the viewport.
    fn project(&self, point: Vec3) -> (Vec3, bool) {
        let local = self.transform.inverse().transform_point3(point);
        let depth = -local.z;
        let aspect = self.width / self.height;
        let x = (local.x / (depth * self.tan_y * aspect) + 1.0) * 0.5 * self.width;
        let y = (1.0 - local.y / (depth * self.tan_y)) * 0.5 * self.height;
        let visible = depth > 0.0 && (0.0..=self.width).contains(&x) && (0.0..=self.height).contains(&y);
        (Vec3::new(x, y, depth), visible)
    }

    /// The unit ray through a viewport pixel, starting `depth` studs along it from the camera.
    fn ray(&self, x: f32, y: f32, depth: f32) -> (Vec3, Vec3) {
        let aspect = self.width / self.height;
        let local = Vec3::new((2.0 * x / self.width - 1.0) * self.tan_y * aspect, (1.0 - 2.0 * y / self.height) * self.tan_y, -1.0);
        let direction = self.transform.transform_vector3(local).normalize();
        (Vec3::from(self.transform.translation) + direction * depth, direction)
    }
}

fn point(at: Vec3, visible: bool) -> LuaResult<LuaMultiValue> {
    Ok(LuaMultiValue::from_vec(vec![LuaValue::Vector(mlua::Vector::new(at.x, at.y, at.z)), LuaValue::Boolean(visible)]))
}

fn ray(lua: &Lua, (origin, direction): (Vec3, Vec3)) -> LuaResult<LuaMultiValue> {
    let ray = lune::Ray::from(dom::Ray::new(dom::Vector3::new(origin.x, origin.y, origin.z), dom::Vector3::new(direction.x, direction.y, direction.z)));
    LuaValue::UserData(lua.create_userdata(ray)?).into_lua_multi(lua)
}

/// The world point argument of WorldTo*Point.
fn world_point(args: &Args) -> Result<Vec3, Flow> {
    match vector_arg(&args.get(1)) {
        Some(point) => Ok(point),
        None => raise("Argument 1 missing or nil (a Vector3)"),
    }
}

pub fn install(engine: &Rc<Engine>) {
    let Some(camera) = engine.reflection.class_id("Camera") else { return };
    let inset = engine.options.topbar_inset as f32;
    let mut behaviors = engine.behaviors.borrow_mut();
    let mut add = |member: &str, f: super::MethodFn| {
        if let Some(name) = engine.reflection.name_id(member) {
            behaviors.add_method(camera, name, f);
        }
    };
    add("WorldToViewportPoint", method(|_, engine, id, args| {
        let (at, visible) = view(engine, id).project(world_point(args)?);
        Ok(point(at, visible)?)
    }));
    add("WorldToScreenPoint", method(move |_, engine, id, args| {
        let (at, visible) = view(engine, id).project(world_point(args)?);
        Ok(point(at - Vec3::new(0.0, inset, 0.0), visible)?)
    }));
    add("ViewportPointToRay", method(|lua, engine, id, args| {
        let (x, y, depth) = (args.number(1).unwrap_or(0.0) as f32, args.number(2).unwrap_or(0.0) as f32, args.number(3).unwrap_or(0.0) as f32);
        Ok(ray(lua, view(engine, id).ray(x, y, depth))?)
    }));
    add("ScreenPointToRay", method(move |lua, engine, id, args| {
        let (x, y, depth) = (args.number(1).unwrap_or(0.0) as f32, args.number(2).unwrap_or(0.0) as f32, args.number(3).unwrap_or(0.0) as f32);
        Ok(ray(lua, view(engine, id).ray(x, y + inset, depth))?)
    }));
}

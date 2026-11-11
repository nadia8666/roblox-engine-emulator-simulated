//! Path2D: a list of control points drawn as a stroked curve over its parent, and the questions a script asks of it.
//!
//! Measured in a playing client (Studio 0.744) and in Studio 0.747 (tools/parity/probes/path2d.luau and
//! path2d_layout.luau; docs/engine.md "Path2D"):
//!
//! - The points are the hidden property `PropertiesSerialize` ([`encode`]: what a model file stores), so a clone, a
//!   model file and a recording carry them; a script reaches them through the methods only.
//! - Behind the list sits the spline the engine draws, and it trails the list: SetControlPoints does not touch it,
//!   Insert / Update / Remove edit the list and then the spline, and raise when the spline is shorter than the edit
//!   needs (the list keeps the edit). A curve query, and the layout of a path that is on screen ([`laid_out`], a
//!   host's), bring it up to date. Only its length is kept here ([`Paths::spline`]).
//! - A path resolves against its parent, a GuiObject or a LayerCollector ([`Frame`]): positions against the parent's
//!   rectangle, tangents as sizes. The core has no layout: a parent measures nothing there (a ScreenGui or SurfaceGui
//!   800 by 600, as one outside the DataModel does in Roblox), and a host with a layout answers for the parents it
//!   lays out ([`set_frame_source`]).
//! - The curve ([`Curve`]) is computed in float32 as Roblox computes it; the formulas are the ones whose results match
//!   the captured digits.

use std::{cell::RefCell, rc::Rc};

use mlua::prelude::*;
use rbx_dom_weak::types as dom;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    ClassId, Engine, Extension, InstanceId, NameId, Reflection, State,
    instance as core,
    lua::{Args, Flow, convert, instance::set_property, none, raise, typeof_value, values},
    services::{Behaviors, getter, method},
    value::{Str, Value},
};

/// Path2D:GetMaxControlPoints
pub const MAX_CONTROL_POINTS: usize = 100;

/// A Path2DControlPoint: where the curve passes, and the handles of the segments before and after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPoint {
    pub position: dom::UDim2,
    pub left: dom::UDim2,
    pub right: dom::UDim2,
}

impl Default for ControlPoint {
    fn default() -> Self {
        let zero = dom::UDim2::new(dom::UDim::new(0.0, 0), dom::UDim::new(0.0, 0));
        ControlPoint { position: zero, left: zero, right: zero }
    }
}

/// The byte that starts each point of [`encode`].
const POINT_MARK: u8 = 0x23;
const POINT_BYTES: usize = 49;

/// `PropertiesSerialize`: the count (u32), then for each point a mark byte and Position, LeftTangent and RightTangent
/// as X.Scale (f32), X.Offset (i32), Y.Scale, Y.Offset, little endian. No points: four zero bytes.
pub fn encode(points: &[ControlPoint]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + points.len() * POINT_BYTES);
    bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for point in points {
        bytes.push(POINT_MARK);
        for value in [point.position, point.left, point.right] {
            for axis in [value.x, value.y] {
                bytes.extend_from_slice(&axis.scale.to_le_bytes());
                bytes.extend_from_slice(&axis.offset.to_le_bytes());
            }
        }
    }
    bytes
}

/// The points of [`encode`]'s bytes; what does not parse (a cut or foreign value) holds no points past it.
pub fn decode(bytes: &[u8]) -> Vec<ControlPoint> {
    let Some(count) = bytes.get(..4).map(|head| u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize) else {
        return Vec::new();
    };
    let mut points = Vec::with_capacity(count.min(MAX_CONTROL_POINTS));
    for chunk in bytes[4..].as_chunks::<POINT_BYTES>().0.iter().take(count) {
        if chunk[0] != POINT_MARK {
            break;
        }
        let word = |index: usize| [chunk[1 + index * 4], chunk[2 + index * 4], chunk[3 + index * 4], chunk[4 + index * 4]];
        let value = |index: usize| {
            dom::UDim2::new(
                dom::UDim::new(f32::from_le_bytes(word(index)), i32::from_le_bytes(word(index + 1))),
                dom::UDim::new(f32::from_le_bytes(word(index + 2)), i32::from_le_bytes(word(index + 3))),
            )
        };
        points.push(ControlPoint { position: value(0), left: value(4), right: value(8) });
    }
    points
}

pub type Point = [f32; 2];

/// What a path resolves against: its parent's rectangle on the screen (in GUI pixels, the top bar's inset included)
/// and how the parent is turned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The rectangle's top left corner, before `transform`
    pub origin: Point,
    pub size: Point,
    /// Where a point of the unturned rectangle shows: `[a, b, c, d, x, y]` maps `(px, py)` to
    /// `(a px + c py + x, b px + d py + y)` (a rotated parent, and its rotated ancestors). None: where it is.
    pub transform: Option<[f32; 6]>,
}

impl Frame {
    /// The rectangle at `position` of `size`; a negative size (a GuiObject's AbsoluteSize can be) is the same
    /// rectangle from its least corner.
    pub fn new(position: Point, size: Point) -> Frame {
        let mut frame = Frame { origin: position, size, transform: None };
        for axis in 0..2 {
            if size[axis] < 0.0 {
                frame.origin[axis] = position[axis] + size[axis];
                frame.size[axis] = -size[axis];
            }
        }
        frame
    }

    pub fn turned(self, transform: [f32; 6]) -> Frame {
        Frame { transform: Some(transform), ..self }
    }

    fn show(&self, point: Point) -> Point {
        match self.transform {
            Some([a, b, c, d, x, y]) => [a * point[0] + c * point[1] + x, b * point[0] + d * point[1] + y],
            None => point,
        }
    }

    /// [`Self::show`] undone: where in the unturned rectangle a point of the screen is.
    fn place(&self, point: Point) -> Point {
        match self.transform {
            Some([a, b, c, d, x, y]) => {
                let determinant = a * d - b * c;
                if determinant == 0.0 {
                    return point;
                }
                let (px, py) = (point[0] - x, point[1] - y);
                [(d * px - c * py) / determinant, (a * py - b * px) / determinant]
            }
            None => point,
        }
    }
}

/// One piece of the curve between two control points, in pixels. Which kind follows the handles as they resolve:
/// neither (both zero pixels long) a line, one a quadratic through it, both a cubic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    Line(Point, Point),
    Quadratic(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

fn distance(from: Point, to: Point) -> f32 {
    let (x, y) = (to[0] - from[0], to[1] - from[1]);
    (x * x + y * y).sqrt()
}

/// GetLength stops halving a piece when its two halves' chords are this much longer than its own.
const LENGTH_TOLERANCE: f32 = 1e-5;
/// How often a piece is halved at most (Roblox's limit is not known; a curve of ordinary size stops well before).
const LENGTH_DEPTH: u32 = 18;
/// The steps of the walk that finds a parameter by length.
const ARC_STEPS: u32 = 32;

impl Segment {
    pub fn start(&self) -> Point {
        match *self {
            Segment::Line(start, _) | Segment::Quadratic(start, _, _) | Segment::Cubic(start, _, _, _) => start,
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Segment::Line(_, end) | Segment::Quadratic(_, _, end) | Segment::Cubic(_, _, _, end) => end,
        }
    }

    /// The polynomial's coefficients on one axis, highest power first (position `((a t + b) t + c) t + start`).
    fn coefficients(&self, axis: usize) -> (f32, f32, f32) {
        match *self {
            Segment::Line(start, end) => (0.0, 0.0, end[axis] - start[axis]),
            Segment::Quadratic(start, control, end) => {
                let (p0, p1, p2) = (start[axis], control[axis], end[axis]);
                (0.0, p0 - 2.0 * p1 + p2, 2.0 * (p1 - p0))
            }
            Segment::Cubic(start, first, second, end) => {
                let (p0, p1, p2, p3) = (start[axis], first[axis], second[axis], end[axis]);
                ((p3 - p0) + 3.0 * (p1 - p2), 3.0 * p0 - 6.0 * p1 + 3.0 * p2, 3.0 * p1 - 3.0 * p0)
            }
        }
    }

    pub fn position(&self, t: f32) -> Point {
        let start = self.start();
        let axis = |axis: usize| {
            let (a, b, c) = self.coefficients(axis);
            ((a * t + b) * t + c) * t + start[axis]
        };
        [axis(0), axis(1)]
    }

    /// The derivative by the segment's own parameter (a line's: its end less its start). A cubic's multiplies 3
    /// by `t` before the coefficient: the order sets the answer's last float32 digit, and this one is the client's.
    pub fn tangent(&self, t: f32) -> Point {
        let axis = |axis: usize| {
            let (a, b, c) = self.coefficients(axis);
            match self {
                Segment::Cubic(..) => (3.0 * t * a + 2.0 * b) * t + c,
                _ => 2.0 * b * t + c,
            }
        };
        [axis(0), axis(1)]
    }

    /// The derivative's length as the walk by length takes it: a cubic's with 3 times the coefficient first, which
    /// is not [`Self::tangent`]'s order (measured: the client's answers by length need this one inside the walk
    /// and the other for the tangent it reports).
    fn speed(&self, t: f32) -> f32 {
        let axis = |axis: usize| {
            let (a, b, c) = self.coefficients(axis);
            match self {
                Segment::Cubic(..) => (3.0 * a * t + 2.0 * b) * t + c,
                _ => 2.0 * b * t + c,
            }
        };
        let (x, y) = (axis(0), axis(1));
        (x * x + y * y).sqrt()
    }

    /// Where the segment is at `t`, from its start: [`Self::position`] before the start is added.
    fn reach(&self, t: f32) -> Point {
        let axis = |axis: usize| {
            let (a, b, c) = self.coefficients(axis);
            ((a * t + b) * t + c) * t
        };
        [axis(0), axis(1)]
    }

    pub fn length(&self) -> f32 {
        match *self {
            Segment::Line(start, _) => distance(start, self.position(1.0)),
            _ => self.span(0.0, 1.0, self.reach(0.0), self.reach(1.0), 0),
        }
    }

    /// A piece's length: its two halves' chords when they are no longer than its own chord by the tolerance,
    /// else the halves' lengths. (A curve whose midpoint lies on its chord stops at once: an S between two points
    /// measures as the two chords to its middle.) The points are taken from the segment's start, not from the
    /// screen's corner: float32 is finer near zero, and where a curve turns back on itself the client's length
    /// shows that fineness (measured on 1160 single segments, all equal to the last digit).
    fn span(&self, from: f32, to: f32, start: Point, end: Point, depth: u32) -> f32 {
        let middle_at = (from + to) * 0.5;
        let middle = self.reach(middle_at);
        let halves = distance(start, middle) + distance(middle, end);
        let excess = halves - distance(start, end);
        if excess <= LENGTH_TOLERANCE || !excess.is_finite() || depth >= LENGTH_DEPTH {
            return halves;
        }
        self.span(from, middle_at, start, middle, depth + 1) + self.span(middle_at, to, middle, end, depth + 1)
    }

    /// The parameter at which the segment is `length` long from its start, as the client finds it. A line's is
    /// the share of its length. A curve's is walked: 32 classical Runge-Kutta steps of `length / 32` each along
    /// the parameter's rate by length (one over the derivative's length), all in float32 and in this order of
    /// operations. The walk is not held to the segment: near a tight bend it stops short of the end or runs past
    /// it (a parameter over 1), and so does the client.
    fn parameter_at(&self, length: f32, whole: f32) -> f32 {
        if matches!(self, Segment::Line(..)) {
            return if whole > 0.0 { length / whole } else { 0.0 };
        }
        let step = length / ARC_STEPS as f32;
        let rate = |at: f32| step / self.speed(at);
        let mut at = 0.0f32;
        for _ in 0..ARC_STEPS {
            let first = rate(at);
            let second = rate(at + first * 0.5);
            let third = rate(at + second * 0.5);
            let fourth = rate(at + third);
            at += (first + 2.0 * (second + third) + fourth) * (1.0 / 6.0);
        }
        at
    }

    /// Grows `least` / `most` to hold the segment: its ends, and where its derivative is zero inside it. A cubic
    /// whose derivative has no square term on an axis gets no inner point there (measured: a symmetric arch reports
    /// its ends only). The inner point's value is summed in the client's order, which sets its last float32
    /// digits: a quadratic's nested by `1 - t`, a cubic's by `t` (measured on 1471 quadratic and 1649 cubic
    /// bounds, all equal).
    fn bounds(&self, least: &mut Point, most: &mut Point) {
        let mut hold = |axis: usize, value: f32| {
            least[axis] = least[axis].min(value);
            most[axis] = most[axis].max(value);
        };
        for axis in 0..2 {
            hold(axis, self.start()[axis]);
            hold(axis, self.end()[axis]);
            match *self {
                Segment::Line(..) => {}
                Segment::Quadratic(start, control, end) => {
                    let (p0, p1, p2) = (start[axis], control[axis], end[axis]);
                    let t = (p0 - p1) / (p0 - 2.0 * p1 + p2);
                    if t > 0.0 && t < 1.0 {
                        let s = 1.0 - t;
                        hold(axis, s * (s * p0 + 2.0 * t * p1) + t * t * p2);
                    }
                }
                Segment::Cubic(start, first, second, end) => {
                    let (p0, p1, p2, p3) = (start[axis], first[axis], second[axis], end[axis]);
                    let a = (p3 - p0) + 3.0 * (p1 - p2);
                    let b = p0 - 2.0 * p1 + p2;
                    let c = p1 - p0;
                    let root = b * b - a * c;
                    if a != 0.0 && root > 0.0 {
                        let root = root.sqrt();
                        for t in [(-b - root) / a, (-b + root) / a] {
                            if t > 0.0 && t < 1.0 {
                                let s = 1.0 - t;
                                hold(axis, s * s * s * p0 + t * (s * s * (3.0 * p1) + t * (s * (3.0 * p2) + t * p3)));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A path's curve on the screen: its segments in GUI pixels, as its parent shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub segments: Vec<Segment>,
    pub frame: Frame,
}

impl Curve {
    /// The curve of `points` under a parent at `frame`. Segments join consecutive points; a closed path of three
    /// points or more adds the last to the first. Offsets are pixels whatever scales the parent (a UIScale does not
    /// reach them).
    pub fn new(points: &[ControlPoint], closed: bool, frame: &Frame) -> Curve {
        let size = |value: dom::UDim2| [value.x.scale * frame.size[0] + value.x.offset as f32, value.y.scale * frame.size[1] + value.y.offset as f32];
        let count = match points.len() {
            0 | 1 => 0,
            2 => 1,
            length if closed => length,
            length => length - 1,
        };
        let segments = (0..count)
            .map(|index| {
                let (from, to) = (&points[index], &points[(index + 1) % points.len()]);
                let place = |point: &ControlPoint| {
                    let within = size(point.position);
                    [frame.origin[0] + within[0], frame.origin[1] + within[1]]
                };
                let (start, end) = (place(from), place(to));
                let (right, left) = (size(from.right), size(to.left));
                let first = [start[0] + right[0], start[1] + right[1]];
                let second = [end[0] + left[0], end[1] + left[1]];
                let (start, first, second, end) = (frame.show(start), frame.show(first), frame.show(second), frame.show(end));
                match (right != [0.0, 0.0], left != [0.0, 0.0]) {
                    (false, false) => Segment::Line(start, end),
                    (true, false) => Segment::Quadratic(start, first, end),
                    (false, true) => Segment::Quadratic(start, second, end),
                    (true, true) => Segment::Cubic(start, first, second, end),
                }
            })
            .collect();
        Curve { segments, frame: *frame }
    }

    /// The segment and its parameter at `t` of the whole path (each segment takes an equal share; a joint is the
    /// next segment's start). `t` outside 0..1 is the nearer end, NaN the start.
    pub fn locate(&self, t: f32) -> Option<(usize, f32)> {
        let count = self.segments.len();
        if count == 0 {
            return None;
        }
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let along = t * count as f32;
        let index = (along.floor() as usize).min(count - 1);
        Some((index, along - index as f32))
    }

    /// [`Self::locate`] by length, as the client does it: `t` (held to 0..1) times the path's length is what is
    /// left to go; each segment in turn takes it when it is no longer than the segment, else the segment's length
    /// comes off it. Float32 throughout, so at `t` = 1 the last segment can be left with a hair more than its
    /// length: then no segment takes it and the answer is the path's start (measured on three straight segments).
    /// NaN stays NaN.
    pub fn locate_by_length(&self, t: f32) -> Option<(usize, f32)> {
        if self.segments.is_empty() {
            return None;
        }
        let lengths: Vec<f32> = self.segments.iter().map(Segment::length).collect();
        let mut left = t.clamp(0.0, 1.0) * sum(&lengths);
        if left.is_nan() {
            return Some((0, f32::NAN));
        }
        for (index, &length) in lengths.iter().enumerate() {
            if left <= length {
                return Some((index, self.segments[index].parameter_at(left, length)));
            }
            left -= length;
        }
        Some((0, 0.0))
    }

    pub fn length(&self) -> f32 {
        let lengths: Vec<f32> = self.segments.iter().map(Segment::length).collect();
        sum(&lengths)
    }

    /// The least and greatest corner of the segments (None without one).
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let first = self.segments.first()?.start();
        let (mut least, mut most) = (first, first);
        for segment in &self.segments {
            segment.bounds(&mut least, &mut most);
        }
        Some((least, most))
    }

    /// A point of the screen as GetPositionOnCurve reports it: in the parent's own rectangle (a turned parent's
    /// unturned), as scale; a parent with no size on an axis gets the pixels as offsets.
    pub fn report(&self, point: Point) -> dom::UDim2 {
        let [x, y] = self.frame.place(point);
        let Frame { origin, size, .. } = self.frame;
        if size[0] == 0.0 || size[1] == 0.0 {
            dom::UDim2::new(dom::UDim::new(0.0, x as i32), dom::UDim::new(0.0, y as i32))
        } else {
            dom::UDim2::new(dom::UDim::new((x - origin[0]) / size[0], 0), dom::UDim::new((y - origin[1]) / size[1], 0))
        }
    }
}

/// How far GetBoundingRect reaches past the curve on every side.
pub fn margin(thickness: f32) -> f32 {
    if thickness > 0.0 { (thickness + 1.0) / 2.0 } else { 0.0 }
}

fn sum(lengths: &[f32]) -> f32 {
    lengths.iter().fold(0.0, |total, length| total + length)
}

/// A host's answer to where a path's parent is (None: the core's own, [`static_frame`]).
pub type FrameSource = Rc<dyn Fn(&Rc<Engine>, InstanceId) -> Option<Frame>>;

struct Names {
    points: NameId,
    closed: NameId,
    thickness: NameId,
    changed: NameId,
    gui_object: ClassId,
    layer_collector: ClassId,
    /// ScreenGui and SurfaceGui: the collectors that measure 800 by 600 with no screen to sit on
    sized_collectors: [Option<ClassId>; 2],
}

#[derive(Default)]
struct Paths {
    names: RefCell<Option<Rc<Names>>>,
    /// How many points the spline behind each path holds (absent: none)
    spline: RefCell<FxHashMap<InstanceId, usize>>,
    /// The paths that have a parent
    parented: RefCell<FxHashSet<InstanceId>>,
    frames: RefCell<Option<FrameSource>>,
}

impl Extension for Paths {
    fn teardown(&self) {
        *self.frames.borrow_mut() = None;
    }
}

fn paths(engine: &Engine) -> &Paths {
    engine.extension_or_insert_with(Paths::default)
}

fn names(engine: &Engine) -> Option<Rc<Names>> {
    paths(engine).names.borrow().clone()
}

/// A host with a layout answers where the parents it lays out are. Roblox resolves a path against its parent as the
/// last frame laid it out (a resize reaches the queries with the next frame): the host's answer is that rectangle.
pub fn set_frame_source(engine: &Engine, source: FrameSource) {
    *paths(engine).frames.borrow_mut() = Some(source);
}

/// A host's layout laid out `path`'s parent with the path on screen: its spline now holds its points.
pub fn laid_out(engine: &Engine, path: InstanceId) {
    let count = control_points(&engine.state.borrow(), &engine.reflection, path).len();
    paths(engine).spline.borrow_mut().insert(path, count);
}

/// Every Path2D that has a parent, in no order (a host picks the ones its layout shows).
pub fn parented(engine: &Engine) -> Vec<InstanceId> {
    paths(engine).parented.borrow().iter().copied().collect()
}

/// A path's control points.
pub fn control_points(state: &State, reflection: &Reflection, path: InstanceId) -> Vec<ControlPoint> {
    let Some(name) = reflection.name_id("PropertiesSerialize") else { return Vec::new() };
    match state.dom.raw_get(path, name) {
        Some(Value::String(bytes)) => decode(bytes.as_bytes()),
        _ => Vec::new(),
    }
}

/// Path2D.Transparency: a model file's value (scripts can neither read nor write it), 0 without one. Studio 0.747
/// keeps and saves the value and draws the path opaque whatever it is, so nothing here draws with it.
pub fn transparency(state: &State, reflection: &Reflection, path: InstanceId) -> f32 {
    reflection.name_id("Transparency").and_then(|name| state.dom.number(path, name)).unwrap_or(0.0) as f32
}

/// Whether `parent` can hold a path: a GuiObject or a LayerCollector, in the DataModel or not.
pub fn valid_parent(engine: &Engine, parent: Option<InstanceId>) -> bool {
    let (Some(names), Some(parent)) = (names(engine), parent) else { return false };
    let class = engine.state.borrow().dom.at(parent).class;
    let class = engine.reflection.class(class);
    class.is_a(names.gui_object) || class.is_a(names.layer_collector)
}

/// Where a parent is with no layout to ask: nowhere and of no size; a ScreenGui or SurfaceGui 800 by 600.
pub fn static_frame(engine: &Engine, parent: InstanceId) -> Frame {
    let sized = names(engine).is_some_and(|names| {
        let class = engine.state.borrow().dom.at(parent).class;
        names.sized_collectors.iter().flatten().any(|&collector| engine.reflection.class(class).is_a(collector))
    });
    Frame::new([0.0, 0.0], if sized { [800.0, 600.0] } else { [0.0, 0.0] })
}

/// The curve of a path under a usable parent, as its queries see it (None: no such parent).
pub fn curve(engine: &Rc<Engine>, path: InstanceId) -> Option<Curve> {
    let names = names(engine)?;
    let parent = engine.state.borrow().dom.parent_of(path);
    if !valid_parent(engine, parent) {
        return None;
    }
    let parent = parent?;
    let source = paths(engine).frames.borrow().clone();
    let frame = source.and_then(|source| source(engine, parent)).unwrap_or_else(|| static_frame(engine, parent));
    let state = engine.state.borrow();
    let points = control_points(&state, &engine.reflection, path);
    let closed = matches!(state.dom.raw_get(path, names.closed), Some(Value::Bool(true)));
    Some(Curve::new(&points, closed, &frame))
}

const INVALID_PARENT: &str = "Attempting to use Path2D with invalid parent";

/// A value's name in a failed cast ("Unable to cast double to Array").
fn cast_name(engine: &Engine, value: &LuaValue) -> String {
    match value {
        LuaValue::Number(_) | LuaValue::Integer(_) => "double".into(),
        LuaValue::Boolean(_) => "bool".into(),
        LuaValue::UserData(_) if crate::lua::raw::instance_id(value).is_some() => "Object".into(),
        other => typeof_value(engine, other),
    }
}

/// An index argument as Roblox casts a value to int: a number rounds half away from zero (what is not an int32,
/// infinity and NaN included, is the least one), a string reads as a number, true is 1.
fn index_arg(lua: &Lua, engine: &Engine, args: &Args, at: usize) -> Result<i32, Flow> {
    let number = match args.get(at) {
        LuaValue::Nil => return raise(format!("Argument {at} missing or nil")),
        LuaValue::Number(number) => *number,
        LuaValue::Integer(number) => *number as f64,
        LuaValue::Boolean(on) => *on as u8 as f64,
        value @ LuaValue::String(_) => match lua.coerce_number(value.clone())? {
            Some(number) => number,
            None => return raise("Unable to cast string to int"),
        },
        other => return raise(format!("Unable to cast {} to int", cast_name(engine, other))),
    };
    let rounded = number.round();
    Ok(if rounded >= i32::MIN as f64 && rounded <= i32::MAX as f64 { rounded as i32 } else { i32::MIN })
}

fn float_arg(lua: &Lua, engine: &Engine, args: &Args, at: usize) -> Result<f32, Flow> {
    match args.get(at) {
        LuaValue::Nil => raise(format!("Argument {at} missing or nil")),
        LuaValue::Number(number) => Ok(*number as f32),
        LuaValue::Integer(number) => Ok(*number as f32),
        value @ LuaValue::String(_) => match lua.coerce_number(value.clone())? {
            Some(number) => Ok(number as f32),
            None => raise("Unable to cast string to float"),
        },
        other => raise(format!("Unable to cast {} to float", cast_name(engine, other))),
    }
}

/// A script's Path2DControlPoint (the prelude keeps its fields in `Path2DPoints`).
fn point_of(engine: &Engine, value: &LuaValue) -> Option<ControlPoint> {
    if !matches!(value, LuaValue::UserData(_)) {
        return None;
    }
    let stored: LuaTable = engine.prelude().exports.raw_get("Path2DPoints").ok()?;
    let fields: LuaTable = stored.raw_get(value.clone()).ok()?;
    let field = |name: &str| match convert::from_lua(&fields.raw_get::<LuaValue>(name).ok()?) {
        Value::UDim2(value) => Some(value),
        _ => None,
    };
    Some(ControlPoint { position: field("Position")?, left: field("LeftTangent")?, right: field("RightTangent")? })
}

fn point_arg(engine: &Engine, args: &Args, at: usize) -> Result<ControlPoint, Flow> {
    match args.get(at) {
        LuaValue::Nil => raise(format!("Argument {at} missing or nil")),
        value => match point_of(engine, value) {
            Some(point) => Ok(point),
            None => raise(format!("Unable to cast {} to Path2DControlPoint", cast_name(engine, value))),
        },
    }
}

fn point_value(lua: &Lua, engine: &Engine, point: &ControlPoint) -> LuaResult<LuaValue> {
    let class: LuaTable = engine.prelude().exports.raw_get("Path2DControlPoint")?;
    let new: LuaFunction = class.raw_get("new")?;
    let field = |value: dom::UDim2| convert::to_lua(lua, engine, &Value::UDim2(value));
    new.call((field(point.position)?, field(point.left)?, field(point.right)?))
}

fn points_of(engine: &Engine, path: InstanceId) -> Vec<ControlPoint> {
    control_points(&engine.state.borrow(), &engine.reflection, path)
}

fn store(engine: &Engine, names: &Names, path: InstanceId, points: &[ControlPoint]) {
    let hooks = engine.hooks.borrow();
    let mut state = engine.state.borrow_mut();
    core::raw_set_unsignalled(&mut state, &engine.reflection, &hooks, path, names.points, Value::String(Str::new(&encode(points))));
}

fn changed(engine: &Engine, names: &Names, path: InstanceId) {
    core::fire_event(&mut engine.state.borrow_mut(), path, names.changed, Vec::new());
}

fn spline(engine: &Engine, path: InstanceId) -> usize {
    paths(engine).spline.borrow().get(&path).copied().unwrap_or(0)
}

fn set_spline(engine: &Engine, path: InstanceId, count: usize) {
    paths(engine).spline.borrow_mut().insert(path, count);
}

fn out_of_bounds<T>(method: &str, index: i32) -> Result<T, Flow> {
    raise(format!("Path2D:{method}() at index {index} access out of bounds"))
}

fn spline_error<T>(action: &str, index: usize) -> Result<T, Flow> {
    raise(format!("Spline {action} control point at index {index} access out of bounds"))
}

fn has_parent(engine: &Engine, path: InstanceId) -> bool {
    let parent = engine.state.borrow().dom.parent_of(path);
    valid_parent(engine, parent)
}

/// What a query computes on: the curve, after the checks every query makes in this order (a position or tangent
/// query: a path of no segment; then the parent), with the spline brought up to date.
fn queried(engine: &Rc<Engine>, path: InstanceId, segments_for: Option<&str>) -> Result<Curve, Flow> {
    let count = points_of(engine, path).len();
    if let Some(method) = segments_for
        && count < 2
    {
        return raise(format!("Path2D:{method}() attempting to query with zero segments"));
    }
    let Some(curve) = curve(engine, path) else { return raise(INVALID_PARENT) };
    set_spline(engine, path, count);
    Ok(curve)
}

fn vector(point: Point) -> Value {
    Value::Vector2(dom::Vector2::new(point[0], point[1]))
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let (Some(class), Some(points), Some(closed), Some(thickness), Some(transparency), Some(changed_event), Some(gui_object), Some(layer_collector)) = (
        reflection.class_id("Path2D"),
        reflection.name_id("PropertiesSerialize"),
        reflection.name_id("Closed"),
        reflection.name_id("Thickness"),
        reflection.name_id("Transparency"),
        reflection.name_id("ControlPointChanged"),
        reflection.class_id("GuiObject"),
        reflection.class_id("LayerCollector"),
    ) else {
        return;
    };
    let shared = Rc::new(Names {
        points,
        closed,
        thickness,
        changed: changed_event,
        gui_object,
        layer_collector,
        sized_collectors: [reflection.class_id("ScreenGui"), reflection.class_id("SurfaceGui")],
    });
    *paths(engine).names.borrow_mut() = Some(shared.clone());

    {
        let mut hooks = engine.hooks.borrow_mut();
        let weak = Rc::downgrade(engine);
        hooks.instance_freed.push(Box::new(move |_, id| {
            let Some(engine) = weak.upgrade() else { return };
            let paths = paths(&engine);
            paths.spline.borrow_mut().remove(&id);
            paths.parented.borrow_mut().remove(&id);
        }));
        let weak = Rc::downgrade(engine);
        hooks.hierarchy_changed.push(Box::new(move |state, id, _, parent| {
            if state.dom.at(id).class != class {
                return;
            }
            let Some(engine) = weak.upgrade() else { return };
            let mut parented = paths(&engine).parented.borrow_mut();
            if parent.is_some() {
                parented.insert(id);
            } else {
                parented.remove(&id);
            }
        }));
    }

    let mut add = |name: &str, function: crate::services::MethodFn| {
        if let Some(name) = reflection.name_id(name) {
            behaviors.add_method(class, name, function);
        }
    };

    add("GetMaxControlPoints", method(|_, _, _, _| Ok(values([LuaValue::Number(MAX_CONTROL_POINTS as f64)]))));
    add("GetSegmentCount", method(|_, _, _, _| raise("The current thread cannot call 'GetSegmentCount' (lacking capability RobloxScript)")));

    add("GetControlPoints", method(|lua, engine, path, _| {
        let list = lua.create_table()?;
        for (index, point) in points_of(engine, path).iter().enumerate() {
            list.raw_set(index + 1, point_value(lua, engine, point)?)?;
        }
        Ok(values([LuaValue::Table(list)]))
    }));
    add("GetControlPoint", method(|lua, engine, path, args| {
        let index = index_arg(lua, engine, args, 1)?;
        let points = points_of(engine, path);
        if index < 1 || index as usize > points.len() {
            return out_of_bounds("GetControlPoint", index);
        }
        Ok(values([point_value(lua, engine, &points[index as usize - 1])?]))
    }));

    // SetControlPoints: the list replaces the points and the spline is left as it was. A list over the limit keeps
    // its first hundred and raises.
    let names = shared.clone();
    add("SetControlPoints", method(move |_, engine, path, args| {
        let list = match args.get(1) {
            LuaValue::Nil => return raise("Argument 1 missing or nil"),
            LuaValue::Table(list) if list.raw_len() > 0 || list.clone().pairs::<LuaValue, LuaValue>().next().is_none() => list.clone(),
            LuaValue::Table(_) => return raise("Unable to cast Dictionary to Array"),
            other => return raise(format!("Unable to cast {} to Array", cast_name(engine, other))),
        };
        let mut points = Vec::with_capacity(list.raw_len());
        for index in 1..=list.raw_len() {
            let Some(point) = point_of(engine, &list.raw_get::<LuaValue>(index)?) else { return raise("Variant cast failed") };
            points.push(point);
        }
        let over = points.len() > MAX_CONTROL_POINTS;
        points.truncate(MAX_CONTROL_POINTS);
        store(engine, &names, path, &points);
        if over {
            return raise(format!("Path2D:SetControlPoints() only supports {MAX_CONTROL_POINTS} control points."));
        }
        changed(engine, &names, path);
        Ok(none())
    }));

    // The edits check in this order: the index, the limit (insert), the parent (insert, update); then the list
    // changes, then the spline, which raises when it is too short for the edit (no ControlPointChanged then).
    let names = shared.clone();
    add("InsertControlPoint", method(move |lua, engine, path, args| {
        let index = index_arg(lua, engine, args, 1)?;
        let point = point_arg(engine, args, 2)?;
        let mut points = points_of(engine, path);
        if index < 1 || index as usize > points.len() + 1 {
            return out_of_bounds("InsertControlPoint", index);
        }
        if points.len() >= MAX_CONTROL_POINTS {
            return raise(format!("Path2D:InsertControlPoint() only supports {MAX_CONTROL_POINTS} control points."));
        }
        if !has_parent(engine, path) {
            return raise(INVALID_PARENT);
        }
        let at = index as usize - 1;
        points.insert(at, point);
        store(engine, &names, path, &points);
        let held = spline(engine, path);
        if at > held {
            return spline_error("inserting", at);
        }
        set_spline(engine, path, held + 1);
        changed(engine, &names, path);
        Ok(none())
    }));
    let names = shared.clone();
    add("UpdateControlPoint", method(move |lua, engine, path, args| {
        let index = index_arg(lua, engine, args, 1)?;
        let point = point_arg(engine, args, 2)?;
        let mut points = points_of(engine, path);
        if index < 1 || index as usize > points.len() {
            return out_of_bounds("UpdateControlPoint", index);
        }
        if !has_parent(engine, path) {
            return raise(INVALID_PARENT);
        }
        let at = index as usize - 1;
        points[at] = point;
        store(engine, &names, path, &points);
        if at >= spline(engine, path) {
            return spline_error("updating", at);
        }
        changed(engine, &names, path);
        Ok(none())
    }));
    let names = shared.clone();
    add("RemoveControlPoint", method(move |lua, engine, path, args| {
        let index = index_arg(lua, engine, args, 1)?;
        let mut points = points_of(engine, path);
        if index < 1 || index as usize > points.len() {
            return out_of_bounds("RemoveControlPoint", index);
        }
        let at = index as usize - 1;
        points.remove(at);
        store(engine, &names, path, &points);
        let held = spline(engine, path);
        if at >= held {
            return spline_error("removing", at);
        }
        set_spline(engine, path, held - 1);
        changed(engine, &names, path);
        Ok(none())
    }));

    // The queries. A path without points answers GetLength and GetBoundingRect before its parent is looked at.
    add("GetLength", method(|_, engine, path, _| {
        if points_of(engine, path).is_empty() {
            return Ok(values([LuaValue::Number(0.0)]));
        }
        let curve = queried(engine, path, None)?;
        Ok(values([LuaValue::Number(curve.length() as f64)]))
    }));
    let names = shared.clone();
    add("GetBoundingRect", method(move |lua, engine, path, _| {
        let curve = if points_of(engine, path).is_empty() { None } else { Some(queried(engine, path, None)?) };
        let (least, most) = match curve.as_ref().and_then(Curve::bounds) {
            Some((least, most)) => {
                let thickness = engine.state.borrow().dom.number(path, names.thickness).unwrap_or(1.0) as f32;
                let margin = margin(thickness);
                ([least[0] - margin, least[1] - margin], [most[0] + margin, most[1] + margin])
            }
            None => ([-f32::MAX; 2], [f32::MAX; 2]),
        };
        let rect = Value::Rect(dom::Rect::new(dom::Vector2::new(least[0], least[1]), dom::Vector2::new(most[0], most[1])));
        Ok(values([convert::to_lua(lua, engine, &rect)?]))
    }));
    for (name, by_length, tangent) in [
        ("GetPositionOnCurve", false, false),
        ("GetTangentOnCurve", false, true),
        ("GetPositionOnCurveArcLength", true, false),
        ("GetTangentOnCurveArcLength", true, true),
    ] {
        add(name, method(move |lua, engine, path, args| {
            let t = float_arg(lua, engine, args, 1)?;
            let curve = queried(engine, path, Some(name))?;
            let located = if by_length { curve.locate_by_length(t) } else { curve.locate(t) };
            let Some((index, local)) = located else {
                return raise(format!("Path2D:{name}() attempting to query with zero segments"));
            };
            let segment = &curve.segments[index];
            let value = if tangent { vector(segment.tangent(local)) } else { Value::UDim2(curve.report(segment.position(local))) };
            Ok(values([convert::to_lua(lua, engine, &value)?]))
        }));
    }

    // Thickness holds 0 to 100
    if let Some(info) = reflection.class_property(class, thickness).cloned() {
        behaviors.add_setter(class, thickness, Rc::new(move |lua, engine, path, value| {
            let Value::Number(number) = convert::coerce(lua, engine, &info, &value)? else { return Ok(()) };
            set_property(engine, path, thickness, Value::Number(if number.is_nan() { 0.0 } else { number.clamp(0.0, 100.0) }));
            Ok(())
        }));
    }

    // What the engine's own scripts reach and a game's do not
    for name in [Some(transparency), reflection.name_id("SelectedControlPoint"), reflection.name_id("SelectedControlPointData")].into_iter().flatten() {
        let text = reflection.name(name).to_string();
        let read = format!("The current thread cannot read '{text}' (lacking capability RobloxScript)");
        let write = format!("The current thread cannot write '{text}' (lacking capability RobloxScript)");
        behaviors.add_getter(class, name, getter(move |_, _, _| raise(read.clone())));
        behaviors.add_setter(class, name, Rc::new(move |_, _, _, _| raise(write.clone())));
    }
}

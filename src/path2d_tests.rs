//! Path2D through the script API and the host's side of it (path2d.rs). The numbers are the ones a playing client
//! printed (tools/parity/expected/path2d.txt, path2d_layout.txt).

use std::rc::Rc;

use mlua::prelude::*;
use rbx_dom_weak::types as dom;

use super::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    path2d::{self, ControlPoint, Curve, Frame, Segment},
    scheduler,
};

fn start() -> (Lua, Rc<Engine>) {
    let engine = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    (lua, engine)
}

/// Runs `source` as a script, then the clock to `until`: the output lines ("kind: message").
fn run_in(lua: &Lua, engine: &Rc<Engine>, source: &str, until: f64) -> Vec<String> {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        super::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    scheduler::run_until(lua, engine, until).unwrap();
    engine.state.borrow().output.entries.iter().map(|entry| format!("{}: {}", entry.kind, entry.message)).collect()
}

fn run(source: &str) -> Vec<String> {
    let (lua, engine) = start();
    run_in(&lua, &engine, source, 0.2)
}

/// The lines a script printed, without the "print: " before each.
fn printed(source: &str) -> Vec<String> {
    let output = run(source);
    let errors: Vec<&String> = output.iter().filter(|line| !line.starts_with("print: ")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    output.iter().map(|line| line["print: ".len()..].to_string()).collect()
}

const HELPERS: &str = r#"
    local Point = Path2DControlPoint.new
    local Offset = UDim2.fromOffset
    local function Try(Callback)
        local Ok, Result = pcall(Callback)
        return if Ok then "ok" else tostring(Result)
    end
    local function Positions(Path)
        local Parts = {}
        for Index, Entry in Path:GetControlPoints() do
            Parts[Index] = Entry.Position.X.Offset .. "," .. Entry.Position.Y.Offset
        end
        return table.concat(Parts, ";")
    end
"#;

fn udim2(x_scale: f32, x_offset: i32, y_scale: f32, y_offset: i32) -> dom::UDim2 {
    dom::UDim2::new(dom::UDim::new(x_scale, x_offset), dom::UDim::new(y_scale, y_offset))
}

fn at(x: i32, y: i32) -> ControlPoint {
    ControlPoint { position: udim2(0.0, x, 0.0, y), ..ControlPoint::default() }
}

#[test]
fn a_path_has_studios_defaults_and_holds_its_thickness_in_range() {
    let lines = printed(
        r#"
        local Path = Instance.new("Path2D")
        print(Path.Closed, Path.Color3, Path.Thickness, Path.Visible, Path.ZIndex, Path:GetMaxControlPoints())
        print(Path:IsA("GuiBase"), Path:IsA("GuiBase2d"), Path:IsA("GuiObject"))
        local Kept = {}
        for _, Value in { -3, 0, 2.5, 100, 100.5, 1e6 } do
            Path.Thickness = Value
            table.insert(Kept, Path.Thickness)
        end
        print(table.concat(Kept, " "))
        for _, Name in { "Transparency", "SelectedControlPoint", "PropertiesSerialize" } do
            print(select(2, pcall(function() return (Path :: any)[Name] end)))
        end
        print(select(2, pcall(function() (Path :: any).Transparency = 0.5 end)))
        print(select(2, pcall(function() return (Path :: any):GetSegmentCount() end)))
        print(select(2, pcall(function() return Path:GetPropertyChangedSignal("ControlPoints") end)))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false 0, 0, 0 1 true 1 100",
            "true false false",
            "0 0 2.5 100 100 100",
            "The current thread cannot read 'Transparency' (lacking capability RobloxScript)",
            "The current thread cannot read 'SelectedControlPoint' (lacking capability RobloxScript)",
            "PropertiesSerialize is not a valid member of Path2D \"Path2D\"",
            "The current thread cannot write 'Transparency' (lacking capability RobloxScript)",
            "The current thread cannot call 'GetSegmentCount' (lacking capability RobloxScript)",
            "ControlPoints is not a valid property name.",
        ]
    );
}

#[test]
fn a_control_point_takes_none_one_or_three_udim2s_and_compares_by_value() {
    let lines = printed(&format!(
        r#"{HELPERS}
        print(Point(), Point(UDim2.new(0.25, 7, 0.5, -9)))
        print(Try(function() return (Point :: any)(Offset(1, 2), Offset(3, 4)) end))
        print(Try(function() return (Point :: any)(1) end))
        print(Try(function() return (Point :: any)(Offset(1, 2), 1, Offset(5, 6)) end))
        print(Point() == Point(), Point(Offset(1, 2)) == Point(Offset(1, 3)), typeof(Point()))
        local Value = Point(Offset(1, 2))
        Value.Position = Offset(9, 9)
        print(Value.Position, Try(function() (Value :: any).Position = 5 end), Try(function() (Value :: any).Foo = 5 end), Try(function() return (Value :: any).Foo end))
        -- a path holds copies
        local Path = Instance.new("Path2D")
        Path:SetControlPoints({{ Value }})
        Value.Position = Offset(50, 50)
        Path:GetControlPoint(1).Position = Offset(70, 70)
        print(Positions(Path))
        "#
    ));
    assert_eq!(
        lines,
        [
            "Path2DControlPoint { Position = {0, 0}, {0, 0}, LeftTangent = {0, 0}, {0, 0}, RightTangent = {0, 0}, {0, 0} } Path2DControlPoint { Position = {0.25, 7}, {0.5, -9}, LeftTangent = {0, 0}, {0, 0}, RightTangent = {0, 0}, {0, 0} }",
            "Trying to create a Path2DControlPoint with wrong number of arguments",
            "Test:18: invalid argument #1 to 'new' (UDim2 expected, got number)",
            "Test:19: invalid argument #2 to 'new' (UDim2 expected, got number)",
            "true false Path2DControlPoint",
            "{0, 9}, {0, 9} Test:23: invalid argument #3 (UDim2 expected, got number) Foo is not a valid member of Path2DControlPoint 'Foo' is not a valid member of Path2DControlPoint",
            "9,9",
        ]
    );
}

#[test]
fn the_list_checks_its_arguments_as_roblox_does() {
    let lines = printed(&format!(
        r#"{HELPERS}
        local Path = Instance.new("Path2D")
        for _, Bad in {{ 5, "a", {{ a = Point() }}, {{ Point(), 5 }} }} do
            print(Try(function() (Path :: any):SetControlPoints(Bad) end))
        end
        print(Try(function() (Path :: any):SetControlPoints() end))
        Path:SetControlPoints({{ Point(Offset(10, 0)), Point(Offset(20, 0)), Point(Offset(30, 0)) }})
        local Read = {{}}
        for _, Index in {{ 0, 1, 3, 4, 1.5, 2.49, 0.4, 0.5, "2", "x", true, math.huge, 2 ^ 31 }} do
            local Ok, Result = pcall(function() return (Path :: any):GetControlPoint(Index).Position.X.Offset end)
            table.insert(Read, if Ok then tostring(Result) else "[" .. Result .. "]")
        end
        print(table.concat(Read, " "))
        print(Try(function() (Path :: any):GetControlPoint() end), Try(function() (Path :: any):InsertControlPoint(1, 5) end), Try(function() (Path :: any):InsertControlPoint(1) end))
        -- without a parent: an insert and an update are refused after their index is checked, a removal is not
        print(Try(function() Path:InsertControlPoint(5, Point()) end), Try(function() Path:InsertControlPoint(4, Point()) end))
        print(Try(function() Path:UpdateControlPoint(4, Point()) end), Try(function() Path:UpdateControlPoint(2, Point()) end))
        print(Try(function() Path:RemoveControlPoint(4) end), Try(function() Path:RemoveControlPoint(2) end), Positions(Path))
        -- the limit: a longer list keeps its first hundred
        local Limit = Instance.new("Path2D")
        print(Try(function() Limit:SetControlPoints(table.create(101, Point())) end), #Limit:GetControlPoints())
        print(Try(function() Limit:InsertControlPoint(1, Point()) end), Try(function() Limit:InsertControlPoint(102, Point()) end))
        "#
    ));
    assert_eq!(
        lines,
        [
            "Unable to cast double to Array",
            "Unable to cast string to Array",
            "Unable to cast Dictionary to Array",
            "Variant cast failed",
            "Argument 1 missing or nil",
            "[Path2D:GetControlPoint() at index 0 access out of bounds] 10 30 [Path2D:GetControlPoint() at index 4 access out of bounds] 20 20 [Path2D:GetControlPoint() at index 0 access out of bounds] 10 20 [Unable to cast string to int] 10 [Path2D:GetControlPoint() at index -2147483648 access out of bounds] [Path2D:GetControlPoint() at index -2147483648 access out of bounds]",
            "Argument 1 missing or nil Unable to cast double to Path2DControlPoint Argument 2 missing or nil",
            "Path2D:InsertControlPoint() at index 5 access out of bounds Attempting to use Path2D with invalid parent",
            "Path2D:UpdateControlPoint() at index 4 access out of bounds Attempting to use Path2D with invalid parent",
            "Path2D:RemoveControlPoint() at index 4 access out of bounds Spline removing control point at index 1 access out of bounds 10,0;30,0",
            "Path2D:SetControlPoints() only supports 100 control points. 100",
            "Path2D:InsertControlPoint() only supports 100 control points. Path2D:InsertControlPoint() at index 102 access out of bounds",
        ]
    );
}

#[test]
fn the_spline_trails_the_list_until_a_query() {
    let lines = printed(&format!(
        r#"{HELPERS}
        local Holder = Instance.new("Frame")
        local Path = Instance.new("Path2D")
        Path.Parent = Holder
        local Three = {{ Point(Offset(0, 0)), Point(Offset(10, 0)), Point(Offset(10, 10)) }}
        Path:SetControlPoints(Three)
        -- the spline is still empty: an edit past its end raises, and the list keeps the edit
        print(Try(function() Path:InsertControlPoint(2, Point(Offset(4, 4))) end), Positions(Path))
        print(Try(function() Path:InsertControlPoint(1, Point(Offset(5, 5))) end), Try(function() Path:UpdateControlPoint(3, Point()) end))
        Path:SetControlPoints(Three)
        print(Path:GetLength(), Try(function() Path:InsertControlPoint(4, Point(Offset(4, 4))) end))
        -- a frame does not bring it up to date where no layout shows the path
        local Loose = Instance.new("Path2D")
        Loose:SetControlPoints(Three)
        Loose.Parent = Holder
        task.wait()
        task.wait()
        print(Try(function() Loose:RemoveControlPoint(3) end))
        -- a clone has the points and an empty spline
        local Copy = Path:Clone()
        Copy.Parent = Holder
        print(Positions(Copy), Try(function() Copy:InsertControlPoint(5, Point()) end), Try(function() Copy:InsertControlPoint(1, Point()) end))
        "#
    ));
    assert_eq!(
        lines,
        [
            "Spline inserting control point at index 1 access out of bounds 0,0;4,4;10,0;10,10",
            "ok Spline updating control point at index 2 access out of bounds",
            "20 ok",
            "Spline removing control point at index 2 access out of bounds",
            "0,0;10,0;10,10;4,4 Spline inserting control point at index 4 access out of bounds ok",
        ]
    );
}

#[test]
fn control_point_changed_fires_deferred_for_every_edit_that_went_through() {
    let lines = printed(&format!(
        r#"{HELPERS}
        local Path = Instance.new("Path2D")
        Path.Parent = Instance.new("Frame")
        local Log = {{}}
        Path.ControlPointChanged:Connect(function(...) table.insert(Log, "point" .. select('#', ...)) end)
        Path.Changed:Connect(function(Name) table.insert(Log, Name) end)
        local function Flush()
            local Now = table.concat(Log, " ")
            task.wait()
            print("[" .. Now .. "]", table.concat(Log, " "))
            table.clear(Log)
        end
        Path:SetControlPoints({{ Point(Offset(0, 0)), Point(Offset(10, 0)) }})
        Path:SetControlPoints({{ Point(Offset(0, 0)), Point(Offset(10, 0)) }})
        Flush()
        Path:GetLength()
        Path:InsertControlPoint(3, Point(Offset(10, 10)))
        Path:UpdateControlPoint(1, Point(Offset(1, 1)))
        Path:RemoveControlPoint(2)
        Flush()
        pcall(function() (Path :: any):SetControlPoints({{ 5 }}) end)
        pcall(function() Path:InsertControlPoint(9, Point()) end)
        Path:SetControlPoints({{ Point(), Point(), Point(), Point() }})
        pcall(function() Path:InsertControlPoint(5, Point()) end)
        Flush()
        Path.Thickness = 5
        Path.Thickness = 5
        Path.Thickness = 500
        Path.Thickness = 600
        Path.Closed = true
        Flush()
        "#
    ));
    assert_eq!(lines, ["[] point0 point0", "[] point0 point0 point0", "[] point0", "[] Thickness Thickness Closed"]);
}

#[test]
fn queries_check_the_segments_then_the_parent() {
    let lines = printed(&format!(
        r#"{HELPERS}
        local Path = Instance.new("Path2D")
        local function Rect() local Value = Path:GetBoundingRect() return Value.Min.X .. " " .. Value.Max.X end
        print(Path:GetLength(), Rect(), Try(function() return Path:GetPositionOnCurve(0.5) end))
        Path:SetControlPoints({{ Point(Offset(3, 4)) }})
        print(Try(function() return Path:GetLength() end), Try(function() return Path:GetBoundingRect() end), Try(function() return Path:GetTangentOnCurveArcLength(0.5) end))
        Path:SetControlPoints({{ Point(Offset(3, 4)), Point(Offset(13, 4)) }})
        print(Try(function() return Path:GetPositionOnCurve(0.5) end), Try(function() return (Path :: any):GetPositionOnCurve() end), Try(function() return (Path :: any):GetPositionOnCurve("x") end))
        local Kept = {{}}
        for _, Class in {{ "Folder", "UICorner", "Frame", "ScreenGui", "BillboardGui" }} do
            Path.Parent = Instance.new(Class)
            table.insert(Kept, Path.Parent)
            local Ok, Result = pcall(function() return Path:GetPositionOnCurve(0.5) end)
            print(Class, Ok and Path:GetLength(), Result)
        end
        Path.Parent = Instance.new("Frame")
        Path.Closed = true
        print(Path:GetLength(), Path:GetPositionOnCurve(0.75), Path:GetBoundingRect())
        Path:SetControlPoints({{ Point(Offset(3, 4)), Point(Offset(3, 4)) }})
        print(Path:GetLength(), Path:GetTangentOnCurve(0.5), Path:GetPositionOnCurveArcLength(0.5), Path:GetBoundingRect())
        "#
    ));
    assert_eq!(
        lines,
        [
            "0 -3.4028234663852886e+38 3.4028234663852886e+38 Path2D:GetPositionOnCurve() attempting to query with zero segments",
            "Attempting to use Path2D with invalid parent Attempting to use Path2D with invalid parent Path2D:GetTangentOnCurveArcLength() attempting to query with zero segments",
            "Attempting to use Path2D with invalid parent Argument 1 missing or nil Unable to cast string to float",
            "Folder false Attempting to use Path2D with invalid parent",
            "UICorner false Attempting to use Path2D with invalid parent",
            "Frame 10 {0, 8}, {0, 4}",
            // (a ScreenGui that is nowhere measures 800 by 600)
            "ScreenGui 10 {0.00999999978, 0}, {0.00666666683, 0}",
            "BillboardGui 10 {0, 8}, {0, 4}",
            "10 {0, 10}, {0, 4} 2, 3, 14, 5",
            "0 0, 0 {0, 3}, {0, 4} 2, 3, 4, 5",
        ]
    );
}

#[test]
fn the_curve_answers_in_float32_as_roblox_does() {
    let lines = printed(&format!(
        r#"{HELPERS}
        local Path = Instance.new("Path2D")
        Path.Parent = Instance.new("Frame")
        local function Number(Value) return string.format("%.9g", Value) end
        local function Show(Value)
            if typeof(Value) == "Vector2" then return Number(Value.X) .. " " .. Number(Value.Y) end
            if typeof(Value) == "Rect" then return Show(Value.Min) .. " " .. Show(Value.Max) end
            if typeof(Value) == "UDim2" then return Value.X.Offset .. " " .. Value.Y.Offset end
            return Number(Value)
        end
        local function Report(Label, ...)
            local Parts = {{ Label }}
            for _, Value in {{ ... }} do table.insert(Parts, Show(Value)) end
            print(table.concat(Parts, " | "))
        end
        -- lines, by parameter (each segment an equal share) and by length
        Path:SetControlPoints({{ Point(Offset(0, 0)), Point(Offset(100, 0)), Point(Offset(100, 50)) }})
        Report("lines", Path:GetLength(), Path:GetBoundingRect(), Path:GetPositionOnCurve(0.625), Path:GetTangentOnCurve(0.5), Path:GetPositionOnCurveArcLength(0.875), Path:GetPositionOnCurve(0 / 0), Path:GetPositionOnCurve(7))
        Path.Thickness = 20
        Report("thick", Path:GetBoundingRect())
        Path.Thickness = 0
        Report("thin", Path:GetBoundingRect())
        Path.Thickness = 1
        Path.Closed = true
        Report("closed", Path:GetLength(), Path:GetPositionOnCurve(0.75), Path:GetTangentOnCurve(1), Path:GetPositionOnCurveArcLength(0.75))
        Path.Closed = false
        -- one handle: a quadratic through it; two: a cubic
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(0, 0), Offset(0, 60)), Point(Offset(100, 0)) }})
        Report("quadratic", Path:GetLength(), Path:GetBoundingRect(), Path:GetPositionOnCurve(0.25), Path:GetTangentOnCurve(0.125))
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(0, 0), Offset(0, 55)), Point(Offset(100, 100), Offset(-55, 0), Offset(0, 0)) }})
        Report("cubic", Path:GetLength(), Path:GetBoundingRect(), Path:GetPositionOnCurve(0.625), Path:GetTangentOnCurve(0.125))
        -- the length stops halving where the middle lies on the chord
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(0, 0), Offset(100, 0)), Point(Offset(100, 80), Offset(-100, 0), Offset(0, 0)) }})
        Report("s", Path:GetLength())
        -- the bounds take no inner point on an axis whose derivative has no square term
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(0, 0), Offset(30, 90)), Point(Offset(90, 0), Offset(-30, 90), Offset(0, 0)) }})
        Report("symmetric", Path:GetBoundingRect())
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(0, 0), Offset(30, 90)), Point(Offset(90, 0), Offset(-30, 91), Offset(0, 0)) }})
        Report("nearly", Path:GetBoundingRect())
        Path:SetControlPoints({{ Point(Offset(0, 0), Offset(-30, 0), Offset(0, 60)), Point(Offset(100, 50), Offset(-50, 0), Offset(25, 50)), Point(Offset(180, 10), Offset(0, 40), Offset(20, -20)) }})
        Report("three", Path:GetLength(), Path:GetTangentOnCurve(0.4), Path:GetTangentOnCurve(0.9))
        "#
    ));
    assert_eq!(
        lines,
        [
            "lines | 150 | -1 -1 101 51 | 100 12 | 0 50 | 100 31 | 0 0 | 100 50",
            "thick | -10.5 -10.5 110.5 60.5",
            "thin | 0 0 100 50",
            "closed | 261.803406 | 75 37 | -100 -50 | 58 29",
            "quadratic | 126.277237 | -1 -1 101 31 | 6 22 | 25 90",
            "cubic | 157.002319 | -1 -1 101 101 | 44 82 | 32.109375 155.859375",
            "s | 128.062485",
            "symmetric | -1 -1 91 1",
            "nearly | -1 -1 91 68.8755188",
            "three | 255.111176 | 144 -2.40000916 | 55.8000107 -118.800018",
        ]
    );
}

#[test]
fn the_points_are_bytes_a_file_and_a_clone_carry() {
    let points = [
        ControlPoint { position: udim2(0.25, 7, 0.5, -9), left: udim2(0.125, 1, 0.0625, 2), right: udim2(-0.5, 3, 2.0, 4) },
        at(10, 20),
    ];
    let bytes = path2d::encode(&points);
    assert_eq!(bytes.len(), 4 + 2 * 49);
    assert_eq!(&bytes[..5], [2, 0, 0, 0, 0x23]);
    assert_eq!(path2d::decode(&bytes), points);
    assert_eq!(path2d::encode(&[]), [0, 0, 0, 0]);
    // a cut value holds the points that are whole
    assert_eq!(path2d::decode(&bytes[..60]), points[..1]);
    assert!(path2d::decode(&[]).is_empty());

    // a model file's path: its points and its Transparency reach the engine, and a script reads the points
    let mut model = rbx_dom_weak::WeakDom::new(rbx_dom_weak::InstanceBuilder::new("Folder"));
    let root = model.root_ref();
    let frame = model.insert(root, rbx_dom_weak::InstanceBuilder::new("Frame").with_name("Holder"));
    model.insert(
        frame,
        rbx_dom_weak::InstanceBuilder::new("Path2D")
            .with_name("Stored")
            .with_property("PropertiesSerialize", dom::Variant::BinaryString(bytes.clone().into()))
            .with_property("Transparency", dom::Variant::Float32(0.5))
            .with_property("Thickness", dom::Variant::Float32(4.0)),
    );
    let mut file = Vec::new();
    rbx_binary::to_writer(&mut file, &model, &[frame]).unwrap();
    let path = std::env::temp_dir().join(format!("ree-path2d-{}.rbxm", std::process::id()));
    std::fs::write(&path, &file).unwrap();
    let (lua, engine) = start();
    let loaded = super::project::load_model(&engine, &path).expect("model");
    let _ = std::fs::remove_file(&path);
    let stored = engine.state.borrow().dom.at(loaded[0]).children[0];
    {
        let state = engine.state.borrow();
        assert_eq!(path2d::control_points(&state, &engine.reflection, stored), points);
        assert_eq!(path2d::transparency(&state, &engine.reflection, stored), 0.5);
    }
    let holder = engine_lua::instance_value(&lua, &engine, loaded[0]).unwrap();
    engine.lua.borrow().shared.as_ref().unwrap().raw_set("__holder", holder).unwrap();
    let output = run_in(
        &lua,
        &engine,
        r#"
        local Stored = __holder.Stored
        local Copy = Stored:Clone()
        -- (the copy's spline is empty: the removal raises after the list changed)
        pcall(function() Copy:RemoveControlPoint(1) end)
        print(#Stored:GetControlPoints(), Stored:GetControlPoint(1), #Copy:GetControlPoints(), Copy:GetControlPoint(1).Position)
        "#,
        0.1,
    );
    assert!(
        output.contains(&"print: 2 Path2DControlPoint { Position = {0.25, 7}, {0.5, -9}, LeftTangent = {0.125, 1}, {0.0625, 2}, RightTangent = {-0.5, 3}, {2, 4} } 1 {0, 10}, {0, 20}".to_string()),
        "{output:?}"
    );
}

/// A host with a layout says where a path's parent is: positions resolve against that rectangle, offsets stay
/// pixels, and a turned parent turns the curve (the numbers of tools/parity/expected/path2d_layout.txt).
#[test]
fn a_host_places_the_parent_and_brings_the_spline_up_to_date() {
    let (lua, engine) = start();
    let turn = 30f32.to_radians();
    let (sin, cos) = turn.sin_cos();
    // a 100 by 60 parent at (100, 200), turned 30 degrees about its middle (150, 230)
    let turned = [cos, sin, -sin, cos, 150.0 - cos * 150.0 + sin * 230.0, 230.0 - sin * 150.0 - cos * 230.0];
    let shown = engine.clone();
    let laid_out = lua
        .create_function(move |_, path: LuaAnyUserData| {
            path2d::laid_out(&shown, engine_lua::raw::instance_of_ud(&path).ok_or_else(|| LuaError::runtime("Instance expected"))?);
            Ok(())
        })
        .unwrap();
    engine.lua.borrow().shared.as_ref().unwrap().raw_set("__laidOut", laid_out).unwrap();
    path2d::set_frame_source(
        &engine,
        Rc::new(move |engine, parent| {
            let name = engine.state.borrow().dom.at(parent).name.to_str_lossy().to_string();
            match name.as_str() {
                "Plain" => Some(Frame::new([50.0, 30.0], [200.0, 100.0])),
                "Flat" => Some(Frame::new([10.0, 20.0], [200.0, 0.0])),
                "Negative" => Some(Frame::new([10.0, 20.0], [-50.0, 100.0])),
                "Turned" => Some(Frame::new([100.0, 200.0], [100.0, 60.0]).turned(turned)),
                _ => None,
            }
        }),
    );
    let output = run_in(
        &lua,
        &engine,
        r#"
        local Point = Path2DControlPoint.new
        local Offset = UDim2.fromOffset
        local function Number(Value) return string.format("%.6g", Value) end
        local function Show(Value)
            if typeof(Value) == "Vector2" then return Number(Value.X) .. " " .. Number(Value.Y) end
            if typeof(Value) == "Rect" then return Show(Value.Min) .. " " .. Show(Value.Max) end
            if typeof(Value) == "UDim2" then return Number(Value.X.Scale) .. " " .. Value.X.Offset .. " " .. Number(Value.Y.Scale) .. " " .. Value.Y.Offset end
            return Number(Value)
        end
        local Kept = {}
        local function Under(Name, Points)
            local Parent = Instance.new("Frame")
            Parent.Name = Name
            table.insert(Kept, Parent)
            local Path = Instance.new("Path2D")
            Path.Name = Name .. "Path"
            Path:SetControlPoints(Points)
            Path.Parent = Parent
            return Path
        end
        local function Report(Label, Path, Time)
            print(Label, Show(Path:GetLength()), "|", Show(Path:GetBoundingRect()), "|", Show(Path:GetPositionOnCurve(Time)), "|", Show(Path:GetTangentOnCurve(Time)))
        end
        Report("plain", Under("Plain", { Point(UDim2.new(0, 0, 0, 0)), Point(UDim2.new(0.5, 10, 1, -20)), Point(UDim2.new(1, 0, 0, 0)) }), 0.25)
        Report("handle", Under("Plain", { Point(Offset(0, 0), Offset(0, 0), UDim2.new(0.1, -20, 0, 0)), Point(Offset(100, 50), UDim2.new(-0.25, 0, 0, 0), Offset(0, 0)) }), 0)
        Report("flat", Under("Flat", { Point(UDim2.new(0.5, 10, 0.5, 20)), Point(UDim2.new(1, 0, 1, 60)) }), 0.5)
        Report("negative", Under("Negative", { Point(UDim2.new(0.5, 10, 0.5, 20)), Point(UDim2.new(1, 0, 1, 60)) }), 0.5)
        Report("turned", Under("Turned", { Point(Offset(0, 0), Offset(0, 0), Offset(40, 0)), Point(UDim2.new(1, 0, 1, 0)) }), 0.5)
        Report("unplaced", Under("Other", { Point(Offset(3, 4)), Point(Offset(13, 4)) }), 0.5)
        local Shown = Under("Plain", { Point(), Point(), Point() })
        print(pcall(function() Shown:InsertControlPoint(4, Point()) end))
        -- the host's layout shows the path: its spline holds the four points, and the next insert goes through
        __laidOut(Shown)
        print(pcall(function() Shown:InsertControlPoint(5, Point()) end))
        "#,
        0.1,
    );
    let printed: Vec<&str> = output.iter().map(|line| line.trim_start_matches("print: ")).collect();
    assert_eq!(
        printed,
        [
            "plain 256.431 | 49 29 251 111 | 0.275 0 0.4 0 | 110 80",
            // (a handle whose scale and offset cancel is none: the segment is a quadratic through the other)
            "handle 114.779 | 49 29 151 81 | 0 0 0 0 | 100 100",
            "flat 98.4886 | 119 39 211 81 | 0 165 0 60 | 90 40",
            "negative 91.2414 | -6 89 11 181 | 0.85 0 1.15 0 | 15 90",
            "turned 119.237 | 120.699 178.019 179.301 281.981 | 0.45 0 0.25 0 | 56.6025 101.962",
            "unplaced 10 | 2 3 14 5 | 0 8 0 4 | 10 0",
            "false Spline inserting control point at index 3 access out of bounds",
            "true",
        ]
    );
    assert_eq!(path2d::parented(&engine).len(), 7);
}

#[test]
fn a_curve_is_lines_quadratics_and_cubics_by_its_handles() {
    let frame = Frame::new([10.0, 20.0], [100.0, 50.0]);
    let handle = |x: i32, y: i32| udim2(0.0, x, 0.0, y);
    let points = [
        ControlPoint { position: udim2(0.0, 0, 0.0, 0), left: handle(-9, -9), right: handle(0, 0) },
        ControlPoint { position: udim2(1.0, 0, 0.0, 0), left: handle(0, 0), right: handle(0, 30) },
        ControlPoint { position: udim2(1.0, 0, 1.0, 0), left: handle(10, 0), right: handle(0, 0) },
    ];
    let open = Curve::new(&points, false, &frame);
    assert_eq!(
        open.segments,
        [
            Segment::Line([10.0, 20.0], [110.0, 20.0]),
            Segment::Cubic([110.0, 20.0], [110.0, 50.0], [120.0, 70.0], [110.0, 70.0]),
        ]
    );
    // closed: the last point joins the first, whose left handle is the closing segment's
    let closed = Curve::new(&points, true, &frame);
    assert_eq!(closed.segments.len(), 3);
    assert_eq!(closed.segments[2], Segment::Quadratic([110.0, 70.0], [1.0, 11.0], [10.0, 20.0]));
    // two points make one segment closed or not, one point none
    assert_eq!(Curve::new(&points[..2], true, &frame).segments.len(), 1);
    assert!(Curve::new(&points[..1], true, &frame).segments.is_empty());
    assert_eq!(open.locate(0.5), Some((1, 0.0)));
    assert_eq!(open.locate(1.0), Some((1, 1.0)));
    assert_eq!(open.locate(f32::NAN), Some((0, 0.0)));
    assert_eq!(path2d::margin(0.0), 0.0);
    assert_eq!(path2d::margin(10.0), 5.5);
}

/// By length the client walks each curved segment in 32 float32 Runge-Kutta steps and answers wherever the walk
/// ends. Every number is Studio 0.747's (`%.9g`), on offset-only points under a 1 by 1 frame at the origin.
#[test]
#[allow(clippy::excessive_precision)] // (the numbers as Studio prints them)
fn by_length_the_walk_ends_where_roblox_ends() {
    let frame = Frame::new([0.0, 0.0], [1.0, 1.0]);
    let curve = |points: &[[i32; 6]]| {
        let points: Vec<ControlPoint> = points
            .iter()
            .map(|p| ControlPoint { position: udim2(0.0, p[0], 0.0, p[1]), left: udim2(0.0, p[2], 0.0, p[3]), right: udim2(0.0, p[4], 0.0, p[5]) })
            .collect();
        Curve::new(&points, false, &frame)
    };
    let at = |curve: &Curve, t: f32| {
        let (index, local) = curve.locate_by_length(t).unwrap();
        let (position, tangent) = (curve.segments[index].position(local), curve.segments[index].tangent(local));
        [position[0], position[1], tangent[0], tangent[1]]
    };
    // a quarter turn: the tangent's digits, and at the end the walk stops a hair short
    let arc = curve(&[[0, 0, 0, 0, 0, 55], [100, 100, -55, 0, 0, 0]]);
    assert_eq!(arc.length(), 157.002319);
    assert_eq!(at(&arc, 0.25), [7.64234352, 38.2384453, 60.0430107, 143.897491]);
    assert_eq!(at(&arc, 0.5), [29.3749218, 70.6249237, 108.749886, 108.750122]);
    assert_eq!(at(&arc, 1.0), [99.9997711, 100.0, 164.999924, 0.000381469727]);
    // a tight bend: the walk loses the curve and ends past the segment (a parameter over 1)
    let tight = curve(&[[197, 91, 0, 0, 108, 104], [190, 299, -197, 230, 0, 0]]);
    assert_eq!(tight.length(), 420.165405);
    assert_eq!(at(&tight, 0.75), [123.864845, 376.943817, -46.3157043, 71.7278595]);
    assert_eq!(at(&tight, 1.0), [195.488174, 292.600189, 618.936646, -720.897217]);
    assert!(tight.locate_by_length(1.0).unwrap().1 > 1.0);
    // a quadratic, and what is left after the segments before
    let quadratic = curve(&[[0, 0, 0, 0, 80, 140], [220, 30, 0, 0, 0, 0]]);
    assert_eq!(at(&quadratic, 0.425), [80.5935745, 74.3869705, 211.996368, 63.3484955]);
    let three = curve(&[[0, 0, -30, 0, 0, 60], [100, 50, -50, 0, 25, 50], [180, 10, 0, 40, 20, -20]]);
    assert_eq!(at(&three, 0.625), [119.502274, 69.497612, 101.95517, 35.9767075]);
    let mixed = curve(&[[0, 0, 0, 0, 0, 0], [130, 20, 0, 0, 40, 90], [300, 70, -90, -10, 0, 0], [310, 200, 0, 0, 0, 0]]);
    assert_eq!(at(&mixed, 0.325), [137.702026, 35.6282768, 120.615822, 218.641174]);
    assert_eq!(at(&mixed, 0.825), [303.848328, 120.028137, 10.0, 130.0]);
    // straight segments whose float32 lengths leave the last one a hair short: the end answers the start
    let lines = curve(&[[0, 0, 0, 0, 0, 0], [110, 80, 0, 0, 0, 0], [200, 0, 0, 0, 0, 0]]);
    assert_eq!(lines.length(), 256.430664);
    assert_eq!(lines.locate_by_length(1.0), Some((0, 0.0)));
    assert_eq!(lines.locate_by_length(0.5).unwrap().0, 0);
    // outside 0..1 is the nearer end; NaN stays NaN
    assert_eq!(at(&arc, 7.0), at(&arc, 1.0));
    assert_eq!(at(&arc, -7.0), [0.0, 0.0, 0.0, 165.0]);
    assert!(arc.locate_by_length(f32::NAN).unwrap().1.is_nan());
}

/// The last float32 digits of a length and of an inner bound follow the client's order of operations. Every
/// number is Studio 0.747's (`%.9g`, Thickness 0), on offset-only points under a 1 by 1 frame at the origin.
#[test]
#[allow(clippy::excessive_precision)] // (the numbers as Studio prints them)
fn lengths_and_inner_bounds_end_in_roblox_digits() {
    let frame = Frame::new([0.0, 0.0], [1.0, 1.0]);
    let curve = |points: &[[i32; 6]]| {
        let points: Vec<ControlPoint> = points
            .iter()
            .map(|p| ControlPoint { position: udim2(0.0, p[0], 0.0, p[1]), left: udim2(0.0, p[2], 0.0, p[3]), right: udim2(0.0, p[4], 0.0, p[5]) })
            .collect();
        Curve::new(&points, false, &frame)
    };
    // a quadratic that turns back along its own line, 24 down and 6 up: measured from its start the points near
    // the turn are finer than from the screen's corner, and the sum falls short of 30 by what that leaves
    let back = curve(&[[13, -16, 0, 0, 0, -36], [13, -34, 0, 0, 0, 0]]);
    assert_eq!(back.length(), 29.9999962);
    assert_eq!(back.bounds(), Some(([13.0, -40.0], [13.0, -16.0])));
    // a quadratic's inner bounds (exactly -3 and -7 in real numbers)
    let quadratic = curve(&[[0, -5, 0, 0, -9, -6], [9, 1, 0, 0, 0, 0]]);
    assert_eq!(quadratic.bounds(), Some(([-2.99999952, -6.99999905], [9.0, 1.0])));
    // cubics
    let small = curve(&[[-6, 0, 0, 0, 7, 0], [0, 28, 0, -52, 0, 0]]);
    assert_eq!(small.length(), 41.8456917);
    assert_eq!(small.bounds(), Some(([-6.0, -5.52959967], [0.0493827015, 28.0])));
    let large = curve(&[[296, -241, 0, 0, 219, -81], [-262, -212, 144, 128, 0, 0]]);
    assert_eq!(large.length(), 669.68689);
    assert_eq!(large.bounds(), Some(([-262.0, -258.23114], [341.613342, -173.369263])));
}

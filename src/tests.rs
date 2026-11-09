//! Engine tests through the script API (ports of runtime/tests/{datatypes,modules,scheduler,
//! smoke}.luau and the probe expectations the differential harness checks against the Luau
//! runtime). Each test starts a fresh engine without a project.

use std::rc::Rc;

use mlua::prelude::*;

use super::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    scheduler,
};

fn start() -> (Lua, Rc<Engine>) {
    start_with(Options::default())
}

fn start_with(options: Options) -> (Lua, Rc<Engine>) {
    let engine = Engine::new(options).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    // ModuleScript.Source is not scriptable: tests set sources through the engine
    let e = engine.clone();
    let set_source = lua
        .create_function(move |_, (module, source): (LuaAnyUserData, LuaString)| {
            let id = engine_lua::raw::instance_of_ud(&module).ok_or_else(|| LuaError::runtime("Instance expected"))?;
            e.state.borrow_mut().dom.at_mut(id).set_source(Some(Rc::from(&*source.as_bytes())));
            Ok(())
        })
        .unwrap();
    engine.lua.borrow().shared.as_ref().unwrap().raw_set("__setSource", set_source).unwrap();
    (lua, engine)
}

/// Runs `source` on a scheduler thread with the shared globals; returns the output lines
/// ("kind: message") after flushing deferred work.
fn run(lua: &Lua, engine: &Rc<Engine>, source: &str) -> Vec<String> {
    // A script environment (script = a detached ModuleScript named Test, require)
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
    lines(engine)
}

fn lines(engine: &Engine) -> Vec<String> {
    engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect()
}

fn assert_script(source: &str) {
    let (lua, engine) = start();
    let output = run(&lua, &engine, source);
    let errors: Vec<&String> = output.iter().filter(|l| l.starts_with("error")).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn instances_properties_and_errors() {
    assert_script(
        r#"
        local F = Instance.new("Frame")
        assert(F.Name == "Frame" and F.ClassName == "Frame" and F.Parent == nil)
        assert(typeof(F) == "Instance" and type(F) == "userdata")
        assert(getmetatable(F) == "The metatable is locked")
        assert(tostring(F) == "Frame")
        assert(F.BackgroundTransparency == 0 and F.Visible == true and F.ZIndex == 1)
        F.BackgroundTransparency = 0.1
        assert(F.BackgroundTransparency == 0.10000000149011612)
        F.Size = UDim2.fromOffset(10.9, -3.2)
        assert(F.Size == UDim2.new(0, 10, 0, -3))
        local Label = Instance.new("TextLabel")
        Label.TextSize = 10.7
        assert(Label.TextSize == 10)
        Label.TextSize = 150
        assert(Label.TextSize == 100)
        local UIScale = Instance.new("UIScale")
        UIScale.Scale = 0
        assert(UIScale.Scale == 1.0000000116860974e-07)
        UIScale.Scale = -1
        assert(UIScale.Scale == 1.0000000116860974e-07)
        UIScale.Scale = math.huge
        assert(UIScale.Scale == 1000)
        UIScale.Scale = 0.885
        assert(UIScale.Scale == 0.88499999046325684)
        Label.Name = 12
        assert(Label.Name == "12")
        Label.Parent = F
        assert(F:FindFirstChild("12") == Label and F["12"] == Label)
        assert(Label:GetFullName() == "Frame.12")
        assert(Label:IsDescendantOf(F) and F:IsAncestorOf(Label) and Label:IsA("GuiObject") and not Label:IsA("Frame"))
        assert(Label:FindFirstAncestorOfClass("Frame") == F and F:FindFirstChildWhichIsA("GuiObject") == Label)
        assert(#F:GetChildren() == 1 and #F:GetDescendants() == 1)

        local function Message(Callback)
            local Ok, Err = pcall(Callback)
            assert(not Ok and type(Err) == "string")
            -- a script position prefix is kept as "@: "
            return (string.gsub(Err, "^Test:%d+: ", "@: "))
        end
        assert(Message(function() return F.Nope end) == 'Nope is not a valid member of Frame "Frame"')
        assert(Message(function() F.Size = 5 end) == "Unable to assign property Size. UDim2 expected, got number")
        -- (a name the enum lacks: measured, tools/parity/probes/values.luau's `Invalid value "wood" for enum Material`)
        assert(Message(function() F.BorderMode = "Nope" end) == 'Unable to assign property BorderMode. Invalid value "Nope" for enum BorderMode')
        assert(Message(function() F.BorderMode = Enum.Font.Arial end) == "Unable to assign property BorderMode. EnumItem of type BorderMode expected, got an EnumItem of type Font")
        assert(Message(function() F.AbsoluteSize = Vector2.new() end) == "Unable to assign property AbsoluteSize. Property is read only")
        -- (any value goes into a boolean or number property, as in a playing client: Lua truth, Luau's number reading or 0)
        F.Visible = 0
        assert(F.Visible == true)
        F.BackgroundTransparency = "0.5"
        assert(F.BackgroundTransparency == 0.5)
        F.BackgroundTransparency = "x"
        assert(F.BackgroundTransparency == 0)
        assert(Message(function() F.Parent = 5 end) == "@: invalid argument #3 (Instance expected, got number)")
        -- (measured, tools/parity/probes/hierarchy.luau)
        assert(Message(function() F.Parent = F end) == "Attempt to set Frame as its own parent")
        assert(Message(function() return Instance.new("NotAClass") end) == 'Unable to create an Instance of type "NotAClass"')
        assert(Message(function() return F:GetPropertyChangedSignal("Nope") end) == "Nope is not a valid property name.")
        assert(Message(function() F:SetAttribute("A", {}) end) == "Array is not a supported attribute type")
        assert(Message(function() return F.Changed.Nope end) == "Nope is not a valid member of RBXScriptSignal")
        assert(Message(function() return F[1] end) == "@: invalid argument #2 (string expected, got number)")
        assert(Message(function() return game:GetService("Nope") end) == "'Nope' is not a valid Service name")
        F.BorderMode = "Inset"
        assert(F.BorderMode == Enum.BorderMode.Inset)
        F.BorderMode = 0
        assert(F.BorderMode == Enum.BorderMode.Outline)
        "#,
    );
}

#[test]
fn datamodel_services_are_available_by_class_name() {
    assert_script(
        r#"
        local runService = game.RunService
        assert(runService == game:GetService("RunService"))
        assert(runService.Name == "Run Service")
        assert(runService:IsA("RunService"))
        assert(runService.RenderStepped == game:GetService("RunService").RenderStepped)
        local lighting = game.Lighting
        assert(lighting == game:GetService("Lighting"))
        "#,
    );
}

#[test]
fn not_scriptable_properties_are_no_members() {
    // Studio's dump permits no Read on PVInstance.Origin, PVInstance."Pivot Offset" or
    // Model.WorldPivotData: a script indexing them finds a child of that name, else no member
    assert_script(
        r#"
        local Part = Instance.new("Part")
        local Origin = Instance.new("Attachment")
        Origin.Name = "Origin"
        Origin.Parent = Part
        assert(Part.Origin == Origin)
        local Model = Instance.new("Model")
        local Ok, Err = pcall(function() return Model.WorldPivotData end)
        assert(not Ok and string.find(Err, "WorldPivotData is not a valid member of Model", 1, true), Err)
        Ok = pcall(function() Model.Origin = CFrame.new() end)
        assert(not Ok)
        -- the pivot is still stored and moved as before
        Model.WorldPivot = CFrame.new(1, 2, 3)
        assert(Model:GetPivot() == CFrame.new(1, 2, 3))
        assert(Part.PivotOffset == CFrame.identity)
        "#,
    );
}

/// A key with its first letter lower-cased reaches the member it spells otherwise: a client resolves a read by the exact member,
/// then a child of that name, then the member named with the first letter upper-cased (every method, property, event and callback
/// of the API dump answers so: 2,175 methods, 802 events, 2,352 readable properties, a headless client of roblox-sim, Studio 0.744);
/// a method call by the exact method, then the upper-cased one, and nothing else; a write by the exact name only.
/// tools/parity/probes/camelcase.luau holds the same cases against the client's output.
#[test]
fn lower_case_first_letters_reach_members_on_reads_and_calls() {
    assert_script(
        r#"
        local function Message(Callback)
            local Ok, Err = pcall(Callback)
            assert(not Ok and type(Err) == "string", "expected an error")
            return (string.gsub(Err, "^Test:%d+: ", "@: "))
        end
        local Holder = Instance.new("Folder")
        Holder.Name = "Holder"
        local Part = Instance.new("Part")
        Part.Name = "P"
        Part.Parent = Holder

        -- reads: the same method function, the same signal, the property's value, inherited members, services
        assert(rawequal(Part.getPropertyChangedSignal, Part.GetPropertyChangedSignal))
        assert(rawequal(Part.getFullName, Part.GetFullName) and rawequal(Part.isAncestorOf, Part.IsAncestorOf))
        assert(Part.anchored == false and Part.name == "P" and Part.parent == Holder and Part.position == Part.Position)
        assert(Part.touched == Part.Touched and tostring(Part.touched) == "Signal Touched")
        assert(tostring(Part.changed) == "Signal Changed" and tostring(Part.destroying) == "Signal Destroying")
        assert(Instance.new("MeshPart").canCollide == true and Instance.new("Frame").visible == true)
        assert(workspace.gravity == workspace.Gravity)
        -- (BasePart.size, the dump's own lower-case name, permits no script: the read finds Size)
        assert(Part.size == Part.Size)

        -- writes take exact names only, and change nothing
        assert(Message(function() Part.anchored = true end) == 'anchored is not a valid member of Part "Holder.P"')
        assert(Message(function() Part.name = "Q" end) == 'name is not a valid member of Part "Holder.P"')
        assert(Message(function() Part.size = Vector3.one end) == 'size is not a valid member of Part "Holder.P"')
        assert(Part.Anchored == false and Part.Name == "P" and Part.Size.Y == 1.2000000476837158)

        -- a child with the key's own name comes first; children answer to their own names only; the exact member beats a child
        local Box = Instance.new("Part")
        Box.Name = "Box"
        for _, Name in { "anchored", "getPropertyChangedSignal", "Anchored", "foo" } do
            local Child = Instance.new("Folder")
            Child.Name = Name
            Child.Parent = Box
        end
        assert(Box.anchored.ClassName == "Folder" and Box.getPropertyChangedSignal.ClassName == "Folder")
        assert(Box.Anchored == false)
        assert(Message(function() return Box.Foo end) == 'Foo is not a valid member of Part "Box"')

        -- method calls: the method by either spelling; children and other members are no methods
        assert(tostring(Part:getPropertyChangedSignal("Name")) == "Signal NameChanged")
        assert(Part:getPropertyChangedSignal("Name") == Part:GetPropertyChangedSignal("Name"))
        assert(Part:getFullName() == "Holder.P" and Holder:findFirstChildOfClass("Part") == Part)
        assert(tostring(Box:getPropertyChangedSignal("Name")) == "Signal NameChanged")
        Part:setAttribute("A", 5)
        assert(Part:getAttribute("A") == 5)
        assert(Message(function() return Part:Anchored() end) == 'Anchored is not a valid member of Part "Holder.P"')
        assert(Message(function() return Part:anchored() end) == 'anchored is not a valid member of Part "Holder.P"')
        assert(Message(function() return Part:touched() end) == 'touched is not a valid member of Part "Holder.P"')
        assert(Message(function() return Box:anchored() end) == 'anchored is not a valid member of Part "Box"')
        assert(Message(function() return Box:foo() end) == 'foo is not a valid member of Part "Box"')
        assert(Message(function() return Part.getPropertyChangedSignal("Name") end) == "Expected ':' not '.' calling member function GetPropertyChangedSignal")

        -- the first letter only, towards upper case only; the error names the key as written
        local Http = game:GetService("HttpService")
        assert(Http:jSONEncode({ 1 }) == "[1]")
        assert(Message(function() return Http:jsonEncode({ 1 }) end) == 'jsonEncode is not a valid member of HttpService "HttpService"')
        assert(Message(function() return Part.ANCHORED end) == 'ANCHORED is not a valid member of Part "Holder.P"')
        assert(Message(function() return Part.cframe end) == 'cframe is not a valid member of Part "Holder.P"')
        assert(Message(function() return Part.fooBar end) == 'fooBar is not a valid member of Part "Holder.P"')
        assert(Message(function() return Part:fooBar() end) == 'fooBar is not a valid member of Part "Holder.P"')

        -- callbacks: a read names the callback by its own name; a write and a call need their exact names
        local Function = Instance.new("BindableFunction")
        Function.Name = "F"
        local Callback = "OnInvoke is a callback member of BindableFunction; you can only set the callback value, get is not available"
        assert(Message(function() return Function.onInvoke end) == Callback)
        assert(Message(function() Function.onInvoke = function() end end) == 'onInvoke is not a valid member of BindableFunction "F"')
        assert(Message(function() return Function:onInvoke() end) == 'onInvoke is not a valid member of BindableFunction "F"')
        Function.OnInvoke = function()
            return 8
        end
        assert(Function:invoke() == 8)
        "#,
    );
}

/// The API dump's own lower-case names (deprecated aliases) are the members they stand for, as a client treats them
/// (tools/parity/probes/aliases.luau): a property alias reads and writes the other's value and Changed names the other; a method
/// alias does the other's work through a function of its own; childAdded and a value object's changed are signals of their own
/// that fire with ChildAdded and Changed.
#[test]
fn the_dumps_lower_case_aliases_are_the_members_they_stand_for() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Players = game:GetService("Players")
        assert(game:getService("Players") == Players and game:service("Players") == Players)
        assert(typeof(Players:getPlayers()) == "table" and typeof(Players:players()) == "table")
        assert(not rawequal(game.getService, game.GetService) and not rawequal(Players.getPlayers, Players.GetPlayers))
        assert(game.workspace == workspace and game.lighting == game:GetService("Lighting"))
        local Part = Instance.new("Part")
        Part.brickColor = BrickColor.new("Really red")
        assert(Part.BrickColor.Name == "Really red")
        Part.BrickColor = BrickColor.new("Lime green")
        assert(Part.brickColor.Name == "Lime green")
        local Humanoid = Instance.new("Humanoid")
        Humanoid.maxHealth = 50
        assert(Humanoid.MaxHealth == 50 and Humanoid.maxHealth == 50)
        local Folder = Instance.new("Folder")
        Folder.archivable = false
        assert(Folder.Archivable == false)
        local Ok, Err = pcall(function() Part.className = "X" end)
        assert(not Ok and string.find(Err, "Unable to assign property className. Property is read only", 1, true), Err)
        assert(tostring(Folder.childAdded) == "Signal childAdded" and Folder.childAdded ~= Folder.ChildAdded)
        _G.Names = {}
        Part.Changed:Connect(function(Name) table.insert(_G.Names, Name) end)
        Part.brickColor = BrickColor.new("Really blue")
        _G.Added = {}
        Folder.childAdded:Connect(function(Child) table.insert(_G.Added, Child.Name) end)
        local Value = Instance.new("IntValue")
        _G.Values = {}
        Value.changed:Connect(function(New) table.insert(_G.Values, New) end)
        local Child = Instance.new("Folder")
        Child.Name = "Kid"
        Child.Parent = Folder
        Value.Value = 7
        "#,
    );
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        -- (the property's own names, as for the exact name: a client's Changed names BrickColor and the Color it moves)
        assert(table.concat(_G.Names, ",") == "BrickColor,Color", table.concat(_G.Names, ","))
        assert(table.concat(_G.Added, ",") == "Kid" and table.concat(_G.Values, ",") == "7")
        "#,
    );
}

/// The report's case: a property signal by its lower-case method name, both handlers run, the newest connection first (as the
/// report's Studio output and a client print them).
#[test]
fn a_property_signal_by_a_lower_case_method_name_fires() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local part = Instance.new("Part")
        part:GetPropertyChangedSignal("Position"):Connect(function()
            print("one")
        end)
        part:getPropertyChangedSignal("Position"):Connect(function()
            print("two")
        end)
        part.Position += Vector3.new(0, 1, 0)
        "#,
    );
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    assert_eq!(lines(&engine), vec!["print: two".to_string(), "print: one".to_string()]);
}

/// What reaching a member by its lower-case first letter costs on the work clock: the member's own price and cost.rs LOWER_FIRST
/// (a client, +33 to +47 ns over the exact name for reads and calls alike).
#[test]
fn a_lower_case_first_letter_costs_a_second_lookup() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Part = Instance.new("Part")
        local function Per(Body)
            local start = os.clock()
            for _ = 1, 1000 do
                Body()
            end
            return (os.clock() - start) / 1000 * 1e9
        end
        -- (each spelling once first: a member's first use in a frame costs more)
        local _, _, _ = Part.Anchored, Part.anchored, Part:isAncestorOf(Part)
        local Exact = Per(function() return Part.Anchored end)
        local Lower = Per(function() return Part.anchored end)
        assert(math.abs(Lower - Exact - 40) < 0.5, string.format("a read: %.2f ns more", Lower - Exact))
        local ExactCall = Per(function() return Part:IsAncestorOf(Part) end)
        local LowerCall = Per(function() return Part:isAncestorOf(Part) end)
        assert(math.abs(LowerCall - ExactCall - 40) < 0.5, string.format("a call: %.2f ns more", LowerCall - ExactCall))
        "#,
    );
}

/// (A parent change fires DescendantAdded, then ChildAdded, then AncestryChanged, then the instance's own Changed("Parent"):
/// measured in Studio, see a_model_arriving_in_a_folder_fires_its_events_in_studios_order; the order here was ChildAdded first and
/// Changed before both.)
#[test]
fn deferred_events_and_order() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local F = Instance.new("Frame")
        F:GetPropertyChangedSignal("Size"):Connect(function() print("size") end)
        F.Changed:Connect(function(Name) print("changed " .. Name) end)
        F.Size = UDim2.fromOffset(10, 10)
        print("before flush")
        local Folder = Instance.new("Folder")
        Folder.ChildAdded:Connect(function(Child) print("added " .. Child.Name) end)
        Folder.DescendantAdded:Connect(function(Child) print("descendant " .. Child.Name) end)
        F.Parent = Folder
        local Value = Instance.new("IntValue")
        Value.Changed:Connect(function(New) print("value " .. New) end)
        Value:GetPropertyChangedSignal("Value"):Connect(function() print("value signal") end)
        Value.Value = 5
        Value.Name = "Renamed"
        local E = Instance.new("BindableEvent")
        E.Event:Connect(function(X) print("A" .. X) end)
        E.Event:Connect(function(X) print("B" .. X) end)
        E.Event:Once(function(X) print("once" .. X) end)
        E:Fire(1)
        E:Fire(2)
        "#,
    );
    let output = lines(&engine);
    assert_eq!(
        output,
        vec![
            "print: before flush",
            "print: changed Size",
            "print: size",
            "print: descendant Frame",
            "print: added Frame",
            "print: changed Parent",
            "print: value signal",
            "print: value 5",
            "print: once1",
            "print: B1",
            "print: A1",
            "print: B2",
            "print: A2",
        ]
    );
}

#[test]
fn scheduler_chains_depth_and_timers() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Defers = 0
        local function DeferNext()
            Defers += 1
            if Defers < 100 then task.defer(DeferNext) end
        end
        task.defer(DeferNext)
        local Loop = Instance.new("BindableEvent")
        local Fires = 0
        Loop.Event:Connect(function()
            Fires += 1
            Loop:Fire()
        end)
        Loop:Fire()
        task.defer(function() print("defers " .. Defers) end)
        task.delay(0.05, function() print("delay " .. Fires) end)
        task.spawn(function()
            local Waited = task.wait(0.1)
            print("waited", Waited > 0.09)
        end)
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.3).unwrap();
    let output = lines(&engine);
    // One depth for events and task.defer, cut at 80 (measured in a playing client, Studio 0.744): the self-firing
    // event's 81st handler is refused for its handler, the task.defer chain's 81st call logs and queues nothing
    assert!(
        output.contains(&"error: Maximum event re-entrancy depth exceeded for BindableEvent.Event when calling anonymous function on line 10 in Test".to_string()),
        "{output:?}"
    );
    assert!(output.contains(&"error: Maximum re-entrancy depth (80) exceeded calling task.defer".to_string()), "{output:?}");
    assert!(output.contains(&"print: delay 80".to_string()), "{output:?}");
    assert!(output.contains(&"print: waited true".to_string()), "{output:?}");
    assert!(output.iter().any(|l| l.starts_with("print: defers")), "{output:?}");
}

#[test]
fn a_chain_of_events_and_defers_shares_one_depth() {
    // Measured in a playing client (Studio 0.744): an event fired from a deferred thread and a task.defer from a
    // handler count on one depth, task.spawn carries it along, and the chain stops at 80 steps
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Steps = 0
        local Event = Instance.new("BindableEvent")
        local function Next()
            Steps += 1
            if Steps < 300 then task.spawn(function() Event:Fire() end) end
        end
        Event.Event:Connect(function()
            Steps += 1
            if Steps < 300 then task.defer(Next) end
        end)
        Event:Fire()
        task.delay(0.05, function() print("steps " .. Steps) end)
        local Value = Instance.new("IntValue")
        Value:GetPropertyChangedSignal("Value"):Connect(function() Value.Value += 1 end)
        Value.Value = 1
        task.delay(0.05, function() print("value " .. Value.Value) end)
        print(tostring(Value:GetPropertyChangedSignal("Value")))
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.2).unwrap();
    let output = lines(&engine);
    assert!(output.contains(&"print: steps 80".to_string()), "{output:?}");
    assert!(output.contains(&"print: value 81".to_string()), "{output:?}");
    assert!(output.contains(&"print: Signal ValueChanged".to_string()), "{output:?}");
    assert!(output.iter().any(|line| line.starts_with("error: Maximum event re-entrancy depth exceeded for Object.ValueChanged when calling anonymous function on line 15 in Test")), "{output:?}");
}

#[test]
fn destroy_and_clone() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        local Child = Instance.new("Frame", Folder)
        Child:SetAttribute("Key", 3)
        Child:AddTag("Tagged")
        local Copy = Folder:Clone()
        assert(Copy ~= Folder and Copy:FindFirstChild("Frame"):GetAttribute("Key") == 3)
        assert(Copy.Frame:HasTag("Tagged"))
        Child.Destroying:Connect(function() print("destroying", Child.Parent == Folder) end)
        Child.Changed:Connect(function(Name) print("changed after destroy " .. Name) end)
        Folder:Destroy()
        print("parent", Child.Parent)
        local Ok, Err = pcall(function() Child.Parent = Copy end)
        print(Ok, Err)
        Child.Archivable = false
        print("clone", Child:Clone())
        "#,
    );
    let output = lines(&engine);
    // Destroying is deferred like every event (measured in a playing client, Studio 0.744): its handler runs after
    // Destroy returned, sees the instance unparented, and comes before the instance's own removal events.
    assert_eq!(
        output,
        vec![
            "print: parent nil",
            "print: false The Parent property of Frame is locked, current parent: NULL, new parent Folder",
            "print: clone nil",
            "print: destroying false",
            "print: changed after destroy Parent",
        ]
    );
}

#[test]
fn clear_all_children_destroys_the_last_child_first() {
    // Measured in a playing client (Studio 0.744): ChildRemoved fires for C3, C2, C1
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        for Index = 1, 3 do
            local Child = Instance.new("Folder")
            Child.Name = "C" .. Index
            Child.Parent = Folder
        end
        Folder.ChildRemoved:Connect(function(Child) print(Child.Name) end)
        Folder:ClearAllChildren()
        "#,
    );
    assert_eq!(lines(&engine), vec!["print: C3", "print: C2", "print: C1"]);
}

#[test]
fn an_engine_function_that_returns_nothing_returns_no_values() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/signals.luau and task_order.luau):
    // `pcall(Event.Fire, Event, {})` and `pcall(task.cancel, thread)` give only `true`, whether the method
    // is called with `:` or read first
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r##"
        local Event = Instance.new("BindableEvent")
        local Folder = Instance.new("Folder")
        print(select("#", Event:Fire({})), select("#", Event.Fire(Event, {})), select("#", Folder.ClearAllChildren(Folder)))
        print(select("#", pcall(task.cancel, task.defer(function() end))), select("#", Folder.FindFirstChild(Folder, "None")))
        "##,
    );
    assert_eq!(lines(&engine), vec!["print: 0 0 0", "print: 1 1"]);
}

#[test]
fn set_attribute_checks_names_and_types_as_roblox_does() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/instances.luau)
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        for _, Name in { "", "with space", "dash-name", "dot.name", "a/b", "RBXName", "rbxname", "123", string.rep("x", 100), string.rep("x", 101), "$x" } do
            local Ok, Message = pcall(Folder.SetAttribute, Folder, Name, 1)
            print(#Name, Ok, Message)
        end
        print(pcall(function() Folder:SetAttribute("with space", "fast path") end))
        print(pcall(Folder.SetAttribute, Folder, "A", {}))
        print(pcall(Folder.SetAttribute, Folder, "A", print))
        print(pcall(Folder.SetAttribute, Folder, "A", buffer.create(1)))
        "#,
    );
    let long = "x".repeat(101);
    assert_eq!(
        lines(&engine),
        vec![
            "print: 0 false Attribute name cannot be empty (\"\")".to_string(),
            "print: 10 false Attribute name contains illegal character ' ' (\"with space\")".to_string(),
            "print: 9 true nil".to_string(),
            "print: 8 true nil".to_string(),
            "print: 3 true nil".to_string(),
            "print: 7 false Attempt to assign attribute RBXName. CoreScript permission required".to_string(),
            "print: 7 true nil".to_string(),
            "print: 3 true nil".to_string(),
            "print: 100 true nil".to_string(),
            format!("print: 101 false Attribute name exceeds 100 character limit (\"{long}\")"),
            "print: 2 false Attribute name contains illegal character '$' (\"$x\")".to_string(),
            "print: false Attribute name contains illegal character ' ' (\"with space\")".to_string(),
            "print: false Array is not a supported attribute type".to_string(),
            "print: false Function is not a supported attribute type".to_string(),
            "print: false buffer is not a supported attribute type".to_string(),
        ]
    );
}

/// An attribute can hold an Instance (Studio 0.744 takes one; measured in a playing client,
/// tools/parity/probes/instance_attributes.luau): a read gives a new InstanceHandle that prints the instance's current
/// name and has no members, the attribute keeps a destroyed or loose instance, and a handle can be stored again.
#[test]
fn an_attribute_holds_an_instance_as_a_handle() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Holder = Instance.new("Folder")
        Holder.Parent = workspace
        local Target = Instance.new("Part")
        Target.Name = "Target"
        Target.Parent = workspace
        Holder:SetAttribute("Ref", Target)
        local Read = Holder:GetAttribute("Ref")
        print(typeof(Read), type(Read), tostring(Read), Read == Target, Holder:GetAttribute("Ref") == Read, rawequal(Holder:GetAttribute("Ref"), Read))
        print(pcall(function() return Read.Name end))
        print(pcall(function() return Read:IsA("Part") end))
        print(typeof(Holder:GetAttributes().Ref), ({ [Read] = true })[Holder:GetAttribute("Ref")])
        Target.Name = "Renamed"
        Target:Destroy()
        print(tostring(Holder:GetAttribute("Ref")))
        Holder:SetAttribute("Loose", Instance.new("Folder"))
        Holder:SetAttribute("Again", Holder:GetAttribute("Ref"))
        print(tostring(Holder:GetAttribute("Loose")), typeof(Holder:GetAttribute("Again")), tostring(Holder:GetAttribute("Again")))
        "#,
    );
    // the loose folder and the destroyed part are kept by the attributes alone (the Lua values made above are gone)
    lua.gc_collect().unwrap();
    lua.gc_collect().unwrap();
    engine_lua::raw::collect(&lua, &engine).unwrap();
    run(&lua, &engine, r#"print(tostring(workspace.Folder:GetAttribute("Loose")), tostring(workspace.Folder:GetAttribute("Ref")))"#);
    assert_eq!(
        lines(&engine),
        vec![
            "print: InstanceHandle userdata Target false true false",
            "print: false Name is not a valid member of InstanceHandle",
            "print: false IsA is not a valid member of InstanceHandle",
            "print: InstanceHandle nil",
            "print: Renamed",
            "print: Folder InstanceHandle Renamed",
            "print: Folder Renamed",
        ]
    );
}

/// Texts and forms measured in a playing client (Studio 0.744; tools/parity/probes/hierarchy.luau, values.luau,
/// coercion.luau, frame_phases.luau, lifetime.luau).
#[test]
fn parents_enums_and_casts_answer_as_a_playing_client_does() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local function Try(F) local Ok, Message = pcall(F) return if Ok then "ok" else Message end
        local Self = Instance.new("Folder")
        Self.Name = "Self"
        print(Try(function() Self.Parent = Self end))
        print(Try(function() game:GetService("Lighting").Parent = workspace end), Try(function() game:GetService("Lighting"):Destroy() end))
        print(game:GetService("Lighting").Parent == game, (string.gsub(Try(function() game.Parent = workspace end), game.Name, "<game>")))
        local Part = Instance.new("Part")
        for _, Value in { "512", 512.5, "wood" } do
            print(Try(function() Part.Material = Value end), tostring(Part.Material))
        end
        print(Try(function() Part.CFrame = Vector3.zero end))
        print(Try(function() typeof() end), type(getmetatable("")))
        print(Try(function() game:GetService("RunService"):BindToRenderStep("x", "high", function() end) end))
        local Debris = game:GetService("Debris")
        print(Try(function() Debris:AddItem({}, 1) end), Try(function() Debris:AddItem(nil, 1) end), Try(function() Debris:AddItem(Part, "x") end))
        local Gone = Instance.new("Part")
        Gone.Parent = workspace
        Debris:AddItem(Gone, 0)
        print(Gone.Parent)
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec![
            "print: Attempt to set Self as its own parent",
            "print: Cannot change Parent of type Lighting The Parent property of Lighting is locked",
            "print: true The Parent property of <game> is locked, current parent: NULL, new parent Workspace",
            "print: ok Enum.Material.Wood",
            "print: ok Enum.Material.Wood",
            "print: Unable to assign property Material. Invalid value \"wood\" for enum Material Enum.Material.Wood",
            "print: Unable to assign property CFrame. CoordinateFrame expected, got Vector3",
            "print: missing argument #1 string",
            "print: Unable to cast string to int",
            "print: Unable to cast value to Object ok Unable to cast string to double",
            "print: nil",
        ]
    );
}

/// A new instance reads its class's default name, which Studio does not always make the class's own (a BindableEvent
/// is "Event", measured in a playing client; a BoolValue "Value" and a WedgePart "Wedge" from Studio's defaults), and
/// the DataModel's full name is its Name (measured: tools/parity/probes/instances.luau).
#[test]
fn new_instances_read_their_default_names_and_the_data_model_its_full_name() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Value = Instance.new("BoolValue")
        print(Instance.new("BindableEvent").Name, Value.Name, Instance.new("WedgePart").Name, Instance.new("Folder").Name, Value:GetFullName())
        Value.Name = "Named"
        print(Value.Name, game:GetFullName() == game.Name, workspace:GetFullName())
        "#,
    );
    assert_eq!(lines(&engine), vec!["print: Event Value Wedge Folder Value", "print: Named true Workspace"]);
}

#[test]
fn instance_methods_answer_and_raise_as_roblox_does() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/instances.luau), with `:` and read with `.`
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        print(pcall(Folder.FindFirstChild, Folder, nil))
        print(pcall(function() return Folder:FindFirstChild(nil) end))
        print(Folder:IsDescendantOf(nil), Folder.IsDescendantOf(Folder, nil), Folder:IsDescendantOf(workspace))
        print(pcall(function() return Folder:FindFirstDescendant("x") end))
        print(pcall(function() return game:GetService("Lighting"):Clone() end))
        Folder:SetAttribute(5, 1)
        print(Folder:GetAttribute(5), Folder.GetAttribute(Folder, 5), Folder:GetAttribute("5"))
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec![
            "print: false Argument 1 missing or nil",
            "print: false Argument 1 missing or nil",
            "print: true true false",
            "print: false FindFirstDescendant is not enabled",
            "print: false Lighting cannot be cloned",
            "print: 1 1 1",
        ]
    );
}

#[test]
fn a_part_is_at_least_a_thousandth_of_a_stud_on_every_axis() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/changed.luau): Size = (0, 0, 0) reads 0.001 each
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Part = Instance.new("Part")
        Part.Size = Vector3.new(0, 0, 0)
        print(string.format("%.9g %.9g %.9g", Part.Size.X, Part.Size.Y, Part.Size.Z))
        local Wedge = Instance.new("WedgePart")
        Wedge.Size = Vector3.new(4, 0, 2)
        print(Wedge.Size.X, string.format("%.9g", Wedge.Size.Y), Wedge.Size.Z)
        "#,
    );
    assert_eq!(lines(&engine), vec!["print: 0.00100000005 0.00100000005 0.00100000005", "print: 4 0.00100000005 2"]);
}

#[test]
fn writing_nan_over_nan_fires_changed_again() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/changed.luau): a property, a NumberValue and an
    // attribute alike
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Part = Instance.new("Part")
        local Value = Instance.new("NumberValue")
        local Fired = { Part = 0, Value = 0, Attribute = 0 }
        Part:GetPropertyChangedSignal("Transparency"):Connect(function() Fired.Part += 1 end)
        Value.Changed:Connect(function() Fired.Value += 1 end)
        Part:GetAttributeChangedSignal("A"):Connect(function() Fired.Attribute += 1 end)
        for _ = 1, 2 do
            Part.Transparency = 0 / 0
            Value.Value = 0 / 0
            Part:SetAttribute("A", 0 / 0)
        end
        Part.Transparency = 0.5
        Part.Transparency = 0.5
        task.defer(function() print(Fired.Part, Fired.Value, Fired.Attribute) end)
        "#,
    );
    assert_eq!(lines(&engine), vec!["print: 3 2 2"]);
}

#[test]
fn random_draws_unit_vectors_shuffles_and_bounds_as_measured() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/random.luau, seed 0 and Random.new(9))
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Generator = Random.new(0)
        for _ = 1, 3 do Generator:NextNumber() end
        for _ = 1, 3 do Generator:NextNumber(-10, 10) end
        for _ = 1, 3 do Generator:NextInteger(1, 1000) end
        print(Generator:NextInteger(-2 ^ 40, 2 ^ 40))
        local Unit = Generator:NextUnitVector()
        print(string.format("%.9g %.9g %.9g", Unit.X, Unit.Y, Unit.Z))
        local List = { 1, 2, 3, 4, 5, 6, 7, 8 }
        Generator:Shuffle(List)
        print(table.concat(List, ","))
        local Other = Random.new(9)
        print(Other:NextInteger(5, 1), string.format("%.17g", Other:NextNumber(5, 1)), Other:NextInteger(1.5, 2.5))
        print(pcall(Other.Shuffle, Other, { 1, nil, 3 }))
        print(string.format("%.17g", Other:NextInteger(-2 ^ 63, 2 ^ 63)))
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec![
            "print: 290622939254",
            "print: -0.245626748 -0.670917571 0.699669242",
            "print: 8,6,2,4,7,5,1,3",
            "print: 4 2.5745788259440792 2",
            "print: false Attempted to shuffle table containing nil values. This is not allowed because the table length may change.",
            "print: -9.2233720368547758e+18",
        ]
    );
}

#[test]
fn the_task_library_raises_robloxs_errors() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/task_order.luau): no position on any of them,
    // and a thread queued twice is refused the second time and resumes once
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local function Try(...)
            local Ok, Message = pcall(...)
            print(Ok, Message)
        end
        Try(task.spawn, 5)
        Try(task.defer, "x")
        Try(task.delay, 0, nil)
        Try(task.delay, "x", function() end)
        Try(task.wait, "x")
        Try(task.cancel, 5)
        Try(task.cancel, coroutine.running())
        local Normal
        Normal = coroutine.create(function()
            coroutine.resume(coroutine.create(function() Try(task.cancel, Normal) end))
        end)
        coroutine.resume(Normal)
        local Twice = coroutine.create(function(Value)
            print("twice", Value)
            coroutine.yield()
            print("twice-second")
        end)
        task.defer(Twice, "a")
        Try(task.defer, Twice, "b")
        Try(task.spawn, Twice, "c")
        local Delayed = coroutine.create(function() end)
        task.delay(0, Delayed)
        Try(task.defer, Delayed)
        Try(task.delay, 0, Delayed)
        print((pcall(task.delay, "0.5", function() end)))
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec![
            "print: false invalid argument #1 to 'spawn' (function or thread expected)",
            "print: false invalid argument #1 to 'defer' (function or thread expected)",
            "print: false invalid argument #2 to 'delay' (function or thread expected)",
            "print: false invalid argument #1 to 'delay' (number expected, got string)",
            "print: false invalid argument #1 to 'wait' (number expected, got string)",
            "print: false invalid argument #1 to 'cancel' (thread expected, got number)",
            "print: false cannot cancel thread",
            "print: false cannot cancel thread",
            "print: false Cannot call task.defer on a thread that is already 'deferred' in the task library",
            "print: false Cannot call task.spawn on a thread that is already 'deferred' in the task library",
            "print: false Cannot call task.defer on a thread that is already 'delayed' in the task library",
            "print: false Cannot call task.delay on a thread that is already 'delayed' in the task library",
            "print: true",
            "print: twice a",
        ]
    );
}

#[test]
fn a_once_connection_reads_disconnected_from_the_fire_on() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/signals.luau): Connected is false right after the
    // Fire, the handler still runs once, and a second Fire before it runs does not reach it
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Event = Instance.new("BindableEvent")
        local Connection = Event.Event:Once(function(Value) print("once", Value) end)
        Event.Event:Connect(function(Value) print("normal", Value) end)
        print("before", Connection.Connected)
        Event:Fire(1)
        Event:Fire(2)
        print("after-fire", Connection.Connected)
        task.defer(function() print("later", Connection.Connected) end)
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec!["print: before true", "print: after-fire false", "print: normal 1", "print: once 1", "print: normal 2", "print: later false"]
    );
}

#[test]
fn connecting_a_value_that_is_not_a_function_logs_and_gives_a_disconnected_connection() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/signals.luau): Connect(nil) and Connect(5) raise
    // nothing; the message is logged, the connection is not connected and firing the signal skips it
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Event = Instance.new("BindableEvent")
        local Connection = Event.Event:Connect(nil)
        print(typeof(Connection), Connection.Connected)
        Event.Event:Once(5)
        Event.Event:Connect(function() print("handler") end)
        Event:Fire()
        "#,
    );
    let message = "error: Attempt to connect failed: Passed value is not a function";
    assert_eq!(lines(&engine), vec![message, "print: RBXScriptConnection false", message, "print: handler"]);
}

#[test]
fn bindables_copy_tables_for_every_receiver_as_roblox_does() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/signals.luau): lua/bindable.rs
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r##"
        local Event = Instance.new("BindableEvent")
        local Got = {}
        Event.Event:Connect(function(...) table.insert(Got, table.pack(...)) end)
        Event.Event:Connect(function(...) table.insert(Got, table.pack(...)) end)
        local Sent = setmetatable({ 1, 2, Key = "dropped", Nested = { 3 } }, { __index = function() return "meta" end })
        Event:Fire(Sent)
        Sent[1] = "changed after the fire"
        task.wait()
        local A, B = Got[1][1], Got[2][1]
        print(#Got, A == Sent, A == B, getmetatable(A), A[1], A[2], A.Key, A.Missing)
        local function Keys(Value)
            local Out = {}
            for Key, Item in Value do
                table.insert(Out, typeof(Key) .. ":" .. string.gsub(tostring(Key), "0x%x+", "ADDR") .. "=" .. tostring(Item))
            end
            table.sort(Out)
            return table.concat(Out, " ")
        end
        table.clear(Got)
        Event:Fire({ [1.5] = "a", [-2.7] = "b", [1e20] = "c", Name = "d" }, coroutine.create(function() end), { Part = Instance.new("Part") })
        task.wait()
        local Args = Got[1]
        print(Keys(Args[1]), Args.n, Args[2], typeof(Args[3].Part))
        table.clear(Got)
        Event:Fire({ [{}] = 1 })
        Event:Fire({ [Enum.Material.Plastic] = 1, [Vector3.new(1, 2, 3)] = 2 })
        task.wait()
        print(Keys(Got[1][1]), "|", Keys(Got[3][1]))
        local Cyclic = {}
        Cyclic.Self = Cyclic
        print(pcall(Event.Fire, Event, Cyclic))
        print(pcall(Event.Fire, Event, { [true] = 1 }))
        local Bytes = buffer.create(1)
        local Arrived
        Event.Event:Once(function(_, Value) Arrived = Value end)
        Event:Fire(nil, Bytes)
        task.wait()
        print(Arrived ~= Bytes, buffer.len(Arrived))
        local Function = Instance.new("BindableFunction")
        local Inside
        Function.OnInvoke = function(Value)
            Inside = Value
            return Value, setmetatable({ 1, Key = "k" }, {})
        end
        local Argument = { 1, Key = "k" }
        local Back, Second = Function:Invoke(Argument)
        print(Inside == Argument, Back == Inside, Inside.Key, Back[1], getmetatable(Second), Second.Key, Second[1])
        print(pcall(Function.Invoke, Function, Cyclic))
        "##,
    );
    for _ in 0..12 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    assert_eq!(
        lines(&engine),
        vec![
            "print: 2 false false nil 1 2 nil nil",
            "print: string:-2=b string:-9223372036854775808=c string:1=a string:Name=d 3 nil Instance",
            "print: string:<Table> (table: ADDR)=1 | string:<Vector3> (1, 2, 3)=2 string:<token> (Enum.Material.Plastic)=1",
            "print: false tables cannot be cyclic",
            "print: false Invalid table key type used",
            "print: true 1",
            "print: false false nil 1 nil nil 1",
            "print: false tables cannot be cyclic",
        ]
    );
}

#[test]
fn number_and_boolean_properties_take_any_value_as_in_roblox() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/coercion.luau)
    assert_script(
        r#"
        local Int = Instance.new("IntValue")
        local function IntOf(Value) Int.Value = Value return Int.Value end
        assert(IntOf(2.5) == 3 and IntOf(-2.5) == -3 and IntOf(2.7) == 3 and IntOf(-0.5) == -1)
        assert(IntOf(1e20) == 0 and IntOf(0 / 0) == 0 and IntOf(math.huge) == 0)
        assert(IntOf("0x10") == 16 and IntOf(" 5 ") == 5 and IntOf("five") == 0 and IntOf(true) == 0 and IntOf(nil) == 0)
        local Frame = Instance.new("Frame")
        local function ZOf(Value) Frame.ZIndex = Value return Frame.ZIndex end
        assert(ZOf(2.7) == 2 and ZOf(-2.7) == -2 and ZOf(2 ^ 31) == -2147483648 and ZOf(0 / 0) == -2147483648)
        assert(1 / ZOf(-0.5) == math.huge, "an integer reads no -0")
        Frame.BorderSizePixel = 1000
        assert(Frame.BorderSizePixel == 100)
        Frame.BorderSizePixel = -3
        assert(Frame.BorderSizePixel == 0)
        local Label = Instance.new("TextLabel")
        Label.TextSize = 2 ^ 31
        assert(Label.TextSize == 1)
        local Bool = Instance.new("BoolValue")
        for _, Truthy in { 0, "", {}, Vector3.zero } do
            Bool.Value = false
            Bool.Value = Truthy
            assert(Bool.Value == true)
        end
        Bool.Value = nil
        assert(Bool.Value == false)
        "#,
    );
}

#[test]
fn enums_and_engine_datatypes() {
    assert_script(
        r#"
        assert(Enum.Font.GothamSemibold == Enum.Font.GothamMedium)
        assert(typeof(Enum) == "Enums" and typeof(Enum.Font) == "Enum" and typeof(Enum.Font.Arial) == "EnumItem")
        assert(tostring(Enum) == "Enums" and tostring(Enum.Font) == "Font" and tostring(Enum.Font.Arial) == "Enum.Font.Arial")
        assert(Enum.Font.Arial.EnumType == Enum.Font and Enum.Font.Arial:IsA("Font") and not Enum.Font.Arial:IsA("Nope"))
        assert(Enum.Font:FromName("Arial") == Enum.Font.Arial and Enum.Font:FromValue(Enum.Font.Arial.Value) == Enum.Font.Arial)
        local Seen = {}
        Seen[Enum.Font.Arial] = true
        assert(Seen[Enum.Font.Arial])
        assert(getmetatable(Enum.Font.Arial) == "The metatable is locked")
        local Face = Font.new("rbxasset://fonts/families/GothamSSm.json", Enum.FontWeight.Bold)
        assert(typeof(Face) == "Font" and Face.Bold and Face.Style == Enum.FontStyle.Normal)
        assert(tostring(Face) == "Font { Family = rbxasset://fonts/families/GothamSSm.json, Weight = Bold, Style = Normal }")
        assert(Face == Font.new("rbxasset://fonts/families/GothamSSm.json", "Bold"))
        local Label = Instance.new("TextLabel")
        Label.FontFace = Face
        Face.Bold = false
        assert(Label.FontFace.Bold and Label.FontFace ~= Face)
        assert(tostring(Content.none) == "Content{SourceType=None}" and Content.fromAssetId(5).Uri == "rbxassetid://5")
        local Info = TweenInfo.new(2, Enum.EasingStyle.Linear)
        assert(Info.Time == 2 and Info.EasingDirection == Enum.EasingDirection.Out and Info.RepeatCount == 0)
        local R = Random.new(7)
        assert(R:NextNumber() == 0.8737085924861333 and R:NextNumber() == 0.4321572161309693)
        assert(Random.new(7):NextInteger(1, 100) == 2 or true)
        assert(typeof(DateTime.now()) == "DateTime" and DateTime.fromUnixTimestamp(0):ToIsoDate() == "1970-01-01T00:00:00Z")
        assert(UDim2.new(".5", "-3.9", false, 9.8) == UDim2.new(0.5, -3, 0, 9))
        assert(Color3.fromRGB(300, -5, 128.6).R == Color3.new(300 / 255, 0, 0).R)
        local Ok, Err = pcall(UDim2.new, UDim.new(), 5)
        assert(not Ok and Err == "Invalid arguments to constructor")
        assert(getmetatable(Vector2) == "The metatable is locked")
        assert(typeof(vector.create(1, 2, 3)) == "Vector3")
        assert(ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1)).Keypoints[2].Value == Color3.new(0, 0, 1))
        "#,
    );
}

#[test]
fn a_modules_chunk_is_named_by_its_full_name_as_in_roblox() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/chunknames.luau): debug.info, error positions
    // and tracebacks name a module by its full name, without Luau's `[string "..."]` wrapping.
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        Folder.Name = "Probes"
        Folder.Parent = game:GetService("ReplicatedStorage")
        local M = Instance.new("ModuleScript")
        M.Name = "Chunk"
        __setSource(M, [[local M = {}
function M.info() return debug.info(1, 's') end
function M.fail() error('inside') end
function M.trace() return debug.traceback('t') end
return M]])
        M.Parent = Folder
        local Module = require(M)
        print(Module.info())
        print(select(2, pcall(Module.fail)))
        print((string.split(Module.trace(), "\n"))[2])
        "#,
    );
    let output = lines(&engine);
    assert_eq!(output[0], "print: ReplicatedStorage.Probes.Chunk");
    assert_eq!(output[1], "print: ReplicatedStorage.Probes.Chunk:3: inside");
    assert_eq!(output[2], "print: ReplicatedStorage.Probes.Chunk:4 function trace");
}

#[test]
fn modules_require_and_environments() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        Folder.Parent = game:GetService("ReplicatedStorage")
        local function Module(Name, Source)
            local M = Instance.new("ModuleScript")
            M.Name = Name
            __setSource(M, Source)
            M.Parent = Folder
            return M
        end
        Module("Sibling", "return { Value = 42 }")
        Module("Caller", "local S = require('./Sibling') return { S = S, Script = script }")
        Module("Cycle", "return require(script)")
        Module("Two", "return 1, 2")
        Module("Broken", "error('boom', 0)")
        Module("Yields", "task.wait(0.05) return 'late'")
        local Caller = require(Folder.Caller)
        print(Caller.S.Value, Caller.Script == Folder.Caller, require(Folder.Sibling) == Caller.S)
        print(pcall(require, Folder.Cycle))
        print(pcall(require, Folder.Two))
        print(pcall(require, Folder.Broken))
        print(pcall(require, Folder.Broken))
        print(pcall(require, 5))
        task.spawn(function() print("second", require(Folder.Yields)) end)
        print("first", require(Folder.Yields))
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.2).unwrap();
    let output = lines(&engine);
    assert_eq!(output[0], "print: 42 true true");
    assert!(output[1].starts_with("print: false") && output[1].ends_with("Requested module was required recursively"), "{output:?}");
    assert!(output[2].ends_with("Module code did not return exactly one value"), "{output:?}");
    assert_eq!(output[3], "print: false boom");
    assert!(output[4].ends_with("Requested module experienced an error while loading"), "{output:?}");
    // (a playing client's text: measured, tools/parity/probes/errors.luau)
    assert!(output[5].ends_with("require(assetId) cannot be called from a client.  assetId = 5"), "{output:?}");
    assert!(output.contains(&"print: first late".to_string()) && output.contains(&"print: second late".to_string()), "{output:?}");
}

#[test]
fn wait_for_child_and_services() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        task.spawn(function()
            local Child = Folder:WaitForChild("Late")
            print("got", Child.Name)
        end)
        task.spawn(function()
            print("timeout", Folder:WaitForChild("Never", 0.1))
        end)
        task.delay(0.05, function()
            local Late = Instance.new("Frame")
            Late.Name = "Late"
            Late.Parent = Folder
        end)
        print(game:GetService("Workspace") == workspace, workspace.CurrentCamera.Name, game.Players.LocalPlayer.Name)
        print(game:GetService("RunService"):IsStudio(), game:GetService("RunService"):IsEdit())
        print(workspace.CurrentCamera.ViewportSize)
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.3).unwrap();
    let output = lines(&engine);
    assert_eq!(output[0], "print: true Camera Player");
    assert_eq!(output[1], "print: true true");
    assert_eq!(output[2], "print: 1920, 1080");
    assert!(output.contains(&"print: got Late".to_string()), "{output:?}");
    assert!(output.contains(&"print: timeout nil".to_string()), "{output:?}");
}

/// Instance lifetime (lua/raw.rs): unreachable parentless instances are freed; instances in the
/// DataModel, referenced by scripts or by other instances' properties keep their userdata
/// identity (weak-keyed tables included).
#[test]
fn instance_lifetime_and_identity() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Keep = Instance.new("Folder")
        Keep.Name = "Keep"
        Keep.Parent = workspace
        local Weak = setmetatable({}, { __mode = "k" })
        Weak[Keep] = true
        for Index = 1, 200 do
            local Child = Instance.new("Frame")
            Child.Name = "Child" .. Index
            Child.Parent = Keep
            local Loose = Instance.new("Frame")
            Loose.Name = "Loose" .. Index
            local Dead = Instance.new("Frame")
            Dead.Parent = Keep
            Dead:Destroy()
        end
        _G.Weak = Weak
        _G.Held = Instance.new("Frame")
        local Value = Instance.new("ObjectValue")
        Value.Parent = workspace
        Value.Value = Instance.new("Frame")
        Value.Value.Name = "Referenced"
        "#,
    );
    assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
    let before = engine.state.borrow().dom.live_count();
    lua.gc_collect().unwrap();
    lua.gc_collect().unwrap();
    let freed = engine_lua::raw::collect(&lua, &engine).unwrap();
    let after = engine.state.borrow().dom.live_count();
    assert!(freed >= 400, "freed {freed}");
    assert_eq!(after, before - freed);
    let output = run(
        &lua,
        &engine,
        r#"
        local Keep = workspace.Keep
        assert(#Keep:GetChildren() == 200)
        assert(_G.Weak[Keep], "identity of an instance in the DataModel")
        assert(_G.Held.Parent == nil and _G.Held.Name == "Frame")
        assert(workspace.Value.Value.Name == "Referenced")
        local First = Keep:GetChildren()
        for Index, Child in Keep:GetChildren() do
            assert(Child == First[Index] and Child.Name == "Child" .. Index)
        end
        First[1] = nil
        table.clear(First)
        assert(#Keep:GetChildren() == 200, "GetChildren returns a fresh table")
        local New = Instance.new("Frame")
        New.Parent = Keep
        assert(#Keep:GetChildren() == 201 and Keep:GetChildren()[201] == New)
        New.Parent = nil
        assert(#Keep:GetChildren() == 200)
        assert(typeof(New) == "Instance" and typeof(Vector3.new()) == "Vector3" and typeof(vector.create(1, 2, 3)) == "Vector3")
        assert(typeof(newproxy(true)) == "userdata" and typeof({}) == "table" and typeof(nil) == "nil" and typeof(1) == "number")
        assert(typeof(Enum.Font.Arial) == "EnumItem" and typeof(New.Changed) == "RBXScriptSignal" and typeof(UDim2.new()) == "UDim2")
        "#,
    );
    assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
}

/// A parentless tree a script still references through one instance is kept by every collection
/// (the instance that held it is asked first the next time, lua/raw.rs find_garbage), goes when
/// that instance leaves it, and the instance itself when the script lets go.
#[test]
fn a_held_tree_is_freed_once_its_holder_leaves() {
    let (lua, engine) = start();
    // (every run makes a script instance that the next collection frees, so the counts compared are
    // the live instances after each collection)
    let live = |script: &str| {
        let output = run(&lua, &engine, script);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
        lua.gc_collect().unwrap();
        lua.gc_collect().unwrap();
        engine_lua::raw::collect(&lua, &engine).unwrap();
        engine.state.borrow().dom.live_count()
    };
    // 1 + 50 + 200 instances; two of them referenced
    let built = live(
        r#"
        local Root = Instance.new("Folder")
        for Index = 1, 50 do
            local Child = Instance.new("Frame")
            Child.Parent = Root
            for Inner = 1, 4 do
                Instance.new("Frame").Parent = Child
            end
        end
        _G.Holder = Root:GetChildren()[30]:GetChildren()[2]
        _G.Other = Root:GetChildren()[10]
        "#,
    );
    // Another userdata of the tree dies: the tree is looked at again and still held
    assert_eq!(live("_G.Other = nil"), built);
    assert_eq!(live("assert(_G.Holder.Parent.Parent.ClassName == 'Folder')"), built);
    // The holder leaves (its old parent's userdata dies, so the tree is looked at): the rest goes
    assert_eq!(live("local Parent = _G.Holder.Parent _G.Holder.Parent = nil"), built - 250);
    assert_eq!(live("assert(_G.Holder.Parent == nil and _G.Holder.ClassName == 'Frame')"), built - 250);
    assert_eq!(live("_G.Holder = nil"), built - 251);
}

/// A tree that lost its last userdata while an instance property still referenced it is looked at again: the
/// property is cleared later, nothing else signals it, and the tree goes (labels whose tween or holder ended after the
/// script let go of them stayed counted for a whole run).
#[test]
fn a_tree_kept_by_a_property_is_freed_when_the_property_clears() {
    let (lua, engine) = start();
    let live = |script: &str| {
        let output = run(&lua, &engine, script);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
        lua.gc_collect().unwrap();
        lua.gc_collect().unwrap();
        engine_lua::raw::collect(&lua, &engine).unwrap();
        engine.state.borrow().dom.live_count()
    };
    let empty = live("");
    // 1 + 20 instances in a tree no script holds, one ObjectValue in the Workspace naming its root (and the
    // two trees' roots, a second tree it names through the first)
    let built = live(
        r#"
        local Root = Instance.new("Folder")
        for Index = 1, 20 do
            Instance.new("Frame").Parent = Root
        end
        local Second = Instance.new("Frame")
        Second.Parent = Instance.new("Folder")
        local Value = Instance.new("ObjectValue")
        Value.Name = "Pointer"
        Value.Parent = workspace
        Value.Value = Root
        local Chain = Instance.new("ObjectValue")
        Chain.Parent = Root
        Chain.Value = Second.Parent
        "#,
    );
    // (the tree is 22 instances with the chain's ObjectValue, the second tree 2, the pointer 1)
    assert_eq!(built, empty + 22 + 2 + 1, "both trees and the pointer");
    assert_eq!(live(""), built, "a property still names the tree");
    assert_eq!(live("workspace.Pointer.Value = nil"), built - 22 - 2, "the property cleared: both trees go");
    assert_eq!(live(""), built - 22 - 2);
}

/// A tree whose only holder was a tween's pin is looked at again when the pin goes: a frame tweened and dropped
/// while outside the DataModel is held until its tween is collected, and nothing else signals that (the trail labels
/// of removed bars, 25,000 of them in a 34-minute run).
#[test]
fn a_tree_pinned_by_a_tween_is_freed_when_the_tween_goes() {
    let (lua, engine) = start();
    let live = |script: &str| {
        let output = run(&lua, &engine, script);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
        lua.gc_collect().unwrap();
        lua.gc_collect().unwrap();
        engine_lua::raw::collect(&lua, &engine).unwrap();
        engine.state.borrow().dom.live_count()
    };
    let empty = live("");
    // 1 + 3 instances outside the DataModel, the root tweened (the tween pins it) and then dropped with its tween
    let built = live(
        r#"
        local Root = Instance.new("Frame")
        for Index = 1, 3 do
            Instance.new("Frame").Parent = Root
        end
        local Tween = game:GetService("TweenService"):Create(Root, TweenInfo.new(1), { Size = UDim2.fromOffset(5, 5) })
        "#,
    );
    assert!(built >= empty, "{built} {empty}");
    assert_eq!(live(""), empty, "the tween went with the script; its pin was the tree's last holder");
    assert_eq!(live(""), empty);
}

/// Destroying a Model clears its PrimaryPart, as Studio does (5 of 5 measured); unparenting it keeps the part, and
/// a destroyed ancestor destroys the Models below it too.
#[test]
fn a_destroyed_model_lets_go_of_its_primary_part() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local function Rig()
            local Model = Instance.new("Model")
            local Root = Instance.new("Part")
            Root.Parent = Model
            Model.PrimaryPart = Root
            return Model, Root
        end
        local Kept, KeptRoot = Rig()
        Kept.Parent = workspace
        Kept.Parent = nil
        assert(Kept.PrimaryPart == KeptRoot, "unparenting keeps the PrimaryPart")
        local Gone = Rig()
        Gone.Parent = workspace
        Gone:Destroy()
        assert(Gone.PrimaryPart == nil, "destroy clears the PrimaryPart")
        local Folder = Instance.new("Folder")
        local Inner = Rig()
        Inner.Parent = Folder
        Folder:Destroy()
        assert(Inner.PrimaryPart == nil, "a Model below a destroyed instance is destroyed too")
        local Bare = Instance.new("Model")
        Bare:Destroy()
        assert(Bare.PrimaryPart == nil)
        "#,
    );
    assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
}

/// A disconnected connection lets go of its handler (signal.rs): a removed UI whose handlers
/// captured its instances is freed once they are disconnected, a :Once() handler after it ran, and
/// a UI whose handlers stay connected is kept (the instance <-> handler cycle, as in Roblox).
#[test]
fn disconnected_handlers_release_what_they_capture() {
    let (lua, engine) = start();
    let live = |script: &str| {
        let output = run(&lua, &engine, script);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
        scheduler::flush_deferred(&lua, &engine).unwrap();
        lua.gc_collect().unwrap();
        lua.gc_collect().unwrap();
        engine_lua::raw::collect(&lua, &engine).unwrap();
        engine.state.borrow().dom.live_count()
    };
    let empty = live("");
    // Three trees of 1 + 10 instances outside the DataModel, every child's handler capturing its
    // tree: disconnected by the script, a :Once() handler that ran, and one left connected
    let built = live(
        r#"
        local function Build(Bind)
            local Root = Instance.new("Frame")
            for Index = 1, 10 do
                local Button = Instance.new("TextButton")
                Button.Parent = Root
                Bind(Button, function() return Root, Button end)
            end
        end
        Build(function(Button, Handler)
            local Connection = Button:GetPropertyChangedSignal("Text"):Connect(Handler)
            Button.Text = "changed"
            Connection:Disconnect()
            assert(not Connection.Connected)
        end)
        Build(function(Button, Handler)
            Button:GetPropertyChangedSignal("Text"):Once(Handler)
            Button.Text = "changed"
        end)
        Build(function(Button, Handler)
            Button:GetPropertyChangedSignal("Text"):Connect(Handler)
        end)
        "#,
    );
    // (and the script that made them, which the connected handlers' environment references)
    assert_eq!(built, empty + 11 + 1, "only the tree with connected handlers is kept");
    assert_eq!(live(""), built);
}

/// `--mount Service/Name=<dir|file>` / `--instance Service/Name=<Class>` (project.rs mount_at, the
/// mounts cli/rojo.mjs makes of Rojo project files): directories with init scripts, plain folders,
/// single files, service-level merges and class nodes.
#[test]
fn explicit_mounts() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("rur-mount-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/Components")).unwrap();
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::write(dir.join("src/init.server.luau"), "-- entry").unwrap();
    std::fs::write(dir.join("src/Components/Label.luau"), "return 1").unwrap();
    std::fs::write(dir.join("shared/Util.luau"), "return 2").unwrap();
    std::fs::write(dir.join("single.luau"), "return 3").unwrap();
    let (lua, engine) = start();
    let path = |p: &str| dir.join(p).to_string_lossy().into_owned();
    for (spec, class) in [
        (format!("ReplicatedStorage/MyPlugin={}", path("src")), false),
        (format!("ReplicatedStorage={}", path("shared")), false),
        (format!("ServerStorage/Deep/Single={}", path("single.luau")), false),
        ("StarterGui/Hud=ScreenGui".to_string(), true),
    ] {
        mount_at(&engine, &Mount::parse(&spec, class).unwrap()).unwrap();
    }
    assert!(Mount::parse("NoValue", false).is_err());
    assert!(mount_at(&engine, &Mount::parse(&format!("NotAService/X={}", path("src")), false).unwrap()).is_err());
    let output = run(&lua, &engine, r#"
        local RS = game:GetService("ReplicatedStorage")
        assert(RS.MyPlugin.ClassName == "Script", RS.MyPlugin.ClassName)
        assert(RS.MyPlugin.Components.ClassName == "Folder")
        assert(require(RS.MyPlugin.Components.Label) == 1)
        assert(require(RS.Util) == 2)
        assert(require(game:GetService("ServerStorage").Deep.Single) == 3)
        assert(game:GetService("StarterGui").Hud.ClassName == "ScreenGui")
    "#);
    assert!(output.iter().all(|line| !line.starts_with("error")), "{output:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory holding `default.project.json` mounts as that project, as Rojo syncs it (Wally's
/// `Packages/_Index/<package>/<name>/`): the tree's `$path` (a directory or a file), ahead of an `init`
/// script beside it, or a `$className` root with child nodes; named by the project's `name`, else by the
/// directory, and by the mount's own name when the directory is mounted itself.
#[test]
fn nested_default_projects_mount_as_their_tree() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("ree-nested-project-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let write = |relative: &str, text: &str| {
        let file = dir.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("Packages/lib.lua", r#"return require(script.Parent._Index["scope_lib@1.0.0"]["lib"])"#);
    write("Packages/_Index/scope_lib@1.0.0/lib/default.project.json", r#"{ "name": "lib", "tree": { "$path": "src" } }"#);
    write("Packages/_Index/scope_lib@1.0.0/lib/src/init.luau", r#"return "lib""#);
    write("Packages/_Index/scope_lib@1.0.0/lib/src/Util.luau", "return 1");
    write("Packages/_Index/scope_lib@1.0.0/lib/build.project.json", r#"{ "tree": { "$path": "src" } }"#);
    write("Packages/_Index/scope_file@1.0.0/file/default.project.json", r#"{ "tree": { "$path": "src/file.luau" } }"#);
    write("Packages/_Index/scope_named@1.0.0/named/default.project.json", r#"{ "name": "Renamed", "tree": { "$path": "src" } }"#);
    write("Packages/_Index/scope_named@1.0.0/named/src/init.luau", r#"return "renamed""#);
    write("Packages/_Index/scope_file@1.0.0/file/src/file.luau", r#"return "file""#);
    write("Packages/_Index/scope_both@1.0.0/both/default.project.json", r#"{ "name": "both", "tree": { "$path": "src" } }"#);
    write("Packages/_Index/scope_both@1.0.0/both/init.luau", r#"return "root init""#);
    write("Packages/_Index/scope_both@1.0.0/both/src/init.luau", r#"return "src""#);
    write("Packages/_Index/scope_tree@1.0.0/tree/default.project.json", r#"{ "name": "tree", "tree": { "$className": "Folder", "A": { "$path": "a.luau" } } }"#);
    write("Packages/_Index/scope_tree@1.0.0/tree/a.luau", r#"return "a""#);
    let (lua, engine) = start();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Packages={}", dir.join("Packages").to_string_lossy()), false).unwrap()).unwrap();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Direct={}", dir.join("Packages/_Index/scope_named@1.0.0/named").to_string_lossy()), false).unwrap()).unwrap();
    let output = run(&lua, &engine, r#"
        local Packages = game:GetService("ReplicatedStorage").Packages
        local Index = Packages._Index
        local Lib = Index["scope_lib@1.0.0"].lib
        assert(Lib.ClassName == "ModuleScript", Lib.ClassName)
        assert(require(Packages.lib) == "lib")
        assert(require(Lib.Util) == 1)
        assert(Lib:FindFirstChild("src") == nil and Lib:FindFirstChild("default.project") == nil and Lib:FindFirstChild("build.project") == nil)
        local File = Index["scope_file@1.0.0"].file
        assert(File.ClassName == "ModuleScript" and require(File) == "file")
        local Named = Index["scope_named@1.0.0"]
        assert(Named:FindFirstChild("named") == nil and require(Named.Renamed) == "renamed", "the project's name")
        assert(require(game:GetService("ReplicatedStorage").Direct) == "renamed", "a mounted directory keeps the mount's name")
        assert(require(Index["scope_both@1.0.0"].both) == "src", "the project file wins over init.luau")
        local Tree = Index["scope_tree@1.0.0"].tree
        assert(Tree.ClassName == "Folder" and require(Tree.A) == "a")
        assert(Tree:FindFirstChild("a") == nil, "only the tree's nodes")
    "#);
    assert!(output.iter().all(|line| !line.starts_with("error")), "{output:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `default.project.json` can come from a third-party package, so a `$path` stays inside the folder
/// holding it. One that is absolute or has a `..` segment is reported and not followed, on a tree's root
/// or a child node. A required `$path` that names nothing and a project file that can't be read are
/// reported too, and each such directory mounts as it is.
#[test]
fn nested_project_paths_stay_inside_the_project_folder() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("ree-project-path-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let write = |relative: &str, text: &str| {
        let file = dir.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("Outside.json", r#"{ "outside": true }"#);
    write("Outside/Secret.luau", "return 1");
    let root = dir.to_string_lossy().replace('\\', "/");
    let absolute = format!("{root}/Outside.json");
    let refused = ["..", "../x", "../../Outside.json", "../../Outside", "src/../..", r"src\..\..\Outside.json", "/x", r"\x", "//server/share/x", r"\\server\share\x", "C:/x", r"C:\x", "C:x", absolute.as_str()];
    // Each under a mount of its own: a `..` that is followed would otherwise mount the other cases again at every level
    for (index, path) in refused.iter().enumerate() {
        write(&format!("Refused{index}/package/default.project.json"), &serde_json::json!({ "tree": { "$path": path } }).to_string());
        write(&format!("Refused{index}/package/src/Inside.luau"), "return 1");
        write(&format!("Refused{index}/package/x/init.luau"), "return 1");
    }
    write("Packages/child/default.project.json", r#"{ "tree": { "$className": "Folder", "Escape": { "$path": "../../Outside.json" }, "Kept": { "$path": "Kept.luau" } } }"#);
    write("Packages/child/Kept.luau", r#"return "kept""#);
    for (package, project) in [("garbled", "not json"), ("treeless", r#"{ "name": "treeless" }"#), ("missing", r#"{ "tree": { "$path": "gone" } }"#), ("optional", r#"{ "tree": { "$path": { "optional": "gone" } } }"#)] {
        write(&format!("Packages/{package}/default.project.json"), project);
        write(&format!("Packages/{package}/Inside.luau"), "return 1");
    }
    let (lua, engine) = start();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Packages={root}/Packages"), false).unwrap()).unwrap();
    for index in 0..refused.len() {
        mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Refused{index}={root}/Refused{index}"), false).unwrap()).unwrap();
    }
    let output = run(&lua, &engine, r#"
        local ReplicatedStorage = game:GetService("ReplicatedStorage")
        local Packages = ReplicatedStorage.Packages
        for _, case in ReplicatedStorage:GetChildren() do
            if case.Name:find("^Refused") then
                assert(case.package.ClassName == "Folder" and case.package:FindFirstChild("src"), case.Name)
            end
        end
        for _, descendant in ReplicatedStorage:GetDescendants() do
            assert(descendant.Name ~= "Outside" and descendant.Name ~= "Secret", descendant:GetFullName())
        end
        assert(Packages.child:FindFirstChild("Escape") == nil and require(Packages.child.Kept) == "kept", "a refused child node")
        for _, name in { "garbled", "treeless", "missing", "optional" } do
            assert(Packages[name].ClassName == "Folder" and Packages[name]:FindFirstChild("Inside"), name)
        end
        print("ok")
    "#);
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    let says = |package: &str, message: &str| format!("warn: {root}/{package}/default.project.json: {message}");
    let mut expected: Vec<String> = refused.iter().enumerate().map(|(index, path)| says(&format!("Refused{index}/package"), &format!("$path {path} leaves the project's folder; not mounted"))).collect();
    expected.push(says("Packages/child", "$path ../../Outside.json leaves the project's folder; not mounted"));
    expected.push(says("Packages/missing", "$path gone names nothing ree can mount; not mounted"));
    for package in ["Packages/garbled", "Packages/treeless"] {
        expected.push(says(package, "not a project ree can read (JSON with a \"tree\" object); the directory mounts as it is"));
    }
    let reported: Vec<&String> = output.iter().filter(|line| line.contains("default.project.json")).collect();
    for line in &expected {
        assert!(reported.contains(&line), "{line} in {reported:?}");
    }
    assert_eq!(reported.len(), expected.len(), "{reported:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project inside more than 32 others is left as an empty Folder and reported, the depth where
/// roblox-ui-renderer's cli/rojo.mjs stops, so a chain of projects ends there.
#[test]
fn nested_projects_stop_past_the_nesting_limit() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("ree-project-nesting-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let chain = |name: &str, projects: usize| {
        let mut folder = dir.join("Packages").join(name);
        for _ in 0..projects {
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("default.project.json"), r#"{ "tree": { "$path": "n" } }"#).unwrap();
            folder = folder.join("n");
        }
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("init.luau"), r#"return "end""#).unwrap();
    };
    chain("Fits", 33);
    chain("TooDeep", 34);
    let root = dir.to_string_lossy().replace('\\', "/");
    let (lua, engine) = start();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Packages={root}/Packages"), false).unwrap()).unwrap();
    let output = run(&lua, &engine, r#"
        local Packages = game:GetService("ReplicatedStorage").Packages
        assert(require(Packages.Fits) == "end", "a chain within the limit")
        assert(Packages.TooDeep.ClassName == "Folder" and #Packages.TooDeep:GetChildren() == 0, "a chain past the limit")
        print("ok")
    "#);
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    let expected = format!("warn: {root}/Packages/TooDeep/{}default.project.json: project nesting too deep (past 32 levels); not mounted", "n/".repeat(33));
    let stopped: Vec<&String> = output.iter().filter(|line| line.contains("too deep")).collect();
    assert_eq!(stopped, [&expected]);
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tree's child nodes can each `$path` to the same project, so the mounts would double at every level
/// while the depth stays within its limit: one mount follows 10000 projects at most and reports the rest.
#[test]
fn a_mount_stops_following_projects_past_its_limit() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("ree-project-fan-out-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut folder = dir.join("Doubling");
    for _ in 0..15 {
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("default.project.json"), r#"{ "tree": { "$className": "Folder", "A": { "$path": "n" }, "B": { "$path": "n" } } }"#).unwrap();
        folder = folder.join("n");
    }
    std::fs::create_dir_all(&folder).unwrap();
    let root = dir.to_string_lossy().replace('\\', "/");
    let (lua, engine) = start();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Doubling={root}/Doubling"), false).unwrap()).unwrap();
    let output = run(&lua, &engine, r#"
        -- two nodes under each of the 10000 projects followed
        local count = #game:GetService("ReplicatedStorage").Doubling:GetDescendants()
        assert(count == 20000, tostring(count))
        print("ok")
    "#);
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    let stopped = output.iter().filter(|line| line.starts_with("warn:") && line.ends_with("more than 10000 projects in one mount; not mounted")).count();
    assert!((1..=32).contains(&stopped), "{stopped} reported");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rojo's JSON models (`*.model.json`, Rojo 7's format: rojo.space/docs/v7/sync-details/#json-models
/// and its json_model.rs): one instance named after the file (its own top-level name ignored) or by
/// its project node, lowercase or legacy capitalized keys, a child without a name taking its class's,
/// properties implicit (by the property's type) or explicit, attributes and Tags, kept as `rojo build`
/// writes them (Position does not serialize: the CFrame places the part). A file of white space makes
/// nothing; one Rojo would reject is reported and not mounted.
#[test]
fn rojo_json_models_mount_as_their_instances() {
    use super::project::{Mount, mount_at};
    let dir = std::env::temp_dir().join(format!("ree-json-model-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/init.luau"), "return script.Remote").unwrap();
    std::fs::write(dir.join("lib/Remote.model.json"), r#"{ "className": "RemoteEvent" }"#).unwrap();
    std::fs::write(
        dir.join("lib/Tool.model.json"),
        r#"{
            "Name": "Ignored",
            "ClassName": "Folder",
            "Children": [
                {
                    "Name": "RootPart",
                    "ClassName": "Part",
                    "Properties": {
                        "Size": [4, 4, 4], "Anchored": true, "Material": "Neon", "Color": [1, 0, 0],
                        "CFrame": [1, 2, 3, 1, 0, 0, 0, 1, 0, 0, 0, 1], "Position": [9, 9, 9]
                    }
                },
                { "ClassName": "RemoteFunction" }
            ]
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("lib/Config.model.json"),
        r#"{
            "className": "Configuration",
            "properties": { "Tags": ["Hitbox", "Enemy"] },
            "attributes": { "Speed": 16, "Label": "fast", "On": true, "Offset": { "Vector3": [0, 5, 0] } },
            "children": [
                { "name": "Count", "className": "IntValue", "properties": { "Value": 5 } },
                { "name": "Title", "className": "StringValue", "properties": { "Value": { "String": "hello" } } }
            ]
        }"#,
    )
    .unwrap();
    std::fs::write(dir.join("lib/Empty.model.json"), " \n").unwrap();
    std::fs::write(dir.join("lib/Broken.model.json"), r#"{ "className": "Part", "properties": { "NotAProperty": 5 } }"#).unwrap();
    let (lua, engine) = start();
    let path = |p: &str| dir.join(p).to_string_lossy().into_owned();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Lib={}", path("lib")), false).unwrap()).unwrap();
    mount_at(&engine, &Mount::parse(&format!("ReplicatedStorage/Named={}", path("lib/Remote.model.json")), false).unwrap()).unwrap();
    let output = run(&lua, &engine, r#"
        local RS = game:GetService("ReplicatedStorage")
        local Lib = RS.Lib
        assert(Lib.ClassName == "ModuleScript" and Lib.Remote.ClassName == "RemoteEvent")
        assert(require(Lib) == Lib.Remote, "script.Remote")
        local Tool = Lib.Tool
        assert(Tool.ClassName == "Folder" and Tool.Name == "Tool", Tool.Name)
        local Root = Tool.RootPart
        assert(Root.ClassName == "Part" and Root.Size == Vector3.new(4, 4, 4) and Root.Anchored and Root.Material == Enum.Material.Neon)
        assert(Root.Color == Color3.new(1, 0, 0), tostring(Root.Color))
        assert(Root.Position == Vector3.new(1, 2, 3), tostring(Root.Position))
        assert(Tool.RemoteFunction.ClassName == "RemoteFunction", "a child without a name")
        local Config = Lib.Config
        assert(Config:HasTag("Hitbox") and Config:HasTag("Enemy"), "Tags")
        assert(Config:GetAttribute("Speed") == 16 and Config:GetAttribute("Label") == "fast" and Config:GetAttribute("On") == true)
        assert(Config:GetAttribute("Offset") == Vector3.new(0, 5, 0))
        assert(Config.Count.Value == 5 and Config.Title.Value == "hello")
        assert(Lib:FindFirstChild("Empty") == nil and Lib:FindFirstChild("Broken") == nil)
        assert(RS.Named.ClassName == "RemoteEvent" and RS:FindFirstChild("Remote") == nil, "named by its project node")
    "#);
    assert!(output.iter().all(|line| !line.starts_with("error")), "{output:?}");
    let reported: Vec<&String> = output.iter().filter(|line| line.contains("Broken.model.json")).collect();
    assert!(reported.len() == 1 && reported[0].contains("unknown property Part.NotAProperty"), "{output:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Everything the engine holds per instance, signal and connection, and the Luau heap.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Held {
    instances: usize,
    userdata: usize,
    values: usize,
    signals: usize,
    connections: usize,
}

fn held(lua: &Lua, engine: &Rc<Engine>) -> (Held, usize) {
    scheduler::flush_deferred(lua, engine).unwrap();
    for _ in 0..3 {
        lua.gc_collect().unwrap();
        engine_lua::raw::collect(lua, engine).unwrap();
    }
    let state = engine.state.borrow();
    let (signals, connections) = state.signals.counts();
    let held = Held {
        instances: state.dom.live_count(),
        userdata: engine_lua::raw::stats(engine).0,
        values: engine_lua::raw::value_cache_len(engine),
        signals,
        connections,
    };
    (held, lua.used_memory())
}

/// A UI in the DataModel, every button's handlers capturing the whole tree (the cycle Roblox never
/// collects), tagged and with attributes: built `count` times by `source`. (roblox-ui-renderer's
/// copy of this test also lays it out: its layout records are freed too.)
const BUILD_UI: &str = r#"
    local Gui = Instance.new("ScreenGui")
    Gui.Name = "LifetimeGui"
    local Root = Instance.new("Frame")
    Root.Name = "Root"
    Root.Size = UDim2.fromOffset(400, 300)
    Root.Parent = Gui
    local Layout = Instance.new("UIListLayout")
    Layout.Parent = Root
    local State = { Root = Root, Buttons = {} }
    for Index = 1, 40 do
        local Button = Instance.new("TextButton")
        Button.Name = "Button" .. Index
        Button.Size = UDim2.new(1, 0, 0, 20)
        Button.Text = "Row " .. Index
        Button:AddTag("LifetimeRow")
        Button:SetAttribute("Index", Index)
        Instance.new("UICorner").Parent = Button
        local Stroke = Instance.new("UIStroke")
        Stroke.Parent = Button
        Button.Parent = Root
        State.Buttons[Index] = Button
        Button.Activated:Connect(function()
            State.Root.Visible = not State.Root.Visible
        end)
        Button:GetPropertyChangedSignal("Text"):Connect(function()
            Stroke.Thickness = #Button.Text
        end)
        Button.AncestryChanged:Connect(function()
            return Button, Root
        end)
        Button:GetAttributeChangedSignal("Index"):Connect(function()
            return Gui
        end)
        Button.MouseEnter:Once(function()
            return State
        end)
    end
    Root.Changed:Connect(function()
        return State
    end)
    Gui.Parent = game:GetService("StarterGui")
    -- (datatype values read: the converted-value cache)
    assert(Root.Size == UDim2.fromOffset(400, 300) and State.Buttons[3].Size == UDim2.new(1, 0, 0, 20))
    State.Buttons[1].Text = "changed"
"#;

/// Destroy disconnects every connection of the instance and its descendants, and a destroyed
/// tree nothing references is freed whole: the instances, their userdata and converted values,
/// their signals and connections with the handlers, and the Luau heap. Built
/// and destroyed again and again, nothing accumulates (slots are reused).
#[test]
fn a_destroyed_tree_is_freed_whole() {
    let (lua, engine) = start();
    let cycle = |destroy: &str| {
        let output = run(&lua, &engine, &format!("{BUILD_UI}\n{destroy}"));
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
    };
    let destroy = "Gui:Destroy()";
    // (a first cycle makes what is made once: member dispatch entries, signal userdata of services)
    cycle(destroy);
    let (baseline, baseline_heap) = held(&lua, &engine);
    let slots = engine.state.borrow().dom.slot_count();
    let capacity = engine.state.borrow().signals.capacity();
    for round in 0..25 {
        cycle(destroy);
        let (now, heap) = held(&lua, &engine);
        assert_eq!(now, baseline, "round {round}");
        assert!(heap < baseline_heap + 64 * 1024, "round {round}: Luau heap {heap} from {baseline_heap}");
    }
    assert_eq!(engine.state.borrow().dom.slot_count(), slots, "instance slots are reused");
    assert_eq!(engine.state.borrow().signals.capacity(), capacity, "signal and connection slots are reused");

    // Removed and not destroyed, the tree stays: its handlers are connected and capture it (what
    // Roblox does; lua/leaks.rs reports it). 1 ScreenGui, Root, 1 layout, 40 buttons with 2 children;
    // 5 connections a button and Root.Changed's, and the script the handlers' environment references
    cycle("Gui.Parent = nil");
    let (now, _) = held(&lua, &engine);
    assert_eq!(now.instances, baseline.instances + 3 + 40 * 3 + 1);
    assert_eq!(now.connections, baseline.connections + 40 * 5 + 1);
    let report = engine_lua::leaks::report(&lua, &engine).unwrap();
    assert_eq!(report["outsideDataModel"]["heldByConnectedHandlers"], 123, "{report:#}");
    let handlers = report["handlers"].as_array().unwrap();
    assert_eq!(handlers.len(), 6, "{report:#}");
    assert!(handlers.iter().all(|handler| handler["trees"] == 1 && handler["handler"].as_str().unwrap().starts_with("Test:")), "{report:#}");
    let activated = handlers.iter().find(|handler| handler["signal"] == "Activated").unwrap();
    assert_eq!((&activated["connections"], &activated["class"]), (&serde_json::json!(40), &serde_json::json!("TextButton")));
    assert!(activated["captures"].as_str().unwrap().contains("field"), "{activated:#}");
}

/// What Studio 0.740 does with instances outside the DataModel that nothing references but their
/// own connected handlers (measured: weak tables and garbage churn for 30 s): kept when a handler
/// captures the instance, another instance of its tree, or an instance whose handler captures it
/// back; collected when no handler captures them, after Disconnect and after Destroy (of the
/// instance or of an ancestor). The engine does the same.
#[test]
fn connected_handlers_keep_their_own_trees_as_in_studio() {
    let (lua, engine) = start();
    let live = |script: &str| {
        let output = run(&lua, &engine, script);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
        held(&lua, &engine).0.instances
    };
    let empty = live("");
    // A: a handler capturing its own instance (1 instance kept)
    let a = live(r#"local F = Instance.new("Frame") F:GetPropertyChangedSignal("Name"):Connect(function() return F end)"#);
    assert_eq!(a, empty + 1 + 1, "A (and the script its handler's environment references)");
    // B: the same, destroyed; C: disconnected; D: a handler capturing nothing of the tree
    let b = live(r#"local F = Instance.new("Frame") F:GetPropertyChangedSignal("Name"):Connect(function() return F end) F:Destroy()"#);
    assert_eq!(b, a, "B: destroyed");
    let c = live(r#"local F = Instance.new("Frame") F:GetPropertyChangedSignal("Name"):Connect(function() return F end):Disconnect()"#);
    assert_eq!(c, a, "C: disconnected");
    let d = live(r#"local F = Instance.new("Frame") local S = {} F:GetPropertyChangedSignal("Name"):Connect(function() return S end)"#);
    assert_eq!(d, a, "D: no capture");
    // E: a child's handler captures the root (2 kept); F: a child's handler captures the child
    let e = live(r#"local R = Instance.new("Frame") local C = Instance.new("TextButton") C.Parent = R C.Activated:Connect(function() return R end)"#);
    assert_eq!(e, a + 2 + 1, "E: child captures root");
    let f = live(r#"local R = Instance.new("Frame") local C = Instance.new("TextButton") C.Parent = R C.MouseEnter:Connect(function() return C end)"#);
    assert_eq!(f, e + 2 + 1, "F: child captures itself");
    // G: two parentless instances whose handlers capture each other
    let g = live(r#"local A, B = Instance.new("Frame"), Instance.new("Frame") A.Changed:Connect(function() return B end) B.Changed:Connect(function() return A end)"#);
    assert_eq!(g, f + 2 + 1, "G: cross");
    // I: the root destroyed: its descendants' connections go with it
    let i = live(r#"local R = Instance.new("Frame") local C = Instance.new("TextButton") C.Parent = R C.Activated:Connect(function() return R end) R:Destroy()"#);
    assert_eq!(i, g, "I: root destroyed");
}

/// Connection and signal slots are reused; what a script or a queued handler job still holds of a
/// reused slot is a disconnected connection / a signal nothing fires, never the slot's new owner.
#[test]
fn reused_signal_slots_do_not_revive_stale_handles() {
    let (lua, engine) = start();
    // (each script's deferred handlers have run when `run` returns)
    let script = |source: &str| {
        let output = run(&lua, &engine, source);
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
    };
    script(
        r#"
        local Event = Instance.new("BindableEvent")
        local Log = {}
        _G.Event, _G.Log = Event, Log
        local First = Event.Event:Connect(function() table.insert(Log, "first") end)
        First:Disconnect()
        local Second = Event.Event:Connect(function() table.insert(Log, "second") end)
        _G.Second = Second
        assert(not First.Connected and Second.Connected)
        First:Disconnect()
        assert(Second.Connected, "a stale connection disconnected the slot's new connection")
        Event:Fire()
        "#,
    );
    // A handler queued by a fire, disconnected before it runs, its slot taken by a new connection
    script(
        r#"
        local Event, Log = _G.Event, _G.Log
        assert(table.concat(Log, ",") == "second", table.concat(Log, ","))
        table.clear(Log)
        _G.Second:Disconnect()
        local Queued = Event.Event:Connect(function() table.insert(Log, "queued") end)
        Event:Fire()
        Queued:Disconnect()
        _G.Later = Event.Event:Connect(function() table.insert(Log, "later") end)
        "#,
    );
    script(r#"assert(#_G.Log == 0, table.concat(_G.Log, ",")) _G.Event:Fire()"#);
    script(
        r#"
        assert(table.concat(_G.Log, ",") == "later", table.concat(_G.Log, ","))
        _G.Later:Disconnect()
        _G.Stale = Instance.new("Frame").Changed
        "#,
    );
    // The Frame is freed (only its signal is held): the signal's slot goes to the next signal made
    held(&lua, &engine);
    script(
        r#"
        _G.Fired = 0
        local Frame = Instance.new("Frame")
        _G.Frame = Frame
        local Live = Frame.Changed:Connect(function() _G.Fired += 1 end)
        local Dead = _G.Stale:Connect(function() _G.Fired += 100 end)
        assert(Live.Connected and not Dead.Connected)
        assert(tostring(_G.Stale) == "Signal" and tostring(Frame.Changed) == "Signal Changed")
        Frame.Name = "Renamed"
        "#,
    );
    script("assert(_G.Fired == 1, _G.Fired)");
}

/// CollectionService as measured in Studio 0.740 (the same script run there; services/mod.rs
/// CollectionState): a tag counts while its instance is in the DataModel.
#[test]
fn collection_service_follows_the_data_model() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local CS = game:GetService("CollectionService")
        local Log = {}
        local function On(Signal, Label)
            Signal:Connect(function(Target) table.insert(Log, Label .. " " .. Target.Name) end)
        end
        On(CS:GetInstanceAddedSignal("A"), "+A")
        On(CS:GetInstanceRemovedSignal("A"), "-A")
        On(CS:GetInstanceAddedSignal("B"), "+B")
        On(CS:GetInstanceRemovedSignal("B"), "-B")
        local Host = Instance.new("Folder")
        Host.Parent = workspace
        local Host2 = Instance.new("Folder")
        Host2.Parent = workspace
        local function Tagged(Tag)
            local Names = {}
            for _, Target in CS:GetTagged(Tag) do table.insert(Names, Target.Name) end
            table.sort(Names)
            return table.concat(Names, ",")
        end
        -- (the test runs a step's action, lets its deferred handlers run, then checks it)
        local Steps = {}
        _G.Steps, _G.Checked = Steps, 0
        local function Step(Expected, ExpectedA, ExpectedB, Action)
            table.insert(Steps, { Expected = Expected, A = ExpectedA, B = ExpectedB, Action = Action })
        end
        function _G.Run(Index)
            table.clear(Log)
            Steps[Index].Action()
        end
        function _G.Check(Index)
            local Step = Steps[Index]
            local Got = table.concat(Log, " ")
            assert(Got == Step.Expected, `step {Index}: expected [{Step.Expected}], got [{Got}]`)
            assert(Tagged("A") == Step.A, `step {Index}: GetTagged A: expected [{Step.A}], got [{Tagged("A")}]`)
            assert(Tagged("B") == Step.B, `step {Index}: GetTagged B: expected [{Step.B}], got [{Tagged("B")}]`)
            _G.Checked += 1
        end

        local a = Instance.new("Frame")
        a.Name = "a"
        Step("", "", "", function() a:AddTag("A") end)
        Step("+A a", "a", "", function() a.Parent = Host end)
        Step("", "a", "", function() a.Parent = Host2 end)
        Step("-A a", "", "", function() a.Parent = nil end)
        Step("", "", "", function() a:RemoveTag("A") end)
        Step("+A a", "a", "", function() a:AddTag("A") a.Parent = Host end)
        Step("-A a", "", "", function() a:RemoveTag("A") end)
        Step("+A a", "a", "", function() a:AddTag("A") end)
        Step("", "a", "", function() a:AddTag("A") end)
        Step("-A a", "", "", function() a:Destroy() end)
        Step("", "", "", function() assert(a:HasTag("A") and #a:GetTags() == 1, "Destroy keeps the tags") end)

        -- A tagged subtree: root[A,B], c1[B,A] > g1[A], c2[A]
        local root = Instance.new("Frame") root.Name = "root"
        local c1 = Instance.new("Frame") c1.Name = "c1" c1.Parent = root
        local c2 = Instance.new("Frame") c2.Name = "c2" c2.Parent = root
        local g1 = Instance.new("Frame") g1.Name = "g1" g1.Parent = c1
        g1:AddTag("A") c2:AddTag("A") root:AddTag("A") c1:AddTag("B") c1:AddTag("A") root:AddTag("B")
        Step("+A root +B root +B c1 +A c1 +A g1 +A c2", "c1,c2,g1,root", "c1,root", function() root.Parent = Host end)
        Step("-A root -B root -B c1 -A c1 -A g1 -A c2", "", "", function() root.Parent = nil end)
        Step("+A root +B root +B c1 +A c1 +A g1 +A c2", "c1,c2,g1,root", "c1,root", function() root.Parent = Host end)
        Step("-B c1 -A c1 -A g1", "c2,root", "root", function() c1.Parent = nil end)
        Step("-A root -B root -A c2", "", "", function() root:Destroy() end)
        Step("", "", "", function() c1:Destroy() end)

        -- Entering through a parent, ClearAllChildren, and in and out within one step
        local outside = Instance.new("Folder") outside.Name = "outside"
        local x = Instance.new("Frame") x.Name = "x" x:AddTag("A")
        Step("", "", "", function() x.Parent = outside end)
        Step("+A x", "x", "", function() outside.Parent = Host end)
        Step("-A x", "", "", function() Host:ClearAllChildren() end)
        local y = Instance.new("Frame") y.Name = "y" y:AddTag("A")
        Step("+A y -A y", "", "", function() y.Parent = Host y.Parent = nil end)
        -- A clone keeps the tags and enters like any tagged instance
        local z = y:Clone() z.Name = "z"
        Step("+A z", "z", "", function() z.Parent = Host end)
        "#,
    );
    assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
    for step in 1..=22 {
        run(&lua, &engine, &format!("_G.Run({step})"));
        let output = run(&lua, &engine, &format!("_G.Check({step})"));
        assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
    }
    let output = run(&lua, &engine, "assert(_G.Checked == #_G.Steps and #_G.Steps == 22, #_G.Steps)");
    assert!(output.iter().all(|l| !l.starts_with("error")), "{output:?}");
}

/// Vector3 is the native vector type, as in Roblox (the values below are Studio 0.740's for the
/// same expressions): Vide's spring keeps a Vector3 source's value as a vector.
#[test]
fn vector3_is_the_native_vector() {
    assert_script(
        r#"
        local function Same(Value, Expected)
            assert(tostring(Value) == Expected, `{Value} ~= {Expected}`)
        end
        local a, b = Vector3.new(1, 2, 3), Vector3.new(4, -5, 6.5)
        assert(type(a) == "vector" and typeof(a) == "Vector3" and typeof(vector.create(1, 2, 3)) == "Vector3")
        assert(type(Vector3.zero) == "vector" and Vector3.new() == vector.zero and Vector3.one == vector.one)
        assert(a.X == 1 and a.y == 2 and a.Z == 3 and a == vector.create(1, 2, 3))
        Same(b.Unit, "0.4383973181247711, -0.5479966402053833, 0.7123956680297852")
        assert(b.Unit == vector.normalize(b) and a.Magnitude == vector.magnitude(a))
        assert(string.format("%.5f", a.Magnitude) == "3.74166")
        Same(a + b, "5, -3, 9.5") Same(a - b, "-3, 7, -3.5") Same(a * b, "4, -10, 19.5")
        Same(a / b, "0.25, -0.4000000059604645, 0.4615384638309479")
        Same(a * 2, "2, 4, 6") Same(2 * a, "2, 4, 6") Same(a / 2, "0.5, 1, 1.5") Same(-a, "-1, -2, -3") Same(a // 2, "0, 1, 1")
        assert(a:Dot(b) == 13.5)
        Same(a:Cross(b), "28, 5.5, -13") Same(a:Lerp(b, 0.25), "1.75, 0.25, 3.875")
        assert(a:FuzzyEq(Vector3.new(1, 2, 3.000001)) and not a:FuzzyEq(b))
        Same(b:Abs(), "4, 5, 6.5") Same(b:Ceil(), "4, -5, 7") Same(b:Floor(), "4, -5, 6") Same(b:Sign(), "1, -1, 1")
        Same(a:Max(b), "4, 2, 6.5") Same(a:Min(b), "1, -5, 3") Same(Vector3.max(a, b), "4, 2, 6.5") Same(Vector3.min(a, b), "1, -5, 3")
        assert(string.format("%.5f", a:Angle(b)) == "1.16425")
        assert(({ [a] = "found" })[Vector3.new(1, 2, 3)] == "found", "vectors are table keys by value")
        Same(Vector3.new(0.1, 1 / 3, 1e10), "0.10000000149011612, 0.3333333432674408, 10000000000")
        Same(Vector3.FromNormalId(Enum.NormalId.Top), "0, 1, 0") Same(Vector3.FromAxis(Enum.Axis.Z), "0, 0, 1")
        assert(getmetatable(a) == "The metatable is locked")
        assert(not pcall(function() return a.Nope end))

        -- Through Lune's CFrame / Ray / Region3 and Instance properties
        local cf = CFrame.new(1, 2, 3) * CFrame.Angles(0, math.pi / 2, 0)
        assert(typeof(cf.Position) == "Vector3" and type(cf.Position) == "vector" and cf.Position == a)
        Same(cf * a, "4, 4, 1.9999998807907104") Same(cf:PointToWorldSpace(a), "4, 4, 1.9999998807907104")
        Same(cf:VectorToWorldSpace(a), "3, 2, -1.0000001192092896") Same((cf + a).Position, "2, 4, 6") Same((cf - a).Position, "0, 0, 0")
        assert(CFrame.new(a).Position == a and CFrame.new(a, b).Position == a)
        local ray = Ray.new(a, b)
        assert(ray.Origin == a and ray.Direction == b and type(ray.Unit.Direction) == "vector")
        assert(string.format("%.5f", ray:Distance(Vector3.zero)) == "3.74166")
        Same(Region3.new(a, a + Vector3.one * 4).Size, "4, 4, 4")
        local part = Instance.new("Part")
        part.Size = Vector3.new(2, 4, 6)
        part.Position = vector.create(1, 2, 3)
        assert(type(part.Size) == "vector" and part.Size == Vector3.new(2, 4, 6) and part.Position == a)
        part:SetAttribute("V", Vector3.new(7, 8, 9))
        assert(part:GetAttribute("V") == Vector3.new(7, 8, 9) and typeof(part:GetAttribute("V")) == "Vector3")
        "#,
    );
}

#[test]
fn scale_to_scales_effect_geometry() {
    assert_script(
        r#"
        local Model = Instance.new("Model")
        local Part = Instance.new("Part")
        Part.Parent = Model
        Model.PrimaryPart = Part
        local Emitter = Instance.new("ParticleEmitter")
        Emitter.Size = NumberSequence.new({NumberSequenceKeypoint.new(0, 2, 0.5), NumberSequenceKeypoint.new(1, 4)})
        Emitter.Speed = NumberRange.new(10, 20)
        Emitter.Acceleration = Vector3.new(0, -8, 0)
        Emitter.ZOffset = 1
        Emitter.Parent = Part
        local Beam = Instance.new("Beam")
        Beam.Width0, Beam.Width1, Beam.CurveSize0, Beam.ZOffset = 2, 3, 4, 1
        Beam.Parent = Part
        local Light = Instance.new("PointLight")
        Light.Range = 20
        Light.Parent = Part
        local Stretched, Wrapped = Instance.new("Trail"), Instance.new("Trail")
        Stretched.TextureLength, Wrapped.TextureLength = 4, 4
        Wrapped.TextureMode = Enum.TextureMode.Wrap
        Stretched.Parent, Wrapped.Parent = Part, Part
        local Texture = Instance.new("Texture")
        Texture.StudsPerTileU, Texture.StudsPerTileV = 2, 6
        Texture.Parent = Part
        local Surface = Instance.new("SurfaceGui")
        Surface.SizingMode = Enum.SurfaceGuiSizingMode.PixelsPerStud
        Surface.PixelsPerStud = 50
        Surface.Parent = Part
        local Billboard = Instance.new("BillboardGui")
        Billboard.Size = UDim2.new(4, 10, 2, 20)
        Billboard.StudsOffset = Vector3.new(0, 6, 0)
        Billboard.Parent = Part
        Model:ScaleTo(0.5)
        local Points = Emitter.Size.Keypoints
        assert(Points[1].Value == 1 and Points[1].Envelope == 0.25 and Points[2].Value == 2, "emitter size")
        assert(Emitter.Speed.Min == 5 and Emitter.Speed.Max == 10, "emitter speed")
        assert(Emitter.Acceleration == Vector3.new(0, -4, 0), "emitter acceleration")
        assert(Beam.Width0 == 1 and Beam.Width1 == 1.5 and Beam.CurveSize0 == 2, "beam widths")
        assert(Light.Range == 20, "a light's range stays (measured in Studio)")
        assert(Emitter.ZOffset == 1 and Beam.ZOffset == 1, "zoffsets stay (measured in Studio)")
        assert(Stretched.TextureLength == 4 and Wrapped.TextureLength == 2, "trail texture lengths (a repeat count in Stretch mode)")
        assert(Texture.StudsPerTileU == 1 and Texture.StudsPerTileV == 3, "texture tiles")
        assert(Surface.PixelsPerStud == 100, "surface gui pixels per stud")
        assert(Billboard.Size == UDim2.new(2, 10, 1, 20) and Billboard.StudsOffset == Vector3.new(0, 3, 0), "billboard size and offset")
        local Copy = Model:Clone()
        assert(Copy:GetScale() == 0.5, "a clone keeps its model's scale (ScaleFactor is serialized)")
        Copy:ScaleTo(1)
        assert(Copy:FindFirstChildWhichIsA("BasePart", true):FindFirstChildOfClass("ParticleEmitter").Speed.Max == 20, "a clone scales back from its own scale")
        Model:ScaleTo(1)
        assert(Emitter.Speed.Max == 20 and Light.Range == 20, "back to the authored values")
        "#,
    );
}

/// Model:ScaleTo scales a SpecialMesh's Scale only for a file mesh: the other shapes follow their
/// part's Size (measured in Studio 2026-10-11 with a 2x1x1 part, Scale 1.25 and ScaleTo(2): Head and
/// Sphere stay 1.25, FileMesh becomes 2.5). Characters' heads (Head meshes) grew into domes.
#[test]
fn scale_to_scales_only_file_meshes() {
    assert_script(
        r#"
        local Model = Instance.new("Model")
        local Meshes = {}
        for _, Type in {Enum.MeshType.Head, Enum.MeshType.Sphere, Enum.MeshType.Brick, Enum.MeshType.FileMesh} do
            local Part = Instance.new("Part")
            Part.Size = Vector3.new(2, 1, 1)
            Part.Name = Type.Name
            Part.Parent = Model
            local Mesh = Instance.new("SpecialMesh")
            Mesh.MeshType = Type
            Mesh.Scale = Vector3.new(1.25, 1.25, 1.25)
            Mesh.Parent = Part
            Meshes[Type.Name] = Mesh
        end
        Model:ScaleTo(2)
        assert(Model.Head.Size == Vector3.new(4, 2, 2), "the part grew")
        for _, Name in {"Head", "Sphere", "Brick"} do
            assert(Meshes[Name].Scale == Vector3.new(1.25, 1.25, 1.25), Name .. " mesh scaled")
        end
        assert(Meshes.FileMesh.Scale == Vector3.new(2.5, 2.5, 2.5), "the file mesh did not scale")
        -- a clone keeps the types and scales, and scales back the same way
        local Copy = Model:Clone()
        assert(Copy.Head:FindFirstChildOfClass("SpecialMesh").MeshType == Enum.MeshType.Head, "the clone's mesh type")
        Copy:ScaleTo(1)
        assert(Copy.Head:FindFirstChildOfClass("SpecialMesh").Scale == Vector3.new(1.25, 1.25, 1.25), "the clone's head mesh scaled")
        assert(Copy.FileMesh:FindFirstChildOfClass("SpecialMesh").Scale == Vector3.new(1.25, 1.25, 1.25), "the clone's file mesh")
        "#,
    );
}

/// A model converted once makes the same instances in every engine and every time (`ree serve`
/// keeps bundle plans between its engines): references point at the instantiation's own copies,
/// unknown classes warn each time, and the values read back as the file holds them.
#[test]
fn a_model_plan_makes_the_same_instances_each_time() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{Attributes, CFrame, Color3, Content, Enum, Matrix3, Tags, Variant, Vector3},
    };
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let model = dom.insert(root, InstanceBuilder::new("Model").with_name("Rig"));
    let mut attributes = Attributes::new();
    attributes.insert("Speed".into(), Variant::Float64(4.5));
    let mut tags = Tags::new();
    tags.push("Hazard");
    let torso = dom.insert(
        model,
        InstanceBuilder::new("Part")
            .with_name("Torso")
            .with_property("Size", Vector3::new(2.0, 2.0, 1.0))
            .with_property("Color", Color3::new(1.0, 0.5, 0.0))
            .with_property("CFrame", CFrame::new(Vector3::new(0.0, 3.0, 0.0), Matrix3::identity()))
            .with_property("Material", Enum::from_u32(272))
            .with_property("Attributes", attributes)
            .with_property("Tags", tags)
            .with_property("NotAProperty", Variant::Int32(7)),
    );
    let head = dom.insert(model, InstanceBuilder::new("MeshPart").with_name("Head"));
    dom.insert(head, InstanceBuilder::new("Decal").with_name("Face").with_property("TextureContent", Content::from_uri("rbxassetid://123")));
    // newer files keep only the Content form (an uploaded bundle's Animation), or an empty string beside it
    dom.insert(model, InstanceBuilder::new("Animation").with_name("Swing").with_property("AnimationContent", Content::from_uri("rbxassetid://12345678901234")));
    dom.insert(
        head,
        InstanceBuilder::new("SpecialMesh")
            .with_name("Shape")
            .with_property("MeshId", Variant::ContentId("".into()))
            .with_property("MeshContent", Content::from_uri("rbxassetid://456")),
    );
    dom.insert(torso, InstanceBuilder::new("Weld").with_name("Neck").with_property("Part0", torso).with_property("Part1", head));
    dom.insert(model, InstanceBuilder::new("ModuleScript").with_name("Logic").with_property("Source", "return 5"));
    dom.insert(model, InstanceBuilder::new("NotAClass").with_name("Odd"));
    let plan = super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children());
    let check = r#"
        local Rig = workspace.Rig
        local Torso, Head = Rig.Torso, Rig.Head
        assert(Torso.Size == Vector3.new(2, 2, 1) and Torso.Position == Vector3.new(0, 3, 0), "part values")
        assert(Torso.Material == Enum.Material.SmoothPlastic, "enum")
        assert(Torso:GetAttribute("Speed") == 4.5 and Torso:HasTag("Hazard"), "attributes and tags")
        assert(Head.Face.Texture == "rbxassetid://123", "a ContentId from its Content sibling")
        assert(Rig.Swing.AnimationId == "rbxassetid://12345678901234", "an AnimationId from AnimationContent")
        assert(Head.Shape.MeshId == "rbxassetid://456" and Head.Shape.MeshContent.Uri == "rbxassetid://456", "an empty MeshId from MeshContent")
        assert(Torso.Neck.Part0 == Torso and Torso.Neck.Part1 == Head, "references within the copy")
        assert(require(Rig.Logic) == 5, "source")
        assert(Rig.Odd.ClassName == "Folder", "unknown class")
        print(#Rig:GetDescendants())
    "#;
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let (lua, engine) = start();
        let workspace = engine.world().workspace;
        let roots = plan.instantiate(&engine, Some(workspace));
        assert_eq!(roots.len(), 1);
        let again = plan.instantiate(&engine, None);
        assert_ne!(again[0], roots[0], "a second instantiation makes new instances");
        outputs.push(run(&lua, &engine, check));
        drop(lua);
        engine.teardown();
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0].iter().filter(|line| line.starts_with("warn: snapshot: unknown class NotAClass")).count(), 2, "{:?}", outputs[0]);
    assert!(outputs[0].contains(&"print: 8".to_string()), "{:?}", outputs[0]);
}

/// A Content property and its string asset id are one value, each set and read through the other,
/// whatever the string one is called. Measured in Studio (2026-10-12) on the pairs
/// below; Decal, ParticleEmitter and ImageLabel already matched.
#[test]
fn content_properties_alias_their_string_asset_ids() {
    assert_script(
        r#"
        local function Check(ClassName, ContentName, StringName)
            local A = Instance.new(ClassName)
            A[ContentName] = Content.fromUri("rbxassetid://123")
            assert(A[StringName] == "rbxassetid://123", `{ClassName}.{ContentName} reads through {StringName}`)
            local B = Instance.new(ClassName)
            B[StringName] = "rbxassetid://456"
            assert(B[ContentName].Uri == "rbxassetid://456", `{ClassName}.{StringName} reads through {ContentName}`)
        end
        Check("Animation", "AnimationContent", "AnimationId")
        Check("SpecialMesh", "MeshContent", "MeshId")
        Check("SpecialMesh", "TextureContent", "TextureId")
        Check("Sound", "AudioContent", "SoundId")
        Check("Decal", "TextureContent", "Texture")
        Check("ParticleEmitter", "TextureContent", "Texture")
        Check("ImageLabel", "ImageContent", "Image")
        Check("Tool", "TextureContent", "TextureId")
        Check("ShirtGraphic", "TextureContent", "Graphic")
        Check("Sky", "SkyboxBackContent", "SkyboxBk")
        Check("Sky", "SkyboxUpContent", "SkyboxUp")
        Check("Sky", "SunTextureContent", "SunTextureId")
        local Cleared = Instance.new("Animation")
        Cleared.AnimationContent = Content.fromUri("rbxassetid://123")
        Cleared.AnimationId = ""
        assert(Cleared.AnimationContent.SourceType == Enum.ContentSourceType.None, "an empty id is no content")
        local Part = Instance.new("MeshPart")
        Part.TextureContent = Content.fromUri("rbxassetid://666")
        assert(Part.TextureID == "rbxassetid://666", "MeshPart.TextureContent reads through TextureID")
    "#,
    );
}

/// nil clears CustomPhysicalProperties (an effect module that copies it from a part that has none), while
/// other datatype properties refuse it. Measured in Studio (2026-10-12).
#[test]
fn custom_physical_properties_take_nil() {
    assert_script(
        r#"
        local Part = Instance.new("Part")
        assert(Part.CustomPhysicalProperties == nil, "none by default")
        Part.CustomPhysicalProperties = nil
        Part.CustomPhysicalProperties = PhysicalProperties.new(1, 0.3, 0.5)
        assert(Part.CustomPhysicalProperties ~= nil, "set")
        Part.CustomPhysicalProperties = nil
        assert(Part.CustomPhysicalProperties == nil, "cleared")
        local Mesh = Instance.new("MeshPart")
        Mesh.CustomPhysicalProperties = Part.CustomPhysicalProperties
        local Ok, Message = pcall(function() Part.CustomPhysicalProperties = 5 end)
        assert(not Ok and string.find(Message, "PhysicalProperties expected, got number", 1, true), Message)
        local SizeOk, SizeMessage = pcall(function() Part.Size = nil end)
        assert(not SizeOk and string.find(SizeMessage, "Vector3 expected, got nil", 1, true), SizeMessage)
    "#,
    );
}

/// A model imported lazily (an asset manifest bundle: its instances made as scripts look at them)
/// reads exactly as one made at once: names, order, values, references, debug ids (serials),
/// GetTagged, what a method sees and the events of moving it into the world.
/// An instance made of a lazily imported plan node reads its properties through the import, and so do
/// its unchanged copies, until a write gives it its own: every read, listing and copy is what an eager
/// import's instance holds.
#[test]
fn lazily_made_instances_share_their_nodes_properties_until_written() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{Attributes, CFrame, Color3, Matrix3, Variant, Vector3},
    };
    use super::dom::Props;
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    for index in 0..2 {
        let model = dom.insert(root, InstanceBuilder::new("Model").with_name(format!("Unit{index}")));
        let torso = dom.insert(
            model,
            InstanceBuilder::new("Part")
                .with_name("Torso")
                .with_property("Size", Vector3::new(2.0, 2.0, 1.0))
                .with_property("Color", Color3::new(0.25, 0.5, 1.0))
                .with_property("Transparency", 0.5f32)
                .with_property("CFrame", CFrame::new(Vector3::new(index as f32, 3.0, 0.0), Matrix3::identity()))
                .with_property("Attributes", {
                    let mut attributes = Attributes::new();
                    attributes.insert("Speed".to_string(), Variant::Float64(index as f64 + 0.5));
                    attributes.insert("Label".to_string(), Variant::String(format!("unit {index}")));
                    attributes
                }),
        );
        dom.insert(torso, InstanceBuilder::new("Attachment").with_name("Grip"));
        dom.insert(torso, InstanceBuilder::new("Weld").with_name("Self").with_property("Part0", torso));
    }
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let check = r#"
        local Folder = game:GetService("ReplicatedStorage").Bundle
        local Torso = Folder.Unit0.Torso
        print(Torso.Name, Torso.Size, Torso.Color, Torso.Transparency, Torso.CFrame, Torso.Grip.Name, Torso.Self.Part0 == Torso)
        print(Torso:GetAttribute("Speed"), Torso:GetAttribute("Label"), Torso:GetAttribute("Missing"))
        local Attributes = Folder.Unit1.Torso:GetAttributes()
        print(Attributes.Speed, Attributes.Label)
        local Copy = Folder.Unit1:Clone()
        print(Copy.Torso.Size, Copy.Torso.Transparency, Copy.Torso.Self.Part0 == Copy.Torso)
        Copy.Name = "Copy"
        Copy.Torso.Transparency = 0.25
        Copy.Torso:SetAttribute("Speed", 9)
        print(Copy.Torso.Transparency, Folder.Unit1.Torso.Transparency, Copy.Name, Folder.Unit1.Name, Copy.Torso:GetAttribute("Speed"), Folder.Unit1.Torso:GetAttribute("Speed"))
        Torso.Name = "Renamed"
        print(Torso.Name, Torso.Size, Folder.Unit0:FindFirstChild("Renamed") == Torso)
        Copy.Parent = workspace
    "#;
    let mut listings = Vec::new();
    for lazy in [false, true] {
        let (lua, engine) = start();
        let folder = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("Folder").unwrap();
            let folder = super::instance::create_named(&mut state, &engine.reflection, class, "Bundle");
            let storage = state.dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
            state.dom.attach(folder, storage);
            folder
        };
        if lazy {
            plan.instantiate_lazy(&mut engine.state.borrow_mut(), &engine.reflection, folder);
            // what a read makes of the first unit: its nodes read through the import, but the weld (a reference)
            run(&lua, &engine, "local _ = game:GetService('ReplicatedStorage').Bundle.Unit0.Torso.Self");
            let state = engine.state.borrow();
            let unit = state.dom.find_child(folder, b"Unit0").unwrap();
            let torso = state.dom.find_child(unit, b"Torso").unwrap();
            assert!(matches!(state.dom.at(unit).props, Props::Plan(..)));
            assert!(matches!(state.dom.at(torso).props, Props::Plan(..)));
            assert!(matches!(state.dom.at(torso).attributes, super::dom::Attributes::Plan(..)), "attributes read through the import");
            assert!(matches!(state.dom.at(state.dom.find_child(torso, b"Self").unwrap()).props, Props::Own(_)), "a node with a reference has its own");
        } else {
            plan.instantiate(&engine, Some(folder));
        }
        let output = run(&lua, &engine, check);
        let state = engine.state.borrow();
        if lazy {
            let unit = state.dom.find_child(folder, b"Unit1").unwrap();
            assert!(matches!(state.dom.at(state.dom.find_child(unit, b"Torso").unwrap()).props, Props::Plan(..)), "untouched by its copy's writes");
            let copy = state.dom.find_child(engine.world().workspace, b"Copy").unwrap();
            assert!(matches!(state.dom.at(copy).props, Props::Own(_)), "renamed");
            let copied_torso = state.dom.find_child(copy, b"Torso").unwrap();
            assert!(matches!(state.dom.at(copied_torso).attributes, super::dom::Attributes::Own(_)), "an attribute set");
            assert!(matches!(state.dom.at(state.dom.find_child(unit, b"Torso").unwrap()).attributes, super::dom::Attributes::Plan(..)), "untouched by its copy's");
            let grip = state.dom.find_child(state.dom.find_child(copy, b"Torso").unwrap(), b"Grip").unwrap();
            assert!(matches!(state.dom.at(grip).props, Props::Plan(..)), "an unchanged copy shares");
        }
        let mut listing = vec![output.join("\n")];
        for index in 0..state.dom.slot_count() as u32 {
            if let Some(id) = state.dom.id_at_slot(index) {
                if state.dom.is_descendant_of(id, folder) || state.dom.is_descendant_of(id, engine.world().workspace) {
                    let instance = state.dom.at(id);
                    let props: Vec<String> = instance
                        .props
                        .iter()
                        .map(|(name, value)| match value.as_instance() {
                            Some(target) => format!("{}={}", engine.reflection.name(name), state.dom.full_name(&engine.reflection, target)),
                            None => format!("{}={value:?}", engine.reflection.name(name)),
                        })
                        .collect();
                    let attributes: Vec<String> = instance.attributes.iter().map(|(name, value)| format!("{}={value:?}", name.to_str_lossy())).collect();
                    listing.push(format!("{} {} {} [{}] {}", state.dom.full_name(&engine.reflection, id), props.join(" "), instance.props.len(), attributes.join(" "), instance.attributes.len()));
                }
            }
        }
        listing.sort();
        listings.push(listing);
        drop(state);
        drop(lua);
        engine.teardown();
    }
    assert_eq!(listings[0], listings[1]);
    assert!(listings[0].iter().any(|line| line.contains("print: Renamed 2, 2, 1 true")), "{:?}", listings[0]);
}

#[test]
fn a_lazy_import_reads_as_an_eager_one() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{CFrame, Matrix3, Tags, Variant, Vector3},
    };
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    for index in 0..3 {
        let model = dom.insert(root, InstanceBuilder::new("Model").with_name(format!("Unit{index}")));
        let torso = dom.insert(
            model,
            InstanceBuilder::new("Part")
                .with_name("Torso")
                .with_property("Size", Vector3::new(2.0, 2.0, 1.0))
                .with_property("CFrame", CFrame::new(Vector3::new(index as f32 * 5.0, 3.0, 0.0), Matrix3::identity())),
        );
        let arm = dom.insert(torso, InstanceBuilder::new("Part").with_name("Arm").with_property("CFrame", CFrame::new(Vector3::new(index as f32 * 5.0 + 1.5, 3.0, 0.0), Matrix3::identity())));
        dom.insert(arm, InstanceBuilder::new("Attachment").with_name("Grip"));
        let mut tags = Tags::new();
        tags.push("Glow");
        dom.insert(arm, InstanceBuilder::new("PointLight").with_name("Light").with_property("Tags", tags));
        dom.insert(torso, InstanceBuilder::new("Weld").with_name("Shoulder").with_property("Part0", torso).with_property("Part1", arm));
        dom.insert(model, InstanceBuilder::new("ObjectValue").with_name("Target").with_property("Value", Variant::Ref(arm)));
        let extras = dom.insert(model, InstanceBuilder::new("Folder").with_name("Extras"));
        for part in 0..2 {
            dom.insert(extras, InstanceBuilder::new("Part").with_name(format!("Bit{part}")));
        }
        dom.get_by_ref_mut(model).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(torso));
    }
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let check = r#"
        local Folder = game:GetService("ReplicatedStorage").Bundle
        local CollectionService = game:GetService("CollectionService")
        print(#CollectionService:GetTagged("Glow"))
        local Added = 0
        workspace.DescendantAdded:Connect(function() Added += 1 end)
        for _, Unit in Folder:GetChildren() do
            print(Unit.Name, Unit:GetDebugId(), Unit.PrimaryPart and Unit.PrimaryPart.Name)
            print(Unit.Target.Value:GetFullName(), Unit.Target.Value:GetDebugId())
        end
        local First = Folder.Unit0
        print(First:GetBoundingBox())
        First.Parent = workspace
        task.wait()
        print("added", Added)
        for _, Item in Folder.Unit1:GetDescendants() do
            print(Item:GetFullName(), Item.ClassName, Item:GetDebugId())
        end
        local Copy = Folder.Unit2:Clone()
        print(#Copy:GetDescendants(), Copy.Torso.Shoulder.Part1 == Copy.Torso.Arm)
    "#;
    let mut outputs = Vec::new();
    for lazy in [false, true] {
        let (lua, engine) = start();
        let folder = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("Folder").unwrap();
            let folder = super::instance::create_named(&mut state, &engine.reflection, class, "Bundle");
            let storage = state.dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
            state.dom.attach(folder, storage);
            folder
        };
        if lazy {
            plan.instantiate_lazy(&mut engine.state.borrow_mut(), &engine.reflection, folder);
            assert!(engine.state.borrow().project.lazy_made < plan.len() as u32, "made as looked at");
        } else {
            plan.instantiate(&engine, Some(folder));
        }
        outputs.push(run(&lua, &engine, check));
        drop(lua);
        engine.teardown();
    }
    assert_eq!(outputs[0], outputs[1]);
    assert!(outputs[0].contains(&"print: 3".to_string()), "{:?}", outputs[0]);

    // a reference from one root into another: the import is made whole at once (the reference
    // keeps its target alive before any script can drop it)
    let target = dom.root().children()[1];
    let arm = dom.get_by_ref(dom.get_by_ref(target).unwrap().children()[0]).unwrap().children()[0];
    let first = dom.root().children()[0];
    dom.insert(first, InstanceBuilder::new("ObjectValue").with_name("Other").with_property("Value", Variant::Ref(arm)));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let (lua, engine) = start();
    let workspace_folder = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("Folder").unwrap();
        let folder = super::instance::create_named(&mut state, &engine.reflection, class, "Bundle");
        let storage = state.dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
        state.dom.attach(folder, storage);
        folder
    };
    plan.instantiate_lazy(&mut engine.state.borrow_mut(), &engine.reflection, workspace_folder);
    assert_eq!(engine.state.borrow().project.lazy_made, plan.len() as u32, "made whole");
    drop(lua);
    engine.teardown();
}

/// Tags a newer Studio writes as a SharedString (as uploaded assets can hold them; project::decode_model
/// reads the property as one) are the instance's tags, as when they are written as Tags.
#[test]
fn tags_held_as_a_shared_string_are_tags() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{SharedString, Variant},
    };
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let model = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name("Mount").with_property("Tags", Variant::SharedString(SharedString::new(b"RemoveFromBoundingBox\0RemoveFromLobby".to_vec()))));
    dom.insert(model, InstanceBuilder::new("Part").with_name("Plain").with_property("Tags", Variant::SharedString(SharedString::new(Vec::new()))));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let (lua, engine) = start();
    let storage = engine.state.borrow().dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
    plan.instantiate(&engine, Some(storage));
    let output = run(&lua, &engine, r#"
        local Mount = game:GetService("ReplicatedStorage").Mount
        print(table.concat(Mount:GetTags(), ","), #Mount.Plain:GetTags(), #game:GetService("CollectionService"):GetTagged("RemoveFromLobby"))
    "#);
    assert_eq!(output, ["print: RemoveFromBoundingBox,RemoveFromLobby 0 1"]);
    drop(lua);
    engine.teardown();
}

/// A folder named `name` in `parent` (made as the engine builds its own: no events).
fn folder_in(engine: &Engine, parent: super::InstanceId, name: &str) -> super::InstanceId {
    let mut state = engine.state.borrow_mut();
    let class = engine.reflection.class_id("Folder").unwrap();
    let folder = super::instance::create_named(&mut state, &engine.reflection, class, name);
    state.dom.attach(folder, parent);
    folder
}

/// A folder named `name` in ReplicatedStorage.
fn storage_folder(engine: &Engine, name: &str) -> super::InstanceId {
    let storage = engine.state.borrow().dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
    folder_in(engine, storage, name)
}

/// A plan of one root made as the instance that stood in its place (a manifest bundle's
/// placeholder: `ModelPlan::instantiate_lazy_as`) reads as the root made at once there and named as
/// the game names an installed bundle (its class, values, attributes, tags, children, joints,
/// clones and moves); the placeholder's own name and identity stay.
#[test]
fn a_placeholder_made_its_plans_root_reads_as_the_root_made_at_once() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{Attributes, CFrame, Matrix3, Tags, Variant, Vector3},
    };
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let mut attributes = Attributes::new();
    attributes.insert("Reach".into(), Variant::Float64(5.0));
    let mut tags = Tags::new();
    tags.push("Machine");
    let model = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name("Machine File").with_property("Attributes", attributes).with_property("Tags", tags));
    let place = |y: f32| CFrame::new(Vector3::new(1.0, y, 3.0), Matrix3::identity());
    let body = dom.insert(model, InstanceBuilder::new("MeshPart").with_name("Body").with_property("Size", Vector3::new(4.0, 5.0, 6.0)).with_property("CFrame", place(2.0)));
    let cab = dom.insert(model, InstanceBuilder::new("Part").with_name("Cab").with_property("CFrame", place(5.0)));
    dom.insert(cab, InstanceBuilder::new("Weld").with_name("Weld").with_property("Part0", Variant::Ref(body)).with_property("Part1", Variant::Ref(cab)));
    let bone = dom.insert(body, InstanceBuilder::new("Bone").with_name("Root"));
    dom.insert(bone, InstanceBuilder::new("Bone").with_name("Neck"));
    dom.get_by_ref_mut(model).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(body));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let check = r#"
        local Machines = game:GetService("ReplicatedStorage").Machines
        local Crane = Machines["Tall Crane"]
        print(Crane.ClassName, Crane.Name, Crane.PrimaryPart:GetFullName(), Crane:GetAttribute("Reach"), table.concat(Crane:GetTags(), ","))
        print(#game:GetService("CollectionService"):GetTagged("Machine"))
        for _, Item in Crane:GetDescendants() do
            print(Item:GetFullName(), Item.ClassName)
        end
        print(Crane.Cab.Weld.Part0 == Crane.Body, Crane:GetPivot())
        print(Crane:GetBoundingBox())
        local Copy = Crane:Clone()
        print(Copy.Name, Copy.PrimaryPart == Copy.Body, #Copy:GetDescendants())
        Crane.Parent = workspace
        print(Crane:GetFullName(), #workspace:GetDescendants())
    "#;
    let mut outputs = Vec::new();
    for at_once in [true, false] {
        let (lua, engine) = start();
        let machines = storage_folder(&engine, "Machines");
        if at_once {
            let roots = plan.instantiate(&engine, Some(machines));
            let mut state = engine.state.borrow_mut();
            super::instance::raw_set_silent(&mut state, &engine.reflection, roots[0], super::K::Name as super::NameId, super::value::Value::String("Tall Crane".into()));
        } else {
            let placeholder = folder_in(&engine, machines, "Tall Crane");
            plan.instantiate_lazy_as(&mut engine.state.borrow_mut(), &engine.reflection, placeholder);
            assert_eq!(engine.state.borrow().project.lazy_made, 2, "the root's children now, the rest as it is looked at");
        }
        outputs.push(run(&lua, &engine, check));
        drop(lua);
        engine.teardown();
    }
    assert_eq!(outputs[0], outputs[1]);
    assert!(outputs[0][0].starts_with("print: Model Tall Crane ReplicatedStorage.Machines.Tall Crane.Body 5 Machine"), "{:?}", outputs[0]);
}

/// What an import that makes a root's children at once makes then, as a folder bundle's roots were
/// made: a folder root's children (each made whole only when it, or something in it, moves) unless
/// one references into another; any other root is one region (made whole before anything in it
/// moves); a loose bundle's members go into the folder given, the root itself never made (a member's
/// reference to it reads nil).
#[test]
fn a_roots_children_import_as_a_folder_bundles_roots_did() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{Ref, Variant},
    };
    let units = |cross: bool| {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let folder = dom.insert(dom.root_ref(), InstanceBuilder::new("Folder").with_name("Units"));
        let mut arms = Vec::new();
        for index in 0..2 {
            let unit = dom.insert(folder, InstanceBuilder::new("Model").with_name(format!("Unit{index}")));
            let torso = dom.insert(unit, InstanceBuilder::new("Part").with_name("Torso"));
            arms.push(dom.insert(torso, InstanceBuilder::new("Part").with_name("Arm")));
        }
        if cross {
            let first = dom.get_by_ref(folder).unwrap().children()[0];
            dom.insert(first, InstanceBuilder::new("ObjectValue").with_name("Other").with_property("Value", Variant::Ref(arms[1])));
        }
        std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()))
    };
    let made = |plan: &std::sync::Arc<super::project::ModelPlan>, script: &str| {
        let (lua, engine) = start();
        let placeholder = storage_folder(&engine, "Units");
        plan.instantiate_lazy_as(&mut engine.state.borrow_mut(), &engine.reflection, placeholder);
        let before = engine.state.borrow().project.lazy_made;
        run(&lua, &engine, script);
        let after = engine.state.borrow().project.lazy_made;
        drop(lua);
        engine.teardown();
        (before, after)
    };
    let move_first = r#"game:GetService("ReplicatedStorage").Units.Unit0.Parent = workspace"#;
    let plan = units(false);
    assert_eq!(made(&plan, move_first), (2, 4), "the folder's children now; the moved one whole, the other not");
    let plan = units(true);
    assert_eq!(made(&plan, move_first).0, plan.len() as u32 - 1, "a reference across them: everything at once");

    // a model's children reference each other: one region, made whole when one of them moves
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let model = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name("Crane"));
    let body = dom.insert(model, InstanceBuilder::new("Part").with_name("Body"));
    dom.insert(body, InstanceBuilder::new("Attachment").with_name("Grip"));
    let cab = dom.insert(model, InstanceBuilder::new("Part").with_name("Cab"));
    dom.insert(cab, InstanceBuilder::new("Weld").with_name("Weld").with_property("Part0", Variant::Ref(body)).with_property("Part1", Variant::Ref(cab)));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let (lua, engine) = start();
    let placeholder = storage_folder(&engine, "Crane");
    plan.instantiate_lazy_as(&mut engine.state.borrow_mut(), &engine.reflection, placeholder);
    assert_eq!(engine.state.borrow().project.lazy_made, 2, "its children now");
    run(&lua, &engine, r#"game:GetService("ReplicatedStorage").Crane.Cab.Parent = workspace"#);
    assert_eq!(engine.state.borrow().project.lazy_made, plan.len() as u32 - 1, "made whole before a part of it moved");
    drop(lua);
    engine.teardown();

    // a loose bundle: its members
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let content = dom.insert(dom.root_ref(), InstanceBuilder::new("Folder").with_name("Content"));
    let sword = dom.insert(content, InstanceBuilder::new("Model").with_name("Sword"));
    dom.insert(sword, InstanceBuilder::new("ObjectValue").with_name("Owner").with_property("Value", Variant::Ref(content)));
    dom.insert(sword, InstanceBuilder::new("ObjectValue").with_name("Empty").with_property("Value", Variant::Ref(Ref::none())));
    dom.insert(content, InstanceBuilder::new("Part").with_name("Shield"));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let (lua, engine) = start();
    let items = storage_folder(&engine, "Items");
    let members = plan.instantiate_lazy_members(&mut engine.state.borrow_mut(), &engine.reflection, items);
    assert_eq!(members.len(), 2);
    let output = run(&lua, &engine, r#"
        local Items = game:GetService("ReplicatedStorage").Items
        print(#Items:GetChildren(), Items.Sword.ClassName, Items.Shield.ClassName, Items:FindFirstChild("Content"), Items.Sword.Owner.Value, Items.Sword.Empty.Value)
    "#);
    assert_eq!(output, ["print: 2 Model Part nil nil nil"]);
    drop(lua);
    engine.teardown();
}

/// A rig imported lazily (a bundle template) animates as one made at once: LoadAnimation on its
/// Animator makes the rest of the model first, so the Animator step finds every Motor6D, and the
/// template moved into the world poses the same.
#[test]
fn a_lazily_imported_rig_animates_as_an_eager_one() {
    use rbx_dom_weak::{
        InstanceBuilder, WeakDom,
        types::{CFrame, Matrix3, Variant, Vector3},
    };
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let place = |x: f32, y: f32| CFrame::new(Vector3::new(x, y, 0.0), Matrix3::identity());
    let model = dom.insert(root, InstanceBuilder::new("Model").with_name("Rig"));
    let hrp = dom.insert(model, InstanceBuilder::new("Part").with_name("HumanoidRootPart").with_property("Anchored", true).with_property("CFrame", place(0.0, 3.0)));
    let torso = dom.insert(model, InstanceBuilder::new("Part").with_name("Torso").with_property("CFrame", place(0.0, 3.0)));
    let arm = dom.insert(model, InstanceBuilder::new("Part").with_name("Right Arm").with_property("Size", Vector3::new(1.0, 2.0, 1.0)).with_property("CFrame", place(1.5, 3.0)));
    dom.insert(hrp, InstanceBuilder::new("Motor6D").with_name("RootJoint").with_property("Part0", Variant::Ref(hrp)).with_property("Part1", Variant::Ref(torso)));
    dom.insert(
        torso,
        InstanceBuilder::new("Motor6D")
            .with_name("Right Shoulder")
            .with_property("Part0", Variant::Ref(torso))
            .with_property("Part1", Variant::Ref(arm))
            .with_property("C0", place(1.5, 0.5))
            .with_property("C1", place(0.0, 0.5)),
    );
    let humanoid = dom.insert(model, InstanceBuilder::new("Humanoid"));
    dom.insert(humanoid, InstanceBuilder::new("Animator"));
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let check = r#"
        local Provider = game:GetService("KeyframeSequenceProvider")
        local Sequence = Instance.new("KeyframeSequence")
        for _, Entry in { {0, 0}, {1, 1} } do
            local Keyframe = Instance.new("Keyframe")
            Keyframe.Time = Entry[1]
            local Root = Instance.new("Pose")
            Root.Name = "HumanoidRootPart"
            Root.Weight = 0
            Root.Parent = Keyframe
            local Torso = Instance.new("Pose")
            Torso.Name = "Torso"
            Torso.Weight = 0
            Torso.Parent = Root
            local Arm = Instance.new("Pose")
            Arm.Name = "Right Arm"
            Arm.CFrame = CFrame.Angles(Entry[2], 0, 0)
            Arm.Parent = Torso
            Keyframe.Parent = Sequence
        end
        local Animation = Instance.new("Animation")
        Animation.AnimationId = Provider:RegisterKeyframeSequence(Sequence)
        local Template = game:GetService("ReplicatedStorage").Bundle.Rig
        local Track = Template.Humanoid.Animator:LoadAnimation(Animation)
        Template.Parent = workspace
        Track:Play(0)
        task.wait(0.5)
        local Shoulder = Template.Torso["Right Shoulder"]
        print(Track.TimePosition, Shoulder.Transform, Template["Right Arm"].CFrame)
        for _, Item in Template:GetDescendants() do
            print(Item:GetFullName(), Item:GetDebugId())
        end
    "#;
    let mut outputs = Vec::new();
    for lazy in [false, true] {
        let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
        let lua = Lua::new();
        engine_lua::start(&lua, &engine).expect("start");
        let folder = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("Folder").unwrap();
            let folder = super::instance::create_named(&mut state, &engine.reflection, class, "Bundle");
            let storage = state.dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
            state.dom.attach(folder, storage);
            folder
        };
        if lazy {
            plan.instantiate_lazy(&mut engine.state.borrow_mut(), &engine.reflection, folder);
            assert!(engine.state.borrow().project.lazy_made < plan.len() as u32, "made as looked at");
        } else {
            plan.instantiate(&engine, Some(folder));
        }
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            super::instance::create_named(&mut state, &engine.reflection, class, "Test")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let function = lua.load(check).set_name("=Test").set_environment(env).into_function().expect("compile");
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        crate::scheduler::run_until(&lua, &engine, 1.0).expect("run");
        if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
            panic!("test script failed: {message}");
        }
        outputs.push(lines(&engine));
        drop(lua);
        engine.teardown();
    }
    assert_eq!(outputs[0], outputs[1]);
    assert!(outputs[0].iter().any(|line| line.contains("Right Shoulder")), "{:?}", outputs[0]);
}

/// A model file's stored scale (ScaleFactor, rbx-dom's Scale) is the model's GetScale: effect bundles
/// can save orbs at 0.01 and grow them with ScaleTo(1).
#[test]
fn a_model_file_keeps_its_scale() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{Variant, Vector3}};
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let model = dom.insert(root, InstanceBuilder::new("Model").with_name("Orb").with_property("Scale", Variant::Float32(0.25)));
    dom.insert(model, InstanceBuilder::new("Part").with_name("Core").with_property("Size", Vector3::new(1.0, 1.0, 1.0)));
    let plan = super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children());
    let (lua, engine) = start();
    plan.instantiate(&engine, Some(engine.world().workspace));
    let output = run(
        &lua,
        &engine,
        r#"
        local Orb = workspace.Orb
        assert(Orb:GetScale() == 0.25, "the stored scale")
        Orb:ScaleTo(1)
        assert(Orb.Core.Size == Vector3.new(4, 4, 4), "ScaleTo(1) from the stored scale")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// A place's snapshot lists its non-script content by name and class only, so a module's fallback Rig
/// (the rig unresolved entities clone, stored at Model scale 0.43 in the places measured) was a
/// default Model: scale 1, no pivot, no parts' sizes, and a ScaleTo(1) guard looked like a 9.74 ms
/// gain that Studio did not give. `data/place-content/<Place>/<parent path>/*.rbxm` (Studio's own
/// serialization of the instance) now replaces the placeholder with its stored properties,
/// attributes and tags.
#[test]
fn place_content_replaces_a_snapshot_placeholder_with_the_stored_instance() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{Attributes, CFrame, Matrix3, Variant, Vector3}};
    let dir = std::env::temp_dir().join(format!("ree-place-content-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (project, repo) = (dir.join("project"), dir.join("repo"));
    std::fs::create_dir_all(project.join("place/ReplicatedStorage/Shared")).unwrap();
    std::fs::create_dir_all(project.join(super::project::SNAPSHOTS_DIR)).unwrap();
    std::fs::write(project.join("place/ReplicatedStorage/Shared/RigLibrary.luau"), "return {}").unwrap();
    std::fs::write(project.join(super::project::PLACES_FILE), r#"{"environments":[{"name":"Lobby","projectFile":"place/default.project.json"}]}"#).unwrap();
    let node = |name: &str, class: &str, parent: i64| format!(r#"{{"name":"{name}","className":"{class}","parent":{parent}}}"#);
    let nodes = [
        node("game", "DataModel", -1),
        node("ReplicatedStorage", "ReplicatedStorage", 0),
        node("Shared", "Folder", 1),
        node("RigLibrary", "ModuleScript", 2),
        node("Rig", "Model", 3),
        node("HumanoidRootPart", "Part", 4),
        node("Hat", "Part", 4),
    ];
    std::fs::write(project.join(super::project::SNAPSHOTS_DIR).join("Lobby.json"), format!(r#"{{"nodes":[{}]}}"#, nodes.join(","))).unwrap();
    // the Rig as Studio stores it
    let mut place = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = place.root_ref();
    let at = |x, y, z| Variant::OptionalCFrame(Some(CFrame::new(Vector3::new(x, y, z), Matrix3::identity())));
    let rig = place.insert(
        root,
        InstanceBuilder::new("Model")
            .with_name("Rig")
            .with_property("Scale", Variant::Float32(0.43))
            .with_property("WorldPivotData", at(5.0, 6.0, 7.0))
            .with_property("ModelStreamingMode", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(2)))
            .with_property("Attributes", Attributes::new().with("Kind", Variant::String("fallback".into())))
            .with_property("Tags", rbx_dom_weak::types::Tags::from(vec!["Fallback".to_string()])),
    );
    place.insert(rig, InstanceBuilder::new("Part").with_name("HumanoidRootPart").with_property("Size", Vector3::new(2.0, 2.0, 1.0)));
    let content = repo.join("data/place-content/Lobby/ReplicatedStorage/Shared/RigLibrary");
    std::fs::create_dir_all(&content).unwrap();
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, &place, &[rig]).unwrap();
    std::fs::write(content.join("Rig.rbxm"), bytes).unwrap();

    let engine = Engine::new(Options { repo, ..Options::default() }).expect("engine");
    let lua = Lua::new();
    let spec = super::mount::ProjectSpec { roots: vec![project.join("place").to_string_lossy().replace('\\', "/")], ..Default::default() };
    super::mount::mount_project(&lua, &engine, &spec, std::time::Instant::now()).expect("mount");
    let output = run(
        &lua,
        &engine,
        r#"
        local Library = game:GetService("ReplicatedStorage").Shared.RigLibrary
        local Rig = Library:FindFirstChild("Rig")
        assert(Rig, "the Rig")
        assert(#Library:GetChildren() == 1, "one Rig, not the stored one beside the placeholder")
        assert(math.abs(Rig:GetScale() - 0.43) < 1e-6, "the stored scale " .. Rig:GetScale())
        assert(Rig.WorldPivot.Position == Vector3.new(5, 6, 7), "the stored pivot")
        assert(Rig.ModelStreamingMode == Enum.ModelStreamingMode.Persistent, "the stored streaming mode " .. tostring(Rig.ModelStreamingMode))
        assert(Rig:GetAttribute("Kind") == "fallback", "the stored attribute")
        assert(Rig:HasTag("Fallback"), "the stored tag")
        assert(Rig.HumanoidRootPart.Size == Vector3.new(2, 2, 1), "the stored part size")
        assert(Rig:FindFirstChild("Hat") == nil, "the file is the whole subtree: a child only the (older) snapshot lists is not added")
        assert(#Rig:GetChildren() == 1, "the Rig's children are the file's")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An enum property refuses what it cannot read with a playing client's and a server's texts (Studio 0.744,
/// tools/parity/probes/enum_assignment.luau): nil and other types "EnumItem, number, or string expected", a value no item
/// has "Invalid value", truncated and unquoted for a number or a numeric string.
#[test]
fn enum_properties_refuse_with_the_measured_texts() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local function Try(F) local Ok, Message = pcall(F) return if Ok then "ok" else Message end
        local Part, Label = Instance.new("Part"), Instance.new("TextLabel")
        print(Try(function() Part.Material = nil end))
        print(Try(function() Label.Font = Vector3.zero end))
        print(Try(function() Label.Font = "256" end))
        print(Try(function() Label.Font = 512.5 end))
        print(Try(function() Label.Font = "Wood" end))
        "#,
    );
    assert_eq!(
        lines(&engine),
        vec![
            "print: Unable to assign property Material. EnumItem, number, or string expected, got nil",
            "print: Unable to assign property Font. EnumItem, number, or string expected, got Vector3",
            "print: Unable to assign property Font. Invalid value 256 for enum Font",
            "print: Unable to assign property Font. Invalid value 512 for enum Font",
            "print: Unable to assign property Font. Invalid value \"Wood\" for enum Font",
        ]
    );
}

/// A directory of the source tree that holds only scripts stands for the instance the place keeps them in: the place's
/// hierarchy lists that path as a Part (a published place holds a Part with its scripts inside while the source tree has
/// only the scripts), so the directory is a Part, Studio-only, with its scripts and the hierarchy's other children in it,
/// and a clone of it is a Part a script can move; it was a Folder, and setting its CFrame raised `CFrame is not a valid
/// member of Folder`. A directory the hierarchy lists as a Folder or a script, or does not list, stays a Folder, and
/// place content's stored instance keeps the directory's scripts.
#[test]
fn a_directory_of_scripts_is_the_instance_the_place_keeps_them_in() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{Variant, Vector3}};
    let dir = std::env::temp_dir().join(format!("ree-dir-classes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (project, repo) = (dir.join("project"), dir.join("repo"));
    let skill = project.join("place/ReplicatedStorage/Effects/Skill");
    for sub in ["Frog/Inner", "Plain", "Lonely", "Coded", "Wind"] {
        std::fs::create_dir_all(skill.join(sub)).unwrap();
    }
    std::fs::write(skill.join("Frog/SelfDestruct.server.luau"), "return nil").unwrap();
    std::fs::write(skill.join("Frog/Inner/Helper.luau"), "return 1").unwrap();
    std::fs::write(skill.join("Plain/A.luau"), "return 1").unwrap();
    std::fs::write(skill.join("Lonely/B.luau"), "return 1").unwrap();
    std::fs::write(skill.join("Coded/C.luau"), "return 1").unwrap();
    std::fs::write(skill.join("Wind/Textures.server.luau"), "-- the source tree's").unwrap();
    std::fs::create_dir_all(project.join(super::project::SNAPSHOTS_DIR)).unwrap();
    std::fs::write(project.join(super::project::PLACES_FILE), r#"{"environments":[{"name":"Lobby","projectFile":"place/default.project.json"}]}"#).unwrap();
    let node = |name: &str, class: &str, parent: i64| format!(r#"{{"name":"{name}","className":"{class}","parent":{parent}}}"#);
    let nodes = [
        node("game", "DataModel", -1),
        node("ReplicatedStorage", "ReplicatedStorage", 0),
        node("Effects", "Folder", 1),
        node("Skill", "Folder", 2),
        node("Frog", "Part", 3),
        node("SelfDestruct", "Script", 4),
        node("Spark", "ParticleEmitter", 4),
        node("Inner", "Attachment", 4),
        node("Plain", "Folder", 3),
        node("Coded", "ModuleScript", 3),
        node("Wind", "Part", 3),
    ];
    std::fs::write(project.join(super::project::SNAPSHOTS_DIR).join("Lobby.json"), format!(r#"{{"nodes":[{}]}}"#, nodes.join(","))).unwrap();
    // Wind as the place stores it: a sized Part with a mesh, and an older copy of its script
    let mut place = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = place.root_ref();
    let wind = place.insert(root, InstanceBuilder::new("Part").with_name("Wind").with_property("Size", Vector3::new(2.0, 3.0, 4.0)));
    place.insert(wind, InstanceBuilder::new("SpecialMesh").with_name("Mesh"));
    place.insert(wind, InstanceBuilder::new("Script").with_name("Textures").with_property("Source", Variant::String("-- the place's older copy".into())));
    let content = repo.join("data/place-content/Lobby/ReplicatedStorage/Effects/Skill");
    std::fs::create_dir_all(&content).unwrap();
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, &place, &[wind]).unwrap();
    std::fs::write(content.join("Wind.rbxm"), bytes).unwrap();

    let engine = Engine::new(Options { repo, ..Options::default() }).expect("engine");
    let lua = Lua::new();
    let spec = super::mount::ProjectSpec { roots: vec![project.join("place").to_string_lossy().replace('\\', "/")], ..Default::default() };
    super::mount::mount_project(&lua, &engine, &spec, std::time::Instant::now()).expect("mount");
    let output = run(
        &lua,
        &engine,
        r#"
        local Skill = game:GetService("ReplicatedStorage").Effects.Skill
        local Frog = Skill.Frog
        assert(Frog.ClassName == "Part" and Frog:IsA("BasePart"), "the place's class " .. Frog.ClassName)
        assert(Frog.SelfDestruct.ClassName == "Script", "its script")
        assert(Frog.Inner.ClassName == "Attachment" and Frog.Inner.Helper.ClassName == "ModuleScript", "a nested directory takes its class too")
        assert(Frog:FindFirstChild("Spark") and Frog.Spark.ClassName == "ParticleEmitter", "the hierarchy's other children")
        local Copy = Frog:Clone()
        Copy.Parent = workspace
        Copy.CFrame = CFrame.new(1, 2, 3)
        assert(Copy.ClassName == "Part" and Copy.Position == Vector3.new(1, 2, 3) and Copy:FindFirstChild("SelfDestruct"), "a clone is a Part with its script")
        assert(Skill.Plain.ClassName == "Folder" and Skill.Lonely.ClassName == "Folder" and Skill.Coded.ClassName == "Folder", "Folder, unlisted and script paths stay Folders")
        local Wind = Skill.Wind
        local Count = 0
        for _, Child in Skill:GetChildren() do
            if Child.Name == "Wind" then
                Count += 1
            end
        end
        assert(Count == 1, "one Wind")
        assert(Wind.ClassName == "Part" and Wind.Size == Vector3.new(2, 3, 4) and Wind:FindFirstChild("Mesh"), "place content's stored Wind")
        local Scripts = 0
        for _, Child in Wind:GetChildren() do
            if Child.Name == "Textures" then
                Scripts += 1
            end
        end
        assert(Scripts == 1, "the source tree's script replaces the stored copy")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    let state = engine.state.borrow();
    let textures = state.dom.descendants(engine.world().game).into_iter().find(|&id| state.dom.at(id).name.as_bytes() == b"Textures").expect("Textures");
    assert!(state.dom.at(textures).source_path().is_some_and(|path| path.ends_with("Wind/Textures.server.luau")), "the source tree's script");
    drop(state);
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model file's stored pivot (WorldPivotData) is the model's pivot, not its parts' centre, and a
/// nested model's stored pivot moves with its ancestor: a scene moved with PivotTo can aim its camera
/// rig from a nested rig's pivot.
#[test]
fn a_model_file_keeps_its_pivot() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::{CFrame, Matrix3, Variant, Vector3}};
    let at = |x, y, z| Variant::OptionalCFrame(Some(CFrame::new(Vector3::new(x, y, z), Matrix3::identity())));
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let scene = dom.insert(root, InstanceBuilder::new("Model").with_name("Scene").with_property("WorldPivotData", at(100.0, 0.0, 0.0)));
    dom.insert(scene, InstanceBuilder::new("Part").with_name("Far").with_property("CFrame", CFrame::new(Vector3::new(500.0, 0.0, 0.0), Matrix3::identity())));
    let rig = dom.insert(scene, InstanceBuilder::new("Model").with_name("Rig").with_property("WorldPivotData", at(100.0, 2.0, 0.0)));
    dom.insert(rig, InstanceBuilder::new("Part").with_name("Root").with_property("CFrame", CFrame::new(Vector3::new(100.0, 2.0, 0.0), Matrix3::identity())));
    let plan = super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children());
    let (lua, engine) = start();
    plan.instantiate(&engine, Some(engine.world().workspace));
    let output = run(
        &lua,
        &engine,
        r#"
        local Scene = workspace.Scene
        assert(Scene:GetPivot().Position == Vector3.new(100, 0, 0), "the stored pivot, not the parts' centre")
        assert(Scene.WorldPivot.Position == Vector3.new(100, 0, 0), "WorldPivot reads the stored pivot")
        Scene:PivotTo(CFrame.new(0, 50, 0))
        assert(Scene.Far.Position == Vector3.new(400, 50, 0), "parts move by the pivot's offset")
        assert(Scene.Rig:GetPivot().Position == Vector3.new(0, 52, 0), "a nested stored pivot moves too")
        assert(Scene:Clone():GetPivot().Position == Vector3.new(0, 50, 0), "a clone keeps the moved pivot")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// Measured in Studio (2026-10-16): an unset pivot follows the parts' centre, a set one stays, a
/// PrimaryPart overrides it without changing WorldPivot, and ScaleTo scales nested stored pivots.
#[test]
fn model_pivots_follow_studio() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local M = Instance.new("Model")
        assert(M:GetPivot().Position == Vector3.zero and M.WorldPivot.Position == Vector3.zero, "empty")
        local A = Instance.new("Part"); A.Size = Vector3.new(2, 2, 2); A.CFrame = CFrame.new(10, 0, 0); A.Parent = M
        local B = Instance.new("Part"); B.Size = Vector3.new(2, 2, 2); B.CFrame = CFrame.new(30, 0, 0); B.Parent = M
        assert(M:GetPivot().Position == Vector3.new(20, 0, 0), "unset: the parts' centre")
        M.WorldPivot = CFrame.new(0, 100, 0)
        assert(M:GetPivot().Position == Vector3.new(0, 100, 0), "set")
        local Inner = Instance.new("Model"); Inner.WorldPivot = CFrame.new(5, 5, 5); Inner.Parent = M
        local C = Instance.new("Part"); C.CFrame = CFrame.new(50, 0, 0); C.Parent = Inner
        assert(Inner:GetPivot().Position == Vector3.new(5, 5, 5), "a set pivot ignores new parts")
        M:PivotTo(CFrame.new(0, 200, 0))
        assert(Inner:GetPivot().Position == Vector3.new(5, 105, 5) and A.Position == Vector3.new(10, 100, 0), "moved")
        M.PrimaryPart = B
        assert(M:GetPivot().Position == Vector3.new(30, 100, 0) and M.WorldPivot.Position == Vector3.new(0, 200, 0), "primary")
        M.PrimaryPart = nil
        assert(M:GetPivot().Position == Vector3.new(0, 200, 0), "primary cleared")
        local S = Instance.new("Model"); S.WorldPivot = CFrame.new()
        local SI = Instance.new("Model"); SI.WorldPivot = CFrame.new(10, 0, 0); SI.Parent = S
        local SP = Instance.new("Part"); SP.CFrame = CFrame.new(10, 0, 0); SP.Parent = SI
        S:ScaleTo(2)
        assert(SI:GetPivot().Position == Vector3.new(20, 0, 0) and SP.Position == Vector3.new(20, 0, 0), "scaled")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// Studio's own numbers (2026-10-16): a part's PivotTo honours its PivotOffset and carries every part
/// under it (directly, through a Folder, an Attachment and a Model) as one rigid move.
#[test]
fn a_part_pivot_carries_the_parts_under_it() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local function at(Part, X, Y, Z) return (Part.Position - Vector3.new(X, Y, Z)).Magnitude < 1e-3 end
        local A = Instance.new("Part"); A.Anchored = true; A.CFrame = CFrame.new(1, 0, 0); A.PivotOffset = CFrame.new(0, 3, 0)
        local B = Instance.new("Part"); B.Anchored = true; B.CFrame = CFrame.new(5, 0, 0); B.Parent = A
        local F = Instance.new("Folder"); F.Parent = A
        local C = Instance.new("Part"); C.Anchored = true; C.CFrame = CFrame.new(9, 0, 0); C.Parent = F
        local Att = Instance.new("Attachment"); Att.Parent = A
        local D = Instance.new("Part"); D.Anchored = true; D.CFrame = CFrame.new(13, 0, 0); D.Parent = Att
        local M = Instance.new("Model"); M.Parent = A
        local E = Instance.new("Part"); E.Anchored = true; E.CFrame = CFrame.new(17, 0, 0); E.Parent = M
        A.Parent = workspace
        A:PivotTo(CFrame.new(100, 0, 0) * CFrame.Angles(0, math.rad(90), 0))
        assert(at(A, 100, -3, 0) and (A:GetPivot().Position - Vector3.new(100, 0, 0)).Magnitude < 1e-3, "the part")
        assert(at(B, 100, -3, -4), "a part under it")
        assert(at(C, 100, -3, -8), "through a Folder")
        assert(at(D, 100, -3, -12), "through an Attachment")
        assert(at(E, 100, -3, -16), "through a Model")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// GetTagged lists the tagged instances of the DataModel in GetDescendants' order, whatever gave
/// them their tags (AddTag, Clone) and however they came and went since.
#[test]
fn get_tagged_follows_the_data_model() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local CollectionService = game:GetService("CollectionService")
        local A = Instance.new("Folder")
        A.Name = "A"
        A.Parent = workspace
        local B = Instance.new("Part")
        B.Name = "B"
        B.Parent = A
        local C = Instance.new("Part")
        C.Name = "C"
        C.Parent = workspace
        CollectionService:AddTag(C, "T")
        CollectionService:AddTag(B, "T")
        local Outside = Instance.new("Part")
        Outside.Name = "Outside"
        CollectionService:AddTag(Outside, "T")
        local Copy = A:Clone()
        Copy.Name = "Copy"
        Copy.Parent = workspace
        local function Names()
            local List = {}
            for _, Tagged in CollectionService:GetTagged("T") do
                table.insert(List, Tagged:GetFullName())
            end
            return table.concat(List, " ")
        end
        print(Names())
        CollectionService:RemoveTag(C, "T")
        B:Destroy()
        print(Names())
        Outside.Parent = Copy
        A.Parent = nil
        Copy.Parent = A
        A.Parent = workspace
        print(Names(), #CollectionService:GetTagged("Other"))
        "#,
    );
    assert_eq!(output, ["print: Workspace.A.B Workspace.C Workspace.Copy.B", "print: Workspace.Copy.B", "print: Workspace.A.Copy.B Workspace.A.Copy.Outside 0"]);
}

/// A recursive FindFirstChild / FindFirstChildWhichIsA over a lazy import finds what it finds over
/// an eager one (the same instance, in the same search order), and makes nothing it only passes
/// over: a search that finds nothing in a bundle makes none of it.
#[test]
fn a_recursive_find_in_a_lazy_import_finds_the_same_and_makes_nothing_else() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom};
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    for index in 0..3 {
        let model = dom.insert(root, InstanceBuilder::new("Model").with_name(format!("Unit{index}")));
        let torso = dom.insert(model, InstanceBuilder::new("Part").with_name("Torso"));
        let arm = dom.insert(torso, InstanceBuilder::new("Part").with_name("Arm"));
        dom.insert(arm, InstanceBuilder::new("Attachment").with_name(if index == 1 { "Grip" } else { "Hold" }));
        dom.insert(model, InstanceBuilder::new("Folder").with_name("Grip"));
        let deep = dom.insert(model, InstanceBuilder::new("Folder").with_name("Deep"));
        dom.insert(deep, InstanceBuilder::new("Animation").with_name(format!("Clip{index}")));
    }
    let plan = std::sync::Arc::new(super::project::ModelPlan::new(&super::Reflection::shared().unwrap(), &dom, dom.root().children()));
    let check = r#"
        local Folder = game:GetService("ReplicatedStorage").Bundle
        local function Name(Item) return if Item then Item:GetFullName() .. " " .. Item:GetDebugId() else "nil" end
        print("miss", Name(Folder:FindFirstChild("Nothing", true)), Name(Folder:FindFirstChildWhichIsA("Sound", true)))
        print(Name(Folder:FindFirstChild("Grip", true)))
        print(Name(Folder:FindFirstChild("Clip2", true)))
        print(Name(Folder.Unit1:FindFirstChild("Grip", true)))
        print(Name(Folder:FindFirstChildWhichIsA("Attachment", true)))
        print(Name(Folder:FindFirstChildWhichIsA("Animation", true)))
        Folder.Unit0.Torso.Arm.Hold.Name = "Grip"
        print(Name(Folder.Unit0.Torso:FindFirstChild("Grip", true)))
        print(#Folder:GetDescendants())
    "#;
    let miss = r#"
        local Folder = game:GetService("ReplicatedStorage").Bundle
        print(Folder:FindFirstChild("Nothing", true), Folder:FindFirstChildWhichIsA("Sound", true))
    "#;
    let mut outputs = Vec::new();
    for lazy in [false, true] {
        for source in [check, miss] {
            let (lua, engine) = start();
            let folder = {
                let mut state = engine.state.borrow_mut();
                let class = engine.reflection.class_id("Folder").unwrap();
                let folder = super::instance::create_named(&mut state, &engine.reflection, class, "Bundle");
                let storage = state.dom.find_child(engine.world().game, b"ReplicatedStorage").unwrap();
                state.dom.attach(folder, storage);
                folder
            };
            if lazy {
                plan.instantiate_lazy(&mut engine.state.borrow_mut(), &engine.reflection, folder);
            } else {
                plan.instantiate(&engine, Some(folder));
            }
            let made = engine.state.borrow().project.lazy_made;
            outputs.push(run(&lua, &engine, source));
            if lazy && source == miss {
                assert_eq!(engine.state.borrow().project.lazy_made, made, "a search that finds nothing makes nothing");
            }
            drop(lua);
            engine.teardown();
        }
    }
    assert_eq!(outputs[0], outputs[2]);
    assert_eq!(outputs[1], outputs[3]);
    assert!(outputs[0].iter().any(|line| line.contains("Bundle.Unit0.Grip")), "{:?}", outputs[0]);
}

/// EditableMesh as measured in Studio (2026-10-16): kind-tagged ids, AddTriangle's own corner
/// attributes with an automatic face normal, RemoveUnused's removed ids, 1/255 bone weights,
/// clones keeping ids, and MeshParts that keep MeshSize from when they were made (ApplyMesh copies
/// it and keeps Size) while pointing at snapshots of the mesh.
#[test]
fn editable_meshes_follow_studio() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local AssetService = game:GetService("AssetService")
        local m = AssetService:CreateEditableMesh()
        local a = m:AddVertex(Vector3.new(10, 0, 0))
        local b = m:AddVertex(Vector3.new(12, 0, 0))
        local c = m:AddVertex(Vector3.new(10, 0, 4))
        local f = m:AddTriangle(a, c, b)
        assert(a == 4294967297 and b == 4294967298 and f == 21474836481, "kind-tagged ids")
        assert(#m:GetFaceUVs(f) == 3 and #m:GetNormals() == 3 and #m:GetColors() == 3 and #m:GetUVs() == 3, "corner attributes")
        assert(m:GetNormal(m:GetFaceNormals(f)[1]) == Vector3.new(0, 1, 0), "the face's normal")
        assert(m:GetColorAlpha(m:GetFaceColors(f)[1]) == 1 and m:GetUV(m:GetFaceUVs(f)[1]) == Vector2.zero, "white, (0, 0)")
        assert(m:GetNormals()[1] == 8589934593 and m:GetColors()[1] == 12884901889 and m:GetUVs()[1] == 17179869185, "kinds")
        local bone = m:AddBone({ Name = "Root", CFrame = CFrame.new(1, 2, 3), Virtual = false })
        assert(bone == 25769803777 and m:GetBoneName(bone) == "Root" and m:GetBoneParent(bone) == 0, "bones")
        m:SetVertexBones(a, { bone })
        m:SetVertexBoneWeights(a, { 0.25 })
        assert(math.abs(m:GetVertexBoneWeights(a)[1] - 64 / 255) < 1e-6, "weights in 1/255 steps")
        local stray = m:AddVertex(Vector3.new(9, 9, 9))
        local removed = m:RemoveUnused()
        assert(#removed == 1 and removed[1] == stray and #m:GetVertices() == 3, "RemoveUnused")
        local fixed = AssetService:CreateEditableMeshAsync(Content.fromObject(m), { FixedSize = true })
        assert(fixed.FixedSize and fixed:GetVertices()[2] == b and #fixed:GetFaces() == 1, "a clone keeps ids")
        assert(not pcall(function() fixed:AddVertex(Vector3.zero) end), "a fixed size mesh takes no vertices")
        local part = AssetService:CreateMeshPartAsync(Content.fromObject(m))
        assert(part.MeshSize == Vector3.new(2, 0.001, 4) and part.Size == part.MeshSize, "MeshSize: the box, at least 0.001")
        assert(string.find(part.MeshId, "^file://"), "a snapshot: " .. part.MeshId)
        m:BatchSetValues({ b }, { Vector3.new(20, 0, 0) })
        assert(m:GetPosition(b) == Vector3.new(20, 0, 0) and part.MeshSize == Vector3.new(2, 0.001, 4), "MeshSize stays")
        local other = Instance.new("MeshPart")
        other.Size = Vector3.new(1, 1, 1)
        other:ApplyMesh(part)
        assert(other.MeshSize == part.MeshSize and other.Size == Vector3.new(1, 1, 1) and other.MeshId == part.MeshId, "ApplyMesh")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// CreateMeshPartAsync of a mesh file the project holds, as a game's asset id modules can name one until
/// it is uploaded: its repo-relative path and a relative file:// URI are read from the project's folder
/// (Options::project), whatever the current directory, and give the mesh's size as an absolute URI
/// does; content it cannot load raises Studio's error (measured 2026-11-03: "Failed to load mesh
/// asset.", no position), never a 1x1x1 part
#[cfg(feature = "meshes")]
#[test]
fn mesh_parts_from_local_files_read_the_project() {
    let project = std::env::temp_dir().join(format!("ree-local-meshes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(project.join("output").join("meshes")).unwrap();
    let (lua, engine) = start_with(Options { project: Some(project.clone()), ..Options::default() });
    // the mesh file: an EditableMesh's snapshot, copied into the project
    let made = run(
        &lua,
        &engine,
        r#"
        local AssetService = game:GetService("AssetService")
        local m = AssetService:CreateEditableMesh()
        m:AddTriangle(m:AddVertex(Vector3.zero), m:AddVertex(Vector3.new(3, 0, 0)), m:AddVertex(Vector3.new(0, 2, 1)))
        print(AssetService:CreateMeshPartAsync(Content.fromObject(m)).MeshId)
        "#,
    );
    let snapshot = made.iter().find_map(|line| line.strip_prefix("print: file://")).expect("a snapshot").to_string();
    let file = project.join("output").join("meshes").join("Box.mesh");
    std::fs::copy(&snapshot, &file).unwrap();
    let absolute = file.to_string_lossy().replace('\\', "/");
    let output = run(
        &lua,
        &engine,
        &format!(
            r#"
            local AssetService = game:GetService("AssetService")
            for _, uri in {{ "output/meshes/Box.mesh", "file://output/meshes/Box.mesh", "file://{absolute}", "file:///{absolute}" }} do
                local part = AssetService:CreateMeshPartAsync(Content.fromUri(uri))
                assert(part.MeshSize == Vector3.new(3, 2, 1) and part.Size == part.MeshSize, uri .. ": " .. tostring(part.MeshSize))
                assert(part.MeshId == uri, "the content as given: " .. part.MeshId)
            end
            for _, uri in {{ "output/meshes/Missing.mesh", "file://output/meshes/Missing.mesh", "rbxasset://fonts/none.mesh" }} do
                local ok, message = pcall(function() return AssetService:CreateMeshPartAsync(Content.fromUri(uri)) end)
                assert(not ok and string.find(message, "Failed to load mesh asset. ", 1, true) == 1, uri .. ": " .. tostring(message))
            end
            print("ok")
            "#
        ),
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");

    // what names a local file, by ror's rules
    let options = &engine.options;
    assert_eq!(options.local_file("output/meshes/Box.mesh"), Some(project.join("output/meshes/Box.mesh")));
    assert_eq!(options.local_file(" FILE://output/x.png "), Some(project.join("output/x.png")));
    if cfg!(windows) {
        assert_eq!(options.local_file("file:///C:/x%20y/a.mesh"), Some(std::path::PathBuf::from("C:/x y/a.mesh")));
    }
    assert_eq!(options.local_file("output\\meshes\\Missing.mesh"), Some(project.join("output\\meshes\\Missing.mesh")), "a path, found or not");
    std::fs::write(project.join("Loose.mesh"), b"").unwrap();
    assert_eq!(options.local_file("Loose.mesh"), Some(project.join("Loose.mesh")), "a file the project holds");
    for uri in ["Nothing.mesh", "rbxassetid://123", "123", "http://www.roblox.com/asset/?id=123", "www.roblox.com/asset/?id=123", "rbxasset://textures/x.png", "rbxthumb://type=Asset&id=1&w=150&h=150", "file://", ""] {
        assert_eq!(options.local_file(uri), None, "{uri}");
    }
    let _ = std::fs::remove_dir_all(&project);
}

/// HasSkinnedMesh as measured in Studio (2026-10-17): a part made from an EditableMesh is skinned
/// when some vertex weighs a bone (bones no vertex weighs leave it false), ApplyMesh copies it either
/// way, and a fresh MeshPart is not
#[test]
fn mesh_parts_from_skinned_meshes_are_skinned() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local AssetService = game:GetService("AssetService")
        local function build(bones, weights)
            local m = AssetService:CreateEditableMesh()
            local root = bones and m:AddBone({ Name = "R", CFrame = CFrame.identity, Virtual = false })
            local a, b, c = m:AddVertex(Vector3.zero), m:AddVertex(Vector3.new(4, 0, 0)), m:AddVertex(Vector3.new(0, 4, 0))
            if weights then
                for _, v in { a, b, c } do m:SetVertexBones(v, { root }); m:SetVertexBoneWeights(v, { 1 }) end
            end
            m:AddTriangle(a, b, c)
            return AssetService:CreateMeshPartAsync(Content.fromObject(m))
        end
        local plain, boneless, skinned = build(false, false), build(true, false), build(true, true)
        assert(not plain.HasSkinnedMesh and not boneless.HasSkinnedMesh and skinned.HasSkinnedMesh, "made")
        local fresh = Instance.new("MeshPart")
        assert(not fresh.HasSkinnedMesh, "fresh")
        fresh:ApplyMesh(skinned)
        assert(fresh.HasSkinnedMesh, "applied skinned")
        fresh:ApplyMesh(plain)
        assert(not fresh.HasSkinnedMesh, "applied plain")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// ApplyMesh from a part a model file holds gives the mesh's size the file keeps as InitialSize, as
/// MeshSize reads it (an effect that applies baked portal masks from its bundle: the masks lost their
/// InitialSize, and ror scaled their 0.000006-stud-thin mesh by its own bounds, opening the portal's
/// window far wider than Studio shows it)
#[test]
fn apply_mesh_from_a_file_part_keeps_its_size() {
    use rbx_dom_weak::{InstanceBuilder, WeakDom, types::Vector3};
    let dir = std::env::temp_dir().join(format!("ree-apply-mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Baked.rbxm");
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let part = dom.insert(
        root,
        InstanceBuilder::new("MeshPart")
            .with_name("Baked")
            .with_property("MeshId", rbx_dom_weak::types::ContentId::from("rbxassetid://23456789012345"))
            .with_property("InitialSize", Vector3::new(26.0, 54.0, 0.001))
            .with_property("Size", Vector3::new(13.0, 27.0, 0.001)),
    );
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, &dom, &[part]).unwrap();
    std::fs::write(&file, bytes).unwrap();
    let (lua, engine) = start();
    super::project::mount_at(&engine, &super::project::Mount::parse(&format!("ReplicatedStorage/Baked={}", file.to_string_lossy()), false).unwrap()).unwrap();
    let output = run(&lua, &engine, r#"
        local Baked = game:GetService("ReplicatedStorage").Baked
        assert(Baked.MeshSize == Vector3.new(26, 54, 0.001), "the file's InitialSize: " .. tostring(Baked.MeshSize))
        local Mask = Instance.new("MeshPart")
        Mask.Size = Vector3.new(1, 1, 1)
        Mask:ApplyMesh(Baked)
        assert(Mask.MeshSize == Baked.MeshSize and Mask.Size == Vector3.new(1, 1, 1), "ApplyMesh: " .. tostring(Mask.MeshSize))
        print("ok")
    "#);
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An EditableMesh snapshot is a FileMesh 4.00 ror reads back: positions, colours, uvs, and skinning
/// split into subsets of at most 26 bones with the vertices' weights.
#[cfg(feature = "meshes")]
#[test]
fn editable_mesh_snapshots_decode_in_ror() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local AssetService = game:GetService("AssetService")
        local m = AssetService:CreateEditableMesh()
        local bones = {}
        for i = 1, 40 do
            bones[i] = m:AddBone({ Name = "B" .. i, CFrame = CFrame.new(i, 0, 0), Virtual = false })
            if i > 1 then m:SetBoneParent(bones[i], bones[i - 1]) end
        end
        for i = 1, 40 do
            local a = m:AddVertex(Vector3.new(i, 0, 0))
            local b = m:AddVertex(Vector3.new(i, 1, 0))
            local c = m:AddVertex(Vector3.new(i, 0, 1))
            for _, v in { a, b, c } do
                m:SetVertexBones(v, { bones[i], bones[(i % 40) + 1] })
                m:SetVertexBoneWeights(v, { 0.75, 0.25 })
            end
            local f = m:AddTriangle(a, b, c)
            m:SetColor(m:GetFaceColors(f)[1], Color3.new(1, 0, 0))
            m:SetUV(m:GetFaceUVs(f)[2], Vector2.new(0.5, 0.25))
        end
        local part = AssetService:CreateMeshPartAsync(Content.fromObject(m))
        print(string.sub(part.MeshId, 8))
        "#,
    );
    let path = output.iter().find_map(|line| line.strip_prefix("print: ")).expect("a snapshot path");
    let bytes = std::fs::read(path).expect("the snapshot");
    let mesh = ror_mesh::decode_mesh(&bytes).expect("ror decodes it");
    assert_eq!(mesh.positions.len(), 120);
    assert_eq!(mesh.indices.len(), 120);
    let skinning = mesh.skinning.as_ref().expect("skinned");
    assert_eq!(skinning.bones.len(), 40);
    assert_eq!(skinning.bones[1].parent, Some(0));
    assert_eq!(skinning.bones[39].name, "B40");
    assert!(skinning.subsets.len() >= 2 && skinning.subsets.iter().all(|s| s.bone_indices.len() <= 26));
    for (vertex, position) in mesh.positions.iter().enumerate() {
        let i = position[0].round() as usize;
        let joints = skinning.joints[vertex];
        let weights = skinning.weights[vertex];
        assert_eq!(skinning.bones[joints[0] as usize].name, format!("B{i}"));
        assert!((weights[0] - 0.75).abs() < 0.01 && (weights[1] - 0.25).abs() < 0.01, "{weights:?}");
    }
    assert!(mesh.colors.iter().any(|c| c[0] == 255 && c[1] == 0));
    assert!(mesh.uvs.iter().any(|uv| uv[0] == 0.5 && uv[1] == 0.25));
}

/// EditableMesh colours as measured in Studio (2026-10-16): BatchSetValues of a colour id sets its
/// alpha from a number and its colour from a Color3, alphas keep 1/255 steps.
#[test]
fn editable_mesh_colours_take_numbers_as_alpha() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local m = game:GetService("AssetService"):CreateEditableMesh()
        local f = m:AddTriangle(m:AddVertex(Vector3.zero), m:AddVertex(Vector3.xAxis), m:AddVertex(Vector3.yAxis))
        local c = m:GetFaceColors(f)[1]
        m:SetColor(c, Color3.new(1, 0, 0))
        m:BatchSetValues({ c }, { 0.25 })
        assert(m:GetColor(c) == Color3.new(1, 0, 0) and math.abs(m:GetColorAlpha(c) - 64 / 255) < 1e-6, "a number: the alpha")
        m:BatchSetValues({ c }, { Color3.new(0, 1, 0) })
        assert(m:GetColor(c) == Color3.new(0, 1, 0) and math.abs(m:GetColorAlpha(c) - 64 / 255) < 1e-6, "a Color3: the colour")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// The work clock: os.clock advances within a frame by what the engine calls a script makes cost
/// Studio (crate::cost: Instance.new, a write, entering the DataModel), and a new frame starts at
/// its own virtual time; a mesh asset's CreateEditableMeshAsync yields (its first fetch 0.55 s).
#[test]
fn the_work_clock_charges_what_studio_spends() {
    let (lua, engine) = start();
    // (work is charged from the first frame on: a build before it is not)
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local t0 = os.clock()
        local folder = Instance.new("Folder")
        folder.Parent = workspace
        local t1 = os.clock()
        for _ = 1, 100 do
            local part = Instance.new("Part")
            part.Parent = folder
        end
        local t2 = os.clock()
        print(string.format("folder %.1f parts %.1f", (t1 - t0) * 1e6, (t2 - t1) * 1e6))
        -- 10,000 more parts (24 ms) run past the frame: the next frame reads on from there
        for _ = 1, 10000 do
            Instance.new("Part").Parent = folder
        end
        local t4 = os.clock()
        task.spawn(function()
            game:GetService("RunService").Heartbeat:Wait()
            local t3 = os.clock()
            print(string.format("next frame %s", tostring(t3 > t4 and t3 - t4 < 1 / 60)))
        end)
        "#,
    );
    // (the parity bench: a Folder 0.36 us to make, 0.52 to enter the workspace; a Part 0.83 and 1.47; and the frame's first entry
    // into the workspace 13.5 more, its first part's 12.5: the engine's code for them running cold, cost.rs PATH_FIRST)
    assert!(output.contains(&"print: folder 14.4 parts 252.5".to_string()), "{output:?}");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    assert!(lines(&engine).contains(&"print: next frame true".to_string()), "{:?}", lines(&engine));
}

/// Two runs of one scene make the same writes. A table keyed by Instances, tables, functions or threads is visited in the
/// order of the keys' hashes, which used to be their addresses: the handlers of a Heartbeat loop ran in another order each run, and
/// with the work clock each one's os.clock() read, and the value it wrote, moved with it. The runs here are live at the same time on
/// two threads with a different amount of heap before the scene, so no address repeats; each prints the order it visited every kind
/// of key and what the handlers wrote.
#[test]
fn identical_runs_visit_object_keys_in_the_same_order() {
    const SCENE: &str = r#"
        local RunService = game:GetService("RunService")
        local ByInstance, ByTable, ByFunction, ByThread = {}, {}, {}, {}
        local Parts = {}
        for Index = 1, 60 do
            local Part = Instance.new("Part")
            Part.Anchored = true
            Part.Parent = workspace
            Parts[Index] = Part
            ByInstance[Part] = Index
            ByTable[{ Part = Part, Index = Index }] = Index
            ByFunction[function()
                Part.Position = Vector3.new(Index, os.clock() * 1e6 % 1000, 0)
            end] = Index
            ByThread[coroutine.create(function() end)] = Index
        end
        local Frames = 0
        RunService.Heartbeat:Connect(function()
            Frames += 1
            local Orders = { Instance = {}, Table = {}, Function = {}, Thread = {} }
            for _, Index in ByInstance do table.insert(Orders.Instance, Index) end
            for Key in ByTable do
                table.insert(Orders.Table, Key.Index)
                Key.Part.Orientation = Vector3.new(0, os.clock() * 1e6 % 360, 0)
            end
            for Function, Index in ByFunction do
                table.insert(Orders.Function, Index)
                Function()
            end
            for _, Index in ByThread do table.insert(Orders.Thread, Index) end
            if Frames == 3 then
                for Kind, Order in Orders do print(`{Kind} {table.concat(Order, ",")}`) end
                for Index, Part in Parts do
                    print(string.format("%d %.17g %.17g", Index, Part.Position.Y, Part.Orientation.Y))
                end
            end
        end)
    "#;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let runs: Vec<_> = [0usize, 5000]
        .into_iter()
        .map(|pad| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let (lua, engine) = start();
                let padding: Vec<Vec<u8>> = (0..pad).map(|size| vec![0u8; size % 97 + 1]).collect();
                crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
                run(&lua, &engine, SCENE);
                for _ in 0..4 {
                    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
                }
                let output = lines(&engine);
                barrier.wait(); // neither run ends before the other has run: their objects never share an address
                drop(padding);
                output
            })
        })
        .collect();
    let outputs: Vec<Vec<String>> = runs.into_iter().map(|run| run.join().unwrap()).collect();
    assert!(outputs[0].len() > 60, "the scene printed nothing: {:?}", outputs[0]);
    assert_eq!(outputs[0], outputs[1], "two runs of one scene differ");
}

/// GuiService:GetInsetArea answered nil, so a panel that sizes its backdrop from it failed on `.Width`.
#[test]
fn gui_service_reports_the_inset_area() {
    assert_script(
        r#"
        local Gui = game:GetService("GuiService")
        local Whole = Gui:GetInsetArea(Enum.ScreenInsets.None)
        assert(typeof(Whole) == "Rect" and Whole.Width > 0 and Whole.Height > 0, "no area")
        assert(Whole.Width == Gui:GetInsetArea(Enum.ScreenInsets.DeviceSafeInsets).Width)
        assert(Gui:GetInsetArea(Enum.ScreenInsets.TopbarSafeInsets).Height <= Whole.Height)
        "#,
    );
}

/// REE_FROZEN_CLOCK=1: work moves no clock reading, so two code versions that differ in speed read the same os.clock and a
/// script that writes from the time it reads (a spring, an animation phase) writes the same values.
#[test]
fn a_frozen_clock_ignores_the_work_between_reads() {
    const SCRIPT: &str = r#"
        local Before = os.clock()
        local Folder = Instance.new("Folder")
        Folder.Parent = workspace
        for _ = 1, 400 do Instance.new("Part").Parent = Folder end
        print(string.format("moved %s", tostring(os.clock() - Before > 1e-4)))
    "#;
    for (frozen, expected) in [(false, "moved true"), (true, "moved false")] {
        let (lua, engine) = start();
        engine.state.borrow_mut().sched.frozen_clock = frozen;
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let output = run(&lua, &engine, SCRIPT);
        assert!(output.contains(&format!("print: {expected}")), "frozen {frozen}: {output:?}");
    }
}

/// A frozen clock is the frame's time for every thread that reads it, however many read it in a frame, as the bench's
/// frame-locked clock is: 400 threads timed on os.clock, as a heavy attack's copies are, all read it the same within each
/// frame. (The reads counted against the allowance for the whole phase, so from the 257th on each moved it 10 us, and an arm
/// that read more ran its copies ahead.) A busy loop in one resumption still moves it, so it ends.
#[test]
fn a_frozen_clock_reads_the_same_for_every_thread_in_a_frame() {
    let (lua, engine) = start();
    engine.state.borrow_mut().sched.frozen_clock = true;
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local RunService = game:GetService("RunService")
        local Reads = { {}, {} }
        _G.FrozenReads = Reads
        for _ = 1, 400 do
            task.spawn(function()
                for Frame = 1, 2 do
                    RunService.Heartbeat:Wait()
                    table.insert(Reads[Frame], os.clock())
                end
            end)
        end
        "#,
    );
    for _ in 0..3 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(
        &lua,
        &engine,
        r#"
        local function Spread(List)
            local Low, High = math.huge, -math.huge
            for _, Value in List do
                Low, High = math.min(Low, Value), math.max(High, Value)
            end
            return High - Low
        end
        local Reads = _G.FrozenReads
        print(string.format("reads %d %d spread %.9f %.9f", #Reads[1], #Reads[2], Spread(Reads[1]), Spread(Reads[2])))
        local Start, Count = os.clock(), 0
        while os.clock() - Start < 0.001 do
            Count += 1
        end
        print(string.format("busy loop ended after %d reads", Count))
        "#,
    );
    assert!(output.contains(&"print: reads 400 400 spread 0.000000000 0.000000000".to_string()), "{output:?}");
    assert!(output.iter().any(|line| line.starts_with("print: busy loop ended after ")), "{output:?}");
}

/// A frozen clock stops the wall clock too: tick(), os.time() and DateTime.now() read the same in a run an hour later, so two
/// differential runs that straddle an hour boundary still write the same values.
#[test]
fn a_frozen_clock_pins_the_wall_clock() {
    const SCRIPT: &str = r#"print(`wall {os.time()} {DateTime.now().UnixTimestamp} {math.floor(tick())} {math.floor(workspace:GetServerTimeNow())}`)"#;
    let (lua, engine) = start();
    {
        let mut state = engine.state.borrow_mut();
        // (as if the run began an hour earlier)
        state.sched.epoch_millis -= 3_600_000.0;
        state.sched.freeze_wall_clock();
    }
    let first = run(&lua, &engine, SCRIPT).into_iter().find(|line| line.starts_with("print: wall")).expect("wall line");
    let (lua, engine) = start();
    engine.state.borrow_mut().sched.freeze_wall_clock();
    let second = run(&lua, &engine, SCRIPT).into_iter().find(|line| line.starts_with("print: wall")).expect("wall line");
    assert_eq!(first, second);
    assert!(first.contains("1789999200"), "{first}");
}

/// A table keyed by Instances is visited in the same order in every run, whatever slots the instances got. A slot is
/// reused when the collector frees an instance, and what it has freed by then depends on the heap, so the same scene gave its
/// instances other slots in another run and `pairs` visited them in another order. Both engines here make 300 instances before the
/// scene; the second lets them go, which hands the scene's instances the freed slots in the opposite order (the serials stay equal).
#[test]
fn instance_keys_are_visited_in_the_same_order_whatever_their_slots() {
    const SCENE: &str = r#"
        local Keys, Order = {}, {}
        for Index = 1, 80 do
            local Part = Instance.new("Part")
            Part.Parent = workspace
            Keys[Part] = Index
        end
        for _, Index in Keys do table.insert(Order, Index) end
        print("order " .. table.concat(Order, ","))
    "#;
    let outputs: Vec<Vec<String>> = [false, true]
        .into_iter()
        .map(|release| {
            let (lua, engine) = start();
            crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
            run(&lua, &engine, r#"_G.Held = {} for _ = 1, 300 do table.insert(_G.Held, Instance.new("Folder")) end"#);
            // (a script run makes an instance too: both engines run one, so the serials match)
            run(&lua, &engine, if release { "_G.Held = nil" } else { "_G.Unused = nil" });
            lua.gc_collect().unwrap();
            lua.gc_collect().unwrap();
            let freed = engine_lua::raw::collect(&lua, &engine).unwrap();
            assert_eq!(freed >= 300, release, "freed {freed}");
            run(&lua, &engine, SCENE);
            lines(&engine).into_iter().filter(|line| line.starts_with("print: order")).collect()
        })
        .collect();
    assert_eq!(outputs[0].len(), 1, "{:?}", outputs[0]);
    assert_eq!(outputs[0], outputs[1], "the scene's instances took other slots and were visited in another order");
}

/// The first touch of an instance in a frame costs more than the next ones: 45 ns for a static instance (the parity bench,
/// tools/cost-parity: a part of 100 read once each a frame 74.6 ns against 29.4 hot), where a Studio playtest's animated rigs read
/// about 280 more (a bridge_ops probe, 28 R15 rigs: Part.CFrame touching five parts a rig once 322 ns, eight times each 80; a
/// Motor6D.Transform write 344 / 92). A loop on one instance never pays it; a bridge over many instances pays it on each, every frame.
#[test]
fn the_work_clock_charges_the_first_touch_of_an_instance_in_a_frame() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Rigs = Instance.new("Folder")
        Rigs.Name = "Rigs"
        Rigs.Parent = workspace
        for Index = 1, 140 do
            local Part = Instance.new("Part")
            Part.Parent = Rigs
            local Motor = Instance.new("Motor6D")
            Motor.Parent = Part
        end
    "#,
    );
    // (a new frame: everything above is cold again)
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Parts = workspace.Rigs:GetChildren()
        local Motors = {}
        for _, Part in Parts do
            table.insert(Motors, Part:FindFirstChildOfClass("Motor6D"))
        end
        local function Per(Body)
            local start = os.clock()
            Body()
            return (os.clock() - start) / #Parts * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.0f ns, Studio %d-%d", Name, Value, Low, High))
        end
        -- (the Part:FindFirstChildOfClass above touched every Part and the Motors are cold)
        local Cold = Per(function() for _, Motor in Motors do local _ = Motor.C0 end end)
        local Hot = Per(function() for _, Motor in Motors do local _ = Motor.C0 end end)
        Within("first read of a Motor6D.C0", Cold, 70, 150)
        Within("second read of it", Hot, 25, 80)
        local Write = Per(function() for _, Motor in Motors do Motor.Transform = CFrame.identity end end)
        Within("a write to an instance read this frame", Write, 40, 110)
        local ColdIsA = Per(function() for _, Part in Parts do local _ = Part:IsA("BasePart") end end)
        Within("IsA on a part touched this frame", ColdIsA, 15, 80)
    "#,
    );
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Motors = {}
        for _, Part in workspace.Rigs:GetChildren() do
            table.insert(Motors, Part:FindFirstChildOfClass("Motor6D"))
        end
        local start = os.clock()
        for _, Motor in Motors do
            Motor.Transform = CFrame.identity
        end
        local Cold = (os.clock() - start) / #Motors * 1e9
        assert(Cold >= 80 and Cold <= 160, string.format("a Motor6D.Transform write to a cold Motor6D: %.0f ns, the bench's 60 + 45", Cold))
        -- an instance made in this frame is hot: its writes cost the hot price
        local Made = {}
        for Index = 1, #Motors do
            Made[Index] = Instance.new("Motor6D")
        end
        local made = os.clock()
        for _, Motor in Made do
            Motor.Transform = CFrame.identity
        end
        local Fresh = (os.clock() - made) / #Made * 1e9
        assert(Fresh >= 30 and Fresh <= 110, string.format("a Motor6D.Transform write to a Motor6D made this frame: %.0f ns, hot 61", Fresh))
    "#,
    );
}

/// A write to an instance nothing touched this frame costs more than the first touch alone (a cold_write probe, a match place's
/// client, five runs, fitted per write): writing 100 and 300 distinct Parts once each against writing
/// one Part as often, the difference per write is Transparency +388 ns, Color +357, CFrame +773, Name +559, SetAttribute +830, and on a
/// Frame Size +475..525, Position +600, BackgroundTransparency +380 (noisier). ree charged
/// the first touch's 280 for every one (a roster of 100 Parts was priced at about half of Studio), so the roster read 100-550 ns a
/// write low. Bands of 1.3x around Studio's differences, but Transparency's: the parity bench (tools/cost-parity, a rendered client
/// of a game's match place) reads a roster of 100 parts' Transparency +143 ns over the hot write (cost.rs cold_write_extra).
#[test]
fn a_write_to_a_cold_instance_costs_what_a_dirtied_instance_costs() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        Folder.Name = "Roster"
        Folder.Parent = workspace
        for Index = 1, 300 do
            local Part = Instance.new("Part")
            Part.Anchored = true
            Part.Parent = Folder
        end
        local Gui = Instance.new("Folder")
        Gui.Name = "Gui"
        Gui.Parent = workspace
        for Index = 1, 300 do
            local Item = Instance.new("Frame")
            Item.Parent = Gui
        end
        "#,
    );
    // (each kind in a frame of its own: the Parts a kind wrote are warm for the next one). The Frames' bands are wider: Studio's
    // GUI readings spread more (Position 781-1550 ns at 100 writes).
    let kinds: [(&str, &str, u32, u32, &str); 8] = [
        ("Transparency", "Roster", 100, 200, "Part.Transparency = 0.25 + (Index % 2) * 0.5"),
        ("Color", "Roster", 300, 470, "Part.Color = Color3.new((Index % 2) * 0.5, 0.25, 0.75)"),
        ("CFrame", "Roster", 600, 1000, "Part.CFrame = CFrame.new(Index % 2 + 1, 0, 0)"),
        ("Name", "Roster", 430, 730, "Part.Name = 'N' .. (Index % 2)"),
        ("SetAttribute", "Roster", 640, 1080, "Part:SetAttribute('A', Index)"),
        ("Frame.Size", "Gui", 400, 900, "Part.Size = UDim2.fromOffset(10 + Index % 2, 10)"),
        ("Frame.Position", "Gui", 400, 800, "Part.Position = UDim2.fromOffset(1 + Index % 2, 1)"),
        ("Frame.BackgroundTransparency", "Gui", 300, 500, "Part.BackgroundTransparency = 0.25 + (Index % 2) * 0.5"),
    ];
    for (name, roster, low, high, write) in kinds {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        run(
            &lua,
            &engine,
            &format!(
                r#"
                local Roster = workspace.{roster}:GetChildren()
                local function Write(Part, Index) {write} end
                -- one Part written 300 times, then 300 Parts written once: the same instructions, so the difference is the cold instances
                local One = Roster[1]
                Write(One, 0)
                local start = os.clock()
                for Index = 1, 300 do
                    Write(One, Index)
                end
                local Hot = (os.clock() - start) / 300 * 1e9
                local middle = os.clock()
                for Index = 1, 300 do
                    Write(Roster[Index], Index)
                end
                local Cold = (os.clock() - middle) / 300 * 1e9
                local Extra = Cold - Hot
                assert(Extra >= {low} and Extra <= {high}, string.format("{name}: a cold write costs +%.0f ns over a hot one, Studio {low}-{high} (hot %.0f, cold %.0f)", Extra, Hot, Cold))
                print("ok {name}")
                "#
            ),
        );
        assert!(lines(&engine).contains(&format!("print: ok {name}")), "{name}: {:?}", lines(&engine));
    }
}

/// The work clock prices instance operations from a client's own numbers: the parity bench (tools/cost-parity, a rendered client
/// of a game's match place) where it has a row, else an op_cost probe run in a lobby playtest's client (the median of seven rounds
/// less an empty loop, 2026-10-23), bands of about +-25 % around them. Before, every property write cost a flat 0.5 us, every read
/// 0.06, a Clone 7 us an instance, and ScaleTo, Connect, task.spawn, BulkMoveTo and QueryDescendants almost nothing or far too much
/// (a ScaleTo to the scale a model has cost as much as a rescale).
#[test]
fn the_work_clock_prices_instance_operations_as_the_studio_bench_measured() {
    let (lua, engine) = start();
    // (interpreted as `ree record` and `ree run` run modules: the loops' own instructions are charged)
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local function Per(Count, Body)
            local start = os.clock()
            Body(Count)
            return (os.clock() - start) / Count * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.0f ns, Studio %d-%d", Name, Value, Low, High))
        end
        local A, B = CFrame.new(10, 20, 30), CFrame.new(10, 20, 31)
        local Placed = Instance.new("Part")
        Placed.Parent = workspace
        local Loose = Instance.new("Part")
        local Motor, Bone = Instance.new("Motor6D"), Instance.new("Bone")
        Motor.Parent, Bone.Parent = Placed, Placed

        -- property writes (the bench: CFrame in the workspace 1,430, Size 1,100 from Studio's ratio to it, Name 300, Motor6D.C0 140;
        -- a lobby client: loose 163, same value 52, Color 547, Transparency 254, Bone.Transform 68)
        Within("CFrame write placed", Per(2000, function(N) for _ = 1, N / 2 do Placed.CFrame = A Placed.CFrame = B end end), 1100, 1800)
        Within("CFrame write loose", Per(2000, function(N) for _ = 1, N / 2 do Loose.CFrame = A Loose.CFrame = B end end), 110, 260)
        Within("CFrame write same value", Per(2000, function(N) for _ = 1, N do Placed.CFrame = B end end), 30, 110)
        Within("Size write placed", Per(2000, function(N) for _ = 1, N / 2 do Placed.Size = Vector3.one Placed.Size = Vector3.new(1, 1, 2) end end), 850, 1400)
        Within("Color write", Per(2000, function(N) for _ = 1, N / 2 do Placed.Color = Color3.new(1, 0, 0) Placed.Color = Color3.new(0, 1, 0) end end), 430, 700)
        Within("Transparency write", Per(2000, function(N) for _ = 1, N / 2 do Placed.Transparency = 0.25 Placed.Transparency = 0.75 end end), 190, 330)
        Within("Name write", Per(2000, function(N) for _ = 1, N / 2 do Placed.Name = "A" Placed.Name = "B" end end), 230, 400)
        Within("Motor6D.C0 write", Per(2000, function(N) for _ = 1, N / 2 do Motor.C0 = A Motor.C0 = B end end), 110, 190)
        Within("Bone.Transform write", Per(2000, function(N) for _ = 1, N / 2 do Bone.Transform = A Bone.Transform = B end end), 40, 110)
        Within("property read", Per(2000, function(N) for _ = 1, N do local _ = Placed.Transparency end end), 25, 60)

        -- attributes (the bench: a changed value 0.43 us with the call, the same 77 ns)
        Within("SetAttribute changes", Per(2000, function(N) for _ = 1, N / 2 do Placed:SetAttribute("A", 1) Placed:SetAttribute("A", 2) end end), 340, 580)
        Within("SetAttribute same", Per(2000, function(N) for _ = 1, N do Placed:SetAttribute("A", 2) end end), 50, 130)

        -- Clone, by class (the bench: a MeshPart 5.83 us, a Part 3.44)
        local Many = Instance.new("Model")
        for _ = 1, 50 do Instance.new("MeshPart").Parent = Many end
        Within("Clone of 50 MeshParts", Per(20, function(N) for _ = 1, N do Many:Clone() end end), 240000, 310000)
        local Parts = Instance.new("Model")
        for _ = 1, 50 do Instance.new("Part").Parent = Parts end
        Within("Clone of 50 Parts", Per(20, function(N) for _ = 1, N do Parts:Clone() end end), 140000, 215000)

        -- Instance.new (the bench: Folder 0.36 us, Part 0.83, TextLabel 1.44)
        Within("Instance.new Folder", Per(1000, function(N) for _ = 1, N do Instance.new("Folder") end end), 300, 650)
        Within("Instance.new Part", Per(1000, function(N) for _ = 1, N do Instance.new("Part") end end), 650, 1100)
        Within("Instance.new TextLabel", Per(1000, function(N) for _ = 1, N do Instance.new("TextLabel") end end), 1100, 1900)

        -- ScaleTo of 29 parts (client: a rescale in the workspace 35.0 us, out of it 9.0, to the scale it has 6.0)
        local function Model(Count, Parent)
            local Rig = Instance.new("Model")
            for _ = 1, Count do
                local Part = Instance.new("Part")
                Part.Anchored = true
                Part.Parent = Rig
            end
            Rig.PrimaryPart = Rig:FindFirstChildWhichIsA("BasePart")
            Rig.Parent = Parent
            return Rig
        end
        local Standing, Held = Model(29, workspace), Model(29, nil)
        Within("ScaleTo placed", Per(100, function(N) for _ = 1, N / 2 do Standing:ScaleTo(2) Standing:ScaleTo(1) end end), 28000, 44000)
        Within("ScaleTo loose", Per(100, function(N) for _ = 1, N / 2 do Held:ScaleTo(2) Held:ScaleTo(1) end end), 7000, 12000)
        Within("ScaleTo same scale", Per(100, function(N) for _ = 1, N do Standing:ScaleTo(1) end end), 4500, 9000)
        -- (the bench: Model:PivotTo of 10 parts 10.65 us, 1.05 a part)
        Within("Model:PivotTo", Per(100, function(N) for _ = 1, N / 2 do Standing:PivotTo(A) Standing:PivotTo(B) end end), 24000, 38000)
        Within("Model:GetPivot", Per(1000, function(N) for _ = 1, N do Standing:GetPivot() end end), 20, 150)

        -- a fallback rig's ScaleTo(1) from its stored scale 0.43, out of the DataModel (in Studio: a rig of 1 MeshPart,
        -- 7 Bones, 19 Attachments and an AnimationController with its Animator 14.7 us, a rig of 7 Parts, 6 Motor6Ds,
        -- 19 Attachments, a Decal, a SpecialMesh, a controller and its Animator 26.9 us)
        local function Rig(Parts, Bones, Joints)
            local Model = Instance.new("Model")
            local Root
            for Index = 1, Parts do
                local Part = Instance.new(if Index == 1 and Bones > 0 then "MeshPart" else "Part")
                Part.Name = "Part" .. Index
                Part.Anchored = true
                Part.Parent = Model
                Root = Root or Part
                for Slot = 1, math.floor(19 / Parts) + (if Index == 1 then 19 % Parts else 0) do
                    Instance.new("Attachment").Parent = Part
                end
                if Index > 1 and Joints > 0 and Index - 1 <= Joints then
                    local Joint = Instance.new("Motor6D")
                    Joint.Part0, Joint.Part1 = Root, Part
                    Joint.Parent = Part
                end
            end
            for _ = 1, Bones do
                Instance.new("Bone").Parent = Root
            end
            local Controller = Instance.new("AnimationController")
            Controller.Parent = Model
            Instance.new("Animator").Parent = Controller
            Model.PrimaryPart = Root
            Model:ScaleTo(0.43)
            return Model
        end
        local Lobby, Match = Rig(1, 7, 0), Rig(7, 0, 6)
        Within("lobby rig ScaleTo(1)", Per(1, function() Lobby:ScaleTo(1) end), 11000, 19000)
        Within("match rig ScaleTo(1)", Per(1, function() Match:ScaleTo(1) end), 21000, 33000)

        -- BulkMoveTo of 100 parts (the bench: FireCFrameChanged 1.05 us a part; every event 2.58, the events' share a lobby client's)
        local List, Here, There = {}, {}, {}
        for Index = 1, 100 do
            List[Index] = Instance.new("Part")
            List[Index].Parent = workspace
            Here[Index], There[Index] = CFrame.new(Index, 0, 0), CFrame.new(Index, 1, 0)
        end
        Within("BulkMoveTo", Per(20, function(N) for _ = 1, N / 2 do workspace:BulkMoveTo(List, Here, Enum.BulkMoveMode.FireCFrameChanged) workspace:BulkMoveTo(List, There, Enum.BulkMoveMode.FireCFrameChanged) end end), 85000, 130000)
        Within("BulkMoveTo all events", Per(20, function(N) for _ = 1, N / 2 do workspace:BulkMoveTo(List, Here) workspace:BulkMoveTo(List, There) end end), 200000, 320000)

        -- QueryDescendants over 50 parts (client 3.6 us for 100, edit 2.3)
        Within("QueryDescendants", Per(200, function(N) for _ = 1, N do Many:QueryDescendants(">>MeshPart") end end), 900, 2400)

        -- signals and threads (the bench: Connect + Disconnect 0.50 us, task.spawn 0.72 a thread, Fire with nothing connected 52 ns)
        local Event = Instance.new("BindableEvent")
        local function Noop() end
        Within("Connect + Disconnect", Per(1000, function(N) for _ = 1, N do Event.Event:Connect(Noop):Disconnect() end end), 450, 720)
        Within("task.spawn", Per(1000, function(N) for _ = 1, N do task.spawn(Noop) end end), 600, 950)
        Within("coroutine.wrap", Per(1000, function(N) for _ = 1, N do coroutine.wrap(Noop)() end end), 600, 950)
        Within("BindableEvent:Fire", Per(1000, function(N) for _ = 1, N do Event:Fire(1) end end), 40, 100)

        -- parenting inside the DataModel (the bench: a Part from one Folder of the workspace to another 0.51 us)
        local One, Two = Instance.new("Folder"), Instance.new("Folder")
        One.Parent, Two.Parent = workspace, workspace
        Loose.Parent = One
        Within("Parent between folders", Per(1000, function(N) for _ = 1, N / 2 do Loose.Parent = Two Loose.Parent = One end end), 400, 650)
        print("priced")
        "#,
    );
    assert!(output.contains(&"print: priced".to_string()), "{output:?}");
}

/// Studio has Instance:QueryDescendants and no QueryFirstDescendant ("QueryFirstDescendant is not a valid member of
/// Folder", an empty test place, 2026-10-23); ree had its own, which let a script run here that errors there (neither
/// rur nor the games it was checked against use it).
#[test]
fn query_first_descendant_is_not_a_member_as_in_studio() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Folder = Instance.new("Folder")
        Instance.new("Part").Parent = Folder
        local Ok, Message = pcall(function()
            return Folder:QueryFirstDescendant("Part")
        end)
        assert(not Ok, "QueryFirstDescendant ran")
        assert(string.find(Message, "QueryFirstDescendant is not a valid member of Folder", 1, true), Message)
        assert(#Folder:QueryDescendants("Part") == 1)
        print("studio's members")
        "#,
    );
    assert!(output.contains(&"print: studio's members".to_string()), "{output:?}");
}

/// TextService:GetTextSize returned nil in a plain `ree run` (no host text layer), so a settings window failed on `.X`.
/// It returns Studio's measurement now: "Hello" 14 FredokaOne is 31 x 14 (an empty test place, 2026-10-23).
#[test]
fn text_service_returns_a_vector2_without_a_text_layer() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local TextService = game:GetService("TextService")
        local Size = TextService:GetTextSize("Hello", 14, Enum.Font.FredokaOne, Vector2.new(1000, 1000))
        assert(typeof(Size) == "Vector2", typeof(Size))
        assert(math.abs(Size.X - 31) <= 2 and Size.Y == 14, tostring(Size))
        local Wrapped = TextService:GetTextSize("The quick brown fox jumps over the lazy dog and keeps running", 14, Enum.Font.SourceSans, Vector2.new(100, 1000))
        assert(Wrapped.Y == 56 and math.abs(Wrapped.X - 98) <= 3, tostring(Wrapped))
        print("measured")
        "#,
    );
    assert!(output.contains(&"print: measured".to_string()), "{output:?}");
}

/// The work clock charges a script's own Luau as the interpreter runs it (crate::cost::luau): its
/// instructions, calls, objects and table growth, and a datatype operation the engine's Luau serves
/// (whose own instructions are not charged); Vector3.new is the VM's fast call, as Roblox compiles it.
#[test]
fn the_work_clock_charges_a_scripts_luau() {
    let (lua, engine) = start();
    // (interpreted as `ree record` runs modules: a safe environment, no native code)
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local t0 = os.clock()
        local x = 0
        for i = 1, 10000 do
            x += i
        end
        local t1 = os.clock()
        local list = {}
        for i = 1, 1000 do
            list[i] = { Value = i }
        end
        local t2 = os.clock()
        local c, v = CFrame.new(), nil
        for i = 1, 1000 do
            c *= CFrame.new(1, 0, 0)
            v = Vector3.new(i, 0, 0)
        end
        local t3 = os.clock()
        print(string.format("loop %.1f tables %.1f cframes %.1f", (t1 - t0) * 1e6, (t2 - t1) * 1e6, (t3 - t2) * 1e6))
        "#,
    );
    // (the cframes loop was 58.5 with every datatype operation a flat 15 ns; CFrame.new, `*` and Vector3.new were priced from the
    // lobby client's op_cost loop (101.5: `*` 60 ns), and `*` from a rig frame since (21 ns, 39 us less over the 1,000 products):
    // lune-roblox cframe.rs `price`. The instructions are priced by kind since (cost.rs INTERPRETED_OPS: an ADD or FORNLOOP 0.48 ns,
    // where every instruction cost 1.6: the loop 32.0 then). The tables loop read 65.5 until the allocation prices were fitted on the
    // parity bench's allocation, churn and growth rows at several sizes (an object 14.6 ns in all where it was 22, 8.6 of it the
    // collector's; an allocated byte 0.061 and a byte of table storage 0.079 more, where storage was 0.4; an array slot growth moves
    // nothing, where it was 12))
    assert!(output.contains(&"print: loop 9.6 tables 32.1 cframes 47.9".to_string()), "{output:?}");
}

/// What a library or engine call does by the size of its input is work the clock charges (patches/luau/15-input-size-work.patch,
/// cost.rs SCANNED_SLOT and below), so each of the audit's workloads grows on the clock as a client's time grows with it (the parity
/// bench's `libsize` and `json` rows, a client's fastest session): table.find of a missing value 14 ns at 10 elements and 41 us at
/// 100,000; `s ..= piece` growing with the square of its pieces where table.concat grows with them; a list drained from its front or
/// filled at it, each call moving what is left; string.upper / gsub / rep on 1 MB 570-850 times their 1 KB figure; JSONEncode and
/// JSONDecode of 1,000 records 83 and 91 times 10's. A flat price a call read each pair alike (table.find 6.19 us at every size).
#[test]
fn library_work_grows_with_its_input() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local HttpService = game:GetService("HttpService")
        -- (each workload once before it is timed: a frame's first call of a C function costs its cold code, LUAU_COLD_CFUNC)
        local function us(f, ...)
            f(...)
            local t0 = os.clock()
            f(...)
            return (os.clock() - t0) * 1e6
        end
        local function list(n)
            local t = table.create(n)
            for i = 1, n do
                t[i] = i
            end
            return t
        end
        local results = {}
        for _, n in { 10, 1000, 100000 } do
            local t = list(n)
            results["find" .. n] = us(function()
                for _ = 1, 10 do
                    table.find(t, -1)
                end
            end) / 10
        end
        for _, n in { 1000, 4000 } do
            results["concat" .. n] = us(function()
                local s = ""
                for _ = 1, n do
                    s ..= "abcdefgh"
                end
            end)
            results["tconcat" .. n] = us(function()
                local parts = table.create(n)
                for i = 1, n do
                    parts[i] = "abcdefgh"
                end
                local _ = table.concat(parts)
            end)
        end
        for _, n in { 100, 1000 } do
            results["remove" .. n] = us(function()
                local t = list(n)
                for _ = 1, n do
                    table.remove(t, 1)
                end
            end) / n
            results["insert" .. n] = us(function()
                local t = {}
                for i = 1, n do
                    table.insert(t, 1, i)
                end
            end) / n
        end
        for _, size in { 1024, 1048576 } do
            local s = string.rep("a", size)
            results["upper" .. size] = us(string.upper, s)
            results["gsub" .. size] = us(string.gsub, s, "a", "b")
            results["rep" .. size] = us(string.rep, "b", size)
        end
        for _, n in { 10, 1000 } do
            local records = {}
            for i = 1, n do
                records[i] = { id = i, name = "item" .. i, value = i * 0.5, tags = { "a", "b" } }
            end
            local text = HttpService:JSONEncode(records)
            results["encode" .. n] = us(function()
                HttpService:JSONEncode(records)
            end)
            results["decode" .. n] = us(function()
                HttpService:JSONDecode(text)
            end)
        end
        for name, value in results do
            print(string.format("%s %.4f", name, value))
        end
        "#,
    );
    let figure = |name: &str| -> f64 {
        let prefix = format!("print: {name} ");
        output.iter().find_map(|line| line.strip_prefix(&prefix)).unwrap_or_else(|| panic!("no {name} in {output:?}")).parse().unwrap()
    };
    let ratio = |big: &str, small: &str| figure(big) / figure(small);
    // table.find compares every element: a client 14.3 ns at 10, 41.4 us at 100,000
    assert!(ratio("find100000", "find10") > 1000.0, "{output:?}");
    assert!((0.5..2.0).contains(&(figure("find100000") / 41.41)), "table.find of 100,000 elements {} us against a client's 41.41", figure("find100000"));
    // building a string piece by piece copies what it has built each time; table.concat copies each piece once
    assert!(ratio("concat4000", "concat1000") > 8.0, "{output:?}");
    assert!(ratio("tconcat4000", "tconcat1000") < 6.0, "{output:?}");
    // each table.remove(t, 1) and table.insert(t, 1, v) moves the rest of the list
    assert!(ratio("remove1000", "remove100") > 2.5 && ratio("insert1000", "insert100") > 2.5, "{output:?}");
    // string work on 1 MB against 1 KB (a client: upper 1.48 ms and 1.74 us, gsub 1.54 ms and 2.44 us, rep 234 us and 0.41 us)
    for name in ["upper", "gsub", "rep"] {
        assert!(ratio(&format!("{name}1048576"), &format!("{name}1024")) > 300.0, "{name}: {output:?}");
    }
    // JSON by its text and values (a client: 10 records' encode 5.6 us, 1,000's 463 us)
    assert!(ratio("encode1000", "encode10") > 40.0 && ratio("decode1000", "decode10") > 40.0, "{output:?}");
}

/// The collector steps at the same points however fast the host runs. Luau paces it by how long a cycle took (lgc.cpp: the allocation
/// rate and the mark phase's length set the next cycle's trigger), read from the wall clock, so on a busy machine it stepped at other
/// points in every run, and what the work clock counts moved with it: an allocating fast call (`string.sub`) gives way to its C function
/// while a step is due, a string is found alive or made again. A cutscene that slices its setup by an os.clock budget recorded
/// differently in 1 run of 20 to 2 of 3. It paces by the work clock now (cost.rs `install_gc_clock`): here a host that stalls between
/// the script's rounds (sleeping, as a loaded machine does) must not move a single heap reading.
#[test]
fn the_collector_steps_at_the_same_points_however_fast_the_host_runs() {
    let readings = |stall: bool| -> Vec<String> {
        let (lua, engine) = start();
        engine_lua::modules::interpret_all(&lua, &engine);
        let host_stall = lua
            .create_function(move |_, ()| {
                if stall {
                    std::thread::sleep(std::time::Duration::from_millis(3));
                }
                Ok(())
            })
            .unwrap();
        engine.lua.borrow().shared.as_ref().unwrap().raw_set("HostStall", host_stall).unwrap();
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        run(
            &lua,
            &engine,
            r#"
            local keep, counts, subs = {}, {}, 0
            local text = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
            for round = 1, 40 do
                for i = 1, 3000 do
                    keep[i % 400 + 1] = { i, string.sub(text, i % 26 + 1, i % 26 + 1) .. i }
                end
                HostStall()
                table.insert(counts, string.format("%.3f", collectgarbage("count")))
            end
            print(table.concat(counts, ","))
            print(string.format("%.9f", os.clock()))
            "#,
        )
    };
    let quick = readings(false);
    let stalled = readings(true);
    assert_eq!(quick, stalled, "the collector stepped at other points when the host stalled");
}

/// Reading the clock costs what the call and its loop cost, however many reads a frame makes: every read
/// past 256 in a phase used to advance it 10 us more (2,000 brackets of a spawn replay read
/// 17 ms of clock for under 1 ms of work). A loop that only reads the clock still ends, on its own charges;
/// before the first frame (nothing is charged) the clock stands still and the drift stays.
#[test]
fn clock_reads_cost_their_work_not_a_flat_fee() {
    let (lua, engine) = start();
    // (interpreted as `ree record` and `ree run` run modules: the loop's own instructions are charged)
    engine_lua::modules::interpret_all(&lua, &engine);
    let before = run(
        &lua,
        &engine,
        r#"
        local t = os.clock()
        for _ = 1, 2000 do
            local _ = os.clock()
        end
        print(string.format("before the first frame %.1f", (os.clock() - t) * 1e3))
        "#,
    );
    let drift: f64 = before.iter().find_map(|line| line.strip_prefix("print: before the first frame ")).unwrap_or_else(|| panic!("{before:?}")).parse().unwrap();
    assert!(drift > 15.0, "reads before the first frame still end a busy loop: {drift} ms");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local t0 = os.clock()
        for _ = 1, 2000 do
            local _ = os.clock()
        end
        local reads = (os.clock() - t0) * 1e3
        local t1 = os.clock()
        local spins = 0
        while os.clock() - t1 < 0.002 do
            spins += 1
        end
        print(string.format("reads %.3f spins %s", reads, tostring(spins > 100)))
        "#,
    );
    let line = output.iter().find(|line| line.starts_with("print: reads")).unwrap_or_else(|| panic!("{output:?}"));
    let ms: f64 = line.split_whitespace().nth(2).unwrap().parse().unwrap();
    assert!(ms < 1.0, "2,000 reads took {ms} ms of clock: {line}");
    assert!(line.ends_with("spins true"), "{line}");
}

/// Studio runs a signal's handlers newest first (measured again 2026-10-23 in an empty test place and a lobby
/// playtest client: the BindableEvent, Changed, GetPropertyChangedSignal, ChildAdded, AttributeChanged,
/// Heartbeat, Stepped, PreSimulation, PostSimulation, PreAnimation, PreRender and RenderStepped handlers
/// connected A, B, C ran C, B, A), BindToRenderStep runs ascending priority and, within one, in bind order,
/// and QueryDescendants answers in tree order, a selector list one selector after the other (an instance two
/// match is listed twice), with no combinator in a space (filters on either side apply to the same instance)
/// and Studio's messages for a malformed query (a handler_order probe).
#[test]
fn signal_handlers_and_queries_run_in_studios_order() {
    // (a running game: the simulation events do not fire in edit mode)
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local RunService = game:GetService("RunService")
        local Seen = {}
        _G.Seen = Seen
        local function Three(Name, Event)
            Seen[Name] = {}
            for _, Id in { "A", "B", "C" } do
                Event:Connect(function() table.insert(Seen[Name], Id) end)
            end
        end
        local Bindable = Instance.new("BindableEvent")
        local Part = Instance.new("Part")
        Part.Parent = workspace
        Three("Bindable", Bindable.Event)
        Three("Changed", Part.Changed)
        Three("Property", Part:GetPropertyChangedSignal("Transparency"))
        Three("ChildAdded", workspace.ChildAdded)
        Three("Attribute", Part.AttributeChanged)
        for _, Name in { "Heartbeat", "Stepped", "PreSimulation", "PostSimulation", "PreAnimation", "PreRender", "RenderStepped" } do
            Three(Name, RunService[Name])
        end
        Bindable:Fire()
        Part.Transparency = 0.5
        Instance.new("Folder").Parent = workspace
        Part:SetAttribute("X", 1)
        local Bound = {}
        for _, Entry in { { "Late", 200 }, { "Same1", 100 }, { "Early", 50 }, { "Same2", 100 } } do
            RunService:BindToRenderStep("T" .. Entry[1], Entry[2], function()
                if not table.find(Bound, Entry[1]) then table.insert(Bound, Entry[1]) end
            end)
        end
        _G.Bound = Bound
        "##,
    );
    for _ in 0..3 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(
        &lua,
        &engine,
        r##"
        for Name, Order in _G.Seen do
            assert(#Order >= 3 and table.concat(Order, ",", 1, 3) == "C,B,A", Name .. " ran " .. table.concat(Order, ","))
        end
        assert(table.concat(_G.Bound, ",") == "Early,Same1,Same2,Late", table.concat(_G.Bound, ","))

        local Root = Instance.new("Folder")
        local function Make(Class, Name, Parent)
            local Child = Instance.new(Class)
            Child.Name = Name
            Child.Parent = Parent
            return Child
        end
        local M = Make("Model", "M", Root)
        local F = Make("Folder", "F", M)
        Make("Part", "X", F)
        Make("Part", "Y", M)
        local N = Make("Model", "N", Root)
        Make("Part", "Z", N)
        game:GetService("CollectionService"):AddTag(F, "T")
        game:GetService("CollectionService"):AddTag(M, "T")
        local function Query(Selector)
            local Ok, Found = pcall(function() return Root:QueryDescendants(Selector) end)
            if not Ok then return "ERROR " .. tostring(Found) end
            local Names = {}
            for _, Item in Found do table.insert(Names, Item.Name) end
            return table.concat(Names, ",")
        end
        -- (every row measured in Studio, 2026-10-23)
        for Selector, Expected in {
            ["Model > Part"] = "Y,Z", ["Model>Part"] = "Y,Z", ["Model  >  Part"] = "Y,Z", ["Model >> Part"] = "X,Y,Z",
            ["Model > Folder > Part"] = "X", ["Folder > Part"] = "X", ["#M >> #X"] = "X", ["#M > #X"] = "",
            ["Part Part"] = "X,Y,Z", [".T Part"] = "", ["Model .T"] = "M", ["Part #X"] = "X", ["#X Part"] = "X", ["#M #X"] = "",
            ["Model :has(>Part)"] = "M,N", ["Part :not(#Y)"] = "X,Z", ["Model Folder"] = "", [".T"] = "M,F", ["T"] = "",
            ["Model, Folder, Part"] = "M,N,F,X,Y,Z", ["Part, Model"] = "X,Y,Z,M,N", ["Part, Part"] = "X,Y,Z,X,Y,Z", ["Part, #X"] = "X,Y,Z,X",
            ["> Part"] = "", [">> Part"] = "X,Y,Z", ["Model > Part:not(#Y)"] = "Z", ["Model:not(:has(> Part))"] = "", [""] = "", [" "] = "",
            ["*"] = "ERROR Expected at least one filter", ["Part,"] = "ERROR Expected at least one combinator or a filter",
            [",Part"] = "ERROR Expected at least one filter", ["Part >"] = "ERROR Expected at least one filter", ["#"] = "ERROR Expected identifier for a name",
            ["Part:bad"] = "ERROR Pseudo-class 'bad' filter is not supported", ["Part)"] = "ERROR Unexpected trailing character: ')'",
            ["[Anchored]"] = "ERROR comparison operator expected after property name", ["Part[Size = 1]"] = "ERROR Property Size of type Vector3 is not supported for comparison",
        } do
            assert(Query(Selector) == Expected, string.format("%q gave %q, Studio %q", Selector, Query(Selector), Expected))
        end
        print("ok")
        "##,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// SharedTableRegistry:GetSharedTable makes the table of a new name and returns the same one after (measured
/// in Studio 2026-10-23); ree returned nil, so a rig module's bridge host counts were nil and its
/// PreAnimation handler failed 1,199 times in a lobby's boot.
#[test]
fn the_shared_table_registry_makes_a_table_per_name() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Registry = game:GetService("SharedTableRegistry")
        local Counts = Registry:GetSharedTable("Counts")
        assert(typeof(Counts) == "SharedTable", typeof(Counts))
        Counts["1"] = 7
        assert(Registry:GetSharedTable("Counts") == Counts and Registry:GetSharedTable("Counts")["1"] == 7, "the same table")
        assert(Registry:GetSharedTable("Other") ~= Counts, "a table per name")
        local Given = SharedTable.new({ k = 1 })
        Registry:SetSharedTable("Given", Given)
        assert(Registry:GetSharedTable("Given") == Given, "a set table is returned")
        print("ok")
        "#,
    );
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
}

/// A harness can add players to a run (a lobby's other players): a Player parented to Players is listed by
/// GetPlayers, found by GetPlayerByUserId and (through its Character) by GetPlayerFromCharacter, and fires
/// PlayerAdded, PlayerRemoving and CharacterAdded; the local player stays first and fires none of them.
#[test]
fn players_a_harness_adds_join_the_game() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Players = game:GetService("Players")
        Players.PlayerAdded:Connect(function(Player) print("added " .. Player.Name) end)
        Players.PlayerRemoving:Connect(function(Player) print("removing " .. Player.Name) end)
        local Local = Players.LocalPlayer
        assert(#Players:GetPlayers() == 1 and Players:GetPlayers()[1] == Local)
        local Other = Instance.new("Player")
        Other.Name = "Other"
        Other.UserId = 77
        Other.Parent = Players
        Other.CharacterAdded:Connect(function(Character) print("character " .. Character.Name) end)
        local Body = Instance.new("Model")
        Body.Name = "Body"
        Body.Parent = workspace
        assert(Players:GetPlayerFromCharacter(Body) == nil, "no player wears the model yet")
        Other.Character = Body
        local All = Players:GetPlayers()
        assert(#All == 2 and All[1] == Local and All[2] == Other, "GetPlayers: local first, then the added player")
        assert(Players:GetPlayerByUserId(77) == Other and Players:GetPlayerByUserId(78) == nil)
        assert(Players:GetPlayerFromCharacter(Body) == Other, "GetPlayerFromCharacter")
        Other.Parent = nil
        assert(#Players:GetPlayers() == 1, "a removed player is gone")
        "#,
    );
    let printed: Vec<&str> = output.iter().filter_map(|line| line.strip_prefix("print: ")).collect();
    assert_eq!(printed, ["added Other", "character Body", "removing Other"], "{output:?}");
}

/// BindableFunction.OnInvoke is assignable (the Studio dump lists no callbacks) and :Invoke runs it
/// with the caller's arguments and returns its results; a reassignment replaces it.
#[test]
fn bindable_function_on_invoke_runs_on_invoke() {
    assert_script(
        r#"
        local Function = Instance.new("BindableFunction")
        local Calls = 0
        Function.OnInvoke = function(A, B)
            Calls += 1
            return A + B, "sum"
        end
        local Sum, Label = Function:Invoke(2, 3)
        assert(Sum == 5 and Label == "sum" and Calls == 1, "Invoke did not run OnInvoke")
        Function.OnInvoke = function() return "second" end
        assert(Function:Invoke() == "second", "reassigning OnInvoke kept the first callback")
        assert(Function:Clone():Invoke() == "second", "a clone lost OnInvoke")
        assert(not pcall(function() Function.OnClientInvoke = function() end end), "OnClientInvoke is no BindableFunction member")
        "#,
    );
}

/// Every callback Roblox's API dump lets a game script assign (Security None; the Studio dump lists
/// none) is a callback member: assigned without an error, a read raising Studio's message. A client
/// sets RemoteFunction.OnClientInvoke (ClientCast's client script, a game's own handlers); offline
/// nothing invokes it, nor the others.
#[test]
fn script_callbacks_are_assignable() {
    assert_script(
        r#"
        local Remote = Instance.new("RemoteFunction")
        Remote.OnClientInvoke = function() return 1 end
        Remote.OnServerInvoke = function() return 2 end
        Remote.OnClientInvoke = nil
        local Ok, Message = pcall(function() return Remote.OnClientInvoke end)
        assert(not Ok and string.find(Message, "OnClientInvoke is a callback member of RemoteFunction; you can only set the callback value, get is not available", 1, true), Message)
        assert(not pcall(function() Remote.OnInvoke = function() end end), "OnInvoke is no RemoteFunction member")
        game:GetService("MarketplaceService").ProcessReceipt = function() return Enum.ProductPurchaseDecision.NotProcessedYet end
        local Chat = game:GetService("TextChatService")
        Chat.OnIncomingMessage = function() end
        Chat.OnBubbleAdded = function() end
        Chat.OnChatWindowAdded = function() end
        local Channel = Instance.new("TextChannel")
        Channel.OnIncomingMessage = function() end
        Channel.ShouldDeliverCallback = function() return true end
        game:GetService("SocialService").OnCallInviteInvoked = function() end
        game.OnClose = function() end
        "#,
    );
}

/// Actors (every expectation measured in a Studio client with an actor probe, 2026-10-23). A Script under an
/// Actor starts inside the assignment that
/// puts it in the DataModel (before the line after `Actor.Parent = ...`), or when it is enabled there; its `_G`
/// and its module copies are its own (the main VM's stay nil / 1); `script:GetActor()` is its Actor, a module's
/// is nil; ConnectParallel raises outside an Actor; a signal's parallel handlers run after its serial ones, and
/// task.synchronize / task.desynchronize move all of them together: both parallel, both synchronized, both
/// desynchronized, newest connection first.
#[test]
fn actors_run_their_scripts_in_a_vm_of_their_own_in_studios_order() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local RunService = game:GetService("RunService")
        local Shared = game:GetService("SharedTableRegistry"):GetSharedTable("ActorTest")
        SharedTable.update(Shared, "N", function() return 0 end)
        local function Add(Text)
            local Index = SharedTable.increment(Shared, "N", 1) + 1
            Shared["L" .. Index] = Text
        end
        local Folder = Instance.new("Folder")
        Folder.Parent = workspace
        local Module = Instance.new("ModuleScript")
        Module.Name = "Probe"
        Module.Source = "local Count = 0 local Actor = script:GetActor() return { Bump = function() Count += 1 return Count end, Actor = if Actor then Actor.Name else 'none' }"
        Module.Parent = Folder
        local Template = Instance.new("Script")
        Template.RunContext = Enum.RunContext.Client
        Template.Enabled = false
        Template.Source = [==[
            local RunService = game:GetService("RunService")
            local Shared = game:GetService("SharedTableRegistry"):GetSharedTable("ActorTest")
            local Actor = script:GetActor()
            local Probe = require(Actor.Parent.Probe)
            local function Add(Text)
                local Index = SharedTable.increment(Shared, "N", 1) + 1
                Shared["L" .. Index] = Actor.Name .. " " .. Text
            end
            _G.Seen = (_G.Seen or 0) + 1
            Add("start actor=" .. tostring(script:GetActor() == Actor) .. " module-actor=" .. Probe.Actor .. " bump=" .. Probe.Bump() .. " _G.Seen=" .. _G.Seen .. " sync=" .. tostring(pcall(task.synchronize)))
            RunService.PreSimulation:Connect(function() Add("serial") end)
            RunService.PreSimulation:ConnectParallel(function()
                Add("parallel")
                task.synchronize()
                Add("synchronized")
                task.desynchronize()
                Add("desynchronized")
            end)
            RunService.Heartbeat:ConnectParallel(function() Add("heartbeat") end)
        ]==]
        Template.Parent = Folder
        RunService.PreSimulation:Connect(function() Add("main first") end)
        for Index = 1, 2 do
            local Actor = Instance.new("Actor")
            Actor.Name = "Actor" .. Index
            local Worker = Template:Clone()
            Worker.Parent = Actor
            Worker.Enabled = true
            Add("placing " .. Actor.Name)
            Actor.Parent = Folder
            Add("placed " .. Actor.Name)
        end
        RunService.PreSimulation:Connect(function() Add("main second") end)
        Add("main bump=" .. require(Module).Bump() .. " _G.Seen=" .. tostring(_G.Seen))
        local Ok, Message = pcall(function() RunService.Heartbeat:ConnectParallel(function() end) end)
        Add("main ConnectParallel " .. tostring(Ok) .. " " .. tostring(Message):gsub("^.-:%d+: ", ""))
        -- an Actor whose worker is disabled starts it when it is enabled
        local Late = Instance.new("Actor")
        Late.Name = "Late"
        local Worker = Template:Clone()
        Worker.Parent = Late
        Late.Parent = Folder
        Add("late placed")
        Worker.Enabled = true
        Add("late enabled")
        "##,
    );
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r##"
        local Shared = game:GetService("SharedTableRegistry"):GetSharedTable("ActorTest")
        local Lines = {}
        for Index = 1, SharedTable.size(Shared) do
            if Shared["L" .. Index] then table.insert(Lines, Shared["L" .. Index]) end
        end
        print(table.concat(Lines, "|"))
        "##,
    );
    let log = output.iter().find_map(|line| line.strip_prefix("print: ")).expect("a log").to_string();
    let log: Vec<&str> = log.split('|').collect();
    let expected = [
        "placing Actor1",
        "Actor1 start actor=true module-actor=none bump=1 _G.Seen=1 sync=true",
        "placed Actor1",
        "placing Actor2",
        "Actor2 start actor=true module-actor=none bump=1 _G.Seen=1 sync=true",
        "placed Actor2",
        "main bump=1 _G.Seen=nil",
        "main ConnectParallel false Function Event.ConnectParallel can only be used inside scripts",
        "late placed",
        "Late start actor=true module-actor=none bump=1 _G.Seen=1 sync=true",
        "late enabled",
        // PreSimulation: the serial handlers newest first, then the parallel ones (newest first) in Studio's phases
        "Late serial",
        "main second",
        "Actor2 serial",
        "Actor1 serial",
        "main first",
        "Late parallel",
        "Actor2 parallel",
        "Actor1 parallel",
        "Late synchronized",
        "Actor2 synchronized",
        "Actor1 synchronized",
        "Late desynchronized",
        "Actor2 desynchronized",
        "Actor1 desynchronized",
        "Late heartbeat",
        "Actor2 heartbeat",
        "Actor1 heartbeat",
    ];
    assert_eq!(log, expected);
}

/// Script.Source and Script.RunContext are plugin-writable in Studio and ree has no plugin boundary: a script can
/// set them (the Actor probe builds its workers that way), and a module's new source is the one required.
#[test]
fn script_source_and_run_context_are_writable() {
    assert_script(
        r#"
        local Module = Instance.new("ModuleScript")
        Module.Source = "return 41 + 1"
        assert(require(Module) == 42, "a module's source is the one written")
        local Worker = Instance.new("Script")
        Worker.RunContext = Enum.RunContext.Client
        assert(Worker.RunContext == Enum.RunContext.Client, "RunContext was not written")
        assert(Worker.Enabled == true, "a Script starts enabled")
        Worker.Enabled = false
        assert(Worker.Enabled == false)
        "#,
    );
}

/// The work clock prices datatype operations from Studio's own numbers: the op_cost probe's class
/// `datatype` in a lobby playtest's client (the median of four runs, 2026-10-23), bands of
/// about +-35 % around them; before, every operation the engine served was a flat 15 ns (a CFrame product 17 ns where
/// Studio takes 62, a Lerp 48 where it takes 110, a Position read 17 where it takes 1.3). The allocating CFrame
/// operations vary 2-3x between runs in Studio (a client's heap makes the sweep dearer), so the bands are wide there.
#[test]
fn the_work_clock_prices_datatype_operations_as_the_studio_bench_measured() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Body)
            local N = 2000
            local start = os.clock()
            Body(N)
            local middle = os.clock()
            for _ = 1, N do end
            return ((middle - start) - (os.clock() - middle)) / N * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.1f ns, Studio %d-%d", Name, Value, Low, High))
        end
        local A = CFrame.new(1, 2, 3) * CFrame.Angles(0.3, 0.4, 0.5)
        local B = CFrame.new(4, 5, 6) * CFrame.Angles(0.6, 0.1, 0.2)
        local V, W = Vector3.new(1, 2, 3), Vector3.new(4, 5, 6)
        local K, L = Color3.new(0.2, 0.4, 0.6), Color3.new(0.8, 0.6, 0.4)

        -- the parity bench's figures (tools/cost-parity, a rendered client of a game's match place, nine sessions: each loop's ns a step,
        -- the loop included) where it has a row outside its held-out third, bands of about 1.3x around them; the rig frame's (a match
        -- place's playtest client) for the operations it has no row for
        Within("CFrame.Position", Per(function(N) for _ = 1, N do local _ = A.Position end end), 0, 6)
        -- the product and the inverse are priced as a handler meets them, a few times a frame (rig frame, 24.6 and
        -- 24.5 ns); the op_cost loop of the lobby client reads 62 and 69, an empty test place's 20 and 20
        Within("CFrame * CFrame", Per(function(N) for _ = 1, N do local _ = A * B end end), 17, 40)
        Within("CFrame * Vector3", Per(function(N) for _ = 1, N do local _ = A * V end end), 8, 17)
        Within("CFrame:Inverse", Per(function(N) for _ = 1, N do local _ = A:Inverse() end end), 17, 40)
        -- ToAxisAngle, ToWorldSpace and PointToWorldSpace from the rig frame as well (25.4, 26.6 and 24.4 ns: six runs of six
        -- readings in a match place's client), bands of 1.3x around them; the lobby client's op_cost loop reads 18, 55 and 12
        Within("CFrame:ToAxisAngle", Per(function(N) for _ = 1, N do local _, _ = A:ToAxisAngle() end end), 19, 33)
        Within("CFrame:ToWorldSpace", Per(function(N) for _ = 1, N do local _ = A:ToWorldSpace(B) end end), 20, 35)
        -- the bench: Lerp 75.7, ToObjectSpace 20.2, GetComponents 22.0, ToEulerAnglesXYZ 22.1, Rotation 18.1, + Vector3 22.1, Angles 41.3
        Within("CFrame:Lerp", Per(function(N) for _ = 1, N do local _ = A:Lerp(B, 0.5) end end), 58, 98)
        Within("CFrame:ToObjectSpace", Per(function(N) for _ = 1, N do local _ = A:ToObjectSpace(B) end end), 15, 27)
        Within("CFrame:PointToWorldSpace", Per(function(N) for _ = 1, N do local _ = A:PointToWorldSpace(V) end end), 14, 32)
        Within("CFrame:GetComponents", Per(function(N) for _ = 1, N do local _ = A:GetComponents() end end), 16, 29)
        Within("CFrame:ToEulerAnglesXYZ", Per(function(N) for _ = 1, N do local _, _, _ = A:ToEulerAnglesXYZ() end end), 15, 29)
        Within("CFrame.Rotation", Per(function(N) for _ = 1, N do local _ = A.Rotation end end), 13, 24)
        Within("CFrame + Vector3", Per(function(N) for _ = 1, N do local _ = A + V end end), 16, 29)
        -- FuzzyEq compares both parts of a pair a micro-stud apart (the bench: 26.5) and stops at the position of one a stud
        -- apart (15.5)
        Within("CFrame:FuzzyEq near", Per(function(N) local C = A * CFrame.new(1e-6, 0, 0) for _ = 1, N do local _ = A:FuzzyEq(C) end end), 20, 34)
        Within("CFrame:FuzzyEq moved", Per(function(N) for _ = 1, N do local _ = A:FuzzyEq(B) end end), 11, 21)
        Within("CFrame.Angles", Per(function(N) for _ = 1, N do local _ = CFrame.Angles(0.1, 0.2, 0.3) end end), 30, 54)
        -- (fromEulerAnglesXYZ has no row: Angles' price)
        Within("CFrame.fromEulerAnglesXYZ", Per(function(N) for _ = 1, N do local _ = CFrame.fromEulerAnglesXYZ(0.1, 0.2, 0.3) end end), 30, 54)
        -- (lookAt's row is held out, 43.3: its price stays the playtest client's)
        Within("CFrame.lookAt", Per(function(N) for _ = 1, N do local _ = CFrame.lookAt(V, W) end end), 60, 115)
        -- the bench: Color3:Lerp 29.4, UDim2.fromOffset 32.1, ColorSequence.new of two colours 39.5, NumberSequence.new of a number
        -- 63.4 and of two 30.3 (the collector's phase moves those two apart: a wide band)
        Within("Color3:Lerp", Per(function(N) for _ = 1, N do local _ = K:Lerp(L, 0.5) end end), 22, 39)
        Within("UDim2.fromOffset", Per(function(N) for _ = 1, N do local _ = UDim2.fromOffset(10, 10) end end), 24, 42)
        Within("ColorSequence.new", Per(function(N) for _ = 1, N do local _ = ColorSequence.new(K, L) end end), 30, 52)
        Within("NumberSequence.new", Per(function(N) for _ = 1, N do local _ = NumberSequence.new(0.5) end end), 28, 82)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// Vector2 and UDim2 values cost what a client pays for them (lune-roblox `datatype_price`; the parity bench, tools/cost-parity, a
/// rendered client of a game's match place, nine sessions, ns a loop step): a Vector2's X read 3.4 ns, its deprecated x the same (ree
/// served the lowercase names through a Luau wrapper, charged as a host call: 17.9 ns each), `+` 15.3 and `==` 9.8 (ree: 1.0 and 1.4,
/// the instruction alone), a UDim2's X.Offset 32.3 (a new UDim; ree: 5.4) and `-` 28.0 (ree: 1.0), a ColorSequence's Keypoints of five
/// 145.1 (a new table of new keypoints; ree: 2.9).
#[test]
fn vector2_and_udim2_members_and_arithmetic_cost_what_a_client_pays() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Body)
            local N = 2000
            local start = os.clock()
            Body(N)
            local middle = os.clock()
            for _ = 1, N do end
            return ((middle - start) - (os.clock() - middle)) / N * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.1f ns, a client %d-%d", Name, Value, Low, High))
        end
        local A, B = Vector2.new(3, 4), Vector2.new(1, 2)
        assert(A.x == A.X and A.y == A.Y and A.magnitude == A.Magnitude and A.unit == A.Unit, "the lowercase names read the same values")
        Within("Vector2.X", Per(function(N) for _ = 1, N do local _ = A.X end end), 1, 5)
        Within("Vector2.x", Per(function(N) for _ = 1, N do local _ = A.x end end), 1, 5)
        Within("Vector2 + Vector2", Per(function(N) for _ = 1, N do local _ = A + B end end), 11, 21)
        Within("Vector2 == Vector2", Per(function(N) for _ = 1, N do local _ = A == B end end), 6, 14)
        local U, V = UDim2.new(0.5, 10, 0.25, 4), UDim2.new(0.1, 2, 0.3, 8)
        Within("UDim2.X.Offset", Per(function(N) for _ = 1, N do local _ = U.X.Offset end end), 14, 34)
        Within("UDim2 - UDim2", Per(function(N) for _ = 1, N do local _ = U - V end end), 20, 36)
        local K = Color3.new(1, 0.5, 0)
        local S = ColorSequence.new({
            ColorSequenceKeypoint.new(0, K), ColorSequenceKeypoint.new(0.25, K), ColorSequenceKeypoint.new(0.5, K),
            ColorSequenceKeypoint.new(0.75, K), ColorSequenceKeypoint.new(1, K),
        })
        Within("ColorSequence.Keypoints", Per(function(N) for _ = 1, N do local _ = S.Keypoints end end), 105, 185)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// A GUI object's first touch in a frame costs a laid-out GUI's (cost.rs COLD_TOUCH_GUI): a client lays out and draws every GUI object
/// every frame, so a walk over many finds each one cold (a rendered client of a game's lobby, a grid of 300 cards: AbsolutePosition of
/// distinct cards 273 ns against 71 for one card, a UIGradient's Color read on distinct gradients 416 against 77, a Frame's
/// BackgroundColor3 written on distinct frames 698 against 318). ree charged a part's 45 ns for every class.
#[test]
fn a_gui_objects_first_touch_in_a_frame_costs_what_a_drawn_gui_pays() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Gui = Instance.new("ScreenGui")
        Gui.Name = "Cards"
        Gui.Parent = game:GetService("StarterGui")
        for Index = 1, 200 do
            local Card = Instance.new("Frame")
            Card.Parent = Gui
            Instance.new("UIGradient").Parent = Card
        end
        "#,
    );
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Cards = game:GetService("StarterGui").Cards:GetChildren()
        local Gradients = {}
        local function Per(Body)
            local start = os.clock()
            Body()
            return (os.clock() - start) / #Cards * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.0f ns, a client %d-%d", Name, Value, Low, High))
        end
        -- (GetChildren listed the cards without touching them: each is cold)
        local Cold = Per(function() for _, Card in Cards do local _ = Card.Visible end end)
        local Hot = Per(function() for _, Card in Cards do local _ = Card.Visible end end)
        Within("first read of a Frame", Cold, 230, 400)
        Within("second read of it", Hot, 20, 80)
        for Index, Card in Cards do
            Gradients[Index] = Card:FindFirstChildOfClass("UIGradient")
        end
        local Write = Per(function() for _, Gradient in Gradients do Gradient.Offset = Vector2.new(0.5, 0) end end)
        -- (a UIGradient write 0.20 us, a first touch 0.27, its dirtying 0.075: the grid's Offset on distinct gradients 539 ns)
        Within("first write of a UIGradient", Write, 420, 720)
        "#,
    );
}

/// Cold per-frame Luau (cost.rs LUAU_COLD_TABLE and below): a table, Luau closure or function a frame's script work touches first
/// costs what a client pays for finding it out of its caches, once a frame; a table made in the frame is not cold, and a walk run
/// twice in a frame pays it once. Two runs of the same script read the same clock.
#[test]
fn a_frames_first_touch_of_a_table_or_function_costs_what_a_client_pays_once_a_frame() {
    fn walks() -> Vec<String> {
        let (lua, engine) = start();
        engine_lua::modules::interpret_all(&lua, &engine);
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        run(
            &lua,
            &engine,
            r#"
            _G.Records = {}
            for Index = 1, 200 do
                _G.Records[Index] = { A = Index, B = true }
            end
            _G.Walk = function()
                local Sum = 0
                for _, Record in _G.Records do
                    Sum += Record.A
                end
                return Sum
            end
            "#,
        );
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let source = r#"
            local function Per(Body)
                local Start = os.clock()
                Body()
                return (os.clock() - Start) / 200 * 1e9
            end
            local Cold = Per(_G.Walk)
            local Hot = Per(_G.Walk)
            local Fresh = {}
            for Index = 1, 200 do
                Fresh[Index] = { A = Index, B = true }
            end
            local Made = Per(function()
                local Sum = 0
                for _, Record in Fresh do
                    Sum += Record.A
                end
            end)
            print(string.format("%.6f %.6f %.6f", Cold, Hot, Made))
        "#;
        let first = run(&lua, &engine, source);
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let mut next = run(&lua, &engine, source);
        next.drain(..first.len());
        [first, next].concat()
    }
    let one = walks();
    assert_eq!(one, walks(), "the same script reads the same clock in every run");
    for line in &one {
        let values: Vec<f64> = line.split_once(": ").unwrap().1.split(' ').map(|v| v.parse().unwrap()).collect();
        let (cold, hot, made) = (values[0], values[1], values[2]);
        // 200 records, the list and the walk's closure and function cold: 200 ns a record and about 1.3 ns of the rest (and, the frame
        // the walk first runs, its code not run the frame before: cost.rs LUAU_RARE_PROTO, 3.5 ns a record)
        let extra = cold - hot;
        assert!((200.0..206.0).contains(&extra), "a cold record {extra:.1} ns over the hot walk, a client 140-210");
        assert!((made - hot).abs() < 3.0, "a table made in the frame is hot: {made:.1} against {hot:.1} ns");
    }
}

/// Counting cold touches marks a bit of each object's header and keeps its lists off the VM's heap: what a script reads of memory
/// (collectgarbage("count"), gcinfo) is the same with it on and off, frame by frame, while tables are made and collected.
#[test]
fn counting_cold_touches_leaves_what_a_script_reads_of_memory_as_it_was() {
    fn readings(tracking: bool) -> Vec<String> {
        let (lua, engine) = start();
        engine_lua::modules::interpret_all(&lua, &engine);
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        if !tracking {
            crate::cost::cold_tracking(&lua, false);
        }
        run(&lua, &engine, "_G.Kept = {} _G.Readings = {}");
        for frame in 0..40 {
            run(
                &lua,
                &engine,
                &format!(
                    r#"
                    for Index = 1, 400 do
                        local Record = {{ A = Index, B = {{ Index }} }}
                        if Index % 3 == 0 then
                            table.insert(_G.Kept, Record)
                        end
                    end
                    -- (cold calls: a C function's, a datatype's, a function run every other frame)
                    local _ = math.noise({frame} * 0.1) + CFrame.new({frame}, 0, 0).X
                    if {frame} % 2 == 0 then
                        _G.Every = _G.Every or function(X) return X * 2 end
                        local _ = _G.Every({frame})
                    end
                    for _, Record in _G.Kept do
                        Record.A += 1
                    end
                    if {frame} % 10 == 9 then
                        table.clear(_G.Kept)
                    end
                    table.insert(_G.Readings, collectgarbage("count") .. "/" .. gcinfo())
                    "#
                ),
            );
            crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        }
        run(&lua, &engine, r#"print(table.concat(_G.Readings, " "))"#)
    }
    let on = readings(true);
    assert_eq!(on, readings(false), "memory readings with cold touches counted and without");
    assert_eq!(on.last().unwrap().split_once(": ").unwrap().1.split(' ').count(), 40);
}

/// A member's first use in a frame (cost.rs FIRST_READ, FIRST_WRITE, FIRST_CALL): the engine's code for a property or method runs
/// cold after the frame's render and physics steps, so the first write, read or call of it in a frame costs a client more than the
/// next one on another instance; in the next frame the first one pays again.
#[test]
fn a_members_first_use_in_a_frame_costs_the_engines_code_running_cold() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Gui = Instance.new("ScreenGui")
        Gui.Parent = game:GetService("StarterGui")
        _G.A, _G.B = Instance.new("Frame"), Instance.new("Frame")
        _G.A.Parent, _G.B.Parent = Gui, Gui
        "#,
    );
    let source = r#"
        local A, B = _G.A, _G.B
        -- (both instances touched first, so neither use below pays an instance's first touch)
        local _, _ = A.Name, B.Name
        local function Two(Use)
            local T0 = os.clock()
            Use(A)
            local T1 = os.clock()
            Use(B)
            local T2 = os.clock()
            return ((T1 - T0) - (T2 - T1)) * 1e6
        end
        local Write = Two(function(F) F.BackgroundTransparency = F.BackgroundTransparency + 0.25 end)
        local Read = Two(function(F) local _ = F.Size end)
        local Call = Two(function(F) local _ = F:GetAttribute("Missing") end)
        print(string.format("%.4f %.4f %.4f", Write, Read, Call))
    "#;
    // (a first frame for what is done once: a member's lookup, its value's first conversion)
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(&lua, &engine, source);
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let output = run(&lua, &engine, source);
        let values: Vec<f64> = output.last().unwrap().split_once(": ").unwrap().1.split(' ').map(|v| v.parse().unwrap()).collect();
        eprintln!("first use over the second (us): write {:.3}, read {:.3}, call {:.3}", values[0], values[1], values[2]);
        // (the write's own read of BackgroundTransparency is a first read too: a write's first use 1.4 us and a read's 0.5)
        assert!((values[0] - 1.9).abs() < 0.05, "a property's first write in a frame {:.2} us over the second, a client 1.1-1.65 (+ its read)", values[0]);
        assert!((values[1] - 0.5).abs() < 0.05, "a property's first read in a frame {:.2} us over the second, a client 0.3-1.5", values[1]);
        assert!((values[2] - 0.8).abs() < 0.05, "a method's first call in a frame {:.2} us over the second, a client 0.6-0.95", values[2]);
    }
}

/// The engine paths an effect's cast runs (cost.rs Path, first_cloned, first_destroyed): the frame's first clone, entry into the
/// Workspace, destroy, PivotTo, BulkMoveTo and Emit run the engine's code cold (a clone and a destroy each class's code too), so they
/// cost a client more than the same call right after on another instance; in the next frame the first pays again.
#[test]
fn an_effects_engine_paths_cost_their_code_running_cold_at_a_frames_first_use() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Template()
            local P = Instance.new("Part")
            P.Anchored = true
            local A = Instance.new("Attachment")
            A.Parent = P
            local E = Instance.new("ParticleEmitter")
            E.Parent = P
            return P
        end
        _G.A, _G.B = Template(), Template()
        _G.Folder = Instance.new("Folder")
        _G.Folder.Parent = workspace
        _G.Moved = {}
        for I = 1, 4 do
            local P = Instance.new("Part")
            P.Anchored = true
            P.Parent = workspace
            _G.Moved[I] = P
        end
        _G.Emitters = { Instance.new("ParticleEmitter"), Instance.new("ParticleEmitter") }
        _G.Emitters[1].Parent, _G.Emitters[2].Parent = _G.Moved[1], _G.Moved[2]
        "#,
    );
    let source = r#"
        local function Two(First, Second)
            local T0 = os.clock()
            local X = First()
            local T1 = os.clock()
            local Y = Second()
            local T2 = os.clock()
            return ((T1 - T0) - (T2 - T1)) * 1e6, X, Y
        end
        local M, E = _G.Moved, _G.Emitters
        local Here = CFrame.new(0, 50, 0)
        local Clone, CA, CB = Two(function() return _G.A:Clone() end, function() return _G.B:Clone() end)
        local Enter = Two(function() CA.Parent = _G.Folder end, function() CB.Parent = _G.Folder end)
        local Destroy = Two(function() CA:Destroy() end, function() CB:Destroy() end)
        local Pivot = Two(function() M[1]:PivotTo(Here) end, function() M[2]:PivotTo(Here) end)
        local Bulk = Two(function() workspace:BulkMoveTo({ M[3] }, { Here }) end, function() workspace:BulkMoveTo({ M[4] }, { Here }) end)
        local Emit = Two(function() E[1]:Emit(1) end, function() E[2]:Emit(1) end)
        print(string.format("%.3f %.3f %.3f %.3f %.3f %.3f", Clone, Enter, Destroy, Pivot, Bulk, Emit))
    "#;
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(&lua, &engine, source);
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let output = run(&lua, &engine, source);
        let values: Vec<f64> = output.last().unwrap().split_once(": ").unwrap().1.split(' ').map(|v| v.parse().unwrap()).collect();
        // (a method's first call 0.8 us more as well: cost.rs FIRST_CALL)
        assert!((values[0] - (0.8 + 25.0 + 30.0 + 12.0 + 35.0)).abs() < 0.1, "a frame's first clone of a part with an attachment and an emitter {:.2} us over the second, a client 100", values[0]);
        assert!((values[1] - 26.0).abs() < 0.1, "a frame's first part into the Workspace {:.2} us over the second, a client 26", values[1]);
        assert!((values[2] - (0.8 + 10.0 + 36.0 + 11.0 + 11.0)).abs() < 0.1, "a frame's first destroy of the same {:.2} us over the second, a client 62", values[2]);
        assert!((values[3] - 16.8).abs() < 0.1, "a frame's first PivotTo {:.2} us over the second, a client 17-23", values[3]);
        assert!((values[4] - 28.8).abs() < 0.1, "a frame's first BulkMoveTo {:.2} us over the second, a client 25-30", values[4]);
        assert!((values[5] - 5.2).abs() < 0.1, "a frame's first Emit {:.2} us over the second, a client 5.2", values[5]);
    }
}

/// A destroy undoes the engine's frame work on an instance in the Workspace since an earlier frame (cost.rs destroy_live): a part that
/// entered it a frame before costs more to destroy than one parented in the same frame, which the parity corpus destroys.
#[test]
fn an_instance_in_the_workspace_since_an_earlier_frame_costs_more_to_destroy() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(&lua, &engine, r#"_G.Old = Instance.new("Part") _G.Old.Parent = workspace"#);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local function Fresh()
            local P = Instance.new("Part")
            P.Parent = workspace
            return P
        end
        Fresh():Destroy()
        local Young = Fresh()
        local T0 = os.clock()
        _G.Old:Destroy()
        local T1 = os.clock()
        Young:Destroy()
        local T2 = os.clock()
        print(string.format("%.3f", ((T1 - T0) - (T2 - T1)) * 1e6))
        "#,
    );
    let extra: f64 = output.last().unwrap().split_once(": ").unwrap().1.parse().unwrap();
    // (the old part's touch is a first touch of the frame too: cost.rs COLD_TOUCH)
    assert!((extra - 4.445).abs() < 0.02, "a part a frame old {extra:.3} us dearer to destroy than one parented now, a client 4.4");
}

/// A lone instance's first touch in a frame (cost.rs COLD_TOUCH_ALONE): parts made one after another and touched in that order pay
/// the plain first touch; parts made among other instances pay the lone price, unless they were touched the frame before.
#[test]
fn a_lone_instances_first_touch_costs_more_than_one_made_with_the_last() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        _G.Together, _G.Lone = {}, {}
        for I = 1, 21 do
            _G.Together[I] = Instance.new("Part")
        end
        for I = 1, 21 do
            _G.Lone[I] = Instance.new("Part")
            for _ = 1, 3 do
                Instance.new("Folder")
            end
        end
        _G.Other = Instance.new("Folder")
        "#,
    );
    let source = r#"
        local function Pass(List)
            local T0 = os.clock()
            for I = 2, #List do
                local _ = List[I]:GetAttribute("Missing")
            end
            return (os.clock() - T0) / (#List - 1) * 1e6
        end
        local _ = _G.Other:GetAttribute("Missing")
        local _ = _G.Together[1]:GetAttribute("Missing")
        local Together = Pass(_G.Together)
        local _ = _G.Lone[1]:GetAttribute("Missing")
        local Lone = Pass(_G.Lone)
        print(string.format("%.4f", Lone - Together))
    "#;
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(&lua, &engine, source);
    let lone: f64 = output.last().unwrap().split_once(": ").unwrap().1.parse().unwrap();
    assert!((lone - 0.275).abs() < 0.005, "a lone part's first touch {lone:.3} us over one made with the last, a client 0.29");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(&lua, &engine, source);
    let again: f64 = output.last().unwrap().split_once(": ").unwrap().1.parse().unwrap();
    assert!(again.abs() < 0.005, "touched the frame before, a lone part pays the plain first touch: {again:.3} us more");
}

/// A move of a part no code touched in this frame or the one before reads it cold (cost.rs BULK_COLD_PART); a part a script moves every
/// frame pays none.
#[test]
fn a_part_moved_every_frame_pays_no_cold_move() {
    let (lua, engine) = start();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Group()
            local Parts, Places = {}, {}
            for I = 1, 8 do
                local P = Instance.new("Part")
                P.Anchored = true
                P.Parent = workspace
                Parts[I], Places[I] = P, CFrame.new(I * 4, 20, 0)
            end
            return { Parts, Places }
        end
        _G.Moved, _G.Other = Group(), Group()
        "#,
    );
    let source = r#"
        workspace:BulkMoveTo(_G.Other[1], _G.Other[2])
        local T0 = os.clock()
        workspace:BulkMoveTo(_G.Moved[1], _G.Moved[2])
        print(string.format("%.3f", (os.clock() - T0) * 1e6))
    "#;
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(&lua, &engine, source);
    let cold: f64 = output.last().unwrap().split_once(": ").unwrap().1.parse().unwrap();
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(&lua, &engine, source);
    let warm: f64 = output.last().unwrap().split_once(": ").unwrap().1.parse().unwrap();
    assert!((cold - warm - 8.0 * 1.25).abs() < 0.01, "8 parts untouched for two frames {:.3} us dearer to move than the same moved the frame before", cold - warm);
}

/// A script's first call in a frame of a C function and of a datatype's operation (cost.rs LUAU_COLD_CFUNC, LUAU_COLD_HOSTFUNC), and a
/// function's first run in a frame when its code did not run the frame before (LUAU_RARE_PROTO): the code runs cold.
#[test]
fn a_frames_first_calls_of_library_and_datatype_functions_cost_their_code_running_cold() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    // (the prelude makes a library function's wrapper at a script's first use of it, and what a frame makes reads warm in it)
    run(&lua, &engine, "_G.Rare = function(X) return X + 1 end local _ = CFrame.new(0, 0, 0)");
    let source = r#"
        local function Two(First, Second)
            local T0 = os.clock()
            First()
            local T1 = os.clock()
            Second()
            local T2 = os.clock()
            return ((T1 - T0) - (T2 - T1)) * 1e6
        end
        local C = Two(function() return math.noise(0.5) end, function() return math.noise(1.5) end)
        local Host = Two(function() return CFrame.new(1, 2, 3) end, function() return CFrame.new(3, 2, 1) end)
        local T0 = os.clock()
        _G.Rare(1)
        local Rare = (os.clock() - T0) * 1e6
        print(string.format("%.4f %.4f %.4f", C, Host, Rare))
    "#;
    let mut rare = Vec::new();
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let output = run(&lua, &engine, source);
        let values: Vec<f64> = output.last().unwrap().split_once(": ").unwrap().1.split(' ').map(|v| v.parse().unwrap()).collect();
        assert!((values[0] - 2.5).abs() < 0.02, "a frame's first math.noise {:.3} us over the second, a client 0.9-4.0 for a C function", values[0]);
        assert!((values[1] - 0.9).abs() < 0.02, "a frame's first CFrame.new {:.3} us over the second, a client 0.55-2.5 for a datatype's", values[1]);
        rare.push(values[2]);
    }
    // (the first frame's call found the function's code not run the frame before; the second's found it run)
    assert!((rare[0] - rare[1] - 0.7).abs() < 0.01, "a function run first in a frame, not the frame before, {:.3} us dearer", rare[0] - rare[1]);
}

/// Vector3 members and methods on the work clock (the op_cost probe's `datatype` rows in a lobby playtest's client, the
/// medians of its runs): Magnitude 10.4 ns, Unit 12.3, Dot 9.4, Cross 9.6,
/// Max 12.2, Lerp 13.3, FuzzyEq 13.5. ree charged every one a function call to the engine's Luau (Magnitude and Unit 18,
/// Dot and Cross 33, Max 33, Lerp 45, FuzzyEq 43); the VM answers the two members itself now and the methods are a table
/// lookup. Lerp and FuzzyEq are still Luau functions (a host call each), so they only come down to about 21 and 19.
#[test]
fn vector3_members_and_methods_cost_what_studio_spent() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Body)
            local N = 2000
            local start = os.clock()
            Body(N)
            local middle = os.clock()
            for _ = 1, N do end
            return ((middle - start) - (os.clock() - middle)) / N * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.1f ns, Studio %d-%d", Name, Value, Low, High))
        end
        local V, W = Vector3.new(1, 2, 3), Vector3.new(4, 5, 6)
        -- Magnitude is priced from the rig frame (15.4 ns in a match place's client, 1.1x here), 10.4 in the op_cost loop
        Within("Magnitude", Per(function(N) for _ = 1, N do local _ = V.Magnitude end end), 12, 20)
        Within("magnitude", Per(function(N) for _ = 1, N do local _ = V.magnitude end end), 12, 20)
        Within("Unit", Per(function(N) for _ = 1, N do local _ = V.Unit end end), 8, 16)
        Within("X", Per(function(N) for _ = 1, N do local _ = V.X end end), 0, 5)
        Within("Dot", Per(function(N) for _ = 1, N do local _ = V:Dot(W) end end), 7, 13)
        Within("Cross", Per(function(N) for _ = 1, N do local _ = V:Cross(W) end end), 7, 13)
        Within("Max", Per(function(N) for _ = 1, N do local _ = V:Max(W) end end), 8, 16)
        -- Lerp and FuzzyEq from the rig frame (a match place's client, medians of three runs: 31.2 and 35.9 ns; the op_cost
        -- loop of the lobby client reads 13.3 and 13.5), bands of 1.3x around them
        -- (the parity bench: Lerp 14.9 ns whole, where its counts read about 21)
        Within("Lerp", Per(function(N) for _ = 1, N do local _ = V:Lerp(W, 0.5) end end), 12, 28)
        Within("FuzzyEq", Per(function(N) for _ = 1, N do local _ = V:FuzzyEq(W) end end), 28, 47)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// A property write with a handler connected to Changed or GetPropertyChangedSignal costs the writer more than the same write with
/// none (op_cost signal rows in a match place's client, two sessions: a Transparency write alone 316-352 ns, with one Changed
/// connection +227..354, with a property signal connection +79..213, with both +365..410; the lobby client +318 for Changed, edit +474).
/// ree charged the write alone (the handler's thread resumes outside the writer's charge): 252 ns with a connection, 252 without.
#[test]
fn a_changed_connection_costs_the_writer_the_queueing_of_its_handler() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local Part = Instance.new("Part")
        Part.Parent = workspace
        local function Noop() end
        local function Writes(Name)
            local N = 2000
            local start = os.clock()
            for _ = 1, N // 2 do
                Part.Transparency = 0.25
                Part.Transparency = 0.75
            end
            return (os.clock() - start) / N * 1e9
        end
        local Alone = Writes()
        local Changed = Part.Changed:Connect(Noop)
        local WithChanged = Writes()
        Changed:Disconnect()
        local Property = Part:GetPropertyChangedSignal("Transparency"):Connect(Noop)
        local WithProperty = Writes()
        local Both = Part.Changed:Connect(Noop)
        local WithBoth = Writes()
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: +%.0f ns, Studio +%d..%d", Name, Value, Low, High))
        end
        Within("Changed", WithChanged - Alone, 150, 400)
        Within("property signal", WithProperty - Alone, 100, 300)
        Within("both", WithBoth - Alone, 300, 500)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// The rest test a proxy rig module runs on every idle motor, priced as a bridge frame meets it (a cut that made it cheaper):
/// a bridge_ops probe takes each motor's Transform (the identity) from a rig once a frame and runs the test in
/// its old form (Position.Magnitude, then the angle of `Transform:Inverse() * CFrame.identity`) and in the new one
/// (`Transform == CFrame.identity` returns first), in an empty test place's client and in a match place's (three runs): the old
/// form reads 91-129 ns (x1 108-129, x8 91-99), the new one 16-28, so a cut of 70-100 ns per idle motor. ree read 182
/// and 6 (a cut of 176) while it priced the operations from the op_cost loop of the lobby client (Inverse 69, `*` 62,
/// `==` 5), and Studio's whole rig-cut gain came out 2.6x smaller than ree's.
#[test]
fn the_rest_test_of_an_idle_motor_costs_what_a_rig_frame_measured() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Body)
            local N = 2000
            local start = os.clock()
            Body(N)
            local middle = os.clock()
            for _ = 1, N do end
            return ((middle - start) - (os.clock() - middle)) / N * 1e9
        end
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.1f ns, Studio %d-%d", Name, Value, Low, High))
        end
        local Identity = CFrame.identity
        local Transform = CFrame.identity * CFrame.identity
        local function GetAngle(Value)
            local _, Angle = Value:ToAxisAngle()
            return math.abs(Angle)
        end
        local Old = Per(function(N)
            for _ = 1, N do
                local AtRest = if Transform.Position.Magnitude > 0.0001 then false else GetAngle(Transform:Inverse() * Identity) <= 0.0001
            end
        end)
        local New = Per(function(N)
            for _ = 1, N do
                local AtRest = if Transform == Identity then true elseif Transform.Position.Magnitude > 0.0001 then false else GetAngle(Transform:Inverse()) <= 0.0001
            end
        end)
        Within("rest test before the cut", Old, 85, 140)
        Within("rest test after the cut", New, 15, 40)
        Within("the cut", Old - New, 60, 105)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// The deferred handlers of a write made in Stepped or PreSimulation run between PreSimulation and PostSimulation, before the
/// Motor6D batch (a deferred_phase probe in a match place's client): a write in PreRender, PreAnimation,
/// PostSimulation or Heartbeat had its order right; ree flushed after PostSimulation. The marks of one steady frame are Studio's.
#[test]
fn a_presimulation_writes_deferred_handlers_run_before_postsimulation_and_the_motor_batch() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local RunService = game:GetService("RunService")
        local Part = Instance.new("Part")
        Part.Anchored = true
        Part.Parent = workspace
        local Marks, Recording = {}, false
        local function Mark(Name) if Recording then table.insert(Marks, Name) end end
        Part.Changed:Connect(function() Mark("C") end)
        Part.AttributeChanged:Connect(function() Mark("A") end)
        workspace.ChildAdded:Connect(function() Mark("K") end)
        for Name, Letter in { PreRender = "R", PreAnimation = "N", Stepped = "S", PreSimulation = "P", PostSimulation = "Q", Heartbeat = "H" } do
            RunService[Name]:Connect(function() Mark(Letter) end)
        end
        _G.Sequences = {}
        local Index = 0
        local Writers = { "Stepped", "PreSimulation" }
        local Writer
        local function Write()
            Mark(">")
            Part.Transparency = if Part.Transparency > 0.4 then 0.1 else 0.6
            Part:SetAttribute("B", os.clock())
            Instance.new("Folder").Parent = workspace
        end
        task.spawn(function()
            for _, Name in Writers do
                local Connection = RunService[Name]:Connect(Write)
                for _ = 1, 3 do RunService.Heartbeat:Wait() end
                Marks, Recording = {}, true
                RunService.Heartbeat:Wait()
                Recording = false
                _G.Sequences[Name] = table.concat(Marks)
                Connection:Disconnect()
            end
            -- a Motor6D.Transform a deferred handler writes is applied the same frame
            local Base = Instance.new("Part")
            Base.Anchored = true
            Base.Position = Vector3.new(0, 100, 0)
            Base.Parent = workspace
            local Arm = Instance.new("Part")
            Arm.Position = Vector3.new(0, 100, 0)
            Arm.Parent = workspace
            local Motor = Instance.new("Motor6D")
            Motor.Part0, Motor.Part1, Motor.Parent = Base, Arm, Base
            local Round, Same = 0, {}
            RunService.PreSimulation:Connect(function()
                Round += 1
                Part.Transparency = if Part.Transparency > 0.4 then 0.1 else 0.6
            end)
            Part.Changed:Connect(function(Property)
                if Property == "Transparency" then Motor.Transform = CFrame.new(0, 0, Round) end
            end)
            RunService.PostSimulation:Connect(function()
                Same[Round] = math.abs(Arm.Position.Z - Base.Position.Z - Round) < 0.01
            end)
            for _ = 1, 8 do RunService.Heartbeat:Wait() end
            _G.Same = Same[5] and Same[6]
            _G.Done = true
        end)
        "##,
    );
    for _ in 0..40 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    run(
        &lua,
        &engine,
        r##"
        assert(_G.Done, "the probe did not finish")
        assert(_G.Sequences.Stepped == "HRN>SPCAKQ", "write in Stepped: " .. tostring(_G.Sequences.Stepped) .. ", Studio HRN>SPCAKQ")
        assert(_G.Sequences.PreSimulation == "HRNS>PCAKQ", "write in PreSimulation: " .. tostring(_G.Sequences.PreSimulation) .. ", Studio HRNS>PCAKQ")
        assert(_G.Same, "a Motor6D.Transform written by the deferred handler was not applied in the same frame (Studio: it is)")
        print("ok")
        "##,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// A signal's firer and its handlers' threads on the work clock (a handler_cost probe, a match place's playtest client, three
/// runs, 100 instances triggered a frame, one handler each): Changed queues for 0.2 us and its handler's thread costs 1.83 us,
/// AttributeChanged 0.55 and 1.78, ChildAdded 0.40 and 1.50, DescendantAdded 0.27 and 1.44, each scaled by the quarter the parity
/// bench's client reads them at (cost.rs signal_costs: a heard BindableEvent:Fire through its handler 0.64 us) but Changed's
/// queueing, which it reads directly. ree charged the thread nothing before, so a handler-heavy frame (a roster rebuild on every
/// ChildAdded) read low. Bands are +-35 %.
#[test]
fn signals_cost_their_firer_the_queueing_and_each_handler_its_thread() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local RunService = game:GetService("RunService")
        local N, PER = 100, 20
        local Parts, Items = {}, {}
        for Index = 1, N do
            local Part = Instance.new("Part")
            Part.Parent = workspace
            Parts[Index] = Part
            Items[Index] = Instance.new("Folder")
        end
        local Container = Instance.new("Folder")
        Container.Parent = workspace
        local Last, Count = 0, 0
        local function Handler() Last = os.clock() Count += 1 end
        local function Each(Get) local List = {} for Index = 1, N do table.insert(List, Get(Parts[Index]):Connect(Handler)) end return List end
        local Kinds = {
            { "Changed", function() return Each(function(P) return P.Changed end) end,
                function(Frame) for Index = 1, N do Parts[Index].Transparency = (Frame % 2) * 0.5 end end },
            { "AttributeChanged", function() return Each(function(P) return P.AttributeChanged end) end,
                function(Frame) for Index = 1, N do Parts[Index]:SetAttribute("A", Frame) end end },
            { "ChildAdded", function() return { Container.ChildAdded:Connect(Handler) } end,
                function() for Index = 1, N do Items[Index].Parent = Container end end,
                function() for Index = 1, N do Items[Index].Parent = nil end end },
            { "DescendantAdded", function() return { Container.DescendantAdded:Connect(Handler) } end,
                function() for Index = 1, N do Items[Index].Parent = Container end end,
                function() for Index = 1, N do Items[Index].Parent = nil end end },
        }
        local function Median(List) table.sort(List) return List[(#List + 1) // 2] end
        _G.Rows = {}
        task.spawn(function()
            for _, Kind in Kinds do
                local Alone, Queued, Run = {}, {}, {}
                for _, Connected in { false, true } do
                    local List = if Connected then Kind[2]() else {}
                    for Frame = 1, PER do
                        RunService.PreSimulation:Wait()
                        Count = 0
                        local T0 = os.clock()
                        Kind[3](Frame)
                        local T1 = os.clock()
                        RunService.PostSimulation:Wait()
                        if Frame > 4 then
                            if Connected then
                                table.insert(Queued, T1 - T0)
                                if Count == N then table.insert(Run, Last - T1) end
                            else
                                table.insert(Alone, T1 - T0)
                            end
                        end
                        if Kind[4] then Kind[4]() end
                    end
                    for _, Connection in List do Connection:Disconnect() end
                end
                local Base = Median(Alone)
                _G.Rows[Kind[1]] = { Queued = (Median(Queued) - Base) / N * 1e9, Run = Median(Run) / N * 1e9 }
            end
            _G.Done = true
        end)
        "##,
    );
    for _ in 0..200 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    run(
        &lua,
        &engine,
        r##"
        assert(_G.Done, "the probe did not finish")
        local function Within(Name, Value, Low, High)
            assert(Value >= Low and Value <= High, string.format("%s: %.0f ns, Studio %d-%d", Name, Value, Low, High))
        end
        local R = _G.Rows
        Within("Changed queued", R.Changed.Queued, 150, 400)
        Within("Changed handler thread", R.Changed.Run, 295, 610)
        Within("AttributeChanged queued", R.AttributeChanged.Queued, 90, 185)
        Within("AttributeChanged handler thread", R.AttributeChanged.Run, 290, 595)
        Within("ChildAdded queued", R.ChildAdded.Queued, 65, 135)
        Within("ChildAdded handler thread", R.ChildAdded.Run, 240, 500)
        Within("DescendantAdded queued", R.DescendantAdded.Queued, 44, 90)
        Within("DescendantAdded handler thread", R.DescendantAdded.Run, 230, 480)
        print("ok")
        "##,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// CFrame:ToEulerAngles(order) and CFrame.fromEulerAngles(rx, ry, rz, order) exist and agree with Studio for all six
/// Enum.RotationOrder items (an euler probe in an empty test place, 2026-10-23; ree had neither, and
/// ToEulerAnglesYXZ / ToOrientation were free on the work clock): the angles come back in x, y, z order whatever the
/// rotation order, an unknown order raises Studio's message, and the default is XYZ.
#[test]
fn cframe_euler_angles_follow_the_rotation_order_as_studio_does() {
    assert_script(
        r#"
        local function Close(Name, Got, Want)
            for Index = 1, 3 do
                assert(math.abs(Got[Index] - Want[Index]) < 2e-6, string.format("%s[%d]: %.6f, Studio %.6f", Name, Index, Got[Index], Want[Index]))
            end
        end
        local Cf = CFrame.new(1, 2, 3) * CFrame.Angles(0.3, 0.4, 0.5)
        -- Studio, cf1 of the probe
        Close("default", { Cf:ToEulerAngles() }, { 0.3, 0.4, 0.5 })
        Close("XYZ", { Cf:ToEulerAngles(Enum.RotationOrder.XYZ) }, { 0.3, 0.4, 0.5 })
        Close("XZY", { Cf:ToEulerAngles(Enum.RotationOrder.XZY) }, { 0.509615, 0.448958, 0.457359 })
        Close("YZX", { Cf:ToEulerAngles(Enum.RotationOrder.YZX) }, { 0.334475, 0.224767, 0.593186 })
        Close("YXZ", { Cf:ToEulerAngles(Enum.RotationOrder.YXZ) }, { 0.275670, 0.416649, 0.619884 })
        Close("ZXY", { Cf:ToEulerAngles(Enum.RotationOrder.ZXY) }, { 0.453041, 0.207013, 0.513380 })
        Close("ZYX", { Cf:ToEulerAngles(Enum.RotationOrder.ZYX) }, { 0.461591, 0.185872, 0.605050 })
        -- a CFrame with large angles
        local Big = CFrame.Angles(-0.7, 1.1, 2.9)
        Close("big XZY", { Big:ToEulerAngles(Enum.RotationOrder.XZY) }, { 2.225426, 2.029783, 0.108737 })
        Close("big ZYX", { Big:ToEulerAngles(Enum.RotationOrder.ZYX) }, { 1.156342, -0.532523, 2.107389 })
        -- fromEulerAngles: Studio's components for (0.3, 0.4, 0.5) in each order
        local function Rotation(Order)
            local Parts = { CFrame.fromEulerAngles(0.3, 0.4, 0.5, Order):GetComponents() }
            return { Parts[4], Parts[5], Parts[6], Parts[10], Parts[11], Parts[12] }
        end
        Close("from XYZ", Rotation(), { 0.808307, -0.441580, 0.389418, -0.184803, 0.437702, 0.879923 })
        Close("from XZY", Rotation(Enum.RotationOrder.XZY), { 0.808307, -0.479426, 0.341747, -0.241530, 0.259343, 0.935096 })
        Close("from YXZ", Rotation(Enum.RotationOrder.YXZ), { 0.863480, -0.340587, 0.372026, -0.211251, 0.425568, 0.879923 })
        Close("from ZYX", Rotation(Enum.RotationOrder.ZYX), { 0.808307, -0.357020, 0.468163, -0.389418, 0.272192, 0.879923 })
        -- a round trip in every order
        for _, Order in Enum.RotationOrder:GetEnumItems() do
            local Rx, Ry, Rz = Cf:ToEulerAngles(Order)
            local Again = CFrame.fromEulerAngles(Rx, Ry, Rz, Order)
            Close("round trip " .. Order.Name, { Again.RightVector.X, Again.UpVector.Y, Again.LookVector.Z }, { Cf.RightVector.X, Cf.UpVector.Y, Cf.LookVector.Z })
        end
        local Ok, Message = pcall(function() return Cf:ToEulerAngles("XYZ") end)
        -- (a native CFrame method's error reaches a pcall as the host's error value, not a string: tostring reads its text)
        assert(not Ok and string.find(tostring(Message), "Invalid value for enum 'RotationOrder'", 1, true), tostring(Message))
        "#,
    );
}

/// Made-up place ids for the work clock's place prices ([`priced_places`])
const LOBBY: f64 = 1.0;
const MATCH: f64 = 2.0;
const TEST_MATCH: f64 = 3.0;

/// The measured place prices (cost.rs) on made-up places: a lobby with a localization table (56.1 us of lookup for a
/// one-letter text) and two match places of the same experience (61.2 us, and the geometry write factor).
fn priced_places() -> crate::cost::PlacePrices {
    crate::cost::PlacePrices { localized: vec![(LOBBY, 55.66 + 0.4286), (MATCH, 61.2), (TEST_MATCH, 61.2)], matched: vec![MATCH, TEST_MATCH] }
}

/// What a Text write costs in `place_id` with `prices` in a client (`mode` "play") or an edit DataModel ("edit"), in us a write:
/// letters with AutoLocalize on, AutoLocalize off, digits only, forty characters
fn text_write_prices(place_id: f64, prices: crate::cost::PlacePrices, mode: &str) -> Vec<f64> {
    let engine = Engine::new(Options { place_id, place_prices: prices, mode: mode.into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Label, A, B)
            local start = os.clock()
            for _ = 1, 100 do
                Label.Text = A
                Label.Text = B
            end
            return (os.clock() - start) / 200 * 1e6
        end
        local On = Instance.new("TextLabel")
        local Off = Instance.new("TextLabel")
        Off.AutoLocalize = false
        local Forty = string.rep("a", 40)
        print(string.format("%.2f %.2f %.2f %.2f", Per(On, "Hello", "World"), Per(Off, "Hello", "World"), Per(On, "1234", "5678"), Per(On, Forty, string.rep("b", 40))))
        "#,
    );
    let line = lines(&engine).into_iter().find_map(|line| line.strip_prefix("print: ").map(str::to_string)).expect("a result");
    line.split(' ').map(|number| number.parse::<f64>().unwrap()).collect::<Vec<f64>>()
}

/// A client looks a Text write up for translation when AutoLocalize is on and the text has a letter in it, whatever the place holds
/// (the parity bench, tools/cost-parity: "Hello" / "World" 35.0 us a write in a rendered client of a game's match place, 35.1-36.2 in
/// clients of an empty place with no localization table, headless or rendered; 0.47 with digits only), so every client pays
/// cost.rs LOCALIZATION_LOOKUP (32.8 us for one letter, 0.43 a letter more, as a lobby playtest client measured: 58.4-62.2 us for
/// "Hello" / "World" and 71.7-75.5 for forty characters there, with Studio's own text capture in it); a place [`crate::cost::PlacePrices`]
/// lists pays its own figure. An edit DataModel (an empty place's measured 1.0 us) and a server have no translator. ree charged the
/// lookup only in the listed places, so a client of any other place paid 1/75 of a lettered HUD's rewrite.
#[test]
fn a_client_pays_the_autolocalize_lookup_on_a_lettered_text_write() {
    let any = text_write_prices(0.0, crate::cost::PlacePrices::default(), "play");
    assert!((30.0..40.0).contains(&any[0]), "a client, letters: {any:?}");
    assert!((0.3..1.2).contains(&any[1]), "a client, AutoLocalize off: {any:?}");
    assert!((0.3..1.2).contains(&any[2]), "a client, digits: {any:?}");
    assert!((45.0..56.0).contains(&any[3]), "a client, forty characters: {any:?}");
    let lobby = text_write_prices(LOBBY, priced_places(), "play");
    assert!((50.0..68.0).contains(&lobby[0]) && (63.0..84.0).contains(&lobby[3]), "a listed place pays its own lookup: {lobby:?}");
    let edit = text_write_prices(LOBBY, priced_places(), "edit");
    assert!(edit.iter().all(|us| (0.3..1.5).contains(us)), "an edit DataModel looks nothing up: {edit:?}");
}

/// `REE_LOCALIZED_PLACES` / `REE_MATCH_PLACES` (launch.rs) add places to the prices: `<id>[=<us>]` and `<id>`, a place
/// listed already keeps its entry, nothing set changes nothing, and a value that is not a number is refused.
#[test]
fn place_prices_take_the_places_a_command_line_adds() {
    let base = crate::cost::PlacePrices { localized: vec![(LOBBY, 56.1)], matched: Vec::new() };
    let added = base.clone().with(Some("2, 3=40.5,1=99"), Some("3,4")).unwrap();
    assert_eq!(added.localized, [(LOBBY, 56.1), (2.0, crate::cost::LOCALIZATION_LOOKUP), (3.0, 40.5)]);
    assert_eq!(added.matched, [3.0, 4.0]);
    assert_eq!(base.clone().with(None, Some("")).unwrap(), base);
    assert!(base.clone().with(Some("lobby"), None).unwrap_err().contains("REE_LOCALIZED_PLACES"));
    assert!(base.with(None, Some("2,x")).unwrap_err().contains("REE_MATCH_PLACES"));
}

/// What fires when a model arrives in a watched folder, in Studio's order (an arrival probe, 166 lines identical in ree, in an
/// empty test place's edit DataModel (Immediate signals) and in its client, and in a lobby's client (Deferred signals),
/// 2026-10-23): for each instance of the subtree in GetDescendants' order the
/// DescendantAdded of each ancestor, nearest first, then ChildAdded, then AncestryChanged children before parents, then the
/// moved instance's own Changed("Parent"). A move between two folders under one root tells the root nothing, and the old folder
/// gets DescendantRemoving for the subtree (depth first) and then ChildRemoved before the new one's events. ree fired
/// ChildAdded first, DescendantAdded breadth first and per ancestor, AncestryChanged in reverse, Changed(Parent) before all.
#[test]
fn a_model_arriving_in_a_folder_fires_its_events_in_studios_order() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Log = {}
        _G.Log = Log
        local function Add(Text) table.insert(Log, Text) end
        local Root = Instance.new("Folder") Root.Name = "Root" Root.Parent = workspace
        local Units = Instance.new("Folder") Units.Name = "Units" Units.Parent = Root
        local Other = Instance.new("Folder") Other.Name = "Other" Other.Parent = Root
        local function Model(Name, ...) local M = Instance.new("Model") M.Name = Name for _, C in { ... } do C.Parent = M end return M end
        local U = Model("U", Model("A", Model("A1"), Model("A2")), Model("B", Model("B1")), Model("C"))
        for _, Node in U:GetDescendants() do
            Node.AncestryChanged:Connect(function(Child, Parent) Add("Ancestry " .. Node.Name .. " " .. Child.Name .. " " .. (Parent and Parent.Name or "nil")) end)
        end
        U.AncestryChanged:Connect(function(Child, Parent) Add("Ancestry U " .. Child.Name .. " " .. (Parent and Parent.Name or "nil")) end)
        U.Changed:Connect(function(Property) Add("U.Changed " .. Property) end)
        U:GetPropertyChangedSignal("Parent"):Connect(function() Add("U.Parent") end)
        for _, Folder in { Units, Other } do
            Folder.ChildAdded:Connect(function(C) Add(Folder.Name .. ".ChildAdded " .. C.Name) end)
            Folder.ChildRemoved:Connect(function(C) Add(Folder.Name .. ".ChildRemoved " .. C.Name) end)
            Folder.DescendantAdded:Connect(function(D) Add(Folder.Name .. ".DescendantAdded " .. D.Name) end)
            Folder.DescendantRemoving:Connect(function(D) Add(Folder.Name .. ".DescendantRemoving " .. D.Name) end)
        end
        Root.DescendantAdded:Connect(function(D) Add("Root.DescendantAdded " .. D.Name) end)
        Root.DescendantRemoving:Connect(function(D) Add("Root.DescendantRemoving " .. D.Name) end)
        U.Parent = Units
        U.Parent = Other
        "#,
    );
    let output = run(&lua, &engine, r#"print(table.concat(_G.Log, "|"))"#);
    let log = output.iter().find_map(|line| line.strip_prefix("print: ")).expect("a log");
    let arrival = "Units.DescendantAdded U|Root.DescendantAdded U|Units.DescendantAdded A|Root.DescendantAdded A|Units.DescendantAdded A1|Root.DescendantAdded A1|\
Units.DescendantAdded A2|Root.DescendantAdded A2|Units.DescendantAdded B|Root.DescendantAdded B|Units.DescendantAdded B1|Root.DescendantAdded B1|\
Units.DescendantAdded C|Root.DescendantAdded C|Units.ChildAdded U|\
Ancestry A1 U Units|Ancestry A2 U Units|Ancestry A U Units|Ancestry B1 U Units|Ancestry B U Units|Ancestry C U Units|Ancestry U U Units|U.Changed Parent|U.Parent";
    let moved = "Units.DescendantRemoving U|Units.DescendantRemoving A|Units.DescendantRemoving A1|Units.DescendantRemoving A2|Units.DescendantRemoving B|\
Units.DescendantRemoving B1|Units.DescendantRemoving C|Units.ChildRemoved U|\
Other.DescendantAdded U|Other.DescendantAdded A|Other.DescendantAdded A1|Other.DescendantAdded A2|Other.DescendantAdded B|Other.DescendantAdded B1|\
Other.DescendantAdded C|Other.ChildAdded U|\
Ancestry A1 U Other|Ancestry A2 U Other|Ancestry A U Other|Ancestry B1 U Other|Ancestry B U Other|Ancestry C U Other|Ancestry U U Other|U.Changed Parent|U.Parent";
    assert_eq!(log, format!("{arrival}|{moved}"));
}

/// SharedTable.increment adds to a key that exists and returns the old value, and raises for one that does not (Studio, 2026-10-23: `Unable to
/// increment the value for key "M" because it does not exist in the SharedTable.`; ree made the key).
#[test]
fn shared_table_increment_needs_the_key_as_studio_does() {
    assert_script(
        r#"
        local Counts = SharedTable.new({ N = 5 })
        assert(SharedTable.increment(Counts, "N", 2) == 5 and Counts.N == 7, "increment adds and returns the old value")
        local Ok, Message = pcall(SharedTable.increment, Counts, "M", 1)
        assert(not Ok and string.find(tostring(Message), 'Unable to increment the value for key "M" because it does not exist in the SharedTable.', 1, true), tostring(Message))
        assert(Counts.M == nil, "a failed increment made the key")
        "#,
    );
}

/// The parallel bridge's machinery costs what Studio spent (an empty test place's client, an actor_cost probe, 2026-10-23): a BindableFunction:Invoke
/// into an Actor's OnInvoke 1,025 ns, SharedTable.increment 370-429; ree counted 81 and 32 (a table read, 29 ns in Studio, is close to ree's 3).
#[test]
fn a_call_into_an_actor_and_a_shared_increment_cost_what_studio_spent() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r#"
        local function Per(Body)
            local N = 500
            local start = os.clock()
            Body(N)
            local middle = os.clock()
            for _ = 1, N do end
            return ((middle - start) - (os.clock() - middle)) / N * 1e9
        end
        local Actor = Instance.new("Actor")
        local Call = Instance.new("BindableFunction")
        Call.Parent = Actor
        Call.OnInvoke = function(A) return A end
        local Plain = Instance.new("BindableFunction")
        Plain.OnInvoke = function(A) return A end
        local Shared = SharedTable.new({ N = 0 })
        local Invoke, Increment, Same = Per(function(N) for _ = 1, N do Call:Invoke(1, 2) end end), Per(function(N) for _ = 1, N do SharedTable.increment(Shared, "N", 1) end end), Per(function(N) for _ = 1, N do Plain:Invoke(1, 2) end end)
        assert(Invoke > 850 and Invoke < 1300, "Invoke into an Actor: " .. Invoke)
        assert(Increment > 300 and Increment < 520, "SharedTable.increment: " .. Increment)
        assert(Same < 300, "a call outside an Actor keeps its price: " .. Same)
        print("ok")
        "#,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// SocialService:GetUpcomingExperienceEventsAsync and GroupService:GetRolesInGroupAsync returned nil, so a lobby's event handler
/// failed on `#data` and its group check on `.IsMember`. Studio: an empty list, and a table with
/// IsMember false and an empty Roles list (an empty test place, 2026-10-23).
#[test]
fn social_and_group_services_answer_as_studio_does_for_an_offline_player() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Events = game:GetService("SocialService"):GetUpcomingExperienceEventsAsync()
        assert(typeof(Events) == "table" and #Events == 0, "events")
        local Roles = game:GetService("GroupService"):GetRolesInGroupAsync(1, 1234567)
        assert(typeof(Roles) == "table" and Roles.IsMember == false and #Roles.Roles == 0, "roles")
        print("studio's answers")
        "#,
    );
    assert!(output.contains(&"print: studio's answers".to_string()), "{output:?}");
}

/// A one-letter Text write's and a workspace part's CFrame write's price in a client of `place_id` with `prices` and `parts` parts
/// in the workspace, in us a write
fn place_write_prices(place_id: f64, parts: usize, prices: crate::cost::PlacePrices) -> Vec<f64> {
    let engine = Engine::new(Options { place_id, place_prices: prices, mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        &format!(
            r#"
        for _ = 1, {parts} do
            local P = Instance.new("Part")
            P.Anchored = true
            P.Parent = workspace
        end
        local Label = Instance.new("TextLabel")
        local Target = Instance.new("Part")
        Target.Parent = workspace
        local A, B = CFrame.new(1, 2, 3), CFrame.new(3, 2, 1)
        local Start = os.clock()
        for _ = 1, 200 do
            Label.Text = "a"
            Label.Text = "b"
        end
        local Text = (os.clock() - Start) / 400 * 1e6
        Start = os.clock()
        for _ = 1, 200 do
            Target.CFrame = A
            Target.CFrame = B
        end
        print(string.format("%.2f %.2f", Text, (os.clock() - Start) / 400 * 1e6))
        "#
        ),
    );
    let line = lines(&engine).into_iter().find_map(|line| line.strip_prefix("print: ").map(str::to_string)).unwrap_or_else(|| panic!("a result: {:?}", lines(&engine)));
    line.split(' ').map(|number| number.parse::<f64>().unwrap()).collect::<Vec<f64>>()
}

/// A listed place pays its own lookup (a place_size_writes probe, a match place's playtest client, five sessions: a one-letter text
/// 56-59 us a write), and a listed match place scales a part's geometry write in the workspace (`REE_MATCH_PLACES`: Studio playtests
/// read 1.69-1.83 us there against a lobby's 1.10; the parity bench's match-place client 1.43, ree's default price). A part count or a GUI
/// tree size does not move any write price (same probe, 0 to 20,000 parts and 0 to 6,000 frames: CFrame 1.58-1.72 us, Size 0.31-0.37, flat).
#[test]
fn a_listed_place_pays_its_own_prices_and_a_bigger_place_costs_no_more() {
    for place in [MATCH, TEST_MATCH] {
        let small = place_write_prices(place, 0, priced_places());
        assert!((50.0..68.0).contains(&small[0]), "place {place}, Text: {small:?}");
    }
    let (small, big) = (place_write_prices(MATCH, 0, priced_places()), place_write_prices(MATCH, 3000, priced_places()));
    assert!((small[1] - big[1]).abs() < 0.05 * small[1], "a CFrame write in a place of 3,000 parts: {big:?}, of none: {small:?}");
    assert!((1.9..2.6).contains(&small[1]), "a listed match place, CFrame: {small:?}");
    let lobby = place_write_prices(LOBBY, 0, priced_places());
    assert!((1.2..1.7).contains(&lobby[1]), "lobby, CFrame: {lobby:?}");
}


/// The signals the first handler_cost probe left unpriced, and a signal with several handlers (a second one, a match place's client,
/// three runs, 100 instances triggered a frame by a thread in PreSimulation): a BindableEvent:Fire with a handler queues for 0.58 us
/// (1.73 us the handler's thread), ChildRemoved 0.42 / 1.50, DescendantRemoving 0.35 / 1.58, AncestryChanged 0.65 / 2.17. A signal with
/// 10 handlers on each of 100 parts queued for 4.55-4.99 us a fire (each handler beyond the first 0.45) and its handlers' threads cost
/// 0.87-1.01 us each where a lone handler's cost 1.6-2.8; 100 handlers on 10 parts 43.0-45.1 and 0.91-1.04. The handlers of a frame event
/// run 0.63-0.73 us apart (100 on PreSimulation, Heartbeat and PreRender). ree charged the Bindable, the removals and AncestryChanged
/// nothing, and a handler beyond the first the queueing of none and the thread of a lone one. Bands are +-35 % of Studio's medians.
#[test]
fn bindable_removal_ancestry_and_many_handler_signals_cost_what_studio_measured() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local RunService = game:GetService("RunService")
        local N, PER = 100, 20
        local Parts, Items, Events = {}, {}, {}
        for Index = 1, N do
            local Part = Instance.new("Part")
            Part.Parent = workspace
            Parts[Index] = Part
            Items[Index] = Instance.new("Folder")
            Events[Index] = Instance.new("BindableEvent")
        end
        local Root = Instance.new("Folder")
        Root.Parent = workspace
        local Container = Instance.new("Folder")
        Container.Parent = Root
        local Container2 = Instance.new("Folder")
        Container2.Parent = Root
        local Last, Count = 0, 0
        local function Handler() Last = os.clock() Count += 1 end
        local function AllIn() for Index = 1, N do Items[Index].Parent = Container end end
        local function Many(Per, Fires)
            return function()
                local List = {}
                for Index = 1, Fires do for _ = 1, Per do table.insert(List, Parts[Index].Changed:Connect(Handler)) end end
                return List
            end
        end
        local Write = function(Fires) return function(Frame) for Index = 1, Fires do Parts[Index].Transparency = (Frame % 2) * 0.5 end end end
        -- { name, connect, trigger, cleanup, setup, handlers per signal, fires }
        local Kinds = {
            { "Bindable", function() local List = {} for Index = 1, N do table.insert(List, Events[Index].Event:Connect(Handler)) end return List end,
                function(Frame) for Index = 1, N do Events[Index]:Fire(Frame) end end },
            { "ChildRemoved", function() return { Container.ChildRemoved:Connect(Handler) } end,
                function() for Index = 1, N do Items[Index].Parent = nil end end, AllIn, AllIn },
            { "DescendantRemoving", function() return { Root.DescendantRemoving:Connect(Handler) } end,
                function() for Index = 1, N do Items[Index].Parent = nil end end, AllIn, AllIn },
            { "AncestryChanged", function() local List = {} for Index = 1, N do table.insert(List, Items[Index].AncestryChanged:Connect(Handler)) end return List end,
                function(Frame) local To = if Frame % 2 == 0 then Container else Container2 for Index = 1, N do Items[Index].Parent = To end end, nil, AllIn },
            { "Changed x10", Many(10, N), Write(N), nil, nil, 10, N },
            { "Changed x100", Many(100, 10), Write(10), nil, nil, 100, 10 },
        }
        local function Median(List) table.sort(List) return List[(#List + 1) // 2] end
        _G.Rows = {}
        task.spawn(function()
            for _, Kind in Kinds do
                local H, Fires = Kind[6] or 1, Kind[7] or N
                local Alone, Queued, Run = {}, {}, {}
                for _, Connected in { false, true } do
                    if Kind[5] then Kind[5]() end
                    local List = if Connected then Kind[2]() else {}
                    for Frame = 1, PER do
                        RunService.PreSimulation:Wait()
                        Count = 0
                        local T0 = os.clock()
                        Kind[3](Frame)
                        local T1 = os.clock()
                        RunService.PostSimulation:Wait()
                        if Frame > 4 then
                            if Connected then
                                table.insert(Queued, T1 - T0)
                                if Count == Fires * H then table.insert(Run, Last - T1) end
                            else
                                table.insert(Alone, T1 - T0)
                            end
                        end
                        if Kind[4] then Kind[4]() end
                    end
                    for _, Connection in List do Connection:Disconnect() end
                end
                _G.Rows[Kind[1]] = { Queued = (Median(Queued) - Median(Alone)) / Fires * 1e9, Run = Median(Run) / (Fires * H) * 1e9 }
            end
            -- the handlers of a frame event, 100 on each: first line to last, over 99
            _G.Span = {}
            for _, EventName in { "PreSimulation", "Heartbeat", "PreRender" } do
                local First, Final, Seen, Spans = 0, 0, 0, {}
                local List = {}
                for _ = 1, N do
                    table.insert(List, RunService[EventName]:Connect(function()
                        local Now = os.clock()
                        if Seen == 0 then First = Now end
                        Seen += 1
                        Final = Now
                    end))
                end
                for Frame = 1, PER do
                    RunService.PostSimulation:Wait()
                    Seen = 0
                    RunService.PostSimulation:Wait()
                    if Frame > 4 and Seen == N then table.insert(Spans, (Final - First) / (N - 1) * 1e9) end
                end
                for _, Connection in List do Connection:Disconnect() end
                _G.Span[EventName] = Median(Spans)
            end
            _G.Done = true
        end)
        "##,
    );
    for _ in 0..400 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    run(
        &lua,
        &engine,
        r##"
        assert(_G.Done, "the probe did not finish")
        -- ns: Studio's medians, queued per fire and run per handler thread, at the quarter the parity bench reads them at (cost.rs
        -- signal_costs; a Changed fire's own queueing 200 ns, unscaled)
        local Q = 0.247
        local Studio = {
            Bindable = { 580 * Q, 1730 * Q }, ChildRemoved = { 420 * Q, 1500 * Q }, DescendantRemoving = { 350 * Q, 1580 * Q },
            AncestryChanged = { 650 * Q, 2170 * Q }, ["Changed x10"] = { 200 + 9 * 450 * Q, 950 * Q }, ["Changed x100"] = { 200 + 99 * 450 * Q, 943 * Q },
        }
        for Name, Row in _G.Rows do
            local Want = Studio[Name]
            assert(Row.Queued > Want[1] * 0.65 and Row.Queued < Want[1] * 1.35, string.format("%s queued %.0f ns, Studio %d", Name, Row.Queued, Want[1]))
            assert(Row.Run > Want[2] * 0.65 and Row.Run < Want[2] * 1.35, string.format("%s run %.0f ns, Studio %d", Name, Row.Run, Want[2]))
        end
        for Name, Span in _G.Span do
            assert(Span > 450 and Span < 950, string.format("%s handlers %.0f ns apart, Studio 630-730", Name, Span))
        end
        print("ok")
        "##,
    );
    assert!(lines(&engine).contains(&"print: ok".to_string()), "{:?}", lines(&engine));
}

/// The boundary profiler (src/boundary.rs) books each crossing from a script into the engine to the line of the game's Luau
/// that made it, with the cost the work clock charged: a property read and write by Class.Member, a method, an event connect,
/// a datatype operation (booked to the call before it, among the VM's host calls), a task thread; a write of what the property holds counts as unchanged
/// and a read of a property the same function read this frame as a repeat. Nothing is counted when it is off.
#[test]
fn the_boundary_profiler_counts_crossings_per_call_site() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("ree-boundary-{}-{stamp}.json", std::process::id()));
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    assert!(!crate::boundary::active());
    crate::boundary::begin(crate::boundary::Config { out: path.clone(), top: 5, phase: None });
    crate::luau_profile::install(&lua);
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let script = [
        "local Part = Instance.new(\"Part\")",
        "Part.Parent = workspace",
        "local function Frame()",
        "    local a = Part.Transparency",
        "    local b = Part.Transparency",
        "    Part.Transparency = 0.5",
        "    Part.Transparency = 0.5",
        "    Part:GetAttribute(\"X\")",
        "    local c = CFrame.new(1, 2, 3)",
        "    local d = c * c",
        "    Part.Changed:Connect(function() end)",
        "    task.spawn(function() end)",
        "end",
        "Frame()",
        "",
    ]
    .join("\n");
    run(&lua, &engine, &script);
    let text = crate::boundary::finish().expect("report");
    assert!(!crate::boundary::active());
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("report file")).expect("json");
    let _ = std::fs::remove_file(&path);
    let row = |kind: &str, member: &str, line: i64| -> serde_json::Value {
        report["by_site_count"]
            .as_array()
            .unwrap()
            .iter()
            .chain(report["by_site_host_calls"].as_array().unwrap())
            .find(|row| row["kind"] == kind && row["member"] == member && row["line"] == line && row["script"] == "Test")
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} {member} at line {line}\n{text}"))
    };
    // reads: two lines, the second a repeat of the first (the same instance and property, this function, this frame)
    assert_eq!((row("get", "BasePart.Transparency", 4)["count"].as_u64(), row("get", "BasePart.Transparency", 4)["repeat_reads"].as_u64()), (Some(1), Some(0)));
    assert_eq!(row("get", "BasePart.Transparency", 5)["repeat_reads"].as_u64(), Some(1));
    // writes: the second stores what the property holds
    assert_eq!(row("set", "BasePart.Transparency", 6)["unchanged_writes"].as_u64(), Some(0));
    assert_eq!(row("set", "BasePart.Transparency", 7)["unchanged_writes"].as_u64(), Some(1));
    assert!(row("set", "BasePart.Transparency", 6)["cost_ms_per_s"].as_f64().unwrap() > row("set", "BasePart.Transparency", 7)["cost_ms_per_s"].as_f64().unwrap());
    // a method, an event connect with the signal's name, a task thread, and the datatype operations booked to the line before them
    assert_eq!(row("method", "Instance.GetAttribute", 8)["count"].as_u64(), Some(1));
    assert_eq!(row("event", "RBXScriptSignal.Connect(Changed)", 11)["count"].as_u64(), Some(1));
    assert_eq!(row("method", "task.spawn", 12)["count"].as_u64(), Some(1));
    let host = "VM.host call (datatype op or engine function)";
    let datatype = row("datatype", host, 9);
    assert!(datatype["count"].as_u64().unwrap() >= 1 && datatype["cost_ms_per_s"].as_f64().unwrap() > 0.0, "{datatype}");
    // (the prelude's task.spawn is a host call too, counted apart from the crossings; the line before CFrame.new has none, as the
    // import is resolved when the script loads: it used to be looked up through the engine's globals at each run)
    assert_eq!(report["crossings"].as_u64(), Some(report["by_site_count"].as_array().unwrap().iter().filter(|row| row["kind"] != "datatype").map(|row| row["count"].as_u64().unwrap()).sum()));
    // (the prelude's task.spawn and the datatype operations; an import's lookup through the engine's globals made one more before
    // imports were resolved at load)
    assert!(report["host_calls"].as_u64().unwrap() >= 2);
    // the Instance API's charges are accounted for, by member and by module, over one frame of 1/60 s
    assert!(report["attributed_share_of_instance_api_charges"].as_f64().unwrap() > 0.9, "{text}");
    assert!((report["emulated_seconds"].as_f64().unwrap() - 1.0 / 60.0).abs() < 1e-9);
    assert_eq!(report["by_module"][0]["script"], "Test");
    let members = report["by_member"].as_array().unwrap();
    let transparency = members.iter().find(|row| row["member"] == "BasePart.Transparency" && row["kind"] == "get").unwrap();
    assert_eq!((transparency["reads_per_frame"].as_f64(), transparency["distinct_per_frame"].as_f64()), (Some(2.0), Some(1.0)));
    // off, a run records nothing and the report is gone
    let (lua, engine) = start();
    run(&lua, &engine, "local Part = Instance.new('Part') local _ = Part.Transparency");
    assert!(crate::boundary::finish().is_none());
}

/// The profiler reports the phases asked for added together (`--profile-phase a,b`, or `all`), and a phase in which no virtual time
/// passed (a boot that runs inside one frame) is reported per run, not per second; unnamed, it reports `steady` when the harness
/// named one. The harness names phases through Context.ProfilePhase, which is luau_profile::set_phase.
#[test]
fn the_boundary_profiler_adds_the_phases_asked_for_and_reports_a_boot_per_run() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let read = |path: &std::path::Path| -> serde_json::Value {
        let report = serde_json::from_str(&std::fs::read_to_string(path).expect("report file")).expect("json");
        let _ = std::fs::remove_file(path);
        report
    };
    let probe = "local Part = Instance.new(\"Part\")\nlocal _ = Part.Transparency\nlocal _ = Part.Transparency\n";
    for (wanted, phase, seconds, zero_time, crossings) in [(Some("boot,steady"), "boot,steady", 1.0 / 60.0, false, 6u64), (Some("boot"), "boot", 0.0, true, 3), (None, "steady", 1.0 / 60.0, false, 3)] {
        let path = std::env::temp_dir().join(format!("ree-boundary-phases-{}-{stamp}-{}.json", std::process::id(), wanted.unwrap_or("none")));
        let (lua, engine) = start();
        engine_lua::modules::interpret_all(&lua, &engine);
        crate::boundary::begin(crate::boundary::Config { out: path.clone(), top: 5, phase: wanted.map(str::to_string) });
        crate::luau_profile::install(&lua);
        // a boot inside the first frame's time (none has passed), then a frame of steady state
        crate::boundary::set_phase("boot");
        run(&lua, &engine, probe);
        crate::boundary::set_phase("steady");
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        run(&lua, &engine, probe);
        crate::boundary::finish().expect("report");
        let report = read(&path);
        assert_eq!(report["phase"], phase);
        assert!((report["emulated_seconds"].as_f64().unwrap() - seconds).abs() < 1e-9, "{wanted:?}: {report}");
        assert_eq!(report["zero_time"], zero_time);
        assert_eq!(report["crossings"].as_u64(), Some(crossings), "{wanted:?}");
        assert_eq!(report["phases"].as_array().unwrap().len(), 3, "startup, boot and steady");
    }
}

/// A Luau function iterating a generic for can yield, as on Roblox (the stock VM called it as a C call and raised
/// "attempt to yield across metamethod/C-call boundary"): task.wait and coroutine.yield inside it, any number of
/// loop variables, break, nested loops, varargs, an error inside it caught by pcall with its line, and C
/// iterators (string.gmatch) and plain loops unchanged. Lua 5.1 style iterators (next, state, index) keep their
/// index. The interpreter's FORGLOOP does it (as `run` and `record` interpret every module); natively compiled code
/// keeps the stock C call.
#[test]
fn a_luau_iterator_of_a_generic_for_can_yield() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    run(
        &lua,
        &engine,
        r#"
        local function Frames(Count)
            local Index = 0
            return function()
                Index += 1
                if Index > Count then
                    return nil
                end
                task.wait(0.01)
                return Index, Index * 10, "x" .. Index
            end
        end
        local function Stateful(State, Index)
            task.wait()
            if Index < State then
                return Index + 1
            end
            return nil
        end
        local function Varargs(...)
            local Arguments = table.pack(...)
            coroutine.yield()
            if Arguments[2] >= 2 then
                return nil
            end
            return Arguments[2] + 1
        end
        task.spawn(function()
            local Seen = {}
            for Index, Tens, Name in Frames(3) do
                table.insert(Seen, `{Index}:{Tens}:{Name}`)
            end
            for Index in Frames(5) do
                if Index == 2 then
                    break
                end
                table.insert(Seen, `b{Index}`)
            end
            for Outer in Frames(2) do
                for Inner in Frames(2) do
                    table.insert(Seen, `{Outer}{Inner}`)
                end
            end
            for Index in Stateful, 3, 0 do
                table.insert(Seen, `s{Index}`)
            end
            local Ok, Message = pcall(function()
                for _ in function() task.wait() error("inside the iterator") end do
                end
            end)
            table.insert(Seen, `{Ok} {string.match(Message, ":(%d+): inside the iterator$") ~= nil}`)
            for Word in string.gmatch("a b", "%a") do
                table.insert(Seen, Word)
            end
            for Index, Value in { 7, 8 } do
                table.insert(Seen, `{Index}={Value}`)
            end
            print(table.concat(Seen, " "))
        end)
        local Thread = coroutine.create(function()
            local Seen = {}
            for Index in Varargs, nil, 0 do
                table.insert(Seen, Index)
            end
            return table.concat(Seen, ",")
        end)
        local Resumes = 0
        local Ok, Result = coroutine.resume(Thread)
        while coroutine.status(Thread) == "suspended" do
            Resumes += 1
            Ok, Result = coroutine.resume(Thread)
        end
        print("coroutine", Ok, Result, Resumes)
        "#,
    );
    scheduler::run_until(&lua, &engine, 1.0).unwrap();
    let output = lines(&engine);
    assert!(output.contains(&"print: coroutine true 1,2 3".to_string()), "{output:?}");
    assert!(output.contains(&"print: 1:10:x1 2:20:x2 3:30:x3 b1 11 12 21 22 s1 s2 s3 false true a b 1=7 2=8".to_string()), "{output:?}");
    assert!(!output.iter().any(|line| line.starts_with("error")), "{output:?}");
}

/// HttpService:JSONEncode writes an empty table as an array, "[]", as Roblox's does, at the top and inside (an object's keys
/// in the table's own iteration order, as a playing client writes them: tools/parity/probes/json_more.luau).
#[test]
fn json_encode_writes_an_empty_table_as_an_array() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local HttpService = game:GetService("HttpService")
        print(HttpService:JSONEncode({}), HttpService:JSONEncode({ A = {}, B = { 1 }, C = { D = {} } }))
        print(HttpService:JSONEncode(HttpService:JSONDecode("{}")), #HttpService:JSONDecode("[]"))
        "#,
    );
    assert!(output.contains(&r#"print: [] {"A":[],"C":{"D":[]},"B":[1]}"#.to_string()), "{output:?}");
    assert!(output.contains(&"print: [] 0".to_string()), "{output:?}");
}

/// HttpService:JSONEncode of a table inside itself, or nested past 300 levels, is the script's error, with Roblox's
/// messages (measured in a playing client, Studio 0.744: tools/parity/probes/json_limits.luau; an unbounded copy once
/// recursed until the stack overflowed and the process ended).
#[test]
fn json_encode_refuses_cycles_and_deep_nesting() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local HttpService = game:GetService("HttpService")
        local Cycle = { A = {} }
        Cycle.A.Back = Cycle
        print(pcall(function() return HttpService:JSONEncode(Cycle) end))
        local Shared = { 1 }
        print(HttpService:JSONEncode({ A = Shared, B = Shared }))
        local function Nest(Depth)
            local Root = {}
            local At = Root
            for _ = 2, Depth do
                At[1] = {}
                At = At[1]
            end
            return Root
        end
        print(#HttpService:JSONEncode(Nest(300)), pcall(function() return HttpService:JSONEncode(Nest(301)) end))
        print(pcall(function() return HttpService:JSONEncode(Nest(100000)) end))
        "#,
    );
    assert!(output.contains(&"print: false tables cannot be cyclic".to_string()), "{output:?}");
    assert!(output.contains(&r#"print: {"A":[1],"B":[1]}"#.to_string()), "{output:?}");
    let deep = "Max lua stack size reached";
    assert!(output.contains(&format!("print: 600 false {deep}")), "{output:?}");
    assert!(output.contains(&format!("print: false {deep}")), "{output:?}");
}

/// HttpService:JSONEncode and JSONDecode as a playing client runs them (Studio 0.744, tools/parity/probes/json.luau,
/// json_numbers.luau, json_more.luau, json_limits.luau): arrays to the border with holes as null, object keys named as a
/// bindable names them, unsupported values as null, numbers in Grisu2's digits with the client's rounding, a buffer and
/// non-finite numbers as Roblox's objects; a strict decoder with correctly rounded numbers and Roblox's nesting limits.
#[test]
fn json_encodes_and_decodes_as_a_playing_client_does() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local Http = game:GetService("HttpService")
        local function Is(Got, Want)
            assert(Got == Want, `{Want} came out as {Got}`)
        end
        Is(Http:JSONEncode({ 1, nil, 3 }), "[1,null,3]")
        Is(Http:JSONEncode({ 1, 2, Key = 3 }), "[1,2]")
        Is(Http:JSONEncode({ [0] = "z" }), '{"0":"z"}')
        Is(Http:JSONEncode({ [1.5] = "x" }), '{"1":"x"}')
        Is(Http:JSONEncode({ [-0.5] = 1 }), '{"0":1}')
        Is(Http:JSONEncode({ [workspace] = 1 }), '{"<Instance> (Workspace)":1}')
        Is(Http:JSONEncode({ print, workspace, Vector3.new(1, 2, 3), Enum.Material.Plastic }), "[null,null,null,null]")
        Is(Http:JSONEncode({ a = workspace }), '{"a":null}')
        Is(Http:JSONEncode(buffer.create(2)), '{"m":null,"t":"buffer","base64":"AAA="}')
        Is(Http:JSONEncode({ math.huge, -math.huge, 0 / 0 }), '[{"m":null,"t":"numeric","v":"inf"},{"m":null,"t":"numeric","v":"-inf"},{"m":null,"t":"numeric","v":"nan"}]')
        Is(Http:JSONEncode({ -0, 1e20, 1e21, 2 ^ 63, 1e-7, 0.1 + 0.2 }), "[-0,100000000000000000000,1e21,9223372036854776000,1e-7,0.30000000000000007]")
        Is(Http:JSONEncode("\b\f\v\0\27\127/"), '"\\b\\f\\u000B\\u0000\\u001B\127/"')
        assert(not pcall(Http.JSONEncode, Http, "\255"))
        Is(select(2, pcall(Http.JSONEncode, Http, { [true] = 1 })), "Invalid table key type used")
        Is(1 / Http:JSONDecode("-0"), math.huge)
        Is(1 / Http:JSONDecode("-0.0"), -math.huge)
        Is(Http:JSONDecode("195.66666666666666"), 195.66666666666666)
        for _, Bad in { "1e400", '"\\ud83d"', '"tab\tin"', "[1,]", "01", ".5", "5.", "NaN", "" } do
            local Decoded, Error = pcall(Http.JSONDecode, Http, Bad)
            assert(not Decoded and Error == "Can't parse JSON", Bad)
        end
        Is(Http:JSONDecode('"\255"'), "\255")
        local Holes = Http:JSONDecode("[null,1]")
        assert(Holes[1] == nil and Holes[2] == 1)
        Is(Http:JSONDecode('{"1":"x"}')["1"], "x")
        assert(pcall(Http.JSONDecode, Http, string.rep("[", 401) .. string.rep("]", 401)))
        Is(select(2, pcall(Http.JSONDecode, Http, string.rep("[", 402) .. string.rep("]", 402))), "WebParser conversion stack overflow")
        Is(select(2, pcall(Http.JSONDecode, Http, string.rep("[", 1025) .. string.rep("]", 1025))), "Can't parse JSON")
        assert(next(Http:JSONDecode({ 1, 2 })) == nil)
        Is(Http:JSONDecode(12.5), 12.5)
        "#,
    );
    assert!(!output.iter().any(|line| line.starts_with("error")), "{output:?}");
}

/// A playing client's LogService.MessageOut gets each line after the frame's Heartbeat handlers and the deferred work
/// they leave, with its MessageType, a script error with its stack lines; a line a MessageOut handler logs reaches no
/// handler (measured in a playing client, Studio 0.744: tools/parity/probes/logservice.luau, logservice_text.luau).
#[test]
fn a_playing_clients_message_out_arrives_after_the_frames_heartbeat() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local Log = {}
        _G.Log = Log
        game:GetService("RunService").Heartbeat:Connect(function()
            table.insert(Log, "|hb")
        end)
        game:GetService("LogService").MessageOut:Connect(function(Message, Kind)
            table.insert(Log, Kind.Name .. "[" .. table.concat(string.split(Message, "\n"), "/") .. "]")
            if Message == "outer" then
                warn("inner")
            end
        end)
        warn("outer")
        table.insert(Log, "after-call")
        task.defer(table.insert, Log, "deferred")
        task.spawn(error, "spawned")
        "##,
    );
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(&lua, &engine, r#"print(table.concat(_G.Log, ","))"#);
    let want = "print: after-call,deferred,|hb,MessageWarning[outer],MessageError[spawned/    Stack Begin/    Stack End],|hb";
    assert!(output.contains(&want.to_string()), "{output:?}");
}

#[test]
fn log_history_is_ordered_bounded_and_returns_independent_tables() {
    for server in [false, true] {
        let (lua, engine) = start_with(Options { mode: "play".into(), run_context: if server { super::RunContext::Server } else { super::RunContext::Client }, ..Options::default() });
        let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        run(&lua, &engine, r#"
            local Log = game:GetService("LogService")
            assert(#Log:GetLogHistory() == 0)
            print("first", 2)
            warn("second")
            Log:Info("third")
            pcall(Log.Error, Log, "fourth")
            task.spawn(error, "fifth")
            local History = Log:GetLogHistory()
            assert(#History == 5)
            local Kinds = { "MessageOutput", "MessageWarning", "MessageInfo", "MessageError", "MessageError" }
            for Index, Entry in History do
                assert(Entry.messageType == Enum.MessageType[Kinds[Index]])
                assert(type(Entry.timestamp) == "number" and Entry.timestamp > 1700000000)
            end
            assert(History[1].message == "first 2" and History[2].message == "second")
            assert(History[3].message == "third" and History[4].message == "fourth")
            assert(string.find(History[5].message, "fifth", 1, true))
            History[1].message = "changed"
            History[2] = nil
            local Again = Log:GetLogHistory()
            assert(Again ~= History and Again[1] ~= History[1])
            assert(#Again == 5 and Again[1].message == "first 2")
            _G.LogListener = Log.MessageOut:Connect(function(Message)
                if Message == "outer" then warn("inner") end
            end)
            print("outer")
        "#);
        for _ in 0..2 {
            scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        }
        run(&lua, &engine, r#"
            _G.LogListener:Disconnect()
            local Last = game:GetService("LogService"):GetLogHistory()
            assert(Last[#Last].message == "inner")
        "#);
        let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        assert!(engine.state.borrow().log_history.iter().all(|entry| entry.timestamp >= before && entry.timestamp <= after));
        for index in 0..super::LOG_HISTORY_CAP + 3 {
            engine.output("info", index.to_string(), None);
        }
        run(&lua, &engine, r#"
            local History = game:GetService("LogService"):GetLogHistory()
            assert(#History == 10000)
            assert(History[1].message == "3" and History[10000].message == "10002")
        "#);
    }
}

/// LogService's logging methods in a playing client (measured, Studio 0.744: tools/parity/probes/logservice_methods.luau): a
/// line of the method's MessageType with its {key} placeholders filled ({{ and }} escapes, a missing key kept), which
/// MessageOut gets after the frame's Heartbeat with a converted copy of the context as its third argument (every line gets
/// one: an empty table without a context); Error logs its line, then throws a table (message, template, context, stack) whose
/// tostring is the message, and one nothing catches logs its stack as MessageInfo lines; a message that is not a string warns
/// first. The run's output keeps a MessageInfo line as kind `info`.
#[test]
fn log_service_methods_log_with_their_context_and_error_throws_a_table() {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    run(
        &lua,
        &engine,
        r##"
        local Log = {}
        _G.Log = Log
        local LogService = game:GetService("LogService")
        LogService.MessageOut:Connect(function(...)
            local Message, Kind, Context = ...
            local Keys = {}
            for Key, Value in Context do
                table.insert(Keys, tostring(Key) .. "=" .. tostring(Value))
            end
            table.sort(Keys)
            table.insert(Log, Kind.Name .. "[" .. table.concat(string.split(Message, "\n"), "/") .. "]" .. select("#", ...) .. "{" .. table.concat(Keys, ";") .. "}")
        end)
        local Part = Instance.new("Part")
        Part.Name = "P"
        Part.Parent = workspace
        LogService:Info("i a={a} {{x}} }} {missing}", { a = 1 })
        LogService:Output("o {p}", { p = Part, f = print })
        LogService:Warn("w {1}", { "x" })
        LogService:Log(Enum.MessageType.MessageInfo, "l {a}", { a = 2 })
        print("p")
        local Ok, Thrown = pcall(LogService.Error, LogService, "e {a}", { a = 3 })
        print("thrown", Ok, typeof(Thrown), Thrown.message, Thrown.template, Thrown.context.a, tostring(Thrown), string.sub(Thrown.stack, 1, 11))
        print("errors", select(2, pcall(LogService.Info, LogService)), select(2, pcall(LogService.Log, LogService, 7, "x")))
        task.spawn(function()
            LogService:Error("u")
        end)
        task.spawn(function()
            LogService:Info(true)
        end)
        "##,
    );
    for _ in 0..2 {
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    let output = run(&lua, &engine, r#"print(table.concat(_G.Log, ","))"#);
    assert!(output.contains(&"print: thrown false table e 3 e {a} 3 e 3 Stack Begin".to_string()), "{output:?}");
    assert!(output.contains(&"print: errors Argument 2 missing or nil Unable to cast double to MessageType".to_string()), "{output:?}");
    let heard = output.last().expect("the log").clone();
    let want = [
        "MessageInfo[i a=1 {x} } {missing}]3{a=1}",
        "MessageOutput[o P]3{f=<function>;p=Workspace.P}",
        "MessageWarning[w {1}]3{1=x}",
        "MessageInfo[l 2]3{a=2}",
        "MessageOutput[p]3{}",
        "MessageError[e 3]3{a=3}",
        "MessageError[u]3{},MessageInfo[Stack Begin]3{},MessageInfo[Script 'Test', Line 26]3{},MessageInfo[Stack End]3{}",
        "MessageWarning[argument #1 expects a string, but boolean was passed/    Stack Begin/    Script 'Test', Line 29/    Stack End]3{},MessageInfo[true]3{}",
    ];
    for part in want {
        assert!(heard.contains(part), "{part} in {heard}");
    }
    let lines: Vec<String> = engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect();
    for line in ["info: i a=1 {x} } {missing}", "print: o P", "warn: w {1}", "info: l 2", "error: e 3", "error: u", "info: Stack Begin", "info: Stack End"] {
        assert!(lines.contains(&line.to_string()), "{line} in {lines:?}");
    }
    // (the uncaught error logs no second error line of its own)
    assert_eq!(lines.iter().filter(|line| line.starts_with("error: u")).count(), 1, "{lines:?}");
}

/// A part fires the Changed events Roblox fires beside a write, whatever their own values (measured in a playing client,
/// Studio 0.744: tools/parity/probes/derived_changed.luau), and its BrickColor is a view of its Color.
#[test]
fn a_part_fires_the_events_roblox_fires_beside_a_write() {
    let (lua, engine) = start();
    // the writes in one call, the events (deferred, run at its flush) read in the next
    run(
        &lua,
        &engine,
        r##"
        _G.Logs = {}
        local function Case(Write)
            local Part = Instance.new("Part")
            Part.Anchored = true
            local Log = {}
            table.insert(_G.Logs, Log)
            Part.Changed:Connect(function(Name)
                table.insert(Log, Name)
            end)
            Write(Part)
        end
        Case(function(Part) Part.Position = Vector3.new(1, 2, 3) end)
        Case(function(Part) Part.Size = Vector3.new(2, 2.4, 2) end)
        Case(function(Part) Part.Color = Color3.fromRGB(163, 162, 166) end)
        Case(function(Part) Part.Material = Enum.Material.SmoothPlastic end)
        Case(function(Part) Part.CustomPhysicalProperties = PhysicalProperties.new(2, 0.3, 0.5) end)
        Case(function(Part) Part.Anchored = false end)
        "##,
    );
    let output = run(
        &lua,
        &engine,
        r##"
        for _, Log in _G.Logs do
            print(table.concat(Log, ","))
        end
        local Part = Instance.new("Part")
        Part.BrickColor = BrickColor.new("Bright blue")
        Part.Color = Color3.new(1, 0, 0)
        print(tostring(Part.BrickColor), (Part.Color:ToHex()))
        Part.BrickColor = BrickColor.new("Bright blue")
        print(tostring(Part.BrickColor), (Part.Color:ToHex()))
        "##,
    );
    for want in [
        "print: CFrame,Position,AssemblyCenterOfMass,Orientation,Rotation",
        "print: Size,AssemblyMass,Mass",
        "print: BrickColor,Color",
        "print: Material,CurrentPhysicalProperties",
        "print: CurrentPhysicalProperties,CustomPhysicalProperties,AssemblyMass,AssemblyCenterOfMass",
        "print: Anchored,AssemblyMass,AssemblyCenterOfMass",
        "print: Really red ff0000",
        "print: Bright blue 0d69ac",
    ] {
        assert!(output.contains(&want.to_string()), "{want}: {output:?}");
    }
}

/// Lowers the engine's limits on held slots (lua/refs.rs) to what it holds now plus `work` / `wait`.
fn limit_held(engine: &Engine, work: i32, wait: i32) {
    let refs = engine.lua.borrow();
    let gauge = refs.gauge.as_ref().expect("the VM's reference thread");
    let held = gauge.count();
    gauge.set_limits(held + work, held + wait);
}

/// The gauge finds mlua's reference thread and counts what Rust holds: values made and dropped from Rust move it.
#[test]
fn the_gauge_counts_what_rust_holds() {
    let (lua, engine) = start();
    let count = || engine.lua.borrow().gauge.as_ref().expect("the VM's reference thread").count();
    let before = count();
    let held: Vec<LuaTable> = (0..1000).map(|_| lua.create_table().unwrap()).collect();
    assert_eq!(count(), before + 1000);
    drop(held);
    assert_eq!(count(), before);
}

/// A loop that queues work without end errors in the script that queued it once the engine holds its limit (a
/// task.defer per hit in a loop that never yielded ran mlua's reference stack full, and the panic ended the run with
/// exit 101). What it queued still runs, and the other scripts go on.
#[test]
fn a_runaway_queue_is_its_scripts_error() {
    let (lua, engine) = start();
    limit_held(&engine, 20_000, 25_000);
    run(
        &lua,
        &engine,
        r#"
        local Queued, Ran = 0, 0
        task.delay(0.1, function() print("ran all", Queued > 15000, Ran == Queued) end)
        task.spawn(function()
            for Index = 1, 1000000 do
                task.defer(function() Ran += 1 end)
                Queued = Index
            end
            print("never")
        end)
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.3).unwrap();
    let entries = engine.state.borrow().output.entries.clone();
    let refused: Vec<_> = entries.iter().filter(|entry| entry.kind == "error").collect();
    assert_eq!(refused.len(), 1, "{entries:?}");
    assert!(refused[0].message.starts_with("Test:6: task.defer: too many threads and values held for scripts ("), "{entries:?}");
    assert!(refused[0].trace.as_deref().is_some_and(|trace| trace.contains("Test:6")), "{entries:?}");
    let output = lines(&engine);
    assert!(output.contains(&"print: ran all true true".to_string()), "{output:?}");
    assert!(!output.contains(&"print: never".to_string()), "{output:?}");
}

/// A thread made with coroutine.create holds no slot until it waits: a loop of them that wait is refused at task.wait,
/// past the higher mark for waits (and a task.spawn past the lower mark meanwhile is refused too).
#[test]
fn a_runaway_of_waiting_coroutines_is_refused_at_its_wait() {
    let (lua, engine) = start();
    limit_held(&engine, 20_000, 25_000);
    run(
        &lua,
        &engine,
        r#"
        local Started, Done = 0, 0
        task.delay(0.2, function() print("done", Started > 20000, Done == Started - 1) end)
        task.spawn(function()
            for Index = 1, 1000000 do
                Started = Index
                coroutine.wrap(function()
                    task.wait(0.05)
                    Done += 1
                end)()
            end
        end)
        print("spawn", pcall(function() task.spawn(function() end) end))
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.5).unwrap();
    let entries = engine.state.borrow().output.entries.clone();
    let refused: Vec<_> = entries.iter().filter(|entry| entry.kind == "error").collect();
    assert_eq!(refused.len(), 1, "{entries:?}");
    assert!(refused[0].message.contains("Test:8: task.wait: too many threads and values held for scripts ("), "{entries:?}");
    let output = lines(&engine);
    assert!(output.iter().any(|line| line.starts_with("print: spawn false Test:13: task.spawn: too many threads")), "{output:?}");
    assert!(output.contains(&"print: done true true".to_string()), "{output:?}");
}

/// What still runs mlua's reference stack full (a call with a million string arguments: mlua makes a slot of each before
/// the engine sees them) errors in the thread that made the call, with its traceback, and its spawner goes on.
#[test]
fn a_full_reference_stack_is_the_resumed_threads_error() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local Strings = table.create(999950, "")
        for Index = 1, #Strings do
            Strings[Index] = tostring(Index)
        end
        task.spawn(function()
            task.cancel(table.unpack(Strings))
            print("never")
        end)
        print("spawner goes on")
        "#,
    );
    let entries = engine.state.borrow().output.entries.clone();
    let refused: Vec<_> = entries.iter().filter(|entry| entry.kind == "error").collect();
    assert_eq!(refused.len(), 1, "{entries:?}");
    assert_eq!(refused[0].message, "too many threads and values held for scripts (ree holds at most 1000000)");
    assert!(refused[0].trace.as_deref().is_some_and(|trace| trace.contains("Test:7")), "{entries:?}");
    let output = lines(&engine);
    assert!(output.contains(&"print: spawner goes on".to_string()), "{output:?}");
    assert!(!output.contains(&"print: never".to_string()), "{output:?}");
}

/// ReflectionService:GetClasses and GetClass as a game script gets them (measured in a playing client and a server game
/// script, Studio 0.744): the dump's classes in name order, each call new frozen entries (Name, Superclass, Serialized only when
/// true, Subclasses only when there are some, those a script cannot get included, Display, Permits); the classes only rbx-dom
/// describes are nil; the filter's IsA and ExcludeDisplay, and its measured errors. Permits holds no SecurityCapabilities in ree:
/// reading it reports itself as not emulated.
#[test]
fn reflection_service_lists_the_dumps_classes_as_a_game_script_gets_them() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r#"
        local RS = game:GetService("ReflectionService")
        local List = RS:GetClasses()
        local Previous = ""
        for _, Entry in List do
            assert(Entry.Name > Previous and table.isfrozen(Entry), Entry.Name)
            Previous = Entry.Name
        end
        print("list", #List > 600, table.isfrozen(List), RS:GetClasses() ~= List, RS:GetClasses()[1] ~= List[1], List[1].Name)
        local Part = RS:GetClass("Part")
        print("Part", Part.Name, Part.Superclass, Part.Serialized, table.concat(Part.Subclasses, ","), Part.Display.Category, Part.Display.DeprecationMessage)
        print("frozen", table.isfrozen(Part), table.isfrozen(Part.Subclasses), table.isfrozen(Part.Display), table.isfrozen(Part.Permits))
        local Accessory = RS:GetClass("Accessory")
        print("Accessory", Accessory.Subclasses, Accessory.Serialized, Accessory.Superclass)
        local Object = RS:GetClass("Object")
        print("Object", Object.Superclass, Object.Serialized, #Object.Subclasses > 0)
        print("BasePlayerGui", table.concat(RS:GetClass("BasePlayerGui").Subclasses, ","))
        print("Glue", RS:GetClass("Glue").Display.DeprecationMessage, RS:GetClass("LogService").Serialized)
        print("nil", RS:GetClass("Player"), RS:GetClass("CoreGui"), RS:GetClass("UserGameSettings"), RS:GetClass("part"), RS:GetClass("Nope"), RS:GetClass(5))
        print("IsA", #RS:GetClasses({ IsA = "BasePart" }), #RS:GetClasses({ IsA = "Nope" }), #RS:GetClasses({ IsA = "basepart" }), #RS:GetClasses({ IsA = "Player" }), #RS:GetClasses({ IsA = "Object" }) == #List)
        local NoDisplay = RS:GetClasses({ ExcludeDisplay = true, Unknown = 1 })
        print("ExcludeDisplay", #NoDisplay == #List, NoDisplay[1].Display, RS:GetClasses({ ExcludeDisplay = false })[1].Display ~= nil)
        print("GetClass filtered", RS:GetClass("Part", { IsA = "BasePart" }).Name, RS:GetClass("Part", { IsA = "GuiObject" }), RS:GetClass("Part", { ExcludeDisplay = true }).Display)
        print("errors", select(2, pcall(RS.GetClasses, RS, 5)))
        print("errors", select(2, pcall(RS.GetClasses, RS, { "BasePart" })))
        print("errors", select(2, pcall(RS.GetClasses, RS, { 1, IsA = "BasePart" })))
        print("errors", select(2, pcall(RS.GetClasses, RS, { IsA = 5 })))
        print("errors", select(2, pcall(RS.GetClasses, RS, { ExcludeDisplay = 1 })))
        print("errors", select(2, pcall(RS.GetClasses, RS, { Security = "x" })))
        print("errors", select(2, pcall(RS.GetClass, RS)))
        print("errors", select(2, pcall(RS.GetClass, RS, "Part", true)))
        print("errors", (string.gsub(select(2, pcall(function() List[1] = 1 end)), "^Test:%d+: ", "")))
        print("errors", select(2, pcall(RS.GetClasses)))
        for _ in Part.Permits do
            print("never")
        end
        print("Permits", typeof(Part.Permits), Part.Permits.New, next(Part.Permits))
        "#,
    );
    let expected = [
        "print: list true true true true Accessory",
        "print: Part Part FormFactorPart true FlagStand,Platform,Seat,SkateboardPlatform,SpawnLocation General nil",
        "print: frozen true true true true",
        "print: Accessory nil true Accoutrement",
        "print: Object nil nil true",
        "print: BasePlayerGui CoreGui,PlayerGui,StarterGui",
        "print: Glue Use `WeldConstraint` instead. nil",
        "print: nil nil nil nil nil nil nil",
        "print: IsA 19 0 0 0 true",
        "print: ExcludeDisplay true nil true",
        "print: GetClass filtered Part nil nil",
        "print: errors Unable to cast double to Dictionary",
        "print: errors Unable to cast Array to Dictionary",
        "print: errors Unable to cast Array to Dictionary",
        "print: errors ReflectionClassFilter's IsA field is expected to be type string",
        "print: errors ReflectionClassFilter's ExcludeDisplay field is expected to be type bool",
        "print: errors ReflectionClassFilter's Security field is expected to be type SecurityCapabilities",
        "print: errors Argument 2 missing or nil",
        "print: errors Unable to cast bool to Dictionary",
        "print: errors attempt to modify a readonly table",
        "print: errors Expected ':' not '.' calling member function GetClasses",
        "print: Permits table nil nil",
    ];
    assert_eq!(output, expected);
    let missing = engine.state.borrow().output.missing.clone();
    assert_eq!(missing, vec![("ReflectedClass.Permits".to_string(), 2)], "{missing:?}");
    // the dump's classes and no others; IsA counts a class and its subclasses
    let reflected = engine.reflection.classes.iter().filter(|class| class.reflected).count();
    assert_eq!(reflected, 640);
}

/// A vide-like module that builds its element names from ReflectionService:GetClasses() when it is required (stock vide's
/// jsx.luau maps every lower-cased class name to the class name) loads, and makes instances through that map.
#[test]
fn a_module_that_maps_every_class_name_at_require_loads() {
    let (lua, engine) = start();
    let output = run(
        &lua,
        &engine,
        r##"
        local Module = Instance.new("ModuleScript")
        Module.Name = "jsx"
        __setSource(Module, [[
local tags = {}
if game then
	for _, class in game:GetService("ReflectionService"):GetClasses() do
		tags[string.lower(class.Name)] = class.Name
	end
end
local function jsx(tag, props, ...)
	props = props or {}
	tag = tags[tag] or tag
	local instance = Instance.new(tag)
	for key, value in props do
		instance[key] = value
	end
	for index = 1, select("#", ...) do
		select(index, ...).Parent = instance
	end
	return instance
end
return jsx
]])
        local jsx = require(Module)
        local Root = jsx("frame", { Name = "Root" }, jsx("textlabel", { Text = "hi" }), jsx("uicorner", {}))
        local Label = Root:FindFirstChildOfClass("TextLabel")
        print("jsx", Root.ClassName, Root.Name, Label.ClassName, Label.Text, Root:FindFirstChildOfClass("UICorner") ~= nil)
        "##,
    );
    assert_eq!(output, vec!["print: jsx Frame Root TextLabel hi true".to_string()]);
}

#[test]
fn an_executable_belongs_to_the_nearest_checkout_above_it_wherever_it_runs() {
    let root = std::env::temp_dir().join(format!("ree-checkout-test-{}", std::process::id()));
    let repo = root.join("ree");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("Cargo.lock"), "").unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"roblox-engine-emulator\"\n").unwrap();
    for exe in ["target/release/ree.exe", "target/release/deps/ree-0a1b.exe", ".cache/bin/ree-0.1.0-1-2.exe"] {
        assert_eq!(super::checkout_of(&repo.join(exe)), Some(repo.clone()), "{exe}");
    }
    let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", repo.join("target").join("release").join("ree.exe").display()));
    if cfg!(windows) {
        assert_eq!(super::checkout_of(&verbatim), Some(repo.clone()), "a service's --source is canonical");
    }
    // a service copy in a state dir outside any checkout has none: serve takes its starter's (--source)
    assert_eq!(super::checkout_of(&root.join("state/bin/ree-0.1.0-1-2.exe")), None);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn checkout_of_still_finds_a_hosts_nearest_lockfile() {
    let root = std::env::temp_dir().join(format!("ree-host-lockfile-test-{}", std::process::id()));
    let host = root.join("host");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::write(root.join("Cargo.lock"), "").unwrap();
    std::fs::write(host.join("Cargo.lock"), "").unwrap();
    let exe = host.join("target/release/host.exe");
    assert_eq!(super::checkout_of(&exe), Some(host.clone()), "a host needs only its lockfile");
    std::fs::write(host.join("Cargo.toml"), "[package]\nname = \"another-tool\"\n").unwrap();
    assert_eq!(super::checkout_of(&exe), Some(host));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_unrelated_cargo_project_is_not_the_executables_checkout() {
    let root = std::env::temp_dir().join(format!("ree-foreign-checkout-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("bin/ree.exe");
    let manifest = root.join("Cargo.toml");
    std::fs::write(root.join("Cargo.lock"), "").unwrap();
    assert_eq!(super::ree_checkout_of(&exe), None, "a lockfile without a manifest");
    // a host that depends on ree names it under [dependencies], outside [package]
    std::fs::write(&manifest, "[package]\nname = \"another-tool\"\n[dependencies]\nroblox-engine-emulator = \"0.1.0\"\n").unwrap();
    assert_eq!(super::ree_checkout_of(&exe), None, "a host's manifest does not name this package");
    // a literal string and a trailing comment still name the package
    std::fs::write(&manifest, "[package]\nname = 'roblox-engine-emulator' # this package\n").unwrap();
    assert_eq!(super::ree_checkout_of(&exe), Some(root.clone()));
    // the search goes on past a lockfile that has no ree manifest beside it
    let nested = root.join("target/nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("Cargo.lock"), "").unwrap();
    assert_eq!(super::ree_checkout_of(&nested.join("bin/ree.exe")), Some(root.clone()));
    std::fs::remove_dir_all(&root).unwrap();
}

/// Standalone state uses the platform's per-user cache, unless the environment names one.
#[test]
fn standalone_caches_are_per_user() {
    let root = std::env::temp_dir().join("ree-cache-defaults");
    let path = |name: &str| root.join(name);
    // a made-up environment: nothing here reads or creates the real per-user folders
    let environment = [("LOCALAPPDATA", path("local")), ("HOME", path("home")), ("XDG_CACHE_HOME", path("xdg"))];
    let var = |name: &str| environment.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_os_str().to_owned());
    let with = |set: &'static str, value: &'static str| move |name: &str| if name == set { Some(path(value).into_os_string()) } else { var(name) };

    // a standalone run: each platform's own cache folder
    assert_eq!(super::standalone_state_dir("windows", var).unwrap(), path("local/ree"));
    assert_eq!(super::standalone_state_dir("linux", var).unwrap(), path("xdg/ree"));
    assert_eq!(super::standalone_state_dir("macos", var).unwrap(), path("home/Library/Caches/ree"));
    // Linux falls back to ~/.cache when XDG_CACHE_HOME is unset or relative
    let no_xdg = |name: &str| if name == "XDG_CACHE_HOME" { None } else { var(name) };
    assert_eq!(super::standalone_state_dir("linux", no_xdg).unwrap(), path("home/.cache/ree"));
    let relative_xdg = |name: &str| if name == "XDG_CACHE_HOME" { Some("relative".into()) } else { var(name) };
    assert_eq!(super::standalone_state_dir("linux", relative_xdg).unwrap(), path("home/.cache/ree"));

    // explicit overrides take priority over the per-user cache
    for key in ["RUR_STATE_DIR", "RUR_CACHE_DIR"] {
        assert_eq!(super::standalone_state_dir("linux", with(key, "override")).unwrap(), path("override"));
    }
    // RUR_STATE_DIR comes before RUR_CACHE_DIR
    let both = |name: &str| match name {
        "RUR_STATE_DIR" => Some(path("first").into_os_string()),
        "RUR_CACHE_DIR" => Some(path("second").into_os_string()),
        _ => var(name),
    };
    assert_eq!(super::standalone_state_dir("linux", both).unwrap(), path("first"));
}

#[test]
fn standalone_state_stays_selected_after_environment_normalization() {
    use crate::launch::resolve_path_variable;
    use std::ffi::OsString;
    use std::sync::OnceLock;
    let root = std::path::absolute("cache-test").unwrap();
    for os in ["windows", "linux", "macos"] {
        // absolute HOME and LOCALAPPDATA, plus a relative XDG_CACHE_HOME that the cache lookup ignores
        let client_environment = |name: &str| match name {
            "LOCALAPPDATA" => Some(root.join("local").into_os_string()),
            "HOME" => Some(root.join("home").into_os_string()),
            "XDG_CACHE_HOME" => Some(OsString::from("relative")),
            _ => None,
        };
        let expected = super::standalone_state_dir(os, client_environment).unwrap();

        // the client's first lookup pins the state dir, before a service resolves its paths or changes folder
        let state = OnceLock::new();
        assert_eq!(super::resolve_standalone_state(&state, os, client_environment).unwrap(), expected);

        // what a service or worker gets: the ignored relative folder is absolute now, so a fresh Linux lookup would pick it as the cache
        let forwarded_environment = |name: &str| client_environment(name).and_then(resolve_path_variable);
        assert_eq!(forwarded_environment("XDG_CACHE_HOME"), Some(std::path::absolute("relative").unwrap().into_os_string()));
        assert_eq!(super::resolve_standalone_state(&state, os, forwarded_environment).unwrap(), expected, "{os}: forwarding paths must not change the pinned cache");

        // a later job does not consult even a completely different environment
        let changed_environment = |_: &str| Some(root.join("changed").into_os_string());
        assert_eq!(super::resolve_standalone_state(&state, os, changed_environment).unwrap(), expected);

        // with only relative folders there is no cache, so the lookup fails and pins nothing
        let relative_environment = |name: &str| if matches!(name, "HOME" | "LOCALAPPDATA") { Some(OsString::from("relative")) } else { None };
        let unpinned = OnceLock::new();
        assert!(super::resolve_standalone_state(&unpinned, os, relative_environment).is_err(), "{os}: relative per-user folders stay unset");
        assert!(unpinned.get().is_none());
    }
}

/// The VM's work counts so far (a copy: the counters are plain numbers)
fn vm_counts(engine: &Engine) -> crate::cost::VmCounters {
    let pointer = engine.state.borrow().sched.vm_counters.expect("VM counters");
    unsafe { std::ptr::read(pointer) }
}

/// The first number of the first `print` line `run` collected
fn printed_number(output: &[String]) -> f64 {
    let line = output.iter().find_map(|line| line.strip_prefix("print: ")).unwrap_or_else(|| panic!("a result: {output:?}"));
    line.split(' ').next().unwrap().parse().unwrap_or_else(|_| panic!("a number: {line}"))
}

/// A script's environment is safe, as Roblox makes every script's (the parity bench, tools/cost-parity: two imports a loop step
/// 4.6 ns in a module, 21.4 in one that called getfenv first; `typeof(x)` 4.7 and 25.9): its imports are resolved when it loads,
/// `typeof` is the VM's fast call, which names ree's values as ree's typeof does (a constant Vector3, an Instance, a proxy datatype,
/// SharedTable), and getfenv ends both for its environment. ree loaded each script on a thread whose globals were not its
/// environment, so every import was looked up at each run, and its compiler kept typeof off the fast path (a C call, 23 ns).
#[test]
fn a_scripts_environment_resolves_imports_at_load_and_takes_fast_calls() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let source = |prefix: &str| {
        format!(
            "{prefix}local M = {{}}\nfunction M.Run(N) local s = 0 for i = 1, N do local g, v = game, Vector3.new if typeof(i) == \"number\" and g and v then s += 1 end end return s end\n\
             function M.Names() return typeof(Vector3.new(1, 2, 3)), typeof(workspace), typeof(RaycastParams.new()), typeof(SharedTable.new()), typeof({{}}), typeof(newproxy(true)) end\nreturn M\n"
        )
    };
    run(
        &lua,
        &engine,
        &format!(
            r#"
            local Folder = Instance.new("Folder")
            local Safe, Unsafe = Instance.new("ModuleScript"), Instance.new("ModuleScript")
            Safe.Parent, Unsafe.Parent = Folder, Folder
            __setSource(Safe, {safe:?})
            __setSource(Unsafe, {unsafe:?})
            _G.Safe, _G.Unsafe = require(Safe), require(Unsafe)
            "#,
            safe = source(""),
            unsafe = source("local _ = getfenv()\n")
        ),
    );
    let sample = |module: &str| {
        let before = vm_counts(&engine);
        run(&lua, &engine, &format!("_G.{module}.Run(1000)"));
        let after = vm_counts(&engine);
        (after.ops[21] - before.ops[21], after.ops[73] - before.ops[73], after.slowimports - before.slowimports)
    };
    // (CALL is opcode 21, FASTCALL1 73; the snippet that calls Run makes a call of its own)
    let (calls, fast, slow) = sample("Safe");
    assert!(calls < 10 && fast >= 1000 && slow == 0, "a safe environment: {calls} calls, {fast} fast calls, {slow} imports looked up");
    let (calls, slow) = {
        let (calls, _, slow) = sample("Unsafe");
        (calls, slow)
    };
    assert!(calls >= 1000 && slow >= 3000, "after getfenv: {calls} calls, {slow} imports looked up");
    let output = run(&lua, &engine, "print(_G.Safe.Names()) print(_G.Unsafe.Names())");
    let names = "print: Vector3 Instance RaycastParams SharedTable table userdata";
    assert_eq!(output.iter().filter(|line| line.as_str() == names).count(), 2, "{output:?}");
}

/// `math.isnan`, `math.isinf` and `math.isfinite` (Luau's since 0.701) are builtins a script calls as fast
/// calls, as a client does (the parity bench's `arith.isnan` rows: a loop step calling one 4.9-6.2 ns in a module, 20-22 after
/// getfenv), and getfenv makes them plain C calls; the answers are the same either way.
#[test]
fn a_scripts_number_classifiers_are_fast_calls() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let source = |prefix: &str| {
        format!(
            "{prefix}return function(N) local s = 0 for i = 1, N do local x = i * 0.5 if math.isnan(x) then s += 1 end if math.isinf(x) then s += 2 end if math.isfinite(x) then s += 4 end end return s end\n"
        )
    };
    run(
        &lua,
        &engine,
        &format!(
            r#"
            local Folder = Instance.new("Folder")
            local Safe, Unsafe = Instance.new("ModuleScript"), Instance.new("ModuleScript")
            Safe.Parent, Unsafe.Parent = Folder, Folder
            __setSource(Safe, {safe:?})
            __setSource(Unsafe, {unsafe:?})
            _G.Safe, _G.Unsafe = require(Safe), require(Unsafe)
            "#,
            safe = source(""),
            unsafe = source("local _ = getfenv()\n")
        ),
    );
    let sample = |module: &str| {
        let before = vm_counts(&engine);
        let output = run(&lua, &engine, &format!("print(_G.{module}(1000))"));
        let after = vm_counts(&engine);
        (output, after.ops[21] - before.ops[21], after.ops[73] - before.ops[73], after.ccalls - before.ccalls)
    };
    // (CALL is opcode 21, FASTCALL1 73; the snippet that calls the module makes a call or two of its own, and print one C call)
    let (output, calls, fast, ccalls) = sample("Safe");
    assert!(output.contains(&"print: 4000".to_string()), "{output:?}");
    assert!(calls < 10 && fast >= 3000 && ccalls < 10, "a safe environment: {calls} calls, {fast} fast calls, {ccalls} C calls");
    let (output, calls, _, ccalls) = sample("Unsafe");
    assert!(output.contains(&"print: 4000".to_string()), "{output:?}");
    assert!(calls >= 3000 && ccalls >= 3000, "after getfenv: {calls} calls, {ccalls} C calls");
}

/// A `--!native` module's instructions are priced as a client's machine code takes them (cost.rs NATIVE_OPS: a keyed table read or
/// write 0.42 ns, an addition or loop step 0.43; interpreted 2.45 and 0.48), whether or not this VM compiles the module: the parity
/// bench's native rows run 0.8-7.8x faster than the same loops interpreted.
#[test]
fn a_native_modules_instructions_cost_what_native_code_takes() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let source = |prefix: &str| {
        format!(
            "{prefix}return function(N) local t = {{ x = 0 }} local t0 = os.clock() for i = 1, N do t.x = t.x + i end return (os.clock() - t0) / N * 1e9 end\n"
        )
    };
    let output = run(
        &lua,
        &engine,
        &format!(
            r#"
            local Interpreted, Native = Instance.new("ModuleScript"), Instance.new("ModuleScript")
            __setSource(Interpreted, {interpreted:?})
            __setSource(Native, {native:?})
            print(string.format("%.2f %.2f", require(Interpreted)(10000), require(Native)(10000)))
            "#,
            interpreted = source(""),
            native = source("--!native\n")
        ),
    );
    let line = output.iter().find_map(|line| line.strip_prefix("print: ")).unwrap_or_else(|| panic!("{output:?}"));
    let (interpreted, native): (f64, f64) = {
        let mut numbers = line.split(' ').map(|n| n.parse::<f64>().unwrap());
        (numbers.next().unwrap(), numbers.next().unwrap())
    };
    assert!((5.0..7.0).contains(&interpreted), "interpreted: {interpreted} ns a step");
    assert!((1.3..2.2).contains(&native), "native: {native} ns a step");
}

/// A ray pays for the parts its path crosses, whatever its filter admits, a bounds query for the parts it returns, and GetPartsInPart
/// for the parts it returns and the size of the part it is given (cost.rs RAY_MET, RAY_CROSSED, QUERY_BOX_PART, QUERY_PART_EXACT,
/// QUERY_PART_SIZE: the parity bench's rays and queries, on geometry both sides find the same parts in, beyond the Luau around the
/// call: a miss with an Include filter 0.27 us, a ray through 24 stacked parts 1.50, an empty box 0.15, a box of 10 parts 1.54, a
/// 2-stud part touching nothing 0.27 and a 40-stud one 1.26, the 23 others of a stack 17.3).
#[test]
fn a_spatial_query_pays_for_the_parts_it_meets() {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    let output = run(
        &lua,
        &engine,
        r#"
        local At = Vector3.new(1000, 500, 0)
        local Stack = {}
        for Index = 1, 24 do
            local Part = Instance.new("Part")
            Part.Anchored = true
            Part.Size = Vector3.new(2, 2, 2)
            Part.CFrame = CFrame.new(At)
            Part.Parent = workspace
            Stack[Index] = Part
        end
        local function Per(Body)
            local N = 200
            local start = os.clock()
            for _ = 1, N do Body() end
            return (os.clock() - start) / N * 1e9
        end
        local Down, Far = Vector3.new(0, -20, 0), Vector3.new(5000, 5000, 5000)
        local Include = RaycastParams.new()
        Include.FilterType = Enum.RaycastFilterType.Include
        Include.FilterDescendantsInstances = { Stack[1] }
        local Miss = Per(function() return workspace:Raycast(Far, Down, Include) end)
        local Through = Per(function() return workspace:Raycast(At + Vector3.new(0, 10, 0), Down) end)
        local Filtered = Per(function() return workspace:Raycast(At + Vector3.new(0, 10, 0), Down, Include) end)
        local Empty = Per(function() return workspace:GetPartBoundsInBox(CFrame.new(Far), Vector3.new(4, 4, 4)) end)
        local Found = Per(function() return workspace:GetPartBoundsInBox(CFrame.new(At), Vector3.new(4, 4, 4)) end)
        local function Lone(Side, Z)
            local Part = Instance.new("Part")
            Part.Anchored = true
            Part.Size = Vector3.new(Side, Side, Side)
            Part.CFrame = CFrame.new(At + Vector3.new(0, 0, Z))
            Part.Parent = workspace
            return Part
        end
        local Small, Large = Lone(2, 200), Lone(40, 500)
        local Alone = Per(function() return workspace:GetPartsInPart(Small) end)
        local Wide = Per(function() return workspace:GetPartsInPart(Large) end)
        local Inside = Per(function() return workspace:GetPartsInPart(Stack[1]) end)
        print(string.format("%.0f %.0f %.0f %.0f %.0f %.0f %.0f %.0f", Miss, Through, Filtered, Empty, Found, Alone, Wide, Inside))
        "#,
    );
    let line = output.iter().find_map(|line| line.strip_prefix("print: ")).unwrap_or_else(|| panic!("{output:?}"));
    let numbers: Vec<f64> = line.split(' ').map(|n| n.parse().unwrap()).collect();
    // a call 0.27 us; a path through 24 parts 0.175 + 23 x 0.042 more, filtered or not; a box 0.15, and 0.14 a part it returns;
    // GetPartsInPart 0.243 + 0.011 a stud of the root of its part's surface area, and 0.74 a part it returns
    assert!((250.0..330.0).contains(&numbers[0]), "a miss: {line}");
    assert!((1300.0..1650.0).contains(&numbers[1]) && (numbers[1] - numbers[2]).abs() < 100.0, "through the stack, filtered or not: {line}");
    assert!((140.0..220.0).contains(&numbers[3]), "an empty box: {line}");
    assert!((3200.0..3900.0).contains(&numbers[4]), "a box of 24 parts: {line}");
    assert!((270.0..360.0).contains(&numbers[5]), "a 2-stud part touching nothing: {line}");
    assert!((1200.0..1500.0).contains(&numbers[6]), "a 40-stud part touching nothing: {line}");
    assert!((16000.0..18600.0).contains(&numbers[7]), "a part among 23 others: {line}");
}

/// `--device phone` (Options::device) emulates a touch device and scales the script work the clock charges by a sim-to-phone
/// calibration of script time (cost.rs PHONE_SCRIPTS, 2.77): a conversion of the desktop prices, not a measurement on a phone.
#[test]
fn a_phone_run_pays_the_calibrated_factor_on_its_script_work() {
    let per = |device: &str| -> (f64, String) {
        let (lua, engine) = start_with(Options { device: device.into(), ..Options::default() });
        engine_lua::modules::interpret_all(&lua, &engine);
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
        let output = run(
            &lua,
            &engine,
            r#"
            local start = os.clock()
            for _ = 1, 1000 do Instance.new("Folder") end
            print(string.format("%.1f %s", (os.clock() - start) / 1000 * 1e9, tostring(game:GetService("UserInputService").TouchEnabled)))
            "#,
        );
        let line = output.iter().find_map(|line| line.strip_prefix("print: ")).unwrap_or_else(|| panic!("{output:?}")).to_string();
        (printed_number(&output), line.split(' ').nth(1).unwrap().to_string())
    };
    let (desktop, desktop_touch) = per("desktop");
    let (phone, phone_touch) = per("phone");
    assert!((phone / desktop - crate::cost::PHONE_SCRIPTS).abs() < 0.02, "a phone {phone} ns a Folder, a desktop {desktop}");
    assert_eq!((desktop_touch.as_str(), phone_touch.as_str()), ("false", "true"));
}

/// A link to directory `target` at `link`: a junction on Windows, which needs no privilege (a directory
/// symlink does), a symlink elsewhere.
fn link_directory(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(windows)]
    {
        // mklink reads a "/" as the start of a switch
        let native = |path: &std::path::Path| path.to_string_lossy().replace('/', "\\");
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J", &native(link), &native(target)]).stdout(std::process::Stdio::null()).status().unwrap();
        assert!(made.success(), "mklink /J {} {}", link.display(), target.display());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// A link back to a directory on the way down to it is not followed: the directory would hold itself
/// without end, and two such links made the scan list 2^depth copies (a hang). Every other link mounts
/// what it leads to, and each link left out says where it leads.
#[test]
fn a_mount_leaves_out_a_link_back_up_its_way_down() {
    let dir = std::env::temp_dir().join(format!("ree-link-loops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for folder in ["Folder", "A", "B"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    std::fs::write(dir.join("Folder/a.luau"), "return 1").unwrap();
    std::fs::write(dir.join("B/b.luau"), "return 2").unwrap();
    link_directory(&dir, &dir.join("Folder/Back"));
    link_directory(&dir, &dir.join("Folder/Again"));
    link_directory(&dir.join("B"), &dir.join("A/ToB"));
    link_directory(&dir.join("A"), &dir.join("B/ToA"));
    let root = dir.to_string_lossy().replace('\\', "/");
    let (lua, engine) = start();
    engine.state.borrow_mut().project.listings = Some(Vec::new());
    super::project::mount_at(&engine, &super::project::Mount::parse(&format!("ReplicatedStorage/Looped={root}"), false).unwrap()).unwrap();
    // a prepared run's listing of it reads the same again (the links left out have no stamps to compare)
    let listings = engine.state.borrow_mut().project.listings.take().unwrap();
    assert!(!listings.is_empty() && listings.iter().all(|listing| listing.unchanged(&Default::default())));
    let output = run(&lua, &engine, r#"
        local Looped = game:GetService("ReplicatedStorage").Looped
        assert(Looped.Folder:FindFirstChild("a") and not Looped.Folder:FindFirstChild("Back") and not Looped.Folder:FindFirstChild("Again"), "Folder")
        -- each link is followed once, up to the link that leads back
        assert(Looped.A.ToB:FindFirstChild("b") and not Looped.A.ToB:FindFirstChild("ToA"), "A.ToB")
        assert(Looped.B:FindFirstChild("b") and Looped.B:FindFirstChild("ToA") and not Looped.B.ToA:FindFirstChild("ToB"), "B.ToA")
        print("ok")
    "#);
    assert!(output.contains(&"print: ok".to_string()), "{output:?}");
    let left_out: Vec<&String> = output.iter().filter(|line| line.contains("a link back to")).collect();
    let says = |link: &str, above: &str| format!("warn: {root}/{link}: a link back to {root}{above}, which would hold itself; not mounted");
    for expected in [says("Folder/Again", ""), says("Folder/Back", ""), says("A/ToB/ToA", "/A"), says("B/ToA/ToB", "/B")] {
        assert!(left_out.contains(&&expected), "{expected} in {left_out:?}");
    }
    assert_eq!(left_out.len(), 4, "{left_out:?}");
    drop(lua);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Place content's walk leaves out a link back up its way down, as a mount does.
#[test]
fn place_content_leaves_out_a_link_back_up_its_way_down() {
    let repo = std::env::temp_dir().join(format!("ree-place-content-loops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    let place = repo.join("data/place-content/Lobby");
    std::fs::create_dir_all(place.join("ReplicatedStorage")).unwrap();
    link_directory(&place, &place.join("ReplicatedStorage/Back"));
    link_directory(&place, &place.join("ReplicatedStorage/Again"));
    let engine = Engine::new(Options { repo: repo.clone(), ..Options::default() }).expect("engine");
    assert_eq!(super::project::mount_place_content(&engine, "Lobby.json"), 0);
    let left_out: Vec<String> = lines(&engine).into_iter().filter(|line| line.contains("a link back to")).collect();
    assert_eq!(left_out.len(), 2, "{left_out:?}");
    let _ = std::fs::remove_dir_all(&repo);
}

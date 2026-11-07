//! The server run context (`--run-context server`, lua/server.luau) through the script API: RunService's
//! answers and no local player, the in-memory DataStoreService, MemoryStoreService and MessagingService,
//! Player:Kick and the server side of remotes; EncodingService, and a client that keeps its stubs.

use std::rc::Rc;

use mlua::prelude::*;

use super::{
    Engine, Options, RunContext,
    lua::{self as engine_lua, globals::CallOutcome},
    scheduler,
};

fn start(run_context: RunContext) -> (Lua, Rc<Engine>) {
    let options = match run_context {
        RunContext::Server => Options { mode: "play".into(), run_context, local_player_name: None, ..Options::default() },
        RunContext::Client => Options { mode: "play".into(), ..Options::default() },
    };
    let engine = Engine::new(options).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    (lua, engine)
}

/// Runs `source` as a script of a fresh engine, then its clock to `until`: the output lines ("kind: message"),
/// none of which may be an error.
fn run(run_context: RunContext, source: &str, until: f64) -> Vec<String> {
    let (lua, engine) = start(run_context);
    run_in(&lua, &engine, source, until)
}

fn run_in(lua: &Lua, engine: &Rc<Engine>, source: &str, until: f64) -> Vec<String> {
    run_with(lua, engine, source, until, &[])
}

/// [`run_in`] with `globals` set in the script's environment.
fn run_with(lua: &Lua, engine: &Rc<Engine>, source: &str, until: f64, globals: &[(&str, LuaValue)]) -> Vec<String> {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        super::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    for (name, value) in globals {
        env.set(*name, value.clone()).expect("global");
    }
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    let now = engine.state.borrow().sched.now;
    scheduler::run_until(lua, engine, now + until).unwrap();
    let lines: Vec<String> = engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect();
    let errors: Vec<&String> = lines.iter().filter(|line| line.starts_with("error")).collect();
    assert!(errors.is_empty(), "{errors:?}");
    lines
}

fn has(lines: &[String], wanted: &str) -> bool {
    lines.iter().any(|line| line == wanted)
}

/// A printed line `label` then an error ending in `message` (after the script position, when it has one).
fn has_error(lines: &[String], label: &str, message: &str) -> bool {
    let start = format!("print: {label} ");
    lines.iter().any(|line| line.starts_with(&start) && line.ends_with(message))
}

#[test]
fn a_server_has_no_local_player_and_renders_nothing() {
    let lines = run(
        RunContext::Server,
        r#"
        local RunService = game:GetService("RunService")
        local Players = game:GetService("Players")
        assert(RunService:IsServer() and not RunService:IsClient() and RunService:IsStudio() and RunService:IsRunning())
        assert(not RunService:IsRunMode())
        -- (measured on a Studio 0.744 server, tools/parity/probes/run_service_sides.luau: a game script may not call IsEdit
        -- nor connect to RenderStepped; PreRender and BindToRenderStep are taken and never run)
        print("is-edit", pcall(RunService.IsEdit, RunService))
        print("render-stepped", pcall(function() RunService.RenderStepped:Connect(function() end) end))
        assert(Players.LocalPlayer == nil and #Players:GetPlayers() == 0)
        local Rendered, Bound, Beats = 0, 0, 0
        RunService.PreRender:Connect(function() Rendered += 1 end)
        RunService:BindToRenderStep("Test", 1, function() Bound += 1 end)
        RunService.Heartbeat:Connect(function() Beats += 1 end)
        Players.PlayerAdded:Connect(function(Player)
            print("added", Player.Name, Player.UserId)
            local Names = {}
            for _, Child in Player:GetChildren() do
                table.insert(Names, Child.ClassName)
            end
            table.sort(Names)
            print("containers", table.concat(Names, ","))
        end)
        Players.PlayerRemoving:Connect(function(Player) print("removing", Player.Name) end)
        local Player = Instance.new("Player")
        Player.Name = "Fake"
        Player.UserId = 7
        Player.Parent = Players
        task.wait(0.1)
        print("frames", Rendered, Bound, Beats > 3, #Players:GetPlayers())
        Player:Kick("bye")
        print("kicked", Player.Parent == Players)
        task.wait()
        print("left", Player.Parent)
        "#,
        0.5,
    );
    for wanted in [
        "print: is-edit false The current thread cannot call 'IsEdit' (lacking capability PluginOrOpenCloud)",
        "print: render-stepped false RenderStepped event can only be used from local scripts",
        "print: added Fake 7",
        "print: containers Backpack,PlayerGui,StarterGear",
        "print: frames 0 0 true 1",
        "print: kicked true",
        "print: removing Fake",
        "print: left nil",
    ] {
        assert!(has(&lines, wanted), "{wanted}: {lines:?}");
    }
}

#[test]
fn data_stores_keep_copies_versions_and_metadata() {
    let (lua, engine) = start(RunContext::Server);
    let lines = run_in(
        &lua,
        &engine,
        r#"
        local DataStoreService = game:GetService("DataStoreService")
        local Store = DataStoreService:GetDataStore("Profiles")
        assert(DataStoreService:GetDataStore("Profiles") == Store and Store.Name == "Profiles" and Store:IsA("GlobalDataStore"))
        assert(DataStoreService:GetRequestBudgetForRequestType(Enum.DataStoreRequestType.UpdateAsync) == 60)
        local Before = time()
        local Value = { Coins = 5, Items = { "a", "b" }, Blob = buffer.fromstring("zz") }
        local Options = Instance.new("DataStoreSetOptions")
        Options:SetMetadata({ Tag = "x" })
        local Version = Store:SetAsync("1", Value, { 1 }, Options)
        assert(time() > Before, "SetAsync yields a frame")
        Value.Coins = 6
        local Read, Info = Store:GetAsync("1")
        assert(Read.Coins == 5 and Read.Items[2] == "b" and buffer.tostring(Read.Blob) == "zz", "a copy of what was written")
        assert(Info:IsA("DataStoreKeyInfo") and Info.Version == Version and Info:GetMetadata().Tag == "x" and Info:GetUserIds()[1] == 1)
        local Updated, UpdatedInfo = Store:UpdateAsync("1", function(Old, OldInfo)
            assert(OldInfo.Version == Version)
            Old.Coins += 10
            return Old, { 2 }, { Tag = "y" }
        end)
        assert(Updated.Coins == 15 and UpdatedInfo.Version ~= Version and UpdatedInfo:GetUserIds()[1] == 2)
        assert(UpdatedInfo.CreatedTime == Info.CreatedTime and UpdatedInfo:GetMetadata().Tag == "y")
        assert(Store:UpdateAsync("1", function() return nil end) == nil and Store:GetAsync("1").Coins == 15, "a nil transform cancels")
        assert(Store:GetVersionAsync("1", Version).Coins == 5)
        print("increment", Store:IncrementAsync("n", 3), Store:IncrementAsync("n"))
        local Removed = Store:RemoveAsync("1")
        assert(Removed.Coins == 15 and Store:GetAsync("1") == nil)
        local Versions = Store:ListVersionsAsync("1"):GetCurrentPage()
        print("versions", #Versions, Versions[1].IsDeleted, Versions[3].IsDeleted)
        local Keys = Store:ListKeysAsync("", 0, "", true):GetCurrentPage()
        print("keys", #Keys, Keys[1].KeyName)
        for _, Bad in {
            function() Store:GetAsync("") end,
            function() Store:GetAsync(string.rep("k", 51)) end,
            function() Store:SetAsync("f", print) end,
            function() Store:SetAsync("t", { 1, x = 2 }) end,
            function() Store:SetAsync("big", string.rep("x", 4194304)) end,
            function() Store:UpdateAsync("y", function() task.wait() return 1 end) end,
        } do
            print("error", (select(2, pcall(Bad))))
        end
        "#,
        1.0,
    );
    for wanted in ["print: increment 3 4", "print: versions 3 false true", "print: keys 1 n"] {
        assert!(has(&lines, wanted), "{wanted}: {lines:?}");
    }
    for wanted in [
        "Test:33: 101: Key name can't be empty.",
        "Test:34: 102: Key name exceeds the 50 character limit.",
        "Test:35: 104: Cannot store function in data store. Data stores can only accept valid UTF-8 characters.",
        "Test:36: 104: Cannot store Array in data store. Data stores can only accept valid UTF-8 characters.",
        "Test:37: 105: Serialized value exceeds 4MB limit.",
        "UpdateAsync: the transform function cannot yield",
    ] {
        assert!(has(&lines, &format!("print: error {wanted}")), "{wanted}: {lines:?}");
    }
    // another run's stores start empty
    let other = run(RunContext::Server, r#"print("other", game:GetService("DataStoreService"):GetDataStore("Profiles"):GetAsync("n"))"#, 0.1);
    assert!(has(&other, "print: other nil"), "{other:?}");
}

#[test]
fn ordered_data_stores_page_by_value() {
    let lines = run(
        RunContext::Server,
        r#"
        local Board = game:GetService("DataStoreService"):GetOrderedDataStore("Board")
        for Index, Score in { 30, 10, 50, 20, 40 } do
            Board:SetAsync("p" .. Index, Score)
        end
        local Pages = Board:GetSortedAsync(false, 2)
        local First = Pages:GetCurrentPage()
        print("first", First[1].key, First[1].value, First[2].value, Pages.IsFinished)
        Pages:AdvanceToNextPageAsync()
        Pages:AdvanceToNextPageAsync()
        print("last", #Pages:GetCurrentPage(), Pages.IsFinished, (select(2, pcall(Pages.AdvanceToNextPageAsync, Pages))))
        print("ranged", #Board:GetSortedAsync(true, 100, 20, 40):GetCurrentPage())
        print("bad", (select(2, pcall(Board.GetSortedAsync, Board, true, 0))))
        "#,
        1.0,
    );
    for wanted in ["print: first p3 50 40 false", "print: last 1 true Test:11: No pages to advance to.", "print: ranged 3", "print: bad Test:13: 106: PageSize must be within a predefined range."] {
        assert!(has(&lines, wanted), "{wanted}: {lines:?}");
    }
}

#[test]
fn memory_stores_order_expire_and_hide_read_items() {
    let lines = run(
        RunContext::Server,
        r#"
        local MemoryStoreService = game:GetService("MemoryStoreService")
        local Map = MemoryStoreService:GetSortedMap("Ranks")
        print("overwrote", Map:SetAsync("b", 1, 60, 5), Map:SetAsync("b", 2, 60, 5))
        Map:SetAsync("a", 3, 60, 5)
        Map:SetAsync("c", 4, 60, "s")
        Map:SetAsync("d", 5, 60)
        Map:SetAsync("e", 6, 60, -1)
        local Order = {}
        for _, Item in Map:GetRangeAsync(Enum.SortDirection.Ascending, 10) do
            table.insert(Order, Item.key)
        end
        print("order", table.concat(Order, ","))
        local After = {}
        for _, Item in Map:GetRangeAsync(Enum.SortDirection.Descending, 2, { key = "a", sortKey = 5 }) do
            table.insert(After, Item.key)
        end
        print("after", table.concat(After, ","))
        print("update", Map:UpdateAsync("a", function(Value, Sort) return Value + 1, Sort end, 60))
        local Hash = MemoryStoreService:GetHashMap("Cache")
        Hash:SetAsync("k", { 1 }, 1)
        print("hash", Hash:GetAsync("k")[1])
        task.wait(1.05)
        print("expired", Hash:GetAsync("k"), Map:GetSizeAsync())
        local Queue = MemoryStoreService:GetQueue("Jobs", 0.5)
        Queue:AddAsync("low", 60)
        Queue:AddAsync("high", 60, 5)
        local Items, Id = Queue:ReadAsync(1)
        print("read", Items[1], Queue:GetSizeAsync(), Queue:GetSizeAsync(true))
        Queue:RemoveAsync(Id)
        print("removed", Queue:GetSizeAsync())
        print("hidden", Queue:ReadAsync(1)[1], #Queue:ReadAsync(1, false, 0))
        task.wait(0.6)
        print("visible", Queue:ReadAsync(1)[1])
        print("bad", (select(2, pcall(Map.SetAsync, Map, "x", 1, 4000000))))
        "#,
        3.0,
    );
    for wanted in [
        "print: overwrote false true",
        "print: order e,a,b,c,d",
        "print: after d,c",
        "print: update 4 5",
        "print: hash 1",
        "print: expired nil 5",
        "print: read high 2 1",
        "print: removed 1",
        "print: hidden low 0",
        "print: visible low",
        "print: bad Test:35: Code: InvalidExpirationTime, Error: The field 'expiration' time must be between 0 and 3,888,000.",
    ] {
        assert!(has(&lines, wanted), "{wanted}: {lines:?}");
    }
}

#[test]
fn messages_reach_subscribers_and_remotes_reach_no_client() {
    let lines = run(
        RunContext::Server,
        r#"
        local MessagingService = game:GetService("MessagingService")
        local Got = {}
        local Connection = MessagingService:SubscribeAsync("Topic", function(Message)
            table.insert(Got, `{Message.Data.N}:{type(Message.Sent)}`)
        end)
        MessagingService:PublishAsync("Topic", { N = 1 })
        task.wait()
        Connection:Disconnect()
        MessagingService:PublishAsync("Topic", { N = 2 })
        task.wait()
        print("messages", table.concat(Got, ","))
        local Players = game:GetService("Players")
        local Player = Instance.new("Player")
        Player.Parent = Players
        local Remote = Instance.new("RemoteEvent")
        Remote.OnServerEvent:Connect(function() end)
        Remote:FireClient(Player, 1)
        Remote:FireAllClients(2)
        Instance.new("UnreliableRemoteEvent"):FireAllClients(3)
        print("fire server", (select(2, pcall(Remote.FireServer, Remote))))
        print("fire client", (select(2, pcall(Remote.FireClient, Remote, workspace))))
        local Function = Instance.new("RemoteFunction")
        print("invoke server", (select(2, pcall(Function.InvokeServer, Function))))
        local Answered = false
        task.spawn(function()
            local Ok, Message = pcall(Function.InvokeClient, Function, Player, 1)
            Answered = true
            print("invoke client", Ok, Message)
        end)
        task.wait(0.5)
        print("waiting", Answered)
        Player.Parent = nil
        "#,
        1.0,
    );
    for wanted in [
        "print: messages 1:number",
        "print: fire server Test:21: FireServer can only be called from the client",
        "print: fire client Test:22: FireClient: player argument must be a Player object",
        "print: invoke server Test:24: InvokeServer can only be called from the client",
        "print: waiting false",
        "print: invoke client false InvokeClient: Player left the game before RemoteFunction returned",
    ] {
        assert!(has(&lines, wanted), "{wanted}: {lines:?}");
    }
}

/// A client keeps what it had: its local player, no data stores and no EncodingService (their members are not emulated
/// there, Output `missing`).
#[test]
fn a_client_keeps_its_stubs() {
    let lines = run(
        RunContext::Client,
        r#"
        local RunService = game:GetService("RunService")
        print("client", RunService:IsClient(), RunService:IsServer(), game.Players.LocalPlayer.Name, select('#', game:GetService("DataStoreService"):GetDataStore("X")))
        print("encoding", select('#', game:GetService("EncodingService"):Base64Encode(buffer.fromstring("hi!"))))
        "#,
        0.1,
    );
    assert!(has(&lines, "print: client true false Player 0") && has(&lines, "print: encoding 0"), "{lines:?}");
}

#[test]
fn a_server_compresses_and_encodes_buffers() {
    let lines = run(
        RunContext::Server,
        r#"
        local EncodingService = game:GetService("EncodingService")
        local Text = string.rep("abc", 1000)
        local Packed = EncodingService:CompressBuffer(buffer.fromstring(Text), Enum.CompressionAlgorithm.Zstd, 4)
        assert(buffer.len(Packed) < 100)
        assert(EncodingService:GetDecompressedBufferSize(Packed, Enum.CompressionAlgorithm.Zstd) == 3000)
        assert(buffer.tostring(EncodingService:DecompressBuffer(Packed, Enum.CompressionAlgorithm.Zstd)) == Text)
        assert(buffer.tostring(EncodingService:Base64Encode(buffer.fromstring("hi!"))) == "aGkh")
        assert(buffer.tostring(EncodingService:Base64Decode(buffer.fromstring("aGkh"))) == "hi!")
        print("bad", pcall(EncodingService.DecompressBuffer, EncodingService, buffer.fromstring("nope"), Enum.CompressionAlgorithm.Zstd))
        print("level", pcall(EncodingService.CompressBuffer, EncodingService, buffer.fromstring("x"), Enum.CompressionAlgorithm.Zstd, 23))
        print("size", EncodingService:GetDecompressedBufferSize(buffer.fromstring("nope"), Enum.CompressionAlgorithm.Zstd))
        "#,
        0.1,
    );
    assert!(has(&lines, "print: size nil"), "{lines:?}");
    assert!(has_error(&lines, "bad false", "DecompressBuffer: the data does not hold a valid decompressed size"), "{lines:?}");
    assert!(has_error(&lines, "level false", "CompressBuffer: compressionLevel must be an integer from -7 to 22"), "{lines:?}");
}

/// A server's output reaches its scripts: LogService.MessageOut (message, MessageType) for each line and ScriptContext.Error
/// (message, stack trace, nil) for a script error, deferred. MessageOut gets them after the frame's Heartbeat, an error with
/// its stack lines, on a playing client and a server alike (measured: tools/parity/probes/logservice.luau, run.mjs and
/// run.mjs --server; a server passes the script to ScriptContext.Error, here nil); a playing client's ScriptContext.Error is
/// not emulated.
#[test]
fn a_server_hears_its_output_through_log_service_and_script_context() {
    for run_context in [RunContext::Server, RunContext::Client] {
        let (lua, engine) = start(run_context);
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            super::instance::create_named(&mut state, &engine.reflection, class, "Test")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let source = r#"
            local Heard, Errors = {}, {}
            game:GetService("LogService").MessageOut:Connect(function(Message, Kind)
                table.insert(Heard, `{Kind.Name}={Message}`)
            end)
            game:GetService("ScriptContext").Error:Connect(function(Message, Trace, Script)
                table.insert(Errors, `{Message}|{Trace ~= ""}|{Script}`)
            end)
            print("one", 2)
            warn("careful")
            task.spawn(function() error("boom", 0) end)
            task.wait(0.05)
            print(`heard {table.concat(Heard, ";")}`)
            print(`errors {table.concat(Errors, ";")}`)
        "#;
        let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        assert!(!matches!(call.outcome().unwrap(), CallOutcome::Failed(..)));
        let now = engine.state.borrow().sched.now;
        scheduler::run_until(&lua, &engine, now + 0.2).unwrap();
        let lines: Vec<String> = engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect();
        if run_context == RunContext::Server {
            // (a server's error has its stack lines too: measured, run.mjs --server's logservice probes)
            let heard = "print: heard MessageOutput=one 2;MessageWarning=careful;MessageError=boom
    Stack Begin
    Script 'Test', Line 11
    Stack End";
            assert!(has(&lines, heard), "{lines:?}");
            assert!(has(&lines, "print: errors boom|true|nil"), "{lines:?}");
        } else {
            let heard = "print: heard MessageOutput=one 2;MessageWarning=careful;MessageError=boom\n    Stack Begin\n    Script 'Test', Line 11\n    Stack End";
            assert!(has(&lines, heard) && has(&lines, "print: errors "), "{lines:?}");
        }
    }
}

/// A server's LogService.MessageOut gets its lines as a playing client's does (measured in a server game script, Studio 0.744):
/// after the frame's Heartbeat handlers, not when the line is logged, three arguments, the third a table (a LogService
/// method's converted context, an empty one for print); a line logged in a MessageOut handler reaches no handler. Its
/// LogService methods log and throw as a client's.
#[test]
fn a_servers_message_out_is_queued_with_a_context_argument() {
    let (lua, engine) = start(RunContext::Server);
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        super::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
    let source = r##"
        local LogService = game:GetService("LogService")
        local Marks = {}
        LogService.MessageOut:Connect(function(...)
            local Message, Kind, Context = ...
            local Keys = {}
            for Key, Value in Context do
                table.insert(Keys, tostring(Key) .. "=" .. tostring(Value))
            end
            table.insert(Marks, Message .. ":" .. Kind.Name .. ":" .. select("#", ...) .. ":" .. table.concat(Keys, ";"))
            if Message == "outer" then
                LogService:Info("inner")
            end
        end)
        game:GetService("RunService").Heartbeat:Once(function()
            table.insert(Marks, "|hb")
        end)
        LogService:Info("i {a}", { a = 1 })
        table.insert(Marks, "after-info")
        print("p")
        local Ok, Thrown = pcall(LogService.Error, LogService, "e {a}", { a = 2 })
        table.insert(Marks, "after-error:" .. typeof(Thrown) .. ":" .. tostring(Thrown))
        LogService:Info("outer")
        task.wait(0.05)
        print("marks " .. table.concat(Marks, ","))
    "##;
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
    assert!(!matches!(call.outcome().unwrap(), CallOutcome::Failed(..)));
    let now = engine.state.borrow().sched.now;
    scheduler::run_until(&lua, &engine, now + 0.1).unwrap();
    let lines: Vec<String> = engine.state.borrow().output.entries.iter().map(|e| format!("{}: {}", e.kind, e.message)).collect();
    assert!(has(&lines, "print: marks after-info,after-error:table:e 2,|hb,i 1:MessageInfo:3:a=1,p:MessageOutput:3:,e 2:MessageError:3:a=2,outer:MessageInfo:3:"), "{lines:?}");
    // (the output keeps every line, the one no handler heard included)
    assert!(has(&lines, "info: i 1") && has(&lines, "error: e 2") && has(&lines, "info: inner"), "{lines:?}");
}

/// Offline a server makes the default R6 rig for Players:CreateHumanoidModelFromDescription(Async) with an R6 rig type (a
/// root part, a torso, a head and four limbs joined by the standard Motor6Ds, PrimaryPart the root), R15 a bare Model with
/// a Humanoid; InsertService:LoadAsset raises after a frame as a failed load does; Player:LoadCharacterAsync is a no-op.
#[test]
fn a_server_makes_r6_rigs_raises_on_load_asset_and_loads_no_character() {
    let lines = run(
        RunContext::Server,
        r#"
        local Players = game:GetService("Players")
        local Rig = Players:CreateHumanoidModelFromDescriptionAsync(Instance.new("HumanoidDescription"), Enum.HumanoidRigType.R6)
        local Names = {}
        for _, Child in Rig:GetChildren() do
            table.insert(Names, Child.Name)
        end
        table.sort(Names)
        print("rig", table.concat(Names, ","), Rig.PrimaryPart and Rig.PrimaryPart.Name, Rig.Humanoid.RigType.Name)
        local Neck = Rig.Torso.Neck
        print("neck", Neck.Part0.Name, Neck.Part1.Name, Rig.HumanoidRootPart.RootJoint.Part1.Name, #Rig.Torso:GetChildren())
        local Bare = Players:CreateHumanoidModelFromDescription(Instance.new("HumanoidDescription"), Enum.HumanoidRigType.R15)
        print("r15", #Bare:GetChildren(), Bare:FindFirstChildOfClass("Humanoid") ~= nil)
        print("cast", pcall(Players.CreateHumanoidModelFromDescription, Players, 5, Enum.HumanoidRigType.R6))
        local Before = time()
        local Ok, Message = pcall(game:GetService("InsertService").LoadAsset, game:GetService("InsertService"), 123)
        print("load", Ok, Message, time() > Before)
        local VersionBefore = time()
        local VersionOk, VersionMessage = pcall(game:GetService("InsertService").LoadAssetVersion, game:GetService("InsertService"), 456)
        print("version", VersionOk, VersionMessage, time() > VersionBefore)
        local AsyncBefore = time()
        local AsyncOk, AsyncMessage = pcall(game:GetService("AssetService").LoadAssetAsync, game:GetService("AssetService"), 789)
        print("async", AsyncOk, AsyncMessage, time() > AsyncBefore)
        local Player = Instance.new("Player")
        Player.Parent = Players
        print("character", select('#', Player:LoadCharacterAsync()), Player.Character)
        "#,
        0.2,
    );
    assert!(has(&lines, "print: rig Head,Humanoid,HumanoidRootPart,Left Arm,Left Leg,Right Arm,Right Leg,Torso HumanoidRootPart R6"), "{lines:?}");
    assert!(has(&lines, "print: neck Torso Head Torso 5"), "{lines:?}");
    assert!(has(&lines, "print: r15 1 true"), "{lines:?}");
    assert!(has_error(&lines, "cast false", "Unable to cast number to HumanoidDescription"), "{lines:?}");
    assert!(has(&lines, "print: load false InsertService:LoadAsset: asset 123 cannot be loaded offline true"), "{lines:?}");
    assert!(has(&lines, "print: version false InsertService:LoadAssetVersion: asset version 456 cannot be loaded offline true"), "{lines:?}");
    assert!(has(&lines, "print: async false AssetService:LoadAssetAsync: asset 789 cannot be loaded offline true"), "{lines:?}");
    assert!(has(&lines, "print: character 0 nil"), "{lines:?}");
}

#[test]
fn a_live_server_is_not_studio_and_its_players_carry_join_data() {
    let options = Options { mode: "play".into(), run_context: RunContext::Server, local_player_name: None, studio: false, ..Options::default() };
    let engine = Engine::new(options).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    let set_join_data: LuaValue = lua.named_registry_value(crate::lua::stubs::SET_JOIN_DATA).expect("SetJoinData");
    let lines = run_with(
        &lua,
        &engine,
        r#"
        local RunService = game:GetService("RunService")
        local Players = game:GetService("Players")
        print("studio", RunService:IsStudio(), RunService:IsServer())
        local Host = Instance.new("Player")
        Host.UserId = 11
        local Plain = Instance.new("Player")
        local Given = { TeleportData = { Stage = "Stage1", MatchPlayers = { 11, 12 } }, Members = { 11, 12 } }
        SetJoinData(Host, Given)
        Given.TeleportData.Stage = "Changed"
        Host.Parent = Players
        Plain.Parent = Players
        local First, Second = Host:GetJoinData(), Host:GetJoinData()
        print("join", First.TeleportData.Stage, #First.TeleportData.MatchPlayers, First.Members[2], First ~= Second, next(Plain:GetJoinData()))
        print("bad", pcall(SetJoinData, Host, { Part = Instance.new("Part") }))
        print("not player", pcall(SetJoinData, workspace, {}))
        "#,
        0.1,
        &[("SetJoinData", set_join_data)],
    );
    assert!(has(&lines, "print: studio false true"), "{lines:?}");
    assert!(has(&lines, "print: join Stage1 2 12 true nil"), "{lines:?}");
    assert!(lines.iter().any(|line| line.starts_with("print: bad false") && line.contains("not Instance")), "{lines:?}");
    assert!(lines.iter().any(|line| line.starts_with("print: not player false") && line.contains("a Player expected")), "{lines:?}");
    let (studio_lua, studio_engine) = start(RunContext::Server);
    let studio = run_in(&studio_lua, &studio_engine, r#"print("studio", game:GetService("RunService"):IsStudio())"#, 0.0);
    assert!(has(&studio, "print: studio true"), "{studio:?}");
}

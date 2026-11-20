//! A loopback run's pair (src/loopback): a server and its client in one process. Each test runs a harness with Server and
//! Client sides through `record::run` as `ree run --run-context loopback` does, and reads both sides' output.

use std::path::{Path, PathBuf};

use crate::{
    launch::Host,
    record::run::{self, RecordArgs},
};

/// A scratch folder of its own for a test
fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ree-loopback-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `ree run --run-context loopback` of `harness` (and `extra` arguments) with the project `roots` (none: an empty one): its
/// summary
pub(crate) fn run_pair(name: &str, harness: &str, roots: Option<&Path>, extra: &[&str]) -> serde_json::Value {
    let dir = scratch(name);
    let file = dir.join(format!("{name}.luau"));
    std::fs::write(&file, harness).unwrap();
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let roots = roots.map(Path::to_path_buf).unwrap_or(empty);
    let mut words: Vec<String> = ["--harness", &file.to_string_lossy(), "--roots", &roots.to_string_lossy(), "--seconds", "2", "--run-context", "loopback"].iter().map(|s| s.to_string()).collect();
    words.extend(extra.iter().map(|s| s.to_string()));
    let args = run::parse(&words, false).unwrap();
    let summary = run::run_only(&args, &Host::default()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    summary
}

/// The output lines of a summary as `side kind: message`
pub(crate) fn lines(summary: &serde_json::Value) -> Vec<String> {
    summary["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| format!("{} {}: {}", line["side"].as_str().unwrap_or("-"), line["kind"].as_str().unwrap_or("?"), line["message"].as_str().unwrap_or("")))
        .collect()
}

fn has(lines: &[String], wanted: &str) -> bool {
    lines.iter().any(|line| line == wanted)
}

/// Each side answers RunService and Players as its own: the server has the joining player in Players and no LocalPlayer,
/// the client its LocalPlayer of the same name and UserId; PlayerAdded fires on the server for the join
#[test]
fn each_side_is_its_own_and_the_player_joins_the_server() {
    let summary = run_pair(
        "sides",
        r##"
local RunService = game:GetService("RunService")
local Players = game:GetService("Players")
return {
	Server = function(Context)
		Players.PlayerAdded:Connect(function(Player)
			print("added", Player.Name, Player.UserId, Player == Context.Player)
		end)
		print("server", Context.Side, RunService:IsServer(), RunService:IsClient(), Players.LocalPlayer, #Players:GetPlayers())
		task.wait()
		print("players", #Players:GetPlayers(), Players:GetPlayers()[1].Name)
	end,
	Client = function(Context)
		print("client", Context.Side, RunService:IsServer(), RunService:IsClient(), Players.LocalPlayer.Name, Players.LocalPlayer.UserId)
		task.wait(0.1)
		print("@done")
	end,
}
"##,
        None,
        &["--player", "Tester"],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    assert!(has(&lines, "server print: server server true false nil 0"), "{lines:?}");
    assert!(has(&lines, "server print: added Tester 1 true"), "{lines:?}");
    assert!(has(&lines, "server print: players 1 Tester"), "{lines:?}");
    assert!(has(&lines, "client print: client client false true Tester 1"), "{lines:?}");
    assert_eq!(summary["runContext"], "loopback");
}

/// Both sides of a pair refuse the fast-flag methods to a game script (reflection.rs CAPABILITY_CALLS), so a scheduler that
/// reads its frame budget through a define-under-pcall helper takes its default on each side, where the define answered
/// nil and the budget then divided by the nil the getter read
#[test]
fn each_side_refuses_the_fast_flag_methods_and_a_scheduler_takes_its_default() {
    let summary = run_pair(
        "fast-flags",
        r##"
local function IntGetter(Name, Default)
	local Ok, Problem = pcall(function()
		return game:DefineFastInt(Name, Default)
	end)
	if Ok then
		return function()
			return game:GetFastInt(Name)
		end
	end
	print("define", Problem)
	return function()
		return Default
	end
end
local function Budget(Side)
	local TargetFps = IntGetter("TestTargetFps", 50)
	print(Side, "budget", 1000 / TargetFps(), select(2, pcall(game.GetFastInt, game, "TestTargetFps")))
end
return {
	Server = function(Context)
		Budget("server")
	end,
	Client = function(Context)
		Budget("client")
		task.wait()
		print("@done")
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    for side in ["server", "client"] {
        assert!(has(&lines, &format!("{side} print: define The current thread cannot call 'DefineFastInt' (lacking capability RobloxScript)")), "{lines:?}");
        assert!(has(&lines, &format!("{side} print: {side} budget 20 The current thread cannot call 'GetFastInt' (lacking capability LocalUser)")), "{lines:?}");
    }
}

/// A loopback run refuses edit mode and a harness of another shape; `--latency` is a loopback run's
#[test]
fn launch_refuses_what_a_loopback_run_cannot_be() {
    let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(run::parse(&words(&["--harness", "x.luau", "--latency", "2"]), false).unwrap_err().contains("loopback run's"));
    assert!(run::parse(&words(&["--harness", "x.luau", "--run-context", "loopback", "--latency", "0"]), false).unwrap_err().contains("1 or more"));
    assert!(run::parse(&words(&["--harness", "x.luau", "--record-side", "server"]), true).is_err());
    let args: RecordArgs = run::parse(&words(&["--run-context", "loopback", "--mode", "edit"]), false).unwrap();
    assert!(run::run_only(&args, &Host::default()).unwrap_err().contains("needs --mode play"));
    let summary = run_pair("shape", "return function(Context) print('one side') end", None, &[]);
    assert_eq!(summary["status"], "error");
    assert!(summary["failure"].as_str().unwrap_or_default().contains("a loopback harness returns { Server"), "{summary}");
}

/// A frame is the server's, then the client's, with the same dt; a run ends when either side prints `@done`, and both
/// sides' lines come in one list in the order written, each with its side
#[test]
fn the_pair_steps_together_and_either_side_ends_the_run() {
    let summary = run_pair(
        "steps",
        r##"
local RunService = game:GetService("RunService")
return {
	Server = function(Context)
		local Frames = 0
		RunService.Heartbeat:Connect(function(Delta)
			Frames += 1
			print("server beat", Frames, string.format("%.4f", Delta))
			if Frames == 3 then
				print("@done")
			end
		end)
	end,
	Client = function(Context)
		local Frames = 0
		RunService.Heartbeat:Connect(function(Delta)
			Frames += 1
			print("client beat", Frames, string.format("%.4f", Delta))
		end)
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    let beats: Vec<&String> = lines.iter().filter(|line| line.contains(" beat ")).collect();
    assert_eq!(
        beats,
        [
            "server print: server beat 1 0.0167",
            "client print: client beat 1 0.0167",
            "server print: server beat 2 0.0167",
            "client print: client beat 2 0.0167",
            "server print: server beat 3 0.0167",
            "client print: client beat 3 0.0167",
        ],
        "{lines:?}"
    );
    assert_eq!(summary["done"], true);
    assert_eq!(summary["frames"], 4, "frame 0 and three frames");
}

/// A project with a shared module in ReplicatedStorage, a secret in ServerStorage and a server script: the join copies
/// what replicates, keeps what does not, and the harness's own RURFixtures stays each side's
fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    for (path, text) in [
        ("ReplicatedStorage/Shared/Counter.luau", "local Count = 0 return { Bump = function() Count += 1 return Count end }"),
        ("ServerStorage/Secret.luau", "return 'secret'"),
        ("ServerScriptService/Boot.server.luau", "print('server boot')"),
        ("Workspace/Spot.model.json", "{\"className\": \"Part\", \"properties\": {\"Anchored\": true}}"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    root
}

/// The join: the client has a copy of the replicated containers (a project module it requires, a rename and an attribute
/// the server's harness made before the join), nothing of ServerStorage or ServerScriptService, and its own RURFixtures;
/// each side requires its own copy of a shared module
#[test]
fn the_join_copies_the_replicated_containers() {
    let root = project("join");
    let summary = run_pair(
        "join",
        r##"
local Storage = game:GetService("ReplicatedStorage")
return {
	Server = function(Context)
		local Counter = require(Storage.Shared.Counter)
		print("server bumps", Counter.Bump(), Counter.Bump())
		workspace.Spot.Name = "Renamed"
		workspace.Renamed:SetAttribute("Mark", 7)
		local Folder = Instance.new("Folder")
		Folder.Name = "MadeBeforeJoin"
		Folder.Parent = Storage
	end,
	Client = function(Context)
		local Counter = require(Storage.Shared.Counter)
		print("client bumps", Counter.Bump())
		print("renamed", workspace:FindFirstChild("Renamed") ~= nil, workspace.Renamed:GetAttribute("Mark"), workspace.Renamed.Anchored)
		print("made", Storage:FindFirstChild("MadeBeforeJoin") ~= nil)
		print("server storage", #game:GetService("ServerStorage"):GetChildren(), "scripts", #game:GetService("ServerScriptService"):GetChildren())
		print("fixtures", #Storage.RURFixtures:GetChildren(), Storage.RURFixtures:GetChildren()[1].Name)
		print("@done")
	end,
}
"##,
        Some(&root),
        &[],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    assert!(has(&lines, "server print: server bumps 1 2"), "{lines:?}");
    assert!(has(&lines, "client print: client bumps 1"), "{lines:?}");
    assert!(has(&lines, "client print: renamed true 7 true"), "{lines:?}");
    assert!(has(&lines, "client print: made true"), "{lines:?}");
    assert!(has(&lines, "client print: server storage 0 scripts 0"), "{lines:?}");
    assert!(has(&lines, "client print: fixtures 1 join"), "{lines:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A host that makes its own client (rur's stories): the server started alone, the host's client readied, started with
/// no project and linked; `begin_server` starts the server's part of the harness, which holds the join for a few frames,
/// and the player joins once it lets it in. An error the server's harness raises at once is returned.
#[test]
fn a_host_links_its_own_client_and_the_servers_harness_holds_the_join() {
    use crate::{Emulator, Engine, Options, RunContext, loopback, mount::ProjectSpec};
    let dir = scratch("host");
    let harness = dir.join("Host.luau");
    std::fs::write(&harness, "return { Server = function(Context) Context.HoldJoin() print('server holds') task.wait(0.05) Context.Join() print('server lets in') end }").unwrap();
    let failing = dir.join("Failing.luau");
    std::fs::write(&failing, "return { Server = function(Context) error('the server harness failed') end }").unwrap();
    let options = Options { mode: "play".into(), run_context: RunContext::Loopback, ..Options::default() };
    let pair = |harness: &Path| {
        let server = loopback::start_server(&Engine::new, &options, &ProjectSpec::default(), &|_| {}).unwrap();
        let seed = server.engine.state.borrow().sched.random_seed;
        let engine = Engine::new(loopback::side_options(&options, loopback::Side::Client)).unwrap();
        loopback::ready_client(&engine);
        assert_eq!(engine.state.borrow().sched.random_seed, seed + 1.0, "the client's own sequence");
        let client = Emulator::start(engine, &ProjectSpec::default()).unwrap();
        loopback::link(server, &client.lua, &client.engine, loopback::Spec::default()).unwrap();
        let begun = run::begin_server(&client.lua, &client.engine, Some(harness), 1.0 / 60.0, 600);
        (client, begun)
    };
    let (client, begun) = pair(&harness);
    begun.unwrap();
    let bridge = client.engine.loopback().unwrap().bridge.clone();
    assert!(bridge.joined.get(), "the player joined");
    assert!(bridge.joined_at.get() >= 3, "the server stepped alone until it let the player in (joined at {})", bridge.joined_at.get());
    let printed: Vec<String> = client.engine.state.borrow().output.entries.iter().map(|entry| entry.message.clone()).collect();
    assert!(printed.iter().any(|line| line == "server holds") && printed.iter().any(|line| line == "server lets in"), "{printed:?}");
    let (_failed, begun) = pair(&failing);
    assert!(begun.is_err_and(|message| message.contains("the server harness failed")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A module both sides require is compiled once in the run: the second side loads the bytecode the first one's compile
/// made (each side still runs its own copy of it)
#[test]
fn a_shared_module_compiles_once() {
    let root = project("compile");
    let dir = scratch("compile-run");
    let harness = dir.join("Compile.luau");
    std::fs::write(&harness, "local Storage = game:GetService('ReplicatedStorage') return { Server = function() print('server', require(Storage.Shared.Counter).Bump()) end, Client = function() print('client', require(Storage.Shared.Counter).Bump()) print('@done') end }").unwrap();
    let words: Vec<String> = ["--harness", &harness.to_string_lossy(), "--roots", &root.to_string_lossy(), "--seconds", "1", "--run-context", "loopback"].iter().map(|s| s.to_string()).collect();
    let args = run::parse(&words, false).unwrap();
    let (done, emulator) = run::execute(&args, &Host::default(), false).unwrap();
    let lines = lines(&done.summary);
    assert!(has(&lines, "server print: server 1") && has(&lines, "client print: client 1"), "{lines:?}");
    let (_, server) = emulator.engine.peer_server().unwrap();
    assert_eq!(emulator.engine.modules.borrow().text_compiles, 0, "the client compiled a module's text");
    assert_eq!(server.modules.borrow().text_compiles, 0, "the server's require loaded the process's bytecode too");
    drop(server);
    drop(emulator);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project whose place has a Studio hierarchy snapshot: Studio-only placeholders below ReplicatedStorage and the Workspace
/// (a Folder of remotes, an asset, a map), and one module on disk. Returns the root to mount.
fn studio_project(name: &str) -> PathBuf {
    let dir = scratch(name);
    let root = dir.join("place");
    std::fs::create_dir_all(root.join("ReplicatedStorage")).unwrap();
    std::fs::write(root.join("ReplicatedStorage/Note.luau"), "return 'note'").unwrap();
    std::fs::create_dir_all(dir.join(crate::project::SNAPSHOTS_DIR)).unwrap();
    std::fs::write(dir.join(crate::project::PLACES_FILE), r#"{"environments":[{"name":"Lobby","projectFile":"place/default.project.json"}]}"#).unwrap();
    let node = |name: &str, class: &str, parent: i64| format!(r#"{{"name":"{name}","className":"{class}","parent":{parent}}}"#);
    let nodes = [
        node("game", "DataModel", -1),
        node("ReplicatedStorage", "ReplicatedStorage", 0),
        node("Workspace", "Workspace", 0),
        node("Remotes", "Folder", 1),
        node("Ping", "RemoteEvent", 3),
        node("Assets", "Folder", 1),
        node("Sword", "Model", 5),
        node("Handle", "Part", 6),
        node("Map", "Model", 2),
        node("Door", "Part", 8),
        node("Lamp", "Part", 8),
        node("Bulb", "Part", 10),
        node("Props", "Folder", 1),
        node("Crate", "Part", 12),
        node("Lid", "Part", 13),
    ];
    std::fs::write(dir.join(crate::project::SNAPSHOTS_DIR).join("Lobby.json"), format!(r#"{{"nodes":[{}]}}"#, nodes.join(","))).unwrap();
    root
}

/// The place's Studio-only instances stay lazy at the join: the client makes those its scripts look at itself, as a plain
/// client run does, and the server's own (made after the join, when its scripts look) pair with the client's when a
/// message names them: a remote both sides found, a part the server passes and writes, a part the client passes back
/// that the server had not made. The server never makes what only the client looked at.
#[test]
fn studio_only_instances_stay_lazy_and_pair_as_they_are_named() {
    let root = studio_project("studio");
    let dir = scratch("studio-run");
    let harness = dir.join("Studio.luau");
    std::fs::write(
        &harness,
        r##"
local Storage = game:GetService("ReplicatedStorage")
return {
	Server = function(Context)
		task.wait(0.1)
		local Ping = Storage.Remotes.Ping
		Ping.OnServerEvent:Connect(function(Player, Part, Same)
			print("server got", Part and Part:GetFullName(), Part == Storage.Props.Crate.Lid, Same == Ping)
		end)
		local Crate = Storage.Props.Crate
		Crate.Transparency = 0.5
		Crate:SetAttribute("Open", true)
		Ping:FireAllClients(Crate, Ping)
	end,
	Client = function(Context)
		local Map = workspace:WaitForChild("Map")
		local Names = {}
		for _, Part in Map:GetDescendants() do
			table.insert(Names, Part.Name)
		end
		table.sort(Names)
		print("client map", table.concat(Names, " "))
		local Sword = Storage.Assets.Sword
		print("client sword", Sword.Handle.ClassName)
		local Ping = Storage.Remotes.Ping
		Ping.OnClientEvent:Connect(function(Crate, Same)
			task.wait()
			print("client got", Crate == Storage.Props.Crate, Crate.Transparency, Crate:GetAttribute("Open"), Same == Ping)
			Ping:FireServer(Crate.Lid, Ping)
		end)
	end,
}
"##,
    )
    .unwrap();
    let words: Vec<String> = ["--harness", &harness.to_string_lossy(), "--roots", &root.to_string_lossy(), "--seconds", "1", "--run-context", "loopback"].iter().map(|s| s.to_string()).collect();
    let args = run::parse(&words, false).unwrap();
    let (done, emulator) = run::execute(&args, &Host::default(), false).unwrap();
    let lines = lines(&done.summary);
    assert!(has(&lines, "client print: client map Bulb Door Lamp"), "{lines:?}");
    assert!(has(&lines, "client print: client sword Part"), "{lines:?}");
    assert!(has(&lines, "client print: client got true 0.5 true true"), "{lines:?}");
    assert!(has(&lines, "server print: server got ReplicatedStorage.Props.Crate.Lid true true"), "{lines:?}");
    // the server never made the asset only the client looked at; both sides made Studio-only instances after the join
    let (_, server) = emulator.engine.peer_server().unwrap();
    assert!(server.loopback().unwrap().lazy.borrow().made() >= 3, "the server made its remote, the crate and its lid after the join");
    assert!(emulator.engine.loopback().unwrap().lazy.borrow().made() >= 5, "the client made its own");
    {
        let state = server.state.borrow();
        let storage = crate::services::find_service(&server, server.world().game, "ReplicatedStorage").unwrap();
        let sword = state.dom.find_child(storage, b"Assets").and_then(|assets| state.dom.find_child(assets, b"Sword"));
        assert!(sword.is_none_or(|sword| state.dom.find_child(sword, b"Handle").is_none()), "the server made the client's asset");
    }
    drop(server);
    drop(emulator);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

/// After the join every change the server makes in the replicated containers arrives at the client's next frame, as its
/// value when the server's frame ended: three writes give one Changed with the last, a made child arrives whole, a move
/// out of the containers leaves the client's copy unparented (the same instance comes back), a destroy destroys it; a
/// write to ServerStorage sends nothing, and a client's own write stays until the server writes the property again
#[test]
fn the_servers_changes_arrive_at_the_clients_next_frame() {
    let summary = run_pair(
        "changes",
        r##"
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
local function Frames(Count)
	for _ = 1, Count do
		RunService.Heartbeat:Wait()
	end
end
return {
	Server = function(Context)
		local Value = Instance.new("IntValue")
		Value.Name = "Value"
		Value.Parent = Storage
		local Hidden = Instance.new("IntValue")
		Hidden.Parent = game:GetService("ServerStorage")
		Frames(3)
		Value.Value = 1
		Value.Value = 2
		Value.Value = 3
		Hidden.Value = 9
		Frames(3)
		local Child = Instance.new("IntValue")
		Child.Name = "Child"
		Child.Parent = Value
		Child.Value = 5
		Frames(3)
		Value.Parent = game:GetService("ServerStorage")
		Frames(3)
		Value.Parent = Storage
		Frames(3)
		Value.Value = 4
		Frames(3)
		Value:Destroy()
	end,
	Client = function(Context)
		local Value = Storage:WaitForChild("Value")
		local Seen = {}
		Value.Changed:Connect(function(New)
			table.insert(Seen, "Changed " .. New)
		end)
		Value.ChildAdded:Connect(function(Child)
			table.insert(Seen, "ChildAdded " .. Child.Name .. " " .. Child.Value)
		end)
		Value.AncestryChanged:Connect(function(_, Parent)
			table.insert(Seen, "Parent " .. tostring(Parent))
		end)
		Value.Destroying:Connect(function()
			table.insert(Seen, "Destroying")
		end)
		Value.Value = 100
		Frames(20)
		print("seen", table.concat(Seen, " / "))
		print("gone", Storage:FindFirstChild("Value") == nil, Value.Parent)
		print("@done")
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    assert!(
        has(&lines, "client print: seen Changed 100 / Changed 3 / ChildAdded Child 5 / Parent nil / Parent ReplicatedStorage / Changed 4 / Destroying / Parent nil"),
        "{lines:?}"
    );
    assert!(has(&lines, "client print: gone true nil"), "{lines:?}");
}

/// Remote events each way: OnServerEvent gets the player first; the arguments arrive as the call left them (a table, a
/// buffer, an instance the client has, nil for one it lacks); the wrong side's calls raise with the measured texts;
/// events that reach a remote nothing listens to wait for its first connection
#[test]
fn remote_events_cross_each_way() {
    let summary = run_pair(
        "remotes",
        r##"
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
return {
	Server = function(Context)
		local Remote = Instance.new("RemoteEvent")
		Remote.Name = "Remote"
		Remote.Parent = Storage
		local Late = Instance.new("RemoteEvent")
		Late.Name = "Late"
		Late.Parent = Storage
		local Shared = Instance.new("Part")
		Shared.Name = "Shared"
		Shared.Parent = workspace
		local Hidden = Instance.new("Part")
		Hidden.Parent = game:GetService("ServerStorage")
		Remote.OnServerEvent:Connect(function(Player, Text, Table)
			print("server got", Player.Name, Text, Table.a, Table[1])
			local Data = { 1, 2, x = 3 }
			Remote:FireClient(Player, Data, buffer.fromstring("ab"), Shared, Hidden, nil)
			Data[1] = 9
		end)
		print("server FireServer", pcall(Remote.FireServer, Remote))
		-- (before the join nobody is there to get it; after it the client's Late waits for a connection)
		Late:FireAllClients(0)
		task.wait()
		for Index = 1, 3 do
			Late:FireAllClients(Index)
		end
	end,
	Client = function(Context)
		local Remote = Storage:WaitForChild("Remote")
		Remote.OnClientEvent:Connect(function(...)
			local Data, Buffer, Shared, Hidden = ...
			print("client got", Data[1], Data[2], Data.x, buffer.tostring(Buffer), Shared and Shared.Name, Hidden, select("#", ...))
		end)
		print("client FireClient", pcall(Remote.FireClient, Remote, game.Players.LocalPlayer))
		print("client connect", pcall(function() Remote.OnServerEvent:Connect(print) end))
		Remote:FireServer("hello", { a = 1 })
		for _ = 1, 10 do
			RunService.Heartbeat:Wait()
		end
		local Got = {}
		Storage.Late.OnClientEvent:Connect(function(Index)
			table.insert(Got, Index)
		end)
		Storage.Late.OnClientEvent:Connect(function(Index)
			table.insert(Got, "second " .. Index)
		end)
		RunService.Heartbeat:Wait()
		print("late", table.concat(Got, " "))
		print("@done")
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    assert!(has(&lines, "server print: server FireServer false FireServer can only be called from the client"), "{lines:?}");
    assert!(has(&lines, "client print: client FireClient false FireClient can only be called from the server"), "{lines:?}");
    assert!(has(&lines, "client print: client connect false OnServerEvent can only be used on the server"), "{lines:?}");
    assert!(has(&lines, "server print: server got Player hello 1 nil"), "{lines:?}");
    assert!(has(&lines, "client print: client got 1 2 nil ab Shared nil 5"), "{lines:?}");
    assert!(has(&lines, "client print: late 1 2 3"), "{lines:?}");
}

/// RemoteFunction both ways: results and nils kept, the callback's error raised in the caller with its position, a call
/// that waits for the callback to be set; when the player leaves the server's InvokeClient calls raise, and no call is
/// left held by either side
#[test]
fn remote_functions_answer_and_nothing_is_left_waiting() {
    let summary = run_pair(
        "invokes",
        r##"
local Storage = game:GetService("ReplicatedStorage")
return {
	Server = function(Context)
		local Function = Instance.new("RemoteFunction")
		Function.Name = "Function"
		Function.OnServerInvoke = function(Player, Kind)
			if Kind == "error" then
				error("server failed")
			end
			task.wait(0.05)
			return Kind, nil, Player.Name
		end
		Function.Parent = Storage
		local Later = Instance.new("RemoteFunction")
		Later.Name = "Later"
		Later.Parent = Storage
		task.delay(0.2, function()
			Later.OnServerInvoke = function(_, Value)
				return Value * 2
			end
		end)
		local Stuck = Instance.new("RemoteFunction")
		Stuck.Name = "Stuck"
		Stuck.Parent = Storage
		task.wait(0.4)
		task.spawn(function()
			print("stuck", pcall(Stuck.InvokeClient, Stuck, Context.Player))
		end)
		task.wait(0.1)
		Context.Leave()
		task.wait(0.1)
		print("@done")
	end,
	Client = function(Context)
		local Function = Storage:WaitForChild("Function")
		Storage:WaitForChild("Stuck").OnClientInvoke = function()
			task.wait(100)
		end
		print("ok", select("#", Function:InvokeServer("plain")), Function:InvokeServer("plain"))
		local Ok, Message = pcall(Function.InvokeServer, Function, "error")
		print("error", Ok, (string.gsub(Message, "^.-:%d+: ", "<position>: ")))
		print("later", Storage:WaitForChild("Later"):InvokeServer(21))
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    assert!(has(&lines, "client print: ok 3 plain nil Player"), "{lines:?}");
    assert!(has(&lines, "client print: error false <position>: server failed"), "{lines:?}");
    assert!(has(&lines, "client print: later 42"), "{lines:?}");
    assert!(has(&lines, "server print: stuck false InvokeClient: Player left the game before RemoteFunction returned"), "{lines:?}");
}


/// With no harness a loopback run boots both sides from the place's own scripts: the server's Scripts (Legacy under
/// ServerScriptService and Workspace, Server-context anywhere but ServerStorage), the client's LocalScripts (ReplicatedFirst,
/// StarterPlayerScripts' copies in PlayerScripts) and Client-context Scripts; each once per side, a shared module once per
/// side, and a script on the wrong side never
#[test]
fn a_place_boots_both_sides_from_its_own_scripts() {
    let root = scratch("boot");
    for (path, text) in [
        ("ReplicatedStorage/Shared/Counter.luau", "local Count = 0 return { Bump = function() Count += 1 return Count end }"),
        ("ServerScriptService/Main.server.luau", "local Counter = require(game:GetService('ReplicatedStorage').Shared.Counter) print('server main', game:GetService('RunService'):IsServer(), Counter.Bump())"),
        ("Workspace/Spinner.server.luau", "print('workspace script', game:GetService('RunService'):IsServer())"),
        ("ServerStorage/Stored.server.luau", "print('WRONG: a ServerStorage script ran')"),
        ("ReplicatedStorage/Stray.client.luau", "print('WRONG: a LocalScript in ReplicatedStorage ran')"),
        ("ReplicatedFirst/First.client.luau", "print('client first', game:GetService('RunService'):IsClient())"),
        ("StarterPlayer/StarterPlayerScripts/Main.client.luau", "local Counter = require(game:GetService('ReplicatedStorage').Shared.Counter) print('client main', Counter.Bump(), script.Parent.ClassName) task.wait(0.1) print('@done')"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let dir = scratch("boot-run");
    let words: Vec<String> = ["--roots", &root.to_string_lossy(), "--seconds", "1", "--run-context", "loopback"].iter().map(|s| s.to_string()).collect();
    let args = run::parse(&words, false).unwrap();
    let summary = run::run_only(&args, &Host::default()).unwrap();
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    for wanted in ["server print: server main true 1", "server print: workspace script true", "client print: client first true", "client print: client main 1 PlayerScripts"] {
        assert_eq!(lines.iter().filter(|line| *line == wanted).count(), 1, "{wanted}: {lines:?}");
    }
    assert!(!lines.iter().any(|line| line.contains("WRONG")), "{lines:?}");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--latency 2`: what one side sends reaches the other a frame later than by default (an echo from the other side takes
/// two frames more in all); the default is the next frame (measured: an echo comes back at the sender's next frame start)
#[test]
fn latency_delays_each_way_by_its_frames() {
    let harness = r##"
local Storage = game:GetService("ReplicatedStorage")
local RunService = game:GetService("RunService")
return {
	Server = function(Context)
		local Echo = Instance.new("RemoteEvent")
		Echo.Name = "Echo"
		Echo.Parent = Storage
		Echo.OnServerEvent:Connect(function(Player, Index)
			Echo:FireClient(Player, Index)
		end)
	end,
	Client = function(Context)
		local Echo = Storage:WaitForChild("Echo")
		local Frame = 0
		RunService.PreSimulation:Connect(function()
			Frame += 1
		end)
		local Sent
		Echo.OnClientEvent:Connect(function()
			print("round trip", Frame - Sent)
			print("@done")
		end)
		RunService.Heartbeat:Once(function()
			Sent = Frame
			Echo:FireServer(1)
		end)
	end,
}
"##;
    let plain = lines(&run_pair("latency1", harness, None, &[]));
    assert!(has(&plain, "client print: round trip 0"), "{plain:?}");
    let slow = lines(&run_pair("latency2", harness, None, &["--latency", "2"]));
    assert!(has(&slow, "client print: round trip 2"), "{slow:?}");
}

/// Context.HoldJoin keeps the player out until the server's harness calls Context.Join: the server steps alone meanwhile,
/// and the client's harness starts at the join, its clock from 0 then (a client's DataModel starts after the server's)
#[test]
fn a_held_join_waits_for_the_servers_harness() {
    let summary = run_pair(
        "held",
        r##"
local RunService = game:GetService("RunService")
local Players = game:GetService("Players")
return {
	Server = function(Context)
		Context.HoldJoin()
		for _ = 1, 5 do
			RunService.Heartbeat:Wait()
		end
		print("players before the join", #Players:GetPlayers(), math.floor(time() * 60 + 0.5))
		Context.Join()
		RunService.Heartbeat:Wait()
		RunService.Heartbeat:Wait()
		print("players after", #Players:GetPlayers())
	end,
	Client = function(Context)
		print("client starts at", math.floor(time() * 60 + 0.5))
		task.wait(0.1)
		print("@done")
	end,
}
"##,
        None,
        &[],
    );
    let lines = lines(&summary);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    assert!(has(&lines, "server print: players before the join 0 5"), "{lines:?}");
    assert!(has(&lines, "server print: players after 1"), "{lines:?}");
    assert!(has(&lines, "client print: client starts at 0"), "{lines:?}");
}

/// Two loopback recordings of one harness are byte-identical (scene, playback, events, camera, summary); events.json holds
/// both sides' lines, each with its side; `--record-side server` records the server's world
#[test]
fn loopback_recordings_are_deterministic() {
    let dir = scratch("record");
    let harness = dir.join("Recorded.luau");
    std::fs::write(
        &harness,
        r##"
local Storage = game:GetService("ReplicatedStorage")
return {
	Server = function(Context)
		local Remote = Instance.new("RemoteEvent")
		Remote.Name = "Remote"
		Remote.Parent = Storage
		local Part = Instance.new("Part")
		Part.Name = "Mover"
		Part.Anchored = true
		Part.Parent = workspace
		Remote.OnServerEvent:Connect(function(Player, X)
			Part.Position = Vector3.new(X, 0, 0)
			print("moved", X)
		end)
	end,
	Client = function(Context)
		local Remote = Storage:WaitForChild("Remote")
		for X = 1, 5 do
			Remote:FireServer(X)
			task.wait(0.05)
		end
		local Own = Instance.new("Part")
		Own.Name = "ClientOnly"
		Own.Parent = workspace
	end,
}
"##,
    )
    .unwrap();
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let record = |out: &Path, extra: &[&str]| {
        let mut words: Vec<String> = ["--harness", &harness.to_string_lossy(), "--roots", &empty.to_string_lossy(), "--seconds", "0.5", "--run-context", "loopback", "--out", &out.to_string_lossy()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        words.extend(extra.iter().map(|s| s.to_string()));
        let args = run::parse(&words, true).unwrap();
        run::record(&args, &Host::default()).unwrap()
    };
    let (a, b, server) = (dir.join("a"), dir.join("b"), dir.join("server"));
    let summary = record(&a, &[]);
    record(&b, &[]);
    for file in ["scene.rbxm", "playback.json", "events.json", "camera.json", "summary.json"] {
        let bytes = |root: &Path| std::fs::read(root.join(file)).unwrap();
        // (summary.json names its folder)
        if file == "summary.json" {
            let text = |root: &Path| String::from_utf8(bytes(root)).unwrap().replace(&root.to_string_lossy().replace('\\', "\\\\"), "OUT");
            assert_eq!(text(&a), text(&b), "{file}");
        } else {
            assert_eq!(bytes(&a), bytes(&b), "{file}");
        }
    }
    assert_eq!(summary["runContext"], "loopback");
    assert_eq!(summary["loopback"]["latency"], 1);
    let events = std::fs::read_to_string(a.join("events.json")).unwrap();
    assert!(events.contains("\"side\": \"server\"") && events.contains("moved"), "{events}");
    let scene = |root: &Path| std::fs::read(root.join("scene.rbxm")).unwrap();
    let has_name = |bytes: &[u8], name: &str| bytes.windows(name.len()).any(|window| window == name.as_bytes());
    assert!(has_name(&scene(&a), "ClientOnly") && has_name(&scene(&a), "Mover"), "the client's world holds both");
    record(&server, &["--record-side", "server"]);
    assert!(has_name(&scene(&server), "Mover") && !has_name(&scene(&server), "ClientOnly"), "the server's world has no client part");
    let _ = std::fs::remove_dir_all(&dir);
}

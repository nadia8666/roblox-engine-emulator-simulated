//! Members the engine does not emulate report themselves (docs/engine.md "Parity first"): the first use of each writes an Output
//! line of kind `missing` with the script's position, and `ree run` / `ree record` summaries list them all (`unemulated`:
//! member, count, first position). Each test runs a harness as `ree run` does (record::run).

use std::path::PathBuf;

use serde_json::{Value as Json, json};

use crate::{
    launch::Host,
    record::run::{self, RecordArgs},
};

/// A scratch folder of its own for a test, with an empty project folder in it
fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ree-unemulated-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    dir
}

/// The arguments of `ree run` (or `ree record` with `--out`) of `harness` (named `name`, mounted at
/// ReplicatedStorage.RURFixtures.<name>) for 0.2 s with no project, and `extra`
fn args(dir: &std::path::Path, name: &str, harness: &str, extra: &[&str]) -> RecordArgs {
    let file = dir.join(format!("{name}.luau"));
    std::fs::write(&file, harness).unwrap();
    let mut words: Vec<String> = ["--harness", &file.to_string_lossy(), "--roots", &dir.join("empty").to_string_lossy(), "--seconds", "0.2"].iter().map(|s| s.to_string()).collect();
    words.extend(extra.iter().map(|s| s.to_string()));
    run::parse(&words, false).unwrap()
}

/// `ree run` of `harness`: its summary
fn run_harness(name: &str, harness: &str, extra: &[&str]) -> Json {
    let dir = scratch(name);
    let summary = run::run_only(&args(&dir, name, harness, extra), &Host::default()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    summary
}

/// The summary's output lines of kind `missing`, as `message @ trace`
fn missing_lines(summary: &Json) -> Vec<String> {
    summary["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|line| line["kind"] == "missing")
        .map(|line| format!("{} @ {}", line["message"].as_str().unwrap(), line["trace"].as_str().unwrap_or("-")))
        .collect()
}

/// The summary's other output lines, as `kind: message`
fn other_lines(summary: &Json) -> Vec<String> {
    summary["output"].as_array().unwrap().iter().filter(|line| line["kind"] != "missing").map(|line| format!("{}: {}", line["kind"].as_str().unwrap(), line["message"].as_str().unwrap())).collect()
}

/// A reflected method nothing implements (Humanoid:TakeDamage) still answers nil and changes nothing, and now says so: its
/// first call writes a `missing` line naming the script's line, and the summary lists it with every call counted and the
/// first call's position
#[test]
fn an_unimplemented_method_reports_its_first_call_and_counts_the_rest() {
    let summary = run_harness(
        "TakeDamage",
        r##"return function(Context)
	local Humanoid = Instance.new("Humanoid")
	print(select("#", Humanoid:TakeDamage(150)), Humanoid.Health)
	for _ = 1, 2 do
		Humanoid:TakeDamage(10)
	end
end
"##,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    assert_eq!(summary["errors"], 0);
    assert_eq!(missing_lines(&summary), ["Humanoid.TakeDamage is not emulated @ ReplicatedStorage.RURFixtures.TakeDamage:3"]);
    assert_eq!(other_lines(&summary), ["print: 0 100"]);
    assert_eq!(summary["unemulated"], json!([{ "member": "Humanoid.TakeDamage", "count": 3, "first": "ReplicatedStorage.RURFixtures.TakeDamage:3" }]));
}

/// A write of a Humanoid's Health is stored as written, but what a client does beside it (HealthChanged, Died at 0, keeping it
/// within 0..MaxHealth) is not emulated: a script's write that changes it reports those at the writing line, and Died still
/// never fires. A write that changes nothing reports nothing
#[test]
fn a_health_write_reports_the_humanoid_events_it_does_not_fire() {
    let summary = run_harness(
        "Health",
        r#"return function(Context)
	local Humanoid = Instance.new("Humanoid")
	Humanoid.Parent = workspace
	local Died = 0
	Humanoid.Died:Connect(function() Died += 1 end)
	Humanoid.Health = 100
	Humanoid.Health = 40
	local Write = Humanoid
	Write.Health = 0
	task.wait(0.1)
	print(Humanoid.Health, Died)
	Humanoid.Health = 150
end
"#,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    assert_eq!(
        missing_lines(&summary),
        [
            "Humanoid.HealthChanged is not emulated @ ReplicatedStorage.RURFixtures.Health:7",
            "Humanoid.Died is not emulated @ ReplicatedStorage.RURFixtures.Health:9",
            "Humanoid.Health outside 0..MaxHealth is not emulated @ ReplicatedStorage.RURFixtures.Health:12",
        ]
    );
    assert_eq!(other_lines(&summary), ["print: 0 0"]);
    assert_eq!(
        summary["unemulated"],
        json!([
            { "member": "Humanoid.HealthChanged", "count": 3, "first": "ReplicatedStorage.RURFixtures.Health:7" },
            { "member": "Humanoid.Died", "count": 1, "first": "ReplicatedStorage.RURFixtures.Health:9" },
            { "member": "Humanoid.Health outside 0..MaxHealth", "count": 1, "first": "ReplicatedStorage.RURFixtures.Health:12" },
        ])
    );
}

/// A Health write the engine makes, not the C fast path (a tween's step, through instance::assign), reports as a script's does;
/// no script's line is running then, so the first use names none (`first` null, no trace)
#[test]
fn a_tweened_health_reports_without_a_position() {
    let summary = run_harness(
        "HealthTween",
        r#"return function(Context)
	local Humanoid = Instance.new("Humanoid")
	game:GetService("TweenService"):Create(Humanoid, TweenInfo.new(0.1), { Health = 0 }):Play()
	task.wait(0.15)
	print(Humanoid.Health)
end
"#,
        &[],
    );
    assert_eq!(other_lines(&summary), ["print: 0"], "{summary}");
    assert_eq!(missing_lines(&summary), ["Humanoid.HealthChanged is not emulated @ -", "Humanoid.Died is not emulated @ -"]);
    let list = summary["unemulated"].as_array().unwrap();
    assert_eq!((list[0]["member"].as_str(), list[0]["first"].is_null()), (Some("Humanoid.HealthChanged"), true));
    assert!(list[0]["count"].as_u64().unwrap() > 1);
    assert_eq!(list[1], json!({ "member": "Humanoid.Died", "count": 1, "first": null }));
}

/// A run that uses nothing unemulated has no `missing` line and no `unemulated` key: its summary is as it was
#[test]
fn a_clean_run_lists_nothing() {
    let summary = run_harness(
        "Clean",
        r#"return function(Context)
	local Part = Instance.new("Part")
	Part.Parent = workspace
	game:GetService("ContextActionService"):UnbindAction("none")
	game:GetService("AnalyticsService"):LogCustomEvent(nil, "event")
	game:GetService("Players").LocalPlayer:Move(Vector3.new(1, 0, 0))
	print(Part.Name)
end
"#,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    assert_eq!(other_lines(&summary), ["print: Part"]);
    assert!(missing_lines(&summary).is_empty(), "{summary}");
    let keys: Vec<&str> = summary.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["duration", "errors", "fps", "frames", "harness", "mode", "output", "status"]);
}

/// Methods the engine registered as doing nothing where a client does something a script can see report themselves now (a
/// stub in the engine's Luau, at the script's line, not the stub's; one in Rust), and the list orders the most used first,
/// then by member
#[test]
fn stubs_that_do_nothing_report_themselves() {
    let summary = run_harness(
        "Stubs",
        r#"return function(Context)
	local Humanoid = Instance.new("Humanoid")
	Humanoid:ChangeState(Enum.HumanoidStateType.Jumping)
	Humanoid:MoveTo(Vector3.zero)
	local StarterGui = game:GetService("StarterGui")
	StarterGui:SetCoreGuiEnabled(Enum.CoreGuiType.All, false)
	StarterGui:SetCoreGuiEnabled(Enum.CoreGuiType.Chat, false)
	game:GetService("ContextActionService"):BindAction("Jump", function() end, false, Enum.KeyCode.Space)
	game:GetService("MarketplaceService"):PromptProductPurchase(game:GetService("Players").LocalPlayer, 1)
end
"#,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    let at = |line: u32| format!("ReplicatedStorage.RURFixtures.Stubs:{line}");
    assert_eq!(
        summary["unemulated"],
        json!([
            { "member": "StarterGui.SetCoreGuiEnabled", "count": 2, "first": at(6) },
            { "member": "ContextActionService.BindAction", "count": 1, "first": at(8) },
            { "member": "Humanoid.ChangeState", "count": 1, "first": at(3) },
            { "member": "Humanoid.MoveTo", "count": 1, "first": at(4) },
            { "member": "MarketplaceService.PromptProductPurchase", "count": 1, "first": at(9) },
        ])
    );
}

/// A `missing` line is the engine's, not a script's: LogService.MessageOut never gets it and GetLogHistory does not hold it,
/// and it counts as no error
#[test]
fn a_missing_line_is_not_logged_to_scripts() {
    let summary = run_harness(
        "Logged",
        r#"return function(Context)
	local LogService = game:GetService("LogService")
	local Seen = {}
	LogService.MessageOut:Connect(function(Message) table.insert(Seen, Message) end)
	Instance.new("Humanoid"):TakeDamage(1)
	task.wait(0.05)
	local History = 0
	for _, Entry in LogService:GetLogHistory() do
		if string.find(Entry.message, "not emulated", 1, true) then History += 1 end
	end
	print(#Seen, History)
end
"#,
        &[],
    );
    assert_eq!(other_lines(&summary), ["print: 0 0"], "{summary}");
    assert_eq!(summary["errors"], 0);
    assert_eq!(missing_lines(&summary).len(), 1);
}

/// `ree record` writes the same list into summary.json (and its status line counts them)
#[test]
fn a_recording_lists_the_unemulated_members_too() {
    let dir = scratch("record");
    let out = dir.join("out");
    let harness = "return function(Context)\n\tInstance.new(\"Humanoid\"):TakeDamage(5)\nend\n";
    let args = args(&dir, "Recorded", harness, &["--out", &out.to_string_lossy()]);
    let summary = run::record(&args, &Host::default()).unwrap();
    let written: Json = serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let expected = json!([{ "member": "Humanoid.TakeDamage", "count": 1, "first": "ReplicatedStorage.RURFixtures.Recorded:2" }]);
    assert_eq!(summary["unemulated"], expected, "{summary}");
    assert_eq!(written["unemulated"], expected);
}

/// The offline player's own answers stay quiet, Player's Async group methods as the older ones (public issue 19: a live game's
/// server asks IsInGroupAsync and GetRankInGroupAsync as the player joins, and ree raised "IsInGroupAsync is not a valid member
/// of Player" on both sides): the answers of a player in no group, no error, no `missing` line and no `unemulated` list
#[test]
fn the_offline_players_group_answers_stay_quiet_on_both_sides() {
    let summary = crate::loopback_tests::run_pair(
        "groups",
        r#"return {
	Server = function(Context)
		game:GetService("Players").PlayerAdded:Connect(function(Player)
			print("server", Player:IsInGroupAsync(7), Player:GetRankInGroupAsync(7), Player:GetRoleInGroupAsync(7), Player:IsInGroup(7))
		end)
	end,
	Client = function(Context)
		local Player = game:GetService("Players").LocalPlayer
		print("client", Player:IsInGroupAsync(7), Player:GetRankInGroupAsync(7), Player:GetRoleInGroupAsync(7), Player:GetRankInGroup(7))
	end,
}
"#,
        None,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    assert_eq!(summary["errors"], 0, "{summary}");
    let mut printed = other_lines(&summary);
    printed.sort();
    assert_eq!(printed, ["print: client false 0 Guest 0", "print: server false 0 Guest false"], "{summary}");
    assert!(missing_lines(&summary).is_empty(), "{summary}");
    assert!(summary.get("unemulated").is_none(), "{summary}");
}

/// A loopback run's summary holds both sides' members: the server's use (its line forwarded into the one output list, with
/// its side) and the client's
#[test]
fn a_loopback_run_lists_both_sides() {
    let summary = crate::loopback_tests::run_pair(
        "unemulated",
        r#"return {
	Server = function(Context)
		Instance.new("Humanoid"):TakeDamage(1)
	end,
	Client = function(Context)
		local Humanoid = Instance.new("Humanoid")
		Humanoid:TakeDamage(1)
		Humanoid:TakeDamage(2)
		game:GetService("StarterGui"):SetCore("TopbarEnabled", false)
	end,
}
"#,
        None,
        &[],
    );
    assert_eq!(summary["status"], "ok", "{summary}");
    let sides: Vec<String> = summary["output"].as_array().unwrap().iter().filter(|line| line["kind"] == "missing").map(|line| format!("{} {}", line["side"].as_str().unwrap(), line["message"].as_str().unwrap())).collect();
    assert_eq!(sides, ["server Humanoid.TakeDamage is not emulated", "client Humanoid.TakeDamage is not emulated", "client StarterGui.SetCore is not emulated"]);
    let list = summary["unemulated"].as_array().unwrap();
    assert_eq!(list[0]["member"], "Humanoid.TakeDamage");
    assert_eq!(list[0]["count"], 3);
    assert_eq!(list[1], json!({ "member": "StarterGui.SetCore", "count": 1, "first": "ReplicatedStorage.RURFixtures.unemulated:9" }));
}

/// Stand-ins for what a client fetches from the web (another account's name, an avatar, a badge) and a setting that is not
/// kept answer as before and report themselves
#[test]
fn stand_ins_report_themselves() {
    let summary = run_harness(
        "Standins",
        r#"return function(Context)
	local Players = game:GetService("Players")
	local Description = Players:GetHumanoidDescriptionFromUserId(1)
	local Model = Players:CreateHumanoidModelFromUserId(1)
	print(Description.ClassName, Model:FindFirstChildOfClass("Humanoid") ~= nil, Players:GetNameFromUserIdAsync(123456789))
	local Teleport = game:GetService("TeleportService")
	Teleport:SetTeleportSetting("Key", 1)
	print(Teleport:GetTeleportSetting("Key"))
	game:GetService("BadgeService"):GetBadgeInfoAsync(1)
end
"#,
        &[],
    );
    assert_eq!(other_lines(&summary), ["print: HumanoidDescription true User123456789", "print: nil"], "{summary}");
    let at = |line: u32| format!("ReplicatedStorage.RURFixtures.Standins:{line}");
    assert_eq!(
        summary["unemulated"],
        json!([
            { "member": "BadgeService.GetBadgeInfoAsync", "count": 1, "first": at(9) },
            { "member": "Players.CreateHumanoidModelFromUserId", "count": 1, "first": at(4) },
            { "member": "Players.GetHumanoidDescriptionFromUserId", "count": 1, "first": at(3) },
            { "member": "Players.GetNameFromUserIdAsync", "count": 1, "first": at(5) },
            { "member": "TeleportService.SetTeleportSetting", "count": 1, "first": at(7) },
        ])
    );
}

/// A server run has no client: FireClient, FireAllClients to a game with players and InvokeClient report themselves (at the
/// script's line, under pcall too), and FireAllClients with no player reaches no one, as in a client, so it stays quiet
#[test]
fn a_server_runs_remotes_to_clients_report_themselves() {
    let summary = run_harness(
        "Server",
        r#"return function(Context)
	local Players = game:GetService("Players")
	local Remote = Instance.new("RemoteEvent")
	Remote:FireAllClients(1)
	local Player = Instance.new("Player")
	Player.Parent = Players
	Remote:FireClient(Player, 2)
	Remote:FireAllClients(3)
	local Function = Instance.new("RemoteFunction")
	task.spawn(function() print(pcall(Function.InvokeClient, Function, Player, 1)) end)
	task.wait(0.05)
	Player.Parent = nil
	task.wait(0.05)
end
"#,
        &["--run-context", "server"],
    );
    assert_eq!(other_lines(&summary), ["print: false InvokeClient: Player left the game before RemoteFunction returned"], "{summary}");
    let at = |line: u32| format!("ReplicatedStorage.RURFixtures.Server:{line}");
    assert_eq!(
        summary["unemulated"],
        json!([
            { "member": "RemoteEvent.FireAllClients", "count": 1, "first": at(8) },
            { "member": "RemoteEvent.FireClient", "count": 1, "first": at(7) },
            { "member": "RemoteFunction.InvokeClient", "count": 1, "first": at(10) },
        ])
    );
}

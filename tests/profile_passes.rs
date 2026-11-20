//! A profile's figures that a kind would distort, taken through the built `ree`: the CPU's figures, host time a frame and the run
//! thread's cycles of a run that `work` (stepping), `alloc`, `heap`, `time` or `boundary` slows come from a pass of their own
//! (`<profile>/cpu-pass`, `--profile cpu` alone), so `--profile all` reports the host time a frame a run without them takes.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value as Json;

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("ree-passes-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// The built `ree` in `root`, with none of the environment's profiles or guards
fn ree(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ree"));
    command.current_dir(root);
    for name in ["REE_PROFILE", "REE_PROFILE_OUT", "REE_PROFILE_COUNTERS", "REE_SCRIPT_TIMEOUT", "REE_MAX_WALL_SECONDS", "REE_MAX_SCRIPT_TIMEOUTS", "REE_PARENT_WATCH", "REE_HW_COUNTERS"] {
        command.env_remove(name);
    }
    command
}

/// A harness whose frames each spin a Heartbeat handler through 60,000 steps
const BUSY: &str = "return function(Context)
	local RunService = game:GetService(\"RunService\")
	RunService.Heartbeat:Connect(function()
		local x = 0
		for i = 1, 60000 do
			x += i % 7
		end
	end)
end
";

fn profile(root: &Path, kinds: &str, out: &str) -> Json {
    let done = ree(root).args(["run", "--harness", "Busy.luau", "--seconds", "0.5", "--profile", kinds, "--profile-out", out]).output().unwrap();
    assert!(done.status.success(), "{}{}", String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    serde_json::from_str(&std::fs::read_to_string(root.join(out).join("profile.json")).unwrap()).unwrap()
}

/// A table's (a timeline's, the cpu frames') rows, each cut to the named columns
fn columns(table: &Json, names: &[&str]) -> Vec<Vec<Json>> {
    let index = |name: &str| table["columns"].as_array().unwrap().iter().position(|column| column == name).unwrap_or_else(|| panic!("no column {name}: {}", table["columns"]));
    let indices: Vec<usize> = names.iter().map(|name| index(name)).collect();
    table["rows"].as_array().unwrap().iter().map(|row| indices.iter().map(|&at| row[at].clone()).collect()).collect()
}

/// A profile's kinds
fn kinds(profile: &Json) -> Vec<&str> {
    profile["kinds"].as_array().unwrap().iter().map(|kind| kind.as_str().unwrap()).collect()
}

/// `--profile all` steps the VM (about five times slower), so its own host time a frame is not the run's: the CPU's figures and
/// the timeline's host time come from a pass of their own, a `--profile cpu` run of the same harness, as `--profile cpu` alone
/// takes it. The test checks where the figures come from, not how fast they read: the pass and a run of `--profile cpu` alone
/// are two processes at two moments, and the pass runs beside this file's other tests' runs, so on a busy machine their host
/// time a frame is apart by more than any margin stepping's five times leaves (a 4-core GitHub runner read the pass 2.7 times
/// cpu alone; bursty load on this file's tests read up to 4.2 times)
#[test]
fn cpu_figures_come_from_a_pass_of_their_own_when_a_kind_slows_the_run() {
    let root = scratch("cpu");
    std::fs::write(root.join("Busy.luau"), BUSY).unwrap();
    let all = profile(&root, "all", "all");
    let alone = profile(&root, "cpu", "alone");
    let folder = root.join("all").join("cpu-pass");
    assert!(folder.join("profile.json").is_file(), "the pass's profile");
    let kept: Vec<String> = std::fs::read_dir(&folder).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(kept, vec!["profile.json".to_string()], "the pass keeps only what it is for");
    let pass: Json = serde_json::from_str(&std::fs::read_to_string(folder.join("profile.json")).unwrap()).unwrap();
    assert!(all["cpu"]["pass"].as_str().is_some_and(|pass| pass.contains("a run of its own")), "{}", all["cpu"]);
    assert!(all["cpu"]["pass_differs"].is_null(), "the pass went the profiled run's way: {}", all["cpu"]);
    assert!(alone["cpu"]["pass"].is_null(), "cpu alone is its own pass: {}", alone["cpu"]);
    // all's kinds step the VM; the pass is cpu alone over the same run (its harness, its frames), as the run of cpu alone is
    assert!(kinds(&all).contains(&"work") && kinds(&all).contains(&"timeline"), "{:?}", kinds(&all));
    assert_eq!(kinds(&pass), ["cpu"], "the pass's kinds");
    assert_eq!(kinds(&alone), ["cpu"], "cpu alone's kinds");
    for key in ["harness_hash", "frames", "status"] {
        assert_eq!(pass["run"][key], all["run"][key], "the pass's run: {key}");
    }
    assert_eq!(columns(&pass["cpu"]["frames"], &["frame", "phase"]), columns(&alone["cpu"]["frames"], &["frame", "phase"]), "the pass's frames are the ones cpu alone takes");
    // the CPU's figures are the pass's, as its profile has them
    let mut cpu = all["cpu"].clone();
    cpu.as_object_mut().unwrap().remove("pass");
    assert_eq!(cpu, pass["cpu"], "the CPU's figures are the pass's");
    // the timeline's host time and cycles are the pass's, frame by frame and to the digit: the stepped run's own (several times
    // slower) are in no frame, and no frame goes without
    let (timeline, theirs) = (columns(&all["timeline"], &["frame", "host_ms", "cycles"]), columns(&pass["cpu"]["frames"], &["frame", "host_ms", "cycles"]));
    assert!(timeline.len() >= 25 && timeline.iter().all(|row| row[1].as_f64().is_some_and(|ms| ms > 0.0)), "a host time each frame: {timeline:?}");
    assert_eq!(timeline, theirs, "the timeline's host time a frame is the pass's");
    // the run's figures: the thread's CPU by phase and frame, memory from the run's start
    for cpu in [&all["cpu"], &alone["cpu"]] {
        assert!(cpu["thread_ms"].as_f64().is_some_and(|ms| ms > 0.0) || !cfg!(windows), "{cpu}");
        assert!(cpu["phases"].as_array().is_some_and(|phases| !phases.is_empty()), "{cpu}");
        if cfg!(windows) {
            assert!(cpu["working_set"]["baseline"].as_u64().unwrap() > 0 && cpu["working_set"]["growth"].is_i64(), "{cpu}");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `--profile phases` is a tree whose own times add up to the command's: the recorder's first walk is timed inside the harness's
/// start, not beside it
#[test]
fn phases_are_a_tree_whose_own_times_add_up() {
    let root = scratch("phases");
    std::fs::write(root.join("Busy.luau"), BUSY).unwrap();
    let out = root.join("recording");
    let done = ree(&root).args(["record", "--harness", "Busy.luau", "--out", &out.to_string_lossy(), "--seconds", "0.1", "--profile", "phases", "--profile-out", "p"]).output().unwrap();
    assert!(done.status.success(), "{}{}", String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    let json: Json = serde_json::from_str(&std::fs::read_to_string(root.join("p").join("profile.json")).unwrap()).unwrap();
    let nodes = json["phase_tree"].as_array().unwrap_or_else(|| panic!("{}", json["phase_timing"]));
    let index = |name: &str| nodes.iter().position(|node| node["name"] == name).unwrap_or_else(|| panic!("no {name}: {}", json["phase_timing"]));
    let (start, walk) = (index("harness start"), index("recorder: first walk"));
    assert_eq!(nodes[walk]["parent"].as_u64(), Some(start as u64), "{}", json["phase_timing"]);
    let own: f64 = nodes.iter().map(|node| node["self_ms"].as_f64().unwrap()).sum();
    let total = nodes[0]["total_ms"].as_f64().unwrap();
    assert!((own - total).abs() < 0.01 * total + 0.5, "the own times {own:.1} ms add up to the command's {total:.1} ms\n{}", json["phase_timing"]);
    assert!(nodes.iter().all(|node| node["self_ms"].as_f64().unwrap() >= -0.5), "{}", json["phase_timing"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A module's main chunk, which `require` runs from the engine's side, is a function of the profile like any other: its cache
/// sites name a function profile.json lists (they read "function 17 line 4" when the chunk had no node of its own)
#[test]
fn a_modules_main_chunk_is_a_function_of_the_profile() {
    let root = scratch("chunk");
    std::fs::write(root.join("Mod.luau"), "return function(Context)\n\tlocal Helper = require(script.Helper)\n\tHelper.go()\nend\n").unwrap();
    std::fs::create_dir_all(root.join("Mod")).unwrap();
    std::fs::write(
        root.join("Mod").join("Helper.luau"),
        "local config = table.clone({speed = 1, size = 2})\nlocal total = 0\nfor i = 1, 100 do\n\ttotal += config.speed + config.size\nend\nlocal M = {}\nfunction M.go()\n\treturn total\nend\nreturn M\n",
    )
    .unwrap();
    let done = ree(&root).args(["run", "--harness", "Mod.luau", "--seconds", "0.05", "--profile", "work,caches", "--profile-out", "p"]).output().unwrap();
    assert!(done.status.success(), "{}{}", String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    let json: Json = serde_json::from_str(&std::fs::read_to_string(root.join("p").join("profile.json")).unwrap()).unwrap();
    let functions = json["functions"].as_array().unwrap();
    let chunk = functions.iter().find(|f| f["module"] == "ReplicatedStorage.RURFixtures.Mod.Helper" && f["name"] == "" && f["line"].as_i64().is_some_and(|line| line <= 1)).unwrap_or_else(|| panic!("no main chunk of Helper: {:#}", json["functions"]));
    assert!(chunk["calls"].as_u64().unwrap() >= 1 && chunk["self"]["insns"].as_u64().unwrap() > 300, "{chunk}");
    let sites: Vec<&Json> = json["luau_caches"]["sites"].as_array().unwrap().iter().filter(|site| site["function"] == chunk["id"]).collect();
    assert!(sites.iter().any(|site| site["line"] == 4 && site["key"] == "size" && site["op"] == "GETTABLEKS"), "the chunk's own sites: {sites:?}");
    for site in json["luau_caches"]["sites"].as_array().unwrap() {
        assert!(functions.iter().any(|f| f["id"] == site["function"]), "a site in a function profile.json lists: {site}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A recording's host time stays where it was spent: the recorder's walk of 3,000 parts and its samples each frame are the
/// recorder's frame (`(ree engine step: recorder)`), not the first handler's loop line after them
#[test]
fn a_recorders_host_time_is_its_own() {
    let root = scratch("recorder");
    std::fs::write(
        root.join("Parts.luau"),
        "return function(Context)\n\tlocal RunService = game:GetService(\"RunService\")\n\tlocal folder = Instance.new(\"Folder\")\n\tfor i = 1, 3000 do\n\t\tlocal part = Instance.new(\"Part\")\n\t\tpart.Anchored = true\n\t\tpart.Position = Vector3.new(i % 60, math.floor(i / 60), 0)\n\t\tpart.Parent = folder\n\tend\n\tfolder.Parent = workspace\n\tlocal M = {}\n\tfunction M.handler()\n\t\tlocal x = 0\n\t\tfor i = 1, 2000 do\n\t\t\tx += i % 3\n\t\tend\n\t\treturn x\n\tend\n\tRunService.Heartbeat:Connect(M.handler)\nend\n",
    )
    .unwrap();
    let out = root.join("recording");
    let done = ree(&root).args(["record", "--harness", "Parts.luau", "--out", &out.to_string_lossy(), "--seconds", "0.5", "--profile", "time", "--profile-out", "p"]).output().unwrap();
    assert!(done.status.success(), "{}{}", String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    let json: Json = serde_json::from_str(&std::fs::read_to_string(root.join("p").join("profile.json")).unwrap()).unwrap();
    let functions = json["functions"].as_array().unwrap();
    let time = |named: &dyn Fn(&str) -> bool| functions.iter().filter(|f| f["name"].as_str().is_some_and(named)).map(|f| f["total"]["time_ms"].as_f64().unwrap()).sum::<f64>();
    let (recorder, handler) = (time(&|name| name == "(ree engine step: recorder)"), time(&|name| name == "handler" || name.ends_with(".handler")));
    assert!(recorder > 20.0 && handler < 0.2 * recorder, "the recorder {recorder:.1} ms, the handler {handler:.1} ms\n{}", json["summary"].as_str().unwrap_or(""));
    let _ = std::fs::remove_dir_all(&root);
}

/// A harness that allocates 1.5 million tables in one resumption, 80 frames down: about 35 ms of the work clock, under its
/// 50 ms timeout, and about as much host time on its own; the allocation profile takes each allocation's stack, 80 frames
/// deep, so host time runs far past the timeout's host-time backstop (6 x 50 ms) in the profiled run and not in its cpu pass
const ALLOCATES: &str = "local function deep(n, count)
	if n > 0 then
		return deep(n - 1, count)
	end
	local t
	for i = 1, count do
		t = {}
	end
	return t
end
return function(Context)
	deep(80, 1500000)
end
";

/// A script timeout that host time fired in one of a profile's two runs (its backstop counts host time, which the profiling
/// slows) makes the runs go different ways: the profile says the CPU figures are the other run's
#[test]
fn a_timeout_host_time_fires_in_one_pass_only_is_told() {
    let root = scratch("timeout");
    std::fs::write(root.join("Allocates.luau"), ALLOCATES).unwrap();
    let done = ree(&root)
        .args(["run", "--harness", "Allocates.luau", "--seconds", "0.1", "--script-timeout", "0.05", "--max-script-timeouts", "0", "--profile", "all", "--profile-out", "p"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&done.stderr);
    let json: Json = serde_json::from_str(&std::fs::read_to_string(root.join("p").join("profile.json")).unwrap_or_else(|_| panic!("{stderr}"))).unwrap();
    let pass: Json = serde_json::from_str(&std::fs::read_to_string(root.join("p").join("cpu-pass").join("profile.json")).unwrap()).unwrap();
    assert_eq!(pass["run"]["script_timeouts"].as_u64(), Some(0), "the cpu pass runs unhindered: {}", pass["run"]);
    assert!(json["run"]["host_backstop_timeouts"].as_u64().is_some_and(|n| n >= 1), "host time fired the profiled run's timeout: {}\n{stderr}", json["run"]);
    assert!(json["cpu"]["pass_differs"].as_str().is_some_and(|text| text.contains("script timeouts")), "{}", json["cpu"]);
    assert!(stderr.contains("the cpu pass went another way"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

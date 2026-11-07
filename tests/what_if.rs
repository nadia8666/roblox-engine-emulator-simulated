//! `ree profile try` (src/profiler/whatif.rs, docs/cli.md "What-if comparisons") through the built `ree`: the folder and
//! compare.json a comparison writes, the table shapes and growth of each variant's run, the check of their results, the
//! figures that are the same every run and the hardware counters when they count; and a profile of every kind, table
//! shapes and growth included, leaves a recording and what a script reads as they were.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value as Json;

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("ree-what-if-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(root.join("empty")).unwrap();
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

fn try_file(root: &Path, file: &str, source: &str, out: &str, extra: &[&str], hw: bool) -> (Output, Json) {
    let path = root.join(file);
    std::fs::write(&path, source).unwrap();
    let mut command = ree(root);
    command.args(["profile", "try", &path.to_string_lossy(), "--out", &root.join(out).to_string_lossy()]).args(extra);
    if hw {
        command.env("REE_HW_COUNTERS", "mock");
    }
    let done = command.output().unwrap();
    let compare = std::fs::read_to_string(root.join(out).join("compare.json")).map(|text| serde_json::from_str(&text).unwrap()).unwrap_or(Json::Null);
    (done, compare)
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Two variants of a table keyed two ways, with the line each table is made on known (lines 8, 10, 22 and 24)
const NESTED: &str = "return {
	title = \"nested either way\",
	sizes = { 8, 64 },
	variants = {
		{
			name = \"by kind\",
			run = function(n)
				local t = {}
				for k = 1, 2 do
					local inner = {}
					for v = 1, n do
						inner[v] = v
					end
					t[k] = inner
				end
				return t
			end,
		},
		{
			name = \"by item\",
			run = function(n)
				local t = {}
				for v = 1, n do
					t[v] = { v, v }
				end
				return t
			end,
		},
	},
	check = function(a, b, n)
		for v = 1, n do
			if a[1][v] ~= b[v][1] or a[2][v] ~= b[v][2] then
				return false, `item {v}`
			end
		end
		return true
	end,
}
";

fn run_of<'a>(compare: &'a Json, variant: &str, size: f64) -> &'a Json {
    compare["runs"].as_array().unwrap().iter().find(|run| run["variant"] == variant && run["size"].as_f64() == Some(size)).unwrap_or_else(|| panic!("no run of {variant} at {size}"))
}

/// The table site of a run made on `line` of the variant file
fn site<'a>(run: &'a Json, line: i64) -> &'a Json {
    run["tables"]["sites"].as_array().unwrap().iter().find(|site| site["frames"][0]["line"].as_i64() == Some(line)).unwrap_or_else(|| panic!("no site at line {line}: {:#}", run["tables"]))
}

#[test]
fn a_comparison_writes_profiles_and_an_index_of_what_they_show() {
    let root = scratch("index");
    let (done, compare) = try_file(&root, "nested.luau", NESTED, "out", &["--rounds", "2", "--time-ms", "1", "--no-hw"], false);
    let output = text(&done);
    assert_eq!(done.status.code(), Some(0), "{output}");
    assert_eq!((compare["format"].as_str(), compare["version"].as_u64(), compare["baseline"].as_str()), (Some("ree-what-if"), Some(1), Some("by kind")));
    let variants: Vec<(&str, &str, &str)> = compare["variants"].as_array().unwrap().iter().map(|v| (v["name"].as_str().unwrap(), v["slug"].as_str().unwrap(), v["kind"].as_str().unwrap())).collect();
    assert_eq!(variants, [("by kind", "by-kind", "function"), ("by item", "by-item", "function")]);
    assert_eq!(compare["sizes"], serde_json::json!([8.0, 64.0]));
    assert!(compare["fidelity"].as_str().unwrap().starts_with("fidelity:"));
    assert!(compare["metrics"].as_array().unwrap().iter().any(|metric| metric["key"] == "work_ms" && metric["exact"] == true));
    // a profile folder a run, read by ree's other profile commands
    for run in compare["runs"].as_array().unwrap() {
        assert_eq!(run["status"], "ok", "{run}");
        assert!(root.join("out").join(run["profile"].as_str().unwrap()).join("profile.json").is_file(), "{run}");
        assert_eq!(run["check"]["ok"], true, "{run}");
        assert!(run["digest"].as_str().is_some_and(|digest| digest.len() == 8), "{run}");
        assert!(run["run"]["work_ms"].as_f64().unwrap() > 0.0 && run["run"]["insns"].as_f64().unwrap() > 0.0, "{run}");
        assert_eq!(run["host"]["ms"].as_array().unwrap().len(), 2, "a figure a round: {run}");
        assert!(run["run"]["host_ms"].as_f64().unwrap() > 0.0, "{run}");
        let heap = &run["heap"];
        assert!(heap["after_run"].as_f64().unwrap() > heap["after_setup"].as_f64().unwrap(), "the result is held: {heap}");
    }
    // what each variant's run allocated and the shape of what it holds: by kind, three tables (two arrays of n grown from
    // empty and the table of two holding them); by item, n + 1 (n arrays of two, made whole, and the array of n holding them)
    let (kind, item) = (run_of(&compare, "by kind", 64.0), run_of(&compare, "by item", 64.0));
    assert_eq!((kind["run"]["allocs"].as_f64(), item["run"]["allocs"].as_f64()), (Some(3.0), Some(65.0)));
    assert_eq!(run_of(&compare, "by item", 8.0)["run"]["allocs"].as_f64(), Some(9.0));
    let inner = site(item, 24);
    assert_eq!((inner["phase"].as_str(), inner["count"].as_f64()), (Some("run"), Some(64.0)));
    assert_eq!((inner["array"]["min"].as_u64(), inner["array"]["max"].as_u64(), inner["array"]["used"].as_u64(), inner["hash"]["max"].as_u64()), (Some(2), Some(2), Some(128), Some(0)));
    assert_eq!((inner["levels"]["max"].as_u64(), inner["rehashes"].as_u64()), (Some(1), Some(0)));
    let outer = site(item, 22);
    assert_eq!((outer["count"].as_f64(), outer["array"]["max"].as_u64(), outer["levels"]["max"].as_u64()), (Some(1.0), Some(64), Some(2)));
    // (grown 1, 2, 4, ..., 64: seven rehashes moving 1 + 2 + 3 + 5 + 9 + 17 + 33 keys)
    assert_eq!((outer["rehashes"].as_u64(), outer["rehash_keys"].as_u64()), (Some(7), Some(70)));
    let arrays = site(kind, 10);
    assert_eq!((arrays["count"].as_f64(), arrays["array"]["min"].as_u64(), arrays["array"]["used"].as_u64(), arrays["rehashes"].as_u64(), arrays["rehash_keys"].as_u64()), (Some(2.0), Some(64), Some(128), Some(14), Some(140)));
    assert_eq!((site(kind, 8)["array"]["max"].as_u64(), site(kind, 8)["levels"]["max"].as_u64()), (Some(2), Some(2)));
    assert_eq!((kind["tables"]["run"]["count"].as_f64(), item["tables"]["run"]["count"].as_f64()), (Some(3.0), Some(65.0)));
    // (the outer table of two never rehashes: a loop with constant bounds, for k = 1, 2, lets the compiler size it ahead)
    assert_eq!(kind["run"]["rehashes"].as_f64(), Some(14.0), "the run's own rehashes: the inner arrays'");
    assert_eq!(site(kind, 8)["rehashes"].as_u64(), Some(0));
    // how they grow with n, and who wins
    let scaling = |variant: &str, metric: &str| -> f64 {
        compare["scaling"].as_array().unwrap().iter().find(|entry| entry["variant"] == variant && entry["metric"] == metric).and_then(|entry| entry["exponent"].as_f64()).unwrap_or_else(|| panic!("no {metric} scaling of {variant}"))
    };
    assert!(scaling("by item", "allocs") > 0.9 && scaling("by kind", "allocs").abs() < 1e-9);
    // (two sizes: the exponent is the slope between them on a log scale)
    let insns = |size: f64| run_of(&compare, "by kind", size)["run"]["insns"].as_f64().unwrap();
    assert!((scaling("by kind", "insns") - (insns(64.0) / insns(8.0)).ln() / 8f64.ln()).abs() < 1e-9);
    let allocs = compare["winners"].as_array().unwrap().iter().find(|winner| winner["metric"] == "allocs" && winner["size"] == 64.0).unwrap();
    assert_eq!((allocs["winner"].as_str(), allocs["runner_up"].as_str(), allocs["clear"].as_bool()), (Some("by kind"), Some("by item"), Some(true)));
    assert!((allocs["ratio"].as_f64().unwrap() - 65.0 / 3.0).abs() < 1e-9);
    assert_eq!(allocs["ranking"].as_array().unwrap().len(), 2);
    assert_eq!((compare["check"]["ran"].as_bool(), compare["check"]["ok"].as_bool()), (Some(true), Some(true)));
    assert_eq!(compare["aa"].as_array().unwrap().len(), 2, "an A/A pair a size");
    let diff = &compare["diffs"][0];
    assert_eq!((diff["variant"].as_str(), diff["size"].as_f64()), (Some("by item"), Some(64.0)));
    assert!(root.join("out").join(diff["dir"].as_str().unwrap()).join("diff.json").is_file());
    // the summary, printed and in compare.txt
    let summary = std::fs::read_to_string(root.join("out").join("compare.txt")).unwrap();
    assert!(output.contains(&summary), "the summary is printed");
    assert!(summary.contains("table shapes at n = 64") && summary.contains("by item: 65 tables") && summary.contains("by kind: 3 tables"), "{summary}");
    assert!(summary.contains("check: every variant's result holds"), "{summary}");
    assert!(!root.join("out").join("timed").exists(), "the timed runs' folders go");
    // a second comparison into the same folder replaces the first; a folder of other files is refused
    let (again, _) = try_file(&root, "nested.luau", NESTED, "out", &["--no-time", "--no-hw", "--no-check", "--sizes", "8"], false);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    assert!(!root.join("out").join("runs").join("by-kind").join("64").exists());
    std::fs::write(root.join("empty").join("notes.txt"), "mine").unwrap();
    let (refused, _) = try_file(&root, "nested.luau", NESTED, "empty", &["--no-time"], false);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused).contains("not a what-if comparison's") && root.join("empty").join("notes.txt").is_file(), "{}", text(&refused));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_check_and_a_variant_that_raises_are_reported() {
    let root = scratch("failures");
    let source = "return {
	sizes = { 4 },
	variants = {
		{ name = \"sum\", run = function(n) local s = 0 for i = 1, n do s += i end return s end },
		{ name = \"off by one\", run = function(n) local s = 0 for i = 1, n + 1 do s += i end return s end },
		{ name = \"raises\", run = function(n) error(\"no such thing\") end },
	},
	check = true,
}
";
    let (done, compare) = try_file(&root, "failures.luau", source, "out", &["--no-time", "--no-hw"], false);
    let output = text(&done);
    assert_eq!(done.status.code(), Some(1), "{output}");
    let raises = run_of(&compare, "raises", 4.0);
    assert_eq!(raises["status"], "error");
    assert!(raises["error"].as_str().unwrap().contains("run raised") && raises["error"].as_str().unwrap().contains("no such thing"), "{raises}");
    assert_eq!(run_of(&compare, "off by one", 4.0)["status"], "ok");
    assert_ne!(run_of(&compare, "off by one", 4.0)["digest"], run_of(&compare, "sum", 4.0)["digest"]);
    let failures = compare["check"]["failures"].as_array().unwrap();
    assert_eq!(compare["check"]["ok"], false);
    assert!(failures.iter().any(|failure| failure["variant"] == "off by one" && failure["size"] == 4.0), "{failures:?}");
    assert!(failures.iter().any(|failure| failure["variant"] == "raises" && failure["message"].as_str().unwrap().contains("raised")), "{failures:?}");
    assert!(output.contains("check FAILED: off by one at n = 4"), "{output}");
    assert!(compare["notes"].as_array().unwrap().iter().any(|note| note.as_str().unwrap().starts_with("raises at n = 4: error")), "{}", compare["notes"]);
    // a variant file that is not one says so
    let (bad, _) = try_file(&root, "bad.luau", "return 5\n", "bad", &["--no-time"], false);
    assert_eq!(bad.status.code(), Some(1));
    assert!(text(&bad).contains("the variant file returns a table"), "{}", text(&bad));
    let (unknown, _) = try_file(&root, "failures.luau", source, "out2", &["--variants", "nope"], false);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown).contains("no variant \"nope\""), "{}", text(&unknown));
    let (option, _) = try_file(&root, "failures.luau", source, "out3", &["--sizez", "4"], false);
    assert_eq!(option.status.code(), Some(2), "{}", text(&option));
    let _ = std::fs::remove_dir_all(&root);
}

/// What the profiler counts is the same every run of the same code: two comparisons agree on every exact figure and
/// every table, in every run
#[test]
fn the_exact_figures_are_the_same_every_run() {
    let root = scratch("exact");
    let (first, a) = try_file(&root, "nested.luau", NESTED, "a", &["--no-time", "--no-hw", "--no-check"], false);
    let (second, b) = try_file(&root, "nested.luau", NESTED, "b", &["--no-time", "--no-hw", "--no-check"], false);
    assert_eq!((first.status.code(), second.status.code()), (Some(0), Some(0)), "{}{}", text(&first), text(&second));
    let exact: Vec<String> = a["metrics"].as_array().unwrap().iter().filter(|metric| metric["exact"] == true).map(|metric| metric["key"].as_str().unwrap().to_string()).collect();
    assert!(exact.len() >= 10, "{exact:?}");
    for (x, y) in a["runs"].as_array().unwrap().iter().zip(b["runs"].as_array().unwrap()) {
        for phase in ["setup", "run"] {
            for key in &exact {
                assert_eq!(x[phase][key], y[phase][key], "{phase} {key} of {} at {}", x["variant"], x["size"]);
            }
        }
        assert_eq!(x["heap"], y["heap"]);
        assert_eq!(x["digest"], y["digest"]);
        let strip = |tables: &Json| -> Json {
            let mut tables = tables.clone();
            for site in tables["sites"].as_array_mut().unwrap() {
                for frame in site["frames"].as_array_mut().unwrap() {
                    frame.as_object_mut().unwrap().remove("path");
                }
            }
            tables
        };
        assert_eq!(strip(&x["tables"]), strip(&y["tables"]));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// With the counters counting (the mock source here), a run of its own a variant and size reads them, and its figures join
/// the rest
#[test]
fn hardware_counters_join_the_figures_when_they_count() {
    let root = scratch("hw");
    let (done, compare) = try_file(&root, "nested.luau", NESTED, "out", &["--no-time", "--no-check", "--sizes", "8"], true);
    assert_eq!(done.status.code(), Some(0), "{}", text(&done));
    assert_eq!(compare["hw"]["read"], true, "{}", compare["hw"]);
    let summary = std::fs::read_to_string(root.join("out").join("compare.txt")).unwrap();
    assert!(summary.contains("hardware counters: median of 3 rounds"), "{summary}");
    for run in compare["runs"].as_array().unwrap() {
        assert_eq!(run["hw"]["rounds"], serde_json::json!([0, 1, 2]), "{run}");
        assert!(run["hw_profile"].as_str().is_some_and(|dir| root.join("out").join(dir).join("profile.json").is_file()), "{run}");
        assert!(run["run"]["hw_cycles"].as_f64().unwrap() > 0.0, "{run}");
        assert!(run["run"]["hw_instructions"].as_f64().is_some(), "{run}");
    }
    // without them, the comparison says why
    let (plain, without) = try_file(&root, "nested.luau", NESTED, "plain", &["--no-time", "--no-check", "--sizes", "8"], false);
    if plain.status.code() == Some(0) && without["hw"]["read"] == false {
        assert!(without["hw"]["why"].as_str().is_some_and(|why| !why.is_empty()), "{}", without["hw"]);
        assert!(without["runs"][0]["hw_profile"].is_null());
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A harness that grows tables every frame, keeps them nested, marks phases and prints what a script reads of the clock
/// and the heap
const GROWING: &str = "return function(Context)
	local RunService = game:GetService(\"RunService\")
	Context.ProfilePhase(\"setup\")
	local rows = {}
	for i = 1, 400 do
		rows[i] = { id = i, tags = { \"a\" .. i % 7, \"b\" } }
	end
	local byTag = {}
	Context.ProfilePhase(\"steady\")
	RunService.Heartbeat:Connect(function()
		for _, row in rows do
			for _, tag in row.tags do
				local list = byTag[tag]
				if not list then
					list = {}
					byTag[tag] = list
				end
				table.insert(list, row.id)
			end
		end
		print(os.clock(), collectgarbage(\"count\"), #byTag.b)
	end)
end
";

/// Profiling, table shapes and growth included, changes nothing a script sees: a recording with every kind of the suite
/// is byte for byte the recording without, and a run prints the same clock and heap readings
#[test]
fn a_profile_of_every_kind_leaves_a_recording_as_it_was() {
    let root = scratch("recording");
    let harness = root.join("Growing.luau");
    std::fs::write(&harness, GROWING).unwrap();
    let pinned = |command: &mut Command| {
        command.env("REE_FROZEN_CLOCK", "1").env("RUR_EPOCH", "1790460000");
    };
    let record = |name: &str, profile: bool| -> PathBuf {
        let out = root.join(name);
        let mut command = ree(&root);
        command.args(["record", "--harness", &harness.to_string_lossy(), "--roots", &root.join("empty").to_string_lossy(), "--out", &out.to_string_lossy(), "--seconds", "0.5"]);
        pinned(&mut command);
        if profile {
            command.args(["--profile", "all", "--profile-out", &root.join(format!("{name}-profile")).to_string_lossy()]);
        }
        let done = command.output().unwrap();
        assert_eq!(done.status.code(), Some(0), "{}", text(&done));
        out
    };
    let (plain, profiled) = (record("plain", false), record("profiled", true));
    let profile: Json = serde_json::from_str(&std::fs::read_to_string(root.join("profiled-profile").join("profile.json")).unwrap()).unwrap();
    assert!(profile["alloc"]["rehash"]["events"].as_u64().unwrap() > 0, "the tables grew under the profile");
    assert!(profile["heap"]["snapshots"].as_array().unwrap().iter().any(|snapshot| snapshot["tables"]["count"].as_u64().unwrap() > 400));
    let mut files: Vec<String> = std::fs::read_dir(&plain).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    assert!(files.iter().any(|file| file == "events.json") && files.iter().any(|file| file == "playback.json"), "{files:?}");
    for file in &files {
        let (a, b) = (std::fs::read(plain.join(file)).unwrap(), std::fs::read(profiled.join(file)).unwrap());
        if file == "summary.json" {
            // (the summary names the folder it was written to)
            let strip = |bytes: &[u8]| -> Json {
                let mut summary: Json = serde_json::from_slice(bytes).unwrap();
                summary.as_object_mut().unwrap().remove("out");
                summary
            };
            assert_eq!(strip(&a), strip(&b), "summary.json");
        } else {
            assert!(a == b, "{file} differs with --profile all");
        }
    }
    // what a run prints: the clock and the heap read the same
    let run = |profile: bool| -> Json {
        let mut command = ree(&root);
        command.args(["run", "--harness", &harness.to_string_lossy(), "--roots", &root.join("empty").to_string_lossy(), "--seconds", "0.5"]);
        pinned(&mut command);
        if profile {
            command.args(["--profile", "all", "--profile-out", &root.join("run-profile").to_string_lossy()]);
        }
        let done = command.output().unwrap();
        assert_eq!(done.status.code(), Some(0), "{}", text(&done));
        let summary: Json = serde_json::from_slice(&done.stdout).unwrap();
        summary["output"].clone()
    };
    let (quiet, watched) = (run(false), run(true));
    assert!(quiet.as_array().unwrap().len() >= 20, "{quiet}");
    assert_eq!(quiet, watched);
    let _ = std::fs::remove_dir_all(&root);
}

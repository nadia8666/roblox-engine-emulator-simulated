//! A real profile: the engine runs a harness in this process (`ree run --profile all`), and the viewer reads what it
//! wrote. Its figures are tied to what the profile counts on its own: the call tree's work and instructions add up to
//! the run's totals, every allocated byte lands on one site, and the sort the harness does every frame is found.

// (the engine's service code calls mimalloc's mi_collect, which ree's binary links the same way)
extern crate mimalloc;

mod common;

use common::Temp;
use ree_profile_view::{app::View, cli, metric::Metric, sites::alloc_sites, theme::ThemeKind};

const HARNESS: &str = r#"local RunService = game:GetService("RunService")

local items = {}
for index = 1, 500 do
	items[index] = { score = (index * 37) % 500 }
end

-- (a table's function: the compiler inlines a small local function into its caller)
local Sorter = {}

function Sorter.rank()
	local copy = table.clone(items)
	table.sort(copy, function(a, b)
		return a.score < b.score
	end)
	return copy
end

return function(Context)
	RunService.Heartbeat:Connect(function()
		Sorter.rank()
	end)
end
"#;

fn run_engine(harness: &std::path::Path, out: &std::path::Path) -> i32 {
    run_engine_with(harness, out, &[])
}

/// [`run_engine`] with more of `ree run`'s arguments (`--context`)
fn run_engine_with(harness: &std::path::Path, out: &std::path::Path, more: &[&str]) -> i32 {
    let mut args: Vec<String> = ["--harness", harness.to_str().unwrap(), "--seconds", "1", "--profile", "all", "--profile-out", out.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
    args.extend(more.iter().map(|s| s.to_string()));
    // (on a thread with a service worker's stack, as the ree binary runs its commands)
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || roblox_engine_emulator::record::run::cli(&args, &roblox_engine_emulator::launch::Host::default(), false))
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn a_profile_the_engine_wrote_reads_whole() {
    let temp = Temp::new("engine");
    let harness = temp.path().join("Sorter.luau");
    std::fs::write(&harness, HARNESS).unwrap();
    let out = temp.path().join("profile");
    assert_eq!(run_engine(&harness, &out), 0);
    let mut app = cli::open(&[out.clone()], ThemeKind::Dark).unwrap();
    let p = app.profile();
    let work = p.raw.work.as_ref().expect("work");

    // the call tree's work is the work clock's, less what it charged outside any instruction; its instructions are
    // every instruction the profile counted
    let tree = app.tree_of(0, Metric::Work, false);
    let booked = work.total_ms - work.outside_instructions_ms;
    let rounding = 1e-8;
    assert!((tree.total() - booked).abs() <= rounding, "tree {} ms, work clock less outside {booked} ms, {} nodes", tree.total(), p.raw.tree.len());
    let insns = app.tree_of(0, Metric::Insns, false);
    assert_eq!(insns.total(), work.script_insns + work.engine_insns);

    // every allocated byte is on one site (the folded stacks hold every stack), and on one line or the engine's
    let p = app.profile();
    let sites = alloc_sites(p);
    let alloc = p.raw.alloc.as_ref().expect("alloc");
    let on_sites: f64 = sites.iter().map(|site| site.bytes).sum();
    assert!((on_sites - alloc.bytes).abs() < 1.0, "sites {on_sites}, allocated {}", alloc.bytes);
    let on_lines: f64 = p.line_alloc.values().map(|v| v.0).sum();
    let unplaced: f64 = sites.iter().filter(|site| site.func == ree_profile_view::sites::ENGINE || site.line < 0).map(|site| site.bytes).sum();
    assert!((on_lines + unplaced - alloc.bytes).abs() < 1.0);

    // the harness sorts every frame, in rank
    let issues = app.issues();
    let sort = issues.0.iter().find(|issue| issue.kind == "sort calls").unwrap_or_else(|| panic!("{:#?}", issues.0));
    assert!(sort.title.contains("rank"), "{}", sort.title);
    assert!(sort.evidence[0].starts_with("recorded removable subtotal "), "{}", sort.evidence[0]);
    assert!(sort.upper_ms.unwrap() >= sort.ms.unwrap());
    assert!(sort.evidence[1].contains("1.0 a frame"), "{}", sort.evidence[1]);

    // every view draws, and the Source view shows the harness's own lines
    for view in View::ALL {
        if app.available(view) {
            app.view = view;
            cli::screen(&mut app, 120, 40);
        }
    }
    app.select("rank");
    app.view = View::Source;
    let text = ree_profile_view::export::text(&cli::screen(&mut app, 120, 40));
    assert!(text.contains("table.sort(copy, function(a, b)"), "{text}");
}

/// The game's module the Clone/Destroy harness mounts: a Heartbeat handler that clones a model and destroys the copy,
/// the same Luau whatever the model holds
const SPAWNER: &str = r#"local RunService = game:GetService("RunService")

local Spawner = {}

function Spawner.start(parts)
	local template = Instance.new("Model")
	for index = 1, parts do
		local part = Instance.new("Part")
		part.Name = "Part" .. index
		part.Parent = template
	end
	RunService.Heartbeat:Connect(function()
		local copy = template:Clone()
		copy.Parent = workspace
		copy:Destroy()
	end)
end

return Spawner
"#;

/// The harness: mounts the Spawner as the game's code and starts it with `--context`'s Parts
const SPAWNER_HARNESS: &str = r#"return function(Context)
	local Spawner = require(Context.Mount("ReplicatedStorage/Game/Spawner", Context.SpawnerFile))
	Spawner.start(Context.Parts)
end
"#;

/// The Heartbeat handler's function in a profile.json: the anonymous function of the Spawner module on its `Connect` line
fn heartbeat_handler(profile: &serde_json::Value) -> serde_json::Value {
    let connect_line = SPAWNER.lines().position(|line| line.contains("Heartbeat:Connect(function()")).unwrap() + 1;
    for function in profile["functions"].as_array().unwrap() {
        let in_spawner = function["module"].as_str().is_some_and(|module| module.ends_with(".Spawner"));
        if in_spawner && function["line"] == connect_line {
            return function.clone();
        }
    }
    panic!("the Heartbeat handler is not in the profile");
}

/// Whether a row of `ree profile diff` is an engine call (it runs no Luau instructions) whose work more than doubled and
/// that is listed as an ordinary change
fn is_engine_work_that_grew(row: &serde_json::Value) -> bool {
    let (before, after) = (&row["before"], &row["after"]);
    let runs_no_instructions = before["insns"] == 0.0 && after["insns"] == 0.0;
    let work_before = before["self_work_ms"].as_f64().unwrap();
    let work_after = after["self_work_ms"].as_f64().unwrap();
    runs_no_instructions && work_after > 2.0 * work_before && row["price_only"] == false
}

/// A handler that runs the same Luau, the same calls and instructions, on a model of 10 parts and then of 200 has not
/// moved in price: the engine's charges for what it cloned and destroyed grew with the model. Neither `ree profile
/// diff` nor the viewer's Diff tags it as priced differently on the same path. Version 2 books the engine calls on
/// their own C rows; the handler's total work still includes them.
#[test]
fn a_handler_that_clones_a_larger_model_is_not_priced_differently() {
    let temp = Temp::new("clone-destroy");
    let spawner = temp.path().join("Spawner.luau");
    std::fs::write(&spawner, SPAWNER).unwrap();
    let harness = temp.path().join("Clones.luau");
    std::fs::write(&harness, SPAWNER_HARNESS).unwrap();
    let run = |parts: u32, name: &str| -> std::path::PathBuf {
        let out = temp.path().join(name);
        let context = serde_json::json!({"Parts": parts, "SpawnerFile": spawner.to_str().unwrap()}).to_string();
        assert_eq!(run_engine_with(&harness, &out, &["--context", &context]), 0);
        out
    };
    let read_profile = |dir: &std::path::Path| -> serde_json::Value {
        let text = std::fs::read_to_string(dir.join("profile.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    };
    let small_model_dir = run(10, "before");
    let large_model_dir = run(200, "after");
    let before_json = read_profile(&small_model_dir);
    let after_json = read_profile(&large_model_dir);

    // the handler ran the same Luau both times, and its inclusive work (the engine calls it makes) grew with the model
    let before_handler = heartbeat_handler(&before_json);
    let after_handler = heartbeat_handler(&after_json);
    assert_eq!(before_handler["calls"], after_handler["calls"]);
    assert_eq!(before_handler["self"]["insns"], after_handler["self"]["insns"]);
    let before_total_ms = before_handler["total"]["work_ms"].as_f64().unwrap();
    let after_total_ms = after_handler["total"]["work_ms"].as_f64().unwrap();
    assert!(after_total_ms > 2.0 * before_total_ms, "the engine calls grew with the model: {before_total_ms} -> {after_total_ms}");

    // ree profile diff: the handler's own instructions are unchanged, and the C calls it makes carry the increased engine
    // work in version 2. That work is an ordinary change, not a change of price.
    let (text, json) = roblox_engine_emulator::profiler::diff::compare(&before_json, &after_json, 20);
    let rows = json["functions"].as_array().unwrap();
    assert!(rows.iter().any(is_engine_work_that_grew), "the C calls' increased engine work remains a change: {text}");
    assert!(!text.contains("priced differently, same path"), "{text}");

    // the viewer's Diff: the same rows, not tagged
    let app = cli::open(&[small_model_dir, large_model_dir], ThemeKind::Dark).unwrap();
    let pairs = ree_profile_view::diff::functions(&app, Metric::Work);
    let engine_work_grew = |pair: &ree_profile_view::diff::Pair| pair.insns == Some((0.0, 0.0)) && pair.after > 2.0 * pair.before && !pair.price_only;
    assert!(pairs.iter().any(engine_work_grew), "{pairs:#?}");
    for pair in &pairs {
        let is_spawner_handler = pair.label.starts_with("anonymous") && pair.label.contains("Spawner");
        assert!(!(is_spawner_handler && pair.price_only), "{pairs:#?}");
    }
}

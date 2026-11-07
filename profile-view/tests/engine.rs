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
for index = 1, 50 do
	items[index] = { score = (index * 37) % 50 }
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
    let args: Vec<String> = ["--harness", harness.to_str().unwrap(), "--seconds", "1", "--profile", "all", "--profile-out", out.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
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
    // (profile.json writes each node's work in whole microseconds: half a microsecond a node either way)
    let tree = app.tree_of(0, Metric::Work, false);
    let booked = work.total_ms - work.outside_instructions_ms;
    let rounding = p.raw.tree.len() as f64 * 0.0005;
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
    let sort = issues.0.iter().find(|issue| issue.kind == "sort every frame").unwrap_or_else(|| panic!("{:#?}", issues.0));
    assert!(sort.title.contains("rank"), "{}", sort.title);
    assert!(sort.evidence[0].contains("1.0 a frame"), "{}", sort.evidence[0]);

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

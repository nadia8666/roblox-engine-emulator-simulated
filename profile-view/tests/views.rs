//! Every view and the key paths through them, drawn headless at a fixed size and compared with snapshots
//! (tests/snapshots; REE_VIEW_BLESS=1 rewrites them), and the files `render` writes: text, SVG and PNG.

mod common;

use common::*;
use ree_profile_view::{
    app::View,
    cli,
    export::{self, CellFont, Format},
    metric::Metric,
    theme::{Mark, Theme, ThemeKind},
};

const COLS: u16 = 120;
const ROWS: u16 = 40;

#[test]
fn every_view_matches_its_snapshot() {
    let temp = Temp::new("views");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    for view in [View::Overview, View::Flame, View::Calls, View::Source, View::Alloc, View::Heap, View::Timeline, View::Caches, View::Issues] {
        let mut app = open(&[&dir]);
        app.view = view;
        let text = screen(&mut app, "", COLS, ROWS);
        snapshot(&format!("view-{}", view.name()), &text, temp.path());
    }
}

/// (snapshot, key script): each a path a user takes, from the Overview
const PATHS: &[(&str, &str)] = &[
    // the first issue (a sort every frame) opens its function in Calls
    ("overview-enter", "enter"),
    // the flame starts on the heaviest phase; down its heaviest callees twice, enter zooms in there, esc back out
    ("flame-zoom", "2 down down enter"),
    ("flame-zoom-out", "2 down down enter esc"),
    // Calls on rank (found by name): down to its callee (sort), enter centres on it, esc back to rank
    ("calls-centre", "3 /rank down down enter"),
    ("calls-back", "3 /rank down down enter esc"),
    // Source lists lines by heat with s
    ("source-hot", "4 s"),
    // Allocations sorted by bytes a frame in the steady state, then enter opens the site's line
    ("alloc-steady", "5 s s"),
    ("alloc-enter", "5 enter"),
    // the Heap's next pane and the snapshot before the last
    ("heap-pane", "6 tab left"),
    // the Timeline: three frames left of the worst one, then zoomed in
    ("timeline-move", "7 left left left"),
    ("timeline-zoom", "7 enter"),
    // Caches by miss rate
    ("caches-rate", "8 s"),
    // Issues: the second one opened
    ("issues-open", "9 down enter"),
    // the metric switched to allocated bytes on the Flame, and to instructions on the Overview
    ("flame-alloc", "2 m m m"),
    ("overview-insns", "m"),
    // a search in the Flame finds compare's node
    ("flame-search", "2 /anonymous n"),
    // scripts only folds C functions into their callers
    ("flame-scripts", "2 f"),
    // the help over the Flame
    ("flame-help", "2 ?"),
    // the light theme draws the same text (colours are the PNG test's)
    ("overview-light", "t"),
];

#[test]
fn key_paths_match_their_snapshots() {
    let temp = Temp::new("paths");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    for (name, keys) in PATHS {
        let mut app = open(&[&dir]);
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("path-{name}"), &text, temp.path());
    }
}

#[test]
fn a_pair_opens_on_the_diff_and_its_tables() {
    let temp = Temp::new("pair");
    let before = write_profile(&temp.path().join("before"), Shape::BEFORE);
    let after = write_profile(&temp.path().join("after"), Shape::AFTER);
    for (name, keys) in [("diff-functions", ""), ("diff-lines", "tab"), ("diff-sites", "tab tab"), ("diff-grew", "s"), ("diff-enter", "enter"), ("diff-side", "x 1")] {
        let mut app = open(&[&before, &after]);
        assert_eq!(app.view, View::Diff, "a pair starts on the diff");
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("pair-{name}"), &text, temp.path());
    }
}

/// A comparison from ree profile try's version 2: a gap against the baseline gets ▲▼ and colour only where compare.json's
/// verdict calls it clear (host time within noise reads `≈ noise`, not "better"), a variant whose result differs by digest
/// is ✗ and named in the header, never ◆, and the ratio plot's log axis labels only positive ratios
#[test]
fn a_comparison_colours_only_clear_gaps_and_blocks_a_differing_result() {
    let temp = Temp::new("compare-v2");
    let dir = write_comparison(&temp.path().join("what-if"));
    let path = dir.join("compare.json");
    let mut compare: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    compare["version"] = serde_json::json!(2);
    compare["check"] = serde_json::json!({"ran": true, "by": "digest", "ok": false, "failures": [{"variant": "t[k][v] off", "size": 1000, "message": "its result's digest is 0badd16e"}]});
    // a third variant computing something else (its digest), and the fastest at n = 1000 by work: it must not win
    let mut off = compare["runs"][4].clone();
    off["variant"] = serde_json::json!("t[k][v] off");
    off["digest"] = serde_json::json!("0badd16e");
    off["check"] = serde_json::json!({"ok": false, "message": "its result's digest is 0badd16e, the baseline's 9f3c0a21"});
    off["run"]["work_ms"] = serde_json::json!(0.001);
    compare["runs"].as_array_mut().unwrap().push(off);
    compare["variants"].as_array_mut().unwrap().push(serde_json::json!({"name": "t[k][v] off", "slug": "t-k-v-off", "kind": "function", "source": "nested-keys.luau:30"}));
    // the baseline verdicts at n = 1000: allocated clear, host time within noise, the work clock the same
    // (a verdict's ratio is the variant's figure over the baseline's, oriented so above 1 is worse)
    let verdicts = [("alloc_bytes", 1.0 / 3.0, true), ("host_ms", 0.82, false), ("work_ms", 1.0, false), ("cache_hit_rate", 0.96, true)];
    for (metric, ratio, clear) in verdicts {
        let entry = serde_json::json!({"variant": "t[v][k]", "ratio": ratio, "clear": clear, "material": true, "differs": false, "rounds": if metric == "host_ms" { serde_json::json!({"won": 4, "of": 9, "noise": 0.05}) } else { serde_json::Value::Null }});
        let winners = compare["winners"].as_array_mut().unwrap();
        match winners.iter_mut().find(|w| w["metric"] == metric && w["size"] == 1000) {
            Some(winner) => winner["baseline"] = serde_json::json!([entry]),
            None => winners.push(serde_json::json!({"metric": metric, "size": 1000, "winner": "t[v][k]", "value": 0.0, "clear": clear, "baseline": [entry]})),
        }
    }
    std::fs::write(&path, serde_json::to_string(&compare).unwrap()).unwrap();
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "down", 160, 48);
    assert!(text.contains("✗ results differ from t[k][v]'s (by digest: no check function): t[k][v] off at n = 1,000"), "{text}");
    let off_row = text.lines().find(|line| line.contains("■ t[k][v] off")).unwrap_or_default();
    assert!(off_row.contains('✗') && !off_row.contains('◆'), "a differing result wins nothing: {off_row}");
    let line_of = |label: &str| text.lines().find(|line| line.contains(label) && line.contains('x')).unwrap_or_default().to_string();
    assert!(line_of("allocated").contains("▼ better"), "a clear gap keeps its verdict: {}", line_of("allocated"));
    assert!(line_of("host time").contains("≈ noise") && !line_of("host time").contains("better"), "noise gets no verdict: {}", line_of("host time"));
    assert!(line_of("work clock  ").contains("= same") || line_of("work clock  ").contains("1.00x"), "{}", line_of("work clock  "));
    // (the ratio plot's labels: a log axis has no negative ratio)
    assert!(!text.contains("-0.") && !text.lines().any(|line| line.contains("x│") && line.contains("│ -")), "{text}");
    // a figure with no better side (the collector's host time, measured: compare.json weighs no verdict on it) shows its
    // ratio against the baseline, `~` and no verdict, where its column was blank: beside the other figures, and in the
    // table on that figure
    let collector = text.lines().find(|line| line.contains("collector")).unwrap_or_default();
    assert!(collector.contains('~') && (collector.contains("more") || collector.contains("less")) && !collector.contains('≈'), "{collector}");
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "M down", 160, 48);
    let row = text.lines().find(|line| line.contains("■ t[v][k]")).unwrap_or_default();
    assert!(row.contains('~') && (row.contains("more") || row.contains("less")) && !row.contains('▲') && !row.contains('▼'), "{row}\n{text}");
}

/// A diff pairs an anonymous function an edit moved (its key carries its line; its code hash does not) as one function,
/// and tags one that ran the same calls and instructions `price only`, with ≈ for its mark, only when its cold and
/// first-use charges account for its change (the work clock's split); without the split it is a change like any other.
/// A profile from before run.harness_hash reads the same harness as one after it. Host time's tables name no verdict:
/// each change is marked `~` in no colour
#[test]
fn a_diff_pairs_a_moved_function_and_tags_a_price_move() {
    let temp = Temp::new("pair-moved");
    // (cold: the first and second functions' cold and first-use charges, when the profile splits them; hashed: whether
    // the profile carries run.harness_hash, else only source_hashes as a ree before it wrote)
    let write = |name: &str, handler_line: i64, first_work: f64, second_work: f64, edited_insns: f64, cold: Option<(f64, f64)>, hashed: bool| -> std::path::PathBuf {
        let dir = temp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let function = |id: i64, name: &str, line: i64, code: &str, work: f64, calls: u64, insns: f64, cold: Option<f64>| {
            let key = if name.is_empty() { format!("Game:anonymous@{line}") } else { format!("Game:{name}") };
            let mut f = serde_json::json!({"id": id, "key": key, "name": name, "module": "Game", "line": line, "last_line": line + 4, "kind": "Luau", "code_hash": code, "source_hash": format!("s{code}"),
                "calls": calls, "self": {"insns": insns, "work_ms": work}, "total": {"insns": insns, "work_ms": work}, "harness": false,
                "phases": {"frames": {"self_work_ms": work, "total_work_ms": work, "calls": calls, "insns": insns, "harness_self_work_ms": 0.0}}});
            if let Some(cold) = cold {
                f["self"]["cold_work_ms"] = serde_json::json!(cold);
                f["phases"]["frames"]["cold_work_ms"] = serde_json::json!(cold);
            }
            f
        };
        let mut profile = serde_json::json!({
            "version": 1, "kinds": ["work", "time"], "run": {"status": "ok", "frames": 10, "harness": "Fixture.luau"}, "phases": ["frames"],
            "scope": {"phases": ["frames"], "game_self_ms": 1.0, "harness_self_ms": 0.0}, "work": {"total_ms": 1.0, "per_frame_ms": 0.1, "script_insns": 1000.0},
            "functions": [
                function(0, "", handler_line, "c0ffee", 0.5, 10, 400.0, None),
                function(1, "first", 30, "beef01", first_work, 10, 200.0, cold.map(|c| c.0)),
                function(2, "second", 40, "beef02", second_work, 10, 200.0, cold.map(|c| c.1)),
                function(3, "", handler_line + 10, if edited_insns > 100.0 { "f00d02" } else { "f00d01" }, edited_insns / 1000.0, 10, edited_insns, None),
            ],
            "source_hashes": {"Game": if handler_line == 12 { "1" } else { "2" }, "ReplicatedStorage.RURFixtures.Fixture": "aaaa"},
        });
        if hashed {
            profile["run"]["harness_hash"] = serde_json::json!("aaaa");
        }
        std::fs::write(dir.join("profile.json"), serde_json::to_string(&profile).unwrap()).unwrap();
        std::fs::write(dir.join("summary.txt"), "ree profile: run ok; kinds work\n").unwrap();
        // host time, sampled: first and second move, and a function only the after profile sampled
        let time = if handler_line == 12 { "frames;first Game:30 @31 2.0\nframes;second Game:40 @41 1.0\n" } else { "frames;first Game:30 @31 3.0\nframes;second Game:40 @41 0.5\nframes;extra Game:60 @61 1.0\n" };
        std::fs::write(dir.join("time.folded"), time).unwrap();
        dir
    };
    // the handler moves from :12 to :15 unchanged; first and second swap who pays 0.8 ms of cold charges; the handler
    // below it is edited (100 instructions to 1,000)
    let before = write("before", 12, 0.9, 0.1, 100.0, Some((0.85, 0.05)), false);
    let after = write("after", 15, 0.1, 0.9, 1000.0, Some((0.05, 0.85)), true);
    let mut app = open(&[&before, &after]);
    assert_eq!(app.view, View::Diff);
    let text = screen(&mut app, "", 160, 40);
    assert!(text.contains("moved from :22 to :25"), "the edited handler pairs as one function: {text}");
    assert!(!text.contains("anonymous  Game:12") && !text.contains("anonymous  Game:15 "), "the unchanged moved handler is no row (it did not change): {text}");
    let first = text.lines().find(|line| line.contains("first  Game:30")).unwrap_or_default();
    assert!(first.contains("price only") && first.contains('≈') && !first.contains('▼'), "a price move the cold split shows is tagged, not a verdict: {first}");
    assert!(!text.contains("other harnesses"), "the same harness by content, the before profile from before run.harness_hash: {text}");
    // scripts only folds C functions into their callers: the cold split no longer covers a row's work, and it says so
    let text = screen(&mut app, "f", 160, 40);
    assert!(text.contains("no function is tagged price only"), "{text}");
    let first = text.lines().find(|line| line.contains("first  Game:30")).unwrap_or_default();
    assert!(!first.contains("price only") && !first.contains('≈'), "{first}");

    // the same calls and instructions without the split: no tag, a change with its verdict
    let before = write("before-plain", 12, 0.9, 0.1, 100.0, None, true);
    let after = write("after-plain", 15, 0.1, 0.9, 1000.0, None, true);
    let mut app = open(&[&before, &after]);
    let text = screen(&mut app, "", 160, 40);
    let first = text.lines().find(|line| line.contains("first  Game:30")).unwrap_or_default();
    assert!(!first.contains("price only") && !first.contains('≈') && first.contains('▼'), "no evidence, no tag: {first}");
    // a cold split that leaves most of the change unexplained: no tag either
    let after = write("after-unexplained", 15, 0.1, 0.9, 1000.0, Some((0.80, 0.10)), true);
    let before = write("before-unexplained", 12, 0.9, 0.1, 100.0, Some((0.85, 0.05)), true);
    let pairs = ree_profile_view::diff::functions(&open(&[&before, &after]), Metric::Work);
    let first = pairs.iter().find(|pair| pair.label.starts_with("first")).unwrap();
    assert!(!first.price_only, "{first:#?}");

    // host time: every change `~`, none ▲ or ▼, a function only the after profile sampled too
    app.set_metric(Metric::Time);
    let text = screen(&mut app, "", 160, 40);
    assert!(text.contains("~ measured: no verdict"), "{text}");
    for name in ["first  Game:30", "second  Game:40", "extra  Game:60"] {
        let row = text.lines().find(|line| line.contains(name)).unwrap_or_else(|| panic!("{name}: {text}"));
        let figures = row.split(name).nth(1).unwrap_or_default();
        assert!(!row.contains('▲') && !row.contains('▼') && figures.contains(" ~ "), "{row}");
    }
}

/// Two profiles of host time alone: a function that ran in both is never "new" or "gone" because the samples missed it in
/// one (each ran 60 times both times), and the instructions such a profile never counts read "not counted", not 0
#[test]
fn a_host_time_diff_names_no_function_new_or_gone_that_ran_in_both() {
    let temp = Temp::new("time-pair");
    let write = |name: &str, folded: &str| -> std::path::PathBuf {
        let dir = temp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let function = |id: i64, name: &str, line: i64| {
            let key = if name.is_empty() { format!("Game:anonymous@{line}") } else { format!("Game:{name}") };
            serde_json::json!({"id": id, "key": key, "name": name, "module": "Game", "line": line, "last_line": line + 4, "kind": "Luau",
                "calls": 60, "self": {"time_ms": 0.0}, "total": {"time_ms": 0.0}, "harness": false})
        };
        let profile = serde_json::json!({
            "version": 1, "kinds": ["time"], "run": {"status": "ok", "frames": 60, "harness": "Fixture.luau", "harness_hash": "aaaa"}, "phases": ["frames"],
            "time": {"sampled_ms": 5.0}, "functions": [function(0, "tick", 10), function(1, "", 20)],
        });
        std::fs::write(dir.join("profile.json"), serde_json::to_string(&profile).unwrap()).unwrap();
        std::fs::write(dir.join("summary.txt"), "ree profile: run ok; kinds time\n").unwrap();
        std::fs::write(dir.join("time.folded"), folded).unwrap();
        dir
    };
    // (the samples caught tick only before, the handler only after)
    let before = write("before", "frames;tick Game:10 @11 3.0\n");
    let after = write("after", "frames;anonymous Game:20 @21 2.0\n");
    let mut app = open(&[&before, &after]);
    app.set_metric(Metric::Time);
    let text = screen(&mut app, "", 160, 40);
    for name in ["tick  Game:10", "anonymous  Game:20"] {
        let row = text.lines().find(|line| line.contains(name)).unwrap_or_else(|| panic!("{name}: {text}"));
        let figures = row.split(name).nth(1).unwrap_or_default();
        assert!(!figures.contains("new") && !figures.contains("gone"), "it ran in both: {row}");
        assert!(figures.contains("not counted") && figures.contains("60"), "{row}");
    }
}

/// A function whose work is the same both times is no row however its last bits fell, by the rule `ree profile diff`
/// uses (0.039 ms against 0.039 ms and 6.9e-18), and 1 ns or more is a move
#[test]
fn float_noise_is_no_move_in_the_diff() {
    let temp = Temp::new("noise-pair");
    let write = |name: &str, work: f64| -> std::path::PathBuf {
        let dir = temp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let profile = serde_json::json!({
            "version": 1, "kinds": ["work"], "run": {"status": "ok", "frames": 10, "harness": "Fixture.luau", "harness_hash": "aaaa"}, "phases": ["frames"],
            "scope": {"phases": ["frames"], "game_self_ms": work, "harness_self_ms": 0.0}, "work": {"total_ms": work, "per_frame_ms": work / 10.0, "script_insns": 100.0},
            "functions": [{"id": 0, "key": "Cloner:build", "name": "build", "module": "Cloner", "line": 6, "last_line": 10, "kind": "Luau", "calls": 10,
                "self": {"insns": 100.0, "work_ms": work}, "total": {"insns": 100.0, "work_ms": work}, "harness": false,
                "phases": {"frames": {"self_work_ms": work, "total_work_ms": work, "calls": 10, "insns": 100, "harness_self_work_ms": 0.0}}}],
        });
        std::fs::write(dir.join("profile.json"), serde_json::to_string(&profile).unwrap()).unwrap();
        std::fs::write(dir.join("summary.txt"), "ree profile: run ok; kinds work\n").unwrap();
        dir
    };
    let work = 0.039f64;
    let noisy = f64::from_bits(work.to_bits() + 1);
    let (before, after) = (write("before", work), write("after", noisy));
    let pairs = ree_profile_view::diff::functions(&open(&[&before, &after]), Metric::Work);
    assert!(pairs.is_empty(), "{pairs:#?}");
    let after = write("after-ns", work + 2e-6);
    assert_eq!(ree_profile_view::diff::functions(&open(&[&before, &after]), Metric::Work).len(), 1);
}

/// What a profile of the `Walker:walk` function holds for a diff test: the profile's version, the cold and first-use part of
/// its work, and the bytes it allocated
struct WalkerRun {
    version: u32,
    cold_ms: f64,
    allocated_bytes: u64,
}

/// Writes profile.json and alloc.folded for a run of `Walker:walk`: 5 calls and 300 instructions, one warm millisecond of work
/// plus the cold part, split across two call-tree nodes (two contexts it ran in) whose calling lines `tree_lines` gives
fn write_walker_profile(dir: &std::path::Path, run: &WalkerRun) {
    std::fs::create_dir_all(dir).unwrap();
    let warm_ms = 1.0;
    let work_ms = warm_ms + run.cold_ms;
    let profile = serde_json::json!({
        "version": run.version,
        "kinds": ["work", "alloc"],
        "phases": ["run"],
        "run": {"status": "ok", "frames": 10},
        "work": {"total_ms": work_ms, "per_frame_ms": work_ms / 10.0, "script_insns": 300},
        "alloc": {"bytes": run.allocated_bytes, "objects": 1},
        "functions": [{
            "id": 7, "key": "Walker:walk", "name": "walk", "module": "Walker", "line": 4,
            "kind": "Luau", "harness": false, "calls": 5, "code_hash": "c1", "source_hash": "s1",
            "self": {"work_ms": work_ms, "insns": 300, "cold_work_ms": run.cold_ms},
            "total": {"work_ms": work_ms}
        }],
        "tree": [
            [-1, -1, 0, 0.0, 0, "run"],
            [0, 7, 2, work_ms * 400.0, 100, null],
            [0, 7, 3, work_ms * 600.0, 200, null]
        ],
        "tree_lines": [-1, 12, 18],
        "source_hashes": {"Walker": "s1"}
    });
    std::fs::write(dir.join("profile.json"), profile.to_string()).unwrap();
    std::fs::write(dir.join("alloc.folded"), format!("run;walk Walker:4 @12 {}
", run.allocated_bytes)).unwrap();
}

/// Two version 2 profiles of a function that ran in two contexts pair into one row: the contexts add up, the calls and
/// instructions are the same, and the exact cold split covers the change in work, so the row is tagged as priced
/// differently. Against a version 1 profile the work, calls and instructions are left out, and the allocations compare.
#[test]
fn version_two_diffs_pair_contexts_and_keep_cross_version_allocations() {
    let temp = Temp::new("contexts-pair");
    let before = temp.path().join("before");
    let after = temp.path().join("after");
    let older = temp.path().join("older");
    write_walker_profile(&before, &WalkerRun { version: 2, cold_ms: 2.0, allocated_bytes: 100 });
    write_walker_profile(&after, &WalkerRun { version: 2, cold_ms: 3.0, allocated_bytes: 200 });
    write_walker_profile(&older, &WalkerRun { version: 1, cold_ms: 2.0, allocated_bytes: 100 });

    let app = open(&[&before, &after]);

    // the calling lines come from tree_lines, one for each context node
    let nodes = app.profiles[1].raw_nodes();
    assert_eq!(nodes[1].line, Some(12));
    assert_eq!(nodes[2].line, Some(18));
    let rows = ree_profile_view::diff::functions(&app, Metric::Work);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].calls, Some((5, 5)));
    assert_eq!(rows[0].insns, Some((300.0, 300.0)));
    assert!(rows[0].price_only, "context sums and the function's exact cold split cover the same work: {rows:#?}");

    let app = open(&[&older, &after]);

    assert!(ree_profile_view::diff::functions(&app, Metric::Work).is_empty());
    let rows = ree_profile_view::diff::functions(&app, Metric::AllocBytes);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!((rows[0].before, rows[0].after), (100.0, 200.0));
    assert!(rows[0].calls.is_none() && rows[0].insns.is_none() && !rows[0].price_only);
}

#[test]
fn a_comparison_opens_on_compare_and_its_runs_open() {
    let temp = Temp::new("compare");
    let dir = write_comparison(&temp.path().join("what-if"));
    for (name, keys) in [("compare", ""), ("compare-alloc", "m m"), ("compare-size", "left down"), ("compare-open", "down enter"), ("compare-diff", "down D"), ("compare-issues", "9")] {
        let mut app = open(&[&dir]);
        assert_eq!(app.view, View::Compare, "a what-if folder starts on Compare");
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("compare-{name}"), &text, temp.path());
    }
    // the baseline's run at the largest size is open in the other views
    let mut app = open(&[&dir]);
    assert!(app.has_profile());
    assert_eq!(app.profile().raw.run.frames, 120);
    // enter on a run with no profile folder says so and stays
    let text = screen(&mut app, "left enter", COLS, ROWS);
    assert_eq!(app.view, View::Compare, "{text}");
    assert!(text.contains("no profile folder"), "{text}");
}

#[test]
fn the_same_screen_draws_the_same_bytes() {
    let temp = Temp::new("determinism");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let draw = || {
        let mut app = open(&[&dir]);
        app.view = View::Flame;
        cli::screen(&mut app, COLS, ROWS)
    };
    let (a, b) = (draw(), draw());
    let theme = Theme::new(ThemeKind::Dark);
    for format in [Format::Png, Format::Svg, Format::Text] {
        assert_eq!(export::encode(&a, &theme, format, 1).unwrap(), export::encode(&b, &theme, format, 1).unwrap(), "{format:?}");
    }
}

/// The PNG is the cells rasterized: its size is the grid's, a cell's background and a bar's colour are where the buffer
/// puts them, and a bar is drawn as a band through the middle of its cell (a gap above and below)
#[test]
fn the_png_is_the_buffer_rasterized() {
    let temp = Temp::new("png");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    let buf = cli::screen(&mut app, COLS, ROWS);
    let theme = app.theme.clone();
    let bytes = export::encode(&buf, &theme, Format::Png, 1).unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let (cw, ch) = (export::CELL_W, export::CELL_H);
    assert_eq!((info.width, info.height), (COLS as u32 * cw, ROWS as u32 * ch));
    let pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let at = ((y * info.width + x) * 3) as usize;
        (pixels[at], pixels[at + 1], pixels[at + 2])
    };
    let rgb = |color: ratatui::style::Color| match color {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => panic!("the themes use RGB colours"),
    };
    // the brand's pill, at the first cell's corner
    assert_eq!(pixel(0, 0), rgb(theme.accent));
    // a full bar cell: the bar's colour across the middle of the cell, its row's background above and below it
    let (bx, by) = (0..ROWS)
        .flat_map(|y| (0..COLS).map(move |x| (x, y)))
        .find(|&(x, y)| buf[(x, y)].symbol() == "█" && Theme::unmark(buf[(x, y)].bg).1 == Mark::Bar)
        .expect("a bar on the Overview");
    let fg = rgb(buf[(bx, by)].fg);
    let row_bg = rgb(Theme::unmark(buf[(bx, by)].bg).0);
    let (x0, y0) = (bx as u32 * cw, by as u32 * ch);
    assert_eq!(pixel(x0 + cw / 2, y0 + ch / 2), fg, "the bar's middle");
    assert_eq!(pixel(x0 + cw / 2, y0 + 1), row_bg, "a gap above the bar");
    assert_eq!(pixel(x0 + cw / 2, y0 + ch - 2), row_bg, "a gap below the bar");
    // a letter's cell has ink in it
    let (lx, ly) = (0..COLS).map(|x| (x, 0)).find(|&(x, y)| buf[(x, y)].symbol() == "r").unwrap();
    let ink = rgb(buf[(lx, ly)].fg);
    let inked = (0..cw).flat_map(|dx| (0..ch).map(move |dy| (dx, dy))).filter(|&(dx, dy)| pixel(lx as u32 * cw + dx, ly as u32 * ch + dy) == ink).count();
    assert!(inked >= 3, "the r of the header is drawn ({inked} pixels in ink)");
    // and twice as large at scale 2
    let bytes = export::encode(&buf, &theme, Format::Png, 2).unwrap();
    let info = png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().unwrap().info().clone();
    assert_eq!((info.width, info.height), (COLS as u32 * cw * 2, ROWS as u32 * ch * 2));
}

/// Every character any view draws is in the bundled font, or drawn as a shape (blocks, box lines, braille)
#[test]
fn every_character_the_views_draw_has_a_glyph() {
    let temp = Temp::new("glyphs");
    let before = write_profile(&temp.path().join("before"), Shape::BEFORE);
    let after = write_profile(&temp.path().join("after"), Shape::AFTER);
    let what_if = write_comparison(&temp.path().join("what-if"));
    let mut seen = std::collections::BTreeSet::new();
    let mut take = |app: &mut ree_profile_view::app::App, keys: &str| {
        let text = screen(app, keys, COLS, ROWS);
        seen.extend(text.chars().filter(|c| *c != '\n'));
    };
    for view in View::ALL {
        let mut app = open(&[&before]);
        if app.available(view) {
            app.view = view;
            take(&mut app, "");
            take(&mut app, "?");
        }
    }
    let mut pair = open(&[&before, &after]);
    take(&mut pair, "");
    take(&mut pair, "tab tab");
    let mut compare = open(&[&what_if]);
    take(&mut compare, "");
    take(&mut compare, "9");
    let font = CellFont::new(1);
    let missing: Vec<char> = seen.into_iter().filter(|c| !font.covers(*c)).collect();
    assert!(missing.is_empty(), "characters the font lacks: {missing:?}");
}

#[test]
fn render_writes_a_file_a_view_and_prints_each() {
    let temp = Temp::new("render");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let out = temp.path().join("shots");
    let args: Vec<String> = [dir.to_str().unwrap(), "--view", "all", "--out", out.to_str().unwrap(), "--size", "100x30"].iter().map(|s| s.to_string()).collect();
    assert_eq!(cli::render("ree profile", &args), 0);
    for view in ["overview", "flame", "calls", "source", "alloc", "heap", "timeline", "caches", "issues"] {
        let file = out.join(format!("{view}.png"));
        assert!(file.is_file(), "{}", file.display());
    }
    assert!(!out.join("diff.png").exists(), "one profile has no diff");
    // one view to text and to SVG, by the file's extension
    for (file, check) in [("issues.txt", "Sort calls"), ("flame.svg", "<svg ")] {
        let path = temp.path().join(file);
        let view = file.split('.').next().unwrap();
        let args: Vec<String> = [dir.to_str().unwrap(), "--view", view, "--out", path.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
        assert_eq!(cli::render("ree profile", &args), 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(check), "{file}: {text}");
    }
    // what it refuses, before it writes anything
    for (args, error) in [
        (vec![dir.to_str().unwrap(), "--view", "flame"], "--out"),
        (vec![dir.to_str().unwrap(), "--view", "diff", "--out", "x.png"], "two profiles"),
        (vec![dir.to_str().unwrap(), "--view", "nope", "--out", "x.png"], "no view"),
        (vec![dir.to_str().unwrap(), "--metric", "nope", "--out", "x.png"], "no metric"),
        (vec![dir.to_str().unwrap(), "--select", "nothing_like_this", "--out", "x.png"], "--select"),
        (vec![dir.to_str().unwrap(), "--view", "all", "--out", "x.png"], "a folder"),
        (vec![dir.to_str().unwrap(), "--size", "10x5", "--out", "x.png"], "--size"),
        (vec![temp.path().join("nothing").to_str().unwrap(), "--out", "x.png"], "no profile in"),
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        assert_eq!(cli::render("ree profile", &args), 2, "{args:?} ({error})");
    }
    assert!(!std::path::Path::new("x.png").exists());
}

#[test]
fn select_finds_a_function_or_a_line() {
    let temp = Temp::new("select");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    assert!(app.select("layout"));
    let focus = app.focus.unwrap();
    assert_eq!(app.profile().funcs[focus.func as usize].name, "layout");
    let mut app = open(&[&dir]);
    assert!(app.select("Main.luau:21"));
    let focus = app.focus.unwrap();
    assert_eq!((app.profile().funcs[focus.func as usize].name.as_str(), focus.line), ("rank", Some(21)));
    app.view = View::Source;
    let text = screen(&mut app, "", COLS, ROWS);
    assert!(text.lines().any(|line| line.contains("▌") && line.contains("21")), "the line is selected:\n{text}");
    assert!(!open(&[&dir]).select("nothing_like_this"));
}

#[test]
fn the_games_code_leads_and_the_harnesss_comes_apart() {
    use ree_profile_view::ui::overview::{Side, sides};
    let temp = Temp::new("sides");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "", COLS, ROWS);
    snapshot("harness-overview", &text, temp.path());
    assert!(text.contains("The game's code") && text.contains("The harness's code"), "{text}");
    // the steady phase's self work: the game's code 10.63 ms (rank's call from the harness is the game's: a script
    // function is its module's side), the harness's 4.000 ms (its scan, a C function, goes with the harness)
    let labels = |app: &ree_profile_view::app::App, side: &Side| side.rows.iter().map(|r| (app.profile().funcs[r.func as usize].label(), (r.own * 1e6).round() / 1e6)).collect::<Vec<_>>();
    let got = sides(&app);
    assert!(got.apart);
    assert!((got.lead.total - 10.63).abs() < 1e-9 && (got.harness.total - 4.0).abs() < 1e-9, "{} {}", got.lead.total, got.harness.total);
    assert_eq!(labels(&app, &got.harness), [("GetDescendants [C]".to_string(), 2.5), ("watch  Fixture:5".to_string(), 1.5)]);
    assert_eq!(labels(&app, &got.lead)[..3], [("anonymous  Main:22".to_string(), 4.0), ("rank  Main:20".to_string(), 3.03), ("layout  Main:30".to_string(), 2.0)]);
    // a profile from before ree c692e40 (no harness flags, no figures per phase, no scope) reads the same from its call
    // tree, its steady phase and the harness's folder
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for f in json["functions"].as_array_mut().unwrap() {
        let f = f.as_object_mut().unwrap();
        f.remove("harness");
        f.remove("phases");
    }
    json.as_object_mut().unwrap().remove("scope");
    let old = temp.path().join("old");
    std::fs::create_dir_all(&old).unwrap();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), old.join(entry.file_name())).unwrap();
    }
    std::fs::write(old.join("profile.json"), json.to_string()).unwrap();
    let older = open(&[&old]);
    let from_tree = sides(&older);
    assert!(from_tree.apart);
    assert_eq!((from_tree.lead.total * 1e6).round(), (got.lead.total * 1e6).round());
    assert_eq!(labels(&older, &from_tree.harness), labels(&app, &got.harness));
    assert_eq!(labels(&older, &from_tree.lead), labels(&app, &got.lead));
}

#[test]
fn what_the_scripts_asked_the_engine_to_do_reads_by_phase() {
    // the steady phase's counts are its mark's to the run's end: 100 clones, 3,000 writes (2,400 changed, 600 moving a
    // part), 50 emits; the change clones a tenth and writes less
    let temp = Temp::new("engine");
    let before = write_profile(&temp.path().join("before"), Shape::BEFORE);
    let after = write_profile(&temp.path().join("after"), Shape::AFTER);
    let mut app = open(&[&before]);
    let ops = app.profile().engine_ops(&app.profile().scope()).expect("the counts");
    assert_eq!((ops["cloned"], ops["writes"], ops["changed"], ops["geometry"], ops["emits"]), (100.0, 3000.0, 2400.0, 600.0, 50.0));
    let every = app.profile().scope_of(&[]);
    assert_eq!(app.profile().engine_ops(&every).unwrap()["cloned"], 140.0, "the mount's 40 and the steady state's 100");
    let text = screen(&mut app, "", 200, ROWS);
    assert!(text.contains("engine  phase steady  100 cloned · 100 destroyed · 100 into the Workspace · 3,000 writes (2,400 changed: 600 moved a part, 20 switched an effect) · 10 pivots · 50 emits · 200 queries"), "{text}");
    // the diff compares them in the phases both report, as ree profile diff groups them
    let mut pair = open(&[&before, &after]);
    let text = screen(&mut pair, "", 160, 48);
    for row in ["engine: instances made", "engine: property writes", "engine: parts moved", "engine: pivots"] {
        assert!(text.contains(row), "{row}: {text}");
    }
    assert!(text.contains("the first figure, the engine's counts and the functions' work cover phase steady"), "{text}");
    // a profile from before the counts shows none
    let path = before.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for mark in json["marks"].as_array_mut().unwrap() {
        mark.as_object_mut().unwrap().remove("engine");
    }
    json.as_object_mut().unwrap().remove("end");
    std::fs::write(&path, json.to_string()).unwrap();
    let mut older = open(&[&before]);
    assert!(older.profile().engine_ops(&older.profile().scope()).is_none());
    assert!(!screen(&mut older, "", COLS, ROWS).contains("◇ engine"));
}

/// A profile's JSON changed by `edit`, in a folder of its own
fn edited(temp: &Temp, name: &str, shape: Shape, edit: impl FnOnce(&mut serde_json::Value)) -> std::path::PathBuf {
    let dir = write_profile(&temp.path().join(name), shape);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut json);
    std::fs::write(&path, json.to_string()).unwrap();
    dir
}

#[test]
fn the_header_marks_only_a_failure_and_says_what_was_not_measured() {
    let temp = Temp::new("header");
    // a harness still waiting when the run ends (`yielded`) is a run's usual end: no ✗
    for (status, mark) in [("ok", "✓ run ok"), ("yielded", "○ run yielded"), ("error", "✗ run error")] {
        let dir = edited(&temp, status, Shape::BEFORE, |json| json["run"]["status"] = serde_json::json!(status));
        let text = screen(&mut open(&[&dir]), "", COLS, ROWS);
        assert!(text.lines().next().unwrap().contains(mark), "{status}: {}", text.lines().next().unwrap());
    }
    // without the work kind: the frames' work clock where the timeline has it, else not measured, never 0 ms
    let no_work = |json: &mut serde_json::Value| {
        let object = json.as_object_mut().unwrap();
        for key in ["work", "tree", "luau_caches", "alloc"] {
            object.remove(key);
        }
        json["kinds"] = serde_json::json!(["timeline"]);
    };
    let dir = edited(&temp, "timeline", Shape::BEFORE, no_work);
    let header = screen(&mut open(&[&dir]), "", COLS, ROWS).lines().next().unwrap().to_string();
    assert!(header.contains("Studio work") && header.contains("in frames") && !header.contains(" 0 ms"), "{header}");
    let dir = edited(&temp, "nothing", Shape::BEFORE, |json| {
        no_work(json);
        json.as_object_mut().unwrap().remove("timeline");
    });
    let header = screen(&mut open(&[&dir]), "", COLS, ROWS).lines().next().unwrap().to_string();
    assert!(header.contains("Studio work not measured"), "{header}");
}

#[test]
fn a_stepping_profile_shows_no_host_time_a_frame() {
    // the VM stepped (work): its host time a frame is the slowed interpreter's, so the Timeline leaves it out and says so
    let temp = Temp::new("host");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let app = open(&[&dir]);
    let series = ree_profile_view::ui::timeline::series(app.profile());
    assert!(!series.iter().any(|s| s == "host_ms" || s == "cycles" || s == "part"), "{series:?}");
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "7", COLS, ROWS);
    assert!(text.contains("host time not shown: stepping slows the interpreter"), "{text}");
    // a timeline of its own keeps it
    let dir = edited(&temp, "plain", Shape::BEFORE, |json| json["kinds"] = serde_json::json!(["timeline"]));
    let app = open(&[&dir]);
    assert!(ree_profile_view::ui::timeline::series(app.profile()).iter().any(|s| s == "host_ms"));
}

/// The harness fixture with each timeline row's work split by side, as a stepping profile's timeline has it: the game's
/// code 0.06 ms a frame in the steady phase (0.6 at frame 60) and the harness's 0.04 ms
fn sided_timeline(json: &mut serde_json::Value) {
    json["timeline"]["columns"].as_array_mut().unwrap().extend([serde_json::json!("part"), serde_json::json!("game_ms"), serde_json::json!("harness_ms")]);
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        let frame = row[0].as_f64().unwrap();
        let game = if frame == 60.0 { 0.6 } else { 0.06 };
        row.as_array_mut().unwrap().extend([serde_json::json!(0), serde_json::json!(game), serde_json::json!(0.04)]);
    }
}

#[test]
fn a_frames_figures_are_the_games_code_when_the_harness_is_apart() {
    let temp = Temp::new("frames");
    let dir = edited(&temp, "run", Shape::HARNESS, sided_timeline);
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "", 160, ROWS);
    // the steady phase's 100 frames: 99 at 0.06 ms and one at 0.6 ms, a mean of 0.0654 ms
    assert!(text.contains("game code/frame"), "{text}");
    assert!(text.contains("65.4 µs") && text.contains("max 0.600 ms"), "{text}");
    assert!(text.contains("The game's code a frame"), "the chart is the game's code a frame: {text}");
    // the Timeline opens on the same series
    assert_eq!(ree_profile_view::ui::timeline::current(&app).as_deref(), Some("game_ms"));
}

#[test]
fn both_sides_hottest_functions_share_one_heat_scale() {
    // the harness's scan (2.5 ms) against the game's comparator (4.0 ms): heat and bars are shares of both sides'
    // 14.63 ms, so the harness's top row is not drawn as its side's whole
    use ree_profile_view::ui::overview::sides;
    let temp = Temp::new("heat");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let mut app = open(&[&dir]);
    let got = sides(&app);
    let theme = app.theme.clone();
    let text = screen(&mut app, "", COLS, ROWS);
    assert!(text.contains("both sides"), "{text}");
    // (the colours: the harness's top row at its share of both sides, not of its own side)
    let whole = got.lead.total + got.harness.total;
    assert_ne!(theme.heat_of(2.5 / whole), theme.heat_of(2.5 / got.harness.total), "the two scales differ for this fixture");
}

#[test]
fn a_function_c_calls_says_its_calls_are_counted_once_a_c_call() {
    // the comparator (Main:22) is called by sort [C]: 100 sorts read as 50,100 calls in the fixture's call tree, which a
    // profile without invocation counts cannot tell from runs
    let temp = Temp::new("from-c");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "3 /anonymous", 200, ROWS);
    assert!(text.contains("calls, counted once a C call (needs invocation counts)"), "{text}");
    let dir = edited(&temp, "counted", Shape::BEFORE, |json| json["work"]["c_invocations"] = serde_json::json!(true));
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "3 /anonymous", 200, ROWS);
    assert!(!text.contains("counted once a C call") && text.contains(" each"), "{text}");
}

#[test]
fn lines_counted_per_phase_and_the_collectors_work_show_where_a_profile_has_them() {
    // a newer ree counts lines per phase (lines[].phases) and prices the collector inside the work clock
    // (work.collector_ms): the Calls view's hottest lines are then the reported phase's, and the Overview has the
    // collector's card; a profile without them says its lines are the whole run's and shows no such card
    let temp = Temp::new("newer");
    let plain = write_profile(&temp.path().join("plain"), Shape::BEFORE);
    let mut app = open(&[&plain]);
    let text = screen(&mut app, "3 /layout", 200, ROWS);
    assert!(text.contains("the whole run (lines are not counted per phase)"), "{text}");
    assert!(!screen(&mut open(&[&plain]), "", 200, ROWS).contains("the collector"));
    let dir = edited(&temp, "newer", Shape::BEFORE, |json| {
        json["work"]["collector_ms"] = serde_json::json!(1.5);
        for line in json["lines"].as_array_mut().unwrap() {
            let work = line["work_ms"].as_f64().unwrap();
            line["phases"] = serde_json::json!({"steady": {"work_ms": work * 0.75, "total_work_ms": work * 0.75, "insns": 1}});
        }
    });
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "3 /layout", 200, ROWS);
    assert!(text.contains("own Studio work · phase steady") && !text.contains("not counted per phase"), "{text}");
    // layout's line 31: 1.7 ms over the run, 1.275 ms in the steady phase
    assert!(text.contains("1.275 ms"), "{text}");
    let text = screen(&mut open(&[&dir]), "", 200, ROWS);
    assert!(text.contains("the collector") && text.contains("1.500 ms"), "{text}");
}

#[test]
fn host_time_a_frame_shows_when_a_run_of_its_own_measured_it() {
    // a newer ree takes a stepping profile's CPU figures in a pass of its own (`cpu.pass`) and fills the timeline's host
    // time from it: then it is the run's and shown; where it writes null, it is not
    let temp = Temp::new("cpu-pass");
    let dir = edited(&temp, "pass", Shape::BEFORE, |json| json["cpu"] = serde_json::json!({"pass": "a run of its own"}));
    let app = open(&[&dir]);
    assert!(ree_profile_view::ui::timeline::series(app.profile()).iter().any(|s| s == "host_ms"));
    let dir = edited(&temp, "null", Shape::BEFORE, |json| {
        json["kinds"] = serde_json::json!(["timeline"]);
        for row in json["timeline"]["rows"].as_array_mut().unwrap() {
            row[3] = serde_json::Value::Null;
        }
    });
    let app = open(&[&dir]);
    assert!(!ree_profile_view::ui::timeline::series(app.profile()).iter().any(|s| s == "host_ms"));
}

#[test]
fn a_timeline_profiles_header_never_reads_zero_work_from_its_empty_call_tree() {
    // a real timeline profile keeps a call tree of one empty node and no work section
    let temp = Temp::new("one-node-header");
    let dir = edited(&temp, "run", Shape::BEFORE, |json| {
        let object = json.as_object_mut().unwrap();
        for key in ["work", "luau_caches", "alloc"] {
            object.remove(key);
        }
        json["tree"] = serde_json::json!([[-1, -1, 0, 0.0, 0, "startup"]]);
        json["kinds"] = serde_json::json!(["timeline"]);
    });
    let header = screen(&mut open(&[&dir]), "", COLS, ROWS).lines().next().unwrap().to_string();
    assert!(header.contains("in frames") && !header.contains(" 0 ms"), "{header}");
}

#[test]
fn every_not_checked_note_shows_in_the_issues_view() {
    // a profile missing most kinds: each note is shown, wrapped, none dropped
    let temp = Temp::new("notes");
    let dir = edited(&temp, "run", Shape::HARNESS, |json| {
        let object = json.as_object_mut().unwrap();
        for key in ["work", "alloc", "timeline", "luau_caches"] {
            object.remove(key);
        }
        json["tree"] = serde_json::json!([[-1, -1, 0, 0.0, 0, "startup"]]);
    });
    std::fs::remove_file(dir.join("boundary.json")).unwrap();
    let mut app = open(&[&dir]);
    let notes = app.issues().1.clone();
    assert!(notes.len() >= 4, "{notes:?}");
    let text = screen(&mut app, "9", COLS, 60);
    let flat = text.replace('│', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    for note in &notes {
        let start: String = note.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
        assert!(flat.contains(&start), "{start:?} missing:\n{text}");
    }
    assert!(text.contains(&format!("Not checked  {} ·", notes.len())), "{text}");
}

#[test]
fn a_short_screen_shows_whole_not_checked_notes_and_counts_the_rest() {
    // at 60 by 16 the panel holds a few lines: each note it shows is whole and wrapped inside the panel, and the count says
    // how many are left, a note that did not fit among them, never half of one on screen
    let temp = Temp::new("notes-short");
    let dir = edited(&temp, "run", Shape::HARNESS, |json| {
        let object = json.as_object_mut().unwrap();
        for key in ["work", "alloc", "timeline", "luau_caches"] {
            object.remove(key);
        }
        json["tree"] = serde_json::json!([[-1, -1, 0, 0.0, 0, "startup"]]);
    });
    std::fs::remove_file(dir.join("boundary.json")).unwrap();
    let mut app = open(&[&dir]);
    let notes = app.issues().1.clone();
    let text = screen(&mut app, "9", 60, 16);
    let lines: Vec<&str> = text.lines().collect();
    let top = lines.iter().position(|line| line.contains("Not checked")).unwrap_or_else(|| panic!("{text}"));
    let (mut shown, mut more): (Vec<String>, Option<usize>) = (Vec::new(), None);
    for line in &lines[top + 1..] {
        if line.starts_with('╰') {
            break;
        }
        let inner = line.trim_start_matches('│').trim_end_matches('│').trim_end();
        let inner = inner.strip_prefix(' ').unwrap_or(inner);
        if let Some(rest) = inner.strip_prefix("○ ") {
            shown.push(rest.trim().to_string());
        } else if inner.starts_with("… and ") {
            more = inner.split_whitespace().nth(2).and_then(|count| count.parse().ok());
        } else if let Some(rest) = inner.strip_prefix("  ") {
            let last = shown.last_mut().unwrap_or_else(|| panic!("{text}"));
            last.push(' ');
            last.push_str(rest.trim());
        }
    }
    assert!(!shown.is_empty() && more.is_some(), "{text}");
    for note in &shown {
        assert!(notes.contains(note), "{note:?} is not a whole note:\n{text}");
    }
    assert_eq!(more, Some(notes.len() - shown.len()), "{text}");
}

#[test]
fn a_small_screen_still_lists_the_first_issues() {
    // at 60 by 16 and 80 by 20 the issues list keeps room for its first issues: the detail and the notes take what is left
    let temp = Temp::new("issues-small");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    let found = app.issues().0.len();
    assert!(found >= 3, "{found}");
    for (cols, rows) in [(60, 16), (80, 20)] {
        let text = screen(&mut app, "9", cols, rows);
        let lines: Vec<&str> = text.lines().collect();
        let top = lines.iter().position(|line| line.contains("Issues")).unwrap_or_else(|| panic!("{text}"));
        let listed = lines[top + 1..].iter().take_while(|line| !line.starts_with('╰')).filter(|line| ["info", "warning", "serious", "critical"].iter().any(|word| line.contains(word))).count();
        assert!(listed >= found.min(3), "{cols}x{rows}: {listed} issues listed of {found}:\n{text}");
    }
}

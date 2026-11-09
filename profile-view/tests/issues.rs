//! The issue detectors on profiles whose numbers are known: each pattern found with its numbers and its link, none
//! found where the profile shows none, and what a profile's kinds cannot show said as such.

mod common;

use common::*;
use ree_profile_view::{
    issues::{Issue, Severity, Target},
    model::{Kind, Profile},
};

fn issues_of(dir: &std::path::Path) -> (Vec<Issue>, Vec<String>, ree_profile_view::app::App) {
    let app = open(&[dir]);
    let found = app.issues();
    (found.0.clone(), found.1.clone(), app)
}

fn find<'a>(issues: &'a [Issue], kind: &str) -> Vec<&'a Issue> {
    issues.iter().filter(|issue| issue.kind == kind).collect()
}

fn func_named(app: &ree_profile_view::app::App, name: &str) -> u32 {
    app.profile().funcs.iter().position(|f| f.name == name).unwrap() as u32
}

#[test]
fn each_pattern_is_named_with_its_numbers() {
    let temp = Temp::new("detect");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let (issues, unchecked, app) = issues_of(&dir);
    // (every kind is there but the CPU's counters, which a run of their own reads: tests/counters.rs; this profile records a C function only when Luau runs under it)
    assert_eq!(
        unchecked,
        vec![
            "sorts without a comparator: need C call rows (this profile records a C function only when Luau runs under it)".to_string(),
            "function cadence and hitch attribution: needs frame summaries from a newer profile".to_string(),
            "code that waits on memory: needs hw (the CPU's counters), in a run of its own".to_string()
        ]
    );
    let all = |issue: &Issue| format!("{}\n{}", issue.title, issue.evidence.join("\n"));

    // a sort every frame of the steady state: 100 sorts in 100 frames, 4.2 ms of the phase's 10.6 ms (39.6 %): 42 µs a
    // frame, a quarter of a percent of a 60 fps frame, so info
    let sorts = find(&issues, "sort calls");
    assert_eq!(sorts.len(), 1, "{issues:#?}");
    assert_eq!(sorts[0].severity, Severity::Info);
    assert!((sorts[0].ms.unwrap() - 0.042).abs() < 1e-9, "{:?}", sorts[0].ms);
    assert_eq!(sorts[0].target, Target::Func(func_named(&app, "rank")));
    assert!(sorts[0].evidence[0].starts_with("recorded removable subtotal 42.0 µs a frame"), "{}", sorts[0].evidence[0]);
    assert!(all(sorts[0]).contains("table.sort ran 100 times in the 100 frames of the steady phase, 1.0 a frame"), "{}", all(sorts[0]));
    assert!(all(sorts[0]).contains("42.0 µs a frame in the sorts, 39.6% of the code's work"), "{}", all(sorts[0]));
    // (this profile counts its comparator once a sort: no figure a run is made up)
    assert!(all(sorts[0]).contains("counted once a sort, not once a run (needs invocation counts)"), "{}", all(sorts[0]));

    // rank:21 allocates 200 records a frame (64 + 48 bytes each), 22,400 bytes, every frame of the steady state; the
    // fixture's frames allocate 22,912 bytes each, so it fits in every frame
    let steady = find(&issues, "steady allocation");
    assert_eq!(steady.len(), 2, "rank:21 and layout:32, not the engine's: {steady:#?}");
    assert_eq!(steady[0].target, Target::Site(func_named(&app, "rank"), 21));
    assert!(steady[0].title.starts_with("21.9 KB a frame on average: rank  Main:21"), "{}", steady[0].title);
    assert!(all(steady[0]).contains("400 objects a frame over the 100 frames of the steady phase"), "{}", all(steady[0]));
    assert!(all(steady[0]).contains("table 57.1%, table storage 42.9%"), "{}", all(steady[0]));
    assert_eq!(steady[1].target, Target::Site(func_named(&app, "layout"), 32));
    assert!(find(&issues, "allocation bursts").is_empty());

    // layout:32: 120 tables and 840 table storage allocations, 720 regrowths over 120 frames, 6.0 a frame
    let growth = find(&issues, "table growth");
    assert_eq!(growth.len(), 1, "{growth:#?}");
    assert_eq!(growth[0].severity, Severity::Warning);
    assert!(all(growth[0]).contains("720 table storage allocations past the 120 tables' first: 6.0 regrowths a frame over 120 frames"), "{}", all(growth[0]));

    // frame 90's collector took 3 ms against a median of 0.05 ms; frame 60's work 2 ms against 0.106 ms
    let gc = find(&issues, "gc spike");
    assert_eq!(gc.len(), 1);
    assert_eq!(gc[0].target, Target::Frame(89));
    assert!(gc[0].title.contains("1 frame, worst 400 steps (frame 90)"), "{}", gc[0].title);
    assert!(gc[0].evidence[0].starts_with("not priced: "), "{}", gc[0].evidence[0]);
    assert!(gc[0].evidence[1].contains("400 collector steps, 878.9 KB freed, 3.000 ms of host time; a frame's median is 2 steps"), "{}", gc[0].evidence[1]);
    // frame 60's 2 ms of work against a mean frame of 0.106 ms: a burst, whose makers this profile (from before ree recorded
    // each function's work a frame) cannot say, so it is not priced; its severity is its hitch, and 2 ms fits in a 16.67 ms
    // frame
    let spike = find(&issues, "frame spike");
    assert_eq!(spike[0].title, "Frame spike: frame 60 took 2.000 ms of work");
    assert_eq!(spike[0].once, Some(60.0));
    assert!(spike[0].ms.is_none());
    assert_eq!(spike[0].severity, Severity::Info);
    assert!(spike[0].evidence[0].starts_with("not priced: what made it, so what doing it ahead saves, is not in this profile (a newer ree splits a frame's work"), "{}", spike[0].evidence[0]);
    assert!(all(spike[0]).contains("frame 60: 2.000 ms of work, against a mean frame of 0.106 ms in phase steady"), "{}", all(spike[0]));

    // connections: 10 at frame 21 to 109 at frame 120, one more every frame
    let growing = find(&issues, "growth");
    assert_eq!(growing.len(), 1, "connections only: {growing:#?}");
    assert!(growing[0].title.starts_with("Connections keep growing: +99 over the 100 frames of the steady phase"), "{}", growing[0].title);
    assert!(all(growing[0]).contains("+1.0 a frame, up in 99 of 99 frames"), "{}", all(growing[0]));

    // the reachable heap: 1.00 MB at steady to 3.50 MB at the end
    let heap = find(&issues, "heap growth");
    assert!(heap[0].title.contains("+2.50 MB from steady to end"), "{}", heap[0].title);
    assert_eq!(heap[0].target, Target::Snapshot(2));

    // layout:31's GETTABLEKS missed 5,000 of 6,000 lookups; rank:21's never missed
    let misses = find(&issues, "cache misses");
    assert_eq!(misses.len(), 1);
    assert!(misses[0].title.contains("GETTABLEKS at Main:31 misses 83.3% of its lookups"), "{}", misses[0].title);
    assert!(misses[0].advice.contains("differ in shape"), "repredicted, not slow: {}", misses[0].advice);

    // Both boundary savings are below the listing floor: they are not listed, and their work is in the closing row
    assert!(find(&issues, "repeat reads").is_empty());
    assert!(find(&issues, "unchanged writes").is_empty());

    // The fixes: the priced first, then the unpriced, each group most severe first, and each unpriced one saying why
    let fixes: Vec<&Issue> = issues.iter().filter(|issue| issue.kind != "work below floor").collect();
    let priced_count = fixes.iter().take_while(|issue| issue.ms.is_some()).count();
    let (priced, unpriced) = fixes.split_at(priced_count);
    assert!(unpriced.iter().all(|issue| issue.ms.is_none()));
    assert!(priced.windows(2).all(|pair| pair[0].severity <= pair[1].severity));
    assert!(unpriced.windows(2).all(|pair| pair[0].severity <= pair[1].severity));
    for issue in unpriced {
        assert!(issue.evidence[0].starts_with("not priced: "), "{issue:#?}");
    }

    // The closing row is last, and every issue's counted work adds up to the steady phase's 100 frames of work
    assert_eq!(issues.last().unwrap().kind, "work below floor");
    let counted: f64 = issues.iter().map(|issue| issue.counted_ms).sum();
    let frames = 100.0;
    assert!((counted - app.profile().scope_work().unwrap().0 / frames).abs() < 1e-9);
}

#[test]
fn issues_rank_by_the_work_they_account_for_not_by_their_counts() {
    // five unchanged writes a frame at 10 µs each (50 µs a frame) are listed, though under the 10 a frame a count once had
    // to reach, and rank first; the 200 repeat reads a frame (5.8 µs) are under the listing floor and not listed
    let temp = Temp::new("rank");
    let dir = hot_profile(&temp.path().join("run"));
    let path = dir.join("boundary.json");
    let mut boundary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let site = &mut boundary["by_site_count"][0];
    site["count"] = serde_json::json!(500);
    site["unchanged_writes"] = serde_json::json!(500);
    site["per_s"] = serde_json::json!(300.0);
    site["cost_ms_per_s"] = serde_json::json!(3.0);
    site["unchanged_cost_ms"] = serde_json::json!(5.0);
    std::fs::write(&path, boundary.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let writes = find(&issues, "unchanged writes");
    assert_eq!(writes.len(), 1, "{issues:#?}");
    assert!((writes[0].ms.unwrap() - 0.05).abs() < 1e-9, "{:?}", writes[0].ms);
    assert!(writes[0].evidence.iter().any(|line| line == "10.0 µs an unchanged write: 50.0 µs a frame"), "{:?}", writes[0].evidence);
    let at = |kind: &str| issues.iter().position(|issue| issue.kind == kind).unwrap();
    assert!(find(&issues, "repeat reads").is_empty());
    assert!(at("unchanged writes") < at("cache misses"), "{issues:#?}");
    // an unpriced issue (a cache miss: the work clock charges a miss as a hit) ranks below every priced one
    let misses = at("cache misses");
    assert!(issues[misses].ms.is_none() && issues[misses].unpriced.as_deref().is_some_and(|text| text.contains("charges a miss as a hit")));
    assert!(issues[..misses].iter().all(|issue| issue.ms.is_some() || issue.severity < issues[misses].severity), "{issues:#?}");
}

/// The fixture with one function, layout, holding most of the steady phase's work: 50 ms of its own over 100 frames
fn hot_profile(dir: &std::path::Path) -> std::path::PathBuf {
    let dir = write_profile(dir, Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["tree"][13][3] = serde_json::json!(50_000.0);
    let steady = json["scope"]["game_self_ms"].as_f64().unwrap() - 2.0 + 50.0;
    json["scope"]["game_self_ms"] = serde_json::json!(steady);
    for f in json["functions"].as_array_mut().unwrap() {
        if f["name"] == "layout" {
            f["phases"]["steady"]["self_work_ms"] = serde_json::json!(50.0);
        }
    }
    std::fs::write(&path, json.to_string()).unwrap();
    dir
}

#[test]
fn a_function_holding_most_of_the_games_work_is_named_with_its_cost_and_lines() {
    // layout's 50 ms and the 0.3 ms of the C function it calls, 0.503 ms a frame of the steady phase's 0.589: the repeat
    // reads at its line 31 (5.8 µs a frame) and the unchanged writes at its line 33 (5.0 µs) are those issues', so its own
    // price is the 0.492 ms a frame left, 84.0% of the code's work and 3.0% of a 60 fps frame. Its line 31 holds 70.8% of
    // its lines' work over the run, so the issue opens that line
    let temp = Temp::new("hot");
    let dir = hot_profile(&temp.path().join("run"));
    let (issues, unchecked, app) = issues_of(&dir);
    let hot: Vec<&Issue> = issues.iter().filter(|issue| issue.kind.starts_with("hot")).collect();
    assert_eq!(hot.len(), 1, "{issues:#?}");
    let issue = hot[0];
    assert_eq!(issue.kind, "hot line");
    assert_eq!(issue.target, Target::Line(func_named(&app, "layout"), 31));
    assert!((issue.ms.unwrap() - 0.503).abs() < 1e-9, "{issue:#?}");
    assert_eq!(issue.severity, Severity::Serious);
    assert!(issue.title.starts_with("layout Main:30: 0.503 ms a frame"));
    assert!(find(&issues, "repeat reads").is_empty() && find(&issues, "unchanged writes").is_empty());
    let text = issue.evidence.join("\n");
    assert!(!issue.advice.ends_with("nothing elsewhere can save as much"), "another removal's bound overlaps this subtotal: {}", issue.advice);
    assert!(text.contains("100 calls, 1.0 a frame"), "{text}");
    assert!(text.contains("its lines (lines are counted over the whole run): Main:31 70.8% of it (GETTABLEKS 6,000 · ADD 2,000)"), "{text}");
    assert!(text.contains("needs C call rows"), "{text}");
    assert!(unchecked.iter().any(|note| note.starts_with("sorts without a comparator: need C call rows")), "{unchecked:?}");
    // with lines counted per phase (a newer ree's), its lines are the steady phase's: no note on the whole run
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for line in json["lines"].as_array_mut().unwrap() {
        let work = line["work_ms"].as_f64().unwrap();
        line["phases"] = serde_json::json!({"steady": {"work_ms": work * 0.8, "total_work_ms": work * 0.8, "insns": 1}});
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let issue = issues.iter().find(|issue| issue.kind.starts_with("hot")).unwrap();
    assert!(issue.evidence.iter().any(|line| line.starts_with("its lines: Main:31 70.8% of it")), "{:?}", issue.evidence);
}

#[test]
fn a_quiet_profile_names_no_hot_code() {
    // the fixture's functions each hold under 40 µs a frame: nothing is hot, however its shares fall
    let temp = Temp::new("quiet");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let (issues, _, _) = issues_of(&dir);
    assert!(issues.iter().all(|issue| !issue.kind.starts_with("hot")), "{issues:#?}");
}

/// The fixture as a newer ree writes it: C functions recorded at their call sites by library name, with the calling line
/// on each node (`work.c_calls`), and a function C calls counted once a run (`work.c_invocations`)
fn c_rows_profile(dir: &std::path::Path, invocations: bool, comparator: bool) -> std::path::PathBuf {
    let dir = write_profile(dir, Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["work"]["c_calls"] = serde_json::json!(true);
    json["work"]["c_invocations"] = serde_json::json!(invocations);
    for f in json["functions"].as_array_mut().unwrap() {
        if f["name"] == "sort [C]" {
            f["name"] = serde_json::json!("table.sort");
        }
    }
    let tree = json["tree"].as_array_mut().unwrap();
    for node in tree.iter_mut() {
        if node[5] == "sort [C]" {
            node[5] = serde_json::json!("table.sort [C]");
        }
    }
    // (the steady phase's sort, called at rank's line 22; its comparator 500 runs a sort, or none)
    tree[11].as_array_mut().unwrap().push(serde_json::json!(22));
    if comparator {
        tree[12][2] = serde_json::json!(50_000);
    } else {
        tree[12][2] = serde_json::json!(0);
        tree[12][3] = serde_json::json!(0.0);
        tree[12][4] = serde_json::json!(0);
    }
    std::fs::write(&path, json.to_string()).unwrap();
    dir
}

#[test]
fn a_sort_with_no_comparator_is_found_from_the_c_calls_a_newer_ree_records() {
    // table.sort called 100 times at rank's line 22 with no comparator: 50 us a sort, found because the profile
    // records C calls whether or not Luau runs under them
    let temp = Temp::new("sort-c");
    let dir = c_rows_profile(&temp.path().join("run"), true, false);
    // Row 11 of the call tree is the table.sort node: 100 calls of 50 us each is 5,000 us. That puts the steady phase's game
    // work at 11.4 ms, which the issue's price must stay under
    const SORT_NODE: usize = 11;
    let sort_work_us = 100.0 * 50.0;
    let game_work_ms = 11.4;
    edit_json(&dir.join("profile.json"), |json| {
        json["tree"][SORT_NODE][3] = serde_json::json!(sort_work_us);
        json["scope"]["game_self_ms"] = serde_json::json!(game_work_ms);
    });
    let (issues, unchecked, app) = issues_of(&dir);
    let sorts = find(&issues, "sort calls");
    assert_eq!(sorts.len(), 1, "{issues:#?}");
    assert_eq!(sorts[0].target, Target::Line(func_named(&app, "rank"), 22));
    assert!(sorts[0].title.contains("rank  Main:20, line 22:"));
    assert!((sorts[0].ms.unwrap() - 0.05).abs() < 1e-9, "{:#?}", sorts[0]);
    assert!(unchecked.iter().all(|note| !note.contains("C call rows")));
}

#[test]
fn a_comparator_is_counted_once_a_run_when_the_profile_counts_its_invocations() {
    let temp = Temp::new("sort-runs");
    let dir = c_rows_profile(&temp.path().join("run"), true, true);
    let (issues, _, _) = issues_of(&dir);
    let sorts = find(&issues, "sort calls");
    assert!(sorts[0].evidence[2].ends_with("its comparator ran 50,000 times, 500 a sort"), "{:?}", sorts[0].evidence);
    // without invocation counts the same rows say what they cannot count
    let dir = c_rows_profile(&temp.path().join("old"), false, true);
    let (issues, _, _) = issues_of(&dir);
    let sorts = find(&issues, "sort calls");
    assert!(sorts[0].evidence[2].ends_with("counted once a sort, not once a run (needs invocation counts)"), "{:?}", sorts[0].evidence);
}

#[test]
fn a_hot_function_lists_the_c_functions_it_called() {
    let temp = Temp::new("hot-c");
    let dir = hot_profile(&temp.path().join("run"));
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["work"]["c_calls"] = serde_json::json!(true);
    json["tree"][14].as_array_mut().unwrap().push(serde_json::json!(31));
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let issue = issues.iter().find(|issue| issue.kind.starts_with("hot")).unwrap();
    assert!(issue.evidence.iter().any(|line| line == "the C functions it called, a frame: __index at line 31 ×30,000 3.0 µs"), "{:?}", issue.evidence);
}

#[test]
fn a_what_if_run_is_not_read_frame_by_frame() {
    // a run of ree profile try: its variant runs once, in one frame, so no spike, sort, hot code or growth across frames
    // is read from it; the comparison reads it per call
    let temp = Temp::new("what-if");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["run"]["harness"] = serde_json::json!("C:/what-if/harness/WhatIf.luau");
    json["phases"] = serde_json::json!(["startup", "setup", "run", "after"]);
    for node in json["tree"].as_array_mut().unwrap() {
        if node[5] == "frames" {
            node[5] = serde_json::json!("setup");
        } else if node[5] == "steady" {
            node[5] = serde_json::json!("run");
        }
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, unchecked, app) = issues_of(&dir);
    assert!(app.profile().is_what_if());
    for kind in ["frame spike", "gc spike", "sort calls", "growth", "hot line", "hot function"] {
        assert!(find(&issues, kind).is_empty(), "{kind}: {issues:#?}");
    }
    assert!(unchecked.iter().any(|note| note.contains("a what-if run (ree profile try)")), "{unchecked:?}");
}

#[test]
fn the_fixed_profile_has_none_of_them() {
    let temp = Temp::new("fixed");
    let dir = write_profile(&temp.path().join("run"), Shape::AFTER);
    let (issues, _, _) = issues_of(&dir);
    assert!(find(&issues, "sort calls").is_empty(), "one sort in 100 frames is not every frame: {issues:#?}");
    let steady: Vec<&Issue> = find(&issues, "steady allocation");
    assert!(steady.iter().all(|issue| !issue.title.contains("rank")), "rank allocates nothing: {steady:#?}");
}

#[test]
fn an_allocation_that_no_frame_could_hold_is_a_burst() {
    let temp = Temp::new("burst");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    // the frames now allocate 10 KB each: rank:21's 21.9 KB a frame on average cannot be every frame
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        row[7] = serde_json::json!(10240.0);
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let bursts = find(&issues, "allocation bursts");
    assert_eq!(bursts.len(), 1, "layout:32's 512 bytes a frame fit: {issues:#?}");
    assert!(bursts[0].title.ends_with("rank  Main:21"));
    assert!(bursts[0].evidence[1].contains("2.14 MB in the 100 frames of the steady phase, 21.9 KB a frame on average, but some frames allocate 10.0 KB in all"), "{}", bursts[0].evidence[1]);
}

#[test]
fn what_the_kinds_cannot_show_is_said_and_not_guessed() {
    let temp = Temp::new("kinds");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for key in ["tree", "alloc", "timeline", "heap", "luau_caches"] {
        json.as_object_mut().unwrap().remove(key);
    }
    std::fs::write(&path, json.to_string()).unwrap();
    std::fs::remove_file(dir.join("boundary.json")).unwrap();
    std::fs::remove_file(dir.join("alloc.folded")).unwrap();
    std::fs::remove_file(dir.join("alloc.counts.folded")).unwrap();
    let (issues, unchecked, _) = issues_of(&dir);
    assert!(issues.is_empty(), "{issues:#?}");
    assert_eq!(unchecked.len(), 5, "{unchecked:?}");
    assert!(unchecked.iter().any(|note| note.contains("boundary")));
    assert!(unchecked.iter().any(|note| note.contains("waits on memory") && note.contains("hw")));
}

#[test]
fn a_comparison_names_cost_that_grows_faster_than_n() {
    let temp = Temp::new("scaling");
    let dir = write_comparison(&temp.path().join("what-if"));
    let compare = ree_profile_view::compare::Compare::load(&dir).unwrap();
    let issues = compare.issues();
    // t[v][k]'s work grows as n^1.15 (under the 1.3 the list starts at); its sortAll as n^1.9
    assert!(issues.iter().all(|issue| !issue.title.contains("work grows")), "{issues:#?}");
    let function = issues.iter().find(|issue| issue.title.contains("sortAll")).expect("sortAll");
    assert_eq!(function.severity, Severity::Serious);
    assert_eq!(function.target, Target::Variant("t[v][k]".into()));
    assert!(function.title.contains("grows as n^1.90"), "{}", function.title);
}

/// An O(n²) scan with a fixed cost a call: a log-log fit over n = 10 to 1,000 reads n^1.15, under the 1.3 the list starts
/// at, while it grows as n^1.76 between the two largest sizes (0.816 ms against 14.2 us). The issue reads the large-n
/// growth: a version 1 comparison's slope between the two largest sizes, a version 2 one's exponent at large n (its fit
/// with the fixed cost apart)
#[test]
fn a_quadratic_scan_is_named_by_its_growth_at_large_n() {
    let temp = Temp::new("scaling-scan");
    let dir = write_comparison(&temp.path().join("what-if"));
    let path = dir.join("compare.json");
    let points = serde_json::json!([[10, 0.0041], [100, 0.0142], [1000, 0.816]]);
    let mut compare: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    compare["scaling"][1] = serde_json::json!({"variant": "t[v][k]", "metric": "work_ms", "exponent": 1.15, "r2": 0.97, "last": 1.76, "points": points});
    std::fs::write(&path, serde_json::to_string(&compare).unwrap()).unwrap();
    let issues = ree_profile_view::compare::Compare::load(&dir).unwrap().issues();
    let scan = issues.iter().find(|issue| issue.title.contains("t[v][k]: work grows")).expect("the version 1 scan is named");
    assert!(scan.title.contains("n^1.76") && scan.severity == Severity::Warning, "{scan:#?}");
    // version 2: the exponent at large n, the growing part's slope with the fixed cost apart (ree's stats::growth on
    // these points: 1.90 with a fixed 3.97 us), and each segment's own slope in the evidence
    compare["version"] = serde_json::json!(2);
    compare["scaling"][1] = serde_json::json!({"variant": "t[v][k]", "metric": "work_ms", "exponent": 1.90, "last": 1.76,
        "fit": {"k": 1.90, "fixed": 0.00397, "coefficient": 1.62e-6, "error": 0.0, "fitted": true},
        "segments": [[10, 100, 0.54], [100, 1000, 1.76]], "points": points});
    std::fs::write(&path, serde_json::to_string(&compare).unwrap()).unwrap();
    let issues = ree_profile_view::compare::Compare::load(&dir).unwrap().issues();
    let scan = issues.iter().find(|issue| issue.title.contains("t[v][k]: work grows")).expect("the version 2 scan is named");
    assert!(scan.title.contains("n^1.90") && scan.severity == Severity::Serious, "{scan:#?}");
    assert!(scan.evidence[0].contains("10-100 0.54") && scan.evidence[0].contains("100-1000 1.76"), "{:?}", scan.evidence);
}

#[test]
fn line_allocations_are_the_lines_own() {
    // (ree's lines[].alloc_bytes adds what a line's calls allocated and counts the allocating line twice: the viewer
    // books each allocation once, to the line that made it)
    let temp = Temp::new("lines");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let profile = Profile::load(&dir).unwrap();
    let rank = profile.funcs.iter().position(|f| f.name == "rank").unwrap();
    let layout = profile.funcs.iter().position(|f| f.name == "layout").unwrap();
    let update = profile.funcs.iter().position(|f| f.name == "update").unwrap();
    assert_eq!(profile.line_alloc[&(rank, 21)], (24000.0 * 112.0, 48000.0));
    assert_eq!(profile.line_alloc[&(layout, 32)], (61440.0, 960.0));
    assert!(!profile.line_alloc.contains_key(&(update, 11)), "the caller's line allocated nothing itself");
    let total: f64 = profile.line_alloc.values().map(|v| v.0).sum();
    assert_eq!(total + 3072.0, profile.raw.alloc.as_ref().unwrap().bytes, "every byte on one line, the engine's apart");
    assert!(profile.line_alloc_complete);
}

#[test]
fn a_run_with_no_phase_after_startup_has_no_steady_state() {
    // (a render that marks no phase after its mount: every frame is startup's, the mount's cost among them)
    let temp = Temp::new("startup");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        row[2] = serde_json::json!(0);
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, unchecked, _) = issues_of(&dir);
    for kind in ["sort calls", "steady allocation", "allocation bursts", "growth"] {
        assert!(find(&issues, kind).is_empty(), "{kind}: {issues:#?}");
    }
    assert!(unchecked.iter().any(|note| note.contains("marks no phase after startup")), "{unchecked:?}");
    // the spikes are still found, over every frame
    assert_eq!(find(&issues, "gc spike").len(), 1);
}

#[test]
fn growing_threads_the_collector_had_not_swept_are_not_a_leak() {
    // threads 10 at frame 21 to 109 at frame 120 (one more a frame), with the end snapshot's reachable threads at 5
    // (garbage, not swept yet) and then at 105 (held)
    for (reachable, leak) in [(5.0, false), (105.0, true)] {
        let temp = Temp::new("threads");
        let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
        let path = dir.join("profile.json");
        let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for row in json["timeline"]["rows"].as_array_mut().unwrap() {
            let frame = row[0].as_f64().unwrap();
            row[14] = serde_json::json!(if frame >= 21.0 { 10.0 + frame - 21.0 } else { 10.0 });
        }
        let snapshots = json["heap"]["snapshots"].as_array_mut().unwrap();
        snapshots.last_mut().unwrap()["by_type"].as_array_mut().unwrap().push(serde_json::json!({"type": "thread", "count": reachable, "bytes": 6768.0 * reachable}));
        std::fs::write(&path, json.to_string()).unwrap();
        let (issues, _, _) = issues_of(&dir);
        let threads: Vec<&Issue> = find(&issues, "growth").into_iter().filter(|issue| issue.title.starts_with("Threads")).collect();
        assert_eq!(threads.len(), leak as usize, "{reachable} reachable: {threads:#?}");
        if leak {
            assert!(threads[0].evidence[1].ends_with("; 105 reachable at the end, what a full collection keeps"), "{}", threads[0].evidence[1]);
        }
    }
}

#[test]
fn each_cache_has_its_own_advice() {
    // the fixture's missing site (5,000 of 6,000 lookups missed) as each cache: a builtin's fast path turned down, a
    // method not where the cache looked, a table key slot
    for (op, says) in [("FASTCALL2K", "fast path turned these arguments down"), ("NAMECALL", "objects of several shapes or classes"), ("GETTABLEKS", "differ in shape")] {
        let temp = Temp::new("advice");
        let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
        let path = dir.join("profile.json");
        let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["luau_caches"]["sites"][0]["op"] = serde_json::json!(op);
        std::fs::write(&path, json.to_string()).unwrap();
        let (issues, _, _) = issues_of(&dir);
        let misses = find(&issues, "cache misses");
        assert_eq!(misses.len(), 1, "{op}");
        assert!(misses[0].title.starts_with(&format!("{op} at Main:31")), "{}", misses[0].title);
        assert!(misses[0].advice.contains(says), "{op}: {}", misses[0].advice);
    }
}

#[test]
fn mluas_own_chunks_are_the_engines() {
    // mlua's userdata __index in Luau, which the profile names Luau: the engine's, like its other Luau, so an
    // allocation in it is the script line's that called it
    let temp = Temp::new("mlua");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["functions"].as_array_mut().unwrap().push(serde_json::json!({"id": 900, "key": "__mlua_index:anonymous@6", "kind": "Luau", "module": "__mlua_index", "name": "", "line": 6, "last_line": 11, "calls": 10, "self": {}, "total": {}}));
    std::fs::write(&path, json.to_string()).unwrap();
    let p = Profile::load(&dir).unwrap();
    let f = p.funcs.iter().find(|f| f.module == "__mlua_index").unwrap();
    assert_eq!(f.kind, Kind::Engine);
    assert!(p.funcs.iter().any(|f| f.kind == Kind::Script), "the fixture's own scripts stay scripts");
}

#[test]
fn the_harnesss_own_code_is_left_out_of_the_issues() {
    // the harness's watcher (Fixture:5) misses every lookup, regrows its tables, allocates 28 KB a frame and reads Name
    // again: none of it is the game's, so none of it is an issue, and the game's own issues stay as they were
    let temp = Temp::new("harness");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let (issues, unchecked, _) = issues_of(&dir);
    for issue in &issues {
        assert!(!issue.title.contains("Fixture:") && !issue.title.contains("watch"), "{issue:#?}");
    }
    let misses = find(&issues, "cache misses");
    assert_eq!(misses.len(), 1, "{misses:#?}");
    assert!(misses[0].title.contains("Main:31"), "{}", misses[0].title);
    let steady = find(&issues, "steady allocation");
    assert!(steady[0].title.starts_with("21.9 KB a frame on average: rank  Main:21"), "{}", steady[0].title);
    assert!(find(&issues, "repeat reads").iter().all(|issue| issue.title.contains("Main:31")));
    assert!(unchecked.iter().any(|note| note == "the harness's code (1 function): the issues cover the game's code"), "{unchecked:?}");
    // (a timeline without the game's and the harness's work a frame: a spike says it reads the frame's whole work, and what
    // a newer ree would tell)
    let spikes = find(&issues, "frame spike");
    assert!(spikes[0].title.ends_with("of work"), "{}", spikes[0].title);
    assert!(spikes[0].evidence[0].contains("a newer ree splits a frame's work between the game's code, the harness's and the engine, and records each function's"), "{:?}", spikes[0].evidence);
    // without a harness, no such note
    let (_, unchecked, _) = issues_of(&write_profile(&temp.path().join("plain"), Shape::BEFORE));
    assert!(unchecked.iter().all(|note| !note.contains("harness")), "{unchecked:?}");
}

#[test]
fn a_run_whose_code_is_all_the_harnesss_is_reported_whole() {
    // code measured by --harness alone: every script function the harness's, the game's side (the recorder's few calls)
    // under 1% of the work; its sort, its allocation and its misses are the run's issues
    let temp = Temp::new("standalone");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for f in json["functions"].as_array_mut().unwrap() {
        if f["kind"] == "Luau" {
            f["harness"] = serde_json::json!(true);
        }
    }
    json["scope"]["game_self_ms"] = serde_json::json!(0.01);
    json["scope"]["harness_self_ms"] = serde_json::json!(14.62);
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, unchecked, _) = issues_of(&dir);
    assert!(find(&issues, "sort calls").iter().any(|issue| issue.title.contains("rank")), "{issues:#?}");
    assert!(find(&issues, "cache misses").iter().any(|issue| issue.title.contains("Fixture:6")), "{issues:#?}");
    assert!(unchecked.iter().all(|note| !note.contains("harness's code (")), "{unchecked:?}");
}

#[test]
fn threads_that_keep_growing_are_not_checked_without_a_heap_snapshot() {
    // threads 10 at frame 21 to 109 at frame 120, one more a frame, and no heap snapshot: the timeline's count holds the
    // threads the collector had not swept, so whether they are held cannot be told; never called a leak
    let temp = Temp::new("threads-no-heap");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        let frame = row[0].as_f64().unwrap();
        row[14] = serde_json::json!(if frame >= 21.0 { 10.0 + frame - 21.0 } else { 10.0 });
    }
    json.as_object_mut().unwrap().remove("heap");
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, unchecked, _) = issues_of(&dir);
    assert!(find(&issues, "growth").iter().all(|issue| !issue.title.starts_with("Threads")), "{issues:#?}");
    assert!(unchecked.iter().any(|note| note.starts_with("threads keep growing: needs heap")), "{unchecked:?}");
    // the connections the same timeline grows are still found: they need no snapshot
    assert!(find(&issues, "growth").iter().any(|issue| issue.title.starts_with("Connections")), "{issues:#?}");
}

#[test]
fn a_folder_that_kept_profile_json_alone_says_steady_allocation_is_not_checked() {
    // the folded allocation stacks hold each site's phases; without them a site's bytes are the run's, and a phase's
    // allocation a frame is not checked rather than found missing
    let temp = Temp::new("json-only");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    std::fs::remove_file(dir.join("alloc.folded")).unwrap();
    std::fs::remove_file(dir.join("alloc.counts.folded")).unwrap();
    let (issues, unchecked, _) = issues_of(&dir);
    assert!(find(&issues, "steady allocation").is_empty() && find(&issues, "allocation bursts").is_empty(), "{issues:#?}");
    assert!(unchecked.iter().any(|note| note.starts_with("steady allocation per site: needs the folded allocation stacks")), "{unchecked:?}");
    // the issues profile.json holds alone are still found
    assert_eq!(find(&issues, "sort calls").len(), 1, "{issues:#?}");
}

#[test]
fn repeat_reads_say_where_the_value_was_read_again_and_every_unchanged_write_is_found() {
    // a newer ree splits a site's repeats: 15,000 of the 20,000 in later calls of layout in the frame, so the advice is to
    // read it once in the caller; and it lists every site whose writes changed nothing, here one the site tables leave out
    let temp = Temp::new("repeat-split");
    let dir = hot_profile(&temp.path().join("run"));
    // The savings must exceed the listing floor, so give the steady phase's frame function (call tree row 9, update) 19 ms
    // more work to hold them: 20 ms in all
    const UPDATE_NODE: usize = 9;
    let update_work_us = 20_000.0;
    let added_work_ms = 19.0;
    edit_json(&dir.join("profile.json"), |profile| {
        profile["tree"][UPDATE_NODE][3] = serde_json::json!(update_work_us);
        let game_work_ms = profile["scope"]["game_self_ms"].as_f64().unwrap();
        profile["scope"]["game_self_ms"] = serde_json::json!(game_work_ms + added_work_ms);
    });
    edit_json(&dir.join("boundary.json"), |boundary| {
        let site = &mut boundary["repeat_reads"][0];
        site["repeat_cost_ms"] = serde_json::json!(5.0);
        site["repeat_same_call"] = serde_json::json!(5000);
        site["repeat_later_call"] = serde_json::json!(15000);
        site["repeat_other_function"] = serde_json::json!(700);
        boundary["unchanged_writes"] = serde_json::json!([{
            "script": common::MODULE, "function": "update", "line": 12, "kind": "write", "member": "Frame.Visible",
            "count": 2000, "per_s": 1200.0, "cost_ms_per_s": 6.0, "unchanged_cost_ms": 10.0, "unchanged_writes": 2000,
            "repeat_reads": 0, "repeat_reads_per_s": 0.0,
        }]);
    });
    let (issues, _, _) = issues_of(&dir);
    let reads = find(&issues, "repeat reads");
    assert!(reads[0].advice.starts_with("Read it once in the caller and pass it in"), "{}", reads[0].advice);
    assert!(reads[0].evidence.iter().any(|line| line == "5,000 in the same call, 15,000 in later calls of the same function in the frame; another function read the same value 700 times more (not counted here: share the read)"), "{:?}", reads[0].evidence);
    let writes = find(&issues, "unchanged writes");
    assert!(writes.iter().any(|issue| issue.title.starts_with("Frame.Visible set to its own value: 20 a frame at Main:12")), "{writes:#?}");
    assert!(writes.iter().all(|issue| !issue.title.starts_with("Part.Position set to its own value")), "the 5 us write is below the floor: {writes:#?}");
}

#[test]
fn a_cache_miss_takes_the_advice_of_what_happened_most() {
    // a newer ree names each lookup's outcome and what removes it; the harness's own sites are marked
    let temp = Temp::new("outcomes");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let site = &mut json["luau_caches"]["sites"][0];
    site["key"] = serde_json::json!("Size");
    site["outcomes"] = serde_json::json!({"hit": 1000, "repredicted": 5000, "absent": 0});
    json["luau_caches"]["outcome_info"] = serde_json::json!({"repredicted": {"meaning": "the key was elsewhere in the table", "advice": "Build the tables alike, keys in one order"}});
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let misses = find(&issues, "cache misses");
    assert!(misses[0].title.starts_with("GETTABLEKS Size at Main:31"), "{}", misses[0].title);
    assert_eq!(misses[0].advice, "Build the tables alike, keys in one order");
    assert!(misses[0].evidence.iter().any(|line| line == "most often repredicted (5,000): the key was elsewhere in the table"), "{:?}", misses[0].evidence);
}

#[test]
fn rees_own_code_is_never_an_issue() {
    // a newer ree marks its own code (`tool`: its rur.* and ree.* chunks, a what-if run's harness): with the run's code all
    // the harness's (reported whole), the tool's sort and misses are still left out
    let temp = Temp::new("tool");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for f in json["functions"].as_array_mut().unwrap() {
        if f["kind"] == "Luau" {
            f["harness"] = serde_json::json!(true);
        }
        if f["name"] == "rank" {
            f["tool"] = serde_json::json!(true);
        }
    }
    json["scope"]["game_self_ms"] = serde_json::json!(0.01);
    json["scope"]["harness_self_ms"] = serde_json::json!(14.62);
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, app) = issues_of(&dir);
    assert!(app.profile().funcs.iter().any(|f| f.tool && f.name == "rank"));
    assert!(find(&issues, "sort calls").is_empty(), "rank is ree's own: {issues:#?}");
}

/// How a function's work in the 120 frames of a profile was spread: the frames it worked in, and its work in each
struct Spread {
    name: &'static str,
    busy_frames: Vec<usize>,
    busy_ms: f64,
}

#[test]
fn a_function_seen_once_or_repeated_keeps_its_full_mean_price() {
    let temp = Temp::new("recur");
    let frames = 120;
    let middle_frame = 60;
    let a_whole_frame_and_more_ms = 40.0;
    let spreads = [
        // one 2 ms event: it is priced at its mean and says what it cost once
        Spread { name: "one", busy_frames: vec![middle_frame], busy_ms: 2.0 },
        // four 2 ms events: a rate, with no single event to name
        Spread { name: "four", busy_frames: vec![30, 60, 90, 120], busy_ms: 2.0 },
        // one event that overruns a whole 16.7 ms frame: it is also a hitch
        Spread { name: "long", busy_frames: vec![middle_frame], busy_ms: a_whole_frame_and_more_ms },
    ];
    for spread in spreads {
        let rank_work: Vec<f64> = (1..=frames).map(|frame| if spread.busy_frames.contains(&frame) { spread.busy_ms } else { 0.0 }).collect();
        let dir = cost_profile(&temp.path().join(spread.name), &[("rank", rank_work), ("layout", vec![0.106; frames])]);
        let (issues, _, _) = issues_of(&dir);
        let rank = issues.iter().find(|issue| issue.title.starts_with("rank Main:1:")).unwrap();
        let busy_count = spread.busy_frames.len();
        let expected_mean = spread.busy_ms * busy_count as f64 / frames as f64;
        assert!((rank.ms.unwrap() - expected_mean).abs() < 1e-12);
        assert_eq!(rank.active_frames, busy_count);
        assert_eq!(rank.frame_ms, Some(spread.busy_ms));
        if busy_count == 1 {
            assert_eq!(rank.once, Some(middle_frame as f64));
            assert_eq!(rank.total_ms, Some(spread.busy_ms));
            assert!(rank.cost_text().contains("once in this run"));
        } else {
            assert!(rank.once.is_none());
            assert!(rank.title.contains("active in 4 of 120 frames"));
        }
        if spread.busy_ms == a_whole_frame_and_more_ms {
            // a hitch names what worked in the frame and claims no work
            let hitch = find(&issues, "frame hitch")[0];
            assert_eq!(hitch.severity, Severity::Critical);
            assert!(hitch.ms.is_none() && hitch.claims.is_empty());
            assert!(hitch.evidence.iter().any(|line| line.contains("rank Main:1 40.00 ms (99.7%)")));
        }
    }
}

#[test]
fn a_profile_from_before_split_rows_says_a_phases_first_frame_holds_what_came_before_its_mark() {
    // no `part` column: the steady phase's first frame (21) holds 16 ms, setup done before the mark in that frame
    let temp = Temp::new("old-split");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        if row[0] == 21 {
            row[4] = serde_json::json!(16.0);
        }
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let spike = find(&issues, "frame spike")[0];
    assert!(spike.evidence.iter().any(|line| line.starts_with("frame 21 is the first of the steady phase: this profile, from before a frame's row split at a phase mark")), "{:?}", spike.evidence);
}

#[test]
fn a_profile_kept_without_boundary_json_says_so() {
    // a profile that ran boundary (in `all`) keeps its sites in boundary.json: a folder without it says that is missing
    let temp = Temp::new("no-boundary-json");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["kinds"].as_array_mut().unwrap().push(serde_json::json!("boundary"));
    std::fs::write(&path, json.to_string()).unwrap();
    std::fs::remove_file(dir.join("boundary.json")).unwrap();
    let (_, unchecked, _) = issues_of(&dir);
    assert!(unchecked.iter().any(|note| note == "repeat reads and unchanged writes: need boundary.json, which this folder does not keep (the profile ran boundary)"), "{unchecked:?}");
}

#[test]
fn the_harnesss_cache_sites_are_left_out_before_the_top_sites_are_taken() {
    // five of the harness's sites, each missing more than the game's, ahead of the game's GETTABLEKS at Main:31: the game's
    // is still listed
    let temp = Temp::new("cache-harness");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let sites = json["luau_caches"]["sites"].as_array_mut().unwrap();
    for pc in 0..5 {
        sites.push(serde_json::json!({"function": 4, "line": 40 + pc, "pc": 20 + pc, "op": "GETTABLEKS", "hits": 0, "misses": 9000, "slow": 0, "other": 0, "harness": true}));
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let misses = find(&issues, "cache misses");
    assert!(misses.iter().any(|issue| issue.title.starts_with("GETTABLEKS at Main:31")), "{misses:#?}");
    assert!(misses.iter().all(|issue| !issue.title.contains("Main:4")), "the harness's sites: {misses:#?}");
    assert!(misses[0].evidence.iter().any(|line| line.contains("a frame over the run's 120 frames (caches are counted over the whole run)")), "{:?}", misses[0].evidence);
}

#[test]
fn a_highest_subtotal_cannot_beat_another_removals_upper_bound() {
    // rank's 30 ms and layout's 50 ms of the steady phase's work: both hot, only the first the most a change can save
    let temp = Temp::new("two-hot");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["tree"][10][3] = serde_json::json!(30_000.0);
    json["tree"][13][3] = serde_json::json!(50_000.0);
    let steady = json["scope"]["game_self_ms"].as_f64().unwrap() - 3.0 - 2.0 + 30.0 + 50.0;
    json["scope"]["game_self_ms"] = serde_json::json!(steady);
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let hot: Vec<&Issue> = issues.iter().filter(|issue| issue.kind.starts_with("hot")).collect();
    assert_eq!(hot.len(), 2, "{issues:#?}");
    assert!(!hot[0].advice.ends_with("nothing elsewhere can save as much"), "{}", hot[0].advice);
    let most = issues.iter().filter(|issue| issue.advice.contains("nothing elsewhere")).count();
    assert_eq!(most, 0, "{issues:#?}");
}

#[test]
fn a_profile_without_the_work_kind_says_hot_code_needs_work() {
    // a timeline profile keeps a call tree of one empty node: no work was measured, so hot code and sorts need work
    let temp = Temp::new("one-node");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("work");
    json["tree"] = serde_json::json!([[-1, -1, 0, 0.0, 0, "startup"]]);
    json["kinds"] = serde_json::json!(["timeline"]);
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, unchecked, app) = issues_of(&dir);
    assert!(!app.profile().has_work());
    assert!(unchecked.iter().any(|note| note == "hot functions and lines, sorts every frame: need work"), "{unchecked:?}");
    assert!(unchecked.iter().all(|note| !note.contains("C call rows")), "{unchecked:?}");
    assert!(issues.iter().all(|issue| !issue.kind.starts_with("hot")));
}

#[test]
fn priced_issues_never_add_up_to_more_than_the_work_they_come_from() {
    // the newer rows (a C function recorded at its call site): table.sort at rank's line 22 with 20 ms of its own C work in
    // the steady phase and its comparator's 4 ms, under rank, which does 30 ms of its own. The sort's issue prices the
    // sort (0.24 ms a frame), rank's hot line what is left of rank (0.3 ms), never the sort's C work again: the priced
    // issues add up to no more than the game's work
    let temp = Temp::new("no-double");
    let dir = c_rows_profile(&temp.path().join("run"), true, true);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (rank, sort) = (json["tree"][10][3].as_f64().unwrap(), json["tree"][11][3].as_f64().unwrap());
    json["tree"][10][3] = serde_json::json!(30_000.0);
    json["tree"][11][3] = serde_json::json!(20_000.0);
    let game = json["scope"]["game_self_ms"].as_f64().unwrap() + (30_000.0 - rank + 20_000.0 - sort) / 1000.0;
    json["scope"]["game_self_ms"] = serde_json::json!(game);
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, app) = issues_of(&dir);
    let sorts = find(&issues, "sort calls");
    assert!((sorts[0].ms.unwrap() - 0.24).abs() < 1e-9, "{:?}", sorts[0].ms);
    let hot = issues.iter().find(|issue| issue.kind.starts_with("hot") && issue.title.contains("rank")).unwrap_or_else(|| panic!("{issues:#?}"));
    assert!((hot.ms.unwrap() - 0.5).abs() < 1e-9, "{:?}", hot.ms);
    assert!(sorts[0].overlap > 0.0, "{issues:#?}");
    let p = app.profile();
    let frames = p.scope_frames(&p.scope()).unwrap();
    let priced: f64 = issues.iter().filter(|issue| !issue.host_time).map(|issue| issue.counted_ms).sum();
    assert!(priced <= game / frames + 1e-9, "{priced} ms a frame priced of {} ms: {issues:#?}", game / frames);
    // An uncertain source edit cannot establish that another candidate saves less.
    assert!(!hot.advice.ends_with("nothing elsewhere can save as much"), "the sort's caller-work bound can exceed the known function subtotal: {issues:#?}");
}

#[test]
fn a_newer_profiles_cache_and_builtin_prices_split_by_what_the_clock_charged() {
    // a version 2 profile prices a cache site and a declined builtin call on the work clock, counted in the reported
    // phases (100 frames). layout:31's GETTABLEKS was charged 50 µs a frame past its dispatch (rehashes), of which 5 µs is
    // its first touch and 5 µs the presized replacement's constructor: fixing it saves 40 µs a frame, claimed from layout.
    // Its uncharged outcomes cost 50 µs a frame over hits (added beside the measured work), three more are within their
    // noise. string.sub's 300,000 declines, 20 ns each the clock charged (6,000 µs in all), all at layout:31: 60 µs a
    // frame claimed from layout
    let temp = Temp::new("prices-v2");
    let dir = hot_profile(&temp.path().join("run"));
    let declined_calls = 300_000;
    edit_json(&dir.join("profile.json"), |json| {
        let layout = json["functions"].as_array().unwrap().iter().find(|f| f["name"] == "layout").unwrap()["id"].as_i64().unwrap();
        json["version"] = serde_json::json!(2);
        json["luau_caches"]["prices"] = serde_json::json!({"clock": "work", "units": "work clock ns"});
        for site in json["luau_caches"]["sites"].as_array_mut().unwrap() {
            if site["line"] != 31 {
                continue;
            }
            site["outcomes"] = serde_json::json!({"hit": 1000, "repredicted": 50000, "rehashed": 2000});
            site["charged_work_us"] = serde_json::json!(5000.0);
            site["charged_work_us_per_frame"] = serde_json::json!(50.0);
            site["extra_work_us"] = serde_json::json!(5000.0);
            site["extra_work_us_per_frame"] = serde_json::json!(50.0);
            site["first_touch_us"] = serde_json::json!(500.0);
            site["node"] = serde_json::json!(13);
            site["outcome_charged_us"] = serde_json::json!(4500.0);
            site["presized_us"] = serde_json::json!(500.0);
            site["within_noise"] = serde_json::json!(3);
        }
        json["builtins"] = serde_json::json!([{
            "name": "string.sub", "calls": declined_calls, "fast": 0, "declines": {"args": declined_calls},
            "decline_charged_ns": 20.0, "decline_charged_us": 6000.0, "advice": {"args": "Pass its end"},
            "sites": [{"function": layout, "node": 13, "decline_charged_us": 6000.0, "fast_equivalent_us": 1500.0, "fix_cost_exact": true, "fix_instructions_us": 0, "fix_cold_us": 0, "line": 31, "pc": 4, "fallback_end_pc": 4, "calls": declined_calls, "fast": 0, "declines": {"args": declined_calls}}],
        }]);
    });
    let (issues, _, app) = issues_of(&dir);
    let misses = find(&issues, "cache misses");
    assert!((misses[0].ms.unwrap() - 0.04).abs() < 1e-12, "{:?}", misses[0].ms);
    assert!((misses[0].uncharged - 0.05).abs() < 1e-12 && !misses[0].host_time, "{:#?}", misses[0]);
    assert_eq!(misses[0].claims.len(), 1);
    assert!(misses[0].claims[0].0 == func_named(&app, "layout") && (misses[0].claims[0].1 - 0.04).abs() < 1e-12, "{:?}", misses[0].claims);
    assert!(misses[0].evidence.iter().any(|line| line.contains("a frame over the 100 frames of the steady phase")), "{:?}", misses[0].evidence);
    assert!(misses[0].evidence.iter().any(|line| line == "3 outcomes priced within their noise, left out of the price"), "{:?}", misses[0].evidence);
    let declines = find(&issues, "builtin declines");
    assert!((declines[0].ms.unwrap() - 0.045).abs() < 1e-9 && declines[0].uncharged == 0.0, "{:#?}", declines[0]);
    assert!(declines[0].claims.len() == 1 && declines[0].claims[0].0 == func_named(&app, "layout") && (declines[0].claims[0].1 - 0.045).abs() < 1e-12, "{:?}", declines[0].claims);
    // (layout's hot line is what is left of its work once the charged misses' and the declines' parts are theirs)
    let hot = issues.iter().find(|issue| issue.kind.starts_with("hot")).unwrap();
    assert!(find(&issues, "repeat reads").is_empty() && find(&issues, "unchanged writes").is_empty());
    assert!((hot.ms.unwrap() - 0.503).abs() < 1e-9, "{hot:#?}");
    let measured: f64 = issues.iter().filter(|issue| !issue.host_time).map(|issue| issue.counted_ms).sum();
    let p = app.profile();
    assert!(measured <= p.scope_work().unwrap().0 / p.scope_frames(&p.scope()).unwrap() + 1e-9, "{issues:#?}");
}

#[test]
fn a_newer_profiles_absent_and_inserted_lookups_are_misses() {
    // a version 2 site whose lookups found no key (4,390 absent, 43.9 µs a frame uncharged) and three that inserted keys
    // (52.5 µs a frame each, all above the listing floor): the older counts (misses and slow) hold none of them, the
    // outcomes do, and the sites are listed by their price
    let temp = Temp::new("absent-v2");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let layout = json["functions"].as_array().unwrap().iter().find(|f| f["name"] == "layout").unwrap()["id"].as_i64().unwrap();
    json["luau_caches"]["prices"] = serde_json::json!({"clock": "work", "units": "work clock ns"});
    let sites = json["luau_caches"]["sites"].as_array_mut().unwrap();
    sites.clear();
    sites.push(serde_json::json!({"function": layout, "line": 31, "pc": 3, "op": "GETTABLEKS", "kind": "table key slot", "hits": 0, "misses": 0, "slow": 0, "other": 0, "outcomes": {"hit": 610, "absent": 4390}, "extra_work_us": 4390.0, "extra_work_us_per_frame": 43.9}));
    for line in [32, 33, 34] {
        sites.push(serde_json::json!({"function": layout, "line": line, "pc": line, "op": "SETTABLEKS", "kind": "table key slot", "hits": 0, "misses": 0, "slow": 0, "other": 0, "outcomes": {"hit": 0, "inserted": 3000}, "extra_work_us": 5250.0, "extra_work_us_per_frame": 52.5}));
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let misses = find(&issues, "cache misses");
    assert_eq!(misses.len(), 4, "{misses:#?}");
    let absent = misses.iter().find(|issue| issue.title.contains("Main:31")).unwrap();
    assert!(absent.ms.is_none() && (absent.uncharged - 0.0439).abs() < 1e-12 && absent.title.ends_with("misses 87.8% of its lookups"), "{absent:#?}");
    assert!(absent.cost_text().starts_with("estimated"));
    assert!(absent.evidence.iter().any(|line| line.starts_with("4,390 of 5,000 missed (absent 4,390)")), "{:?}", absent.evidence);
    let inserts = misses.iter().filter(|issue| issue.title.starts_with("SETTABLEKS")).count();
    assert_eq!(inserts, 3, "{misses:#?}");
}


/// Reads a profile's JSON file, lets `change` edit it, and writes it back
fn edit_json(path: &std::path::Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut json);
    std::fs::write(path, json.to_string()).unwrap();
}

/// The name of the one reported phase in `cost_profile`
const STEADY_PHASE: &str = "steady";

/// A work-clock profile whose numbers are exact. Unlike the older, broader detector fixture, its call tree and its
/// timeline describe the same work, so what an issue prices can be checked to the last digit.
///
/// `functions` is each function's name with its work in each reported frame (ms), all the same number of frames. Function
/// number `n` is `Main:{n + 1}`. The profile holds no allocation, heap, cache, builtin or boundary data: a test adds the
/// piece it checks.
fn cost_profile(dir: &std::path::Path, functions: &[(&str, Vec<f64>)]) -> std::path::PathBuf {
    let dir = write_profile(dir, Shape::AFTER);
    let frames = functions[0].1.len();

    // The call tree: the startup phase, the steady phase, then one node for each function under it.
    // A row is [parent node, function id, calls, own work us, instructions, label, calling line]
    let mut tree = vec![serde_json::json!([-1, -1, 0, 0.0, 0, "startup"]), serde_json::json!([0, -1, 0, 0.0, 0, STEADY_PHASE])];
    let mut function_entries = Vec::new();
    let mut total_ms = 0.0;
    for (id, (name, work_by_frame)) in functions.iter().enumerate() {
        let work_ms: f64 = work_by_frame.iter().sum();
        total_ms += work_ms;
        tree.push(serde_json::json!([1, id, frames, work_ms * 1e3, 1, null]));
        function_entries.push(serde_json::json!({
            "id": id, "key": format!("{MODULE}:{name}"), "name": name, "module": MODULE, "line": id + 1, "last_line": id + 1,
            "kind": "Luau", "harness": false,
            "self": {"work_ms": work_ms, "insns": 1},
            "total": {"work_ms": work_ms, "insns": 1},
            "phases": {STEADY_PHASE: {"self_work_ms": work_ms, "total_work_ms": work_ms, "calls": frames, "insns": 1}},
        }));
    }

    // The timeline: a row for each frame, [frame, phase, part, work ms, game ms, harness ms], and each row's functions as
    // the older flat cells [function id, work us, id, us, ...] (a function that did no work in the frame is left out)
    let mut rows = Vec::new();
    let mut function_cells = Vec::new();
    for frame in 0..frames {
        let frame_ms: f64 = functions.iter().map(|(_, work_by_frame)| work_by_frame[frame]).sum();
        rows.push(serde_json::json!([frame + 1, 2, 0, frame_ms, frame_ms, 0]));

        let mut cells = Vec::new();
        for (id, (_, work_by_frame)) in functions.iter().enumerate() {
            if work_by_frame[frame] > 0.0 {
                cells.push(serde_json::json!(id));
                cells.push(serde_json::json!(work_by_frame[frame] * 1e3));
            }
        }
        function_cells.push(cells);
    }

    let path = dir.join("profile.json");
    edit_json(&path, |json| {
        json["functions"] = serde_json::json!(function_entries);
        json["tree"] = serde_json::json!(tree);
        json["lines"] = serde_json::json!([]);
        json["timeline"] = serde_json::json!({"columns": ["frame", "phase", "part", "work_ms", "game_ms", "harness_ms"], "rows": rows, "functions": function_cells});
        json["scope"] = serde_json::json!({"phases": [STEADY_PHASE], "game_self_ms": total_ms, "harness_self_ms": 0});
        json["run"]["frames"] = serde_json::json!(frames);
        json["work"]["total_ms"] = serde_json::json!(total_ms);
        json["work"]["c_calls"] = serde_json::json!(true);
        json["run"]["kinds"] = serde_json::json!(["work", "timeline"]);
        for kind in ["alloc", "heap", "luau_caches", "builtins", "boundary"] {
            json.as_object_mut().unwrap().remove(kind);
        }
    });
    std::fs::remove_file(dir.join("boundary.json")).unwrap();
    dir
}

#[test]
fn cache_prices_retain_first_touch_and_pay_the_presized_replacement() {
    let temp = Temp::new("cache-saving");
    let frames = 120;
    let dir = cost_profile(&temp.path().join("run"), &[("Build", vec![2.0; frames])]);
    let path = dir.join("profile.json");

    // Site 1 rehashes 1,000 tables a frame. Each charged 607 ns (72,840 us over 120 frames), of which 175 ns (21,000 us) is
    // what the presized replacement costs anyway, so fixing it saves 432 ns a table: 0.432 ms a frame.
    // Site 2 pays a 200 ns first touch a table a frame (24,000 us), which a fix keeps paying, and has 1,000 outcomes
    // priced within their noise, left out. Its repredicted lookups are the uncharged work: 600,000 at 10 ns over 120
    // frames, 0.05 ms a frame.
    edit_json(&path, |json| {
        json["luau_caches"] = serde_json::json!({
            "prices": {"clock": "work", "kinds": {"table key slot": {"outcomes": {
                "rehashed": {"charged": true},
                "repredicted": {"charged": false, "ns": 10},
                "absent": {"charged": false, "ns": 100, "within_noise": true},
            }}}},
            "sites": [
                {"function": 0, "node": 2, "line": 1, "op": "SETTABLEKS", "kind": "table key slot", "outcomes": {"rehashed": 120000},
                 "charged_work_us": 72840, "first_touch_us": 0, "outcome_charged_us": 72840, "presized_us": 21000, "extra_work_us": 0},
                {"function": 0, "line": 2, "op": "GETTABLEKS", "kind": "table key slot", "outcomes": {"repredicted": 600000, "absent": 1000},
                 "charged_work_us": 24000, "first_touch_us": 24000, "outcome_charged_us": 0, "within_noise": 1000},
            ],
        });
    });
    let (found, _, _) = issues_of(&dir);
    let caches = find(&found, "cache misses");
    assert_eq!(caches.len(), 2, "{found:#?}");

    let rehash = caches.iter().find(|issue| issue.title.starts_with("SETTABLEKS")).unwrap();
    assert!((rehash.ms.unwrap() - 0.432).abs() < 1e-12, "{rehash:#?}");

    let lookup = caches.iter().find(|issue| issue.title.starts_with("GETTABLEKS")).unwrap();
    assert!(lookup.ms.is_none() && (lookup.uncharged - 0.05).abs() < 1e-12, "{lookup:#?}");
    assert!(lookup.evidence.iter().any(|line| line.contains("1,000 outcomes priced within their noise")));

    // Without the breakdown, the old charged total is evidence, not a claimed saving
    edit_json(&path, |json| {
        json["luau_caches"]["sites"] = serde_json::json!([
            {"function": 0, "line": 1, "op": "SETTABLEKS", "outcomes": {"rehashed": 120000}, "charged_work_us": 72840, "extra_work_us": 0},
        ]);
    });
    let (found, _, _) = issues_of(&dir);
    let old = find(&found, "cache misses")[0];
    assert!(old.ms.is_none() && old.evidence[0].starts_with("not priced: needs a newer profile"), "{old:#?}");
}

#[test]
fn nested_declines_keep_their_standalone_saving_and_totals_count_the_union() {
    let temp = Temp::new("nested-saving");
    let frames = 120;
    let dir = cost_profile(&temp.path().join("run"), &[("Sort", vec![0.2; frames]), ("Compare", vec![0.3; frames])]);
    let path = dir.join("profile.json");

    // table.sort (a C function) runs the Sort function and, under it, Compare 1,000 times a frame. Together they are 0.5 ms
    // a frame: the sort's own 24,000 us and Compare's 36,000 us over 120 frames. string.byte is declined 513,000 times in
    // Compare (4,275 a frame), charged 10,260 us in all: 85.5 us a frame, work already inside the sort's price.
    let compare_calls = 120_000;
    let declined_calls = 513_000;
    let declines_charged_us = 10_260;
    edit_json(&path, |json| {
        json["functions"].as_array_mut().unwrap().push(serde_json::json!({"id": -2, "key": "table.sort", "name": "table.sort", "kind": "C", "self": {"work_ms": 24}, "total": {"work_ms": 60}}));
        json["tree"] = serde_json::json!([
            [-1, -1, 0, 0, 0, "startup"],
            [0, -1, 0, 0, 0, STEADY_PHASE],
            [1, 0, frames, 0, 1, null],
            [2, -2, frames, 24000, 0, "table.sort [C]", 1],
            [3, 1, compare_calls, 36000, 1, null],
        ]);
        json["luau_caches"] = serde_json::json!({"prices": {"clock": "work"}, "sites": []});
        json["builtins"] = serde_json::json!([{
            "name": "string.byte", "calls": declined_calls, "declines": {"args": declined_calls},
            "decline_charged_ns": 20, "decline_charged_us": declines_charged_us,
            "sites": [{"function": 1, "node": 4, "decline_charged_us": declines_charged_us, "fast_equivalent_us": 0, "fix_cost_exact": true, "fix_instructions_us": 0, "fix_cold_us": 0, "line": 2, "declines": {"args": declined_calls}}],
            "advice": {"args": "Pass the last index"},
        }]);
    });
    let (found, _, _) = issues_of(&dir);
    let sort = find(&found, "sort calls")[0];
    let decline = find(&found, "builtin declines")[0];

    assert!((sort.ms.unwrap() - 0.5).abs() < 1e-12);
    // the decline keeps its standalone price, all of it overlapping the sort, so it adds nothing to the union
    assert!((decline.ms.unwrap() - 0.0855).abs() < 1e-12, "{decline:#?}");
    assert!((decline.overlap - 0.0855).abs() < 1e-12 && decline.counted_ms.abs() < 1e-12);
    assert!(decline.evidence.iter().any(|line| line.contains("overlaps sort calls")));
    // the sort names every function whose work it holds
    assert!(sort.evidence.iter().any(|line| line.contains("Compare Main:2")));
    let counted: f64 = found.iter().map(|issue| issue.counted_ms).sum();
    assert!((counted - 0.5).abs() < 1e-12);
}

#[test]
fn exact_cadences_and_small_transfers_are_conserved_without_relative_floors() {
    let temp = Temp::new("exact-rates");
    let frame_budget_ms = 1000.0 / 60.0;

    // A runs 7 ms every third frame, B 10 ms every seventh and Base 2 ms every frame, over runs of three lengths: each
    // keeps its mean rate, and the frame where all three meet names all three with their shares
    for frames in [120, 300, 600] {
        let a: Vec<f64> = (1..=frames).map(|frame| if frame % 3 == 0 { 7.0 } else { 0.0 }).collect();
        let b: Vec<f64> = (1..=frames).map(|frame| if frame % 7 == 0 { 10.0 } else { 0.0 }).collect();
        let dir = cost_profile(&temp.path().join(format!("n{frames}")), &[("A", a), ("B", b), ("Base", vec![2.0; frames])]);
        let (found, _, _) = issues_of(&dir);

        let hot: Vec<&Issue> = found.iter().filter(|issue| issue.kind.starts_with("hot")).collect();
        assert_eq!(hot.len(), 3, "{found:#?}");
        let a_issue = hot.iter().find(|issue| issue.title.contains("A Main:1")).unwrap();
        assert!((a_issue.ms.unwrap() - 7.0 / 3.0).abs() < 1e-12);
        for issue in hot {
            assert_eq!(issue.severity, Severity::of(issue.ms.unwrap(), frame_budget_ms));
        }

        let hitch = find(&found, "frame hitch")[0];
        let names_all_three = hitch.evidence.iter().any(|line| line.contains("A Main:1 7.000 ms (36.8%)") && line.contains("B Main:2 10.00 ms (52.6%)") && line.contains("Base Main:3 2.000 ms (10.5%)"));
        assert!(names_all_three, "{hitch:#?}");
    }

    // Moving 0.1 ms a frame of work from one function to another leaves the total at 0.182 ms a frame; only the part
    // that crosses the floor moves between the listed fixes and the closing row
    let mut listed_ms = Vec::new();
    let rates_ms = [[0.139, 0.043], [0.039, 0.143]];
    for (index, rates) in rates_ms.iter().enumerate() {
        let frames = 120;
        let dir = cost_profile(&temp.path().join(format!("transfer{index}")), &[("A", vec![rates[0]; frames]), ("B", vec![rates[1]; frames])]);
        let (found, _, _) = issues_of(&dir);
        let counted: f64 = found.iter().map(|issue| issue.counted_ms).sum();
        assert!((counted - 0.182).abs() < 1e-12);
        listed_ms.push(found.iter().filter(|issue| issue.kind.starts_with("hot")).map(|issue| issue.ms.unwrap()).sum::<f64>());
    }
    assert!((listed_ms[0] - listed_ms[1]).abs() <= 0.1, "{listed_ms:?}");
}

#[test]
fn a_counter_claim_cannot_exceed_its_named_functions_work() {
    let temp = Temp::new("claim-cap");
    let frames = 120;
    let dir = cost_profile(&temp.path().join("run"), &[("Read", vec![0.1; frames]), ("Other", vec![0.2; frames])]);

    // The cache site claims 50,000 us over 120 frames (0.417 ms a frame) of a function that only did 0.1 ms a frame
    edit_json(&dir.join("profile.json"), |json| {
        json["luau_caches"] = serde_json::json!({"prices": {"clock": "work"}, "sites": [
            {"function": 0, "node": 2, "line": 1, "op": "GETIMPORT", "outcomes": {"absent": 120000},
             "charged_work_us": 50000, "first_touch_us": 0, "outcome_charged_us": 50000, "extra_work_us": 0},
        ]});
    });
    let (found, _, _) = issues_of(&dir);
    let cache = find(&found, "cache misses")[0];
    assert!(cache.ms.is_none(), "{cache:#?}");
    assert!(cache.claims.is_empty());
    assert!(cache.evidence[0].contains("exceeds its function's recorded work"));
    let counted: f64 = found.iter().map(|issue| issue.counted_ms).sum();
    assert!((counted - 0.3).abs() < 1e-12);
}

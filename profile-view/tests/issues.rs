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
    // (every kind but the CPU's counters, which a run of their own reads: tests/counters.rs)
    assert_eq!(unchecked, vec!["code that waits on memory: needs hw (the CPU's counters), in a run of its own".to_string()], "every other kind is there");
    let all = |issue: &Issue| format!("{}\n{}", issue.title, issue.evidence.join("\n"));

    // a sort every frame of the steady state: 100 sorts in 100 frames, 4.2 ms of the phase's 10.6 ms (39.6 %)
    let sorts = find(&issues, "sort every frame");
    assert_eq!(sorts.len(), 1, "{issues:#?}");
    assert_eq!(sorts[0].severity, Severity::Critical);
    assert_eq!(sorts[0].target, Target::Func(func_named(&app, "rank")));
    assert!(all(sorts[0]).contains("table.sort ran 100 times in the 100 frames of the steady phase, 1.0 a frame"), "{}", all(sorts[0]));
    assert!(all(sorts[0]).contains("42.0 µs a frame in the sorts, 39.6% of the phase's work"), "{}", all(sorts[0]));

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
    assert!(gc[0].evidence[0].contains("400 collector steps, 878.9 KB freed, 3.000 ms of host time; a frame's median is 2 steps"), "{}", gc[0].evidence[0]);
    let spike = find(&issues, "frame spike");
    assert!(spike[0].title.contains("worst 2.000 ms (frame 60)"), "{}", spike[0].title);

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

    // the boundary profile: Part.Size read again 200 times a frame at Main:31, Position set unchanged 30 a frame
    let reads = find(&issues, "repeat reads");
    assert_eq!(reads[0].severity, Severity::Serious);
    assert_eq!(reads[0].target, Target::Line(func_named(&app, "layout"), 31));
    assert!(all(reads[0]).contains("20,000 repeat reads in 100 frames, of 30,000 reads"), "{}", all(reads[0]));
    assert!(all(reads[0]).contains("200 ns a read: 40.0 µs a frame"), "{}", all(reads[0]));
    let writes = find(&issues, "unchanged writes");
    assert!(writes[0].title.contains("Part.Position set to its own value: 30 a frame at Main:33"), "{}", writes[0].title);

    // most severe first
    assert!(issues.windows(2).all(|pair| pair[0].severity <= pair[1].severity));
}

#[test]
fn the_fixed_profile_has_none_of_them() {
    let temp = Temp::new("fixed");
    let dir = write_profile(&temp.path().join("run"), Shape::AFTER);
    let (issues, _, _) = issues_of(&dir);
    assert!(find(&issues, "sort every frame").is_empty(), "one sort in 100 frames is not every frame: {issues:#?}");
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
    assert!(bursts[0].evidence[0].contains("2.14 MB in the 100 frames of the steady phase, 21.9 KB a frame on average, but some frames allocate 10.0 KB in all"), "{}", bursts[0].evidence[0]);
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
    for kind in ["sort every frame", "steady allocation", "allocation bursts", "growth"] {
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
            assert!(threads[0].evidence[0].ends_with("; 105 reachable at the end, what a full collection keeps"), "{}", threads[0].evidence[0]);
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
    // (a frame's figures are the whole frame's: a spike says the harness's code is in them)
    let spikes = find(&issues, "frame spike");
    assert!(spikes[0].evidence.iter().any(|line| line == "a frame's work counts the harness's code too: 4.000 ms of self work in phase steady"), "{:?}", spikes[0].evidence);
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
    assert!(find(&issues, "sort every frame").iter().any(|issue| issue.title.contains("rank")), "{issues:#?}");
    assert!(find(&issues, "cache misses").iter().any(|issue| issue.title.contains("Fixture:6")), "{issues:#?}");
    assert!(unchecked.iter().all(|note| !note.contains("harness's code (")), "{unchecked:?}");
}

/// A version 2 timeline: instances in the DataModel and outside it (what a frame keeps is their sum), threads made a frame (a
/// flow: thread growth is the heap snapshots' reachable threads), host time null where a kind slowed the run (no figure, never 0)
#[test]
fn a_version_2_timeline_reads_its_split_columns() {
    let temp = Temp::new("v2");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["version"] = serde_json::json!(2);
    let columns = json["timeline"]["columns"].as_array_mut().unwrap();
    columns[12] = serde_json::json!("instances_in_datamodel");
    columns[14] = serde_json::json!("instances_outside");
    columns.push(serde_json::json!("threads_made"));
    for row in json["timeline"]["rows"].as_array_mut().unwrap() {
        let frame = row[0].as_f64().unwrap();
        let row = row.as_array_mut().unwrap();
        row[3] = serde_json::Value::Null;
        row[12] = serde_json::json!(100.0);
        // (a part made outside the DataModel each frame of the steady phase, kept)
        row[14] = serde_json::json!(if frame >= 21.0 { 10.0 + frame - 21.0 } else { 10.0 });
        row.push(serde_json::json!(3.0));
    }
    // reachable threads at the steady snapshot (frame 20) and the end's (frame 120): 10, then 110
    let snapshots = json["heap"]["snapshots"].as_array_mut().unwrap();
    for (index, count) in [(1usize, 10.0), (2, 110.0)] {
        snapshots[index]["by_type"].as_array_mut().unwrap().push(serde_json::json!({"type": "thread", "count": count, "bytes": 6768.0 * count}));
    }
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, _) = issues_of(&dir);
    let growing = find(&issues, "growth");
    let titled = |start: &str| growing.iter().find(|issue| issue.title.starts_with(start)).unwrap_or_else(|| panic!("no {start}: {growing:#?}"));
    assert!(titled("Instances keep growing: +99").title.contains("steady phase"), "{growing:#?}");
    assert!(titled("Threads keep growing: +100").evidence[0].contains("10 threads reachable at frame 20 → 110 at frame 120: +1.0 a frame"), "{growing:#?}");
    let profile = Profile::load(&dir).unwrap();
    assert!(profile.raw.timeline.as_ref().unwrap().values("host_ms").is_none(), "a null host time is no figure, not 0");
}

/// A version 2 profile's cache sites are read by their outcomes, with one hit rate: a site that rehashes 10,000 times a frame
/// is flagged (the counts before the outcomes put a rehash in no column), and a table-key cache at 33.3 % of its lookups reads
/// so (the counts before read every miss but the repredictions as hits)
#[test]
fn cache_sites_are_read_by_their_outcomes() {
    let temp = Temp::new("outcomes");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["version"] = serde_json::json!(2);
    let frames = json["run"]["frames"].as_f64().unwrap();
    let outcomes = |hit: f64, rehashed: f64, absent: f64| serde_json::json!({"hit": hit, "repredicted": 0, "absent": absent, "inserted": 0, "rehashed": rehashed, "metatable_table": 0, "metatable_function": 0, "unsafe_env": 0, "not_table": 6, "readonly": 0});
    let caches = &mut json["luau_caches"];
    caches["frames"] = serde_json::json!(frames);
    caches["by_kind"] = serde_json::json!([{"kind": "table key slot", "outcomes": outcomes(10002.0, 10000.0, 10004.0), "lookups": 30006.0, "hits": 10002, "misses": 0, "slow": 0, "other": 6}]);
    let site = caches["sites"][0].clone();
    caches["sites"] = serde_json::json!([{
        "function": site["function"], "line": site["line"], "pc": site["pc"], "op": "SETTABLEKS",
        "outcomes": outcomes(0.0, 10000.0 * frames, 0.0), "hits": 0, "misses": 0, "slow": 0, "other": 0,
        "charged_work_us_per_frame": 1624.0, "extra_work_us_per_frame": 0.0, "within_noise": 3000.0 * frames,
    }]);
    caches["outcome_info"] = serde_json::json!({"rehashed": {"meaning": "a new key was added and the table rehashed to fit it", "advice": "Build the table with its keys at once"}});
    std::fs::write(&path, json.to_string()).unwrap();
    let (issues, _, app) = issues_of(&dir);
    let misses = find(&issues, "cache misses");
    assert_eq!(misses.len(), 1, "{issues:#?}");
    assert!(misses[0].title.contains("misses 100.0% of its lookups"), "{}", misses[0].title);
    assert_eq!(misses[0].severity, Severity::Serious);
    assert_eq!(misses[0].advice, "Build the table with its keys at once");
    assert!(misses[0].evidence.iter().any(|line| line.contains("rehashed") && line.contains("10,000 a frame")), "{:?}", misses[0].evidence);
    // (what the charge holds, a table's cold first touch among it, and what the extra leaves out: the outcomes priced within the noise)
    let charge = misses[0].evidence.iter().find(|line| line.contains("charged 1624.00 µs a frame")).unwrap_or_else(|| panic!("{:?}", misses[0].evidence));
    assert!(charge.contains("a table's cold first touch in the frame among it"), "{charge}");
    assert!(charge.contains(&format!("leaving out {} outcomes priced within the noise, 3,000 a frame", ree_profile_view::fmt::grouped(3000.0 * frames))), "{charge}");
    let kind = &app.profile().raw.luau_caches.as_ref().unwrap().by_kind[0];
    assert_eq!((kind.lookups(), kind.hit_count()), (30006.0, 10002.0), "the lookups a table serves and the hits: 33.3 %");
}

/// Across profile versions the viewer's Diff leaves out what changed meaning: functions' and lines' self work and host time, and a
/// function's calls and instructions (version 1 counted a module's main chunk at none)
#[test]
fn a_diff_across_versions_leaves_out_what_changed_meaning() {
    let temp = Temp::new("versions");
    let (before, after) = (write_profile(&temp.path().join("before"), Shape::BEFORE), write_profile(&temp.path().join("after"), Shape::BEFORE));
    let path = after.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["version"] = serde_json::json!(2);
    std::fs::write(&path, json.to_string()).unwrap();
    let app = open(&[&before, &after]);
    let (figures, notes) = ree_profile_view::diff::headline(&app.profiles[0], &app.profiles[1]);
    assert!(notes.iter().any(|note| note.contains("two versions (1 and 2)")), "{notes:?}");
    assert!(!figures.iter().any(|figure| figure.name.contains("host time") || figure.name.contains("the game's code")), "{:?}", figures.iter().map(|f| &f.name).collect::<Vec<_>>());
    assert!(notes.iter().any(|note| note.contains("calls and instructions")), "{notes:?}");
    use ree_profile_view::metric::Metric;
    for metric in [Metric::Work, Metric::Time, Metric::CacheMisses, Metric::Insns] {
        assert!(ree_profile_view::diff::functions(&app, metric).is_empty(), "{metric:?}");
        assert!(ree_profile_view::diff::lines(&app, metric).is_empty(), "{metric:?}");
    }
    // (allocations compare across versions, their functions' calls do not; within a version both do)
    let changed = |name: &str, version: u64| {
        let dir = write_profile(&temp.path().join(name), Shape::AFTER);
        let path = dir.join("profile.json");
        let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["version"] = serde_json::json!(version);
        std::fs::write(&path, json.to_string()).unwrap();
        dir
    };
    let (across, within) = (open(&[&before, &changed("after-v2", 2)]), open(&[&before, &changed("after-v1", 1)]));
    let (across, within) = (ree_profile_view::diff::functions(&across, Metric::AllocBytes), ree_profile_view::diff::functions(&within, Metric::AllocBytes));
    assert!(!across.is_empty() && across.iter().all(|pair| pair.calls.is_none()), "{:?}", across.iter().map(|pair| (&pair.label, pair.calls)).collect::<Vec<_>>());
    assert!(!within.is_empty() && within.iter().all(|pair| pair.calls.is_some()), "{:?}", within.iter().map(|pair| (&pair.label, pair.calls)).collect::<Vec<_>>());
}

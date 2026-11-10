//! The hardware counters in the viewer: a profile from before the fill counters were named for what they count opens
//! with their names now (its counts, its folded files, its metric and a diff against a newer profile), the figures of
//! what memory cost come from the counts, and a function that waits on memory is named from its measured numbers only.

mod common;

use common::*;
use ree_profile_view::{issues::Target, metric::Metric, model::Profile};
use serde_json::json;

/// The fixture profile with `counters` in its profile.json (and each counter's folded file as `counters.<name>.folded`)
fn with_counters(dir: &std::path::Path, counters: serde_json::Value, folded: &[(&str, &str)]) {
    let path = dir.join("profile.json");
    let mut profile: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    profile["counters"] = counters;
    std::fs::write(&path, profile.to_string()).unwrap();
    for (name, text) in folded {
        std::fs::write(dir.join(format!("counters.{name}.folded")), text).unwrap();
    }
}

/// The counters of a profile ree writes now: the default set, read around sampled calls; `rank` (function 2) waits on
/// memory, `layout` (4) computes
fn new_counters() -> serde_json::Value {
    let names = ["instructions", "cycles", "load-stall-cycles", "miss-buffer-occupancy", "miss-buffer-allocs", "branch-misses"];
    let counts = |values: [f64; 6]| -> serde_json::Value { names.iter().zip(values).map(|(name, value)| (name.to_string(), json!(value))).collect::<serde_json::Map<_, _>>().into() };
    json!({
        "source": "service",
        "names": names,
        "about": names.iter().map(|name| ree_profile_view::hw_counters::known(name).map(|known| known.about)).collect::<Vec<_>>(),
        "attribution": {"method": "calls", "calls": 300, "threshold_us": 100.0, "min_sampled_us": 10.0},
        "run": {"counts": counts([4_000_000.0, 4_000_000.0, 1_000_000.0, 1_200_000.0, 3_000.0, 2_000.0]), "rates": {}, "seconds": 1.0},
        "frames": {"columns": ["frame", "phase", "host_ms", "instructions", "cycles", "load-stall-cycles", "miss-buffer-occupancy", "miss-buffer-allocs", "branch-misses"], "rows": [[1, 2, 1.0, 40000, 40000, 20000, 24000, 60, 20], [2, 2, 1.0, 40000, 40000, 2000, 2400, 6, 20]]},
        "functions": [
            {"id": 2, "key": "Main:rank", "name": "rank", "self": counts([300_000.0, 1_500_000.0, 900_000.0, 1_000_000.0, 2_500.0, 500.0]), "total": counts([300_000.0, 1_500_000.0, 900_000.0, 1_000_000.0, 2_500.0, 500.0])},
            {"id": 4, "key": "Main:layout", "name": "layout", "self": counts([2_000_000.0, 1_000_000.0, 30_000.0, 40_000.0, 100.0, 500.0]), "total": counts([2_000_000.0, 1_000_000.0, 30_000.0, 40_000.0, 100.0, 500.0])},
        ],
        "lines": [{"function": 1, "line": 12, "self": {}, "total": counts([300_000.0, 1_500_000.0, 900_000.0, 1_000_000.0, 2_500.0, 500.0]), "calls": true}],
    })
}

#[test]
fn a_cut_short_counter_profile_is_marked_in_the_header() {
    let temp = Temp::new("hw-cut-short");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut counters = new_counters();
    counters["cut_short"] = json!(12.345);
    with_counters(&dir, counters, &[]);
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "1", 120, 40);
    let header = text.lines().next().unwrap();
    assert!(header.contains("HW CUT SHORT at 12.345 s"), "{header}");
}

/// A function holding most of the run's stall cycles, most of its own cycles stalled, is named as waiting on memory with
/// its numbers and linked to Calls; one that computes is not; the Overview's cards and the timeline's series say what
/// memory cost; a line shows what the calls made from it cost
#[test]
fn a_function_that_waits_on_memory_is_named_from_its_numbers() {
    let temp = Temp::new("hw-waits");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    with_counters(&dir, new_counters(), &[]);
    let app = open(&[&dir]);
    let (issues, unchecked) = { let found = app.issues(); (found.0.clone(), found.1.clone()) };
    assert!(!unchecked.iter().any(|note| note.contains("memory")), "{unchecked:?}");
    let waits: Vec<_> = issues.iter().filter(|issue| issue.kind == "memory waits").collect();
    assert_eq!(waits.len(), 1, "{waits:#?}");
    let rank = app.profile().funcs.iter().position(|f| f.name == "rank").unwrap() as u32;
    assert_eq!(waits[0].target, Target::Func(rank));
    let text = format!("{}\n{}", waits[0].title, waits[0].evidence.join("\n"));
    assert!(text.contains("60.0%") && text.contains("900.0K") && text.contains("22.5%"), "its stall share, its stall cycles, its share of the run: {text}");
    assert!(text.contains("400 cycles"), "its misses' latency (1,000,000 in flight over 2,500 allocated): {text}");
    // the metric switch has the stall cycles, with what they count
    let p = app.profile();
    let stalls = Metric::parse("load-stall-cycles", p).unwrap();
    assert!(Metric::available(p).contains(&stalls));
    assert!(stalls.describe(p).contains("waited for load data") && stalls.describe(p).contains("sampled Luau calls"), "{}", stalls.describe(p));
    // a line's figure is what the calls made from it cost
    let update = p.funcs.iter().position(|f| f.name == "update").unwrap();
    let line = p.lines_of(update).find(|(line, _)| *line == 12).map(|(_, fig)| stalls.line_self(fig)).unwrap();
    assert_eq!(line, 900_000.0);
    // the timeline: the counters a frame, and the stall share a frame derived from them
    let series = ree_profile_view::ui::timeline::series(p);
    assert!(series.contains(&"hw:load stall share".to_string()) && series.contains(&"hw:L1D miss latency".to_string()), "{series:?}");
    let (values, _, _) = ree_profile_view::ui::timeline::values(p, "hw:load stall share").unwrap();
    assert_eq!(values, vec![0.5, 0.05]);
    let mut app = app;
    let overview = screen(&mut app, "1", 240, 48);
    assert!(overview.contains("stalled on loads") && overview.contains("25.0%") && overview.contains("400 cycles"), "{overview}");
}

/// A profile from before the rename: its counters, folded files, metric and figures under their names now, and a diff
/// against a newer profile matches its fills to the newer one's
#[test]
fn a_profile_with_the_old_counter_names_opens_with_their_names_now() {
    let temp = Temp::new("hw-old");
    let old = write_profile(&temp.path().join("old"), Shape::BEFORE);
    let old_names = ["instructions", "cycles", "l1d-misses", "l2-misses", "llc-misses", "branch-misses"];
    let counts = |names: &[&str], values: [f64; 6]| -> serde_json::Value { names.iter().zip(values).map(|(name, value)| (name.to_string(), json!(value))).collect::<serde_json::Map<_, _>>().into() };
    with_counters(
        &old,
        json!({
            "source": "service",
            "names": old_names,
            "run": {"counts": counts(&old_names, [3000.0, 1000.0, 30.0, 3.0, 1.0, 6.0]), "rates": {"IPC": 3.0, "l1d-misses per 1000 instructions": 10.0}, "seconds": 1.0},
            "frames": {"columns": ["frame", "phase", "host_ms", "instructions", "cycles", "l1d-misses", "l2-misses", "llc-misses", "branch-misses"], "rows": [[1, 2, 1.0, 3000, 1000, 30, 3, 1, 6]]},
            "functions": [{"id": 2, "key": "Main:rank", "name": "rank", "self": counts(&old_names, [3000.0, 1000.0, 30.0, 3.0, 1.0, 6.0]), "total": counts(&old_names, [3000.0, 1000.0, 30.0, 3.0, 1.0, 6.0])}],
            "lines": [{"function": 2, "line": 21, "self": counts(&old_names, [3000.0, 1000.0, 30.0, 3.0, 1.0, 6.0]), "total": {}}],
        }),
        &[("l1d-misses", "steady;update Main:10;rank Main:20 @21 30\n")],
    );
    let p = Profile::load(&old).unwrap();
    let c = p.raw.counters.as_ref().unwrap();
    assert_eq!(c.names, vec!["instructions", "cycles", "demand-fills", "demand-fills-beyond-l2", "demand-fills-beyond-l3", "branch-misses"]);
    assert_eq!(c.run.counts.get("demand-fills"), Some(&30.0));
    assert!(c.frames.index("demand-fills").is_some() && c.frames.index("l1d-misses").is_none());
    let fills = Metric::parse("l1d-misses", &p).or_else(|| Metric::parse("demand-fills", &p)).unwrap();
    assert_eq!(Metric::parse("demand-fills", &p), Some(fills));
    assert_eq!(fills.total(&p), 30.0);
    assert!(fills.has_stacks(&p), "its folded file under its new name");
    let rank = p.funcs.iter().position(|f| f.name == "rank").unwrap();
    assert_eq!(fills.func_self(&p, rank), 30.0);
    // (from 1 ms samples a line's figure is its own)
    assert_eq!(p.lines_of(rank).find(|(line, _)| *line == 21).map(|(_, fig)| fills.line_self(fig)), Some(30.0));
    let figures = c.figures(&c.names.iter().map(|name| c.run.counts[name]).collect::<Vec<_>>());
    assert!(figures.iter().any(|figure| figure.key == "demand fills per 1000 instructions" && figure.value == 10.0), "{figures:?}");
    // a diff against a profile ree writes now: the fills matched, by their name now
    let new = write_profile(&temp.path().join("new"), Shape::AFTER);
    let new_names = ["instructions", "cycles", "demand-fills", "demand-fills-beyond-l2", "demand-fills-beyond-l3", "branch-misses"];
    with_counters(&new, json!({"source": "service", "names": new_names, "attribution": {"method": "calls", "version": 2}, "run": {"counts": counts(&new_names, [2000.0, 1000.0, 15.0, 2.0, 1.0, 5.0]), "rates": {}, "seconds": 1.0}}), &[]);
    let (figures, notes) = ree_profile_view::diff::headline(&p, &Profile::load(&new).unwrap());
    assert!(notes.iter().any(|note| note.contains("reached functions in other ways") && note.contains("1 ms samples")), "the counters reached functions in other ways: {notes:?}");
    let row = figures.iter().find(|figure| figure.name == "demand-fills (CPU counter)").unwrap_or_else(|| panic!("{:?}", figures.iter().map(|f| &f.name).collect::<Vec<_>>()));
    assert_eq!((row.before, row.after), (30.0, 15.0));
    assert!(figures.iter().any(|figure| figure.name == "IPC (CPU counters)" && figure.before == 3.0 && figure.after == 2.0));
}

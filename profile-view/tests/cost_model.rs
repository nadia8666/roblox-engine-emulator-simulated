//! The cost model's rules, each tried on small profiles built here: an issue's price is the full observed mean over the
//! reported frames however its work is spread, overlapping fixes are counted once, and the listed prices plus the
//! closing row add up to the measured work.
mod common;
use common::Temp;
use ree_profile_view::{
    app::App,
    cli,
    issues::{Issue, Severity},
    theme::ThemeKind,
};
use serde_json::{Value, json};

/// The frame rate most profiles here run at, and the time one frame has (ms)
const SIXTY_FPS: f64 = 60.0;
const FRAME_BUDGET_MS: f64 = 1000.0 / SIXTY_FPS;

/// The frames of a profile whose work is the same every frame
const STEADY_FRAMES: usize = 120;

/// What one frame's work is in a profile of that many frames: the work a function did in each frame (ms)
type WorkByFrame = Vec<f64>;

/// The call-tree node numbers of the profiles below: nodes 0 and 1 are the startup and steady phases, and the first
/// function's node comes next
const FIRST_FUNCTION_NODE: usize = 2;

/// How far an advised source edit is bounded
#[derive(Clone, Copy)]
enum ExpectedBound {
    /// All the scoped code work, beside the recorded site part. This is every edit's default
    WholeCode,
    /// The site's own price: the rewrite only adds omitted arguments and keeps every operand
    SiteOnly,
}

/// One advised rewrite of a builtin's declined calls
struct BuiltinEditCase {
    builtin: &'static str,
    /// why the calls decline, which picks the advice
    decline_reason: &'static str,
    bound: ExpectedBound,
}

#[test]
fn advised_nonpositive_site_part_keeps_complete_edit_bound() {
    let caller_ms = 0.5;
    let argument_ms = 0.3;
    // the declined calls cost less than the fast path would, so the site part is negative
    let charged_us = 6000.0;
    let fast_us = 6600.0;
    let expected_upper_ms = caller_ms + argument_ms;
    let temp = Temp::new("model-nonpositive-edit-bound");
    let mut profile = fixture(&[("Step", vec![caller_ms; STEADY_FRAMES]), ("Argument", vec![argument_ms; STEADY_FRAMES])], SIXTY_FPS);
    profile["builtins"] = json!([{
        "name": "table.insert", "advice": {"args": "When pos is #t+1 use table.insert(t, value)"},
        "sites": [{
            "function": 0, "pc": 7, "line": 12, "declines": {"args": 1000},
            "decline_charged_us": charged_us, "fast_equivalent_us": fast_us,
            "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"args": 1000}, "decline_charged_us": charged_us, "fast_equivalent_us": fast_us}]
        }]
    }]);
    let app = open(&temp, "run", &profile);
    let issues = found(&app);
    let insert = of_kind(&issues, "builtin declines");
    assert!(insert.ms.is_none());
    assert!(!insert.no_saving);
    close(insert.upper_ms.unwrap(), expected_upper_ms);
    assert!(insert.cost_text().contains("recorded site part saves nothing on this clock"));
    assert!(insert.cost_text().contains("complete edit at most"));
    assert!(insert.evidence[0].contains("complete edit at most"));
    assert!(issues.iter().all(|issue| !issue.advice.contains("nothing elsewhere")));
    accounts(&app, &issues);
}

#[test]
fn cache_advice_defaults_to_complete_edit_bounds() {
    let frames = 60;
    let caller_ms = 0.5;
    let argument_ms = 0.3;
    let first_touch_us = 1000.0;
    let outcome_us = 6000.0;
    let constructor_us = 1000.0;
    let expected_upper_ms = caller_ms + argument_ms;
    // every outcome ree names, and one it does not know yet: none of them is on the allowlist
    let outcomes = ["repredicted", "absent", "inserted", "rehashed", "metatable_table", "metatable_function", "unsafe_env", "future_outcome"];
    let temp = Temp::new("model-cache-edit-bounds");
    for outcome in outcomes {
        let mut profile = fixture(&[("Step", vec![caller_ms; frames]), ("Argument", vec![argument_ms; frames])], SIXTY_FPS);
        profile["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": [{
            "function": 0, "pc": 3, "line": 12, "op": "GETTABLEKS", "kind": "table key slot", "outcomes": {(outcome): 1000},
            "charged_work_us": first_touch_us + outcome_us, "first_touch_us": first_touch_us,
            "outcome_charged_us": outcome_us, "presized_us": constructor_us,
            "nodes": [{"node": FIRST_FUNCTION_NODE, "outcomes": {(outcome): 1000}, "charged_work_us": first_touch_us + outcome_us,
                "first_touch_us": first_touch_us, "outcome_charged_us": outcome_us, "presized_us": constructor_us}]
        }]});
        let app = open(&temp, outcome, &profile);
        let issues = found(&app);
        let cache = of_kind(&issues, "cache misses");
        let replacement_us = if outcome == "rehashed" { constructor_us } else { 0.0 };
        let expected_site_ms = (outcome_us - replacement_us) / 1000.0 / frames as f64;
        close(cache.ms.unwrap(), expected_site_ms);
        close(cache.upper_ms.unwrap(), expected_upper_ms);
        assert!(cache.cost_text().contains("recorded work"));
        assert!(issues.iter().all(|issue| !issue.advice.contains("nothing elsewhere")));
        accounts(&app, &issues);
    }
}

#[test]
fn builtin_removal_bounds_cover_work_outside_the_site() {
    // An advised edit can also remove the caller's instructions and the evaluation of the arguments, so by default its
    // issue bounds the complete edit by all the code's work. Only a rewrite that keeps every operand (adding the omitted
    // index to string.byte, or the end to string.sub) has no such bound.
    let frames = 60;
    let caller_work_ms = 0.5;
    let argument_work_ms = 0.3;
    let charged_us = 30_000;
    let fast_path_us = 6_000;
    let expected_site_ms = 0.4;
    let expected_upper_ms = caller_work_ms + argument_work_ms;
    let cases = [
        BuiltinEditCase { builtin: "math.abs", decline_reason: "types", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "Vector3.new", decline_reason: "types", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "future.builtin", decline_reason: "args", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "table.insert", decline_reason: "args", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "table.unpack", decline_reason: "results", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "string.byte", decline_reason: "results", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "string.sub", decline_reason: "values", bound: ExpectedBound::WholeCode },
        BuiltinEditCase { builtin: "string.byte", decline_reason: "args", bound: ExpectedBound::SiteOnly },
        BuiltinEditCase { builtin: "string.sub", decline_reason: "args", bound: ExpectedBound::SiteOnly },
    ];
    let temp = Temp::new("model-builtin-edit-bounds");
    for (number, case) in cases.into_iter().enumerate() {
        let BuiltinEditCase { builtin: name, decline_reason: reason, bound } = case;
        let mut profile = fixture(&[("Step", vec![caller_work_ms; frames]), ("Argument", vec![argument_work_ms; frames])], SIXTY_FPS);
        profile["builtins"] = json!([{
            "name": name, "advice": {(reason): "Use the equivalent advised form"},
            "sites": [{
                "function": 0, "pc": 3, "line": 12,
                "declines": {(reason): 1000}, "decline_charged_us": charged_us, "fast_equivalent_us": fast_path_us,
                "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {(reason): 1000}, "decline_charged_us": charged_us, "fast_equivalent_us": fast_path_us}]
            }]
        }]);
        let app = open(&temp, &number.to_string(), &profile);
        let issues = found(&app);
        let builtin = of_kind(&issues, "builtin declines");

        close(builtin.ms.unwrap(), expected_site_ms);
        match bound {
            ExpectedBound::WholeCode => {
                close(builtin.upper_ms.unwrap(), expected_upper_ms);
                assert!(builtin.cost_text().contains("recorded work"), "{name} {reason}");
            }
            ExpectedBound::SiteOnly => {
                assert!(builtin.at_most, "{name} {reason}");
                assert!(builtin.upper_ms.is_none(), "{name} {reason}");
                assert!(builtin.cost_text().starts_with("at most"), "{name} {reason}");
            }
        }
        accounts(&app, &issues);
    }
}

#[test]
fn cache_and_builtin_fallbacks_share_physical_work() {
    // One function works 0.3 ms in each of 60 frames. In every frame an unsafe environment declines a `math.abs` call, and
    // the fallback's import lookup (pc 5) lies between the call (pc 3) and the end of its fallback (pc 7), so the clock
    // charged that lookup inside the decline
    let frames = 60.0;
    let function_work_ms = 0.3;
    let lookup_first_touch_us = 100.0;
    let lookup_outcomes_us = 5_700.0;
    let lookup_charged_us = lookup_first_touch_us + lookup_outcomes_us;
    let decline_charged_us = 15_000.0;
    let fast_path_us = 600.0;
    let unsafe_env_outcomes = 720_000;
    let function_work_total_ms = function_work_ms * frames;

    let temp = Temp::new("model-fallback-union");
    let mut data = fixture(&[("Step", vec![function_work_ms; frames as usize])], SIXTY_FPS);
    data["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": [{
        "function": 0, "pc": 5, "line": 12, "op": "GETIMPORT", "kind": "import",
        "outcomes": {"unsafe_env": unsafe_env_outcomes}, "charged_work_us": lookup_charged_us,
        "first_touch_us": lookup_first_touch_us, "outcome_charged_us": lookup_outcomes_us, "extra_work_us": 0,
        "nodes": [{"node": FIRST_FUNCTION_NODE, "first_touch_us": lookup_first_touch_us, "outcome_charged_us": lookup_outcomes_us, "outcomes": {"unsafe_env": unsafe_env_outcomes}}]
    }]});
    data["builtins"] = json!([{"name": "math.abs", "advice": {"unsafe_env": "Restore the safe environment"}, "sites": [{
        "function": 0, "pc": 3, "fallback_end_pc": 7, "line": 12,
        "declines": {"unsafe_env": unsafe_env_outcomes}, "decline_charged_us": decline_charged_us, "fast_equivalent_us": fast_path_us,
        "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"unsafe_env": unsafe_env_outcomes}, "decline_charged_us": decline_charged_us, "fast_equivalent_us": fast_path_us}]
    }]}]);
    let app = open(&temp, "fallback", &data);
    let issues = found(&app);
    let cache = of_kind(&issues, "cache misses");
    let builtin = of_kind(&issues, "builtin declines");

    // The lookup's price is its outcomes (the first touch stays); the builtin's is its decline less the fast path, with the
    // lookup's outcomes taken out of it, since the cache issue already holds them
    let lookup_ms_per_frame = lookup_outcomes_us / 1000.0 / frames;
    let builtin_ms_per_frame = (decline_charged_us - fast_path_us) / 1000.0 / frames;
    close(cache.ms.unwrap(), lookup_ms_per_frame);
    close(builtin.ms.unwrap(), builtin_ms_per_frame);
    assert!(cache.upper_ms.is_some());
    assert!(cache.cost_text().contains("recorded work"));

    // Together they hold the builtin's work once, not the lookup's twice
    let mut both = cache.work.clone();
    both.union(&builtin.work);
    let work_by_node = vec![0.0, 0.0, function_work_total_ms];
    close(both.total(&work_by_node) / frames, builtin_ms_per_frame);

    assert!(builtin.evidence.iter().any(|line| line.contains("overlaps cache misses")));
    assert!(builtin.at_most);
    assert!(builtin.cost_text().contains("recorded work"));
    close(builtin.upper_ms.unwrap(), function_work_ms);
    assert!(builtin.advice.contains("combination") && builtin.advice.contains("types"));
    assert!(!builtin.advice.contains("nothing elsewhere"));
    accounts(&app, &issues);
}

#[test]
fn complete_issue_summaries_ignore_unrelated_ten_nanoseconds() {
    // The issue's frame summary comes with the profile (`issue_stats`), so changing another function's work by 10 ns, which
    // moves which heavy frames the timeline keeps, must not change the issue's peak or its listing
    let frames = 600;
    let spike_frame = 29;
    let summary_key = "builtin:math.abs:0:12";
    let summary_active_frames: usize = 1;
    let summary_largest_us = 800.0;
    let summary_peak_frame = 30;
    let summary_over_budget_us = 0.0;
    let declined_calls = 1000;
    let decline_charged_us = 900.0;
    let fast_path_us = 100.0;
    let unrelated_work_ms = 7.333330;
    let ten_nanoseconds_ms = 0.000010;

    let temp = Temp::new("model-complete-summaries");
    for (index, noise_ms) in [0.0, ten_nanoseconds_ms].into_iter().enumerate() {
        let mut data = fixture(
            &[("Step", spike(frames, 1.0, spike_frame)), ("Other", spike(frames, unrelated_work_ms + noise_ms, spike_frame))],
            SIXTY_FPS,
        );
        // No frame detail is kept at all, so the summary is the only source of the issue's peak
        data["timeline"]["functions"] = Value::Null;
        data["timeline"]["node_frames"] = json!([]);
        data["timeline"]["site_frames"] = json!([]);
        data["timeline"]["issue_stats"] = json!([[summary_key, summary_active_frames, summary_largest_us, summary_peak_frame, summary_over_budget_us]]);
        data["builtins"] = json!([{"name": "math.abs", "advice": {"types": "Use the existing numeric value"}, "sites": [{
            "function": 0, "pc": 3, "line": 12, "declines": {"types": declined_calls},
            "decline_charged_us": decline_charged_us, "fast_equivalent_us": fast_path_us,
            "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"types": declined_calls}, "decline_charged_us": decline_charged_us, "fast_equivalent_us": fast_path_us}]
        }]}]);
        let app = open(&temp, &index.to_string(), &data);
        let issues = found(&app);
        let builtin = of_kind(&issues, "builtin declines");

        let saving_ms = (decline_charged_us - fast_path_us) / 1000.0;
        close(builtin.ms.unwrap(), saving_ms / frames as f64);
        close(builtin.frame_ms.unwrap(), summary_largest_us / 1000.0);
        assert_eq!(builtin.active_frames, summary_active_frames);
        accounts(&app, &issues);
    }
}

#[test]
fn uncharged_cache_estimates_have_spreads_and_claim_no_clock_work() {
    // 6 million absent-key lookups, at a calibrated 2 ns each (1 to 3 ns), over 60 frames of 0.1 ms: the clock charged
    // nothing for them, so they are an estimate of 0.2 ms a frame (0.1 to 0.3), never a price
    let frames = 60.0;
    let lookups = 6_000_000;
    let uncharged_us = 12_000.0;
    let expected_estimate_ms = 0.2;
    let expected_spread_ms = (0.1, 0.3);

    let temp = Temp::new("model-estimate-spread");
    let mut data = fixture(&[("Step", vec![0.1; frames as usize])], SIXTY_FPS);
    let absent_price = json!({"charged": false, "ns": 2, "low_ns": 1, "high_ns": 3});
    data["luau_caches"] = json!({"prices": {"clock": "work", "kinds": {"table key slot": {"outcomes": {"absent": absent_price}}}}, "sites": [{
        "function": 0, "pc": 3, "line": 12, "op": "GETTABLEKS", "kind": "table key slot",
        "outcomes": {"absent": lookups}, "charged_work_us": 0, "first_touch_us": 0,
        "outcome_charged_us": 0, "extra_work_us": uncharged_us
    }]});
    let app = open(&temp, "estimate", &data);
    let issues = found(&app);
    let cache = of_kind(&issues, "cache misses");

    assert!(cache.ms.is_none());
    close(cache.uncharged, expected_estimate_ms);
    assert_eq!(cache.estimate_range, Some(expected_spread_ms));
    assert!(cache.cost_text().contains("estimated") && cache.cost_text().contains("calibration"));
    close(cache.counted_ms, 0.0);
    assert!(!cache.advice.contains("nothing elsewhere"));
    accounts(&app, &issues);
}

// ---- Building profiles ----

/// One timeline function entry as profile.json lists it
fn function_entry(id: usize, name: &str, total_ms: f64, active_frames: usize) -> Value {
    json!({
        "id": id,
        "key": format!("Model:{name}"),
        "name": name,
        "module": "ReplicatedStorage.Fixture.Model",
        "line": 10 * (id + 1),
        "kind": "Luau",
        "calls": active_frames,
        "self": {"work_ms": total_ms},
        "total": {"work_ms": total_ms},
        "phases": {"steady": {"self_work_ms": total_ms, "total_work_ms": total_ms, "calls": active_frames}},
    })
}

/// The function's summary row in the timeline: [function, total us, active frames, largest frame us, that frame]. The
/// busiest frame is the first of equal ones, and frames are numbered from 1.
fn function_stats_row(id: usize, work: &WorkByFrame) -> Value {
    let total_ms: f64 = work.iter().sum();
    let active_frames = work.iter().filter(|ms| **ms > 0.0).count();
    let (busiest_index, largest_ms) = work.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0))).unwrap();
    json!([id, total_ms * 1000.0, active_frames, largest_ms * 1000.0, busiest_index + 1])
}

/// Each frame's functions, flat: [function id, us, function id, us, ...], for those that worked in it
fn function_history(series: &[(&str, WorkByFrame)]) -> Vec<Vec<Value>> {
    let frames = series[0].1.len();
    let mut history = Vec::new();
    for frame in 0..frames {
        let mut cells = Vec::new();
        for (id, (_, work)) in series.iter().enumerate() {
            if work[frame] > 0.0 {
                cells.push(json!(id));
                cells.push(json!(work[frame] * 1000.0));
            }
        }
        history.push(cells);
    }
    history
}

/// A profile of one steady phase in which each named function works the given ms in each frame. The frames are numbered
/// from 1 and every frame's work is the functions' together.
fn fixture(series: &[(&str, WorkByFrame)], fps: f64) -> Value {
    let frames = series[0].1.len();
    let startup_node = json!([-1, -1, 0, 0, 0, "startup"]);
    let steady_node = json!([0, -1, 0, 0, 0, "steady"]);
    let mut functions = Vec::new();
    let mut tree = vec![startup_node, steady_node];
    let mut stats = Vec::new();
    for (id, (name, work)) in series.iter().enumerate() {
        let total_ms: f64 = work.iter().sum();
        let active_frames = work.iter().filter(|ms| **ms > 0.0).count();
        functions.push(function_entry(id, name, total_ms, active_frames));
        // (a node under the steady phase, running that function: [parent, function, calls, own us, ?, name])
        tree.push(json!([1, id, active_frames, total_ms * 1000.0, 0, null]));
        stats.push(function_stats_row(id, work));
    }
    let total_ms: f64 = series.iter().flat_map(|(_, work)| work).sum();
    // (a row: [frame, phase 1 (steady), part 0, work, game work, harness work])
    let rows: Vec<Value> = (0..frames)
        .map(|frame| {
            let frame_work: f64 = series.iter().map(|(_, work)| work[frame]).sum();
            json!([frame + 1, 1, 0, frame_work, frame_work, 0])
        })
        .collect();
    json!({
        "version": 2,
        "tool": "ree",
        "kinds": ["work", "timeline"],
        "run": {"command": "run", "status": "ok", "frames": frames, "seconds": frames as f64 / fps, "fps": fps},
        "phases": ["startup", "steady"],
        "scope": {"phases": ["steady"], "game_self_ms": total_ms, "harness_self_ms": 0},
        "functions": functions,
        "tree": tree,
        "lines": [],
        "work": {"total_ms": total_ms, "c_calls": true, "c_invocations": true},
        "timeline": {
            "columns": ["frame", "phase", "part", "work_ms", "game_ms", "harness_ms"],
            "rows": rows,
            "functions": function_history(series),
            "function_stats": stats,
        },
    })
}

/// Writes the profile into its own folder under `temp` and opens it
fn open(temp: &Temp, name: &str, profile: &Value) -> App {
    let folder = temp.path().join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("profile.json"), profile.to_string()).unwrap();
    cli::open(&[folder], ThemeKind::Dark).unwrap()
}

/// A function that works `amount` ms in the frame at index `at` and not otherwise
fn spike(frames: usize, amount: f64, at: usize) -> WorkByFrame {
    (0..frames).map(|frame| if frame == at { amount } else { 0.0 }).collect()
}

/// A function that works `amount` ms in every `period`th frame (frames counted from 1)
fn pulse(frames: usize, amount: f64, period: usize) -> WorkByFrame {
    (1..=frames).map(|frame| if frame % period == 0 { amount } else { 0.0 }).collect()
}

// ---- Reading the issue list ----

fn found(app: &App) -> Vec<Issue> {
    app.issues().0.clone()
}

/// The issue of the function with that name
fn function<'a>(issues: &'a [Issue], name: &str) -> &'a Issue {
    issues
        .iter()
        .find(|issue| issue.title.starts_with(&format!("{name} ")))
        .unwrap_or_else(|| panic!("{name}: {issues:#?}"))
}

/// The one issue of that kind
fn of_kind<'a>(issues: &'a [Issue], kind: &str) -> &'a Issue {
    issues.iter().find(|issue| issue.kind == kind).unwrap_or_else(|| panic!("no {kind} issue: {issues:#?}"))
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

/// The issues' counted work, with the closing row's, is the work the profile measured, and there is one closing row
fn accounts(app: &App, issues: &[Issue]) {
    let profile = app.profile();
    let measured_per_frame = profile.scope_work().unwrap().0 / profile.scope_frames(&profile.scope()).unwrap();
    close(issues.iter().map(|issue| issue.counted_ms).sum(), measured_per_frame);
    assert_eq!(issues.iter().filter(|issue| issue.kind == "work below floor").count(), 1);
}

// ---- Rates and events ----

#[test]
fn every_review_cadence_uses_the_full_observed_mean() {
    let temp = Temp::new("model-cadences");
    for frames in [120, 300, 600] {
        // a function that works every fifth frame is priced at its mean, whether or not another function spikes
        let fifth_ms = 0.59;
        let mean_of_fifth = fifth_ms / 5.0;
        for other_spikes in [false, true] {
            let other_ms = if other_spikes { 0.45 } else { 0.0 };
            let profile = fixture(&[("Fifth", pulse(frames, fifth_ms, 5)), ("Other", spike(frames, other_ms, 59))], SIXTY_FPS);
            let app = open(&temp, &format!("fifth-{frames}-{other_spikes}"), &profile);
            let issues = found(&app);
            close(function(&issues, "Fifth").ms.unwrap(), mean_of_fifth);
            accounts(&app, &issues);
        }

        // a steady function with a periodic bump is priced at its mean, and was active in every frame
        let steady_ms = 1.18;
        let bump_ms = 2.95;
        let bump_every_frames = 120;
        let bump_at = 29;
        let work: WorkByFrame = (0..frames).map(|frame| steady_ms + if frame % bump_every_frames == bump_at { bump_ms } else { 0.0 }).collect();
        let mean = work.iter().sum::<f64>() / frames as f64;
        let app = open(&temp, &format!("base-{frames}"), &fixture(&[("Work", work)], SIXTY_FPS));
        let issues = found(&app);
        close(function(&issues, "Work").ms.unwrap(), mean);
        assert_eq!(function(&issues, "Work").active_frames, frames);
        accounts(&app, &issues);

        // cadences that are not a divisor of the frames: each is its mean
        let third_ms = 7.0;
        let seventh_ms = 10.0;
        let app = open(
            &temp,
            &format!("cadence-{frames}"),
            &fixture(&[("Third", pulse(frames, third_ms, 3)), ("Seventh", pulse(frames, seventh_ms, 7)), ("Base", vec![2.0; frames])], SIXTY_FPS),
        );
        let issues = found(&app);
        close(function(&issues, "Third").ms.unwrap(), third_ms / 3.0);
        let seventh_runs = (frames / 7) as f64;
        close(function(&issues, "Seventh").ms.unwrap(), seventh_ms * seventh_runs / frames as f64);
        accounts(&app, &issues);
    }
}

#[test]
fn two_computations_never_become_a_six_millisecond_computation() {
    let temp = Temp::new("model-events");
    let first_at = 59;
    let second_at = 89;
    for frames in [120, 300, 600] {
        for (first_ms, second_ms) in [(19.0, 0.0), (25.0, 0.0), (19.0, 19.0), (25.0, 19.0), (19.001, 19.0)] {
            let mut work = spike(frames, first_ms, first_at);
            work[second_at] = second_ms;
            let app = open(
                &temp,
                &format!("n{frames}-{first_ms}-{second_ms}"),
                &fixture(&[("Spike", work), ("Hot", vec![3.0; frames])], SIXTY_FPS),
            );
            let issues = found(&app);
            let issue = function(&issues, "Spike");
            close(issue.ms.unwrap(), (first_ms + second_ms) / frames as f64);
            let largest_frame_ms = first_ms.max(second_ms);
            assert_eq!(issue.severity, Severity::of_hitch(largest_frame_ms, FRAME_BUDGET_MS));
            close(issue.frame_ms.unwrap(), largest_frame_ms);
            if second_ms == 0.0 {
                // (one computation: a one-off, with its size and frame)
                close(issue.total_ms.unwrap(), first_ms);
                assert_eq!(issue.once, Some(60.0));
                assert!(issue.cost_text().contains("once in this run"));
            } else {
                assert!(issue.total_ms.is_none());
                assert_eq!(issue.active_frames, 2);
                assert!(!issue.title.contains("6.000 ms"));
            }
            accounts(&app, &issues);
        }
    }
}

#[test]
fn ten_nanoseconds_at_the_old_event_threshold_cannot_remove_a_rate() {
    let temp = Temp::new("model-threshold");
    let spike_at = 59;
    let base_ms = 0.04;
    let mut prices = Vec::new();
    // (two spikes 10 ns apart, either side of what used to make a frame an event)
    for spike_ms in [0.41666, 0.41667] {
        let work: WorkByFrame = spike(STEADY_FRAMES, spike_ms, spike_at).into_iter().map(|ms| ms + base_ms).collect();
        let app = open(&temp, &spike_ms.to_string(), &fixture(&[("Border", work), ("Base", vec![1.0; STEADY_FRAMES])], SIXTY_FPS));
        let issues = found(&app);
        let border = function(&issues, "Border");
        prices.push(border.ms.unwrap());
        assert_eq!(border.active_frames, STEADY_FRAMES);
        accounts(&app, &issues);
    }
    let ten_nanoseconds_ms = 0.00001;
    close(prices[1] - prices[0], ten_nanoseconds_ms / STEADY_FRAMES as f64);

    // a small function with a small bump is still priced at its mean, and holds all of the listed work
    let work: WorkByFrame = spike(STEADY_FRAMES, 0.3, spike_at).into_iter().map(|ms| ms + 0.1).collect();
    let app = open(&temp, "small", &fixture(&[("Build", work)], SIXTY_FPS));
    let issues = found(&app);
    close(function(&issues, "Build").ms.unwrap(), 0.1025);
    assert!(function(&issues, "Build").evidence.iter().any(|line| line.starts_with("100.0%")));
    accounts(&app, &issues);
}

#[test]
fn summaries_keep_sub_floor_events_without_filtered_histories() {
    let temp = Temp::new("model-summaries");
    let event_at = 29;
    // (a 120 fps profile, and a 60 fps one whose event is two parts of a split frame added together)
    for (name, fps, frames, event_ms) in [("fast", 120.0, 240, 0.30094479), ("split", 60.0, 600, 0.60459703)] {
        let mut profile = fixture(&[("Event", spike(frames, event_ms, event_at))], fps);
        profile["timeline"].as_object_mut().unwrap().remove("functions");
        let app = open(&temp, name, &profile);
        let issues = found(&app);
        let issue = function(&issues, "Event");
        close(issue.ms.unwrap(), event_ms / frames as f64);
        close(issue.total_ms.unwrap(), event_ms);
        assert_eq!(issue.once, Some(30.0));
        accounts(&app, &issues);
    }
}

// ---- Overlapping fixes ----

/// The call-tree nodes of `nested`'s profile
const SORT_FUNCTION_NODE: usize = 2;
const TABLE_SORT_NODE: usize = 3;
const COMPARATOR_NODE_UNDER_SORT: usize = 4;
const COMPARATOR_NODE_OUTSIDE_SORT: usize = 5;

/// A profile with a function `Sort` that calls `table.sort` with the comparator `Compare`, which also runs outside the
/// sort for `outside_ms` a frame. A string.byte site in the comparator declines 513,000 calls (the work the sort's
/// subtree and the builtin fix both hold).
fn nested(outside_ms: f64) -> Value {
    let mut profile = fixture(&[("Sort", vec![0.2; STEADY_FRAMES]), ("Compare", vec![0.3 + outside_ms; STEADY_FRAMES])], SIXTY_FPS);
    let table_sort = json!({"id": -2, "key": "table.sort", "name": "table.sort", "kind": "C"});
    profile["functions"].as_array_mut().unwrap().push(table_sort);
    let sort_work_us = 24_000;
    let comparator_work_under_sort_us = 36_000;
    let comparator_calls_under_sort = 120_000;
    profile["tree"] = json!([
        [-1, -1, 0, 0, 0, "startup"],
        [0, -1, 0, 0, 0, "steady"],
        [1, 0, STEADY_FRAMES, 0, 0, null],
        // (the C function table.sort, called from line 10 of Sort)
        [SORT_FUNCTION_NODE, -2, STEADY_FRAMES, sort_work_us, 0, "table.sort [C]", 10],
        [TABLE_SORT_NODE, 1, comparator_calls_under_sort, comparator_work_under_sort_us, 0, null],
        [1, 1, comparator_calls_under_sort, outside_ms * 120_000.0, 0, null]
    ]);
    profile["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": []});
    let site_in_comparator = json!({
        "function": 1,
        "node": COMPARATOR_NODE_UNDER_SORT,
        "line": 20,
        "declines": {"args": 513_000},
        "decline_charged_us": 10_260,
        "fast_equivalent_us": 0,
        "fix_cost_exact": true, "fix_instructions_us": 0,
        "fix_cold_us": 0,
    });
    profile["builtins"] = json!([{
        "name": "string.byte",
        "declines": {"args": 513_000},
        "advice": {"args": "Pass the last index"},
        "sites": [site_in_comparator],
    }]);
    // (a profile of contexts: the site's numbers are in its one context, not on the site)
    let site = &mut profile["builtins"][0]["sites"][0];
    let mut context = site.clone();
    context.as_object_mut().unwrap().remove("function");
    context.as_object_mut().unwrap().remove("line");
    site.as_object_mut().unwrap().remove("node");
    site["nodes"] = json!([context]);
    profile
}

/// The work of `nested`'s nodes, ms over its 120 frames, as the union's total reads them
fn nested_node_work(outside_ms: f64) -> [f64; 6] {
    let steady_frames = STEADY_FRAMES as f64;
    [0.0, 0.0, 0.0, 24.0, 36.0, outside_ms * steady_frames]
}

#[test]
fn partial_comparator_overlap_intersects_nodes_and_preserves_outside_work() {
    let temp = Temp::new("model-union");
    for outside_ms in [0.3, 0.04] {
        let app = open(&temp, &outside_ms.to_string(), &nested(outside_ms));
        let issues = found(&app);
        let sort = of_kind(&issues, "sort calls");
        let decline = of_kind(&issues, "builtin declines");
        close(sort.ms.unwrap(), 0.5);
        // (10.26 ms of declined calls over 120 frames, less the 0.3 ms of the sort-and-comparator work already in the sort)
        close(decline.ms.unwrap(), 0.0855);
        let mut union = sort.work.clone();
        union.union(&decline.work);
        close(union.total(&nested_node_work(outside_ms)) / STEADY_FRAMES as f64, 0.5);
        close(function(&issues, "Compare").ms.unwrap(), 0.3 + outside_ms);
        assert!(function(&issues, "Compare").work.nodes.contains(&COMPARATOR_NODE_OUTSIDE_SORT));
        accounts(&app, &issues);
    }
}

#[test]
fn comparison_refinalization_keeps_the_complete_edit_bound() {
    let temp = Temp::new("model-most");
    let _ = open(&temp, "comparison", &nested(0.3));
    let folder = temp.path().join("comparison");
    let comparison = json!({
        "format": "ree-what-if",
        "baseline": "base",
        "variants": [{"name": "base", "slug": "base"}],
        "sizes": [1],
        "runs": [{"variant": "base", "size": 1, "status": "ok", "profile": "."}],
    });
    std::fs::write(folder.join("compare.json"), comparison.to_string()).unwrap();
    let app = cli::open(&[folder.join("compare.json")], ThemeKind::Dark).unwrap();
    let mut issues = found(&app);
    // Re-finalizing must not forget that the sort edit's bound can exceed Compare's recorded part.
    for _ in 0..3 {
        ree_profile_view::issues::finish(&mut issues);
    }
    // The sort edit could save up to 0.8 ms, more than Compare's 0.6, so no issue can claim to save the most
    let strongest: Vec<_> = issues.iter().filter(|issue| issue.advice.contains("nothing elsewhere")).collect();
    assert!(strongest.is_empty());
    close(function(&issues, "Compare").ms.unwrap(), 0.6);
    let sort = of_kind(&issues, "sort calls");
    close(sort.ms.unwrap(), 0.5);
    close(sort.upper_ms.unwrap(), 0.8);
}

#[test]
fn builtin_prices_subtract_the_same_calls_replacement_cost() {
    let temp = Temp::new("model-net");
    let mut profile = fixture(&[("Read", vec![0.5; STEADY_FRAMES])], SIXTY_FPS);
    profile["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": []});
    let declined_calls = 1_200_000;
    let charged_us = 14_652.342;
    let fast_path_us = 3_221.3860002;
    profile["builtins"] = json!([{
        "name": "string.sub",
        "declines": {"args": declined_calls},
        "advice": {"args": "Pass the final index"},
        "sites": [{
            "function": 0,
            "node": FIRST_FUNCTION_NODE,
            "declines": {"args": declined_calls},
            "decline_charged_us": charged_us,
            "fix_cost_exact": true, "fix_instructions_us": 0,
            "fix_cold_us": 0,
            "fast_equivalent_us": fast_path_us,
        }],
    }]);
    let app = open(&temp, "net", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    // (what the calls were charged, less what the same calls cost on the fast path, over the frames)
    close(issue.ms.unwrap(), (charged_us - fast_path_us) / 1000.0 / STEADY_FRAMES as f64);
    accounts(&app, &issues);

    // a rewrite whose own instruction cost is unknown cannot be priced from the fast path
    let site = &mut profile["builtins"][0]["sites"][0];
    site["fix_unpriced"] = json!("the advised replacement's instruction cost is unavailable");
    let app = open(&temp, "unknown-setup", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    assert!(issue.ms.is_none(), "a known fast path cannot price a different rewrite: {issue:#?}");

    // nor can one whose fast path cost is unknown
    let site = profile["builtins"][0]["sites"][0].as_object_mut().unwrap();
    site.remove("fix_unpriced");
    site.remove("fast_equivalent_us");
    let app = open(&temp, "unknown", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    assert!(issue.ms.is_none());
    assert!(issue.evidence[0].starts_with("not priced:"));
}

#[test]
fn numeric_declines_with_complete_prices_use_known_advice() {
    let temp = Temp::new("model-numeric-advice");
    let mut profile = fixture(&[("Read", vec![0.5; STEADY_FRAMES])], SIXTY_FPS);
    profile["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": []});
    let declined_calls = 120_000;
    let charged_us = 30_000;
    let fast_path_us = 12_000;
    profile["builtins"] = json!([{
        "name": "math.abs",
        "advice": {},
        "sites": [{
            "function": 0,
            "line": 10,
            "pc": 3,
            "declines": {"types": declined_calls},
            "decline_charged_us": charged_us,
            "fast_equivalent_us": fast_path_us,
            "nodes": [{
                "node": FIRST_FUNCTION_NODE,
                "declines": {"types": declined_calls},
                "decline_charged_us": charged_us,
                "fast_equivalent_us": fast_path_us,
            }],
        }],
    }]);
    let app = open(&temp, "numeric", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    close(issue.ms.unwrap(), (charged_us - fast_path_us) as f64 / 1000.0 / STEADY_FRAMES as f64);
    assert!(issue.advice.contains("existing numeric value"));
    assert!(issue.advice.contains("no added conversion"));
    accounts(&app, &issues);
}

// ---- Listing, severity and the totals ----

#[test]
fn duty_stretches_chunks_listing_and_severity_are_independent_of_other_work() {
    let temp = Temp::new("model-other");

    // a function busy in a growing share of its frames: the price grows with the share and the severity never falls
    let busy_ms = 0.59;
    let duty_steps = 20;
    let mut previous_price = 0.0;
    let mut previous_severity = Severity::Info;
    for duty in 1..duty_steps {
        let busy_in_frame = |frame: usize| (frame + 1) * duty / duty_steps > frame * duty / duty_steps;
        let work: WorkByFrame = (0..STEADY_FRAMES).map(|frame| if busy_in_frame(frame) { busy_ms } else { 0.0 }).collect();
        let app = open(&temp, &format!("d{duty}"), &fixture(&[("Work", work)], SIXTY_FPS));
        let issues = found(&app);
        accounts(&app, &issues);
        let expected_price = busy_ms * duty as f64 / duty_steps as f64;
        assert!(expected_price > previous_price);
        previous_price = expected_price;
        let work_issue = function(&issues, "Work");
        close(work_issue.ms.unwrap(), expected_price);
        assert!(work_issue.severity <= previous_severity);
        previous_severity = work_issue.severity;
    }

    // a stretch of busy frames is a rate, however long it is
    for frames in [120, 300, 600] {
        for fraction in [0.1, 0.2, 0.33, 0.5] {
            let busy_frames = (frames as f64 * fraction).round() as usize;
            let work: WorkByFrame = (0..frames).map(|frame| if frame < busy_frames { 1.0 } else { 0.0 }).collect();
            let app = open(&temp, &format!("s{frames}-{fraction}"), &fixture(&[("Wave", work)], SIXTY_FPS));
            let issues = found(&app);
            let wave = function(&issues, "Wave");
            close(wave.ms.unwrap(), busy_frames as f64 / frames as f64);
            assert!(wave.once.is_none(), "a sustained stretch is a rate: {wave:#?}");
            accounts(&app, &issues);
        }
    }

    // chunks of one size are one price, however they are arranged
    let chunk_ms = 4.75;
    let chunks_total_ms = 19.0;
    for chunk_frames in [[30, 32, 34, 36], [30, 31, 32, 33]] {
        let work: WorkByFrame = (0..STEADY_FRAMES).map(|frame| if chunk_frames.contains(&frame) { chunk_ms } else { 0.0 }).collect();
        let app = open(&temp, &format!("c{}", chunk_frames[1]), &fixture(&[("Chunks", work)], SIXTY_FPS));
        let issues = found(&app);
        close(function(&issues, "Chunks").ms.unwrap(), chunks_total_ms / STEADY_FRAMES as f64);
        accounts(&app, &issues);
    }

    // a small function's severity does not depend on another function's spike
    for other_spikes in [false, true] {
        let other_ms = if other_spikes { 30.0 } else { 0.0 };
        let app = open(
            &temp,
            &format!("isolate{other_spikes}"),
            &fixture(&[("Small", vec![0.0796; STEADY_FRAMES]), ("Other", spike(STEADY_FRAMES, other_ms, 59))], SIXTY_FPS),
        );
        let issues = found(&app);
        assert_eq!(function(&issues, "Small").severity, Severity::Info);
        accounts(&app, &issues);
    }

    // six equal functions are each critical: no one's severity is taken by the others
    let equal_functions: Vec<_> = (0..6).map(|_| ("Equal", vec![2.49; STEADY_FRAMES])).collect();
    let app = open(&temp, "six", &fixture(&equal_functions, SIXTY_FPS));
    let issues = found(&app);
    let critical_priced = issues.iter().filter(|issue| issue.severity == Severity::Critical && issue.ms.is_some()).count();
    assert_eq!(critical_priced, 6);
    accounts(&app, &issues);

    // moving a little work between two functions across the listing floor moves the listed work by about that much
    let mut listed_ms = Vec::new();
    for (index, rates) in [[0.139, 0.043], [0.039, 0.143]].into_iter().enumerate() {
        let app = open(
            &temp,
            &format!("transfer{index}"),
            &fixture(&[("A", vec![rates[0]; STEADY_FRAMES]), ("B", vec![rates[1]; STEADY_FRAMES])], SIXTY_FPS),
        );
        let issues = found(&app);
        listed_ms.push(issues.iter().filter(|issue| issue.kind != "work below floor").map(|issue| issue.counted_ms).sum::<f64>());
        accounts(&app, &issues);
    }
    close(listed_ms[0], 0.182);
    close(listed_ms[1], 0.143);
    assert!(listed_ms[0] - listed_ms[1] <= 0.1);
}

#[test]
fn sites_without_context_records_cannot_claim_a_partial_intersection() {
    let temp = Temp::new("model-old-context");
    let mut profile = nested(0.3);
    profile["builtins"][0]["sites"][0].as_object_mut().unwrap().remove("nodes");
    let app = open(&temp, "old", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    assert!(issue.ms.is_none());
    assert_eq!(issue.evidence[0], "not priced: needs per-context site records");
    accounts(&app, &issues);
}

#[test]
fn calling_lines_accept_the_parallel_array_and_earlier_seventh_elements() {
    let temp = Temp::new("model-tree-lines");
    // an earlier profile keeps a node's calling line as the tree row's seventh element
    let earlier = nested(0.3);
    let from_seventh_elements = open(&temp, "old", &earlier);
    // a current one keeps the lines in a parallel array
    let mut current = earlier.clone();
    let mut lines = Vec::new();
    for node in current["tree"].as_array_mut().unwrap() {
        let cells = node.as_array_mut().unwrap();
        let has_line = cells.len() == 7;
        lines.push(if has_line { cells.pop().unwrap() } else { Value::Null });
    }
    current["tree_lines"] = json!(lines);
    let from_parallel_array = open(&temp, "new", &current);

    let lines_of = |app: &App| -> Vec<Option<i64>> { app.profile().raw_nodes().iter().map(|node| node.line).collect() };
    assert_eq!(lines_of(&from_seventh_elements), lines_of(&from_parallel_array));
    for app in [&from_seventh_elements, &from_parallel_array] {
        let issues = found(app);
        let sort = of_kind(&issues, "sort calls");
        close(sort.ms.unwrap(), 0.5);
        assert_eq!(sort.target, ree_profile_view::issues::Target::Line(0, 10));
    }
}

// ---- Severity, hitches and what is listed ----

#[test]
fn hitch_severity_uses_the_recorded_budget_and_every_reported_phase() {
    let temp = Temp::new("model-budget");
    let event_ms = 10.0;
    // (10 ms is inside a 60 fps frame's 16.7 ms and over a 120 fps frame's 8.3 ms)
    for (fps, severity) in [(60.0, Severity::Info), (120.0, Severity::Warning)] {
        let app = open(&temp, &format!("fps{fps}"), &fixture(&[("Event", spike(600, event_ms, 29))], fps));
        let issues = found(&app);
        let issue = function(&issues, "Event");
        close(issue.ms.unwrap(), event_ms / 600.0);
        assert_eq!(issue.severity, severity);
        accounts(&app, &issues);
    }

    // a one-off in the startup phase counts when the report covers it: move the profile's one frame into startup
    let setup_ms = 30.0;
    let mut profile = fixture(&[("Setup", spike(STEADY_FRAMES, setup_ms, 0))], SIXTY_FPS);
    profile["scope"]["phases"] = json!(["startup", "steady"]);
    let startup_phase = 0;
    profile["tree"][FIRST_FUNCTION_NODE][0] = json!(startup_phase);
    profile["timeline"]["rows"][0][0] = json!(0); // the frame number
    profile["timeline"]["rows"][0][1] = json!(startup_phase);
    profile["timeline"]["function_stats"][0][4] = json!(0); // the busiest frame
    let app = open(&temp, "startup", &profile);
    let issues = found(&app);
    let issue = function(&issues, "Setup");
    close(issue.ms.unwrap(), setup_ms / STEADY_FRAMES as f64);
    assert_eq!(issue.once, Some(0.0));
    assert_eq!(issue.severity, Severity::Serious);
    accounts(&app, &issues);
}

#[test]
fn an_unlisted_larger_rate_prevents_the_strongest_saving_claim() {
    let temp = Temp::new("model-unlisted-most");
    // (Quiet's mean is under the listing floor, so it is not listed, yet it is larger than Event's)
    let work = [("Quiet", vec![0.04; STEADY_FRAMES]), ("Event", spike(STEADY_FRAMES, 3.6, 59))];
    let app = open(&temp, "run", &fixture(&work, SIXTY_FPS));
    let mut issues = found(&app);
    close(function(&issues, "Event").ms.unwrap(), 0.03);
    assert!(issues.iter().all(|issue| !issue.title.starts_with("Quiet ")));
    for _ in 0..3 {
        ree_profile_view::issues::finish(&mut issues);
        assert!(issues.iter().all(|issue| !issue.advice.contains("nothing elsewhere")));
    }
    accounts(&app, &issues);
}

#[test]
fn a_site_hitch_uses_its_own_net_work_in_the_heavy_frame() {
    let temp = Temp::new("model-site-hitch");
    let frames = 600;
    let heavy_frame_index = 59;
    let heavy_frame_ms = 30.0;
    let mut profile = fixture(&[("Work", spike(frames, heavy_frame_ms, heavy_frame_index))], SIXTY_FPS);
    let site_pc = 7;
    // (the site charged 22.5 ms and saves 20 ms net of its 2.5 ms replacement, all in the one heavy frame)
    let saving_us_in_heavy_frame = 20_000;
    profile["builtins"] = json!([{
        "name": "string.byte",
        "declines": {"args": 100_000},
        "advice": {"args": "Pass the first index"},
        "sites": [{
            "function": 0,
            "pc": site_pc,
            "node": FIRST_FUNCTION_NODE,
            "declines": {"args": 100_000},
            "decline_charged_us": 22_500,
            "fast_equivalent_us": 2_500,
            "fix_cost_exact": true, "fix_instructions_us": 0,
            "fix_cold_us": 0,
        }],
    }]);
    profile["timeline"]["site_frames"] = json!([[heavy_frame_index, "builtin", 0, site_pc, FIRST_FUNCTION_NODE, saving_us_in_heavy_frame]]);
    let app = open(&temp, "builtin", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "builtin declines");
    close(issue.ms.unwrap(), 20.0 / frames as f64);
    close(issue.over_budget_ms, 20.0);
    assert_eq!(issue.severity, Severity::Warning);
    accounts(&app, &issues);

    // the same site as a cache site that rehashed: the same price and hitch
    profile.as_object_mut().unwrap().remove("builtins");
    profile["luau_caches"] = json!({"prices": {"clock": "work"}, "sites": [{
        "function": 0,
        "pc": site_pc,
        "node": FIRST_FUNCTION_NODE,
        "op": "SETTABLEKS",
        "outcomes": {"rehashed": 100_000},
        "charged_work_us": 25_000,
        "first_touch_us": 2_500,
        "outcome_charged_us": 22_500,
        "presized_us": 2_500,
        "extra_work_us": 0,
    }]});
    profile["timeline"]["site_frames"][0][1] = json!("cache");
    let app = open(&temp, "cache", &profile);
    let issues = found(&app);
    let issue = of_kind(&issues, "cache misses");
    close(issue.ms.unwrap(), 20.0 / frames as f64);
    close(issue.over_budget_ms, 20.0);
    assert_eq!(issue.severity, Severity::Warning);
    accounts(&app, &issues);
}

#[test]
fn a_zero_or_negative_builtin_saving_is_reported_as_no_saving_on_this_clock() {
    let temp = Temp::new("model-no-saving");
    let declined_calls = 1_200_000;
    let charged_us = 6_000.0;
    // (the fast path costs as much as the declined calls did, then more)
    for fast_path_us in [6_000.0, 6_600.0] {
        let mut profile = fixture(&[("Read", vec![0.5; STEADY_FRAMES])], SIXTY_FPS);
        profile["builtins"] = json!([{
            "name": "string.sub",
            "declines": {"args": declined_calls},
            "advice": {"args": "Pass the final index"},
            "sites": [{
                "function": 0,
                "pc": 7,
                "declines": {"args": declined_calls},
                "decline_charged_us": charged_us,
                "fast_equivalent_us": fast_path_us,
                "nodes": [{
                    "node": FIRST_FUNCTION_NODE,
                    "declines": {"args": declined_calls},
                    "decline_charged_us": charged_us,
                    "fast_equivalent_us": fast_path_us,
                }],
            }],
        }]);
        let app = open(&temp, &format!("cost{fast_path_us}"), &profile);
        let issues = found(&app);
        let issue = of_kind(&issues, "builtin declines");
        assert!(issue.no_saving && issue.ms.is_none());
        assert_eq!(issue.cost_text(), "fixing it saves nothing on this clock");
        assert_eq!(issue.cost_short(FRAME_BUDGET_MS), "no saving");
        assert!(issue.evidence[0].contains("fixing it saves nothing on this clock"));
        accounts(&app, &issues);
    }
}

#[test]
fn a_prototype_replacement_cost_is_shared_without_splitting_it_between_callers() {
    let temp = Temp::new("model-prototype-cost");
    let mut profile = nested(0.3);
    // (one edit of the comparator's line adds 0.6 ms of cold code to the prototype, which both callers' contexts share)
    let cold_code_us = 600;
    let charged_under_sort_us = 10_260;
    let charged_outside_sort_us = 12_000;
    profile["builtins"][0] = json!({
        "name": "string.sub",
        "declines": {"args": 1_113_000},
        "advice": {"args": "Pass the final index"},
        "sites": [{
            "function": 1,
            "pc": 9,
            "line": 20,
            "decline_charged_us": charged_under_sort_us + charged_outside_sort_us,
            "fast_equivalent_us": 0,
            "fix_cost_exact": true, "fix_instructions_us": 0,
            "fix_cold_us": cold_code_us,
            "nodes": [
                {"node": COMPARATOR_NODE_UNDER_SORT, "declines": {"args": 513_000}, "decline_charged_us": charged_under_sort_us, "fast_equivalent_us": 0, "fix_cost_exact": true, "fix_instructions_us": 0},
                {"node": COMPARATOR_NODE_OUTSIDE_SORT, "declines": {"args": 600_000}, "decline_charged_us": charged_outside_sort_us, "fast_equivalent_us": 0, "fix_cost_exact": true, "fix_instructions_us": 0},
            ],
        }],
    });
    // The function's contexts as [function, node, first frame, last frame]: both callers active in every frame, so the
    // sort fix removes only half of the callers' frames and the cold code is still paid; or the callers take turns,
    // so removing the sort's context leaves the other caller's frames
    let same_frames = json!([[1, COMPARATOR_NODE_UNDER_SORT, 1, 120], [1, COMPARATOR_NODE_OUTSIDE_SORT, 1, 120]]);
    let alternating_callers = json!([[1, COMPARATOR_NODE_UNDER_SORT, 1, 108], [1, COMPARATOR_NODE_OUTSIDE_SORT, 109, 120]]);
    for (name, contexts, sort_and_builtin_per_frame) in [("same-frames", same_frames, 0.595), ("alternating-callers", alternating_callers, 0.5995)] {
        profile["timeline"]["function_contexts"] = contexts;
        let app = open(&temp, name, &profile);
        let issues = found(&app);
        let sort = of_kind(&issues, "sort calls");
        let builtin = of_kind(&issues, "builtin declines");
        // (22.26 ms charged, less the 0.6 ms of cold code paid once, over the frames)
        close(builtin.ms.unwrap(), (22.26 - 0.6) / STEADY_FRAMES as f64);
        let mut union = sort.work.clone();
        union.union(&builtin.work);
        let node_work = [0.0, 0.0, 0.0, 24.0, 36.0, 36.0];
        close(union.total(&node_work) / STEADY_FRAMES as f64, sort_and_builtin_per_frame);
        union.union(&function(&issues, "Compare").work);
        close(union.total(&node_work) / STEADY_FRAMES as f64, 0.8);
        accounts(&app, &issues);
    }
}

#[test]
fn compiled_copies_on_one_source_line_keep_the_source_edits_full_net_price() {
    let temp = Temp::new("model-compiled-copies");
    let mut profile = fixture(&[("Read", vec![0.5; STEADY_FRAMES])], SIXTY_FPS);
    // (two compiled copies of one source line: one saves 12 ms, the other costs 1 ms more than it saves; both pay 60 us of
    // cold code, which the one edit pays twice over its 120 frames)
    let cold_code_us_per_copy = 60;
    profile["builtins"] = json!([{
        "name": "string.byte",
        "declines": {"args": 200_000},
        "advice": {"args": "Pass the first index"},
        "sites": [
            {
                "function": 0, "pc": 7, "line": 20,
                "decline_charged_us": 12_000, "fast_equivalent_us": 0, "fix_cost_exact": true, "fix_instructions_us": 0, "fix_cold_us": cold_code_us_per_copy,
                "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"args": 100_000}, "decline_charged_us": 12_000, "fast_equivalent_us": 0, "fix_cost_exact": true, "fix_instructions_us": 0}],
            },
            {
                "function": 0, "pc": 12, "line": 20,
                "decline_charged_us": 1_000, "fast_equivalent_us": 2_000, "fix_cost_exact": true, "fix_instructions_us": 0, "fix_cold_us": cold_code_us_per_copy,
                "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"args": 100_000}, "decline_charged_us": 1_000, "fast_equivalent_us": 2_000, "fix_cost_exact": true, "fix_instructions_us": 0}],
            },
        ],
    }]);
    profile["timeline"]["function_contexts"] = json!([[0, FIRST_FUNCTION_NODE, 1, STEADY_FRAMES]]);
    let app = open(&temp, "run", &profile);
    let issues = found(&app);
    let copies: Vec<_> = issues.iter().filter(|issue| issue.kind == "builtin declines").collect();
    assert_eq!(copies.len(), 1);
    close(copies[0].ms.unwrap(), (12.0 - 1.0 - 0.12) / STEADY_FRAMES as f64);
    accounts(&app, &issues);
}

#[test]
fn a_nonpositive_append_site_part_still_bounds_the_complete_edit() {
    let temp = Temp::new("model-fast-rehash");
    let mut profile = fixture(&[("Append", vec![0.5; STEADY_FRAMES])], SIXTY_FPS);
    // (the fast path of table.insert costs more than the declined calls did: it keeps rehashing the growing table)
    let charged_us = 2_470.116;
    let fast_path_us = 6_604.884;
    profile["builtins"] = json!([{
        "name": "table.insert",
        "declines": {"args": 132_000},
        "advice": {"args": "When pos is #t+1 use table.insert(t, value)"},
        "sites": [{
            "function": 0,
            "pc": 7,
            "line": 20,
            "decline_charged_us": charged_us,
            "fast_equivalent_us": fast_path_us,
            "fix_unpriced": "the advised argument rewrite has no measured instruction cost",
            "nodes": [{"node": FIRST_FUNCTION_NODE, "declines": {"args": 132_000}, "decline_charged_us": charged_us, "fast_equivalent_us": fast_path_us}],
        }],
    }]);
    let app = open(&temp, "run", &profile);
    let issues = found(&app);
    let append = of_kind(&issues, "builtin declines");
    assert!(!append.no_saving && append.ms.is_none(), "{append:#?}");
    close(append.upper_ms.unwrap(), 0.5);
    assert!(append.cost_text().contains("recorded site part saves nothing on this clock"));
    assert!(append.cost_text().contains("complete edit at most"));
    accounts(&app, &issues);
}

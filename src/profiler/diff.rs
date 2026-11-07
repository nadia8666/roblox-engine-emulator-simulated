//! `ree profile diff <before> <after> [--out <dir>] [--top <n>]`: two profile directories compared (their
//! profile.json): the headline figures, the functions whose work, instructions, calls, allocations or host time moved,
//! the lines of modules whose source is the same in both, the caches, the heap and the allocations by type, and the
//! timeline. Writes `diff.txt` (also printed), `diff.json` and `diff.html` in `--out` (default: the after directory).
//!
//! The work clock is the same every run of the same code, so a difference in work, instructions or allocations is the
//! code's; host time is noise unless runs were interleaved (ABBA) and the noise of a repeat run is known.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use serde_json::{Value as Json, json};

fn load(dir: &Path) -> Result<Json, String> {
    let path = if dir.is_file() { dir.to_path_buf() } else { dir.join("profile.json") };
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn num(value: &Json) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn change(before: f64, after: f64) -> String {
    let delta = after - before;
    if before.abs() < 1e-12 {
        return if delta.abs() < 1e-12 { "same".into() } else { "new".into() };
    }
    format!("{:+.1} %", delta / before * 100.0)
}

fn bytes(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let value = value.abs();
    if value >= 1024.0 * 1024.0 {
        format!("{sign}{:.2} MB", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{sign}{:.1} KB", value / 1024.0)
    } else {
        format!("{sign}{value:.0} B")
    }
}

/// One compared figure: (name, before, after, unit)
struct Figure {
    name: String,
    before: f64,
    after: f64,
    unit: &'static str,
}

impl Figure {
    fn show(&self, value: f64) -> String {
        match self.unit {
            "bytes" => bytes(value),
            "ms" => format!("{value:.3} ms"),
            "%" => format!("{value:.1} %"),
            "ratio" => format!("{value:.2}"),
            _ => format!("{value:.0}"),
        }
    }
}

/// Per-function figures keyed across runs
#[derive(Default, Clone, Copy)]
struct FunctionFigures {
    self_work: f64,
    total_work: f64,
    calls: f64,
    insns: f64,
    alloc: f64,
    time: f64,
}

/// The phases both profiles report the same way (report.rs's `scope`), else `steady` when both marked one; None for
/// profiles from a ree that kept no per-phase figures, or runs with no phase in common: they are compared whole
fn shared_scope(before: &Json, after: &Json) -> Option<Vec<String>> {
    let names = |value: &Json| -> Option<Vec<String>> { value.as_array().map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()) };
    let (b, a) = (names(&before["scope"]["phases"])?, names(&after["scope"]["phases"])?);
    if b == a && !b.is_empty() {
        return Some(b);
    }
    let marked = |profile: &Json| names(&profile["phases"]).is_some_and(|phases| phases.iter().any(|phase| phase == "steady"));
    (marked(before) && marked(after)).then(|| vec!["steady".to_string()])
}

/// A function's figures in a diff, and whether it is the harness's own code (report.rs `harness_module`). With a scope,
/// the work, calls and instructions are those phases' and only the game's side of the work (a C function's part done for
/// the harness's code goes with the harness); allocations and host time stay the whole run's.
fn functions(profile: &Json, scope: Option<&[String]>) -> BTreeMap<String, (String, bool, FunctionFigures)> {
    let mut out = BTreeMap::new();
    for f in profile["functions"].as_array().into_iter().flatten() {
        if f["kind"] == "engine Luau" {
            continue;
        }
        let key = f["key"].as_str().unwrap_or_default().to_string();
        let label = match f["module"].as_str() {
            Some(module) => format!("{}  {}:{}", f["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("anonymous"), module.rsplit(['.', '/', '\\']).next().unwrap_or(module), f["line"]),
            None => f["name"].as_str().unwrap_or("?").to_string(),
        };
        let harness = f["harness"].as_bool().unwrap_or(false);
        let entry: &mut (String, bool, FunctionFigures) = out.entry(key).or_insert_with(|| (label, harness, FunctionFigures::default()));
        match scope {
            Some(phases) => {
                for phase in phases {
                    let figures = &f["phases"][phase.as_str()];
                    let harness_part = if harness { 0.0 } else { num(&figures["harness_self_work_ms"]) };
                    entry.2.self_work += num(&figures["self_work_ms"]) - harness_part;
                    entry.2.total_work += num(&figures["total_work_ms"]);
                    entry.2.calls += num(&figures["calls"]);
                    entry.2.insns += num(&figures["insns"]);
                }
            }
            None => {
                entry.2.self_work += num(&f["self"]["work_ms"]);
                entry.2.total_work += num(&f["total"]["work_ms"]);
                // (calls through copies -O2 inlined count too: a change that inlines a function or stops inlining it keeps its calls)
                entry.2.calls += num(&f["calls"]) + num(&f["inlined"]["calls"]);
                entry.2.insns += num(&f["self"]["insns"]);
            }
        }
        entry.2.alloc += num(&f["self"]["alloc_bytes"]);
        entry.2.time += num(&f["self"]["time_ms"]);
    }
    out
}

/// The self work of the game's code and of the harness's in the scope's phases: (game, harness), in ms
fn side_totals(profile: &Json, scope: &[String]) -> (f64, f64) {
    let mut sides = (0.0, 0.0);
    for f in profile["functions"].as_array().into_iter().flatten() {
        for phase in scope {
            let figures = &f["phases"][phase.as_str()];
            let harness = num(&figures["harness_self_work_ms"]);
            sides.0 += num(&figures["self_work_ms"]) - harness;
            sides.1 += harness;
        }
    }
    sides
}

/// Lines of modules whose source is unchanged: (module, line) -> (self work, insns, alloc)
fn lines(profile: &Json, same: &BTreeSet<String>) -> BTreeMap<(String, i64), (f64, f64, f64)> {
    let mut by_id: BTreeMap<i64, String> = BTreeMap::new();
    for f in profile["functions"].as_array().into_iter().flatten() {
        if let (Some(id), Some(module)) = (f["id"].as_i64(), f["module"].as_str()) {
            by_id.insert(id, module.to_string());
        }
    }
    let mut out = BTreeMap::new();
    for line in profile["lines"].as_array().into_iter().flatten() {
        let Some(module) = line["function"].as_i64().and_then(|id| by_id.get(&id)) else { continue };
        if !same.contains(module) {
            continue;
        }
        let entry: &mut (f64, f64, f64) = out.entry((module.clone(), line["line"].as_i64().unwrap_or(0))).or_default();
        entry.0 += num(&line["work_ms"]);
        entry.1 += num(&line["insns"]);
        entry.2 += num(&line["alloc_bytes"]);
    }
    out
}

fn last_snapshot(profile: &Json) -> Option<&Json> {
    profile["heap"]["snapshots"].as_array().and_then(|snapshots| snapshots.last())
}

/// A snapshot's reachable heap and its garbage not swept yet: (reachable bytes, reachable objects, garbage bytes, whether
/// the profile counted them apart). A profile from before has only its walk's unreachable objects, which counted the
/// names only function prototypes hold as garbage: its reachable heap is estimated as the rest.
fn heap_split(snapshot: &Json) -> (f64, f64, f64, bool) {
    if snapshot["reachable"].is_object() {
        return (num(&snapshot["reachable"]["bytes"]), num(&snapshot["reachable"]["count"]), num(&snapshot["unswept"]["bytes"]), true);
    }
    let unreachable = &snapshot["unreachable"];
    (num(&snapshot["object_bytes"]) - num(&unreachable["bytes"]), num(&snapshot["objects"]) - num(&unreachable["count"]), num(&unreachable["bytes"]), false)
}

fn timeline_stats(profile: &Json) -> Option<(f64, f64, f64, f64)> {
    let timeline = &profile["timeline"];
    let columns: Vec<&str> = timeline["columns"].as_array()?.iter().filter_map(Json::as_str).collect();
    let rows = timeline["rows"].as_array()?;
    if rows.is_empty() {
        return None;
    }
    let column = |name: &str| columns.iter().position(|c| *c == name);
    let values = |name: &str| -> Vec<f64> { column(name).map(|index| rows.iter().map(|row| num(&row[index])).collect()).unwrap_or_default() };
    let work = values("work_ms");
    let host = values("host_ms");
    let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
    let max = work.iter().copied().fold(0.0, f64::max);
    Some((mean(&work), max, mean(&host), values("heap_bytes").last().copied().unwrap_or(0.0)))
}

/// Compares two profiles; the text (printed by the command) and the JSON
pub fn compare(before: &Json, after: &Json, top: usize) -> (String, Json) {
    let mut figures: Vec<Figure> = Vec::new();
    let mut add = |name: &str, b: f64, a: f64, unit: &'static str| figures.push(Figure { name: name.into(), before: b, after: a, unit });
    // the headline: the game's code in the phases both report, apart from the harness's (whose work moves with what it
    // scans, not with the game)
    let scope = shared_scope(before, after);
    let scope_label = scope.as_ref().map(|phases| format!("{} {}", if phases.len() == 1 { "phase" } else { "phases" }, phases.join(", ")));
    if let (Some(phases), Some(label)) = (&scope, &scope_label) {
        let (b, a) = (side_totals(before, phases), side_totals(after, phases));
        add(&format!("the game's code, {label}"), b.0, a.0, "ms");
        add(&format!("the harness's code, {label}"), b.1, a.1, "ms");
    }
    if before["work"].is_object() || after["work"].is_object() {
        add("work clock", num(&before["work"]["total_ms"]), num(&after["work"]["total_ms"]), "ms");
        add("work clock a frame", num(&before["work"]["per_frame_ms"]), num(&after["work"]["per_frame_ms"]), "ms");
        add("script instructions", num(&before["work"]["script_insns"]), num(&after["work"]["script_insns"]), "");
    }
    if before["alloc"].is_object() || after["alloc"].is_object() {
        add("allocated", num(&before["alloc"]["bytes"]), num(&after["alloc"]["bytes"]), "bytes");
        add("allocations", num(&before["alloc"]["objects"]), num(&after["alloc"]["objects"]), "");
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(label) = &scope_label {
        notes.push(format!("the first two figures and the function tables cover {label} (work, calls, instructions); allocations, host time and lines cover the whole run"));
    }
    if let (Some(b), Some(a)) = (last_snapshot(before), last_snapshot(after)) {
        let (b_heap, a_heap) = (heap_split(b), heap_split(a));
        for (side, split) in [("before", &b_heap), ("after", &a_heap)] {
            if !split.3 {
                notes.push(format!("the {side} profile is from a ree that did not count reachable memory apart: its reachable heap is estimated, and its own objects and heap by type hold garbage"));
            }
        }
        // (the reachable heap is what a full collection keeps; the garbage not swept yet depends on where the collector's
        // cycle stood when the run ended, so it is shown apart and never added in)
        add("reachable heap at the end", b_heap.0, a_heap.0, "bytes");
        add("reachable objects at the end", b_heap.1, a_heap.1, "");
        add("garbage not swept yet at the end", b_heap.2, a_heap.2, "bytes");
        add("the run's own reachable objects", num(&b["allocated_since"]["bytes"]), num(&a["allocated_since"]["bytes"]), "bytes");
        add("collector cycles", num(&before["heap"]["gc"]["cycles"]), num(&after["heap"]["gc"]["cycles"]), "");
        add("collector freed", num(&before["heap"]["gc"]["freed"]), num(&after["heap"]["gc"]["freed"]), "bytes");
    }
    if before["time"].is_object() || after["time"].is_object() {
        add("host time sampled (noisy)", num(&before["time"]["sampled_ms"]), num(&after["time"]["sampled_ms"]), "ms");
    }
    // the hardware counters both profiles read (the run thread's; the CPU's own, so they move a little run to run)
    if let (Some(b), Some(a)) = (before["counters"]["run"]["counts"].as_object(), after["counters"]["run"]["counts"].as_object()) {
        for (name, value) in b {
            if let Some(after_value) = a.get(name) {
                add(&format!("{name} (hardware counter)"), num(value), num(after_value), "");
            }
        }
        if let (Some(b), Some(a)) = (before["counters"]["run"]["rates"]["IPC"].as_f64(), after["counters"]["run"]["rates"]["IPC"].as_f64()) {
            add("IPC (hardware counters)", b, a, "ratio");
        }
    }
    if let (Some(b), Some(a)) = (timeline_stats(before), timeline_stats(after)) {
        add("work a frame, mean", b.0, a.0, "ms");
        add("work a frame, worst", b.1, a.1, "ms");
        add("host a frame, mean (noisy)", b.2, a.2, "ms");
        add("VM heap at the last frame, with garbage", b.3, a.3, "bytes");
    }
    add("frames", num(&before["run"]["frames"]), num(&after["run"]["frames"]), "");

    let mut text = String::new();
    let _ = writeln!(text, "ree profile diff: before {} ({} frames), after {} ({} frames)", before["run"]["status"].as_str().unwrap_or("?"), before["run"]["frames"], after["run"]["status"].as_str().unwrap_or("?"), after["run"]["frames"]);
    if before["run"]["frames"] != after["run"]["frames"] || before["run"]["harness"] != after["run"]["harness"] {
        let _ = writeln!(text, "note: the runs differ in frames or harness; compare per-frame figures");
    }
    for note in &notes {
        let _ = writeln!(text, "note: {note}");
    }
    let width = figures.iter().map(|figure| figure.name.len()).max().unwrap_or(0).max(30);
    let _ = writeln!(text, "{:<width$} {:>14} {:>14} {:>10}", "", "before", "after", "change");
    for figure in &figures {
        let _ = writeln!(text, "{:<width$} {:>14} {:>14} {:>10}", figure.name, figure.show(figure.before), figure.show(figure.after), change(figure.before, figure.after));
    }

    // functions: the game's first, the harness's apart
    let (fb, fa) = (functions(before, scope.as_deref()), functions(after, scope.as_deref()));
    let keys: BTreeSet<&String> = fb.keys().chain(fa.keys()).collect();
    let mut rows: Vec<(String, bool, FunctionFigures, FunctionFigures)> = keys
        .into_iter()
        .map(|key| {
            let b = fb.get(key).map(|entry| entry.2).unwrap_or_default();
            let a = fa.get(key).map(|entry| entry.2).unwrap_or_default();
            let (label, harness) = fa.get(key).or_else(|| fb.get(key)).map(|entry| (entry.0.clone(), entry.1)).unwrap_or_else(|| (key.clone(), false));
            (label, harness, b, a)
        })
        .collect();
    let weight = |row: &(String, bool, FunctionFigures, FunctionFigures)| -> f64 {
        (row.3.self_work - row.2.self_work).abs() * 1000.0 + (row.3.alloc - row.2.alloc).abs() / 1024.0 + (row.3.insns - row.2.insns).abs() / 1000.0 + (row.3.time - row.2.time).abs() * 0.01
    };
    rows.retain(|row| weight(row) > 0.0);
    rows.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then_with(|| a.0.cmp(&b.0)));
    let scope_part = scope_label.as_ref().map_or(String::new(), |label| format!(", {label}"));
    for (harness, title, count) in [
        (false, format!("functions that moved most: the game's code{scope_part} (self work ms, calls, self instructions, self allocated: before -> after)"), top),
        (true, format!("the harness's functions that moved most{scope_part} (its own code, left out above)"), top.min(8)),
    ] {
        let side: Vec<&(String, bool, FunctionFigures, FunctionFigures)> = rows.iter().filter(|row| row.1 == harness).collect();
        if side.is_empty() {
            continue;
        }
        let _ = writeln!(text, "\n{title}");
        for (label, _, b, a) in side.iter().take(count) {
            let _ = writeln!(
                text,
                "{:>9.3} -> {:<9.3} {:>9} -> {:<9} {:>10} -> {:<10} {:>9} -> {:<9}  {}",
                b.self_work,
                a.self_work,
                b.calls as u64,
                a.calls as u64,
                b.insns as u64,
                a.insns as u64,
                bytes(b.alloc),
                bytes(a.alloc),
                label
            );
        }
    }

    // lines of unchanged modules
    let hashes = |profile: &Json| -> BTreeMap<String, String> { profile["source_hashes"].as_object().map(|map| map.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default() };
    let (hb, ha) = (hashes(before), hashes(after));
    let same: BTreeSet<String> = hb.iter().filter(|(module, hash)| ha.get(*module) == Some(*hash)).map(|(module, _)| module.clone()).collect();
    let changed: Vec<&String> = hb.keys().filter(|module| ha.get(*module).is_some_and(|hash| Some(hash) != hb.get(*module))).collect();
    let (lb, la) = (lines(before, &same), lines(after, &same));
    let line_keys: BTreeSet<&(String, i64)> = lb.keys().chain(la.keys()).collect();
    let mut line_rows: Vec<(&(String, i64), (f64, f64, f64), (f64, f64, f64))> =
        line_keys.into_iter().map(|key| (key, lb.get(key).copied().unwrap_or_default(), la.get(key).copied().unwrap_or_default())).filter(|(_, b, a)| (a.0 - b.0).abs() > 1e-9 || (a.1 - b.1).abs() > 0.0 || (a.2 - b.2).abs() > 0.0).collect();
    line_rows.sort_by(|x, y| ((y.2.0 - y.1.0).abs()).total_cmp(&(x.2.0 - x.1.0).abs()).then_with(|| x.0.cmp(y.0)));
    if !line_rows.is_empty() {
        let _ = writeln!(text, "\nlines that moved most in modules whose source is unchanged (self work ms, instructions)");
        for ((module, line), b, a) in line_rows.iter().take(top) {
            let _ = writeln!(text, "{:>9.3} -> {:<9.3} {:>10} -> {:<10}  {}:{}", b.0, a.0, b.1 as u64, a.1 as u64, module.rsplit(['.', '/', '\\']).next().unwrap_or(module), line);
        }
    }
    if !changed.is_empty() {
        let _ = writeln!(text, "modules whose source changed (their lines are compared by function only): {}", changed.iter().map(|m| m.rsplit(['.', '/', '\\']).next().unwrap_or(m)).collect::<Vec<_>>().join(", "));
    }

    // caches
    let rates = |profile: &Json, path: &str| -> BTreeMap<String, (f64, f64)> {
        profile[path]
            .as_array()
            .or_else(|| profile[path]["by_kind"].as_array())
            .into_iter()
            .flatten()
            .map(|row| {
                let name = row["name"].as_str().or_else(|| row["kind"].as_str()).unwrap_or("?").to_string();
                let hits = num(&row["hits"]);
                let lookups = hits + num(&row["misses"]) + num(&row["slow"]);
                (name, (hits, lookups))
            })
            .collect()
    };
    let mut cache_rows = Vec::new();
    for (title, path) in [("Luau inline caches", "luau_caches"), ("ree caches", "ree_caches")] {
        let (b, a) = (rates(before, path), rates(after, path));
        let names: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
        let mut wrote = false;
        for name in names {
            let (bh, bl) = b.get(name).copied().unwrap_or_default();
            let (ah, al) = a.get(name).copied().unwrap_or_default();
            if bl == 0.0 && al == 0.0 {
                continue;
            }
            if !wrote {
                let _ = writeln!(text, "\n{title} (hit rate, lookups: before -> after)");
                wrote = true;
            }
            let rate = |hits: f64, lookups: f64| if lookups > 0.0 { hits / lookups * 100.0 } else { 0.0 };
            let _ = writeln!(text, "{:<22} {:>7.2} % -> {:<7.2} % {:>12} -> {:<12}", name, rate(bh, bl), rate(ah, al), bl as u64, al as u64);
            cache_rows.push(json!({"group": title, "name": name, "before": {"hits": bh, "lookups": bl}, "after": {"hits": ah, "lookups": al}}));
        }
    }

    // memory by type
    let by_type = |profile: &Json, path: &[&str]| -> BTreeMap<String, f64> {
        let mut value = profile;
        for key in path {
            value = &value[*key];
        }
        value.as_array().into_iter().flatten().map(|row| (row["type"].as_str().unwrap_or("?").to_string(), num(&row["bytes"]))).collect()
    };
    let mut type_rows = Vec::new();
    for (title, b, a) in [
        ("allocated by type", by_type(before, &["alloc", "by_type"]), by_type(after, &["alloc", "by_type"])),
        ("reachable at the end by type", last_snapshot(before).map(|s| by_type(s, &["by_type"])).unwrap_or_default(), last_snapshot(after).map(|s| by_type(s, &["by_type"])).unwrap_or_default()),
    ] {
        let names: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
        let mut moved: Vec<(&String, f64, f64)> = names.into_iter().map(|name| (name, b.get(name).copied().unwrap_or(0.0), a.get(name).copied().unwrap_or(0.0))).filter(|(_, x, y)| (x - y).abs() >= 1.0).collect();
        moved.sort_by(|x, y| (y.2 - y.1).abs().total_cmp(&(x.2 - x.1).abs()));
        if !moved.is_empty() {
            let _ = writeln!(text, "\n{title}");
            for (name, x, y) in moved.iter().take(top.min(12)) {
                let _ = writeln!(text, "{:>12} -> {:<12} {:>10}  {}", bytes(*x), bytes(*y), change(*x, *y), name);
                type_rows.push(json!({"group": title, "type": name, "before": x, "after": y}));
            }
        }
    }

    let json = json!({
        "version": 1,
        "figures": figures.iter().map(|f| json!({"name": f.name, "before": f.before, "after": f.after, "unit": f.unit})).collect::<Vec<_>>(),
        "scope": scope,
        "functions": rows.iter().take(500).map(|(label, harness, b, a)| json!({"function": label, "harness": harness,
            "before": {"self_work_ms": b.self_work, "total_work_ms": b.total_work, "calls": b.calls, "insns": b.insns, "alloc_bytes": b.alloc, "time_ms": b.time},
            "after": {"self_work_ms": a.self_work, "total_work_ms": a.total_work, "calls": a.calls, "insns": a.insns, "alloc_bytes": a.alloc, "time_ms": a.time}})).collect::<Vec<_>>(),
        "lines": line_rows.iter().take(500).map(|((module, line), b, a)| json!({"module": module, "line": line, "before": {"work_ms": b.0, "insns": b.1, "alloc_bytes": b.2}, "after": {"work_ms": a.0, "insns": a.1, "alloc_bytes": a.2}})).collect::<Vec<_>>(),
        "changed_modules": changed,
        "notes": notes,
        "caches": cache_rows,
        "types": type_rows,
        "summary": text,
    });
    (text, json)
}

/// The command: `ree profile diff <before> <after> [--out <dir>] [--top <n>]`; the exit code
pub fn cli(args: &[String]) -> i32 {
    let value = |key: &str| args.iter().position(|arg| arg == key).and_then(|index| args.get(index + 1)).cloned();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index].starts_with("--") {
            index += 2;
            continue;
        }
        positional.push(args[index].clone());
        index += 1;
    }
    let [before_dir, after_dir] = positional.as_slice() else {
        eprintln!("usage: ree profile diff <before dir> <after dir> [--out <dir>] [--top <n>]");
        return 2;
    };
    let top = value("--top").and_then(|top| top.parse().ok()).unwrap_or(20);
    let (before, after) = match (load(Path::new(before_dir)), load(Path::new(after_dir))) {
        (Ok(before), Ok(after)) => (before, after),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("ree profile diff: {error}");
            return 1;
        }
    };
    let out = value("--out").map(PathBuf::from).unwrap_or_else(|| {
        let after = Path::new(after_dir);
        if after.is_file() { after.parent().map(Path::to_path_buf).unwrap_or_default() } else { after.to_path_buf() }
    });
    let (text, written) = write(&before, &after, top, &out);
    print!("{text}");
    match written {
        Ok(()) => {
            println!("\nfiles in {}: diff.txt, diff.json, diff.html", out.display());
            0
        }
        Err(error) => {
            eprintln!("ree profile diff: {error}");
            1
        }
    }
}

/// Two profiles compared ([`compare`]) into `out`: diff.txt, diff.json and diff.html; the text, and whether the files were
/// written
pub fn write(before: &Json, after: &Json, top: usize, out: &Path) -> (String, Result<(), String>) {
    let (text, json) = compare(before, after, top);
    let written = (|| -> Result<(), String> {
        std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
        std::fs::write(out.join("diff.txt"), &text).map_err(|error| error.to_string())?;
        let data = serde_json::to_string(&json).map_err(|error| error.to_string())?;
        std::fs::write(out.join("diff.json"), &data).map_err(|error| error.to_string())?;
        std::fs::write(out.join("diff.html"), html(&data)).map_err(|error| error.to_string())
    })();
    (text, written)
}

/// A profile directory's profile.json (or the file itself)
pub fn load_profile(dir: &Path) -> Result<Json, String> {
    load(dir)
}

/// `ree profile report <dir>`: the summary of a profile directory printed again, and its report page rewritten from its
/// profile.json (a page made by an older build gets this build's page)
pub fn report_cli(args: &[String]) -> i32 {
    let Some(dir) = args.iter().find(|arg| !arg.starts_with("--")) else {
        eprintln!("usage: ree profile report <profile dir>");
        return 2;
    };
    let dir = Path::new(dir);
    let path = if dir.is_file() { dir.to_path_buf() } else { dir.join("profile.json") };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("ree profile report: {}: {error}", path.display());
            return 1;
        }
    };
    let json: Json = match serde_json::from_str(&text) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("ree profile report: {}: {error}", path.display());
            return 1;
        }
    };
    let page = path.with_file_name("report.html");
    if let Err(error) = std::fs::write(&page, super::report::html(&text)) {
        eprintln!("ree profile report: {}: {error}", page.display());
        return 1;
    }
    print!("{}", json["summary"].as_str().unwrap_or_default());
    0
}

/// The diff's page: its figures, functions, lines, caches and types as tables, a change bar on each row
fn html(data: &str) -> String {
    const TEMPLATE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>ree profile diff</title>
<style>
:root { color-scheme: light; --page: #f9f9f7; --surface: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781; --grid: #e1e0d9; --ring: rgba(11,11,11,0.10); --less: #2a78d6; --more: #e34948; --mid: #f0efec; }
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { color-scheme: dark; --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781; --grid: #2c2c2a; --ring: rgba(255,255,255,0.10); --less: #3987e5; --more: #e66767; --mid: #383835; } }
:root[data-theme="dark"] { color-scheme: dark; --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781; --grid: #2c2c2a; --ring: rgba(255,255,255,0.10); --less: #3987e5; --more: #e66767; --mid: #383835; }
* { box-sizing: border-box; } body { margin: 0; background: var(--page); color: var(--ink); font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1200px; margin: 0 auto; padding: 20px 16px 48px; } h1 { font-size: 20px; margin: 0 0 6px; } h2 { font-size: 16px; margin: 26px 0 8px; }
.lead { color: var(--ink-2); margin: 0 0 10px; } .card { background: var(--surface); border: 1px solid var(--ring); border-radius: 8px; padding: 10px; overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 13px; } th, td { padding: 4px 8px; text-align: right; white-space: nowrap; border-bottom: 1px solid var(--grid); font-variant-numeric: tabular-nums; }
th { color: var(--ink-2); font-weight: 600; } th.text, td.text { text-align: left; } td.text { max-width: 480px; overflow: hidden; text-overflow: ellipsis; }
.delta { display: inline-flex; align-items: center; width: 140px; height: 10px; position: relative; } .delta::before { content: ""; position: absolute; left: 70px; top: 0; bottom: 0; width: 1px; background: var(--muted); }
.delta span { position: absolute; height: 8px; top: 1px; } .legend { color: var(--ink-2); font-size: 12px; display: flex; gap: 14px; margin: 4px 0 8px; }
.legend span::before { content: ""; display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 5px; vertical-align: -1px; background: var(--key); }
pre { font: 12px/1.45 ui-monospace, SFMono-Regular, Consolas, monospace; white-space: pre; overflow-x: auto; margin: 0; }
</style></head><body><main><h1>ree profile diff</h1><p class="lead">Work, instructions and allocations are the same every run of the same code: a change there is the code's. Host time is noise unless the runs were interleaved.</p>
<div class="legend"><span style="--key: var(--less)">less after</span><span style="--key: var(--more)">more after</span></div><div id="body"></div>
<h2>Text</h2><div class="card"><pre id="text"></pre></div></main>
<script>
const DIFF = /*DIFF_DATA*/null;
(function () {
	const D = DIFF || {};
	const el = (tag, cls, ...children) => { const n = document.createElement(tag); if (cls) n.className = cls; for (const c of children) n.append(c instanceof Node ? c : document.createTextNode(c == null ? "" : String(c))); return n; };
	const bytes = (v) => { const s = v < 0 ? "-" : ""; v = Math.abs(v || 0); return s + (v >= 1048576 ? (v / 1048576).toFixed(2) + " MB" : v >= 1024 ? (v / 1024).toFixed(1) + " KB" : Math.round(v) + " B"); };
	const num = (v, d) => Number(v || 0).toLocaleString("en-US", { minimumFractionDigits: d || 0, maximumFractionDigits: d || 0 });
	const bar = (before, after, scale) => {
		const wrap = el("span", "delta"); const delta = after - before; const w = Math.min(68, Math.abs(delta) / Math.max(scale, 1e-12) * 68);
		const s = el("span"); s.style.width = w + "px"; s.style.left = (delta < 0 ? 70 - w : 70) + "px"; s.style.background = delta < 0 ? "var(--less)" : "var(--more)";
		s.style.borderRadius = delta < 0 ? "4px 0 0 4px" : "0 4px 4px 0"; wrap.append(s); wrap.title = (delta < 0 ? "less" : "more") + " after"; return wrap; };
	const change = (b, a) => Math.abs(b) < 1e-12 ? (Math.abs(a) < 1e-12 ? "same" : "new") : ((a - b) / b * 100 >= 0 ? "+" : "") + ((a - b) / b * 100).toFixed(1) + " %";
	const section = (title, headers, rows) => { const body = document.getElementById("body"); body.append(el("h2", "", title)); const t = el("table", ""); const h = el("tr", ""); headers.forEach((x, i) => h.append(el("th", i === 0 ? "text" : "", x))); t.append(h); rows.forEach((r) => { const tr = el("tr", ""); r.forEach((c, i) => tr.append(el("td", i === 0 ? "text" : "", c))); t.append(tr); }); body.append(el("div", "card", t)); };
	const show = (unit, v) => unit === "bytes" ? bytes(v) : unit === "ms" ? num(v, 3) + " ms" : num(v);
	const figures = D.figures || [];
	section("Headline", ["figure", "before", "after", "change", ""], figures.map((f) => [f.name, show(f.unit, f.before), show(f.unit, f.after), change(f.before, f.after), bar(f.before, f.after, Math.max(Math.abs(f.before), Math.abs(f.after)))]));
	const fs = D.functions || []; const fscale = Math.max(1e-9, ...fs.map((f) => Math.abs(f.after.self_work_ms - f.before.self_work_ms)));
	if (fs.length) section("Functions that moved most", ["function", "self work ms", "", "calls", "self instructions", "self allocated"], fs.slice(0, 150).map((f) => [f.function, `${num(f.before.self_work_ms, 3)} → ${num(f.after.self_work_ms, 3)}`, bar(f.before.self_work_ms, f.after.self_work_ms, fscale), `${num(f.before.calls)} → ${num(f.after.calls)}`, `${num(f.before.insns)} → ${num(f.after.insns)}`, `${bytes(f.before.alloc_bytes)} → ${bytes(f.after.alloc_bytes)}`]));
	const ls = D.lines || []; const lscale = Math.max(1e-9, ...ls.map((l) => Math.abs(l.after.work_ms - l.before.work_ms)));
	if (ls.length) section("Lines (modules whose source is unchanged)", ["line", "self work ms", "", "instructions", "allocated"], ls.slice(0, 150).map((l) => [`${l.module.split(/[.\/\\]/).pop()}:${l.line}`, `${num(l.before.work_ms, 3)} → ${num(l.after.work_ms, 3)}`, bar(l.before.work_ms, l.after.work_ms, lscale), `${num(l.before.insns)} → ${num(l.after.insns)}`, `${bytes(l.before.alloc_bytes)} → ${bytes(l.after.alloc_bytes)}`]));
	const cs = D.caches || [];
	if (cs.length) section("Caches", ["cache", "hit rate", "lookups"], cs.map((c) => { const r = (x) => x.lookups > 0 ? (x.hits / x.lookups * 100).toFixed(2) + " %" : "-"; return [`${c.group}: ${c.name}`, `${r(c.before)} → ${r(c.after)}`, `${num(c.before.lookups)} → ${num(c.after.lookups)}`]; }));
	const ts = D.types || []; const tscale = Math.max(1, ...ts.map((t) => Math.abs(t.after - t.before)));
	if (ts.length) section("Memory by type", ["type", "before", "after", "change", ""], ts.map((t) => [`${t.group}: ${t.type}`, bytes(t.before), bytes(t.after), change(t.before, t.after), bar(t.before, t.after, tscale)]));
	document.getElementById("text").textContent = D.summary || "";
})();
</script></body></html>"#;
    TEMPLATE.replacen("/*DIFF_DATA*/null", &data.replace("</", "<\\/"), 1)
}

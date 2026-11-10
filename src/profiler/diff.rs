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

/// One compared figure: (name, before, after, unit), and whether it is the same every run of the same code (else it was
/// measured, and one run each gives no basis for calling its change the code's)
struct Figure {
    name: String,
    before: f64,
    after: f64,
    unit: &'static str,
    exact: bool,
}

/// What tells a profile's harness apart, and whether by content: its contents hashed (run.harness_hash), else, for a
/// profile from before the hash, its module's source hash (the same hash of the same text), else its file's name
fn harness_identity(profile: &Json) -> Option<(bool, String)> {
    // (run.harness_hash hashes the text the harness's module loaded as source_hashes does: a profile from before it and
    // one after read the same harness as the same)
    if let Some(hash) = profile["run"]["harness_hash"].as_str() {
        return Some((true, hash.to_string()));
    }
    let path = profile["run"]["harness"].as_str()?;
    let stem = Path::new(path).file_stem().map(|stem| stem.to_string_lossy().into_owned())?;
    let module = format!("ReplicatedStorage.RURFixtures.{stem}");
    match profile["source_hashes"][&module].as_str() {
        Some(hash) => Some((true, hash.to_string())),
        None => Some((false, Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default())),
    }
}

impl Figure {
    fn show(&self, value: f64) -> String {
        match self.unit {
            "bytes" => bytes(value),
            "ms" => format!("{value:.3} ms"),
            "%" => format!("{value:.1} %"),
            "ratio" => format!("{value:.2}"),
            "share" => format!("{:.1} %", value * 100.0),
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
    /// The cold and first-use charges in the self work, when the profile splits them (functions[].self.cold_work_ms)
    cold: Option<f64>,
}

/// A function as a diff pairs it: its figures, and what tells it apart across an edit
#[derive(Default, Clone)]
struct FunctionEntry {
    label: String,
    harness: bool,
    figures: FunctionFigures,
    module: String,
    name: String,
    line: i64,
    /// Its compiled code's hash and its text's (report.rs code_hash, source_hash), when the profile has them
    code: Option<String>,
    source: Option<String>,
    /// Instructions run by opcode, over the whole run (profile.json lines[].ops), for "the same path"
    ops: BTreeMap<String, f64>,
}

impl FunctionEntry {
    fn anonymous(&self) -> bool {
        self.name.is_empty() || self.name == "anonymous"
    }
}

/// Pairs the anonymous functions of before and after, whose keys carry their line, across an edit that moved them: per
/// module, first those with the same code and text (in order), then the same code (moved and re-indented or commented),
/// then, between those anchors, the rest in order where both sides hold as many; an older profile without hashes pairs
/// by key, then the same way in order. Named functions keep their key (module and name). before key -> after key
fn pair_anonymous(before: &BTreeMap<String, FunctionEntry>, after: &BTreeMap<String, FunctionEntry>) -> BTreeMap<String, String> {
    let mut pairs = BTreeMap::new();
    let modules: BTreeSet<&String> = before.values().chain(after.values()).filter(|entry| entry.anonymous()).map(|entry| &entry.module).collect();
    for module in modules {
        let side = |map: &BTreeMap<String, FunctionEntry>| -> Vec<(String, FunctionEntry)> {
            let mut list: Vec<(String, FunctionEntry)> = map.iter().filter(|(_, entry)| entry.anonymous() && &entry.module == module).map(|(key, entry)| (key.clone(), entry.clone())).collect();
            list.sort_by_key(|(_, entry)| entry.line);
            list
        };
        let (b, a) = (side(before), side(after));
        let mut taken_b = vec![false; b.len()];
        let mut taken_a = vec![false; a.len()];
        let mut anchors: Vec<(usize, usize)> = Vec::new();
        let hashed = |entry: &FunctionEntry| entry.code.is_some();
        anchor_pass(&b, &a, |x, y| hashed(x) && x.code == y.code && x.source.is_some() && x.source == y.source, &mut taken_b, &mut taken_a, &mut anchors, false);
        anchor_pass(&b, &a, |x, y| hashed(x) && x.code == y.code, &mut taken_b, &mut taken_a, &mut anchors, false);
        // (a profile from before the hashes: the same key, as before)
        anchor_pass(&b, &a, |x, y| !hashed(x) && !hashed(y) && x.line == y.line, &mut taken_b, &mut taken_a, &mut anchors, true);
        anchors.sort();
        // between anchors (those in order on both sides), the rest in order where both sides hold as many (a handler edited
        // where it stands)
        let mut bounds: Vec<(isize, isize)> = vec![(-1, -1)];
        for &(i, j) in &anchors {
            if bounds.last().is_some_and(|last| (j as isize) > last.1) {
                bounds.push((i as isize, j as isize));
            }
        }
        bounds.push((b.len() as isize, a.len() as isize));
        for window in bounds.windows(2) {
            let (from, to) = (window[0], window[1]);
            let gap_b: Vec<usize> = ((from.0 + 1)..to.0).map(|i| i as usize).filter(|&i| !taken_b[i]).collect();
            let gap_a: Vec<usize> = ((from.1 + 1)..to.1).map(|j| j as usize).filter(|&j| !taken_a[j]).collect();
            if gap_b.len() == gap_a.len() {
                for (&i, &j) in gap_b.iter().zip(&gap_a) {
                    anchors.push((i, j));
                }
            }
        }
        for (i, j) in anchors {
            pairs.insert(b[i].0.clone(), a[j].0.clone());
        }
    }
    pairs
}

/// One pass of [`pair_anonymous`]: each unpaired before function to the first unpaired after one `same` accepts (the same
/// code may have moved past another function, so a match by hash may cross an earlier one; one by line may not)
fn anchor_pass(b: &[(String, FunctionEntry)], a: &[(String, FunctionEntry)], same: impl Fn(&FunctionEntry, &FunctionEntry) -> bool, taken_b: &mut [bool], taken_a: &mut [bool], anchors: &mut Vec<(usize, usize)>, ordered: bool) {
    for i in 0..b.len() {
        if taken_b[i] {
            continue;
        }
        let found = (0..a.len()).find(|&j| !taken_a[j] && same(&b[i].1, &a[j].1) && !(ordered && anchors.iter().any(|&(x, y)| (x < i) != (y < j))));
        if let Some(j) = found {
            taken_b[i] = true;
            taken_a[j] = true;
            anchors.push((i, j));
        }
    }
}

/// What a price-only change may leave unexplained: this share of the larger warm part (the self work less its cold and
/// first-use charges), and at least this much (ms). The work clock is deterministic, so a move of who pays the cold
/// charges leaves nothing unexplained but float rounding: two identical walkers swapping them leave 0.000 ms, while 1 % of
/// a 50 ms function would hide 0.5 ms of real growth under the tag
const PRICE_SHARE: f64 = 0.0001;
const PRICE_FLOOR_MS: f64 = 0.002;

/// The one rule for whether a figure moved, as the viewer's Diff decides it: work by more than 1 ns (two sums of the same
/// charges in another order part in their last bits: 0.039 ms against 0.039 ms and 6.9e-18), counts and bytes by a whole one
const SAME_MS: f64 = 1e-6;

fn moved_ms(before: f64, after: f64) -> bool {
    (after - before).abs() > SAME_MS
}

fn moved_count(before: f64, after: f64) -> bool {
    (after - before).abs() >= 0.5
}

/// Whether a Luau function's change in work is a change of price only, shown by the profiles: it ran the same path both
/// times (the same calls, instructions and count of each opcode) and its cold and first-use charges account for the
/// change in its work, to within [`PRICE_SHARE`] of its warm part and [`PRICE_FLOOR_MS`] (profile.json `cold_work_ms`, the work
/// clock's split): the rest of its work, the engine's charges for what it asked included, stayed. The same instructions
/// prove nothing on their own (a model of 200 parts cloned and destroyed runs the instructions a model of 10 does, and
/// its engine charges grow twentyfold), so without the split in both profiles no function is tagged; a C function, which
/// runs no instructions to tell its path by, never is. A function whose work did not move (a row kept for its allocations
/// alone) was not priced differently either
fn price_only(b: &FunctionEntry, a: &FunctionEntry) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0);
    let same_path = close(b.figures.calls, a.figures.calls) && close(b.figures.insns, a.figures.insns) && b.figures.insns > 0.0 && (b.ops.is_empty() || a.ops.is_empty() || b.ops == a.ops);
    let (Some(cold_before), Some(cold_after)) = (b.figures.cold, a.figures.cold) else { return false };
    if !moved_ms(b.figures.self_work, a.figures.self_work) {
        return false;
    }
    let unexplained = (a.figures.self_work - b.figures.self_work) - (cold_after - cold_before);
    let warm = (a.figures.self_work - cold_after).max(b.figures.self_work - cold_before).max(0.0);
    same_path && unexplained.abs() <= (PRICE_SHARE * warm).max(PRICE_FLOOR_MS)
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
fn functions(profile: &Json, scope: Option<&[String]>) -> BTreeMap<String, FunctionEntry> {
    // each function's instructions by opcode over the run, from its lines
    let mut ops: BTreeMap<i64, BTreeMap<String, f64>> = BTreeMap::new();
    for line in profile["lines"].as_array().into_iter().flatten() {
        let (Some(id), Some(counts)) = (line["function"].as_i64(), line["ops"].as_object()) else { continue };
        let entry = ops.entry(id).or_default();
        for (op, count) in counts {
            *entry.entry(op.clone()).or_insert(0.0) += num(count);
        }
    }
    let mut out: BTreeMap<String, FunctionEntry> = BTreeMap::new();
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
        let entry = out.entry(key).or_insert_with(|| FunctionEntry {
            label,
            harness,
            module: f["module"].as_str().unwrap_or_default().to_string(),
            name: if f["module"].is_string() { f["name"].as_str().unwrap_or_default().to_string() } else { f["name"].as_str().unwrap_or("?").to_string() },
            line: f["line"].as_i64().unwrap_or(0),
            code: f["code_hash"].as_str().map(str::to_string),
            source: f["source_hash"].as_str().map(str::to_string),
            ..FunctionEntry::default()
        });
        if let Some(counts) = f["id"].as_i64().and_then(|id| ops.get(&id)) {
            for (op, count) in counts {
                *entry.ops.entry(op.clone()).or_insert(0.0) += count;
            }
        }
        let figures = &mut entry.figures;
        // (the cold and first-use charges, when the profile splits them out of the work)
        let cold = |sum: &mut Option<f64>, value: &Json| {
            if let Some(ms) = value.as_f64() {
                *sum = Some(sum.unwrap_or(0.0) + ms);
            }
        };
        match scope {
            Some(phases) => {
                for phase in phases {
                    let at = &f["phases"][phase.as_str()];
                    if let Some(cold_ms) = scoped_cold_ms(at, harness) {
                        figures.cold = Some(figures.cold.unwrap_or(0.0) + cold_ms);
                    }
                    let harness_part = if harness { 0.0 } else { num(&at["harness_self_work_ms"]) };
                    figures.self_work += num(&at["self_work_ms"]) - harness_part;
                    figures.total_work += num(&at["total_work_ms"]);
                    figures.calls += num(&at["calls"]);
                    figures.insns += num(&at["insns"]);
                }
            }
            None => {
                cold(&mut figures.cold, &f["self"]["cold_work_ms"]);
                figures.self_work += num(&f["self"]["work_ms"]);
                figures.total_work += num(&f["total"]["work_ms"]);
                // (calls through copies -O2 inlined count too: a change that inlines a function or stops inlining it keeps its calls)
                figures.calls += num(&f["calls"]) + num(&f["inlined"]["calls"]);
                figures.insns += num(&f["self"]["insns"]);
            }
        }
        figures.alloc += num(&f["self"]["alloc_bytes"]);
        figures.time += num(&f["self"]["time_ms"]);
    }
    out
}

/// The cold and first-use charges a function holds in one phase, without the part paid for the harness, or `None` where the
/// profile does not split them. A harness function's charges are all the harness's, so they stay whole; for any other
/// function `harness_cold_work_ms` is the part paid for the harness, taken off on the same side as `harness_self_work_ms`.
fn scoped_cold_ms(phase_figures: &Json, function_is_harness: bool) -> Option<f64> {
    let cold_ms = phase_figures["cold_work_ms"].as_f64()?;
    let harness_cold_ms = if function_is_harness { 0.0 } else { num(&phase_figures["harness_cold_work_ms"]) };
    Some((cold_ms - harness_cold_ms).max(0.0))
}

/// A work figure in a table cell: `-` where the two profiles' versions measure it differently
fn work_cell(ms: f64, comparable: bool) -> String {
    if comparable { format!("{ms:.3}") } else { "-".to_string() }
}

/// A count in a table cell: `-` where the two profiles' versions count it differently
fn count_cell(count: f64, comparable: bool) -> String {
    if comparable { (count as u64).to_string() } else { "-".to_string() }
}

/// A function's row in a diff: before and after (a function only on one side has zeros on the other), whether it ran the
/// same path both times, and the line it moved from when an edit moved it
struct Row {
    label: String,
    harness: bool,
    before: FunctionFigures,
    after: FunctionFigures,
    status: &'static str,
    price_only: bool,
    moved: Option<(i64, i64)>,
}

/// The functions of before and after paired (named ones by module and name, anonymous ones across a move:
/// [`pair_anonymous`])
fn function_rows(fb: &BTreeMap<String, FunctionEntry>, fa: &BTreeMap<String, FunctionEntry>) -> Vec<Row> {
    let moved = pair_anonymous(fb, fa);
    let paired_after: BTreeSet<&String> = moved.values().collect();
    let mut rows = Vec::new();
    for (key, b) in fb {
        let after_key = if b.anonymous() { moved.get(key) } else { fa.contains_key(key).then_some(key) };
        match after_key.and_then(|key| fa.get(key)) {
            Some(a) => {
                let shifted = (b.anonymous() && a.line != b.line).then_some((b.line, a.line));
                let label = match shifted {
                    Some((from, to)) => format!("{} (moved from :{from} to :{to})", a.label),
                    None => a.label.clone(),
                };
                rows.push(Row { label, harness: a.harness, before: b.figures, after: a.figures, status: "changed", price_only: price_only(b, a), moved: shifted });
            }
            None => rows.push(Row { label: b.label.clone(), harness: b.harness, before: b.figures, after: FunctionFigures::default(), status: "gone", price_only: false, moved: None }),
        }
    }
    for (key, a) in fa {
        let taken = if a.anonymous() { paired_after.contains(key) } else { fb.contains_key(key) };
        if !taken {
            rows.push(Row { label: a.label.clone(), harness: a.harness, before: FunctionFigures::default(), after: a.figures, status: "new", price_only: false, moved: None });
        }
    }
    rows
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

/// What the scripts asked the engine to do in the scope's phases (the whole run without one), from profile.json's marks: each
/// mark's counts (cost::EngineOps) to the next mark's, the last to the run's end. None for a profile from before the counts.
fn engine_ops(profile: &Json, scope: Option<&[String]>) -> Option<BTreeMap<String, f64>> {
    let marks = profile["marks"].as_array()?;
    let mut sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut counted = false;
    for (index, mark) in marks.iter().enumerate() {
        let phase = mark["phase"].as_str().unwrap_or_default();
        if scope.is_some_and(|phases| !phases.iter().any(|reported| reported == phase)) {
            continue;
        }
        let next = marks.get(index + 1).unwrap_or(&profile["end"]);
        let (Some(from), Some(to)) = (mark["engine"].as_object(), next["engine"].as_object()) else { continue };
        counted = true;
        for (name, value) in to {
            *sum.entry(name.clone()).or_insert(0.0) += (num(value) - from.get(name).map_or(0.0, num)).max(0.0);
        }
    }
    counted.then_some(sum)
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

/// A hardware counters' counts object by the counters' names now (counters.rs `canonical`)
fn counter_counts(counts: &Json) -> Option<BTreeMap<String, f64>> {
    Some(counts.as_object()?.iter().map(|(name, value)| (super::hw::counters::canonical(name).to_string(), num(value))).collect())
}

/// The hardware counters' functions: key -> (label, self counts by the counters' names now)
fn counter_functions(profile: &Json) -> BTreeMap<String, (String, BTreeMap<String, f64>)> {
    profile["counters"]["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let key = row["key"].as_str()?.to_string();
            let name = row["name"].as_str().unwrap_or("?");
            let label = match (row["module"].as_str(), row["line"].as_i64()) {
                (Some(module), Some(line)) => format!("{} {}:{line}", if name.is_empty() { "anonymous" } else { name }, module.rsplit(['.', '/', '\\']).next().unwrap_or(module)),
                _ => name.to_string(),
            };
            Some((key, (label, counter_counts(&row["self"]).unwrap_or_default())))
        })
        .collect()
}

/// `1.23G`, `45.6M`, `789K`: a count in a few characters
fn compact(value: f64) -> String {
    let (sign, value) = if value < 0.0 { ("-", -value) } else { ("", value) };
    if value >= 1e9 {
        format!("{sign}{:.2}G", value / 1e9)
    } else if value >= 1e6 {
        format!("{sign}{:.1}M", value / 1e6)
    } else if value >= 1e4 {
        format!("{sign}{:.0}K", value / 1e3)
    } else {
        format!("{sign}{value:.0}")
    }
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

/// A timeline's work a frame (mean, worst), host time a frame (mean; None when a kind slowed the run and no cpu pass filled it in)
/// and the VM heap at its last frame
fn timeline_stats(profile: &Json) -> Option<(f64, f64, Option<f64>, f64)> {
    let timeline = &profile["timeline"];
    let columns: Vec<&str> = timeline["columns"].as_array()?.iter().filter_map(Json::as_str).collect();
    let rows = timeline["rows"].as_array()?;
    if rows.is_empty() {
        return None;
    }
    let column = |name: &str| columns.iter().position(|c| *c == name);
    let (frame_column, part_column) = (column("frame"), column("part"));
    // A frame a phase mark split has one row for each part, numbered from 1. A part after the first continues the
    // frame of the row before it.
    let continues_previous_frame = |at: usize| -> bool {
        let (Some(frame), Some(part)) = (frame_column, part_column) else { return false };
        let is_later_part = num(&rows[at][part]) > 1.0;
        at > 0 && is_later_part && num(&rows[at][frame]) == num(&rows[at - 1][frame])
    };
    // A column's value for each frame. Work and host time flow, so a frame's parts add up; a level (the heap) is the
    // last part's value.
    let per_frame = |name: &str, flows: bool| -> Option<Vec<f64>> {
        let index = column(name)?;
        let mut values: Vec<f64> = Vec::new();
        for (at, row) in rows.iter().enumerate() {
            let value = row.get(index)?.as_f64()?;
            match values.last_mut().filter(|_| continues_previous_frame(at)) {
                Some(last) if flows => *last += value,
                Some(last) => *last = value,
                None => values.push(value),
            }
        }
        Some(values)
    };
    let work = per_frame("work_ms", true).unwrap_or_default();
    let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
    // (no host time in a run a kind slowed: not the run's own)
    let host = per_frame("host_ms", true).map(|host| mean(&host));
    let max = work.iter().copied().fold(0.0, f64::max);
    let heap_at_end = per_frame("heap_bytes", false).and_then(|heap| heap.last().copied()).unwrap_or(0.0);
    Some((mean(&work), max, host, heap_at_end))
}

/// Compares two profiles; the text (printed by the command) and the JSON
pub fn compare(before: &Json, after: &Json, top: usize) -> (String, Json) {
    // (profiles of two versions: what changed meaning between them is left out, rather than read as a change)
    let versions = (before["version"].as_u64().unwrap_or(1), after["version"].as_u64().unwrap_or(1));
    let same_version = versions.0 == versions.1;
    let mut figures: Vec<Figure> = Vec::new();
    // (measured figures: host time and the CPU's counters move run to run, the rest is the same every run of the same code)
    let mut add = |name: &str, b: f64, a: f64, unit: &'static str| figures.push(Figure { name: name.into(), before: b, after: a, unit, exact: !(name.contains("(noisy)") || name.contains("hardware counter")) });
    // the headline: the game's code in the phases both report, apart from the harness's (whose work moves with what it
    // scans, not with the game)
    let scope = shared_scope(before, after);
    let scope_label = scope.as_ref().map(|phases| format!("{} {}", if phases.len() == 1 { "phase" } else { "phases" }, phases.join(", ")));
    if let (Some(phases), Some(label), true) = (&scope, &scope_label, same_version) {
        let (b, a) = (side_totals(before, phases), side_totals(after, phases));
        add(&format!("the game's code, {label}"), b.0, a.0, "ms");
        add(&format!("the harness's code, {label}"), b.1, a.1, "ms");
    }
    if before["work"].is_object() || after["work"].is_object() {
        add("work clock", num(&before["work"]["total_ms"]), num(&after["work"]["total_ms"]), "ms");
        add("work clock a frame", num(&before["work"]["per_frame_ms"]), num(&after["work"]["per_frame_ms"]), "ms");
        add("script instructions", num(&before["work"]["script_insns"]), num(&after["work"]["script_insns"]), "");
    }
    // the engine's side of the same phases: what the scripts asked it to do, counted, whatever the work clock prices it at
    if let (Some(b), Some(a)) = (engine_ops(before, scope.as_deref()), engine_ops(after, scope.as_deref())) {
        let label = scope_label.as_ref().map_or_else(|| "the whole run".to_string(), String::clone);
        let get = |ops: &BTreeMap<String, f64>, names: &[&str]| names.iter().map(|name| ops.get(*name).copied().unwrap_or(0.0)).sum::<f64>();
        for (name, counts) in [
            ("instances made", &["cloned", "created"][..]),
            ("instances destroyed", &["destroyed"][..]),
            ("property writes", &["writes"][..]),
            ("parts moved", &["geometry", "bulk_moved"][..]),
            ("pivots", &["pivots"][..]),
            ("emits", &["emits"][..]),
        ] {
            add(&format!("engine: {name}, {label}"), get(&b, counts), get(&a, counts), "");
        }
    }
    if before["alloc"].is_object() || after["alloc"].is_object() {
        add("allocated", num(&before["alloc"]["bytes"]), num(&after["alloc"]["bytes"]), "bytes");
        add("allocations", num(&before["alloc"]["objects"]), num(&after["alloc"]["objects"]), "");
    }
    let mut notes: Vec<String> = Vec::new();
    if !same_version {
        notes.push(format!(
            "the profiles are of two versions ({} and {}): left out is what changed meaning between them: a function's and a line's self work (a C function's work is its own row since version 2, not its caller's), a function's calls and instructions (version 1 counted a module's main chunk at none), the game's and the harness's code, host time (the wall clock's before version 2, the run thread's CPU time since), Luau's inline caches and builtin calls (outcomes as the interpreter ran them, builtins apart, since version 2), ree's own caches (what each counts)",
            versions.0, versions.1
        ));
    }
    if let (Some(label), true) = (&scope_label, same_version) {
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
    // (host time only when neither run stepped every instruction: work and caches make the interpreter about five times
    // slower, so a stepping profile's host time is the profiler's, not the code's)
    let stepping = |profile: &Json| profile["kinds"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind == "work" || kind == "caches"));
    let host_time = same_version && !stepping(before) && !stepping(after);
    if host_time && (before["time"].is_object() || after["time"].is_object()) {
        add("host time sampled (noisy)", num(&before["time"]["sampled_ms"]), num(&after["time"]["sampled_ms"]), "ms");
    }
    // the hardware counters both profiles read (the run thread's; the CPU's own, so they move a little run to run), by
    // their names now (a profile from before the fills were named for what they count matches a newer one), then what
    // memory cost the run thread in each: the share of its cycles stalled on loads, those cycles a thousand
    // instructions, a miss's latency, and IPC
    let (counts_before, counts_after) = (counter_counts(&before["counters"]["run"]["counts"]), counter_counts(&after["counters"]["run"]["counts"]));
    if let (Some(b), Some(a)) = (&counts_before, &counts_after) {
        for (name, value) in b {
            if let Some(after_value) = a.get(name) {
                add(&format!("{name} (hardware counter)"), *value, *after_value, "");
            }
        }
        let figures = |counts: &BTreeMap<String, f64>| {
            let names: Vec<String> = counts.keys().cloned().collect();
            let values: Vec<f64> = counts.values().copied().collect();
            super::hw::counters::figures(&names, &values)
        };
        let (fb, fa) = (figures(b), figures(a));
        for (key, unit) in [(super::hw::counters::LOAD_STALL_SHARE, "share"), (super::hw::counters::STALL_PER_THOUSAND, "ratio"), (super::hw::counters::MISS_LATENCY, "cycles"), ("IPC", "ratio")] {
            let get = |figures: &[super::hw::counters::Figure]| figures.iter().find(|figure| figure.key == key).map(|figure| figure.value);
            if let (Some(x), Some(y)) = (get(&fb), get(&fa)) {
                add(&format!("{key} (hardware counters)"), x, y, unit);
            }
        }
    }
    // (the functions' counter figures compare only between profiles whose counters reached functions the same way)
    if before["counters"]["run"].is_object() && after["counters"]["run"].is_object() {
        let (b, a) = (super::report::attribution_of(&before["counters"]), super::report::attribution_of(&after["counters"]));
        if b != a {
            notes.push(format!("the hardware counters reached functions in other ways (before: {b}; after: {a}): the run's counts compare, the functions' counter figures only roughly"));
        }
    }
    if let (Some(b), Some(a)) = (timeline_stats(before), timeline_stats(after)) {
        add("work a frame, mean", b.0, a.0, "ms");
        add("work a frame, worst", b.1, a.1, "ms");
        if let (Some(before_host), Some(after_host), true) = (b.2, a.2, host_time) {
            add("host a frame, mean (noisy)", before_host, after_host, "ms");
        }
        add("VM heap at the last frame, with garbage", b.3, a.3, "bytes");
    }
    add("frames", num(&before["run"]["frames"]), num(&after["run"]["frames"]), "");

    let mut text = String::new();
    let _ = writeln!(text, "ree profile diff: before {} ({} frames), after {} ({} frames)", before["run"]["status"].as_str().unwrap_or("?"), before["run"]["frames"], after["run"]["status"].as_str().unwrap_or("?"), after["run"]["frames"]);
    if before["run"]["frames"] != after["run"]["frames"] {
        let _ = writeln!(text, "note: the runs ran {} and {} frames; compare per-frame figures", before["run"]["frames"], after["run"]["frames"]);
    }
    // (by the harness's contents: two copies of one harness in other folders are the same harness)
    // (by content where both profiles hashed it, else by the files' names)
    let name = |profile: &Json| profile["run"]["harness"].as_str().map(|path| Path::new(path).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).unwrap_or_default();
    let differ = match (harness_identity(before), harness_identity(after)) {
        (Some((true, b)), Some((true, a))) => (b != a).then_some("their contents differ"),
        (Some(_), Some(_)) => (name(before) != name(after)).then_some("by their file names: a profile from before ree hashed the harness"),
        _ => None,
    };
    if let Some(how) = differ {
        let _ = writeln!(text, "note: the runs ran different harnesses ({} and {}: {how}); the harness's own code is set apart below", name(before), name(after));
    }
    for note in &notes {
        let _ = writeln!(text, "note: {note}");
    }
    let width = figures.iter().map(|figure| figure.name.len()).max().unwrap_or(0).max(30);
    let _ = writeln!(text, "{:<width$} {:>14} {:>14} {:>10}", "", "before", "after", "change");
    for figure in &figures {
        // (a measured figure's change, `~`, is no verdict: one run each says nothing of how far runs of the same code part)
        let change = if figure.exact { change(figure.before, figure.after) } else { format!("~{}", change(figure.before, figure.after)) };
        let _ = writeln!(text, "{:<width$} {:>14} {:>14} {:>10}", figure.name, figure.show(figure.before), figure.show(figure.after), change);
    }
    if figures.iter().any(|figure| !figure.exact) {
        let _ = writeln!(text, "~ measured: host time and the CPU's counters move from run to run (two runs of the same code read tens of percent apart in host time), so one run each gives their change no verdict; the rest is the same every run of the same code");
    }

    // functions: the game's first, the harness's apart; anonymous ones paired across a move (their key carries their line);
    // those that ran the same path both times (the same calls, instructions and opcodes) and moved only in price apart
    let (mut fb, mut fa) = (functions(before, scope.as_deref()), functions(after, scope.as_deref()));
    // Across versions only the allocations compare: version 1 counted a C function's work in its caller's self work and gave
    // a module's main chunk no calls and no instructions. A function keeps its identity, so it still pairs.
    if !same_version {
        for entry in fb.values_mut().chain(fa.values_mut()) {
            let allocated = entry.figures.alloc;
            entry.figures = FunctionFigures { alloc: allocated, ..FunctionFigures::default() };
        }
    }
    let mut rows = function_rows(&fb, &fa);
    // (by the exact figures: host time moves run to run and the tables do not show it, so a function whose host time alone
    // moved has not moved)
    let weight = |row: &Row| -> f64 { (row.after.self_work - row.before.self_work).abs() * 1000.0 + (row.after.alloc - row.before.alloc).abs() / 1024.0 + (row.after.insns - row.before.insns).abs() / 1000.0 + (row.after.calls - row.before.calls).abs() / 1000.0 };
    rows.retain(|row| moved_ms(row.before.self_work, row.after.self_work) || moved_count(row.before.alloc, row.after.alloc) || moved_count(row.before.insns, row.after.insns) || moved_count(row.before.calls, row.after.calls));
    rows.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then_with(|| a.label.cmp(&b.label)));
    let scope_part = scope_label.as_ref().map_or(String::new(), |label| format!(", {label}"));
    // (the game's self work that moved, by path changes and by price alone)
    let game: Vec<&Row> = rows.iter().filter(|row| !row.harness).collect();
    let moved_work = |rows: &[&Row]| rows.iter().map(|row| row.after.self_work - row.before.self_work).sum::<f64>();
    let (path_rows, priced_rows): (Vec<&Row>, Vec<&Row>) = game.iter().copied().partition(|row| !row.price_only);
    if !priced_rows.is_empty() {
        let _ = writeln!(
            text,
            "\nthe game's self work{scope_part}: {:+.3} ms in {} functions whose path or charges changed; {:+.3} ms in {} that ran the same path and were priced differently, their cold and first-use charges accounting for the change (cold first touches and first runs in a frame go to whichever code touches first)",
            moved_work(&path_rows),
            path_rows.len(),
            moved_work(&priced_rows),
            priced_rows.len()
        );
    }
    let line = |row: &Row| -> String {
        let (b, a) = (&row.before, &row.after);
        let tag = match row.status {
            "gone" => "  [gone]",
            "new" => "  [new]",
            _ => "",
        };
        let work_before = work_cell(b.self_work, same_version);
        let work_after = work_cell(a.self_work, same_version);
        let calls_before = count_cell(b.calls, same_version);
        let calls_after = count_cell(a.calls, same_version);
        let insns_before = count_cell(b.insns, same_version);
        let insns_after = count_cell(a.insns, same_version);
        let (alloc_before, alloc_after) = (bytes(b.alloc), bytes(a.alloc));
        format!(
            "{work_before:>9} -> {work_after:<9} {calls_before:>9} -> {calls_after:<9} {insns_before:>10} -> {insns_after:<10} {alloc_before:>9} -> {alloc_after:<9}  {}{tag}",
            row.label
        )
    };
    for (harness, title, count) in [
        (false, format!("functions that moved most: the game's code{scope_part} (self work ms, calls, self instructions, self allocated: before -> after)"), top),
        (true, format!("the harness's functions that moved most{scope_part} (its own code, left out above)"), top.min(8)),
    ] {
        let side: Vec<&Row> = rows.iter().filter(|row| row.harness == harness && (harness || !row.price_only)).collect();
        if side.is_empty() {
            continue;
        }
        let _ = writeln!(text, "\n{title}");
        for row in side.iter().take(count) {
            let _ = writeln!(text, "{}", line(row));
        }
    }
    if !priced_rows.is_empty() {
        let _ = writeln!(
            text,
            "\npriced differently, same path{scope_part}: the same calls, instructions and opcodes, and the cold and first-use charges account for the change in work to within {} % of the rest and {} us: which code touched first in a frame moved (self work ms, the cold and first-use part's change, calls, self instructions)",
            PRICE_SHARE * 100.0,
            PRICE_FLOOR_MS * 1000.0
        );
        for row in priced_rows.iter().take(top.min(12)) {
            let (b, a) = (&row.before, &row.after);
            let cold = a.cold.unwrap_or(0.0) - b.cold.unwrap_or(0.0);
            let _ = writeln!(text, "{:>9.3} -> {:<9.3} ({cold:+.3} cold) {:>9} calls {:>10} instructions  {}", b.self_work, a.self_work, a.calls as u64, a.insns as u64, row.label);
        }
    }
    // (without the work clock's cold split nothing tells a change of price from a change in what a function did: a function
    // with the same calls and instructions stays among the changes, and the reader is told why)
    let untold = game.iter().filter(|row| !row.price_only && row.status == "changed" && (row.before.cold.is_none() || row.after.cold.is_none()) && row.before.calls == row.after.calls && row.before.insns == row.after.insns && row.before.insns > 0.0).count();
    if untold > 0 {
        let _ = writeln!(text, "note: {untold} of the game's functions above ran the same calls and instructions both times, and these profiles carry no cold and first-use split (profile.json cold_work_ms) to show their work moved only in price, so they stay with the changes: an engine call's charges grow with what it is given");
    }

    // the functions whose cycles stalled on loads moved most (the hardware counters' self counts, by function key)
    let mut counter_rows: Vec<Json> = Vec::new();
    let (cfb, cfa) = (counter_functions(before), counter_functions(after));
    let stall_key = ["load-stall-cycles", "cycles"].into_iter().find(|name| cfb.values().chain(cfa.values()).any(|(_, counts)| counts.contains_key(*name)));
    if let Some(lead) = stall_key {
        let keys: BTreeSet<&String> = cfb.keys().chain(cfa.keys()).collect();
        let empty = (String::new(), BTreeMap::new());
        let mut moved: Vec<(&String, f64, f64, f64, f64)> = keys
            .into_iter()
            .map(|key| {
                let (b, a) = (cfb.get(key).unwrap_or(&empty), cfa.get(key).unwrap_or(&empty));
                let get = |counts: &BTreeMap<String, f64>, name: &str| counts.get(name).copied().unwrap_or(0.0);
                (key, get(&b.1, lead), get(&a.1, lead), get(&b.1, "cycles"), get(&a.1, "cycles"))
            })
            .filter(|(_, b, a, _, _)| (a - b).abs() > 0.0)
            .collect();
        moved.sort_by(|x, y| (y.2 - y.1).abs().total_cmp(&(x.2 - x.1).abs()).then_with(|| x.0.cmp(y.0)));
        if !moved.is_empty() {
            let stalls = lead == "load-stall-cycles";
            let _ = writeln!(text, "
functions whose {} moved most (hardware counters, self: before -> after{}; they vary run to run)", if stalls { "cycles stalled on loads" } else { "cycles" }, if stalls { ", and the share of their own cycles stalled" } else { "" });
            let share = |stalled: f64, cycles: f64| if cycles > 0.0 { format!("{:.1} %", stalled / cycles * 100.0) } else { "-".into() };
            for (key, b, a, cb, ca) in moved.iter().take(top) {
                let label = cfa.get(*key).or_else(|| cfb.get(*key)).map_or(key.as_str(), |(label, _)| label.as_str());
                if stalls {
                    let _ = writeln!(text, "{:>12} -> {:<12} {:>9} -> {:<9} {:>10}  {}", compact(*b), compact(*a), share(*b, *cb), share(*a, *ca), change(*b, *a), label);
                } else {
                    let _ = writeln!(text, "{:>12} -> {:<12} {:>10}  {}", compact(*b), compact(*a), change(*b, *a), label);
                }
                counter_rows.push(json!({"function": label, "key": key, "counter": lead, "before": b, "after": a, "before_cycles": cb, "after_cycles": ca}));
            }
        }
    }

    // lines of unchanged modules
    let hashes = |profile: &Json| -> BTreeMap<String, String> { profile["source_hashes"].as_object().map(|map| map.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default() };
    let (hb, ha) = (hashes(before), hashes(after));
    let same: BTreeSet<String> = hb.iter().filter(|(module, hash)| ha.get(*module) == Some(*hash)).map(|(module, _)| module.clone()).collect();
    let changed: Vec<&String> = hb.keys().filter(|module| ha.get(*module).is_some_and(|hash| Some(hash) != hb.get(*module))).collect();
    // (a line's self work holds its C calls' before version 2 and not since: lines are compared within a version only)
    let (lb, la) = if same_version { (lines(before, &same), lines(after, &same)) } else { (BTreeMap::new(), BTreeMap::new()) };
    let line_keys: BTreeSet<&(String, i64)> = lb.keys().chain(la.keys()).collect();
    let mut line_rows: Vec<(&(String, i64), (f64, f64, f64), (f64, f64, f64))> =
        line_keys.into_iter().map(|key| (key, lb.get(key).copied().unwrap_or_default(), la.get(key).copied().unwrap_or_default())).filter(|(_, b, a)| moved_ms(b.0, a.0) || moved_count(b.1, a.1) || moved_count(b.2, a.2)).collect();
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
    // (ree's own caches are profile.json's emulator.caches; a profile before that has them as ree_caches)
    let rates = |profile: &Json, path: &str| -> BTreeMap<String, (f64, f64)> {
        let rows = if path == "ree_caches" && profile["emulator"]["caches"].is_array() { &profile["emulator"]["caches"] } else { &profile[path] };
        rows.as_array()
            .or_else(|| rows["by_kind"].as_array())
            .into_iter()
            .flatten()
            .map(|row| {
                let name = row["name"].as_str().or_else(|| row["kind"].as_str()).unwrap_or("?").to_string();
                // (a Luau cache's outcomes: every lookup but a value another type serves; a profile before them had hits, misses, slow)
                if let Some(outcomes) = row["outcomes"].as_object() {
                    let hits = outcomes.get("hit").map_or(0.0, num);
                    let lookups = outcomes.iter().filter(|(outcome, _)| *outcome != "not_table").map(|(_, count)| num(count)).sum::<f64>();
                    return (name, (hits, lookups));
                }
                let hits = num(&row["hits"]);
                let lookups = hits + num(&row["misses"]) + num(&row["slow"]);
                (name, (hits, lookups))
            })
            .collect()
    };
    let mut cache_rows = Vec::new();
    let cache_groups: &[(&str, &str)] = if same_version { &[("Luau inline caches", "luau_caches"), ("ree's own caches (the emulator's)", "ree_caches")] } else { &[] };
    for &(title, path) in cache_groups {
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

    // a function's figures on one side: across versions only the allocations compare, the rest is null
    let side_json = |f: &FunctionFigures| -> Json {
        if !same_version {
            return json!({"self_work_ms": null, "total_work_ms": null, "calls": null, "insns": null, "alloc_bytes": f.alloc, "time_ms": null, "cold_work_ms": null});
        }
        json!({"self_work_ms": f.self_work, "total_work_ms": f.total_work, "calls": f.calls, "insns": f.insns, "alloc_bytes": f.alloc, "time_ms": f.time, "cold_work_ms": f.cold})
    };
    let json = json!({
        "version": 1,
        "figures": figures.iter().map(|f| json!({"name": f.name, "before": f.before, "after": f.after, "unit": f.unit, "exact": f.exact})).collect::<Vec<_>>(),
        "scope": scope,
        "functions": rows.iter().take(500).map(|row| { let (b, a) = (&row.before, &row.after); json!({"function": row.label, "harness": row.harness, "status": row.status, "price_only": row.price_only,
            "moved": row.moved.map(|(from, to)| json!({"from": from, "to": to})),
            "before": side_json(b), "after": side_json(a)}) }).collect::<Vec<_>>(),
        "lines": line_rows.iter().take(500).map(|((module, line), b, a)| json!({"module": module, "line": line, "before": {"work_ms": b.0, "insns": b.1, "alloc_bytes": b.2}, "after": {"work_ms": a.0, "insns": a.1, "alloc_bytes": a.2}})).collect::<Vec<_>>(),
        "counter_functions": counter_rows,
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
    // a standalone binary resolves its state before it reads or writes anything, so a missing cache stops the command
    if let Err(error) = crate::require_standalone_state(&crate::default_repo()) {
        eprintln!("ree profile diff: {error}");
        return 2;
    }
    let top = value("--top").and_then(|top| top.parse().ok()).unwrap_or(20);
    let (before, after) = match (load(Path::new(before_dir)), load(Path::new(after_dir))) {
        (Ok(before), Ok(after)) => (before, after),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("ree profile diff: {error}");
            return 1;
        }
    };
    let out = value("--out").map(PathBuf::from).unwrap_or_else(|| default_diff_out(after_dir));
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

/// Where `profile diff` writes when `--out` names no folder: under state for a standalone binary, which must not write
/// into whatever folder it was started in, else beside the after profile as hosts always had it
fn default_diff_out(after_dir: &str) -> PathBuf {
    if crate::is_standalone_binary() {
        return crate::default_output_path("ree-profile");
    }
    let after = Path::new(after_dir);
    if after.is_file() { after.parent().map(Path::to_path_buf).unwrap_or_default() } else { after.to_path_buf() }
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
    // as in `profile diff`: the page needs state, even though the profile it reads may be anywhere
    if let Err(error) = crate::require_standalone_state(&crate::default_repo()) {
        eprintln!("ree profile report: {error}");
        return 2;
    }
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
    let page = match report_page(&path) {
        Ok(page) => page,
        Err(error) => {
            eprintln!("ree profile report: {error}");
            return 1;
        }
    };
    if let Err(error) = std::fs::write(&page, super::report::html(&text)) {
        eprintln!("ree profile report: {}: {error}", page.display());
        return 1;
    }
    print!("{}", json["summary"].as_str().unwrap_or_default());
    // a standalone report is not beside its profile, so say where it went
    if crate::is_standalone_binary() {
        println!("\nreport in {}", page.display());
    }
    0
}

/// Where `profile report` writes its page for the profile at `profile`: `report.html` in `<state>/ree-profile` for a
/// standalone binary (the folder is created; an input never names an output folder), else beside the profile
fn report_page(profile: &Path) -> Result<PathBuf, String> {
    if !crate::is_standalone_binary() {
        return Ok(profile.with_file_name("report.html"));
    }
    let out = crate::default_output_path("ree-profile");
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    Ok(out.join("report.html"))
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
	const num = (v, d) => v == null ? "-" : Number(v).toLocaleString("en-US", { minimumFractionDigits: d || 0, maximumFractionDigits: d || 0 });
	const bar = (before, after, scale, quiet) => {
		if (before == null || after == null) return "";
		const wrap = el("span", "delta"); const delta = after - before; const w = Math.min(68, Math.abs(delta) / Math.max(scale, 1e-12) * 68);
		const s = el("span"); s.style.width = w + "px"; s.style.left = (delta < 0 ? 70 - w : 70) + "px"; s.style.background = quiet ? "var(--muted)" : delta < 0 ? "var(--less)" : "var(--more)";
		s.style.borderRadius = delta < 0 ? "4px 0 0 4px" : "0 4px 4px 0"; wrap.append(s); wrap.title = quiet ? "measured: no verdict from one run each" : (delta < 0 ? "less" : "more") + " after"; return wrap; };
	const change = (b, a) => Math.abs(b) < 1e-12 ? (Math.abs(a) < 1e-12 ? "same" : "new") : ((a - b) / b * 100 >= 0 ? "+" : "") + ((a - b) / b * 100).toFixed(1) + " %";
	const section = (title, headers, rows) => { const body = document.getElementById("body"); body.append(el("h2", "", title)); const t = el("table", ""); const h = el("tr", ""); headers.forEach((x, i) => h.append(el("th", i === 0 ? "text" : "", x))); t.append(h); rows.forEach((r) => { const tr = el("tr", ""); r.forEach((c, i) => tr.append(el("td", i === 0 ? "text" : "", c))); t.append(tr); }); body.append(el("div", "card", t)); };
	const show = (unit, v) => unit === "bytes" ? bytes(v) : unit === "ms" ? num(v, 3) + " ms" : num(v);
	const figures = D.figures || [];
	section("Headline", ["figure", "before", "after", "change", ""], figures.map((f) => [f.name, show(f.unit, f.before), show(f.unit, f.after), (f.exact === false ? "~" : "") + change(f.before, f.after), bar(f.before, f.after, Math.max(Math.abs(f.before), Math.abs(f.after)), f.exact === false)]));
	const fs = D.functions || []; const fscale = Math.max(1e-9, ...fs.map((f) => Math.abs(f.after.self_work_ms - f.before.self_work_ms)));
	const path = (f) => f.price_only ? "priced differently, same path" : f.status === "gone" ? "gone" : f.status === "new" ? "new" : f.moved ? `moved ${f.moved.from} → ${f.moved.to}` : "";
	if (fs.length) section("Functions that moved most", ["function", "self work ms", "", "calls", "self instructions", "self allocated", "path"], fs.slice(0, 150).map((f) => [f.function, `${num(f.before.self_work_ms, 3)} → ${num(f.after.self_work_ms, 3)}`, bar(f.before.self_work_ms, f.after.self_work_ms, fscale, f.price_only), `${num(f.before.calls)} → ${num(f.after.calls)}`, `${num(f.before.insns)} → ${num(f.after.insns)}`, `${bytes(f.before.alloc_bytes)} → ${bytes(f.after.alloc_bytes)}`, path(f)]));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile of four functions in module Game: an anonymous handler at `handler`, `first` and `second` (whose work
    /// moves between them as who pays the cold charges), and an anonymous handler ten lines below the first whose code an
    /// edit may change (`edited` instructions)
    fn profile(handler: i64, first: f64, second: f64, edited: f64, harness: (&str, &str), time_ms: f64) -> Json {
        let function = |id: i64, name: &str, line: i64, code: &str, work: f64, insns: f64| {
            let key = if name.is_empty() { format!("Game:anonymous@{line}") } else { format!("Game:{name}") };
            json!({"id": id, "key": key, "name": name, "module": "Game", "line": line, "kind": "Luau", "code_hash": code, "source_hash": format!("s{code}"),
                "calls": 10, "self": {"insns": insns, "work_ms": work, "alloc_bytes": 0.0, "time_ms": 0.0}, "total": {"work_ms": work}})
        };
        json!({
            "run": {"status": "ok", "frames": 10, "harness": harness.0, "harness_hash": harness.1},
            "kinds": ["work", "time"], "time": {"sampled_ms": time_ms},
            "functions": [
                function(0, "", handler, "c0ffee", 0.5, 400.0),
                function(1, "first", 30, "beef01", first, 200.0),
                function(2, "second", 40, "beef02", second, 200.0),
                function(3, "", handler + 10, if edited > 100.0 { "f00d02" } else { "f00d01" }, edited / 1000.0, edited),
            ],
        })
    }

    /// An anonymous function an edit moved pairs as one function (its key carries its line, its code hash does not): the
    /// unchanged handler is no row at all where it was "gone" and "new" before, outranking the real change; the edited
    /// one below it pairs in order and shows its move
    #[test]
    fn an_anonymous_function_an_edit_moved_pairs_as_one() {
        let before = profile(12, 0.9, 0.1, 100.0, ("a/run.luau", "h1"), 10.0);
        let after = profile(15, 0.9, 0.1, 1000.0, ("b/run.luau", "h1"), 10.0);
        let (text, json) = compare(&before, &after, 20);
        let rows = json["functions"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{text}");
        assert_eq!(rows[0]["status"], "changed");
        assert_eq!(rows[0]["moved"], json!({"from": 22, "to": 25}));
        assert!(text.contains("anonymous  Game:25 (moved from :22 to :25)") && !text.contains("[gone]") && !text.contains("[new]"), "{text}");
        // two copies of one harness in other folders are the same harness
        assert!(!text.contains("other harnesses"), "{text}");
        let other = profile(15, 0.9, 0.1, 1000.0, ("b/run.luau", "h2"), 10.0);
        assert!(compare(&before, &other, 20).0.contains("note: the runs ran different harnesses (run.luau and run.luau: their contents differ)"));
        // without hashes (an older profile) a function keyed by its line that moved is gone and new, as before
        let strip = |mut profile: Json| {
            for function in profile["functions"].as_array_mut().unwrap() {
                function.as_object_mut().unwrap().remove("code_hash");
            }
            profile
        };
        let (_, json) = compare(&strip(before.clone()), &strip(profile(15, 0.9, 0.1, 100.0, ("a/run.luau", "h1"), 10.0)), 20);
        assert!(json["functions"].as_array().unwrap().is_empty(), "the same functions in the same order, paired in order between anchors: {json}");
    }

    /// A profile from before run.harness_hash names its harness by its module's source hash, the same hash of the same
    /// text: it and a newer profile of the same harness ran the same harness, either way round; where one profile has no
    /// hash at all, the files' names decide
    #[test]
    fn a_harness_reads_the_same_across_ree_versions() {
        let new = profile(12, 0.9, 0.1, 100.0, ("a/run.luau", "h1"), 10.0);
        let mut old = new.clone();
        old["run"].as_object_mut().unwrap().remove("harness_hash");
        old["source_hashes"] = json!({"ReplicatedStorage.RURFixtures.run": "h1"});
        assert!(!compare(&old, &new, 20).0.contains("different harnesses"));
        assert!(!compare(&new, &old, 20).0.contains("different harnesses"));
        old["source_hashes"] = json!({"ReplicatedStorage.RURFixtures.run": "h2"});
        assert!(compare(&old, &new, 20).0.contains("note: the runs ran different harnesses (run.luau and run.luau: their contents differ)"));
        // (no hash in the old one: by the files' names, never a hash against a name)
        old["source_hashes"] = json!({});
        assert!(!compare(&old, &new, 20).0.contains("different harnesses"));
        let mut renamed = new.clone();
        renamed["run"]["harness"] = json!("b/other.luau");
        assert!(compare(&old, &renamed, 20).0.contains("different harnesses (run.luau and other.luau: by their file names"));
    }

    /// Sets the named functions' cold and first-use charges (functions[].self.cold_work_ms, the work clock's split)
    fn with_cold(mut profile: Json, cold: &[(&str, f64)]) -> Json {
        for function in profile["functions"].as_array_mut().unwrap() {
            if let Some((_, ms)) = cold.iter().find(|(name, _)| function["name"] == *name) {
                function["self"]["cold_work_ms"] = json!(ms);
            }
        }
        profile
    }

    /// A function that ran the same path (calls, instructions) and whose cold and first-use charges account for the
    /// change in its work was priced differently: listed apart, after the functions whose path or charges changed, with
    /// the totals of each side. Without the cold split nothing shows that, and both stay with the changes, with a note
    #[test]
    fn a_function_priced_differently_on_the_same_path_is_listed_apart() {
        // (the first touches in a frame moved from `first` to `second`: 0.8 ms of cold charges each way)
        let before = with_cold(profile(12, 0.9, 0.1, 100.0, ("run.luau", "h1"), 10.0), &[("first", 0.85), ("second", 0.05)]);
        let after = with_cold(profile(12, 0.1, 0.9, 1000.0, ("run.luau", "h1"), 14.0), &[("first", 0.05), ("second", 0.85)]);
        let (text, json) = compare(&before, &after, 20);
        let priced: Vec<&Json> = json["functions"].as_array().unwrap().iter().filter(|row| row["price_only"] == true).collect();
        assert_eq!(priced.len(), 2, "{text}");
        assert!(text.contains("+0.000 ms in 2 that ran the same path and were priced differently"), "{text}");
        let path_table = text.split("priced differently, same path").next().unwrap();
        assert!(!path_table.contains("first  Game:30") && text.contains("first  Game:30"), "listed apart, not among the path changes: {text}");
        assert!(text.contains("(-0.800 cold)") && text.contains("(+0.800 cold)"), "{text}");
        // (the same profiles without the split: the same calls and instructions prove no price change)
        let (text, json) = compare(&profile(12, 0.9, 0.1, 100.0, ("run.luau", "h1"), 10.0), &profile(12, 0.1, 0.9, 1000.0, ("run.luau", "h1"), 14.0), 20);
        assert!(json["functions"].as_array().unwrap().iter().all(|row| row["price_only"] == false), "{text}");
        assert!(!text.contains("priced differently, same path") && text.contains("first  Game:30"), "{text}");
        assert!(text.contains("note: 2 of the game's functions above ran the same calls and instructions both times"), "{text}");
    }

    /// Host time is shown only between profiles that did not step every instruction (work and caches make the
    /// interpreter about five times slower: a stepping profile's host time is the profiler's), and then as measured:
    /// its change carries `~`
    #[test]
    fn host_time_is_left_out_of_stepping_profiles_and_measured_otherwise() {
        let (before, after) = (profile(12, 0.9, 0.1, 100.0, ("run.luau", "h1"), 10.0), profile(12, 0.9, 0.1, 100.0, ("run.luau", "h1"), 14.0));
        let (text, _) = compare(&before, &after, 20);
        assert!(!text.contains("host time sampled"), "work steps every instruction: {text}");
        let sampled = |mut profile: Json| {
            profile["kinds"] = json!(["time"]);
            profile
        };
        let (text, _) = compare(&sampled(before), &sampled(after), 20);
        assert!(text.contains("host time sampled (noisy)") && text.contains("~+40.0 %") && text.contains("~ measured:"), "host time's change carries ~: {text}");
    }

    /// One Heartbeat handler that clones a model and destroys the copy (profile.json's figures from such a run: 60 calls,
    /// the same instructions, its self work 12.494 ms for a model of 10 parts and 77.702 ms for one of 200)
    fn clone_handler(work: f64, cold: Option<f64>) -> Json {
        let mut own = json!({"insns": 1080.0, "work_ms": work, "alloc_bytes": 0.0, "time_ms": 0.0});
        if let Some(cold) = cold {
            own["cold_work_ms"] = json!(cold);
        }
        json!({
            "run": {"status": "ok", "frames": 60, "harness": "run.luau", "harness_hash": "h1"},
            "kinds": ["work"],
            "functions": [{"id": 0, "key": "Spawner:anonymous@13", "name": "", "module": "ReplicatedStorage.Game.Spawner", "line": 13, "kind": "Luau",
                "code_hash": "c1", "source_hash": "s1", "calls": 60, "self": own, "total": {"work_ms": work}}],
        })
    }

    /// The same calls and instructions are no evidence of a price change: a handler that clones a model of 200 parts runs
    /// the instructions one of 10 parts does, and the engine's charges for what it cloned grow twentyfold. It is tagged
    /// only when its cold and first-use charges account for its change, within the warm-part tolerance and 2 us
    #[test]
    fn the_same_instructions_are_no_evidence_of_a_price_change() {
        let tagged = |before: Json, after: Json| {
            let (text, json) = compare(&before, &after, 20);
            (json["functions"][0]["price_only"] == true, text)
        };
        // no cold split: not tagged, listed among the changes, with the reason
        let (priced, text) = tagged(clone_handler(12.494, None), clone_handler(77.702, None));
        assert!(!priced && !text.contains("priced differently, same path"), "{text}");
        assert!(text.contains("functions that moved most") && text.contains("12.494 -> 77.702") && text.contains("note: 1 of the game's functions above ran the same calls"), "{text}");
        // a cold split that explains 0.1 ms of 65 ms: not tagged, and no note (the split is there and says no)
        let (priced, text) = tagged(clone_handler(12.494, Some(0.2)), clone_handler(77.702, Some(0.3)));
        assert!(!priced && !text.contains("note: 1 of the game's functions"), "{text}");
        // cold charges that account for the change: tagged, within the stated 0.01 % of the warm part and 2 us
        let (priced, text) = tagged(clone_handler(12.494, Some(0.2)), clone_handler(13.0, Some(0.706)));
        assert!(priced && text.contains("priced differently, same path") && text.contains("within 0.01 % of the rest and 2 us") && text.contains("(+0.506 cold)"), "{text}");
        assert!(tagged(clone_handler(12.494, Some(0.2)), clone_handler(13.0, Some(0.705))).0, "1 us unexplained, inside 2 us");
        assert!(!tagged(clone_handler(12.494, Some(0.2)), clone_handler(13.0, Some(0.703))).0, "3 us unexplained, outside 2 us");
        // 0.5 ms of real growth in a 50 ms function is no price change (1 % of its work would have hidden it); a move of
        // 0.5 ms of cold charges onto it is
        assert!(!tagged(clone_handler(50.0, Some(1.0)), clone_handler(50.5, Some(1.0))).0, "0.5 ms of growth in 50 ms");
        assert!(tagged(clone_handler(50.0, Some(1.0)), clone_handler(50.5, Some(1.5))).0, "0.5 ms of cold charges moved onto it");
    }

    /// A function whose figures are the same both times is no row, however its work's last bits fell: a profile from an
    /// older ree against a newer one read `build` 0.039 -> 0.039 ms, 6.9e-18 apart, with the same calls, instructions and
    /// allocations, and listed it as moved with the note (the viewer, by the same rule, lists nothing)
    #[test]
    fn float_noise_in_a_figure_is_no_move() {
        let work = 0.039f64;
        let noisy = f64::from_bits(work.to_bits() + 1);
        assert!(noisy != work && (noisy - work).abs() < 1e-17);
        let (text, json) = compare(&clone_handler(work, None), &clone_handler(noisy, None), 20);
        assert!(json["functions"].as_array().unwrap().is_empty(), "{text}");
        assert!(!text.contains("functions that moved most") && !text.contains("note: 1 of the game's functions"), "{text}");
        // (1 ns and more is a move)
        let (_, json) = compare(&clone_handler(work, None), &clone_handler(work + 2e-6, None), 20);
        assert_eq!(json["functions"].as_array().unwrap().len(), 1);
    }

    /// A C function the harness called: of its 5 ms of self work, 2 ms were done for the harness, and of its 3 ms of cold
    /// charges, 1.5 ms were. A scoped row keeps what was done for the game, in both figures.
    #[test]
    fn scoped_cold_charges_follow_the_work_owners_side() {
        let self_work_ms = 5.0;
        let harness_self_work_ms = 2.0;
        let cold_work_ms = 3.0;
        let harness_cold_work_ms = 1.5;
        let profile = json!({"functions": [{
            "id": -2,
            "key": "C:helper",
            "name": "helper",
            "kind": "C",
            "calls": 10,
            "phases": {"run": {
                "self_work_ms": self_work_ms,
                "harness_self_work_ms": harness_self_work_ms,
                "cold_work_ms": cold_work_ms,
                "harness_cold_work_ms": harness_cold_work_ms,
                "calls": 10,
                "insns": 0
            }}
        }]});
        let scope = vec!["run".to_string()];

        let entries = functions(&profile, Some(&scope));

        let helper = &entries["C:helper"];
        assert_eq!(helper.figures.self_work, self_work_ms - harness_self_work_ms);
        assert_eq!(helper.figures.cold, Some(cold_work_ms - harness_cold_work_ms));
        assert!(!price_only(helper, helper), "a C function has no instruction path to compare");
    }

    /// The review's `record` closure ran the same path with exactly the same self work and cold charges on both sides, and
    /// allocated one byte less. Its row stays for the allocations, but nothing about its price changed, so it is not tagged;
    /// the same closure whose work moved by its cold charges alone is.
    #[test]
    fn an_allocation_change_alone_is_not_a_change_of_price() {
        let self_work_ms = 0.0016853;
        let cold_work_ms = 0.00002805;
        let before = FunctionEntry {
            figures: FunctionFigures {
                self_work: self_work_ms,
                cold: Some(cold_work_ms),
                calls: 2.0,
                insns: 33.0,
                alloc: 5334.0,
                ..FunctionFigures::default()
            },
            ..FunctionEntry::default()
        };

        let mut allocated_less = before.clone();
        allocated_less.figures.alloc = 5333.0;
        assert!(!price_only(&before, &allocated_less), "unchanged work is no change of price");

        let repriced_ms = 0.001;
        let mut repriced = before.clone();
        repriced.figures.self_work += repriced_ms;
        repriced.figures.cold = Some(cold_work_ms + repriced_ms);
        assert!(price_only(&before, &repriced), "work moved by its cold charges alone is a change of price");
    }

    /// The same handler in a version 1 profile and a version 2 profile, moved from line 13 to line 16 by an edit. It still
    /// pairs, and its allocations compare; its work, calls and instructions mean something else in the two versions, so
    /// they are left out, and it is never tagged as priced differently.
    #[test]
    fn moved_functions_keep_only_comparable_figures_across_versions() {
        let allocated_before = 100;
        let allocated_after = 200;
        let mut before = clone_handler(12.0, Some(2.0));
        before["version"] = json!(1);
        before["functions"][0]["self"]["alloc_bytes"] = json!(allocated_before);
        let mut after = clone_handler(14.0, Some(4.0));
        after["version"] = json!(2);
        after["functions"][0]["key"] = json!("Spawner:anonymous@16");
        after["functions"][0]["line"] = json!(16);
        after["functions"][0]["self"]["alloc_bytes"] = json!(allocated_after);

        let (text, diff) = compare(&before, &after, 20);

        let rows = diff["functions"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{text}");
        let row = &rows[0];
        assert_eq!(row["moved"], json!({"from": 13, "to": 16}));
        assert_eq!(row["price_only"], false);
        let figures_left_out = ["self_work_ms", "total_work_ms", "calls", "insns", "time_ms", "cold_work_ms"];
        for side in ["before", "after"] {
            for figure in figures_left_out {
                assert!(row[side][figure].is_null(), "{side} {figure}: {row}");
            }
        }
        assert_eq!(row["before"]["alloc_bytes"], allocated_before as f64);
        assert_eq!(row["after"]["alloc_bytes"], allocated_after as f64);
        assert!(!text.contains("priced differently, same path"), "{text}");
    }
}

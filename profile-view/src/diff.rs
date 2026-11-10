//! Two profiles side by side (before and after): the headline figures `ree profile diff` prints, and the functions,
//! lines and allocation sites paired across the two. Functions pair by their key (module and name); lines only in
//! modules whose source is the same in both (the profile's source hashes), since a line number means nothing across an
//! edit; allocation sites by line in unchanged modules, else by function.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    app::App,
    fmt,
    metric::{Metric, Unit},
    model::{Kind, Profile},
    sites::ENGINE,
};

/// A headline figure
#[derive(Clone, Debug)]
pub struct Figure {
    pub name: String,
    pub before: f64,
    pub after: f64,
    pub unit: Unit,
    /// The same every run of the same code (else noisy: a change may be noise)
    pub exact: bool,
    /// Lower is better (a cost); false for figures that only describe the run
    pub cost: bool,
}

/// A paired row: a function, a line or a site
#[derive(Clone, Debug, Default)]
pub struct Pair {
    pub label: String,
    pub before: f64,
    pub after: f64,
    pub calls: Option<(u64, u64)>,
    /// A function's own instructions before and after (in the phases its work covers)
    pub insns: Option<(f64, f64)>,
    /// The after profile's function (and line), where enter goes
    pub func: Option<u32>,
    pub line: Option<i64>,
    /// The harness's own code (listed after the game's)
    pub harness: bool,
    /// A Luau function whose change in work the profiles show to be a change of price only ([`price_only`]): it ran the
    /// same path both times and its cold and first-use charges account for the change
    pub price_only: bool,
    /// `gone` or `new` for a function on one side only
    pub status: Option<&'static str>,
    /// An anonymous function an edit moved: its line before and after
    pub moved: Option<(i64, i64)>,
}

/// The phases two profiles both report, as `ree profile diff` picks them: the same scope in both, else `steady` when
/// both marked one; None for profiles without figures per phase (from before ree c692e40), which are compared whole
pub fn shared_scope(b: &Profile, a: &Profile) -> Option<Vec<String>> {
    if !b.has_phase_figures() || !a.has_phase_figures() {
        return None;
    }
    if let (Some(sb), Some(sa)) = (&b.raw.scope, &a.raw.scope) {
        if sb.phases == sa.phases && !sb.phases.is_empty() {
            return Some(sb.phases.clone());
        }
    }
    let marked = |p: &Profile| p.raw.phases.iter().any(|phase| phase == "steady");
    (marked(b) && marked(a)).then(|| vec!["steady".to_string()])
}

impl Pair {
    pub fn delta(&self) -> f64 {
        self.after - self.before
    }
}

fn last_heap(p: &Profile) -> Option<(f64, f64, f64, f64)> {
    let snapshot = p.raw.heap.as_ref()?.snapshots.last()?;
    let (reachable, objects, unswept) = snapshot.split();
    Some((reachable, objects, unswept, snapshot.allocated_since.bytes))
}

fn timeline_stats(p: &Profile) -> Option<(f64, f64, Option<f64>)> {
    let timeline = p.raw.timeline.as_ref()?;
    let work = timeline.values("work_ms")?;
    if work.is_empty() {
        return None;
    }
    // (past startup, as the Overview reads a frame: the mount apart; host time a frame only where the run took it)
    let worst = p.frames_past_startup(&work).into_iter().fold(0.0, f64::max);
    Some((p.frames_mean(&work), worst, timeline.values("host_ms").map(|host| p.frames_mean(&host))))
}

/// Whether two profiles count alike: of one profile.json version (none given is 1). Across versions, a function's and a line's
/// self work (C functions are rows of their own since version 2), host time (the wall clock's before, thread CPU since) and the
/// caches' counts changed meaning
pub fn same_version(b: &Profile, a: &Profile) -> bool {
    b.raw.version.max(1) == a.raw.version.max(1)
}

/// The headline figures both profiles have, and notes on how to read them
pub fn headline(b: &Profile, a: &Profile) -> (Vec<Figure>, Vec<String>) {
    let same = same_version(b, a);
    let mut out = Vec::new();
    let mut add = |name: &str, before: f64, after: f64, unit: Unit, exact: bool, cost: bool| out.push(Figure { name: name.into(), before, after, unit, exact, cost });
    let frames = (timeline_stats(b), timeline_stats(a));
    // the game's code in the phases both report, apart from the harness's (whose work moves with what it watches, not
    // with the game), first, as ree profile diff leads with them
    let shared = shared_scope(b, a);
    if let (Some(names), true) = (&shared, same) {
        let (scope_b, scope_a) = (b.scope_of(names), a.scope_of(names));
        if let (Some(sb), Some(sa)) = (b.side_work(&scope_b), a.side_work(&scope_a)) {
            add(&format!("the game's code, {}", scope_a.label), sb.0, sa.0, Unit::Ms, true, true);
            if b.has_harness() || a.has_harness() {
                add(&format!("the harness's code, {}", scope_a.label), sb.1, sa.1, Unit::Ms, true, true);
            }
        }
    }
    // the engine's side of the same phases (the whole run without them): what the scripts asked it to do, counted,
    // whatever the work clock prices it at, in ree profile diff's groups (the note says which phases)
    let (scope_b, scope_a) = match &shared {
        Some(names) => (b.scope_of(names), a.scope_of(names)),
        None => (crate::model::Scope { phases: None, label: String::new() }, crate::model::Scope { phases: None, label: String::new() }),
    };
    let mut engine_rows = false;
    if let (Some(eb), Some(ea)) = (b.engine_ops(&scope_b), a.engine_ops(&scope_a)) {
        let get = |ops: &BTreeMap<String, f64>, names: &[&str]| names.iter().map(|name| ops.get(*name).copied().unwrap_or(0.0)).sum::<f64>();
        for (name, counts) in [
            ("instances made", &["cloned", "created"][..]),
            ("instances destroyed", &["destroyed"][..]),
            ("property writes", &["writes"][..]),
            ("parts moved", &["geometry", "bulk_moved"][..]),
            ("pivots", &["pivots"][..]),
            ("emits", &["emits"][..]),
        ] {
            let (before, after) = (get(&eb, counts), get(&ea, counts));
            if before > 0.0 || after > 0.0 {
                add(&format!("engine: {name}"), before, after, Unit::Count, true, true);
                engine_rows = true;
            }
        }
    }
    if let (Some(wb), Some(wa)) = (&b.raw.work, &a.raw.work) {
        add("Studio work", wb.total_ms, wa.total_ms, Unit::Ms, true, true);
        // (a frame's from the timeline when both have one, below: the mount apart)
        if frames.0.is_none() || frames.1.is_none() {
            add("Studio work a frame", wb.per_frame_ms, wa.per_frame_ms, Unit::Ms, true, true);
        }
        add("script instructions", wb.script_insns, wa.script_insns, Unit::Count, true, true);
    }
    if let (Some(ab), Some(aa)) = (&b.raw.alloc, &a.raw.alloc) {
        add("allocated", ab.bytes, aa.bytes, Unit::Bytes, true, true);
        add("allocations", ab.objects, aa.objects, Unit::Count, true, true);
    }
    if let (Some(hb), Some(ha)) = (last_heap(b), last_heap(a)) {
        add("reachable heap at end", hb.0, ha.0, Unit::Bytes, true, true);
        add("reachable objects at end", hb.1, ha.1, Unit::Count, true, true);
        add("unswept garbage at end", hb.2, ha.2, Unit::Bytes, true, false);
        add("the run's objects still reachable", hb.3, ha.3, Unit::Bytes, true, true);
    }
    if let (Some(hb), Some(ha)) = (&b.raw.heap, &a.raw.heap) {
        add("collector cycles", hb.gc.cycles, ha.gc.cycles, Unit::Count, true, true);
        add("collector freed", hb.gc.freed, ha.gc.freed, Unit::Bytes, true, true);
    }
    // (host time only when neither run stepped every instruction: work and caches make the interpreter about five times
    // slower, so a stepping profile's host time is the profiler's, not the code's)
    let stepping = |p: &Profile| p.has_kind("work") || p.has_kind("caches");
    let host_time = same && !stepping(b) && !stepping(a);
    if let (true, Some(tb), Some(ta)) = (host_time, &b.raw.time, &a.raw.time) {
        add("host time (noisy)", tb.sampled_ms, ta.sampled_ms, Unit::Ms, false, true);
    }
    // the CPU's counters both profiles read (by their names now: an older profile's fills match a newer one's), then
    // what memory cost the run thread in each: the share of its cycles stalled on loads, those cycles a thousand
    // instructions, a miss's latency, IPC
    if let (Some(cb), Some(ca)) = (&b.raw.counters, &a.raw.counters) {
        for (name, value) in &cb.run.counts {
            if let Some(after) = ca.run.counts.get(name) {
                add(&format!("{name} (CPU counter)"), *value, *after, Unit::Count, false, true);
            }
        }
        let figures = |c: &crate::model::Counters| c.figures(&c.names.iter().map(|name| c.run.counts.get(name).copied().unwrap_or(0.0)).collect::<Vec<_>>());
        let (fb, fa) = (figures(cb), figures(ca));
        for (key, unit, cost) in [(crate::hw_counters::LOAD_STALL_SHARE, Unit::Share, true), (crate::hw_counters::STALL_PER_THOUSAND, Unit::Decimal, true), (crate::hw_counters::MISS_LATENCY, Unit::Count, false), ("IPC", Unit::Decimal, false)] {
            let get = |figures: &[crate::hw_counters::Figure]| figures.iter().find(|figure| figure.key == key).map(|figure| figure.value);
            if let (Some(before), Some(after)) = (get(&fb), get(&fa)) {
                add(&format!("{key} (CPU counters)"), before, after, unit, false, cost);
            }
        }
    }
    if let (Some(sb), Some(sa)) = frames {
        add("Studio work a frame (mean)", sb.0, sa.0, Unit::Ms, true, true);
        add("Studio work a frame (worst)", sb.1, sa.1, Unit::Ms, true, true);
        if let (Some(before), Some(after), true) = (sb.2, sa.2, host_time) {
            add("host time a frame (mean, noisy)", before, after, Unit::Ms, false, true);
        }
    }
    add("frames", b.raw.run.frames as f64, a.raw.run.frames as f64, Unit::Count, true, false);
    let mut notes = Vec::new();
    if !same {
        notes.push(format!(
            "the profiles are of two versions ({} and {}): functions' and lines' self work, calls and instructions, the game's and the harness's code, host time and the caches are not compared (their meaning changed)",
            b.raw.version.max(1),
            a.raw.version.max(1)
        ));
    }
    if b.raw.run.frames != a.raw.run.frames {
        notes.push(format!("the runs ran {} and {} frames: compare figures a frame", fmt::grouped(b.raw.run.frames as f64), fmt::grouped(a.raw.run.frames as f64)));
    }
    // (by the harness's contents where both profiles hashed it, else by the files' names: two copies of one harness in
    // other folders are the same harness)
    let differ = match (b.harness_identity(), a.harness_identity()) {
        (Some((true, hb)), Some((true, ha))) => (hb != ha).then_some("their contents differ"),
        (Some(_), Some(_)) => (b.harness() != a.harness()).then_some("by their file names: a profile from before ree hashed the harness"),
        _ => None,
    };
    if let Some(how) = differ {
        notes.push(format!("the runs ran other harnesses: {} and {} ({how})", b.harness().unwrap_or_default(), a.harness().unwrap_or_default()));
    }
    if let Some(names) = &shared {
        let label = a.scope_of(names).label;
        let harness = b.has_harness() || a.has_harness();
        let first = if harness { "the first two figures" } else { "the first figure" };
        let engine = if engine_rows { ", the engine's counts" } else { "" };
        let last = if harness { " (the harness's functions last)" } else { "" };
        notes.push(format!("{first}{engine} and the functions' work cover {label}{last}; the rest cover the whole run"));
    }
    // (the functions' counter figures compare only between profiles whose counters reached functions the same way)
    if let (Some(cb), Some(ca)) = (&b.raw.counters, &a.raw.counters)
        && cb.attribution_text() != ca.attribution_text()
    {
        notes.push(format!("the CPU counters reached functions in other ways (before: {}; after: {}): the run's counts compare, the functions' only roughly", cb.attribution_text(), ca.attribution_text()));
    }
    let changed = changed_modules(b, a);
    if !changed.is_empty() {
        notes.push(format!("source changed in {}: its lines are not compared", changed.iter().map(|m| crate::model::short_module(m).to_string()).collect::<Vec<_>>().join(", ")));
    }
    (out, notes)
}

/// Modules whose source differs between the two
pub fn changed_modules(b: &Profile, a: &Profile) -> BTreeSet<String> {
    b.raw.source_hashes.iter().filter(|(module, hash)| a.raw.source_hashes.get(*module).is_some_and(|other| other != *hash)).map(|(module, _)| module.clone()).collect()
}

/// Modules whose source is the same in both
fn same_modules(b: &Profile, a: &Profile) -> BTreeSet<String> {
    b.raw.source_hashes.iter().filter(|(module, hash)| a.raw.source_hashes.get(*module) == Some(*hash)).map(|(module, _)| module.clone()).collect()
}

/// A function's figures in a diff, keyed by its key: the chosen metric's own figure, calls, its own instructions, and what
/// pairs it across an edit (module, name, line, code and text hashes, its opcodes over the run)
#[derive(Clone, Debug, Default)]
struct FuncFig {
    label: String,
    own: f64,
    calls: u64,
    insns: f64,
    func: u32,
    harness: bool,
    module: String,
    anonymous: bool,
    line: i64,
    code: Option<String>,
    source: Option<String>,
    ops: BTreeMap<String, u64>,
    /// Of `own` (the work metric's, where the profile splits it), the cold and first-use charges
    cold: Option<f64>,
}

/// What a price-only change may leave unexplained, as `ree profile diff` allows: this share of the larger warm part (the
/// self work less its cold and first-use charges), and at least this much (ms). The work clock is deterministic, so a move
/// of who pays the cold charges leaves nothing unexplained but float rounding
pub const PRICE_SHARE: f64 = 0.0001;
pub const PRICE_FLOOR_MS: f64 = 0.002;

/// Whether a figure moved, by the one rule `ree profile diff` uses: a ms figure by more than 1 ns (two sums of the same
/// charges in another order part in their last bits), a count or bytes by a whole one
pub fn moved_by(unit: Unit, before: f64, after: f64) -> bool {
    let same = if matches!(unit, Unit::Ms) { 1e-6 } else { 0.5 };
    (after - before).abs() > same
}

/// Whether a function's change in self work is a change of price only, as `ree profile diff` decides it: it ran the same
/// path (the same calls, instructions and opcodes) and its cold and first-use charges account for the change, to within
/// [`PRICE_SHARE`] of the warm part and [`PRICE_FLOOR_MS`]. The same instructions prove nothing on their own (an engine call's charges grow
/// with what it is given: a model of 200 parts cloned costs twenty times one of 10), so without the work clock's cold
/// split on both sides nothing is tagged; nor is a C function, which runs no instructions to tell its path by
fn price_only(b: &FuncFig, a: &FuncFig) -> bool {
    let same_path = b.calls == a.calls && (b.insns - a.insns).abs() < 0.5 && b.insns > 0.0 && (b.ops.is_empty() || a.ops.is_empty() || b.ops == a.ops);
    let (Some(cold_before), Some(cold_after)) = (b.cold, a.cold) else { return false };
    let unexplained = (a.own - b.own) - (cold_after - cold_before);
    let warm = (a.own - cold_after).max(b.own - cold_before).max(0.0);
    same_path && unexplained.abs() <= (PRICE_SHARE * warm).max(PRICE_FLOOR_MS)
}

fn function_figures(app: &App, profile: usize, metric: Metric) -> BTreeMap<String, FuncFig> {
    let p = &app.profiles[profile];
    // (each function's instructions by opcode over the run, from its lines)
    let mut ops: BTreeMap<usize, BTreeMap<String, u64>> = BTreeMap::new();
    for (&(func, _), fig) in &p.lines {
        let entry = ops.entry(func).or_default();
        for (op, count) in &fig.ops {
            *entry.entry(op.clone()).or_insert(0) += count;
        }
    }
    let shared = (app.profiles.len() == 2).then(|| shared_scope(&app.profiles[0], &app.profiles[1])).flatten();
    let scoped = matches!((metric, app.scripts_only, &shared), (Metric::Work, false, Some(_)));
    let scope = shared.as_ref().map(|names| p.scope_of(names));
    let mut out: BTreeMap<String, FuncFig> = BTreeMap::new();
    // (cold: the cold and first-use part of `own`, only where `own` is the function's own work as the profile splits it:
    // the work metric, without C functions folded into their callers)
    // (`fill`: only a function not there yet, with nothing of the metric)
    let mut put = |func: u32, own: f64, calls: u64, cold: Option<f64>, fill: bool| {
        let f = &p.funcs[func as usize];
        if f.kind == Kind::Engine || (app.scripts_only && f.kind != Kind::Script) || (fill && out.contains_key(&f.key)) {
            return;
        }
        // (a function's own instructions in the phases its work covers)
        let insns = match (&scope, scoped) {
            (Some(scope), true) => f.phases.iter().filter(|(phase, _)| scope.covers(**phase)).map(|(_, figures)| figures.insns).sum(),
            _ => f.own.insns,
        };
        let entry = out.entry(f.key.clone()).or_insert_with(|| FuncFig {
            label: f.label(),
            func,
            harness: f.harness,
            module: f.module.clone(),
            anonymous: f.name == "anonymous" && !f.module.is_empty(),
            line: f.line,
            code: f.code_hash.clone(),
            source: f.source_hash.clone(),
            ops: ops.get(&(func as usize)).cloned().unwrap_or_default(),
            ..FuncFig::default()
        });
        entry.own += own;
        entry.calls += calls;
        entry.insns += insns;
        if let Some(ms) = cold {
            entry.cold = Some(entry.cold.unwrap_or(0.0) + ms);
        }
    };
    if let (true, Some(scope)) = (scoped, &scope) {
        // (a harness function's whole self work; the rest only the game's side of it, as ree profile diff counts them)
        for (index, f) in p.funcs.iter().enumerate() {
            let (mut own, mut calls, mut cold) = (0.0, 0.0, None::<f64>);
            for (&phase, figures) in &f.phases {
                if scope.covers(phase) {
                    own += if f.harness { figures.self_work_ms } else { (figures.self_work_ms - figures.harness_self_work_ms).max(0.0) };
                    calls += figures.calls;
                    if let Some(ms) = figures.cold_work_ms {
                        let harness_cold = if f.harness { 0.0 } else { figures.harness_cold_work_ms.unwrap_or(0.0) };
                        cold = Some(cold.unwrap_or(0.0) + (ms - harness_cold).max(0.0));
                    }
                }
            }
            if own > 0.0 || calls > 0.0 {
                put(index as u32, own, calls as u64, cold, false);
            }
        }
        return out;
    }
    if metric.has_stacks(p) {
        let tree = app.tree_of(profile, metric, app.scripts_only);
        let split = matches!(metric, Metric::Work) && !app.scripts_only;
        for (&func, stat) in &tree.funcs {
            // (folded stacks carry no calls: the profile's own count of them)
            let calls = if tree.has_calls { stat.calls } else { p.funcs[func as usize].calls };
            put(func, stat.own, calls, if split { p.funcs[func as usize].own.cold_work_ms } else { None }, false);
        }
    } else {
        for (&func, &(missed, _)) in &p.miss_funcs {
            put(func as u32, missed, p.funcs[func].calls, None, false);
        }
    }
    // (a function the profile lists as run that the metric did not see, host time's samples missing it say, ran all the
    // same: it is in this profile with nothing of the metric, not gone from it)
    for (index, f) in p.funcs.iter().enumerate() {
        if f.calls > 0 {
            put(index as u32, 0.0, f.calls, None, true);
        }
    }
    out
}

/// Pairs the anonymous functions of before and after, whose keys carry their line, across an edit that moved them, as ree
/// profile diff does: per module, the same code and text, then the same code, then (between those anchors, in order) the
/// rest where both sides hold as many; a profile without hashes pairs by line first. before key -> after key
fn pair_anonymous(before: &BTreeMap<String, FuncFig>, after: &BTreeMap<String, FuncFig>) -> BTreeMap<String, String> {
    let mut pairs = BTreeMap::new();
    let modules: BTreeSet<&String> = before.values().chain(after.values()).filter(|f| f.anonymous).map(|f| &f.module).collect();
    for module in modules {
        let side = |map: &BTreeMap<String, FuncFig>| -> Vec<(String, FuncFig)> {
            let mut list: Vec<(String, FuncFig)> = map.iter().filter(|(_, f)| f.anonymous && &f.module == module).map(|(key, f)| (key.clone(), f.clone())).collect();
            list.sort_by_key(|(_, f)| f.line);
            list
        };
        let (b, a) = (side(before), side(after));
        let (mut taken_b, mut taken_a) = (vec![false; b.len()], vec![false; a.len()]);
        let mut anchors: Vec<(usize, usize)> = Vec::new();
        let mut pass = |same: &dyn Fn(&FuncFig, &FuncFig) -> bool, ordered: bool| {
            for i in 0..b.len() {
                if taken_b[i] {
                    continue;
                }
                if let Some(j) = (0..a.len()).find(|&j| !taken_a[j] && same(&b[i].1, &a[j].1) && !(ordered && anchors.iter().any(|&(x, y)| (x < i) != (y < j)))) {
                    taken_b[i] = true;
                    taken_a[j] = true;
                    anchors.push((i, j));
                }
            }
        };
        pass(&|x, y| x.code.is_some() && x.code == y.code && x.source.is_some() && x.source == y.source, false);
        pass(&|x, y| x.code.is_some() && x.code == y.code, false);
        pass(&|x, y| x.code.is_none() && y.code.is_none() && x.line == y.line, true);
        anchors.sort();
        let mut bounds: Vec<(isize, isize)> = vec![(-1, -1)];
        for &(i, j) in &anchors {
            if bounds.last().is_some_and(|last| (j as isize) > last.1) {
                bounds.push((i as isize, j as isize));
            }
        }
        bounds.push((b.len() as isize, a.len() as isize));
        for window in bounds.windows(2) {
            let gap_b: Vec<usize> = ((window[0].0 + 1)..window[1].0).map(|i| i as usize).filter(|&i| !taken_b[i]).collect();
            let gap_a: Vec<usize> = ((window[0].1 + 1)..window[1].1).map(|j| j as usize).filter(|&j| !taken_a[j]).collect();
            if gap_b.len() == gap_a.len() {
                anchors.extend(gap_b.into_iter().zip(gap_a));
            }
        }
        for (i, j) in anchors {
            pairs.insert(b[i].0.clone(), a[j].0.clone());
        }
    }
    pairs
}

/// Functions paired (named ones by key, anonymous ones across a move), the ones that moved
pub fn functions(app: &App, metric: Metric) -> Vec<Pair> {
    let same = same_version(&app.profiles[0], &app.profiles[1]);
    if !same && matches!(metric, Metric::Work | Metric::Time | Metric::CacheMisses | Metric::Insns) {
        return Vec::new();
    }
    let (fb, fa) = (function_figures(app, 0, metric), function_figures(app, 1, metric));
    let moved = pair_anonymous(&fb, &fa);
    let paired_after: BTreeSet<&String> = moved.values().collect();
    // (instructions are counted only where the VM stepped every one, `work` or `caches`: a `time` profile has none)
    let counted = same && app.profiles.iter().take(2).all(|p| p.has_kind("work") || p.has_kind("caches"));
    let pair = |b: Option<&FuncFig>, a: Option<&FuncFig>| -> Pair {
        let price_only = match (b, a) {
            (Some(b), Some(a)) => price_only(b, a),
            _ => false,
        };
        let shifted = match (b, a) {
            (Some(b), Some(a)) if b.anonymous && b.line != a.line => Some((b.line, a.line)),
            _ => None,
        };
        let label = a.or(b).map(|f| f.label.clone()).unwrap_or_default();
        Pair {
            label: match shifted {
                Some((from, to)) => format!("{label} (moved from :{from} to :{to})"),
                None => label,
            },
            before: b.map_or(0.0, |f| f.own),
            after: a.map_or(0.0, |f| f.own),
            calls: same.then(|| (b.map_or(0, |f| f.calls), a.map_or(0, |f| f.calls))),
            insns: counted.then(|| (b.map_or(0.0, |f| f.insns), a.map_or(0.0, |f| f.insns))),
            func: a.map(|f| f.func),
            line: None,
            harness: a.or(b).is_some_and(|f| f.harness),
            price_only,
            status: match (b, a) {
                (Some(_), None) => Some("gone"),
                (None, Some(_)) => Some("new"),
                _ => None,
            },
            moved: shifted,
        }
    };
    let mut rows = Vec::new();
    for (key, b) in &fb {
        let after = if b.anonymous { moved.get(key).and_then(|key| fa.get(key)) } else { fa.get(key) };
        rows.push(pair(Some(b), after));
    }
    for (key, a) in &fa {
        let taken = if a.anonymous { paired_after.contains(key) } else { fb.contains_key(key) };
        if !taken {
            rows.push(pair(None, Some(a)));
        }
    }
    let unit = metric.unit();
    rows.into_iter().filter(|pair| moved_by(unit, pair.before, pair.after) || pair.calls.is_some_and(|(x, y)| x != y) || pair.insns.is_some_and(|(x, y)| moved_by(Unit::Count, x, y))).collect()
}

/// Lines of unchanged modules, paired by (module, line)
pub fn lines(app: &App, metric: Metric) -> Vec<Pair> {
    let (b, a) = (&app.profiles[0], &app.profiles[1]);
    if !same_version(b, a) && matches!(metric, Metric::Work | Metric::Time | Metric::CacheMisses | Metric::Insns) {
        return Vec::new();
    }
    let same = same_modules(b, a);
    let figures = |p: &Profile| -> BTreeMap<(String, i64), (f64, u32)> {
        let mut out: BTreeMap<(String, i64), (f64, u32)> = BTreeMap::new();
        for (&(func, line), fig) in &p.lines {
            let f = &p.funcs[func];
            if !same.contains(&f.module) || (app.scripts_only && f.kind != Kind::Script) {
                continue;
            }
            let entry = out.entry((f.module.clone(), line)).or_insert((0.0, func as u32));
            entry.0 += metric.line_self(fig);
        }
        out
    };
    let (lb, la) = (figures(b), figures(a));
    let keys: BTreeSet<&(String, i64)> = lb.keys().chain(la.keys()).collect();
    keys.into_iter()
        .map(|key| {
            let before = lb.get(key).map_or(0.0, |e| e.0);
            let after = la.get(key);
            Pair { label: format!("{}:{}", crate::model::short_module(&key.0), key.1), before, after: after.map_or(0.0, |e| e.0), func: after.map(|e| e.1), line: Some(key.1), ..Pair::default() }
        })
        .filter(|pair| moved_by(metric.unit(), pair.before, pair.after))
        .collect()
}

/// Allocation sites: by line in unchanged modules, else by function; bytes (or objects with the allocations metric)
pub fn sites(app: &App, metric: Metric) -> Vec<Pair> {
    let (b, a) = (&app.profiles[0], &app.profiles[1]);
    let same = same_modules(b, a);
    let by_count = metric == Metric::AllocCount;
    let figures = |index: usize| -> BTreeMap<String, (String, f64, Option<u32>, Option<i64>)> {
        let p = &app.profiles[index];
        let mut out: BTreeMap<String, (String, f64, Option<u32>, Option<i64>)> = BTreeMap::new();
        for site in app.sites_of(index).iter() {
            let value = if by_count { site.count } else { site.bytes };
            let (key, label, func, line) = if site.func == ENGINE {
                ("(engine)".to_string(), "(engine: no Luau on the stack)".to_string(), None, None)
            } else {
                let f = &p.funcs[site.func as usize];
                if app.scripts_only && f.kind != Kind::Script {
                    continue;
                }
                if same.contains(&f.module) && site.line >= 0 {
                    (format!("{}:{}", f.module, site.line), format!("{}  {}:{}", f.name, f.short, site.line), Some(site.func), Some(site.line))
                } else {
                    (f.key.clone(), format!("{}  {} (whole function)", f.name, f.short), Some(site.func), None)
                }
            };
            let entry = out.entry(key).or_insert((label, 0.0, func, line));
            entry.1 += value;
        }
        out
    };
    let (sb, sa) = (figures(0), figures(1));
    let keys: BTreeSet<&String> = sb.keys().chain(sa.keys()).collect();
    keys.into_iter()
        .map(|key| {
            let before = sb.get(key);
            let after = sa.get(key);
            Pair { label: after.or(before).map(|e| e.0.clone()).unwrap_or_default(), before: before.map_or(0.0, |e| e.1), after: after.map_or(0.0, |e| e.1), func: after.and_then(|e| e.2), line: after.and_then(|e| e.3), ..Pair::default() }
        })
        .filter(|pair| moved_by(Unit::Count, pair.before, pair.after))
        .collect()
}

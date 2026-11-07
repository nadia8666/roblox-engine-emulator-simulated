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
#[derive(Clone, Debug)]
pub struct Pair {
    pub label: String,
    pub before: f64,
    pub after: f64,
    pub calls: Option<(u64, u64)>,
    /// The after profile's function (and line), where enter goes
    pub func: Option<u32>,
    pub line: Option<i64>,
    /// The harness's own code (listed after the game's)
    pub harness: bool,
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

fn timeline_stats(p: &Profile) -> Option<(f64, f64, f64)> {
    let timeline = p.raw.timeline.as_ref()?;
    let work = timeline.values("work_ms")?;
    let host = timeline.values("host_ms").unwrap_or_default();
    if work.is_empty() {
        return None;
    }
    // (past startup, as the Overview reads a frame: the mount apart)
    let worst = p.frames_past_startup(&work).into_iter().fold(0.0, f64::max);
    Some((p.frames_mean(&work), worst, p.frames_mean(&host)))
}

/// The headline figures both profiles have, and notes on how to read them
pub fn headline(b: &Profile, a: &Profile) -> (Vec<Figure>, Vec<String>) {
    let mut out = Vec::new();
    let mut add = |name: &str, before: f64, after: f64, unit: Unit, exact: bool, cost: bool| out.push(Figure { name: name.into(), before, after, unit, exact, cost });
    let frames = (timeline_stats(b), timeline_stats(a));
    // the game's code in the phases both report, apart from the harness's (whose work moves with what it watches, not
    // with the game), first, as ree profile diff leads with them
    let shared = shared_scope(b, a);
    if let Some(names) = &shared {
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
    if let (Some(tb), Some(ta)) = (&b.raw.time, &a.raw.time) {
        add("host time (noisy)", tb.sampled_ms, ta.sampled_ms, Unit::Ms, false, true);
    }
    if let (Some(cb), Some(ca)) = (&b.raw.counters, &a.raw.counters) {
        for (name, value) in &cb.run.counts {
            if let Some(after) = ca.run.counts.get(name) {
                add(&format!("{name} (CPU counter)"), *value, *after, Unit::Count, false, true);
            }
        }
    }
    if let (Some(sb), Some(sa)) = frames {
        add("Studio work a frame (mean)", sb.0, sa.0, Unit::Ms, true, true);
        add("Studio work a frame (worst)", sb.1, sa.1, Unit::Ms, true, true);
        add("host time a frame (mean, noisy)", sb.2, sa.2, Unit::Ms, false, true);
    }
    add("frames", b.raw.run.frames as f64, a.raw.run.frames as f64, Unit::Count, true, false);
    let mut notes = Vec::new();
    if b.raw.run.frames != a.raw.run.frames {
        notes.push(format!("the runs ran {} and {} frames: compare figures a frame", fmt::grouped(b.raw.run.frames as f64), fmt::grouped(a.raw.run.frames as f64)));
    }
    // (by the harness's file name: a fixed copy of a harness elsewhere is the same harness)
    if let (Some(hb), Some(ha)) = (b.harness(), a.harness()) {
        if hb != ha {
            notes.push(format!("the runs ran other harnesses: {hb} and {ha}"));
        }
    }
    if let Some(names) = &shared {
        let label = a.scope_of(names).label;
        let harness = b.has_harness() || a.has_harness();
        let first = if harness { "the first two figures" } else { "the first figure" };
        let engine = if engine_rows { ", the engine's counts" } else { "" };
        let last = if harness { " (the harness's functions last)" } else { "" };
        notes.push(format!("{first}{engine} and the functions' work cover {label}{last}; the rest cover the whole run"));
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

/// Per function key: (label, own figure, calls, the function's index, the harness's)
fn function_figures(app: &App, profile: usize, metric: Metric) -> BTreeMap<String, (String, f64, u64, u32, bool)> {
    let p = &app.profiles[profile];
    let mut out: BTreeMap<String, (String, f64, u64, u32, bool)> = BTreeMap::new();
    let mut put = |func: u32, own: f64, calls: u64| {
        let f = &p.funcs[func as usize];
        if f.kind == Kind::Engine || (app.scripts_only && f.kind != Kind::Script) {
            return;
        }
        let entry = out.entry(f.key.clone()).or_insert_with(|| (f.label(), 0.0, 0, func, f.harness));
        entry.1 += own;
        entry.2 += calls;
    };
    let shared = (app.profiles.len() == 2).then(|| shared_scope(&app.profiles[0], &app.profiles[1])).flatten();
    if let (Metric::Work, false, Some(names)) = (metric, app.scripts_only, &shared) {
        // (a harness function's whole self work; the rest only the game's side of it, as ree profile diff counts them)
        let scope = p.scope_of(names);
        for (index, f) in p.funcs.iter().enumerate() {
            let (mut own, mut calls) = (0.0, 0.0);
            for (&phase, figures) in &f.phases {
                if scope.covers(phase) {
                    own += if f.harness { figures.self_work_ms } else { (figures.self_work_ms - figures.harness_self_work_ms).max(0.0) };
                    calls += figures.calls;
                }
            }
            if own > 0.0 || calls > 0.0 {
                put(index as u32, own, calls as u64);
            }
        }
        return out;
    }
    if metric.has_stacks(p) {
        let tree = app.tree_of(profile, metric, app.scripts_only);
        for (&func, stat) in &tree.funcs {
            put(func, stat.own, stat.calls);
        }
    } else {
        for (&func, &(missed, _)) in &p.miss_funcs {
            put(func as u32, missed, p.funcs[func].calls);
        }
    }
    out
}

/// Functions paired by key, the ones that moved
pub fn functions(app: &App, metric: Metric) -> Vec<Pair> {
    let (fb, fa) = (function_figures(app, 0, metric), function_figures(app, 1, metric));
    let harness = |key: &String| fa.get(key).or_else(|| fb.get(key)).is_some_and(|entry| entry.4);
    let keys: BTreeSet<&String> = fb.keys().chain(fa.keys()).collect();
    keys.into_iter()
        .map(|key| {
            let b = fb.get(key);
            let a = fa.get(key);
            Pair {
                label: a.or(b).map(|e| e.0.clone()).unwrap_or_else(|| key.clone()),
                before: b.map_or(0.0, |e| e.1),
                after: a.map_or(0.0, |e| e.1),
                calls: Some((b.map_or(0, |e| e.2), a.map_or(0, |e| e.2))),
                func: a.map(|e| e.3),
                line: None,
                harness: harness(key),
            }
        })
        .filter(|pair| pair.delta().abs() > 1e-12 || pair.calls.is_some_and(|(x, y)| x != y))
        .collect()
}

/// Lines of unchanged modules, paired by (module, line)
pub fn lines(app: &App, metric: Metric) -> Vec<Pair> {
    let (b, a) = (&app.profiles[0], &app.profiles[1]);
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
            Pair { label: format!("{}:{}", crate::model::short_module(&key.0), key.1), before, after: after.map_or(0.0, |e| e.0), calls: None, func: after.map(|e| e.1), line: Some(key.1), harness: false }
        })
        .filter(|pair| pair.delta().abs() > 1e-12)
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
            Pair { label: after.or(before).map(|e| e.0.clone()).unwrap_or_default(), before: before.map_or(0.0, |e| e.1), after: after.map_or(0.0, |e| e.1), calls: None, func: after.and_then(|e| e.2), line: after.and_then(|e| e.3), harness: false }
        })
        .filter(|pair| pair.delta().abs() > 1e-9)
        .collect()
}

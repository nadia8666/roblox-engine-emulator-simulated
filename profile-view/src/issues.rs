//! The Issues list: code-flow and allocation problems named from measured patterns only. Each issue states the
//! numbers it rests on and links to the view and line that show them; a pattern the profile's kinds cannot show is
//! listed as not checked, never guessed at.

use crate::{
    fmt,
    model::{Kind, Profile},
    sites::{AllocSite, ENGINE, site_label},
    tree::{Frame, Tree},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    Serious,
    Warning,
    Info,
}

impl Severity {
    /// The icon beside the word (status colours never carry meaning alone)
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Critical => "■",
            Severity::Serious => "▲",
            Severity::Warning => "●",
            Severity::Info => "○",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Serious => "serious",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// Where an issue's evidence is
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    None,
    /// A function (Calls)
    Func(u32),
    /// A line of a function (Source)
    Line(u32, i64),
    /// A timeline row (Timeline)
    Frame(usize),
    /// An allocation site (Allocations)
    Site(u32, i64),
    /// A Luau inline cache site, by its index in `luau_caches.sites` (Caches)
    Cache(usize),
    /// A heap snapshot (Heap)
    Snapshot(usize),
    /// A variant of a comparison (Compare)
    Variant(String),
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub severity: Severity,
    /// Which detector: `sort every frame`, `repeat reads`, ...
    pub kind: &'static str,
    pub title: String,
    /// The measured numbers, a line each
    pub evidence: Vec<String>,
    /// What would remove it, said as a condition where the profile cannot see the cause
    pub advice: String,
    pub target: Target,
    /// Ranks issues of one severity (a share of the run's cost, a rate)
    pub weight: f64,
}

/// A site's allocation a frame below which it is not listed
const SMALL_BYTES: f64 = 256.0;

/// The profile's issues, most severe first, and the patterns its kinds could not check
pub fn detect(p: &Profile, work: Option<&Tree>, sites: &[AllocSite]) -> (Vec<Issue>, Vec<String>) {
    let mut issues = Vec::new();
    let mut unchecked = Vec::new();
    let steady = p.steady_phase();
    // `the 240 frames`, `the 150 frames of the steady phase`
    let steady_frames = steady.map(|(phase, frames)| format!("the {} frames{}", fmt::grouped(frames as f64), p.phase_words(phase).map(|words| format!(" of {words}")).unwrap_or_default())).unwrap_or_default();
    if steady.is_none() && (work.is_some() || p.raw.alloc.is_some() || p.raw.timeline.is_some()) {
        unchecked.push("the steady state's sorts, allocation and growth: the run marks no phase after startup (the mount)".into());
    }

    // the harness's own code is not the game's: what it sorts, allocates, misses or reads again is left out, as ree's
    // summary leaves it out of its tables, unless the run's code is all the harness's
    let harness_out = p.has_harness()
        && p.scope_work()
            .or_else(|| {
                work.map(|tree| {
                    let maps = tree.sided(&p.scope(), |func| p.funcs.get(func as usize).filter(|f| f.kind == Kind::Script && !f.module.is_empty()).map(|f| f.harness));
                    (maps[0].values().sum(), maps[1].values().sum())
                })
            })
            .is_some_and(|(game, harness)| crate::model::harness_apart(game, harness));
    match work {
        Some(tree) => sorts_every_frame(p, tree, steady, &steady_frames, harness_out, &mut issues),
        None => unchecked.push("sorts every frame: needs work".into()),
    }
    if p.raw.alloc.is_some() {
        steady_allocation(p, sites, steady, &steady_frames, harness_out, &mut issues);
        table_growth(p, sites, harness_out, &mut issues);
    } else {
        unchecked.push("allocation and table growth: need alloc".into());
    }
    if p.raw.timeline.is_some() {
        spikes(p, &mut issues);
        growth(p, steady, &steady_frames, &mut issues);
    } else {
        unchecked.push("spikes and growing counts: need timeline".into());
    }
    if p.raw.heap.is_some() {
        heap_growth(p, &mut issues);
    }
    if p.raw.luau_caches.is_some() {
        cache_misses(p, harness_out, &mut issues);
    }
    match &p.boundary {
        Some(boundary) => repeat_reads(p, boundary, harness_out, &mut issues),
        None => unchecked.push("repeat reads and unchanged writes: need boundary, in a run of its own".into()),
    }
    match p.raw.counters.as_ref().filter(|c| c.error.is_none()) {
        Some(c) if c.names.iter().any(|name| name == "load-stall-cycles") && c.names.iter().any(|name| name == "cycles") => memory_waits(p, c, harness_out, &mut issues),
        Some(_) => unchecked.push("code that waits on memory: needs the counters' load-stall-cycles and cycles (the default set)".into()),
        None => unchecked.push("code that waits on memory: needs hw (the CPU's counters), in a run of its own".into()),
    }
    let harness = p.funcs.iter().filter(|f| f.harness).count();
    if harness_out {
        unchecked.push(format!("the harness's code ({harness} {}): the issues cover the game's code", if harness == 1 { "function" } else { "functions" }));
    }
    issues.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.weight.total_cmp(&a.weight)).then(a.title.cmp(&b.title)));
    (issues, unchecked)
}

/// The steady phase's work (ms), from the work tree's phase node
fn phase_work(tree: &Tree, phase: u32) -> f64 {
    tree.node(crate::tree::ROOT).children.iter().map(|&c| tree.node(c)).find(|n| n.frame == Frame::Phase(phase)).map_or(0.0, |n| n.total)
}

/// `table.sort` called about once a frame or more through the steady state, by the function that calls it
fn sorts_every_frame(p: &Profile, tree: &Tree, steady: Option<(u32, u64)>, steady_frames: &str, harness_out: bool, issues: &mut Vec<Issue>) {
    let Some((phase, frames)) = steady else { return };
    if frames < 10 || !tree.has_calls {
        return;
    }
    let Some(phase_node) = tree.node(crate::tree::ROOT).children.iter().copied().find(|&c| tree.node(c).frame == Frame::Phase(phase)) else { return };
    let phase_total = phase_work(tree, phase);
    // caller -> (sorts, work in them)
    let mut by_caller: std::collections::BTreeMap<u32, (u64, f64)> = std::collections::BTreeMap::new();
    for index in tree.preorder(phase_node) {
        let node = tree.node(index);
        let Frame::Func(func) = node.frame else { continue };
        let f = &p.funcs[func as usize];
        // (a newer ree names a C function by where the globals hold it: table.sort)
        if f.kind != Kind::C || !(f.name == "table.sort" || f.name == "sort [C]" || f.name == "sort") {
            continue;
        }
        let caller = tree.path(index).iter().rev().skip(1).find_map(|&up| match tree.node(up).frame {
            Frame::Func(g) if p.funcs[g as usize].kind != Kind::C => Some(g),
            _ => None,
        });
        let Some(caller) = caller else { continue };
        if harness_out && p.funcs[caller as usize].harness {
            continue;
        }
        let entry = by_caller.entry(caller).or_default();
        entry.0 += node.calls;
        entry.1 += node.total;
    }
    for (caller, (sorts, work)) in by_caller {
        let per_frame = sorts as f64 / frames as f64;
        if per_frame < 0.9 {
            continue;
        }
        let share = if phase_total > 0.0 { work / phase_total } else { 0.0 };
        let f = &p.funcs[caller as usize];
        issues.push(Issue {
            severity: if share >= 0.25 {
                Severity::Critical
            } else if share >= 0.10 {
                Severity::Serious
            } else {
                Severity::Warning
            },
            kind: "sort every frame",
            title: format!("Sorted every frame: {}", f.label()),
            evidence: vec![
                format!("table.sort ran {} times in {steady_frames}, {per_frame:.1} a frame", fmt::grouped(sorts as f64)),
                format!("{} a frame in the sorts, {} of the phase's work", fmt::ms(work / frames as f64), fmt::share(work, phase_total)),
            ],
            advice: "Keep the sorted list and sort again only when an item changes".into(),
            target: Target::Func(caller),
            weight: share,
        });
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// The bytes the phase's frames allocated in all, (smallest, median, largest), from the timeline. (Its `allocations`
/// column counts objects without their table storage, so counts are not compared with a site's.)
fn frame_allocation(p: &Profile, phase: u32) -> Option<(f64, f64, f64)> {
    let timeline = p.raw.timeline.as_ref()?;
    let phases = timeline.values("phase")?;
    let bytes = timeline.values("allocated_bytes")?;
    let rows: Vec<f64> = (0..phases.len()).filter(|&i| phases[i] as u32 == phase).map(|i| bytes[i]).collect();
    if rows.is_empty() {
        return None;
    }
    let smallest = rows.iter().copied().fold(f64::INFINITY, f64::min);
    let largest = rows.iter().copied().fold(0.0, f64::max);
    Some((smallest, median(&rows), largest))
}

/// Sites that allocate through the steady state. A site whose average a frame is more than the phase's smallest frame
/// allocates in all cannot allocate that every frame: it comes in bursts
fn steady_allocation(p: &Profile, sites: &[AllocSite], steady: Option<(u32, u64)>, steady_frames: &str, harness_out: bool, issues: &mut Vec<Issue>) {
    let Some((phase, frames)) = steady else { return };
    if frames < 10 {
        return;
    }
    let frame_bytes = frame_allocation(p, phase);
    let mut rows: Vec<(&AllocSite, f64, f64)> = sites
        .iter()
        .filter(|site| site.func != ENGINE && !(harness_out && p.funcs[site.func as usize].harness))
        .map(|site| {
            let (bytes, count) = site.per_frame(phase, frames);
            (site, bytes, count)
        })
        .filter(|(_, bytes, count)| *count >= 1.0 && *bytes >= SMALL_BYTES)
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.total_cmp(&a.2)));
    let all_bytes: f64 = sites.iter().map(|site| site.per_frame(phase, frames).0).sum();
    for (site, bytes, count) in rows.into_iter().take(6) {
        let types: Vec<String> = site.types.iter().take(3).map(|(ty, b, _)| format!("{ty} {}", fmt::share(*b, site.bytes.max(1.0)))).collect();
        let types = if types.is_empty() { String::new() } else { format!("; {}", types.join(", ")) };
        let target = if site.line >= 0 { Target::Site(site.func, site.line) } else { Target::Func(site.func) };
        let label = site_label(p, site);
        let (total, objects) = site.phases.get(&phase).copied().unwrap_or_default();
        if let Some((smallest, median_frame, largest)) = frame_bytes.filter(|(smallest, _, _)| bytes > *smallest) {
            issues.push(Issue {
                severity: if total >= 8.0 * 1024.0 * 1024.0 { Severity::Serious } else { Severity::Warning },
                kind: "allocation bursts",
                title: format!("Allocation bursts: {label}"),
                evidence: vec![
                    format!("{} in {steady_frames}, {} a frame on average, but some frames allocate {} in all", fmt::bytes(total), fmt::bytes(bytes), fmt::bytes(smallest)),
                    format!("frames allocate {} in the median, {} at most · {} objects{types}", fmt::bytes(median_frame), fmt::bytes(largest), fmt::grouped(objects)),
                ],
                advice: "A burst is collector work a few frames later: make less of it, or spread it over frames".into(),
                target,
                weight: total,
            });
            continue;
        }
        issues.push(Issue {
            severity: if bytes >= 1024.0 * 1024.0 {
                Severity::Critical
            } else if bytes >= 64.0 * 1024.0 {
                Severity::Serious
            } else {
                Severity::Warning
            },
            kind: "steady allocation",
            title: format!("{} a frame on average: {label}", fmt::bytes(bytes)),
            evidence: vec![
                format!("{} objects a frame over {steady_frames}; {} of what they allocate", fmt::grouped(count), fmt::share(bytes, all_bytes)),
                format!("over the run{}", if types.is_empty() { ": types not in the kept stacks".to_string() } else { types }),
            ],
            advice: "Reuse them between frames (clear and refill a kept table), or build them only when the input changes".into(),
            target,
            weight: bytes,
        });
    }
}

/// Table storage allocated past the new tables' own: tables grown a step at a time (each growth rehashes the keys)
fn table_growth(p: &Profile, sites: &[AllocSite], harness_out: bool, issues: &mut Vec<Issue>) {
    let frames = p.raw.run.frames.max(1) as f64;
    let mut rows: Vec<(&AllocSite, f64, f64, f64)> = sites
        .iter()
        .filter(|site| site.func != ENGINE && !(harness_out && p.funcs[site.func as usize].harness))
        .map(|site| {
            let (_, tables) = site.of_type("table");
            let (storage_bytes, storage) = site.of_type("table storage");
            (site, (storage - tables).max(0.0), tables, storage_bytes)
        })
        .filter(|(_, excess, _, _)| *excess >= frames.max(10.0))
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (site, excess, tables, storage_bytes) in rows.into_iter().take(4) {
        let per_frame = excess / frames;
        issues.push(Issue {
            severity: if per_frame >= 100.0 {
                Severity::Serious
            } else if per_frame >= 5.0 {
                Severity::Warning
            } else {
                Severity::Info
            },
            kind: "table growth",
            title: format!("Tables regrown: {}", site_label(p, site)),
            evidence: vec![
                format!("{} table storage allocations past the {} tables' first: {per_frame:.1} regrowths a frame over {} frames", fmt::grouped(excess), fmt::grouped(tables), fmt::grouped(frames)),
                format!("{} of table storage; each regrowth moves every key", fmt::bytes(storage_bytes)),
            ],
            advice: "Make the table at its final size (table.create(n), or a constructor with every field), or keep it between frames".into(),
            target: if site.line >= 0 { Target::Site(site.func, site.line) } else { Target::Func(site.func) },
            weight: excess,
        });
    }
}

/// Frames whose collector time or work stands far above the run's typical frame
fn spikes(p: &Profile, issues: &mut Vec<Issue>) {
    let Some(timeline) = &p.raw.timeline else { return };
    let frames = timeline.values("frame").unwrap_or_default();
    let frame = |i: usize| frames.get(i).copied().unwrap_or(i as f64);
    // (past the mount when the run marks a phase after startup: the mount's frame is expected to stand out)
    let phases = timeline.values("phase").unwrap_or_default();
    let later = phases.iter().any(|phase| *phase > 0.0);
    let kept = |i: usize| !later || phases.get(i).is_some_and(|phase| *phase > 0.0);
    let list = |rows: &[usize]| {
        let shown = rows.iter().take(8).map(|&i| format!("{}", frame(i))).collect::<Vec<_>>().join(", ");
        if rows.len() > 8 { format!("{shown} and {} more", rows.len() - 8) } else { shown }
    };
    // (by the collector's steps, which are the same every run of the same code: its milliseconds are host time)
    if let (Some(gc), Some(freed), Some(steps)) = (timeline.values("gc_ms"), timeline.values("gc_freed"), timeline.values("gc_steps")) {
        let typical = median(&(0..steps.len()).filter(|&i| kept(i)).map(|i| steps[i]).collect::<Vec<_>>());
        let floor = (typical * 5.0).max(50.0);
        let spikes: Vec<usize> = (0..steps.len()).filter(|&i| kept(i) && steps[i] >= floor).collect();
        if let Some(&worst) = spikes.iter().max_by(|&&a, &&b| steps[a].total_cmp(&steps[b]).then(b.cmp(&a))) {
            issues.push(Issue {
                severity: if steps[worst] >= 1000.0 { Severity::Serious } else { Severity::Warning },
                kind: "gc spike",
                title: format!("Collector spikes: {}, worst {} steps (frame {})", frames_text(spikes.len()), fmt::grouped(steps[worst]), frame(worst)),
                evidence: vec![
                    format!("frame {}: {} collector steps, {} freed, {} of host time; a frame's median is {} steps", frame(worst), fmt::grouped(steps[worst]), fmt::bytes(freed[worst]), fmt::ms(gc[worst]), fmt::grouped(typical)),
                    format!("frames of {} steps or more: {}", fmt::grouped(floor), list(&spikes)),
                ],
                advice: "The collector works off what was allocated: see the allocation issues".into(),
                target: Target::Frame(worst),
                weight: steps[worst],
            });
        }
    }
    if let Some(work) = timeline.values("work_ms") {
        let typical = median(&(0..work.len()).filter(|&i| kept(i)).map(|i| work[i]).collect::<Vec<_>>());
        let floor = (typical * 3.0).max(0.5);
        let spikes: Vec<usize> = (0..work.len()).filter(|&i| kept(i) && work[i] >= floor).collect();
        if let Some(&worst) = spikes.iter().max_by(|&&a, &&b| work[a].total_cmp(&work[b]).then(b.cmp(&a))) {
            issues.push(Issue {
                severity: if work[worst] >= 16.7 {
                    Severity::Critical
                } else if work[worst] >= 4.0 {
                    Severity::Serious
                } else {
                    Severity::Warning
                },
                kind: "frame spike",
                title: format!("Frame spikes: {}, worst {} (frame {})", frames_text(spikes.len()), fmt::ms(work[worst]), frame(worst)),
                evidence: {
                    let mut evidence = vec![format!("frame {}: {} of work; median frame {}", frame(worst), fmt::ms(work[worst]), fmt::ms(typical)), format!("frames at {} or more: {}", fmt::ms(floor), list(&spikes))];
                    // (a frame's figures are the whole frame's: the harness's code is in them too)
                    if let Some((_, harness)) = harness_work(p) {
                        evidence.push(format!("a frame's work counts the harness's code too: {} of self work in {}", fmt::ms(harness), p.scope().label));
                    }
                    evidence
                },
                advice: "Spread the work over frames, or do it ahead".into(),
                target: Target::Frame(worst),
                weight: work[worst],
            });
        }
    }
}

/// Instances, connections or threads that keep growing through the steady state
fn growth(p: &Profile, steady: Option<(u32, u64)>, steady_frames: &str, issues: &mut Vec<Issue>) {
    let (Some(timeline), Some((phase, frames))) = (&p.raw.timeline, steady) else { return };
    if frames < 10 {
        return;
    }
    let phases = timeline.values("phase").unwrap_or_default();
    let rows: Vec<usize> = (0..phases.len()).filter(|&i| phases[i] as u32 == phase).collect();
    let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else { return };
    // (the instances a frame keeps: those in the DataModel and those made outside it; a profile before they were apart has one
    // column. Threads made a frame are a flow, not a count held: their growth is the heap snapshots' below)
    let instances = timeline.values("instances").or_else(|| {
        let (inside, outside) = (timeline.values("instances_in_datamodel")?, timeline.values("instances_outside").unwrap_or_default());
        Some(inside.iter().enumerate().map(|(row, n)| n + outside.get(row).copied().unwrap_or(0.0)).collect())
    });
    thread_growth(p, steady_frames, issues);
    for what in ["connections", "instances", "threads"] {
        let values = match what {
            "instances" => instances.clone(),
            _ => timeline.values(what),
        };
        let Some(values) = values else { continue };
        let delta = values[last] - values[first];
        let per_frame = delta / (rows.len().max(2) - 1) as f64;
        if delta < 20.0 || per_frame < 0.5 {
            continue;
        }
        // (growth in most frames, not one step: a leak keeps adding)
        let rising = rows.windows(2).filter(|pair| values[pair[1]] > values[pair[0]]).count();
        if rising * 4 < rows.len() {
            continue;
        }
        // (a thread counts from when it is made to when the collector frees it: with the last heap snapshot, the threads a
        // full collection keeps say whether the count is held or garbage the collector had not swept yet)
        let reachable = (what == "threads").then(|| reachable_threads(p)).flatten();
        if reachable.is_some_and(|kept| kept * 2.0 < values[last]) {
            continue;
        }
        let kept = reachable.map(|kept| format!("; {} reachable at the end, what a full collection keeps", fmt::grouped(kept))).unwrap_or_default();
        issues.push(Issue {
            severity: if per_frame >= 10.0 { Severity::Serious } else { Severity::Warning },
            kind: "growth",
            title: format!("{} keep growing: +{} over {steady_frames}", capitalize(what), fmt::grouped(delta)),
            evidence: vec![format!(
                "{} at frame {} → {} at frame {}: {per_frame:+.1} a frame, up in {rising} of {} frames",
                fmt::grouped(values[first]),
                timeline.value(first, "frame").unwrap_or(0.0),
                fmt::grouped(values[last]),
                timeline.value(last, "frame").unwrap_or(0.0),
                rows.len() - 1
            ) + &kept],
            advice: format!("Frames add {what} that nothing removes: disconnect or destroy what a frame makes"),
            target: Target::Frame(last),
            weight: per_frame,
        });
    }
}

/// Threads that keep growing, when the timeline counts the threads made a frame (a flow): the threads the heap snapshots reach,
/// at the first snapshot past startup and at the last, what a full collection keeps
fn thread_growth(p: &Profile, steady_frames: &str, issues: &mut Vec<Issue>) {
    if p.raw.timeline.as_ref().is_some_and(|timeline| timeline.index("threads").is_some()) {
        return;
    }
    let Some(heap) = &p.raw.heap else { return };
    let threads = |snapshot: &crate::model::Snapshot| -> Option<f64> {
        snapshot.reachable.as_ref()?;
        Some(snapshot.by_type.iter().filter(|row| row.ty == "thread").map(|row| row.count).sum())
    };
    let snapshots: Vec<(&crate::model::Snapshot, f64)> = heap.snapshots.iter().skip(1).filter_map(|snapshot| Some((snapshot, threads(snapshot)?))).collect();
    let (Some(&(first, from)), Some(&(last, to))) = (snapshots.first(), snapshots.last()) else { return };
    let frames = last.frame.saturating_sub(first.frame).max(1) as f64;
    let (delta, per_frame) = (to - from, (to - from) / frames);
    if snapshots.len() < 2 || delta < 20.0 || per_frame < 0.5 {
        return;
    }
    issues.push(Issue {
        severity: if per_frame >= 10.0 { Severity::Serious } else { Severity::Warning },
        kind: "growth",
        title: format!("Threads keep growing: +{} over {steady_frames}", fmt::grouped(delta)),
        evidence: vec![format!(
            "{} threads reachable at frame {} → {} at frame {}: {per_frame:+.1} a frame (what a full collection keeps, at the heap snapshots)",
            fmt::grouped(from),
            first.frame,
            fmt::grouped(to),
            last.frame
        )],
        advice: "Frames make threads that something keeps: a coroutine or task held in a table, a connection whose handler waits".into(),
        target: Target::None,
        weight: per_frame,
    });
}

/// `1 frame`, `4 frames`
fn frames_text(count: usize) -> String {
    if count == 1 { "1 frame".into() } else { format!("{count} frames") }
}

/// The self work of the game's code and the harness's in the profile's scope, when some of its code is the harness's
fn harness_work(p: &Profile) -> Option<(f64, f64)> {
    if !p.has_harness() {
        return None;
    }
    p.scope_work().filter(|(_, harness)| *harness > 0.0)
}

/// The threads the last heap snapshot found reachable
fn reachable_threads(p: &Profile) -> Option<f64> {
    let snapshot = p.raw.heap.as_ref()?.snapshots.last()?;
    snapshot.reachable?;
    Some(snapshot.by_type.iter().filter(|row| row.ty == "thread").map(|row| row.count).sum())
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or(String::new(), |first| first.to_uppercase().collect::<String>() + chars.as_str())
}

/// The reachable heap growing from the first snapshot past startup to the last
fn heap_growth(p: &Profile, issues: &mut Vec<Issue>) {
    let Some(heap) = &p.raw.heap else { return };
    let snapshots = &heap.snapshots;
    if snapshots.len() < 3 {
        return;
    }
    // (from the first mark after the start, so the mount's own memory is not counted as growth)
    let (from, to) = (1, snapshots.len() - 1);
    let (before, _, _) = snapshots[from].split();
    let (after, _, _) = snapshots[to].split();
    let delta = after - before;
    if delta < 256.0 * 1024.0 || delta < before * 0.25 {
        return;
    }
    issues.push(Issue {
        severity: if delta >= 8.0 * 1024.0 * 1024.0 { Severity::Serious } else { Severity::Warning },
        kind: "heap growth",
        title: format!("Reachable heap grows: +{} from {} to {}", fmt::bytes(delta), snapshots[from].label, snapshots[to].label),
        evidence: vec![format!("{} at {} (frame {}) → {} at {} (frame {})", fmt::bytes(before), snapshots[from].label, snapshots[from].frame, fmt::bytes(after), snapshots[to].label, snapshots[to].frame)],
        advice: "Heap's biggest holders say what keeps it".into(),
        target: Target::Snapshot(to),
        weight: delta,
    });
}

/// Luau inline cache sites that miss often
fn cache_misses(p: &Profile, harness_out: bool, issues: &mut Vec<Issue>) {
    let Some(caches) = &p.raw.luau_caches else { return };
    // (the counts cover the reported phases' frames in a version 2 profile, the run's before)
    let frames = caches.frames.unwrap_or(p.raw.run.frames as f64).max(1.0);
    let all: f64 = caches.sites.iter().map(|site| site.missed()).sum();
    let mut rows: Vec<(usize, f64, f64)> = caches.sites.iter().enumerate().map(|(index, site)| (index, site.missed(), site.lookups())).filter(|(_, missed, lookups)| *missed >= 1000.0 && *missed >= 0.2 * *lookups).collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (index, missed, lookups) in rows.into_iter().take(4) {
        let site = &caches.sites[index];
        let Some(func) = p.func_of_id(site.function) else { continue };
        let f = &p.funcs[func];
        if f.kind == Kind::Engine || (harness_out && f.harness) {
            continue;
        }
        // what the cached slot did instead: each outcome as the interpreter ran it (version 2), with its advice, the commonest's
        let commonest = site.commonest_miss();
        let advice = match commonest {
            Some((outcome, _)) => caches.outcome_info.get(outcome).map(|info| info.advice.clone()).filter(|advice| !advice.is_empty()).unwrap_or_else(|| outcome_advice(outcome).to_string()),
            None => legacy_advice(site).to_string(),
        };
        let told: Vec<String> = if site.outcomes.is_empty() {
            vec![format!("{} predicted wrong", fmt::grouped(site.misses)), format!("{} slow", fmt::grouped(site.slow))]
        } else {
            site.outcomes.iter().filter(|(name, n)| name.as_str() != "hit" && name.as_str() != "not_table" && **n > 0.0).map(|(name, n)| format!("{} {}", name.replace('_', " "), fmt::grouped(*n))).collect()
        };
        let mut evidence = vec![format!(
            "{} of {} lookups not served by the slot ({}), {} a frame; {} of all",
            fmt::grouped(missed),
            fmt::grouped(lookups),
            told.join(", "),
            fmt::grouped((missed / frames).round()),
            fmt::share(missed, all)
        )];
        if site.charged_work_us_per_frame > 0.0 || site.extra_work_us_per_frame > 0.0 || site.within_noise > 0.0 {
            // (the charge is whatever the clock charged the instruction past its dispatch, a table's cold first touch in the frame
            // among it; the extra leaves out the outcomes whose prices are within the noise)
            let noise = if site.within_noise > 0.0 {
                let a_frame = site.within_noise / frames;
                let a_frame = if a_frame >= 1.0 { format!(", {} a frame", fmt::grouped(a_frame.round())) } else { String::new() };
                format!(", leaving out {} outcomes priced within the noise{a_frame}", fmt::grouped(site.within_noise))
            } else {
                String::new()
            };
            evidence.push(format!(
                "the work clock charged {:.2} µs a frame there (in the function's work; whatever it charged the instruction past its dispatch, a table's cold first touch in the frame among it); the outcomes it does not charge cost {:.2} µs a frame more at the committed prices{noise}",
                site.charged_work_us_per_frame, site.extra_work_us_per_frame
            ));
        }
        issues.push(Issue {
            severity: if missed / frames >= 1000.0 { Severity::Serious } else { Severity::Warning },
            kind: "cache misses",
            title: format!("{} at {}:{} misses {} of its lookups", site.op, f.short, site.line, fmt::share(missed, lookups)),
            evidence,
            advice,
            target: Target::Cache(index),
            weight: missed,
        });
    }
}

/// What helps a cache outcome, when the profile gives none (ree's report.rs OUTCOME_INFO says the same)
fn outcome_advice(outcome: &str) -> &'static str {
    match outcome {
        "repredicted" => "The tables here differ in shape (keys added in other orders, or other keys): build them alike",
        "absent" => "The key is missing: give the table the field (nil-able fields read faster once present), or test for it once",
        "inserted" => "A new key each time: create the table with its fields, in one constructor",
        "rehashed" => "The table grows a key at a time and rehashes: build it with its keys at once (a constructor, table.create)",
        "metatable_table" | "metatable_function" => "A metatable serves the lookup (__index): a plain field, or a local copy of the method, avoids the walk",
        "unsafe_env" => "The script's environment is unsafe (it calls getfenv or setfenv): drop those calls",
        _ => "The cache could not serve the lookup from its slot",
    }
}

/// What helps a site of a profile before the outcomes (its op and its counts)
fn legacy_advice(site: &crate::model::CacheSite) -> &'static str {
    let slow_mostly = site.slow > site.misses;
    match site.op.as_str() {
        op if slow_mostly && op.starts_with("FASTCALL") => "The script's environment is unsafe (it calls getfenv or setfenv), so no builtin takes its fast path: drop those calls",
        _ if slow_mostly => "A metatable serves the lookup (__index, __newindex), or the environment is unsafe: a plain field avoids it",
        op if op.starts_with("FASTCALL") => "The builtin's fast path turned these arguments down (a type, or a count, it does not take there: string.sub without its end, say), so each call went the full way: pass what the fast path takes",
        "NAMECALL" => "The method is not where the cache looked: objects of several shapes or classes meet here, or the method sits further up an __index chain",
        "GETGLOBAL" | "SETGLOBAL" => "A global the cache could not keep its slot for: read it into a local once",
        "GETIMPORT" => "The import was not resolved when the script loaded: a global the script sets, or one missing then",
        _ => "The tables here differ in shape (keys added in other orders, or other keys): build them alike",
    }
}

/// Whether a boundary site is in the harness's code (its script under the harness's folder)
fn harness_site(p: &Profile, site: &crate::model::BoundarySite) -> bool {
    crate::model::harness_module(crate::model::module_name(&site.script)) || boundary_func(p, &site.script, site.line).is_some_and(|func| p.funcs[func as usize].harness)
}

/// The function a boundary site names (its script and line)
fn boundary_func(p: &Profile, script: &str, line: i64) -> Option<u32> {
    let short = crate::model::short_module(crate::model::module_name(script));
    p.funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == Kind::Script && (f.module == script || f.short == short) && f.line <= line && line <= f.last_line.max(f.line))
        .max_by_key(|(_, f)| f.line)
        .map(|(index, _)| index as u32)
}

/// The boundary profile's repeat table and unchanged writes
fn repeat_reads(p: &Profile, boundary: &crate::model::Boundary, harness_out: bool, issues: &mut Vec<Issue>) {
    let frames = boundary.frames.max(1.0);
    let mut rows: Vec<&crate::model::BoundarySite> = boundary.repeat_reads.iter().filter(|site| site.repeat_reads / frames >= 10.0 && !(harness_out && harness_site(p, site))).collect();
    rows.sort_by(|a, b| b.repeat_reads.total_cmp(&a.repeat_reads));
    for site in rows.into_iter().take(5) {
        let per_frame = site.repeat_reads / frames;
        let cost_per_read = if site.per_s > 0.0 { site.cost_ms_per_s / site.per_s } else { 0.0 };
        let func = boundary_func(p, &site.script, site.line);
        issues.push(Issue {
            severity: if per_frame >= 200.0 { Severity::Serious } else { Severity::Warning },
            kind: "repeat reads",
            title: format!("{} read again in the frame: {per_frame:.0} a frame at {}:{}", site.member, crate::model::short_module(&site.script), site.line),
            evidence: vec![
                format!("{} repeat reads in {} frames, of {} reads: the same instance's {} read again by the same function", fmt::grouped(site.repeat_reads), fmt::grouped(frames), fmt::grouped(site.count), site.member),
                format!("{} a read: {} a frame", fmt::ms(cost_per_read), fmt::ms(cost_per_read * per_frame)),
            ],
            advice: "Read it once into a local".into(),
            target: func.map_or(Target::None, |f| Target::Line(f, site.line)),
            weight: site.repeat_reads,
        });
    }
    let mut writes: Vec<&crate::model::BoundarySite> = boundary.by_site_count.iter().filter(|site| site.unchanged_writes / frames >= 10.0 && !(harness_out && harness_site(p, site))).collect();
    writes.sort_by(|a, b| b.unchanged_writes.total_cmp(&a.unchanged_writes));
    for site in writes.into_iter().take(3) {
        let func = boundary_func(p, &site.script, site.line);
        issues.push(Issue {
            severity: Severity::Warning,
            kind: "unchanged writes",
            title: format!("{} set to its own value: {:.0} a frame at {}:{}", site.member, site.unchanged_writes / frames, crate::model::short_module(&site.script), site.line),
            evidence: vec![format!("{} of {} writes in {} frames changed nothing", fmt::grouped(site.unchanged_writes), fmt::grouped(site.count), fmt::grouped(frames))],
            advice: "Write only when the value changes".into(),
            target: func.map_or(Target::None, |f| Target::Line(f, site.line)),
            weight: site.unchanged_writes,
        });
    }
}

/// Functions whose own cycles went mostly to waiting for load data, from the hardware counters read around their sampled
/// calls: a function holding at least 5 % of the run's stall cycles, with at least 30 % of its own cycles stalled
fn memory_waits(p: &Profile, c: &crate::model::Counters, harness_out: bool, issues: &mut Vec<Issue>) {
    let index = |name: &str| c.names.iter().position(|known| known == name);
    let (Some(stalls), Some(cycles)) = (index("load-stall-cycles"), index("cycles")) else { return };
    let (occupancy, allocs) = (index("miss-buffer-occupancy"), index("miss-buffer-allocs"));
    let run_cycles = c.run.counts.get("cycles").copied().unwrap_or(0.0);
    let run_stalls = c.run.counts.get("load-stall-cycles").copied().unwrap_or(0.0);
    if run_cycles <= 0.0 || run_stalls <= 0.0 {
        return;
    }
    let mut rows: Vec<(usize, f64, f64, Vec<f64>)> = p
        .counter_funcs
        .iter()
        .filter(|(func, _)| p.funcs[**func].kind == Kind::Script && !(harness_out && p.funcs[**func].harness))
        .filter_map(|(&func, (own, _))| {
            let (stalled, spent) = (own.get(stalls).copied().unwrap_or(0.0), own.get(cycles).copied().unwrap_or(0.0));
            (stalled >= 0.05 * run_stalls && spent > 0.0 && stalled / spent >= 0.3).then(|| (func, stalled, spent, own.clone()))
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let how = if c.by_calls() { "read around its sampled calls (its own, its callees' apart)" } else { "booked from the 1 ms samples after it" };
    for (func, stalled, spent, own) in rows.into_iter().take(4) {
        let of_run = stalled / run_cycles;
        let mut evidence = vec![format!(
            "{} of its own {} cycles stalled waiting for load data ({}), {} of the run thread's cycles; {how}",
            fmt::compact(stalled),
            fmt::compact(spent),
            fmt::share(stalled, spent),
            fmt::share(stalled, run_cycles)
        )];
        if let (Some(occupancy), Some(allocs)) = (occupancy, allocs) {
            let (inflight, allocated) = (own.get(occupancy).copied().unwrap_or(0.0), own.get(allocs).copied().unwrap_or(0.0));
            if allocated > 0.0 && spent > 0.0 {
                evidence.push(format!("its L1 data cache misses stayed outstanding {:.0} cycles on average, {:.2} outstanding on average over its cycles", inflight / allocated, inflight / spent));
            }
        }
        evidence.push("the counters measure ree's own process running the Luau: where memory costs, not a client's frame time (the work clock's)".into());
        let f = &p.funcs[func];
        issues.push(Issue {
            severity: if of_run >= 0.10 {
                Severity::Serious
            } else if of_run >= 0.03 {
                Severity::Warning
            } else {
                Severity::Info
            },
            kind: "memory waits",
            title: format!("Waits on memory: {} ({} of its cycles stalled on loads)", f.label(), fmt::share(stalled, spent)),
            evidence,
            advice: "Its loads wait for data from beyond the L1: fewer cache lines touched, or the same lines touched again while they are near, would shorten the wait".into(),
            target: Target::Func(func as u32),
            weight: stalled,
        });
    }
}

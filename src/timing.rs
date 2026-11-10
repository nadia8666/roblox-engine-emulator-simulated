//! Where a command's wall time goes (`ree record --profile`, `ree run --profile`): named phases and costs summed over the run
//! (module compiles, source reads, bundle loads, frames), printed on stderr at the end as a tree: a phase timed inside another
//! is its child (the recorder's first walk inside the harness's start), so each phase's own time (self) is its time less its
//! children's and the tree's times add up to the command's, where a flat list counted a nested phase twice. Off unless a
//! command begins it: every entry point is then one thread-local read.

use std::{borrow::Cow, cell::RefCell, time::Instant};

/// A timed stretch: its name, when it began and ended (seconds since the profile began)
struct Span {
    name: Cow<'static, str>,
    start: f64,
    end: f64,
}

struct Profile {
    started: Instant,
    spans: Vec<Span>,
    /// Counts with no time (a tally), by name in first-seen order
    tallies: Vec<(Cow<'static, str>, u64)>,
}

thread_local! {
    static PROFILE: RefCell<Option<Profile>> = const { RefCell::new(None) };
}

/// Starts profiling this thread's command; `started` is when the process began its work.
pub fn begin(started: Instant) {
    PROFILE.with(|profile| *profile.borrow_mut() = Some(Profile { started, spans: Vec::new(), tallies: Vec::new() }));
}

pub fn enabled() -> bool {
    PROFILE.with(|profile| profile.borrow().is_some())
}

/// Adds `seconds` (one occurrence) to `name`: a stretch that ended now.
pub fn add(name: impl Into<Cow<'static, str>>, seconds: f64) {
    PROFILE.with(|profile| {
        let Ok(mut profile) = profile.try_borrow_mut() else { return };
        let Some(profile) = profile.as_mut() else { return };
        let end = profile.started.elapsed().as_secs_f64();
        profile.spans.push(Span { name: name.into(), start: (end - seconds).max(0.0), end });
    });
}

/// Adds `count` occurrences (no time) to `name`: a tally the profile prints with the phases.
pub fn tally(name: impl Into<Cow<'static, str>>, count: u64) {
    PROFILE.with(|profile| {
        let Ok(mut profile) = profile.try_borrow_mut() else { return };
        let Some(profile) = profile.as_mut() else { return };
        let name = name.into();
        match profile.tallies.iter_mut().find(|entry| entry.0 == name) {
            Some(entry) => entry.1 += count,
            None => profile.tallies.push((name, count)),
        }
    });
}

/// Runs `f`, adding its wall time to `name` when profiling.
pub fn time<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let result = f();
    add(name, start.elapsed().as_secs_f64());
    result
}

/// A timer started now when profiling ([`Timer::stop`] adds it).
pub struct Timer(Option<(&'static str, Instant)>);

pub fn start(name: &'static str) -> Timer {
    Timer(enabled().then(|| (name, Instant::now())))
}

impl Timer {
    pub fn stop(self) {
        if let Some((name, start)) = self.0 {
            add(name, start.elapsed().as_secs_f64());
        }
    }
}

/// A phase of the tree: its name, its parent (an index into the same list; none for the command itself), its time, its own
/// time without its children's, and how many stretches it adds up
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub seconds: f64,
    pub own: f64,
    pub count: u64,
}

/// The phases as a tree, the command first (its own time is what no phase covered): a stretch inside another (it began at or
/// after the other's start, before its end, and ended at or before its end) is its child; one that begins as the other ends, a
/// stretch of no length too, is not; stretches of one name under one parent add up
fn tree(total: f64, spans: &[Span]) -> Vec<Node> {
    let mut nodes = vec![Node { name: "command".into(), parent: None, seconds: total, own: total, count: 1 }];
    let mut order: Vec<usize> = (0..spans.len()).collect();
    // (the outer of two stretches that begin together is the longer: it comes first)
    order.sort_by(|&a, &b| spans[a].start.total_cmp(&spans[b].start).then(spans[b].end.total_cmp(&spans[a].end)).then(a.cmp(&b)));
    // the open stretches: (their end, their node)
    let mut open: Vec<(f64, usize)> = Vec::new();
    // (a little slack: the timers' clocks are read a moment apart)
    const SLACK: f64 = 1e-6;
    for index in order {
        let span = &spans[index];
        while open.last().is_some_and(|&(end, _)| span.start >= end || span.end > end + SLACK) {
            open.pop();
        }
        let parent = open.last().map_or(0, |&(_, node)| node);
        let seconds = span.end - span.start;
        let node = match nodes.iter().position(|node| node.parent == Some(parent) && node.name == span.name) {
            Some(node) => node,
            None => {
                nodes.push(Node { name: span.name.to_string(), parent: Some(parent), seconds: 0.0, own: 0.0, count: 0 });
                nodes.len() - 1
            }
        };
        nodes[node].seconds += seconds;
        nodes[node].count += 1;
        open.push((span.end, node));
    }
    // own: a node's time less its children's
    for node in nodes.iter_mut() {
        node.own = node.seconds;
    }
    for node in 1..nodes.len() {
        if let Some(parent) = nodes[node].parent {
            nodes[parent].own -= nodes[node].seconds;
        }
    }
    nodes
}

/// The profile so far, ended: the tree as text (total and self milliseconds, the stretches a phase adds up, the tallies) and
/// as nodes
pub fn finish_tree() -> Option<(String, Vec<Node>)> {
    let profile = PROFILE.with(|profile| profile.borrow_mut().take())?;
    let total = profile.started.elapsed().as_secs_f64();
    let nodes = tree(total, &profile.spans);
    let mut lines = vec![format!("phases: where the command's {:.1} ms of wall time went (self: a phase's own, without the phases inside it)", total * 1000.0)];
    lines.push(format!("  {:>10} {:>10} {:>7}  phase", "total ms", "self ms", "count"));
    // depth first, in the order the phases first ran
    fn walk(nodes: &[Node], node: usize, depth: usize, lines: &mut Vec<String>) {
        let entry = &nodes[node];
        let name = if node == 0 { "(the command; self: what no phase covers)".to_string() } else { entry.name.clone() };
        lines.push(format!("  {:>10.1} {:>10.1} {:>7}  {}{name}", entry.seconds * 1000.0, entry.own * 1000.0, entry.count, "  ".repeat(depth)));
        for child in (0..nodes.len()).filter(|&child| nodes[child].parent == Some(node)) {
            walk(nodes, child, depth + 1, lines);
        }
    }
    walk(&nodes, 0, 0, &mut lines);
    for (name, count) in &profile.tallies {
        lines.push(format!("  {:>10} {:>10} {:>7}  {name}", "", "", count));
    }
    Some((lines.join("\n"), nodes))
}

/// [`finish_tree`]'s text
pub fn finish() -> Option<String> {
    finish_tree().map(|(text, _)| text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(name: &'static str, start: f64, end: f64) -> Span {
        Span { name: name.into(), start, end }
    }

    /// A phase timed inside another is its child, its time counted once: the harness's start holds the recorder's first walk,
    /// and the tree's own times add up to the command's
    #[test]
    fn phases_nest_and_their_own_times_add_up() {
        let spans = vec![span("harness start", 0.1, 0.4), span("recorder: first walk", 0.15, 0.35), span("frames", 0.4, 0.9), span("frames: sample", 0.5, 0.52), span("frames: sample", 0.6, 0.63), span("mount: x", 0.0, 0.05)];
        let nodes = tree(1.0, &spans);
        let find = |name: &str| nodes.iter().position(|node| node.name == name).unwrap();
        let (start, walk, frames, sample) = (find("harness start"), find("recorder: first walk"), find("frames"), find("frames: sample"));
        assert_eq!(nodes[walk].parent, Some(start));
        assert_eq!(nodes[sample].parent, Some(frames));
        assert_eq!(nodes[sample].count, 2);
        assert!((nodes[start].own - 0.1).abs() < 1e-9, "{:?}", nodes[start]);
        assert!((nodes[frames].own - 0.45).abs() < 1e-9, "{:?}", nodes[frames]);
        let own: f64 = nodes.iter().map(|node| node.own).sum();
        assert!((own - 1.0).abs() < 1e-9, "the own times add up to the command's: {own}");
        assert!((nodes[0].own - (1.0 - 0.3 - 0.5 - 0.05)).abs() < 1e-9, "{:?}", nodes[0]);
    }

    /// A stretch of no length timed right after another ended (within the clocks' slack) is not inside it: it is the command's,
    /// as one that begins exactly at another's end is
    #[test]
    fn a_phase_of_no_length_after_another_is_not_its_child() {
        let spans = vec![span("load", 0.0, 0.5), span("mark", 0.5000005, 0.5000005), span("next", 0.5, 0.5), span("inner", 0.1, 0.1)];
        let nodes = tree(1.0, &spans);
        let find = |name: &str| nodes.iter().position(|node| node.name == name).unwrap();
        assert_eq!(nodes[find("mark")].parent, Some(0), "{nodes:?}");
        assert_eq!(nodes[find("next")].parent, Some(0), "{nodes:?}");
        assert_eq!(nodes[find("inner")].parent, Some(find("load")), "{nodes:?}");
    }
}

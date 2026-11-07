//! The call tree a metric's flame graph and callers come from: the profile's calling-context tree for work and
//! instructions (with call counts), the folded stacks for host time, allocations and the hardware counters. Siblings
//! that are the same function are merged; with `scripts_only`, C functions, the engine's own Luau and its steps fold
//! into the script function that called them (their cost becomes its own).

use rustc_hash::FxHashMap;

use crate::{metric::Metric, model::Profile};

/// What a node is
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Frame {
    /// The run: every phase below it
    Root,
    /// A profile phase (`startup`, `frames`, a `Context.ProfilePhase` mark)
    Phase(u32),
    /// A function, by its index in `Profile::funcs`
    Func(u32),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: u32,
    pub frame: Frame,
    pub own: f64,
    pub total: f64,
    pub calls: u64,
    pub children: Vec<u32>,
    pub depth: u16,
}

/// A function's figures in a tree: own, total (recursion counted once) and calls
#[derive(Clone, Copy, Debug, Default)]
pub struct FuncStat {
    pub own: f64,
    pub total: f64,
    pub calls: u64,
}

/// A caller-callee pair: what the callee cost under that caller, and how often it was called there
#[derive(Clone, Copy, Debug, Default)]
pub struct EdgeStat {
    pub total: f64,
    pub calls: u64,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    /// The metric the values are (work's when the asked one has no stacks)
    pub metric: Metric,
    pub scripts_only: bool,
    /// Whether the nodes carry call counts (the calling-context tree does; folded stacks do not)
    pub has_calls: bool,
    pub funcs: FxHashMap<u32, FuncStat>,
    /// (caller, callee) where the caller is a function or a phase
    pub edges: FxHashMap<(Frame, u32), EdgeStat>,
}

pub const ROOT: u32 = 0;

struct Builder {
    nodes: Vec<Node>,
    index: FxHashMap<(u32, Frame), u32>,
}

impl Builder {
    fn new() -> Builder {
        Builder { nodes: vec![Node { parent: u32::MAX, frame: Frame::Root, own: 0.0, total: 0.0, calls: 0, children: Vec::new(), depth: 0 }], index: FxHashMap::default() }
    }

    fn child(&mut self, parent: u32, frame: Frame) -> u32 {
        if let Some(&child) = self.index.get(&(parent, frame)) {
            return child;
        }
        let child = self.nodes.len() as u32;
        let depth = self.nodes[parent as usize].depth + 1;
        self.nodes.push(Node { parent, frame, own: 0.0, total: 0.0, calls: 0, children: Vec::new(), depth });
        self.nodes[parent as usize].children.push(child);
        self.index.insert((parent, frame), child);
        child
    }
}

impl Tree {
    pub fn build(p: &Profile, metric: Metric, scripts_only: bool) -> Tree {
        let source = metric.stacks(p);
        let keep = |func: u32| !scripts_only || p.funcs.get(func as usize).is_some_and(|f| f.is_script());
        let mut b = Builder::new();
        let has_calls;
        match source.folded(p).and_then(|name| p.folded.get(&name)) {
            Some(folded) => {
                has_calls = false;
                for row in &folded.rows {
                    let mut at = match row.phase {
                        Some(phase) => b.child(ROOT, Frame::Phase(phase)),
                        None => ROOT,
                    };
                    for &func in &row.frames {
                        if keep(func) {
                            at = b.child(at, Frame::Func(func));
                        }
                    }
                    b.nodes[at as usize].own += row.weight;
                }
            }
            None => {
                has_calls = true;
                // the calling-context tree: node 0 is the run's start (the first phase), a node under it with function
                // -1 a later phase's root; the rest are functions (ids >= 0 Luau, below -1 C functions and steps)
                let raw = &p.raw.tree;
                let mut map: Vec<u32> = vec![ROOT; raw.len()];
                for (index, node) in raw.iter().enumerate() {
                    let value = if source == Metric::Insns { node.4 } else { node.3 / 1000.0 };
                    let at = if index == 0 {
                        let phase = node.5.as_deref().and_then(|label| p.raw.phases.iter().position(|phase| phase == label)).unwrap_or(0);
                        b.child(ROOT, Frame::Phase(phase as u32))
                    } else if node.1 == -1 {
                        let phase = node.5.as_deref().and_then(|label| p.raw.phases.iter().position(|phase| phase == label)).unwrap_or(0);
                        b.child(ROOT, Frame::Phase(phase as u32))
                    } else {
                        let parent = if node.0 >= 0 && (node.0 as usize) < index { map[node.0 as usize] } else { ROOT };
                        match p.func_of_id(node.1) {
                            Some(func) if keep(func as u32) => {
                                let at = b.child(parent, Frame::Func(func as u32));
                                b.nodes[at as usize].calls += node.2;
                                at
                            }
                            _ => parent,
                        }
                    };
                    map[index] = at;
                    b.nodes[at as usize].own += value;
                }
            }
        }
        let mut nodes = b.nodes;
        // totals bottom-up (children come after their parents)
        for index in (0..nodes.len()).rev() {
            let total = nodes[index].own + nodes[index].children.iter().map(|&child| nodes[child as usize].total).sum::<f64>();
            nodes[index].total = total;
        }
        // children: phases in their order, functions by total (ties by their place in the profile)
        for index in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[index].children);
            children.sort_by(|&a, &b| {
                let (na, nb) = (&nodes[a as usize], &nodes[b as usize]);
                match (na.frame, nb.frame) {
                    (Frame::Phase(x), Frame::Phase(y)) => x.cmp(&y),
                    (Frame::Phase(_), _) => std::cmp::Ordering::Less,
                    (_, Frame::Phase(_)) => std::cmp::Ordering::Greater,
                    _ => nb.total.total_cmp(&na.total).then(na.frame.cmp(&nb.frame)),
                }
            });
            nodes[index].children = children;
        }
        let mut tree = Tree { nodes, metric: source, scripts_only, has_calls, funcs: FxHashMap::default(), edges: FxHashMap::default() };
        tree.index();
        tree
    }

    /// Per-function figures and caller-callee edges (a depth-first walk: a function's total counts its outermost calls
    /// on each path only)
    fn index(&mut self) {
        let mut funcs: FxHashMap<u32, FuncStat> = FxHashMap::default();
        let mut edges: FxHashMap<(Frame, u32), EdgeStat> = FxHashMap::default();
        let mut on_path: FxHashMap<u32, u32> = FxHashMap::default();
        // (node, caller frame, entering)
        let mut stack: Vec<(u32, Frame, bool)> = vec![(ROOT, Frame::Root, true)];
        while let Some((index, caller, entering)) = stack.pop() {
            let node = &self.nodes[index as usize];
            let Frame::Func(func) = node.frame else {
                if entering {
                    for &child in node.children.iter().rev() {
                        stack.push((child, node.frame, true));
                    }
                }
                continue;
            };
            if !entering {
                if let Some(count) = on_path.get_mut(&func) {
                    *count -= 1;
                }
                continue;
            }
            let stat = funcs.entry(func).or_default();
            stat.own += node.own;
            stat.calls += node.calls;
            let depth = on_path.entry(func).or_insert(0);
            if *depth == 0 {
                stat.total += node.total;
            }
            *depth += 1;
            let edge = edges.entry((caller, func)).or_default();
            edge.total += node.total;
            edge.calls += node.calls;
            stack.push((index, caller, false));
            for &child in node.children.iter().rev() {
                stack.push((child, Frame::Func(func), true));
            }
        }
        self.funcs = funcs;
        self.edges = edges;
    }

    pub fn total(&self) -> f64 {
        self.nodes[ROOT as usize].total
    }

    pub fn node(&self, index: u32) -> &Node {
        &self.nodes[index as usize]
    }

    /// A node's path from the root (the root first)
    pub fn path(&self, mut index: u32) -> Vec<u32> {
        let mut path = vec![index];
        while self.nodes[index as usize].parent != u32::MAX {
            index = self.nodes[index as usize].parent;
            path.push(index);
        }
        path.reverse();
        path
    }

    /// The heaviest node of a function (where a jump to it from another view lands)
    pub fn heaviest(&self, func: u32) -> Option<u32> {
        let mut best: Option<u32> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            if node.frame == Frame::Func(func) && best.is_none_or(|b| node.total > self.nodes[b as usize].total) {
                best = Some(index as u32);
            }
        }
        best
    }

    /// Nodes in depth-first order (search walks them in this order)
    pub fn preorder(&self, from: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut stack = vec![from];
        while let Some(index) = stack.pop() {
            out.push(index);
            for &child in self.nodes[index as usize].children.iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    /// The callers of a function, heaviest first
    pub fn callers(&self, func: u32) -> Vec<(Frame, EdgeStat)> {
        let mut rows: Vec<(Frame, EdgeStat)> = self.edges.iter().filter(|((_, callee), _)| *callee == func).map(|(&(caller, _), &stat)| (caller, stat)).collect();
        rows.sort_by(|a, b| b.1.total.total_cmp(&a.1.total).then(a.0.cmp(&b.0)));
        rows
    }

    /// The callees of a function, heaviest first
    pub fn callees(&self, func: u32) -> Vec<(u32, EdgeStat)> {
        let mut rows: Vec<(u32, EdgeStat)> = self.edges.iter().filter(|((caller, _), _)| *caller == Frame::Func(func)).map(|(&(_, callee), &stat)| (callee, stat)).collect();
        rows.sort_by(|a, b| b.1.total.total_cmp(&a.1.total).then(a.0.cmp(&b.0)));
        rows
    }

    /// Functions by own figure, heaviest first
    /// Each function's own figure on each side, the game's (0) and the harness's (1), in the phases a scope covers: a
    /// node is on its nearest script function's side (`side` names a script function's, None for the rest), so a C
    /// function, the engine's Luau and mlua's helpers go with their caller, as ree's report puts them
    pub fn sided(&self, scope: &crate::model::Scope, side: impl Fn(u32) -> Option<bool>) -> [FxHashMap<u32, f64>; 2] {
        let mut out: [FxHashMap<u32, f64>; 2] = [FxHashMap::default(), FxHashMap::default()];
        // (node, the side it inherits)
        let mut stack: Vec<(u32, bool)> = Vec::new();
        for &child in &self.node(ROOT).children {
            match self.node(child).frame {
                Frame::Phase(phase) if !scope.covers(phase) => {}
                _ => stack.push((child, false)),
            }
        }
        while let Some((index, inherited)) = stack.pop() {
            let node = self.node(index);
            let on = match node.frame {
                Frame::Func(func) => {
                    let on = side(func).unwrap_or(inherited);
                    *out[usize::from(on)].entry(func).or_default() += node.own;
                    on
                }
                _ => inherited,
            };
            for &child in &node.children {
                stack.push((child, on));
            }
        }
        out
    }

    pub fn ranked(&self) -> Vec<(u32, FuncStat)> {
        let mut rows: Vec<(u32, FuncStat)> = self.funcs.iter().map(|(&func, &stat)| (func, stat)).collect();
        rows.sort_by(|a, b| b.1.own.total_cmp(&a.1.own).then(b.1.total.total_cmp(&a.1.total)).then(a.0.cmp(&b.0)));
        rows
    }
}

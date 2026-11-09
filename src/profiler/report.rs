//! A profile's files (profiler/mod.rs): `profile.json` (everything, for tools and `ree profile diff`), `summary.txt`
//! (the short text an agent reads; also printed), folded stacks for flame graph tools (`work.folded`,
//! `instructions.folded`, `time.folded`, `alloc.folded`, `alloc.counts.folded`, `heap.folded`), `profile.speedscope.json`
//! and `report.html` (one self-contained page).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    path::Path,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Value as Json, json};

use super::{Config, Mark, Row, Run, Source, attribution, caches::Counts, cpu, hw, memory, vm};

/// What a profile collected
pub struct Input<'a> {
    pub config: &'a Config,
    pub run: &'a Run,
    pub elapsed: f64,
    pub vm: Option<&'a vm::Results>,
    /// Who paid the engine's cold charges, handlers' dispatch by connect site, resumptions by wait site (`work`)
    pub attribution: Option<&'a attribution::Results>,
    pub allocations: Option<&'a memory::Allocations>,
    pub time: Option<&'a super::time::Time>,
    pub phases: &'a [String],
    pub marks: &'a [Mark],
    pub rows: &'a [Row],
    pub snapshots: &'a [memory::Snapshot],
    pub collector: &'a memory::Collector,
    pub caches: &'a [(&'static str, &'static str, Counts)],
    /// What the engine's caches say of the game (`caches`)
    pub facts: super::caches::Facts,
    pub cpu: &'a cpu::Report,
    /// The `cpu` kind's frames (host time, the run thread's cycles, the working set)
    pub cpu_frames: &'a [super::CpuFrame],
    pub hw: Option<&'a hw::Report>,
    pub sources: &'a FxHashMap<String, Source>,
    /// The work clock's reading when the profile was collected (the last phase's end)
    pub end_work: f64,
    /// The profile's end as a mark: the last phase's end, with the counts then
    pub end: Option<&'a Mark>,
    pub boundary: Option<&'a str>,
    pub phase_timing: Option<&'a str>,
    /// The phases as a tree (timing.rs: the command first, a phase timed inside another its child)
    pub phase_tree: Option<&'a [crate::timing::Node]>,
    pub warnings: &'a [String],
    /// What the clock multiplied script work by (Scheduler::price_scale) and the device the run priced (Options::device)
    pub price_scale: f64,
    pub device: &'a str,
}

/// profile.json's version. 2: host time is the run thread's CPU time (time_ms; wall_ms beside it), a C function is a row at its
/// call sites, the inline caches' outcomes are as the interpreter ran them (luau_caches.outcomes, by kind and site, the
/// reported phases') priced at committed work clock prices (luau_caches.prices), builtin calls are apart (builtins), ree's own
/// caches are emulator.caches, the timeline's instance and thread columns are split. A reader of another version says so, and
/// `ree profile diff` leaves out what changed meaning between two.
pub const PROFILE_VERSION: u64 = 2;

/// A C function's call sites, most work first: ((caller, line), (calls, its own work there, its total work there))
type CallSites = Vec<((i32, i32), (u64, f64, f64))>;

/// The engine's own steps get function ids of their own (below every C function's)
const ENGINE_STEP_BASE: i32 = -1_000_000_000;

/// Frames for counts no Luau stack took (hw.rs: what ran before the VM, after a phase's last sample), below the engine
/// steps' ids
const OUTSIDE_BASE: i32 = -2_000_000_000;

thread_local! {
    static ENGINE_STEPS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static OUTSIDE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The function id of an engine step's frame in host time samples
pub fn engine_step_function(name: &str) -> i32 {
    ENGINE_STEPS.with(|steps| {
        let mut steps = steps.borrow_mut();
        let index = steps.iter().position(|known| known == name).unwrap_or_else(|| {
            steps.push(name.to_string());
            steps.len() - 1
        });
        ENGINE_STEP_BASE - index as i32
    })
}

/// The function id of a frame that names where counts outside any Luau stack came from (`(label)`)
pub fn outside_function(label: &str) -> i32 {
    OUTSIDE.with(|outside| {
        let mut outside = outside.borrow_mut();
        let index = outside.iter().position(|known| known == label).unwrap_or_else(|| {
            outside.push(label.to_string());
            outside.len() - 1
        });
        OUTSIDE_BASE - index as i32
    })
}

/// A chunk's name as a module: `[string "ReplicatedStorage.X"]` is `ReplicatedStorage.X`
pub fn module_name(source: &str) -> &str {
    source.strip_prefix("[string \"").and_then(|inner| inner.strip_suffix("\"]")).or_else(|| source.strip_prefix('=')).or_else(|| source.strip_prefix('@')).unwrap_or(source)
}

/// The last segment of a module's name (`Grid` of `ReplicatedStorage.Interface.Grid`)
pub fn short_module(module: &str) -> &str {
    let module = module.trim_end_matches(".luau").trim_end_matches(".lua");
    module.rsplit(['.', '/', '\\']).next().unwrap_or(module)
}

/// Where a run's `--harness` module and what it brings along are mounted (record/run.rs): code there is the harness's,
/// not the code it measures
pub const HARNESS_FOLDER: &str = "ReplicatedStorage.RURFixtures";

/// Whether a module is the harness's own code: under [`HARNESS_FOLDER`], or the engine's chunks that drive it
/// (`rur.record`, which calls the harness, compiled as script code; the engine's own Luau is apart, `exempt`)
pub fn harness_module(module: &str) -> bool {
    module.strip_prefix(HARNESS_FOLDER).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')) || module.starts_with("rur.") || module.starts_with("ree.")
}

/// The environment variable a tool of ree's own sets for the runs it makes (`ree profile try`: its harness is ree's, not the
/// user's): the harness's code is then ree's tool code too
pub const TOOL_HARNESS_ENV: &str = "REE_PROFILE_TOOL_HARNESS";

/// Whether a module is ree's own tool code, never the game's nor a user's harness: the engine's chunks that drive a harness
/// (`rur.record`), and the harness itself when a tool of ree's ran it ([`TOOL_HARNESS_ENV`]: the what-if runner's)
pub fn tool_module(module: &str) -> bool {
    module.starts_with("rur.") || module.starts_with("ree.") || (std::env::var_os(TOOL_HARNESS_ENV).is_some_and(|value| value == "1") && harness_module(module))
}

/// Each module's short name (its last segment), qualified with the segments before it where two modules would share one (the
/// what-if runner's harness `RURFixtures.WhatIf` and a user's `ReplicatedStorage.WhatIf`)
pub fn short_names<'a>(modules: impl Iterator<Item = &'a str>) -> FxHashMap<String, String> {
    let mut modules: Vec<&str> = modules.collect();
    modules.sort_unstable();
    modules.dedup();
    let segments = |module: &str| -> Vec<String> {
        let trimmed = module.trim_end_matches(".luau").trim_end_matches(".lua");
        trimmed.split(['.', '/', '\\']).map(str::to_string).collect()
    };
    let mut shorts: FxHashMap<String, String> = FxHashMap::default();
    let mut depth = 1;
    let mut pending: Vec<&str> = modules;
    while !pending.is_empty() {
        let mut by_short: FxHashMap<String, Vec<&str>> = FxHashMap::default();
        for module in &pending {
            let parts = segments(module);
            let start = parts.len().saturating_sub(depth);
            by_short.entry(parts[start..].join(".")).or_default().push(module);
        }
        let mut next = Vec::new();
        for (short, group) in by_short {
            // (unique, or as long as the modules' names: written whole)
            if group.len() == 1 || group.iter().all(|module| segments(module).len() <= depth) {
                for module in group {
                    shorts.insert(module.to_string(), short.clone());
                }
            } else {
                next.extend(group);
            }
        }
        pending = next;
        depth += 1;
    }
    shorts
}

/// The phases the summary reports, as indices into the run's phases: `--profile-phase a,b`, or `all`; by default
/// `steady` when the run marked one, else every phase. Names the run never marked are skipped; when none is left, every
/// phase.
fn reported_phases(chosen: Option<&str>, phases: &[String]) -> Vec<u32> {
    let every: Vec<u32> = (0..phases.len().max(1) as u32).collect();
    let picked: Vec<u32> = match chosen.map(str::trim) {
        Some("all") => return every,
        Some(names) => names.split(',').filter_map(|name| phases.iter().position(|phase| phase == name.trim())).map(|index| index as u32).collect(),
        None => phases.iter().position(|phase| phase == "steady").map(|index| vec![index as u32]).unwrap_or_default(),
    };
    if picked.is_empty() { every } else { picked }
}

/// Names of functions by id: Luau functions, C functions and the engine's steps
struct Names<'a> {
    vm: Option<&'a vm::Results>,
    steps: Vec<String>,
    outside: Vec<String>,
    /// Each module's short name, qualified where two would share one
    shorts: FxHashMap<String, String>,
}

impl Names<'_> {
    fn function(&self, id: i32) -> Option<&vm::Function> {
        (id >= 0).then(|| self.vm.and_then(|vm| vm.functions.get(id as usize))).flatten()
    }

    /// `name module:line` (folded stacks, speedscope)
    fn frame(&self, id: i32) -> String {
        if id >= 0 {
            return match self.function(id) {
                Some(f) => format!("{} {}:{}", if f.name.is_empty() { "anonymous" } else { &f.name }, module_name(&f.source), f.line),
                None => format!("function #{id}"),
            };
        }
        if id <= OUTSIDE_BASE {
            return format!("({})", self.outside.get((OUTSIDE_BASE - id) as usize).map_or("?", String::as_str));
        }
        if id <= ENGINE_STEP_BASE {
            return format!("(ree engine step: {})", self.steps.get((ENGINE_STEP_BASE - id) as usize).map_or("?", String::as_str));
        }
        format!("{} [C]", self.c_name(id, false))
    }

    /// A C function's name: as the globals hold it (`table.sort`), the method a NAMECALL looked up, its debug name; one with none
    /// by the call site that reached it, `? called at Module:12` (the VM names it `?@<chunk>:<line>`), so two read apart
    fn c_name(&self, id: i32, short: bool) -> String {
        let name = self.vm.and_then(|vm| vm.cfunctions.get((-2 - id) as usize)).map_or("", String::as_str);
        if name.is_empty() {
            return "?".into();
        }
        match name.strip_prefix("?@").and_then(|site| site.rsplit_once(':')) {
            Some((source, line)) => format!("? called at {}:{line}", if short { self.short(source).to_string() } else { module_name(source).to_string() }),
            None => name.to_string(),
        }
    }

    /// A function's name as profile.json lists it: a Luau function's own (empty when anonymous), a C function's bare
    /// (`table.sort`, as the globals hold it), an engine step's frame
    fn name(&self, id: i32) -> String {
        if let Some(f) = self.function(id) {
            return f.name.clone();
        }
        if id < -1 && id > ENGINE_STEP_BASE {
            return self.c_name(id, true);
        }
        self.frame(id)
    }

    /// A module's short name ([`short_names`])
    fn short<'s>(&'s self, source: &'s str) -> &'s str {
        let module = module_name(source);
        self.shorts.get(module).map_or_else(|| short_module(module), String::as_str)
    }

    /// `name  Module:line` (tables)
    fn label(&self, id: i32) -> String {
        match self.function(id) {
            Some(f) => format!("{}  {}:{}", if f.name.is_empty() { "anonymous" } else { &f.name }, self.short(&f.source), f.line),
            None => self.frame(id),
        }
    }

    /// ree's own tool code ([`tool_module`])
    fn tool(&self, id: i32) -> bool {
        self.function(id).is_some_and(|f| !f.exempt() && tool_module(module_name(&f.source)))
    }

    fn engine(&self, id: i32) -> bool {
        self.function(id).is_some_and(vm::Function::exempt)
    }

    /// A helper chunk of the binding (mlua's "=__mlua..."): its work is the calling line's, so tables leave it out
    fn binding(&self, id: i32) -> bool {
        self.function(id).is_some_and(|f| f.binding || module_name(&f.source).starts_with("__mlua"))
    }

    /// A Luau function of the harness's own code ([`harness_module`])
    fn harness(&self, id: i32) -> bool {
        self.function(id).is_some_and(|f| !f.exempt() && harness_module(module_name(&f.source)))
    }
}

/// Per-function figures
#[derive(Clone, Copy, Debug, Default)]
struct Figures {
    calls: u64,
    self_insns: u64,
    total_insns: u64,
    self_work: f64,
    total_work: f64,
    /// Host time: the run thread's CPU milliseconds (wall when its cycles cannot be read), and the wall clock's
    self_time: f64,
    total_time: f64,
    self_wall: f64,
    total_wall: f64,
    self_alloc: f64,
    total_alloc: f64,
    self_allocs: f64,
    total_allocs: f64,
    /// As a function Luau's `-O2` inlined into others: the times a copy was entered, and the instructions and work on its
    /// own lines there (the callers' figures, as the VM ran them)
    inlined_calls: u64,
    inlined_insns: u64,
    inlined_work: f64,
    /// Host time on its own lines in those copies (CPU, wall)
    inlined_time: f64,
    inlined_wall: f64,
    /// Of its self work, each kind's (vm::SPLITS: cold and first-use, the collector's, Studio-era, defaulted, dispatch); of the cold,
    /// the charges for objects nothing else touched in their frame; and its subtree's cold charges (outermost on a path)
    self_split: [f64; vm::SPLITS],
    self_exclusive: f64,
    total_cold: f64,
}

impl Figures {
    /// What cutting the function saves: its hot self work and only the cold charges for objects nothing else touched in their frame
    /// (a shared object's charge moves to the code that touches it next)
    fn saving(&self) -> f64 {
        (self.self_work - self.self_split[vm::COLD] + self.self_exclusive).max(0.0)
    }
}

/// A game function's cold charges in the reported phases, and what cutting it saves there
#[derive(Clone, Copy, Debug, Default)]
struct ColdAndSaving {
    cold: f64,
    saving: f64,
}

/// A function's figures in one phase (profile.json's `phases` of a function)
#[derive(Clone, Copy, Debug, Default)]
struct FunctionPhase {
    self_work: f64,
    total_work: f64,
    calls: u64,
    self_insns: u64,
    /// Self work on the harness's side
    harness_self_work: f64,
    /// Of the self work: the cold and first-use charges, what cutting the function saves, the collector's
    cold: f64,
    saving: f64,
    collector: f64,
    /// Cold charges on the harness's side
    harness_cold: f64,
}

/// A function's copies inlined into one caller (the caller, or the function whose copy holds them)
#[derive(Clone, Copy, Debug, Default)]
struct Inlined {
    copies: u64,
    calls: u64,
    /// Instructions and work in the copies, the copies inside them included
    insns: u64,
    work: f64,
}

/// Per-line figures (a function's line)
#[derive(Clone, Debug, Default)]
struct LineFigures {
    insns: u64,
    /// The work of the line's own instructions, and of the C functions it called (their own: what a Luau function a C function
    /// called did is that function's lines'): the two are the line's self work, and every line's add up to the scripts' work
    work: f64,
    c_work: f64,
    total_work: f64,
    total_insns: u64,
    time: f64,
    total_time: f64,
    wall: f64,
    total_wall: f64,
    alloc: f64,
    allocs: f64,
    ops: BTreeMap<u8, u64>,
    /// Of its work, the cold and first-use charges and the collector's
    cold: f64,
    collector: f64,
    /// Its figures in each phase that ran it
    phases: BTreeMap<u32, LinePhase>,
}

/// A line's figures in one phase
#[derive(Clone, Copy, Debug, Default)]
struct LinePhase {
    insns: u64,
    work: f64,
    c_work: f64,
    cold: f64,
    total_work: f64,
}

impl LinePhase {
    /// The line's own instructions' work and the C functions it called
    fn self_work(&self) -> f64 {
        self.work + self.c_work
    }
}

impl LineFigures {
    /// The line's figures summed over the phases in `phases`
    fn within(&self, phases: &[u32]) -> LinePhase {
        let mut sum = LinePhase::default();
        for (phase, figures) in &self.phases {
            if phases.contains(phase) {
                sum.insns += figures.insns;
                sum.work += figures.work;
                sum.c_work += figures.c_work;
                sum.cold += figures.cold;
                sum.total_work += figures.total_work;
            }
        }
        sum
    }
}

/// A line's figures in the reported phases, for the summary's table of lines
struct ScopedLine<'a> {
    function: i32,
    line: i32,
    figures: &'a LineFigures,
    phase: LinePhase,
}

/// How a function's self work in one phase splits by kind, and the cold charges nothing else shared
#[derive(Clone, Copy, Debug, Default)]
struct PhaseSplit {
    /// Self work of each kind (vm::SPLITS)
    kinds: [f64; vm::SPLITS],
    /// The cold charges for objects nothing else touched in their frame
    exclusive_cold: f64,
}

impl PhaseSplit {
    fn cold(&self) -> f64 {
        self.kinds[vm::COLD]
    }

    /// What cutting the function in this phase saves, given its `self_work` there: the hot work and the unshared cold charges
    fn saving(&self, self_work: f64) -> f64 {
        (self_work - self.cold() + self.exclusive_cold).max(0.0)
    }
}

/// Adds each kind of `more` to the same kind of `total`
fn add_kinds(total: &mut [f64; vm::SPLITS], more: &[f64; vm::SPLITS]) {
    for (sum, value) in total.iter_mut().zip(more) {
        *sum += value;
    }
}

#[derive(Default)]
struct Model {
    functions: FxHashMap<i32, Figures>,
    lines: FxHashMap<(i32, i32), LineFigures>,
    /// (caller, callee) -> (calls, total work, total instructions)
    edges: FxHashMap<(i32, i32), (u64, f64, u64)>,
    /// (caller, inlined function) -> its copies there
    inlined: FxHashMap<(i32, i32), Inlined>,
    /// (function, line) -> the function whose inlined copy the line's instructions are
    line_inlined: FxHashMap<(i32, i32), i32>,
    /// Per-node subtree totals (work, instructions) and cold charges
    subtree: Vec<(f64, u64)>,
    subtree_cold: Vec<f64>,
    /// The phase each node is under
    node_phase: Vec<u32>,
    /// (function, phase, the harness's side) -> self work, calls and self instructions there. A node is on the side of
    /// its nearest Luau function that is not the engine's own: a C function or the engine's Luau is its caller's.
    phase_self: FxHashMap<(i32, u32, bool), (f64, u64, u64)>,
    /// (function, phase, the harness's side) -> total work there (a function's outermost node on its path)
    phase_total: FxHashMap<(i32, u32, bool), f64>,
    /// (function, phase, the harness's side) -> each kind's share of its self work there, and its unshared cold charges
    phase_split: FxHashMap<(i32, u32, bool), PhaseSplit>,
}

/// The call tree's per-function and per-line figures, and its edges. `engine_exclusive`: the engine's cold charges nothing else touched
/// in their frame, by tree node (profiler/attribution.rs), beside the VM's own (each node's `exclusive`).
fn tree_model(vm: &vm::Results, engine_exclusive: &FxHashMap<i32, f64>) -> Model {
    let nodes = &vm.nodes;
    let mut model = Model { subtree: vec![(0.0, 0); nodes.len()], subtree_cold: vec![0.0; nodes.len()], node_phase: vec![0; nodes.len()], ..Model::default() };
    // phases: a node under the root with function -1 is a phase's
    for (index, node) in nodes.iter().enumerate() {
        model.node_phase[index] = if node.function == -1 && node.parent == 0 && index != 0 {
            node.callpc.max(0) as u32
        } else if node.parent >= 0 {
            model.node_phase[node.parent as usize]
        } else {
            0
        };
    }
    // the harness's side: a node's own Luau function's module, else its parent's (a parent comes before its children).
    // The engine's own Luau and mlua's (`__mlua_index`, which runs when a script indexes userdata) work for their caller.
    let mut harness_side = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        let own = (node.function >= 0)
            .then(|| vm.functions.get(node.function as usize))
            .flatten()
            .filter(|f| !f.exempt() && !f.binding && !module_name(&f.source).starts_with("__mlua"));
        harness_side[index] = match own {
            Some(function) => harness_module(module_name(&function.source)),
            None => node.parent >= 0 && harness_side[node.parent as usize],
        };
    }
    for index in (0..nodes.len()).rev() {
        let node = &nodes[index];
        model.subtree[index].0 += node.work;
        model.subtree[index].1 += node.insns;
        model.subtree_cold[index] += node.split[vm::COLD];
        if node.parent >= 0 {
            let (work, insns) = model.subtree[index];
            let cold = model.subtree_cold[index];
            model.subtree[node.parent as usize].0 += work;
            model.subtree[node.parent as usize].1 += insns;
            model.subtree_cold[node.parent as usize] += cold;
        }
    }
    let real = |id: i32| id >= 0 || id < -1;
    // whether a node has an ancestor of its own function: totals count only a function's outermost node on a path, and only the calls
    // its outermost nodes made (a recursion's inner calls are inside the outer one's subtree already: a fib(15)'s line read 16.2 ms in a
    // 3.0 ms run when every level added its subtree)
    let mut nested = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        let mut up = node.parent;
        while up >= 0 {
            if nodes[up as usize].function == node.function && real(node.function) {
                nested[index] = true;
                break;
            }
            up = nodes[up as usize].parent;
        }
    }
    // each function's self work in its outermost nodes, against all of it (its lines' own work in the outermost frames)
    let mut outer_self: FxHashMap<i32, (f64, f64)> = FxHashMap::default();
    for (index, node) in nodes.iter().enumerate() {
        let entry = outer_self.entry(node.function).or_default();
        entry.1 += node.work;
        if !nested[index] {
            entry.0 += node.work;
        }
    }
    for (index, node) in nodes.iter().enumerate() {
        if !real(node.function) {
            continue;
        }
        let phase = model.node_phase[index];
        let scoped = (node.function, phase, harness_side[index]);
        let in_phase = model.phase_self.entry(scoped).or_default();
        in_phase.0 += node.work;
        in_phase.1 += node.calls;
        in_phase.2 += node.insns;
        let exclusive = node.exclusive + engine_exclusive.get(&(index as i32)).copied().unwrap_or(0.0);
        let split = model.phase_split.entry(scoped).or_default();
        add_kinds(&mut split.kinds, &node.split);
        split.exclusive_cold += exclusive;
        let figures = model.functions.entry(node.function).or_default();
        figures.calls += node.calls;
        figures.self_insns += node.insns;
        figures.self_work += node.work;
        add_kinds(&mut figures.self_split, &node.split);
        figures.self_exclusive += exclusive;
        if !nested[index] {
            figures.total_work += model.subtree[index].0;
            figures.total_insns += model.subtree[index].1;
            figures.total_cold += model.subtree_cold[index];
            *model.phase_total.entry(scoped).or_default() += model.subtree[index].0;
        }
        if node.parent >= 0 {
            let parent = &nodes[node.parent as usize];
            if real(parent.function) {
                let edge = model.edges.entry((parent.function, node.function)).or_default();
                edge.0 += node.calls;
                // (a call's subtree counts in its edge's and its calling line's totals when the caller's node is outermost)
                if !nested[node.parent as usize] {
                    edge.1 += model.subtree[index].0;
                    edge.2 += model.subtree[index].1;
                    if parent.function >= 0
                        && node.callpc >= 0
                        && let Some(&line) = vm.functions.get(parent.function as usize).and_then(|f| f.lines.get(node.callpc as usize))
                    {
                        let figures = model.lines.entry((parent.function, line)).or_default();
                        figures.total_work += model.subtree[index].0;
                        figures.total_insns += model.subtree[index].1;
                        figures.phases.entry(phase).or_default().total_work += model.subtree[index].0;
                    }
                }
                // A C call's own work is part of its calling line's self work even
                // inside recursion. Only the inclusive totals skip nested callers.
                if node.function < -1 && parent.function >= 0 && node.callpc >= 0
                    && let Some(&line) = vm.functions.get(parent.function as usize).and_then(|f| f.lines.get(node.callpc as usize))
                {
                    let figures = model.lines.entry((parent.function, line)).or_default();
                    figures.c_work += node.work;
                    figures.phases.entry(phase).or_default().c_work += node.work;
                }
            }
        }
    }
    let outer_share = |id: i32| outer_self.get(&id).map_or(1.0, |&(outer, all)| if all > 0.0 { outer / all } else { 1.0 });
    for (id, function) in vm.functions.iter().enumerate() {
        for (pc, &count) in function.counts.iter().enumerate() {
            let work = function.work.get(pc).copied().unwrap_or(0.0);
            if count == 0 && work == 0.0 {
                continue;
            }
            let line = function.lines.get(pc).copied().unwrap_or(0);
            let figures = model.lines.entry((id as i32, line)).or_default();
            figures.insns += count;
            figures.work += work;
            // (in its total, the outermost frames' share of it: deeper frames of a recursion are in an outer call's subtree)
            figures.total_work += work * outer_share(id as i32);
            figures.total_insns += count;
            figures.cold += function.split.get(pc * vm::SPLITS + vm::COLD).copied().unwrap_or(0.0);
            figures.collector += function.split.get(pc * vm::SPLITS + vm::COLLECTOR).copied().unwrap_or(0.0);
            if count > 0 {
                *figures.ops.entry((function.code[pc] & 0xff) as u8).or_default() += count;
            }
        }
        // each phase's own lines
        for lines in &function.phase_lines {
            for pc in 0..function.code.len() {
                let (count, work, cold) = (lines.counts.get(pc).copied().unwrap_or(0), lines.work.get(pc).copied().unwrap_or(0.0), lines.cold.get(pc).copied().unwrap_or(0.0));
                if count == 0 && work == 0.0 {
                    continue;
                }
                let line = function.lines.get(pc).copied().unwrap_or(0);
                let phase = model.lines.entry((id as i32, line)).or_default().phases.entry(lines.phase).or_default();
                phase.insns += count;
                phase.work += work;
                phase.total_work += work * outer_share(id as i32);
                phase.cold += cold;
            }
        }
    }
    inlined_model(vm, &mut model);
    model
}

/// The copies of functions `-O2` inlined into others: each inlined function's calls (entries into its copies), and the
/// instructions and work on its own lines in them; per caller, the same with the copies inside included
fn inlined_model(vm: &vm::Results, model: &mut Model) {
    for (id, function) in vm.functions.iter().enumerate() {
        if function.inlines.is_empty() {
            continue;
        }
        let caller_of = |copy: &vm::Inline| if copy.parent >= 0 { function.inlines.get(copy.parent as usize).map_or(id as i32, |holder| holder.function) } else { id as i32 };
        let mut innermost = vec![-1i32; function.code.len()];
        for (index, copy) in function.inlines.iter().enumerate() {
            model.functions.entry(copy.function).or_default().inlined_calls += copy.entries;
            let edge = model.inlined.entry((caller_of(copy), copy.function)).or_default();
            edge.copies += 1;
            edge.calls += copy.entries;
            // (a copy comes after the one it sits in: the innermost is written last)
            for pc in copy.first_pc.max(0)..=copy.last_pc {
                if let Some(slot) = innermost.get_mut(pc as usize) {
                    *slot = index as i32;
                }
            }
        }
        for (pc, &copy) in innermost.iter().enumerate() {
            if copy < 0 {
                continue;
            }
            let inline = function.inlines[copy as usize];
            if let Some(&line) = function.lines.get(pc) {
                model.line_inlined.insert((id as i32, line), inline.function);
            }
            let count = function.counts.get(pc).copied().unwrap_or(0);
            let work = function.work.get(pc).copied().unwrap_or(0.0);
            if count == 0 && work == 0.0 {
                continue;
            }
            let figures = model.functions.entry(inline.function).or_default();
            figures.inlined_insns += count;
            figures.inlined_work += work;
            let mut at = copy;
            while at >= 0 {
                let holder = function.inlines[at as usize];
                let edge = model.inlined.entry((caller_of(&holder), holder.function)).or_default();
                edge.insns += count;
                edge.work += work;
                at = holder.parent;
            }
        }
    }
}

/// Stack samples (innermost first) folded into per-function and per-line self and total figures
fn add_samples(model: &mut Model, stack: &[memory::Frame], weight: f64, field: fn(&mut Figures, bool) -> &mut f64, line_field: fn(&mut LineFigures, bool) -> &mut f64, luau_self: bool) {
    // self: the innermost frame (or, for allocations, the innermost Luau frame: the line that called a C function)
    let innermost = if luau_self { stack.iter().find(|frame| frame.0 >= 0).or(stack.first()) } else { stack.first() };
    if let Some(&(function, line)) = innermost {
        *field(model.functions.entry(function).or_default(), true) += weight;
        if function >= 0 {
            *line_field(model.lines.entry((function, line)).or_default(), true) += weight;
        }
    }
    let mut seen_functions: FxHashSet<i32> = FxHashSet::default();
    let mut seen_lines: FxHashSet<(i32, i32)> = FxHashSet::default();
    for &(function, line) in stack {
        if seen_functions.insert(function) {
            *field(model.functions.entry(function).or_default(), false) += weight;
        }
        if function >= 0 && seen_lines.insert((function, line)) {
            *line_field(model.lines.entry((function, line)).or_default(), false) += weight;
        }
    }
}

/// Host time on the copies of functions `-O2` inlined, an estimate: each sample at a safepoint (or an Instance API call) stands for
/// the instructions since the safepoint before it (vm::sample_segment), and a copy's share of them, by the work clock's prices of
/// their opcodes, is its function's inlined time. A copy with no safepoint of its own (a leaf: no call, no loop) gets its share
/// of the samples at the safepoint after it. A sample right after a call that reached a C function is not split: it holds the
/// call's own time too (a C function has no safepoint to sample it), so it stays its line's; each sample says whether a C
/// function ran since the safepoint before it (the VM's count of C calls, read at each safepoint while host time is sampled), so
/// a call site that reaches a C function at one time and a Luau function at another is split after the Luau calls, whose own
/// safepoints sampled them, and so is a builtin's call its fast path served.
fn add_inlined_time(model: &mut Model, vm: &vm::Results, time: &super::time::Time) {
    let mut segments: FxHashMap<(i32, i32, bool), Vec<(i32, f64)>> = FxHashMap::default();
    for (&(function, pc, after_c), &(ticks, cycles)) in &time.pcs {
        let Some(f) = vm.functions.get(function.max(0) as usize).filter(|f| !f.inlines.is_empty()) else { continue };
        let shares = segments.entry((function, pc, after_c)).or_insert_with(|| {
            // (each instruction's innermost copy: a copy comes after the one it sits in)
            let innermost = |at: usize| f.inlines.iter().rev().find(|copy| copy.first_pc >= 0 && copy.first_pc as usize <= at && at <= copy.last_pc as usize).map(|copy| copy.function);
            let (segment, call) = vm::sample_segment(&f.code, pc.max(0) as usize);
            if call.is_some() && after_c {
                return Vec::new();
            }
            let price = |at: usize| crate::cost::INTERPRETED_OPS[vm::base_op((f.code[at] & 0xff) as u8) as usize];
            let total: f64 = segment.iter().map(|&at| price(at)).sum();
            let weight = |at: usize| if total > 0.0 { price(at) / total } else { 1.0 / segment.len() as f64 };
            let mut by_copy: FxHashMap<i32, f64> = FxHashMap::default();
            for &at in &segment {
                if let Some(copy) = innermost(at) {
                    *by_copy.entry(copy).or_default() += weight(at);
                }
            }
            by_copy.into_iter().collect()
        });
        let wall = ticks as f64;
        let cpu = time.cpu_ms(cycles).unwrap_or(wall);
        for &(copy, share) in shares.iter() {
            let figures = model.functions.entry(copy).or_default();
            figures.inlined_time += cpu * share;
            figures.inlined_wall += wall * share;
        }
    }
}

/// A host time sample (innermost frame first) into the model: the innermost frame's self time (a C function's: an Instance API
/// call's own), the innermost Luau line's (the line that made the call: its next safepoint's, for a line with none), every frame
/// and line on the stack total. An inlined function's share is [`add_inlined_time`]'s.
fn add_time(model: &mut Model, stack: &[memory::Frame], cpu: f64, wall: f64) {
    if let Some(&(function, _)) = stack.first() {
        let figures = model.functions.entry(function).or_default();
        figures.self_time += cpu;
        figures.self_wall += wall;
    }
    if let Some(&(function, line)) = stack.iter().find(|frame| frame.0 >= 0) {
        let figures = model.lines.entry((function, line)).or_default();
        figures.time += cpu;
        figures.wall += wall;
    }
    let mut seen_functions: FxHashSet<i32> = FxHashSet::default();
    let mut seen_lines: FxHashSet<(i32, i32)> = FxHashSet::default();
    for &(function, line) in stack {
        if seen_functions.insert(function) {
            let figures = model.functions.entry(function).or_default();
            figures.total_time += cpu;
            figures.total_wall += wall;
        }
        if function >= 0 && line >= 0 && seen_lines.insert((function, line)) {
            let figures = model.lines.entry((function, line)).or_default();
            figures.total_time += cpu;
            figures.total_wall += wall;
        }
    }
}

thread_local! {
    /// What the clock multiplied script work by in the run being written (Scheduler::price_scale: a phone's factor, a timing check's):
    /// the profile's milliseconds are the clock's, the units os.clock advanced in
    static SCALE: std::cell::Cell<f64> = const { std::cell::Cell::new(1.0) };
}

/// The scale of the run being written
fn price_scale() -> f64 {
    SCALE.with(std::cell::Cell::get)
}

/// A row's code work on each side, in the run's device units.
fn row_sides_ms(row: &Row) -> Option<(f64, f64)> {
    row.sides.map(|(game, harness)| (game * price_scale(), harness * price_scale()))
}

/// Two parts of a frame's work on each side added; unknown when either part's is
fn add_sides(first: Option<(f64, f64)>, second: Option<(f64, f64)>) -> Option<(f64, f64)> {
    let (first, second) = first.zip(second)?;
    Some((first.0 + second.0, first.1 + second.1))
}

/// A timeline row as profile.json stores it, its values in the order of the timeline's `columns`. `host`: the frame's host time
/// (ms) and cycles
fn timeline_row(row: &Row, host: Option<(f64, u64)>) -> Json {
    let sides = row_sides_ms(row);
    json!([
        row.frame,
        round(row.seconds, 4),
        row.phase,
        host.map(|(ms, _)| round(ms, 4)),
        round(row.work_ms * price_scale(), 5),
        row.instructions,
        row.heap_bytes,
        row.allocated_bytes,
        row.allocations,
        row.gc_steps,
        round(row.gc_ms, 4),
        row.gc_freed,
        row.instances.0,
        row.instances.1,
        row.instances.2,
        row.connections,
        row.threads_made,
        host.map(|(_, cycles)| cycles),
        row.part,
        sides.map(|(game, _)| game),
        sides.map(|(_, harness)| harness),
        round(row.collector_ms * price_scale(), 5),
    ])
}

/// The row a timeline keeps when it drops the detail: the frame's work, its sides and its collector share
fn compact_timeline_row(row: &Row) -> Json {
    let sides = row_sides_ms(row);
    json!([
        row.frame,
        row.seconds,
        row.phase,
        row.work_ms * price_scale(),
        row.part,
        sides.map(|(game, _)| game),
        sides.map(|(_, harness)| harness),
        row.collector_ms * price_scale(),
    ])
}

/// Milliseconds of the work clock as the run's clock ran (`seconds` at desktop prices times the run's scale)
fn ms(seconds: f64) -> f64 {
    seconds * 1e3 * price_scale()
}

fn round(value: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    (value * scale).round() / scale
}

fn group_digits(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn bytes(value: f64) -> String {
    let value = value.max(0.0);
    if value >= 1024.0 * 1024.0 {
        format!("{:.2} MB", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{value:.0} B")
    }
}

fn percent(part: f64, whole: f64) -> String {
    if whole <= 0.0 { "-".into() } else { format!("{:.1} %", part / whole * 100.0) }
}

/// The source text of a function's lines (for the annotated listing): (line, text) from its first to its last line
fn listing(input: &Input, function: &vm::Function, model: &Model, id: i32) -> Option<(String, Vec<(i32, String)>)> {
    let module = module_name(&function.source);
    let source = input.sources.get(module)?;
    let text = String::from_utf8_lossy(&source.text);
    let lines: Vec<&str> = text.lines().collect();
    let first = function.line.max(1);
    let last = function.lines.iter().copied().max().unwrap_or(first).max(first);
    // (the lines the model holds for this function, so the listing shows each with its figures)
    let _ = (model, id);
    let rows = (first..=last.min(first + 400)).filter_map(|line| lines.get(line as usize - 1).map(|text| (line, text.to_string()))).collect();
    Some((source.path.clone().unwrap_or_else(|| module.to_string()), rows))
}

/// Writes every file of the profile and returns the summary text
pub fn write(input: &Input) -> Result<String, String> {
    let out = &input.config.out;
    std::fs::create_dir_all(out).map_err(|error| format!("--profile-out {}: {error}", out.display()))?;
    let shorts = short_names(input.vm.iter().flat_map(|vm| vm.functions.iter()).map(|f| module_name(&f.source)));
    let names = Names { vm: input.vm, steps: ENGINE_STEPS.with(|steps| steps.borrow().clone()), outside: OUTSIDE.with(|outside| outside.borrow().clone()), shorts };
    let scale = if input.price_scale > 0.0 { input.price_scale } else { 1.0 };
    SCALE.with(|cell| cell.set(scale));
    // (the engine's unshared cold charges, by tree node)
    let mut engine_exclusive: FxHashMap<i32, f64> = FxHashMap::default();
    if let Some(attribution) = input.attribution {
        for charge in &attribution.exclusive {
            *engine_exclusive.entry(charge.node).or_default() += charge.cost;
        }
    }
    let mut model = input.vm.map(|vm| tree_model(vm, &engine_exclusive)).unwrap_or_default();

    // host time samples (the run thread's CPU milliseconds, and wall ticks of a millisecond) and allocations (bytes) into the model
    let (mut time_total, mut wall_total) = (0.0, 0.0);
    if let Some(time) = input.time {
        for (&(_, stack), &(ticks, cycles)) in &time.samples {
            let wall = ticks as f64;
            let cpu = time.cpu_ms(cycles).unwrap_or(wall);
            time_total += cpu;
            wall_total += wall;
            add_time(&mut model, &time.stacks.stacks[stack as usize], cpu, wall);
        }
        if let Some(vm) = input.vm {
            add_inlined_time(&mut model, vm, time);
        }
    }
    if let Some(allocations) = input.allocations {
        for (&(_, stack, _), amount) in &allocations.by_stack {
            let frames = &allocations.stacks.stacks[stack as usize];
            add_samples(&mut model, frames, amount.bytes, |f, own| if own { &mut f.self_alloc } else { &mut f.total_alloc }, |l, _| &mut l.alloc, true);
            add_samples(&mut model, frames, amount.count, |f, own| if own { &mut f.self_allocs } else { &mut f.total_allocs }, |l, _| &mut l.allocs, true);
        }
    }

    let seconds = input.run.seconds.max(0.0);
    let frames = input.run.frames.max(1) as f64;
    let mut files: Vec<String> = Vec::new();
    let mut json = json!({
        "version": PROFILE_VERSION,
        "tool": "ree",
        "kinds": input.config.kinds.names(),
        "run": {
            "command": input.run.command, "status": input.run.status, "harness": input.run.harness, "frames": input.run.frames,
            "seconds": input.run.seconds, "fps": input.run.fps, "elapsed": input.elapsed,
            // (the harness's contents hashed: two copies of one harness in other folders are the same harness. The text its
            // module loaded, hashed as source_hashes hashes it, so that a profile from before this field, compared by that
            // entry, reads the same harness as the same; the file's bytes where no module of its name loaded)
            "harness_hash": input.run.harness.as_ref().and_then(|path| match Path::new(path).file_stem().and_then(|stem| input.sources.get(&format!("{HARNESS_FOLDER}.{}", stem.to_string_lossy()))) {
                Some(source) => Some(format!("{:016x}", fxhash(&source.text))),
                None => std::fs::read(path).ok().map(|bytes| format!("{:016x}", fxhash(&bytes))),
            }),
            "script_timeouts": input.run.script_timeouts, "host_backstop_timeouts": input.run.host_backstop_timeouts,
        },
        "phases": input.phases,
        "marks": input.marks.iter().map(mark_json).collect::<Vec<_>>(),
        "end": input.end.map(mark_json),
        "warnings": input.warnings,
    });

    // functions: every one with figures, keyed so a diff finds it again
    let mut function_rows: Vec<(i32, Figures)> = model.functions.iter().map(|(&id, figures)| (id, *figures)).collect();
    function_rows.sort_by(|a, b| b.1.self_work.total_cmp(&a.1.self_work).then(b.1.total_time.total_cmp(&a.1.total_time)).then(b.1.total_alloc.total_cmp(&a.1.total_alloc)).then(a.0.cmp(&b.0)));
    // each inlined function's callers, most work first
    let mut inlined_into: FxHashMap<i32, Vec<(i32, Inlined)>> = FxHashMap::default();
    for (&(caller, callee), inlined) in &model.inlined {
        inlined_into.entry(callee).or_default().push((caller, *inlined));
    }
    for callers in inlined_into.values_mut() {
        callers.sort_by(|a, b| b.1.work.total_cmp(&a.1.work).then(b.1.calls.cmp(&a.1.calls)).then(a.0.cmp(&b.0)));
    }
    let phase_name = |phase: u32| input.phases.get(phase as usize).cloned().unwrap_or_else(|| "startup".to_string());
    let mut by_phase: FxHashMap<i32, BTreeMap<u32, FunctionPhase>> = FxHashMap::default();
    for (&(id, phase, harness), &(work, calls, insns)) in &model.phase_self {
        let figures = by_phase.entry(id).or_default().entry(phase).or_default();
        figures.self_work += work;
        figures.calls += calls;
        figures.self_insns += insns;
        if harness {
            figures.harness_self_work += work;
        }
        if let Some(split) = model.phase_split.get(&(id, phase, harness)) {
            figures.cold += split.cold();
            figures.saving += split.saving(work);
            figures.collector += split.kinds[vm::COLLECTOR];
            if harness {
                figures.harness_cold += split.cold();
            }
        }
    }
    for (&(id, phase, _), &total) in &model.phase_total {
        by_phase.entry(id).or_default().entry(phase).or_default().total_work += total;
    }
    // the phases the summary reports, and their self work on each side
    let scope = reported_phases(input.config.phases.as_deref(), input.phases);
    let (game_self, harness_self) = model
        .phase_self
        .iter()
        .filter(|((_, phase, _), _)| scope.contains(phase))
        .fold((0.0, 0.0), |(game, harness), ((_, _, side), figures)| if *side { (game, harness + figures.0) } else { (game + figures.0, harness) });
    // what the scripts asked the engine to do in the reported phases: each of their marks' counts to the next mark's (the last
    // to the run's end)
    let scope_names: Vec<String> = scope.iter().map(|phase| phase_name(*phase)).collect();
    let mut scope_engine: Option<crate::cost::EngineOps> = None;
    for (index, mark) in input.marks.iter().enumerate() {
        let next = input.marks.get(index + 1).or(input.end);
        if let (true, Some(from), Some(to)) = (scope_names.contains(&mark.phase), mark.engine, next.and_then(|next| next.engine)) {
            let ops = to.since(&from);
            scope_engine = Some(scope_engine.map_or(ops, |sum| sum.plus(&ops)));
        }
    }
    // the line a node was called from (its parent's instruction that made the call; -1 for a C or native caller)
    let call_line = |node: &vm::Node| -> i32 {
        let parent = input.vm.and_then(|vm| vm.nodes.get(node.parent.max(0) as usize));
        match parent {
            Some(parent) if node.parent >= 0 && parent.function >= 0 && node.callpc >= 0 => names.function(parent.function).and_then(|f| f.lines.get(node.callpc as usize).copied()).unwrap_or(-1),
            _ => -1,
        }
    };
    // a C function's call sites: (caller, line) -> (calls, self work, total work), most work first
    let mut c_sites: FxHashMap<i32, CallSites> = FxHashMap::default();
    if let Some(vm_results) = input.vm {
        let mut sites: FxHashMap<(i32, i32, i32), (u64, f64, f64)> = FxHashMap::default();
        for (index, node) in vm_results.nodes.iter().enumerate() {
            if node.function >= -1 || node.parent < 0 {
                continue;
            }
            let caller = vm_results.nodes[node.parent as usize].function;
            let entry = sites.entry((node.function, caller, call_line(node))).or_default();
            entry.0 += node.calls;
            entry.1 += node.work;
            entry.2 += model.subtree.get(index).map_or(node.work, |subtree| subtree.0);
        }
        for ((function, caller, line), figures) in sites {
            c_sites.entry(function).or_default().push(((caller, line), figures));
        }
        for rows in c_sites.values_mut() {
            rows.sort_by(|a, b| b.1.2.total_cmp(&a.1.2).then(b.1.0.cmp(&a.1.0)).then(a.0.cmp(&b.0)));
        }
    }
    let stepping = input.config.kinds.steps();
    let function_json = |id: i32, figures: &Figures| -> Json {
        let function = names.function(id);
        json!({
            "id": id,
            "key": function_key(&names, id),
            "name": names.name(id),
            "module": function.map(|f| module_name(&f.source).to_string()),
            "line": function.map(|f| f.line),
            "last_line": function.and_then(|f| f.lines.iter().copied().max()),
            // (what pairs a function across an edit that moved it: its code as compiled, and its text)
            "code_hash": function.and_then(code_hash),
            "source_hash": function.and_then(|f| source_hash(input.sources, f)),
            "kind": match function { Some(f) if f.exempt() => "engine Luau", Some(_) => "Luau", None if id <= ENGINE_STEP_BASE => "engine step", None => "C" },
            "native": function.is_some_and(vm::Function::native),
            // (the harness's own code, under HARNESS_FOLDER; tool: ree's own, never the game's nor a user's: the engine's chunks
            // that drive a harness, and the harness of a run a tool of ree's made)
            "harness": names.harness(id),
            "tool": names.tool(id),
            // (the module's last segment, qualified where two modules would share it)
            "short": function.map(|f| names.short(&f.source).to_string()),
            // (per phase; harness_self_work_ms is the part done for the harness's code: all of a harness function's, a C
            // function's or the engine's Luau when the harness called it)
            "phases": by_phase.get(&id).map(|phases| phases.iter().map(|(phase, f)| (phase_name(*phase), json!({
                "self_work_ms": ms(f.self_work), "total_work_ms": ms(f.total_work), "calls": f.calls, "insns": f.self_insns, "harness_self_work_ms": ms(f.harness_self_work),
                "cold_work_ms": ms(f.cold), "saving_ms": ms(f.saving), "collector_ms": ms(f.collector), "harness_cold_work_ms": ms(f.harness_cold),
            }))).collect::<serde_json::Map<_, _>>()),
            "native_entries": function.map_or(0, |f| f.native_entries),
            "native_work_ms": function.map_or(0.0, |f| ms(f.native_work)),
            "calls": figures.calls,
            "self": {
                "insns": figures.self_insns, "work_ms": ms(figures.self_work), "time_ms": figures.self_time, "wall_ms": figures.self_wall, "alloc_bytes": figures.self_alloc.round(), "allocs": figures.self_allocs.round(),
                // (of work_ms: the cold and first-use charges, the collector's, the engine's dispatch of its handlers and resumptions of its
                // threads, and by where the prices came from; saving_ms is what cutting it saves: hot work and unshared cold charges)
                "cold_work_ms": ms(figures.self_split[vm::COLD]), "collector_ms": ms(figures.self_split[vm::COLLECTOR]), "dispatch_ms": ms(figures.self_split[vm::DISPATCH]),
                "exclusive_cold_ms": ms(figures.self_exclusive), "saving_ms": ms(figures.saving()),
                "by_provenance_ms": {
                    "client": ms((figures.self_work - figures.self_split[vm::STUDIO] - figures.self_split[vm::DEFAULTED]).max(0.0)),
                    "studio": ms(figures.self_split[vm::STUDIO]), "defaulted": ms(figures.self_split[vm::DEFAULTED]),
                },
            },
            "total": {"insns": figures.total_insns, "work_ms": ms(figures.total_work), "cold_work_ms": ms(figures.total_cold), "time_ms": figures.total_time, "wall_ms": figures.total_wall, "alloc_bytes": figures.total_alloc.round(), "allocs": figures.total_allocs.round()},
            // (inlined by -O2 into the functions in `into`: its calls through those copies, and the instructions and work on
            // its own lines there, which are also theirs)
            // (a C function: where it was called from, by the calling function and line)
            "call_sites": c_sites.get(&id).map(|rows| rows.iter().take(100).map(|((caller, line), (calls, own, total))| json!({
                "caller": caller, "line": line, "calls": calls, "self_work_ms": ms(*own), "total_work_ms": ms(*total),
            })).collect::<Vec<_>>()),
            // (calls and instructions are counted while the VM steps, `work` or `caches`: null without)
            "inlined": inlined_into.get(&id).map(|callers| json!({
                "calls": stepping.then_some(figures.inlined_calls), "insns": stepping.then_some(figures.inlined_insns), "work_ms": ms(figures.inlined_work),
                // (host time: an estimate, each sample's share of the code since the safepoint before it by the work clock's opcode prices)
                "estimated_time_ms": figures.inlined_time, "estimated_wall_ms": figures.inlined_wall,
                "into": callers.iter().map(|(caller, inlined)| json!({"function": caller, "copies": inlined.copies, "calls": inlined.calls, "insns": inlined.insns, "work_ms": ms(inlined.work)})).collect::<Vec<_>>(),
            })),
        })
    };
    json["functions"] = Json::Array(function_rows.iter().map(|(id, figures)| function_json(*id, figures)).collect());
    // the reported phases' cold and collector work on both sides, and what cutting the game's code saves
    let (mut scope_cold, mut scope_collector, mut scope_saving_game) = (0.0, 0.0, 0.0);
    for (&(id, phase, harness_side), split) in &model.phase_split {
        if !scope.contains(&phase) {
            continue;
        }
        scope_cold += split.cold();
        scope_collector += split.kinds[vm::COLLECTOR];
        if !harness_side {
            let work = model.phase_self.get(&(id, phase, harness_side)).map_or(0.0, |figures| figures.0);
            scope_saving_game += split.saving(work);
        }
    }
    json["scope"] = json!({
        "phases": scope.iter().map(|phase| phase_name(*phase)).collect::<Vec<_>>(),
        "game_self_ms": ms(game_self),
        "harness_self_ms": ms(harness_self),
        "cold_work_ms": ms(scope_cold),
        "collector_ms": ms(scope_collector),
        "game_saving_ms": ms(scope_saving_game),
        "harness_folder": HARNESS_FOLDER,
        "engine": scope_engine.as_ref().map(engine_json),
    });

    // lines (with their opcodes)
    let mut line_rows: Vec<(&(i32, i32), &LineFigures)> = model.lines.iter().collect();
    let line_self = |figures: &LineFigures| figures.work + figures.c_work;
    line_rows.sort_by(|a, b| line_self(b.1).total_cmp(&line_self(a.1)).then(b.1.time.total_cmp(&a.1.time)).then(b.1.alloc.total_cmp(&a.1.alloc)).then(a.0.cmp(b.0)));
    json["lines"] = Json::Array(
        line_rows
            .iter()
            .map(|((function, line), figures)| {
                json!({
                    // (self_work_ms: its own instructions' work_ms and its C calls' own c_work_ms)
                    "function": function, "line": line, "insns": figures.insns, "work_ms": ms(figures.work), "c_work_ms": ms(figures.c_work), "self_work_ms": ms(line_self(figures)), "total_work_ms": ms(figures.total_work),
                    "total_insns": figures.total_insns, "time_ms": figures.time, "total_time_ms": figures.total_time, "wall_ms": figures.wall, "total_wall_ms": figures.total_wall,
                    "alloc_bytes": figures.alloc.round(), "allocs": figures.allocs.round(),
                    "cold_work_ms": ms(figures.cold), "collector_ms": ms(figures.collector),
                    "phases": figures.phases.iter().map(|(phase, f)| (phase_name(*phase), json!({"work_ms": ms(f.work), "self_work_ms": ms(f.work + f.c_work), "c_work_ms": ms(f.c_work), "total_work_ms": ms(f.total_work), "insns": f.insns, "cold_work_ms": ms(f.cold)}))).collect::<serde_json::Map<_, _>>(),
                    "ops": figures.ops.iter().map(|(op, count)| (vm::opcode_name(*op).to_string(), json!(count))).collect::<serde_json::Map<_, _>>(),
                    "inlined": model.line_inlined.get(&(*function, *line)),
                })
            })
            .collect(),
    );
    json["edges"] = Json::Array({
        let mut edges: Vec<_> = model.edges.iter().collect();
        edges.sort_by(|a, b| b.1.1.total_cmp(&a.1.1).then(a.0.cmp(b.0)));
        let mut rows: Vec<Json> = edges.iter().map(|((caller, callee), (calls, work, insns))| json!({"caller": caller, "callee": callee, "calls": calls, "work_ms": ms(*work), "insns": insns})).collect();
        // (calls -O2 compiled into the caller: entries into the callee's copies there)
        let mut inlined: Vec<_> = model.inlined.iter().collect();
        inlined.sort_by(|a, b| b.1.work.total_cmp(&a.1.work).then(a.0.cmp(b.0)));
        rows.extend(inlined.iter().map(|((caller, callee), inlined)| json!({"caller": caller, "callee": callee, "calls": inlined.calls, "work_ms": ms(inlined.work), "insns": inlined.insns, "inlined": true, "copies": inlined.copies})));
        rows
    });

    // The call tree: [parent, function, calls, self work in us, self instructions, label].
    // Keep fractional work so node unions reconcile with the clock.
    if let Some(vm_results) = input.vm {
        json["tree"] = Json::Array(
            vm_results
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    let label = if index == 0 {
                        Some(input.phases.first().cloned().unwrap_or_else(|| "startup".into()))
                    } else if node.function == -1 {
                        Some(input.phases.get(node.callpc.max(0) as usize).cloned().unwrap_or_default())
                    } else if node.function < -1 {
                        Some(names.frame(node.function))
                    } else {
                        None
                    };
                    json!([node.parent, node.function, node.calls, node.work * 1e6, node.insns, label])
                })
                .collect(),
        );
        // (each node's calling line, -1 for none, beside the rows: a row keeps the six fields every reader of the tree takes)
        json["tree_lines"] = Json::Array(vm_results.nodes.iter().map(|node| json!(call_line(node))).collect());
    }

    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "ree profile: {} {}; {} frames, {:.2} s emulated{}; kinds {}; {:.2} s wall",
        input.run.command,
        input.run.status,
        input.run.frames,
        seconds,
        if input.run.fps > 0.0 { format!(" at {} fps", input.run.fps) } else { String::new() },
        input.config.kinds.names().join(", "),
        input.elapsed
    );
    for warning in input.warnings {
        let _ = writeln!(summary, "warning: {warning}");
    }

    // work and instructions
    if let Some(vm_results) = input.vm.filter(|vm| !vm.nodes.is_empty() && input.config.kinds.steps()) {
        let total_work: f64 = vm_results.nodes.iter().map(|node| node.work).sum::<f64>() + vm_results.engine_work;
        let script_insns: u64 = vm_results.functions.iter().filter(|f| !f.exempt()).flat_map(|f| f.counts.iter()).sum();
        let engine_insns: u64 = vm_results.functions.iter().filter(|f| f.exempt()).flat_map(|f| f.counts.iter()).sum();
        // (the ones seen running: entered, booked to, or in a frame; the profile also registers a chunk's functions unrun)
        let natives: Vec<&vm::Function> = vm_results
            .functions
            .iter()
            .enumerate()
            .filter(|(id, f)| f.native() && !f.exempt() && (f.native_entries > 0 || f.native_work != 0.0 || model.functions.contains_key(&(*id as i32))))
            .map(|(_, f)| f)
            .collect();
        let mut opcodes: BTreeMap<u8, (u64, f64)> = BTreeMap::new();
        for function in vm_results.functions.iter().filter(|f| !f.exempt()) {
            for (pc, &count) in function.counts.iter().enumerate() {
                if count > 0 {
                    let entry = opcodes.entry((function.code[pc] & 0xff) as u8).or_default();
                    entry.0 += count;
                    entry.1 += function.work.get(pc).copied().unwrap_or(0.0);
                }
            }
        }
        let mut opcode_rows: Vec<_> = opcodes.into_iter().collect();
        opcode_rows.sort_by(|a, b| b.1.0.cmp(&a.1.0));
        // the run's kinds of work (of the total): cold and first-use charges, the collector's, dispatch, Studio-era and defaulted prices
        let mut kinds_total = [0.0f64; vm::SPLITS];
        for node in &vm_results.nodes {
            add_kinds(&mut kinds_total, &node.split);
        }
        let collector_objects = input.end.and_then(|end| end.totals).map_or(0.0, |totals| totals.collector / (crate::cost::LUAU_COLLECTOR_OBJECT * 1e-9).max(1e-30));
        // (the fidelity line weighs what the summary reports: the reported phases' game code, its work by kind)
        let (scope_work, scope_kinds) = scope_work_kinds(&model, &scope);
        let fidelity = if scope_work > 0.0 {
            let luau = scope_luau(input, &scope, scope_work).unwrap_or_else(|| whole_luau(input, total_work, &kinds_total) / total_work.max(1e-12) * scope_work);
            fidelity_json(input, scope_work, &scope_kinds, luau, &format!("the game's code in {}", scope_phrase(&scope, input.phases)))
        } else {
            fidelity_json(input, total_work, &kinds_total, whole_luau(input, total_work, &kinds_total), "the run")
        };
        json["work"] = json!({
            "total_ms": ms(total_work),
            "outside_instructions_ms": ms(vm_results.engine_work),
            "per_second_ms": if seconds > 0.0 { ms(total_work) / seconds } else { 0.0 },
            "per_frame_ms": ms(total_work) / frames,
            "fidelity": fidelity["text"].clone(),
            "cold_work_ms": ms(kinds_total[vm::COLD]),
            "collector_ms": ms(kinds_total[vm::COLLECTOR]),
            "dispatch_ms": ms(kinds_total[vm::DISPATCH]),
            "by_provenance_ms": {
                "client": ms((total_work - kinds_total[vm::STUDIO] - kinds_total[vm::DEFAULTED]).max(0.0)),
                "studio": ms(kinds_total[vm::STUDIO]), "defaulted": ms(kinds_total[vm::DEFAULTED]),
            },
            "collector": {
                // (desktop ns, as work.prices: a phone run's are these times work.units.scale)
                "object_ns": crate::cost::LUAU_COLLECTOR_OBJECT,
                "objects": collector_objects.round(),
                "basis": "every object the scripts' work allocated (its instructions and the library and engine calls it made), each swept once garbage and marked while it lives: the collector's share of the parity bench's churn rows on a heap of about 50 MB (its steps' time in ree's VM, which runs the client's collector) applied to a rendered client's time; booked to the allocating instruction",
            },
            "units": {"device": input.device, "scale": price_scale()},
            "script_insns": script_insns,
            "engine_insns": engine_insns,
            "native_functions": natives.iter().map(|f| json!({"name": f.name, "module": module_name(&f.source), "line": f.line, "entries": f.native_entries, "work_ms": ms(f.native_work)})).collect::<Vec<_>>(),
            "opcodes": opcode_rows.iter().map(|(op, (count, work))| json!({"op": vm::opcode_name(*op), "count": count, "work_ms": ms(*work)})).collect::<Vec<_>>(),
            // (a C function a CALL reached is a node of its own at the call site, whether or not it ran Luau; a Luau function a C
            // function calls counts a call each time it is entered)
            "c_calls": true,
            "c_invocations": true,
        });
        json["fidelity"] = fidelity.clone();
        // (bytes the clock counts and prices at nothing: a large buffer's fresh memory, a buffer call's bytes past a core's caches)
        let (fresh, big) = match (input.marks.first().and_then(|mark| mark.totals), input.end.and_then(|end| end.totals)) {
            (Some(first), Some(last)) => (last.buffer_fresh.saturating_sub(first.buffer_fresh), last.buffer_big.saturating_sub(first.buffer_big)),
            _ => (0, 0),
        };
        json["work"]["unpriced"] = json!({
            "buffer_fresh_bytes": fresh,
            "buffer_big_bytes": big,
            "basis": "bytes of new buffers past Luau's own pages (over 1 KB) and of buffer copies, fills and string writes in calls over 64 KB: their bytes are priced as a small buffer's are, their memory past a core's caches at nothing (no price a byte fits a client's sizes), so the clock reads such work low",
        });
        let _ = writeln!(
            summary,
            "work clock {:.2} ms ({:.2} ms/s, {:.3} ms a frame) at {}; {:.2} ms charged outside any instruction",
            ms(total_work),
            if seconds > 0.0 { ms(total_work) / seconds } else { 0.0 },
            ms(total_work) / frames,
            fidelity["prices"].as_str().unwrap_or_default(),
            ms(vm_results.engine_work)
        );
        let _ = writeln!(
            summary,
            "of it: cold and first-use charges {:.2} ms, the collector {:.2} ms, dispatch of handlers and resumptions {:.2} ms; priced from a client {:.0} %, from Studio-era measurements {:.0} %, borrowed (defaulted) {:.0} %",
            ms(kinds_total[vm::COLD]),
            ms(kinds_total[vm::COLLECTOR]),
            ms(kinds_total[vm::DISPATCH]),
            100.0 * (total_work - kinds_total[vm::STUDIO] - kinds_total[vm::DEFAULTED]).max(0.0) / total_work.max(1e-12),
            100.0 * kinds_total[vm::STUDIO] / total_work.max(1e-12),
            100.0 * kinds_total[vm::DEFAULTED] / total_work.max(1e-12)
        );
        if fresh + big > 0 {
            let _ = writeln!(
                summary,
                "unpriced: {} of new buffers past Luau's own pages and {} moved by buffer copies, fills and string writes of over 64 KB a call: the clock prices their bytes as a small buffer's and their memory past a core's caches at nothing, so it reads this work low",
                bytes(fresh as f64),
                bytes(big as f64)
            );
        }
        // (what the clock is beside a client's own time: read before taking its milliseconds as a client's)
        let _ = writeln!(summary, "{}", fidelity["text"].as_str().unwrap_or_default());
        let _ = writeln!(summary, "instructions: {} by scripts, {} by the engine's own Luau", group_digits(script_insns), group_digits(engine_insns));
        if natives.is_empty() {
            let _ = writeln!(summary, "natively compiled functions: none ran natively (every instruction is counted)");
        } else {
            let entries: u64 = natives.iter().map(|f| f.native_entries).sum();
            let work: f64 = natives.iter().map(|f| f.native_work).sum();
            let named: Vec<String> = natives.iter().take(6).map(|f| format!("{}:{}", names.short(&f.source), f.line)).collect();
            let _ = writeln!(
                summary,
                "natively compiled functions: {} seen ({}{}): {} {} from interpreted code, {:.3} ms of work booked while they ran; their instructions are not counted, and a call from native code into native code is not seen (its work stays with the caller)",
                natives.len(),
                named.join(", "),
                if natives.len() > named.len() { ", ..." } else { "" },
                group_digits(entries),
                if entries == 1 { "entry" } else { "entries" },
                ms(work)
            );
        }
        if !input.marks.is_empty() {
            // (each mark's work clock reading to the next one's, the last to the run's end)
            let mut parts = Vec::new();
            for (index, mark) in input.marks.iter().enumerate() {
                let end = input.marks.get(index + 1).map_or(input.end_work, |next| next.work);
                parts.push(format!("{} (frame {}) {:.2} ms", mark.phase, mark.frame, ms((end - mark.work).max(0.0))));
            }
            let _ = writeln!(summary, "phases' work: {}", parts.join("; "));
        }
        // the tables below cover the reported phases (--profile-phase) and the game's code; the harness's comes apart
        let every_phase = scope.len() >= input.phases.len().max(1);
        let scope_label = if every_phase {
            "every phase".to_string()
        } else {
            format!("{} {}", if scope.len() == 1 { "phase" } else { "phases" }, scope.iter().map(|phase| phase_name(*phase)).collect::<Vec<_>>().join(", "))
        };
        let _ = writeln!(
            summary,
            "reported below: {scope_label}: the game's code {:.2} ms, the harness's code {:.2} ms of self work (the harness's is its code under {HARNESS_FOLDER} and what that called; --profile-phase picks phases, or all)",
            ms(game_self),
            ms(harness_self)
        );
        if let Some(ops) = &scope_engine {
            let _ = writeln!(summary, "{}", engine_line(&scope_label, ops));
        }
        // one side's functions in the reported phases: (id, self work, total work, calls, self instructions)
        // each game function's cold charges and saving in the reported phases
        let mut game_cold: FxHashMap<i32, ColdAndSaving> = FxHashMap::default();
        for (&(id, phase, harness_side), split) in &model.phase_split {
            if harness_side || !scope.contains(&phase) {
                continue;
            }
            let work = model.phase_self.get(&(id, phase, harness_side)).map_or(0.0, |figures| figures.0);
            let row = game_cold.entry(id).or_default();
            row.cold += split.cold();
            row.saving += split.saving(work);
        }
        let side_rows = |harness: bool| -> Vec<(i32, f64, f64, u64, u64)> {
            let mut rows: FxHashMap<i32, (f64, f64, u64, u64)> = FxHashMap::default();
            for (&(id, phase, side), &(work, calls, insns)) in &model.phase_self {
                if side == harness && scope.contains(&phase) && !names.engine(id) && !names.binding(id) {
                    let row = rows.entry(id).or_default();
                    row.0 += work;
                    row.2 += calls;
                    row.3 += insns;
                }
            }
            for (&(id, phase, side), &total) in &model.phase_total {
                if side == harness && scope.contains(&phase) && !names.engine(id) {
                    rows.entry(id).or_default().1 += total;
                }
            }
            let mut rows: Vec<(i32, f64, f64, u64, u64)> = rows.into_iter().map(|(id, (work, total, calls, insns))| (id, work, total, calls, insns)).collect();
            rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            rows
        };
        let (game_rows, harness_rows) = (side_rows(false), side_rows(true));
        let top = input.config.top;
        let _ = writeln!(
            summary,
            "\ntop functions by self work: the game's code, {scope_label} (ms of the work clock; cold: its cold and first-use charges; saving: what cutting it saves, its hot work and the cold charges for objects nothing else touched that frame; total includes callees)"
        );
        let _ = writeln!(summary, "{:>9} {:>8} {:>9} {:>9} {:>10} {:>12}  function", "self ms", "cold ms", "saving ms", "total ms", "calls", "insns");
        // (rows that print as zero are left out)
        for (id, work, total, calls, insns) in game_rows.iter().filter(|row| row.1 >= 5e-7).take(top) {
            let ColdAndSaving { cold, saving } = game_cold.get(id).copied().unwrap_or(ColdAndSaving { cold: 0.0, saving: *work });
            let _ = writeln!(summary, "{:>9.3} {:>8.3} {:>9.3} {:>9.3} {:>10} {:>12}  {}", ms(*work), ms(cold), ms(saving), ms(*total), group_digits(*calls), group_digits(*insns), names.label(*id));
        }
        let mut by_total: Vec<&(i32, f64, f64, u64, u64)> = game_rows.iter().filter(|row| row.0 >= 0).collect();
        by_total.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        let _ = writeln!(summary, "\ntop functions by total work: the game's code, {scope_label}");
        let _ = writeln!(summary, "{:>9} {:>9} {:>10}  function", "total ms", "self ms", "calls");
        for (id, work, total, calls, _) in by_total.iter().filter(|row| row.2 >= 5e-7).take(top.min(12)) {
            let _ = writeln!(summary, "{:>9.3} {:>9.3} {:>10}  {}", ms(*total), ms(*work), group_digits(*calls), names.label(*id));
        }
        let harness_rows: Vec<&(i32, f64, f64, u64, u64)> = harness_rows.iter().filter(|row| row.1 >= 5e-7).collect();
        if !harness_rows.is_empty() {
            let _ = writeln!(summary, "\nthe harness's code, {scope_label}: {:.3} ms of self work, left out of the tables above", ms(harness_self));
            let _ = writeln!(summary, "{:>9} {:>9} {:>10}  function", "self ms", "total ms", "calls");
            for (id, work, total, calls, _) in harness_rows.iter().take(top.min(8)) {
                let _ = writeln!(summary, "{:>9.3} {:>9.3} {:>10}  {}", ms(*work), ms(*total), group_digits(*calls), names.label(*id));
            }
        }
        // functions -O2 inlined: no frame of theirs runs there, so the tables above count only their other calls
        let mut inlined: Vec<&(i32, Figures)> = function_rows.iter().filter(|(id, _)| inlined_into.contains_key(id) && !names.engine(*id) && !names.harness(*id)).collect();
        if !inlined.is_empty() {
            inlined.sort_by(|a, b| b.1.inlined_work.total_cmp(&a.1.inlined_work).then(b.1.inlined_calls.cmp(&a.1.inlined_calls)).then(a.0.cmp(&b.0)));
            let _ = writeln!(
                summary,
                "\ninlined functions (Luau's -O2 compiled their bodies into their callers: a call is counted where a copy was entered; the instructions and work are on their own lines in the callers, counted there too)"
            );
            let _ = writeln!(summary, "{:>10} {:>10} {:>12} {:>9}  function  (inlined into)", "inlined", "called", "insns", "work ms");
            for (id, figures) in inlined.iter().take(top) {
                let into: Vec<String> = inlined_into[id]
                    .iter()
                    .take(3)
                    .map(|(caller, copies)| format!("{} ({} {})", names.label(*caller).replace("  ", " "), copies.copies, if copies.copies == 1 { "copy" } else { "copies" }))
                    .collect();
                let more = inlined_into[id].len().saturating_sub(into.len());
                let _ = writeln!(
                    summary,
                    "{:>10} {:>10} {:>12} {:>9.3}  {}  ({}{})",
                    group_digits(figures.inlined_calls),
                    group_digits(figures.calls),
                    group_digits(figures.inlined_insns),
                    ms(figures.inlined_work),
                    names.label(*id),
                    into.join(", "),
                    if more > 0 { format!(", {more} more") } else { String::new() }
                );
            }
        }
        // each line's figures in the reported phases, the heaviest first
        let mut scoped_lines: Vec<ScopedLine> = Vec::new();
        for &(&(function, line), figures) in line_rows.iter() {
            scoped_lines.push(ScopedLine { function, line, figures, phase: figures.within(&scope) });
        }
        scoped_lines.sort_by(|a, b| b.phase.self_work().total_cmp(&a.phase.self_work()).then((a.function, a.line).cmp(&(b.function, b.line))));
        let _ = writeln!(summary, "\ntop lines by self work: the game's code, {scope_label}");
        let _ = writeln!(summary, "{:>9} {:>8} {:>11} {:>12}  line  (opcodes)", "self ms", "cold ms", "total ms", "insns");
        let is_game_line = |line: &&ScopedLine| {
            line.phase.self_work() >= 5e-7 && !names.engine(line.function) && !names.harness(line.function) && !names.binding(line.function)
        };
        for ScopedLine { function, line, figures, phase } in scoped_lines.iter().filter(is_game_line).take(top) {
            let ops: Vec<String> = {
                let mut ops: Vec<_> = figures.ops.iter().collect();
                ops.sort_by(|a, b| b.1.cmp(a.1));
                ops.iter().take(4).map(|(op, count)| format!("{} {}", vm::opcode_name(**op), group_digits(**count))).collect()
            };
            let module = names.function(*function).map_or(String::new(), |f| names.short(&f.source).to_string());
            let _ = writeln!(summary, "{:>9.3} {:>8.3} {:>11.3} {:>12}  {module}:{line}  ({})", ms(phase.self_work()), ms(phase.cold), ms(phase.total_work), group_digits(phase.insns), ops.join(", "));
        }
        attribution_sections(input, &names, &mut summary, &mut json, top);
        // inline caches
        if input.config.kinds.has(super::Kinds::CACHES) {
            inline_caches(input, vm_results, &names, top, &mut summary, &mut json);
        }
    }

    // the work clock's price of one count of the counters an issue prices what it names by (ns, at desktop prices: a phone run's
    // are these times work.units.scale): an allocated object, a byte of table storage, a key a table's growth rehashed, a C
    // function a script called, a comparison table.sort made and a call of its comparison function; a byte any script work
    // allocated, a key a growth put back into a hash part, a block over 1 KB, an object a library call made, and the
    // collector's charge an object (in work.collector_ms, not in the others)
    if json["work"].is_object() {
        let price = |set: fn(&mut crate::cost::VmCounters)| -> f64 {
            let mut counters: crate::cost::VmCounters = unsafe { std::mem::zeroed() };
            set(&mut counters);
            crate::cost::luau(&counters) * 1e9
        };
        json["work"]["prices"] = json!({
            "object_ns": price(|c| c.objects = 1),
            "stored_byte_ns": price(|c| c.stored = 1),
            "rehashed_key_ns": price(|c| c.rehashed = 1),
            "c_call_ns": price(|c| c.ccalls = 1),
            "sort_compare_ns": price(|c| c.sortcompares = 1),
            "sort_call_ns": price(|c| c.sortcalls = 1),
            "allocated_byte_ns": price(|c| c.hostops[crate::cost::ALLOCBYTES_SLOT] = 1),
            "hash_rehashed_key_ns": price(|c| c.hostops[crate::cost::HASHREHASHED_SLOT] = 1),
            "large_block_ns": price(|c| c.hostops[crate::cost::LARGEBLOCKS_SLOT] = 1),
            "library_object_ns": price(|c| c.hostops[crate::cost::LIBOBJECTS_SLOT] = 1),
            "collector_object_ns": price(|c| c.hostops[crate::cost::GCOBJECTS_SLOT] = 1),
        });
    }

    // host time
    if let Some(time) = input.time {
        let cpu = time.cycles_per_ms > 0.0;
        json["time"] = json!({
            "sampled_ms": time_total, "wall_ms": wall_total, "weight": if cpu { "thread CPU" } else { "wall" }, "cycles_per_ms": time.cycles_per_ms,
            "about": "time_ms is the run thread's CPU milliseconds (its cycles, the time it ran), wall_ms the wall clock's; a sample is booked to what ran since the one before: the Luau stack at a safepoint, the Luau that ran last when the interpreter is left, the engine's step that was open when it is entered (else outside scripts), an Instance API call on its own line",
        });
        let _ = writeln!(
            summary,
            "\nhost time: {:.1} ms {} ({:.1} ms wall), sampled each millisecond at the VM's calls, returns and loop edges, at the interpreter's entry and exit, around the engine's steps and the Instance API's calls",
            time_total,
            if cpu { "of the run thread's CPU" } else { "(wall: the thread's cycles cannot be read here)" },
            wall_total
        );
        let mut by_self: Vec<&(i32, Figures)> = function_rows.iter().filter(|(_, f)| f.self_time > 0.0).collect();
        by_self.sort_by(|a, b| b.1.self_time.total_cmp(&a.1.self_time).then(a.0.cmp(&b.0)));
        let _ = writeln!(summary, "{:>9} {:>9} {:>9}  function", "self ms", "total ms", "wall ms");
        for (id, figures) in by_self.iter().take(input.config.top) {
            let _ = writeln!(summary, "{:>9.1} {:>9.1} {:>9.1}  {}", figures.self_time, figures.total_time, figures.self_wall, names.label(*id));
        }
        // functions -O2 inlined into their callers: their lines' time there (also their callers' own)
        let mut inlined: Vec<&(i32, Figures)> = function_rows.iter().filter(|(id, f)| f.inlined_time > 0.0 && !names.engine(*id)).collect();
        if !inlined.is_empty() {
            inlined.sort_by(|a, b| b.1.inlined_time.total_cmp(&a.1.inlined_time).then(a.0.cmp(&b.0)));
            let _ = writeln!(summary, "inlined functions' host time, an estimate (each sample's share of the code since the safepoint before it, by the work clock's prices of its opcodes, in the copies -O2 compiled into their callers; a sample right after a call that reached a C function is not split; their callers' self time holds it)");
            for (id, figures) in inlined.iter().take(input.config.top.min(10)) {
                let _ = writeln!(summary, "{:>9.1} {:>9} {:>9.1}  {} (inlined)", figures.inlined_time, "", figures.inlined_wall, names.label(*id));
            }
        }
    }

    // allocations
    if let Some(allocations) = input.allocations {
        let mut by_type: Vec<(String, memory::Amount)> = allocations.types.names.iter().cloned().zip(allocations.by_type.iter().copied()).collect();
        by_type.sort_by(|a, b| b.1.bytes.total_cmp(&a.1.bytes));
        let mut groups: BTreeMap<String, memory::Amount> = BTreeMap::new();
        for (name, amount) in &by_type {
            let entry = groups.entry(memory::group(name).to_string()).or_default();
            entry.bytes += amount.bytes;
            entry.count += amount.count;
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|a, b| b.1.bytes.total_cmp(&a.1.bytes));
        let total = allocations.bytes as f64;
        let _ = writeln!(
            summary,
            "\nallocations: {} in {} objects ({}){}",
            bytes(total),
            group_digits(allocations.objects),
            groups.iter().take(6).map(|(name, amount)| format!("{name} {}", percent(amount.bytes, total))).collect::<Vec<_>>().join(", "),
            if allocations.step > 1 { format!("; stacks sampled every {} bytes", allocations.step) } else { String::new() }
        );
        let mut stacks: FxHashMap<(u32, u32), memory::Amount> = FxHashMap::default();
        for (&(_, stack, ty), amount) in &allocations.by_stack {
            let entry = stacks.entry((stack, ty)).or_default();
            entry.bytes += amount.bytes;
            entry.count += amount.count;
        }
        let mut stack_rows: Vec<_> = stacks.into_iter().collect();
        stack_rows.sort_by(|a, b| b.1.bytes.total_cmp(&a.1.bytes));
        let describe = |stack: u32| -> String {
            let frames = &allocations.stacks.stacks[stack as usize];
            let luau: Vec<String> = frames.iter().filter(|frame| frame.0 >= 0 && !names.engine(frame.0)).take(3).map(|&(function, line)| {
                let module = names.function(function).map_or(String::new(), |f| names.short(&f.source).to_string());
                format!("{module}:{line}")
            }).collect();
            let top_c = frames.first().filter(|frame| frame.0 < -1).map(|frame| format!("{} < ", names.frame(frame.0))).unwrap_or_default();
            format!("{top_c}{}", if luau.is_empty() { "(engine)".into() } else { luau.join(" < ") })
        };
        let _ = writeln!(summary, "{:>10} {:>10}  type  stack (innermost Luau lines first)", "bytes", "count");
        for ((stack, ty), amount) in stack_rows.iter().take(input.config.top) {
            let _ = writeln!(summary, "{:>10} {:>10}  {}  {}", bytes(amount.bytes), group_digits(amount.count.round() as u64), allocations.types.names[*ty as usize], describe(*stack));
        }
        json["alloc"] = json!({
            "bytes": allocations.bytes,
            "objects": allocations.objects,
            "step": allocations.step,
            "by_type": by_type.iter().map(|(name, amount)| json!({"type": name, "group": memory::group(name), "bytes": amount.bytes, "count": amount.count})).collect::<Vec<_>>(),
            "by_stack": stack_rows.iter().take(500).map(|((stack, ty), amount)| json!({
                "frames": allocations.stacks.stacks[*stack as usize].iter().map(|&(f, l)| json!([f, l])).collect::<Vec<_>>(),
                "type": allocations.types.names[*ty as usize], "bytes": amount.bytes.round(), "count": amount.count.round(),
            })).collect::<Vec<_>>(),
        });
        // tables that rehashed to grow: by the line that made them rehash and the site that made the table
        let total = &allocations.rehashes;
        if total.events > 0 {
            let _ = writeln!(
                summary,
                "table rehashes: {} ({} grew the table), {} keys moved; by scripts' own instructions {} rehashes, {} keys",
                group_digits(total.events),
                group_digits(total.grew),
                group_digits(total.keys),
                group_digits(total.script_events),
                group_digits(total.script_keys)
            );
            let mut rows: Vec<(&(u32, u32, u32), &memory::Rehash)> = allocations.rehash.iter().collect();
            rows.sort_by(|a, b| b.1.keys.cmp(&a.1.keys).then(b.1.events.cmp(&a.1.events)).then(a.0.cmp(b.0)));
            let _ = writeln!(summary, "{:>10} {:>12}  rehashed at  (the table made at)", "rehashes", "keys moved");
            for ((_, stack, made), rehash) in rows.iter().filter(|(key, _)| key.1 != memory::NO_STACK).take(input.config.top.min(10)) {
                let made = if *made == memory::NO_STACK { "not followed".to_string() } else { describe(*made) };
                let _ = writeln!(summary, "{:>10} {:>12}  {}  ({made})", group_digits(rehash.events), group_digits(rehash.keys), describe(*stack));
            }
            let frames = |stack: u32| -> Json {
                if stack == memory::NO_STACK { Json::Null } else { Json::Array(allocations.stacks.stacks[stack as usize].iter().map(|&(f, l)| json!([f, l])).collect()) }
            };
            json["alloc"]["rehash"] = json!({
                "events": total.events, "keys": total.keys, "script_events": total.script_events, "script_keys": total.script_keys, "grew": total.grew,
                "bytes_before": total.bytes_before, "bytes_after": total.bytes_after,
                "sites": rows.iter().take(500).map(|((phase, stack, made), rehash)| json!({
                    "phase": input.phases.get(*phase as usize), "frames": frames(*stack), "table_frames": frames(*made),
                    "events": rehash.events, "keys": rehash.keys, "script_events": rehash.script_events, "script_keys": rehash.script_keys,
                    "grew": rehash.grew, "bytes_before": rehash.bytes_before, "bytes_after": rehash.bytes_after,
                })).collect::<Vec<_>>(),
            });
        }
    }

    // heap snapshots and the collector
    // (each table site's rehashes over the run: (rehashes, keys moved) by the stack that made the tables)
    let mut rehash_by_site: FxHashMap<u32, (u64, u64)> = FxHashMap::default();
    if let Some(allocations) = input.allocations {
        for (&(_, _, made), rehash) in &allocations.rehash {
            if made != memory::NO_STACK {
                let entry = rehash_by_site.entry(made).or_default();
                entry.0 += rehash.events;
                entry.1 += rehash.keys;
            }
        }
    }
    if !input.snapshots.is_empty() {
        let last = input.snapshots.last().unwrap();
        let _ = writeln!(
            summary,
            "\nheap at {}: {} reachable in {} objects, what a full collection keeps; the run's own allocations still reachable: {}",
            last.label,
            bytes(last.reachable.1 as f64),
            group_digits(last.reachable.0),
            bytes(last.allocated_since.1),
        );
        let _ = writeln!(
            summary,
            "  not swept yet: {} in {} objects, garbage the collector had not freed (not in the reachable figures; gcinfo() {} KB counts both)",
            bytes(last.unswept.1 as f64),
            group_digits(last.unswept.0),
            last.total_bytes / 1024
        );
        let _ = writeln!(summary, "  reachable by type: {}", last.by_type.iter().take(8).map(|(name, count, size)| format!("{name} {} ({})", bytes(*size as f64), group_digits(*count))).collect::<Vec<_>>().join(", "));
        for held in last.biggest.iter().take(input.config.top.min(8)) {
            let _ = writeln!(summary, "  {:>10} kept alive by {} {}  ({})", bytes(held.retained as f64), held.ty, if held.name.is_empty() { String::new() } else { format!("\"{}\"", held.name) }, held.path);
        }
        if input.snapshots.len() > 1 {
            let _ = writeln!(
                summary,
                "  snapshots, reachable (+ not swept yet): {}",
                input.snapshots.iter().map(|s| format!("{} {} (+{})", s.label, bytes(s.reachable.1 as f64), bytes(s.unswept.1 as f64))).collect::<Vec<_>>().join(" -> ")
            );
        }
        // the reachable tables' shapes by the site that made them
        let tables = &last.tables;
        if tables.count > 0 {
            let whole = tables.bytes.max(1) as f64;
            let _ = writeln!(
                summary,
                "  tables at {}: {} reachable, {} (the tables {}, array parts {}, hash parts {}); the run's own by the site that made them:",
                last.label,
                group_digits(tables.count),
                bytes(tables.bytes as f64),
                percent(tables.header_bytes as f64, whole),
                percent(tables.array_bytes as f64, whole),
                percent(tables.hash_bytes as f64, whole)
            );
            let _ = writeln!(summary, "  {:>9} {:>10} {:>8} {:>16} {:>16} {:>6}  site", "tables", "bytes", "each", "array slots", "hash nodes", "levels");
            let site_of = |stack: u32| -> String {
                let Some(allocations) = input.allocations else { return String::new() };
                let frames = &allocations.stacks.stacks[stack as usize];
                let luau: Vec<String> = frames.iter().filter(|frame| frame.0 >= 0 && !names.engine(frame.0)).take(3).map(|&(function, line)| {
                    let module = names.function(function).map_or(String::new(), |f| names.short(&f.source).to_string());
                    format!("{module}:{line}")
                }).collect();
                if luau.is_empty() { "(engine)".into() } else { luau.join(" < ") }
            };
            let shape = |distribution: &memory::Distribution| -> String {
                if distribution.min == distribution.max {
                    format!("{} ({})", group_digits(distribution.min as u64), group_digits(distribution.used))
                } else {
                    format!("{}-{} ({})", group_digits(distribution.min as u64), group_digits(distribution.max as u64), group_digits(distribution.used))
                }
            };
            for site in tables.sites.iter().take(input.config.top.min(8)) {
                let levels = if site.levels.min == site.levels.max { site.levels.min.to_string() } else { format!("{}-{}", site.levels.min, site.levels.max) };
                let _ = writeln!(
                    summary,
                    "  {:>9} {:>10} {:>8} {:>16} {:>16} {:>6}  {}",
                    group_digits(site.count.round() as u64),
                    bytes(site.bytes),
                    bytes(site.bytes / site.count.max(1e-9)),
                    shape(&site.array),
                    shape(&site.hash),
                    levels,
                    site_of(site.stack)
                );
            }
        }
        let collector = input.collector;
        let _ = writeln!(
            summary,
            "collector: {} cycles, {} steps, {} freed, {:.1} ms host time",
            collector.cycles,
            group_digits(collector.steps),
            bytes(collector.freed as f64),
            ms(collector.seconds)
        );
        json["heap"] = json!({
            "snapshots": input.snapshots.iter().map(|snapshot| json!({
                "label": snapshot.label, "frame": snapshot.frame, "seconds": snapshot.seconds, "gcinfo_kb": snapshot.total_bytes / 1024, "total_bytes": snapshot.total_bytes,
                "objects": snapshot.objects, "object_bytes": snapshot.object_bytes,
                "reachable": {"count": snapshot.reachable.0, "bytes": snapshot.reachable.1},
                "unswept": {"count": snapshot.unswept.0, "bytes": snapshot.unswept.1,
                    "by_type": snapshot.unswept_by_type.iter().map(|(name, count, size)| json!({"type": name, "count": count, "bytes": size})).collect::<Vec<_>>()},
                "by_type": snapshot.by_type.iter().map(|(name, count, size)| json!({"type": name, "count": count, "bytes": size})).collect::<Vec<_>>(),
                "allocated_since": {"count": snapshot.allocated_since.0.round(), "bytes": snapshot.allocated_since.1.round()},
                "biggest": snapshot.biggest.iter().map(|held| json!({"type": held.ty, "name": held.name, "size": held.size, "retained": held.retained, "path": held.path,
                    "stack": held.stack.and_then(|stack| input.allocations.map(|allocations| allocations.stacks.stacks[stack as usize].iter().map(|&(f, l)| json!([f, l])).collect::<Vec<_>>()))})).collect::<Vec<_>>(),
                "by_stack": snapshot.by_stack.iter().take(300).map(|(stack, ty, count, size)| json!({
                    "frames": input.allocations.map(|allocations| allocations.stacks.stacks[*stack as usize].iter().map(|&(f, l)| json!([f, l])).collect::<Vec<_>>()),
                    "type": ty, "count": count.round(), "bytes": size.round()})).collect::<Vec<_>>(),
                "tables": {
                    "count": snapshot.tables.count, "bytes": snapshot.tables.bytes, "header_bytes": snapshot.tables.header_bytes,
                    "array_bytes": snapshot.tables.array_bytes, "hash_bytes": snapshot.tables.hash_bytes,
                    "sizes": {"table": snapshot.tables.sizes.0, "slot": snapshot.tables.sizes.1, "node": snapshot.tables.sizes.2},
                    "sites": snapshot.tables.sites.iter().take(300).map(|site| {
                        let (events, keys) = rehash_by_site.get(&site.stack).copied().unwrap_or((0, 0));
                        json!({
                            "phase": input.phases.get(site.phase as usize),
                            "frames": input.allocations.map(|allocations| allocations.stacks.stacks[site.stack as usize].iter().map(|&(f, l)| json!([f, l])).collect::<Vec<_>>()),
                            "count": site.count.round(), "bytes": site.bytes.round(), "header_bytes": site.header_bytes.round(),
                            "array_bytes": site.array_bytes.round(), "hash_bytes": site.hash_bytes.round(),
                            "array": distribution_json(&site.array), "hash": distribution_json(&site.hash), "levels": levels_json(&site.levels),
                            "rehashes": events, "rehash_keys": keys,
                        })
                    }).collect::<Vec<_>>(),
                },
            })).collect::<Vec<_>>(),
            "gc": {"cycles": collector.cycles, "steps": collector.steps, "freed": collector.freed, "ms": ms(collector.seconds),
                "by_state": memory::GC_STATES.iter().zip(collector.by_state.iter()).map(|(state, (steps, seconds))| json!({"state": state, "steps": steps, "ms": ms(*seconds)})).collect::<Vec<_>>()},
        });
    }

    // what the engine's caches say of the game, then ree's own caches (how fast ree runs, not what the game costs)
    if input.config.kinds.has(super::Kinds::CACHES) {
        let facts = input.facts;
        let _ = writeln!(
            summary,
            "\ngame facts (counted by the engine): {} instances touched by scripts ({} userdata made, {} collected); {} modules ran ({} requires served by a kept result); {} datatype property reads, a new value each on a client (not measured against one): ree handed back the value it had made for {} of them",
            group_digits(facts.instances_touched),
            group_digits(facts.userdata_made),
            group_digits(facts.userdata_collected),
            group_digits(facts.modules_run),
            group_digits(facts.requires_kept),
            group_digits(facts.datatype_reads),
            group_digits(facts.datatype_reads_reused)
        );
        json["game"] = json!({
            "instances_touched": facts.instances_touched, "userdata_made": facts.userdata_made, "userdata_collected": facts.userdata_collected,
            "modules_run": facts.modules_run, "requires_kept": facts.requires_kept,
            "datatype_reads": facts.datatype_reads, "datatype_reads_reused": facts.datatype_reads_reused,
            "about": {
                "instances_touched": "instances a userdata was made for: the ones the scripts reached (a client makes one for each too)",
                "userdata_made": "userdata made for instances, an instance's again after the VM collected its first",
                "modules_run": "ModuleScripts that ran: a require that ran its module",
                "datatype_reads": "UDim2, Color3, ... property reads: a client makes a new value for each read (not measured against a client); ree hands back the value it made while the property is unchanged (datatype_reads_reused), so rawequal of two reads is true in ree",
            },
        });
        let _ = writeln!(summary, "\nemulator: ree's own caches (how fast ree runs, not what the game costs)");
        let _ = writeln!(summary, "{:<20} {:>8} {:>12} {:>10} {:>10}", "cache", "hit", "lookups", "misses", "evicted");
        for (name, _, counts) in input.caches {
            let lookups = counts.hits + counts.misses;
            if lookups == 0 && counts.evictions == 0 {
                continue;
            }
            let _ = writeln!(summary, "{:<20} {:>8} {:>12} {:>10} {:>10}", name, percent(counts.hits as f64, lookups as f64), group_digits(lookups), group_digits(counts.misses), group_digits(counts.evictions));
        }
        json["emulator"] = json!({
            "about": "ree's own caches: they measure how fast ree runs, not what the game costs",
            "caches": input.caches.iter().map(|(name, about, counts)| json!({"name": name, "about": about, "hits": counts.hits, "misses": counts.misses, "evictions": counts.evictions})).collect::<Vec<_>>(),
        });
    }

    // the CPU's figures: this run's own, or the cpu pass's (a run a kind slows took them in a process of its own)
    let pass = input.config.cpu_pass.as_ref().and_then(|folder| std::fs::read_to_string(folder.join("profile.json")).ok()).and_then(|text| serde_json::from_str::<Json>(&text).ok());
    // the cpu frames by frame: (host ms, cycles), this run's or the pass's
    let mut cpu_by_frame: FxHashMap<u64, (f64, u64)> = input.cpu_frames.iter().map(|frame| (frame.frame, (frame.host_ms, frame.cycles))).collect();
    if let Some(pass) = &pass {
        let frames = &pass["cpu"]["frames"];
        let column = |name: &str| frames["columns"].as_array().and_then(|columns| columns.iter().position(|column| column == name));
        if let (Some(frame), Some(host), Some(cycles)) = (column("frame"), column("host_ms"), column("cycles")) {
            for row in frames["rows"].as_array().into_iter().flatten() {
                if let (Some(number), Some(host_ms), Some(count)) = (row[frame].as_u64(), row[host].as_f64(), row[cycles].as_u64()) {
                    cpu_by_frame.insert(number, (host_ms, count));
                }
            }
        }
    }

    // the timeline
    if !input.rows.is_empty() {
        let rows = input.rows;
        // (a frame a phase mark split is one frame here: its parts summed)
        struct Whole {
            frame: u64,
            work: f64,
            sides: Option<(f64, f64)>,
        }
        let mut frames_work: Vec<Whole> = Vec::new();
        for row in rows {
            let work = row.work_ms * price_scale();
            let sides = row_sides_ms(row);
            match frames_work.last_mut().filter(|last| last.frame == row.frame && row.part > 1) {
                Some(last) => {
                    last.work += work;
                    last.sides = add_sides(last.sides, sides);
                }
                None => frames_work.push(Whole { frame: row.frame, work, sides }),
            }
        }
        let count = frames_work.len() as f64;
        let (worst, worst_work) = frames_work.iter().fold((0, 0.0), |best, whole| if whole.work > best.1 { (whole.frame, whole.work) } else { best });
        let mean = frames_work.iter().map(|whole| whole.work).sum::<f64>() / count;
        let sides = frames_work.iter().all(|whole| whole.sides.is_some()).then(|| frames_work.iter().filter_map(|whole| whole.sides).fold((0.0, 0.0), |sum, (game, harness)| (sum.0 + game / count, sum.1 + harness / count)));
        let (first, last) = (&rows[0], rows.last().unwrap());
        // (a frame's host time and cycles: its own, else the cpu pass's for the same frame, else none: a kind slowed the run)
        let host = |row: &Row| row.host_ms.map(|host| (host, row.cycles.unwrap_or(0))).or_else(|| (row.part == 0).then(|| cpu_by_frame.get(&row.frame).copied()).flatten());
        let mut own_hosts: BTreeMap<u64, Option<f64>> = BTreeMap::new();
        for row in rows {
            let whole = own_hosts.entry(row.frame).or_insert(Some(0.0));
            *whole = whole.zip(row.host_ms).map(|(before, part)| before + part);
        }
        let hosts: Vec<f64> = own_hosts.iter().filter_map(|(frame, own)| own.or_else(|| cpu_by_frame.get(frame).map(|(host, _)| *host))).collect();
        let host_text = if hosts.len() == count as usize {
            format!("host a frame mean {:.2} ms{}", hosts.iter().sum::<f64>() / count, if rows[0].host_ms.is_none() { " (the cpu pass's)" } else { "" })
        } else {
            "host time a frame not taken (a kind slowed the run: --profile cpu takes it in a pass of its own)".to_string()
        };
        let threads_made: u64 = rows.iter().map(|row| row.threads_made).sum();
        let reachable_threads: Vec<String> = input.snapshots.iter().filter_map(|snapshot| snapshot.by_type.iter().find(|(name, _, _)| name == "thread").map(|(_, count, _)| format!("{} {}", snapshot.label, group_digits(*count)))).collect();
        let detailed = input.config.kinds.has(super::Kinds::TIMELINE);
        if detailed {
            let _ = writeln!(
                summary,
                "\ntimeline ({} frames{}): work a frame mean {:.3} ms{}, max {:.3} ms (frame {}); {host_text}; VM heap with its garbage {} -> {}; instances in the DataModel {} -> {}, alive outside it {} -> {}, destroyed awaiting collection {} -> {}; connections {} -> {}; threads made {} ({:.1} a frame){}",
                count as usize,
                if rows.len() > count as usize { format!(", {} rows split by phase marks", rows.len()) } else { String::new() },
                mean,
                sides.map_or(String::new(), |(game, harness)| format!(" (the game's code {game:.3} ms, the harness's {harness:.3} ms)")),
                worst_work,
                worst,
                bytes(first.heap_bytes as f64),
                bytes(last.heap_bytes as f64),
                group_digits(first.instances.0),
                group_digits(last.instances.0),
                group_digits(first.instances.1),
                group_digits(last.instances.1),
                group_digits(first.instances.2),
                group_digits(last.instances.2),
                group_digits(first.connections),
                group_digits(last.connections),
                group_digits(threads_made),
                threads_made as f64 / count,
                if reachable_threads.is_empty() { String::new() } else { format!("; threads reachable at the heap snapshots: {}", reachable_threads.join(", ")) }
            );
        } else {
            let _ = writeln!(summary, "\nframe work ({} frames): mean {mean:.3} ms, max {worst_work:.3} ms (frame {worst})", count as usize);
        }
        let split = rows.iter().all(|row| row.sides.is_some());
        json["timeline"] = json!({
            "columns": ["frame", "seconds", "phase", "host_ms", "work_ms", "instructions", "heap_bytes", "allocated_bytes", "allocations", "gc_steps", "gc_ms", "gc_freed", "instances_in_datamodel", "instances_outside", "instances_destroyed", "connections", "threads_made", "cycles", "part", "game_ms", "harness_ms", "collector_ms"],
            "about": {
                "host_ms": "the frame's host time and cycles: this run's, else the cpu pass's for the same frame (a kind that slows the run makes its own not the run's), else null",
                "instances_in_datamodel": "live instances at the frame's end in the DataModel's tree; instances_outside: alive outside it; instances_destroyed: destroyed and kept until the VM collects the userdata that holds them",
                "threads_made": "threads the VM made in the frame (coroutines, task threads, handlers' threads); the reachable ones are the heap snapshots' (by_type thread)",
            },
            "rows": rows.iter().map(|row| timeline_row(row, host(row))).collect::<Vec<_>>(),
        });
        if !detailed {
            json["timeline"]["columns"] = json!(["frame", "seconds", "phase", "work_ms", "part", "game_ms", "harness_ms", "collector_ms"]);
            json["timeline"]["rows"] = json!(rows.iter().map(compact_timeline_row).collect::<Vec<_>>());
            json["timeline"]["about"] = json!({"work_ms": "compact frame work for issue summaries and hitch attribution; timeline records the other frame metrics"});
        }
        // Summaries retain small events; heavy frames retain exact node and site contributions
        if split {
            let (stats, heavy, site_frames) = frame_evidence(rows, &scope, input.run.fps);
            json["timeline"]["function_stats"] = stats;
            json["timeline"]["node_frames"] = heavy;
            json["timeline"]["site_frames"] = site_frames;
        }
    }

    if let Some(vm_results) = input.vm {
        price_builtin_fixes(vm_results, input.rows, &scope, input.run.fps, &mut json);
        write_issue_summaries(vm_results, input.rows, &scope, input.run.fps, &mut json);
    }

    // the CPU
    if let Some(pass) = pass.as_ref().filter(|pass| pass["cpu"].is_object()) {
        let mut cpu = pass["cpu"].clone();
        cpu["pass"] = json!(format!("a run of its own ({}): the kinds asked with cpu slow the run, so its figures here would not be the run's", input.config.cpu_pass.as_ref().map(|folder| folder.display().to_string()).unwrap_or_default()));
        let _ = writeln!(summary, "\n{}", pass["cpu"]["text"].as_str().unwrap_or("cpu: (the pass wrote no summary)").trim_end());
        let _ = writeln!(summary, "  (taken in a pass of its own: {})", input.config.cpu_pass.as_ref().map(|folder| folder.display().to_string()).unwrap_or_default());
        // (the two runs went the same way, or the CPU figures are another run's: a script timeout host time fired in one pass, its
        // backstop counting host time the profiling slows, ends a script the other let run)
        let theirs = &pass["run"];
        let (timeouts, by_host) = (theirs["script_timeouts"].as_u64().unwrap_or(0), theirs["host_backstop_timeouts"].as_u64().unwrap_or(0));
        let (frames, status) = (theirs["frames"].as_u64().unwrap_or(0), theirs["status"].as_str().unwrap_or("?"));
        if timeouts != input.run.script_timeouts || frames != input.run.frames || status != input.run.status {
            let text = format!(
                "the cpu pass went another way than this run: {timeouts} script timeouts there ({by_host} host time fired alone), {} here ({} by host time); {frames} frames ({status}) there, {} ({}) here: the CPU figures are of a run that differs from this one",
                input.run.script_timeouts, input.run.host_backstop_timeouts, input.run.frames, input.run.status
            );
            let _ = writeln!(summary, "  warning: {text}");
            cpu["pass_differs"] = json!(text);
        }
        json["cpu"] = cpu;
    } else if input.config.kinds.has(super::Kinds::CPU) {
        let cpu = input.cpu;
        let rate = cpu.cycles_per_ms;
        let cpu_ms = |cycles: u64| (rate > 0.0).then(|| cycles as f64 / rate);
        // per phase: each mark's thread cycles to the next mark's (the last to the profile's end)
        let mut phases: Vec<(String, f64, Option<f64>)> = Vec::new();
        for (index, mark) in input.marks.iter().enumerate() {
            let Some(next) = input.marks.get(index + 1).or(input.end) else { continue };
            let host = (next.host - mark.host) * 1e3;
            let ms = cpu_ms(next.cycles.saturating_sub(mark.cycles));
            match phases.iter_mut().find(|(phase, _, _)| *phase == mark.phase) {
                Some(entry) => {
                    entry.1 += host;
                    entry.2 = entry.2.zip(ms).map(|(a, b)| a + b);
                }
                None => phases.push((mark.phase.clone(), host, ms)),
            }
        }
        let frames = input.cpu_frames;
        let frame_ms: Vec<f64> = frames.iter().filter_map(|frame| cpu_ms(frame.cycles)).collect();
        let mut text = format!(
            "cpu: the run thread {} of CPU ({} cycles); process CPU {}; page faults {}; working set {}, private bytes {}",
            cpu.thread_ms.map_or("-".into(), |ms| format!("{ms:.1} ms")),
            cpu.thread_cycles.map_or("-".into(), group_digits),
            cpu.process_cpu.map_or("-".into(), |(user, kernel)| format!("{:.2} s user, {:.2} s kernel", user, kernel)),
            cpu.page_faults.map_or("-".into(), group_digits),
            cpu.working_set.map_or("-".into(), |(base, growth)| format!("{} at the run's start, {}{} by its end (most {})", bytes(base as f64), if growth < 0 { "-" } else { "+" }, bytes(growth.unsigned_abs() as f64), cpu.run_peak_working_set.map_or("-".into(), |peak| bytes(peak as f64)))),
            cpu.private_bytes.map_or("-".into(), |(base, growth)| format!("{} {}{}", bytes(base as f64), if growth < 0 { "-" } else { "+" }, bytes(growth.unsigned_abs() as f64)))
        );
        if !phases.is_empty() {
            let _ = write!(text, "\n  thread CPU by phase: {}", phases.iter().map(|(phase, host, ms)| format!("{phase} {} ({host:.1} ms wall)", ms.map_or("-".into(), |ms| format!("{ms:.1} ms")))).collect::<Vec<_>>().join("; "));
        }
        if !frame_ms.is_empty() {
            let max = frame_ms.iter().cloned().fold(0.0, f64::max);
            let _ = write!(text, "\n  thread CPU a frame: mean {:.3} ms, max {:.3} ms over {} frames", frame_ms.iter().sum::<f64>() / frame_ms.len() as f64, max, frame_ms.len());
        }
        let _ = writeln!(summary, "\n{text}");
        if let Some(missing) = cpu.pmc_missing.as_ref().filter(|_| input.hw.is_none()) {
            let _ = writeln!(summary, "hardware counters: not read: {missing}");
        }
        json["cpu"] = json!({
            "text": text,
            "thread_cycles": cpu.thread_cycles, "thread_ms": cpu.thread_ms, "cycles_per_ms": rate,
            "process_cpu_s": cpu.process_cpu.map(|(user, kernel)| json!({"user": user, "kernel": kernel})),
            "page_faults": cpu.page_faults,
            "working_set": cpu.working_set.map(|(base, growth)| json!({"baseline": base, "growth": growth, "run_peak": cpu.run_peak_working_set})),
            "private_bytes": cpu.private_bytes.map(|(base, growth)| json!({"baseline": base, "growth": growth})),
            "phases": phases.iter().map(|(phase, host, ms)| json!({"phase": phase, "thread_ms": ms, "host_ms": host})).collect::<Vec<_>>(),
            "frames": {
                "columns": ["frame", "phase", "host_ms", "cycles", "thread_ms", "working_set"],
                "rows": frames.iter().map(|frame| json!([frame.frame, frame.phase, round(frame.host_ms, 4), frame.cycles, cpu_ms(frame.cycles).map(|ms| round(ms, 4)), frame.working_set])).collect::<Vec<_>>(),
            },
            "about": "from the run's start (its VM's) to the profile's end: the run thread's CPU (its cycles at the counter's rate), by phase from the marks and by frame; memory as the working set and the private bytes at the run's start and their growth",
            "pmc_sources": cpu.pmc_sources, "pmc": cpu.pmc.as_ref().map(|counters| counters.iter().map(|(name, value)| json!({"counter": name, "value": value})).collect::<Vec<_>>()),
            "pmc_missing": cpu.pmc_missing,
        });
    }

    // the hardware counters
    let mut counter_folded: Vec<(String, Vec<(String, u64)>)> = Vec::new();
    if let Some(hw) = input.hw {
        json["counters"] = counters(input, hw, &names, &mut summary, &mut counter_folded);
    }

    if let Some(nodes) = input.phase_tree {
        json["phase_tree"] = Json::Array(nodes.iter().map(|node| json!({"name": node.name, "parent": node.parent, "total_ms": node.seconds * 1e3, "self_ms": node.own * 1e3, "count": node.count})).collect());
    }
    if let Some(text) = input.phase_timing {
        json["phase_timing"] = json!(text);
        let _ = writeln!(summary, "
{text}");
    }
    if let Some(text) = input.boundary {
        json["boundary"] = json!(text);
        let _ = writeln!(summary, "\n{}", text.trim_end());
        files.push("boundary.json".into());
    }

    // sources of the hottest functions, for the annotated listing
    let mut listed: Vec<i32> = Vec::new();
    for (id, figures) in &function_rows {
        if *id < 0 || names.engine(*id) || listed.len() >= 12 {
            continue;
        }
        if figures.self_work > 0.0 || figures.self_time > 0.0 || figures.self_alloc > 0.0 {
            listed.push(*id);
        }
    }
    // (and the inlined functions with the most work in their copies: the page shows it on their own lines)
    let mut inlined_hot: Vec<&(i32, Figures)> = function_rows.iter().filter(|(id, figures)| *id >= 0 && figures.inlined_work > 0.0 && !listed.contains(id) && !names.engine(*id)).collect();
    inlined_hot.sort_by(|a, b| b.1.inlined_work.total_cmp(&a.1.inlined_work).then(a.0.cmp(&b.0)));
    listed.extend(inlined_hot.iter().take(6).map(|(id, _)| *id));
    let mut listings = serde_json::Map::new();
    for id in &listed {
        if let Some(function) = names.function(*id) {
            if let Some((path, rows)) = listing(input, function, &model, *id) {
                listings.insert(id.to_string(), json!({"path": path, "lines": rows.iter().map(|(line, text)| json!([line, text])).collect::<Vec<_>>()}));
            }
        }
    }
    json["listings"] = Json::Object(listings);
    json["source_hashes"] = Json::Object(input.sources.iter().map(|(name, source)| (name.clone(), json!(format!("{:016x}", fxhash(&source.text))))).collect());

    // folded stacks
    let mut folded_files: Vec<(String, Vec<(String, u64)>)> = Vec::new();
    if let Some(vm_results) = input.vm.filter(|vm| !vm.nodes.is_empty()) {
        let (work, insns) = fold_tree(vm_results, &names, input.phases);
        folded_files.push(("work.folded".into(), work));
        folded_files.push(("instructions.folded".into(), insns));
    }
    if let Some(time) = input.time {
        // (milliseconds of host time, the thread's CPU where it can be read, rounded per stack)
        let mut lines: HashMap<String, f64> = HashMap::new();
        for (&(phase, stack), &(ticks, cycles)) in &time.samples {
            *lines.entry(fold(&names, input.phases, phase, &time.stacks.stacks[stack as usize])).or_default() += time.cpu_ms(cycles).unwrap_or(ticks as f64);
        }
        folded_files.push(("time.folded".into(), lines.into_iter().map(|(stack, weight)| (stack, weight.round() as u64)).collect()));
    }
    if let Some(allocations) = input.allocations {
        let mut by_bytes: HashMap<String, f64> = HashMap::new();
        let mut by_count: HashMap<String, f64> = HashMap::new();
        for (&(phase, stack, _), amount) in &allocations.by_stack {
            let key = fold(&names, input.phases, phase, &allocations.stacks.stacks[stack as usize]);
            *by_bytes.entry(key.clone()).or_default() += amount.bytes;
            *by_count.entry(key).or_default() += amount.count;
        }
        folded_files.push(("alloc.folded".into(), by_bytes.into_iter().map(|(key, value)| (key, value.round() as u64)).collect()));
        folded_files.push(("alloc.counts.folded".into(), by_count.into_iter().map(|(key, value)| (key, value.round() as u64)).collect()));
        if let Some(last) = input.snapshots.last() {
            let mut live: HashMap<String, u64> = HashMap::new();
            for (stack, _, _, size) in &last.by_stack {
                *live.entry(fold(&names, &[], u32::MAX, &allocations.stacks.stacks[*stack as usize])).or_default() += size.round() as u64;
            }
            folded_files.push(("heap.folded".into(), live.into_iter().collect()));
        }
    }
    folded_files.extend(counter_folded);
    for (name, mut rows) in folded_files.clone() {
        rows.retain(|row| row.1 > 0);
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut text = String::with_capacity(rows.len() * 80);
        for (stack, weight) in rows {
            let _ = writeln!(text, "{stack} {weight}");
        }
        write_file(&out.join(&name), text.as_bytes())?;
        files.push(name);
    }

    // speedscope: one sampled profile per folded file
    let speedscope = speedscope(&folded_files, &input.run.command);
    write_file(&out.join("profile.speedscope.json"), speedscope.to_string().as_bytes())?;
    files.push("profile.speedscope.json".into());

    files.insert(0, "summary.txt".into());
    files.insert(1, "profile.json".into());
    files.insert(2, "report.html".into());
    let _ = writeln!(summary, "\nfiles in {}: {}", out.display(), files.join(", "));
    json["summary"] = json!(summary);
    json["files"] = json!(files);
    let text = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    write_file(&out.join("profile.json"), text.as_bytes())?;
    write_file(&out.join("summary.txt"), summary.as_bytes())?;
    // (the page never reads each frame's functions, the bulk of a big profile: it is left out of the page's copy)
    if let Some(timeline) = json.get_mut("timeline").and_then(Json::as_object_mut) {
        timeline.remove("functions");
        timeline.remove("function_runs");
        timeline.remove("function_stats");
        timeline.remove("issue_stats");
        timeline.remove("node_frames");
        timeline.remove("site_frames");
        timeline.remove("replacement_frames");
        timeline.remove("function_contexts");
    }
    let page = serde_json::to_string(&json).map_err(|error| error.to_string())?;
    write_file(&out.join("report.html"), html(&page).as_bytes())?;
    Ok(summary)
}

/// What each cache outcome means and what removes it (profile.json `luau_caches.outcome_info`): keyed to what the interpreter did
pub const OUTCOME_INFO: [(&str, &str, &str); vm::CACHE_OUTCOMES] = [
    ("hit", "the cached slot served it (an import: resolved when the script loaded)", ""),
    (
        "repredicted",
        "the key was in the table at another slot than the one the instruction kept, so it looked the key up and kept the new slot (a method: in the class's __index table)",
        "Tables of several shapes meet here (keys added in another order, or other keys): build them alike, the same keys in the same order, or give each shape its own line",
    ),
    (
        "absent",
        "the key is not in the table and nothing else was looked at: the read is nil after a full lookup (an import: not resolved when the script loaded, so looked up every time)",
        "Reading a key the table never has: keep a value there (false), or test once outside the loop; for an import, read the global into a local once",
    ),
    ("inserted", "a new key was added to the table", "Each run adds the key to a fresh table: make the table with the key in a constructor ({key = value})"),
    (
        "rehashed",
        "a new key was added and the table had no room: it rehashed (allocated a bigger part and moved its keys)",
        "The table grows one key at a time: make it at its final size, with a constructor holding every key or table.create(n)",
    ),
    (
        "metatable_table",
        "the table lacks the key, so __index tables were walked (depth: how many) to the one holding it, or to the end of the chain",
        "Every access walks the metatable chain: copy the field onto the object, or keep the chain shallow (a field found at depth 1 by a method call is cached; deeper is not)",
    ),
    ("metatable_function", "an __index or __newindex function ran for the access", "Every access runs a function: a plain field, or a table __index, avoids it"),
    (
        "unsafe_env",
        "the script's environment is unsafe (getfenv / setfenv was called): the import is looked up every time",
        "Drop the getfenv / setfenv calls: imports resolve once and builtins take their fast paths again",
    ),
    (
        "not_table",
        "the value is not a table: its type's own metamethod served the access (handlers: whose); an Instance's member is priced by the engine (the boundary kind counts its crossings)",
        "",
    ),
    ("readonly", "a store into a frozen table: the access raised an error", "Do not store into a frozen table (table.freeze)"),
];

/// What a declined builtin call can do instead, where an equivalent call takes the fast path: (builtin, reason, advice)
const BUILTIN_ADVICE: [(&str, usize, &str); 8] = [
    ("math.abs", vm::DECLINE_TYPES, "Pass the existing numeric value directly instead of its string form; this saving includes no added conversion"),
    ("string.byte", vm::DECLINE_RESULTS, "string.byte(s, i, j) whose results go to a list or a call goes the full way when the range holds more than one byte: as many locals as bytes (local a, b = string.byte(s, i, i + 1)), or string.byte(s, k) for each index, take the fast path"),
    ("string.byte", vm::DECLINE_ARGS, "string.byte(s) goes the full way: string.byte(s, 1) takes the fast path"),
    ("string.sub", vm::DECLINE_ARGS, "string.sub(s, i) goes the full way: string.sub(s, i, #s) takes the fast path"),
    ("string.sub", vm::DECLINE_VALUES, "indices outside 1..#s (a negative one too) go the full way: positive indices within the string take the fast path"),
    ("table.insert", vm::DECLINE_ARGS, "table.insert(t, pos, v) goes the full way: when pos is #t + 1, table.insert(t, v) takes the fast path (an insert elsewhere has none)"),
    ("table.unpack", vm::DECLINE_RESULTS, "a fixed count of results (local a, b = table.unpack(t)) goes the full way: t[1], t[2] read them without a call"),
    ("*", vm::DECLINE_UNSAFE_ENV, "the script calls getfenv / setfenv, so no builtin takes its fast path: drop those calls"),
];

/// The advice for a builtin's declines for `reason`, when an equivalent call takes the fast path
pub fn builtin_advice(name: &str, reason: usize) -> Option<&'static str> {
    BUILTIN_ADVICE.iter().find(|(builtin, why, _)| (*builtin == name || *builtin == "*") && *why == reason).map(|(_, _, advice)| *advice)
}

/// Physical frames in the reported phases. Marks are a fallback for inputs without timeline rows.
fn scope_frames(input: &Input, scope: &[u32]) -> f64 {
    // Phase marks can split a frame, and their counters include the frame before steady starts.
    let row_frames: std::collections::BTreeSet<u64> = input.rows.iter().filter(|row| scope.contains(&row.phase)).map(|row| row.frame).collect();
    if !row_frames.is_empty() { return row_frames.len() as f64; }
    let names: Vec<&str> = scope.iter().filter_map(|phase| input.phases.get(*phase as usize).map(String::as_str)).collect();
    let end = input.end.map_or(input.run.frames, |end| end.frame.max(input.run.frames));
    let mut frames = 0u64;
    for (index, mark) in input.marks.iter().enumerate() {
        if names.contains(&mark.phase.as_str()) {
            let next = input.marks.get(index + 1).map_or(end, |next| next.frame);
            frames += next.saturating_sub(mark.frame);
        }
    }
    if frames == 0 { input.run.frames.max(1) as f64 } else { frames as f64 }
}

/// A cache kind's index (vm::CACHE_KINDS) for an opcode as it ran
fn kind_index(op: u8) -> Option<usize> {
    let kind = vm::cache_kind(op)?;
    vm::CACHE_KINDS.iter().position(|name| *name == kind)
}

/// Each kind's outcomes and what they cost over hits: the uncharged outcomes' work beyond the clock's (µs, prices clear of the
/// noise), the work clock's own charge at the sites past their dispatch (µs, part of their functions' work), the outcomes whose
/// prices are within the noise and those with no price
#[derive(Default, Clone)]
struct KindTotals {
    outcomes: [u64; vm::CACHE_OUTCOMES],
    extra_us: f64,
    charged_us: f64,
    /// Of `charged_us`, the first touches in a frame of the tables the sites reach
    first_touch_us: f64,
    /// What allocating the rehashed tables' storage at its final size up front would have charged more
    presized_us: f64,
    within_noise: u64,
    unpriced: u64,
}

/// A cache site in the reported phases, with its kind and what its outcomes cost, and apart by the call-tree node it ran in when
/// it did more than hit (a charge, another outcome)
struct SiteRow {
    function: i32,
    site: vm::CacheSite,
    kind: Option<usize>,
    extra: crate::profiler::prices::Extra,
    charged_us: f64,
    nodes: Vec<(i32, vm::CacheSite, crate::profiler::prices::Extra)>,
}

/// Luau's inline caches and builtin calls in the reported phases (the VM keeps a site's record for each phase it ran in): each
/// kind's outcomes, the sites whose outcomes cost the most, the builtins' fast calls and declines, and why. What an outcome costs
/// is apart by whether the work clock charges it (prices.rs): a charged one is the clock's own charge at the site (part of its
/// function's work), an uncharged one is priced at the committed prices (work beyond the clock's). ree's own tool code (a
/// harness one of ree's tools ran, rur.record) is counted apart: it is not the game's, nor a user's harness.
fn inline_caches(input: &Input, vm_results: &vm::Results, names: &Names, top: usize, summary: &mut String, json: &mut Json) {
    let scope = reported_phases(input.config.phases.as_deref(), input.phases);
    let frames = scope_frames(input, &scope);
    let every_phase = scope.len() >= input.phases.len().max(1);
    let scope_names: Vec<String> = scope.iter().map(|phase| input.phases.get(*phase as usize).cloned().unwrap_or_default()).collect();
    let scope_label = if every_phase { "every phase".to_string() } else { format!("{} {}", if scope.len() == 1 { "phase" } else { "phases" }, scope_names.join(", ")) };
    let scale = price_scale();

    // each instruction's records in the reported phases, summed, and apart by the call-tree node they ran in
    let absorb = |entry: &mut vm::CacheSite, site: &vm::CacheSite| {
        entry.op = site.op;
        entry.charged += site.charged * scale;
        entry.first_touch += site.first_touch * scale;
        entry.presized += site.presized * scale;
        for (sum, count) in entry.outcomes.iter_mut().zip(site.outcomes) {
            *sum += count;
        }
        for (sum, count) in entry.depths.iter_mut().zip(site.depths) {
            *sum += count;
        }
        for (name, count) in &site.handlers {
            match entry.handlers.iter_mut().find(|(known, _)| known == name) {
                Some(known) => known.1 += count,
                None => entry.handlers.push((name.clone(), *count)),
            }
        }
    };
    let mut merged: BTreeMap<(i32, usize), vm::CacheSite> = BTreeMap::new();
    let mut by_node: BTreeMap<(i32, usize), BTreeMap<i32, vm::CacheSite>> = BTreeMap::new();
    for (id, function) in vm_results.functions.iter().enumerate() {
        if function.exempt() {
            continue;
        }
        for site in function.caches.iter().filter(|site| scope.contains(&site.phase)) {
            let fresh = || vm::CacheSite { pc: site.pc, phase: site.phase, node: site.node, op: site.op, key: site.key.clone(), ..Default::default() };
            absorb(merged.entry((id as i32, site.pc)).or_insert_with(fresh), site);
            absorb(by_node.entry((id as i32, site.pc)).or_default().entry(site.node).or_insert_with(fresh), site);
        }
    }
    let mut sites: Vec<SiteRow> = Vec::new();
    let mut by_kind: [KindTotals; 4] = Default::default();
    let mut tool_kinds: [KindTotals; 4] = Default::default();
    let mut tool_sites = 0usize;
    for ((function, pc), site) in merged {
        if site.runs() == 0 {
            continue;
        }
        let kind = kind_index(site.op);
        let extra = kind.map_or_else(Default::default, |kind| crate::profiler::prices::site_extra(kind, &site.outcomes, &site.depths, scale));
        let charged_us = site.charged * 1e6;
        // (apart by node: a site that did more than hit, so a context's share of its charge and its extra can be told)
        let beyond_hits = site.charged > 0.0 || site.outcomes.iter().enumerate().any(|(outcome, count)| outcome != vm::HIT && *count > 0);
        let nodes: Vec<(i32, vm::CacheSite, crate::profiler::prices::Extra)> = if beyond_hits {
            by_node
                .remove(&(function, pc))
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, at)| at.runs() > 0)
                .map(|(node, at)| {
                    let extra = kind.map_or_else(Default::default, |kind| crate::profiler::prices::site_extra(kind, &at.outcomes, &at.depths, scale));
                    (node, at, extra)
                })
                .collect()
        } else {
            Vec::new()
        };
        if let Some(kind) = kind {
            let totals = if names.tool(function) { &mut tool_kinds[kind] } else { &mut by_kind[kind] };
            for (sum, count) in totals.outcomes.iter_mut().zip(site.outcomes) {
                *sum += count;
            }
            totals.extra_us += extra.uncharged_us;
            totals.charged_us += charged_us;
            totals.first_touch_us += site.first_touch * 1e6;
            totals.presized_us += site.presized * 1e6;
            totals.within_noise += extra.within_noise;
            totals.unpriced += extra.unpriced;
        }
        if names.tool(function) {
            tool_sites += 1;
            continue;
        }
        sites.push(SiteRow { function, site, kind, extra, charged_us, nodes });
    }
    let outcomes_json = |outcomes: &[u64; vm::CACHE_OUTCOMES]| -> Json { Json::Object(vm::CACHE_OUTCOME_NAMES.iter().zip(outcomes).map(|(name, count)| (name.to_string(), json!(count))).collect()) };
    // (the four counts profiles had before, each exact: misses are repredictions, slow the metatable walks and unsafe environments, other not a table)
    let legacy = |o: &[u64; vm::CACHE_OUTCOMES]| -> (u64, u64, u64, u64) { (o[vm::HIT], o[vm::REPREDICTED], o[vm::METATABLE] + o[vm::METAFUNCTION] + o[vm::UNSAFE_ENV], o[vm::NOT_TABLE]) };
    // the lookups a table serves (a value another type's metamethod serves, not_table, is no cache's: profile diff and the viewer
    // read the same)
    let lookups = |o: &[u64; vm::CACHE_OUTCOMES]| -> u64 { o.iter().sum::<u64>() - o[vm::NOT_TABLE] };
    let site_label = |function: i32, site: &vm::CacheSite| -> String {
        let f = &vm_results.functions[function as usize];
        let key = if site.key.is_empty() { String::new() } else { format!(" {}", site.key) };
        format!("{} {}:{}{key}", vm::opcode_name(site.op), names.short(&f.source), f.lines.get(site.pc).copied().unwrap_or(0))
    };
    // (the harness's own code, under HARNESS_FOLDER, is listed apart from the game's)
    let harness_site = |function: i32| -> bool { harness_module(module_name(&vm_results.functions[function as usize].source)) };
    let _ = writeln!(
        summary,
        "
Luau inline caches, {scope_label}: what each lookup did, as the interpreter did it (repredicted: the key was at another slot; absent: not there, nothing else looked at; metatable: __index tables walked); hit: of the lookups a table serves. Over {frames:.0} frames, the work clock's us a frame: charged, what the clock charged the sites past their dispatch (a rehash's keys and storage, a looked-up import, an Instance member's engine price, a table's cold first touch in the frame: in their functions' work); extra, what the outcomes the clock does not charge cost over hits at the committed prices (repredictions, absent keys, inserts with room, metatable walks: beyond the clock's work)"
    );
    let _ = writeln!(summary, "{:<15} {:>12} {:>8} {:>11} {:>9} {:>9} {:>9} {:>10} {:>9} {:>11} {:>11} {:>11}", "cache", "lookups", "hit", "repredicted", "absent", "inserted", "rehashed", "metatable", "unsafe", "not table", "charged/fr", "extra/fr");
    for (kind, totals) in by_kind.iter().enumerate() {
        let outcomes = &totals.outcomes;
        if outcomes.iter().all(|n| *n == 0) {
            continue;
        }
        let _ = writeln!(
            summary,
            "{:<15} {:>12} {:>8} {:>11} {:>9} {:>9} {:>9} {:>10} {:>9} {:>11} {:>11.2} {:>11.2}",
            vm::CACHE_KINDS[kind],
            group_digits(lookups(outcomes)),
            percent(outcomes[vm::HIT] as f64, lookups(outcomes) as f64),
            group_digits(outcomes[vm::REPREDICTED]),
            group_digits(outcomes[vm::ABSENT]),
            group_digits(outcomes[vm::INSERTED]),
            group_digits(outcomes[vm::REHASHED]),
            group_digits(outcomes[vm::METATABLE] + outcomes[vm::METAFUNCTION]),
            group_digits(outcomes[vm::UNSAFE_ENV]),
            group_digits(outcomes[vm::NOT_TABLE]),
            totals.charged_us / frames,
            totals.extra_us / frames
        );
    }
    // (the uncharged prices this run's outcomes would use whose spread reaches zero or is wider than the price: left out of the sums)
    let mut noisy: Vec<String> = Vec::new();
    for (kind, totals) in by_kind.iter().enumerate() {
        for (what, &count) in totals.outcomes.iter().enumerate() {
            if count == 0 || what == vm::HIT || what == vm::METATABLE {
                continue;
            }
            if let Some(price) = crate::profiler::prices::outcome(kind, what, scale).filter(|price| price.within_noise) {
                noisy.push(format!("{} {} {:.2} ns ({:.2} to {:.2}), {} outcomes", vm::CACHE_KINDS[kind], vm::CACHE_OUTCOME_NAMES[what].replace('_', " "), price.ns, price.low_ns, price.high_ns, group_digits(count)));
            }
        }
    }
    if !noisy.is_empty() {
        let _ = writeln!(summary, "prices within the noise (their spread reaches zero or is wider than the price), left out of the extra: {}", noisy.join("; "));
    }
    let tool_lookups: u64 = tool_kinds.iter().map(|totals| lookups(&totals.outcomes)).sum();
    if tool_lookups > 0 {
        let tool_hits: u64 = tool_kinds.iter().map(|totals| totals.outcomes[vm::HIT]).sum();
        let _ = writeln!(
            summary,
            "ree's own code (a harness one of ree's tools ran, rur.record): {} lookups at {} sites, {} hit: left out of the rows above",
            group_digits(tool_lookups),
            tool_sites,
            percent(tool_hits as f64, tool_lookups as f64)
        );
    }
    // the sites whose outcomes cost the most: the clock's charge and the uncharged extra together, then the outcomes but hits
    let costly = |row: &SiteRow| lookups(&row.site.outcomes) - row.site.outcomes[vm::HIT];
    let weight = |row: &SiteRow| row.extra.uncharged_us + row.charged_us;
    let game_sites = sites.iter().any(|row| costly(row) > 0 && !harness_site(row.function));
    let mut ranked: Vec<&SiteRow> = sites.iter().filter(|row| costly(row) > 0 && (!game_sites || !harness_site(row.function))).collect();
    ranked.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then(costly(b).cmp(&costly(a))).then(a.function.cmp(&b.function)).then(a.site.pc.cmp(&b.site.pc)));
    if !ranked.is_empty() {
        let _ = writeln!(summary, "sites costing most{} (the work clock's us a frame: charged, then extra)", if game_sites { ": the game's code (the harness's apart)" } else { "" });
        for row in ranked.iter().take(top.min(10)) {
            let parts: Vec<String> = row.site.outcomes.iter().enumerate().filter(|(outcome, count)| **count > 0 && *outcome != vm::HIT).map(|(outcome, count)| format!("{} {}", vm::CACHE_OUTCOME_NAMES[outcome].replace('_', " "), group_digits(*count))).collect();
            let noise = if row.extra.within_noise > 0 { format!(" ({} priced within the noise, left out)", group_digits(row.extra.within_noise)) } else { String::new() };
            let _ = writeln!(summary, "  {:>9.2} {:>9.2}  {}  {} of {}{noise}", row.charged_us / frames, row.extra.uncharged_us / frames, site_label(row.function, &row.site), parts.join(", "), group_digits(row.site.runs()));
        }
    }
    sites.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then(costly(b).cmp(&costly(a))).then(b.site.runs().cmp(&a.site.runs())).then(a.function.cmp(&b.function)).then(a.site.pc.cmp(&b.site.pc)));
    // a charge split into its table's first touch and the rest, each rounded so that the parts add up to the rounded charge:
    // (charged, first touch, the outcome's own)
    let split = |charged: f64, first_touch: f64, digits: i32| -> (f64, f64, f64) {
        let (charged, first_touch) = (round(charged, digits), round(first_touch.min(charged), digits));
        (charged, first_touch, round(charged - first_touch, digits))
    };
    let kind_json = |kind: usize, totals: &KindTotals| -> Json {
        let (hits, misses, slow, other) = legacy(&totals.outcomes);
        let (_, first_touch, outcome) = split(totals.charged_us, totals.first_touch_us, 3);
        let (_, first_touch_frame, outcome_frame) = split(totals.charged_us / frames, totals.first_touch_us / frames, 4);
        json!({
            "kind": vm::CACHE_KINDS[kind], "outcomes": outcomes_json(&totals.outcomes), "lookups": lookups(&totals.outcomes),
            "charged_work_us": round(totals.charged_us, 3), "charged_work_us_per_frame": round(totals.charged_us / frames, 4),
            "first_touch_us": first_touch, "first_touch_us_per_frame": first_touch_frame, "outcome_charged_us": outcome, "outcome_charged_us_per_frame": outcome_frame,
            "presized_us": (totals.outcomes[vm::REHASHED] > 0).then(|| round(totals.presized_us, 3)),
            "extra_work_us": round(totals.extra_us, 3), "extra_work_us_per_frame": round(totals.extra_us / frames, 4), "within_noise": totals.within_noise, "unpriced": totals.unpriced,
            "hits": hits, "misses": misses, "slow": slow, "other": other,
        })
    };
    json["luau_caches"] = json!({
        "scope": scope_names,
        "frames": frames,
        "outcomes": vm::CACHE_OUTCOME_NAMES,
        "outcome_info": Json::Object(OUTCOME_INFO.iter().map(|(name, meaning, advice)| (name.to_string(), json!({"meaning": meaning, "advice": advice}))).collect()),
        "prices": crate::profiler::prices::json(scale),
        "about": "charged_work_us: the work clock's own charge at a site past its dispatch, part of its function's work (claim it from there), split exactly into first_touch_us (the first touch in a frame of the table the site reaches: its operand's, the environment's for a global; any change to the site still pays it) and outcome_charged_us (the rest: what its outcomes cost the clock); presized_us, at a site whose inserts rehashed: what allocating those tables' storage at the size each rehash made, up front, would have charged beyond the storage they had (a presized constructor's or table.create's cost in their place, at the clock's price for storage), so outcome_charged_us - presized_us is what building them presized saves; extra_work_us: what the outcomes the clock does not charge cost over hits at the committed prices, beyond the clock's work (within_noise counts the outcomes whose prices are within the noise, left out of it)",
        "by_kind": by_kind.iter().enumerate().filter(|(_, totals)| totals.outcomes.iter().any(|n| *n > 0)).map(|(kind, totals)| kind_json(kind, totals)).collect::<Vec<_>>(),
        "tool": {
            "about": "ree's own code (a harness one of ree's tools ran, rur.record), left out of by_kind and sites",
            "sites": tool_sites,
            "by_kind": tool_kinds.iter().enumerate().filter(|(_, totals)| totals.outcomes.iter().any(|n| *n > 0)).map(|(kind, totals)| kind_json(kind, totals)).collect::<Vec<_>>(),
        },
        "sites": sites.iter().map(|row| {
            let (function, site) = (row.function, &row.site);
            let f = &vm_results.functions[function as usize];
            let (hits, misses, slow, other) = legacy(&site.outcomes);
            let rehashes = site.outcomes[vm::REHASHED] > 0;
            let (mut charged, mut first_touch, mut outcome) = split(row.charged_us, site.first_touch * 1e6, 4);
            let (mut presized, mut extra) = (round(site.presized * 1e6, 4), round(row.extra.uncharged_us, 4));
            let (charged_frame, first_touch_frame, outcome_frame) = split(row.charged_us / frames, site.first_touch * 1e6 / frames, 5);
            // (each call-tree node's part, rounded as the site's figures are; the site's figures their sums)
            let nodes: Vec<Json> = row
                .nodes
                .iter()
                .map(|(node, at, at_extra)| {
                    let (c, ft, oc) = split(at.charged * 1e6, at.first_touch * 1e6, 4);
                    json!({
                        "node": node, "outcomes": outcomes_json(&at.outcomes),
                        "charged_work_us": c, "first_touch_us": ft, "outcome_charged_us": oc,
                        "presized_us": rehashes.then(|| round(at.presized * 1e6, 4)),
                        "extra_work_us": round(at_extra.uncharged_us, 4), "within_noise": at_extra.within_noise, "unpriced": at_extra.unpriced,
                    })
                })
                .collect();
            if !nodes.is_empty() {
                let sum = |field: &str| round(nodes.iter().map(|node| node[field].as_f64().unwrap_or(0.0)).sum::<f64>(), 4);
                (charged, first_touch, outcome, presized, extra) = (sum("charged_work_us"), sum("first_touch_us"), sum("outcome_charged_us"), sum("presized_us"), sum("extra_work_us"));
            }
            json!({
                "function": function, "line": f.lines.get(site.pc).copied().unwrap_or(0), "pc": site.pc, "op": vm::opcode_name(site.op),
                "key": (!site.key.is_empty()).then_some(site.key.as_str()), "harness": harness_site(function),
                "kind": row.kind.map(|kind| vm::CACHE_KINDS[kind]), "outcomes": outcomes_json(&site.outcomes),
                "depth": Json::Object(site.depths.iter().enumerate().filter(|(_, n)| **n > 0).map(|(d, n)| (if d + 1 == vm::CACHE_DEPTHS { format!("{}+", d + 1) } else { (d + 1).to_string() }, json!(n))).collect()),
                "handlers": Json::Object(site.handlers.iter().map(|(name, count)| (name.clone(), json!(count))).collect()),
                "charged_work_us": charged, "charged_work_us_per_frame": charged_frame,
                "first_touch_us": first_touch, "first_touch_us_per_frame": first_touch_frame, "outcome_charged_us": outcome, "outcome_charged_us_per_frame": outcome_frame,
                "presized_us": rehashes.then_some(presized), "presized_us_per_frame": rehashes.then(|| round(site.presized * 1e6 / frames, 5)),
                "extra_work_us": extra, "extra_work_us_per_frame": round(row.extra.uncharged_us / frames, 5),
                "within_noise": row.extra.within_noise, "unpriced": row.extra.unpriced,
                "hits": hits, "misses": misses, "slow": slow, "other": other,
                "nodes": (!nodes.is_empty()).then_some(nodes),
            })
        }).collect::<Vec<_>>(),
    });

    // builtin calls in the reported phases: per builtin, the calls its fast path served, the declined ones by reason, what the
    // work clock charged the declines (the VM books it to the site: their fast call's dispatch, the fallback's instructions, and
    // the function its CALL reached, a C function's work and its cold first call, the engine's own Luau's) and what the fast path
    // would charge the same calls (its dispatch and the builtin's own counted work); a site apart by the call-tree node it ran in
    #[derive(Default, Clone, Copy)]
    struct Calls {
        fast: u64,
        declines: [u64; vm::DECLINES],
        /// The declines' charge, and what the fast path would charge them, in seconds
        charged: f64,
        fast_equivalent: f64,
    }
    impl Calls {
        fn absorb(&mut self, site: &vm::BuiltinSite) {
            self.fast += site.fast;
            let scale = price_scale();
            self.charged += site.charged * scale;
            self.fast_equivalent += site.fast_equivalent * scale;
            for (sum, count) in self.declines.iter_mut().zip(site.declines) {
                *sum += count;
            }
        }
        fn declined(&self) -> u64 {
            self.declines.iter().sum()
        }
    }
    #[derive(Default)]
    struct Builtin {
        all: Calls,
        /// Per site (function, pc), and per site and node
        sites: BTreeMap<(i32, usize), Calls>,
        nodes: BTreeMap<(i32, usize), BTreeMap<i32, Calls>>,
    }
    let (mut builtins, mut tool_builtins): (BTreeMap<i32, Builtin>, BTreeMap<i32, Builtin>) = (BTreeMap::new(), BTreeMap::new());
    for (id, function) in vm_results.functions.iter().enumerate() {
        if function.exempt() {
            continue;
        }
        let into = if names.tool(id as i32) { &mut tool_builtins } else { &mut builtins };
        for site in function.builtins.iter().filter(|site| scope.contains(&site.phase)) {
            let entry = into.entry(site.builtin).or_default();
            entry.all.absorb(site);
            entry.sites.entry((id as i32, site.pc)).or_default().absorb(site);
            entry.nodes.entry((id as i32, site.pc)).or_default().entry(site.node).or_default().absorb(site);
        }
    }
    let declined = |builtin: &Builtin| builtin.all.declined();
    let charged = |builtin: &Builtin| builtin.all.charged;
    let sorted = |builtins: BTreeMap<i32, Builtin>| -> Vec<(i32, Builtin)> {
        let mut rows: Vec<(i32, Builtin)> = builtins.into_iter().collect();
        rows.sort_by(|a, b| charged(&b.1).total_cmp(&charged(&a.1)).then(declined(&b.1).cmp(&declined(&a.1))).then((b.1.all.fast + declined(&b.1)).cmp(&(a.1.all.fast + declined(&a.1)))).then(a.0.cmp(&b.0)));
        rows
    };
    let (rows, tool_rows) = (sorted(builtins), sorted(tool_builtins));
    let name_of = |id: i32| vm::builtin_name(id).unwrap_or("?").to_string();
    let reasons = |declines: &[u64; vm::DECLINES]| -> String {
        let parts: Vec<String> = declines.iter().enumerate().filter(|(_, n)| **n > 0).map(|(reason, n)| format!("{} {}", vm::DECLINE_NAMES[reason].replace('_', " "), group_digits(*n))).collect();
        if parts.is_empty() { "-".into() } else { parts.join(", ") }
    };
    if !rows.is_empty() {
        let (calls, fast): (u64, u64) = rows.iter().fold((0, 0), |(calls, fast), (_, b)| (calls + b.all.fast + declined(b), fast + b.all.fast));
        let mut why = [0u64; vm::DECLINES];
        for (_, b) in &rows {
            for (reason, count) in b.all.declines.iter().enumerate() {
                why[reason] += count;
            }
        }
        let _ = writeln!(
            summary,
            "
builtin calls (the compiler's fast calls), {scope_label}: {} calls, {} served by the fast path; declined (the call went the full way): {}; a decline's charge is the work clock's (its fast call, its fallback's instructions and the function it reached, a C function's cold first call in the frame or the engine's own Luau among it: in its function's work); the fast path would charge the same calls its fast call and the builtin's own counted work (a new string, the objects it allocates), so a fix saves the difference",
            group_digits(calls),
            group_digits(fast),
            reasons(&why)
        );
        let _ = writeln!(summary, "{:<20} {:>12} {:>12}  {:<40} {:>18} {:>14} {:>12}", "builtin", "calls", "fast path", "declined, why", "clock ns a decline", "fast path ns", "charged/fr");
        for (id, b) in rows.iter().take(top.min(12)) {
            let n = declined(b);
            let _ = writeln!(
                summary,
                "{:<20} {:>12} {:>12}  {:<40} {:>18} {:>14} {:>12}",
                name_of(*id),
                group_digits(b.all.fast + n),
                group_digits(b.all.fast),
                reasons(&b.all.declines),
                if n == 0 { String::new() } else { format!("{:.1}", charged(b) * 1e9 / n as f64) },
                if n == 0 { String::new() } else { format!("{:.1}", b.all.fast_equivalent * 1e9 / n as f64) },
                if n == 0 { String::new() } else { format!("{:.2}", charged(b) * 1e6 / frames) }
            );
            for (reason, count) in b.all.declines.iter().enumerate() {
                if *count > 0
                    && let Some(advice) = builtin_advice(&name_of(*id), reason)
                {
                    let _ = writeln!(summary, "    {}: {advice}", vm::DECLINE_NAMES[reason].replace('_', " "));
                }
            }
        }
    }
    if !tool_rows.is_empty() {
        let (calls, fast): (u64, u64) = tool_rows.iter().fold((0, 0), |(calls, fast), (_, b)| (calls + b.all.fast + declined(b), fast + b.all.fast));
        let _ = writeln!(summary, "ree's own code's builtin calls: {} calls, {} served by the fast path: left out of the table above", group_digits(calls), group_digits(fast));
    }
    let declines_json = |declines: &[u64; vm::DECLINES]| -> Json { Json::Object(vm::DECLINE_NAMES.iter().zip(declines).map(|(name, count)| (name.to_string(), json!(count))).collect()) };
    json["builtins"] = Json::Array(
        rows.iter()
            .map(|(id, b)| {
                let name = name_of(*id);
                let n = declined(b);
                json!({
                    "name": name, "id": id, "calls": b.all.fast + n, "fast": b.all.fast, "declines": declines_json(&b.all.declines),
                    // (the work clock's own charge for the declines: part of their functions' work, claimed from there; and what the
                    // fast path would charge the same calls, which a fix still pays)
                    "decline_charged_us": round(charged(b) * 1e6, 4), "decline_charged_us_per_frame": round(charged(b) * 1e6 / frames, 5),
                    "decline_charged_ns": (n > 0).then(|| round(charged(b) * 1e9 / n as f64, 3)),
                    "fast_equivalent_us": round(b.all.fast_equivalent * 1e6, 4), "fast_equivalent_us_per_frame": round(b.all.fast_equivalent * 1e6 / frames, 5),
                    "fast_equivalent_ns": (n > 0).then(|| round(b.all.fast_equivalent * 1e9 / n as f64, 3)),
                    "advice": Json::Object(b.all.declines.iter().enumerate().filter(|(_, n)| **n > 0).filter_map(|(reason, _)| builtin_advice(&name, reason).map(|advice| (vm::DECLINE_NAMES[reason].to_string(), json!(advice)))).collect()),
                    "sites": b.sites.iter().map(|(&(function, pc), at)| {
                        let f = &vm_results.functions[function as usize];
                        let (mut charged, mut fast_equivalent) = (round(at.charged * 1e6, 4), round(at.fast_equivalent * 1e6, 4));
                        // (a site that declined, apart by call-tree node: its figures their rounded sums, so they add up exactly)
                        let nodes: Vec<Json> = if at.declined() > 0 || at.charged > 0.0 {
                            b.nodes.get(&(function, pc)).into_iter().flatten().map(|(node, calls)| json!({
                                "node": node, "calls": calls.fast + calls.declined(), "fast": calls.fast, "declines": declines_json(&calls.declines),
                                "decline_charged_us": round(calls.charged * 1e6, 4), "fast_equivalent_us": round(calls.fast_equivalent * 1e6, 4),
                            })).collect()
                        } else {
                            Vec::new()
                        };
                        if !nodes.is_empty() {
                            let sum = |field: &str| round(nodes.iter().map(|node| node[field].as_f64().unwrap_or(0.0)).sum::<f64>(), 4);
                            (charged, fast_equivalent) = (sum("decline_charged_us"), sum("fast_equivalent_us"));
                        }
                        json!({
                            "fallback_end_pc": fallback_end_pc(f, pc),
                            "function": function, "line": f.lines.get(pc).copied().unwrap_or(0), "pc": pc, "calls": at.fast + at.declined(), "fast": at.fast,
                            "declines": declines_json(&at.declines), "decline_charged_us": charged, "fast_equivalent_us": fast_equivalent,
                            "nodes": (!nodes.is_empty()).then_some(nodes),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    json["builtins_tool"] = Json::Array(tool_rows.iter().map(|(id, b)| json!({"name": name_of(*id), "id": id, "calls": b.all.fast + declined(b), "fast": b.all.fast, "declines": declines_json(&b.all.declines)})).collect());
}

/// A mark with its running counts (a phase's figures are the difference between two marks')
fn mark_json(mark: &Mark) -> Json {
    let mut json = json!({"phase": mark.phase, "frame": mark.frame, "seconds": mark.seconds, "work_ms": ms(mark.work), "host_ms": mark.host * 1e3, "cycles": mark.cycles});
    if let Some(totals) = &mark.totals {
        json["insns"] = json!(totals.insns);
        json["cold_ms"] = json!(ms(totals.cold));
        json["collector_ms"] = json!(ms(totals.collector));
        json["vm"] = json!({
            "objects": totals.objects, "allocated": totals.allocated, "stored": totals.stored, "rehashed": totals.rehashed,
            "cold_tables": totals.cold_tables, "cold_closures": totals.cold_closures, "cold_protos": totals.cold_protos, "cold_code": totals.cold_code,
            "sort_compares": totals.sort_compares, "sort_calls": totals.sort_calls,
            "buffer_fresh_bytes": totals.buffer_fresh, "buffer_big_bytes": totals.buffer_big,
        });
        if let (Some(bytes), Some(count)) = (totals.alloc_bytes, totals.allocs) {
            json["alloc_bytes"] = json!(bytes);
            json["allocs"] = json!(count);
        }
        if let Some((events, keys, script_events, script_keys)) = totals.rehashes {
            json["rehashes"] = json!(events);
            json["rehash_keys"] = json!(keys);
            json["script_rehashes"] = json!(script_events);
            json["script_rehash_keys"] = json!(script_keys);
        }
        json["gc"] = json!({"steps": totals.gc.0, "cycles": totals.gc.1, "freed": totals.gc.2, "ms": ms(totals.gc.3)});
        if let Some(caches) = &totals.caches {
            // (each kind's outcomes in luau_caches.outcomes' order; the builtin calls served and declined by reason)
            json["caches"] = Json::Object(vm::CACHE_KINDS.iter().zip(caches.kinds.iter()).map(|(kind, outcomes)| (kind.to_string(), json!(outcomes))).collect());
            // (each kind's metatable walks by depth: 1, 2, 3, 4 or more)
            json["cache_depths"] = Json::Object(vm::CACHE_KINDS.iter().zip(caches.depths.iter()).map(|(kind, depths)| (kind.to_string(), json!(depths))).collect());
            json["builtins"] = json!({"fast": caches.fast, "declines": Json::Object(vm::DECLINE_NAMES.iter().zip(caches.declines.iter()).map(|(name, count)| (name.to_string(), json!(count))).collect())});
        }
    }
    if let Some(ops) = &mark.engine {
        json["engine"] = engine_json(ops);
    }
    json
}

/// The reported phases' game code: its self work, and each kind's share of it (vm::SPLITS)
fn scope_work_kinds(model: &Model, scope: &[u32]) -> (f64, [f64; vm::SPLITS]) {
    let mut work = 0.0;
    for (&(_, phase, harness_side), figures) in &model.phase_self {
        if !harness_side && scope.contains(&phase) {
            work += figures.0;
        }
    }
    let mut kinds = [0.0f64; vm::SPLITS];
    for (&(_, phase, harness_side), split) in &model.phase_split {
        if !harness_side && scope.contains(&phase) {
            add_kinds(&mut kinds, &split.kinds);
        }
    }
    (work, kinds)
}

/// The phases `scope` names, as the summary's tables name them
fn scope_phrase(scope: &[u32], phases: &[String]) -> String {
    let names: Vec<&str> = scope.iter().filter_map(|phase| phases.get(*phase as usize).map(String::as_str)).collect();
    match names.len() {
        0 => "every phase".to_string(),
        1 => format!("phase {}", names[0]),
        _ => format!("phases {}", names.join(", ")),
    }
}

/// The Luau part of `scope_work` (the reported phases' game code): the VM's priced work less its cold and the collector's charges,
/// as a share of every script's work in those phases (each phase's mark to the next one's), applied to it; None without marks
fn scope_luau(input: &Input, scope: &[u32], scope_work: f64) -> Option<f64> {
    let (mut work, mut luau) = (0.0, 0.0);
    let mut seen = false;
    for (index, mark) in input.marks.iter().enumerate() {
        let Some(phase) = input.phases.iter().position(|name| *name == mark.phase) else { continue };
        if !scope.contains(&(phase as u32)) {
            continue;
        }
        let next = input.marks.get(index + 1).or(input.end)?;
        let (start, end) = (mark.totals?, next.totals?);
        work += (next.work - mark.work).max(0.0);
        luau += ((end.luau - start.luau) - (end.vm_cold - start.vm_cold) - (end.collector - start.collector)).max(0.0);
        seen = true;
    }
    (seen && work > 0.0).then(|| (luau / work).min(1.0) * scope_work)
}

/// The Luau part of the whole run's work (its first mark to its end), for a profile without phases' figures
fn whole_luau(input: &Input, total: f64, kinds: &[f64; vm::SPLITS]) -> f64 {
    match (input.marks.first().and_then(|mark| mark.totals), input.end.and_then(|end| end.totals)) {
        (Some(first), Some(last)) => ((last.luau - first.luau) - (last.vm_cold - first.vm_cold) - kinds[vm::COLLECTOR]).clamp(0.0, total),
        _ => 0.0,
    }
}

/// One kind of work in a fidelity line: its share of the code's work and its band (cost.rs BENCH_BANDS)
struct FidelityComponent {
    name: &'static str,
    share: f64,
    low: f64,
    high: f64,
    rows: usize,
}

impl FidelityComponent {
    /// A kind of work with the band cost.rs keeps for it; a kind without one reads 0.5-1.5x
    fn new(name: &'static str, share: f64) -> Self {
        match crate::cost::BENCH_BANDS.iter().find(|band| band.component == name) {
            Some(band) => FidelityComponent { name, share, low: band.low, high: band.high, rows: band.rows },
            None => FidelityComponent { name, share, low: 0.5, high: 1.5, rows: 0 },
        }
    }

    /// What the band is made of (profile.json's `basis`)
    fn basis(&self) -> String {
        let rows = self.rows;
        match self.name {
            "cold" => format!("{rows} held-out macro-bench rows: the whole work clock on a game's code where its cold charges weigh most, measured with the prices before the size fit, not a test of the cold charges alone"),
            "studio" => format!("the range of {rows} interpreted bench rows against the Studio playtest prices"),
            "defaulted" => format!("{rows} measured method rows against the borrowed CALL price"),
            _ => format!("{rows} held-out parity-bench rows"),
        }
    }

    /// The same, short, for the fidelity text
    fn rows_text(&self) -> String {
        let rows = self.rows;
        match self.name {
            "cold" => format!("{rows} macro-bench rows of the whole clock, previous prices"),
            "studio" => format!("the range of {rows} rows"),
            _ => format!("{rows} rows"),
        }
    }
}

/// The kinds of work a fidelity line weighs (`total`: all the code's work, `kinds`: its split by kind, `luau`: its Luau part), each with
/// its share. What was priced from a client is split among the Luau, engine, cold, collector and dispatch parts; what was priced from
/// Studio-era measurements and what was borrowed are shares of the whole. Parts with no work are left out.
fn fidelity_components(total: f64, kinds: &[f64; vm::SPLITS], luau: f64, studio: f64, defaulted: f64) -> Vec<FidelityComponent> {
    let cold = kinds[vm::COLD];
    let collector = kinds[vm::COLLECTOR];
    let dispatch = kinds[vm::DISPATCH];
    let luau = luau.clamp(0.0, total);
    let engine = (total - luau - cold - collector - dispatch).max(0.0);
    // (the provenance axis comes out first: what is left was priced from a client)
    let client_share = (total - studio - defaulted).max(0.0) / total;
    let client_parts = (luau + engine + cold + collector + dispatch).max(1e-12);
    let mut components = vec![
        FidelityComponent::new("luau", client_share * luau / client_parts),
        FidelityComponent::new("engine", client_share * engine / client_parts),
        FidelityComponent::new("cold", client_share * cold / client_parts),
        FidelityComponent::new("collector", client_share * collector / client_parts),
        FidelityComponent::new("dispatch", client_share * dispatch / client_parts),
        FidelityComponent::new("studio", studio / total),
        FidelityComponent::new("defaulted", defaulted / total),
    ];
    components.retain(|component| component.share > 0.0);
    components
}

/// What the work clock is beside a client's own time for `what` (the reported phases' game code): each component's band (ree /
/// client on rows no fit saw, cost.rs BENCH_BANDS, each with what it is), weighted by that code's work in each (`total`, its kinds
/// `kinds` and its Luau part `luau`), with the work priced from Studio-era measurements and by borrowed prices at their own bands, and
/// a phone's factor's measured spread folded into the band.
fn fidelity_json(input: &Input, total: f64, kinds: &[f64; vm::SPLITS], luau: f64, what: &str) -> Json {
    let total = total.max(1e-12);
    let studio = kinds[vm::STUDIO].min(total);
    let defaulted = kinds[vm::DEFAULTED].min(total - studio);
    let components = fidelity_components(total, kinds, luau, studio, defaulted);

    // the bands' mean, weighted by each component's share of the work
    let weight: f64 = components.iter().map(|component| component.share).sum::<f64>().max(1e-12);
    let (mut clock_lo, mut clock_hi) = (0.0, 0.0);
    for component in &components {
        clock_lo += component.share / weight * component.low;
        clock_hi += component.share / weight * component.high;
    }

    let device = input.device;
    let phone = device == "phone";
    let (device_lo, device_hi) = if phone { crate::cost::PHONE_SCRIPTS_BAND } else { (1.0, 1.0) };
    // (against a phone: the figures are the desktop prices times PHONE_SCRIPTS where a phone's factor lies within the band)
    let (ratio_lo, ratio_hi) = if phone { (crate::cost::PHONE_SCRIPTS / device_hi, crate::cost::PHONE_SCRIPTS / device_lo) } else { (1.0, 1.0) };
    let (lo, hi) = (clock_lo * ratio_lo, clock_hi * ratio_hi);
    let device_note = if phone {
        format!(
            "; a phone's figures are the desktop prices times {:.2}, a factor measured at {:.2}-{:.2} over phones and scenes, so against a phone they read {:.2}-{:.2}x again, which the band includes",
            crate::cost::PHONE_SCRIPTS, device_lo, device_hi, ratio_lo, ratio_hi
        )
    } else {
        String::new()
    };

    let mut parts: Vec<String> = Vec::new();
    for component in &components {
        parts.push(format!(
            "{} {:.0} % at {:.2}-{:.2}x ({})",
            component.name,
            component.share / weight * 100.0,
            component.low,
            component.high,
            component.rows_text()
        ));
    }
    let text = format!(
        "fidelity: the work clock reads about {lo:.2}-{hi:.2}x a client's time on {what}: each component's error on rows no fit saw (ree / client from the 10th to the 90th percentile, the Studio-era prices' over the range of their rows), weighted by that code's work: {}{device_note}. Size a cut in a client",
        parts.join(", ")
    );
    let prices = if phone {
        format!("phone prices: {} times {:.2}", crate::cost::REFERENCE_CLIENT, crate::cost::PHONE_SCRIPTS)
    } else {
        format!("desktop prices: {}", crate::cost::REFERENCE_CLIENT)
    };

    let mut component_json = serde_json::Map::new();
    for component in &components {
        let row = json!({"share": round(component.share / weight, 4), "band": [component.low, component.high], "rows": component.rows, "basis": component.basis()});
        component_json.insert(component.name.to_string(), row);
    }
    json!({
        "text": text,
        "band": [round(lo, 3), round(hi, 3)],
        "clock_band": [round(clock_lo, 3), round(clock_hi, 3)],
        "weighted": what,
        "components": component_json,
        "unmeasured_share": round(defaulted / total, 4),
        "studio_share": round(studio / total, 4),
        "device": {"name": device, "scale": price_scale(), "band": [device_lo, device_hi], "ratio_band": [round(ratio_lo, 3), round(ratio_hi, 3)]},
        "prices": prices,
        "reference": crate::cost::REFERENCE_CLIENT,
    })
}

/// The line of source the instruction `pc` of `function` is on
fn source_line(names: &Names, function: i32, pc: i32) -> Option<i32> {
    names.function(function).and_then(|f| f.lines.get(pc.max(0) as usize).copied())
}

/// "module:line" for the instruction `pc` of `function`; "(engine)" for the engine's own work
fn line_label(names: &Names, function: i32, pc: i32) -> String {
    match names.function(function) {
        Some(f) => format!("{}:{}", short_module(module_name(&f.source)), f.lines.get(pc.max(0) as usize).copied().unwrap_or(f.line)),
        None if function < 0 => "(engine)".to_string(),
        None => names.frame(function),
    }
}

/// The five dearest instructions that paid cold charges, for profile.json
fn payers_json(names: &Names, payers: &[vm::Payer]) -> Vec<Json> {
    let mut rows = Vec::new();
    for payer in payers.iter().take(5) {
        rows.push(json!({"function": payer.function, "line": source_line(names, payer.function, payer.pc), "cost_ms": ms(payer.cost)}));
    }
    rows
}

/// A row of the summary's table of cold charges by object
struct ColdRow {
    cost: f64,
    kind: String,
    name: String,
    charges: u64,
    /// Charges in frames where other code touched the object too
    shared: u64,
    /// The line that paid most
    first_payer: String,
}

/// The rows' total cost, then the rows sorted for display: the dearest first, ties by kind and name. The total is added in the
/// order the rows were listed in, before the sort: profile.json keeps this sum unrounded, and a sum in display order can
/// differ from it in its last bits.
fn sum_then_sort_cold_rows(rows: &mut [ColdRow]) -> f64 {
    let total = rows.iter().map(|row| row.cost).sum();
    rows.sort_by(|a, b| b.cost.total_cmp(&a.cost).then(a.kind.cmp(&b.kind)).then(a.name.cmp(&b.name)));
    total
}

/// The cold and first-use charges by object, from the VM's (tables, C and engine functions) and the engine's (instances, members,
/// engine paths): the summary's table and profile.json's `cold`. How often other code shared each object in a frame shows what cutting
/// its first payer would really save.
fn cold_object_section(input: &Input, names: &Names, summary: &mut String, json: &mut Json, top: usize) {
    let mut rows: Vec<ColdRow> = Vec::new();
    let mut objects: Vec<Json> = Vec::new();
    if let Some(vm_results) = input.vm {
        for object in &vm_results.cold_objects {
            let (kind, name) = match object.kind {
                vm::COLD_TABLE if object.function >= 0 => ("table", format!("made at {}", line_label(names, object.function, object.pc))),
                vm::COLD_TABLE => ("table", "made before the profile".to_string()),
                vm::COLD_CFUNCTION => ("C function", vm_results.cfunctions.get(object.function.max(0) as usize).cloned().unwrap_or_default()),
                _ => ("engine function", names.label(object.function)),
            };
            let first_payer = object.payers.first().map(|payer| line_label(names, payer.function, payer.pc)).unwrap_or_default();
            rows.push(ColdRow { cost: object.cost, kind: kind.to_string(), name: name.clone(), charges: object.frames, shared: object.shared_frames, first_payer });
            objects.push(json!({
                "kind": kind, "name": name, "cost_ms": ms(object.cost), "frames": object.frames, "shared_frames": object.shared_frames,
                "payers": payers_json(names, &object.payers),
            }));
        }
    }
    if let Some(attribution) = input.attribution {
        for object in &attribution.cold {
            let first_payer = object.payers.first().map(|payer| line_label(names, payer.function, payer.pc)).unwrap_or_default();
            rows.push(ColdRow { cost: object.cost, kind: object.kind.to_string(), name: object.name.clone(), charges: object.charges, shared: object.shared, first_payer });
            objects.push(json!({
                "kind": object.kind, "name": object.name, "cost_ms": ms(object.cost), "frames": object.charges, "shared_frames": object.shared,
                "payers": payers_json(names, &object.payers),
            }));
        }
    }
    if rows.is_empty() {
        return;
    }
    let total = sum_then_sort_cold_rows(&mut rows);
    // (the run's cold charges: the objects' and what no object carries, a function's own code, closure and first run, which each
    // function's cold column holds)
    let run_cold = match (input.marks.first().and_then(|mark| mark.totals), input.end.and_then(|end| end.totals)) {
        (Some(first), Some(last)) => (last.cold - first.cold).max(total),
        _ => total,
    };
    let _ = writeln!(
        summary,
        "\ncold and first-use charges by object: {:.3} ms of the run's {:.3} ms, every phase (the rest is functions' own code, closures and first runs, in their cold column; shared: frames in which other code also touched it, so cutting the first payer moves the charge rather than saving it)",
        ms(total),
        ms(run_cold)
    );
    let _ = writeln!(summary, "{:>9} {:>8} {:>8}  object  (first paid by)", "ms", "charges", "shared");
    for row in rows.iter().take(top.min(12)) {
        let shared_percent = if row.charges > 0 { row.shared as f64 / row.charges as f64 * 100.0 } else { 0.0 };
        let _ = writeln!(summary, "{:>9.3} {:>8} {:>7.0}%  {} {}  ({})", ms(row.cost), group_digits(row.charges), shared_percent, row.kind, row.name, row.first_payer);
    }
    // (the dearest first; equal costs keep their order of listing)
    objects.sort_by(|a, b| b["cost_ms"].as_f64().unwrap_or(0.0).total_cmp(&a["cost_ms"].as_f64().unwrap_or(0.0)));
    objects.truncate(500);
    json["cold"] = json!({"total_ms": ms(run_cold), "objects_ms": ms(total), "objects": objects});
}

/// The handlers by the site that connected them: the engine's dispatch to each, and the runs' whole work
fn handler_section(attribution: &attribution::Results, names: &Names, summary: &mut String, json: &mut Json, top: usize) {
    if attribution.handlers.is_empty() {
        return;
    }
    let _ = writeln!(summary, "\nhandlers by the site that connected them (dispatch: the engine's delivery to each handler, booked to it; work: the runs' whole work, dispatch included)");
    let _ = writeln!(summary, "{:>9} {:>11} {:>8} {:>11}  signal  (connected at)", "work ms", "dispatch ms", "runs", "connections");
    for site in attribution.handlers.iter().take(top.min(10)) {
        let _ = writeln!(
            summary,
            "{:>9.3} {:>11.3} {:>8} {:>11}  {}  ({})",
            ms(site.work),
            ms(site.dispatch),
            group_digits(site.runs),
            group_digits(site.connections as u64),
            site.signal,
            line_label(names, site.function, site.pc)
        );
    }
    let mut sites = Vec::new();
    for site in &attribution.handlers {
        sites.push(json!({
            "signal": site.signal, "function": site.function, "line": source_line(names, site.function, site.pc),
            "connections": site.connections, "runs": site.runs, "dispatch_ms": ms(site.dispatch), "work_ms": ms(site.work),
        }));
    }
    json["handlers"] = Json::Array(sites);
}

/// The resumptions of parked threads, by the line each waited on
fn resumption_section(attribution: &attribution::Results, names: &Names, summary: &mut String, json: &mut Json, top: usize) {
    if attribution.resumes.is_empty() {
        return;
    }
    let count: u64 = attribution.resumes.iter().map(|site| site.count).sum();
    let cost: f64 = attribution.resumes.iter().map(|site| site.cost).sum();
    let _ = writeln!(summary, "\nresumptions of parked threads (task.wait, wait, task.delay): {} costing {:.3} ms, booked to the line each waited on", group_digits(count), ms(cost));
    for site in attribution.resumes.iter().take(top.min(6)) {
        let _ = writeln!(summary, "{:>9.3} ms {:>8}  {}", ms(site.cost), group_digits(site.count), line_label(names, site.function, site.pc));
    }
    let mut sites = Vec::new();
    for site in &attribution.resumes {
        sites.push(json!({"function": site.function, "line": source_line(names, site.function, site.pc), "count": site.count, "cost_ms": ms(site.cost)}));
    }
    json["resumptions"] = Json::Array(sites);
}

/// Whether a line that read os.clock is a budget loop's check: it read the clock more than once a run of its function, and two of its
/// reads in one run made an interval
fn is_budget_loop(line: &attribution::ClockLine) -> bool {
    line.reads > line.runs && line.intervals > 0
}

/// A budget loop's work an iteration in microseconds: the clock's advance between two reads by one run, its callees' included
fn iteration_us(line: &attribution::ClockLine) -> Option<f64> {
    if line.intervals > 0 { Some(line.between / line.intervals as f64 * 1e6) } else { None }
}

/// The lines that read os.clock more than once a run of their function (a call, or what is left of one after its thread yielded): a
/// budget loop's check, whose work is set by the clock (on a frozen clock it does other work than a client's). A read after a yield
/// starts a run, so a loop that waits each iteration is none.
fn clock_section(attribution: &attribution::Results, names: &Names, summary: &mut String, json: &mut Json, top: usize) {
    let budget_loops: Vec<&attribution::ClockLine> = attribution.clock_reads.iter().filter(|line| is_budget_loop(line)).collect();
    if !budget_loops.is_empty() {
        let _ = writeln!(
            summary,
            "\nos.clock budget loops (lines that read the clock more than once a run of their function, a call or what is left of one after a yield: the loop's work is set by its budget, so compare its work an iteration, and profile on the work clock, not a frozen one)"
        );
        let _ = writeln!(summary, "{:>10} {:>8} {:>8} {:>10} {:>16}  line", "reads", "runs", "frames", "per run", "us an iteration");
        for line in budget_loops.iter().take(top.min(8)) {
            let _ = writeln!(
                summary,
                "{:>10} {:>8} {:>8} {:>10.1} {:>16.3}  {}",
                group_digits(line.reads),
                group_digits(line.runs),
                group_digits(line.frames),
                line.reads as f64 / line.runs.max(1) as f64,
                iteration_us(line).unwrap_or(0.0),
                line_label(names, line.function, line.pc)
            );
        }
    }
    let mut lines = Vec::new();
    for line in &attribution.clock_reads {
        lines.push(json!({
            "function": line.function, "line": source_line(names, line.function, line.pc),
            "reads": line.reads, "frames": line.frames, "runs": line.runs, "iterations": line.intervals,
            "iteration_us": iteration_us(line), "budget_loop": is_budget_loop(line),
        }));
    }
    json["clock_reads"] = Json::Array(lines);
}

/// The summary's and profile.json's attributions: the cold and first-use charges by object (the VM's: tables by the line that made them,
/// C and engine functions; the engine's: instances by class, members, engine paths), with how often other code shared each in a frame;
/// handlers by the site that connected them; resumptions by the line threads waited on; events priced by a borrowed price
fn attribution_sections(input: &Input, names: &Names, summary: &mut String, json: &mut Json, top: usize) {
    cold_object_section(input, names, summary, json, top);
    let Some(attribution) = input.attribution else { return };
    handler_section(attribution, names, summary, json, top);
    resumption_section(attribution, names, summary, json, top);
    clock_section(attribution, names, summary, json, top);
    if attribution.unmeasured_fires > 0 {
        let _ = writeln!(summary, "events of kinds no bench measured fired {} times with handlers: each priced as a heard BindableEvent's (a borrowed price, in the defaulted share)", group_digits(attribution.unmeasured_fires));
        json["unmeasured_fires"] = json!(attribution.unmeasured_fires);
    }
}

/// What the scripts asked the engine to do (cost::EngineOps), each count by name
fn engine_json(ops: &crate::cost::EngineOps) -> Json {
    Json::Object(ops.named().iter().map(|(name, count)| (name.to_string(), json!(count))).collect())
}

/// The summary's line of what the scripts asked the engine to do in the reported phases: the instances made, destroyed and taken
/// into the Workspace, the property writes (the moves and switches among them), pivots and bulk moves, emits and queries. Counts,
/// whatever the work clock prices them at: the engine's side of a cast, which the work clock reads lower than a client pays.
fn engine_line(scope_label: &str, ops: &crate::cost::EngineOps) -> String {
    let count = |n: u64, one: &str, many: &str| (n > 0).then(|| format!("{} {}", group_digits(n), if n == 1 { one } else { many }));
    let mut groups: Vec<String> = Vec::new();
    let made: Vec<String> = [
        count(ops.cloned, "instance cloned", "instances cloned"),
        count(ops.created, "made with Instance.new", "made with Instance.new"),
        count(ops.destroyed, "destroyed", "destroyed"),
        count(ops.entered, "entered the Workspace", "entered the Workspace"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !made.is_empty() {
        groups.push(made.join(", "));
    }
    if ops.writes > 0 {
        let among: Vec<String> = [
            count(ops.geometry, "moved or resized a part in the Workspace", "moved or resized a part in the Workspace"),
            count(ops.toggles, "switched an effect", "switched an effect"),
        ]
        .into_iter()
        .flatten()
        .collect();
        groups.push(format!(
            "{} ({} changed a value{})",
            count(ops.writes, "property write", "property writes").unwrap_or_default(),
            group_digits(ops.changed),
            if among.is_empty() { String::new() } else { format!(": {}", among.join(", ")) }
        ));
    }
    let moves: Vec<String> = [count(ops.pivots, "pivot", "pivots"), count(ops.bulk_moved, "part bulk-moved", "parts bulk-moved")].into_iter().flatten().collect();
    if !moves.is_empty() {
        groups.push(moves.join(", "));
    }
    groups.extend(count(ops.emits, "emit", "emits"));
    groups.extend(count(ops.queries, "descendant query", "descendant queries"));
    // (counted at the engine, so every script's, the harness's too: its scans are most of an attack clip's descendant queries)
    if groups.is_empty() {
        format!("engine operations, {scope_label}: none asked by scripts")
    } else {
        format!("engine operations, {scope_label}: {} (every script's, the harness's too)", groups.join("; "))
    }
}

/// A distribution of a table part's size: its slots, those holding a value, the least, median and greatest part, and how
/// many tables fall in each power-of-two bucket
fn distribution_json(distribution: &memory::Distribution) -> Json {
    json!({"slots": distribution.total, "used": distribution.used, "min": distribution.min, "median": distribution.median, "max": distribution.max, "hist": distribution.hist})
}

/// A distribution of nesting levels
fn levels_json(distribution: &memory::Distribution) -> Json {
    json!({"min": distribution.min, "median": distribution.median, "max": distribution.max, "hist": distribution.hist})
}

/// `1.23G`, `45.6M`, `789K` (whole numbers below ten thousand)
fn compact(value: u64) -> String {
    let value = value as f64;
    if value >= 1e9 {
        format!("{:.2}G", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value >= 1e4 {
        format!("{:.0}K", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

/// A signed count in at most six characters (booked counts are estimates and may be below zero for a stack)
fn compact_signed(value: f64) -> String {
    if value < 0.0 { format!("-{}", compact((-value).round() as u64)) } else { compact(value.round() as u64) }
}

/// The hardware counters' part of the summary and of profile.json (`counters`), and their folded stacks: one file a
/// counter (`counters.<name>.folded`), each stack weighted by what was counted while its call was the innermost sampled
/// one (an estimate where calls were sampled; a stack whose estimate falls below zero cannot be in a folded file, so
/// the summary lists those and profile.json says what the folded files left out)
fn counters(input: &Input, hw: &hw::Report, names: &Names, summary: &mut String, folded: &mut Vec<(String, Vec<(String, u64)>)>) -> Json {
    use hw::{Figures, counters as known};
    let n = hw.names.len();
    let whole = |value: f64| -> Json { json!(value.round() as i64) };
    let object = |counts: &Figures| -> Json { Json::Object(hw.names.iter().zip(counts).map(|(name, value)| (name.clone(), whole(*value))).collect()) };
    let rates = |counts: &Figures| -> Json { Json::Object(hw::figures(&hw.names, counts).into_iter().map(|figure| (figure.key.to_string(), json!(round(figure.value, 6)))).collect()) };
    let figure = |counts: &Figures, key: &str| hw::figures(&hw.names, counts).into_iter().find(|figure| figure.key == key);
    let mut out = json!({
        "source": hw.source,
        "names": hw.names,
        "sources": hw.sources,
        "about": hw.names.iter().map(|name| known::known(name).map(|known| known.about)).collect::<Vec<_>>(),
        "amd": hw.names.iter().map(|name| known::known(name).and_then(|known| (!known.amd.is_empty()).then_some(known.amd))).collect::<Vec<_>>(),
        "error": hw.error,
        "notes": hw.notes,
        "cut_short": hw.cut_short,
    });
    let _ = writeln!(summary);
    if let Some(error) = &hw.error {
        let _ = writeln!(summary, "hardware counters: not read: {error}");
        for note in &hw.notes {
            let _ = writeln!(summary, "  note: {note}");
        }
        return out;
    }
    let listed = |counts: &Figures| hw.names.iter().zip(counts).map(|(name, value)| format!("{name} {}", group_digits(value.max(0.0).round() as u64))).collect::<Vec<_>>().join(", ");
    let derived = |counts: &Figures| hw::figures(&hw.names, counts).into_iter().map(|figure| format!("{} {}", figure.key, figure.text())).collect::<Vec<_>>().join(", ");
    // the tables sort by the cycles stalled on loads when counted, else the cycles, else the first counter
    let lead = hw.names.iter().position(|name| name == "load-stall-cycles").or_else(|| hw.names.iter().position(|name| name == "cycles")).unwrap_or(0);
    let lead_name = &hw.names[lead];
    let cycles = hw.names.iter().position(|name| name == "cycles");
    let _ = writeln!(
        summary,
        "hardware counters ({}; the run thread over {:.2} s; {} readings, {} of them around {} sampled Luau calls): {}",
        if hw.source == "mock" { "REE_HW_COUNTERS=mock: made-up counts from the thread's cycle count" } else { "read by the counter service" },
        hw.run_seconds,
        group_digits(hw.readings as u64),
        group_digits(2 * hw.calls as u64),
        group_digits(hw.calls as u64),
        listed(&hw.run)
    );
    if let Some(seconds) = hw.cut_short {
        let _ = writeln!(
            summary,
            "  WARNING: hardware-counter profile CUT SHORT after {seconds:.3} s: the service's slice limit was reached; \
             the rest of the run has no counter figures (count a shorter run)"
        );
    }
    let run_rates = derived(&hw.run);
    if !run_rates.is_empty() {
        let _ = writeln!(summary, "  {run_rates}");
    }
    // what memory cost the run, said plainly
    match (figure(&hw.run, known::LOAD_STALL_SHARE), figure(&hw.run, known::STALL_PER_THOUSAND)) {
        (Some(share), Some(per)) => {
            let latency = figure(&hw.run, known::MISS_LATENCY).map(|latency| format!("; an L1 data cache miss stayed outstanding {} cycles on average", latency.text())).unwrap_or_default();
            let overlap = figure(&hw.run, known::MISSES_OUTSTANDING).map(|outstanding| format!(", {} outstanding on average over all cycles", outstanding.text())).unwrap_or_default();
            let _ = writeln!(summary, "  memory: {} of the run thread's cycles nothing retired because the oldest op waited for load data ({} cycles per 1000 instructions){latency}{overlap}", share.text(), per.text());
        }
        _ if !hw.names.iter().any(|name| name == "load-stall-cycles") => {
            let _ = writeln!(summary, "  memory: not measured (load-stall-cycles not counted): fills count a line once and prefetched lines not at all, so they are no measure of what memory cost");
        }
        _ => {}
    }
    let readings_share = match (&hw.per_reading, cycles) {
        (Some(per_reading), Some(cycles)) => per_reading[cycles] as f64 * hw.readings as f64 / (hw.run[cycles] + per_reading[cycles] as f64 * hw.readings as f64).max(1.0),
        _ => 0.0,
    };
    if let Some(per_reading) = &hw.per_reading {
        let _ = writeln!(
            summary,
            "  a reading's own counts, taken out of every stretch between readings (the median of the back-to-back ones at the start; {:.1} us each): {}; the readings took {} of the run thread's cycles",
            hw.reading_us,
            hw.names.iter().zip(per_reading).map(|(name, value)| format!("{name} {}", group_digits(*value))).collect::<Vec<_>>().join(", "),
            percent(readings_share, 1.0)
        );
    }
    if hw.calls > 0 || hw.sampled_functions > 0 {
        let _ = writeln!(
            summary,
            "  calls: {} read around, of {} functions measured: each call drawn on its own, every call of a function whose calls take {:.0} us or more, one in (that over its mean call) of shorter ones, a recursion's inner calls with its sampled outer one; a function's first {} calls all, to learn its length; none of the {} whose calls take under {:.0} us (their counts are their callers', those first calls' too: {} calls read for their length alone), one of their calls measured now and then in case they grow",
            group_digits(hw.calls as u64),
            group_digits(hw.sampled_functions as u64),
            hw.threshold_us,
            hw::MIN_MEASURED,
            group_digits(hw.unsampled_functions as u64),
            hw::MIN_SAMPLED_US,
            group_digits(hw.transparent_calls as u64)
        );
        if hw.switch_readings > 0 {
            let _ = writeln!(summary, "  thread switches: {} readings where a Luau thread holding a sampled call resumed or stopped, so each thread's work is booked to its own calls", group_digits(hw.switch_readings as u64));
        }
    }
    if hw.matched + 2 < hw.readings {
        let _ = writeln!(summary, "  warning: {} of {} readings found no switch of their own: their windows were merged into the next ones", hw.readings - hw.matched, hw.readings);
    }
    if hw.lost_events > 0 || hw.lost_buffers > 0 {
        let _ = writeln!(summary, "  warning: the trace session lost {} events and {} buffers: some of the threads' stretches are missing", group_digits(hw.lost_events as u64), group_digits(hw.lost_buffers as u64));
    }
    // (a switch without values matters where it cost a reading its switch, which the readings' warning says; a service
    // before this one also counted every CPU's switches from before its counters were set)
    if hw.without_counters > 0 {
        let _ = writeln!(summary, "  note: {} context switches came without counter values", group_digits(hw.without_counters));
    }
    for note in &hw.notes {
        let _ = writeln!(summary, "  note: {note}");
    }
    let phase_name = |phase: u32| input.phases.get(phase as usize).cloned().unwrap_or_else(|| format!("phase {phase}"));
    let cost_text = |counts: &Figures| [known::LOAD_STALL_SHARE, "IPC"].iter().filter_map(|key| figure(counts, key)).map(|figure| format!("{} {}", figure.key, figure.text())).collect::<Vec<_>>().join(", ");
    if !hw.phases.is_empty() {
        let parts: Vec<String> = hw
            .phases
            .iter()
            .map(|(phase, counts)| {
                let cost = cost_text(counts);
                format!("{} {lead_name} {}{}", phase_name(*phase), compact_signed(counts[lead]), if cost.is_empty() { String::new() } else { format!(", {cost}") })
            })
            .collect();
        let _ = writeln!(summary, "  by phase: {}", parts.join("; "));
    }
    let thread_label = |thread: &hw::ThreadCounts| match (&thread.name, thread.run_thread) {
        (Some(name), true) => format!("the run thread ({name})"),
        (None, true) => "the run thread".to_string(),
        (Some(name), false) => name.clone(),
        (None, false) => format!("thread {}", thread.id),
    };
    let by_thread = cycles.unwrap_or(lead);
    if !hw.threads.is_empty() {
        let mut parts: Vec<String> = hw.threads.iter().take(6).map(|thread| format!("{} {}", thread_label(thread), compact_signed(thread.counts[by_thread]))).collect();
        if hw.threads.len() > 6 {
            let rest: f64 = hw.threads[6..].iter().map(|thread| thread.counts[by_thread]).sum();
            parts.push(format!("{} others {}", hw.threads.len() - 6, compact_signed(rest)));
        }
        let _ = writeln!(summary, "  {} by thread: {}", hw.names[by_thread], parts.join("; "));
    }
    if !hw.frames.is_empty() {
        let values: Vec<f64> = hw.frames.iter().map(|frame| frame.counts[lead]).collect();
        let worst = values.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(index, _)| index);
        let mut line = format!(
            "  frames: {}, {lead_name} a frame mean {}, max {} (frame {})",
            hw.frames.len(),
            compact_signed(values.iter().sum::<f64>() / values.len() as f64),
            compact_signed(values[worst]),
            hw.frames[worst].frame
        );
        let shares: Vec<(usize, f64)> = hw.frames.iter().enumerate().filter_map(|(index, frame)| figure(&frame.counts, known::LOAD_STALL_SHARE).map(|figure| (index, figure.value))).collect();
        if let Some(&(most, share)) = shares.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
            let mut sorted: Vec<f64> = shares.iter().map(|(_, share)| *share).collect();
            sorted.sort_by(f64::total_cmp);
            let _ = write!(line, "; load stall share a frame median {:.1} %, most {:.1} % (frame {})", sorted[sorted.len() / 2] * 100.0, share * 100.0, hw.frames[most].frame);
        }
        let _ = writeln!(summary, "{line}");
    }

    // per function and call site: a stack's innermost frame is a sampled call's function (self), every frame on it once
    // its total; a caller's frame carries the line it called from, so a line's total is what the calls it made cost
    let mut functions: FxHashMap<i32, (Figures, Figures)> = FxHashMap::default();
    let mut lines: FxHashMap<(i32, i32), Figures> = FxHashMap::default();
    let mut by_counter: Vec<HashMap<String, f64>> = vec![HashMap::new(); n];
    let add = |total: &mut Figures, counts: &Figures| {
        for (total, count) in total.iter_mut().zip(counts) {
            *total += count;
        }
    };
    for (&(phase, stack), counts) in &hw.by_stack {
        let frames = &hw.stacks.stacks[stack as usize];
        if let Some(&(function, _)) = frames.first() {
            add(&mut functions.entry(function).or_default().0, counts);
        }
        let mut seen_functions: FxHashSet<i32> = FxHashSet::default();
        let mut seen_lines: FxHashSet<(i32, i32)> = FxHashSet::default();
        for &(function, line) in frames.iter() {
            if seen_functions.insert(function) {
                add(&mut functions.entry(function).or_default().1, counts);
            }
            if function >= 0 && line >= 0 && seen_lines.insert((function, line)) {
                add(lines.entry((function, line)).or_default(), counts);
            }
        }
        let key = fold(names, input.phases, phase, frames);
        for (index, rows) in by_counter.iter_mut().enumerate() {
            *rows.entry(key.clone()).or_default() += counts[index];
        }
    }
    // (a folded file's weights cannot fall below zero: the stacks whose estimate did are left out, and said so)
    let mut left_out: Vec<(usize, f64)> = vec![(0, 0.0); n];
    for (index, rows) in by_counter.into_iter().enumerate() {
        let file: String = hw.names[index].chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        for value in rows.values().filter(|value| **value <= -0.5) {
            left_out[index].0 += 1;
            left_out[index].1 += value;
        }
        folded.push((format!("counters.{file}.folded"), rows.into_iter().filter(|(_, value)| *value >= 0.5).map(|(key, value)| (key, value.round() as u64)).collect()));
    }
    let kind = |id: i32| -> &'static str {
        match names.function(id) {
            Some(f) if f.exempt() => "engine Luau",
            Some(_) => "Luau",
            None if id <= OUTSIDE_BASE => "outside Luau",
            None if id <= ENGINE_STEP_BASE => "engine step",
            None => "C",
        }
    };
    let mut function_rows: Vec<(i32, (Figures, Figures))> = functions.into_iter().collect();
    function_rows.sort_by(|a, b| b.1.0[lead].total_cmp(&a.1.0[lead]).then(b.1.1[lead].total_cmp(&a.1.1[lead])).then(a.0.cmp(&b.0)));
    let share_of = |counts: &Figures| figure(counts, known::LOAD_STALL_SHARE).map(|figure| figure.text()).unwrap_or_else(|| "-".into());
    let ipc_of = |counts: &Figures| figure(counts, "IPC").map(|figure| figure.text()).unwrap_or_else(|| "-".into());
    let _ = writeln!(summary, "\ntop functions by self {lead_name} (readings around sampled calls: a function's own counts are its calls', its callees' apart, estimated where one call in several was sampled; total includes callees; a reading costs the code after it some of its caches, so a short sampled function reads a little high and its callers low)");
    let _ = writeln!(summary, "{}  {:>7}  {:>5}  function", hw.names.iter().map(|name| format!(" {name:>13}")).collect::<Vec<_>>().join(""), "stalled", "IPC");
    for (id, (own, _)) in function_rows.iter().filter(|(_, (own, _))| own[lead] > 0.0).take(input.config.top) {
        let _ = writeln!(summary, "{}  {:>7}  {:>5}  {}", own[..n].iter().map(|value| format!(" {:>13}", compact_signed(*value))).collect::<Vec<_>>().join(""), share_of(own), ipc_of(own), names.label(*id));
    }
    // estimates below zero: the sampling's noise (what came off a function for its callees' unsampled calls outweighed
    // what its own sampled calls read), shown so the tables above add up
    let below: Vec<String> = function_rows.iter().filter(|(_, (own, _))| own[lead] <= -0.5).map(|(id, (own, _))| format!("{} {}", names.label(*id), compact_signed(own[lead]))).collect();
    if !below.is_empty() {
        let shown = below.len().min(input.config.top.max(5));
        let rest = if below.len() > shown { format!("; {} more", below.len() - shown) } else { String::new() };
        let _ = writeln!(summary, "estimated below zero by self {lead_name} (sampling noise: what came off them for their callees' unsampled calls outweighed what their sampled calls read): {}{rest}", below[..shown].join("; "));
    }
    if left_out[lead].0 > 0 {
        let _ = writeln!(summary, "the folded files leave out the stacks estimated below zero: {} stacks, {lead_name} {}", group_digits(left_out[lead].0 as u64), compact_signed(left_out[lead].1));
    }
    let mut line_rows: Vec<((i32, i32), Figures)> = lines.into_iter().collect();
    line_rows.sort_by(|a, b| b.1[lead].total_cmp(&a.1[lead]).then(a.0.cmp(&b.0)));
    let _ = writeln!(summary, "\ntop call sites by the {lead_name} of the calls they make (a line's own code is its function's)");
    for ((function, line), total) in line_rows.iter().filter(|(_, total)| total[lead] > 0.0).take(input.config.top.min(15)) {
        let module = names.function(*function).map_or(String::new(), |f| names.short(&f.source).to_string());
        let _ = writeln!(summary, "{}  {:>7}  {:>5}  {module}:{line}", total[..n].iter().map(|value| format!(" {:>13}", compact_signed(*value))).collect::<Vec<_>>().join(""), share_of(total), ipc_of(total));
    }

    // (version 2: each call drawn on its own, a function's first calls all read and its short calls measured again, the
    // readings where threads switch; a profile without a version drew one call of each run of calls and read no switch)
    out["attribution"] = json!({
        "method": "calls", "version": ATTRIBUTION_VERSION,
        "calls": hw.calls, "calls_booked_to_callers": hw.transparent_calls, "switch_readings": hw.switch_readings,
        "functions": hw.sampled_functions, "unsampled_functions": hw.unsampled_functions,
        "threshold_us": round(hw.threshold_us, 3), "min_sampled_us": hw::MIN_SAMPLED_US, "min_measured": hw::MIN_MEASURED, "readings_a_second": hw::READINGS_A_SECOND,
        "readings_share_of_cycles": round(readings_share, 6),
        "folded_left_out": Json::Object(hw.names.iter().zip(&left_out).map(|(name, (stacks, sum))| (name.clone(), json!({"stacks": stacks, "sum": sum.round() as i64}))).collect()),
    });
    out["run"] = json!({"counts": object(&hw.run), "rates": rates(&hw.run), "seconds": hw.run_seconds});
    out["readings"] = json!({"count": hw.readings, "matched": hw.matched, "calls": hw.calls, "own_counts": hw.per_reading.as_ref().map(|own| object(&own.map(|value| value as f64))), "own_us": round(hw.reading_us, 3), "taken_out": true});
    out["trace"] = json!({"switches": hw.switches, "without_counters": hw.without_counters, "lost_events": hw.lost_events, "lost_buffers": hw.lost_buffers});
    out["threads"] = Json::Array(hw.threads.iter().map(|thread| json!({"id": thread.id, "name": thread.name, "run_thread": thread.run_thread, "slices": thread.slices, "counts": object(&thread.counts)})).collect());
    out["phases"] = Json::Array(hw.phases.iter().map(|(phase, counts)| json!({"phase": phase_name(*phase), "counts": object(counts), "rates": rates(counts)})).collect());
    // a row a frame: its counts, then the cost figures they allow
    let frame_figures: Vec<&str> = [known::LOAD_STALL_SHARE, known::STALL_PER_THOUSAND, known::MISS_LATENCY, "IPC"].into_iter().filter(|key| hw.frames.iter().any(|frame| figure(&frame.counts, key).is_some())).collect();
    let mut columns = vec!["frame".to_string(), "phase".to_string(), "host_ms".to_string()];
    columns.extend(hw.names.iter().cloned());
    columns.extend(frame_figures.iter().map(|key| key.to_string()));
    out["frames"] = json!({
        "columns": columns,
        "rows": hw.frames.iter().map(|frame| {
            let mut row = vec![json!(frame.frame), json!(frame.phase), json!(round(frame.host_ms, 4))];
            row.extend(frame.counts[..n].iter().map(|value| whole(*value)));
            row.extend(frame_figures.iter().map(|key| figure(&frame.counts, key).map_or(json!(0.0), |figure| json!(round(figure.value, 6)))));
            Json::Array(row)
        }).collect::<Vec<_>>(),
    });
    out["functions"] = Json::Array(function_rows.iter().take(2000).map(|(id, (own, total))| {
        let function = names.function(*id);
        json!({
            "id": id, "key": function_key(names, *id), "name": function.map_or_else(|| names.frame(*id), |f| f.name.clone()),
            "module": function.map(|f| module_name(&f.source).to_string()), "line": function.map(|f| f.line), "kind": kind(*id),
            "self": object(own), "total": object(total), "self_rates": rates(own), "total_rates": rates(total),
        })
    }).collect());
    // (a line's own code is its function's: "self" is empty, "total" is what the calls made from it cost)
    out["lines"] = Json::Array(line_rows.iter().take(2000).map(|((function, line), total)| {
        let f = names.function(*function);
        json!({"function": function, "name": f.map(|f| f.name.clone()), "module": f.map(|f| module_name(&f.source).to_string()), "line": line, "self": {}, "total": object(total), "total_rates": rates(total), "calls": true})
    }).collect());
    out
}

/// profile.json `counters.attribution.version`: how the counters reached functions (see [`counters`])
pub const ATTRIBUTION_VERSION: u32 = 2;

/// How a profile.json's counters reached functions, in words (`profile diff` and the viewer say when two differ)
pub fn attribution_of(counters: &Json) -> &'static str {
    match (counters["attribution"]["method"].as_str(), counters["attribution"]["version"].as_u64()) {
        (Some("calls"), Some(2..)) => "readings around sampled calls, each drawn on its own, and where threads switch",
        (Some("calls"), _) => "readings around sampled calls, one drawn of each run of calls, threads' switches not read",
        _ => "1 ms samples, each booked to the stack the next reading found",
    }
}

/// A stable key for a function across runs (its module and name, and its line for anonymous ones)
fn function_key(names: &Names, id: i32) -> String {
    match names.function(id) {
        Some(f) if f.name.is_empty() => format!("{}:anonymous@{}", module_name(&f.source), f.line),
        Some(f) => format!("{}:{}", module_name(&f.source), f.name),
        None => names.frame(id),
    }
}

/// A function's bytecode hashed as the compiler wrote it (profile.json `code_hash`): the same code hashes the same wherever
/// it moves in its file, so `ree profile diff` pairs a function across an edit that moved it (an anonymous function's key
/// carries its line). What a run changes in the code is taken out: the slot a table-key, global or method instruction
/// predicts (the VM patches it into the instruction's C field), and the loader's GETUDATAKS / SETUDATAKS / NAMECALLUDATA
/// forms, which read as the GETTABLEKS / SETTABLEKS / NAMECALL they stand for (the profile writes the plain form once a
/// site runs). A second word (an aux) is hashed as it is. Line numbers live outside the code, so a move changes nothing
fn code_hash(function: &vm::Function) -> Option<String> {
    if function.code.is_empty() {
        return None;
    }
    let op = |name: &str| vm::OPCODES.iter().position(|op| *op == name).map_or(u32::MAX, |index| index as u32);
    let rewritten = [(op("GETUDATAKS"), op("GETTABLEKS")), (op("SETUDATAKS"), op("SETTABLEKS")), (op("NAMECALLUDATA"), op("NAMECALL"))];
    let slotted = [op("GETGLOBAL"), op("SETGLOBAL"), op("GETTABLEKS"), op("SETTABLEKS"), op("NAMECALL")];
    // (Luau's BytecodeUtils.h getOpLength: the instructions followed by an aux word)
    let with_aux: Vec<u32> = [
        "GETGLOBAL", "SETGLOBAL", "GETIMPORT", "GETTABLEKS", "SETTABLEKS", "NAMECALL", "JUMPIFEQ", "JUMPIFLE", "JUMPIFLT", "JUMPIFNOTEQ", "JUMPIFNOTLE", "JUMPIFNOTLT", "NEWTABLE", "SETLIST", "FORGLOOP", "LOADKX",
        "FASTCALL2", "FASTCALL2K", "FASTCALL3", "JUMPXEQKNIL", "JUMPXEQKB", "JUMPXEQKN", "JUMPXEQKS", "GETUDATAKS", "SETUDATAKS", "NAMECALLUDATA", "NEWCLASSMEMBER", "CALLFB", "CMPPROTO", "NEWCLASS", "CONSTRUCT",
        "GETPRIVATEFIELD", "SETPRIVATEFIELD",
    ]
    .iter()
    .map(|name| op(name))
    .collect();
    let mut bytes: Vec<u8> = Vec::with_capacity(function.code.len() * 4);
    let mut pc = 0;
    while pc < function.code.len() {
        let insn = function.code[pc];
        let loaded = insn & 0xff;
        let opcode = rewritten.iter().find(|(from, _)| *from == loaded).map_or(loaded, |(_, to)| *to);
        let mut word = (insn & !0xff) | opcode;
        if slotted.contains(&opcode) {
            word &= 0x00ff_ffff;
        }
        bytes.extend_from_slice(&word.to_le_bytes());
        if with_aux.contains(&loaded) {
            if let Some(aux) = function.code.get(pc + 1) {
                bytes.extend_from_slice(&aux.to_le_bytes());
            }
            pc += 2;
        } else {
            pc += 1;
        }
    }
    Some(format!("{:016x}", fxhash(&bytes)))
}

/// A function's source text hashed (profile.json `source_hash`): its lines from where it starts to its last instruction's,
/// each trimmed, so a function moved or re-indented hashes the same and one whose text changed does not. None when the
/// profile did not keep its module's source
fn source_hash(sources: &FxHashMap<String, Source>, function: &vm::Function) -> Option<String> {
    let source = sources.get(module_name(&function.source))?;
    let text = String::from_utf8_lossy(&source.text);
    let first = function.line.max(1) as usize;
    let last = function.lines.iter().copied().max().unwrap_or(function.line).max(function.line).max(1) as usize;
    let span: Vec<&str> = text.lines().skip(first - 1).take(last + 1 - first).map(str::trim).collect();
    (!span.is_empty()).then(|| format!("{:016x}", fxhash(span.join("\n").as_bytes())))
}

fn fxhash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// `phase;outermost;...;innermost @line` (the legacy folded form; the innermost frame carries its line)
fn fold(names: &Names, phases: &[String], phase: u32, stack: &[memory::Frame]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(stack.len() + 1);
    if let Some(name) = phases.get(phase as usize) {
        parts.push(name.clone());
    }
    for (index, &(function, line)) in stack.iter().rev().enumerate() {
        let frame = names.frame(function);
        if index + 1 == stack.len() && line >= 0 {
            parts.push(format!("{frame} @{line}"));
        } else {
            parts.push(frame);
        }
    }
    parts.join(";")
}

/// The call tree as folded stacks: (path, self work in microseconds) and (path, self instructions)
fn fold_tree(vm: &vm::Results, names: &Names, phases: &[String]) -> (Vec<(String, u64)>, Vec<(String, u64)>) {
    let mut paths: Vec<Option<String>> = vec![None; vm.nodes.len()];
    let mut work = Vec::new();
    let mut insns = Vec::new();
    for (index, node) in vm.nodes.iter().enumerate() {
        let path = if index == 0 {
            phases.first().cloned().unwrap_or_else(|| "startup".into())
        } else if node.function == -1 {
            phases.get(node.callpc.max(0) as usize).cloned().unwrap_or_else(|| format!("phase {}", node.callpc))
        } else {
            let parent = paths[node.parent.max(0) as usize].clone().unwrap_or_default();
            format!("{parent};{}", names.frame(node.function))
        };
        if node.work > 0.0 {
            work.push((path.clone(), (node.work * 1e6).round() as u64));
        }
        if node.insns > 0 {
            insns.push((path.clone(), node.insns));
        }
        paths[index] = Some(path);
    }
    (work, insns)
}

/// speedscope's file format: shared frames, one sampled profile per folded file
fn speedscope(files: &[(String, Vec<(String, u64)>)], name: &str) -> Json {
    let mut frames: Vec<String> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut profiles = Vec::new();
    for (file, rows) in files {
        let unit = match file.as_str() {
            "work.folded" => "microseconds",
            "time.folded" => "milliseconds",
            "alloc.folded" | "heap.folded" => "bytes",
            _ => "none",
        };
        let mut samples = Vec::new();
        let mut weights = Vec::new();
        for (stack, weight) in rows.iter().filter(|row| row.1 > 0) {
            let sample: Vec<usize> = stack
                .split(';')
                .map(|frame| {
                    *index.entry(frame.to_string()).or_insert_with(|| {
                        frames.push(frame.to_string());
                        frames.len() - 1
                    })
                })
                .collect();
            samples.push(sample);
            weights.push(*weight);
        }
        let total: u64 = weights.iter().sum();
        profiles.push(json!({"type": "sampled", "name": file.trim_end_matches(".folded"), "unit": unit, "startValue": 0, "endValue": total, "samples": samples, "weights": weights}));
    }
    json!({
        "$schema": "https://www.speedscope.app/file-format-schema.json",
        "shared": {"frames": frames.iter().map(|frame| json!({"name": frame})).collect::<Vec<_>>()},
        "profiles": profiles,
        "name": format!("ree {name}"),
        "activeProfileIndex": 0,
        "exporter": "ree",
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The work a source edit adds that the VM's fast-path counterfactual does not hold. Explicit optional arguments change
/// setup and prototype size outside the builtin itself. The rule for pricing them is kept here, in one place.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FixWork {
    /// The warm instructions the edit adds to every declined call, in seconds over all the calls
    pub instructions: f64,
    /// The cold instructions the edit adds, in seconds; unknown when the replacement's executed PCs are unavailable
    pub cold: Option<f64>,
}

/// What the work clock charges one interpreted instruction (ns)
fn interpreted_op_ns(opcode_name: &str) -> f64 {
    let index = vm::OPCODES.iter().position(|candidate| *candidate == opcode_name).expect("priced opcode");
    crate::cost::INTERPRETED_OPS[index]
}

/// What advising explicit arguments adds to a builtin's call, or None when the advice has no measured cost
/// (`name` is the builtin and `op` the opcode of the call site)
pub(super) fn builtin_extra_work(name: &str, op: u8, declined_calls: u64) -> Option<FixWork> {
    if declined_calls == 0 {
        return Some(FixWork { instructions: 0.0, cold: Some(0.0) });
    }
    let opcode = vm::opcode_name(op);
    // The warm cost is nominal: register moves and the original argument expression are not recovered here.
    let warm_ns_per_call = match (name, opcode) {
        ("string.sub", "FASTCALL2") => interpreted_op_ns("LENGTH"),
        ("string.sub", "FASTCALL2K") => interpreted_op_ns("LENGTH") + interpreted_op_ns("LOADN"),
        ("string.sub", _) => return None,
        ("string.byte", "FASTCALL") => return None,
        ("table.insert" | "table.unpack", _) => return None,
        _ => 0.0,
    };
    // These two rewrites add argument code to the function. Cold charges follow the instruction headers that execute, not
    // code size or function activity, and the edit can add dead fallback words while removing executed instructions, so
    // neither gives the replacement's cold cost.
    let adds_argument_code = matches!((name, opcode), ("string.sub", "FASTCALL2" | "FASTCALL2K") | ("string.byte", "FASTCALL1"));
    let cold = if adds_argument_code { None } else { Some(0.0) };
    let instructions = warm_ns_per_call * declined_calls as f64 * 1e-9 * price_scale();
    Some(FixWork { instructions, cold })
}

/// A frame whose work reaches this share of the frame budget is heavy: it keeps its call-tree nodes' and sites' work in
/// the profile, so a hitch can be traced to them
const HEAVY_FRAME_SHARE: f64 = 0.5;

/// A frame's time at the run's rate (ms); 60 fps where the run recorded none
fn frame_budget_ms(fps: f64) -> f64 {
    1000.0 / if fps > 0.0 { fps } else { 60.0 }
}

/// Milliseconds of desktop-price work as microseconds at the run's prices, which is how the frame evidence stores them
fn microseconds(ms: f64) -> f64 {
    ms * 1000.0 * price_scale()
}

/// A row's work in the code (ms): the game's and the harness's, where the row splits them
fn row_code_work_ms(row: &Row) -> f64 {
    row.sides.map_or(row.work_ms, |(game, harness)| game + harness) * price_scale()
}

/// Each frame's work in the code (ms), the parts a phase mark split it into added together
fn whole_frame_work(rows: &[Row]) -> BTreeMap<u64, f64> {
    let mut whole: BTreeMap<u64, f64> = BTreeMap::new();
    for row in rows {
        *whole.entry(row.frame).or_default() += row_code_work_ms(row);
    }
    whole
}

/// The frames each function was active in, from the timeline's function summaries (stat: id, total, active, ...)
fn active_frames_by_function(json: &Json) -> BTreeMap<i32, u64> {
    let mut active = BTreeMap::new();
    let Some(stats) = json["timeline"]["function_stats"].as_array() else { return active };
    for stat in stats {
        if let (Some(function), Some(frames)) = (stat[0].as_i64(), stat[2].as_u64()) {
            active.insert(function as i32, frames);
        }
    }
    active
}

/// The calls a builtin site (or one context of it) had declined
fn declined_calls(site: &Json) -> u64 {
    let Some(counts) = site["declines"].as_object() else { return 0 };
    counts.values().map(|count| count.as_u64().unwrap_or(0)).sum()
}

/// What pricing one builtin site's fix found: the fix it belongs to and the cold code it adds
struct PricedSite {
    function: i64,
    /// The source edit this site belongs to: the builtin, function and line
    fix: String,
    /// The cold code the edit adds for one active frame (seconds): None when it cannot be known, 0 when it adds none.
    /// Only the first site of an edit carries it, as the edit pays it once.
    cold_per_frame: Option<f64>,
}

/// The last instruction of a declined builtin call's fallback, inclusive: the CALL that runs the full function. The fast
/// call's instruction (at `pc`) says in its top byte how far ahead that CALL is. The instructions between the two (a global
/// or import lookup, argument loads) run only in the fallback, and the work clock charges them inside the decline.
fn fallback_end_pc(function: &vm::Function, pc: usize) -> Option<usize> {
    let fast_call = function.code.get(pc)?;
    let distance_to_call = ((fast_call >> 24) & 255) as usize;
    Some(pc + distance_to_call)
}

/// Whether a builtin's fix adds code to the source (an argument the script left out). The instructions that code costs
/// are priced from the opcode alone, which does not recover the source expression or the fixed form's register layout, so
/// the price of such a fix is not exact (`fix_cost_exact`). The profile viewer lists the same builtins.
fn builtin_fix_adds_source_code(name: &str) -> bool {
    matches!(name, "string.sub" | "string.byte" | "table.insert" | "table.unpack")
}

/// Writes a builtin site's fix costs into its JSON (`fix_instructions_us`, `fix_cold_us`, and `fix_unpriced` where one is
/// unknown). None for a site the VM's results do not describe. `edits_seen` holds the edits already priced.
fn price_builtin_site(vm_results: &vm::Results, name: &str, site: &mut Json, active_frames: &BTreeMap<i32, u64>, edits_seen: &mut BTreeSet<String>) -> Option<PricedSite> {
    let (function, pc) = (site["function"].as_i64()?, site["pc"].as_u64()?);
    let prototype = vm_results.functions.get(function as usize)?;
    site["fix_cost_exact"] = json!(!builtin_fix_adds_source_code(name));
    let op = (*prototype.code.get(pc as usize)? & 255) as u8;
    let line = site["line"].as_i64().unwrap_or(0);
    let fix = format!("builtin:{name}:{function}:{line}");
    let calls = declined_calls(site);
    let frames = active_frames.get(&(function as i32)).copied();
    let extra_work = builtin_extra_work(name, op, calls);
    let cold_per_frame = if calls > 0 && edits_seen.insert(fix.clone()) { extra_work.and_then(|work| work.cold) } else { Some(0.0) };

    let mut instructions_us = extra_work.map(|work| work.instructions * 1e6);
    if let Some(contexts) = site["nodes"].as_array_mut() {
        // (each context pays for its own calls' instructions; the site's is their sum, unknown if any is)
        for context in contexts.iter_mut() {
            let own = builtin_extra_work(name, op, declined_calls(context));
            context["fix_instructions_us"] = own.map_or(Json::Null, |work| json!(work.instructions * 1e6));
        }
        let per_context: Option<Vec<f64>> = contexts.iter().map(|context| context["fix_instructions_us"].as_f64()).collect();
        instructions_us = per_context.map(|values| values.iter().sum());
    }
    let cold_is_known = cold_per_frame.is_some_and(|cold| cold == 0.0) || (cold_per_frame.is_some() && frames.is_some());
    site["fix_instructions_us"] = instructions_us.map_or(Json::Null, |us| json!(us));
    site["fix_cold_us"] = cold_per_frame.filter(|_| cold_is_known).map_or(Json::Null, |seconds| json!(seconds * frames.unwrap_or(0) as f64 * 1e6));
    if extra_work.is_none() {
        site["fix_unpriced"] = json!("the advised argument rewrite has no measured instruction cost");
    } else if !cold_is_known {
        site["fix_unpriced"] = json!("added cold-code work needs the replacement's executed instructions in each frame");
    }
    Some(PricedSite { function, fix, cold_per_frame })
}

/// Consecutive frame numbers as inclusive (first, last) ranges
fn frame_ranges(frames: impl IntoIterator<Item = u64>) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for frame in frames {
        match ranges.last_mut() {
            Some((_, last)) if frame == *last + 1 => *last = frame,
            _ => ranges.push((frame, frame)),
        }
    }
    ranges
}

/// For every call-tree context of a function whose edit adds cold code, the frames it ran in as `[function, node, first
/// frame, last frame, ...]`. A partial fix can remove one caller's frames: the cold code is kept only on the callers left.
fn function_contexts(vm_results: &vm::Results, rows: &[Row], scope: &[u32], cold_functions: &BTreeSet<i32>) -> Json {
    let mut contexts = Vec::new();
    for (node, context) in vm_results.nodes.iter().enumerate() {
        if !cold_functions.contains(&context.function) {
            continue;
        }
        let in_scope = rows.iter().filter(|row| scope.contains(&row.phase));
        let frames: BTreeSet<u64> = in_scope.filter(|row| row.nodes.iter().any(|(id, ms)| *id == node && *ms > 0.0)).map(|row| row.frame).collect();
        if frames.is_empty() {
            continue;
        }
        let mut cells = vec![json!(context.function), json!(node)];
        for (first, last) in frame_ranges(frames) {
            cells.extend([json!(first), json!(last)]);
        }
        contexts.push(Json::Array(cells));
    }
    Json::Array(contexts)
}

/// Prices the source-level replacement work of every builtin fix into the profile's JSON, apart from the VM's fast-path
/// counterfactual. The prototype cost belongs to the site as a whole: dividing it between callers would turn an exact
/// overlap into an estimate.
fn price_builtin_fixes(vm_results: &vm::Results, rows: &[Row], scope: &[u32], fps: f64, json: &mut Json) {
    let active_frames = active_frames_by_function(json);
    let whole = whole_frame_work(rows);
    let heavy_from_ms = frame_budget_ms(fps) * HEAVY_FRAME_SHARE;
    let mut replacement_frames = Vec::new();
    let mut cold_functions: BTreeSet<i32> = BTreeSet::new();
    let mut edits_seen: BTreeSet<String> = BTreeSet::new();
    if let Some(builtins) = json["builtins"].as_array_mut() {
        for builtin in builtins {
            let name = builtin["name"].as_str().unwrap_or("").to_string();
            let Some(sites) = builtin["sites"].as_array_mut() else { continue };
            for site in sites {
                let Some(priced) = price_builtin_site(vm_results, &name, site, &active_frames, &mut edits_seen) else { continue };
                let Some(cold_seconds) = priced.cold_per_frame.filter(|cold| *cold > 0.0) else { continue };
                cold_functions.insert(priced.function as i32);
                // (the edit's cold code is paid in each heavy frame the function was active in, once)
                let mut frames_seen = BTreeSet::new();
                for (index, row) in rows.iter().enumerate() {
                    let function_ran = row.functions.iter().any(|(id, ms, _)| *id == priced.function as i32 && *ms > 0.0);
                    if !scope.contains(&row.phase) || !function_ran || !frames_seen.insert(row.frame) {
                        continue;
                    }
                    if whole[&row.frame] >= heavy_from_ms {
                        replacement_frames.push(json!([index, priced.fix, cold_seconds * 1e6]));
                    }
                }
            }
        }
    }
    if json["timeline"].is_object() {
        json["timeline"]["replacement_frames"] = Json::Array(replacement_frames);
        json["timeline"]["function_contexts"] = function_contexts(vm_results, rows, scope, &cold_functions);
    }
}

/// What each issue contributed to each frame (ms), by the issue's key (`function:3`, `sort:3:12`, `builtin:math.abs:3:12`,
/// `cache:3:5`): the keys the profile viewer's issues carry as `summary_key`
type FrameContributions = BTreeMap<String, BTreeMap<u64, f64>>;

/// A frame's work (ms) on the game's side and on the harness's
type FrameSides = (f64, f64);

/// Writes `timeline.issue_stats`: for each issue key, the frames it was active in, its largest frame and that frame's
/// number, and its largest contribution to a frame over the budget. It reads every physical frame of the reported
/// phases, before the timeline drops detail, so what the viewer lists and how severe it grades never depends on which
/// frames the timeline happened to keep.
fn write_issue_summaries(vm: &vm::Results, rows: &[Row], scope: &[u32], fps: f64, json: &mut Json) {
    if !json["timeline"].is_object() {
        return;
    }
    let sort_keys = sort_keys_by_node(vm);
    let mut contributions = FrameContributions::new();
    let mut frame_work: BTreeMap<u64, FrameSides> = BTreeMap::new();
    for row in rows.iter().filter(|row| scope.contains(&row.phase)) {
        // (a row that does not split its work by side is all the game's)
        let (game, harness) = row_sides_ms(row).unwrap_or((row.work_ms * price_scale(), 0.0));
        let sides = frame_work.entry(row.frame).or_default();
        sides.0 += game;
        sides.1 += harness;
        add_row_contributions(vm, row, &sort_keys, &mut contributions);
    }
    let (game, harness) = frame_work.values().fold((0.0, 0.0), |sum, sides| (sum.0 + sides.0, sum.1 + sides.1));
    // (a frame's total is the game's work alone when the harness's is set apart, else both sides' together)
    let harness_apart = harness > 0.0 && game >= 0.01 * (game + harness);
    let budget = frame_budget_ms(fps);

    let mut summaries = Vec::new();
    for (key, frames) in &contributions {
        if let Some(summary) = summarize_frames(key, frames, &frame_work, harness_apart, budget) {
            summaries.push(summary);
        }
    }
    json["timeline"]["issue_stats"] = Json::Array(summaries);
}

/// For every call-tree node, the keys of the sorts it sits under, as `sort:<calling function>:<line>`. A node under a sort
/// is part of that sort's call site, however deep, and under every sort of a nested pair.
fn sort_keys_by_node(vm: &vm::Results) -> Vec<BTreeSet<String>> {
    let mut sort_keys: Vec<BTreeSet<String>> = Vec::new();
    // Each node's nearest script function: a C function's work belongs to the script that called it
    let mut owners: Vec<Option<i32>> = Vec::new();
    for (index, node) in vm.nodes.iter().enumerate() {
        let parent = usize::try_from(node.parent).ok().filter(|parent| *parent < index);
        let parent_owner = parent.and_then(|parent| owners[parent]);
        owners.push(if node.function >= 0 { Some(node.function) } else { parent_owner });

        let mut keys = match parent {
            Some(parent) => sort_keys[parent].clone(),
            None => BTreeSet::new(),
        };
        if node_is_table_sort(vm, node.function)
            && let Some(caller) = parent_owner
        {
            let line = call_line(vm, caller, node.callpc).unwrap_or(-1);
            keys.insert(format!("sort:{caller}:{line}"));
        }
        sort_keys.push(keys);
    }
    sort_keys
}

/// Whether a call-tree node's function is the C function `table.sort` (a negative function below -1 is a C function)
fn node_is_table_sort(vm: &vm::Results, function: i32) -> bool {
    if function >= -1 {
        return false;
    }
    let c_function = (-2 - function) as usize;
    vm.cfunctions.get(c_function).is_some_and(|name| name == "table.sort")
}

/// The source line of the instruction of `function` that made a call (None for a call from C or native code)
fn call_line(vm: &vm::Results, function: i32, callpc: i32) -> Option<i64> {
    let pc = usize::try_from(callpc).ok()?;
    let line = vm.functions.get(function as usize)?.lines.get(pc)?;
    Some(*line as i64)
}

/// Adds a row's work to the issues it counts toward: each script function, each sort its nodes sit under, and each charged
/// builtin or cache site
fn add_row_contributions(vm: &vm::Results, row: &Row, sort_keys: &[BTreeSet<String>], contributions: &mut FrameContributions) {
    let mut add = |key: String, ms: f64| {
        *contributions.entry(key).or_default().entry(row.frame).or_default() += ms;
    };
    for &(function, ms, _) in &row.functions {
        add(format!("function:{function}"), ms * price_scale());
    }
    for &(node, ms) in &row.nodes {
        for key in sort_keys.get(node).into_iter().flatten() {
            add(key.clone(), ms * price_scale());
        }
    }
    for site in &row.sites {
        if let Some(key) = site_issue_key(vm, site) {
            add(key, site.saving_ms * price_scale());
        }
    }
}

/// The key of the issue a charged site belongs to: a builtin site's is its edit's (`builtin:<name>:<function>:<line>`, which
/// all the compiled copies on that line share), a cache site's its own. None for a site the VM's results do not describe.
fn site_issue_key(vm: &vm::Results, site: &super::SiteWork) -> Option<String> {
    if site.kind != "builtin" {
        return Some(format!("cache:{}:{}", site.function, site.pc));
    }
    let prototype = vm.functions.get(site.function as usize)?;
    let code = prototype.code.get(site.pc)?;
    let builtin_id = ((code >> 8) & 255) as i32;
    let name = vm::builtin_name(builtin_id).unwrap_or("?");
    let line = prototype.lines.get(site.pc).copied().unwrap_or(-1);
    Some(format!("builtin:{name}:{}:{line}", site.function))
}

/// One issue's summary row, `[key, active frames, largest us, that frame, largest us in a frame over the budget]`. None
/// when it was never active
fn summarize_frames(key: &str, frames: &BTreeMap<u64, f64>, frame_work: &BTreeMap<u64, FrameSides>, harness_apart: bool, budget_ms: f64) -> Option<Json> {
    let mut active_frames = 0;
    let mut largest_ms = 0.0;
    let mut largest_frame = 0;
    let mut largest_over_budget_ms: f64 = 0.0;
    for (&frame, &ms) in frames {
        if ms <= 0.0 {
            continue;
        }
        active_frames += 1;
        if ms > largest_ms {
            largest_ms = ms;
            largest_frame = frame;
        }
        let (game, harness) = frame_work[&frame];
        let frame_total = if harness_apart { game } else { game + harness };
        if frame_total > budget_ms {
            largest_over_budget_ms = largest_over_budget_ms.max(ms);
        }
    }
    if active_frames == 0 {
        return None;
    }
    Some(json!([key, active_frames, largest_ms * 1000.0, largest_frame, largest_over_budget_ms * 1000.0]))
}

/// What the timeline keeps of a function over the reported phases (the summary the issue list reads)
#[derive(Default)]
struct FunctionFrames {
    /// Its work in all (ms)
    total: f64,
    /// The frames it did any work in
    active: u64,
    /// Its work in its busiest frame (ms), and that frame
    largest: f64,
    peak_frame: u64,
    /// The frame last counted, and the function's work in it so far (a frame's parts add up)
    last_frame: Option<u64>,
    work_in_last_frame: f64,
}

/// Every function keeps sufficient statistics over the reported phases. Detailed nodes are needed only on heavy
/// whole frames. Retention is by frame, never by part or by a function's mean, so a phase mark cannot hide an event.
/// Returns the function summaries, the heavy frames' node work and their sites' savings, as the profile stores them.
fn frame_evidence(rows: &[Row], scope: &[u32], fps: f64) -> (Json, Json, Json) {
    let whole = whole_frame_work(rows);
    let mut by_function: BTreeMap<i32, FunctionFrames> = BTreeMap::new();
    for row in rows.iter().filter(|row| scope.contains(&row.phase)) {
        for &(function, ms, _) in row.functions.iter().filter(|(_, ms, _)| *ms > 0.0) {
            let stat = by_function.entry(function).or_default();
            if stat.last_frame != Some(row.frame) {
                stat.active += 1;
                stat.work_in_last_frame = 0.0;
                stat.last_frame = Some(row.frame);
            }
            stat.total += ms;
            stat.work_in_last_frame += ms;
            if stat.work_in_last_frame > stat.largest {
                stat.largest = stat.work_in_last_frame;
                stat.peak_frame = row.frame;
            }
        }
    }
    let heavy_from_ms = frame_budget_ms(fps) * HEAVY_FRAME_SHARE;
    let is_heavy = |row: &Row| whole[&row.frame] >= heavy_from_ms;
    // (a heavy row: its index, then each node and its work in us)
    let mut heavy_rows = Vec::new();
    let mut site_frames = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if !is_heavy(row) {
            continue;
        }
        let mut cells = vec![json!(index)];
        for &(node, ms) in &row.nodes {
            cells.extend([json!(node), json!(microseconds(ms))]);
        }
        heavy_rows.push(Json::Array(cells));
    }
    for (index, row) in rows.iter().enumerate().filter(|(_, row)| is_heavy(row)) {
        for site in &row.sites {
            site_frames.push(json!([index, site.kind, site.function, site.pc, site.node, microseconds(site.saving_ms)]));
        }
    }
    let stats = by_function
        .into_iter()
        .map(|(id, stat)| json!([id, microseconds(stat.total), stat.active, microseconds(stat.largest), stat.peak_frame]))
        .collect();
    (Json::Array(stats), Json::Array(heavy_rows), Json::Array(site_frames))
}

// The self-contained report page: the template with the profile's JSON in it
pub fn html(profile_json: &str) -> String {
    const TEMPLATE: &str = include_str!("report.html");
    // (`</` cannot end the script element the data sits in)
    let data = profile_json.replace("</", "<\\/");
    TEMPLATE.replacen("/*PROFILE_DATA*/null", &data, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str) -> u32 {
        vm::OPCODES.iter().position(|op| *op == name).unwrap() as u32
    }

    /// A function's code hash stays as compiled whatever a run made of it: the slot a GETTABLEKS predicts (its C byte,
    /// which the VM patches) and the loader's GETUDATAKS form hash as the plain instruction; its lines, which a move
    /// changes, are not in it; a changed operand is
    #[test]
    fn a_code_hash_ignores_what_a_run_patches_and_where_the_function_lies() {
        let function = |words: Vec<u32>, line: i32| vm::Function { code: words, line, lines: vec![line; 4], ..vm::Function::default() };
        // GETTABLEKS A=1 B=0 C=slot, aux 7; then RETURN
        let get = |opcode: u32, slot: u32| (slot << 24) | (1 << 8) | opcode;
        let ret = op("RETURN") | (1 << 16);
        let loaded = code_hash(&function(vec![get(op("GETTABLEKS"), 0), 7, ret], 10)).unwrap();
        assert_eq!(code_hash(&function(vec![get(op("GETTABLEKS"), 5), 7, ret], 10)).unwrap(), loaded, "a predicted slot");
        assert_eq!(code_hash(&function(vec![get(op("GETUDATAKS"), 3), 7, ret], 10)).unwrap(), loaded, "the loader's form");
        assert_eq!(code_hash(&function(vec![get(op("GETTABLEKS"), 0), 7, ret], 13)).unwrap(), loaded, "moved three lines");
        assert_ne!(code_hash(&function(vec![get(op("GETTABLEKS"), 0), 8, ret], 10)).unwrap(), loaded, "another key");
        assert!(code_hash(&vm::Function::default()).is_none());
    }

    #[test]
    fn cold_object_total_keeps_listing_order_before_display_sort() {
        let smallest = 200e-9;
        let largest = 2500e-9;
        let middle = 1400e-9;
        let mut rows = Vec::new();
        for cost in [smallest, largest, middle] {
            rows.push(ColdRow {
                cost,
                kind: "table".into(),
                name: String::new(),
                charges: 1,
                shared: 0,
                first_payer: String::new(),
            });
        }
        let total = sum_then_sort_cold_rows(&mut rows);
        // Added in the order listed (200, 2500, 1400 ns), the three come to exactly this double
        let expected_total_ms = 0.0041f64;
        assert_eq!(ms(total).to_bits(), expected_total_ms.to_bits());
        assert_eq!(rows[0].cost, largest);
        assert_eq!(rows[1].cost, middle);
        assert_eq!(rows[2].cost, smallest);
    }

    /// A hardware-counter profile cut short by the service's slice limit says so, with the seconds captured, in its
    /// summary and in profile.json
    #[test]
    fn a_cut_short_counter_profile_warns_in_its_summary_and_json() {
        let config = Config::new(super::super::Kinds::HW, std::path::PathBuf::new());
        let run = Run::default();
        let collector = memory::Collector::default();
        let cpu = cpu::Report::default();
        let sources = FxHashMap::default();
        let hw = hw::Report { names: vec!["cycles".into()], cut_short: Some(12.345), ..Default::default() };
        let input = Input {
            config: &config,
            run: &run,
            elapsed: 0.0,
            vm: None,
            attribution: None,
            allocations: None,
            time: None,
            phases: &[],
            marks: &[],
            rows: &[],
            snapshots: &[],
            collector: &collector,
            caches: &[],
            facts: super::super::caches::Facts::default(),
            cpu: &cpu,
            cpu_frames: &[],
            hw: Some(&hw),
            sources: &sources,
            end_work: 0.0,
            end: None,
            boundary: None,
            phase_timing: None,
            phase_tree: None,
            warnings: &[],
            price_scale: 1.0,
            device: "desktop",
        };
        let names = Names { vm: None, steps: Vec::new(), outside: Vec::new(), shorts: FxHashMap::default() };
        let mut summary = String::new();
        let json = counters(&input, &hw, &names, &mut summary, &mut Vec::new());
        assert_eq!(json["cut_short"], json!(12.345));
        assert!(summary.contains("WARNING: hardware-counter profile CUT SHORT after 12.345 s"), "{summary}");
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn issue_summaries_keep_small_sort_peaks_across_unrelated_retention_changes() {
        let mut vm = vm::Results { functions: vec![vm::Function::default(); 8], cfunctions: vec!["table.sort".into()], ..Default::default() };
        vm.functions[7].lines = vec![12];
        vm.nodes = vec![
            vm::Node { parent: -1, function: -1, ..Default::default() },
            vm::Node { parent: 0, function: 7, ..Default::default() },
            vm::Node { parent: 1, function: -2, callpc: 0, ..Default::default() },
        ];
        for other in [7.063333, 7.063343, 20.0] {
            let mut event = row(30, 1, 1.27 + other);
            event.functions = vec![(7, 1.27, false), (6, other, false)];
            event.nodes = vec![(2, 1.27)];
            let mut json = json!({"timeline": {}});
            write_issue_summaries(&vm, &[event], &[1], 60.0, &mut json);
            let sort = json["timeline"]["issue_stats"].as_array().unwrap().iter().find(|stat| stat[0] == "sort:7:12").unwrap();
            assert_eq!(sort[1], 1);
            assert_eq!(sort[2], 1270.0);
            assert_eq!(sort[3], 30);
            assert_eq!(sort[4], if other == 20.0 { 1270.0 } else { 0.0 });
        }
    }

    /// A row of one frame's work in one phase, all of it in function 7 under call-tree node 3
    fn row(frame: u64, phase: u32, ms: f64) -> Row {
        Row { frame, phase, work_ms: ms, sides: Some((ms, 0.0)), functions: vec![(7, ms, false)], nodes: vec![(3, ms)], ..Row::default() }
    }

    /// The `function_stats` row of a function: [function, total us, active frames, largest frame us, that frame]
    fn stats_row(function: i32, total_ms: f64, active_frames: u64, largest_ms: f64, frame: u64) -> Json {
        json!([function, total_ms * 1000.0, active_frames, largest_ms * 1000.0, frame])
    }

    #[test]
    fn summaries_retain_the_120_fps_event_and_combine_phase_parts() {
        // an event of 0.3 ms at 120 fps (an 8.3 ms frame): too small to keep node detail, so only its summary is kept
        let event_ms = 0.30094479;
        let rows = vec![row(30, 1, event_ms)];
        let (stats, nodes, _) = frame_evidence(&rows, &[1], 120.0);
        assert_eq!(stats, json!([stats_row(7, event_ms, 1, event_ms, 30)]));
        assert_eq!(nodes, json!([]));

        // a frame a phase mark split in two: its parts are one active frame, and their work adds up
        let first_part_ms = 0.30365224;
        let second_part_ms = 0.30094479;
        let rows = vec![row(120, 1, first_part_ms), row(120, 2, second_part_ms)];
        let (stats, nodes, _) = frame_evidence(&rows, &[1, 2], 60.0);
        let whole_frame_us = (first_part_ms + second_part_ms) * 1000.0;
        assert_eq!(stats[0][2], 1);
        assert!((stats[0][1].as_f64().unwrap() - whole_frame_us).abs() < 1e-9);
        assert!((stats[0][3].as_f64().unwrap() - whole_frame_us).abs() < 1e-9);
        assert_eq!(stats[0][4], 120);
        assert_eq!(nodes, json!([]));
    }

    #[test]
    fn heavy_frames_keep_every_node_and_every_phase_part_at_the_recorded_fps() {
        // A frame is heavy from half the frame budget: 8.3 ms at 60 fps, 4.2 ms at 120 fps. Frame 2 is two parts of 4.2 ms,
        // which together are heavy at both rates. Frame 1's 5 ms is heavy only at 120 fps.
        let rows = vec![row(1, 1, 5.0), row(2, 1, 4.2), row(2, 2, 4.2)];
        // (a heavy row lists its index, then each node and its work in us)
        let (_, at_sixty, _) = frame_evidence(&rows, &[1, 2], 60.0);
        assert_eq!(at_sixty, json!([[1, 3, 4200.0], [2, 3, 4200.0]]));
        let (_, at_one_twenty, _) = frame_evidence(&rows, &[1, 2], 120.0);
        assert_eq!(at_one_twenty, json!([[0, 3, 5000.0], [1, 3, 4200.0], [2, 3, 4200.0]]));
    }

    #[test]
    fn heavy_frames_keep_the_sites_net_work_beside_their_context_nodes() {
        let mut heavy = row(60, 1, 30.0);
        let saving_ms = 20.0;
        heavy.sites.push(super::super::SiteWork { kind: "builtin", function: 7, pc: 12, node: 3, saving_ms });
        let (_, _, sites) = frame_evidence(&[heavy], &[1], 60.0);
        assert_eq!(sites, json!([[0, "builtin", 7, 12, 3, saving_ms * 1000.0]]));
    }

    #[test]
    fn frame_evidence_and_issue_peaks_use_the_devices_units() {
        let scale = crate::cost::PHONE_SCRIPTS;
        let desktop_scale = SCALE.with(|value| value.replace(scale));
        // Frame 1 is two parts of 2 ms, frame 2 one part of 2 ms. A cache site saved 0.8 ms in frame 1's first part.
        let mut rows = vec![row(1, 1, 2.0), row(1, 2, 2.0), row(2, 1, 2.0)];
        rows[0].sites.push(super::super::SiteWork { kind: "cache", function: 7, pc: 12, node: 3, saving_ms: 0.8 });
        let scope = [1, 2];
        let fps = 60.0;

        // Frame 1 is heavy at phone prices (4 ms desktop is over half of 16.7 ms once scaled); frame 2 is below half its budget.
        let (stats, nodes, sites) = frame_evidence(&rows, &scope, fps);
        let function_us = 6_000.0 * scale;
        let largest_frame_us = 4_000.0 * scale;
        let part_us = 2_000.0 * scale;
        let site_saving_us = 800.0 * scale;
        assert_eq!(stats, json!([[7, function_us, 2, largest_frame_us, 1]]));
        assert_eq!(nodes, json!([[0, 3, part_us], [1, 3, part_us]]));
        assert_eq!(sites, json!([[0, "cache", 7, 12, 3, site_saving_us]]));

        let mut json = json!({"timeline": {}});
        write_issue_summaries(&vm::Results::default(), &rows, &scope, fps, &mut json);
        let issue_stats = json["timeline"]["issue_stats"].as_array().unwrap();
        let function_issue = issue_stats.iter().find(|stat| stat[0] == "function:7").unwrap();
        assert_eq!(function_issue, &json!(["function:7", 2, largest_frame_us, 1, 0.0]));
        assert_eq!(row_sides_ms(&rows[0]), Some((2.0 * scale, 0.0)));
        SCALE.with(|value| value.set(desktop_scale));
    }

    #[test]
    fn substring_replacement_prices_the_added_argument_instructions() {
        let op = |name: &str| vm::OPCODES.iter().position(|candidate| *candidate == name).unwrap() as u8;
        let declined_calls = 600_000;
        let extra = |builtin: &str, opcode: &str| builtin_extra_work(builtin, op(opcode), declined_calls).unwrap();
        // `string.sub(s, 2)` gains an explicit end, one LENGTH instruction in each call, and a LOADN more where the start is
        // a constant
        assert!((extra("string.sub", "FASTCALL2K").instructions - 0.001758).abs() < 1e-12);
        assert!((extra("string.sub", "FASTCALL2").instructions - 0.001470).abs() < 1e-12);
        // `string.byte(s)` gains an explicit index: no nominal added warm instruction in a call
        assert_eq!(extra("string.byte", "FASTCALL1").instructions, 0.0);
        // Added words do not establish the replacement's executed instructions, even with an active-frame count.
        assert_eq!(extra("string.sub", "FASTCALL2K").cold, None);
        assert_eq!(extra("string.byte", "FASTCALL1").cold, None);
        // a call shape the rewrite does not apply to has no measured cost
        assert!(builtin_extra_work("string.sub", op("FASTCALL"), 1).is_none());
    }

    #[test]
    fn builtin_setup_needs_the_replacements_executed_instructions_for_cold_work() {
        let op = vm::OPCODES.iter().position(|name| *name == "FASTCALL2K").unwrap() as u32;
        let function = 7;
        let line = 12;
        let first_context_node = 3;
        let second_context_node = 5;
        let mut vm_results = vm::Results { functions: vec![vm::Function::default(); 8], ..vm::Results::default() };
        let builtin = (0..256).find(|id| vm::builtin_name(*id) == Some("string.sub")).unwrap() as u32;
        // (two string.sub calls in the function, each a FASTCALL2K and its argument word, on the same source line)
        let code = op | (builtin << 8);
        vm_results.functions[function].code = vec![code, 0, code, 0];
        vm_results.functions[function].lines = vec![line; 4];
        vm_results.nodes = vec![vm::Node::default(); 6];
        vm_results.nodes[first_context_node].function = function as i32;
        vm_results.nodes[second_context_node].function = function as i32;
        // Frame 1 is heavy and has two parts, one in each phase; the function runs in both, under the first node and under
        // the second. It runs in frame 2 as well (under the first node), which is light. Frame 3 is a phase outside the
        // report.
        let mut parts = vec![row(1, 1, 10.0), row(1, 2, 10.0), row(2, 1, 1.0), row(3, 9, 12.0)];
        parts[1].nodes = vec![(second_context_node, 10.0)];
        let (stats, _, _) = frame_evidence(&parts, &[1, 2], 60.0);
        let mut json = json!({"timeline": {"function_stats": stats}, "builtins": [{
            "name": "string.sub", "sites": [{"function": function, "pc": 0, "line": line,
                "declines": {"args": 1000}, "decline_charged_us": 30, "fast_equivalent_us": 5,
                "nodes": [
                    {"node": first_context_node, "declines": {"args": 600}, "decline_charged_us": 10, "fast_equivalent_us": 2},
                    {"node": second_context_node, "declines": {"args": 400}, "decline_charged_us": 20, "fast_equivalent_us": 3}
                ]}]
        }]});
        price_builtin_fixes(&vm_results, &parts, &[1, 2], 60.0, &mut json);
        let site = &json["builtins"][0]["sites"][0];
        // 1000 declined calls at the warm cost of a LENGTH and a LOADN each
        assert!((site["fix_instructions_us"].as_f64().unwrap() - 2.93).abs() < 1e-12);
        // Neither dead compiled copies nor the prototype's active frames establish which replacement PCs would run.
        assert_eq!(site["fix_cold_us"], Json::Null);
        assert!(site["fix_unpriced"].as_str().unwrap().contains("executed instructions"));
        assert_eq!(site["fast_equivalent_us"], 5);
        assert_eq!(site["nodes"][0]["fast_equivalent_us"], 2);
        // (a context is [function, node, first frame, last frame]: the frames the node ran in, in the reported phases)
        assert_eq!(json["timeline"]["function_contexts"], json!([]));
        assert_eq!(json["timeline"]["replacement_frames"], json!([]));
    }
}

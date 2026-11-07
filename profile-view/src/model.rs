//! A profile directory as the viewer reads it: `profile.json` (typed: every field the views use, each optional so a
//! profile from an older ree, or of fewer kinds, loads), the folded stacks the JSON leaves out (`time.folded`,
//! `alloc.folded`, `alloc.counts.folded`, `heap.folded`, `counters.<name>.folded`) and `boundary.json`.
//!
//! Nothing here is computed beyond indexing: the figures are the profile's own.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use rustc_hash::FxHashMap;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Run {
    pub command: String,
    pub status: String,
    pub harness: Option<String>,
    pub frames: u64,
    pub seconds: f64,
    pub fps: f64,
    pub elapsed: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Mark {
    pub phase: String,
    pub frame: u64,
    pub seconds: f64,
    pub work_ms: f64,
}

/// A function's figures (self or total)
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Figures {
    pub insns: f64,
    pub work_ms: f64,
    pub time_ms: f64,
    pub alloc_bytes: f64,
    pub allocs: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawFunction {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub module: Option<String>,
    pub line: Option<i64>,
    pub last_line: Option<i64>,
    pub kind: String,
    pub native: bool,
    pub native_entries: u64,
    pub native_work_ms: f64,
    pub calls: u64,
    #[serde(rename = "self")]
    pub own: Figures,
    pub total: Figures,
    /// The harness's own code (its module under ReplicatedStorage.RURFixtures); absent before ree c692e40
    pub harness: Option<bool>,
    /// Per phase, by name: self and total work, calls, self instructions, and the self work done for the harness
    pub phases: Option<BTreeMap<String, PhaseFigures>>,
}

/// A function's figures in one phase (profile.json `functions[].phases`)
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PhaseFigures {
    pub self_work_ms: f64,
    pub total_work_ms: f64,
    pub calls: f64,
    pub insns: f64,
    /// The part of the self work done for the harness's code: all of a harness function's, and a C function's, the
    /// engine's Luau's or mlua's helpers' when the harness called them
    pub harness_self_work_ms: f64,
}

/// The phases the profile's summary reports (`--profile-phase`; by default steady when the run marked one, else every
/// phase), and each side's self work in them (profile.json `scope`)
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawScope {
    pub phases: Vec<String>,
    pub game_self_ms: f64,
    pub harness_self_ms: f64,
    pub harness_folder: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawLine {
    pub function: i64,
    pub line: i64,
    pub insns: f64,
    pub work_ms: f64,
    pub total_work_ms: f64,
    pub total_insns: f64,
    pub time_ms: f64,
    pub total_time_ms: f64,
    /// (not read: a line's allocations here add its own to what its calls allocated, and count the allocating line
    /// twice; the views take a line's own allocations from the allocation stacks, sites.rs)
    pub alloc_bytes: f64,
    pub allocs: f64,
    pub ops: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Edge {
    pub caller: i64,
    pub callee: i64,
    pub calls: u64,
    pub work_ms: f64,
    pub insns: f64,
}

/// A node of the calling-context tree: `[parent, function, calls, self work in us, self instructions, label]`
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TreeNode(pub i64, pub i64, pub u64, pub f64, pub f64, #[serde(default)] pub Option<String>);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Opcode {
    pub op: String,
    pub count: u64,
    pub work_ms: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NativeFunction {
    pub name: String,
    pub module: String,
    pub line: i64,
    pub entries: u64,
    pub work_ms: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Work {
    pub total_ms: f64,
    pub outside_instructions_ms: f64,
    pub per_second_ms: f64,
    pub per_frame_ms: f64,
    pub fidelity: String,
    pub script_insns: f64,
    pub engine_insns: f64,
    pub native_functions: Vec<NativeFunction>,
    pub opcodes: Vec<Opcode>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CacheKind {
    pub kind: String,
    pub hits: f64,
    pub misses: f64,
    pub slow: f64,
    pub other: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CacheSite {
    pub function: i64,
    pub line: i64,
    pub pc: i64,
    pub op: String,
    pub hits: f64,
    pub misses: f64,
    pub slow: f64,
    pub other: f64,
}

impl CacheSite {
    /// What missed: repredicted slots and slow paths (a value that is not a table is neither)
    pub fn missed(&self) -> f64 {
        self.misses + self.slow
    }

    pub fn runs(&self) -> f64 {
        self.hits + self.misses + self.slow + self.other
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LuauCaches {
    pub by_kind: Vec<CacheKind>,
    pub sites: Vec<CacheSite>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Time {
    pub sampled_ms: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TypeAmount {
    #[serde(rename = "type")]
    pub ty: String,
    pub group: String,
    pub bytes: f64,
    pub count: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StackAmount {
    /// (function id, line), innermost first
    pub frames: Option<Vec<(i64, i64)>>,
    #[serde(rename = "type")]
    pub ty: String,
    pub bytes: f64,
    pub count: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Alloc {
    pub bytes: f64,
    pub objects: f64,
    pub step: f64,
    pub by_type: Vec<TypeAmount>,
    pub by_stack: Vec<StackAmount>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CountBytes {
    pub count: f64,
    pub bytes: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Unswept {
    pub count: f64,
    pub bytes: f64,
    pub by_type: Vec<TypeAmount>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Held {
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    pub size: f64,
    pub retained: f64,
    pub path: String,
    pub stack: Option<Vec<(i64, i64)>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub label: String,
    pub frame: u64,
    pub seconds: f64,
    pub gcinfo_kb: f64,
    pub total_bytes: f64,
    pub objects: f64,
    pub object_bytes: f64,
    pub reachable: Option<CountBytes>,
    pub unswept: Option<Unswept>,
    /// Profiles from before reachable memory was counted apart
    pub unreachable: Option<CountBytes>,
    pub by_type: Vec<TypeAmount>,
    pub allocated_since: CountBytes,
    pub biggest: Vec<Held>,
    pub by_stack: Vec<StackAmount>,
    /// Fields a newer ree adds (table shapes and the like), read by name where the views know them
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Snapshot {
    /// (reachable bytes, reachable objects, garbage not swept yet in bytes): an older profile counted its walk's
    /// unreachable objects apart, and its reachable heap is the rest
    pub fn split(&self) -> (f64, f64, f64) {
        if let Some(reachable) = self.reachable {
            return (reachable.bytes, reachable.count, self.unswept.as_ref().map_or(0.0, |u| u.bytes));
        }
        let unreachable = self.unreachable.unwrap_or_default();
        (self.object_bytes - unreachable.bytes, self.objects - unreachable.count, unreachable.bytes)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GcState {
    pub state: String,
    pub steps: f64,
    pub ms: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Gc {
    pub cycles: f64,
    pub steps: f64,
    pub freed: f64,
    pub ms: f64,
    pub by_state: Vec<GcState>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Heap {
    pub snapshots: Vec<Snapshot>,
    pub gc: Gc,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ReeCache {
    pub name: String,
    pub about: String,
    pub hits: f64,
    pub misses: f64,
    pub evictions: f64,
}

/// A table of rows by named columns (the timeline, the counters' frames)
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Columns {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

impl Columns {
    pub fn index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column == name)
    }

    /// A column's values, row by row (0 where a row lacks it)
    pub fn values(&self, name: &str) -> Option<Vec<f64>> {
        let index = self.index(name)?;
        Some(self.rows.iter().map(|row| row.get(index).and_then(serde_json::Value::as_f64).unwrap_or(0.0)).collect())
    }

    pub fn value(&self, row: usize, name: &str) -> Option<f64> {
        let index = self.index(name)?;
        self.rows.get(row)?.get(index)?.as_f64()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CounterRun {
    pub counts: BTreeMap<String, f64>,
    pub rates: BTreeMap<String, f64>,
    pub seconds: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CounterFunction {
    pub id: i64,
    pub key: String,
    pub name: String,
    #[serde(rename = "self")]
    pub own: BTreeMap<String, f64>,
    pub total: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CounterLine {
    pub function: i64,
    pub line: i64,
    #[serde(rename = "self")]
    pub own: BTreeMap<String, f64>,
    pub total: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Counters {
    pub source: String,
    pub names: Vec<String>,
    pub error: Option<String>,
    pub notes: Vec<String>,
    pub run: CounterRun,
    pub frames: Columns,
    pub functions: Vec<CounterFunction>,
    pub lines: Vec<CounterLine>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Listing {
    pub path: String,
    pub lines: Vec<(i64, String)>,
}

/// profile.json
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Raw {
    pub version: u32,
    pub tool: String,
    pub kinds: Vec<String>,
    pub run: Run,
    pub phases: Vec<String>,
    pub marks: Vec<Mark>,
    pub warnings: Vec<String>,
    pub functions: Vec<RawFunction>,
    pub lines: Vec<RawLine>,
    pub edges: Vec<Edge>,
    pub tree: Vec<TreeNode>,
    pub work: Option<Work>,
    pub luau_caches: Option<LuauCaches>,
    pub time: Option<Time>,
    pub alloc: Option<Alloc>,
    pub heap: Option<Heap>,
    pub ree_caches: Option<Vec<ReeCache>>,
    pub timeline: Option<Columns>,
    pub cpu: Option<serde_json::Value>,
    pub counters: Option<Counters>,
    pub phase_timing: Option<String>,
    pub boundary: Option<String>,
    pub listings: BTreeMap<String, Listing>,
    pub source_hashes: BTreeMap<String, String>,
    pub summary: String,
    pub scope: Option<RawScope>,
}

/// boundary.json: a crossing site (a Luau line reading, writing or calling an engine member)
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct BoundarySite {
    pub script: String,
    pub function: String,
    pub line: i64,
    pub kind: String,
    pub member: String,
    pub count: f64,
    pub per_s: f64,
    pub cost_ms_per_s: f64,
    pub unchanged_writes: f64,
    pub repeat_reads: f64,
    pub repeat_reads_per_s: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct BoundaryMember {
    pub member: String,
    pub kind: String,
    pub count: f64,
    pub per_s: f64,
    pub cost_ms_per_s: f64,
    pub call_sites: f64,
    pub reads_per_frame: f64,
    pub distinct_per_frame: f64,
    pub same_function_repeats_per_frame: f64,
    pub cross_function_repeats_per_frame: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Boundary {
    pub phase: String,
    pub emulated_seconds: f64,
    pub frames: f64,
    pub crossings: f64,
    pub crossings_per_s: f64,
    pub cost_ms_per_s: f64,
    pub by_site_count: Vec<BoundarySite>,
    pub by_site_cost: Vec<BoundarySite>,
    pub by_member: Vec<BoundaryMember>,
    pub repeat_reads: Vec<BoundarySite>,
}

/// What a function is
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A script's Luau: what the game or the harness runs
    Script,
    /// The engine's own Luau (prelude, services): not charged as a script's work
    Engine,
    /// A C function (the VM's libraries, the engine's API)
    C,
    /// An engine step outside Luau (Animator, joints, physics) or counts no Luau stack took
    Step,
}

/// A function as the views name it
#[derive(Clone, Debug)]
pub struct Func {
    pub id: i64,
    pub key: String,
    /// `layout`, `anonymous`, `insert [C]`, `(ree engine step: physics)`
    pub name: String,
    /// The chunk's module name (`ReplicatedStorage.Interface.Grid`), empty for C functions
    pub module: String,
    /// The module's last segment (`Grid`)
    pub short: String,
    pub line: i64,
    pub last_line: i64,
    pub kind: Kind,
    pub native: bool,
    pub calls: u64,
    pub own: Figures,
    pub total: Figures,
    /// The harness's own code: a Luau function whose module is under [`HARNESS_FOLDER`] (a C function, the engine's
    /// Luau and mlua's helpers are never the harness's themselves: their work goes with their caller's side)
    pub harness: bool,
    /// Its figures per phase, by phase index (profile.json `phases`, from ree c692e40; empty before)
    pub phases: BTreeMap<u32, PhaseFigures>,
}

/// Where a run's `--harness` module and what it brings along are mounted: code there is the harness's, not the code it
/// measures (ree's report.rs `HARNESS_FOLDER`)
pub const HARNESS_FOLDER: &str = "ReplicatedStorage.RURFixtures";

/// Whether a module is the harness's own code: under [`HARNESS_FOLDER`], or the engine's chunks that drive it
/// (`rur.record`, which calls the harness, is compiled as script code), as ree's report.rs `harness_module` (f8050ce)
pub fn harness_module(module: &str) -> bool {
    module.strip_prefix(HARNESS_FOLDER).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')) || module.starts_with("rur.") || module.starts_with("ree.")
}

/// Whether the views set the harness's code apart, given the self work of the game's code and the harness's in the
/// reported phases: when the harness did some and the game's code at least 1% of the two. A run whose own code is all
/// the harness's (code measured by `--harness` alone) is reported whole
pub fn harness_apart(game: f64, harness: f64) -> bool {
    harness > 0.0 && game >= 0.01 * (game + harness)
}

/// The phases a view reports: some of the run's, or every phase
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    /// Phase indexes, None for every phase
    pub phases: Option<Vec<u32>>,
    /// `phase steady`, `phases setup, run`, `every phase` (the summary's words)
    pub label: String,
}

impl Scope {
    pub fn covers(&self, phase: u32) -> bool {
        self.phases.as_ref().is_none_or(|phases| phases.contains(&phase))
    }
}

impl Func {
    /// `layout  Grid:32`
    pub fn label(&self) -> String {
        if self.module.is_empty() { self.name.clone() } else { format!("{}  {}:{}", self.name, self.short, self.line) }
    }

    /// `Grid:32`, or the name for a function without a module
    pub fn place(&self) -> String {
        if self.module.is_empty() { self.name.clone() } else { format!("{}:{}", self.short, self.line) }
    }

    pub fn is_script(&self) -> bool {
        self.kind == Kind::Script
    }
}

/// A chunk's name as a module: `[string "ReplicatedStorage.X"]` is `ReplicatedStorage.X` (as ree names them)
pub fn module_name(source: &str) -> &str {
    source.strip_prefix("[string \"").and_then(|inner| inner.strip_suffix("\"]")).or_else(|| source.strip_prefix('=')).or_else(|| source.strip_prefix('@')).unwrap_or(source)
}

/// A module's last segment (`Grid` of `ReplicatedStorage.Interface.Grid`)
pub fn short_module(module: &str) -> &str {
    let module = module.trim_end_matches(".luau").trim_end_matches(".lua");
    module.rsplit(['.', '/', '\\']).next().unwrap_or(module)
}

/// A folded stack file: (phase index or None, frames outermost first as function indexes, the innermost frame's line
/// when it carries one, weight)
#[derive(Clone, Debug, Default)]
pub struct Folded {
    pub rows: Vec<FoldedRow>,
    pub total: f64,
}

#[derive(Clone, Debug)]
pub struct FoldedRow {
    pub phase: Option<u32>,
    pub frames: Vec<u32>,
    pub line: Option<i64>,
    pub weight: f64,
}

/// A loaded profile
pub struct Profile {
    pub dir: PathBuf,
    /// A name given by whoever opened it (a comparison's run: its variant and size), else the folder's
    pub label: Option<String>,
    pub raw: Raw,
    pub funcs: Vec<Func>,
    by_id: FxHashMap<i64, usize>,
    by_frame: FxHashMap<String, usize>,
    /// Folded stacks by file stem (`time`, `alloc`, `alloc.counts`, `heap`, `counters.cycles`)
    pub folded: BTreeMap<String, Folded>,
    pub boundary: Option<Boundary>,
    /// Lines' own allocations, (function index, line) -> (bytes, count), and whether they cover every stack
    pub line_alloc: FxHashMap<(usize, i64), (f64, f64)>,
    pub line_alloc_complete: bool,
    /// Every line with figures, by (function index, line)
    pub lines: BTreeMap<(usize, i64), LineFig>,
    /// Hardware counters by function index: (self, total), in `counters.names` order
    pub counter_funcs: FxHashMap<usize, (Vec<f64>, Vec<f64>)>,
    /// Luau inline cache misses (repredicted and slow) and runs by function index
    pub miss_funcs: FxHashMap<usize, (f64, f64)>,
}

/// A line's figures, every kind's
#[derive(Clone, Debug, Default)]
pub struct LineFig {
    pub work: f64,
    pub total_work: f64,
    pub insns: f64,
    pub total_insns: f64,
    pub time: f64,
    pub total_time: f64,
    pub alloc: f64,
    pub allocs: f64,
    pub misses: f64,
    pub cache_runs: f64,
    pub counters: Vec<f64>,
    pub ops: BTreeMap<String, u64>,
}

impl Profile {
    /// Reads a profile directory (or its profile.json)
    pub fn load(path: &Path) -> Result<Profile, String> {
        let (dir, file) = if path.is_file() { (path.parent().unwrap_or(Path::new(".")).to_path_buf(), path.to_path_buf()) } else { (path.to_path_buf(), path.join("profile.json")) };
        let text = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        let raw: Raw = serde_json::from_slice(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        let mut profile = Profile::from_raw(dir.clone(), raw);
        // folded stacks the JSON does not carry
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut names: Vec<String> = entries.filter_map(|entry| entry.ok().map(|entry| entry.file_name().to_string_lossy().into_owned())).collect();
            names.sort();
            for name in names {
                let Some(stem) = name.strip_suffix(".folded") else { continue };
                if stem == "work" || stem == "instructions" {
                    continue; // the call tree in profile.json carries these, with call counts
                }
                if let Ok(text) = std::fs::read_to_string(dir.join(&name)) {
                    let folded = profile.parse_folded(&text);
                    profile.folded.insert(stem.to_string(), folded);
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("boundary.json")) {
            profile.boundary = serde_json::from_str(&text).ok();
        }
        // (again, with the folded stacks' allocations)
        profile.index();
        Ok(profile)
    }

    /// A profile from its parsed JSON (tests build them this way)
    pub fn from_raw(dir: PathBuf, raw: Raw) -> Profile {
        let mut profile = Profile {
            dir,
            label: None,
            raw,
            funcs: Vec::new(),
            by_id: FxHashMap::default(),
            by_frame: FxHashMap::default(),
            folded: BTreeMap::new(),
            boundary: None,
            line_alloc: FxHashMap::default(),
            line_alloc_complete: true,
            lines: BTreeMap::new(),
            counter_funcs: FxHashMap::default(),
            miss_funcs: FxHashMap::default(),
        };
        let functions = std::mem::take(&mut profile.raw.functions);
        for function in &functions {
            // (the JSON's module is already the chunk's module name)
            let module = function.module.clone().unwrap_or_default();
            let kind = match function.kind.as_str() {
                // (mlua's own chunks, a userdata's __index and __newindex in Luau: the engine's, though the profile
                // names them Luau)
                "Luau" if module.starts_with("__mlua") => Kind::Engine,
                "Luau" => Kind::Script,
                "engine Luau" => Kind::Engine,
                "engine step" | "outside Luau" => Kind::Step,
                _ if function.name.starts_with('(') => Kind::Step,
                _ => Kind::C,
            };
            let name = if function.name.is_empty() && !module.is_empty() { "anonymous".to_string() } else { function.name.clone() };
            // (the flag profile.json gives, else the rule ree applies: a script function under the harness's folder)
            let harness = function.harness.unwrap_or(kind == Kind::Script && harness_module(&module));
            let phases = function
                .phases
                .iter()
                .flatten()
                .map(|(name, figures)| (profile.raw.phases.iter().position(|phase| phase == name).unwrap_or(0) as u32, *figures))
                .collect();
            profile.push_func(Func {
                id: function.id,
                key: function.key.clone(),
                short: short_module(&module).to_string(),
                module,
                name,
                line: function.line.unwrap_or(0),
                last_line: function.last_line.or(function.line).unwrap_or(0),
                kind,
                native: function.native,
                calls: function.calls,
                own: function.own,
                total: function.total,
                harness,
                phases,
            });
        }
        profile.raw.functions = functions;
        profile.index();
        profile
    }

    /// The per-line figures: each line's own allocations from the allocation sites, then every kind's figures by line
    pub fn index(&mut self) {
        self.index_line_alloc();
        self.index_lines();
    }

    /// Every line's figures in one place, and the counters' and caches' per-function sums
    fn index_lines(&mut self) {
        let counters = self.raw.counters.as_ref().map_or(0, |c| c.names.len());
        let mut lines: BTreeMap<(usize, i64), LineFig> = BTreeMap::new();
        let blank = || LineFig { counters: vec![0.0; counters], ..LineFig::default() };
        for line in &self.raw.lines {
            let Some(func) = self.func_of_id(line.function) else { continue };
            let fig = lines.entry((func, line.line)).or_insert_with(blank);
            fig.work += line.work_ms;
            fig.total_work += line.total_work_ms;
            fig.insns += line.insns;
            fig.total_insns += line.total_insns;
            fig.time += line.time_ms;
            fig.total_time += line.total_time_ms;
            for (op, count) in &line.ops {
                *fig.ops.entry(op.clone()).or_default() += count;
            }
        }
        for (&key, &(bytes, count)) in &self.line_alloc {
            let fig = lines.entry(key).or_insert_with(blank);
            fig.alloc += bytes;
            fig.allocs += count;
        }
        let mut miss_funcs: FxHashMap<usize, (f64, f64)> = FxHashMap::default();
        if let Some(caches) = &self.raw.luau_caches {
            for site in &caches.sites {
                let Some(func) = self.func_of_id(site.function) else { continue };
                let fig = lines.entry((func, site.line)).or_insert_with(blank);
                fig.misses += site.missed();
                fig.cache_runs += site.runs();
                let entry = miss_funcs.entry(func).or_default();
                entry.0 += site.missed();
                entry.1 += site.runs();
            }
        }
        let mut counter_funcs: FxHashMap<usize, (Vec<f64>, Vec<f64>)> = FxHashMap::default();
        if let Some(c) = &self.raw.counters {
            let read = |map: &BTreeMap<String, f64>| -> Vec<f64> { c.names.iter().map(|name| map.get(name).copied().unwrap_or(0.0)).collect() };
            for row in &c.functions {
                if let Some(func) = self.func_of_id(row.id) {
                    counter_funcs.insert(func, (read(&row.own), read(&row.total)));
                }
            }
            for row in &c.lines {
                let Some(func) = self.func_of_id(row.function) else { continue };
                let fig = lines.entry((func, row.line)).or_insert_with(blank);
                for (slot, value) in fig.counters.iter_mut().zip(read(&row.own)) {
                    *slot += value;
                }
            }
        }
        self.lines = lines;
        self.miss_funcs = miss_funcs;
        self.counter_funcs = counter_funcs;
    }

    /// A function's lines with figures, in line order
    pub fn lines_of(&self, func: usize) -> impl Iterator<Item = (i64, &LineFig)> {
        self.lines.range((func, i64::MIN)..=(func, i64::MAX)).map(|(&(_, line), fig)| (line, fig))
    }

    /// The lines of a module with figures, (line, function index, figures), in line order
    pub fn module_lines(&self, module: &str) -> Vec<(i64, usize, &LineFig)> {
        let mut rows: Vec<(i64, usize, &LineFig)> = self.lines.iter().filter(|((func, _), _)| self.funcs[*func].module == module).map(|(&(func, line), fig)| (line, func, fig)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        rows
    }

    fn push_func(&mut self, func: Func) -> usize {
        let index = self.funcs.len();
        self.by_id.insert(func.id, index);
        self.by_frame.entry(frame_name(&func)).or_insert(index);
        self.funcs.push(func);
        index
    }

    /// The function with this id (functions with no figures are not in the profile)
    pub fn func_of_id(&self, id: i64) -> Option<usize> {
        self.by_id.get(&id).copied()
    }

    /// The function a folded frame names, made up from the name when the profile lists none
    fn func_of_frame(&mut self, frame: &str) -> usize {
        if let Some(&index) = self.by_frame.get(frame) {
            return index;
        }
        // `name Module:line` of a Luau function the JSON did not list, `name [C]`, `(label)`
        let (name, module, line) = match frame.rsplit_once(' ') {
            Some((name, place)) if !frame.ends_with("[C]") && !frame.starts_with('(') => match place.rsplit_once(':') {
                Some((module, line)) if line.parse::<i64>().is_ok() => (name.to_string(), module.to_string(), line.parse().unwrap_or(0)),
                _ => (frame.to_string(), String::new(), 0),
            },
            _ => (frame.to_string(), String::new(), 0),
        };
        let kind = if frame.starts_with('(') {
            Kind::Step
        } else if module.is_empty() {
            Kind::C
        } else {
            Kind::Script
        };
        let id = -3_000_000_000 - self.funcs.len() as i64;
        let harness = kind == Kind::Script && harness_module(&module);
        self.push_func(Func { id, key: frame.to_string(), short: short_module(&module).to_string(), module, name, line, last_line: line, kind, native: false, calls: 0, own: Figures::default(), total: Figures::default(), harness, phases: BTreeMap::new() })
    }

    /// Parses a folded file: `phase;outermost;...;innermost @line weight`
    pub fn parse_folded(&mut self, text: &str) -> Folded {
        let mut folded = Folded::default();
        for line in text.lines() {
            let Some((stack, weight)) = line.rsplit_once(' ') else { continue };
            let Ok(weight) = weight.trim().parse::<f64>() else { continue };
            let mut parts: Vec<&str> = stack.split(';').collect();
            let phase = parts.first().and_then(|first| self.raw.phases.iter().position(|phase| phase == first)).map(|index| index as u32);
            if phase.is_some() {
                parts.remove(0);
            }
            let mut innermost_line = None;
            if let Some(last) = parts.last_mut() {
                if let Some((frame, line)) = last.rsplit_once(" @") {
                    if let Ok(line) = line.parse::<i64>() {
                        innermost_line = Some(line);
                        *last = frame;
                    }
                }
            }
            let frames: Vec<u32> = parts.iter().filter(|part| !part.is_empty()).map(|part| self.func_of_frame(part) as u32).collect();
            folded.total += weight;
            folded.rows.push(FoldedRow { phase, frames, line: innermost_line, weight });
        }
        folded
    }

    /// Each line's own allocations: what the allocation sites (sites.rs) book to it. Complete when the folded stacks
    /// are there (every stack, by phase) or the JSON's stacks cover the run's bytes
    fn index_line_alloc(&mut self) {
        self.line_alloc.clear();
        let Some(alloc) = &self.raw.alloc else { return };
        let folded = self.folded.contains_key("alloc");
        let covered: f64 = alloc.by_stack.iter().map(|stack| stack.bytes).sum();
        let sites = crate::sites::alloc_sites(self);
        let mut unplaced = 0.0;
        for site in &sites {
            if site.func == crate::sites::ENGINE || site.line < 0 {
                unplaced += site.bytes;
                continue;
            }
            let entry = self.line_alloc.entry((site.func as usize, site.line)).or_default();
            entry.0 += site.bytes;
            entry.1 += site.count;
        }
        let _ = unplaced;
        self.line_alloc_complete = folded || alloc.bytes <= 0.0 || covered >= alloc.bytes * 0.999;
    }

    pub fn has_kind(&self, kind: &str) -> bool {
        self.raw.kinds.iter().any(|k| k == kind)
    }

    /// The profile's directory name, for headers (`profile`'s parent when the directory is called profile)
    pub fn name(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        let leaf = self.dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if leaf == "profile" {
            if let Some(parent) = self.dir.parent().and_then(Path::file_name) {
                return format!("{}/profile", parent.to_string_lossy());
            }
        }
        if leaf.is_empty() { self.dir.display().to_string() } else { leaf }
    }

    /// The harness's file name, when the run had one
    pub fn harness(&self) -> Option<String> {
        self.raw.run.harness.as_deref().map(|path| path.rsplit(['/', '\\']).next().unwrap_or(path).to_string())
    }

    pub fn phase_name(&self, phase: u32) -> &str {
        self.raw.phases.get(phase as usize).map_or("?", String::as_str)
    }

    /// A phase as prose names it: `the steady phase`, or nothing for `frames` (ree run's and rur's name for the run
    /// after the mount, which a frame's figures need no word for)
    pub fn phase_words(&self, phase: u32) -> Option<String> {
        let name = self.phase_name(phase);
        (name != "frames").then(|| format!("the {name} phase"))
    }

    /// A timeline column's values past startup (all of them when the run marks no phase after it)
    pub fn frames_past_startup(&self, values: &[f64]) -> Vec<f64> {
        let phases = self.raw.timeline.as_ref().and_then(|t| t.values("phase")).unwrap_or_default();
        let later = phases.iter().any(|phase| *phase > 0.0);
        values.iter().enumerate().filter(|(row, _)| !later || phases.get(*row).is_some_and(|phase| *phase > 0.0)).map(|(_, v)| *v).collect()
    }

    /// A timeline column's mean a frame past startup
    pub fn frames_mean(&self, values: &[f64]) -> f64 {
        let kept = self.frames_past_startup(values);
        if kept.is_empty() { 0.0 } else { kept.iter().sum::<f64>() / kept.len() as f64 }
    }

    /// The phases the views report: the profile's scope (what its summary reports: `--profile-phase`, by default steady
    /// when the run marked one, else every phase), else, for a profile from before ree c692e40, the same default
    pub fn scope(&self) -> Scope {
        let names: Vec<String> = match &self.raw.scope {
            Some(scope) => scope.phases.clone(),
            None => self.raw.phases.iter().filter(|phase| *phase == "steady").cloned().collect(),
        };
        self.scope_of(&names)
    }

    /// A scope of named phases (the ones this run marked; every phase when none is, or when they all are)
    pub fn scope_of(&self, names: &[String]) -> Scope {
        let mut phases: Vec<u32> = names.iter().filter_map(|name| self.raw.phases.iter().position(|phase| phase == name)).map(|index| index as u32).collect();
        phases.sort_unstable();
        phases.dedup();
        if phases.is_empty() || phases.len() >= self.raw.phases.len().max(1) {
            return Scope { phases: None, label: "every phase".into() };
        }
        let label = format!("{} {}", if phases.len() == 1 { "phase" } else { "phases" }, phases.iter().map(|&phase| self.phase_name(phase).to_string()).collect::<Vec<_>>().join(", "));
        Scope { phases: Some(phases), label }
    }

    /// Whether any of the profile's code is the harness's
    pub fn has_harness(&self) -> bool {
        self.funcs.iter().any(|f| f.harness) || self.raw.scope.as_ref().is_some_and(|scope| scope.harness_self_ms > 0.0)
    }

    /// The work clock's self work of the game's code and of the harness's in the scope: the profile's own figures (its
    /// summary's), else the functions' per phase; None for a profile from before ree c692e40 (the call tree has them)
    pub fn scope_work(&self) -> Option<(f64, f64)> {
        match &self.raw.scope {
            Some(scope) => Some((scope.game_self_ms, scope.harness_self_ms)),
            None => self.side_work(&self.scope()),
        }
    }

    /// Whether the functions carry their figures per phase (ree c692e40 and later)
    pub fn has_phase_figures(&self) -> bool {
        self.funcs.iter().any(|f| !f.phases.is_empty())
    }

    /// The self work of the game's code and of the harness's in a scope's phases, from the functions' figures per phase:
    /// None for a profile from before ree c692e40
    pub fn side_work(&self, scope: &Scope) -> Option<(f64, f64)> {
        if !self.has_phase_figures() {
            return None;
        }
        let mut sides = (0.0, 0.0);
        for f in &self.funcs {
            for (&phase, figures) in &f.phases {
                if scope.covers(phase) {
                    sides.0 += (figures.self_work_ms - figures.harness_self_work_ms).max(0.0);
                    sides.1 += figures.harness_self_work_ms;
                }
            }
        }
        Some(sides)
    }

    /// The work clock each marked phase took, from the marks (phase index, milliseconds)
    pub fn phase_work(&self) -> Vec<(u32, f64)> {
        let Some(work) = &self.raw.work else { return Vec::new() };
        let marks = &self.raw.marks;
        marks
            .iter()
            .enumerate()
            .filter_map(|(at, mark)| {
                let phase = self.raw.phases.iter().position(|name| *name == mark.phase)? as u32;
                let end = marks.get(at + 1).map_or(work.total_ms, |next| next.work_ms);
                Some((phase, (end - mark.work_ms).max(0.0)))
            })
            .collect()
    }

    /// Frames per phase (from the timeline), by phase index
    pub fn phase_frames(&self) -> BTreeMap<u32, (u64, u64, u64)> {
        // phase -> (frames, first frame, last frame)
        let mut out: BTreeMap<u32, (u64, u64, u64)> = BTreeMap::new();
        if let Some(timeline) = &self.raw.timeline {
            let (Some(frame), Some(phase)) = (timeline.index("frame"), timeline.index("phase")) else { return out };
            for row in &timeline.rows {
                let f = row.get(frame).and_then(serde_json::Value::as_u64).unwrap_or(0);
                let p = row.get(phase).and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                let entry = out.entry(p).or_insert((0, f, f));
                entry.0 += 1;
                entry.1 = entry.1.min(f);
                entry.2 = entry.2.max(f);
            }
        }
        out
    }

    /// The steady state: the last phase past startup with frames that holds at least a tenth of them (or 10), else the
    /// last phase past startup. (phase index, frames in it). Never startup (phase 0), which holds the mount: a run that
    /// marks no phase after it has no steady state the profile can tell apart
    pub fn steady_phase(&self) -> Option<(u32, u64)> {
        let frames: BTreeMap<u32, (u64, u64, u64)> = self.phase_frames().into_iter().filter(|(phase, _)| *phase > 0).collect();
        let all: u64 = frames.values().map(|entry| entry.0).sum();
        let pick = frames.iter().rev().find(|(_, entry)| entry.0 >= 10.min(all.max(1)) && entry.0 * 10 >= all).or_else(|| frames.iter().next_back());
        if let Some((&phase, entry)) = pick {
            return Some((phase, entry.0));
        }
        if self.raw.phases.len() > 1 && self.raw.timeline.is_none() {
            return Some((self.raw.phases.len() as u32 - 1, self.raw.run.frames));
        }
        None
    }

    /// The source of a module: (path, lines by number) from its listings, or the file on disk when the profile names it
    /// (the listings hold the hottest functions only), and whether the file on disk is the one the profile ran
    pub fn source(&self, module: &str) -> Option<Source> {
        let mut path = None;
        let mut listed: BTreeMap<i64, String> = BTreeMap::new();
        for (id, listing) in &self.raw.listings {
            let Some(func) = id.parse::<i64>().ok().and_then(|id| self.func_of_id(id)) else { continue };
            if self.funcs[func].module != module {
                continue;
            }
            path.get_or_insert_with(|| listing.path.clone());
            for (number, text) in &listing.lines {
                listed.insert(*number, text.clone());
            }
        }
        let path = path?;
        if let Ok(bytes) = std::fs::read(&path) {
            let hash = format!("{:016x}", fxhash(&bytes));
            let same = self.raw.source_hashes.get(module).is_none_or(|known| *known == hash);
            if same {
                let text = String::from_utf8_lossy(&bytes);
                let lines = text.lines().enumerate().map(|(index, line)| (index as i64 + 1, line.trim_end_matches('\r').to_string())).collect();
                return Some(Source { path, lines, whole: true, changed: false });
            }
            return Some(Source { path, lines: listed, whole: false, changed: true });
        }
        Some(Source { path, lines: listed, whole: false, changed: false })
    }

    /// The listing path of a module, when any of its functions was listed
    pub fn module_path(&self, module: &str) -> Option<String> {
        self.raw.listings.iter().find(|(id, _)| id.parse::<i64>().ok().and_then(|id| self.func_of_id(id)).is_some_and(|func| self.funcs[func].module == module)).map(|(_, listing)| listing.path.clone())
    }
}

/// A module's source as the Source view shows it
#[derive(Clone, Debug)]
pub struct Source {
    pub path: String,
    pub lines: BTreeMap<i64, String>,
    /// Every line of the file (else the profile's listings only)
    pub whole: bool,
    /// The file on disk is not the source the profile ran: the listings are shown instead
    pub changed: bool,
}

/// How a folded file names a function (report.rs `Names::frame`)
fn frame_name(func: &Func) -> String {
    match func.kind {
        Kind::Script | Kind::Engine if !func.module.is_empty() => format!("{} {}:{}", func.name, func.module, func.line),
        _ => func.name.clone(),
    }
}

/// The source hash profile.json's `source_hashes` holds (rustc-hash's FxHasher over the bytes, as ree hashes them)
pub fn fxhash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    bytes.hash(&mut hasher);
    hasher.finish()
}

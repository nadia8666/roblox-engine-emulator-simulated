//! `ree profile try <file.luau>`: other ways to write the same thing, measured side by side (docs/cli.md "What-if
//! comparisons"). The variant file names the variants, the input sizes and how to check their results; every variant at
//! every size runs in a fresh `ree run` of a generated harness (whatif.luau) under the profiler, host time is read in runs
//! of its own (interleaved rounds with an A/A pair), and `compare.json` indexes what the profiles show: each run's figures
//! by phase, the reachable tables' shapes by the site that made them, how each figure grows with n and which variant wins
//! on each.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::Instant,
};

use serde_json::{Value as Json, json};

use super::stats;

/// The harness every run of a comparison runs, written into its folder
pub const HARNESS: &str = include_str!("whatif.luau");

const USAGE: &str = "usage: ree profile try <file.luau> [--out <dir>] [--sizes a,b] [--variants a,b] [--frames <n>] [--rounds <n>]
                       [--max-rounds <n>] [--time-ms <ms>] [--jobs <n>] [--no-time] [--no-hw] [--no-check] [--keep-timed]
                       [--script-timeout <s>] [engine options: --roots, --project, --place, --mount, ...]
                       (docs/cli.md \"What-if comparisons\")";

/// The kinds each profiled run takes: what compare.json reads (figures by phase from the marks, heap snapshots and table
/// shapes, the call tree, caches, the timeline)
const PROFILE_KINDS: &str = "work,caches,alloc,heap,timeline,phases";

/// Rounds added at a time for the host-time pairs still undecided
const ADDED_ROUNDS: usize = 6;
/// The smallest host-time gap a verdict names, whatever the noise: below it, how two functions' code happens to lie in
/// memory can move one against the other as much as their design does
const HOST_FLOOR: f64 = 0.02;
/// A 60 fps frame, and the share of it below which a gap in a ms figure is too small to matter at the file's `per_frame`
/// calls a frame; and the absolute floor a call (no price or clock resolves less)
const FRAME_MS: f64 = 1000.0 / 60.0;
const MATERIAL_SHARE: f64 = 0.001;
const MATERIAL_NS: f64 = 1.0;
/// The gap in a count from which it matters, in what a frame pays (the gap a call times the file's `per_frame`): this
/// share of a call's instructions, and at least this many; and the gap in bytes a frame
const MATERIAL_COUNT_SHARE: f64 = 0.001;
const MATERIAL_COUNT: f64 = 10.0;
const MATERIAL_BYTES: f64 = 1024.0;

/// The gap in a ms figure a call from which it matters: 0.1 % of a 60 fps frame over the calls the game makes a frame, and
/// 1 ns at least
pub fn ms_floor(per_frame: f64) -> f64 {
    (MATERIAL_NS * 1e-6).max(FRAME_MS * MATERIAL_SHARE / per_frame.max(1e-9))
}

/// What a gap is judged against: the ms floor a call ([`ms_floor`]), the calls a frame (the file's `per_frame`), a call's
/// instructions and inline cache lookups, and the clock the cycles ran at (cycles a ms, measured from the same runs)
#[derive(Clone, Copy, Debug)]
struct Floor {
    ms: f64,
    per_frame: f64,
    insns: f64,
    lookups: f64,
    cycles_per_ms: Option<f64>,
}

/// Whether a gap between two variants' figures matters, by the gap itself and never by how large either figure is, and
/// by what a frame pays, as the ms floor is: a ms figure's from the floor; cycles as the time they take at the clock they
/// ran at, against the same floor; a count a call (of a per-call figure) times the calls a frame, from 0.1 % of a call's
/// instructions and 10; bytes so from 1 KB; a hit rate's by the lookups it moves, as a count's; instructions always (they
/// are the work itself). A figure that is not per call (the heap a phase holds) is judged as it is
fn material(metric: &Metric, gap: f64, floor: Floor) -> bool {
    let per_frame = if metric.per_call || metric.key == "cache_hit_rate" { floor.per_frame.max(1e-9) } else { 1.0 };
    let count_floor = (floor.insns * MATERIAL_COUNT_SHARE).max(MATERIAL_COUNT);
    match (metric.key, metric.unit) {
        ("host_cycles" | "hw_cycles", _) => match floor.cycles_per_ms.filter(|rate| *rate > 0.0) {
            Some(rate) => gap / rate >= floor.ms,
            None => gap * per_frame >= count_floor,
        },
        (_, "ms") => gap >= floor.ms,
        ("insns", _) => true,
        (_, "count") => gap * per_frame >= count_floor,
        (_, "bytes") => gap * per_frame >= MATERIAL_BYTES,
        ("cache_hit_rate", _) => gap * floor.lookups * per_frame >= count_floor,
        _ => true,
    }
}

const FPS: f64 = 60.0;
/// What a timed run's prepared inputs may hold (its calls are cut to fit)
const TIMED_INPUT_BYTES: f64 = 256.0 * 1024.0 * 1024.0;
/// The fewest calls a timed run makes (when its inputs fit)
const MIN_REPS: f64 = 3.0;
/// Rounds of the hardware counters' runs
const HW_ROUNDS: usize = 3;
/// The inputs a timed run prepares at a time (a group, whose calls run back to back): about what the CPU's L2 cache holds
const TIMED_GROUP_BYTES: f64 = 1024.0 * 1024.0;

/// A timed run's host time and cycles over its "run" stretches: each from a "run" mark to the mark after it
fn run_stretches(profile: &Json) -> Option<(f64, f64)> {
    let marks = profile["marks"].as_array()?;
    let (mut ms, mut cycles, mut stretches) = (0.0, 0.0, 0);
    for pair in marks.windows(2).filter(|pair| pair[0]["phase"] == "run") {
        ms += pair[1]["host_ms"].as_f64()? - pair[0]["host_ms"].as_f64()?;
        cycles += pair[1]["cycles"].as_f64()? - pair[0]["cycles"].as_f64()?;
        stretches += 1;
    }
    (stretches > 0).then_some((ms, cycles))
}

/// `items` in an order of `seed`'s (Fisher-Yates with a xorshift: the same order for the same seed)
fn shuffled<T: Copy>(items: &[T], seed: u64) -> Vec<T> {
    let mut items = items.to_vec();
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ seed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for index in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(index, (state % (index as u64 + 1)) as usize);
    }
    items
}
/// The harness's file name, and its module as a profile names it (its frames are left out of a site)
const HARNESS_MODULE: &str = "WhatIf";
const HARNESS_CHUNK: &str = "ReplicatedStorage.RURFixtures.WhatIf";
/// Where the variant file is mounted (and the versions, below Versions)
const MOUNT: &str = "ReplicatedStorage.WhatIf";

/// Frames of the harness and of the engine's own Luau, which a site or a hot list leaves out
fn skipped_module(module: &str) -> bool {
    module == HARNESS_CHUNK || module.starts_with("rur.") || module.starts_with("ree.")
}

/// A figure every run of a comparison can carry
pub struct Metric {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    /// `lower`, `higher`, or `none` (no winner)
    pub better: &'static str,
    /// The same in every run of the same code
    pub exact: bool,
    /// Fitted against n (`scaling`)
    pub scales: bool,
    /// A run's figure is per call of run (the phase's total divided by its calls)
    pub per_call: bool,
    pub about: &'static str,
}

const fn metric(key: &'static str, label: &'static str, unit: &'static str, better: &'static str, exact: bool, scales: bool, per_call: bool, about: &'static str) -> Metric {
    Metric { key, label, unit, better, exact, scales, per_call, about }
}

/// The figures, in the order a table shows them
pub const METRICS: &[Metric] = &[
    metric("work_ms", "work clock, first in frame", "ms", "lower", true, true, true, "the work clock of the variant's own code (the generated harness's left out), as its first call in a frame pays it: ms of a client's time (docs/engine.md \"Work clock\")"),
    metric("warm_ms", "work clock, hot in a loop", "ms", "lower", true, true, true, "the variant's work clock without any cold or first-use charge (tables, functions and their code, C and engine functions, instances, members and engine paths): what a call costs when a loop has run it already"),
    metric("host_ms", "host time", "ms", "lower", false, true, true, "this machine's time a call took in ree's interpreter: the fastest of the timed rounds (whatever else runs on the machine only ever slows a round)"),
    metric("host_ms_median", "host time, median round", "ms", "none", false, false, true, "the median of the timed rounds: above host time by what slowed the rounds"),
    metric("host_cycles", "host cycles", "count", "lower", false, false, true, "the run thread's CPU cycles a call took (reference cycles): the fastest of the timed rounds"),
    metric("insns", "instructions", "count", "lower", true, true, true, "Luau instructions dispatched"),
    metric("alloc_bytes", "allocated", "bytes", "lower", true, true, true, "bytes the VM allocated: objects, table parts, stacks"),
    metric("allocs", "allocations", "count", "lower", true, true, true, "objects the VM allocated: tables, closures, strings, userdata, threads"),
    metric("heap_bytes", "heap held", "bytes", "lower", true, true, false, "the reachable heap at the phase's end beyond its start: what the phase still holds"),
    metric("gc_steps", "collector steps", "count", "lower", true, true, true, "the collector's steps (it paces itself by the scripts' work, so they are the same every run); the work clock charges their work in the work figure, and gc_ms is their host time"),
    metric("gc_freed", "collector freed", "bytes", "lower", true, false, true, "bytes the collector freed"),
    metric("gc_ms", "collector host time", "ms", "none", false, false, true, "host time of the collector's steps in the profiled run (its profilers slow it: a guide, not a measure)"),
    metric("cache_hit_rate", "inline cache hits", "ratio", "higher", true, false, false, "Luau's inline caches: hits of the lookups a table serves (a value another type's metamethod serves is no cache's)"),
    metric("cache_uncharged_ms", "inline caches' uncharged work", "ms", "lower", true, true, true, "what the inline caches' outcomes the work clock does not charge cost over hits at the committed prices (profiler/prices.rs), in the work clock's ms, beyond work_ms: a reprediction, an absent key, an insert with room, a metatable walk by its depth; a rehash and a looked-up import are the clock's own charge, already in work_ms; prices within the noise are left out"),
    metric("cache_lookups", "inline cache lookups", "count", "none", true, false, true, "the lookups a table serves"),
    metric("cache_hits", "inline cache hit count", "count", "none", true, false, true, "inline cache hits"),
    metric("cold_tables", "cold tables", "count", "lower", true, true, true, "tables the frame touched first that were made before it, the whole phase's with the harness's (a client finds them out of its CPU's caches)"),
    metric("cold_ms", "cold and first-use charges", "ms", "lower", true, false, true, "what the work clock charged the variant's code for the frame's cold and first-use touches: first in frame less hot in a loop"),
    metric("rehashes", "table rehashes", "count", "lower", true, true, true, "tables that rehashed to grow"),
    metric("rehash_keys", "keys rehashed", "count", "lower", true, true, true, "keys those rehashes moved"),
    metric("hw_instructions", "CPU instructions", "count", "lower", false, true, true, "instructions the CPU retired (hardware counter)"),
    metric("hw_cycles", "CPU cycles", "count", "lower", false, true, true, "core cycles (hardware counter)"),
    metric("hw_ipc", "CPU IPC", "ratio", "higher", false, false, false, "instructions a cycle"),
    metric("hw_load_stall_cycles", "cycles stalled on loads", "count", "lower", false, true, true, "cycles nothing retired because the oldest op waited for load data (hardware counter): what memory cost"),
    metric("hw_load_stall_share", "load stall share", "ratio", "lower", false, false, false, "the share of cycles stalled on loads"),
    metric("hw_miss_latency", "L1D miss latency", "count", "lower", false, false, false, "cycles an L1 data cache miss stayed outstanding on average (misses outstanding a cycle, summed, over misses allocated): where misses were served from, not what they cost"),
    metric("hw_miss_buffers", "L1D miss buffers", "count", "lower", false, true, true, "lines the L1 data cache fetched (hardware counter: one a line, however many loads missed it)"),
    metric("hw_demand_fills", "demand fills", "count", "lower", false, true, true, "lines filled for demand loads and stores (hardware counter: one a line; prefetched lines not at all)"),
    metric("hw_demand_fills_beyond_l2", "demand fills beyond L2", "count", "lower", false, true, true, "demand fills from beyond the core's L2 (hardware counter)"),
    metric("hw_demand_fills_beyond_l3", "demand fills beyond L3", "count", "lower", false, true, true, "demand fills from beyond the core complex's L3 (hardware counter)"),
    metric("hw_branch_misses", "branch misses", "count", "lower", false, true, true, "mispredicted branches (hardware counter)"),
];

pub fn metric_of(key: &str) -> Option<&'static Metric> {
    METRICS.iter().find(|metric| metric.key == key)
}

/// A comparison's options (the command line)
#[derive(Clone, Debug)]
pub struct Options {
    pub file: PathBuf,
    pub out: PathBuf,
    pub sizes: Option<Vec<f64>>,
    pub variants: Option<Vec<String>>,
    pub frames: Option<u32>,
    pub rounds: u32,
    /// The most rounds host time may take with the rounds added for undecided pairs (`rounds` adds none)
    pub max_rounds: u32,
    pub time_ms: f64,
    pub jobs: usize,
    pub time: bool,
    pub hw: bool,
    pub check: bool,
    pub keep_timed: bool,
    /// The options every run gets: engine options, the guards'
    pub passed: Vec<String>,
    /// The ree that runs them (this one)
    pub exe: PathBuf,
    pub command: String,
}

fn numbers(key: &str, text: &str) -> Result<Vec<f64>, String> {
    text.split(',').map(str::trim).filter(|word| !word.is_empty()).map(|word| word.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| format!("{key}: {word:?} is not a size"))).collect()
}

/// `ree profile try`'s command line
pub fn parse(args: &[String]) -> Result<Options, String> {
    let args = crate::launch::split_equals(args);
    // A comparison writes its harness and contexts before starting an engine, even with an explicit --out.
    crate::require_standalone_state(&crate::default_repo())?;
    let mut engine = crate::launch::Args::default();
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut options = Options {
        file: PathBuf::new(),
        out: PathBuf::new(),
        sizes: None,
        variants: None,
        frames: None,
        rounds: 9,
        max_rounds: 0,
        time_ms: 10.0,
        jobs: 4,
        time: true,
        hw: true,
        check: true,
        keep_timed: false,
        passed: Vec::new(),
        exe: std::env::current_exe().map_err(|error| format!("this ree: {error}"))?,
        command: format!("ree profile try {}", args.join(" ")),
    };
    let mut index = 0;
    while index < args.len() {
        let key = args[index].clone();
        let value = args.get(index + 1).cloned();
        let need = || value.clone().ok_or_else(|| format!("{key} needs a value"));
        let count = |text: String| text.parse::<u32>().ok().filter(|n| *n >= 1).ok_or_else(|| format!("{key}: a whole number from 1"));
        let mut used = 2;
        match key.as_str() {
            "--out" => out = Some(need()?),
            "--sizes" => options.sizes = Some(numbers(&key, &need()?)?).filter(|sizes| !sizes.is_empty()),
            "--variants" => options.variants = Some(need()?.split(',').map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).collect()),
            "--frames" => options.frames = Some(count(need()?)?),
            "--rounds" => options.rounds = count(need()?)?,
            "--max-rounds" => options.max_rounds = count(need()?)?,
            "--jobs" => options.jobs = count(need()?)? as usize,
            "--time-ms" => options.time_ms = need()?.parse::<f64>().ok().filter(|ms| *ms > 0.0).ok_or("--time-ms: milliseconds above 0")?,
            "--script-timeout" | "--max-script-timeouts" | "--max-wall-seconds" => options.passed.extend([key.clone(), need()?]),
            "--no-time" | "--no-hw" | "--no-check" | "--keep-timed" => {
                used = 1;
                match key.as_str() {
                    "--no-time" => options.time = false,
                    "--no-hw" => options.hw = false,
                    "--no-check" => options.check = false,
                    _ => options.keep_timed = true,
                }
            }
            _ if key.starts_with("--") => match engine.accept(&key, value.as_ref())? {
                Some(taken) => {
                    options.passed.extend(args[index..index + taken].iter().cloned());
                    used = taken;
                }
                None => return Err(format!("no option {key}")),
            },
            _ => {
                if let Some(first) = &file {
                    return Err(format!("one variant file: {first}, then {key}"));
                }
                file = Some(key.clone());
                used = 1;
            }
        }
        index += used;
    }
    let file = file.ok_or("a variant file")?;
    // (by default host time may take three times its rounds, as rounds are added for pairs still undecided)
    options.max_rounds = if options.max_rounds == 0 { options.rounds * 3 } else { options.max_rounds.max(options.rounds) };
    options.file = std::path::absolute(&file).map_err(|error| format!("{file}: {error}"))?;
    if !options.file.is_file() {
        return Err(format!("{file}: no such file"));
    }
    let stem = options.file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "variants".into());
    let out = out.map(PathBuf::from).unwrap_or_else(|| crate::default_output_path(Path::new("what-if").join(&stem)));
    options.out = std::path::absolute(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    // without a project (no roots, project, place or mount), the runs have none: a folder of the harness's own stops the
    // implicit project a checkout may have
    if engine.roots.is_empty() && engine.mounts.is_empty() && engine.project.is_none() && engine.place.is_none() {
        options.passed.extend(["--instance".to_string(), format!("ReplicatedStorage/{HARNESS_MODULE}=Folder")]);
    }
    Ok(options)
}

/// A variant's folder name: letters, digits, `.`, `_` and `-`, the rest as `-`, lowercase, never empty or a name Windows
/// keeps for a device; `taken` makes it unique
pub fn slug(name: &str, taken: &[String]) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c.to_ascii_lowercase() } else { '-' };
        if c == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(c);
    }
    let mut slug = slug.trim_matches(|c| c == '-' || c == '.').to_string();
    if slug.is_empty() {
        slug = "variant".into();
    }
    let device = slug.split('.').next().unwrap_or_default();
    if matches!(device, "con" | "prn" | "aux" | "nul") || (device.len() == 4 && (device.starts_with("com") || device.starts_with("lpt")) && device.as_bytes()[3].is_ascii_digit()) {
        slug.push_str("-v");
    }
    let mut unique = slug.clone();
    let mut next = 2;
    while taken.iter().any(|other| other == &unique) {
        unique = format!("{slug}-{next}");
        next += 1;
    }
    unique
}

/// A size as a folder name and in text: whole numbers without a fraction
pub fn size_label(size: f64) -> String {
    if size.fract() == 0.0 && size.abs() < 1e15 { format!("{}", size as i64) } else { format!("{size}") }
}

/// A variant of the file, as the harness listed it
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub slug: String,
    pub kind: String,
    pub source: Option<String>,
    /// A version's file, as the variant file gives it and resolved
    pub file: Option<String>,
    pub path: Option<PathBuf>,
}

/// The variant file as the harness read it
#[derive(Clone, Debug)]
pub struct Spec {
    pub title: Option<String>,
    pub about: Option<String>,
    pub sizes: Vec<f64>,
    pub frames: u32,
    pub check: bool,
    /// How the results are checked: `function` (the file's check), `equality` (check = true), None (no check: by digest)
    pub check_kind: Option<String>,
    /// How many times a frame the game runs the code (`per_frame`, default 1): what makes a gap in a ms figure material
    pub per_frame: f64,
    pub module: Option<String>,
    pub variants: Vec<Variant>,
}

/// What a `ree run` of the harness printed: its exit code, its summary (stdout), the harness's `@try-` lines, the tail of
/// its stderr
struct Ran {
    code: Option<i32>,
    summary: Json,
    lines: Vec<(String, Json)>,
    stderr: String,
}

impl Ran {
    /// Why the run did not finish, None when it did
    fn failure(&self) -> Option<String> {
        if let Some((_, fail)) = self.lines.iter().find(|(kind, _)| kind == "fail") {
            return Some(fail["message"].as_str().unwrap_or("the harness failed").to_string());
        }
        if self.code == Some(0) && self.summary["status"] == "ok" {
            return None;
        }
        let failure = self.summary["failure"].as_str().map(str::to_string);
        let tail: Vec<&str> = self.stderr.lines().filter(|line| !line.trim().is_empty()).collect();
        let tail = tail[tail.len().saturating_sub(4)..].join("\n");
        Some(failure.unwrap_or_else(|| format!("ree run exited {}: {tail}", self.code.map_or("without a code".to_string(), |code| code.to_string()))))
    }
}

/// The runs' own environment: a profile asked for by the environment would profile every run alike
const UNSET: [&str; 5] = ["REE_PROFILE", "REE_PROFILE_OUT", "REE_PROFILE_COUNTERS", "RUR_LUAU_PROFILE", "RUR_LUAU_ALLOC_PROFILE"];

fn ree_run(options: &Options, args: &[String], timed: bool) -> Ran {
    let mut command = Command::new(&options.exe);
    command.arg("run").args(args).args(&options.passed);
    for variable in UNSET {
        command.env_remove(variable);
    }
    // (the harness is ree's own: its code is never the game's, nor a user's harness: report.rs tool_module)
    command.env(crate::profiler::report::TOOL_HARNESS_ENV, "1");
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // no console window of its own; a timed run above normal priority, as measurements on a shared machine are run
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x0000_8000;
        command.creation_flags(CREATE_NO_WINDOW | if timed { ABOVE_NORMAL_PRIORITY_CLASS } else { 0 });
    }
    #[cfg(not(windows))]
    let _ = timed;
    match command.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let summary: Json = serde_json::from_str(stdout.trim()).unwrap_or(Json::Null);
            let lines = summary["output"]
                .as_array()
                .map(|output| {
                    output
                        .iter()
                        .filter_map(|line| line["message"].as_str())
                        .filter_map(|message| message.strip_prefix("@try-"))
                        .filter_map(|rest| {
                            let (kind, body) = rest.split_once(' ')?;
                            Some((kind.to_string(), serde_json::from_str(body).unwrap_or(Json::Null)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ran { code: output.status.code(), summary, lines, stderr: String::from_utf8_lossy(&output.stderr).into_owned() }
        }
        Err(error) => Ran { code: None, summary: Json::Null, lines: Vec::new(), stderr: format!("{}: {error}", options.exe.display()) },
    }
}

/// One run's arguments: the harness, its context (written to a file), and enough emulated time for `frames` frames
fn run_args(out: &Path, context_name: &str, context: &Json, frames: f64) -> Result<Vec<String>, String> {
    let contexts = out.join("harness").join("context");
    std::fs::create_dir_all(&contexts).map_err(|error| format!("{}: {error}", contexts.display()))?;
    let path = contexts.join(format!("{context_name}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(context).unwrap_or_default()).map_err(|error| format!("{}: {error}", path.display()))?;
    let seconds = (frames + 8.0) / FPS;
    Ok(vec![
        "--harness".into(),
        out.join("harness").join(format!("{HARNESS_MODULE}.luau")).to_string_lossy().into_owned(),
        "--context".into(),
        path.to_string_lossy().into_owned(),
        "--seconds".into(),
        format!("{seconds:.4}"),
        "--fps".into(),
        format!("{FPS}"),
    ])
}

/// The context every mode's run gets: the file, and the versions' sources and paths
fn base_context(options: &Options, spec: Option<&Spec>, mode: &str) -> Json {
    let mut context = json!({"Mode": mode, "File": options.file.to_string_lossy()});
    if let Some(spec) = spec {
        let mut sources = serde_json::Map::new();
        let mut paths = serde_json::Map::new();
        for variant in spec.variants.iter().filter(|variant| variant.kind == "module") {
            if let Some(path) = &variant.path {
                sources.insert(variant.name.clone(), json!(std::fs::read_to_string(path).unwrap_or_default()));
                paths.insert(variant.name.clone(), json!(path.to_string_lossy()));
            }
        }
        if !sources.is_empty() {
            context["Sources"] = Json::Object(sources);
            context["Paths"] = Json::Object(paths);
        }
        if let Some(frames) = options.frames {
            context["Frames"] = json!(frames);
        }
    }
    context
}

/// The variant file's variants, sizes and options, as the harness reads them (`--variants`, `--sizes` and `--frames` applied)
fn list(options: &Options) -> Result<Spec, String> {
    let context = base_context(options, None, "list");
    let ran = ree_run(options, &run_args(&options.out, "list", &context, 4.0)?, false);
    if let Some(failure) = ran.failure() {
        return Err(failure);
    }
    let listed = ran.lines.iter().find(|(kind, _)| kind == "list").map(|(_, body)| body.clone()).ok_or("the harness listed no variants")?;
    let folder = options.file.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut variants: Vec<Variant> = Vec::new();
    for entry in listed["variants"].as_array().into_iter().flatten() {
        let name = entry["name"].as_str().unwrap_or_default().to_string();
        let taken: Vec<String> = variants.iter().map(|variant| variant.slug.clone()).collect();
        let file = entry["file"].as_str().map(str::to_string);
        let path = file.as_ref().map(|file| {
            let path = Path::new(file);
            let path = if path.is_absolute() { path.to_path_buf() } else { folder.join(path) };
            std::path::absolute(&path).unwrap_or(path)
        });
        if let Some(path) = &path {
            if !path.is_file() {
                return Err(format!("version {name}: {}: no such file", path.display()));
            }
        }
        variants.push(Variant { slug: slug(&name, &taken), name, kind: entry["kind"].as_str().unwrap_or("function").to_string(), source: entry["source"].as_str().map(str::to_string), file, path });
    }
    if let Some(wanted) = &options.variants {
        let mut chosen = Vec::new();
        for name in wanted {
            let variant = variants.iter().find(|variant| &variant.name == name).ok_or_else(|| format!("--variants: no variant {name:?} (the file has {})", variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>().join(", ")))?;
            chosen.push(variant.clone());
        }
        variants = chosen;
    }
    let mut sizes: Vec<f64> = match &options.sizes {
        Some(sizes) => sizes.clone(),
        None => listed["sizes"].as_array().into_iter().flatten().filter_map(Json::as_f64).collect(),
    };
    sizes.sort_by(f64::total_cmp);
    sizes.dedup();
    Ok(Spec {
        title: listed["title"].as_str().map(str::to_string),
        about: listed["about"].as_str().map(str::to_string),
        sizes,
        frames: options.frames.unwrap_or_else(|| listed["frames"].as_u64().unwrap_or(1).max(1) as u32),
        // (a string names the kind; a harness from before said only whether the file had a check)
        check: listed["check"].as_bool().unwrap_or(false) || listed["check"].is_string(),
        check_kind: listed["check"].as_str().map(str::to_string).or_else(|| listed["check"].as_bool().filter(|check| *check).map(|_| "function".to_string())),
        per_frame: listed["per_frame"].as_f64().filter(|rate| rate.is_finite() && *rate > 0.0).unwrap_or(1.0),
        module: listed["module"].as_str().map(str::to_string),
        variants,
    })
}

/// A phase's figures by metric key (absent: not measured)
pub type Figures = BTreeMap<&'static str, f64>;

/// One variant at one size: its profiled run, its timed rounds and its hardware counters
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub variant: usize,
    pub size: usize,
    pub status: String,
    pub error: Option<String>,
    pub digest: Option<String>,
    pub check: Option<(bool, Option<String>)>,
    pub setup: Figures,
    pub run: Figures,
    pub heap: Option<(f64, f64, f64)>,
    pub tables: Json,
    pub hot: Vec<Hot>,
    /// The timed rounds' figures a call, one per round (None: that round's run failed)
    pub host_ms: Vec<Option<f64>>,
    pub host_cycles: Vec<Option<f64>>,
    /// The calls a timed run made, and how many of them it prepared and ran at a time
    pub group: u64,
    pub reps: u64,
    pub hw_profile: bool,
    /// The hardware counters' figures a call, by metric key, one per round (None: that round's run failed)
    pub hw_rounds: BTreeMap<&'static str, Vec<Option<f64>>>,
    /// The core clock the counters' runs ran at: core cycles a ms, the median of its rounds
    pub hw_cycles_per_ms: Option<f64>,
}

/// A function's part of the run phase
#[derive(Clone, Debug, Default)]
pub struct Hot {
    pub key: String,
    pub name: String,
    pub module: String,
    pub line: i64,
    pub work_ms: f64,
    pub insns: f64,
    pub calls: f64,
}

fn mark<'a>(profile: &'a Json, phase: &str) -> Option<&'a Json> {
    if phase == "end" {
        return profile.get("end").filter(|end| !end.is_null());
    }
    profile["marks"].as_array()?.iter().find(|mark| mark["phase"] == phase)
}

fn number(value: &Json) -> Option<f64> {
    value.as_f64()
}

/// The figures between two marks of a profile, flows divided by `calls`
pub fn phase_figures(profile: &Json, from: &str, to: &str, calls: f64) -> Figures {
    let mut figures = Figures::new();
    let (Some(a), Some(b)) = (mark(profile, from), mark(profile, to)) else { return figures };
    let calls = calls.max(1.0);
    let delta = |path: &[&str]| -> Option<f64> {
        let read = |mark: &Json| -> Option<f64> { path.iter().try_fold(mark, |value, key| value.get(*key)).and_then(number) };
        Some(read(b)? - read(a)?)
    };
    let mut put = |key: &'static str, value: Option<f64>| {
        if let (Some(value), Some(metric)) = (value, metric_of(key)) {
            figures.insert(key, if metric.per_call { value / calls } else { value });
        }
    };
    put("work_ms", delta(&["work_ms"]));
    put("insns", delta(&["insns"]));
    put("alloc_bytes", delta(&["alloc_bytes"]));
    put("allocs", delta(&["allocs"]));
    // (a script's own: the engine's bookkeeping rehashes too)
    put("rehashes", delta(&["script_rehashes"]));
    put("rehash_keys", delta(&["script_rehash_keys"]));
    if a.get("gc").is_some() && b.get("gc").is_some() && profile["kinds"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind == "heap" || kind == "timeline")) {
        put("gc_steps", delta(&["gc", "steps"]));
        put("gc_freed", delta(&["gc", "freed"]));
        put("gc_ms", delta(&["gc", "ms"]));
    }
    if let (Some(cold_tables), Some(cold_closures), Some(cold_protos), Some(cold_code)) = (delta(&["vm", "cold_tables"]), delta(&["vm", "cold_closures"]), delta(&["vm", "cold_protos"]), delta(&["vm", "cold_code"])) {
        put("cold_tables", Some(cold_tables));
        // (every cold and first-use charge the marks carry, the engine's too; an older profile's VM counts at their prices)
        let cold_ms = delta(&["cold_ms"]).unwrap_or_else(|| {
            use crate::cost::{LUAU_COLD_CLOSURE, LUAU_COLD_CODE, LUAU_COLD_PROTO, LUAU_COLD_TABLE};
            (cold_tables * LUAU_COLD_TABLE + cold_closures * LUAU_COLD_CLOSURE + cold_protos * LUAU_COLD_PROTO + cold_code * LUAU_COLD_CODE) * 1e-6
        });
        put("cold_ms", Some(cold_ms));
        if let Some(work) = delta(&["work_ms"]) {
            put("warm_ms", Some((work - cold_ms).max(0.0)));
        }
    }
    if let (Some(before), Some(after)) = (a["caches"].as_object(), b["caches"].as_object()) {
        use crate::profiler::vm;
        let (mut hits, mut lookups, mut uncharged_us) = (0.0, 0.0, 0.0);
        let counts = |row: Option<&Json>| -> Vec<u64> { row.and_then(Json::as_array).map(|o| o.iter().filter_map(Json::as_u64).collect()).unwrap_or_default() };
        for (kind, outcomes) in after {
            let (now, then) = (counts(Some(outcomes)), counts(before.get(kind)));
            let (depths_now, depths_then) = (counts(b["cache_depths"].get(kind)), counts(a["cache_depths"].get(kind)));
            let at = |values: &Vec<u64>, slot: usize| values.get(slot).copied().unwrap_or(0);
            // (luau_caches.outcomes' order; a value another type serves is no cache's lookup)
            let phase: [u64; vm::CACHE_OUTCOMES] = std::array::from_fn(|slot| at(&now, slot).saturating_sub(at(&then, slot)));
            let walks: [u64; vm::CACHE_DEPTHS] = std::array::from_fn(|depth| at(&depths_now, depth).saturating_sub(at(&depths_then, depth)));
            hits += phase[vm::HIT] as f64;
            lookups += (phase.iter().sum::<u64>() - phase[vm::NOT_TABLE]) as f64;
            if let Some(index) = vm::CACHE_KINDS.iter().position(|name| name == kind) {
                uncharged_us += crate::profiler::prices::site_extra(index, &phase, &walks, 1.0).uncharged_us;
            }
        }
        put("cache_hits", Some(hits));
        put("cache_lookups", Some(lookups));
        put("cache_uncharged_ms", Some(uncharged_us / 1e3));
        if lookups > 0.0 {
            put("cache_hit_rate", Some(hits / lookups));
        }
    }
    figures
}

/// The reachable heap (bytes) at a snapshot
fn reachable(profile: &Json, label: &str) -> Option<f64> {
    profile["heap"]["snapshots"].as_array()?.iter().find(|snapshot| snapshot["label"] == label)?["reachable"]["bytes"].as_f64()
}

/// A function of the profile by id: (key, name, module, line)
fn function_of(functions: &BTreeMap<i64, &Json>, id: i64) -> (String, String, String, i64) {
    match functions.get(&id) {
        Some(function) => (
            function["key"].as_str().unwrap_or_default().to_string(),
            function["name"].as_str().unwrap_or_default().to_string(),
            function["module"].as_str().unwrap_or_default().to_string(),
            function["line"].as_i64().unwrap_or(0),
        ),
        None => (String::new(), String::new(), String::new(), 0),
    }
}

/// The files of a comparison's modules by module name (frames name a module; a viewer opens its file)
pub type ModulePaths = BTreeMap<String, String>;

/// A stack's Luau frames, the harness's left out, innermost first: [{module, name, line, path}] and the site's text
fn frames_of(frames: &Json, functions: &BTreeMap<i64, &Json>, paths: &ModulePaths) -> (Json, String) {
    let mut out = Vec::new();
    let mut text = Vec::new();
    for frame in frames.as_array().into_iter().flatten() {
        let (Some(id), line) = (frame[0].as_i64(), frame[1].as_i64().unwrap_or(-1)) else { continue };
        if id < 0 {
            continue;
        }
        let (_, name, module, _) = function_of(functions, id);
        if skipped_module(&module) || functions.get(&id).is_some_and(|function| function["kind"] == "engine Luau") {
            continue;
        }
        if text.len() < 3 {
            text.push(format!("{}:{line}", super::report::short_module(&module)));
        }
        out.push(json!({"module": module, "name": name, "line": line, "path": paths.get(&module).cloned().unwrap_or_default()}));
    }
    let site = if text.is_empty() { "(engine)".to_string() } else { text.join(" < ") };
    (Json::Array(out), site)
}

/// The tables the setup and run phases made that are reachable after the run (the result held), by site
fn table_sites(profile: &Json, functions: &BTreeMap<i64, &Json>, paths: &ModulePaths) -> Json {
    let Some(snapshot) = profile["heap"]["snapshots"].as_array().and_then(|snapshots| snapshots.iter().find(|snapshot| snapshot["label"] == "after")) else { return Json::Null };
    let tables = &snapshot["tables"];
    let mut sites = Vec::new();
    // (count, bytes) of all, of setup's and of run's
    let mut totals = [(0.0, 0.0); 3];
    for site in tables["sites"].as_array().into_iter().flatten() {
        let phase = site["phase"].as_str().unwrap_or_default();
        let slot = match phase {
            "setup" => 1,
            "run" => 2,
            _ => continue,
        };
        let (frames, text) = frames_of(&site["frames"], functions, paths);
        let (count, bytes) = (site["count"].as_f64().unwrap_or(0.0), site["bytes"].as_f64().unwrap_or(0.0));
        for at in [0, slot] {
            totals[at].0 += count;
            totals[at].1 += bytes;
        }
        let mut entry = site.clone();
        entry["site"] = json!(text);
        entry["frames"] = frames;
        sites.push(entry);
    }
    json!({
        "count": totals[0].0, "bytes": totals[0].1,
        "setup": {"count": totals[1].0, "bytes": totals[1].1}, "run": {"count": totals[2].0, "bytes": totals[2].1},
        "sizes": tables["sizes"], "sites": sites,
    })
}

/// The functions with the most work in the run phase (its part of the call tree), most first
fn hot_functions(profile: &Json, functions: &BTreeMap<i64, &Json>, top: usize) -> Vec<Hot> {
    let Some(tree) = profile["tree"].as_array() else { return Vec::new() };
    let Some(root) = tree.iter().position(|node| node[1].as_i64() == Some(-1) && node[5] == "run") else { return Vec::new() };
    let mut inside = vec![false; tree.len()];
    let mut by_function: BTreeMap<i64, (f64, f64, f64)> = BTreeMap::new();
    for (index, node) in tree.iter().enumerate() {
        let parent = node[0].as_i64().unwrap_or(-1);
        inside[index] = index == root || (parent >= 0 && (parent as usize) < index && inside[parent as usize]);
        let function = node[1].as_i64().unwrap_or(-1);
        if !inside[index] || function < 0 {
            continue;
        }
        let entry = by_function.entry(function).or_default();
        entry.0 += node[3].as_f64().unwrap_or(0.0) * 1e-3;
        entry.1 += node[4].as_f64().unwrap_or(0.0);
        entry.2 += node[2].as_f64().unwrap_or(0.0);
    }
    let mut hot: Vec<Hot> = by_function
        .into_iter()
        .filter_map(|(id, (work_ms, insns, calls))| {
            let (key, name, module, line) = function_of(functions, id);
            let skip = skipped_module(&module) || functions.get(&id).is_some_and(|function| function["kind"] == "engine Luau");
            (!skip && (work_ms > 0.0 || insns > 0.0)).then_some(Hot { key, name, module, line, work_ms, insns, calls })
        })
        .collect();
    hot.sort_by(|a, b| b.work_ms.total_cmp(&a.work_ms).then(a.key.cmp(&b.key)));
    hot.truncate(top);
    hot
}

/// The work of the variant's own code in `phase` and its cold and first-use charges (ms, the run's total): each function's self work
/// there less what it did for the harness, with cold charges split by the same caller side. None for a profile
/// without the per-function split (before the work clock's rework).
fn own_work(profile: &Json, phase: &str) -> Option<(f64, f64)> {
    let functions = profile["functions"].as_array()?;
    let (mut work, mut cold, mut split) = (0.0, 0.0, false);
    for function in functions {
        let Some(figures) = function["phases"].get(phase) else { continue };
        let own = figures["self_work_ms"].as_f64().unwrap_or(0.0) - figures["harness_self_work_ms"].as_f64().unwrap_or(0.0);
        work += own.max(0.0);
        if let Some(function_cold) = figures["cold_work_ms"].as_f64() {
            split = true;
            let harness_cold = figures["harness_cold_work_ms"].as_f64().unwrap_or(if function["harness"] == true { function_cold } else { 0.0 });
            cold += (function_cold - harness_cold).max(0.0);
        }
    }
    split.then_some((work, cold.min(work)))
}

/// A profiled run's figures, read from its profile folder
fn read_profile(dir: &Path, frames: f64, paths: &ModulePaths) -> Result<(Figures, Figures, Option<(f64, f64, f64)>, Json, Vec<Hot>), String> {
    let profile = super::diff::load_profile(dir)?;
    let functions: BTreeMap<i64, &Json> = profile["functions"].as_array().into_iter().flatten().filter_map(|function| Some((function["id"].as_i64()?, function))).collect();
    let setup = phase_figures(&profile, "setup", "run", 1.0);
    let mut run = phase_figures(&profile, "run", "after", frames);
    // (the variant's own subtree: the generated harness's wrapper and its check are not the variant's work; first in frame with every
    // cold and first-use charge, hot in a loop without any)
    if let Some((work, cold)) = own_work(&profile, "run") {
        let calls = frames.max(1.0);
        run.insert("work_ms", work / calls);
        run.insert("warm_ms", (work - cold).max(0.0) / calls);
        run.insert("cold_ms", cold / calls);
    }
    let heap = match (reachable(&profile, "setup"), reachable(&profile, "run"), reachable(&profile, "after")) {
        (Some(before), Some(set), Some(after)) => Some((before, set, after)),
        _ => None,
    };
    let mut setup = setup;
    if let Some((before, set, after)) = heap {
        setup.insert("heap_bytes", set - before);
        run.insert("heap_bytes", after - set);
    }
    let tables = table_sites(&profile, &functions, paths);
    let hot = hot_functions(&profile, &functions, 8);
    Ok((setup, run, heap, tables, hot))
}

/// The hardware counters of a run's hw profile, the run phase per call, and the core clock they ran at: the core cycles
/// a ms of the run's own stretches (its marks' host time)
fn read_hw(dir: &Path, frames: f64) -> Option<(Figures, Option<f64>)> {
    let profile = super::diff::load_profile(dir).ok()?;
    let phases = profile["counters"]["phases"].as_array()?;
    let run = phases.iter().find(|phase| phase["phase"] == "run")?;
    // (by the counters' names now: a profile from before the fills were renamed reads the same)
    let counts: std::collections::BTreeMap<String, f64> = run["counts"].as_object()?.iter().filter_map(|(name, value)| Some((super::hw::counters::canonical(name).to_string(), value.as_f64()?))).collect();
    let mut figures = Figures::new();
    let calls = frames.max(1.0);
    for (counter, key) in [
        ("instructions", "hw_instructions"),
        ("cycles", "hw_cycles"),
        ("load-stall-cycles", "hw_load_stall_cycles"),
        ("miss-buffer-allocs", "hw_miss_buffers"),
        ("demand-fills", "hw_demand_fills"),
        ("demand-fills-beyond-l2", "hw_demand_fills_beyond_l2"),
        ("demand-fills-beyond-l3", "hw_demand_fills_beyond_l3"),
        ("branch-misses", "hw_branch_misses"),
    ] {
        if let Some(value) = counts.get(counter) {
            figures.insert(key, value / calls);
        }
    }
    let names: Vec<String> = counts.keys().cloned().collect();
    let values: Vec<f64> = counts.values().copied().collect();
    for figure in super::hw::counters::figures(&names, &values) {
        let key = match figure.key {
            "IPC" => "hw_ipc",
            super::hw::counters::LOAD_STALL_SHARE => "hw_load_stall_share",
            super::hw::counters::MISS_LATENCY => "hw_miss_latency",
            _ => continue,
        };
        figures.insert(key, figure.value);
    }
    // (the core cycles a millisecond of the run thread's time: what turns a cycle gap into the time it costs)
    let rate = match (counts.get("cycles").copied(), run_stretches(&profile)) {
        (Some(cycles), Some((run_ms, _))) if run_ms > 0.0 => Some(cycles / run_ms),
        _ => None,
    };
    (!figures.is_empty()).then_some((figures, rate))
}

/// Whether the hardware counters are read here, and why not: from a probe's hw profile
fn hw_available(dir: &Path) -> Result<(), String> {
    let profile = super::diff::load_profile(dir)?;
    if let Some(error) = profile["counters"]["error"].as_str() {
        return Err(error.to_string());
    }
    if let Some(warning) = profile["warnings"].as_array().into_iter().flatten().filter_map(Json::as_str).find(|warning| warning.starts_with("hardware counters not read")) {
        return Err(warning.to_string());
    }
    if profile["counters"]["run"]["counts"].as_object().is_none_or(|counts| counts.is_empty()) {
        return Err("the hw profile counted nothing".into());
    }
    Ok(())
}

/// How a figure grows with n, as compare.json's `scaling` and `functions` give it: the exponent at large n (the growing
/// part's slope between the two largest sizes once a fixed cost a call is fitted apart), the fit, each segment's slope, the
/// last segment's raw slope, and the points
fn growth_json(points: &[(f64, f64)]) -> Option<Json> {
    let found = stats::growth(points)?;
    Some(json!({
        "exponent": found.large_n,
        "fit": {"k": found.k, "fixed": found.fixed, "coefficient": found.coefficient, "error": found.error, "fitted": found.fitted},
        "segments": found.segments.iter().map(|(from, to, slope)| json!([from, to, slope])).collect::<Vec<_>>(),
        "last": found.segments.last().map(|segment| segment.2),
        "points": points.iter().map(|(n, value)| json!([n, value])).collect::<Vec<_>>(),
    }))
}

/// Round by round, b's figure over a's (the rounds both have)
pub fn paired(a: &[Option<f64>], b: &[Option<f64>]) -> Vec<f64> {
    a.iter().zip(b.iter()).filter_map(|(a, b)| Some((a.filter(|a| *a > 0.0)?, (*b)?))).map(|(a, b)| b / a).collect()
}

/// The timed rounds beyond each run's own: each size's A/A twin (the baseline run a second time in each round it ran), the
/// rounds run, the sizes rounds were added for and what was added
#[derive(Default)]
pub struct Timing {
    pub twin_ms: BTreeMap<usize, Vec<Option<f64>>>,
    pub twin_cycles: BTreeMap<usize, Vec<Option<f64>>>,
    pub rounds: usize,
    /// The sizes rounds were added at, and the variants (by size) that ran in them: a pair both of whose variants ran in
    /// added rounds was looked at again, and its verdict clears the later level
    pub extended: BTreeSet<usize>,
    pub added_arms: BTreeSet<(usize, usize)>,
    pub added: Vec<Json>,
    pub note: Json,
}

impl Timing {
    /// Whether the pair a, b at a size was looked at again (both ran in added rounds)
    pub fn looked_again(&self, size: usize, a: usize, b: usize) -> bool {
        self.added_arms.contains(&(a, size)) && self.added_arms.contains(&(b, size))
    }
}

/// Every arm's timed rounds with the slowed runs taken out (stats::unslowed, each run against its round's level and its
/// arm's): the runs' by (variant, size), each size's A/A twin's, and how many runs were left out of how many
pub struct Clean {
    pub runs: BTreeMap<(usize, usize), Vec<Option<f64>>>,
    pub twins: BTreeMap<usize, Vec<Option<f64>>>,
    pub slowed: usize,
    pub timed: usize,
}

impl Timing {
    pub fn clean(&self, runs: &[Run], cycles: bool) -> Clean {
        let (mut keys, mut arms) = (Vec::new(), Vec::new());
        for run in runs.iter().filter(|run| run.status == "ok") {
            let samples = if cycles { &run.host_cycles } else { &run.host_ms };
            if samples.iter().any(Option::is_some) {
                keys.push((false, run.variant, run.size));
                arms.push(samples.clone());
            }
        }
        for (size, twin) in if cycles { &self.twin_cycles } else { &self.twin_ms } {
            if twin.iter().any(Option::is_some) {
                keys.push((true, 0, *size));
                arms.push(twin.clone());
            }
        }
        let cleaned = stats::unslowed(&arms);
        let timed: usize = arms.iter().map(|arm| arm.iter().flatten().count()).sum();
        let kept: usize = cleaned.iter().map(|arm| arm.iter().flatten().count()).sum();
        let mut clean = Clean { runs: BTreeMap::new(), twins: BTreeMap::new(), slowed: timed - kept, timed };
        for ((twin, variant, size), arm) in keys.into_iter().zip(cleaned) {
            if twin {
                clean.twins.insert(size, arm);
            } else {
                clean.runs.insert((variant, size), arm);
            }
        }
        clean
    }
}

impl Clean {
    /// The A/A pairs' round-by-round log ratios at every size: the noise two runs of the same code show
    pub fn pool(&self) -> Vec<f64> {
        self.twins.iter().filter_map(|(size, twin)| Some(stats::log_ratios(self.runs.get(&(0, *size))?, twin))).flatten().collect()
    }

    /// ln(b / a) round by round at a size
    pub fn ratios(&self, size: usize, a: usize, b: usize) -> Option<Vec<f64>> {
        Some(stats::log_ratios(self.runs.get(&(a, size))?, self.runs.get(&(b, size))?))
    }

    /// b's fastest run over a's at a size (slowed runs never are a fastest one)
    pub fn fastest_ratio(&self, size: usize, a: usize, b: usize) -> Option<f64> {
        let fastest = |variant: usize| self.runs.get(&(variant, size)).map(|runs| runs.iter().flatten().copied().fold(f64::INFINITY, f64::min)).filter(|ms| ms.is_finite() && *ms > 0.0);
        Some(fastest(b)? / fastest(a)?)
    }

    /// Whether the fastest runs agree with a shift of b over a (`estimate`, ln): b's fastest run is slower than a's by a
    /// third of the shift at least, and by [`HOST_FLOOR`]. Whatever else runs on the machine only slows a run, so a gap in
    /// the code moves both arms' fastest runs as it moves their typical ones, while runs of one arm the machine happened
    /// to slow more move only the typical. Measured (identical variants and planted gaps, 30 comparisons on a loaded
    /// desktop): the fastest runs' ratio read 1.09-1.14 for a 10 % gap and 1.29-1.35 for 30 %; an identical pair the
    /// paired rounds put 10 % apart (8 of 9 rounds) read 1.02 by its fastest runs
    pub fn fastest_agrees(&self, size: usize, a: usize, b: usize, estimate: f64) -> bool {
        self.fastest_ratio(size, a, b).is_some_and(|ratio| ratio - 1.0 >= HOST_FLOOR.max(estimate.exp_m1() / 3.0))
    }

    /// How far the size's A/A pair parted, when it parted beyond the noise (stats::separated): the far end of its shift's
    /// interval (ln), which a verdict at that size must then pass; 0 when it did not part
    pub fn aa_bar(&self, size: usize) -> f64 {
        let Some(d) = self.twins.get(&size).and_then(|twin| Some(stats::log_ratios(self.runs.get(&(0, size))?, twin))) else { return 0.0 };
        if !stats::separated(&d, HOST_FLOOR.ln_1p()) {
            return 0.0;
        }
        stats::shift(&d, 0.95).and_then(|found| found.interval).map_or(0.0, |(low, high)| low.abs().max(high.abs()))
    }
}

/// The paired host-time verdict on b against a at a size: whether b is clearly slower (stats::paired: the sign test, the
/// shift's interval, the A/A noise at that many rounds and [`HOST_FLOOR`], and how far the size's own A/A pair parted when
/// it did; a pair looked at again, both having run in added rounds, clears the later, stricter level), and its fastest
/// round slower too (Clean::fastest_agrees)
fn host_verdict(clean: &Clean, pool: &[f64], timing: &Timing, size: usize, a: usize, b: usize) -> Option<stats::Paired> {
    let d = clean.ratios(size, a, b)?;
    let mut verdict = stats::paired(&d, pool, HOST_FLOOR.ln_1p().max(clean.aa_bar(size)), timing.looked_again(size, a, b));
    verdict.clear &= clean.fastest_agrees(size, a, b, verdict.estimate);
    Some(verdict)
}

/// Whether a run's result holds against the baseline's (its check, or its digest when the file has no check)
fn result_differs(run: &Run) -> bool {
    run.check.as_ref().is_some_and(|(ok, _)| !ok)
}

/// The host-time pairs worth more rounds: at each size, the fastest variant (by its fastest round) against each other one
/// whose verdict is not clear yet but whose gap could matter (over the material floor) and looks real: its shift is over
/// half the A/A noise, or it is a round or two short of the sign test. (size, faster, slower)
fn undecided_pairs(runs: &[Run], timing: &Timing, spec: &Spec) -> Vec<(usize, usize, usize)> {
    let clean = timing.clean(runs, false);
    let pool = clean.pool();
    let floor = ms_floor(spec.per_frame);
    let mut out = Vec::new();
    for size in 0..spec.sizes.len() {
        let fastest = |run: &Run| run.host_ms.iter().flatten().copied().fold(f64::INFINITY, f64::min);
        let candidates: Vec<&Run> = runs.iter().filter(|run| run.size == size && run.status == "ok" && !result_differs(run) && fastest(run).is_finite()).collect();
        let Some(best) = candidates.iter().copied().min_by(|a, b| fastest(a).total_cmp(&fastest(b)).then(a.variant.cmp(&b.variant))) else { continue };
        for other in candidates.iter().filter(|run| run.variant != best.variant) {
            let Some(verdict) = host_verdict(&clean, &pool, timing, size, best.variant, other.variant) else { continue };
            if verdict.clear {
                continue;
            }
            // (the gap either estimate gives: the paired shift, or the fastest rounds' ratio, which disturbed rounds leave
            // standing while they scatter the pairs)
            let fastest_ratio = fastest(other) / fastest(best);
            let gap = fastest(best) * verdict.estimate.exp_m1().max(fastest_ratio - 1.0);
            let needed = stats::wins_needed(verdict.n.max(1), verdict.alpha).unwrap_or(usize::MAX);
            let looks_real = (verdict.estimate > 0.0 && (verdict.noise.is_some_and(|noise| verdict.estimate >= noise * 0.5) || verdict.wins + 2 >= needed)) || fastest_ratio >= 1.0 + HOST_FLOOR;
            if gap >= floor && looks_real {
                out.push((size, best.variant, other.variant));
            }
        }
    }
    out
}

/// The median and the relative interquartile range of some samples
pub fn median_spread(samples: &[f64]) -> Option<(f64, f64)> {
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|value| value.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| -> f64 {
        let position = q * (sorted.len() - 1) as f64;
        let (low, high) = (position.floor() as usize, position.ceil() as usize);
        sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
    };
    let median = at(0.5);
    let spread = if median > 0.0 { (at(0.75) - at(0.25)) / median } else { 0.0 };
    Some((median, spread))
}

/// Which variant leads a metric at one size: (leader, value, runner-up, ratio, vs the baseline), or None for fewer than two
/// figures. Tied leaders lead in the variants' order (the first), the runner-up at ratio 1: a tie names no winner, and
/// every other variant still gets its verdict against the leader. `ratio` is None when the leader's figure is 0 and the
/// runner-up's is not.
pub fn winner(values: &[(usize, f64)], better_higher: bool, baseline: usize) -> Option<(usize, f64, usize, Option<f64>, Option<f64>)> {
    let mut sorted: Vec<(usize, f64)> = values.iter().copied().filter(|(_, value)| value.is_finite()).collect();
    if sorted.len() < 2 {
        return None;
    }
    sorted.sort_by(|a, b| if better_higher { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) }.then(a.0.cmp(&b.0)));
    let (best, runner) = (sorted[0], sorted[1]);
    let ratio = |good: f64, other: f64| -> Option<f64> {
        if better_higher {
            (other > 0.0).then(|| good / other)
        } else {
            (good > 0.0).then(|| other / good)
        }
    };
    let base = values.iter().find(|(variant, _)| *variant == baseline).map(|(_, value)| *value);
    let vs_baseline = base.and_then(|base| ratio(best.1, base));
    Some((best.0, best.1, runner.0, ratio(best.1, runner.1), vs_baseline))
}

/// The leader of host time's ranking: from the fastest round's leader, the variant the paired rounds put furthest ahead
/// of it (`shift(a, b)`: b's paired log ratio over a, below 0 when b is faster), as long as one is ahead (three steps at
/// most: paired shifts need not be transitive). The order the ranking prints then agrees with the ratios it prints
fn paired_leader(values: &[(usize, f64)], start: (usize, f64), shift: impl Fn(usize, usize) -> Option<f64>) -> (usize, f64) {
    let mut leader = start;
    for _ in 0..3 {
        let ahead = values.iter().filter(|(variant, _)| *variant != leader.0).filter_map(|&(variant, figure)| Some((variant, figure, shift(leader.0, variant)?))).filter(|(_, _, estimate)| *estimate < 0.0).min_by(|x, y| x.2.total_cmp(&y.2));
        let Some((variant, figure, _)) = ahead else { break };
        leader = (variant, figure);
    }
    leader
}

/// A number in a few significant digits, with its unit
pub fn show(value: f64, unit: &str) -> String {
    match unit {
        "ms" => {
            let abs = value.abs();
            if abs >= 1000.0 {
                format!("{:.2} s", value / 1000.0)
            } else if abs >= 1.0 {
                format!("{value:.3} ms")
            } else if abs >= 0.001 {
                format!("{:.2} us", value * 1000.0)
            } else {
                format!("{:.0} ns", value * 1e6)
            }
        }
        "bytes" => {
            let abs = value.abs();
            if abs >= 1024.0 * 1024.0 {
                format!("{:.2} MB", value / (1024.0 * 1024.0))
            } else if abs >= 1024.0 {
                format!("{:.1} KB", value / 1024.0)
            } else {
                format!("{value:.0} B")
            }
        }
        "ratio" => {
            if value <= 1.0 && value >= 0.0 { format!("{:.1} %", value * 100.0) } else { format!("{value:.2}") }
        }
        _ => grouped(value),
    }
}

/// A count with thousands separated (one decimal below 10 when it has a fraction)
pub fn grouped(value: f64) -> String {
    if value.fract().abs() > 1e-9 && value.abs() < 10.0 {
        return format!("{value:.1}");
    }
    let whole = value.round() as i64;
    let digits = whole.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if whole < 0 { format!("-{out}") } else { out }
}

fn ratio_text(ratio: Option<f64>) -> String {
    match ratio {
        Some(ratio) if ratio >= 100.0 => format!("{ratio:.0}x"),
        Some(ratio) if ratio >= 10.0 => format!("{ratio:.1}x"),
        Some(ratio) => format!("{ratio:.2}x"),
        None => "only one above 0".into(),
    }
}

/// `ree profile try ...`: the exit code (0: every run finished and every check held; 1: a run failed or a check did not
/// hold, compare.json still written; 2: the command line)
pub fn cli(args: &[String]) -> i32 {
    // checked here as well as in `parse`: a missing cache is an exit 1 without the usage text, as for a run
    if let Err(error) = crate::require_standalone_state(&crate::default_repo()) {
        eprintln!("ree profile try: {error}");
        return 1;
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("ree profile try: {error}\n{USAGE}");
            return 2;
        }
    };
    match compare(&options) {
        Ok((text, ok)) => {
            print!("{text}");
            if ok { 0 } else { 1 }
        }
        Err(error) => {
            eprintln!("ree profile try: {error}");
            1
        }
    }
}

/// Clears what an earlier comparison left in `out` (a folder holding anything else is refused)
fn prepare_out(out: &Path) -> Result<(), String> {
    if out.is_dir() {
        let entries: Vec<String> = std::fs::read_dir(out).map_err(|error| format!("{}: {error}", out.display()))?.filter_map(Result::ok).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        let ours = ["compare.json", "compare.txt", "runs", "diff", "harness", "timed"];
        if !entries.is_empty() && !entries.iter().any(|entry| entry == "compare.json" || entry == "harness") {
            return Err(format!("--out {}: the folder holds files and is not a what-if comparison's", out.display()));
        }
        if let Some(other) = entries.iter().find(|entry| !ours.contains(&entry.as_str())) {
            return Err(format!("--out {}: the folder holds {other}, which a comparison does not write", out.display()));
        }
        for entry in entries {
            let path = out.join(&entry);
            let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            removed.map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    std::fs::create_dir_all(out.join("harness")).map_err(|error| format!("{}: {error}", out.display()))?;
    std::fs::write(out.join("harness").join(format!("{HARNESS_MODULE}.luau")), HARNESS).map_err(|error| format!("{}: {error}", out.display()))
}

/// Runs every job on up to `jobs` threads, each job's result at its index
fn parallel<T: Send, R: Send>(items: Vec<T>, jobs: usize, work: impl Fn(T) -> R + Sync) -> Vec<R> {
    let count = items.len();
    let queue: Mutex<VecDeque<(usize, T)>> = Mutex::new(items.into_iter().enumerate().collect());
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..count).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, count.max(1)) {
            scope.spawn(|| {
                loop {
                    let Some((index, item)) = queue.lock().unwrap().pop_front() else { break };
                    let result = work(item);
                    results.lock().unwrap()[index] = Some(result);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|result| result.expect("every job ran")).collect()
}

/// The comparison: every run, then compare.json and compare.txt in `options.out`; the text and whether it all held
pub fn compare(options: &Options) -> Result<(String, bool), String> {
    let started = Instant::now();
    prepare_out(&options.out)?;
    let spec = list(options)?;
    if spec.variants.is_empty() {
        return Err("no variants to compare".into());
    }
    let (variants, sizes, frames) = (&spec.variants, &spec.sizes, spec.frames as f64);
    // the files of the modules a run's frames name: the variant file, and each version's (standing in for the project's
    // module when the file names one: that module is the run's version)
    let mut paths: ModulePaths = BTreeMap::new();
    if let Some(stem) = options.file.file_stem() {
        paths.insert(format!("{MOUNT}.{}", stem.to_string_lossy()), options.file.to_string_lossy().into_owned());
    }
    let paths_of = |variant: usize| -> ModulePaths {
        let mut paths = paths.clone();
        if let (Some(path), true) = (&variants[variant].path, variants[variant].kind == "module") {
            let module = match &spec.module {
                Some(module) => module.replace(['/', '\\'], "."),
                None => format!("{MOUNT}.Versions.{}", variants[variant].name),
            };
            paths.insert(module, path.to_string_lossy().into_owned());
        }
        paths
    };
    eprintln!("ree profile try: {} variants ({}) at n = {}; profiling {} runs, {} at a time", variants.len(), variants.iter().map(|variant| variant.name.as_str()).collect::<Vec<_>>().join(", "), sizes.iter().map(|size| size_label(*size)).collect::<Vec<_>>().join(", "), variants.len() * sizes.len(), options.jobs);

    // the profiled runs: every variant at every size, in a fresh engine each
    let jobs: Vec<(usize, usize)> = (0..sizes.len()).flat_map(|size| (0..variants.len()).map(move |variant| (variant, size))).collect();
    let mut runs: Vec<Run> = parallel(jobs, options.jobs, |(variant, size)| {
        let mut run = Run { variant, size, ..Run::default() };
        let profile_dir = options.out.join("runs").join(&variants[variant].slug).join(size_label(sizes[size]));
        let mut context = base_context(options, Some(&spec), "profile");
        context["Variant"] = json!(variants[variant].name);
        context["Size"] = json!(sizes[size]);
        let name = format!("profile-{}-{}", variants[variant].slug, size_label(sizes[size]));
        let args = match run_args(&options.out, &name, &context, frames + 4.0) {
            Ok(mut args) => {
                // (the kinds a comparison reads, named: `all` may take in more, such as boundary, that it does not read and
                // that slow every run)
                args.extend(["--profile".into(), PROFILE_KINDS.into(), "--profile-out".into(), profile_dir.to_string_lossy().into_owned()]);
                args
            }
            Err(error) => {
                run.status = "failed".into();
                run.error = Some(error);
                return run;
            }
        };
        let ran = ree_run(options, &args, false);
        if let Some((_, error)) = ran.lines.iter().find(|(kind, _)| kind == "error") {
            run.status = "error".into();
            run.error = Some(format!("{} raised: {}", error["phase"].as_str().unwrap_or("run"), error["message"].as_str().unwrap_or_default()));
        } else if let Some(failure) = ran.failure() {
            run.status = "failed".into();
            run.error = Some(failure);
        } else {
            run.status = "ok".into();
        }
        run.digest = ran.lines.iter().find(|(kind, _)| kind == "result").and_then(|(_, body)| body["digest"].as_str().map(str::to_string));
        if run.status == "ok" {
            match read_profile(&profile_dir, frames, &paths_of(variant)) {
                Ok((setup, figures, heap, tables, hot)) => {
                    run.setup = setup;
                    run.run = figures;
                    run.heap = heap;
                    run.tables = tables;
                    run.hot = hot;
                }
                Err(error) => {
                    run.status = "failed".into();
                    run.error = Some(format!("its profile: {error}"));
                }
            }
        }
        run
    });

    // the check: every variant at every size in one engine, against the baseline's result
    // (`ok` only when something was checked)
    let mut check = json!({"ran": false, "failures": []});
    if options.check && spec.check {
        eprintln!("ree profile try: checking every variant's result against {}'s", variants[0].name);
        let mut context = base_context(options, Some(&spec), "check");
        context["Variants"] = json!(variants.iter().map(|variant| variant.name.clone()).collect::<Vec<_>>());
        context["Sizes"] = json!(sizes);
        let count = (variants.len() * sizes.len()) as f64;
        let ran = run_args(&options.out, "check", &context, count + 8.0).map(|args| ree_run(options, &args, false));
        let mut failures = Vec::new();
        match ran {
            Ok(ran) => {
                for (_, line) in ran.lines.iter().filter(|(kind, _)| kind == "check") {
                    let (Some(name), Some(size)) = (line["variant"].as_str(), line["size"].as_f64()) else { continue };
                    let (Some(variant), Some(size)) = (variants.iter().position(|variant| variant.name == name), sizes.iter().position(|s| *s == size)) else { continue };
                    let ok = line["ok"].as_bool().unwrap_or(false);
                    let message = line["message"].as_str().map(str::to_string);
                    if let Some(run) = runs.iter_mut().find(|run| run.variant == variant && run.size == size) {
                        run.check = Some((ok, message.clone()));
                    }
                    if !ok {
                        failures.push(json!({"variant": name, "size": sizes[size], "message": message}));
                    }
                }
                if let Some(failure) = ran.failure() {
                    failures.push(json!({"variant": Json::Null, "size": Json::Null, "message": failure}));
                }
            }
            Err(error) => failures.push(json!({"variant": Json::Null, "size": Json::Null, "message": error})),
        }
        check = json!({"ran": true, "by": spec.check_kind.clone().unwrap_or_else(|| "function".into()), "ok": failures.is_empty(), "failures": failures});
    } else if options.check {
        // no check function: the results' digests (the result's contents, keys sorted). A variant whose digest differs from
        // the baseline's at a size computes something else there, so it is no other way of writing the same thing
        let mut failures = Vec::new();
        for (size, n) in sizes.iter().enumerate() {
            let Some(base) = runs.iter().find(|run| run.variant == 0 && run.size == size).and_then(|run| run.digest.clone()) else { continue };
            for run in runs.iter_mut().filter(|run| run.size == size && run.variant > 0) {
                let Some(digest) = run.digest.clone() else { continue };
                if digest == base {
                    run.check = Some((true, None));
                } else {
                    let message = format!("its result's digest is {digest}, the baseline's {base}: the results differ (without a check function the results are compared by digest; add check = function(a, b, n) to compare them your way)");
                    failures.push(json!({"variant": variants[run.variant].name, "size": n, "message": message}));
                    run.check = Some((false, Some(message)));
                }
            }
        }
        check = json!({"ran": true, "by": "digest", "ok": failures.is_empty(), "failures": failures});
    }

    // the calls a timed (or counted) run makes at each size: about --time-ms of the slowest variant's warm work clock (nearer
    // this interpreter's speed than the work clock with its cold touches), at least MIN_REPS, inputs that fit; and how many
    // inputs it prepares at a time
    let mut reps: Vec<u64> = Vec::new();
    let mut groups: Vec<u64> = Vec::new();
    for size in 0..sizes.len() {
        let at_size: Vec<&Run> = runs.iter().filter(|run| run.size == size && run.status == "ok").collect();
        let work = at_size.iter().filter_map(|run| run.run.get("warm_ms").or(run.run.get("work_ms")).copied()).fold(0.0f64, f64::max) * frames;
        let held = at_size.iter().filter_map(|run| run.setup.get("heap_bytes").copied()).fold(0.0f64, f64::max).max(1.0);
        let by_time = if work > 0.0 { (options.time_ms / work).ceil() } else { 1000.0 };
        let by_memory = (TIMED_INPUT_BYTES / held).floor().max(1.0);
        reps.push(by_time.max(MIN_REPS).min(by_memory).clamp(1.0, 100_000.0) as u64);
        groups.push((TIMED_GROUP_BYTES / held).floor().clamp(1.0, 100_000.0) as u64);
    }
    // a run of the timing harness: (variant, size), its profile kinds, its folder; the profile folder when it finished
    let timed_run = |variant: usize, size: usize, profile: &str, dir: &Path| -> Result<(), String> {
        let mut context = base_context(options, Some(&spec), "time");
        context["Variant"] = json!(variants[variant].name);
        context["Size"] = json!(sizes[size]);
        context["Reps"] = json!(reps[size]);
        context["Group"] = json!(groups[size]);
        let name = format!("time-{}-{}", variants[variant].slug, size_label(sizes[size]));
        let mut args = run_args(&options.out, &name, &context, 4.0)?;
        args.extend(["--profile".into(), profile.into(), "--profile-out".into(), dir.to_string_lossy().into_owned()]);
        match ree_run(options, &args, true).failure() {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    };
    let timed_dir = options.out.join("timed");

    // the hardware counters: HW_ROUNDS rounds of every variant at every size, each a timed run (grouped calls) with only
    // `hw` on, in a new order each round; the first run is the probe of whether they count here
    let mut hw = json!({"read": false, "why": "--no-hw"});
    if options.hw {
        let arms: Vec<(usize, usize)> = runs.iter().filter(|run| run.status == "ok").map(|run| (run.variant, run.size)).collect();
        let dir_of = |variant: usize, size: usize, round: usize| -> PathBuf {
            if round == 0 {
                options.out.join("runs").join(&variants[variant].slug).join(size_label(sizes[size])).join("hw")
            } else {
                timed_dir.join(format!("hw-{round}")).join(format!("{}-{}", variants[variant].slug, size_label(sizes[size])))
            }
        };
        hw = match arms.first() {
            None => json!({"read": false, "why": "no run finished"}),
            Some(&(variant, size)) => match timed_run(variant, size, "hw", &dir_of(variant, size, 0)).and_then(|()| hw_available(&dir_of(variant, size, 0))) {
                Err(why) => {
                    let _ = std::fs::remove_dir_all(dir_of(variant, size, 0));
                    json!({"read": false, "why": why})
                }
                Ok(()) => {
                    eprintln!("ree profile try: reading the hardware counters, {HW_ROUNDS} rounds of {} runs", arms.len());
                    let mut rates: BTreeMap<(usize, usize), Vec<f64>> = BTreeMap::new();
                    for run in runs.iter_mut() {
                        run.hw_rounds = BTreeMap::new();
                    }
                    for round in 0..HW_ROUNDS {
                        for (variant, size) in shuffled(&arms, 1000 + round as u64) {
                            let dir = dir_of(variant, size, round);
                            // (the probe's run is round 0's for its arm)
                            let probed = round == 0 && (variant, size) == arms[0];
                            if !probed && timed_run(variant, size, "hw", &dir).is_err() {
                                continue;
                            }
                            let Some((figures, rate)) = read_hw(&dir, reps[size] as f64 * frames) else { continue };
                            let Some(run) = runs.iter_mut().find(|run| run.variant == variant && run.size == size) else { continue };
                            rates.entry((variant, size)).or_default().extend(rate);
                            for (key, value) in figures {
                                run.hw_rounds.entry(key).or_insert_with(|| vec![None; HW_ROUNDS])[round] = Some(value);
                            }
                            run.hw_profile |= round == 0;
                        }
                    }
                    for run in runs.iter_mut() {
                        run.hw_cycles_per_ms = rates.get(&(run.variant, run.size)).and_then(|rates| median_spread(rates)).map(|(median, _)| median);
                        for (key, values) in &run.hw_rounds {
                            let values: Vec<f64> = values.iter().flatten().copied().collect();
                            if let Some((median, _)) = median_spread(&values) {
                                run.run.insert(key, median);
                            }
                        }
                    }
                    json!({"read": true, "why": Json::Null, "rounds": HW_ROUNDS})
                }
            },
        };
    }

    // host time: rounds of every variant at every size, in a fresh process each, in a new order each round, the baseline
    // twice (an A/A pair: how far two runs of the same code part); then rounds added for the pairs still undecided
    let mut timing = Timing::default();
    if options.time {
        let mut arms: Vec<(usize, usize, bool)> = Vec::new();
        for size in 0..sizes.len() {
            for variant in 0..variants.len() {
                if runs.iter().any(|run| run.variant == variant && run.size == size && run.status == "ok") {
                    arms.push((variant, size, false));
                    if variant == 0 {
                        arms.push((0, size, true));
                    }
                }
            }
        }
        eprintln!("ree profile try: timing {} rounds of {} runs, one at a time (up to {} rounds for pairs still undecided)", options.rounds, arms.len(), options.max_rounds);
        for run in runs.iter_mut() {
            run.host_ms.clear();
            run.host_cycles.clear();
        }
        // one round of some arms: each round's figure per run, None where the arm did not run or its run failed, so rounds pair up
        let time_round = |runs: &mut Vec<Run>, timing: &mut Timing, which: &[(usize, usize, bool)]| {
            let round = timing.rounds;
            for run in runs.iter_mut() {
                run.host_ms.push(None);
                run.host_cycles.push(None);
            }
            for size in 0..sizes.len() {
                timing.twin_ms.entry(size).or_default().push(None);
                timing.twin_cycles.entry(size).or_default().push(None);
            }
            // (a new order each round, the same every comparison: what ran just before an arm, a run that freed a lot of
            // memory say, moves its time, so no arm keeps one neighbour)
            for (variant, size, second) in shuffled(which, round as u64) {
                let dir = timed_dir.join(format!("{round}")).join(format!("{}-{}{}", variants[variant].slug, size_label(sizes[size]), if second { "-aa" } else { "" }));
                if timed_run(variant, size, "cpu", &dir).is_err() {
                    continue;
                }
                let Ok(profile) = super::diff::load_profile(&dir) else { continue };
                let Some((ms, cycles)) = run_stretches(&profile) else { continue };
                let calls = reps[size] as f64 * frames;
                let (ms, cycles) = (ms / calls, cycles / calls);
                if second {
                    timing.twin_ms.get_mut(&size).unwrap()[round] = Some(ms);
                    timing.twin_cycles.get_mut(&size).unwrap()[round] = Some(cycles);
                } else if let Some(run) = runs.iter_mut().find(|run| run.variant == variant && run.size == size) {
                    run.host_ms[round] = Some(ms);
                    run.host_cycles[round] = Some(cycles);
                    run.reps = reps[size];
                    run.group = groups[size];
                }
            }
            timing.rounds += 1;
        };
        for _ in 0..options.rounds {
            time_round(&mut runs, &mut timing, &arms);
        }
        // rounds added for the pairs not yet decided: a pair whose gap looks larger than the noise, or that is a round or
        // two short of the sign test, gets more rounds, with its size's A/A pair, until it is decided or the rounds run out.
        // Its verdict then clears a stricter level (stats::LATER_ALPHA): a pair looked at again had more chances to cross.
        while timing.rounds < options.max_rounds as usize {
            let undecided = undecided_pairs(&runs, &timing, &spec);
            if undecided.is_empty() {
                break;
            }
            let mut which: Vec<(usize, usize, bool)> = Vec::new();
            for &(size, a, b) in &undecided {
                for arm in [(a, size, false), (b, size, false), (0, size, false), (0, size, true)] {
                    if arms.contains(&arm) && !which.contains(&arm) {
                        which.push(arm);
                    }
                }
                timing.extended.insert(size);
                timing.added_arms.extend([(a, size), (b, size), (0, size)]);
            }
            let adding = ADDED_ROUNDS.min(options.max_rounds as usize - timing.rounds);
            eprintln!(
                "ree profile try: {adding} more rounds of {} runs for {}",
                which.len(),
                undecided.iter().map(|(size, a, b)| format!("{} and {} at n = {}", variants[*a].name, variants[*b].name, size_label(sizes[*size]))).collect::<Vec<_>>().join(", ")
            );
            timing.added.push(json!({
                "after": timing.rounds, "rounds": adding,
                "pairs": undecided.iter().map(|(size, a, b)| json!({"size": sizes[*size], "a": variants[*a].name, "b": variants[*b].name})).collect::<Vec<_>>(),
            }));
            for _ in 0..adding {
                time_round(&mut runs, &mut timing, &which);
            }
        }
        for run in runs.iter_mut() {
            let samples: Vec<f64> = run.host_ms.iter().flatten().copied().collect();
            if let Some((median, _)) = median_spread(&samples) {
                run.run.insert("host_ms", samples.iter().copied().fold(f64::INFINITY, f64::min));
                run.run.insert("host_ms_median", median);
            }
            let cycles: Vec<f64> = run.host_cycles.iter().flatten().copied().collect();
            if !cycles.is_empty() {
                run.run.insert("host_cycles", cycles.iter().copied().fold(f64::INFINITY, f64::min));
            }
        }
        timing.note = json!({
            "rounds": options.rounds, "max_rounds": options.max_rounds, "ran": timing.rounds, "time_ms": options.time_ms,
            "reps": sizes.iter().zip(reps.iter()).map(|(size, reps)| json!([size, reps])).collect::<Vec<_>>(),
            "groups": sizes.iter().zip(groups.iter()).map(|(size, group)| json!([size, group])).collect::<Vec<_>>(),
            "added": timing.added,
        });
    }

    if !options.keep_timed {
        let _ = std::fs::remove_dir_all(&timed_dir);
    }

    // the diffs: the baseline's profile and each variant's at the largest size (and that profile's fidelity, which
    // compare.json carries: how far the work clock's figures may lie from a client's)
    let mut diffs = Vec::new();
    let mut fidelity = Json::Null;
    if let Some(largest) = sizes.len().checked_sub(1) {
        let base_dir = options.out.join("runs").join(&variants[0].slug).join(size_label(sizes[largest]));
        if let Ok(base) = super::diff::load_profile(&base_dir) {
            fidelity = base.get("fidelity").filter(|value| value.is_object()).cloned().unwrap_or(Json::Null);
            for (variant, entry) in variants.iter().enumerate().skip(1) {
                if !runs.iter().any(|run| run.variant == variant && run.size == largest && run.status == "ok") {
                    continue;
                }
                let dir = options.out.join("runs").join(&entry.slug).join(size_label(sizes[largest]));
                let Ok(other) = super::diff::load_profile(&dir) else { continue };
                let out = options.out.join("diff").join(&entry.slug);
                if super::diff::write(&base, &other, 20, &out).1.is_ok() {
                    diffs.push(json!({"variant": entry.name, "size": sizes[largest], "dir": format!("diff/{}", entry.slug)}));
                }
            }
        }
    }

    let (json, text, ok) = index(options, &spec, &runs, check, hw, &timing, diffs, fidelity, started.elapsed().as_secs_f64());
    let written = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())?;
    std::fs::write(options.out.join("compare.json"), written).map_err(|error| format!("{}: {error}", options.out.display()))?;
    std::fs::write(options.out.join("compare.txt"), &text).map_err(|error| format!("{}: {error}", options.out.display()))?;
    Ok((text, ok))
}

fn figures_json(figures: &Figures) -> Json {
    let mut object = serde_json::Map::new();
    for metric in METRICS {
        object.insert(metric.key.to_string(), figures.get(metric.key).map_or(Json::Null, |value| json!(value)));
    }
    Json::Object(object)
}

/// compare.json and compare.txt from the runs
#[allow(clippy::too_many_arguments)]
fn index(options: &Options, spec: &Spec, runs: &[Run], check: Json, hw: Json, timing: &Timing, diffs: Vec<Json>, fidelity: Json, seconds: f64) -> (Json, String, bool) {
    let (variants, sizes) = (&spec.variants, &spec.sizes);
    let run_of = |variant: usize, size: usize| runs.iter().find(|run| run.variant == variant && run.size == size);
    let ok_runs = runs.iter().all(|run| run.status == "ok");
    let check_ok = check["ok"].as_bool() != Some(false);

    // how each figure grows with n: a fixed cost a call fitted apart, the exponent read at large n
    let mut scaling = Vec::new();
    for (variant, entry) in variants.iter().enumerate() {
        for metric in METRICS.iter().filter(|metric| metric.scales) {
            let points: Vec<(f64, f64)> = (0..sizes.len()).filter_map(|size| Some((sizes[size], *run_of(variant, size)?.run.get(metric.key)?))).collect();
            if let Some(mut found) = growth_json(&points) {
                found["variant"] = json!(entry.name);
                found["metric"] = json!(metric.key);
                scaling.push(found);
            }
        }
    }
    // the hot functions' work against n, matched by key
    let mut functions = Vec::new();
    if let Some(largest) = sizes.len().checked_sub(1) {
        for (variant, entry) in variants.iter().enumerate() {
            let Some(top) = run_of(variant, largest) else { continue };
            for hot in top.hot.iter().take(5).filter(|hot| !hot.key.is_empty()) {
                let points: Vec<(f64, f64)> = (0..sizes.len()).filter_map(|size| Some((sizes[size], run_of(variant, size)?.hot.iter().find(|other| other.key == hot.key)?.work_ms))).collect();
                let mut found = growth_json(&points).unwrap_or_else(|| json!({"exponent": null, "points": points.iter().map(|(n, value)| json!([n, value])).collect::<Vec<_>>()}));
                for (key, value) in [("variant", json!(entry.name)), ("key", json!(hot.key)), ("name", json!(hot.name)), ("module", json!(hot.module)), ("line", json!(hot.line))] {
                    found[key] = value;
                }
                functions.push(found);
            }
        }
    }
    // the A/A pairs: how far two runs of the same code part at each size, and whether they parted beyond the noise (a size
    // where they did needs a gap beyond theirs: Clean::aa_bar)
    let cleans = (timing.clean(runs, false), timing.clean(runs, true));
    let pools = (cleans.0.pool(), cleans.1.pool());
    let mut aa = Vec::new();
    let mut parted: BTreeSet<usize> = BTreeSet::new();
    for (size, n) in sizes.iter().enumerate() {
        let (Some(base), Some(twin)) = (runs.iter().find(|run| run.variant == 0 && run.size == size && run.status == "ok"), timing.twin_ms.get(&size)) else { continue };
        let (Some(clean_base), Some(clean_twin)) = (cleans.0.runs.get(&(0, size)), cleans.0.twins.get(&size)) else { continue };
        let d = stats::log_ratios(clean_base, clean_twin);
        let Some(found) = stats::shift(&d, 0.95) else { continue };
        let separated = stats::separated(&d, HOST_FLOOR.ln_1p());
        if separated {
            parted.insert(size);
        }
        // (the rounds both finished, and the pair's fastest rounds and median ratio as before)
        let (first, second): (Vec<f64>, Vec<f64>) = base.host_ms.iter().zip(twin.iter()).filter_map(|(a, b)| Some(((*a)?, (*b)?))).unzip();
        let fastest = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
        let (ratio, spread) = median_spread(&paired(&base.host_ms, twin)).unwrap_or((1.0, 0.0));
        aa.push(json!({
            "size": n, "n": d.len(), "wins": d.iter().filter(|value| **value > 0.0).count(),
            "shift": found.estimate.exp(), "interval": found.interval.map(|(low, high)| json!([low.exp(), high.exp()])),
            "separated": separated, "noise": stats::noise_level(&pools.0, d.len().max(1), stats::NOISE_QUANTILE, stats::NOISE_DRAWS, 0x5eed).map(f64::exp_m1),
            "ratio": ratio, "min_ratio": fastest(&second) / fastest(&first), "spread": spread, "first_ms": first, "second_ms": second,
        }));
    }
    // the noise level two runs of the same code reach at the comparison's rounds, as the verdicts read it
    let noise = json!({
        "pool": pools.0.len(), "quantile": stats::NOISE_QUANTILE, "slowed": stats::SLOWED, "floor": HOST_FLOOR,
        "at_rounds": options.rounds, "level": stats::noise_level(&pools.0, options.rounds as usize, stats::NOISE_QUANTILE, stats::NOISE_DRAWS, 0x5eed).map(f64::exp_m1),
        "alpha": stats::ALPHA, "later_alpha": stats::LATER_ALPHA,
    });
    // the winners. A variant whose result differs from the baseline's (its check, or its digest without one) computes
    // something else, so it is no candidate. Then each other variant against the best: an exact figure clear by 1 % or
    // more; host time by its paired rounds (stats::paired); a hardware counter when it won every round by 5 % or more; any
    // other measured figure by 10 %. A gap that is not material (below the floor: a ms figure's 0.1 % of a frame at the
    // file's per_frame, a count's 0.1 % of the run's instructions, 1 KB) is never clear.
    let floor_ms = ms_floor(spec.per_frame);
    // the reference clock the timed rounds' cycles ran at, from those rounds (cycles over host time, round by round)
    let cycles_per_ms = median_spread(&runs.iter().flat_map(|run| run.host_ms.iter().zip(&run.host_cycles).filter_map(|(ms, cycles)| Some((*cycles)? / (*ms).filter(|ms| *ms > 0.0)?))).collect::<Vec<f64>>()).map(|(median, _)| median);
    let mut winners = Vec::new();
    for size in 0..sizes.len() {
        // (what makes a count or a size material: against the work the run did)
        let insns = (0..variants.len()).filter_map(|variant| run_of(variant, size)?.run.get("insns").copied()).fold(0.0f64, f64::max);
        for metric in METRICS.iter().filter(|metric| metric.better != "none") {
            let all: Vec<(usize, f64)> = (0..variants.len()).filter_map(|variant| Some((variant, *run_of(variant, size).filter(|run| run.status == "ok")?.run.get(metric.key)?))).collect();
            let values: Vec<(usize, f64)> = all.iter().copied().filter(|(variant, _)| run_of(*variant, size).is_some_and(|run| !result_differs(run))).collect();
            let Some((mut best, mut value, _, _, _)) = winner(&values, metric.better == "higher", 0) else { continue };
            let host = matches!(metric.key, "host_ms" | "host_cycles");
            // (the most inline cache lookups any variant made at this size: a hit rate's floor is a share of them)
            let mut lookups = 0.0f64;
            for variant in 0..variants.len() {
                let Some(run) = run_of(variant, size) else { continue };
                if let Some(&variant_lookups) = run.run.get("cache_lookups") {
                    lookups = lookups.max(variant_lookups);
                }
            }
            // (host time is ordered by the paired rounds its verdicts rest on, not by the fastest round alone: a variant whose
            // rounds put it ahead of the fastest round's leader leads, so the order and the ratios printed agree)
            if host {
                let (clean, pool) = if metric.key == "host_cycles" { (&cleans.1, &pools.1) } else { (&cleans.0, &pools.0) };
                (best, value) = paired_leader(&values, (best, value), |a, b| host_verdict(clean, pool, timing, size, a, b).map(|found| found.estimate));
            }
            // (cycles are judged as time at the clock they ran at: host cycles at the timed rounds', the counters' core
            // cycles at the leader's counter runs')
            let floor = Floor { ms: floor_ms, per_frame: spec.per_frame, insns, lookups, cycles_per_ms: if metric.key == "hw_cycles" { run_of(best, size).and_then(|run| run.hw_cycles_per_ms) } else { cycles_per_ms } };
            // b against a (a the best, or the baseline): (b's figure over a's, oriented so above 1 is worse; its interval;
            // clear; material; the rounds) for b worse than a. `both` asks either way (against the baseline)
            let verdict = |a: usize, a_value: f64, b: usize, b_value: f64, both: bool| -> Json {
                let plain = if metric.better == "higher" { (b_value > 0.0).then(|| a_value / b_value) } else { (a_value > 0.0).then(|| b_value / a_value) };
                let (mut ratio, mut interval, mut rounds, mut clear) = (plain, Json::Null, Json::Null, false);
                if host {
                    let (clean, pool) = if metric.key == "host_cycles" { (&cleans.1, &pools.1) } else { (&cleans.0, &pools.0) };
                    let worse = host_verdict(clean, pool, timing, size, a, b);
                    let better = if both { host_verdict(clean, pool, timing, size, b, a) } else { None };
                    if let Some(found) = &worse {
                        ratio = Some(found.estimate.exp());
                        interval = found.interval.map_or(Json::Null, |(low, high)| json!([low.exp(), high.exp()]));
                        let reported = better.as_ref().filter(|better| better.clear).unwrap_or(found);
                        rounds = json!({
                            "won": found.wins, "of": found.n, "p": found.p, "alpha": found.alpha, "noise": found.noise.map(f64::exp_m1),
                            "added": timing.looked_again(size, a, b), "aa_bar": parted.contains(&size).then(|| clean.aa_bar(size).exp_m1()), "faster": better.as_ref().is_some_and(|better| better.clear),
                            "lost": reported.n - reported.wins,
                            // (b's fastest run over a's: a clear verdict needs it to agree with the shift)
                            "fastest": clean.fastest_ratio(size, a, b),
                        });
                        clear = found.clear || better.as_ref().is_some_and(|better| better.clear);
                    }
                }
                // (the gap a call: a host figure's from the paired shift on a's fastest round)
                let gap = match (host, ratio) {
                    (true, Some(ratio)) => (a_value * (ratio - 1.0)).abs(),
                    _ => (b_value - a_value).abs(),
                };
                let material = material(metric, gap, floor);
                if !host {
                    clear = match plain {
                        None => b_value != a_value,
                        Some(ratio) if metric.exact => ratio >= 1.01 || (both && ratio <= 1.0 / 1.01),
                        // (the counters' rounds: every round won, by 5 % or more)
                        Some(ratio) if metric.key.starts_with("hw_") => {
                            let samples = |variant: usize| run_of(variant, size).and_then(|run| run.hw_rounds.get(metric.key).cloned()).unwrap_or_default();
                            let (low, high) = if (metric.better == "higher") == (ratio >= 1.0) { (b, a) } else { (a, b) };
                            let ratios = paired(&samples(low), &samples(high));
                            let won = ratios.iter().filter(|ratio| **ratio > 1.0).count();
                            rounds = json!({"won": won, "of": ratios.len()});
                            won == ratios.len() && won >= 2 && (ratio >= 1.05 || (both && ratio <= 1.0 / 1.05))
                        }
                        Some(ratio) => ratio >= 1.1 || (both && ratio <= 1.0 / 1.1),
                    };
                }
                json!({"ratio": ratio, "interval": interval, "clear": clear && material, "material": material, "gap": gap, "rounds": rounds})
            };
            let value_of = |variant: usize| all.iter().find(|(v, _)| *v == variant).map(|(_, value)| *value);
            let mut ranking: Vec<(usize, f64)> = all.clone();
            ranking.sort_by(|a, b| {
                let differs = |variant: usize| run_of(variant, size).is_some_and(result_differs);
                differs(a.0).cmp(&differs(b.0)).then((a.0 != best).cmp(&(b.0 != best))).then(if metric.better == "higher" { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) }).then(a.0.cmp(&b.0))
            });
            let ranking: Vec<Json> = ranking
                .iter()
                .map(|&(variant, figure)| {
                    let differs = run_of(variant, size).is_some_and(result_differs);
                    let mut entry = if variant == best || differs { json!({"ratio": null, "interval": null, "clear": false, "material": null, "gap": null, "rounds": null}) } else { verdict(best, value, variant, figure, false) };
                    // (the ratio of the figures themselves, for a host figure beside its paired shift)
                    let plain = if metric.better == "higher" { (figure > 0.0).then(|| value / figure) } else { (value > 0.0).then(|| figure / value) };
                    if variant == best {
                        entry["ratio"] = json!(1.0);
                    } else if differs || !host {
                        entry["ratio"] = json!(plain);
                    }
                    entry["value_ratio"] = json!(plain);
                    entry["variant"] = json!(variants[variant].name);
                    entry["value"] = json!(figure);
                    entry["differs"] = json!(differs);
                    entry
                })
                .collect();
            // (host time: the rest by their paired ratio against the leader, the leader first, differing results last)
            let mut ranking = ranking;
            if host {
                let key = |entry: &Json| (entry["differs"] == true, entry["variant"] != variants[best].name.as_str());
                ranking.sort_by(|x, y| key(x).cmp(&key(y)).then(x["ratio"].as_f64().unwrap_or(f64::INFINITY).total_cmp(&y["ratio"].as_f64().unwrap_or(f64::INFINITY))));
            }
            let runner = ranking.iter().skip(1).find(|entry| entry["differs"] != true).and_then(|entry| variants.iter().position(|v| entry["variant"] == v.name.as_str())).unwrap_or(best);
            let vs_baseline = value_of(0).filter(|_| run_of(0, size).is_some_and(|run| !result_differs(run))).and_then(|base| if metric.better == "higher" { (base > 0.0).then(|| value / base) } else { (value > 0.0).then(|| base / value) });
            // each variant against the baseline (the Compare view's column), either way
            let base = value_of(0).filter(|_| run_of(0, size).is_some_and(|run| run.status == "ok"));
            let baseline: Vec<Json> = (1..variants.len())
                .filter_map(|variant| {
                    let (base, figure) = (base?, value_of(variant)?);
                    let mut entry = if run_of(variant, size).is_some_and(result_differs) { json!({"ratio": null, "clear": false, "material": null}) } else { verdict(0, base, variant, figure, true) };
                    entry["variant"] = json!(variants[variant].name);
                    entry["differs"] = json!(run_of(variant, size).is_some_and(result_differs));
                    Some(entry)
                })
                .collect();
            let runner_entry = ranking.iter().find(|entry| entry["variant"] == variants[runner].name.as_str()).cloned().unwrap_or(Json::Null);
            // (a winner is clear of every other variant whose result holds, not of the runner-up alone: host time's paired
            // rounds can leave a third variant within noise of the best where the runner-up's do not)
            let clear_of_all = ranking.iter().filter(|entry| entry["variant"] != variants[best].name.as_str() && entry["differs"] != true).all(|entry| entry["clear"] == true);
            winners.push(json!({
                "metric": metric.key, "size": sizes[size], "winner": variants[best].name, "value": value, "runner_up": variants[runner].name,
                "ratio": runner_entry["ratio"], "interval": runner_entry["interval"], "vs_baseline": vs_baseline,
                "clear": clear_of_all, "material": runner_entry["material"] != false, "rounds": runner_entry["rounds"],
                "ranking": ranking, "baseline": baseline,
            }));
        }
    }
    let mut notes = notes(spec, runs, &hw);
    // how host time was read: runs left out as slowed, A/A pairs that parted, rounds added
    if options.time {
        let (slowed, timed) = (cleans.0.slowed, cleans.0.timed);
        if slowed > 0 {
            notes.push(format!("{slowed} of {timed} timed runs took over {}x what their round and their arm lead one to expect and were left out of the paired rounds the host-time verdicts rest on: the machine slowed them alone, not the code (rounds it slowed as a whole stay: a pair's ratio cancels them)", stats::SLOWED));
        }
        for size in &parted {
            let entry = aa.iter().find(|entry| entry["size"].as_f64() == Some(sizes[*size]));
            notes.push(format!(
                "the A/A pair (the baseline run twice) parted at n = {}: its second run read {} the first's over {} rounds, beyond the noise, so a host-time verdict there needs a gap past {:.1} % (the far end of the pair's own interval); a quieter machine reads smaller gaps",
                size_label(sizes[*size]),
                entry.and_then(|entry| entry["shift"].as_f64()).map_or("?".into(), |shift| ratio_text(Some(shift))),
                entry.and_then(|entry| entry["n"].as_u64()).unwrap_or(0),
                cleans.0.aa_bar(*size).exp_m1() * 100.0
            ));
        }
        if !timing.added.is_empty() {
            let pairs: BTreeSet<String> = timing.added.iter().flat_map(|added| added["pairs"].as_array().cloned().unwrap_or_default()).map(|pair| format!("{} and {} at n = {}", pair["a"].as_str().unwrap_or("?"), pair["b"].as_str().unwrap_or("?"), pair["size"].as_f64().map(size_label).unwrap_or_default())).collect();
            notes.push(format!(
                "host time ran {} rounds, {} more than the first {} for the pairs still undecided ({}); a pair that got more rounds clears its sign test at {} instead of {}, since a pair looked at again had more chances to cross by luck",
                timing.rounds,
                timing.rounds.saturating_sub(options.rounds as usize),
                options.rounds,
                pairs.into_iter().collect::<Vec<_>>().join("; "),
                stats::LATER_ALPHA,
                stats::ALPHA
            ));
        }
    }
    // where host time and the work clock disagree: a clear host-time gap the work clock calls about equal or reverses
    if let Some(largest) = sizes.len().checked_sub(1) {
        let host = winners.iter().find(|winner| winner["metric"] == "host_ms" && winner["size"].as_f64() == Some(sizes[largest]));
        let work_of = |name: &str| -> Option<f64> { runs.iter().find(|run| run.size == largest && variants[run.variant].name == name)?.run.get("work_ms").copied() };
        for entry in host.and_then(|host| host["ranking"].as_array()).into_iter().flatten().skip(1) {
            let (Some(fast), Some(slow)) = (host.and_then(|host| host["winner"].as_str()), entry["variant"].as_str()) else { continue };
            let Some(ratio) = entry["ratio"].as_f64().filter(|ratio| *ratio >= 1.2 && entry["clear"] == true) else { continue };
            let (Some(fast_work), Some(slow_work)) = (work_of(fast), work_of(slow)) else { continue };
            if fast_work <= 0.0 || slow_work / fast_work >= 1.05 {
                continue;
            }
            notes.push(format!(
                "host time puts {slow} {} behind {fast} (slower in {} of {} rounds), where the work clock gives {slow} {}: the clock prices instructions by their kind at a client's measured averages and cannot see all that makes one faster than another (whether a key lies in an array part or a hash part, which loop form runs, whether data is in the CPU's caches), while host time is the Luau VM itself on this machine. Rank this gap by host time, and size it in a client",
                ratio_text(Some(ratio)),
                entry["rounds"]["won"],
                entry["rounds"]["of"],
                if (slow_work / fast_work - 1.0).abs() < 0.01 { "the same figure".to_string() } else { format!("{}", ratio_text(Some(slow_work / fast_work))) }
            ));
        }
    }

    let runs_json: Vec<Json> = runs
        .iter()
        .map(|run| {
            let variant = &variants[run.variant];
            // (each counter's figures, of the rounds whose run finished: `rounds` names them)
            let mut hw: serde_json::Map<String, Json> = run.hw_rounds.iter().map(|(key, values)| (key.to_string(), json!(values.iter().flatten().collect::<Vec<_>>()))).collect();
            if let Some(values) = run.hw_rounds.values().next() {
                hw.insert("rounds".into(), json!(values.iter().enumerate().filter(|(_, value)| value.is_some()).map(|(round, _)| round).collect::<Vec<_>>()));
            }
            json!({
                "variant": variant.name, "size": sizes[run.size], "status": run.status, "error": run.error,
                "profile": format!("runs/{}/{}", variant.slug, size_label(sizes[run.size])),
                "hw_profile": run.hw_profile.then(|| format!("runs/{}/{}/hw", variant.slug, size_label(sizes[run.size]))),
                "digest": run.digest,
                "check": run.check.as_ref().map(|(ok, message)| json!({"ok": ok, "message": message})),
                "setup": figures_json(&run.setup), "run": figures_json(&run.run),
                // (each round's figure, of the rounds whose run finished: `rounds` names them)
                "host": {
                    "ms": run.host_ms.iter().flatten().collect::<Vec<_>>(), "cycles": run.host_cycles.iter().flatten().collect::<Vec<_>>(),
                    "rounds": run.host_ms.iter().enumerate().filter(|(_, ms)| ms.is_some()).map(|(round, _)| round).collect::<Vec<_>>(),
                    "reps": run.reps, "group": run.group,
                },
                "hw": hw,
                "heap": run.heap.map(|(before, set, after)| json!({"before": before, "after_setup": set, "after_run": after})),
                "tables": run.tables,
                "hot": run.hot.iter().map(|hot| json!({"key": hot.key, "name": hot.name, "module": hot.module, "line": hot.line, "work_ms": hot.work_ms, "insns": hot.insns, "calls": hot.calls})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let title = spec.title.clone().unwrap_or_else(|| options.file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
    let json = json!({
        "format": "ree-what-if", "version": 2,
        "title": title, "about": spec.about.clone().unwrap_or_default(), "file": options.file.to_string_lossy(), "out": options.out.to_string_lossy(), "command": options.command,
        "baseline": variants[0].name,
        "variants": variants.iter().map(|variant| json!({"name": variant.name, "slug": variant.slug, "kind": variant.kind, "source": variant.source, "file": variant.path.as_ref().map(|path| path.to_string_lossy().into_owned())})).collect::<Vec<_>>(),
        "sizes": sizes, "frames": spec.frames, "per_frame": spec.per_frame, "module": spec.module,
        "fidelity": crate::cost::FIDELITY,
        // (the baseline's profile's own fidelity at the largest size: the band its figures may lie in against a client's)
        "fidelity_profile": fidelity,
        "floor": {"ms": floor_ms, "per_frame": spec.per_frame, "frame_ms": FRAME_MS, "frame_share": MATERIAL_SHARE, "ns": MATERIAL_NS, "host": HOST_FLOOR,
            "count_share": MATERIAL_COUNT_SHARE, "count": MATERIAL_COUNT, "bytes": MATERIAL_BYTES, "cycles_per_ms": cycles_per_ms},
        "metrics": METRICS.iter().map(|metric| json!({"key": metric.key, "label": metric.label, "unit": metric.unit, "better": metric.better, "exact": metric.exact, "per_call": metric.per_call, "about": metric.about})).collect::<Vec<_>>(),
        "runs": runs_json,
        "scaling": scaling, "functions": functions, "winners": winners, "aa": aa, "noise": noise, "host": timing.note, "hw": hw, "check": check, "diffs": diffs, "notes": notes,
        "seconds": seconds,
    });
    let text = text(options, spec, runs, &json);
    (json, text, ok_runs && check_ok)
}

/// What to keep in mind reading the comparison: where the work clock's model bears on it, failed runs and checks
fn notes(spec: &Spec, runs: &[Run], hw: &Json) -> Vec<String> {
    let (variants, sizes) = (&spec.variants, &spec.sizes);
    let mut notes = Vec::new();
    for run in runs.iter().filter(|run| run.status != "ok") {
        notes.push(format!("{} at n = {}: {} ({})", variants[run.variant].name, size_label(sizes[run.size]), run.status, run.error.as_deref().unwrap_or("no reason given")));
    }
    let Some(largest) = sizes.len().checked_sub(1) else { return notes };
    let at: Vec<&Run> = runs.iter().filter(|run| run.size == largest && run.status == "ok").collect();
    // cold and first-use charges: code run for the first time in a frame and first touches of tables, functions and engine
    // objects; a client that reaches them again soon after a loop pays less, so the work clock's figure is the dear end and
    // the warm figure the cheap one. (The run's cold_tables is the whole phase's, the harness's included, so it is not set
    // beside one variant's charges.)
    let cold: Vec<(&Run, f64, f64)> = at.iter().filter_map(|run| Some((*run, *run.run.get("cold_ms")?, *run.run.get("work_ms")?))).collect();
    if cold.iter().any(|(_, cold_ms, work_ms)| *work_ms > 0.0 && cold_ms / work_ms >= 0.15) {
        let shares: Vec<String> = cold
            .iter()
            .map(|(run, cold_ms, work_ms)| format!("{} {} of {} ({:.0} %)", variants[run.variant].name, show(*cold_ms, "ms"), show(*work_ms, "ms"), 100.0 * cold_ms / work_ms.max(1e-12)))
            .collect();
        let warm: Vec<(usize, f64)> = at.iter().filter_map(|run| Some((run.variant, *run.run.get("warm_ms")?))).collect();
        let full: Vec<(usize, f64)> = at.iter().filter_map(|run| Some((run.variant, *run.run.get("work_ms")?))).collect();
        let ranking = |values: &[(usize, f64)]| {
            winner(values, false, 0).map(|(best, _, runner, ratio, _)| if ratio == Some(1.0) { format!("{} and {} level", variants[best].name, variants[runner].name) } else { format!("{} {} over {}", variants[best].name, ratio_text(ratio), variants[runner].name) })
        };
        let verdict = match (ranking(&full), ranking(&warm)) {
            (Some(full), Some(warm)) if full == warm => format!("the ranking is the same either way ({full})"),
            (Some(full), Some(warm)) => format!("with them {full}; warm {warm}"),
            _ => String::new(),
        };
        notes.push(format!(
            "at n = {} the frame's cold and first-use charges (code run for the first time in the frame, and first touches of tables, functions and engine objects) are a large share of the work clock: {}. A client pays less for code and data a loop reaches again soon after, so a client's figure lies between \"work clock, warm\" and the work clock; {verdict} (docs/engine.md \"Work clock\")",
            size_label(sizes[largest]),
            shares.join("; ")
        ));
    }
    // the collector: its charged work is in the work clock's figure; host time measures it apart
    let steps: Vec<(&Run, f64, f64)> = at.iter().filter_map(|run| Some((*run, *run.run.get("gc_steps")?, run.run.get("gc_ms").copied().unwrap_or(0.0)))).collect();
    let (most_steps, least_steps) = (steps.iter().map(|s| s.1).fold(0.0f64, f64::max), steps.iter().map(|s| s.1).fold(f64::INFINITY, f64::min));
    // rounds that ran several times slower than their fastest: the machine slowed them, not the code
    let slowed = at.iter().filter_map(|run| {
        let samples: Vec<f64> = run.host_ms.iter().flatten().copied().collect();
        let fastest = samples.iter().copied().fold(f64::INFINITY, f64::min);
        let slowest = samples.iter().copied().fold(0.0f64, f64::max);
        (fastest > 0.0 && samples.len() >= 3).then(|| slowest / fastest)
    }).fold(0.0f64, f64::max);
    if slowed >= 2.0 {
        notes.push(format!(
            "some timed rounds at n = {} ran up to {:.1}x slower than the same variant's fastest: something else on the machine slowed them (a shared machine's other work, a scheduler that throttles background processes). Host time is each variant's fastest round, which such slowing does not reach; the hardware counters' cycles count the core's own clock and are steadier",
            size_label(sizes[largest]),
            slowed
        ));
    }
    if most_steps >= 10.0 && most_steps >= least_steps * 2.0 {
        if let Some((run, count, ms)) = steps.iter().find(|s| s.1 == most_steps) {
            notes.push(format!(
                "{} makes the collector work most ({} steps a call at n = {}, {} of host time in the profiled run): the work clock's figure already holds the collector's charged work, and host time measures the collector apart",
                variants[run.variant].name,
                grouped(*count),
                size_label(sizes[largest]),
                show(*ms, "ms")
            ));
        }
    }
    if hw["read"] == false {
        if let Some(why) = hw["why"].as_str().filter(|why| *why != "--no-hw") {
            notes.push(format!("hardware counters not read: {why}"));
        }
    }
    notes
}

/// The text summary (compare.txt, also printed)
fn text(options: &Options, spec: &Spec, runs: &[Run], json: &Json) -> String {
    let (variants, sizes) = (&spec.variants, &spec.sizes);
    let mut out = String::new();
    let name_width = variants.iter().map(|variant| variant.name.chars().count()).max().unwrap_or(4).clamp(4, 28);
    let _ = writeln!(
        out,
        "what-if: {} ({}): {} variants at n = {}; {}",
        json["title"].as_str().unwrap_or_default(),
        options.file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        variants.len(),
        sizes.iter().map(|size| grouped(*size)).collect::<Vec<_>>().join(", "),
        if spec.frames == 1 { "one call".to_string() } else { format!("{} calls, one a frame (figures per call)", spec.frames) }
    );
    if let Some(about) = &spec.about {
        let _ = writeln!(out, "{about}");
    }
    let check = &json["check"];
    let by = match check["by"].as_str() {
        Some("digest") => "by digest: no check function in the file",
        Some("equality") => "by deep equality: check = true",
        _ => "by the file's check function",
    };
    if check["ran"] == true {
        if check["ok"] == true {
            let _ = writeln!(out, "check: every variant's result holds against {}'s at every size ({by})", variants[0].name);
        } else {
            for failure in check["failures"].as_array().into_iter().flatten() {
                let _ = writeln!(out, "check FAILED ({by}): {} at n = {}: {}", failure["variant"].as_str().unwrap_or("?"), failure["size"].as_f64().map(grouped).unwrap_or_else(|| "?".into()), failure["message"].as_str().unwrap_or("?"));
            }
            let _ = writeln!(out, "  a variant whose result differs computes something else: it wins nothing, and its figures are no other way of writing the same work");
        }
    } else {
        let _ = writeln!(out, "results: not checked (--no-check)");
    }
    // the table of figures
    let columns: [(&str, &str); 10] = [("work_ms", "work"), ("warm_ms", "warm work"), ("host_ms", "host"), ("insns", "insns"), ("alloc_bytes", "alloc"), ("allocs", "allocs"), ("heap_bytes", "heap held"), ("gc_steps", "gc steps"), ("rehash_keys", "rehash keys"), ("cache_hit_rate", "cache hit")];
    let shown: Vec<(&str, &str)> = columns.iter().copied().filter(|(key, _)| runs.iter().any(|run| run.run.contains_key(*key))).collect();
    let _ = writeln!(out, "\nrun, per call{}", if spec.frames > 1 { format!(" ({} calls, one a frame; heap held after them all)", spec.frames) } else { String::new() });
    let mut header = format!("{:<name_width$} {:>8}", "variant", "n");
    for (_, label) in &shown {
        let _ = write!(header, " {label:>11}");
    }
    let _ = writeln!(out, "{header}");
    for size in 0..sizes.len() {
        for (variant, entry) in variants.iter().enumerate() {
            let Some(run) = runs.iter().find(|run| run.variant == variant && run.size == size) else { continue };
            let mut line = format!("{:<name_width$} {:>8}", truncate(&entry.name, name_width), grouped(sizes[size]));
            if run.status != "ok" {
                let _ = write!(line, " {}: {}", run.status, run.error.as_deref().unwrap_or_default().lines().next().unwrap_or_default());
                let _ = writeln!(out, "{line}");
                continue;
            }
            for (key, _) in &shown {
                let metric = metric_of(key).unwrap();
                let cell = run.run.get(*key).map_or("-".to_string(), |value| show(*value, metric.unit));
                let _ = write!(line, " {cell:>11}");
            }
            let _ = writeln!(out, "{line}");
        }
    }
    // setup's figures: once when the variants share them
    if let Some(largest) = sizes.len().checked_sub(1) {
        let setups: Vec<(String, f64, f64)> = variants
            .iter()
            .enumerate()
            .filter_map(|(variant, entry)| {
                let run = runs.iter().find(|run| run.variant == variant && run.size == largest)?;
                Some((entry.name.clone(), *run.setup.get("work_ms")?, *run.setup.get("heap_bytes")?))
            })
            .collect();
        let near = |a: f64, b: f64| (a - b).abs() <= 0.005 * a.abs().max(b.abs());
        if setups.len() > 1 && setups.windows(2).all(|pair| near(pair[0].1, pair[1].1) && near(pair[0].2, pair[1].2)) {
            let _ = writeln!(out, "setup at n = {} (the same for every variant, not in the figures above): {} work, {} held", grouped(sizes[largest]), show(setups[0].1, "ms"), show(setups[0].2, "bytes"));
        } else if !setups.is_empty() {
            let _ = writeln!(out, "setup at n = {} (not in the figures above): {}", grouped(sizes[largest]), setups.iter().map(|(name, work, held)| format!("{name} {} work, {} held", show(*work, "ms"), show(*held, "bytes"))).collect::<Vec<_>>().join("; "));
        }
    }
    // scaling
    let scaling = json["scaling"].as_array().cloned().unwrap_or_default();
    if sizes.len() >= 2 && !scaling.is_empty() {
        let fitted = scaling.iter().any(|entry| entry["fit"]["fitted"] == true);
        let _ = writeln!(
            out,
            "\nscaling: the exponent k at large n{} [each segment's slope, n to n]",
            if fitted { ", of a fit to a + b * n^k with a fixed cost a call (a) apart: the growing part's slope between the two largest sizes" } else { ": the slope between the two sizes" }
        );
        for key in ["work_ms", "warm_ms", "host_ms", "allocs", "alloc_bytes", "heap_bytes", "rehash_keys"] {
            let rows: Vec<String> = variants
                .iter()
                .filter_map(|variant| {
                    let entry = scaling.iter().find(|entry| entry["variant"] == variant.name.as_str() && entry["metric"] == key)?;
                    let segments: Vec<String> = entry["segments"].as_array().into_iter().flatten().filter_map(|segment| segment[2].as_f64()).map(|slope| format!("{slope:.2}")).collect();
                    let segments = if segments.len() > 1 { format!(" [{}]", segments.join(" ")) } else { String::new() };
                    Some(format!("{} {:.2}{segments}", variant.name, entry["exponent"].as_f64()?))
                })
                .collect();
            if !rows.is_empty() {
                let _ = writeln!(out, "  {:<17} {}", metric_of(key).unwrap().label, rows.join("   "));
            }
        }
    }
    // winners at the largest size
    if let Some(largest) = sizes.len().checked_sub(1) {
        let winners: Vec<&Json> = json["winners"].as_array().into_iter().flatten().filter(|winner| winner["size"].as_f64() == Some(sizes[largest])).collect();
        let differing: Vec<&str> = runs.iter().filter(|run| run.size == largest && result_differs(run)).map(|run| variants[run.variant].name.as_str()).collect();
        if winners.is_empty() && !differing.is_empty() {
            let _ = writeln!(out, "\nwinners at n = {}: none, as only one variant's result holds ({} computes something else)", grouped(sizes[largest]), differing.join(", "));
        }
        if !winners.is_empty() {
            let _ = writeln!(out, "\nwinners at n = {} (the best where it is clear of every other variant, else no clear winner among those within reach of the best figure; then each other variant's figure over the first)", grouped(sizes[largest]));
            let (mut equal, mut noise, mut negligible) = (Vec::new(), Vec::new(), Vec::new());
            for winner in winners {
                let metric = metric_of(winner["metric"].as_str().unwrap_or_default()).unwrap();
                if matches!(metric.key, "gc_freed" | "cold_ms") {
                    continue;
                }
                let ranking = winner["ranking"].as_array().cloned().unwrap_or_default();
                let others = || ranking.iter().skip(1).filter(|entry| entry["differs"] != true);
                if !ranking.iter().any(|entry| entry["clear"] == true) {
                    // (exact figures within 1 % are about equal, however small; a gap past that under the floor is too small)
                    let level = metric.exact && others().all(|entry| entry["value_ratio"].as_f64().is_some_and(|ratio| ratio < 1.01));
                    if level {
                        equal.push(metric.label);
                    } else if others().all(|entry| entry["material"] == false) && others().next().is_some() {
                        negligible.push(metric.label);
                    } else if metric.exact {
                        equal.push(metric.label);
                    } else {
                        noise.push(metric.label);
                    }
                    continue;
                }
                let best = &ranking[0];
                let best_name = best["variant"].as_str().unwrap_or_default();
                // (a winner only where every other gap is clear; else the variants no clear gap parts from the best figure)
                let leaders: Vec<&str> = std::iter::once(best_name).chain(others().filter(|entry| entry["clear"] != true).filter_map(|entry| entry["variant"].as_str())).collect();
                let best_text = if leaders.len() > 1 {
                    format!("no clear winner among {}", leaders.join(", "))
                } else if best["value"].as_f64() == Some(0.0) {
                    format!("{best_name} (none)")
                } else {
                    best_name.to_string()
                };
                let others: Vec<String> = ranking[1..]
                    .iter()
                    .map(|entry| {
                        let name = entry["variant"].as_str().unwrap_or_default();
                        if entry["differs"] == true {
                            return format!("{name} (its result differs)");
                        }
                        let figure = match entry["ratio"].as_f64() {
                            Some(ratio) => ratio_text(Some(ratio)),
                            None => show(entry["value"].as_f64().unwrap_or(0.0), metric.unit),
                        };
                        let rounds = &entry["rounds"];
                        let interval = entry["interval"].as_array().filter(|_| entry["clear"] == true).map_or(String::new(), |pair| format!(" [{:.2}-{:.2}]", pair[0].as_f64().unwrap_or(0.0), pair[1].as_f64().unwrap_or(0.0)));
                        let flag = match (entry["clear"].as_bool(), rounds["won"].as_u64()) {
                            (Some(true), Some(won)) => format!(" ({won} of {} rounds)", rounds["of"]),
                            _ if entry["material"] == false => " (too small to matter)".into(),
                            (Some(false), _) if metric.exact => " (about equal)".into(),
                            (Some(false), _) if rounds["aa_bar"].is_number() => format!(" (within the A/A pair's own gap here, {:.1} %)", rounds["aa_bar"].as_f64().unwrap_or(0.0) * 100.0),
                            (Some(false), _) => match rounds["noise"].as_f64() {
                                Some(noise) => format!(" (within noise: +-{:.1} % at {} rounds)", noise * 100.0, rounds["of"]),
                                None => " (noise)".into(),
                            },
                            _ => String::new(),
                        };
                        format!("{name} {figure}{interval}{flag}")
                    })
                    .collect();
                let _ = writeln!(out, "  {:<22} {}; {}", metric.label, best_text, others.join(", "));
            }
            if !equal.is_empty() {
                let _ = writeln!(out, "  about equal (within 1 %): {}", equal.join(", "));
            }
            if !noise.is_empty() {
                let _ = writeln!(out, "  within the noise two runs of the same code show (the A/A pair's, at these rounds): {}", noise.join(", "));
            }
            if !negligible.is_empty() {
                let _ = writeln!(out, "  too small to matter: {}", negligible.join(", "));
            }
            let floor = &json["floor"];
            let _ = writeln!(
                out,
                "  (material from: a ms figure's gap of {} a call, 0.1 % of a 60 fps frame at {} a frame (the file's per_frame says how often the game runs the code), {} at least; cycles as the time they take at the clock they ran at, the same; a count's gap a frame of 0.1 % of a call's instructions, 10 at least; a gap of 1 KB a frame; a hit rate's gap over as many lookups)",
                show(floor["ms"].as_f64().unwrap_or(0.0), "ms"),
                if spec.per_frame == 1.0 { "one call".to_string() } else { format!("{} calls", grouped(spec.per_frame)) },
                show(MATERIAL_NS * 1e-6, "ms")
            );
        }
        // the tables after the run, by site: run's (the layout each variant makes), then setup's (its input)
        let largest_runs: Vec<(&Variant, &Run)> = variants.iter().enumerate().filter_map(|(variant, entry)| Some((entry, runs.iter().find(|run| run.variant == variant && run.size == largest && run.status == "ok")?))).collect();
        let _ = writeln!(out, "\ntable shapes at n = {}: the tables each variant's run made, held after it, by the line that made them", grouped(sizes[largest]));
        for (entry, run) in &largest_runs {
            let made = &run.tables["run"];
            let _ = writeln!(out, "  {}: {}, {}", entry.name, tables_text(made["count"].as_f64().unwrap_or(0.0)), show(made["bytes"].as_f64().unwrap_or(0.0), "bytes"));
            for site in run.tables["sites"].as_array().into_iter().flatten().filter(|site| site["phase"] == "run").take(4) {
                let _ = writeln!(out, "{}", site_line(site));
            }
        }
        // setup's: the input (the same for every variant when they share a setup)
        let inputs: Vec<(String, f64, f64)> = largest_runs.iter().map(|(entry, run)| (entry.name.clone(), run.tables["setup"]["count"].as_f64().unwrap_or(0.0), run.tables["setup"]["bytes"].as_f64().unwrap_or(0.0))).collect();
        if inputs.iter().any(|input| input.1 > 0.0) {
            if inputs.windows(2).all(|pair| pair[0].1 == pair[1].1 && (pair[0].2 - pair[1].2).abs() <= 0.005 * pair[0].2.max(pair[1].2)) {
                let _ = writeln!(out, "  the input (setup's, the same for every variant): {}, {}", tables_text(inputs[0].1), show(inputs[0].2, "bytes"));
            } else {
                let _ = writeln!(out, "  setup's tables, held after the run:");
                for (entry, run) in &largest_runs {
                    let _ = writeln!(out, "  {}: {}, {}", entry.name, tables_text(run.tables["setup"]["count"].as_f64().unwrap_or(0.0)), show(run.tables["setup"]["bytes"].as_f64().unwrap_or(0.0), "bytes"));
                    for site in run.tables["sites"].as_array().into_iter().flatten().filter(|site| site["phase"] == "setup").take(3) {
                        let _ = writeln!(out, "{}", site_line(site));
                    }
                }
            }
        }
    }
    let notes = json["notes"].as_array().cloned().unwrap_or_default();
    if !notes.is_empty() {
        let _ = writeln!(out, "\nnotes");
        for note in notes {
            let _ = writeln!(out, "  - {}", note.as_str().unwrap_or_default());
        }
    }
    let _ = writeln!(out, "\nexact (the same every run): work clock, instructions, allocations, heap, collector steps, tables, rehashes, cold touches, cache counts.");
    let host = if options.time {
        let noise = json["noise"]["level"].as_f64().map_or(String::new(), |level| format!(" (two runs of the same code part by up to {:.1} % at {} rounds here)", level * 100.0, options.rounds));
        format!(
            "host time: each variant's fastest of {} interleaved rounds (up to {} for undecided pairs; the median beside it in compare.json); a gap is clear when paired round by round it wins the sign test at {} (8 of 9), the 95 % interval of its shift leaves out none, the shift is past the A/A noise{noise} and {:.0} %, and the fastest runs agree (slower by a third of the shift at least); a run over {}x what its round and its arm lead one to expect is left out",
            options.rounds,
            options.max_rounds,
            stats::ALPHA,
            HOST_FLOOR * 100.0,
            stats::SLOWED
        )
    } else {
        "host time: not timed (--no-time)".to_string()
    };
    let hw = if json["hw"]["read"] == true { format!("hardware counters: median of {} rounds", json["hw"]["rounds"].as_u64().unwrap_or(HW_ROUNDS as u64)) } else { "hardware counters: not read".to_string() };
    let _ = writeln!(out, "measured: {host}; {hw}; the collector's host time.");
    let _ = writeln!(out, "{}", crate::cost::FIDELITY);
    let _ = writeln!(
        out,
        "files in {}: compare.json, compare.txt, runs/<variant>/<n>/ (profiles: ree profile report|diff){} ({:.1} s)",
        options.out.display(),
        if json["diffs"].as_array().is_some_and(|diffs| !diffs.is_empty()) { ", diff/<variant>/ (the baseline's profile against each variant's at the largest n)" } else { "" },
        json["seconds"].as_f64().unwrap_or(0.0)
    );
    out
}

fn tables_text(count: f64) -> String {
    if count == 1.0 { "1 table".into() } else { format!("{} tables", grouped(count)) }
}

/// A table site in one line: how many tables, their parts, nesting, bytes each, rehashes, the line
fn site_line(site: &Json) -> String {
    let count = site["count"].as_f64().unwrap_or(0.0);
    let part = |name: &str, unit: &str| -> String {
        let part = &site[name];
        let (min, max, used) = (part["min"].as_f64().unwrap_or(0.0), part["max"].as_f64().unwrap_or(0.0), part["used"].as_f64().unwrap_or(0.0));
        if max == 0.0 {
            return String::new();
        }
        let size = if min == max { grouped(min) } else { format!("{}-{}", grouped(min), grouped(max)) };
        let used = used / count.max(1.0);
        format!("{name} {size} {unit} ({} used{}), ", grouped(used), if count > 1.0 { " on average" } else { "" })
    };
    let levels = &site["levels"];
    let level = if levels["min"] == levels["max"] { levels["min"].to_string() } else { format!("{}-{}", levels["min"], levels["max"]) };
    let rehash = site["rehashes"].as_f64().filter(|events| *events > 0.0).map_or(String::new(), |events| format!(", {} rehashes moved {} keys", grouped(events), grouped(site["rehash_keys"].as_f64().unwrap_or(0.0))));
    // (a table with neither part is an empty one)
    let empty = if site["array"]["max"].as_f64().unwrap_or(0.0) == 0.0 && site["hash"]["max"].as_f64().unwrap_or(0.0) == 0.0 { "empty, " } else { "" };
    format!(
        "    {:>8} x {}{}{}{} level{}, {} each{}  [{}]",
        grouped(count),
        empty,
        part("array", "slots"),
        part("hash", "nodes"),
        level,
        if level == "1" { "" } else { "s" },
        show(site["bytes"].as_f64().unwrap_or(0.0) / count.max(1.0), "bytes"),
        rehash,
        site["site"].as_str().unwrap_or_default()
    )
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width { text.to_string() } else { text.chars().take(width.saturating_sub(1)).chain(std::iter::once('~')).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_folder_names_and_unique() {
        assert_eq!(slug("t[k][v]", &[]), "t-k-v");
        assert_eq!(slug("t[v][k]", &[]), "t-v-k");
        assert_eq!(slug("Array of Records", &[]), "array-of-records");
        assert_eq!(slug("!!!", &[]), "variant");
        assert_eq!(slug("con", &[]), "con-v");
        assert_eq!(slug("A", &["a".to_string()]), "a-2");
        assert_eq!(slug("a", &["a".to_string(), "a-2".to_string()]), "a-3");
        assert_eq!(size_label(10000.0), "10000");
        assert_eq!(size_label(0.5), "0.5");
    }

    /// compare.json's scaling: a constant overhead a call no longer bends the exponent (a log-log fit without it read a
    /// linear walk as n^0.68); the segments keep the raw slopes
    #[test]
    fn scaling_reads_the_exponent_at_large_n() {
        let overhead: Vec<(f64, f64)> = [10.0, 100.0, 1000.0, 10000.0].iter().map(|&n| (n, 1000.0 + n)).collect();
        let found = growth_json(&overhead).unwrap();
        assert!((found["exponent"].as_f64().unwrap() - 1.0).abs() < 0.02, "{found}");
        assert!((found["fit"]["fixed"].as_f64().unwrap() - 1000.0).abs() < 30.0, "{found}");
        let segments = found["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 3);
        assert!(segments[0][2].as_f64().unwrap() < 0.1 && found["last"].as_f64().unwrap() < 0.97, "{found}");
        let squares: Vec<(f64, f64)> = [10.0, 100.0, 1000.0].iter().map(|&n| (n, n * n + 5.0)).collect();
        assert!((growth_json(&squares).unwrap()["exponent"].as_f64().unwrap() - 2.0).abs() < 0.01);
        assert!(growth_json(&[(10.0, 1.0)]).is_none());
        assert!(growth_json(&[(10.0, 0.0), (100.0, 0.0)]).is_none());
    }

    /// A gap in a ms figure matters from 0.1 % of a 60 fps frame at the file's calls a frame, and 1 ns at least
    #[test]
    fn the_material_floor_follows_the_calls_a_frame() {
        assert!((ms_floor(1.0) - 1000.0 / 60.0 * 0.001).abs() < 1e-12);
        assert!((ms_floor(100.0) - 1000.0 / 60.0 * 0.001 / 100.0).abs() < 1e-12);
        assert_eq!(ms_floor(1e12), 1e-6, "1 ns at least");
    }

    /// A floor at `per_frame` calls a frame, for a call of `insns` instructions and `lookups` inline cache lookups
    fn floor_at(per_frame: f64, insns: f64, lookups: f64, cycles_per_ms: Option<f64>) -> Floor {
        Floor { ms: ms_floor(per_frame), per_frame, insns, lookups, cycles_per_ms }
    }

    /// A gap matters by its own size, never by how large the figures are: heap held 16,048 and 16,432 B (384 B apart) is
    /// too small to matter, where the figures themselves are well past 1 KB; a count likewise, against the run's
    /// instructions; a hit rate by the lookups its gap moves
    #[test]
    fn a_gap_is_material_by_its_own_size() {
        let floor = floor_at(1.0, 1e6, 0.0, None);
        let heap = metric_of("heap_bytes").unwrap();
        assert!(!material(heap, 16_432.0 - 16_048.0, floor), "a 384 B gap");
        assert!(material(heap, 17_500.0 - 16_048.0, floor));
        // (a call of 100,000 instructions: 100 allocations apart matter, 99 do not, however many each made)
        let allocs = metric_of("allocs").unwrap();
        let floor = floor_at(1.0, 1e5, 0.0, None);
        assert!(!material(allocs, 99.0, floor) && material(allocs, 100.0, floor));
        let floor = floor_at(1.0, 1e3, 5_000.0, None);
        assert!(!material(allocs, 9.0, floor) && material(allocs, 10.0, floor), "10 at least");
        // (0.1 % of 5,000 lookups moves 5 of them, under 10; 1 % moves 50)
        let rate = metric_of("cache_hit_rate").unwrap();
        assert!(!material(rate, 0.001, floor) && material(rate, 0.01, floor));
        assert!(material(metric_of("insns").unwrap(), 1.0, floor_at(1.0, 1e9, 0.0, None)), "instructions are the work itself");
        let work = metric_of("work_ms").unwrap();
        let ms = ms_floor(1.0);
        assert!(!material(work, ms * 0.99, floor) && material(work, ms, floor));
    }

    /// One measurement, one verdict: a count or bytes a call matter by what a frame pays, as a ms figure does. At 1,000
    /// calls a frame, a variant allocating 848 B and one collector step more a call (848 KB and 1,000 steps a frame) is
    /// material on every figure, where at one call a frame it is not; and cycles are judged as the time they take at the
    /// clock they ran at, against host time's floor: 41,896 cycles at 4.07 GHz are 10.3 us, under 16.67 us at one call a
    /// frame, as host time's 10.3 us gap is
    #[test]
    fn counts_bytes_and_cycles_are_judged_by_what_a_frame_pays() {
        let (alloc, allocs, steps) = (metric_of("alloc_bytes").unwrap(), metric_of("allocs").unwrap(), metric_of("gc_steps").unwrap());
        let one = floor_at(1.0, 2_400.0, 0.0, None);
        let thousand = floor_at(1000.0, 2_400.0, 0.0, None);
        assert!(!material(alloc, 848.0, one) && !material(allocs, 2.0, one) && !material(steps, 1.0, one));
        assert!(material(alloc, 848.0, thousand) && material(allocs, 2.0, thousand) && material(steps, 1.0, thousand));
        // (the heap a phase holds is no figure a call: per_frame leaves it as it is)
        assert!(!material(metric_of("heap_bytes").unwrap(), 848.0, thousand));
        let (host, cycles) = (metric_of("host_ms").unwrap(), metric_of("host_cycles").unwrap());
        let clock = floor_at(1.0, 2_400.0, 0.0, Some(4_070_000.0));
        assert!(!material(host, 0.0103, clock) && !material(cycles, 41_896.0, clock), "10.3 us under 16.67 us, as time and as cycles");
        let busy = floor_at(1000.0, 2_400.0, 0.0, Some(4_070_000.0));
        assert!(material(host, 0.0103, busy) && material(cycles, 41_896.0, busy), "at 1,000 calls a frame both matter");
        // (the counters' core cycles at their own clock)
        assert!(!material(metric_of("hw_cycles").unwrap(), 41_896.0, clock));
    }

    /// Host time leads with the variant its paired rounds put ahead, not the one with the fastest single round: c's
    /// fastest round is 2 % under a's but a was 6 % faster round by round, so a leads and c reads 1.06x, never "c, a;
    /// a 0.94x"; with no variant ahead the fastest round's leader stays
    #[test]
    fn host_time_leads_with_the_paired_rounds_leader() {
        let values = [(0, 0.102), (1, 0.104), (2, 0.100)];
        let shifts = |a: usize, b: usize| -> Option<f64> {
            let position = |variant: usize| [0.0, 0.03, 0.94f64.ln().abs()][variant];
            Some(position(b) - position(a))
        };
        assert_eq!(paired_leader(&values, (2, 0.100), shifts), (0, 0.102));
        assert!((shifts(0, 2).unwrap().exp() - 1.0 / 0.94).abs() < 1e-12, "c over a, the ratio printed");
        assert_eq!(paired_leader(&values, (0, 0.102), shifts), (0, 0.102));
    }

    #[test]
    fn a_winner_is_clear_of_the_runner_up() {
        // lower is better: 2 beats 3 beats 6
        let values = [(0, 6.0), (1, 2.0), (2, 3.0)];
        let (best, value, runner, ratio, vs_baseline) = winner(&values, false, 0).unwrap();
        assert_eq!((best, value, runner), (1, 2.0, 2));
        assert!((ratio.unwrap() - 1.5).abs() < 1e-12 && (vs_baseline.unwrap() - 3.0).abs() < 1e-12);
        // higher is better
        let (best, _, _, ratio, _) = winner(&[(0, 0.5), (1, 0.9)], true, 0).unwrap();
        assert_eq!(best, 1);
        assert!((ratio.unwrap() - 1.8).abs() < 1e-12);
        // tied leaders lead in the variants' order, the runner-up at ratio 1 (a tie names no winner, and the rest still
        // get their verdicts); nothing above 0 against something is "only one above 0"
        let (best, value, runner, ratio, _) = winner(&[(1, 4.0), (0, 4.0), (2, 6.64)], false, 0).unwrap();
        assert_eq!((best, value, runner, ratio), (0, 4.0, 1, Some(1.0)));
        let (best, _, _, ratio, _) = winner(&[(0, 5.0), (1, 0.0)], false, 0).unwrap();
        assert_eq!((best, ratio), (1, None));
        assert!(winner(&[(0, 1.0)], false, 0).is_none());
    }

    fn run(variant: usize, size: usize, ms: &[f64]) -> Run {
        Run { variant, size, status: "ok".into(), host_ms: ms.iter().map(|value| Some(*value)).collect(), ..Run::default() }
    }

    /// Host time's verdicts pair two variants round by round: slowed runs are left out, identical variants are never
    /// called apart (the audit's case: one called faster at 4-5 % after losing 6 of 9 rounds), and a gap past the A/A
    /// pair's noise in 8 of 9 rounds or more is
    #[test]
    fn host_time_verdicts_pair_rounds_against_the_aa_noise() {
        let noise = [1.00, 1.02, 0.99, 1.03, 0.98, 1.01, 1.00, 0.97, 1.02];
        let base: Vec<f64> = noise.iter().map(|value| value * 0.150).collect();
        let twin: Vec<f64> = noise.iter().rev().map(|value| value * 0.150).collect();
        let mut timing = Timing { rounds: 9, ..Timing::default() };
        timing.twin_ms.insert(0, twin.iter().map(|value| Some(*value)).collect());
        // the same code: losing 6 of 9 rounds, faster by its fastest round
        let same: Vec<f64> = [0.99, 1.03, 1.01, 0.97, 1.02, 1.04, 0.96, 1.01, 1.03].iter().map(|value| value * 0.150).collect();
        // 10 % slower in every round, one round slowed 4x by the machine
        let mut slower: Vec<f64> = noise.iter().map(|value| value * 0.165).collect();
        slower[4] *= 4.0;
        let runs = vec![run(0, 0, &base), run(1, 0, &same), run(2, 0, &slower)];
        let clean = timing.clean(&runs, false);
        let pool = clean.pool();
        assert_eq!(pool.len(), 9);
        assert_eq!((clean.slowed, clean.timed), (1, 36));
        let verdict = host_verdict(&clean, &pool, &timing, 0, 0, 1).unwrap();
        assert!(!verdict.clear, "{verdict:?}");
        let verdict = host_verdict(&clean, &pool, &timing, 0, 0, 2).unwrap();
        assert!(verdict.clear && verdict.n == 8, "the slowed round left out: {verdict:?}");
        assert!((verdict.estimate.exp() - 1.10).abs() < 0.02, "{verdict:?}");
    }

    #[test]
    fn medians_and_spreads() {
        let (median, spread) = median_spread(&[1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
        assert_eq!(median, 3.0);
        assert!((spread - (4.0 - 2.0) / 3.0).abs() < 1e-12);
        assert!(median_spread(&[]).is_none());
        assert_eq!(grouped(1234567.0), "1,234,567");
        assert_eq!(grouped(2.5), "2.5");
        assert_eq!(show(0.0021, "ms"), "2.10 us");
        assert_eq!(show(2048.0, "bytes"), "2.0 KB");
    }

    #[test]
    fn the_variants_own_work_leaves_the_harness_out() {
        // the generated harness's wrapper (its own function), the variant, and a C function both called
        let profile = json!({"functions": [
            {"id": 0, "harness": true, "phases": {"run": {"self_work_ms": 3.0, "harness_self_work_ms": 3.0, "cold_work_ms": 0.5}}},
            {"id": 1, "harness": false, "phases": {"run": {"self_work_ms": 1.0, "harness_self_work_ms": 0.0, "cold_work_ms": 0.2}}},
            {"id": 2, "harness": false, "phases": {"run": {"self_work_ms": 0.5, "harness_self_work_ms": 0.3, "cold_work_ms": 0.15, "harness_cold_work_ms": 0.1}}},
            {"id": 3, "harness": false, "phases": {"setup": {"self_work_ms": 9.0, "harness_self_work_ms": 0.0, "cold_work_ms": 1.0}}},
        ]});
        let (work, cold) = own_work(&profile, "run").expect("the split is there");
        assert!((work - 1.2).abs() < 1e-12, "{work}");
        assert!((cold - 0.25).abs() < 1e-12, "{cold}");
        // (a profile from before the split has no cold per function: the marks' figures stand)
        assert!(own_work(&json!({"functions": [{"id": 0, "phases": {"run": {"self_work_ms": 1.0}}}]}), "run").is_none());
    }

    /// A version 2 profile's marks carry the exact cold charges (`cold_ms`). They decide the cold part of a phase even
    /// where the older VM counts beside them (`cold_tables` and the rest) would have given another figure.
    #[test]
    fn exact_cold_marks_take_precedence_over_legacy_vm_counts() {
        let calls = 2.0;
        let run_work_ms = 1.0;
        let run_cold_ms = 0.25;
        let after_work_ms = 5.0;
        let after_cold_ms = 1.25;
        let profile = json!({"version": 2, "marks": [
            {
                "phase": "run", "work_ms": run_work_ms, "cold_ms": run_cold_ms,
                "vm": {"cold_tables": 0, "cold_closures": 0, "cold_protos": 0, "cold_code": 0}
            },
            {
                "phase": "after", "work_ms": after_work_ms, "cold_ms": after_cold_ms,
                "vm": {"cold_tables": 10, "cold_closures": 2, "cold_protos": 1, "cold_code": 50}
            }
        ]});

        let figures = phase_figures(&profile, "run", "after", calls);

        let cold_ms_a_call = (after_cold_ms - run_cold_ms) / calls;
        let warm_ms_a_call = (after_work_ms - run_work_ms) / calls - cold_ms_a_call;
        assert_eq!(figures["cold_ms"], cold_ms_a_call);
        assert_eq!(figures["warm_ms"], warm_ms_a_call);
    }

    /// A version 2 call tree has one node for each context a function ran in. The hot functions add a function's nodes
    /// together (here 1000 + 2000 us, 100 + 200 instructions, 2 + 3 calls), in the same six-column rows as before, and take
    /// the function's own line, not the calling lines `tree_lines` holds.
    #[test]
    fn hot_functions_sum_context_nodes_without_changing_the_six_column_rows() {
        let profile = json!({
            "version": 2,
            "tree": [
                [-1, -1, 0, 0.0, 0, "startup"],
                [0, -1, 0, 0.0, 0, "run"],
                [1, 7, 2, 1000.0, 100, null],
                [1, 7, 3, 2000.0, 200, null]
            ],
            "tree_lines": [-1, -1, 12, 18],
            "functions": [{"id": 7, "key": "Walker:walk", "name": "walk", "module": "Walker", "line": 4, "kind": "Luau"}]
        });
        let functions = BTreeMap::from([(7, &profile["functions"][0])]);

        let hot = hot_functions(&profile, &functions, 8);

        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].work_ms, 3.0);
        assert_eq!(hot[0].insns, 300.0);
        assert_eq!(hot[0].calls, 5.0);
        assert_eq!(hot[0].line, 4);
    }

    /// A phase's figures are the differences of its marks' counts, flows per call
    #[test]
    fn phase_figures_are_differences_between_marks() {
        let profile = json!({
            "kinds": ["work", "caches", "alloc", "heap"],
            "marks": [
                {"phase": "run", "work_ms": 1.0, "insns": 100, "alloc_bytes": 1000, "allocs": 10, "script_rehashes": 1, "script_rehash_keys": 4,
                 "gc": {"steps": 2, "freed": 64, "ms": 0.5}, "vm": {"cold_tables": 0, "cold_closures": 0, "cold_protos": 0, "cold_code": 0},
                 "caches": {"table key slot": [10, 1, 0, 0, 0, 0], "import": [5, 0, 0, 0]}, "cache_depths": {"table key slot": [0, 0, 0, 0]}},
                {"phase": "after", "work_ms": 5.0, "insns": 500, "alloc_bytes": 3000, "allocs": 30, "script_rehashes": 5, "script_rehash_keys": 24,
                 "gc": {"steps": 6, "freed": 128, "ms": 0.9}, "vm": {"cold_tables": 10, "cold_closures": 2, "cold_protos": 0, "cold_code": 0},
                 "caches": {"table key slot": [40, 3, 2, 0, 1, 2], "import": [9, 0, 0, 0]}, "cache_depths": {"table key slot": [1, 1, 0, 0]}}
            ]
        });
        let figures = phase_figures(&profile, "run", "after", 2.0);
        assert_eq!(figures["work_ms"], 2.0);
        assert_eq!(figures["insns"], 200.0);
        assert_eq!(figures["alloc_bytes"], 1000.0);
        assert_eq!(figures["allocs"], 10.0);
        assert_eq!(figures["rehashes"], 2.0);
        assert_eq!(figures["rehash_keys"], 10.0);
        assert_eq!(figures["gc_steps"], 2.0);
        assert_eq!(figures["cold_tables"], 5.0);
        assert!((figures["cold_ms"] - (10.0 * crate::cost::LUAU_COLD_TABLE + 2.0 * crate::cost::LUAU_COLD_CLOSURE) * 1e-6 / 2.0).abs() < 1e-12);
        // caches: 34 hits of 41 lookups in the phase (2 repredictions, 2 absent reads, a rehash, a walk of one __index table and one
        // of two); the rate is not per call, the uncharged work is: each uncharged outcome by its price (a price within the noise left
        // out), the rehash none (the work clock charges it, in work_ms)
        assert_eq!(figures["cache_hits"], 17.0);
        assert_eq!(figures["cache_lookups"], 20.5);
        assert!((figures["cache_hit_rate"] - 34.0 / 41.0).abs() < 1e-12);
        let clear = |price: Option<crate::profiler::prices::Price>| price.filter(|price| !price.within_noise).map_or(0.0, |price| price.ns);
        let price = |outcome: usize| clear(crate::profiler::prices::outcome(3, outcome, 1.0));
        let walk = |depth: usize| clear(crate::profiler::prices::metatable(3, depth, 1.0));
        assert!(crate::profiler::prices::outcome(3, crate::profiler::vm::REHASHED, 1.0).is_none(), "a rehash is charged, not priced");
        let expected = (2.0 * price(crate::profiler::vm::REPREDICTED) + 2.0 * price(crate::profiler::vm::ABSENT) + walk(1) + walk(2)) / 1e3 / 1e3 / 2.0;
        assert!((figures["cache_uncharged_ms"] - expected).abs() < 1e-12, "{} against {expected}", figures["cache_uncharged_ms"]);
    }

    fn variant(name: &str) -> Variant {
        Variant { name: name.into(), slug: name.into(), kind: "function".into(), source: None, file: None, path: None }
    }

    fn profiled_run(variant: usize, figures: &[(&'static str, f64)]) -> Run {
        Run { variant, size: 0, status: "ok".into(), run: figures.iter().copied().collect(), ..Run::default() }
    }

    /// The review's two probes. A numeric sum reads no table, yet 742 of its 845 ns are cold and first-use charges (its code
    /// run for the first time in the frame); the phase's one cold table is the harness's. The note gives the share and
    /// makes no claim about tables. An allocating variant makes the collector take 137 steps a call, whose charged work is
    /// already in its work figure, and the note says so.
    #[test]
    fn notes_say_what_the_cold_and_collector_figures_hold() {
        let spec = Spec {
            title: None,
            about: None,
            sizes: vec![100.0],
            frames: 60,
            check: false,
            check_kind: None,
            per_frame: 1.0,
            module: None,
            variants: vec![variant("sum"), variant("allocate"), variant("noallocate")],
        };
        let numeric_work_ms = 0.00084509;
        let numeric_cold_ms = 0.0007418;
        let harness_cold_tables = 1.0;
        let collector_steps = 137.0;
        let runs = vec![
            profiled_run(0, &[("work_ms", numeric_work_ms), ("cold_ms", numeric_cold_ms), ("warm_ms", numeric_work_ms - numeric_cold_ms), ("cold_tables", harness_cold_tables)]),
            profiled_run(1, &[("work_ms", 0.328), ("gc_steps", collector_steps), ("gc_ms", 0.12)]),
            profiled_run(2, &[("work_ms", 0.2), ("gc_steps", 0.0), ("gc_ms", 0.0)]),
        ];

        let notes = notes(&spec, &runs, &json!({"read": true}));

        let cold_note = notes.iter().find(|note| note.contains("cold and first-use charges")).expect("a cold share note");
        assert!(cold_note.contains("88 %"), "{cold_note}");
        assert!(!cold_note.contains("1 tables"), "the phase's cold tables are not the variant's: {cold_note}");
        assert!(!cold_note.contains("14 ns") && !cold_note.contains(&format!("{} ns", crate::cost::LUAU_COLD_TABLE)), "no table-layout claim: {cold_note}");
        let collector_note = notes.iter().find(|note| note.contains("makes the collector work most")).expect("a collector note");
        assert!(collector_note.contains("already holds the collector's charged work"), "{collector_note}");
        assert!(!collector_note.contains("does not charge"), "{collector_note}");
    }
}

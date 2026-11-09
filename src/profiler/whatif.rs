//! `ree profile try <file.luau>`: other ways to write the same thing, measured side by side (docs/cli.md "What-if
//! comparisons"). The variant file names the variants, the input sizes and how to check their results; every variant at
//! every size runs in a fresh `ree run` of a generated harness (whatif.luau) under the profiler, host time is read in runs
//! of its own (interleaved rounds with an A/A pair), and `compare.json` indexes what the profiles show: each run's figures
//! by phase, the reachable tables' shapes by the site that made them, how each figure grows with n and which variant wins
//! on each.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::Instant,
};

use serde_json::{Value as Json, json};

/// The harness every run of a comparison runs, written into its folder
pub const HARNESS: &str = include_str!("whatif.luau");

const USAGE: &str = "usage: ree profile try <file.luau> [--out <dir>] [--sizes a,b] [--variants a,b] [--frames <n>] [--rounds <n>]
                       [--time-ms <ms>] [--jobs <n>] [--no-time] [--no-hw] [--no-check] [--keep-timed] [--script-timeout <s>]
                       [engine options: --roots, --project, --place, --mount, ...]   (docs/cli.md \"What-if comparisons\")";

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
    metric("work_ms", "work clock", "ms", "lower", true, true, true, "the work clock: ms of a client's time for the scripts' work (docs/engine.md \"Work clock\")"),
    metric("warm_ms", "work clock, warm", "ms", "lower", true, true, true, "the work clock without the frame's cold first touches: what the same work costs when its data is in the CPU's caches, as in a hot loop"),
    metric("host_ms", "host time", "ms", "lower", false, true, true, "this machine's time a call took in ree's interpreter: the fastest of the timed rounds (whatever else runs on the machine only ever slows a round)"),
    metric("host_ms_median", "host time, median round", "ms", "none", false, false, true, "the median of the timed rounds: above host time by what slowed the rounds"),
    metric("host_cycles", "host cycles", "count", "lower", false, false, true, "the run thread's CPU cycles a call took (reference cycles): the fastest of the timed rounds"),
    metric("insns", "instructions", "count", "lower", true, true, true, "Luau instructions dispatched"),
    metric("alloc_bytes", "allocated", "bytes", "lower", true, true, true, "bytes the VM allocated: objects, table parts, stacks"),
    metric("allocs", "allocations", "count", "lower", true, true, true, "objects the VM allocated: tables, closures, strings, userdata, threads"),
    metric("heap_bytes", "heap held", "bytes", "lower", true, true, false, "the reachable heap at the phase's end beyond its start: what the phase still holds"),
    metric("gc_steps", "collector steps", "count", "lower", true, true, true, "the collector's steps (it paces itself by the scripts' work, so they are the same every run); the work clock does not charge them"),
    metric("gc_freed", "collector freed", "bytes", "lower", true, false, true, "bytes the collector freed"),
    metric("gc_ms", "collector host time", "ms", "none", false, false, true, "host time of the collector's steps in the profiled run (its profilers slow it: a guide, not a measure)"),
    metric("cache_hit_rate", "inline cache hits", "ratio", "higher", true, false, false, "Luau's inline caches: hits of the lookups a table serves (a value another type's metamethod serves is no cache's)"),
    metric("cache_uncharged_ms", "inline caches' uncharged work", "ms", "lower", true, true, true, "what the inline caches' outcomes the work clock does not charge cost over hits at the committed prices (profiler/prices.rs), in the work clock's ms, beyond work_ms: a reprediction, an absent key, an insert with room, a metatable walk by its depth; a rehash and a looked-up import are the clock's own charge, already in work_ms; prices within the noise are left out"),
    metric("cache_lookups", "inline cache lookups", "count", "none", true, false, true, "the lookups a table serves"),
    metric("cache_hits", "inline cache hit count", "count", "none", true, false, true, "inline cache hits"),
    metric("cold_tables", "cold tables", "count", "lower", true, true, true, "tables the frame touched first that were made before it (a client finds them out of its CPU's caches)"),
    metric("cold_ms", "cold touches' work", "ms", "lower", true, false, true, "what the work clock charged for the frame's cold first touches of tables, closures and functions"),
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
    options.file = std::path::absolute(&file).map_err(|error| format!("{file}: {error}"))?;
    if !options.file.is_file() {
        return Err(format!("{file}: no such file"));
    }
    let stem = options.file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "variants".into());
    let out = out.map(PathBuf::from).unwrap_or_else(|| Path::new("what-if").join(&stem));
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
        check: listed["check"].as_bool().unwrap_or(false),
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
        use crate::cost::{LUAU_COLD_CLOSURE, LUAU_COLD_CODE, LUAU_COLD_PROTO, LUAU_COLD_TABLE};
        let cold_ms = (cold_tables * LUAU_COLD_TABLE + cold_closures * LUAU_COLD_CLOSURE + cold_protos * LUAU_COLD_PROTO + cold_code * LUAU_COLD_CODE) * 1e-6;
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

/// A profiled run's figures, read from its profile folder
fn read_profile(dir: &Path, frames: f64, paths: &ModulePaths) -> Result<(Figures, Figures, Option<(f64, f64, f64)>, Json, Vec<Hot>), String> {
    let profile = super::diff::load_profile(dir)?;
    let functions: BTreeMap<i64, &Json> = profile["functions"].as_array().into_iter().flatten().filter_map(|function| Some((function["id"].as_i64()?, function))).collect();
    let setup = phase_figures(&profile, "setup", "run", 1.0);
    let mut run = phase_figures(&profile, "run", "after", frames);
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

/// The hardware counters of a run's hw profile, the run phase per call
fn read_hw(dir: &Path, frames: f64) -> Option<Figures> {
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
    (!figures.is_empty()).then_some(figures)
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

/// Least squares of ln(value) against ln(n) over the positive points: (exponent, r², the exponent between the last two)
pub fn fit(points: &[(f64, f64)]) -> Option<(f64, f64, Option<f64>)> {
    let points: Vec<(f64, f64)> = points.iter().filter(|(n, value)| *n > 0.0 && *value > 0.0).map(|(n, value)| (n.ln(), value.ln())).collect();
    if points.len() < 2 {
        return None;
    }
    let count = points.len() as f64;
    let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / count, points.iter().map(|p| p.1).sum::<f64>() / count);
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let slope = sxy / sxx;
    let syy: f64 = points.iter().map(|p| (p.1 - my).powi(2)).sum();
    let r2 = if syy <= 0.0 { 1.0 } else { (sxy * sxy) / (sxx * syy) };
    let last = points.windows(2).last().map(|pair| (pair[1].1 - pair[0].1) / (pair[1].0 - pair[0].0));
    Some((slope, r2, last))
}

/// Round by round, b's figure over a's (the rounds both have)
pub fn paired(a: &[Option<f64>], b: &[Option<f64>]) -> Vec<f64> {
    a.iter().zip(b.iter()).filter_map(|(a, b)| Some((a.filter(|a| *a > 0.0)?, (*b)?))).map(|(a, b)| b / a).collect()
}

/// Whether a host-time winner is clear of another variant: its fastest round is faster by more than the A/A pair's fastest
/// rounds differ (two runs of the same code: `aa`, their ratio), and by 3 % at least, and its median round is faster too.
/// The fastest round is the figure because whatever else runs on the machine only ever slows a round: on a shared machine
/// a scheduler can slow whole stretches of rounds several times over, so that the rounds' medians, and the rounds won pair
/// by pair, can follow the stretches instead of the code. `min_ratio` and `median_ratio` are the other's figure over the
/// winner's.
pub fn host_clear(min_ratio: f64, median_ratio: f64, aa: Option<f64>) -> bool {
    let noise = aa.map_or(0.0, |aa| (aa - 1.0).abs()).max(0.03);
    min_ratio >= 1.0 + noise && median_ratio > 1.0
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

/// Which variant wins a metric at one size: (winner, value, runner-up, ratio, vs the baseline) or None for a tie or fewer
/// than two figures. `ratio` is None when the winner's figure is 0 and the runner-up's is not.
pub fn winner(values: &[(usize, f64)], better_higher: bool, baseline: usize) -> Option<(usize, f64, usize, Option<f64>, Option<f64>)> {
    let mut sorted: Vec<(usize, f64)> = values.iter().copied().filter(|(_, value)| value.is_finite()).collect();
    if sorted.len() < 2 {
        return None;
    }
    sorted.sort_by(|a, b| if better_higher { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) }.then(a.0.cmp(&b.0)));
    let (best, runner) = (sorted[0], sorted[1]);
    if best.1 == runner.1 {
        return None;
    }
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
                // (the kinds `all` holds but cpu: the timed runs take host time and cycles, `--profile cpu` alone, so this run needs
                // no cpu pass of its own)
                args.extend(["--profile".into(), "work,caches,alloc,heap,timeline,phases".into(), "--profile-out".into(), profile_dir.to_string_lossy().into_owned()]);
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
        check = json!({"ran": true, "ok": failures.is_empty(), "failures": failures});
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
                            let Some(figures) = read_hw(&dir, reps[size] as f64 * frames) else { continue };
                            let Some(run) = runs.iter_mut().find(|run| run.variant == variant && run.size == size) else { continue };
                            for (key, value) in figures {
                                run.hw_rounds.entry(key).or_insert_with(|| vec![None; HW_ROUNDS])[round] = Some(value);
                            }
                            run.hw_profile |= round == 0;
                        }
                    }
                    for run in runs.iter_mut() {
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

    // host time: rounds of every variant at every size in turn, in a fresh process each, the baseline twice (an A/A pair)
    let mut aa: Vec<Json> = Vec::new();
    let mut host_note = Json::Null;
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
        eprintln!("ree profile try: timing {} rounds of {} runs, one at a time", options.rounds, arms.len());
        let rounds = options.rounds as usize;
        // (each round's figure per run, None where the round's run failed, so that rounds pair up)
        for run in runs.iter_mut() {
            run.host_ms = vec![None; rounds];
            run.host_cycles = vec![None; rounds];
        }
        let mut twin: BTreeMap<usize, Vec<Option<f64>>> = BTreeMap::new();
        for round in 0..rounds {
            // (a new order each round, the same every comparison: what ran just before an arm, a run that freed a lot of
            // memory say, moves its time, so no arm keeps one neighbour)
            for (variant, size, second) in shuffled(&arms, round as u64) {
                let dir = timed_dir.join(format!("{round}")).join(format!("{}-{}{}", variants[variant].slug, size_label(sizes[size]), if second { "-aa" } else { "" }));
                if timed_run(variant, size, "cpu", &dir).is_err() {
                    continue;
                }
                let Ok(profile) = super::diff::load_profile(&dir) else { continue };
                let Some((ms, cycles)) = run_stretches(&profile) else { continue };
                let calls = reps[size] as f64 * frames;
                let (ms, cycles) = (ms / calls, cycles / calls);
                if second {
                    twin.entry(size).or_insert_with(|| vec![None; rounds])[round] = Some(ms);
                } else if let Some(run) = runs.iter_mut().find(|run| run.variant == variant && run.size == size) {
                    run.host_ms[round] = Some(ms);
                    run.host_cycles[round] = Some(cycles);
                    run.reps = reps[size];
                    run.group = groups[size];
                }
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
        for size in 0..sizes.len() {
            let Some(base) = runs.iter().find(|run| run.variant == 0 && run.size == size) else { continue };
            let Some(second) = twin.get(&size) else { continue };
            // (round by round: the second run of the baseline over the first)
            let ratios = paired(&base.host_ms, second);
            if let Some((ratio, spread)) = median_spread(&ratios) {
                // (the rounds where both runs of the pair finished)
                let (first, twin): (Vec<f64>, Vec<f64>) = base.host_ms.iter().zip(second.iter()).filter_map(|(a, b)| Some(((*a)?, (*b)?))).unzip();
                // (the pair's fastest rounds, as host time is read)
                let fastest = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
                let min_ratio = fastest(&twin) / fastest(&first);
                aa.push(json!({"size": sizes[size], "ratio": ratio, "min_ratio": min_ratio, "spread": spread, "first_ms": first, "second_ms": twin}));
            }
        }
        host_note = json!({"rounds": options.rounds, "time_ms": options.time_ms, "reps": sizes.iter().zip(reps.iter()).map(|(size, reps)| json!([size, reps])).collect::<Vec<_>>(), "groups": sizes.iter().zip(groups.iter()).map(|(size, group)| json!([size, group])).collect::<Vec<_>>()});
    }

    if !options.keep_timed {
        let _ = std::fs::remove_dir_all(&timed_dir);
    }

    // the diffs: the baseline's profile and each variant's at the largest size
    let mut diffs = Vec::new();
    if let Some(largest) = sizes.len().checked_sub(1) {
        let base_dir = options.out.join("runs").join(&variants[0].slug).join(size_label(sizes[largest]));
        if let Ok(base) = super::diff::load_profile(&base_dir) {
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

    let (json, text, ok) = index(options, &spec, &runs, check, hw, aa, host_note, diffs, started.elapsed().as_secs_f64());
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
fn index(options: &Options, spec: &Spec, runs: &[Run], check: Json, hw: Json, aa: Vec<Json>, host: Json, diffs: Vec<Json>, seconds: f64) -> (Json, String, bool) {
    let (variants, sizes) = (&spec.variants, &spec.sizes);
    let run_of = |variant: usize, size: usize| runs.iter().find(|run| run.variant == variant && run.size == size);
    let ok_runs = runs.iter().all(|run| run.status == "ok");
    let check_ok = check["ok"].as_bool() != Some(false);

    // how each figure grows with n
    let mut scaling = Vec::new();
    for (variant, entry) in variants.iter().enumerate() {
        for metric in METRICS.iter().filter(|metric| metric.scales) {
            let points: Vec<(f64, f64)> = (0..sizes.len()).filter_map(|size| Some((sizes[size], *run_of(variant, size)?.run.get(metric.key)?))).collect();
            if let Some((exponent, r2, last)) = fit(&points) {
                scaling.push(json!({"variant": entry.name, "metric": metric.key, "exponent": exponent, "r2": r2, "last": last, "points": points.iter().map(|(n, value)| json!([n, value])).collect::<Vec<_>>()}));
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
                let fitted = fit(&points);
                functions.push(json!({"variant": entry.name, "key": hot.key, "name": hot.name, "module": hot.module, "line": hot.line, "exponent": fitted.map(|f| f.0), "r2": fitted.map(|f| f.1), "points": points.iter().map(|(n, value)| json!([n, value])).collect::<Vec<_>>()}));
            }
        }
    }
    // the winners: an exact figure by 1 % or more; host time when it won the rounds (clear_of_noise); a figure of one run
    // each (the hardware counters) by 10 % or more
    let aa_at = |size: usize| aa.iter().find(|entry| entry["size"].as_f64() == Some(sizes[size])).and_then(|entry| entry["min_ratio"].as_f64());
    let mut winners = Vec::new();
    for size in 0..sizes.len() {
        // (what makes a count or a size material: against the work the run did)
        let insns = (0..variants.len()).filter_map(|variant| run_of(variant, size)?.run.get("insns").copied()).fold(0.0f64, f64::max);
        for metric in METRICS.iter().filter(|metric| metric.better != "none") {
            let values: Vec<(usize, f64)> = (0..variants.len()).filter_map(|variant| Some((variant, *run_of(variant, size).filter(|run| run.status == "ok")?.run.get(metric.key)?))).collect();
            let Some((best, value, runner, ratio, vs_baseline)) = winner(&values, metric.better == "higher", 0) else { continue };
            // whether a variant's figure is clear of the best's: (clear, the rounds it lost of those paired)
            let samples = |variant: usize| run_of(variant, size).map(|run| if metric.key == "host_ms" { run.host_ms.clone() } else { run.host_cycles.clone() }).unwrap_or_default();
            let largest = values.iter().map(|(_, value)| value.abs()).fold(0.0f64, f64::max);
            let lookups = (0..variants.len()).filter_map(|variant| run_of(variant, size)?.run.get("cache_lookups").copied()).fold(0.0f64, f64::max);
            let material = match metric.unit {
                "count" if metric.key != "insns" => largest >= (insns * 0.001).max(10.0),
                "bytes" => largest >= 1024.0,
                _ if metric.key == "cache_hit_rate" => lookups >= (insns * 0.001).max(10.0),
                _ => true,
            };
            let gap = |other: usize, against: f64| -> (bool, Json) {
                let ratio = if metric.better == "higher" { (against > 0.0).then(|| value / against) } else { (value > 0.0).then(|| against / value) };
                match ratio {
                    _ if !material => (false, Json::Null),
                    None => (true, Json::Null),
                    Some(ratio) if metric.exact => (ratio >= 1.01, Json::Null),
                    Some(ratio) if matches!(metric.key, "host_ms" | "host_cycles") => {
                        let ratios = paired(&samples(best), &samples(other));
                        let won = ratios.iter().filter(|ratio| **ratio > 1.0).count();
                        let median = |variant: usize| median_spread(&samples(variant).into_iter().flatten().collect::<Vec<_>>()).map(|(median, _)| median);
                        let median_ratio = match (median(best), median(other)) {
                            (Some(best), Some(other)) if best > 0.0 => other / best,
                            _ => 0.0,
                        };
                        (host_clear(ratio, median_ratio, aa_at(size)), json!({"won": won, "of": ratios.len()}))
                    }
                    // (the counters' rounds: every round won, by 5 % or more)
                    Some(ratio) if metric.key.starts_with("hw_") => {
                        let rounds = |variant: usize| run_of(variant, size).and_then(|run| run.hw_rounds.get(metric.key).cloned()).unwrap_or_default();
                        let ratios: Vec<f64> = if metric.better == "higher" { paired(&rounds(other), &rounds(best)) } else { paired(&rounds(best), &rounds(other)) };
                        let won = ratios.iter().filter(|ratio| **ratio > 1.0).count();
                        (won == ratios.len() && won >= 2 && ratio >= 1.05, json!({"won": won, "of": ratios.len()}))
                    }
                    Some(ratio) => (ratio >= 1.1, Json::Null),
                }
            };
            let (clear, rounds) = gap(runner, values.iter().find(|(variant, _)| *variant == runner).map_or(value, |(_, v)| *v));
            let mut ranking: Vec<(usize, f64)> = values.clone();
            ranking.sort_by(|a, b| if metric.better == "higher" { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) }.then(a.0.cmp(&b.0)));
            let ranking: Vec<Json> = ranking
                .iter()
                .map(|&(variant, figure)| {
                    let (clear, rounds) = if variant == best { (false, Json::Null) } else { gap(variant, figure) };
                    let ratio = if metric.better == "higher" { (figure > 0.0).then(|| value / figure) } else { (value > 0.0).then(|| figure / value) };
                    json!({"variant": variants[variant].name, "value": figure, "ratio": ratio, "clear": clear, "rounds": rounds})
                })
                .collect();
            winners.push(json!({
                "metric": metric.key, "size": sizes[size], "winner": variants[best].name, "value": value, "runner_up": variants[runner].name,
                "ratio": ratio, "vs_baseline": vs_baseline, "clear": clear, "material": material, "rounds": rounds, "ranking": ranking,
            }));
        }
    }
    let mut notes = notes(spec, runs, &hw);
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
        "format": "ree-what-if", "version": 1,
        "title": title, "about": spec.about.clone().unwrap_or_default(), "file": options.file.to_string_lossy(), "out": options.out.to_string_lossy(), "command": options.command,
        "baseline": variants[0].name,
        "variants": variants.iter().map(|variant| json!({"name": variant.name, "slug": variant.slug, "kind": variant.kind, "source": variant.source, "file": variant.path.as_ref().map(|path| path.to_string_lossy().into_owned())})).collect::<Vec<_>>(),
        "sizes": sizes, "frames": spec.frames, "module": spec.module,
        "fidelity": crate::cost::FIDELITY,
        "metrics": METRICS.iter().map(|metric| json!({"key": metric.key, "label": metric.label, "unit": metric.unit, "better": metric.better, "exact": metric.exact, "per_call": metric.per_call, "about": metric.about})).collect::<Vec<_>>(),
        "runs": runs_json,
        "scaling": scaling, "functions": functions, "winners": winners, "aa": aa, "host": host, "hw": hw, "check": check, "diffs": diffs, "notes": notes,
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
    // cold first touches: priced as data made over many frames, spread among other allocations; data made together in one
    // loop (a setup's) costs a client far less, so the work clock's figure is the dear end and the warm figure the cheap one
    let cold: Vec<(&Run, f64, f64, f64)> = at.iter().filter_map(|run| Some((*run, *run.run.get("cold_ms")?, *run.run.get("work_ms")?, *run.run.get("cold_tables")?))).collect();
    if cold.iter().any(|(_, cold_ms, work_ms, _)| *work_ms > 0.0 && cold_ms / work_ms >= 0.15) {
        let shares: Vec<String> = cold
            .iter()
            .map(|(run, cold_ms, work_ms, tables)| format!("{} {} of {} ({} tables, {:.0} %)", variants[run.variant].name, show(*cold_ms, "ms"), show(*work_ms, "ms"), grouped(*tables), 100.0 * cold_ms / work_ms.max(1e-12)))
            .collect();
        let warm: Vec<(usize, f64)> = at.iter().filter_map(|run| Some((run.variant, *run.run.get("warm_ms")?))).collect();
        let full: Vec<(usize, f64)> = at.iter().filter_map(|run| Some((run.variant, *run.run.get("work_ms")?))).collect();
        let ranking = |values: &[(usize, f64)]| winner(values, false, 0).map(|(best, _, runner, ratio, _)| format!("{} {} over {}", variants[best].name, ratio_text(ratio), variants[runner].name));
        let verdict = match (ranking(&full), ranking(&warm)) {
            (Some(full), Some(warm)) if full == warm => format!("the ranking is the same either way ({full})"),
            (Some(full), Some(warm)) => format!("with them {full}; warm {warm}"),
            _ => String::new(),
        };
        notes.push(format!(
            "at n = {} the frame's cold first touches of data made before it are a large share of the work clock: {}. The clock prices a cold table at {} ns, as data made over many frames among other allocations; a client pays about 14 ns for tables made together in one loop (as a setup makes them), so a client's figure lies between \"work clock, warm\" and the work clock; {verdict} (docs/engine.md \"Work clock\")",
            size_label(sizes[largest]),
            shares.join("; "),
            crate::cost::LUAU_COLD_TABLE
        ));
    }
    // the collector: the work clock does not charge it
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
                "{} makes the collector work most ({} steps a call at n = {}, {} of host time in the profiled run): the work clock does not charge the collector, host time does",
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
    if check["ran"] == true {
        if check["ok"] == true {
            let _ = writeln!(out, "check: every variant's result holds against {}'s at every size", variants[0].name);
        } else {
            for failure in check["failures"].as_array().into_iter().flatten() {
                let _ = writeln!(out, "check FAILED: {} at n = {}: {}", failure["variant"].as_str().unwrap_or("?"), failure["size"].as_f64().map(grouped).unwrap_or_else(|| "?".into()), failure["message"].as_str().unwrap_or("?"));
            }
        }
    } else {
        // the results' digests at least
        let mut differ = Vec::new();
        for size in 0..sizes.len() {
            let base = runs.iter().find(|run| run.variant == 0 && run.size == size).and_then(|run| run.digest.clone());
            for run in runs.iter().filter(|run| run.size == size && run.variant > 0) {
                if run.digest.is_some() && base.is_some() && run.digest != base {
                    differ.push(format!("{} at n = {}", variants[run.variant].name, grouped(sizes[size])));
                }
            }
        }
        if differ.is_empty() {
            let _ = writeln!(out, "results: every variant's result has the baseline's digest (no check function in the file)");
        } else {
            let _ = writeln!(out, "results: these differ from {}'s (by digest; no check function in the file): {}", variants[0].name, differ.join(", "));
        }
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
        let _ = writeln!(out, "\nscaling: the exponent k of a fit of the figure to c * n^k (between the two largest sizes)");
        for key in ["work_ms", "warm_ms", "host_ms", "allocs", "alloc_bytes", "heap_bytes", "rehash_keys"] {
            let rows: Vec<String> = variants
                .iter()
                .filter_map(|variant| {
                    let entry = scaling.iter().find(|entry| entry["variant"] == variant.name.as_str() && entry["metric"] == key)?;
                    Some(format!("{} {:.2} ({})", variant.name, entry["exponent"].as_f64()?, entry["last"].as_f64().map_or("-".into(), |last| format!("{last:.2}"))))
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
        if !winners.is_empty() {
            let _ = writeln!(out, "\nwinners at n = {} (the best, then each other variant's figure over it)", grouped(sizes[largest]));
            let (mut equal, mut noise, mut negligible) = (Vec::new(), Vec::new(), Vec::new());
            for winner in winners {
                let metric = metric_of(winner["metric"].as_str().unwrap_or_default()).unwrap();
                if matches!(metric.key, "gc_freed" | "cold_ms") {
                    continue;
                }
                let ranking = winner["ranking"].as_array().cloned().unwrap_or_default();
                if winner["material"] == false {
                    negligible.push(metric.label);
                    continue;
                }
                if !ranking.iter().any(|entry| entry["clear"] == true) {
                    if metric.exact { equal.push(metric.label) } else { noise.push(metric.label) }
                    continue;
                }
                let best = &ranking[0];
                let best_text = if best["value"].as_f64() == Some(0.0) { format!("{} (none)", best["variant"].as_str().unwrap_or_default()) } else { best["variant"].as_str().unwrap_or_default().to_string() };
                let others: Vec<String> = ranking[1..]
                    .iter()
                    .map(|entry| {
                        let name = entry["variant"].as_str().unwrap_or_default();
                        let figure = match entry["ratio"].as_f64() {
                            Some(ratio) => ratio_text(Some(ratio)),
                            None => show(entry["value"].as_f64().unwrap_or(0.0), metric.unit),
                        };
                        let rounds = &entry["rounds"];
                        let flag = match (entry["clear"].as_bool(), rounds["won"].as_u64()) {
                            (Some(true), Some(won)) => format!(" ({won} of {} rounds)", rounds["of"]),
                            (Some(false), _) if metric.exact => " (about equal)".into(),
                            (Some(false), _) => " (noise)".into(),
                            _ => String::new(),
                        };
                        format!("{name} {figure}{flag}")
                    })
                    .collect();
                let _ = writeln!(out, "  {:<22} {}; {}", metric.label, best_text, others.join(", "));
            }
            if !equal.is_empty() {
                let _ = writeln!(out, "  about equal (within 1 %): {}", equal.join(", "));
            }
            if !noise.is_empty() {
                let _ = writeln!(out, "  within the noise of the A/A pair: {}", noise.join(", "));
            }
            if !negligible.is_empty() {
                let _ = writeln!(out, "  too few to matter beside the run's work: {}", negligible.join(", "));
            }
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
        let aa = json["aa"].as_array().and_then(|aa| aa.last()).map_or(String::new(), |entry| format!(", A/A {:.3} at n = {}", entry["min_ratio"].as_f64().unwrap_or(1.0), grouped(entry["size"].as_f64().unwrap_or(0.0))));
        format!("host time: the fastest of {} interleaved rounds (the median beside it in compare.json){aa}", options.rounds)
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

    #[test]
    fn a_fit_finds_the_exponent_of_n() {
        let linear: Vec<(f64, f64)> = [10.0, 100.0, 1000.0, 10000.0].iter().map(|&n| (n, 3.0 * n)).collect();
        let (k, r2, last) = fit(&linear).unwrap();
        assert!((k - 1.0).abs() < 1e-9 && (r2 - 1.0).abs() < 1e-9 && (last.unwrap() - 1.0).abs() < 1e-9);
        let squares: Vec<(f64, f64)> = [10.0, 100.0, 1000.0].iter().map(|&n| (n, n * n + 5.0)).collect();
        let (k, _, last) = fit(&squares).unwrap();
        assert!(k > 1.9 && k < 2.0 && last.unwrap() > 1.99, "{k} {last:?}");
        // (a constant overhead at small n bends the fit; the last pair shows the trend)
        let overhead: Vec<(f64, f64)> = [10.0, 100.0, 1000.0, 10000.0].iter().map(|&n| (n, 1000.0 + n)).collect();
        let (k, _, last) = fit(&overhead).unwrap();
        assert!(k < 0.5 && last.unwrap() > 0.7 && last.unwrap() > k, "{k} {last:?}");
        assert!(fit(&[(10.0, 1.0)]).is_none());
        assert!(fit(&[(10.0, 0.0), (100.0, 0.0)]).is_none());
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
        // a tie has no winner; nothing above 0 against something is "only one above 0"
        assert!(winner(&[(0, 4.0), (1, 4.0)], false, 0).is_none());
        let (best, _, _, ratio, _) = winner(&[(0, 5.0), (1, 0.0)], false, 0).unwrap();
        assert_eq!((best, ratio), (1, None));
        assert!(winner(&[(0, 1.0)], false, 0).is_none());
    }

    /// Host time ranks by the fastest round: clear only beyond the A/A pair's own gap (3 % at least) and with the median
    /// agreeing, so rounds a busy machine slowed in a stretch decide nothing
    #[test]
    fn a_host_time_winner_is_clear_by_its_fastest_round() {
        assert!(host_clear(1.40, 1.30, Some(1.01)));
        assert!(!host_clear(1.02, 1.30, Some(1.00)), "within 3 %");
        assert!(!host_clear(1.06, 1.30, Some(1.08)), "within the A/A pair's own gap");
        assert!(!host_clear(1.06, 1.30, Some(0.92)), "the gap counts either way");
        assert!(!host_clear(1.40, 0.90, None), "a median that disagrees");
        assert!(host_clear(1.05, 1.10, None));
        // (slowed stretches: the fastest rounds of two equal variants agree, their medians need not)
        let a = [0.112, 0.113, 0.403, 0.114, 0.568, 0.116, 0.113, 0.116, 0.720];
        let b = [0.447, 0.113, 0.579, 0.730, 0.825, 0.223, 0.965, 1.289, 1.521];
        let fastest = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
        let (median_a, median_b) = (median_spread(&a).unwrap().0, median_spread(&b).unwrap().0);
        assert!(!host_clear(fastest(&b) / fastest(&a), median_b / median_a, None), "equal by their fastest rounds");
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
}

//! `ree record` / `ree run`: runs a harness script against the mounted project frame by frame;
//! `record` also records the 3D world (record/mod.rs) into an output directory:
//!
//!   ree record --harness <file.luau> --out <dir> [--seconds 3] [--fps 60]
//!       [--context <json|file>] [--assets-at <Service.Path>] [--assets <prefix>,...]
//!       [--animations <dir>[;<dir>...]] (pre-fetched KeyframeSequences, `<id>.rbxm`)
//!       [--timing-check] (record again with the in-frame clock moved: does the recording hinge on sub-frame timing?)
//!       [the engine options (launch.rs): --roots / --place (default game) / --project / --mode
//!        (default play) / --mount / --snapshot-dir / --player / --offline / --verbose ...]
//!   ree run --harness <file.luau> [the same options but --out]
//!
//! The harness is a ModuleScript (mounted as `ReplicatedStorage.RURFixtures.<name>`) returning
//! `function(Context)`, or a table with `Run = function(Context)`. It builds what the run needs
//! (requiring the game's modules through services, making the instances a server would replicate,
//! connecting RunService) and may yield. `Context` holds `Seconds`, `Fps`, the `--context` object's
//! fields, `StandIn(Module, Source)`, which gives a ModuleScript another source before anything
//! requires it (a stand-in for a module the run cannot have), `Mount(Path, File)`, which mounts a
//! model file or directory as `--mount Path=File` does, and `Checkpoint()`, which marks where the
//! run's setup ends: a service prepares the next run with the same arguments up to it
//! (record/prepare.rs); to the run itself it does nothing.
//!
//! A harness may also split its setup: `{ Prepare = function(Context), Run = function(Context) }`.
//! Prepare gets a Context without the command's `--context` fields, Seconds and Fps (StandIn, Mount
//! and Checkpoint only): what it does is the same for every run of the harness and project. Its end
//! is the checkpoint; then the same Context gets those fields and Run goes on with it. A service
//! prepares such a run up to there for every job of the harness whose other arguments are the same.
//!
//! Frame 0 is sampled once the harness has started; then the virtual clock runs `--fps` frames a
//! second (a Heartbeat and a render step each, in play mode one Heartbeat per frame) for
//! `--seconds`, and the world is sampled after each. `record` writes `scene.rbxm`, `playback.json`,
//! `events.json`, `camera.json` and `summary.json` (also printed) with a `<host>: record ...` line
//! on stderr; `run` prints the status and every output line as JSON.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use mlua::prelude::*;
use serde_json::{Map, Value as Json, json};

use super::Recorder;
use crate::{
    Emulator, Engine, K, NameId, instance as core,
    launch::{self, Host},
    lua::{
        self as engine_lua,
        globals::{CallOutcome, call_then},
        instance_value, modules,
    },
    mount, project, scheduler, services,
    value::{Str, Value},
};

#[derive(Debug, Clone)]
pub struct RecordArgs {
    pub engine: launch::Args,
    pub harness: String,
    /// The output directory (`record`; `run` writes nothing)
    pub out: Option<PathBuf>,
    pub output_log: Option<PathBuf>,
    pub seconds: f64,
    pub fps: f64,
    pub context: Option<Json>,
    pub model_assets: project::ModelAssetsOverride,
    /// Directories of pre-fetched animations (`--animations`; animation/source.rs)
    pub animations: Vec<PathBuf>,
    /// `--frame-times <file.json>`: the moments of the frames after frame 0 (a JSON array of
    /// seconds, increasing), e.g. a Studio take's, stepped as they are; past its last, `--fps`
    pub frame_times: Option<Vec<f64>>,
    /// `--script-timeout <seconds>`: Roblox's script execution timeout, in seconds of the work clock a resumption may run
    /// without yielding (None: `REE_SCRIPT_TIMEOUT`, else Studio's 10; 0 turns it off; watchdog.rs)
    pub script_timeout: Option<f64>,
    /// `--max-script-timeouts <n>`: script timeouts after which the run exits with 126 (None: `REE_MAX_SCRIPT_TIMEOUTS`, else
    /// 3; 0 never)
    pub script_timeouts: Option<u32>,
    /// `--timing-check` (or `REE_TIMING_CHECK=1`): record again with the in-frame clock moved and say whether the recording
    /// changed ([`check_timing`])
    pub timing_check: bool,
    /// This run is that second recording (Options::clock_moved)
    pub clock_moved: bool,
}

impl RecordArgs {
    /// The moment of every frame after frame 0 up to `--seconds`: `--frame-times`' and then one
    /// every 1 / `--fps`, else one every 1 / `--fps` from the start.
    pub fn frame_schedule(&self) -> Vec<f64> {
        let given = self.frame_times.as_deref().unwrap_or(&[]);
        let mut times: Vec<f64> = given.iter().copied().take_while(|&t| t <= self.seconds + 1e-9).collect();
        if times.is_empty() && given.is_empty() {
            let frames = (self.seconds * self.fps).round().max(1.0) as u32;
            return (1..=frames).map(|frame| frame as f64 / self.fps).collect();
        }
        let last = times.last().copied().unwrap_or(0.0);
        let mut step = 1;
        while last + step as f64 / self.fps <= self.seconds + 1e-9 {
            times.push(last + step as f64 / self.fps);
            step += 1;
        }
        times
    }
}

/// `--frame-times`' file: a JSON array of increasing positive seconds (a leading 0 is frame 0's).
fn read_frame_times(path: &str) -> Result<Vec<f64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("--frame-times {path}: {e}"))?;
    let json: Json = serde_json::from_str(&text).map_err(|e| format!("--frame-times {path}: {e}"))?;
    let mut times: Vec<f64> = json.as_array().ok_or("--frame-times: a JSON array of seconds")?.iter().map(|t| t.as_f64().ok_or("--frame-times: a JSON array of seconds")).collect::<Result<_, _>>()?;
    if times.first() == Some(&0.0) {
        times.remove(0);
    }
    if times.first().is_some_and(|&t| t <= 0.0) || times.windows(2).any(|pair| pair[1] <= pair[0]) {
        return Err("--frame-times: the seconds must be positive and increasing".into());
    }
    Ok(times)
}

/// `ree record ...` (`record`) or `ree run ...`: the exit code (0 when the run finished; a
/// harness error is in the printed status). The process is expected to exit after it: the VM and
/// the engine are left to the operating system ([`release`]).
pub fn cli(args: &[String], host: &Host, record: bool) -> i32 {
    let started = std::time::Instant::now();
    let command = if record { "record" } else { "run" };
    let args = launch::split_equals(args);
    if let Err(error) = guard_process(command, &args, started) {
        eprintln!("{} {command}: {error}", host.name);
        return 1;
    }
    // the profiling suite (`--profile <kinds>`, profiler.rs); `--profile` alone (the phases) and `--profile boundary` alone keep
    // their own forms
    let suite = match crate::profiler::Config::from_args(&args, || default_profile_dir(&args, record)) {
        Ok(suite) => suite,
        Err(error) => {
            eprintln!("{} {command}: {error}", host.name);
            return 2;
        }
    };
    if suite.is_none() {
        if crate::profiler::phases_requested(&args) {
            crate::timing::begin(std::time::Instant::now());
        }
        if let Some(config) = crate::boundary::parse(&args) {
            crate::boundary::begin(config);
        }
    }
    let session = suite.map(crate::profiler::Session::begin);
    let luau_profile = crate::luau_profile::begin();
    let mut profiled_run = crate::profiler::Run { command: command.into(), ..Default::default() };
    let result = parse(&args, record).and_then(|args| {
        profiled_run.harness = Some(args.harness.clone());
        profiled_run.seconds = args.seconds;
        profiled_run.fps = args.fps;
        let (done, emulator) = execute_with(&args, host, record, true)?;
        release(emulator);
        Ok(done)
    });
    drop(luau_profile);
    if let Some(session) = session {
        if let Ok(done) = &result {
            profiled_run.status = done.summary["status"].as_str().unwrap_or("?").to_string();
            profiled_run.frames = done.summary["frames"].as_u64().unwrap_or(0);
            if let Some(frames) = done.summary["frames"].as_u64().filter(|_| profiled_run.fps > 0.0) {
                profiled_run.seconds = profiled_run.seconds.min(frames.saturating_sub(1) as f64 / profiled_run.fps).max(0.0);
            }
        } else {
            profiled_run.status = "failed".into();
        }
        match session.finish(profiled_run) {
            Ok(summary) => eprint!("{summary}"),
            Err(error) => eprintln!("{} {command}: profile: {error}", host.name),
        }
    } else if let Some(report) = crate::boundary::finish() {
        eprintln!("{report}");
    }
    match result {
        Ok(done) => {
            eprintln!("{}", done.line);
            if let Some(profile) = crate::timing::finish() {
                eprintln!("{profile}");
            }
            let code = done.code;
            match done.print(&mut std::io::stdout().lock()) {
                Ok(()) => code,
                Err(error) => {
                    eprintln!("{} {command}: stdout: {error}", host.name);
                    1
                }
            }
        }
        Err(error) => {
            if let Some(profile) = crate::timing::finish() {
                eprintln!("{profile}");
            }
            eprintln!("{} {command}: {error}", host.name);
            1
        }
    }
}

/// A profile's directory when `--profile-out` names none: `<out>/profile` for a recording, `ree-profile` here for a run
fn default_profile_dir(args: &[String], record: bool) -> PathBuf {
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned();
    match value("--out").or_else(|| value("--dir")).filter(|_| record) {
        Some(out) => PathBuf::from(out).join("profile"),
        None => PathBuf::from("ree-profile"),
    }
}

/// The process's own guards for a run of `args` (watchdog.rs), counted from `started`: the wall-clock cap
/// (`--max-wall-seconds <n>`, else `REE_MAX_WALL_SECONDS`, else 10 minutes plus 20 s per `--seconds`; 0 turns it off; exit
/// 124) and the exit when the parent process is gone (exit 125; `--no-parent-watch` or `REE_PARENT_WATCH=0` keep running).
/// Once per process: a `--service` client that runs the command itself after all has them already.
pub fn guard_process(command: &str, args: &[String], started: std::time::Instant) -> Result<(), String> {
    static GUARDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1));
    let number = |key: &str| -> Result<Option<f64>, String> {
        value(key).map(|text| text.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| format!("{key}: not a number of seconds"))).transpose()
    };
    let cap = number("--max-wall-seconds")?;
    let seconds = number("--seconds")?.unwrap_or(3.0);
    if GUARDED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    crate::watchdog::cap_run(command, crate::watchdog::cap_seconds(cap, seconds), seconds, started);
    if !args.iter().any(|arg| arg == "--no-parent-watch") {
        crate::watchdog::watch_parent(command);
    }
    Ok(())
}

/// The options of `ree record` (`record`: `--out` is required) or `ree run`.
pub fn parse(args: &[String], record: bool) -> Result<RecordArgs, String> {
    let mut engine = launch::parse(args)?;
    if engine.mode.is_none() {
        engine.mode = Some("play".into());
    }
    if engine.place.is_none() && engine.roots.is_empty() {
        engine.place = Some("game".into());
    }
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned();
    let harness = value("--harness").ok_or("--harness <file.luau> is required")?;
    let out = value("--dir").or_else(|| value("--out")).or_else(|| value("--dump-scene")).map(PathBuf::from);
    let output_log = value("--output-log").map(PathBuf::from);
    if record && out.is_none() {
        return Err("--out <dir> is required".into());
    }
    let number = |key: &str, default: f64| -> Result<f64, String> {
        match value(key) {
            Some(text) => text.parse::<f64>().map_err(|_| format!("{key}: not a number")).and_then(|n| if n > 0.0 && n.is_finite() { Ok(n) } else { Err(format!("{key} must be positive")) }),
            None => Ok(default),
        }
    };
    let seconds = number("--seconds", 3.0)?;
    let fps = number("--fps", 60.0)?;
    let context = match value("--context") {
        Some(spec) => {
            let text = if spec.trim_start().starts_with('{') { spec } else { std::fs::read_to_string(&spec).map_err(|e| format!("--context {spec}: {e}"))? };
            let decoded = crate::lune_json::decode(&text).map_err(|e| format!("--context: {e}"))?;
            if !decoded.is_object() {
                return Err("--context must be a JSON object".into());
            }
            Some(decoded)
        }
        None => None,
    };
    // (the scene: which bundles the last run like it imported, decoded ahead the next time)
    let scene = json!([std::path::absolute(&harness).unwrap_or_else(|_| PathBuf::from(&harness)).to_string_lossy(), engine.place, engine.roots, context]).to_string();
    let model_assets = project::ModelAssetsOverride {
        mount: value("--assets-at"),
        prefixes: value("--assets").map(|list| list.split(',').map(|p| p.trim().trim_matches('/').to_string()).filter(|p| !p.is_empty()).collect()),
        scene: Some(scene),
    };
    let animations = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--animations")
        .filter_map(|(index, _)| args.get(index + 1))
        .flat_map(|list| list.split(';').filter(|dir| !dir.is_empty()).map(|dir| PathBuf::from(launch::absolute(dir))).collect::<Vec<_>>())
        .collect();
    let frame_times = value("--frame-times").map(|path| read_frame_times(&path)).transpose()?;
    let script_timeout = value("--script-timeout")
        .map(|text| text.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).ok_or("--script-timeout: not a number of seconds"))
        .transpose()?;
    let script_timeouts = value("--max-script-timeouts").map(|text| text.parse::<u32>().map_err(|_| "--max-script-timeouts: not a count")).transpose()?;
    let timing_check = record && (args.iter().any(|arg| arg == "--timing-check") || std::env::var("REE_TIMING_CHECK").as_deref() == Ok("1"));
    Ok(RecordArgs { engine, harness, out, output_log, seconds, fps, context, model_assets, animations, frame_times, script_timeout, script_timeouts, timing_check, clock_moved: false })
}

/// What a command produced: the summary (printed on stdout) and its status line (stderr).
pub struct Done {
    pub summary: Json,
    pub line: String,
    /// The harness called Context.Checkpoint where a run may be prepared up to (record/prepare.rs)
    pub checkpoint: bool,
    /// The harness has Prepare: its checkpoint comes before it sees `--context`, `--seconds`, `--fps`
    pub split: bool,
    /// Its checkpoint came between frames (Context.FrameCheckpoint): a prepared run is for its `--fps`
    pub framed: bool,
    /// The command's exit code: 0, or [`crate::watchdog::EXIT_SCRIPT_TIMEOUTS`] / [`crate::watchdog::EXIT_ORPHANED`] when stopped
    pub code: i32,
    /// Why a run prepared up to its checkpoint would fail the same way (it read a mesh, an image... first)
    pub unpreparable: Option<String>,
    /// A run's output lines when they went to a temporary file as they came (the command line's
    /// `ree run`): the summary's `output` is empty and [`Done::print`] writes them into it
    pub output: Option<std::fs::File>,
}

impl Done {
    /// Writes the summary as one line of JSON, as `println!` of `serde_json::to_string` does, with
    /// the lines of a spooled `output` in its array: the same bytes as the summary holding them.
    pub fn print(self, out: &mut impl std::io::Write) -> std::io::Result<()> {
        use std::io::Write;
        let text = serde_json::to_string(&self.summary).unwrap_or_default();
        let mut out = std::io::BufWriter::with_capacity(1 << 16, out);
        // (keys and strings are escaped: `"output":[]` is the key and its empty array)
        match (self.output, text.find("\"output\":[]")) {
            (Some(mut lines), Some(at)) => {
                let inside = at + "\"output\":[".len();
                out.write_all(text[..inside].as_bytes())?;
                std::io::copy(&mut lines, &mut out)?;
                out.write_all(text[inside..].as_bytes())?;
            }
            _ => out.write_all(text.as_bytes())?,
        }
        out.write_all(b"\n")?;
        out.flush()
    }
}

/// Runs `record` (or `run`) with the engine of `args` (the host's) and its project mounted.
/// Returns what it produced and the emulator, which the caller drops ([`Emulator`]'s teardown)
/// or, about to exit, [`release`]s.
///
/// Module chunks stay interpreted ([`modules::interpret_modules`]), and the module files the
/// last run of the same harness and project read are read and compiled to bytecode on other
/// threads while the engine starts and mounts ([`modules::Prefetch`]; the list is kept in the
/// state dir, `record/<key>.txt`, after each run).
pub fn execute(args: &RecordArgs, host: &Host, record: bool) -> Result<(Done, Emulator), String> {
    execute_with(args, host, record, false)
}

/// [`execute`]; with `spool`, the output lines go to a temporary file as they come instead of being
/// kept: a run's as it prints them ([`Done::output`]), a recording's as events.json holds them
/// ([`Recorder::write`]).
pub(crate) fn execute_with(args: &RecordArgs, host: &Host, record: bool, spool: bool) -> Result<(Done, Emulator), String> {
    let (emulator, history) = start(args, host, false)?;
    if let Some(path) = &args.output_log {
        emulator.engine.state.borrow_mut().output.set_log(path)?;
    }
    if spool {
        let mut state = emulator.engine.state.borrow_mut();
        if record {
            state.output.spool_as(super::event_line, b",\n");
        } else {
            state.output.spool();
        }
    }
    let done = if record { record_in(&emulator, args, host)? } else { run_in(&emulator, args, host)? };
    finished(&emulator, &history);
    // (a profile reads the VM while it still runs)
    crate::profiler::collect(&emulator.lua);
    let done = if record && args.timing_check && !args.clock_moved { check_timing(args, host, done) } else { done };
    Ok((done, emulator))
}

/// What a run ends with: the profile's bundle counts, and the history saved.
pub(crate) fn finished(emulator: &Emulator, history: &History) {
    if crate::timing::enabled() {
        let state = emulator.engine.state.borrow();
        let imported: usize = state.project.imports.iter().map(|import| import.len()).sum();
        crate::timing::tally("bundles: instances imported", imported as u64);
        crate::timing::tally("bundles: instances made", state.project.lazy_made as u64);
    }
    history.save(&emulator.engine);
}

/// The engine of `args` with its project mounted, and the history of the harness's last run. With
/// `listings`, the mount keeps the directory listings it made (record/prepare.rs checks them again).
pub(crate) fn start(args: &RecordArgs, host: &Host, listings: bool) -> Result<(Emulator, History), String> {
    let prepared = crate::timing::time("prepare", || launch::prepare(&args.engine, &host.repo))?;
    let history = History::new(host, args, &prepared.roots);
    // (a process that keeps compiled modules compiles the ones it lacks into them)
    let prefetch = match &history.modules {
        Some(files) if modules::reusing_compiled_modules() => {
            modules::precompile(files.clone());
            None
        }
        Some(files) => Some(modules::Prefetch::start(files.clone())),
        None => None,
    };
    let mut options = prepared.options.clone();
    options.clock_moved = args.clock_moved;
    let engine = crate::timing::time("engine (reflection)", || (host.new_engine)(options))?;
    if let Some(prefetch) = prefetch {
        modules::use_prefetch(&engine, prefetch);
    }
    modules::interpret_modules(&engine);
    // AnimationTracks play their keyframes: the directories given, the download cache, the asset worker
    crate::animation::enable(&engine, crate::animation::Loading { directories: args.animations.clone(), download: true });
    if listings {
        engine.state.borrow_mut().project.listings = Some(Vec::new());
    }
    let spec = mount::ProjectSpec {
        roots: prepared.roots.clone(),
        snapshots: args.engine.snapshots.clone(),
        snapshot_dir: prepared.snapshot_dir.clone(),
        mounts: prepared.mounts.clone(),
        model_assets: args.model_assets.clone(),
    };
    let emulator = Emulator::start(engine, &spec)?;
    crate::luau_profile::install(&emulator.lua);
    // Roblox's script execution timeout (watchdog.rs): a script that never yields errors instead of spinning for good
    let limit = crate::watchdog::script_timeout_seconds(args.script_timeout);
    let timeout = crate::watchdog::ScriptTimeout::install(&emulator.lua, &emulator.engine, limit, crate::watchdog::stop_after(args.script_timeouts));
    emulator.engine.state.borrow_mut().sched.timeout = timeout;
    Ok((emulator, history))
}

/// Ends an emulator in a process that exits next: the bundle worker is stopped and the rest is
/// left to the operating system, which frees it at once (a recording's teardown took ~0.3 s).
pub fn release(emulator: Emulator) {
    #[cfg(feature = "bundles")]
    project::model_assets::stop(&emulator.engine);
    crate::animation::source::stop(&emulator.engine);
    std::mem::forget(emulator);
}

/// One recording: the project, the harness, the frames, the files. Returns the summary.
pub fn record(args: &RecordArgs, host: &Host) -> Result<Json, String> {
    let (done, emulator) = execute(args, host, true)?;
    eprintln!("{}", done.line);
    drop(emulator);
    Ok(done.summary)
}

fn record_in(emulator: &Emulator, args: &RecordArgs, host: &Host) -> Result<Done, String> {
    let recorder = Recorder::install(&emulator.engine, args.fps);
    recorder.set_frame_times(args);
    let ran = run(&emulator.lua, &emulator.engine, args, Some(&recorder))?;
    record_done(emulator, args, host, &recorder, ran, emulator.project.started)
}

/// A recording's files and summary once its frames ran; its status line counts from `started`.
pub(crate) fn record_done(emulator: &Emulator, args: &RecordArgs, host: &Host, recorder: &Recorder, ran: Ran, started: std::time::Instant) -> Result<Done, String> {
    let out = args.out.clone().ok_or("--out <dir> is required")?;
    let engine = &emulator.engine;
    let mut extra = Map::new();
    extra.insert("status".into(), json!(ran.status));
    if let Some(failure) = &ran.failure {
        extra.insert("failure".into(), json!(failure));
    }
    extra.insert("harness".into(), json!(ran.harness.to_string_lossy()));
    extra.insert("mode".into(), json!(engine.options.mode));
    if ran.done {
        extra.insert("done".into(), json!(true));
    }
    if engine.options.server() {
        extra.insert("runContext".into(), json!("server"));
    }
    extra.insert("out".into(), json!(std::path::absolute(&out).unwrap_or(out.clone()).to_string_lossy()));
    // (a service's job whose client has gone: nobody reads its files)
    if ran.code == crate::watchdog::EXIT_ORPHANED {
        extra.insert("frames".into(), json!(ran.frames + 1));
        let line = format!("{}: record {}; {} frames; not written: {}", host.name, ran.status, ran.frames + 1, crate::watchdog::CALLER_GONE);
        return Ok(Done { summary: Json::Object(extra), line, checkpoint: ran.checkpoint, split: ran.split, framed: ran.framed, code: ran.code, unpreparable: ran.unchecked.clone(), output: None });
    }
    let summary = crate::timing::time("write", || recorder.write(engine, &out, extra))?;
    let line = format!(
        "{}: record {}; {} frames; {} instances; {} errors; {:.2}s -> {}",
        host.name,
        ran.status,
        ran.frames + 1,
        summary["instances"],
        summary["errors"],
        started.elapsed().as_secs_f64(),
        out.display()
    );
    Ok(Done { summary, line, checkpoint: ran.checkpoint, split: ran.split, framed: ran.framed, code: ran.code, unpreparable: ran.unchecked.clone(), output: None })
}

/// `ree run`: the harness run without a recording. Returns the status and the output.
pub fn run_only(args: &RecordArgs, host: &Host) -> Result<Json, String> {
    let (done, emulator) = execute(args, host, false)?;
    eprintln!("{}", done.line);
    drop(emulator);
    Ok(done.summary)
}

fn run_in(emulator: &Emulator, args: &RecordArgs, host: &Host) -> Result<Done, String> {
    let ran = run(&emulator.lua, &emulator.engine, args, None)?;
    run_done(emulator, args, host, ran, emulator.project.started)
}

/// A run's summary once its frames ran; its status line counts from `started`.
pub(crate) fn run_done(emulator: &Emulator, args: &RecordArgs, host: &Host, ran: Ran, started: std::time::Instant) -> Result<Done, String> {
    let engine = &emulator.engine;
    let spooled = engine.state.borrow_mut().output.take_spool();
    let state = engine.state.borrow();
    let output: Vec<Json> = state.output.entries.iter().map(crate::output::Entry::json).collect();
    let mut errors = output.iter().filter(|line| line["kind"] == "error").count();
    let mut lines = output.len();
    if let Some(spool) = &spooled {
        // (none are kept beside a spool: `output` is empty)
        errors += spool.errors();
        lines += spool.lines();
    }
    if let Some(error) = state.output.log_error() {
        return Err(format!("output log: {error}"));
    }
    let line = format!("{}: run {}; {} frames; {lines} output lines; {errors} errors; {:.2}s", host.name, ran.status, ran.frames + 1, started.elapsed().as_secs_f64());
    let mut summary = json!({
        "status": ran.status,
        "harness": ran.harness.to_string_lossy(),
        "mode": engine.options.mode,
        "frames": ran.frames + 1,
        "fps": args.fps,
        "duration": args.seconds,
        "errors": errors,
        "output": output,
    });
    if let Some(failure) = ran.failure {
        summary["failure"] = json!(failure);
    }
    if ran.done {
        summary["done"] = json!(true);
    }
    if engine.options.server() {
        summary["runContext"] = json!("server");
    }
    let output = spooled.map(|spool| spool.finish()).transpose()?;
    Ok(Done { summary, line, checkpoint: ran.checkpoint, split: ran.split, framed: ran.framed, code: ran.code, unpreparable: ran.unchecked.clone(), output })
}

/// `ree serve --warm [engine options, --assets-at, --assets]`: what a first recording of the project
/// reads, made ready in the service that runs it, recording nothing: every harness's last module
/// list (`record/*.txt`) read and compiled into the kept modules, the plans of the bundles the last
/// recordings of this mount imported read into the kept store, and a mount of the project (the
/// scan, the Studio hierarchy, the catalog, Engine.Start) that is dropped. The summary: how many
/// modules and bundles are ready (`modules`, `bundles`), how many of them it read now (`modulesRead`,
/// `bundlesRead`; the rest the service kept already) and how long it took.
pub fn warm(args: &[String], host: &Host) -> Result<Json, String> {
    let started = std::time::Instant::now();
    let args = launch::split_equals(args);
    let mut engine_args = launch::parse(&args)?;
    if engine_args.mode.is_none() {
        engine_args.mode = Some("play".into());
    }
    if engine_args.place.is_none() && engine_args.roots.is_empty() {
        engine_args.place = Some("game".into());
    }
    let value = |key: &str| args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned();
    let model_assets = project::ModelAssetsOverride {
        mount: value("--assets-at"),
        prefixes: value("--assets").map(|list| list.split(',').map(|p| p.trim().trim_matches('/').to_string()).filter(|p| !p.is_empty()).collect()),
        scene: None,
    };
    let prepared = launch::prepare(&engine_args, &host.repo)?;
    let files = project_module_lists(&crate::state_dir(&host.repo).join("record"), &prepared.roots);
    let files: Vec<String> = files.into_iter().filter(|file| crate::files::is_file(std::path::Path::new(file))).collect();
    let modules_read = modules::precompile_now(&files);
    let engine = (host.new_engine)(prepared.options.clone())?;
    #[cfg(feature = "bundles")]
    let (bundles, bundles_read) = project::model_assets::warm(&host.repo, &prepared.roots, &model_assets, &engine.reflection);
    #[cfg(not(feature = "bundles"))]
    let (bundles, bundles_read) = (0, 0);
    let spec = mount::ProjectSpec { roots: prepared.roots.clone(), snapshots: engine_args.snapshots.clone(), snapshot_dir: prepared.snapshot_dir.clone(), mounts: prepared.mounts.clone(), model_assets };
    let emulator = Emulator::start(engine, &spec)?;
    #[cfg(feature = "bundles")]
    project::model_assets::stop(&emulator.engine);
    drop(emulator);
    Ok(json!({ "warm": true, "modules": files.len(), "modulesRead": modules_read, "bundles": bundles, "bundlesRead": bundles_read, "seconds": started.elapsed().as_secs_f64() }))
}

/// The files in the [`History`] lists of `roots`' project, for a warm-up; lists of removed checkouts are deleted
fn project_module_lists(directory: &std::path::Path, roots: &[String]) -> std::collections::BTreeSet<String> {
    use std::io::BufRead;
    // (compared as the lists write them, with forward slashes; Windows paths ignore case)
    let key = |path: &str| {
        let path = path.replace('\\', "/");
        let path = path.trim_end_matches('/');
        if cfg!(windows) { path.to_ascii_lowercase() } else { path.to_string() }
    };
    let mut prefixes: Vec<String> = roots.iter().map(|root| format!("{}/", key(root))).collect();
    prefixes.extend(roots.iter().filter_map(|root| std::path::Path::new(root).parent()).map(|project| format!("{}/external/", key(&project.to_string_lossy()))));
    let ours = |line: &str| {
        let line = key(line);
        prefixes.iter().any(|prefix| line.starts_with(prefix.as_str()))
    };
    let mut files = std::collections::BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(directory) else { return files };
    for entry in entries.flatten().filter(|entry| entry.path().extension().is_some_and(|extension| extension == "txt")) {
        let Ok(file) = std::fs::File::open(entry.path()) else { continue };
        let mut lines = std::io::BufReader::new(file).lines().map_while(Result::ok).filter(|line| !line.is_empty());
        let Some(first) = lines.next() else { continue };
        if ours(&first) {
            files.extend(std::iter::once(first).chain(lines).filter(|line| ours(line)));
        } else if std::path::Path::new(&first).parent().is_some_and(|folder| !folder.is_dir()) {
            drop(lines);
            let _ = std::fs::remove_file(entry.path());
        }
    }
    files
}

/// What the last run of a harness against a project read, kept in the state dir
/// (`record/<key>.txt`, one module file a line): the next run reads and compiles those files
/// ahead. A guess only (a file read differently is read as usual).
pub(crate) struct History {
    path: PathBuf,
    modules: Option<Vec<String>>,
}

impl History {
    fn new(host: &Host, args: &RecordArgs, roots: &[String]) -> History {
        use std::hash::{Hash, Hasher};
        let mut hasher = rustc_hash::FxHasher::default();
        std::path::absolute(&args.harness).unwrap_or_else(|_| PathBuf::from(&args.harness)).hash(&mut hasher);
        roots.hash(&mut hasher);
        let path = crate::state_dir(&host.repo).join("record").join(format!("{:016x}.txt", hasher.finish()));
        let modules = std::fs::read_to_string(&path).ok().map(|text| text.lines().filter(|line| !line.is_empty()).map(str::to_string).collect());
        History { path, modules }
    }

    fn save(&self, engine: &Engine) {
        #[cfg(feature = "bundles")]
        project::model_assets::remember(engine);
        let files = modules::required_files(engine);
        if self.modules.as_ref() == Some(&files) || files.is_empty() {
            return;
        }
        if let Some(directory) = self.path.parent() {
            let _ = std::fs::create_dir_all(directory);
        }
        let _ = crate::files::write_atomic(&self.path, files.join("\n").as_bytes());
    }
}

/// How a harness run went.
pub struct Ran {
    pub status: String,
    pub failure: Option<String>,
    /// Frames stepped after frame 0
    pub frames: u32,
    pub harness: PathBuf,
    /// The harness called Context.Checkpoint on its own thread before it first yielded
    pub checkpoint: bool,
    /// The harness has Prepare (its Context got the command's fields after its checkpoint)
    pub split: bool,
    /// Its checkpoint came between frames (Context.FrameCheckpoint): a run prepared up to it is for `--fps`
    pub framed: bool,
    /// The command's exit code ([`Done::code`])
    pub code: i32,
    /// A script printed `@done`: the frames ended there, before `--seconds`
    pub done: bool,
    /// What it read before its checkpoint that a prepared run cannot check ([`Done::unpreparable`])
    pub unchecked: Option<String>,
}

/// What a harness with Prepare sees only after its checkpoint: the `--context` fields, Seconds and
/// Fps. A prepared run's job puts its own in before the run goes on (record/prepare.rs).
#[derive(Clone, Default)]
pub(crate) struct Late {
    context: Option<Json>,
    seconds: f64,
    fps: f64,
}

impl Late {
    pub fn of(args: &RecordArgs) -> Late {
        Late { context: args.context.clone(), seconds: args.seconds, fps: args.fps }
    }
}

/// What Context.Checkpoint and the run share: whether this run parks there (record/prepare.rs), the
/// harness's thread, whether its first resumption is still running, and what happened.
#[derive(Default)]
pub(crate) struct Checkpoint {
    park: std::cell::Cell<bool>,
    armed: std::cell::Cell<bool>,
    harness: std::cell::RefCell<Option<LuaThread>>,
    reached: std::cell::Cell<bool>,
    parked: std::cell::Cell<bool>,
    /// The harness has Prepare
    split: std::cell::Cell<bool>,
    /// The command's fields, Seconds and Fps, as the harness's Context gets them
    late: std::cell::RefCell<Late>,
    /// The harness waits at Context.FrameCheckpoint for the run to resume it before the next frame
    between: std::cell::Cell<bool>,
    /// Its checkpoint was Context.FrameCheckpoint after frames
    framed: std::cell::Cell<bool>,
    /// What the run read before its checkpoint that a prepared run cannot check (crate::files::unchecked)
    unchecked: std::cell::RefCell<Option<String>>,
}

/// A harness started: its call, its path, and its checkpoint (parked there or not).
pub(crate) struct Begun {
    harness_call: crate::lua::globals::Call,
    harness: PathBuf,
    checkpoint: Rc<Checkpoint>,
    starting: crate::timing::Timer,
    /// Parked between frames: where the frames prepared ahead stopped
    framed: Option<Steps>,
}

impl Begun {
    /// Whether the harness waits at its checkpoint ([`finish_run`] goes on from there).
    pub fn parked(&self) -> bool {
        self.checkpoint.parked.get()
    }

    /// Whether the harness has Prepare: what ran up to its checkpoint saw nothing of [`Late`].
    pub fn split(&self) -> bool {
        self.checkpoint.split.get()
    }

    /// Whether it parked between frames (its frames ran at `--fps`), and how many it stepped
    pub fn framed(&self) -> Option<u32> {
        self.framed.as_ref().map(|steps| steps.stepped)
    }

    /// What the harness's Context gets after the checkpoint (a prepared run's job's own).
    pub fn set_late(&self, late: Late) {
        *self.checkpoint.late.borrow_mut() = late;
    }
}

/// The harness, frame 0, then the frames (each sampled by `recorder`).
pub fn run(lua: &Lua, engine: &Rc<Engine>, args: &RecordArgs, recorder: Option<&Recorder>) -> Result<Ran, String> {
    let begun = begin_run(lua, engine, args, false)?;
    finish_run(lua, engine, args, recorder, begun)
}

/// The harness mounted and started, up to its first yield, or, when `park`, up to its checkpoint
/// (Context.Checkpoint) when it calls it before that: then nothing after the call has run, not even
/// the deferred flush that follows a harness's first resumption.
pub(crate) fn begin_run(lua: &Lua, engine: &Rc<Engine>, args: &RecordArgs, park: bool) -> Result<Begun, String> {
    let lua_error = |error: LuaError| error.to_string();
    let starting = crate::timing::start("harness start");
    let harness = std::path::absolute(&args.harness).map_err(|e| format!("{}: {e}", args.harness))?;
    let module = mount_harness(engine, &harness)?;
    let checkpoint = Rc::new(Checkpoint::default());
    checkpoint.park.set(park);
    *checkpoint.late.borrow_mut() = Late::of(args);
    let shared = engine.lua.borrow().shared.clone().ok_or("no shared globals")?;
    // (one line, as it was: error positions name its line 1; Arm notes the harness's thread. The
    // Context is made once the harness says which it gets: with Prepare, one without the command's
    // fields until Prepare has returned and the checkpoint passed, when Fill gives it them)
    let runner: LuaFunction = lua
        .load(
            "local Require, Module, MakeContext, Fill, Arm = ... \
             Arm() \
             local Harness = Require(Module) \
             if type(Harness) == 'table' and Harness.Prepare ~= nil then \
             assert(type(Harness.Prepare) == 'function' and type(Harness.Run) == 'function', 'a record harness with Prepare returns { Prepare = function(Context), Run = function(Context) }') \
             local Context = MakeContext(true) \
             local Checkpoint = Context.Checkpoint \
             Harness.Prepare(Context) \
             Checkpoint() \
             Fill(Context) \
             return Harness.Run(Context) \
             end \
             if type(Harness) == 'table' then Harness = Harness.Run end \
             assert(type(Harness) == 'function', 'a record harness returns function(Context) or { Run = function(Context) }') \
             return Harness(MakeContext(false))",
        )
        .set_name("=rur.record")
        .set_environment(shared)
        .into_function()
        .map_err(lua_error)?;
    let make_context = {
        let (engine, checkpoint) = (engine.clone(), checkpoint.clone());
        lua.create_function(move |lua, split: bool| {
            checkpoint.split.set(split);
            context_table(lua, &engine, &checkpoint, split)
        })
        .map_err(lua_error)?
    };
    let fill = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |lua, context: LuaTable| fill_context(lua, &context, &checkpoint.late.borrow())).map_err(lua_error)?
    };
    let require: LuaFunction = engine.prelude().exports.get("Require").map_err(lua_error)?;
    let module_value = instance_value(lua, engine, module).map_err(lua_error)?;
    let arm = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |lua, ()| {
            *checkpoint.harness.borrow_mut() = Some(lua.current_thread());
            Ok(())
        })
        .map_err(lua_error)?
    };
    checkpoint.armed.set(true);
    let parked = checkpoint.clone();
    let arguments = LuaMultiValue::from_iter([LuaValue::Function(require), module_value, LuaValue::Function(make_context), LuaValue::Function(fill), LuaValue::Function(arm)]);
    let harness_call = call_then(lua, engine, runner, arguments, || !parked.parked.get()).map_err(lua_error)?;
    if !checkpoint.parked.get() {
        checkpoint.armed.set(false);
    }
    Ok(Begun { harness_call, harness, checkpoint, starting, framed: None })
}

/// The rest of a run [`begin_run`] started: the harness goes on from its checkpoint when it was
/// parked there (then the deferred flush its first resumption ends with), then frame 0 and the frames.
pub(crate) fn finish_run(lua: &Lua, engine: &Rc<Engine>, args: &RecordArgs, recorder: Option<&Recorder>, begun: Begun) -> Result<Ran, String> {
    let lua_error = |error: LuaError| error.to_string();
    let Begun { harness_call, harness, checkpoint, starting, framed } = begun;
    // (a prepared run's start is timed from here: what it ran ahead is not this command's)
    let starting = if checkpoint.parked.get() { crate::timing::start("harness start") } else { starting };
    let ahead = framed.is_some();
    let mut steps = Steps::default();
    if let Some(framed) = framed {
        // (parked between frames: frame 0 and the frames up to the checkpoint ran ahead; the harness
        // goes on before the next frame, as in a run from the start)
        checkpoint.parked.set(false);
        steps = framed;
    } else if checkpoint.parked.get() {
        checkpoint.parked.set(false);
        let thread = checkpoint.harness.borrow().clone().ok_or("no harness thread")?;
        scheduler::resume(lua, engine, &thread, LuaMultiValue::new());
        checkpoint.armed.set(false);
        scheduler::flush_deferred(lua, engine).map_err(lua_error)?;
    }
    if !ahead {
        if let CallOutcome::Failed(message, trace) = harness_call.outcome().map_err(lua_error)? {
            engine.output("error", message.clone(), trace);
            steps.fail(message);
        }
        scheduler::flush_deferred(lua, engine).map_err(lua_error)?;
        if let Some(recorder) = recorder {
            recorder.sample(engine, 0);
        }
    }
    starting.stop();
    crate::luau_profile::mark_frames();
    step_frames(lua, engine, args, recorder, &checkpoint, &mut steps, false)?;
    if steps.code == 0 {
        if let Some(message) = timeouts_stop(engine, steps.stepped) {
            engine.output("error", message.clone(), None);
            steps.fail(message);
            steps.code = crate::watchdog::EXIT_SCRIPT_TIMEOUTS;
        }
    }
    if steps.status == "ok" {
        match harness_call.outcome().map_err(lua_error)? {
            CallOutcome::Pending => steps.status = "yielded".into(),
            CallOutcome::Failed(message, trace) => {
                engine.output("error", message.clone(), trace);
                steps.fail(message);
            }
            _ => {}
        }
    }
    let done = engine.state.borrow().output.done;
    let Steps { status, failure, stepped, code, .. } = steps;
    Ok(Ran { status, failure, frames: stepped, harness, checkpoint: checkpoint.reached.get(), split: checkpoint.split.get(), framed: checkpoint.framed.get(), code, done, unchecked: checkpoint.unchecked.borrow().clone() })
}

/// Where a run's frames are: the frames stepped, the next one's index in the schedule and the moment
/// of the last, and how it is going.
#[derive(Clone)]
pub(crate) struct Steps {
    stepped: u32,
    next: usize,
    previous: f64,
    status: String,
    failure: Option<String>,
    code: i32,
}

impl Default for Steps {
    fn default() -> Steps {
        Steps { stepped: 0, next: 0, previous: 0.0, status: "ok".into(), failure: None, code: 0 }
    }
}

impl Steps {
    fn fail(&mut self, message: String) {
        self.status = "error".into();
        self.failure.get_or_insert(message);
    }
}

/// The harness parked between frames at Context.FrameCheckpoint goes on: before the next frame, in a
/// run from the start and in a prepared run's job alike.
fn resume_between(lua: &Lua, engine: &Rc<Engine>, checkpoint: &Checkpoint) -> Result<(), String> {
    checkpoint.between.set(false);
    let thread = checkpoint.harness.borrow().clone().ok_or("no harness thread")?;
    scheduler::resume(lua, engine, &thread, LuaMultiValue::new());
    scheduler::flush_deferred(lua, engine).map_err(|error| error.to_string())
}

/// The frames of `args`' schedule from `steps.next`, each sampled by `recorder`, until the schedule
/// ends, a script prints `@done`, a frame fails or script timeouts stop the run; with `stop_between`
/// (a run prepared ahead) also where the harness parks between frames: then true.
fn step_frames(lua: &Lua, engine: &Rc<Engine>, args: &RecordArgs, recorder: Option<&Recorder>, checkpoint: &Checkpoint, steps: &mut Steps, stop_between: bool) -> Result<bool, String> {
    let schedule = args.frame_schedule();
    let mut ended = true;
    while steps.next < schedule.len() {
        if checkpoint.between.get() {
            if stop_between {
                return Ok(true);
            }
            resume_between(lua, engine, checkpoint)?;
        }
        // a script printed `@done` (crate::output::DONE): the run ends with the frame it was printed in
        if engine.state.borrow().output.done {
            ended = false;
            break;
        }
        let at = schedule[steps.next];
        let frame = steps.next as u32 + 1;
        let delta = at - steps.previous;
        steps.previous = at;
        steps.next += 1;
        // script timeouts that stop the run (watchdog.rs)
        if let Some(message) = timeouts_stop(engine, steps.stepped) {
            engine.output("error", message.clone(), None);
            steps.fail(message);
            steps.code = crate::watchdog::EXIT_SCRIPT_TIMEOUTS;
            return Ok(false);
        }
        // a service's job whose client has gone: nobody waits for the rest of it
        if crate::watchdog::caller_gone() {
            let message = format!("after frame {}: {}", steps.stepped, crate::watchdog::CALLER_GONE);
            engine.output("error", message.clone(), None);
            steps.fail(message);
            steps.code = crate::watchdog::EXIT_ORPHANED;
            return Ok(false);
        }
        if let Err(error) = crate::timing::time("frames: step", || scheduler::step(lua, engine, Some(delta))) {
            let message = format!("frame {frame}: {}", scheduler::error_message(&error));
            engine.output("error", message.clone(), None);
            steps.fail(message);
            return Ok(false);
        }
        if let Some(recorder) = recorder {
            crate::timing::time("frames: sample", || recorder.sample(engine, frame));
        }
        steps.stepped = frame;
    }
    if ended && checkpoint.between.get() {
        if stop_between {
            return Ok(true);
        }
        resume_between(lua, engine, checkpoint)?;
    }
    Ok(false)
}

/// A run being prepared ahead (record/prepare.rs) whose harness yielded before any checkpoint: frame 0
/// and its frames until it parks between frames at Context.FrameCheckpoint (Err: it did not, or the
/// run failed first). A recording does not park between frames.
pub(crate) fn advance_to_frame_checkpoint(lua: &Lua, engine: &Rc<Engine>, args: &RecordArgs, begun: &mut Begun) -> Result<(), String> {
    let lua_error = |error: LuaError| error.to_string();
    match begun.harness_call.outcome().map_err(lua_error)? {
        CallOutcome::Pending => {}
        _ => return Err("the harness ended before a checkpoint".into()),
    }
    scheduler::flush_deferred(lua, engine).map_err(lua_error)?;
    let mut steps = Steps::default();
    let parked = step_frames(lua, engine, args, None, &begun.checkpoint, &mut steps, true)?;
    if !parked || steps.status != "ok" {
        return Err("the harness has no checkpoint before its run ends".into());
    }
    if !matches!(begun.harness_call.outcome().map_err(lua_error)?, CallOutcome::Pending) {
        return Err("the harness ended at its frame checkpoint".into());
    }
    begun.checkpoint.parked.set(true);
    begun.framed = Some(steps);
    Ok(())
}

/// The failure of a run its script timeouts stop after frame `stepped` (watchdog.rs `ScriptTimeout::stopped`)
fn timeouts_stop(engine: &Rc<Engine>, stepped: u32) -> Option<String> {
    let timeout = engine.state.borrow().sched.timeout.clone()?;
    timeout.stopped().map(|why| format!("after frame {stepped}: {why}"))
}

/// The harness's module name and the directory holding its own modules: `Foo.luau` is `Foo` with
/// the `Foo/` directory beside it (when there is one), `Foo/init.luau` is `Foo` with `Foo/`.
pub fn harness_layout(path: &std::path::Path) -> (String, Option<PathBuf>) {
    let stem = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".luau").trim_end_matches(".lua").to_string()).unwrap_or_else(|| "Harness".into());
    let parent = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    if stem.eq_ignore_ascii_case("init") {
        let name = parent.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Harness".into());
        return (name, Some(parent));
    }
    let beside = parent.join(&stem);
    crate::files::note(&beside);
    let children = beside.is_dir().then_some(beside);
    (stem, children)
}

/// The harness as a ModuleScript under `ReplicatedStorage.RURFixtures` (requires reach the
/// project through services, as a fixture's do), with its own modules as children
/// ([`harness_layout`]), so it can require them through `script`.
fn mount_harness(engine: &Rc<Engine>, path: &std::path::Path) -> Result<crate::InstanceId, String> {
    crate::files::note(path);
    let source = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (name, children) = harness_layout(path);
    let game = engine.world().game;
    let storage = services::get_service(engine, game, "ReplicatedStorage").map_err(|_| "ReplicatedStorage")?;
    let module = {
        let reflection = &engine.reflection;
        let hooks = engine.hooks.borrow();
        let mut state = engine.state.borrow_mut();
        let folder = match state.dom.find_child(storage, b"RURFixtures") {
            Some(folder) => folder,
            None => {
                let folder = core::create_named(&mut state, reflection, reflection.class_id("Folder").expect("Folder"), "RURFixtures");
                core::set_parent(&mut state, reflection, &hooks, folder, Some(storage)).map_err(|e| e.message)?;
                folder
            }
        };
        let module = core::create(&mut state, reflection, reflection.class_id("ModuleScript").expect("ModuleScript"));
        core::raw_set_silent(&mut state, reflection, module, K::Name as NameId, Value::String(Str::from(name)));
        state.dom.at_mut(module).set_source(Some(Rc::from(source.as_slice())));
        core::set_parent(&mut state, reflection, &hooks, module, Some(folder)).map_err(|e| e.message)?;
        state.dom.at_mut(module).set_source_path(Some(Rc::from(path.to_string_lossy().as_ref())));
        module
    };
    if let Some(directory) = children {
        project::mount_children(engine, &directory.to_string_lossy(), module);
    }
    Ok(module)
}

/// How `--context` values become Lua values.
fn context_options() -> LuaSerializeOptions {
    LuaSerializeOptions::new().set_array_metatable(false).serialize_none_to_null(false).serialize_unit_to_null(false)
}

/// The harness's `Context`: the command's fields, Seconds, Fps (all but in a harness's Prepare,
/// `split`: [`fill_context`] adds them after it), StandIn, Checkpoint and Mount.
fn context_table(lua: &Lua, engine: &Rc<Engine>, checkpoint: &Rc<Checkpoint>, split: bool) -> LuaResult<LuaTable> {
    let context = if split {
        lua.create_table()?
    } else {
        let late = checkpoint.late.borrow();
        let context = match &late.context {
            Some(fields) => match lua.to_value_with(fields, context_options())? {
                LuaValue::Table(table) => table,
                _ => lua.create_table()?,
            },
            None => lua.create_table()?,
        };
        context.set("Seconds", late.seconds)?;
        context.set("Fps", late.fps)?;
        context
    };
    let e = engine.clone();
    context.set(
        "StandIn",
        lua.create_function(move |_, (module, source): (LuaValue, LuaString)| {
            let Some(module) = engine_lua::instance_id(&module) else { return Err(LuaError::runtime("StandIn: a ModuleScript expected")) };
            if !e.reflection.is_a(e.state.borrow().dom.at(module).class, "ModuleScript") {
                return Err(LuaError::runtime("StandIn: a ModuleScript expected"));
            }
            engine_lua::modules::replace_source(&e, module, &source.as_bytes()).map_err(LuaError::runtime)
        })?,
    )?;
    // Context.Checkpoint(): what the harness did so far is the same for every run with these
    // arguments, so a service may run it ahead of the job and go on from here (record/prepare.rs).
    // Anywhere but on the harness's own thread before it first yields it does nothing.
    let check = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |lua, ()| {
            let own = checkpoint.harness.borrow().as_ref().is_some_and(|thread| *thread == lua.current_thread());
            if !checkpoint.armed.get() || !own {
                return Ok(false);
            }
            // (the first call only)
            if checkpoint.reached.replace(true) {
                return Ok(false);
            }
            *checkpoint.unchecked.borrow_mut() = crate::files::unchecked();
            checkpoint.parked.set(checkpoint.park.get());
            Ok(checkpoint.park.get())
        })?
    };
    // (parked or not, the call is the same Lua calls: a yield where the harness parks, a function that
    // returns where it does not; a yield the harness cannot make there, inside a metamethod say, leaves
    // the run unparked)
    let raw_yield: LuaFunction = lua.globals().get::<LuaTable>("coroutine")?.get("yield")?;
    let pass = lua.create_function(|_, ()| Ok(()))?;
    let missed = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |_, ()| {
            checkpoint.parked.set(false);
            Ok(())
        })?
    };
    let chunk = lua
        .load("local Check, Yield, Pass, Missed = ... return function() if not pcall(if Check() then Yield else Pass) then Missed() end end")
        .set_name("=ree.checkpoint")
        .into_function()?;
    let function: LuaFunction = crate::cost::engine_luau(lua, chunk)?.call((check, raw_yield.clone(), pass, missed))?;
    context.set("Checkpoint", function.clone())?;
    // Context.FrameCheckpoint(): Checkpoint for a setup that runs frames (a server's boot): on the
    // harness's own thread after it first yielded, the harness waits here and the run resumes it before
    // the next frame, in every run alike; a service may run it ahead up to there for the jobs with the
    // same `--fps` (record/prepare.rs). Before the first yield it is Checkpoint; elsewhere, and after
    // the first checkpoint, it does nothing.
    let between = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |lua, ()| {
            let own = checkpoint.harness.borrow().as_ref().is_some_and(|thread| *thread == lua.current_thread());
            if !own || checkpoint.armed.get() || checkpoint.reached.replace(true) {
                return Ok(false);
            }
            *checkpoint.unchecked.borrow_mut() = crate::files::unchecked();
            checkpoint.between.set(true);
            checkpoint.framed.set(true);
            Ok(true)
        })?
    };
    let unparked = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |_, ()| {
            checkpoint.between.set(false);
            Ok(())
        })?
    };
    let framed = lua
        .load(
            "local Armed, Before, Between, Yield, Unparked = ... \
             return function() if Armed() then return Before() end if Between() and not pcall(Yield) then Unparked() end end",
        )
        .set_name("=ree.framecheckpoint")
        .into_function()?;
    let armed = {
        let checkpoint = checkpoint.clone();
        lua.create_function(move |lua, ()| Ok(checkpoint.armed.get() && checkpoint.harness.borrow().as_ref().is_some_and(|thread| *thread == lua.current_thread())))?
    };
    let function: LuaFunction = crate::cost::engine_luau(lua, framed)?.call((armed, function, between, raw_yield, unparked))?;
    context.set("FrameCheckpoint", function)?;
    // Context.TraceWrites(Properties?, Depth?, Root?): the Luau call site of every hierarchy change and of
    // writes to the listed properties, read through the returned Drain() (trace.rs)
    let e = engine.clone();
    context.set(
        "TraceWrites",
        lua.create_function(move |lua, (properties, depth, root): (Option<Vec<String>>, Option<usize>, Option<LuaValue>)| {
            let root = root.as_ref().and_then(engine_lua::instance_id);
            crate::trace::install(lua, &e, properties.unwrap_or_default(), depth.unwrap_or(3), root)
        })?,
    )?;
    // Context.Census(Depth?): live threads, connections, handler runs, pending resumptions and instances by
    // where they came from, read through the returned Take(Collect?) (census.rs)
    let e = engine.clone();
    context.set(
        "Census",
        lua.create_function(move |lua, depth: Option<usize>| crate::census::install(lua, &e, depth.unwrap_or(2)))?,
    )?;
    // Context.ProfilePhase(Name): names the phase of the Luau profiles (RUR_LUAU_PROFILE, RUR_LUAU_ALLOC_PROFILE)
    // from here on, the first segment of each stack: `startup` before the first reply, `frames` after it
    context.set(
        "ProfilePhase",
        lua.create_function(|_, name: String| {
            crate::luau_profile::set_phase(&name);
            Ok(())
        })?,
    )?;
    // Context.SetRandomSeed(Seed): the run's random seed from here on, as REE_RANDOM_SEED sets it for the
    // whole run: math.random is reseeded with it and the next unseeded Random.new() starts its sequence
    // from it. A harness varies its runs by seed this way without a service per seed (the variable keys
    // a service); its Run calls it before the code that draws (a Prepare's draws are every run's).
    let e = engine.clone();
    context.set(
        "SetRandomSeed",
        lua.create_function(move |_, seed: f64| {
            if !seed.is_finite() {
                return Err(LuaError::runtime("Context.SetRandomSeed: a finite number expected"));
            }
            {
                let mut state = e.state.borrow_mut();
                state.sched.random_seed = seed;
                state.sched.unseeded_random = None;
            }
            crate::lua::globals::randomseed(&e)
        })?,
    )?;
    // Context.WorkSeconds(): the script work charged so far, in seconds of Studio's time (the work clock's
    // charges, cost.rs), counted whether or not a frozen clock shows them: a run's own measure of how much
    // a frame did that is the same however fast the host is
    let e = engine.clone();
    context.set("WorkSeconds", lua.create_function(move |_, ()| Ok(e.state.borrow().sched.work_total()))?)?;
    // Context.SetJoinData(Player, JoinData): a server run's (`--run-context server`) fake player arrives
    // with the join data a teleport would have given it, which Player:GetJoinData returns (lua/server.luau),
    // so a game that reads its TeleportData there joins the player as live servers do
    if let Ok(LuaValue::Function(set_join_data)) = lua.named_registry_value::<LuaValue>(crate::lua::stubs::SET_JOIN_DATA) {
        context.set("SetJoinData", set_join_data)?;
    }
    // Context.ReseedRandoms(Seed): every Random alive now (made with or without a seed, a Prepare's
    // included) restarts from Seed and the order it was made in, so a setup that made them before the
    // run had its seed (a frame checkpoint's) still draws by the run's seed; how many it restarted
    let e = engine.clone();
    context.set(
        "ReseedRandoms",
        lua.create_function(move |_, seed: f64| {
            if !seed.is_finite() {
                return Err(LuaError::runtime("Context.ReseedRandoms: a finite number expected"));
            }
            Ok(e.state.borrow_mut().sched.randoms.reseed(seed))
        })?,
    )?;
    // Context.Holders(Filters, Examples?): what keeps the trees outside the DataModel whose root reads
    // "Class Name" starting with a filter alive, as a JSON string (lua/holders.rs)
    let e = engine.clone();
    context.set(
        "Holders",
        lua.create_function(move |lua, (filters, examples, requeue): (Vec<String>, Option<usize>, Option<bool>)| {
            let report = crate::lua::holders::report(lua, &e, filters, examples.unwrap_or(2), requeue.unwrap_or(false))?;
            Ok(serde_json::to_string(&report).unwrap_or_default())
        })?,
    )?;
    // Context.Mount(Path, File): `--mount Path=File` from the harness (what a run varies, a bundle
    // say, mounted after its checkpoint); the instance at Path, nil when the mount made none there
    let e = engine.clone();
    context.set(
        "Mount",
        lua.create_function(move |lua, (path, file): (String, String)| {
            let segments: Vec<String> = path.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
            if segments.is_empty() {
                return Err(LuaError::runtime("Context.Mount: an instance path (Service/Name) expected"));
            }
            if !std::path::Path::new(&file).is_absolute() {
                return Err(LuaError::runtime(format!("Context.Mount: {file} is not an absolute path")));
            }
            let mount = project::Mount { segments: segments.clone(), target: project::MountTarget::Path(file) };
            crate::timing::time("harness: Context.Mount", || project::mount_at(&e, &mount)).map_err(|error| LuaError::runtime(format!("Context.Mount: {error}")))?;
            let game = e.world().game;
            let mut found = services::find_service(&e, game, &segments[0]);
            for segment in &segments[1..] {
                found = found.and_then(|parent| e.state.borrow().dom.find_child(parent, segment.as_bytes()));
            }
            found.map(|id| instance_value(lua, &e, id)).transpose()
        })?,
    )?;
    Ok(context)
}

/// What a harness's Run gets that its Prepare did not: the command's `--context` fields, Seconds and Fps.
fn fill_context(lua: &Lua, context: &LuaTable, late: &Late) -> LuaResult<()> {
    if let Some(Json::Object(fields)) = &late.context {
        for (key, value) in fields {
            context.set(key.as_str(), lua.to_value_with(value, context_options())?)?;
        }
    }
    context.set("Seconds", late.seconds)?;
    context.set("Fps", late.fps)
}

#[cfg(test)]
mod warm_tests {
    /// A warm-up reads its project's lists, leaves another live checkout's alone and deletes a removed checkout's
    #[test]
    fn a_warm_up_reads_its_projects_lists_and_drops_those_of_removed_checkouts() {
        let root = std::env::temp_dir().join(format!("ree-warm-lists-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let slash = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");
        let project = root.join("Project");
        let other = root.join("Other");
        let lists = root.join("record");
        for dir in [project.join("shared"), project.join("src"), project.join("external/Data/src"), other.join("src"), lists.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let ours = [slash(&project.join("src/A.luau")), slash(&project.join("shared/B.luau")), slash(&project.join("external/Data/src/C.json"))];
        let theirs = slash(&other.join("src/D.luau"));
        let gone = slash(&root.join("Removed/src/E.luau"));
        std::fs::write(lists.join("ours.txt"), format!("{}\n{}\n", ours[0], ours[1])).unwrap();
        std::fs::write(lists.join("ours-too.txt"), format!("{}\r\n{}\r\n", ours[2], ours[0])).unwrap();
        std::fs::write(lists.join("theirs.txt"), format!("{theirs}\n")).unwrap();
        std::fs::write(lists.join("gone.txt"), format!("{gone}\n")).unwrap();
        std::fs::write(lists.join("empty.txt"), "").unwrap();
        let roots = [project.join("shared"), project.join("src")].map(|dir| dir.to_string_lossy().into_owned());
        let files = super::project_module_lists(&lists, &roots);
        assert_eq!(files.into_iter().collect::<Vec<_>>(), { let mut sorted = ours.to_vec(); sorted.sort(); sorted });
        assert!(lists.join("theirs.txt").exists(), "another live checkout's list stays");
        assert!(!lists.join("gone.txt").exists(), "a removed checkout's list goes");
        assert!(lists.join("ours.txt").exists() && lists.join("empty.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `ree record --timing-check`: records the harness a second time with the in-frame clock moved (twice the modelled work, never
/// frozen: Options::clock_moved) and compares the two recordings' playback and camera. A script that times itself on
/// os.clock within a frame (a visual clock's waits, an elapsed-time sum) can draw differently in a client whenever its real
/// work moves, which ree's modelled work, or a frozen clock, does not follow: when the second recording differs, an A/B whose
/// recordings read identical has not shown the change keeps the look. The verdict goes into the summary (`timing`) and
/// `timing.json`, and the status line says from when the recording hinges on sub-frame timing; the second recording's files
/// are removed.
fn check_timing(args: &RecordArgs, host: &Host, mut done: Done) -> Done {
    let Some(out) = args.out.clone() else { return done };
    let moved_out = out.join("timing-check");
    let mut moved = args.clone();
    moved.out = Some(moved_out.clone());
    moved.timing_check = false;
    moved.clock_moved = true;
    let second = start(&moved, host, false).and_then(|(emulator, _)| {
        let result = record_in(&emulator, &moved, host);
        release(emulator);
        result
    });
    let mut verdict = match second {
        Ok(_) => compare_recordings(&out, &moved_out),
        Err(error) => json!({ "error": error }),
    };
    verdict["moved"] = json!("the in-frame clock ran on twice the modelled work, never frozen");
    let _ = std::fs::remove_dir_all(&moved_out);
    let _ = std::fs::write(out.join("timing.json"), serde_json::to_string_pretty(&verdict).unwrap_or_default());
    if let Ok(text) = std::fs::read_to_string(out.join("summary.json")) {
        if let Ok(mut summary) = serde_json::from_str::<Json>(&text) {
            summary["timing"] = verdict.clone();
            let _ = std::fs::write(out.join("summary.json"), serde_json::to_string(&summary).unwrap_or(text));
        }
    }
    match (verdict["hinges"].as_bool(), &verdict["first"]) {
        (Some(true), first) => {
            done.line.push_str(&format!(
                "; hinges on sub-frame timing from {:.3} s ({}): an identical A/B there is not a pass",
                first["at"].as_f64().unwrap_or(0.0),
                first["what"].as_str().unwrap_or("?")
            ));
        }
        (Some(false), _) => done.line.push_str("; sub-frame timing: no change"),
        _ => done.line.push_str(&format!("; timing check failed: {}", verdict["error"].as_str().unwrap_or("?"))),
    }
    done.summary["timing"] = verdict;
    done
}

/// Where two recordings of one harness part: each of playback.json's lists (actions, tweens, motion) and camera.json compared
/// entry by entry, the first difference by time (an entry's `at`, else its `start`), and how many entries differ.
pub(crate) fn compare_recordings(a: &Path, b: &Path) -> Json {
    let read = |dir: &Path, file: &str| -> Option<Json> { serde_json::from_str(&std::fs::read_to_string(dir.join(file)).ok()?).ok() };
    let (Some(first), Some(second)) = (read(a, "playback.json"), read(b, "playback.json")) else {
        return json!({ "error": "a recording has no playback.json" });
    };
    let mut lists: Vec<(String, Json, Json)> = ["actions", "tweens", "motion"].iter().map(|key| (key.to_string(), first[key].clone(), second[key].clone())).collect();
    lists.push(("camera".into(), read(a, "camera.json").unwrap_or(Json::Null), read(b, "camera.json").unwrap_or(Json::Null)));
    let time = |entry: &Json| entry["at"].as_f64().or_else(|| entry["start"].as_f64()).unwrap_or(0.0);
    // (a motion track's keys: where its first differing key is)
    let parted = |x: Option<&Json>, y: Option<&Json>| -> f64 {
        let keys = |entry: Option<&Json>| entry.and_then(|entry| entry["keys"].as_array()).cloned().unwrap_or_default();
        let (left, right) = (keys(x), keys(y));
        if left.is_empty() && right.is_empty() {
            return x.map(time).unwrap_or(f64::INFINITY).min(y.map(time).unwrap_or(f64::INFINITY));
        }
        (0..left.len().max(right.len()))
            .find(|&index| left.get(index) != right.get(index))
            .map(|index| left.get(index).map(time).unwrap_or(f64::INFINITY).min(right.get(index).map(time).unwrap_or(f64::INFINITY)))
            .unwrap_or(0.0)
    };
    let what = |entry: &Json| {
        let target = entry["target"].as_str().or_else(|| entry["path"].as_str()).unwrap_or("");
        let property = entry["property"].as_str().or_else(|| entry["kind"].as_str()).unwrap_or("");
        format!("{target} {property}").trim().to_string()
    };
    let mut differing = serde_json::Map::new();
    let mut earliest: Option<(f64, String, String)> = None;
    for (name, left, right) in lists {
        let empty = Vec::new();
        let (left, right) = (left.as_array().unwrap_or(&empty), right.as_array().unwrap_or(&empty));
        let mut count = 0usize;
        for index in 0..left.len().max(right.len()) {
            let (x, y) = (left.get(index), right.get(index));
            if x == y {
                continue;
            }
            count += 1;
            let entry = x.or(y).unwrap();
            let at = parted(x, y);
            if earliest.as_ref().is_none_or(|(when, _, _)| at < *when) {
                earliest = Some((at, name.clone(), what(entry)));
            }
        }
        differing.insert(name, json!(count));
    }
    match earliest {
        Some((at, list, what)) => json!({ "hinges": true, "first": { "at": at, "list": list, "what": what }, "differing": differing }),
        None => json!({ "hinges": false, "differing": differing }),
    }
}

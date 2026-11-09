//! Scheduler tests (ports of runtime/tests/scheduler.luau; the session stepper's are
//! roblox-ui-renderer's).

use std::rc::Rc;

use mlua::prelude::*;
use serde_json::json;

use super::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    scheduler,
};

fn start() -> (Lua, Rc<Engine>) {
    let engine = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine.state.borrow_mut().sched.set_main_thread(&lua.current_thread());
    (lua, engine)
}

fn run(lua: &Lua, engine: &Rc<Engine>, source: &str) {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        super::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    let function = lua.load(source).set_name("=Test").set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
}

fn global(engine: &Engine, name: &str) -> LuaValue {
    let shared = engine.lua.borrow().shared.clone().unwrap();
    shared.get::<LuaTable>("_G").unwrap().get(name).unwrap()
}

fn number(engine: &Engine, name: &str) -> f64 {
    match global(engine, name) {
        LuaValue::Number(n) => n,
        LuaValue::Integer(n) => n as f64,
        other => panic!("{name}: {other:?}"),
    }
}

/// A run with no profile books no charge by kind: the scheduler keeps no totals for it, and the work clock still advances.
#[test]
fn ordinary_dispatch_keeps_no_profile_kind_totals() {
    let (lua, engine) = start();
    let script = r#"
        local event = Instance.new("BindableEvent")
        event.Event:Connect(function()
            os.clock()
        end)
        event:Fire()
        task.delay(0, function()
            os.clock()
        end)
    "#;
    run(&lua, &engine, script);
    scheduler::step(&lua, &engine, None).unwrap();
    assert!(engine.state.borrow().sched.kinds.is_none());
    assert!(engine.state.borrow().sched.work_total() > 0.0);
}

/// A job in the queue is the size it was on main. Where a handler's price came from (`kind`) sits in the handler variant of
/// `JobTarget`, which has room for it; as a field of `Job` it made every queued job 8 bytes bigger.
#[test]
fn profiling_does_not_enlarge_queued_jobs() {
    // The shapes main had, before the kinds existed
    #[allow(dead_code)]
    enum MainTarget {
        Thread(LuaThread),
        Handler(super::signal::ConnectionId),
        DisconnectAll(super::InstanceId),
    }
    #[allow(dead_code)]
    struct MainJob {
        target: MainTarget,
        args: Vec<super::Value>,
        event_depth: u32,
        signal: super::signal::SignalId,
        resume_cost: f64,
    }
    // The shape this branch had before moving the byte into the handler variant: main's job and a byte
    #[allow(dead_code)]
    struct JobWithKindsByte {
        target: MainTarget,
        args: Vec<super::Value>,
        event_depth: u32,
        signal: super::signal::SignalId,
        resume_cost: f64,
        resume_kinds: u8,
    }
    let main_size = std::mem::size_of::<MainJob>();
    let with_byte_size = std::mem::size_of::<JobWithKindsByte>();
    let current_size = std::mem::size_of::<scheduler::Job>();
    assert_eq!(std::mem::size_of::<scheduler::DispatchKind>(), 1);
    println!("queued job layout: main {main_size}, previous {with_byte_size}, current {current_size}");
    assert_eq!(std::mem::size_of::<scheduler::JobTarget>(), std::mem::size_of::<MainTarget>());
    assert_eq!(current_size, main_size);
    assert_eq!(std::mem::align_of::<scheduler::Job>(), std::mem::align_of::<MainJob>());
}

#[test]
fn a_playing_clients_resumption_points_run_as_measured() {
    // Measured in a playing client (Studio 0.744, tools/parity/probes/deferred_points.luau, deferred_flush.luau): every
    // due task.wait / task.delay thread resumes first, then one pass over the deferred queue, then the Heartbeat
    // handlers; wait / spawn threads resume after PreRender, before PreAnimation, followed by a full flush. What the
    // render-step bindings defer runs after the PreRender handlers (roblox-sim's rendered and headless clients of the
    // same engine, notes/macro/orderprobe.luau).
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine.state.borrow_mut().sched.set_main_thread(&lua.current_thread());
    run(
        &lua,
        &engine,
        r#"
        local RunService = game:GetService("RunService")
        _G.Log = {}
        local function Note(Text) table.insert(_G.Log, Text) end
        local function Chain(Tag)
            Note(Tag .. "0")
            task.defer(function()
                Note(Tag .. "1")
                task.defer(function() Note(Tag .. "2") end)
            end)
        end
        for _, Name in { "PreRender", "PreAnimation", "Heartbeat" } do
            RunService[Name]:Connect(function() Note("|" .. Name) end)
        end
        task.delay(0, function() Chain("d") end)
        task.spawn(function() task.wait() Chain("w") end)
        spawn(function() Chain("s") end)
        RunService:BindToRenderStep("chain", 100, function()
            RunService:UnbindFromRenderStep("chain")
            Chain("b")
        end)
        "#,
    );
    for _ in 0..3 {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    let log: Vec<String> = match global(&engine, "Log") {
        LuaValue::Table(table) => table.sequence_values::<String>().map(Result::unwrap).collect(),
        other => panic!("{other:?}"),
    };
    let log = log.join(" ");
    assert!(log.contains("d0 w0 d1 w1 |Heartbeat d2 w2"), "{log}");
    assert!(log.contains("b0 |PreRender b1 b2 s0 s1 s2 |PreAnimation"), "{log}");
}

#[test]
fn a_playing_clients_tween_step_follows_the_prerender_handlers() {
    // Measured in roblox-sim's rendered and headless clients (Studio 0.744's engine, notes/macro/orderprobe.luau and
    // bindprobe.luau): the RenderStepped and PreRender handlers read a tween's value from before the frame's tween step, a tween
    // played in one of them or in a binding takes its first step that frame, and the tween step's Changed handlers, then what
    // the bindings (any priority) and the PreRender handlers deferred, run after it with the stepped value.
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine.state.borrow_mut().sched.set_main_thread(&lua.current_thread());
    run(
        &lua,
        &engine,
        r#"
        local RunService = game:GetService("RunService")
        local TweenService = game:GetService("TweenService")
        _G.Log = {}
        local Value = Instance.new("NumberValue")
        local function Note(Tag) table.insert(_G.Log, string.format("%s%.4f", Tag, Value.Value)) end
        local First = true
        Value.Changed:Connect(function() Note("c") end)
        RunService:BindToRenderStep("probe", 100, function()
            if First then
                task.defer(function() Note("b") end)
            end
        end)
        RunService.RenderStepped:Connect(function()
            Note("|RenderStepped ")
            if First then
                TweenService:Create(Value, TweenInfo.new(1, Enum.EasingStyle.Linear), { Value = 1 }):Play()
            end
        end)
        RunService.PreRender:Connect(function()
            Note("|PreRender ")
            if First then
                First = false
                task.defer(function() Note("p") end)
            end
        end)
        "#,
    );
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    let log: Vec<String> = match global(&engine, "Log") {
        LuaValue::Table(table) => table.sequence_values::<String>().map(Result::unwrap).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(log.join(" "), "|RenderStepped 0.0000 |PreRender 0.0000 c0.0167 b0.0167 p0.0167");
}

#[test]
fn deferred_budget_and_errors() {
    let (lua, engine) = start();
    // The emergency flush budget leaves the next job pending instead of silently losing it
    engine.state.borrow_mut().sched.max_deferred_per_flush = 2;
    run(&lua, &engine, "_G.Jobs = 0 for _ = 1, 3 do task.defer(function() _G.Jobs += 1 end) end");
    // (the call flushed once: two jobs ran)
    assert_eq!(number(&engine, "Jobs"), 2.0);
    assert!(engine.state.borrow().sched.has_deferred());
    let errors = engine.state.borrow().output.entries.iter().filter(|e| e.kind == "error").count();
    assert_eq!(errors, 1);
    scheduler::flush_deferred(&lua, &engine).unwrap();
    assert_eq!(number(&engine, "Jobs"), 3.0);
    assert!(!engine.state.borrow().sched.has_deferred());

    // A timer starts a new resumption, so yielded event handlers do not retain old depth
    engine.state.borrow_mut().sched.max_deferred_per_flush = 200_000;
    run(
        &lua,
        &engine,
        r#"
        local Timed = Instance.new("BindableEvent")
        _G.TimedFires = 0
        Timed.Event:Connect(function()
            _G.TimedFires += 1
            if _G.TimedFires < 15 then
                task.wait()
                Timed:Fire()
            end
        end)
        Timed:Fire()
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.2).unwrap();
    assert_eq!(number(&engine, "TimedFires"), 15.0);
    let errors = engine.state.borrow().output.entries.iter().filter(|e| e.kind == "error").count();
    assert_eq!(errors, 1);
}

#[test]
fn deferred_threads_survive_collected_cancelled_ones() {
    // Cancelled threads were listed by address forever, so a task.defer whose new thread was allocated where a
    // collected cancelled one had been never ran (a replay lost a third of its defers across Heartbeat:Wait)
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        _G.Ran, _G.Queued, _G.InOrder, _G.SelfResumed = 0, 0, true, false
        local Last = 0
        task.spawn(function()
            for _ = 1, 4 do
                for _ = 1, 500 do
                    task.cancel(task.defer(function() end))
                    task.cancel(coroutine.create(function() end))
                end
                task.wait()
                collectgarbage("collect")
                for _ = 1, 50 do
                    _G.Queued += 1
                    local Id = _G.Queued
                    task.defer(function()
                        _G.Ran += 1
                        if Id < Last then
                            _G.InOrder = false
                        end
                        Last = Id
                    end)
                end
                task.wait()
            end
        end)
        -- A thread cannot cancel itself: Roblox raises `cannot cancel thread` and the thread goes on (measured in a
        -- playing client, Studio 0.744, tools/parity/probes/task_order.luau)
        task.spawn(function()
            _G.SelfCancel = select(2, pcall(task.cancel, coroutine.running()))
            task.wait()
            _G.SelfResumed = true
        end)
        "#,
    );
    scheduler::run_until(&lua, &engine, 1.0).unwrap();
    assert_eq!(number(&engine, "Queued"), 200.0);
    assert_eq!(number(&engine, "Ran"), 200.0);
    assert_eq!(global(&engine, "InOrder"), LuaValue::Boolean(true));
    assert_eq!(global(&engine, "SelfResumed"), LuaValue::Boolean(true));
    assert_eq!(global(&engine, "SelfCancel").to_string().unwrap(), "cannot cancel thread");
}

#[test]
fn recorded_clock_capture_and_clocks() {
    let (lua, engine) = start();
    // A version 2 clock: Studio's os.clock at the mount (the build took 10 ms), two frames, then the
    // capture of a task.wait(0.05) called at the mount's end
    let clock = json!({
        "version": 2, "time": 0.05, "seed": 7,
        "mount": { "start": 500.0, "end": 500.01, "instances": 0 },
        "unix": 1790000000000.0_f64, "tick": 1790003600.0_f64, "server": 1789999998.0_f64, "elapsed": 509.5,
        "events": [
            ["t", 500.012], ["h", 0.004, 500.0121], ["r", 0.016, 500.02],
            ["t", 500.03], ["h", 0.018, 500.0301], ["r", 0.02, 500.04],
            ["t", 500.061], ["c", 500.061, 500.01, 0.051]
        ]
    });
    scheduler::use_recorded_clock(&engine, Some(&clock)).unwrap();
    {
        let state = engine.state.borrow();
        assert!(state.sched.tracks_build() && state.sched.has_capture_point());
        assert!((state.sched.now + 0.005).abs() < 1e-12, "virtual time starts half a build before the mount end");
    }
    run(
        &lua,
        &engine,
        r#"
        _G.Seeded = math.random()
        _G.Tick = tick()
        _G.Elapsed = elapsedTime()
        task.delay(0.02, function() _G.DelayAt = os.clock() end)
        task.delay(1, function() _G.Late = true end)
        "#,
    );
    // math.random restarted from the recorded seed
    let expected: f64 = lua.load("math.randomseed(7) return math.random()").eval().unwrap();
    assert_eq!(number(&engine, "Seeded"), expected);
    assert!((number(&engine, "Tick") - (1790003600.0 - 0.005)).abs() < 1e-3);
    scheduler::run_until(&lua, &engine, 10.0).unwrap();
    let state = engine.state.borrow();
    assert!(state.sched.captured, "the run ends at Studio's capture");
    let status = state.sched.replay_status().unwrap();
    assert_eq!((status.events, status.replayed, status.frames), (8, 8, 2));
    drop(state);
    // The delay placed at the build's midpoint (-5 ms) + 20 ms is due at 15 ms: the second
    // resumption phase (Studio 500.03 = 20 ms) ran it
    let delay_at = number(&engine, "DelayAt");
    assert!((delay_at - 500.03).abs() < 1e-9, "{delay_at}");
    assert!(matches!(global(&engine, "Late"), LuaValue::Nil));
}

/// The script of the split-frame replay below. The second Heartbeat marks the phase `second` part way through, so that
/// frame ran in two phases. The timer marks `capture-only` after the last frame, in a step that renders nothing.
const SPLIT_FRAME_SOURCE: &str = r#"
local First = Instance.new("Part")
local Second = Instance.new("Part")
local Frames = 0

game:GetService("RunService").Heartbeat:Connect(function()
    Frames += 1
    for _ = 1, 2000 do
        local Position = First.Position
    end
    if Frames == 2 then
        _G.MarkPhase("second")
        for _ = 1, 2000 do
            local Position = Second.Position
        end
    end
end)

task.delay(0.045, function()
    _G.MarkPhase("capture-only")
end)
"#;

/// Two rendered frames, then a last step that resumes a timer and reaches the capture without rendering. An event is
/// `["t", time]` (timers resume), `["h", delta, time]` (Heartbeat), `["r", delta, time]` (render) or `["c", ...]` (capture).
fn split_frame_clock() -> serde_json::Value {
    json!({
        "version": 2, "time": 0.05,
        "mount": { "start": 500.0, "end": 500.0, "instances": 0 },
        "events": [
            ["t", 500.012], ["h", 0.004, 500.0121], ["r", 0.016, 500.02],
            ["t", 500.03], ["h", 0.018, 500.0301], ["r", 0.02, 500.04],
            ["t", 500.061], ["c", 500.061, 500.0, 0.061]
        ]
    })
}

/// What one split-frame replay should write to boundary.json for the phase it was asked to report
struct SplitFrameCase {
    /// the phase named by `--profile-phase`; comma separated phases are reported together
    reported_phase: &'static str,
    frames: u64,
    position_reads_per_frame: f64,
    distinct_per_frame: f64,
    repeats_per_frame: f64,
}

/// How many frames boundary.json counts for a phase, whichever phase is reported
struct PhaseFrames {
    phase: &'static str,
    frames: u64,
}

/// Replays `split_frame_clock` to its capture with the boundary profile reporting `reported_phase`. Returns the
/// profile's text and its boundary.json.
fn replay_split_frame_to_capture(reported_phase: &str) -> (String, serde_json::Value) {
    let (lua, engine) = start();
    engine_lua::modules::interpret_all(&lua, &engine);
    scheduler::use_recorded_clock(&engine, Some(&split_frame_clock())).unwrap();

    let mark_phase = lua.create_function(|_, name: String| {
        crate::luau_profile::set_phase(&name);
        Ok(())
    }).unwrap();
    let shared = engine.lua.borrow().shared.clone().unwrap();
    let globals: LuaTable = shared.get("_G").unwrap();
    globals.set("MarkPhase", mark_phase).unwrap();

    let unique_name = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let report_path = std::env::temp_dir().join(format!("ree-boundary-capture-{}-{unique_name}.json", std::process::id()));
    crate::boundary::begin(crate::boundary::Config { out: report_path.clone(), top: 5, phase: Some(reported_phase.to_string()) });
    crate::luau_profile::install(&lua);
    crate::luau_profile::set_phase("first");
    run(&lua, &engine, SPLIT_FRAME_SOURCE);
    scheduler::run_until(&lua, &engine, 10.0).unwrap();
    let text = crate::boundary::finish().expect("report");

    let state = engine.state.borrow();
    assert!(state.sched.captured);
    let replay = state.sched.replay_status().unwrap();
    assert_eq!(replay.events, 8);
    assert_eq!(replay.replayed, 8);
    assert_eq!(replay.frames, 2);

    let bytes = std::fs::read(&report_path).expect("report file");
    std::fs::remove_file(&report_path).expect("remove report file");
    (text, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn recorded_capture_counts_both_phases_of_a_split_frame() {
    let cases = [
        SplitFrameCase { reported_phase: "first", frames: 2, position_reads_per_frame: 2000.0, distinct_per_frame: 1.0, repeats_per_frame: 1999.0 },
        SplitFrameCase { reported_phase: "second", frames: 1, position_reads_per_frame: 2000.0, distinct_per_frame: 1.0, repeats_per_frame: 1999.0 },
        // (both phases together: 2000 reads in frame 1 and 4000 in frame 2, so 3000 a frame over the two frames)
        SplitFrameCase { reported_phase: "first,second", frames: 2, position_reads_per_frame: 3000.0, distinct_per_frame: 1.5, repeats_per_frame: 2998.5 },
    ];
    // `startup` ran before any frame, and `capture-only` ran after the last one: neither is in a frame
    let frames_per_phase = [
        PhaseFrames { phase: "startup", frames: 0 },
        PhaseFrames { phase: "first", frames: 2 },
        PhaseFrames { phase: "second", frames: 1 },
        PhaseFrames { phase: "capture-only", frames: 0 },
    ];
    for case in cases {
        let wanted = case.reported_phase;
        let (text, report) = replay_split_frame_to_capture(wanted);

        assert_eq!(report["frames"], case.frames, "{wanted}: {text}");
        let phases = report["phases"].as_array().unwrap();
        for expected in &frames_per_phase {
            let phase = phases.iter().find(|phase| phase["phase"] == expected.phase).unwrap();
            assert_eq!(phase["frames"], expected.frames, "{wanted}: {}", expected.phase);
        }
        let members = report["by_member"].as_array().unwrap();
        let position = members.iter().find(|member| member["member"] == "BasePart.Position").unwrap();
        assert_eq!(position["reads_per_frame"], case.position_reads_per_frame, "{wanted}");
        assert_eq!(position["distinct_per_frame"], case.distinct_per_frame, "{wanted}");
        assert_eq!(position["same_call_repeats_per_frame"], case.repeats_per_frame, "{wanted}");
        println!(
            "recorded capture {wanted}: {} frames, {} reads, {} distinct, {} repeats/frame",
            case.frames, case.position_reads_per_frame, case.distinct_per_frame, case.repeats_per_frame
        );
    }
}

/// One erroring Heartbeat handler does not stop the others, in that frame or later ones, and
/// stays connected (as in Roblox: every handler runs on its own thread and its error is reported).
/// A library that steps all its springs in one handler (Vide) still loses the springs after the
/// failing one for that step; that is the library's own loop.
#[test]
fn an_erroring_heartbeat_handler_does_not_stop_the_others() {
    let (lua, engine) = start();
    run(
        &lua,
        &engine,
        r#"
        local RunService = game:GetService("RunService")
        _G.Before, _G.Failing, _G.After, _G.Render = 0, 0, 0, 0
        RunService.Heartbeat:Connect(function() _G.Before += 1 end)
        RunService.Heartbeat:Connect(function()
            _G.Failing += 1
            error("spring step failed")
        end)
        RunService.Heartbeat:Connect(function() _G.After += 1 end)
        RunService.RenderStepped:Connect(function() _G.Render += 1 end)
        "#,
    );
    scheduler::run_until(&lua, &engine, 0.5).unwrap();
    let (before, failing, after, render) = (number(&engine, "Before"), number(&engine, "Failing"), number(&engine, "After"), number(&engine, "Render"));
    assert!(failing >= 20.0 && render >= 20.0, "{failing} {render}");
    assert_eq!((before, after), (failing, failing));
    let errors = engine.state.borrow().output.entries.iter().filter(|entry| entry.kind == "error" && entry.message.contains("spring step failed")).count();
    assert_eq!(errors as f64, failing, "every failure is reported");
}

//! Executed code, rather than a span between two program counters, determines a frame's cold charge.

use super::*;

const BODY: usize = 300;
const FRAMES: u32 = 4;

/// A setup frame lets the threads reach their waits before each measured frame gets its own phase.
fn frame_profile(source: &str) -> serde_json::Value {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).unwrap();
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).unwrap();
    engine_lua::modules::interpret_all(&lua, &engine);
    engine.state.borrow_mut().sched.frozen_clock = false;
    let config = Config { print: false, ..Config::new(Kinds::WORK, temp_dir("cold-frames")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    run(&lua, &engine, "cold-frames", source);
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    for frame in 1..=FRAMES {
        profiler::phase(&format!("frame{frame}"));
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: u64::from(FRAMES), ..Default::default() }).unwrap();
    let json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    std::fs::remove_dir_all(&config.out).unwrap();
    json
}

/// How many instructions the VM has counted as first dispatched in their frame
fn cold_code(lua: &Lua) -> u64 {
    unsafe { (*crate::cost::lua_workcounters(main_state(lua))).coldcode }
}

/// How many functions the VM has counted as running first in a frame without having run in the one before
fn rare_protos(lua: &Lua) -> u64 {
    unsafe { (*crate::cost::lua_workcounters(main_state(lua))).hostops[crate::cost::RARE_PROTOS_SLOT] }
}

fn close(actual: f64, expected: f64, context: &str) {
    assert!((actual - expected).abs() < 1e-10, "{context}: expected {expected}, got {actual}");
}

#[test]
fn newly_loaded_code_pays_once_even_before_the_first_render() {
    let _lock = lock();
    let (lua, _engine) = start();
    let leaf: LuaFunction = lua.load("return function(X) return X + 1 end").eval().unwrap();
    let cold = || cold_code(&lua);
    let rare = || rare_protos(&lua);
    let rare_before = rare();
    let before = cold();
    assert_eq!(leaf.call::<i32>(1).unwrap(), 2);
    assert_eq!(cold() - before, 2, "ADDK and RETURN ran");
    let before = cold();
    assert_eq!(leaf.call::<i32>(2).unwrap(), 3);
    assert_eq!(cold(), before, "the same two instructions are warm");
    crate::cost::cold_frame(&lua);
    assert_eq!(leaf.call::<i32>(3).unwrap(), 4);
    assert_eq!(cold() - before, 2, "the next frame is cold again");
    assert_eq!(rare(), rare_before, "code that ran in its creation frame is not rare in the next frame");
    crate::cost::cold_frame(&lua);
    crate::cost::cold_frame(&lua);
    assert_eq!(leaf.call::<i32>(4).unwrap(), 5);
    assert_eq!(rare() - rare_before, 1, "missing a frame makes its next call rare once");
}

#[test]
fn breakpoint_changes_preserve_whether_an_instruction_ran() {
    let _lock = lock();
    let (lua, _engine) = start();
    let leaf: LuaFunction = lua.load("local M = {}\nfunction M.Leaf(X)\nreturn X + 1\nend\nreturn M.Leaf").eval().unwrap();
    let breakpoint = |enabled| unsafe {
        lua.exec_raw::<()>(&leaf, |state| {
            assert_eq!(mlua::ffi::lua_breakpoint(state, -1, 3, enabled), 3);
        }).unwrap();
    };
    let cold = || cold_code(&lua);
    breakpoint(1);
    breakpoint(0);
    let before = cold();
    assert_eq!(leaf.call::<i32>(1).unwrap(), 2);
    assert_eq!(cold() - before, 2);
    breakpoint(1);
    assert_eq!(leaf.call::<i32>(2).unwrap(), 3);
    assert_eq!(cold() - before, 2, "a breakpoint on warm code does not make it cold");
    crate::cost::cold_frame(&lua);
    breakpoint(0);
    let before = cold();
    assert_eq!(leaf.call::<i32>(3).unwrap(), 4);
    assert_eq!(cold() - before, 2, "removing a breakpoint on cold code does not make it warm");
}

#[test]
fn tracking_can_restart_after_prototypes_are_collected() {
    let _lock = lock();
    let (lua, _engine) = start();
    for _ in 0..3 {
        let leaf: LuaFunction = lua.load("return function(X) return X + 1 end").eval().unwrap();
        let cold = || cold_code(&lua);
        let before = cold();
        assert_eq!(leaf.call::<i32>(1).unwrap(), 2);
        assert_eq!(cold() - before, 2);
        crate::cost::cold_tracking(&lua, false);
        let before = cold();
        assert_eq!(leaf.call::<i32>(2).unwrap(), 3);
        assert_eq!(cold(), before);
        crate::cost::cold_tracking(&lua, true);
        let before = cold();
        assert_eq!(leaf.call::<i32>(3).unwrap(), 4);
        assert_eq!(cold() - before, 2);
        drop(leaf);
        lua.gc_collect().unwrap();
        crate::cost::cold_frame(&lua);
    }
}

#[test]
fn marking_a_loaded_engine_chunk_revokes_its_cold_code() {
    let _lock = lock();
    let (lua, _engine) = start();
    let function = lua.load("return function(X) local function Inner(Y) return Y + 1 end return Inner(X) end").eval().unwrap();
    let function = crate::cost::engine_luau(&lua, function).unwrap();
    let cold = || cold_code(&lua);
    for _ in 0..3 {
        let before = cold();
        assert_eq!(function.call::<i32>(1).unwrap(), 2);
        assert_eq!(cold(), before, "the host prices its engine chunk as an operation");
        crate::cost::cold_frame(&lua);
    }
}

#[test]
fn a_binding_helpers_code_belongs_to_its_executing_frame() {
    let _lock = lock();
    let (lua, _engine) = start();
    let helper: LuaFunction = lua.load("local M = {} function M.Helper(X) return X + 1 end return M.Helper").set_name("=__mlua.cold-test").eval().unwrap();
    let caller: LuaFunction = lua.load("return function(Helper) return Helper(1) end").set_name("=cold-caller").eval().unwrap();
    // Price only code so the objects' first touches cannot obscure which frame owns it.
    let mut prices = vec![0.0; std::mem::size_of::<crate::cost::VmCounters>() / 8];
    prices[std::mem::offset_of!(crate::cost::VmCounters, coldcode) / 8] = crate::cost::LUAU_COLD_CODE * 1e-9;
    let state = main_state(&lua);
    assert!(unsafe { vm::start(state, true, Some(&prices), None) });
    unsafe { vm::split(state, &crate::cost::slot_kinds(), std::ptr::null()) };
    assert_eq!(caller.call::<i32>(helper).unwrap(), 2);
    unsafe { vm::stop(state) };
    let results = unsafe { vm::results(state) };
    unsafe { vm::free(state) };
    let function = results.functions.iter().position(|function| function.name == "Helper").unwrap();
    let helper = &results.functions[function];
    assert!(helper.binding);
    assert_eq!(helper.counts.iter().sum::<u64>(), 2, "ADDK and RETURN");
    let expected = 2.0 * crate::cost::LUAU_COLD_CODE * 1e-9;
    close(helper.split.chunks(vm::SPLITS).map(|split| split[vm::COLD]).sum(), expected, "helper's cold column");
    close(helper.exclusive.iter().sum(), expected, "helper's saving");
    let owned: f64 = results.nodes.iter().filter(|node| node.function == function as i32).map(|node| node.split[vm::COLD]).sum();
    close(owned, expected, "helper's call-tree nodes");
}

/// What the filled body's lines did in one frame: their dispatches, cold charges and work
struct BodyFigures {
    insns: u64,
    cold: f64,
    work: f64,
}

/// The figures of `function`'s lines `first..first + lines` in the phase `phase`
fn body_figures(profile: &serde_json::Value, function: i64, phase: &str, first: i64, lines: usize) -> BodyFigures {
    let mut figures = BodyFigures { insns: 0, cold: 0.0, work: 0.0 };
    for line in profile["lines"].as_array().unwrap() {
        let number = line["line"].as_i64().unwrap();
        let in_body = (first..first + lines as i64).contains(&number);
        if line["function"].as_i64() == Some(function) && in_body {
            let in_phase = &line["phases"][phase];
            figures.insns += in_phase["insns"].as_u64().unwrap_or(0);
            figures.cold += in_phase["cold_work_ms"].as_f64().unwrap_or(0.0);
            figures.work += in_phase["work_ms"].as_f64().unwrap_or(0.0);
        }
    }
    figures
}

/// The cold work of every function but `owner` in `phase`
fn cold_outside(profile: &serde_json::Value, owner: &serde_json::Value, phase: &str) -> f64 {
    let mut cold = 0.0;
    for function in profile["functions"].as_array().unwrap() {
        if function["id"] != owner["id"] {
            cold += function["phases"][phase]["cold_work_ms"].as_f64().unwrap_or(0.0);
        }
    }
    cold
}

/// Each body line compiles to one ADDK. Compare its exact dispatches and cold charges in each frame;
/// the paired empty body also checks that self work and saving belong to Probe rather than a helper.
/// `executions` is how many times the body ran in each frame.
fn check(template: &str, executions: [u32; FRAMES as usize]) {
    check_body(template, executions, BODY);
}

fn check_body(template: &str, executions: [u32; FRAMES as usize], lines: usize) {
    // Keep one common increment before the varied body. It retains X's initialization in
    // both versions and pays any first-touch charges when a suspended caller continues.
    let template = template.replace("$body", "X += 1\n$body");
    let lines_before_body = template[..template.find("$body").unwrap()].bytes().filter(|&byte| byte == b'\n').count() as i64;
    let first = lines_before_body + 1;
    let filled = frame_profile(&template.replace("$body", &"X += 1\n".repeat(lines)));
    // Errors include their line number in the allocated message. Keep it identical too.
    let empty = frame_profile(&template.replace("$body", &"\n".repeat(lines)));
    let probe = function_named(&filled, "Probe");
    let empty_probe = function_named(&empty, "Probe");
    let id = probe["id"].as_i64().unwrap();
    let add = (0..=u8::MAX).find(|&op| vm::opcode_name(op) == "ADDK").unwrap();
    for (index, runs) in executions.into_iter().enumerate() {
        let phase = format!("frame{}", index + 1);
        let body = body_figures(&filled, id, &phase, first, lines);
        let expected_insns = lines as u64 * u64::from(runs);
        let expected_cold = if runs == 0 { 0.0 } else { lines as f64 * crate::cost::LUAU_COLD_CODE * 1e-6 };
        let expected_work = expected_insns as f64 * crate::cost::INTERPRETED_OPS[add as usize] * 1e-6 + expected_cold;
        assert_eq!(body.insns, expected_insns, "{phase}: {template}");
        close(body.cold, expected_cold, &format!("{phase} body cold: {template}"));
        close(body.work, expected_work, &format!("{phase} body work: {template}"));
        // Probe gains exactly the body's figures over the empty version
        for (figure, expected) in [("cold_work_ms", expected_cold), ("self_work_ms", expected_work), ("saving_ms", expected_work)] {
            let delta = probe["phases"][&phase][figure].as_f64().unwrap_or(0.0) - empty_probe["phases"][&phase][figure].as_f64().unwrap_or(0.0);
            close(delta, expected, &format!("{phase} {figure}: {template}"));
        }
        let other_cold_gained = cold_outside(&filled, probe, &phase) - cold_outside(&empty, empty_probe, &phase);
        close(other_cold_gained, 0.0, &format!("{phase}: no body cold work belongs to another function: {template}"));
    }
}

/// Probe waits in an endless loop after each run of its body, in the way `$wait` says
const WAITING_PROBE: &str = r#"
local M = {}
$helper
function M.Probe()
	local X = 0
	while true do
		$body
		$wait
	end
end
task.spawn(M.Probe)
"#;

#[test]
fn waits_and_non_inlined_helpers_charge_the_body_in_every_frame() {
    let _lock = lock();
    for (helper, wait) in [
        ("", "task.wait()"),
        ("", "game:GetService(\"RunService\").Heartbeat:Wait()"),
        ("local function Sleep() task.wait() end", "Sleep()"),
        ("local function Inner() task.wait() end local function Sleep() Inner() end", "Sleep()"),
        ("local function Sleep() task.wait() end", "pcall(Sleep)"),
        ("function M.Sleep() task.wait() end", "M.Sleep()"),
    ] {
        let source = WAITING_PROBE.replace("$helper", helper).replace("$wait", wait);
        check(&source, [1; FRAMES as usize]);
    }
}

/// Probe runs its body, then fails; `$call` runs Probe from every Heartbeat
const FAILING_PROBE: &str = r#"
local M = {}
function M.Handler()
	return 1
end
function M.Probe()
	local X = 0
	$body
	$raise
end
game:GetService("RunService").Heartbeat:Connect(function()
	$call
end)
"#;

#[test]
fn errors_charge_the_body_before_the_handler_runs() {
    let _lock = lock();
    for (raise, call) in [
        ("error(\"expected\")", "pcall(M.Probe)"),
        ("error(\"expected\")", "xpcall(M.Probe, M.Handler)"),
        ("local T = nil T.Field = X", "pcall(M.Probe)"),
        ("error(\"expected\")", "coroutine.resume(coroutine.create(M.Probe))"),
        ("error(\"expected\")", "task.spawn(M.Probe)"),
    ] {
        let source = FAILING_PROBE.replace("$raise", raise).replace("$call", call);
        check(&source, [1; FRAMES as usize]);
    }
}

#[test]
fn a_yielding_coroutine_and_generic_iterator_keep_the_bodys_charge() {
    let _lock = lock();
    let yielding_coroutine = r#"
local M = {}
function M.Probe()
	local X = 0
	while true do
		$body
		coroutine.yield()
	end
end
local Co = coroutine.create(M.Probe)
game:GetService("RunService").Heartbeat:Connect(function()
	coroutine.resume(Co)
end)
"#;
    check(yielding_coroutine, [1; FRAMES as usize]);
    let generic_iterator = r#"
local M = {}
function M.Iter()
	task.wait()
	return true
end
function M.Probe()
	local X = 0
	for _ in M.Iter do
		$body
	end
end
task.spawn(M.Probe)
"#;
    check(generic_iterator, [1; FRAMES as usize]);
}

/// Two threads run Probe, and only the one started with mode 3 ever reaches the body, in a branch the others skip. `$wait` suspends a
/// thread and `$start` starts the threads.
const SUSPENDED_PROBES: &str = r#"
local M = {}
function M.Sleep()
	$wait
end
function M.Probe(Mode)
	local X = 0
	if Mode == 1 then
		while true do
			M.Sleep()
		end
	elseif Mode == 3 then
		$body
	else
		while true do
			M.Sleep()
		end
	end
end
$start
"#;

#[test]
fn suspended_threads_never_charge_the_branch_between_them() {
    let _lock = lock();
    let resume_both = r#"
local A, B = coroutine.create(M.Probe), coroutine.create(M.Probe)
coroutine.resume(A, 1)
coroutine.resume(B, 2)
game:GetService("RunService").Heartbeat:Connect(function()
	coroutine.resume(A)
	coroutine.resume(B)
end)
"#;
    for (wait, start) in [("task.wait()", "task.spawn(M.Probe, 1)\ntask.spawn(M.Probe, 2)"), ("coroutine.yield()", resume_both)] {
        let source = SUSPENDED_PROBES.replace("$wait", wait).replace("$start", start);
        check(&source, [0; FRAMES as usize]);
    }
}

#[test]
fn reentry_through_another_function_charges_both_frames_prefixes() {
    let _lock = lock();
    let template = r#"
local M = {}
function M.Sleep()
	$wait
end
function M.G()
	M.Probe(1)
end
function M.Probe(Mode)
	local X = 0
	$body
	if Mode == 0 then
		M.Sleep()
		M.G()
	else
		M.Sleep()
	end
end
local Co = coroutine.create(M.Probe)
local Count = 0
game:GetService("RunService").Heartbeat:Connect(function()
	Count += 1
	$start
end)
"#;
    for (wait, start) in [
        ("task.wait()", "if Count == 2 then task.spawn(M.Probe, 0) end"),
        ("coroutine.yield()", "if Count == 2 then coroutine.resume(Co, 0) elseif Count > 2 and coroutine.status(Co) ~= \"dead\" then coroutine.resume(Co) end"),
    ] {
        check(&template.replace("$wait", wait).replace("$start", start), [1, 1, 0, 0]);
    }
}

/// Two coroutines run the same Probe, resumed from every Heartbeat
const TWO_COROUTINES: &str = r#"
local M = {}
function M.Probe()
	local X = 0
	while true do
		$body
		coroutine.yield()
	end
end
local A, B = coroutine.create(M.Probe), coroutine.create(M.Probe)
game:GetService("RunService").Heartbeat:Connect(function()
	coroutine.resume(A)
	coroutine.resume(B)
end)
"#;

#[test]
fn two_coroutines_share_the_same_executed_code_once_a_frame() {
    let _lock = lock();
    // (each runs the body once a frame, and its instructions are dispatched twice)
    check(TWO_COROUTINES, [2; FRAMES as usize]);
}

#[test]
fn large_bodies_keep_their_work_and_dead_branches_keep_none() {
    let _lock = lock();
    const LARGE_BODY: usize = 30_000;
    let caught_error = r#"
local M = {}
function M.Handler()
	return 1
end
function M.Probe()
	local X = 0
	$body
	error("expected")
end
game:GetService("RunService").Heartbeat:Connect(function()
	xpcall(M.Probe, M.Handler)
end)
"#;
    check_body(caught_error, [1; FRAMES as usize], LARGE_BODY);
    let generic_iterator = r#"
local M = {}
function M.Iter()
	task.wait()
	return true
end
function M.Probe()
	local X = 0
	for _ in M.Iter do
		$body
	end
end
task.spawn(M.Probe)
"#;
    check_body(generic_iterator, [1; FRAMES as usize], LARGE_BODY);
    let dead_branch = r#"
local M = {}
function M.Sleep()
	coroutine.yield()
end
function M.Probe(Mode)
	local X = 0
	if Mode == 1 then
		while true do
			M.Sleep()
		end
	elseif Mode == 3 then
		$body
	else
		while true do
			M.Sleep()
		end
	end
end
local A, B = coroutine.create(M.Probe), coroutine.create(M.Probe)
coroutine.resume(A, 1)
coroutine.resume(B, 2)
game:GetService("RunService").Heartbeat:Connect(function()
	coroutine.resume(A)
	coroutine.resume(B)
end)
"#;
    check_body(dead_branch, [0; FRAMES as usize], LARGE_BODY);
}

fn frame_mark(json: &serde_json::Value, frame: u32) -> &serde_json::Value {
    if frame > FRAMES {
        &json["end"]
    } else {
        let phase = format!("frame{frame}");
        json["marks"].as_array().unwrap().iter().find(|mark| mark["phase"] == phase).unwrap()
    }
}

#[test]
fn buffer_sizes_count_every_unpriced_byte_in_its_frame() {
    let _lock = lock();
    for size in [1024u64, 1025, 65_536, 65_537, 262_144] {
        let source = r#"
local A, B = buffer.create($size), buffer.create($size)
local S = string.rep("a", $size)
game:GetService("RunService").Heartbeat:Connect(function()
	buffer.copy(A, 0, B)
	buffer.fill(A, 0, 7)
	buffer.writestring(A, 0, S)
	local C = buffer.create($size)
end)
"#;
        let json = frame_profile(&source.replace("$size", &size.to_string()));
        for frame in 1..=FRAMES {
            let before = &frame_mark(&json, frame)["vm"];
            let after = &frame_mark(&json, frame + 1)["vm"];
            for (counter, expected) in [
                ("buffer_big_bytes", if size > 65_536 { 3 * size } else { 0 }),
                ("buffer_fresh_bytes", if size > 1024 { size } else { 0 }),
            ] {
                let counted = after[counter].as_u64().unwrap() - before[counter].as_u64().unwrap();
                assert_eq!(counted, expected, "{counter}, size {size}, frame {frame}");
            }
        }
    }
}

#[test]
fn only_handlers_that_run_count_toward_each_frames_dispatch() {
    let _lock = lock();
    let return_op = (0..=u8::MAX).find(|&op| vm::opcode_name(op) == "RETURN").unwrap();
    for count in 1..=3 {
        let mut source = "local RS = game:GetService(\"RunService\")\nlocal M = {}\n".to_owned();
        for handler in 1..=count {
            source.push_str(&format!("function M.Handler{handler}() end\nRS.Heartbeat:Connect(M.Handler{handler})\n"));
        }
        source.push_str("local Co = task.spawn(function() RS.Heartbeat:Wait() end)\ntask.cancel(Co)\n");
        let json = frame_profile(&source);
        let dispatch_ms = f64::from(count - 1) * crate::cost::FRAME_HANDLER * 1e3;
        let return_ms = f64::from(count) * crate::cost::INTERPRETED_OPS[return_op as usize] * 1e-6;
        for frame in 1..=FRAMES {
            let phase = format!("frame{frame}");
            let mut hot = 0.0;
            for handler in 1..=count {
                let function = function_named(&json, &format!("Handler{handler}"));
                let figures = &function["phases"][&phase];
                assert_eq!(figures["calls"].as_u64(), Some(1));
                assert_eq!(figures["insns"].as_u64(), Some(1));
                hot += figures["self_work_ms"].as_f64().unwrap() - figures["cold_work_ms"].as_f64().unwrap() - figures["collector_ms"].as_f64().unwrap();
            }
            close(hot, return_ms + dispatch_ms, &format!("{count} handlers, {phase}"));
        }
        let sites = json["handlers"].as_array().unwrap();
        assert_eq!(sites.len(), count as usize);
        let dispatch: f64 = sites.iter().map(|site| site["dispatch_ms"].as_f64().unwrap()).sum();
        close(dispatch, dispatch_ms * f64::from(FRAMES + 1), "setup and measured dispatches");
    }
}

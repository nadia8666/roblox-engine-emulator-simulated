//! The allocation profile (`RUR_LUAU_ALLOC_PROFILE`): bytes and counts by Luau stack, checked against
//! what the VM itself reports allocating (`collectgarbage("count")` with the collector stopped).

use std::collections::HashMap;

use mlua::prelude::*;

use crate::luau_profile;

/// Each function allocates a known amount; line numbers matter (`lines` allocates on 10 and 11).
const PROBE: &str = "collectgarbage('stop')\n\
local keep = {}\n\
local function measure(f) local before = collectgarbage('count') f() return (collectgarbage('count') - before) * 1024 end\n\
local function tables() for i = 1, 3000 do keep[1] = {i, i, i, i} end end\n\
local function growth() local t = {} for i = 1, 20000 do t[i] = i end end\n\
local function strings() for i = 1, 400 do keep[1] = string.rep('x', 500 + i) end end\n\
local function depth(n) if n > 0 then keep[1] = {n} depth(n - 1) end end\n\
local function lines()\n\
  for i = 1, 1000 do\n\
    keep[1] = {i}\n\
    keep[2] = {i, i, i, i, i, i, i, i}\n\
  end\n\
end\n\
measured = {tables = measure(tables), growth = measure(growth), strings = measure(strings), depth = measure(function() depth(5000) end), lines = measure(lines)}\n";

struct Profile {
    measured: HashMap<String, f64>,
    bytes: Vec<(String, u64)>,
    counts: Vec<(String, u64)>,
}

fn read(path: &std::path::Path) -> Vec<(String, u64)> {
    let text = std::fs::read_to_string(path).expect("profile file");
    text.lines()
        .map(|line| {
            let (stack, weight) = line.rsplit_once(' ').expect("folded line");
            (stack.to_string(), weight.parse().expect("weight"))
        })
        .collect()
}

/// Runs [`PROBE`] under an allocation profile taking one sample per `step` bytes.
fn profile(step: u64) -> Profile {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("ree-alloc-profile-{}-{step}-{stamp}", std::process::id()));
    let lua = Lua::new();
    luau_profile::begin_allocations(path.clone(), step);
    let guard = luau_profile::ProfileGuard;
    luau_profile::hook_allocations(&lua);
    lua.load(PROBE).set_name("=probe").exec().expect("probe");
    drop(guard);
    let measured = lua.globals().get::<LuaTable>("measured").expect("measured").pairs::<String, f64>().map(|pair| pair.unwrap()).collect();
    let counts_path = std::path::PathBuf::from(format!("{}.counts", path.display()));
    let profile = Profile { measured, bytes: read(&path), counts: read(&counts_path) };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&counts_path);
    profile
}

/// The bytes the probe's function `name` allocated: what it holds afterwards (`measured`, with the collector stopped), but
/// for `growth`, whose array part grows into a new block each time before the old one is freed (DFFlagLuauTableRobustOom,
/// as a client runs): every size it passed through, twice what it ends with.
fn allocated(profile: &Profile, name: &str) -> f64 {
    let measured = profile.measured[name];
    if name == "growth" { measured * 2.0 } else { measured }
}

/// What the profile attributes to stacks passing through the function `name` of the probe.
fn within(rows: &[(String, u64)], name: &str) -> f64 {
    let frame = format!(";{name} probe:");
    rows.iter().filter(|(stack, _)| stack.contains(&frame)).map(|row| row.1 as f64).sum()
}

#[test]
fn every_allocation_is_attributed_to_its_stack() {
    let exact = profile(1);
    for name in ["tables", "growth", "strings", "depth", "lines"] {
        let (attributed, measured) = (within(&exact.bytes, name), allocated(&exact, name));
        assert!(measured > 10_000.0, "{name} allocates {measured}");
        // (bytes allocated, not net: string.rep's buffer shrinks to the string, which the net count nets off, 1.2 %)
        assert!((attributed - measured).abs() <= measured * 0.02, "{name}: {attributed} attributed, {measured} allocated");
    }
    // one table and one array per constructor
    assert_eq!(within(&exact.counts, "tables"), 6000.0, "{:?}", exact.counts);
    // a native function's allocations end the stack in it, under the script that called it
    let rep: u64 = exact.bytes.iter().filter(|(stack, _)| stack.contains(";strings probe:6 @6;rep [C]")).map(|row| row.1).sum();
    assert!(rep as f64 > exact.measured["strings"] * 0.99, "{rep} of {:?}", exact.bytes);
    // lines name the allocating line of their function
    let at = |line: u32| -> u64 { exact.bytes.iter().filter(|(stack, _)| stack.contains(&format!(";lines probe:8 @{line}"))).map(|row| row.1).sum() };
    assert!(at(10) > 0 && at(11) > at(10) * 2, "{} {}", at(10), at(11));
    // deep recursion grows the thread's stack and call-info array while the walk reads them
    assert!(within(&exact.counts, "depth") >= 5000.0);
    assert!(exact.bytes.iter().all(|(stack, _)| stack.starts_with("startup")));
}

#[test]
fn sampling_estimates_the_bytes_within_five_percent_and_the_counts_within_their_spread() {
    let exact = profile(1);
    let sampled = profile(256);
    for name in ["tables", "growth", "strings", "depth", "lines"] {
        let (attributed, measured) = (within(&sampled.bytes, name), allocated(&sampled, name));
        println!("{name}: {attributed} sampled bytes of {measured} allocated ({:+.2} %)", (attributed / measured - 1.0) * 100.0);
        assert!((attributed - measured).abs() <= measured * 0.05, "{name}: {attributed} sampled, {measured} allocated");
    }
    // a count: an allocation of `size` bytes taken with probability size / 256 and weighted by its inverse adds a variance of
    // 256 / size - 1 (by each stack's average size), about 3 % of the count for the probe's small tables and arrays: within
    // four standard deviations (a fixed 5 % failed on one draw of 1.75 of them when Luau 0.746 changed the allocations' order)
    let exact_counts: HashMap<&str, u64> = exact.counts.iter().map(|(stack, count)| (stack.as_str(), *count)).collect();
    for name in ["tables", "lines", "depth"] {
        let (counted, exact_count) = (within(&sampled.counts, name), within(&exact.counts, name));
        let frame = format!(";{name} probe:");
        let variance: f64 = exact
            .bytes
            .iter()
            .filter(|(stack, _)| stack.contains(&frame))
            .filter_map(|(stack, bytes)| {
                let count = *exact_counts.get(stack.as_str())? as f64;
                Some(count * (256.0 * count / *bytes as f64 - 1.0).max(0.0))
            })
            .sum();
        assert!(variance > 0.0, "{name}: no small allocations");
        assert!((counted - exact_count).abs() <= 4.0 * variance.sqrt(), "{name}: {counted} sampled, {exact_count} counted (a standard deviation of {:.0})", variance.sqrt());
    }
}

/// ree's own engine step is not game code: the host-time profile books native time to the next Luau safepoint,
/// so the Animator, joint and physics steps the scheduler runs between a frame's handlers made a spring module's
/// PreSimulation and PostSimulation handlers look like 25-57 % of a frame. Inside `engine_step`
/// the same 100 ms goes to its own row; outside it the next Luau function carries it, as before.
#[test]
fn the_engine_step_has_its_own_row_in_the_host_time_profile() {
    fn wait_for_ticks() {
        let before = luau_profile::ticks();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // A busy host can delay the ticker's first scheduling beyond a fixed sleep.
        while luau_profile::ticks().saturating_sub(before) < 100 {
            assert!(std::time::Instant::now() < deadline, "the host-time ticker did not advance");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("ree-time-profile-{}-{stamp}", std::process::id()));
    let lua = Lua::new();
    assert!(luau_profile::begin_time_at(path.clone(), false, false));
    let guard = luau_profile::ProfileGuard;
    luau_profile::install(&lua);
    let work = lua.create_function(|_, ()| {
        wait_for_ticks();
        Ok(())
    }).unwrap();
    lua.globals().set("sleep", work).unwrap();
    let handler = "local function handler() local n = 0 for i = 1, 200000 do n += i end return n end\nhandler()\n";
    // the engine's step first, as the scheduler runs it between two handlers
    luau_profile::engine_step("probe", wait_for_ticks);
    lua.load(handler).set_name("=after").exec().unwrap();
    // the same time spent by a native function a handler called: the handler carries it
    lua.load(format!("sleep()\n{handler}")).set_name("=control").exec().unwrap();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("profile");
    let _ = std::fs::remove_file(&path);
    let weight = |needle: &str| -> u64 { text.lines().filter(|line| line.contains(needle)).map(|line| line.rsplit_once(' ').unwrap().1.parse::<u64>().unwrap()).sum() };
    let (engine, after, control) = (weight("(ree engine step: probe)"), weight(";handler after:"), weight(";handler control:") + weight("control:1"));
    assert!(engine >= 20, "the engine step booked {engine} ticks\n{text}");
    assert!(after * 4 < engine, "the handler after the step carries {after} ticks of it\n{text}");
    assert!(control >= 20, "the control, native time in the handler's own call, carries {control} ticks\n{text}");
}

/// What a script reads of memory (collectgarbage("count"), gcinfo) frame by frame, while tables are made and collected, in an
/// engine whose VM is hooked for a profile of `kinds` (none: no profile)
fn memory_readings(kinds: Option<crate::profiler::Kinds>) -> Vec<String> {
    use crate::lua as engine_lua;
    let engine = crate::Engine::new(crate::Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let out = std::env::temp_dir().join(format!("ree-hook-readings-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let session = kinds.map(|kinds| crate::profiler::Session::begin(crate::profiler::Config { print: false, ..crate::profiler::Config::new(kinds, out.clone()) }));
    luau_profile::install(&lua);
    let run = |source: &str| {
        let script = {
            let mut state = engine.state.borrow_mut();
            let class = engine.reflection.class_id("ModuleScript").unwrap();
            crate::instance::create_named(&mut state, &engine.reflection, class, "Readings")
        };
        let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
        let function = lua.load(source).set_name("=readings").set_environment(env).into_function().expect("compile");
        let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
        if let engine_lua::globals::CallOutcome::Failed(message, _) = call.outcome().unwrap() {
            panic!("{message}");
        }
    };
    run("_G.Kept = {} _G.Readings = {}");
    for frame in 0..30 {
        run(&format!(
            "for Index = 1, 300 do\n\tlocal Record = {{ A = Index, B = {{ Index }} }}\n\tif Index % 3 == 0 then\n\t\ttable.insert(_G.Kept, Record)\n\tend\nend\nif {frame} % 10 == 9 then\n\ttable.clear(_G.Kept)\nend\ntable.insert(_G.Readings, collectgarbage(\"count\") .. \"/\" .. gcinfo())\n"
        ));
        crate::scheduler::step(&lua, &engine, Some(1.0 / 60.0)).expect("step");
    }
    // (the readings, printed by a last script)
    engine.state.borrow_mut().output.entries.clear();
    run("print(table.concat(_G.Readings, \" \"))");
    if let Some(session) = session {
        crate::profiler::collect(&lua);
        let _ = session.finish(crate::profiler::Run::default());
        let _ = std::fs::remove_dir_all(&out);
    }
    engine.state.borrow().output.entries.iter().map(|entry| entry.message.clone()).collect()
}

/// Hooking the VM for a profile (its interrupt, its allocation callback) allocates nothing on the VM's heap, so what a script
/// reads of memory is the same with a profile that takes the interrupt as without one: the hooks were written through an
/// mlua `exec_raw`, whose protected call made a closure on the heap, and the collector's later cycles then began elsewhere
/// (collectgarbage("count") read 1 KB more at some frames with --profile heap, timeline, time, boundary or hw).
#[test]
fn hooking_the_vm_for_a_profile_leaves_what_a_script_reads_of_memory_as_it_was() {
    let _lock = luau_profile::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let plain = memory_readings(None);
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].split(' ').count(), 30, "{plain:?}");
    for kinds in [crate::profiler::Kinds::TIMELINE, crate::profiler::Kinds::HEAP.with(crate::profiler::Kinds::ALLOC)] {
        assert_eq!(memory_readings(Some(kinds)), plain, "memory readings with a profile of {:?}", kinds.names());
    }
}

/// The profiles share process-wide switches, and a guard's drop turns all of them off: a test that started one while
/// another ran its own (record/tests.rs's allocation profile, which took no lock) switched the work-clock profile
/// off under it, and that test read 0 microseconds profiled. Starting a profile now takes `TEST_LOCK` until
/// its guard drops, whichever test starts it.
#[test]
fn a_started_profile_holds_the_profile_lock_until_its_guard_drops() {
    use std::sync::TryLockError;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("ree-lock-profile-{}-{stamp}", std::process::id()));
    luau_profile::begin_allocations(path.clone(), 1024);
    let guard = luau_profile::ProfileGuard;
    let blocked = std::thread::spawn(|| matches!(luau_profile::TEST_LOCK.try_lock(), Err(TryLockError::WouldBlock))).join().unwrap();
    drop(guard);
    let kept = luau_profile::claimed();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("counts"));
    assert!(blocked, "another thread took the profile lock while a profile ran");
    assert!(!kept, "this thread still held the profile lock after the guard dropped");
}

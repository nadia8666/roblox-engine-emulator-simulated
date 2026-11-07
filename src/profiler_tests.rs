//! The profiling suite (profiler/): each profiler checked against what the VM and the engine count themselves: the
//! instruction profile against the VM's own work counters, the booked work against the work clock, the allocation
//! profile against the VM's heap count, the heap snapshot against what a script holds, the timeline against the frames.

use std::rc::Rc;

use mlua::prelude::*;

use crate::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    profiler::{self, Config, Kinds, memory, vm},
    scheduler,
};

/// Profiles share process-wide switches with luau_profile's (the caches' switch, the ticker): one test at a time
fn lock() -> std::sync::MutexGuard<'static, ()> {
    crate::luau_profile::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn start() -> (Lua, Rc<Engine>) {
    let engine = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    // (interpreted, as `ree run` and `ree record` run modules: every instruction is the interpreter's)
    engine_lua::modules::interpret_all(&lua, &engine);
    (lua, engine)
}

/// Runs `source` as a script (chunk `=name`) on a scheduler thread; its output lines
fn run(lua: &Lua, engine: &Rc<Engine>, name: &str, source: &str) -> Vec<String> {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    let function = lua.load(source).set_name(format!("={name}")).set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    engine.state.borrow().output.entries.iter().map(|entry| entry.message.clone()).collect()
}

fn main_state(lua: &Lua) -> *mut mlua::ffi::lua_State {
    unsafe { mlua::ffi::lua_mainthread(lua.current_thread().state()) }
}

fn ops(lua: &Lua) -> (u64, u64) {
    let counters = unsafe { &*crate::cost::lua_workcounters(main_state(lua)) };
    (counters.ops.iter().sum(), crate::cost::host_instructions(counters))
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("ree-profile-{name}-{}-{stamp}", std::process::id()))
}

/// Every instruction a script dispatches is counted on its own instruction, exactly what the VM's counters count;
/// a loop's lines carry its iterations, calls are counted per callee, and the call tree's totals hold the callees'.
#[test]
fn the_instruction_profile_counts_every_instruction_the_vm_counts() {
    let _lock = lock();
    let lua = Lua::new();
    // (interpreted: a natively compiled chunk dispatches no instruction)
    lua.enable_jit(false);
    let state = main_state(&lua);
    let source = "local function leaf(x)\n\treturn x * 2\nend\nlocal function middle(n)\n\tlocal total = 0\n\tfor i = 1, n do\n\t\ttotal += leaf(i)\n\tend\n\treturn total\nend\nlocal sum = 0\nfor round = 1, 10 do\n\tsum += middle(100)\nend\nresult = sum\n";
    let function = lua.load(source).set_name("=probe").into_function().unwrap();
    let before = ops(&lua);
    assert!(vm::start(state, true, None, None));
    function.call::<()>(()).unwrap();
    vm::stop(state);
    let after = ops(&lua);
    let results = vm::results(state);
    vm::free(state);
    assert_eq!(lua.globals().get::<f64>("result").unwrap(), 10.0 * 2.0 * 5050.0);
    let counted: u64 = results.functions.iter().filter(|f| !f.exempt()).flat_map(|f| f.counts.iter()).sum();
    assert_eq!(counted, after.0 - before.0, "the profile counts what the VM's counters count");
    let by_name = |name: &str| results.functions.iter().position(|f| f.name == name).unwrap_or_else(|| panic!("{name} in {:?}", results.functions.iter().map(|f| &f.name).collect::<Vec<_>>()));
    let (leaf, middle) = (by_name("leaf"), by_name("middle"));
    let line_count = |function: usize, line: i32| -> u64 { results.functions[function].counts.iter().zip(&results.functions[function].lines).filter(|(_, l)| **l == line).map(|(c, _)| *c).sum() };
    // leaf's line 2 runs its MUL and RETURN once a call: 1,000 calls
    assert_eq!(line_count(leaf, 2), 2000, "leaf's body");
    // middle's loop body: the call to leaf (GETUPVAL or MOVE, MOVE, CALL, ADD) and its FORNLOOP 100 times a call
    assert!(line_count(middle, 7) >= 4000, "middle's loop body: {}", line_count(middle, 7));
    let calls = |function: usize| -> u64 { results.nodes.iter().filter(|node| node.function == function as i32).map(|node| node.calls).sum() };
    assert_eq!(calls(leaf), 1000);
    assert_eq!(calls(middle), 10);
    // middle's nodes hold leaf's as children: its subtree counts leaf's instructions too
    let leaf_insns: u64 = results.functions[leaf].counts.iter().sum();
    let middle_node = results.nodes.iter().position(|node| node.function == middle as i32).unwrap();
    let leaf_under_middle: u64 = results.nodes.iter().filter(|node| node.parent == middle_node as i32 && node.function == leaf as i32).map(|node| node.insns).sum();
    assert_eq!(leaf_under_middle, leaf_insns, "every leaf call is under middle");
    // the call site of leaf is middle's line 7
    let site = results.nodes.iter().find(|node| node.function == leaf as i32).unwrap().callpc;
    assert_eq!(results.functions[middle].lines[site as usize], 7);
}

/// What the profile books to the instructions adds up to the work clock's own total for the same span: the VM's
/// counters priced as cost::luau prices them, the engine's charges (Instance API, datatypes) and what was charged
/// between instructions; and a profiled run's work clock is the unprofiled run's, to the bit.
#[test]
fn booked_work_adds_up_to_the_work_clock_and_leaves_it_unchanged() {
    let _lock = lock();
    let script = "local parts = {}\nfor i = 1, 200 do\n\tlocal part = Instance.new(\"Part\")\n\tpart.Name = \"P\" .. i\n\tpart.Transparency = i / 200\n\tparts[i] = part\nend\nlocal total = 0\nfor _, part in parts do\n\ttotal += part.Transparency + part.CFrame.Position.X\nend\nprint(os.clock() > 0, total)\n";
    let measure = |profiled: bool| -> (f64, f64, Vec<String>) {
        let (lua, engine) = start();
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
        let state = main_state(&lua);
        let (work, drained) = {
            let state = engine.state.borrow();
            (state.sched.work.clone(), state.sched.work_drained.clone())
        };
        let before = engine.state.borrow().sched.work_total();
        if profiled {
            let prices = vm::prices().expect("the work clock's model is a weighted sum");
            assert!(vm::start(state, true, Some(&prices), Some((work.as_ptr() as *const f64, drained.as_ptr() as *const f64))));
        }
        let output = run(&lua, &engine, "work", script);
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
        let after = engine.state.borrow().sched.work_total();
        let mut booked = 0.0;
        if profiled {
            vm::stop(state);
            let results = vm::results(state);
            vm::free(state);
            booked = results.nodes.iter().map(|node| node.work).sum::<f64>() + results.engine_work;
        }
        (after - before, booked, output)
    };
    let (plain, _, plain_output) = measure(false);
    let (profiled, booked, profiled_output) = measure(true);
    assert_eq!(plain.to_bits(), profiled.to_bits(), "profiling moved the work clock: {plain} -> {profiled}");
    assert_eq!(plain_output, profiled_output);
    assert!(plain > 50e-6, "the script did work: {plain}");
    assert!((booked - profiled).abs() <= 1e-9 * profiled.max(1e-9) + 1e-12, "booked {booked} of the work clock's {profiled}");
}

/// The inline caches are read as the interpreter reads them: a table key read from one table hits its slot after its
/// first miss; a key the table lacks is looked up every time (a miss); a key its metatable's __index gives takes the
/// slow path; a global hits its slot. (The table is a clone: a compiler since Luau 0.744 reads a local table that a
/// constructor built and nothing changes as constants.)
#[test]
fn inline_caches_tell_hits_from_misses() {
    let _lock = lock();
    let lua = Lua::new();
    lua.enable_jit(false);
    let state = main_state(&lua);
    let source = "local same = table.clone({a = 1, b = 2, c = 3})\nlocal x = 0\nfor i = 1, 100 do\n\tx += same.c\nend\nlocal empty = {}\nfor i = 1, 100 do\n\tx += empty.c or 0\nend\nlocal inherited = setmetatable({}, {__index = {c = 1}})\nfor i = 1, 100 do\n\tx += inherited.c\nend\nG = 0\nfor i = 1, 100 do\n\tG += 1\nend\nresult = x\n";
    let function = lua.load(source).set_name("=caches").into_function().unwrap();
    assert!(vm::start(state, true, None, None));
    function.call::<()>(()).unwrap();
    vm::stop(state);
    let results = vm::results(state);
    vm::free(state);
    let main = results.functions.iter().find(|f| f.source == "caches" && f.line == 0).unwrap_or_else(|| &results.functions[0]);
    let outcomes = |line: i32, op: &str| -> [u64; 4] {
        let mut total = [0u64; 4];
        for pc in 0..main.code.len() {
            if main.lines[pc] == line && vm::opcode_name((main.code[pc] & 0xff) as u8) == op && !main.caches.is_empty() {
                for slot in 0..4 {
                    total[slot] += main.caches[pc * vm::CACHE_OUTCOMES + slot];
                }
            }
        }
        total
    };
    let same = outcomes(4, "GETTABLEKS");
    assert_eq!(same.iter().sum::<u64>(), 100);
    assert!(same[0] >= 99, "one shape: hits after the first lookup {same:?}");
    let absent = outcomes(8, "GETTABLEKS");
    assert_eq!(absent, [0, 100, 0, 0], "a key the table lacks: looked up every time");
    let inherited = outcomes(12, "GETTABLEKS");
    assert_eq!(inherited, [0, 0, 100, 0], "a key its metatable gives: the metatable's path");
    let globals = outcomes(16, "GETGLOBAL");
    assert!(globals[0] >= 99 && globals.iter().sum::<u64>() == 100, "{globals:?}");
}

/// The profiler names opcodes and type tags as the vendored VM numbers them: Bytecode.h's opcodes in order, and lua.h's
/// type tags (a Luau release that adds an opcode or a type moves the ones after it)
#[test]
fn opcode_names_and_type_tags_follow_the_vendored_headers() {
    let identifier = |text: &str| -> String { text.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect() };
    let bytecode = include_str!("../vendor/luau0-src/luau/Common/include/Luau/Bytecode.h");
    let opcodes = &bytecode[bytecode.find("enum LuauOpcode").unwrap()..];
    let opcodes = &opcodes[..opcodes.find("};").unwrap()];
    let names: Vec<String> = opcodes.lines().filter_map(|line| line.trim().strip_prefix("LOP_")).map(identifier).filter(|name| name != "_COUNT").collect();
    assert_eq!(names, vm::OPCODES, "src/profiler/vm.rs OPCODES");
    // lua.h's lua_Type with float vectors (LUA_VECTOR_DOUBLE 0, the layout mlua-sys binds)
    let header = include_str!("../vendor/luau0-src/luau/VM/include/lua.h");
    let types = &header[header.find("enum lua_Type").unwrap()..];
    let types = &types[..types.find("};").unwrap()];
    let (mut tags, mut skipping) = (Vec::new(), false);
    for line in types.lines().map(str::trim) {
        if line.starts_with("#if") {
            skipping = line.contains("LUA_VECTOR_DOUBLE == 1");
        } else if line.starts_with("#endif") {
            skipping = false;
        } else if let Some(rest) = line.strip_prefix("LUA_T").filter(|_| !skipping) {
            let name = identifier(rest);
            // (LUA_T_ALL, LUA_T_COUNT: counts, not tags)
            if name.starts_with('_') {
                break;
            }
            tags.push(name);
        }
    }
    let tag = |name: &str| tags.iter().position(|found| found == name).unwrap_or_else(|| panic!("LUA_T{name} in {tags:?}")) as i32;
    let ours = [
        ("STRING", vm::T_STRING),
        ("TABLE", vm::T_TABLE),
        ("FUNCTION", vm::T_FUNCTION),
        ("USERDATA", vm::T_USERDATA),
        ("THREAD", vm::T_THREAD),
        ("BUFFER", vm::T_BUFFER),
        ("CLASS", vm::T_CLASS),
        ("OBJECT", vm::T_OBJECT),
        ("PROTO", vm::T_PROTO),
        ("UPVAL", vm::T_UPVAL),
    ];
    for (name, value) in ours {
        assert_eq!(tag(name), value, "LUA_T{name}: {tags:?}");
    }
}

/// Allocations are typed exactly (every one), and their stacks name the allocating line; an Instance's userdata
/// names its class; table storage and strings are told apart.
#[test]
fn allocations_are_typed_and_found_by_their_stack() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config::new(Kinds::ALLOC, temp_dir("alloc"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "alloc", "local kept = {}\nlocal function tables()\n\tfor i = 1, 500 do\n\t\tkept[#kept + 1] = {i}\n\tend\nend\nlocal function strings()\n\tfor i = 1, 300 do\n\t\tkept[#kept + 1] = string.rep(\"x\", 100 + i)\n\tend\nend\nlocal function parts()\n\tfor i = 1, 50 do\n\t\tkept[#kept + 1] = Instance.new(\"Part\")\n\tend\nend\ntables()\nstrings()\nparts()\n");
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let by_type: std::collections::HashMap<String, f64> = json["alloc"]["by_type"].as_array().unwrap().iter().map(|row| (row["type"].as_str().unwrap().to_string(), row["count"].as_f64().unwrap())).collect();
    assert!(by_type["table"] >= 500.0, "{by_type:?}");
    assert!(by_type["string"] >= 300.0, "{by_type:?}");
    assert!(by_type.get("userdata: Instance Part").copied().unwrap_or(0.0) >= 50.0, "an Instance's class: {by_type:?}\n{summary}");
    assert!(by_type.contains_key("table storage"), "{by_type:?}");
    // the stacks: the table constructor's line in `tables`, string.rep's caller in `strings`
    let stacks = json["alloc"]["by_stack"].as_array().unwrap();
    let functions = json["functions"].as_array().unwrap();
    let id_of = |name: &str| functions.iter().find(|f| f["name"] == name).and_then(|f| f["id"].as_i64()).unwrap_or_else(|| panic!("{name}"));
    let at = |name: &str, line: i64, ty: &str| -> f64 {
        stacks.iter().filter(|row| row["type"] == ty && row["frames"].as_array().unwrap().iter().any(|frame| frame[0].as_i64() == Some(id_of(name)) && frame[1].as_i64() == Some(line))).map(|row| row["count"].as_f64().unwrap()).sum()
    };
    assert_eq!(at("tables", 4, "table"), 500.0);
    assert_eq!(at("strings", 9, "string"), 300.0);
}

thread_local! {
    /// What this thread's allocation callback was last told by lua_allocblock: the block's address and the kind
    static LAST_ALLOCATION: std::cell::Cell<(usize, i32)> = const { std::cell::Cell::new((0, -1)) };
}

unsafe extern "C-unwind" fn keep_allocation(state: *mut mlua::ffi::lua_State, _block: *mut std::ffi::c_void, _old: usize, _new: usize, _memcat: u8, _tt: i32, _tag: i32) {
    let (block, kind) = vm::alloc_block(state);
    LAST_ALLOCATION.with(|last| last.set((block as usize, kind)));
}

/// Two VMs allocating at once on two threads, each with an allocation callback (two profiled runs in one process, as a
/// host's parallel tests make): what lua_allocblock tells a callback is its own thread's allocation, the object and the
/// kind. The VM kept both in process-wide variables, so a callback could read the object another thread was
/// allocating, and the memory profile then read and marked it in that VM's heap: two profiled renders at once crashed
/// one run in three (access violations in lua_profileobject, the collector's asserts on objects it found dead).
#[test]
fn an_allocation_callback_is_told_its_own_threads_allocation() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let _lock = lock();
    let stop = Arc::new(AtomicBool::new(false));
    let rounds = Arc::new(AtomicU64::new(0));
    let other = {
        let (stop, rounds) = (stop.clone(), rounds.clone());
        std::thread::spawn(move || {
            let lua = Lua::new();
            let state = main_state(&lua);
            unsafe {
                (*mlua::ffi::lua_callbacks(state)).onallocate = Some(keep_allocation);
                // (a round allocates a table, then its array part: the object and the kind both change)
                lua.exec_raw::<()>((), |state| {
                    while !stop.load(Ordering::Relaxed) {
                        mlua::ffi::lua_createtable(state, 4, 0);
                        mlua::ffi::lua_pop(state, 1);
                        rounds.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .unwrap();
                (*mlua::ffi::lua_callbacks(state)).onallocate = None;
            }
        })
    };
    while rounds.load(Ordering::Relaxed) == 0 {
        std::thread::yield_now();
    }
    let lua = Lua::new();
    let state = main_state(&lua);
    let (mut checked, mut wrong_object, mut wrong_kind) = (0u64, 0u64, 0u64);
    let started = rounds.load(Ordering::Relaxed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    unsafe {
        (*mlua::ffi::lua_callbacks(state)).onallocate = Some(keep_allocation);
        lua.exec_raw::<()>((), |state| {
            // 100,000 rounds here, and until the other thread has made as many beside them
            while checked < 100_000 || (rounds.load(Ordering::Relaxed) - started < 100_000 && std::time::Instant::now() < deadline) {
                // a table with no parts is one allocation: the table, an object
                mlua::ffi::lua_createtable(state, 0, 0);
                if LAST_ALLOCATION.with(|last| last.get()) != (mlua::ffi::lua_topointer(state, -1) as usize, vm::ALLOC_OBJECT) {
                    wrong_object += 1;
                }
                mlua::ffi::lua_pop(state, 1);
                // with an array part, the last allocation is the array's storage
                mlua::ffi::lua_createtable(state, 8, 0);
                if LAST_ALLOCATION.with(|last| last.get()) != (0, vm::ALLOC_TABLE) {
                    wrong_kind += 1;
                }
                mlua::ffi::lua_pop(state, 1);
                checked += 1;
            }
        })
        .unwrap();
        (*mlua::ffi::lua_callbacks(state)).onallocate = None;
    }
    stop.store(true, Ordering::Relaxed);
    other.join().unwrap();
    let beside = rounds.load(Ordering::Relaxed) - started;
    assert_eq!((wrong_object, wrong_kind), (0, 0), "{checked} rounds, {beside} on the other thread at the same time: (callbacks told another object, told another kind)");
}

/// A heap snapshot sees what a script holds: by type, the biggest by what they keep alive with the path that holds
/// them, the run's own objects by the stack that made them (a freed object leaves), and garbage not collected yet.
#[test]
fn a_heap_snapshot_finds_what_holds_memory() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config::new(Kinds::HEAP.with(Kinds::ALLOC), temp_dir("heap"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "heap", "local function fill()\n\tlocal big = {}\n\tfor i = 1, 4000 do\n\t\tbig[i] = {i, tostring(i)}\n\tend\n\treturn big\nend\n_G.HeldCache = fill()\nlocal function garbage()\n\tfor i = 1, 2000 do\n\t\tlocal _ = {i}\n\tend\nend\ngarbage()\n");
    lua.gc_collect().unwrap();
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let snapshots = json["heap"]["snapshots"].as_array().unwrap();
    assert_eq!(snapshots.first().unwrap()["label"], "start");
    let end = snapshots.last().unwrap();
    assert_eq!(end["label"], "end");
    // the cache: 4,001 tables and 4,000 strings kept alive by the table the global names (the threads and environments
    // that hold it keep it too: they come before it)
    let biggest = end["biggest"].as_array().unwrap();
    let cache = biggest.iter().find(|held| held["path"].as_str().unwrap().ends_with("HeldCache")).unwrap_or_else(|| panic!("{biggest:#?}"));
    assert!(cache["retained"].as_u64().unwrap() > 4000 * 60, "{cache}");
    assert_eq!(cache["type"], "table");
    // the run's live objects by stack: fill's tables and strings are there; garbage's are not (collected, unmarked)
    let functions = json["functions"].as_array().unwrap();
    let id_of = |name: &str| functions.iter().find(|f| f["name"] == name).and_then(|f| f["id"].as_i64()).unwrap_or(-99);
    let live = |name: &str| -> f64 {
        end["by_stack"].as_array().unwrap().iter().filter(|row| row["frames"].as_array().unwrap().iter().any(|frame| frame[0].as_i64() == Some(id_of(name)))).map(|row| row["count"].as_f64().unwrap()).sum()
    };
    assert!(live("fill") >= 8000.0, "fill's 4,000 tables and 4,000 strings live: {}", live("fill"));
    assert_eq!(live("garbage"), 0.0, "garbage's tables were freed");
}

/// One profile of a script that holds 2,000 tables, then runs a loop of `churn` throwaway tables, with the collector
/// stopped so that whatever garbage the loop makes is still there when the run ends; its profile.json
fn heap_profile_with_garbage(churn: u32) -> serde_json::Value {
    let (lua, engine) = start();
    let config = Config::new(Kinds::HEAP, temp_dir("garbage"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    unsafe { mlua::ffi::lua_gc(main_state(&lua), mlua::ffi::LUA_GCSTOP, 0) };
    run(&lua, &engine, "held", "local held = {}\nfor i = 1, 2000 do\n\theld[i] = {i, tostring(i)}\nend\n_G.Held = held\n");
    run(&lua, &engine, "churn", &format!("for i = 1, {churn} do\n\tlocal _ = {{i, tostring(i)}}\nend\n"));
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    json
}

/// `ree profile diff` headlines the reachable heap: two runs that hold the same data, one leaving 20,000 tables of
/// garbage the collector has not swept, have the same heap at the end, and the garbage is a figure of its own (before,
/// the headline counted it: +42 % for a UI module whose reachable heap had not moved).
#[test]
fn the_heap_headline_is_the_reachable_heap_and_leaves_garbage_out() {
    let _lock = lock();
    let (before, after) = (heap_profile_with_garbage(0), heap_profile_with_garbage(20_000));
    let (text, diff) = profiler::diff::compare(&before, &after, 10);
    let figures = diff["figures"].as_array().unwrap();
    let figure = |name: &str| figures.iter().find(|figure| figure["name"] == name).unwrap_or_else(|| panic!("no {name:?} in\n{text}"));
    // the headline: the first heap figure of the run's end
    let headline = figures.iter().find(|figure| figure["name"].as_str().unwrap().contains("heap at the end")).unwrap_or_else(|| panic!("{text}"));
    let (b, a) = (headline["before"].as_f64().unwrap(), headline["after"].as_f64().unwrap());
    assert!(b > 0.0 && ((a - b) / b).abs() < 0.01, "{} moved {:+.1} % with the garbage alone:\n{text}", headline["name"], (a - b) / b * 100.0);
    assert_eq!(headline["name"], "reachable heap at the end");
    // the garbage is shown apart, under its own name: at least the 20,000 tables (each at least a LuaTable header)
    let garbage = figure("garbage not swept yet at the end");
    assert!(garbage["after"].as_f64().unwrap() - garbage["before"].as_f64().unwrap() >= 20_000.0 * 48.0, "{text}");
    // and the run's own objects are its reachable ones (the garbage it made is still followed until it is freed)
    let own = figure("the run's own reachable objects");
    let (own_before, own_after) = (own["before"].as_f64().unwrap(), own["after"].as_f64().unwrap());
    assert!(own_before > 0.0 && ((own_after - own_before) / own_before).abs() < 0.01, "{text}");
    // the summary and the page's data say the same
    let end = |profile: &serde_json::Value| profile["heap"]["snapshots"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(end(&before)["reachable"]["bytes"].as_f64().unwrap(), b);
    assert!(end(&after)["unswept"]["count"].as_u64().unwrap() >= 20_000, "{}", end(&after)["unswept"]);
    let summary = after["summary"].as_str().unwrap();
    assert!(summary.contains("reachable in") && summary.contains("not swept yet:"), "{summary}");
}

/// Walks of the heap a snapshot can be checked against: (type -> (count, bytes)) of every object, reachable and not
fn objects_by_type(snapshot: &memory::Snapshot) -> std::collections::BTreeMap<String, (u64, u64)> {
    let mut types = std::collections::BTreeMap::new();
    for (name, count, bytes) in snapshot.by_type.iter().chain(&snapshot.unswept_by_type) {
        let entry: &mut (u64, u64) = types.entry(name.clone()).or_default();
        entry.0 += count;
        entry.1 += bytes;
    }
    types
}

/// What a heap snapshot calls reachable is what a full collection keeps, type by type (a thread's size aside: the
/// collector shrinks stacks), and what it calls garbage not swept yet is what that collection frees: the walk follows
/// every reference and root the collector marks, among them the names only a function's prototype holds, the basic
/// types' metatables, the fixed names and the strings of a weak table; a weak table's other values are garbage.
#[test]
fn a_heap_snapshots_reachable_heap_is_what_a_full_collection_keeps() {
    let _lock = lock();
    let (lua, engine) = start();
    let state = main_state(&lua);
    let config = Config::new(Kinds::HEAP.with(Kinds::ALLOC), temp_dir("collector"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCSTOP, 0) };
    run(
        &lua,
        &engine,
        "collector",
        concat!(
            "local function OnlyItsPrototypeNamesMe()\n\treturn 1\nend\n_G.Kept = {OnlyItsPrototypeNamesMe}\n",
            // weak values: the strings stay (in the array part and the hash part), the tables go
            "local weak = setmetatable({}, {__mode = \"v\"})\nweak[1] = string.rep(\"a string a weak table keeps \", 3)\nweak[2] = {}\n",
            "weak.name = string.rep(\"a string a weak table keeps by its key \", 2)\nweak.table = {}\n_G.Weak = weak\n",
            // weak keys: a string key stays, a table key goes
            "local keys = setmetatable({}, {__mode = \"k\"})\nkeys[string.rep(\"a key a weak-keyed table keeps \", 2)] = true\nkeys[{}] = true\n_G.Keys = keys\n",
            // weak both: a string value stays while its key does
            "local both = setmetatable({}, {__mode = \"kv\"})\nlocal key = {}\n_G.BothKey = key\nboth[key] = string.rep(\"kept while its key is \", 2)\n",
            "both[{}] = string.rep(\"gone with its key \", 2)\n_G.Both = both\n",
            "for i = 1, 3000 do\n\tlocal _ = {i, tostring(i)}\nend\n"
        ),
    );
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let end = json["heap"]["snapshots"].as_array().unwrap().last().unwrap().clone();
    let reachable: std::collections::BTreeMap<String, (u64, u64)> =
        end["by_type"].as_array().unwrap().iter().map(|row| (row["type"].as_str().unwrap().to_string(), (row["count"].as_u64().unwrap(), row["bytes"].as_u64().unwrap()))).collect();
    let total_before = vm::total_bytes(state);
    // the collector itself
    unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
    let resolver = memory::resolver(Rc::downgrade(&engine));
    let collected = memory::snapshot(state, "collected", 0, 0.0, Some(&*resolver), 5);
    let kept = objects_by_type(&collected);
    for (name, (count, bytes)) in &reachable {
        let (kept_count, kept_bytes) = kept.get(name).copied().unwrap_or_default();
        assert_eq!(kept_count, *count, "{name}: the snapshot's reachable objects are the ones the collection kept\nreachable {reachable:?}\nkept {kept:?}");
        if name != "thread" {
            assert_eq!(kept_bytes, *bytes, "{name}");
        }
    }
    assert_eq!(kept.keys().collect::<Vec<_>>(), reachable.keys().collect::<Vec<_>>(), "the collection kept no type the snapshot had as garbage only");
    assert_eq!(end["objects"].as_u64().unwrap() - end["unswept"]["count"].as_u64().unwrap(), collected.objects, "the collection freed what the snapshot had as garbage");
    assert!(end["unswept"]["count"].as_u64().unwrap() >= 6000, "the loop's 3,000 tables and strings: {}", end["unswept"]);
    // the VM's own count: the collection freed at least that garbage (and the string table it no longer needs)
    let freed = total_before - vm::total_bytes(state);
    assert!(freed as u64 >= end["unswept"]["bytes"].as_u64().unwrap(), "freed {freed}, garbage {}", end["unswept"]);
    // once the collector has freed what it found, the walk reaches every object left: nothing the collector keeps (a
    // name, a metatable, a fixed string) is taken for garbage. (A collection can leave garbage of its own: a userdata it
    // frees can let go of a reference, so the walk is checked again after each one.)
    let mut settled = memory::snapshot(state, "settled", 0, 0.0, Some(&*resolver), 5);
    for _ in 0..4 {
        if settled.unswept == (0, 0) {
            break;
        }
        unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
        settled = memory::snapshot(state, "settled", 0, 0.0, Some(&*resolver), 5);
    }
    assert_eq!(settled.unswept, (0, 0), "{:?}", settled.unswept_by_type);
    // the weak tables kept their strings and lost their tables
    let check = "local keys = 0\nfor key in _G.Keys do\n\tkeys += if type(key) == \"string\" then 1 else 100\nend\nlocal both = 0\nfor _ in _G.Both do\n\tboth += 1\nend\nprint(type(_G.Weak[1]), type(_G.Weak.name), _G.Weak[2] == nil, _G.Weak.table == nil, keys, both, _G.Both[_G.BothKey] ~= nil)\n";
    assert_eq!(run(&lua, &engine, "check", check).last().unwrap(), "string string true true 1 1 true");
}

/// During a sweep, the objects the collector has found dead and not freed yet are garbage not swept yet too: a snapshot
/// at a phase mark then counts every object the rest of the collection frees (the VM tells the allocation profile of
/// each one it frees).
#[test]
fn a_heap_snapshot_during_a_sweep_counts_the_objects_it_has_yet_to_free() {
    let _lock = lock();
    let (lua, engine) = start();
    let state = main_state(&lua);
    let config = Config::new(Kinds::HEAP.with(Kinds::ALLOC), temp_dir("sweep"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
    run(&lua, &engine, "sweep", "local kept = {}\nfor i = 1, 500 do\n\tkept[i] = {i}\nend\n_G.Kept = kept\nfor i = 1, 20000 do\n\tlocal _ = {i}\nend\n");
    // the collector steps into its sweep: the loop's tables are found dead, and many are not freed yet
    let mut steps = 0;
    while vm::gc_state(state) != 4 {
        unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCSTEP, 0) };
        steps += 1;
        assert!(steps < 1_000_000, "the collector never reached its sweep");
    }
    profiler::phase("sweeping");
    let followed = memory::with(|allocations| allocations.live.len()).unwrap();
    unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
    let freed = followed - memory::with(|allocations| allocations.live.len()).unwrap();
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let snapshots = json["heap"]["snapshots"].as_array().unwrap();
    let sweeping = snapshots.iter().find(|snapshot| snapshot["label"] == "sweeping").unwrap();
    let end = snapshots.last().unwrap();
    // (how many the sweep freed before the mark depends on its step size: thousands are left)
    assert!(freed >= 1_000, "the rest of the collection freed the loop's tables: {freed}");
    // every object the run made that the rest of the collection freed was counted as garbage (with what the engine's
    // start left, which the profile does not follow)
    let unswept = sweeping["unswept"]["count"].as_u64().unwrap();
    assert!(unswept >= freed as u64, "garbage not swept yet {unswept}, freed {freed}: {}", sweeping["unswept"]);
    assert_eq!(sweeping["reachable"]["count"].as_u64().unwrap(), end["objects"].as_u64().unwrap(), "what was reachable is what the collection kept");
}

/// The collector's steps, cycles and bytes freed come from the VM's interrupt at each step.
#[test]
fn the_collectors_work_is_counted() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config::new(Kinds::HEAP, temp_dir("gc"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "churn", "for i = 1, 200000 do\n\tlocal _ = {i, i + 1, i + 2}\nend\n");
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let gc = &json["heap"]["gc"];
    assert!(gc["steps"].as_u64().unwrap() > 10, "{gc}");
    assert!(gc["cycles"].as_u64().unwrap() >= 1, "{gc}");
    assert!(gc["freed"].as_u64().unwrap() > 1_000_000, "{gc}");
}

/// The timeline has a row a frame, with the frame's work, the heap, live instances, connections and threads.
#[test]
fn the_timeline_has_a_row_a_frame() {
    let _lock = lock();
    // (a playing client: one Heartbeat a frame; edit mode beats at 240 Hz)
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config::new(Kinds::TIMELINE.with(Kinds::WORK), temp_dir("timeline"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "frames", "local RunService = game:GetService(\"RunService\")\nlocal made = {}\nRunService.Heartbeat:Connect(function()\n\tmade[#made + 1] = Instance.new(\"Folder\")\n\ttask.spawn(function() task.wait(10) end)\nend)\n");
    for _ in 0..10 {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 10, seconds: 10.0 / 60.0, fps: 60.0, ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let timeline = &json["timeline"];
    let columns: Vec<&str> = timeline["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    let rows = timeline["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    let column = |name: &str| columns.iter().position(|c| *c == name).unwrap();
    let (first, last) = (&rows[0], &rows[9]);
    assert_eq!(last[column("instances")].as_u64().unwrap() - first[column("instances")].as_u64().unwrap(), 9, "a Folder a frame");
    assert!(last[column("threads")].as_i64().unwrap() - first[column("threads")].as_i64().unwrap() >= 9, "a waiting thread a frame");
    assert!(rows.iter().all(|row| row[column("work_ms")].as_f64().unwrap() > 0.0), "each frame's handler did work");
}

/// Host time (`time`): the ticker's ticks are milliseconds, booked to the function that ran when they passed, and they
/// add up to about the wall time the run took.
#[test]
fn host_time_is_booked_in_milliseconds_where_it_was_spent() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::TIME, temp_dir("time")) };
    let started = std::time::Instant::now();
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    // (a table's function: -O2 inlines a small local function into its caller)
    run(&lua, &engine, "busy", "local M = {}\nfunction M.spin(n)\n\tlocal x = 0\n\tfor i = 1, n do\n\t\tx += i % 7\n\tend\n\treturn x\nend\nlocal total = 0\nfor round = 1, 400 do\n\ttotal += M.spin(50000)\nend\nresult = total\n");
    profiler::collect(&lua);
    let wall = started.elapsed().as_secs_f64() * 1000.0;
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let sampled = json["time"]["sampled_ms"].as_f64().unwrap();
    assert!(wall > 20.0, "the script ran long enough to measure: {wall:.1} ms");
    assert!(sampled >= 0.5 * wall - 5.0 && sampled <= wall + 2.0, "sampled {sampled} ms of a {wall:.1} ms run");
    let functions = json["functions"].as_array().unwrap();
    let spin = functions.iter().find(|f| f["name"].as_str().is_some_and(|name| name.ends_with("spin"))).unwrap_or_else(|| panic!("{}", json["functions"]));
    assert!(spin["self"]["time_ms"].as_f64().unwrap() >= 0.8 * sampled, "the loop's function holds the time: {spin}");
}

/// A small local function `-O2` inlines runs as copies in its callers, with no frame of its own: the profile finds the copies by
/// the lines they carry and counts a call each time one is entered (two copies in a row count apart, a loop around one counts
/// each turn), a call that did make a frame stays a call, and the summary, the JSON and a diff show the inlined calls.
#[test]
fn inlined_functions_count_their_calls_where_their_copies_run() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::WORK, temp_dir("inlined")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    // (middle and twice are a table's: only a local function is inlined, so they keep their frames)
    let source = "--!optimize 2\nlocal function leaf(x)\n\treturn x * 2 + 1\nend\nlocal M = {}\nfunction M.middle(n)\n\tlocal total = 0\n\tfor i = 1, n do\n\t\ttotal += leaf(i)\n\tend\n\treturn total\nend\nfunction M.twice(n)\n\tlocal total = 0\n\tfor i = 1, n do\n\t\tlocal a = leaf(i)\n\t\tlocal b = leaf(i)\n\t\ttotal += a + b\n\tend\n\treturn total\nend\nlocal sum = 0\nfor round = 1, 10 do\n\tsum += M.middle(100)\nend\nsum += M.twice(50)\nlocal ok, value = pcall(leaf, 3)\nsum += value\nlocal done = pcall(function(step)\n\tsum += step\nend, 1)\nprint(sum)\n";
    let output = run(&lua, &engine, "inlining", source);
    assert_eq!(output.last().map(String::as_str), Some("107208"), "{output:?}");
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let summary = std::fs::read_to_string(config.out.join("summary.txt")).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let functions = json["functions"].as_array().unwrap();
    let named = |name: &str| {
        functions
            .iter()
            .find(|f| f["module"] == "inlining" && f["name"].as_str().is_some_and(|n| n == name || n.ends_with(&format!(".{name}"))))
            .unwrap_or_else(|| panic!("no {name}: {}", json["functions"]))
    };
    let (leaf, middle, twice) = (named("leaf"), named("middle"), named("twice"));
    // (a call through a local holding it is inlined too: pcall's makes a frame)
    assert_eq!(leaf["calls"], 1, "the call through pcall made a frame: {leaf}");
    let inlined = &leaf["inlined"];
    assert_eq!(inlined["calls"], 10 * 100 + 50 * 2, "{inlined}");
    let into = |caller: &serde_json::Value| inlined["into"].as_array().unwrap().iter().find(|row| row["function"] == caller["id"]).cloned().unwrap_or_else(|| panic!("{inlined}"));
    let (in_middle, in_twice) = (into(middle), into(twice));
    assert_eq!((in_middle["copies"].as_u64(), in_middle["calls"].as_u64()), (Some(1), Some(1000)), "{in_middle}");
    assert_eq!((in_twice["copies"].as_u64(), in_twice["calls"].as_u64()), (Some(2), Some(100)), "two copies side by side: {in_twice}");
    assert!(inlined["insns"].as_u64().unwrap() >= 1100, "{inlined}");
    // (the instructions stay their callers' own: the VM ran them in those frames)
    assert!(middle["self"]["insns"].as_u64().unwrap() >= inlined["insns"].as_u64().unwrap() * 1000 / 1100, "{middle}");
    let edge = json["edges"].as_array().unwrap().iter().find(|e| e["caller"] == middle["id"] && e["callee"] == leaf["id"] && e["inlined"] == true).unwrap_or_else(|| panic!("{}", json["edges"]));
    assert_eq!(edge["calls"], 1000);
    assert!(json["lines"].as_array().unwrap().iter().any(|l| l["function"] == middle["id"] && l["inlined"] == leaf["id"] && l["line"] == 3), "{}", json["lines"]);
    assert!(summary.contains("inlined functions") && summary.lines().any(|line| line.contains("leaf") && line.contains("1,100")), "{summary}");
    // (the statement creating a closure on its first and last lines, a call it is an argument of, is not a copy of it)
    let inlined_here: Vec<&serde_json::Value> = functions.iter().filter(|f| f["module"] == "inlining" && f["inlined"].is_object()).collect();
    assert_eq!(inlined_here.len(), 1, "only leaf was inlined: {inlined_here:?}");
}

/// ree's own caches are counted while a profile asks: a property read twice hits the member dispatch the second time,
/// and an Instance pushed again hits its userdata.
#[test]
fn ree_caches_count_hits_and_misses() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config::new(Kinds::CACHES, temp_dir("caches"));
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "caches", "local part = Instance.new(\"Part\")\nlocal folder = Instance.new(\"Folder\")\npart.Parent = folder\nfor i = 1, 100 do\n\tlocal _ = part.Transparency\n\tlocal _ = part.Parent\nend\n");
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let cache = |name: &str| json["ree_caches"].as_array().unwrap().iter().find(|row| row["name"] == name).cloned().unwrap();
    let members = cache("member dispatch");
    assert!(members["hits"].as_u64().unwrap() >= 198, "{members}");
    let wrappers = cache("instance userdata");
    assert!(wrappers["hits"].as_u64().unwrap() >= 99, "the folder's userdata found again: {wrappers}");
    assert!(json["luau_caches"]["by_kind"].as_array().is_some(), "Luau's caches come with the stepping");
}

/// The command line: kinds parse with their aliases; `--profile` alone and `--profile boundary` alone keep their own
/// forms (the phases, the boundary profiler) and give no suite profile; a kinds value takes `--profile-out`.
#[test]
fn profile_options_keep_the_old_forms() {
    let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert_eq!(Kinds::parse("all").unwrap(), Kinds::ALL);
    assert!(Kinds::parse("instructions").unwrap().has(Kinds::WORK));
    assert!(Kinds::parse("memory").unwrap().has(Kinds::HEAP));
    assert!(Kinds::parse("nonsense").unwrap_err().contains("nonsense"));
    let none = || std::path::PathBuf::from("default");
    assert!(Config::from_args(&args(&["--harness", "x.luau", "--profile"]), none).unwrap().is_none());
    assert!(Config::from_args(&args(&["--profile", "--seconds", "2"]), none).unwrap().is_none());
    assert!(Config::from_args(&args(&["--profile", "boundary", "--profile-out", "b.json"]), none).unwrap().is_none());
    assert!(profiler::phases_requested(&args(&["--profile", "--seconds", "2"])));
    assert!(!profiler::phases_requested(&args(&["--profile", "work"])));
    let config = Config::from_args(&args(&["--profile", "work,alloc", "--profile-out", "dir", "--profile-top", "7"]), none).unwrap().unwrap();
    assert!(config.kinds.has(Kinds::WORK) && config.kinds.has(Kinds::ALLOC) && !config.kinds.has(Kinds::HEAP));
    assert_eq!((config.out, config.top), (std::path::PathBuf::from("dir"), 7));
    assert_eq!(profiler::profile_arguments(&args(&["--profile", "work", "--x"]), 0), 2);
    assert_eq!(profiler::profile_arguments(&args(&["--profile", "--x"]), 0), 1);
}

/// A word after `--profile` that names no kind the suite knows is refused with the kinds listed, so the command stops
/// before it starts: it used to be taken for `--profile` alone, and the run printed the phases, wrote no profile and
/// exited 0. A value that names no kind at all is refused too.
#[test]
fn a_kind_the_suite_does_not_know_is_refused() {
    let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    let none = || std::path::PathBuf::from("default");
    let kinds = "(kinds: work, caches, alloc, heap, timeline, cpu, hw, phases, time, boundary, all)";
    let error = Config::from_args(&args(&["--harness", "x.luau", "--profile", "memroy", "--seconds", "2"]), none).unwrap_err();
    assert!(error.contains("no kind \"memroy\"") && error.contains(kinds), "{error}");
    let error = Config::from_args(&args(&["--profile", "work,memroy"]), none).unwrap_err();
    assert!(error.contains("no kind \"memroy\""), "{error}");
    for empty in ["", ",", " , "] {
        let error = Config::from_args(&args(&["--profile", empty, "--seconds", "2"]), none).unwrap_err();
        assert!(error.contains("no kind named") && error.contains(kinds), "{empty:?}: {error}");
    }
    // the word is the value: skipped with `--profile` by the parsers that skip it, asked for in a process of its own
    // (the service's client runs the command itself, which refuses it), never the phases of before
    assert_eq!(profiler::profile_value(&args(&["--profile", "memroy"])).as_deref(), Some("memroy"));
    assert_eq!(profiler::profile_arguments(&args(&["--profile", "memroy", "--x"]), 0), 2);
    assert!(profiler::requested(&args(&["--profile", "memroy"])));
    assert!(!profiler::phases_requested(&args(&["--profile", "memroy"])));
}

/// A profile of the boundary kind (as rur asks for `--profile boundary`) of a script that writes and reads a part, into
/// a profile folder that does not exist yet: its summary and the folder
fn boundary_profile(name: &str) -> (String, std::path::PathBuf) {
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::BOUNDARY, temp_dir(name).join("profile")) };
    assert!(!config.out.exists());
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "crossings", "local part = Instance.new(\"Part\")\nfor i = 1, 10 do\n\tpart.Transparency = i / 10\n\tlocal _ = part.Transparency\nend\n");
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    (summary, config.out)
}

/// The boundary kind writes its boundary.json into the profile's folder when that folder is new: the boundary profiler
/// wrote it before the suite made the folder, so the file went missing while the summary listed it. The boundary
/// profiler's own form (`--profile boundary --profile-out <file>`) makes its file's folder too.
#[test]
fn the_boundary_kind_writes_its_file_into_a_new_profile_folder() {
    let _lock = lock();
    let (summary, dir) = boundary_profile("boundary-file");
    let written = std::fs::read_to_string(dir.join("boundary.json"));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    let report: serde_json::Value = serde_json::from_str(&written.expect("boundary.json in the profile folder")).unwrap();
    assert!(report["crossings"].as_u64().unwrap() >= 21, "a create, 10 writes and 10 reads: {report}");
    assert!(summary.contains("boundary.json"), "{summary}");
    let file = temp_dir("boundary-own").join("deeper").join("b.json");
    let (lua, engine) = start();
    crate::boundary::begin(crate::boundary::Config { out: file.clone(), top: 5, phase: None });
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "own", "local part = Instance.new(\"Part\")\nlocal _ = part.Transparency\n");
    crate::boundary::finish().expect("report");
    let made = file.is_file();
    let _ = std::fs::remove_dir_all(file.parent().unwrap().parent().unwrap());
    assert!(made, "{}", file.display());
}

/// The boundary kind's report is in the summary (summary.txt, and the text a command prints) before the files line, as
/// every other kind's is: it was only in profile.json.
#[test]
fn the_summary_holds_the_boundary_report() {
    let _lock = lock();
    let (summary, dir) = boundary_profile("boundary-summary");
    let text = std::fs::read_to_string(dir.join("summary.txt")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    assert_eq!(text, summary);
    let report = json["boundary"].as_str().unwrap();
    assert!(report.starts_with("boundary profile:") && report.contains("BasePart.Transparency"), "{report}");
    let at = text.find(report.trim_end()).unwrap_or_else(|| panic!("the boundary report is in summary.txt:\n{text}"));
    assert!(at < text.find("\nfiles in ").unwrap(), "{text}");
}

/// A run writes every file (summary, JSON, folded stacks, speedscope, the page), the page holds the profile, and a diff
/// of a profile with itself finds nothing moved while one with more work finds the function that did it.
#[test]
fn a_profile_writes_its_files_and_diffs() {
    let _lock = lock();
    let profile = |name: &str, rounds: u32| -> std::path::PathBuf {
        let (lua, engine) = start();
        let config = Config { print: false, ..Config::new(Kinds::ALL, temp_dir(name)) };
        let session = profiler::Session::begin(config.clone());
        crate::luau_profile::install(&lua);
        crate::luau_profile::set_phase("frames");
        // (a table's function: -O2 inlines a small local function into its caller)
        run(&lua, &engine, "work", &format!("local M = {{}}\nfunction M.hot(n)\n\tlocal t = {{}}\n\tfor i = 1, n do\n\t\tt[i] = i * i\n\tend\n\treturn t\nend\nfor round = 1, {rounds} do\n\tM.hot(200)\nend\n"));
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
        profiler::collect(&lua);
        let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 1, seconds: 1.0 / 60.0, fps: 60.0, ..Default::default() }).unwrap();
        assert!(summary.contains("top functions by self work") && summary.contains("hot"), "{summary}");
        config.out
    };
    let (before, after) = (profile("before", 5), profile("after", 10));
    for file in ["summary.txt", "profile.json", "report.html", "work.folded", "instructions.folded", "alloc.folded", "profile.speedscope.json"] {
        assert!(before.join(file).is_file(), "{file}");
    }
    let page = std::fs::read_to_string(before.join("report.html")).unwrap();
    assert!(page.contains("\"version\":1") && !page.contains("/*PROFILE_DATA*/"), "the page carries the profile");
    let speedscope: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(before.join("profile.speedscope.json")).unwrap()).unwrap();
    assert!(speedscope["profiles"].as_array().unwrap().len() >= 3);
    let load = |dir: &std::path::Path| -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(dir.join("profile.json")).unwrap()).unwrap() };
    let (same, _) = profiler::diff::compare(&load(&before), &load(&before), 10);
    assert!(!same.contains("functions that moved most"), "{same}");
    let (moved, json) = profiler::diff::compare(&load(&before), &load(&after), 10);
    assert!(moved.contains("hot"), "{moved}");
    let hot = json["functions"].as_array().unwrap().iter().find(|row| row["function"].as_str().unwrap().starts_with("M.hot") || row["function"].as_str().unwrap().starts_with("hot")).unwrap_or_else(|| panic!("{moved}"));
    assert!(hot["after"]["calls"].as_f64().unwrap() == 2.0 * hot["before"]["calls"].as_f64().unwrap(), "{hot}");
    let _ = std::fs::remove_dir_all(&before);
    let _ = std::fs::remove_dir_all(&after);
}

/// A profile of a clip-shaped run: the game mounts in startup, attacks in steady while a harness module watches, and
/// idles after; `watch` spins `watched` times. `phases` is `--profile-phase`. Returns the summary and profile.json.
fn harness_profile(name: &str, watched: u32, phases: Option<&str>) -> (String, serde_json::Value) {
    let (lua, engine) = start();
    let config = Config { print: false, phases: phases.map(str::to_string), ..Config::new(Kinds::WORK, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    // (a table's function: -O2 inlines a small local function into its caller)
    let spin = |function: &str, rounds: u32| format!("local M = {{}}\nfunction M.{function}(n)\n\tlocal x = 0\n\tfor i = 1, n do\n\t\tx += i % 7\n\tend\n\treturn x\nend\nresult = M.{function}({rounds})\n");
    run(&lua, &engine, "ReplicatedStorage.Scene.Mount", &spin("mount", 40000));
    profiler::phase("steady");
    run(&lua, &engine, "ReplicatedStorage.Scene.Attack", &spin("attack", 20000));
    run(&lua, &engine, "ReplicatedStorage.RURFixtures.Clip", &spin("watch", watched));
    // (the engine's chunk that calls a harness, as `ree record` names it)
    run(&lua, &engine, "rur.record", &spin("drive", 3000));
    profiler::phase("after");
    run(&lua, &engine, "ReplicatedStorage.Scene.Idle", &spin("idle", 5000));
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 1, seconds: 1.0 / 60.0, fps: 60.0, ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// The text between a summary's `title` line and the next blank line
fn section<'a>(summary: &'a str, title: &str) -> &'a str {
    let start = summary.find(title).unwrap_or_else(|| panic!("no {title:?} in\n{summary}"));
    let rest = &summary[start..];
    &rest[..rest.find("\n\n").unwrap_or(rest.len())]
}

/// The summary reports the steady phase the run marked, and the harness's own code (under ReplicatedStorage.RURFixtures)
/// apart from the game's: its work moves with what it watches, not with the game
#[test]
fn the_summary_reports_the_steady_phase_with_the_harnesss_code_apart() {
    let _lock = lock();
    let (summary, json) = harness_profile("harness", 30000, None);
    assert!(summary.contains("reported below: phase steady: the game's code"), "{summary}");
    let game = section(&summary, "top functions by self work: the game's code, phase steady");
    assert!(game.contains("attack") && !game.contains("watch") && !game.contains("mount") && !game.contains("idle"), "{summary}");
    let harness = section(&summary, "the harness's code, phase steady");
    assert!(harness.contains("watch") && harness.contains("drive") && !harness.contains("attack"), "{summary}");
    assert!(!game.contains("drive"), "{summary}");
    // profile.json: the harness's functions are marked, every function has its phases, the scope has each side's work
    let function = |name: &str| {
        let suffix = format!(".{name}");
        json["functions"].as_array().unwrap().iter().find(|f| f["name"].as_str().is_some_and(|n| n == name || n.ends_with(&suffix))).unwrap_or_else(|| panic!("no {name} in\n{summary}"))
    };
    assert_eq!(function("watch")["harness"], true);
    assert_eq!(function("attack")["harness"], false);
    assert!(function("attack")["phases"]["steady"]["self_work_ms"].as_f64().unwrap() > 0.0);
    assert!(function("attack")["phases"].get("startup").is_none() && function("mount")["phases"]["startup"]["self_work_ms"].as_f64().unwrap() > 0.0);
    assert_eq!(function("watch")["phases"]["steady"]["harness_self_work_ms"], function("watch")["phases"]["steady"]["self_work_ms"]);
    assert_eq!(json["scope"]["phases"], serde_json::json!(["steady"]));
    let (game_ms, harness_ms) = (json["scope"]["game_self_ms"].as_f64().unwrap(), json["scope"]["harness_self_ms"].as_f64().unwrap());
    let steady = |name: &str| function(name)["phases"]["steady"]["self_work_ms"].as_f64().unwrap();
    assert!(game_ms >= steady("attack") && harness_ms >= steady("watch") && harness_ms < steady("watch") + game_ms, "game {game_ms}, harness {harness_ms}");
    // --profile-phase all: every phase's game code, and still not the harness's
    let (all, _) = harness_profile("harness-all", 30000, Some("all"));
    let game = section(&all, "top functions by self work: the game's code, every phase");
    assert!(game.contains("mount") && game.contains("attack") && game.contains("idle") && !game.contains("watch"), "{all}");
}

/// A diff of two runs leads with the game's code in the phase both report; the harness's moved work stays apart
#[test]
fn a_diff_leads_with_the_games_code_and_keeps_the_harnesss_apart() {
    let _lock = lock();
    let ((_, before), (_, after)) = (harness_profile("diff-before", 20000, None), harness_profile("diff-after", 60000, None));
    let (text, json) = profiler::diff::compare(&before, &after, 10);
    let figure = |name: &str| json["figures"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no {name} in\n{text}")).clone();
    let first = &json["figures"][0];
    assert_eq!(first["name"], "the game's code, phase steady", "{text}");
    let game = figure("the game's code, phase steady");
    let (game_before, game_after) = (game["before"].as_f64().unwrap(), game["after"].as_f64().unwrap());
    assert!((game_after - game_before).abs() <= 0.05 * game_before, "the game's code did the same work: {game_before} -> {game_after}\n{text}");
    let harness = figure("the harness's code, phase steady");
    assert!(harness["after"].as_f64().unwrap() > 2.0 * harness["before"].as_f64().unwrap(), "{text}");
    assert_eq!(json["scope"], serde_json::json!(["steady"]));
    let rows = json["functions"].as_array().unwrap();
    assert!(rows.iter().any(|row| row["function"].as_str().unwrap().contains("watch") && row["harness"] == true), "{text}");
    // (the game's code did the same work, so its table may be empty and left out)
    let game_title = "functions that moved most: the game's code, phase steady";
    assert!(!text.contains(game_title) || !section(&text, game_title).contains("watch"), "{text}");
    assert!(section(&text, "the harness's functions that moved most, phase steady").contains("watch"), "{text}");
}

/// The memory module's own figures: a stack is interned once, a type name once.
#[test]
fn stacks_and_types_intern_once() {
    let mut stacks = memory::Stacks::default();
    let a = stacks.intern(&[(1, 10), (2, 20)]);
    let b = stacks.intern(&[(1, 10), (2, 20)]);
    let c = stacks.intern(&[(1, 11)]);
    assert_eq!((a, b, c), (0, 0, 1));
    let mut types = memory::Types::default();
    assert_eq!((types.id("table"), types.id("string"), types.id("table")), (0, 1, 0));
    assert_eq!(memory::group("table storage"), "table");
    assert_eq!(memory::group("userdata: Instance Part"), "userdata");
}

/// Hardware counters (`hw`) with the mock source (each reading takes the thread's cycle count, QueryThreadCycleTime,
/// and the other counters are fixed shares of it): every window between readings is booked once, so the run's cycles
/// are the thread's own cycles between its start and end readings, inside what the test reads around the profile; the
/// phases and the stacks add up to the run exactly, every frame has its row, and the busy function holds most of it.
#[test]
fn hardware_counters_are_booked_to_the_run_its_phases_frames_and_stacks() {
    let _lock = lock();
    profiler::hw::TEST_MODE.with(|mode| mode.set(Some("mock")));
    // (a playing client: one Heartbeat a frame)
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::HW, temp_dir("hw")) };
    let before = profiler::cpu::thread_cycles();
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "busy", "local M = {}\nfunction M.spin(n)\n\tlocal x = 0\n\tfor i = 1, n do\n\t\tx += i % 7\n\tend\n\treturn x\nend\nlocal total = 0\nfor round = 1, 300 do\n\ttotal += M.spin(50000)\nend\nresult = total\nlocal RunService = game:GetService(\"RunService\")\nRunService.Heartbeat:Connect(function()\n\tlocal y = 0\n\tfor i = 1, 20000 do\n\t\ty += i\n\tend\nend)\n");
    for _ in 0..5 {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::phase("steady");
    for _ in 0..5 {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let after = profiler::cpu::thread_cycles();
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 10, ..Default::default() }).unwrap();
    profiler::hw::TEST_MODE.with(|mode| mode.set(None));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let folded = std::fs::read_to_string(config.out.join("counters.cycles.folded")).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let counters = &json["counters"];
    assert_eq!(counters["source"], "mock", "{summary}");
    let names: Vec<&str> = counters["names"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect();
    assert_eq!(names, profiler::hw::protocol::DEFAULT_COUNTERS);
    let cycles = counters["run"]["counts"]["cycles"].as_u64().unwrap();
    let span = after - before;
    assert!(cycles > 0 && cycles <= span && cycles as f64 >= 0.8 * span as f64, "the run's cycles {cycles}, the thread's around the profile {span}");
    assert_eq!(counters["run"]["counts"]["instructions"].as_u64().unwrap(), 2 * cycles, "the mock's instructions are twice its cycles, window by window");
    assert_eq!(counters["run"]["rates"]["IPC"].as_f64().unwrap(), 2.0);
    assert_eq!(counters["readings"]["matched"], counters["readings"]["count"], "every reading has its switch");
    assert!(counters["readings"]["samples"].as_u64().unwrap() >= 20, "a sample a millisecond: {}", counters["readings"]);
    // the phases add up to the run
    let phases = counters["phases"].as_array().unwrap();
    assert_eq!(phases.iter().map(|phase| phase["counts"]["cycles"].as_u64().unwrap()).sum::<u64>(), cycles);
    assert!(phases.iter().any(|phase| phase["phase"] == "steady"), "{phases:?}");
    // a row a frame, each with its counts
    let columns: Vec<&str> = counters["frames"]["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    let column = columns.iter().position(|c| *c == "cycles").unwrap();
    let rows = counters["frames"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    assert!(rows.iter().all(|row| row[column].as_u64().unwrap() > 0), "{rows:?}");
    assert!(rows.iter().map(|row| row[column].as_u64().unwrap()).sum::<u64>() < cycles);
    // every count is some frame's self: the stacks add up to the run, and the busy function holds most of it
    let functions = counters["functions"].as_array().unwrap();
    assert_eq!(functions.iter().map(|f| f["self"]["cycles"].as_u64().unwrap()).sum::<u64>(), cycles, "{summary}");
    let spin = functions.iter().find(|f| f["name"].as_str().is_some_and(|name| name.ends_with("spin"))).unwrap_or_else(|| panic!("{summary}"));
    assert!(spin["self"]["cycles"].as_u64().unwrap() as f64 >= 0.5 * cycles as f64, "{spin}\n{summary}");
    assert!(functions.iter().any(|f| f["kind"] == "engine step"), "the engine's own steps have frames of their own");
    let folded_total: u64 = folded.lines().map(|line| line.rsplit(' ').next().unwrap().parse::<u64>().unwrap()).sum();
    assert_eq!(folded_total, cycles, "the folded stacks hold the run");
    assert!(summary.contains("hardware counters") && summary.contains("IPC 2.00"), "{summary}");
}

/// Without the counter service the profile says why and the run goes on: no counts, a warning naming what is missing
#[test]
fn hardware_counters_say_why_they_were_not_read() {
    let _lock = lock();
    // (a service run in a console that is not running: deterministic on any machine)
    profiler::hw::TEST_MODE.with(|mode| mode.set(Some("console")));
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::HW, temp_dir("hw-missing")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "quick", "result = 1 + 1\n");
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    profiler::hw::TEST_MODE.with(|mode| mode.set(None));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let error = json["counters"]["error"].as_str().unwrap_or_else(|| panic!("{}", json["counters"]));
    assert!(error.contains("serve") || error.contains("Windows"), "{error}");
    assert!(json["warnings"].as_array().unwrap().iter().any(|warning| warning.as_str().unwrap().starts_with("hardware counters not read")), "{}", json["warnings"]);
    assert!(summary.contains("hardware counters: not read"), "{summary}");
}

/// `hw` is a kind of its own (not in `all`: stepping would be counted with the run), and `--profile-counters` names
/// the counters
#[test]
fn hardware_counter_options_parse() {
    let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert!(Kinds::parse("hw").unwrap().has(Kinds::HW));
    assert!(!Kinds::ALL.has(Kinds::HW));
    let none = || std::path::PathBuf::from("default");
    let config = Config::from_args(&args(&["--profile", "hw,timeline", "--profile-counters", "instructions, cycles,DcacheMisses"]), none).unwrap().unwrap();
    assert_eq!(config.counters, ["instructions", "cycles", "DcacheMisses"]);
    assert_eq!(Config::from_args(&args(&["--profile", "hw"]), none).unwrap().unwrap().counters, profiler::hw::protocol::DEFAULT_COUNTERS);
    assert!(Config::from_args(&args(&["--profile", "hw", "--profile-counters", "cycles,cycles"]), none).unwrap_err().contains("twice"));
}

/// One heap profile of `source` (chunk `=shapes`): its profile.json
fn shape_profile(name: &str, source: &str) -> serde_json::Value {
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::HEAP.with(Kinds::ALLOC), temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "shapes", source);
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    json
}

/// The site of a snapshot's tables whose stack's innermost Luau frame is `line` of the `shapes` chunk
fn site_at<'a>(json: &serde_json::Value, snapshot: &'a serde_json::Value, line: i64) -> &'a serde_json::Value {
    let functions = json["functions"].as_array().unwrap();
    let in_chunk = |id: i64| functions.iter().any(|f| f["id"].as_i64() == Some(id) && f["module"] == "shapes");
    snapshot["tables"]["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|site| {
            let innermost = site["frames"].as_array().and_then(|frames| frames.iter().find(|frame| frame[0].as_i64().is_some_and(|id| id >= 0)).cloned());
            innermost.is_some_and(|frame| in_chunk(frame[0].as_i64().unwrap()) && frame[1].as_i64() == Some(line))
        })
        .unwrap_or_else(|| panic!("no table site at shapes:{line}: {:#}", snapshot["tables"]))
}

/// A part's (or the levels') distribution: (total, used, min, median, max, histogram)
fn distribution(part: &serde_json::Value) -> (f64, f64, f64, f64, f64, Vec<(u64, u64)>) {
    let hist = part["hist"].as_array().unwrap().iter().map(|pair| (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap())).collect();
    (part["slots"].as_f64().unwrap_or(0.0), part["used"].as_f64().unwrap_or(0.0), part["min"].as_f64().unwrap(), part["median"].as_f64().unwrap(), part["max"].as_f64().unwrap(), hist)
}

/// A heap snapshot's tables, exactly: how many each line made, their array and hash parts (slots, and those holding a
/// value), their bytes as the VM counts them, how many levels of tables each is (a cycle shares one), and the rehashes
/// that grew them, by the line that made them and the line that grew them
#[test]
fn table_shapes_are_counted_exactly_by_the_line_that_made_them() {
    let _lock = lock();
    let source = [
        "local function records()",                                        // 1
        "\tlocal list = table.create(100)",                                // 2
        "\tfor i = 1, 100 do",                                             // 3
        "\t\tlist[i] = { a = i, b = i, c = i }",                           // 4
        "\tend",                                                           // 5
        "\treturn list",                                                   // 6
        "end",                                                             // 7
        "local function grown()",                                          // 8
        "\tlocal t = {}",                                                  // 9
        "\tfor _, key in { \"k1\", \"k2\", \"k3\", \"k4\", \"k5\" } do",   // 10
        "\t\tt[key] = true",                                               // 11
        "\tend",                                                           // 12
        "\treturn t",                                                      // 13
        "end",                                                             // 14
        "local a, b = {}, {}",                                             // 15
        "a.other, b.other = b, a",                                         // 16
        "_G.Held = { records = records(), grown = grown(), cycle = a }",   // 17
    ]
    .join("\n");
    let json = shape_profile("shapes", &source);
    let snapshot = json["heap"]["snapshots"].as_array().unwrap().last().unwrap();
    assert_eq!(snapshot["label"], "end");
    let sizes = &snapshot["tables"]["sizes"];
    let (header, slot, node) = (sizes["table"].as_f64().unwrap(), sizes["slot"].as_f64().unwrap(), sizes["node"].as_f64().unwrap());
    assert_eq!((slot, node), (16.0, 32.0), "a TValue and a LuaNode");
    // 100 records of three fields: a hash part of 4 nodes, 3 used, each; tables that hold no table
    let records = site_at(&json, snapshot, 4);
    assert_eq!(records["count"], 100.0);
    assert_eq!(records["bytes"], 100.0 * (header + 4.0 * node));
    assert_eq!((records["hash_bytes"].as_f64(), records["array_bytes"].as_f64(), records["header_bytes"].as_f64()), (Some(100.0 * 4.0 * node), Some(0.0), Some(100.0 * header)));
    assert_eq!(distribution(&records["hash"]), (400.0, 300.0, 4.0, 4.0, 4.0, vec![(4, 100)]));
    assert_eq!(distribution(&records["array"]), (0.0, 0.0, 0.0, 0.0, 0.0, vec![(0, 100)]));
    assert_eq!(distribution(&records["levels"]).5, vec![(1, 100)]);
    assert_eq!((records["rehashes"].as_u64(), records["rehash_keys"].as_u64()), (Some(0), Some(0)), "made whole from a template");
    // their list: an array of 100 slots, all used, holding tables: two levels
    let list = site_at(&json, snapshot, 2);
    assert_eq!(list["count"], 1.0);
    assert_eq!(list["bytes"], header + 100.0 * slot);
    assert_eq!(distribution(&list["array"]), (100.0, 100.0, 100.0, 100.0, 100.0, vec![(128, 1)]));
    assert_eq!(distribution(&list["levels"]).5, vec![(2, 1)]);
    // a table grown key by key to five keys: 1, 2, 4 and 8 nodes, four rehashes moving 1 + 2 + 3 + 5 keys
    let grown = site_at(&json, snapshot, 9);
    assert_eq!(distribution(&grown["hash"]), (8.0, 5.0, 8.0, 8.0, 8.0, vec![(8, 1)]));
    assert_eq!((grown["rehashes"].as_u64(), grown["rehash_keys"].as_u64()), (Some(4), Some(11)));
    // two tables that hold each other: one level, one node each, each grew once
    let cycle = site_at(&json, snapshot, 15);
    assert_eq!(cycle["count"], 2.0);
    assert_eq!(distribution(&cycle["hash"]), (2.0, 2.0, 1.0, 1.0, 1.0, vec![(1, 2)]));
    assert_eq!(distribution(&cycle["levels"]).5, vec![(1, 2)]);
    assert_eq!((cycle["rehashes"].as_u64(), cycle["rehash_keys"].as_u64()), (Some(2), Some(2)));
    // what holds them all: three levels above the records' (it, the list, a record)
    let holder = site_at(&json, snapshot, 17);
    assert_eq!(distribution(&holder["levels"]).5, vec![(3, 1)]);
    assert_eq!(distribution(&holder["hash"]), (4.0, 3.0, 4.0, 4.0, 4.0, vec![(4, 1)]));
    // the snapshot's totals cover every reachable table, the engine's own among them
    let tables = &snapshot["tables"];
    assert!(tables["count"].as_u64().unwrap() >= 105);
    assert_eq!(tables["bytes"].as_u64().unwrap(), tables["header_bytes"].as_u64().unwrap() + tables["array_bytes"].as_u64().unwrap() + tables["hash_bytes"].as_u64().unwrap());
    // the rehashes by the line that made them grow and the line that made the table: the grown table's four, exactly
    let rehash = &json["alloc"]["rehash"];
    let functions = json["functions"].as_array().unwrap();
    let grown_id = functions.iter().find(|f| f["name"] == "grown").and_then(|f| f["id"].as_i64()).unwrap();
    let grown_site = rehash["sites"].as_array().unwrap().iter().find(|site| site["frames"][0][0].as_i64() == Some(grown_id) && site["frames"][0][1] == 11).unwrap_or_else(|| panic!("{rehash:#}"));
    assert_eq!(grown_site["table_frames"][0][1], 9, "the table made at line 9");
    let counts = |site: &serde_json::Value| ["events", "keys", "script_events", "script_keys", "grew"].map(|key| site[key].as_u64().unwrap());
    assert_eq!(counts(grown_site), [4, 11, 4, 11, 4]);
    assert_eq!((grown_site["bytes_before"].as_f64(), grown_site["bytes_after"].as_f64()), (Some((1.0 + 2.0 + 4.0) * node), Some((1.0 + 2.0 + 4.0 + 8.0) * node)));
    assert!(rehash["keys"].as_u64().unwrap() >= rehash["script_keys"].as_u64().unwrap() && rehash["script_keys"].as_u64().unwrap() >= 13, "{rehash}");
}

/// Each phase mark carries the counts so far, so that a phase's figures are the difference of two marks': the objects a
/// script's own instructions allocated, the keys its tables' growth moved (the VM's counter and the rehash hook agree),
/// the inline caches
#[test]
fn a_phase_is_the_difference_of_its_marks_counts() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::ALL, temp_dir("marks")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    crate::luau_profile::set_phase("before");
    run(&lua, &engine, "warm", "_G.Warm = {}\n");
    crate::luau_profile::set_phase("grow");
    // (a bound the compiler cannot see: with a constant one it sizes the table ahead and nothing rehashes)
    run(&lua, &engine, "grow", "local n = tonumber(\"10\")\nlocal t = {}\nfor i = 1, n do\n\tt[i] = {}\nend\n");
    crate::luau_profile::set_phase("after");
    profiler::collect(&lua);
    session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let marks = json["marks"].as_array().unwrap();
    let mark = |phase: &str| marks.iter().find(|mark| mark["phase"] == phase).unwrap_or_else(|| panic!("{marks:#?}"));
    let (grow, after) = (mark("grow"), mark("after"));
    let delta = |path: &[&str]| -> f64 {
        let read = |mark: &serde_json::Value| path.iter().fold(mark, |value, key| &value[*key]).as_f64().unwrap_or_else(|| panic!("{path:?} in {mark}"));
        read(after) - read(grow)
    };
    // 11 tables: the list and its ten; the list grew to 1, 2, 4, 8 and 16 slots, moving 1 + 2 + 3 + 5 + 9 keys
    assert_eq!(delta(&["vm", "objects"]), 11.0);
    assert_eq!(delta(&["vm", "rehashed"]), 20.0);
    assert_eq!(delta(&["script_rehash_keys"]), 20.0, "the rehash hook sees the keys the VM counts");
    assert_eq!(delta(&["script_rehashes"]), 5.0);
    assert!(delta(&["rehash_keys"]) >= 20.0 && delta(&["rehashes"]) >= 5.0);
    assert!(delta(&["allocs"]) >= 11.0 && delta(&["alloc_bytes"]) > 0.0);
    // (three instructions a step of the loop: NEWTABLE, SETTABLE, FORNLOOP)
    assert!(delta(&["insns"]) >= 30.0 && delta(&["work_ms"]) > 0.0, "{} instructions, {} ms of work", delta(&["insns"]), delta(&["work_ms"]));
    assert!(after["host_ms"].as_f64().unwrap() >= grow["host_ms"].as_f64().unwrap());
    assert!(after["caches"]["table key slot"].is_array(), "{after}");
    let end = &json["end"];
    assert_eq!(end["phase"], "end");
    assert!(end["insns"].as_f64().unwrap() >= after["insns"].as_f64().unwrap());
    // what the what-if runner reads of them
    let figures = profiler::whatif::phase_figures(&json, "grow", "after", 1.0);
    assert!(figures["allocs"] >= 11.0);
    assert_eq!((figures["rehash_keys"], figures["rehashes"]), (20.0, 5.0));
}

/// A level is one more than the deepest table a table holds; the tables of a cycle share theirs
#[test]
fn nesting_levels_follow_what_a_table_holds() {
    // 0 holds 1 and 2; 1 holds 3; 3 and 4 hold each other (a cycle) and 4 holds 5; 6 holds nothing
    let edges: [(u32, u32); 6] = [(0, 1), (0, 2), (1, 3), (3, 4), (4, 3), (4, 5)];
    let n = 7;
    let mut offsets = vec![0u32; n + 1];
    for &(from, _) in &edges {
        offsets[from as usize + 1] += 1;
    }
    for at in 0..n {
        offsets[at + 1] += offsets[at];
    }
    let targets: Vec<u32> = edges.iter().map(|&(_, to)| to).collect();
    assert_eq!(memory::table_levels(n, &offsets, &targets), vec![4, 3, 1, 2, 2, 1, 1]);
    let mut sizes = vec![(3, 3), (0, 0), (16, 9), (4, 1)];
    let distribution = memory::Distribution::of(&mut sizes, true);
    assert_eq!((distribution.total, distribution.used, distribution.min, distribution.median, distribution.max), (23, 13, 0, 3, 16));
    assert_eq!(distribution.hist, vec![(0, 1), (4, 2), (16, 1)]);
}

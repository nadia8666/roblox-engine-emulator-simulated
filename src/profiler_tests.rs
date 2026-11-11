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

mod cold;

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

/// A handler queued while no profile ran still knows, when a profile later delivers it, that its price is a Studio-era one: the job
/// keeps its signal kind, not the kind bits of the moment it was queued.
#[test]
fn a_handler_queued_before_a_profile_keeps_its_price_provenance() {
    let _lock = lock();
    let (lua, engine) = start();
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Queued")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).unwrap();
    // A direct call leaves the handler queued; Engine.Call would flush it before profiling begins.
    let source = r#"
        local Part = Instance.new("Part")
        Part:GetPropertyChangedSignal("Transparency"):Connect(function() end)
        Part.Transparency = 0.5
    "#;
    lua.load(source).set_name("=queue-before-profile").set_environment(env).exec().unwrap();
    assert!(engine.state.borrow().sched.kinds.is_none());

    let config = Config { print: false, ..Config::new(Kinds::WORK, temp_dir("queued-before-profile")) };
    let session = profiler::Session::begin(config);
    profiler::install(&lua);
    scheduler::flush_deferred(&lua, &engine).unwrap();
    let totals = engine.state.borrow().sched.kinds.as_ref().unwrap().totals();
    profiler::collect(&lua);
    let run = profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() };
    session.finish(run).unwrap();

    // (the order of cost.rs `kind`'s bits)
    let [_cold, _collector, studio, defaulted, dispatch] = totals;
    let property_handler_dispatch = 452.01e-9;
    let tolerance = 1e-18;
    println!("queued before profile: dispatch {dispatch} s, Studio {studio} s");
    assert!((dispatch - property_handler_dispatch).abs() < tolerance);
    assert!((studio - property_handler_dispatch).abs() < tolerance, "the delivered property handler keeps its Studio price");
    assert_eq!(defaulted, 0.0);
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
    assert!(unsafe { vm::start(state, true, None, None) });
    function.call::<()>(()).unwrap();
    unsafe { vm::stop(state) };
    let after = ops(&lua);
    let results = unsafe { vm::results(state) };
    unsafe { vm::free(state) };
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
            assert!(unsafe { vm::start(state, true, Some(&prices), Some((work.as_ptr() as *const f64, drained.as_ptr() as *const f64))) });
        }
        let output = run(&lua, &engine, "work", script);
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
        let after = engine.state.borrow().sched.work_total();
        let mut booked = 0.0;
        if profiled {
            unsafe { vm::stop(state) };
            let results = unsafe { vm::results(state) };
            unsafe { vm::free(state) };
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

/// The inline caches are read as the interpreter does them: a table key read from one table hits its slot after its first
/// lookup; a key the table lacks is absent every time (a full lookup, no metatable walked); a key its metatable's __index table
/// gives walks one metatable; a global hits its slot. (The table is a clone: a compiler since Luau 0.744 reads a local table
/// that a constructor built and nothing changes as constants.)
#[test]
fn inline_caches_tell_what_each_lookup_did() {
    let _lock = lock();
    let lua = Lua::new();
    lua.enable_jit(false);
    let state = main_state(&lua);
    let source = "local same = table.clone({a = 1, b = 2, c = 3})\nlocal x = 0\nfor i = 1, 100 do\n\tx += same.c\nend\nlocal empty = {}\nfor i = 1, 100 do\n\tx += empty.c or 0\nend\nlocal inherited = setmetatable({}, {__index = {c = 1}})\nfor i = 1, 100 do\n\tx += inherited.c\nend\nG = 0\nfor i = 1, 100 do\n\tG += 1\nend\nresult = x\n";
    let function = lua.load(source).set_name("=caches").into_function().unwrap();
    assert!(unsafe { vm::start(state, true, None, None) });
    function.call::<()>(()).unwrap();
    unsafe { vm::stop(state) };
    let results = unsafe { vm::results(state) };
    unsafe { vm::free(state) };
    let main = results.functions.iter().find(|f| f.source == "caches" && f.line == 0).unwrap_or_else(|| &results.functions[0]);
    let outcomes = |line: i32, op: &str| -> ([u64; vm::CACHE_OUTCOMES], [u64; vm::CACHE_DEPTHS]) {
        let (mut total, mut depths) = ([0u64; vm::CACHE_OUTCOMES], [0u64; vm::CACHE_DEPTHS]);
        for site in main.caches.iter().filter(|site| main.lines[site.pc] == line && vm::opcode_name(site.op) == op) {
            for (into, count) in total.iter_mut().zip(site.outcomes.iter()) {
                *into += count;
            }
            for (into, count) in depths.iter_mut().zip(site.depths.iter()) {
                *into += count;
            }
        }
        (total, depths)
    };
    let (same, _) = outcomes(4, "GETTABLEKS");
    assert_eq!(same.iter().sum::<u64>(), 100);
    assert!(same[vm::HIT] >= 99 && same[vm::HIT] + same[vm::REPREDICTED] == 100, "one shape: hits after the first lookup {same:?}");
    let (absent, _) = outcomes(8, "GETTABLEKS");
    let mut expected = [0u64; vm::CACHE_OUTCOMES];
    expected[vm::ABSENT] = 100;
    assert_eq!(absent, expected, "a key the table lacks: absent, a full lookup every time");
    let (inherited, depths) = outcomes(12, "GETTABLEKS");
    let mut expected = [0u64; vm::CACHE_OUTCOMES];
    expected[vm::METATABLE] = 100;
    assert_eq!(inherited, expected, "a key its metatable's __index table gives: a metatable walk");
    assert_eq!(depths, [100, 0, 0, 0], "one __index table consulted");
    let (globals, _) = outcomes(16, "GETGLOBAL");
    assert!(globals[vm::HIT] >= 99 && globals.iter().sum::<u64>() == 100, "{globals:?}");
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
    let (block, kind) = unsafe { vm::alloc_block(state) };
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
    let total_before = unsafe { vm::total_bytes(state) };
    // the collector itself
    unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
    let resolver = memory::resolver(Rc::downgrade(&engine));
    let collected = unsafe { memory::snapshot(state, "collected", 0, 0.0, Some(&*resolver), 5) };
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
    let freed = total_before - unsafe { vm::total_bytes(state) };
    assert!(freed as u64 >= end["unswept"]["bytes"].as_u64().unwrap(), "freed {freed}, garbage {}", end["unswept"]);
    // once the collector has freed what it found, the walk reaches every object left: nothing the collector keeps (a
    // name, a metatable, a fixed string) is taken for garbage. (A collection can leave garbage of its own: a userdata it
    // frees can let go of a reference, so the walk is checked again after each one.)
    let mut settled = unsafe { memory::snapshot(state, "settled", 0, 0.0, Some(&*resolver), 5) };
    for _ in 0..4 {
        if settled.unswept == (0, 0) {
            break;
        }
        unsafe { mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCCOLLECT, 0) };
        settled = unsafe { memory::snapshot(state, "settled", 0, 0.0, Some(&*resolver), 5) };
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
    while unsafe { vm::gc_state(state) } != 4 {
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

/// The timeline has a row a frame, with the frame's work, the heap, the live instances by where they are, connections and the
/// threads made.
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
    // (the Folders are held by a table, outside the DataModel)
    assert_eq!(last[column("instances_outside")].as_u64().unwrap() - first[column("instances_outside")].as_u64().unwrap(), 9, "a Folder a frame");
    assert_eq!(last[column("instances_in_datamodel")], first[column("instances_in_datamodel")]);
    assert!(rows.iter().all(|row| row[column("threads_made")].as_u64().unwrap() >= 1), "a thread made a frame: {timeline}");
    assert!(rows.iter().all(|row| row[column("work_ms")].as_f64().unwrap() > 0.0), "each frame's handler did work");
}

/// Runs `ree run` in this process on a harness (`source`), with a game module mounted at ReplicatedStorage.Game.Spin
/// (`game`), under `--profile <kinds>`: profile.json and the report page
fn run_profiled_page(name: &str, source: &str, game: &str, kinds: &str, seconds: &str) -> (serde_json::Value, String) {
    let dir = temp_dir(name);
    std::fs::create_dir_all(dir.join("game")).unwrap();
    let harness = dir.join(format!("{name}.luau"));
    std::fs::write(&harness, source).unwrap();
    std::fs::write(dir.join("game").join("Spin.luau"), game).unwrap();
    let out = dir.join("profile");
    let mount = format!("ReplicatedStorage/Game={}", dir.join("game").to_string_lossy().replace('\\', "/"));
    let args: Vec<String> = ["--harness", harness.to_str().unwrap(), "--mount", &mount, "--seconds", seconds, "--profile", kinds, "--profile-out", out.to_str().unwrap()].iter().map(|arg| arg.to_string()).collect();
    // (on a thread with a service worker's stack, as the ree binary runs its commands)
    let code = std::thread::Builder::new().stack_size(64 << 20).spawn(move || crate::record::run::cli(&args, &crate::launch::Host::default(), false)).unwrap().join().unwrap();
    assert_eq!(code, 0, "ree run {name}");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("profile.json")).unwrap()).unwrap();
    let page = std::fs::read_to_string(out.join("report.html")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (json, page)
}

/// A phase mark inside a frame splits the frame's timeline row: the frame so far is the phase before's row (part 1), the
/// rest the new phase's (part 2), both under the frame's number. With work, each row's work splits between the game's code
/// and the harness's as the summary splits a phase (over the steady rows they add up to the summary's own figures), and a
/// row names its heaviest functions
#[test]
fn a_mark_inside_a_frame_splits_its_row_and_each_row_splits_its_work_by_side() {
    let _lock = lock();
    let harness = "return function(Context)\n\tlocal RunService = game:GetService(\"RunService\")\n\tlocal Spin = require(game.ReplicatedStorage.Game.Spin)\n\tlocal frame = 0\n\tRunService.Heartbeat:Connect(function()\n\t\tframe += 1\n\t\tif frame == 3 then\n\t\t\tlocal x = 0\n\t\t\tfor i = 1, 20000 do\n\t\t\t\tx += i % 5\n\t\t\tend\n\t\t\tContext.ProfilePhase(\"steady\")\n\t\tend\n\t\tSpin.spin(if frame == 6 then 300000 else 300)\n\tend)\n\ttask.wait(Context.Seconds)\nend\n";
    let game = "local M = {}\nfunction M.spin(n)\n\tlocal x = 0\n\tfor i = 1, n do\n\t\tx += i % 7\n\tend\n\treturn x\nend\nreturn M\n";
    let (json, page) = run_profiled_page("split", harness, game, "work,timeline", "0.25");
    let timeline = &json["timeline"];
    let columns: Vec<&str> = timeline["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    let column = |name: &str| columns.iter().position(|c| *c == name).unwrap_or_else(|| panic!("no {name} in {columns:?}"));
    let rows = timeline["rows"].as_array().unwrap();
    let get = |row: &serde_json::Value, name: &str| row[column(name)].as_f64().unwrap();
    let phases: Vec<&str> = json["phases"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    let steady = phases.iter().position(|p| *p == "steady").unwrap() as f64;
    // the frame the harness marked steady in: two rows, one number, the part before the mark the phase before's
    let first = rows.iter().position(|row| get(row, "phase") == steady).unwrap();
    let (before, after) = (&rows[first - 1], &rows[first]);
    assert_eq!(get(before, "frame"), get(after, "frame"), "{before} {after}");
    assert_eq!((get(after, "part") - get(before, "part")), 1.0);
    assert!(get(before, "part") >= 1.0 && get(before, "phase") != steady);
    // the harness's 20,000 steps before the mark are the phase before's, in its part of the frame
    let typical_harness = get(&rows[first + 2], "harness_ms");
    assert!(get(before, "harness_ms") > 20.0 * typical_harness.max(1e-6), "{before} against {}", rows[first + 2]);
    assert!(get(after, "harness_ms") < get(before, "harness_ms"), "{after}");
    // a row's sides are the work booked to code: never more than the row's work (the rest charged outside instructions)
    for row in rows {
        assert!(get(row, "game_ms") + get(row, "harness_ms") <= get(row, "work_ms") + 1e-4, "{row}");
    }
    // over the steady rows, each side adds up to the summary's own split of the phase (each row rounded to 10 ns)
    let sum = |name: &str| rows.iter().filter(|row| get(row, "phase") == steady).map(|row| get(row, name)).sum::<f64>();
    let tolerance = 1e-5 * rows.len() as f64;
    assert!((sum("game_ms") - json["scope"]["game_self_ms"].as_f64().unwrap()).abs() <= tolerance, "game {} against {}", sum("game_ms"), json["scope"]);
    assert!((sum("harness_ms") - json["scope"]["harness_self_ms"].as_f64().unwrap()).abs() <= tolerance, "harness {} against {}", sum("harness_ms"), json["scope"]);
    // the frame the game spun 300,000 steps in names spin first, on the game's side
    let functions: std::collections::HashMap<i64, &serde_json::Value> = json["functions"].as_array().unwrap().iter().map(|f| (f["id"].as_i64().unwrap(), f)).collect();
    let spike = (0..rows.len()).filter(|&i| get(&rows[i], "phase") == steady).max_by(|&a, &b| get(&rows[a], "game_ms").total_cmp(&get(&rows[b], "game_ms"))).unwrap();
    // Small events keep their peak and active count even when their frames need no node history.
    let stat = timeline["function_stats"].as_array().unwrap().iter().find(|stat| {
        functions[&stat[0].as_i64().unwrap()]["name"].as_str().is_some_and(|name| name == "spin" || name.ends_with(".spin"))
    }).unwrap();
    let function = functions[&stat[0].as_i64().unwrap()];
    assert_eq!(function["harness"], false);
    assert_eq!(stat[4].as_f64().unwrap(), get(&rows[spike], "frame"));
    assert!((stat[3].as_f64().unwrap() / 1e3 - get(&rows[spike], "game_ms")).abs() <= 0.01 * get(&rows[spike], "game_ms"));
    assert!(page.contains("\"timeline\":{") && !page.contains("\"function_stats\":"), "the page leaves summaries out");
    assert!(!page.contains("\"node_frames\":"));
    // the summary counts frames, not the parts of one
    let summary = json["summary"].as_str().unwrap();
    assert!(summary.contains("split by phase marks") && summary.contains("the game's code"), "{summary}");
}

/// Function activity unions callers and new closures, but cannot price a replacement's unobserved executed code.
#[test]
fn builtin_argument_setup_counts_frames_across_callers_and_new_closures() {
    let _lock = lock();
    let harness = r#"return function(Context)
    local M = require(game.ReplicatedStorage.Game.Spin)
    local frame = 0
    task.wait()
    Context.ProfilePhase("steady")
    game:GetService("RunService").Heartbeat:Connect(function()
        frame += 1
        local read = M.make(frame)
        M.left(read, if frame % 2 == 0 then "abcdefgh" else nil)
        M.right(read, nil)
    end)
    task.wait(Context.Seconds)
end"#;
    let game = r#"local M = {}
function M.make(seed)
    local function Read(value)
        if value then return string.sub(value, 2) end
        return seed
    end
    return Read
end
function M.left(read, value) return read(value) end
function M.right(read, value) return read(value) end
return M"#;
    let (json, _) = run_profiled_page("builtin-cold-union", harness, game, "work,caches", "0.2");
    let functions = json["functions"].as_array().unwrap();
    let reads: Vec<_> = functions.iter().filter(|function| function["name"] == "Read").collect();
    assert_eq!(reads.len(), 1, "new closures share their prototype: {reads:?}");
    let function = reads[0]["id"].as_i64().unwrap();
    let stats = json["timeline"]["function_stats"].as_array().unwrap();
    let stat = stats.iter().find(|stat| stat[0].as_i64() == Some(function)).unwrap();
    let caller_frames = |name: &str| {
        let caller = functions.iter().find(|function| function["name"].as_str().is_some_and(|label| label == name || label.ends_with(&format!(".{name}")))).unwrap();
        stats.iter().find(|stat| stat[0] == caller["id"]).unwrap()[2].as_u64().unwrap()
    };
    let frames = caller_frames("left");
    assert!(frames >= 10);
    assert_eq!(json["luau_caches"]["frames"].as_f64(), Some(frames as f64), "cache rates use the same physical frames as the function summary");
    assert_eq!(caller_frames("right"), frames);
    assert_eq!(stat[2].as_u64(), Some(frames), "both callers and skipped builtin frames count once");
    let builtin = json["builtins"].as_array().unwrap().iter().find(|builtin| builtin["name"] == "string.sub").unwrap();
    let site = builtin["sites"].as_array().unwrap().iter().find(|site| site["function"].as_i64() == Some(function)).unwrap();
    let declines: u64 = site["declines"].as_object().unwrap().values().map(|count| count.as_u64().unwrap()).sum();
    assert!(declines > 0 && declines < frames, "the site skips half the frames");
    assert!(site["fix_cold_us"].is_null());
    assert!(site["fix_unpriced"].as_str().unwrap().contains("executed instructions"));
    assert!(site["fix_instructions_us"].as_f64().unwrap() > 0.0);
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
    // The samples' wall time is what adds up to the run's wall time. Their thread-CPU weight (`sampled_ms` when the weight
    // is the thread's CPU) falls short of it whenever other work shares the machine, so it is checked against the samples'
    // own wall time instead.
    let sampled_wall = json["time"]["wall_ms"].as_f64().unwrap();
    assert!(wall > 20.0, "the script ran long enough to measure: {wall:.1} ms");
    assert!(sampled_wall >= 0.5 * wall - 5.0 && sampled_wall <= wall + 2.0, "sampled {sampled_wall} ms of wall time in a {wall:.1} ms run");
    assert!(sampled > 0.0 && sampled <= sampled_wall + 2.0, "the thread's CPU time {sampled} ms within the {sampled_wall} ms of wall time sampled");
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

/// ree's own caches are counted while a profile asks, under the summary's "emulator": a property read twice hits the member
/// dispatch the second time, and an Instance pushed again hits its userdata; a member name's atom is a miss when the VM looks a
/// new string up by its bytes (1,000 keys made at run time: 1,000 misses), a hit when read off the string. What they say of the
/// game itself is listed as its facts: the instances the scripts touched, the datatype property reads.
#[test]
fn ree_caches_count_hits_and_misses() {
    let _lock = lock();
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::CACHES, temp_dir("caches")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(
        &lua,
        &engine,
        "caches",
        "local part = Instance.new(\"Part\")\nlocal folder = Instance.new(\"Folder\")\npart.Parent = folder\nfor i = 1, 100 do\n\tlocal _ = part.Transparency\n\tlocal _ = part.Parent\n\tlocal _ = part.Color\nend\nfor i = 1, 1000 do\n\tpcall(function()\n\t\treturn folder[\"Run\" .. i]\n\tend)\nend\nfor i = 1, 9 do\n\tInstance.new(\"Part\", folder)\nend\nlocal _ = folder:GetChildren()\n",
    );
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    assert!(json.get("ree_caches").is_none(), "ree's caches are the emulator's");
    let cache = |name: &str| json["emulator"]["caches"].as_array().unwrap().iter().find(|row| row["name"] == name).cloned().unwrap();
    let members = cache("member dispatch");
    assert!(members["hits"].as_u64().unwrap() >= 198, "{members}");
    let wrappers = cache("instance userdata");
    assert!(wrappers["hits"].as_u64().unwrap() >= 99, "the folder's userdata found again: {wrappers}");
    let atoms = cache("member atoms");
    assert!(atoms["misses"].as_u64().unwrap() >= 1000, "each key made at run time is looked up by its bytes: {atoms}");
    assert!(atoms["hits"].as_u64().unwrap() >= 300, "a member name's atom read off the string: {atoms}");
    let game = &json["game"];
    // (the part, the folder, the nine parts made in it and listed)
    assert!(game["instances_touched"].as_u64().unwrap() >= 11, "{game}");
    // (the caches' counts are the process's: another test's engine can add to them, never take from them)
    assert!(game["datatype_reads"].as_u64().unwrap() >= 100, "{game}");
    assert!(game["datatype_reads_reused"].as_u64().unwrap() >= 99, "{game}");
    assert!(summary.contains("game facts") && summary.contains("emulator: ree's own caches"), "{summary}");
    assert!(json["luau_caches"]["by_kind"].as_array().is_some(), "Luau's caches come with the stepping");
}

/// The command line: kinds parse with their aliases; `--profile` alone and `--profile boundary` alone keep their own
/// forms (the phases, the boundary profiler) and give no suite profile; a kinds value takes `--profile-out`.
#[test]
fn profile_options_keep_the_old_forms() {
    let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert_eq!(Kinds::parse("all").unwrap(), Kinds::ALL);
    // (the boundary's crossings are in `all`: one profile holds one issue list; host time and the counters are not)
    assert!(Kinds::ALL.has(Kinds::BOUNDARY) && !Kinds::ALL.has(Kinds::TIME) && !Kinds::ALL.has(Kinds::HW));
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
    assert!(page.contains("\"version\":2") && !page.contains("/*PROFILE_DATA*/"), "the page carries the profile");
    let speedscope: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(before.join("profile.speedscope.json")).unwrap()).unwrap();
    assert!(speedscope["profiles"].as_array().unwrap().len() >= 3);
    let load = |dir: &std::path::Path| -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(dir.join("profile.json")).unwrap()).unwrap() };
    let (same, _) = profiler::diff::compare(&load(&before), &load(&before), 10);
    assert!(!same.contains("functions that moved most"), "{same}");
    let (moved, json) = profiler::diff::compare(&load(&before), &load(&after), 10);
    assert!(moved.contains("hot"), "{moved}");
    let hot = json["functions"].as_array().unwrap().iter().find(|row| row["function"].as_str().unwrap().starts_with("M.hot") || row["function"].as_str().unwrap().starts_with("hot")).unwrap_or_else(|| panic!("{moved}"));
    assert!(hot["after"]["calls"].as_f64().unwrap() == 2.0 * hot["before"]["calls"].as_f64().unwrap(), "{hot}");
    // across versions, what changed meaning is left out: self work (a C function's own row since version 2), a function's calls
    // and instructions (version 1 counted a module's main chunk at none), lines, host time, the caches; allocations still compare
    let mut older = load(&before);
    older["version"] = serde_json::json!(1);
    let (across, json) = profiler::diff::compare(&older, &load(&after), 10);
    assert!(across.contains("the profiles are of two versions (1 and 2)") && !across.contains("lines that moved most") && !across.contains("Luau inline caches"), "{across}");
    assert!(across.contains("a function's calls and instructions (version 1 counted a module's main chunk at none)"), "{across}");
    let hot = json["functions"].as_array().unwrap().iter().find(|row| row["function"].as_str().unwrap().contains("hot")).unwrap_or_else(|| panic!("{across}"));
    for (side, figure) in [("before", "self_work_ms"), ("after", "self_work_ms"), ("before", "calls"), ("after", "calls"), ("before", "insns"), ("after", "insns"), ("after", "time_ms")] {
        assert!(hot[side][figure].is_null(), "{side} {figure} is not compared: {hot}");
    }
    assert!(hot["after"]["alloc_bytes"].as_f64().unwrap() > hot["before"]["alloc_bytes"].as_f64().unwrap(), "allocations compare: {hot}");
    let row = across.lines().find(|line| line.contains("hot") && line.contains(" -> ")).unwrap_or_else(|| panic!("{across}"));
    assert_eq!(row.split_whitespace().take(9).filter(|word| *word == "-").count(), 6, "self work, calls and instructions print as -: {row}");
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
/// and the other counters are fixed shares of it): every window between readings is booked once, a reading's own counts
/// taken out, so the run's cycles are the thread's own cycles between its start and end readings, inside what the test
/// reads around the profile; the VM's hook reads around the Luau calls it samples, and the phases and the functions add
/// up to the run exactly, every frame has its row, and the busy function holds most of it.
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
    assert_eq!(counters["attribution"]["method"], "calls");
    assert!(counters["attribution"]["calls"].as_u64().unwrap() >= 20, "the VM's hook read around its calls: {}", counters["attribution"]);
    assert!(counters["readings"]["taken_out"].as_bool().unwrap(), "a reading's own counts come out of the run's");
    // the cost figures: the mock's stall cycles are a quarter of its cycles
    let share = counters["run"]["rates"]["load stall share"].as_f64().unwrap();
    assert!((share - 0.25).abs() < 0.01, "{share}");
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
    // every count is some function's own: the functions add up to the run (an estimate may be below zero for one), and
    // the busy function holds most of it
    let functions = counters["functions"].as_array().unwrap();
    assert_eq!(functions.iter().map(|f| f["self"]["cycles"].as_i64().unwrap()).sum::<i64>(), cycles as i64, "{summary}");
    let spin = functions.iter().find(|f| f["name"].as_str().is_some_and(|name| name.ends_with("spin"))).unwrap_or_else(|| panic!("{summary}"));
    assert!(spin["self"]["cycles"].as_i64().unwrap() as f64 >= 0.5 * cycles as f64, "{spin}\n{summary}");
    assert!(spin["self_rates"]["load stall share"].as_f64().is_some(), "{spin}");
    assert!(functions.iter().any(|f| f["kind"] == "engine step"), "the engine's own steps have frames of their own");
    // (the folded file leaves out a stack whose estimate fell below zero)
    let folded_total: u64 = folded.lines().map(|line| line.rsplit(' ').next().unwrap().parse::<u64>().unwrap()).sum();
    assert!((folded_total as f64 - cycles as f64).abs() <= 0.01 * cycles as f64, "the folded stacks hold the run: {folded_total} of {cycles}");
    // the call sites: what the calls made from a line cost, the line that calls spin among them
    let lines = counters["lines"].as_array().unwrap();
    assert!(lines.iter().all(|line| line["calls"] == true && line["self"].as_object().is_some_and(|own| own.is_empty())), "{lines:?}");
    assert!(lines.iter().any(|line| line["total"]["cycles"].as_i64().unwrap_or(0) as f64 >= 0.5 * cycles as f64), "the line that calls spin: {lines:?}");
    assert!(summary.contains("hardware counters") && summary.contains("IPC 2.00") && summary.contains("memory: 25.0 %"), "{summary}");
}

/// Calls shorter than the shortest sampled are read around only until their function's first calls measured them, and
/// then one now and then (less often each time): a script calling a tiny function a hundred thousand times takes a few
/// readings for it, not one a call, and the tiny function's counts are all its caller's, those first calls' too
#[test]
fn hardware_counters_leave_short_calls_to_their_callers() {
    let _lock = lock();
    profiler::hw::TEST_MODE.with(|mode| mode.set(Some("mock")));
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::HW, temp_dir("hw-short")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, "tiny", "local M = {}
function M.tiny(x)
	return x + 1
end
local total = 0
for i = 1, 100000 do
	total = M.tiny(total)
end
result = total
");
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() }).unwrap();
    profiler::hw::TEST_MODE.with(|mode| mode.set(None));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let attribution = &json["counters"]["attribution"];
    let calls = attribution["calls"].as_u64().unwrap();
    // (its first 4 calls, then one after 64, 128, ... 65,536 more: 4 + 10 by the hundred thousandth, and the chunk's)
    assert!((14..=20).contains(&calls), "a hundred thousand tiny calls read around {calls} times: {attribution}
{summary}");
    assert!(attribution["unsampled_functions"].as_u64().unwrap() >= 1, "{attribution}");
    assert!(attribution["calls_booked_to_callers"].as_u64().unwrap() >= 14, "every read of the tiny function is its caller's: {attribution}");
    let functions = json["counters"]["functions"].as_array().unwrap();
    assert!(functions.iter().all(|f| !f["name"].as_str().is_some_and(|name| name.ends_with("tiny")) || f["self"]["cycles"].as_i64() == Some(0)), "{summary}");
}

/// Runs `source` (a chunk named `name`) under the mock counters (each reading takes the thread's cycle count, so the
/// booking splits real cycles), then `frames` frames: each function's own cycles by name, the attribution, the summary
fn booked_cycles(name: &str, source: &str, frames: usize) -> (std::collections::HashMap<String, f64>, serde_json::Value, String) {
    let _lock = lock();
    profiler::hw::TEST_MODE.with(|mode| mode.set(Some("mock")));
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::HW, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, name, source);
    for _ in 0..frames {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: frames as u64, ..Default::default() }).unwrap();
    profiler::hw::TEST_MODE.with(|mode| mode.set(None));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let mut own: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for function in json["counters"]["functions"].as_array().unwrap() {
        let name = function["name"].as_str().unwrap_or("").rsplit(['.', ':']).next().unwrap_or("").to_string();
        *own.entry(name).or_default() += function["self"]["cycles"].as_f64().unwrap_or(0.0);
    }
    (own, json["counters"]["attribution"].clone(), summary)
}

/// A function calling itself four deep, its calls sampled one in several: each call is drawn on its own, so the
/// function's own cycles are its calls' (the caller's loop keeps next to nothing), not the bias one draw for each run of
/// calls gave (the nested call and its caller never sampled together)
#[test]
fn hardware_counters_book_a_recursive_function_its_own() {
    // (25 us calls against the 100 us threshold: one call in four is sampled once learned, however fast or loaded the host)
    let _fixed_call_length = profiler::hw::fixed_call_us(25.0);
    let (own, attribution, summary) = booked_cycles(
        "rec",
        "local work = {}
function work.f(n, k)
	if n > 0 then
		work.f(n - 1, k)
	end
	local x = 0
	for i = 1, k do
		x += i % 7
	end
	return x
end
local function main()
	local total = 0
	for round = 1, 3000 do
		total += work.f(3, 3000)
	end
	return total
end
result = main()
",
        0,
    );
    let (f, main) = (own.get("f").copied().unwrap_or(0.0), own.get("main").copied().unwrap_or(0.0));
    let calls = attribution["calls"].as_u64().unwrap();
    assert!(calls < 9000, "f's 12,000 calls sampled one in several: {calls} read\n{summary}");
    assert!(f > 0.0 && main > -0.05 * f && main < 0.05 * f, "f {f}, its caller's loop {main}\n{summary}");
}

/// A coroutine's body runs between the readings where it resumes and yields: its loop is its own, as the same loop in
/// a function called beside it is that function's, and the code resuming it keeps next to nothing
#[test]
fn hardware_counters_book_a_coroutine_its_own() {
    let (own, attribution, summary) = booked_cycles(
        "co",
        "local work = {}
function work.compute(n)
	local x = 0
	for i = 1, n do
		x += i % 7
	end
	return x
end
local function generator(n)
	while true do
		local x = 0
		for i = 1, n do
			x += i % 7
		end
		n = coroutine.yield(x)
	end
end
local body = coroutine.wrap(generator)
local function main()
	local total = 0
	for round = 1, 600 do
		total += body(8000)
		total += work.compute(8000)
	end
	return total
end
result = main()
",
        0,
    );
    let (generator, compute, main) = (own.get("generator").copied().unwrap_or(0.0), own.get("compute").copied().unwrap_or(0.0), own.get("main").copied().unwrap_or(0.0));
    assert!(attribution["switch_readings"].as_u64().unwrap() >= 1000, "a reading where the coroutine resumes and yields: {attribution}");
    assert!(compute > 0.0 && (generator / compute - 1.0).abs() < 0.25, "the coroutine's loop {generator}, the same loop called {compute}\n{summary}");
    assert!(main.abs() < 0.15 * compute, "the code resuming it {main}\n{summary}");
}

/// Two threads the engine resumes each frame, one running its loop inline, one calling a function with the same loop:
/// each thread's stretch is its own, whichever thread's reading closed it
#[test]
fn hardware_counters_book_each_thread_its_own() {
    let (own, attribution, summary) = booked_cycles(
        "task",
        "local work = {}
function work.compute(n)
	local x = 0
	for i = 1, n do
		x += i % 7
	end
	return x
end
local function inlineLoop()
	while true do
		local x = 0
		for i = 1, 30000 do
			x += i % 7
		end
		task.wait()
	end
end
local function callingLoop()
	while true do
		work.compute(30000)
		task.wait()
	end
end
task.spawn(inlineLoop)
task.spawn(callingLoop)
",
        60,
    );
    let (inline, calling, compute) = (own.get("inlineLoop").copied().unwrap_or(0.0), own.get("callingLoop").copied().unwrap_or(0.0), own.get("compute").copied().unwrap_or(0.0));
    assert!(attribution["switch_readings"].as_u64().unwrap() >= 200, "{attribution}");
    assert!(compute > 0.0 && (inline / compute - 1.0).abs() < 0.25, "the inline loop {inline}, the called one {compute}\n{summary}");
    assert!(calling.abs() < 0.15 * compute, "the calling thread's own {calling}\n{summary}");
}

/// A function whose first call is short and the rest long is sampled from its first calls on (one short call decides
/// nothing); one whose first calls are all short is measured again and sampled once its calls grew
#[test]
fn hardware_counters_sample_a_function_whose_first_calls_were_short() {
    let source = |short: usize| {
        format!(
            "local work = {{}}
function work.process(n)
	local x = 0
	for i = 1, n do
		x += i % 7
	end
	return x
end
local function main()
	local total = 0
	for i = 1, {short} do
		total += work.process(10)
	end
	for round = 1, 300 do
		total += work.process(30000)
	end
	return total
end
result = main()
"
        )
    };
    let (own, _, summary) = booked_cycles("sticky", &source(1), 0);
    let (process, main) = (own.get("process").copied().unwrap_or(0.0), own.get("main").copied().unwrap_or(0.0));
    assert!(process > 0.0 && main.abs() < 0.05 * process, "one short call first: process {process}, its caller {main}\n{summary}");
    // (ten short calls: none sampled after the fourth; the 64th call after it is measured, long, and sampling resumes)
    let (own, _, summary) = booked_cycles("sticky-10", &source(10), 0);
    let (process, main) = (own.get("process").copied().unwrap_or(0.0), own.get("main").copied().unwrap_or(0.0));
    assert!(process > 0.6 * (process + main), "ten short calls first: process {process}, its caller {main}\n{summary}");
}

/// A function long at first and short later keeps its long calls: whether its first calls are its callers' is decided
/// when its fourth is measured, not by how it is sampled at the run's end (two long builds then 20,000 tiny calls lost
/// both builds to the caller)
#[test]
fn hardware_counters_keep_a_functions_long_first_calls_when_it_shrinks() {
    let source = |tiny: usize| {
        format!(
            "local work = {{}}
function work.build(n)
	local x = 0
	for i = 1, n do
		x += i % 7
	end
	return x
end
function work.main()
	local total = work.build(600000) + work.build(600000)
	for _ = 1, {tiny} do
		total += work.build(1)
	end
	return total
end
result = work.main()
"
        )
    };
    let (alone, _, _) = booked_cycles("shrink-alone", &source(0), 0);
    let (shrunk, attribution, summary) = booked_cycles("shrink", &source(20_000), 0);
    // (the mock's cycles are the thread's own, which move a quarter from run to run on a busy machine; decided at the
    // run's end, the function kept none of them)
    let (builds, kept) = (alone.get("build").copied().unwrap_or(0.0), shrunk.get("build").copied().unwrap_or(0.0));
    assert!(builds > 0.0 && kept > 0.5 * builds, "the two builds: {builds} alone, {kept} before 20,000 tiny calls\n{summary}");
    assert!(attribution["unsampled_functions"].as_u64().unwrap() >= 1, "by the end it is too short to sample: {attribution}");
}

/// Threads that died by an error inside sampled calls of a recursive function, or were closed there, give those calls
/// up: the recursion's inner calls are still read with their sampled outer one afterwards (the function's count of open
/// sampled calls had stayed up, the nested reads stopped, and the caller's share swung by tens of percent)
#[test]
fn hardware_counters_read_recursion_whole_after_threads_died_in_it() {
    let source = |before: &str| {
        format!(
            "local work = {{}}
function work.f(depth, n, fail)
	if depth > 0 then
		work.f(depth - 1, n, fail)
	end
	local x = 0
	for i = 1, n do
		x += i % 7
	end
	if fail == \"error\" and depth == 0 then
		error(\"down\")
	elseif fail == \"yield\" and depth == 0 then
		coroutine.yield()
	end
	return x
end
function work.main()
	local total = 0
	for _ = 1, 3000 do
		total += work.f(3, 3000)
	end
	return total
end
{before}
result = work.main()
"
        )
    };
    let (_, clean, _) = booked_cycles("rec-clean", &source(""), 0);
    let died = "for _ = 1, 3 do\n\tcoroutine.resume(coroutine.create(function()\n\t\twork.f(3, 3000, \"error\")\n\tend))\nend";
    let closed = "for _ = 1, 3 do\n\tlocal co = coroutine.create(function()\n\t\twork.f(3, 3000, \"yield\")\n\tend)\n\tcoroutine.resume(co)\n\tcoroutine.close(co)\nend";
    for (name, before) in [("rec-died", died), ("rec-closed", closed)] {
        let (own, after, summary) = booked_cycles(name, &source(before), 0);
        let (calls_clean, calls_after) = (clean["calls"].as_u64().unwrap() as f64, after["calls"].as_u64().unwrap() as f64);
        assert!(calls_after > 0.7 * calls_clean, "{name}: calls read {calls_after}, {calls_clean} without\n{summary}");
        let (f, main) = (own.get("f").copied().unwrap_or(0.0), own.get("main").copied().unwrap_or(0.0));
        assert!(f > 0.0 && main.abs() < 0.06 * f, "{name}: f {f}, its caller {main}\n{summary}");
    }
}

/// Runs `script` under a profile of `kinds` whose hardware counters are the mock's, and returns the script's output with
/// the `attribution` the profile's counters section reports
fn run_with_mock_counters(kinds: Kinds, script: &str) -> (Vec<String>, serde_json::Value) {
    let _lock = lock();
    profiler::hw::TEST_MODE.with(|mode| mode.set(Some("mock")));
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(kinds, temp_dir("same")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    let output = run(&lua, &engine, "same", script);
    profiler::collect(&lua);
    let run_result = profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() };
    let _ = session.finish(run_result).unwrap();
    profiler::hw::TEST_MODE.with(|mode| mode.set(None));
    let profile_json = std::fs::read_to_string(config.out.join("profile.json")).unwrap();
    let profile: serde_json::Value = serde_json::from_str(&profile_json).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (output, profile["counters"]["attribution"].clone())
}

/// The interpreter's copy that samples calls (which runs while the counters' hook is set) computes what the plain copy
/// does: the same script, the same output, with and without the hook
#[test]
fn the_interpreter_that_samples_calls_computes_what_the_plain_one_does() {
    let script = "local out = {}
local function emit(...)
	local parts = table.pack(...)
	for i = 1, parts.n do
		parts[i] = tostring(parts[i])
	end
	table.insert(out, table.concat(parts, \" \"))
end
local work = {}
function work.fib(n)
	if n < 2 then
		return n
	end
	return work.fib(n - 1) + work.fib(n - 2)
end
function work.sum(...)
	local total = 0
	for i = 1, select(\"#\", ...) do
		total += select(i, ...)
	end
	return total, select(\"#\", ...)
end
function work.counter()
	local count = 0
	return function(step)
		count += step or 1
		return count
	end
end
local Vector = {}
Vector.__index = Vector
Vector.__add = function(a, b)
	return setmetatable({x = a.x + b.x, y = a.y + b.y}, Vector)
end
Vector.__eq = function(a, b)
	return a.x == b.x and a.y == b.y
end
Vector.__lt = function(a, b)
	return a.x < b.x
end
Vector.__len = function(a)
	return math.floor(math.sqrt(a.x * a.x + a.y * a.y))
end
Vector.__concat = function(a, b)
	return tostring(a) .. tostring(b)
end
Vector.__tostring = function(a)
	return \"(\" .. a.x .. \",\" .. a.y .. \")\"
end
Vector.__call = function(a, k)
	return a.x * k
end
function Vector.length2(self)
	return self.x * self.x + self.y * self.y
end
local proxy = setmetatable({}, {__index = function(_, key)
	return key * 2
end, __newindex = function(t, key, value)
	rawset(t, key, value + 1)
end})
emit(\"fib\", work.fib(18))
emit(\"sum\", work.sum(1, 2, 3, 4.5, 6))
local tick = work.counter()
tick()
tick(5)
emit(\"counter\", tick(10))
local a, b = setmetatable({x = 3, y = 4}, Vector), setmetatable({x = 1, y = -2}, Vector)
local c = a + b
emit(\"vector\", c.x, c.y, a == b, b < a, #a, a .. b, a(7), a:length2())
proxy[5] = 10
emit(\"proxy\", proxy[21], proxy[5])
local words = {}
for word in string.gmatch(\"the quick brown fox jumps over the lazy dog\", \"%a+\") do
	table.insert(words, word)
end
table.sort(words, function(x, y)
	if #x ~= #y then
		return #x < #y
	end
	return x < y
end)
emit(\"sorted\", table.concat(words, \",\"))
emit(\"gsub\", (string.gsub(\"a1b22c333\", \"%d+\", function(d)
	return \"<\" .. #d .. \">\"
end)))
local gen = coroutine.wrap(function(limit)
	for i = 1, limit do
		local ok, err = pcall(function()
			if i % 3 == 0 then
				error(\"tick \" .. i, 0)
			end
			coroutine.yield(i * i)
		end)
		if not ok then
			coroutine.yield(err)
		end
	end
	return \"done\"
end)
local yielded = {}
for _ = 1, 7 do
	table.insert(yielded, tostring(gen(6)))
end
emit(\"coroutine\", table.concat(yielded, \",\"))
local ok, message = xpcall(function()
	local t = nil
	return t.field
end, function(err)
	return \"handled: \" .. tostring(string.find(err, \"index\") ~= nil)
end)
emit(\"xpcall\", ok, message)
local acc = 0
for i = 10, 1, -3 do
	acc += i
end
for i = 0.5, 2.5, 0.5 do
	acc += i
end
emit(\"loops\", acc)
local deep = 0
local function recurse(n)
	if n == 0 then
		return 0
	end
	deep += 1
	return 1 + recurse(n - 1)
end
emit(\"recurse\", recurse(150), deep)
for _, line in out do
	print(line)
end
";
    let plain = {
        let _lock = lock();
        let (lua, engine) = start();
        run(&lua, &engine, "same", script)
    };
    assert!(plain.len() == 11 && plain.iter().any(|line| line == "coroutine 1,4,tick 3,16,25,tick 6,done"), "{plain:#?}");
    // (the counters alone, then beside host time, then beside the work clock's stepping: the interpreter's wrapper picks its copy differently in each)
    let profile_kinds = [Kinds::HW, Kinds::HW.with(Kinds::TIME), Kinds::HW.with(Kinds::WORK)];
    for kinds in profile_kinds {
        let (profiled, attribution) = run_with_mock_counters(kinds, script);
        assert_eq!(plain, profiled, "the same output with {kinds:?}");
        assert!(attribution["calls"].as_u64().unwrap() > 10, "the hook read calls in the profiled run: {attribution}");
    }
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

/// A clip-shaped run whose steady phase casts `casts` times: each cast clones a template of three into the Workspace, moves a
/// part by a write, pivots the copy, bulk-moves both parts, emits from a new emitter and switches it off, lists the copy's
/// descendants and destroys it. Returns the summary and profile.json.
fn engine_ops_profile(name: &str, casts: u32) -> (String, serde_json::Value) {
    let (lua, engine) = start();
    let config = Config { print: false, ..Config::new(Kinds::WORK, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    // (the template, made before the phase: its three instances are startup's)
    run(
        &lua,
        &engine,
        "setup",
        "local model = Instance.new(\"Model\")\nmodel.Name = \"Template\"\nfor i = 1, 2 do\n\tInstance.new(\"Part\", model)\nend\nmodel.Parent = game:GetService(\"ReplicatedStorage\")\n",
    );
    profiler::phase("steady");
    let cast = "local Workspace = game:GetService(\"Workspace\")
local copy = game:GetService(\"ReplicatedStorage\").Template:Clone()
copy.Parent = Workspace
local parts = copy:GetChildren()
parts[1].CFrame = CFrame.new(0, 10, 0)
parts[1].Name = \"Moved\"
parts[1].Name = \"Moved\"
copy:PivotTo(CFrame.new(5, 0, 0))
Workspace:BulkMoveTo(parts, { CFrame.new(1, 0, 0), CFrame.new(2, 0, 0) })
local emitter = Instance.new(\"ParticleEmitter\")
emitter.Parent = parts[2]
emitter:Emit(4)
emitter.Enabled = false
local _ = copy:GetDescendants()
copy:Destroy()
";
    for _ in 0..casts {
        run(&lua, &engine, "cast", cast);
    }
    profiler::phase("after");
    run(&lua, &engine, "idle", "local idle = Instance.new(\"Folder\")\n");
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 1, seconds: 1.0 / 60.0, fps: 60.0, ..Default::default() }).unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// The engine's side of a phase: what its scripts asked the engine to do (cost::EngineOps), carried by each mark like the work
/// clock, so the steady phase's counts are its own; the summary reports them under the reported phase, and a diff compares them
#[test]
fn a_phase_counts_what_its_scripts_asked_the_engine_to_do() {
    let _lock = lock();
    let (summary, json) = engine_ops_profile("engine-ops", 1);
    let ops = &json["scope"]["engine"];
    let count = |name: &str| ops[name].as_u64().unwrap_or_else(|| panic!("no {name} in {ops}"));
    // the copy and its two parts; the emitter; the copy, its parts and the emitter destroyed; the copy and its parts entering the
    // Workspace, then the emitter
    assert_eq!((count("cloned"), count("created"), count("destroyed"), count("entered")), (3, 1, 4, 4), "{ops}");
    // one part moved by a write, one emitter switched, one pivot, both parts bulk-moved, one emit, one descendant query
    assert_eq!((count("geometry"), count("toggles"), count("pivots"), count("bulk_moved"), count("emits"), count("queries")), (1, 1, 1, 2, 1, 1), "{ops}");
    // the CFrame, both names and Enabled at least (the second name changed nothing)
    assert!(count("writes") >= 4 && count("changed") >= 3 && count("changed") < count("writes"), "{ops}");
    // startup's template is not the phase's, and after's folder is not either; the marks carry the counts so far
    let marks = json["marks"].as_array().unwrap();
    let mark = |phase: &str| marks.iter().find(|mark| mark["phase"] == phase).unwrap_or_else(|| panic!("{marks:#?}"));
    assert!(mark("steady")["engine"]["created"].as_u64().unwrap() >= 3, "{marks:#?}");
    assert_eq!(json["end"]["engine"]["created"].as_u64().unwrap() - mark("after")["engine"]["created"].as_u64().unwrap(), 1);
    let line = summary.lines().find(|line| line.starts_with("engine operations, phase steady:")).unwrap_or_else(|| panic!("{summary}"));
    for part in ["3 instances cloned", "1 made with Instance.new", "4 destroyed", "4 entered the Workspace", "1 moved or resized a part in the Workspace", "1 switched an effect", "1 pivot, 2 parts bulk-moved", "1 emit", "1 descendant query"] {
        assert!(line.contains(part), "{part:?} in {line}");
    }
    // a diff: twice the casts, twice the engine's counts in the phase both report
    let (_, twice) = engine_ops_profile("engine-ops-twice", 2);
    let (text, diff) = profiler::diff::compare(&json, &twice, 10);
    let figure = |name: &str| diff["figures"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no {name} in\n{text}")).clone();
    let made = figure("engine: instances made, phase steady");
    assert_eq!((made["before"].as_f64().unwrap(), made["after"].as_f64().unwrap()), (4.0, 8.0), "{text}");
    let moved = figure("engine: parts moved, phase steady");
    assert_eq!((moved["before"].as_f64().unwrap(), moved["after"].as_f64().unwrap()), (3.0, 6.0), "{text}");
}

/// `profile diff` says when the two profiles' hardware counters reached functions in other ways (1 ms samples, readings
/// around sampled calls drawn one of each run, or each drawn on its own with the threads' switches read): the functions'
/// counter figures then compare only roughly
#[test]
fn a_diff_says_when_the_counters_reached_functions_in_other_ways() {
    let profile = |attribution: serde_json::Value| serde_json::json!({"run": {"status": "ok", "frames": 1}, "counters": {"names": ["cycles"], "run": {"counts": {"cycles": 1000}}, "attribution": attribution}});
    let (sampled, now) = (profile(serde_json::Value::Null), profile(serde_json::json!({"method": "calls", "version": 2})));
    let (text, json) = profiler::diff::compare(&sampled, &now, 10);
    assert!(text.contains("reached functions in other ways") && text.contains("1 ms samples") && text.contains("each drawn on its own"), "{text}");
    assert!(json["notes"].as_array().unwrap().iter().any(|note| note.as_str().unwrap().contains("other ways")), "{json}");
    let (same, _) = profiler::diff::compare(&now, &now, 10);
    assert!(!same.contains("other ways"), "{same}");
    let (older, _) = profiler::diff::compare(&profile(serde_json::json!({"method": "calls"})), &now, 10);
    assert!(older.contains("one drawn of each run"), "{older}");
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

/// A Luau source written as a raw string that starts on the line after its opening quotes
fn luau(text: &str) -> &str {
    text.strip_prefix('\n').unwrap_or(text)
}

/// A work profile of `source` (chunk `=name`) run once, then `frames` frames of the scheduler in phase steady: the summary and
/// profile.json. `options` set the engine (a device); `frozen` freezes the clock (REE_FROZEN_CLOCK).
fn stepped_profile(name: &str, source: &str, frames: u32, options: Options, frozen: bool) -> (String, serde_json::Value) {
    stepped_profile_kinds(name, source, frames, options, frozen, Kinds::WORK)
}

fn stepped_profile_kinds(name: &str, source: &str, frames: u32, options: Options, frozen: bool, kinds: Kinds) -> (String, serde_json::Value) {
    // (a playing client: one Heartbeat a frame; edit mode beats at 240 Hz)
    let engine = Engine::new(Options { mode: "play".into(), ..options }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    engine.state.borrow_mut().sched.frozen_clock = frozen;
    let config = Config { print: false, ..Config::new(kinds, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    run(&lua, &engine, name, source);
    profiler::phase("steady");
    for _ in 0..frames {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let summary = session
        .finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: u64::from(frames), seconds: f64::from(frames) / 60.0, fps: 60.0, ..Default::default() })
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// profile.json's function named `name` (a table's function `M.name` is named `name`)
fn function_named<'a>(json: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    json["functions"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap_or_else(|| panic!("no function {name}"))
}

/// A function's figure in phase steady (`self_work_ms`, `cold_work_ms`, `saving_ms`, `calls` ...)
fn steady(function: &serde_json::Value, figure: &str) -> f64 {
    function["phases"]["steady"][figure].as_f64().unwrap_or(0.0)
}

/// Two identical walkers over the same records, run in both orders a frame (the audit: 24.27 and 0.66 ms by call order). The first
/// pays the frame's cold charges for the records and its cold column says so; neither one's saving counts them, as cutting either
/// would move them to the other, so both read the same saving whichever runs first, and so do their hot figures.
#[test]
fn the_cold_split_and_a_cuts_saving_do_not_depend_on_which_walker_runs_first() {
    let _lock = lock();
    // Two walkers over the same 400 records; `$first` runs before `$second` in each frame
    let walkers_source = luau(r#"
        local Records = {}
        for I = 1, 400 do
            Records[I] = { Value = I, Weight = I % 7 }
        end
        local Walkers = {}
        function Walkers.A()
            local Sum = 0
            for _, Record in Records do
                Sum += Record.Value * Record.Weight
            end
            return Sum
        end
        function Walkers.B()
            local Sum = 0
            for _, Record in Records do
                Sum += Record.Value * Record.Weight
            end
            return Sum
        end
        game:GetService("RunService").Heartbeat:Connect(function()
            Walkers.$first()
            Walkers.$second()
        end)
    "#);
    let walkers = |first: &str, second: &str| walkers_source.replace("$first", first).replace("$second", second);
    let mut savings = Vec::new();
    for (first, second) in [("A", "B"), ("B", "A")] {
        let (summary, json) = stepped_profile("walkers", &walkers(first, second), 12, Options::default(), false);
        let (a, b) = (function_named(&json, first), function_named(&json, second));
        assert_eq!(steady(a, "calls"), steady(b, "calls"));
        // the first pays the records' cold charges (400 tables a frame), the second next to none
        assert!(steady(a, "cold_work_ms") > 50.0 * steady(b, "cold_work_ms").max(1e-6), "{summary}");
        let hot = |f: &serde_json::Value| steady(f, "self_work_ms") - steady(f, "cold_work_ms");
        assert!((hot(a) - hot(b)).abs() <= 1e-9 * hot(a), "hot work {} and {}", hot(a), hot(b));
        assert!((steady(a, "saving_ms") - steady(b, "saving_ms")).abs() <= 1e-9 * steady(a, "saving_ms"), "{summary}");
        assert!(steady(a, "saving_ms") < 0.2 * steady(a, "self_work_ms"), "the records' charges are not the first walker's to save");
        // the records: touched by both every frame, so shared in every frame they were charged
        let records = json["cold"]["objects"].as_array().unwrap().iter().find(|o| o["name"] == "made at walkers:3").unwrap_or_else(|| panic!("{}", json["cold"]));
        assert_eq!(records["frames"], records["shared_frames"], "{records}");
        savings.push((steady(function_named(&json, "A"), "saving_ms"), steady(function_named(&json, "B"), "saving_ms")));
    }
    assert!((savings[0].0 - savings[1].0).abs() <= 1e-9 * savings[0].0 && (savings[0].1 - savings[1].1).abs() <= 1e-9 * savings[0].1, "{savings:?}");
}

/// One operation's charges for one object add up: a write charges its instance's first touch and then the dirtying, under the same
/// object (the review: the second replaced the first, so a lone writer of 50 parts read 0.1009 ms of exclusive cold charges where they
/// were 0.1257). A function alone in touching its parts owns every cold charge it pays: its saving is its self work.
#[test]
fn a_lone_writers_cold_charges_are_all_its_own() {
    let _lock = lock();
    let source = luau(r#"
        local Parts = {}
        for I = 1, 50 do
        	Parts[I] = Instance.new("Part")
        end
        local M = {}
        function M.Write()
        	for _, P in Parts do
        		P.Transparency = math.random()
        	end
        end
        game:GetService("RunService").Heartbeat:Connect(M.Write)
    "#);
    let (summary, json) = stepped_profile("writer", source, 11, Options::default(), false);
    let write = function_named(&json, "Write");
    let (cold, exclusive) = (write["self"]["cold_work_ms"].as_f64().unwrap(), write["self"]["exclusive_cold_ms"].as_f64().unwrap());
    assert!(cold > 0.0 && (exclusive - cold).abs() < 1e-9, "cold {cold} ms, exclusive {exclusive} ms\n{summary}");
    let (work, saving) = (write["self"]["work_ms"].as_f64().unwrap(), write["self"]["saving_ms"].as_f64().unwrap());
    assert!((saving - work).abs() < 1e-9, "saving {saving} ms of {work} ms");
    let random = function_named(&json, "math.random");
    let (cold, exclusive) = (random["self"]["cold_work_ms"].as_f64().unwrap(), random["self"]["exclusive_cold_ms"].as_f64().unwrap());
    assert!(cold > 0.0 && (cold - exclusive).abs() < 1e-9, "repeated calls keep the C function's charge on its own row: {random}");
    // (each part's object row adds its first touch and its dirtying: 50 parts a frame, from the frame after the one that made them)
    let parts = json["cold"]["objects"].as_array().unwrap().iter().find(|o| o["kind"] == "instance" && o["name"] == "Part").unwrap_or_else(|| panic!("{}", json["cold"]));
    let charges = parts["frames"].as_u64().unwrap();
    assert!(charges == 500 && parts["shared_frames"].as_u64() == Some(0), "{parts}");
    // (parts made together in one loop: the plain first touch, and a part's Transparency dirtying on top)
    let each = parts["cost_ms"].as_f64().unwrap() * 1e-3 / charges as f64;
    let expected = crate::cost::COLD_TOUCH + crate::cost::cold_write_extra(crate::cost::ColdKind::Part, "Transparency");
    assert!((each - expected).abs() < 1e-12, "{each} s a charge against {expected}");
}

/// A callee's cold code is its own, and only the code it ran: a 6,000-instruction function that returns at its second instruction
/// pays for a few instructions a frame, where the caller's CALL line used to pay for all 6,000 (5.1 us a frame); run to its end it
/// pays for them.
#[test]
fn a_callee_that_returns_early_pays_for_the_code_it_ran() {
    let _lock = lock();
    // Big runs 6,000 increments unless Early; Caller calls it every frame
    let early_source = r#"
        local M = {}
        function M.Big(Early)
            if Early then
                return 0
            end
            local X = 0
            $body
            return X
        end
        function M.Caller()
            return M.Big($early)
        end
        game:GetService("RunService").Heartbeat:Connect(M.Caller)
    "#
    .replace("$body", &"X += 1\n".repeat(6000));
    let source = |early: bool| early_source.replace("$early", &early.to_string());
    let frames = 10u32;
    let cold_a_frame = |early: bool| {
        let (summary, json) = stepped_profile("early", &source(early), frames, Options::default(), false);
        let (big, caller) = (function_named(&json, "Big"), function_named(&json, "Caller"));
        assert_eq!(steady(big, "calls"), f64::from(frames), "{summary}");
        // Code pays at dispatch even in the frame that compiled it; object first touches are separate.
        let cold_frames = f64::from(frames);
        (steady(big, "cold_work_ms") / cold_frames * 1e3, steady(caller, "cold_work_ms") / cold_frames * 1e3)
    };
    let ((early_us, early_caller_us), (full_us, full_caller_us)) = (cold_a_frame(true), cold_a_frame(false));
    let code_us = 6000.0 * crate::cost::LUAU_COLD_CODE * 1e-3;
    assert!(full_us >= 0.95 * code_us, "run to its end it pays for its code: {full_us} us a frame against {code_us}");
    assert!(early_us < 0.02 * code_us, "returning early it pays for a few instructions: {early_us} us a frame");
    // (the caller pays for its own cold touches, its function and the table M among them, and nothing of the callee's code, either way)
    assert!(early_caller_us < 0.1 * code_us && (full_caller_us - early_caller_us).abs() < 1e-6, "caller {early_caller_us} / {full_caller_us} us");
}

/// Recursion adds nothing twice: fib(15)'s recursive line and its fib -> fib edge hold no more than fib's own total (the audit: a line
/// total of 16.24 ms and an edge of 14.94 on a run of 3.01)
#[test]
fn recursion_adds_a_lines_and_an_edges_work_once() {
    let _lock = lock();
    let source = luau(r#"
        local M = {}
        function M.fib(N)
        	if N < 2 then
        		return N
        	end
        	return M.fib(N - 1) + M.fib(N - 2)
        end
        game:GetService("RunService").Heartbeat:Once(function()
        	result = M.fib(15)
        end)
    "#);
    let (summary, json) = stepped_profile("fib", source, 2, Options::default(), false);
    let fib = function_named(&json, "fib");
    let (id, total) = (fib["id"].clone(), fib["total"]["work_ms"].as_f64().unwrap());
    assert_eq!(fib["calls"].as_f64(), Some(1973.0), "fib(15) makes 1,973 calls");
    let lines: Vec<f64> = json["lines"].as_array().unwrap().iter().filter(|l| l["function"] == id).map(|l| l["total_work_ms"].as_f64().unwrap()).collect();
    let line_max = lines.iter().copied().fold(0.0, f64::max);
    assert!(line_max <= total * (1.0 + 1e-9) && line_max >= 0.9 * total, "the recursive line {line_max} ms of fib's {total}\n{summary}");
    let edge = json["edges"].as_array().unwrap().iter().find(|e| e["caller"] == id && e["callee"] == id).expect("fib -> fib");
    assert!(edge["work_ms"].as_f64().unwrap() <= total * (1.0 + 1e-9), "{edge}");
    assert!(total <= json["work"]["total_ms"].as_f64().unwrap());
}

/// A parked thread's resumption is charged as a client resumes it and booked to the line that waited: 500 threads waiting three
/// times each are 1,500 resumptions at 0.35 us (the audit: 30,000 resumptions charged nothing)
#[test]
fn resumptions_are_charged_to_the_line_that_waited() {
    let _lock = lock();
    let source = luau(r#"
        for I = 1, 500 do
        	task.spawn(function()
        		for J = 1, 3 do
        			task.wait()
        		end
        	end)
        end
    "#);
    let (summary, json) = stepped_profile("waits", source, 6, Options::default(), false);
    let rows = json["resumptions"].as_array().unwrap_or_else(|| panic!("{summary}"));
    let at_wait: Vec<&serde_json::Value> = rows.iter().filter(|row| row["line"] == 4).collect();
    let count: f64 = at_wait.iter().map(|row| row["count"].as_f64().unwrap()).sum();
    let cost: f64 = at_wait.iter().map(|row| row["cost_ms"].as_f64().unwrap()).sum();
    assert_eq!(count, 1500.0, "{rows:?}");
    assert!((cost - 1500.0 * crate::cost::RESUMED_WAIT * 1e3).abs() < 1e-9, "{cost} ms");
    assert!(summary.contains("resumptions of parked threads"), "{summary}");
}

/// An event's dispatch is booked to each handler, with the line that connected it: 300 Heartbeat handlers and 200 handlers of a
/// property's changed signal (the audit: 67 % of the work outside any instruction, no connect site named)
#[test]
fn handlers_are_booked_to_the_site_that_connected_them() {
    let _lock = lock();
    let source = luau(r#"
        local RunService = game:GetService("RunService")
        local Part = Instance.new("Part")
        for I = 1, 300 do
        	RunService.Heartbeat:Connect(function() end)
        end
        for I = 1, 200 do
        	Part:GetPropertyChangedSignal("Transparency"):Connect(function() end)
        end
        RunService.Heartbeat:Connect(function()
        	Part.Transparency = math.random()
        end)
    "#);
    let frames = 5u32;
    let (summary, json) = stepped_profile("handlers", source, frames, Options::default(), false);
    let sites = json["handlers"].as_array().unwrap_or_else(|| panic!("{summary}"));
    let heartbeat = sites.iter().find(|site| site["line"] == 4).unwrap_or_else(|| panic!("{sites:?}"));
    assert_eq!((heartbeat["connections"].as_u64(), heartbeat["runs"].as_u64()), (Some(300), Some(300 * u64::from(frames))), "{heartbeat}");
    assert!(heartbeat["signal"].as_str().unwrap().contains("Heartbeat") && heartbeat["dispatch_ms"].as_f64().unwrap() > 0.0, "{heartbeat}");
    let changed = sites.iter().find(|site| site["line"] == 7).unwrap_or_else(|| panic!("{sites:?}"));
    assert_eq!(changed["connections"].as_u64(), Some(200), "{changed}");
    assert!(changed["runs"].as_u64().unwrap() >= 200 * u64::from(frames - 1) && changed["dispatch_ms"].as_f64().unwrap() > 0.0, "{changed}");
    let (total, outside) = (json["work"]["total_ms"].as_f64().unwrap(), json["work"]["outside_instructions_ms"].as_f64().unwrap());
    assert!(outside < 0.01 * total, "{outside} of {total} ms outside any instruction");
    assert!(section(&summary, "handlers by the site that connected them").contains("(handlers:4)"), "{summary}");
}

/// A phone's profile reads in the units its clock ran on (the desktop prices times cost::PHONE_SCRIPTS), names its device and
/// gives the factor's band (the audit: 0.78 ms on a phone against 1.42 on a desktop, no device named)
#[test]
fn a_phone_profile_reads_in_its_clocks_units() {
    let _lock = lock();
    let source = luau(r#"
        local Parts = {}
        for I = 1, 50 do
        	Parts[I] = Instance.new("Part")
        end
        game:GetService("RunService").Heartbeat:Connect(function()
        	for _, P in Parts do
        		P.Transparency = math.random()
        	end
        end)
    "#);
    let (desktop_summary, desktop) = stepped_profile("desktop", source, 6, Options::default(), false);
    let (phone_summary, phone) = stepped_profile("phone", source, 6, Options { device: "phone".into(), ..Options::default() }, false);
    let work = |json: &serde_json::Value| json["work"]["total_ms"].as_f64().unwrap();
    assert!((work(&phone) / work(&desktop) - crate::cost::PHONE_SCRIPTS).abs() < 1e-6, "{} against {}", work(&phone), work(&desktop));
    assert_eq!(phone["work"]["units"], serde_json::json!({"device": "phone", "scale": crate::cost::PHONE_SCRIPTS}));
    assert_eq!(phone["fidelity"]["device"]["band"], serde_json::json!([crate::cost::PHONE_SCRIPTS_BAND.0, crate::cost::PHONE_SCRIPTS_BAND.1]));
    // (the band folds the factor's spread in: against a phone the figures read PHONE_SCRIPTS over its band's ends again)
    let (band, clock) = (&phone["fidelity"]["band"], &phone["fidelity"]["clock_band"]);
    let low = clock[0].as_f64().unwrap() * crate::cost::PHONE_SCRIPTS / crate::cost::PHONE_SCRIPTS_BAND.1;
    let high = clock[1].as_f64().unwrap() * crate::cost::PHONE_SCRIPTS / crate::cost::PHONE_SCRIPTS_BAND.0;
    assert!((band[0].as_f64().unwrap() - low).abs() < 2e-3 && (band[1].as_f64().unwrap() - high).abs() < 2e-3, "{}", phone["fidelity"]);
    assert_eq!(desktop["fidelity"]["band"], desktop["fidelity"]["clock_band"]);
    assert_eq!(phone["fidelity"]["weighted"], "the game's code in phase steady");
    assert!(phone_summary.contains("at phone prices") && desktop_summary.contains("at desktop prices"), "{phone_summary}");
}

#[test]
fn cache_and_builtin_prices_use_the_same_device_units_as_the_clock() {
    let _lock = lock();
    let source = luau(r#"
        getfenv()
        local M = {}
        function M.Frame()
        local T = setmetatable({}, {__index = {Value = 1}})
        local S = string.rep("a", 128)
        for I = 2, 64 do
        local X = T.Value
        local Part = string.sub(S, I, #S)
        end
        end
        game:GetService("RunService").Heartbeat:Connect(M.Frame)
    "#);
    let kinds = Kinds::parse("work,caches").unwrap();
    let (_, desktop) = stepped_profile_kinds("device-sites", source, 4, Options::default(), false, kinds);
    let (_, phone) = stepped_profile_kinds("device-sites", source, 4, Options { device: "phone".into(), ..Options::default() }, false, kinds);
    assert_eq!(phone["luau_caches"]["prices"]["scale"], crate::cost::PHONE_SCRIPTS);
    for field in ["charged_work_us", "extra_work_us"] {
        let sum = |json: &serde_json::Value| json["luau_caches"]["sites"].as_array().unwrap().iter().map(|site| site[field].as_f64().unwrap()).sum::<f64>();
        let (desktop, phone) = (sum(&desktop), sum(&phone));
        assert!(desktop > 0.0);
        assert!((phone - desktop * crate::cost::PHONE_SCRIPTS).abs() < 1e-3, "{field}: desktop {desktop}, phone {phone}");
    }
    let sub = |json: &serde_json::Value| json["builtins"].as_array().unwrap().iter().find(|row| row["name"] == "string.sub").unwrap().clone();
    for field in ["decline_charged_us", "fast_equivalent_us"] {
        let (desktop, phone) = (sub(&desktop)[field].as_f64().unwrap(), sub(&phone)[field].as_f64().unwrap());
        assert!(desktop > 0.0);
        assert!((phone - desktop * crate::cost::PHONE_SCRIPTS).abs() < 1e-3, "{field}: desktop {desktop}, phone {phone}");
    }
}

/// profile.json's work.prices give each count's price on the work clock, the ones the issues price memory and calls by:
/// what the clock charges one of each (cost::luau of a counter set holding one), the collector's apart from the allocation's
#[test]
fn work_prices_give_each_counts_price() {
    let _lock = lock();
    let (_, json) = stepped_profile("prices", "result = {}\n", 1, Options::default(), false);
    let prices = &json["work"]["prices"];
    let close = |key: &str, ns: f64| {
        let value = prices[key].as_f64().unwrap_or_else(|| panic!("no {key} in {prices}"));
        assert!((value - ns).abs() < 1e-9, "{key} {value} against {ns}");
    };
    close("object_ns", crate::cost::LUAU_OBJECT);
    close("stored_byte_ns", crate::cost::LUAU_STORED);
    close("rehashed_key_ns", crate::cost::LUAU_REHASHED);
    close("c_call_ns", crate::cost::LUAU_CCALL);
    close("sort_compare_ns", crate::cost::LUAU_SORT_COMPARE);
    close("sort_call_ns", crate::cost::LUAU_SORT_CALL);
    close("allocated_byte_ns", crate::cost::LUAU_ALLOCATED_BYTE);
    close("hash_rehashed_key_ns", crate::cost::LUAU_HASH_REHASHED);
    close("large_block_ns", crate::cost::LUAU_LARGE_BLOCK);
    close("library_object_ns", crate::cost::LUAU_LIBRARY_OBJECT);
    close("collector_object_ns", crate::cost::LUAU_COLLECTOR_OBJECT);
    assert_eq!(json["work"]["units"], serde_json::json!({"device": "desktop", "scale": 1.0}));
}

/// A loop that spends an os.clock budget does what the budget buys on the work clock, the same in every run, and the profile names
/// it with its work an iteration (the clock's advance between two reads by one call, its callees' included); on a frozen clock it does
/// what the clock's busy-loop allowance lets it instead (the audit: a 0.2 ms budget loop did 66.80 ms of work normally and 321.26
/// frozen), so the game's wrappers profile on the work clock. A function that reads the clock once a call, however often it is called,
/// is no budget loop (the review: a clock accessor, a rate limiter and a cache timestamp were listed)
#[test]
fn a_budget_loop_is_named_and_does_what_its_budget_buys() {
    let _lock = lock();
    // (line 7 is the loop's check; Work is a callee the iteration's work includes; Stamp reads the clock once a call, three calls a
    // frame, at line 17; a thread that waits each iteration reads it once an iteration at line 21, and a generator resumed five times a
    // frame once a resumption at line 27: across a yield the clock's advance is everything else that ran, no iteration's work)
    let source = luau(r#"
        Units = 0
        local M = {}
        function M.Work() local T = {} for I = 1, 20 do T[I] = I end return T end
        game:GetService("RunService").Heartbeat:Connect(function()
        	local Start = os.clock()
        	local Done = 0
        	while os.clock() - Start < 0.0002 and Done < 2000 do
        		M.Work()
        		Done += 1
        	end
        	Units += Done
        	for _ = 1, 3 do
        		M.Stamp()
        	end
        end)
        function M.Stamp()
        	return os.clock()
        end
        task.spawn(function()
        	while true do
        		local Now = os.clock()
        		task.wait()
        	end
        end)
        local Next = coroutine.wrap(function()
        	while true do
        		local Now = os.clock()
        		coroutine.yield()
        	end
        end)
        game:GetService("RunService").Heartbeat:Connect(function()
        	for _ = 1, 5 do
        		Next()
        	end
        end)
    "#);
    let frames = 6u32;
    let (summary, normal) = stepped_profile("budget", source, frames, Options::default(), false);
    let (_, frozen) = stepped_profile("budget-frozen", source, frames, Options::default(), true);
    let per_frame = |json: &serde_json::Value| json["work"]["total_ms"].as_f64().unwrap() / f64::from(frames);
    assert!(per_frame(&normal) >= 0.2 && per_frame(&normal) < 0.25, "the budget buys 0.2 ms a frame: {}", per_frame(&normal));
    assert!((per_frame(&frozen) / per_frame(&normal) - 1.0).abs() > 0.5, "frozen, the loop does other work: {} against {}", per_frame(&frozen), per_frame(&normal));
    let line = |number: u64| normal["clock_reads"].as_array().unwrap().iter().find(|row| row["line"] == number).cloned().unwrap_or_else(|| panic!("{}", normal["clock_reads"]));
    let check = line(7);
    assert_eq!(check["budget_loop"], true, "{check}");
    assert_eq!(check["runs"].as_u64(), Some(u64::from(frames)), "{check}");
    assert!(check["reads"].as_u64().unwrap() > 10 * check["runs"].as_u64().unwrap(), "{check}");
    // (an iteration: the check and M.Work's 20 stores, about a twentieth of a microsecond to a few: never the 0 that dividing the
    // loop's own self work gave)
    let iteration = check["iteration_us"].as_f64().unwrap();
    let per_read = per_frame(&normal) * 1e3 / (check["reads"].as_f64().unwrap() / f64::from(frames));
    assert!(iteration > 0.0 && (iteration / per_read - 1.0).abs() < 0.2, "{iteration} us an iteration, {per_read} us a read");
    let stamp = line(17);
    assert_eq!(stamp["budget_loop"], false, "{stamp}");
    assert_eq!((stamp["reads"].as_u64(), stamp["runs"].as_u64()), (Some(3 * u64::from(frames)), Some(3 * u64::from(frames))), "{stamp}");
    // (each read after a yield starts a run: none of them is an iteration of a budget loop)
    for (number, least) in [(21, u64::from(frames)), (27, 5 * u64::from(frames))] {
        let waits = line(number);
        assert_eq!(waits["budget_loop"], false, "{waits}");
        assert_eq!(waits["iterations"].as_u64(), Some(0), "{waits}");
        assert!(waits["reads"].as_u64().unwrap() >= least && waits["runs"] == waits["reads"], "{waits}");
    }
    let table = section(&summary, "os.clock budget loops");
    assert!(table.contains("us an iteration") && table.contains("budget:7"), "{summary}");
    assert!(!table.contains("budget:17") && !table.contains("budget:21") && !table.contains("budget:27"), "{summary}");
}

/// A loop that yields each iteration pays for its body's code every frame (the review: a 300-instruction body that waited each frame
/// was charged in its first frame only, as each later frame's stretch began after the wait and ended before it): task.wait, an
/// event's Wait, and a wait in a helper the loop calls, which returns into the loop in a frame the loop never entered
#[test]
fn a_loop_that_yields_pays_for_its_body_every_frame() {
    let _lock = lock();
    let frames = 30u32;
    let code_us = 300.0 * crate::cost::LUAU_COLD_CODE * 1e-3;
    for (wait, helper) in [
        ("task.wait()", ""),
        ("game:GetService(\"RunService\").Heartbeat:Wait()", ""),
        ("Sleep()", "local function Sleep()\n\ttask.wait()\nend\n"),
    ] {
        let cold_a_frame = |lines: usize| {
            let source = r#"
                $helper
                local function Loop()
                    local X = 0
                    while true do
                        $body
                        $wait
                    end
                end
                task.spawn(Loop)
            "#
            .replace("$helper", helper)
            .replace("$body", &"X += 1\n".repeat(lines))
            .replace("$wait", wait);
            let (summary, json) = stepped_profile("yields", &source, frames, Options::default(), false);
            // (the first frame runs the function the script made just before it: an object made in the frame is not cold)
            (steady(function_named(&json, "Loop"), "cold_work_ms") / f64::from(frames - 1) * 1e3, summary)
        };
        // (the body's 300 instructions each frame, beyond what the loop's wait and its few other instructions are charged cold)
        let ((with_us, summary), (without_us, _)) = (cold_a_frame(300), cold_a_frame(0));
        let body_us = with_us - without_us;
        assert!(body_us >= 0.95 * code_us && body_us <= 1.05 * code_us, "{wait}: {with_us} us a frame with the body, {without_us} without, against {code_us}\n{summary}");
    }
}

/// A function that errors pays for the code it ran (the review: a pcall'd function that raised was never charged, its stretch never
/// closed): raised by error() or by an instruction, caught by pcall, or ending the thread it ran on
#[test]
fn a_function_that_errors_pays_for_the_code_it_ran() {
    let _lock = lock();
    let frames = 10u32;
    let code_us = 300.0 * crate::cost::LUAU_COLD_CODE * 1e-3;
    for (raise, call) in [
        ("error(\"no\")", "pcall(M.Fails)"),
        ("local T = nil\n\tT.Field = X", "pcall(M.Fails)"),
        ("error(\"no\")", "task.spawn(M.Fails)"),
    ] {
        let cold_a_frame = |lines: usize| {
            let source = r#"
                local M = {}
                function M.Fails()
                    local X = 0
                    $body
                    $raise
                end
                game:GetService("RunService").Heartbeat:Connect(function()
                    $call
                end)
            "#
            .replace("$body", &"X += 1\n".repeat(lines))
            .replace("$raise", raise)
            .replace("$call", call);
            let (summary, json) = stepped_profile("errors", &source, frames, Options::default(), false);
            assert_eq!(steady(function_named(&json, "Fails"), "calls"), f64::from(frames), "{summary}");
            // Code pays at dispatch in every frame, including code loaded during that frame.
            (steady(function_named(&json, "Fails"), "cold_work_ms") / f64::from(frames) * 1e3, summary)
        };
        // (the 300 instructions it ran before the error, beyond what raising it is charged cold)
        let ((with_us, summary), (without_us, _)) = (cold_a_frame(300), cold_a_frame(0));
        let body_us = with_us - without_us;
        assert!(body_us >= 0.95 * code_us && body_us <= 1.05 * code_us, "{raise} / {call}: {with_us} us a frame with the body, {without_us} without, against {code_us}\n{summary}");
    }
}

/// Large buffer work the clock prices at nothing is printed as unpriced with its bytes (the review: a 4 MB copy read as a small one,
/// with nothing to say so): copies and fills of over 64 KB a call and new buffers past Luau's pages; small buffers print nothing
#[test]
fn large_buffer_work_is_printed_as_unpriced() {
    let _lock = lock();
    let frames = 3u32;
    let source = |size: u64| format!("local A, B = buffer.create({size}), buffer.create({size})\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n\tbuffer.copy(A, 0, B)\n\tbuffer.fill(A, 0, 7)\n\tlocal _ = buffer.create({size})\nend)\n");
    let size = 256 * 1024u64;
    let (summary, json) = stepped_profile("buffers-large", &source(size), frames, Options::default(), false);
    let unpriced = &json["work"]["unpriced"];
    assert_eq!(unpriced["buffer_big_bytes"].as_u64(), Some(2 * size * u64::from(frames)), "{unpriced}");
    let fresh = unpriced["buffer_fresh_bytes"].as_u64().unwrap();
    assert!(fresh >= size * u64::from(frames) && fresh <= size * u64::from(frames + 2), "{unpriced}");
    let line = summary.lines().find(|line| line.starts_with("unpriced: ")).unwrap_or_else(|| panic!("{summary}"));
    assert!(line.contains("1.50 MB moved by buffer copies, fills and string writes"), "{line}");
    let (small, json) = stepped_profile("buffers-small", &source(1024), frames, Options::default(), false);
    assert_eq!((json["work"]["unpriced"]["buffer_big_bytes"].as_u64(), json["work"]["unpriced"]["buffer_fresh_bytes"].as_u64()), (Some(0), Some(0)));
    assert!(!small.contains("unpriced: "), "{small}");
}

/// A frame event's first handler that runs pays no FRAME_HANDLER, whatever its list holds ahead of it (the review: a cancelled thread
/// waiting on the event ahead of a lone handler made the handler pay as a second one)
#[test]
fn a_frame_events_first_handler_that_runs_pays_no_frame_handler() {
    let _lock = lock();
    let source = luau(r#"
        local RunService = game:GetService("RunService")
        RunService.Heartbeat:Connect(function() end)
        local Waiter = task.spawn(function()
        	RunService.Heartbeat:Wait()
        end)
        task.cancel(Waiter)
    "#);
    let (summary, json) = stepped_profile("first-handler", source, 3, Options::default(), false);
    let sites = json["handlers"].as_array().unwrap_or_else(|| panic!("{summary}"));
    let site = sites.iter().find(|site| site["line"] == 2).unwrap_or_else(|| panic!("{sites:?}"));
    assert_eq!(site["runs"].as_u64(), Some(3), "{site}");
    assert_eq!(site["dispatch_ms"].as_f64(), Some(0.0), "{site}");
}

/// A declined allocating builtin pays for the same result as its fast path. Its fallback's
/// instructions and C call add work; moving the allocation into C must not make it cheaper.
#[test]
fn declined_substrings_pay_the_same_for_their_result_strings() {
    let _lock = lock();
    let measure = |decline: bool| {
        let lua = Lua::new();
        lua.enable_jit(false);
        lua.globals().set_safeenv(true);
        if decline {
            lua.load("getfenv()").exec().unwrap();
        }
        let function: LuaFunction = lua.load("return function(S) local Total = 0 for I = 2, 257 do local Part = string.sub(S, I, #S) Total += #Part end return Total end").eval().unwrap();
        let source = lua.create_string("a".repeat(512)).unwrap();
        // Unique suffix lengths make every result new; no collector step can force a fast call to decline.
        lua.gc_stop();
        let counters = unsafe { crate::cost::lua_workcounters(main_state(&lua)) };
        let before = unsafe { &*counters };
        let (objects, library, bytes, work, calls) = (before.objects, before.hostops[crate::cost::LIBOBJECTS_SLOT], before.hostops[crate::cost::ALLOCBYTES_SLOT], crate::cost::luau(before), before.ccalls);
        assert_eq!(function.call::<i32>(source).unwrap(), (511 + 256) * 256 / 2);
        let after = unsafe { &*counters };
        (after.objects - objects, after.hostops[crate::cost::LIBOBJECTS_SLOT] - library, after.hostops[crate::cost::ALLOCBYTES_SLOT] - bytes, crate::cost::luau(after) - work, after.ccalls - calls)
    };
    let (fast_objects, fast_library, fast_bytes, fast_work, fast_calls) = measure(false);
    let (slow_objects, slow_library, slow_bytes, slow_work, slow_calls) = measure(true);
    assert_eq!((fast_objects, fast_library, fast_calls), (256, 0, 0));
    assert_eq!((slow_objects, slow_library, slow_calls), (0, 256, 256));
    assert_eq!(fast_bytes, slow_bytes);
    assert_eq!(crate::cost::LUAU_OBJECT, crate::cost::LUAU_LIBRARY_OBJECT);
    assert!(slow_work > fast_work, "declined {slow_work} s, fast {fast_work} s");
}

/// An advised explicit argument adds code words to each compiled call site of the builtin: one for `string.sub`'s end, two
/// for `string.byte`'s index
#[test]
fn substring_argument_fixes_add_the_expected_warm_instructions_and_code_words() {
    let _lock = lock();
    // The prototype of `Read`, which calls the builtin ten times, with the call as the source gives it
    let measure_read = |call: &str| {
        let lua = Lua::new();
        lua.enable_jit(false);
        let state = main_state(&lua);
        assert!(unsafe { vm::start(state, true, None, None) });
        let is_byte = call.starts_with("string.byte");
        // (string.sub returns a string, whose length is summed; string.byte returns a number)
        let summed = if is_byte { call.to_string() } else { format!("#{call}") };
        let source = format!("local M = {{}}\nfunction M.Read(s, index)\n local sum = 0\n for i = 1, 10 do sum += {summed} end\n return sum\nend\nreturn M.Read(\"abcdef\", 2)\n");
        let result = lua.load(&source).set_name("=substring-shape").set_compiler(engine_lua::modules::compiler()).eval::<f64>().unwrap();
        let expected_sum = if is_byte { 970.0 } else { 50.0 };
        assert_eq!(result, expected_sum);
        unsafe { vm::stop(state) };
        let results = unsafe { vm::results(state) };
        unsafe { vm::free(state) };
        results.functions.iter().find(|function| function.name == "Read").unwrap().clone()
    };
    let rewrites = [
        ("string.sub(s, 2)", "string.sub(s, 2, #s)", 1),
        ("string.sub(s, index)", "string.sub(s, index, #s)", 1),
        ("string.byte(s)", "string.byte(s, 1)", 2),
    ];
    for (declined_call, fixed_call, code_words_per_site) in rewrites {
        let declined = measure_read(declined_call);
        let fixed = measure_read(fixed_call);
        // (the compiled call sites that ran: each is a FASTCALL instruction)
        let mut compiled_sites = 0;
        for (code, count) in declined.code.iter().zip(&declined.counts) {
            if *count > 0 && vm::opcode_name((*code & 255) as u8).starts_with("FASTCALL") {
                compiled_sites += 1;
            }
        }
        let added_words = fixed.code.len() - declined.code.len();
        assert_eq!(added_words, code_words_per_site * compiled_sites, "{declined_call}: {} to {} words across {compiled_sites} compiled sites", declined.code.len(), fixed.code.len());
    }
}

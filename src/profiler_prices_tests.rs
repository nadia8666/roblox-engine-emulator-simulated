//! The calibration of profiler/prices.rs: what each inline cache outcome the work clock does not charge costs over its hit on
//! ree's interpreter, as a ratio to the base opcode's hit measured in the same round (a rehash, a looked-up import and a declined
//! builtin call are the clock's own charges, read off a profile, so none is calibrated). Ignored by default (a session takes a
//! minute); its output is the table prices.rs holds:
//!
//!   REE_CALIBRATE_SESSIONS=<file> cargo test --release --lib -- --ignored --nocapture --exact profiler_prices_tests::calibrate_prices
//!
//! A session runs [`calibrate_prices_once`] in several processes of its own (the test binary again), each one's figures a ratio's
//! median over its rounds; with REE_CALIBRATE_SESSIONS it adds the session's figures to that file, and prints the table of every
//! session the file holds: the median of all their processes' medians, with the least and greatest as the spread. What moves from
//! one process to the next (code placement, the clock's frequency) and from one session to the next (the machine's other work at
//! another time) is the uncertainty, which one session's processes understate: run sessions at different times. Every loop runs in a module's environment (safe, as the engine makes
//! every script's), is compiled with the engine's compiler settings, and takes its values as arguments. Each round times every
//! case and its counterpart one after the other (each the fastest of a few runs), rounds rotate the order and start from a
//! collected heap. A store that inserts a key runs on tables made before the timed loop (a pool, made again for each run), so no
//! allocation is timed.

use std::time::Instant;

use mlua::prelude::*;

use crate::{
    Engine, Options,
    lua::{self as engine_lua},
    profiler::vm,
};

/// Processes, the rounds each one runs, the runs of a loop a round takes the fastest of, and the time each run aims at
const PROCESSES: usize = 7;
const ROUNDS: usize = 21;
const REPEATS: usize = 3;
const TARGET_SECONDS: f64 = 0.002;

/// The loops: eight accesses a turn where one is cheap, the pair loops alternating two values each turn
const LOOPS: &str = r#"
local L = {}
function L.empty(n, t) local s; for i = 1, n do s = t end; return s end
function L.read(n, t) local s; for i = 1, n do s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x end; return s end
function L.absent(n, t) local s; for i = 1, n do s = t.y; s = t.y; s = t.y; s = t.y; s = t.y; s = t.y; s = t.y; s = t.y end; return s end
function L.readpair(n, a, b) local s; for i = 1, n do local t = if i % 2 == 0 then a else b; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x; s = t.x end; return s end
function L.store(n, t) for i = 1, n do t.x = i; t.x = i; t.x = i; t.x = i; t.x = i; t.x = i; t.x = i; t.x = i end end
function L.pool(n, proto) local pool = table.create(n); for i = 1, n do pool[i] = table.clone(proto) end; return pool end
function L.poolstore(n, pool) for i = 1, n do pool[i].x = i end end
function L.global(n) local s; for i = 1, n do s = gx; s = gx; s = gx; s = gx; s = gx; s = gx; s = gx; s = gx end; return s end
function L.globalmeta(n) local s; for i = 1, n do s = gy; s = gy; s = gy; s = gy; s = gy; s = gy; s = gy; s = gy end; return s end
function L.call(n, o) for i = 1, n do o:m(); o:m(); o:m(); o:m() end end
function L.callpair(n, a, b) for i = 1, n do local o = if i % 2 == 0 then a else b; o:m(); o:m(); o:m(); o:m() end end
-- (written somewhere in the chunk, gx and gy are read with GETGLOBAL, not resolved at load as imports)
function L.written() gx = gx; gy = gy end
return L
"#;

/// The chunk of the absent global: an environment of its own with no metatable
const BARE: &str = r#"
local L = {}
function L.global(n) local s; for i = 1, n do s = gx; s = gx; s = gx; s = gx; s = gx; s = gx; s = gx; s = gx end; return s end
function L.absent(n) local s; for i = 1, n do s = gz; s = gz; s = gz; s = gz; s = gz; s = gz; s = gz; s = gz end; return s end
function L.written() gx = gx; gz = gz end
return L
"#;

/// The node a table holds `key` in, by the order `next` visits its nodes (the hash part's order)
const SLOT_OF: &str = r#"
return function(t, wanted) local index = 0; for key in next, t do index += 1; if key == wanted then return index end end; return -1 end
"#;

/// A timed call: the loop, its arguments after the turn count, and the accesses a turn; with `pool`, its one argument is made
/// before each run by the pool function from a prototype table (L.pool: a table for each turn)
#[derive(Clone)]
struct Loop {
    function: LuaFunction,
    args: Vec<LuaValue>,
    per_turn: f64,
    turns: i64,
    pool: Option<(LuaFunction, LuaValue)>,
}

impl Loop {
    fn new(function: &LuaFunction, args: Vec<LuaValue>, per_turn: f64) -> Loop {
        Loop::made(function, args, per_turn, None)
    }

    fn pooled(function: &LuaFunction, pool: &LuaFunction, proto: LuaValue) -> Loop {
        Loop::made(function, Vec::new(), 1.0, Some((pool.clone(), proto)))
    }

    fn made(function: &LuaFunction, args: Vec<LuaValue>, per_turn: f64, pool: Option<(LuaFunction, LuaValue)>) -> Loop {
        let mut timed = Loop { function: function.clone(), args, per_turn, turns: 2_000, pool };
        // (turns for TARGET_SECONDS a run, from a short run's time)
        let mut seconds = timed.run();
        while seconds < TARGET_SECONDS / 8.0 && timed.turns < 1 << 30 {
            timed.turns *= 4;
            seconds = timed.run();
        }
        timed.turns = ((timed.turns as f64 * TARGET_SECONDS / seconds.max(1e-9)) as i64).max(1_000);
        timed
    }

    fn run(&self) -> f64 {
        let made: Vec<LuaValue> = match &self.pool {
            Some((pool, proto)) => vec![pool.call::<LuaValue>((self.turns, proto.clone())).expect("a pool")],
            None => self.args.clone(),
        };
        let args: LuaMultiValue = std::iter::once(LuaValue::Integer(self.turns)).chain(made).collect();
        let started = Instant::now();
        self.function.call::<()>(args).expect("a timed loop");
        started.elapsed().as_secs_f64()
    }

    /// Nanoseconds an access: the fastest of REPEATS runs
    fn ns(&self) -> f64 {
        (0..REPEATS).map(|_| self.run()).fold(f64::INFINITY, f64::min) * 1e9 / (self.turns as f64 * self.per_turn)
    }
}

/// A case: its loop and the loop it is measured against; `ratio` divides the difference by a base (a hit's own cost)
struct Case {
    name: String,
    slow: Loop,
    fast: Loop,
    /// The base: a hit's own cost (its loop less the empty loop), which case's figures divide by
    base: &'static str,
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    let at = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[at]
}

/// The engine's environment for a module, a calibration chunk compiled in it, and the Lua it runs in
fn module_env(lua: &Lua, engine: &std::rc::Rc<Engine>) -> LuaTable {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Calibration")
    };
    engine_lua::modules::environment(lua, engine, script).expect("environment")
}

fn compile(lua: &Lua, env: &LuaTable, source: &str) -> LuaTable {
    lua.load(source).set_name("=ree.calibrate").set_compiler(engine_lua::modules::compiler()).set_environment(env.clone()).call::<LuaTable>(()).expect("calibration chunk")
}

/// One process's calibration: each base's cost and each case's ratio, the medians of its rounds, as `CALIBRATION <json>` lines
#[test]
#[ignore = "one process of calibrate_prices, which runs it"]
fn calibrate_prices_once() {
    let _lock = crate::luau_profile::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let env = module_env(&lua, &engine);
    let eval = |source: &str| -> LuaValue { lua.load(source).set_name("=ree.calibrate").set_environment(env.clone()).eval::<LuaValue>().expect("a value") };
    let loops = compile(&lua, &env, LOOPS);
    let get = |name: &str| loops.get::<LuaFunction>(name).unwrap();
    let slot_of: LuaFunction = lua.load(SLOT_OF).eval().unwrap();

    // two tables of the same two keys whose `wanted` key sits in different nodes (a key sharing its node stored first)
    let pair_for = |wanted: &str, value: &str| -> (LuaValue, LuaValue) {
        for candidate in 1..=256 {
            let key = format!("k{candidate}");
            let first = eval(&format!("local t = {{}}; t.{wanted} = {value}; t.{key} = 1; return t"));
            let second = eval(&format!("local t = {{}}; t.{key} = 1; t.{wanted} = {value}; return t"));
            let (a, b): (i64, i64) = (slot_of.call((first.clone(), wanted)).unwrap(), slot_of.call((second.clone(), wanted)).unwrap());
            if a > 0 && b > 0 && a != b {
                return (first, second);
            }
        }
        panic!("no two tables with {wanted} at different nodes");
    };

    // values: a table holding x (and the pair), chains of __index tables, an __index function, tables to clone
    let plain = eval("local t = {}; t.x = 1; t.k = 1; return t");
    let (first, second) = pair_for("x", "1");
    let chain = |depth: usize| eval(&format!("local base = {{x = 1}}; local top = base; for i = 2, {depth} do top = setmetatable({{}}, {{__index = top}}) end; return setmetatable({{}}, {{__index = top}})"));
    let by_function = eval("return setmetatable({}, {__index = function(t, key) return 1 end})");
    let with_x = eval("return {x = 0, a = 1, b = 1, c = 1}");
    let roomy = eval("return {a = 1, b = 1, c = 1}");
    // globals: gx in the module's environment, gy in the shared globals its metatable leads to
    env.raw_set("gx", 1).unwrap();
    let shared: LuaTable = env.metatable().expect("a module environment's metatable").raw_get::<LuaTable>("__index").expect("the shared globals");
    shared.raw_set("gy", 1).unwrap();
    // a bare environment (safe, no metatable): a global absent from it
    let bare = lua.create_table().unwrap();
    bare.raw_set("gx", 1).unwrap();
    unsafe { lua.exec_raw::<()>(&bare, |state| mlua::ffi::lua_setsafeenv(state, -1, 1)).unwrap() };
    let bare_loops = compile(&lua, &bare, BARE);
    // objects: a method in the class (the first __index table), two classes with the method at different nodes, chains of
    // classes, an __index function
    let method = eval("return function(self) end");
    let (class_a, class_b) = pair_for("m", "function(self) end");
    let object = |class: &LuaValue| -> LuaValue {
        let make: LuaFunction = lua.load("return function(class) return setmetatable({}, {__index = class}) end").eval().unwrap();
        make.call(class.clone()).unwrap()
    };
    let (object_a, object_b) = (object(&class_a), object(&class_b));
    let method_chain = |depth: usize| -> LuaValue {
        let make: LuaFunction = lua
            .load("return function(depth, m) local base = {m = m}; local top = base; for i = 2, depth do top = setmetatable({}, {__index = top}) end; return setmetatable({}, {__index = top}) end")
            .eval()
            .unwrap();
        make.call((depth as i64, method.clone())).unwrap()
    };
    let method_function: LuaValue = {
        let make: LuaFunction = lua.load("return function(m) return setmetatable({}, {__index = function(t, key) return m end}) end").eval().unwrap();
        make.call(method.clone()).unwrap()
    };

    let empty = get("empty");
    let read = get("read");
    let call = get("call");
    let lp = |function: &LuaFunction, args: Vec<LuaValue>, per_turn: f64| Loop::new(function, args, per_turn);
    // the bases: a slot read's own cost, a store's, a global read's, a method call's (its hit is the case's fast loop)
    let bases: Vec<(&'static str, Loop, Loop)> = vec![
        ("key read", lp(&read, vec![plain.clone()], 8.0), lp(&empty, vec![plain.clone()], 8.0)),
        ("key store", lp(&get("store"), vec![plain.clone()], 8.0), lp(&empty, vec![plain.clone()], 8.0)),
        ("global read", lp(&get("global"), vec![], 8.0), lp(&empty, vec![plain.clone()], 8.0)),
    ];
    let mut cases: Vec<Case> = vec![
        Case { name: "table key repredicted".into(), slow: lp(&get("readpair"), vec![first.clone(), second.clone()], 8.0), fast: lp(&get("readpair"), vec![first.clone(), first.clone()], 8.0), base: "key read" },
        Case { name: "table key absent".into(), slow: lp(&get("absent"), vec![plain.clone()], 8.0), fast: lp(&read, vec![plain.clone()], 8.0), base: "key read" },
        Case { name: "table key inserted".into(), slow: Loop::pooled(&get("poolstore"), &get("pool"), roomy.clone()), fast: Loop::pooled(&get("poolstore"), &get("pool"), with_x.clone()), base: "key store" },
        Case { name: "table key metatable_function".into(), slow: lp(&read, vec![by_function.clone()], 8.0), fast: lp(&read, vec![plain.clone()], 8.0), base: "key read" },
        Case { name: "global slot metatable 1".into(), slow: lp(&get("globalmeta"), vec![], 8.0), fast: lp(&get("global"), vec![], 8.0), base: "global read" },
        Case { name: "global slot absent".into(), slow: lp(&bare_loops.get::<LuaFunction>("absent").unwrap(), vec![], 8.0), fast: lp(&bare_loops.get::<LuaFunction>("global").unwrap(), vec![], 8.0), base: "global read" },
        Case { name: "method slot repredicted".into(), slow: lp(&get("callpair"), vec![object_a.clone(), object_b.clone()], 4.0), fast: lp(&get("callpair"), vec![object_a.clone(), object_a.clone()], 4.0), base: "key read" },
        Case { name: "method slot metatable_function".into(), slow: lp(&call, vec![method_function.clone()], 4.0), fast: lp(&call, vec![object_a.clone()], 4.0), base: "key read" },
    ];
    for depth in 1..=vm::CACHE_DEPTHS {
        cases.push(Case { name: format!("table key metatable {depth}"), slow: lp(&read, vec![chain(depth)], 8.0), fast: lp(&read, vec![plain.clone()], 8.0), base: "key read" });
        if depth >= 2 {
            cases.push(Case { name: format!("method slot metatable {depth}"), slow: lp(&call, vec![method_chain(depth)], 4.0), fast: lp(&call, vec![object_a.clone()], 4.0), base: "key read" });
        }
    }

    // the rounds: every base and case timed in each, in a rotated order
    let mut base_ns: Vec<Vec<f64>> = vec![Vec::new(); bases.len()];
    let mut case_ratios: Vec<Vec<f64>> = vec![Vec::new(); cases.len()];
    for round in 0..ROUNDS {
        lua.gc_collect().unwrap();
        let mut hits = vec![0.0; bases.len()];
        for (index, (_, with, without)) in bases.iter().enumerate() {
            let (a, b) = if round % 2 == 0 { (with.ns(), without.ns()) } else { let b = without.ns(); (with.ns(), b) };
            hits[index] = a - b;
            base_ns[index].push(a - b);
        }
        let start = round % cases.len().max(1);
        for offset in 0..cases.len() {
            let index = (start + offset) % cases.len();
            let case = &cases[index];
            let (slow, fast) = if round % 2 == 0 { (case.slow.ns(), case.fast.ns()) } else { let fast = case.fast.ns(); (case.slow.ns(), fast) };
            let base = hits[bases.iter().position(|(name, _, _)| *name == case.base).unwrap()];
            case_ratios[index].push((slow - fast) / base);
        }
    }
    let median = |values: &[f64]| -> f64 {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        quantile(&sorted, 0.5)
    };
    for ((name, _, _), values) in bases.iter().zip(&base_ns) {
        println!("CALIBRATION {}", serde_json::json!({"base": name, "ns": median(values)}));
    }
    for (case, values) in cases.iter().zip(&case_ratios) {
        println!("CALIBRATION {}", serde_json::json!({"case": case.name, "ratio": median(values)}));
    }
}

/// The calibration: [`calibrate_prices_once`] in PROCESSES processes of the test binary, one after another; each case's median
/// across them, and their least and greatest, printed as prices.rs's table
#[test]
#[ignore = "a calibration: run by hand on a quiet machine, its output is profiler/prices.rs's table"]
fn calibrate_prices() {
    let exe = std::env::current_exe().expect("the test binary");
    let mut bases: std::collections::BTreeMap<String, Vec<f64>> = std::collections::BTreeMap::new();
    let mut cases: Vec<(String, Vec<f64>)> = Vec::new();
    for process in 0..PROCESSES {
        let output = std::process::Command::new(&exe)
            .args(["profiler_prices_tests::calibrate_prices_once", "--exact", "--ignored", "--nocapture", "--test-threads", "1"])
            .output()
            .expect("a calibration process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "process {process}: {stdout}{}", String::from_utf8_lossy(&output.stderr));
        // (the test harness prints the test's name before its first line, on the same line)
        for line in stdout.lines().filter_map(|line| line.find("CALIBRATION ").map(|at| &line[at + "CALIBRATION ".len()..])) {
            let figure: serde_json::Value = serde_json::from_str(line).unwrap();
            if let Some(name) = figure["base"].as_str() {
                bases.entry(name.to_string()).or_default().push(figure["ns"].as_f64().unwrap());
            } else if let Some(name) = figure["case"].as_str() {
                let ratio = figure["ratio"].as_f64().unwrap();
                match cases.iter_mut().find(|(known, _)| known == name) {
                    Some((_, ratios)) => ratios.push(ratio),
                    None => cases.push((name.to_string(), vec![ratio])),
                }
            }
        }
    }
    // (the sessions so far, this one with them: each a line of its processes' figures)
    let mut sessions = 1;
    if let Some(file) = std::env::var_os("REE_CALIBRATE_SESSIONS") {
        let session = serde_json::json!({
            "when": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "bases": bases, "cases": cases.iter().map(|(name, ratios)| (name.clone(), serde_json::json!(ratios))).collect::<serde_json::Map<_, _>>(),
        });
        let earlier = std::fs::read_to_string(&file).unwrap_or_default();
        std::fs::write(&file, format!("{earlier}{session}\n")).expect("the sessions file");
        let all: Vec<serde_json::Value> = format!("{earlier}{session}\n").lines().filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str(line).unwrap()).collect();
        sessions = all.len();
        bases.clear();
        cases.clear();
        for session in &all {
            for (name, values) in session["bases"].as_object().unwrap() {
                bases.entry(name.clone()).or_default().extend(values.as_array().unwrap().iter().filter_map(serde_json::Value::as_f64));
            }
            for (name, values) in session["cases"].as_object().unwrap() {
                let values = values.as_array().unwrap().iter().filter_map(serde_json::Value::as_f64);
                match cases.iter_mut().find(|(known, _)| known == name) {
                    Some((_, ratios)) => ratios.extend(values),
                    None => cases.push((name.clone(), values.collect())),
                }
            }
        }
    }
    let spread = |values: &[f64]| -> (f64, f64, f64) {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        (quantile(&sorted, 0.5), sorted[0], sorted[sorted.len() - 1])
    };
    let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
    println!("// calibrate_prices: {sessions} sessions of {PROCESSES} processes of {ROUNDS} rounds, each the fastest of {REPEATS} runs of about {:.0} ms; {cpu}", TARGET_SECONDS * 1e3);
    let bases_text: Vec<String> = bases.iter().map(|(name, values)| { let (median, low, high) = spread(values); format!("{name} {median:.3} ns ({low:.3}-{high:.3})") }).collect();
    println!("// bases: {}", bases_text.join("; "));
    for (name, ratios) in &cases {
        let (median, low, high) = spread(ratios);
        println!("    (\"{name}\", r({median:.3}, {low:.3}, {high:.3})),");
    }
}

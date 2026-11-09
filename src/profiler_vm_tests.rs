//! What the VM records for a profile, checked on harnesses with known answers: the inline caches' outcomes as the interpreter
//! ran them (the opcodes the loader rewrites for a member name's key included), the builtin calls' declines and why, a C
//! function's row at its call sites, and a Luau function a C function calls counted per invocation.

use std::rc::Rc;

use mlua::prelude::*;
use serde_json::Value as Json;

use crate::{
    Engine, Options,
    lua::{self as engine_lua, globals::CallOutcome},
    profiler::{self, Config, Kinds, vm},
    scheduler,
};

fn lock() -> std::sync::MutexGuard<'static, ()> {
    crate::luau_profile::TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("ree-vm-profile-{name}-{}-{stamp}", std::process::id()))
}

/// Runs `source` as a script (chunk `=name`) on a scheduler thread
fn run(lua: &Lua, engine: &Rc<Engine>, name: &str, source: &str) {
    run_with(lua, engine, name, source, |_| {});
}

/// [`run`] with `prepare` given the script's environment first (a test's own globals)
fn run_with(lua: &Lua, engine: &Rc<Engine>, name: &str, source: &str, prepare: impl FnOnce(&LuaTable)) {
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(lua, engine, script).expect("environment");
    prepare(&env);
    let function = lua.load(source).set_name(format!("={name}")).set_environment(env).into_function().expect("compile");
    let call = engine_lua::globals::call(lua, engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
}

/// A profile of `source` (chunk `=name`, run once in a playing client: one Heartbeat a frame) and `frames` frames after it: the
/// summary and profile.json
fn profile(name: &str, kinds: Kinds, frames: u32, source: &str) -> (String, Json) {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    // (interpreted, as `ree run` and `ree record` run modules)
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(kinds, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run(&lua, &engine, name, source);
    for _ in 0..frames {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let summary = session
        .finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: frames as u64, seconds: frames as f64 / 60.0, fps: 60.0, ..Default::default() })
        .unwrap();
    let json: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// The function named `name` in `module` (a table's function by its last part)
fn function<'a>(json: &'a Json, module: &str, name: &str) -> &'a Json {
    json["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["module"] == module && f["name"].as_str().is_some_and(|n| n == name || n.ends_with(&format!(".{name}"))))
        .unwrap_or_else(|| panic!("no {name} in {module}: {:#}", json["functions"]))
}

/// The C function named `name`
fn c_function<'a>(json: &'a Json, name: &str) -> &'a Json {
    json["functions"].as_array().unwrap().iter().find(|f| f["kind"] == "C" && f["name"] == name).unwrap_or_else(|| panic!("no C function {name}: {:#}", json["functions"]))
}

/// The cache sites of a function's line, summed: (opcodes as run, outcomes by name)
fn sites_at(json: &Json, function: &Json, line: i64) -> (Vec<String>, std::collections::BTreeMap<String, u64>) {
    let mut ops = Vec::new();
    let mut outcomes = std::collections::BTreeMap::new();
    for site in json["luau_caches"]["sites"].as_array().unwrap().iter().filter(|site| site["function"] == function["id"] && site["line"] == line) {
        ops.push(site["op"].as_str().unwrap().to_string());
        for (outcome, count) in site["outcomes"].as_object().unwrap() {
            let count = count.as_u64().unwrap();
            if count > 0 {
                *outcomes.entry(outcome.clone()).or_default() += count;
            }
        }
    }
    (ops, outcomes)
}

fn counts(pairs: &[(&str, u64)]) -> std::collections::BTreeMap<String, u64> {
    pairs.iter().map(|(name, count)| (name.to_string(), *count)).collect()
}

/// Luau 0.746's loader rewrites a GETTABLEKS, SETTABLEKS or NAMECALL whose key is a member name (`Name`, `GetAttribute`) into
/// GETUDATAKS, SETUDATAKS or NAMECALLUDATA, which ree's VM, with no direct access registered, rewrites back to the table access at
/// its first run: the profile records the access as it ran, so a member-named key and a plain one read the same opcode and the
/// same outcomes, and the per-kind totals (the marks' and the summary's) hold both
#[test]
fn a_member_named_key_and_a_plain_key_give_the_same_opcode_and_totals() {
    let _lock = lock();
    // (tables passed in: the compiler folds a read of a table a constructor built in the same function)
    let source = "local M = {}\nfunction M.readName(t)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\ts = t.Name\n\tend\n\treturn s\nend\nfunction M.readFoo(t)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\ts = t.foo\n\tend\n\treturn s\nend\nfunction M.writeName(t)\n\tfor i = 1, 1000 do\n\t\tt.Name = i\n\tend\nend\nfunction M.writeFoo(t)\n\tfor i = 1, 1000 do\n\t\tt.foo = i\n\tend\nend\nM.readName({Name = 1})\nM.readFoo({foo = 1})\nM.writeName({Name = 1})\nM.writeFoo({foo = 1})\nlocal part = Instance.new(\"Part\")\nlocal function attributes()\n\tfor i = 1, 1000 do\n\t\tpart:GetAttribute(\"x\")\n\tend\nend\nattributes()\n";
    let (summary, json) = profile("member-keys", Kinds::CACHES, 1, source);
    let (read_name, read_foo) = (function(&json, "member-keys", "readName"), function(&json, "member-keys", "readFoo"));
    let (name_ops, name_outcomes) = sites_at(&json, read_name, 5);
    let (foo_ops, foo_outcomes) = sites_at(&json, read_foo, 12);
    assert_eq!(name_ops, vec!["GETTABLEKS"], "the member-named key ran as GETTABLEKS: {summary}");
    assert_eq!(name_ops, foo_ops);
    assert_eq!(name_outcomes, foo_outcomes, "the same outcomes for Name and foo");
    assert_eq!(name_outcomes.values().sum::<u64>(), 1000);
    let (write_name, write_foo) = (function(&json, "member-keys", "writeName"), function(&json, "member-keys", "writeFoo"));
    let (name_ops, name_outcomes) = sites_at(&json, write_name, 18);
    let (foo_ops, foo_outcomes) = sites_at(&json, write_foo, 23);
    assert_eq!((name_ops, &name_outcomes), (vec!["SETTABLEKS".to_string()], &foo_outcomes), "{foo_ops:?}");
    // (the per-kind totals hold the member-named sites: the run's table key reads and writes are 4,000)
    let by_kind = json["luau_caches"]["by_kind"].as_array().unwrap();
    let table_keys = by_kind.iter().find(|row| row["kind"] == "table key slot").unwrap();
    let runs: u64 = table_keys["outcomes"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum();
    assert!(runs >= 4000, "{table_keys}");
    let end_totals = json["end"]["caches"]["table key slot"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).sum::<u64>();
    assert!(end_totals >= 4000, "the marks' totals: {}", json["end"]["caches"]);
    // inst:GetAttribute() ×1000: a NAMECALL as run, on a value that is not a table, served by Instance's __namecall; and its C row
    let (ops, outcomes) = sites_at(&json, function(&json, "member-keys", "attributes"), 33);
    assert_eq!((ops, outcomes), (vec!["NAMECALL".to_string()], counts(&[("not_table", 1000)])));
    let site = json["luau_caches"]["sites"].as_array().unwrap().iter().find(|site| site["op"] == "NAMECALL" && site["line"] == 33).unwrap();
    assert_eq!(site["handlers"]["Instance"], 1000, "{site}");
    assert_eq!(site["key"], "GetAttribute");
    let methods = by_kind.iter().find(|row| row["kind"] == "method slot").unwrap();
    assert!(methods["outcomes"]["not_table"].as_u64().unwrap() >= 1000, "{methods}");
    let get_attribute = c_function(&json, "Instance:GetAttribute");
    assert_eq!(get_attribute["calls"], 1000);
    let call_site = &get_attribute["call_sites"][0];
    assert_eq!((call_site["caller"].clone(), call_site["line"].clone()), (function(&json, "member-keys", "attributes")["id"].clone(), Json::from(33)), "{get_attribute}");
}

/// What a table key lookup did, as the interpreter did it: a read of an absent key is absent (a full lookup, nothing patched); a
/// key stored into a table the compiler sized for it is inserted, into one that must grow, rehashed; a key found in the
/// metatable's __index table walks it (depth 1, 2); a function __index is its own outcome
#[test]
fn table_key_outcomes_say_what_the_interpreter_did() {
    let _lock = lock();
    let source = "local M = {}\nfunction M.absent(t)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\ts = t.missing\n\tend\n\treturn s\nend\nfunction M.sized()\n\tfor i = 1, 1000 do\n\t\tlocal t = {}\n\t\tt.a = 1\n\t\tt.b = 2\n\tend\nend\nfunction M.fill(t)\n\tt.a = 1\n\tt.b = 2\n\tt.c = 3\nend\nfunction M.grow()\n\tfor i = 1, 100 do\n\t\tM.fill({})\n\tend\nend\nfunction M.inherited(t)\n\tlocal s\n\tfor i = 1, 100 do\n\t\ts = t.x\n\tend\n\treturn s\nend\nM.absent({foo = 1})\nM.sized()\nM.grow()\nlocal base = {x = 1}\nM.inherited(setmetatable({}, {__index = base}))\nM.inherited(setmetatable({}, {__index = setmetatable({}, {__index = base})}))\nM.inherited(setmetatable({}, {__index = function() return 1 end}))\n";
    let (summary, json) = profile("outcomes", Kinds::CACHES, 0, source);
    let (_, absent) = sites_at(&json, function(&json, "outcomes", "absent"), 5);
    assert_eq!(absent, counts(&[("absent", 1000)]), "{summary}");
    // (`local t = {}` and the stores after it: the compiler sizes the table for two keys)
    let sized = function(&json, "outcomes", "sized");
    assert_eq!(sites_at(&json, sized, 12).1, counts(&[("inserted", 1000)]));
    assert_eq!(sites_at(&json, sized, 13).1, counts(&[("inserted", 1000)]));
    // (a table passed in grows a key at a time: no room for a, b, c each time)
    let fill = function(&json, "outcomes", "fill");
    for line in [17, 18, 19] {
        assert_eq!(sites_at(&json, fill, line).1, counts(&[("rehashed", 100)]), "line {line}");
    }
    let inherited = function(&json, "outcomes", "inherited");
    assert_eq!(sites_at(&json, inherited, 29).1, counts(&[("metatable_function", 100), ("metatable_table", 200)]));
    let site = json["luau_caches"]["sites"].as_array().unwrap().iter().find(|site| site["function"] == inherited["id"] && site["line"] == 29).unwrap();
    assert_eq!((site["depth"]["1"].as_u64(), site["depth"]["2"].as_u64()), (Some(100), Some(100)), "{site}");
    // the legacy counts are exact: an absent read or an insert is not a "miss"
    let absent_site = json["luau_caches"]["sites"].as_array().unwrap().iter().find(|site| site["line"] == 5 && site["op"] == "GETTABLEKS").unwrap();
    assert_eq!((absent_site["hits"].as_u64(), absent_site["misses"].as_u64(), absent_site["slow"].as_u64()), (Some(0), Some(0), Some(0)), "{absent_site}");
    // prices: committed (the same in every profile), each outcome's over a hit in the work clock's nanoseconds, and a site's in
    // its microseconds
    let prices = &json["luau_caches"]["prices"];
    assert_eq!(prices["clock"], "work", "{prices}");
    let key = &prices["kinds"]["table key slot"]["outcomes"];
    assert_eq!((key["rehashed"]["charged"].as_bool(), key["rehashed"]["ns"].is_null()), (Some(true), true), "a rehash is the work clock's charge: {key}");
    assert_eq!((key["inserted"]["charged"].as_bool(), key["inserted"]["ns"].is_f64()), (Some(false), true), "an insert with room is not: {key}");
    assert_eq!(key["not_table"]["charged"].as_bool(), Some(true), "an Instance's member is the engine's price, charged");
    // (100 rehashes: what the clock charged the store, none of it priced again)
    let fill_site = json["luau_caches"]["sites"].as_array().unwrap().iter().find(|site| site["function"] == fill["id"] && site["line"] == 17).unwrap();
    assert!(fill_site["charged_work_us"].as_f64().unwrap() > 0.0 && fill_site["extra_work_us"].as_f64() == Some(0.0), "{fill_site}");
    let (_, again) = profile("outcomes-again", Kinds::CACHES, 0, "local t = {}\nt.a = 1\n");
    assert_eq!(&again["luau_caches"]["prices"], prices, "every profile carries the same prices");
}

/// A real reprediction: two tables of the same keys whose `x` sits in different nodes (another key that shares x's node was stored
/// first), read in turn by one instruction, repredict at every read; two whose `x` sits in the same node hit. The profile reads
/// which by what the interpreter does, as `next` shows the nodes. A metatable on such tables does not make the displaced own
/// field "slow": the key is found in the table itself, so it is a reprediction too
#[test]
fn a_reprediction_is_a_key_at_another_slot() {
    let _lock = lock();
    let source = "local function build(order)\n\tlocal t = {}\n\tfor _, key in order do\n\t\tt[key] = 1\n\tend\n\treturn t\nend\nlocal function node(t)\n\tlocal index = 0\n\tfor key in next, t do\n\t\tindex += 1\n\t\tif key == \"x\" then\n\t\t\treturn index\n\t\tend\n\tend\n\treturn 0\nend\nlocal M = {}\nfunction M.alternate(a, b)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\tlocal t = if i % 2 == 0 then a else b\n\t\ts = t.x\n\tend\n\treturn s\nend\nfunction M.same(a, b)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\tlocal t = if i % 2 == 0 then a else b\n\t\ts = t.x\n\tend\n\treturn s\nend\nfunction M.wrapped(a, b)\n\tlocal s\n\tfor i = 1, 1000 do\n\t\tlocal t = if i % 2 == 0 then a else b\n\t\ts = t.x\n\tend\n\treturn s\nend\nlocal found = nil\nfor k = 1, 64 do\n\tlocal key = \"k\" .. k\n\tlocal first, second = build({\"x\", key}), build({key, \"x\"})\n\tif node(first) ~= node(second) then\n\t\tfound = {first, second, key}\n\t\tbreak\n\tend\nend\nassert(found, \"no key shares x's node\")\nM.alternate(found[1], found[2])\nM.same(found[1], build({\"x\", found[3]}))\nlocal meta = {__index = {}}\nM.wrapped(setmetatable(found[1], meta), setmetatable(found[2], meta))\n";
    let (summary, json) = profile("repredict", Kinds::CACHES, 0, source);
    let (ops, outcomes) = sites_at(&json, function(&json, "repredict", "alternate"), 23);
    assert_eq!(ops, vec!["GETTABLEKS"]);
    assert!(outcomes.get("repredicted").copied().unwrap_or(0) >= 999, "x at another slot in turn repredicts: {outcomes:?}\n{summary}");
    let (_, outcomes) = sites_at(&json, function(&json, "repredict", "same"), 31);
    assert!(outcomes.get("hit").copied().unwrap_or(0) >= 999 && outcomes.get("repredicted").copied().unwrap_or(0) <= 1, "x in the same node hits: {outcomes:?}");
    let (_, outcomes) = sites_at(&json, function(&json, "repredict", "wrapped"), 39);
    assert!(outcomes.get("repredicted").copied().unwrap_or(0) >= 999 && !outcomes.contains_key("metatable_table"), "a displaced own field with a metatable: {outcomes:?}");
}

/// A builtin's declined calls say why, from the conditions of its fast path: string.byte asked for several results, table.insert
/// with a position (three arguments), string.sub while a collector step is owed, and calls in an unsafe environment; a call the
/// fast path served counts as fast
#[test]
fn builtin_declines_say_why() {
    let _lock = lock();
    let source = "local M = {}\nfunction M.bytes(s)\n\tfor i = 1, 1000 do\n\t\tlocal bytes = {string.byte(s, 1, 3)}\n\tend\n\tfor i = 1, 1000 do\n\t\tlocal a = string.byte(s, 1)\n\tend\nend\nfunction M.front()\n\tlocal list = {}\n\tfor i = 1, 500 do\n\t\ttable.insert(list, 1, i)\n\tend\nend\nfunction M.pieces(text)\n\tlocal count = 0\n\tfor i = 1, 2000 do\n\t\tlocal junk = table.create(512, i)\n\t\tlocal piece = string.sub(text, 2, 5)\n\t\tcount += #piece\n\tend\n\treturn count\nend\nM.bytes(\"hello world\")\nM.front()\nM.pieces(\"hello world\")\n";
    let (summary, json) = profile("builtins", Kinds::CACHES, 0, source);
    let builtins = json["builtins"].as_array().unwrap_or_else(|| panic!("{summary}"));
    let builtin = |name: &str| builtins.iter().find(|row| row["name"] == name).unwrap_or_else(|| panic!("no {name}: {builtins:#?}"));
    let byte = builtin("string.byte");
    assert_eq!((byte["calls"].as_u64(), byte["fast"].as_u64(), byte["declines"]["results"].as_u64()), (Some(2000), Some(1000), Some(1000)), "{byte}");
    assert!(byte["advice"]["results"].as_str().is_some_and(|advice| advice.contains("string.byte(s, k)")), "{byte}");
    assert_eq!(byte["sites"][0]["declines"]["results"].as_u64(), Some(1000), "{byte}");
    let insert = builtin("table.insert");
    assert_eq!((insert["fast"].as_u64(), insert["declines"]["args"].as_u64()), (Some(0), Some(500)), "{insert}");
    let sub = builtin("string.sub");
    let gc = sub["declines"]["gc_debt"].as_u64().unwrap();
    assert!(gc > 0 && sub["calls"] == 2000, "string.sub declines while a collector step is owed: {sub}");
    assert!(sub["advice"].get("gc_debt").is_none(), "no advice where no call takes the fast path: {sub}");
    // FASTCALL sites are not inline caches
    assert!(json["luau_caches"]["sites"].as_array().unwrap().iter().all(|site| !site["op"].as_str().unwrap().starts_with("FASTCALL")));
    assert!(summary.contains("builtin calls") && summary.contains("results 1,000"), "{summary}");
}

/// A C function a CALL reaches is a row of its own at its call site, with its calls and the work booked from the call to its
/// return, whether or not it calls Luau: table.sort of 2,000 numbers each frame is most of the frames' work; and a Luau function a
/// C function calls counts a call each time it is entered (a comparator: as many as table.sort's calls to it, the VM's count)
#[test]
fn c_functions_are_rows_at_their_call_sites_and_comparators_count_each_call() {
    let _lock = lock();
    let source = "local RunService = game:GetService(\"RunService\")\nlocal values = {}\nlocal function frame()\n\tfor i = 1, 2000 do\n\t\tvalues[i] = (i * 7919) % 2003\n\tend\n\ttable.sort(values)\nend\nRunService.Heartbeat:Connect(frame)\nlocal items = {}\nfor i = 1, 500 do\n\titems[i] = (i * 31) % 499\nend\nlocal function less(a, b)\n\treturn a < b\nend\ntable.sort(items, less)\n";
    let frames = 5;
    let (summary, json) = profile("sorts", Kinds::WORK, frames, source);
    let sort = c_function(&json, "table.sort");
    // (five frames' sorts and the comparator's one)
    assert_eq!(sort["calls"].as_u64(), Some(frames as u64 + 1), "{sort}");
    let frame_id = function(&json, "sorts", "frame")["id"].clone();
    let at_frame = sort["call_sites"].as_array().unwrap().iter().find(|site| site["caller"] == frame_id).unwrap_or_else(|| panic!("{sort}"));
    assert_eq!((at_frame["calls"].as_u64(), at_frame["line"].as_i64()), (Some(frames as u64), Some(7)), "{sort}");
    // its own work: what table.sort was charged (its comparisons), most of the frames' work
    let sort_work = at_frame["self_work_ms"].as_f64().unwrap();
    let frame_total = function(&json, "sorts", "frame")["total"]["work_ms"].as_f64().unwrap();
    assert!(sort_work > 0.2 * frame_total && sort_work < frame_total, "table.sort {sort_work} ms of the frames' {frame_total} ms\n{summary}");
    // the calling line's self work is its instructions' and the sort's own (in C): the hot line reads hot, and its total holds both
    let line = json["lines"].as_array().unwrap().iter().find(|l| l["function"] == frame_id && l["line"] == 7).unwrap();
    let (own, in_c, self_work) = (line["work_ms"].as_f64().unwrap(), line["c_work_ms"].as_f64().unwrap(), line["self_work_ms"].as_f64().unwrap());
    // (the total sums the same work in another order: within a rounding of the self work)
    assert!((in_c - sort_work).abs() < 1e-9 && (self_work - own - in_c).abs() < 1e-9 && line["total_work_ms"].as_f64().unwrap() >= self_work * (1.0 - 1e-12), "{line}");
    // every line's self work adds up to the work clock's, less what was charged outside any instruction
    let lines_total: f64 = json["lines"].as_array().unwrap().iter().map(|l| l["self_work_ms"].as_f64().unwrap()).sum();
    let clock = json["work"]["total_ms"].as_f64().unwrap() - json["work"]["outside_instructions_ms"].as_f64().unwrap_or(0.0);
    assert!((lines_total - clock).abs() < 1e-6 * clock.max(1.0), "lines {lines_total} ms, the clock's {clock} ms");
    // the tree's node has its calling line in tree_lines, beside the rows: a row keeps the six fields an older reader takes (its
    // parser wants six: [parent, function, calls, self work in us, self instructions, label])
    let tree = json["tree"].as_array().unwrap();
    assert!(tree.iter().all(|node| node.as_array().is_some_and(|row| row.len() == 6)), "every tree row has six fields: {}", json["tree"]);
    assert_eq!(json["tree_lines"].as_array().map(Vec::len), Some(tree.len()), "a calling line for each node");
    let index = tree.iter().position(|node| node[1] == sort["id"] && node[2] == frames as u64).unwrap_or_else(|| panic!("{}", json["tree"]));
    assert_eq!(json["tree_lines"][index], 7, "{}", tree[index]);
    assert_eq!((json["work"]["c_calls"].as_bool(), json["work"]["c_invocations"].as_bool()), (Some(true), Some(true)));
    // the comparator: one call per invocation (table.sort's calls to it, as the VM counts them)
    let less = function(&json, "sorts", "less");
    let compared = json["end"]["vm"]["sort_calls"].as_u64().unwrap();
    let calls = less["calls"].as_u64().unwrap();
    assert!(calls > 3000, "a comparator of 500 items is called thousands of times: {calls}\n{summary}");
    assert_eq!(calls, compared, "the VM's count of table.sort's calls to it");
}

/// The boundary profiler counts a repeat read only when the value is the one an earlier read in the frame saw: a read after the
/// function wrote a new value is not one; a repeat in the same call of a function (a local holds it) is told from one in a later
/// call (one read a call, the function called twice: the caller can read it once) and from one by another function; a Parent
/// written with the parent it has is an unchanged write, and the report lists unchanged writes. A Parent read after the instance
/// was destroyed (reparented to nil), a child looked up by name after a child was added and one after two children swapped names
/// are new values
#[test]
fn boundary_repeats_count_unchanged_values_by_where_a_cache_would_go() {
    let _lock = lock();
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::BOUNDARY, temp_dir("boundary")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    // (a table's functions: -O2 inlines a local function into its caller, whose frame would hold its reads)
    let source = [
        "local Part = Instance.new(\"Part\")",
        "Part.Parent = workspace",
        "local Folder = Instance.new(\"Folder\")",
        "local M = {}",
        "function M.readWriteRead()",
        "\tlocal a = Part.Transparency",
        "\tPart.Transparency = a + 0.25",
        "\tlocal b = Part.Transparency",
        "\tlocal c = Part.Transparency",
        "end",
        "function M.perCall()",
        "\treturn Part.Size",
        "end",
        "function M.other()",
        "\treturn Part.Size",
        "end",
        "M.readWriteRead()",
        "M.perCall()",
        "M.perCall()",
        "M.other()",
        "Part.Parent = workspace",
        "Part.Parent = Folder",
        "local Part2 = Instance.new(\"Part\")",
        "Part2.Parent = workspace",
        "local Holder = Instance.new(\"Folder\")",
        "local Item = Instance.new(\"Part\")",
        "Item.Name = \"Item\"",
        "Item.Parent = Holder",
        "function M.reparented()",
        "\tlocal a = Part2.Parent",
        "\tPart2:Destroy()",
        "\tlocal b = Part2.Parent",
        "\tlocal c = Holder.Item",
        "\tlocal other = Instance.new(\"Part\")",
        "\tother.Parent = Holder",
        "\tlocal d = Holder.Item",
        "end",
        "M.reparented()",
        "local A = Instance.new(\"Part\")",
        "A.Name = \"Target\"",
        "A.Parent = Holder",
        "local B = Instance.new(\"Part\")",
        "B.Name = \"Other\"",
        "B.Parent = Holder",
        "function M.renamed()",
        "\tlocal first = Holder.Target",
        "\tA.Name = \"Other\"",
        "\tB.Name = \"Target\"",
        "\tlocal second = Holder.Target",
        "end",
        "M.renamed()",
        "",
    ]
    .join("\n");
    run(&lua, &engine, "boundary", &source);
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 1, seconds: 1.0 / 60.0, fps: 60.0, ..Default::default() }).unwrap();
    let report: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("boundary.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let row = |kind: &str, member: &str, line: i64| -> Json {
        report["by_site_count"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == kind && row["member"] == member && row["line"] == line)
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} {member} at line {line}\n{summary}"))
    };
    let repeats = |row: &Json| (row["repeat_same_call"].as_u64().unwrap(), row["repeat_later_call"].as_u64().unwrap(), row["repeat_other_function"].as_u64().unwrap());
    assert_eq!(repeats(&row("get", "BasePart.Transparency", 8)), (0, 0, 0), "after a write of a new value: a new value\n{summary}");
    assert_eq!(repeats(&row("get", "BasePart.Transparency", 9)), (1, 0, 0), "read again in the same call");
    let per_call = row("get", "BasePart.Size", 12);
    assert_eq!((per_call["count"].as_u64(), repeats(&per_call)), (Some(2), (0, 1, 0)), "one read a call, called twice: a later call's repeat");
    assert_eq!(repeats(&row("get", "BasePart.Size", 15)), (0, 0, 1), "another function read it, unchanged");
    assert_eq!(row("set", "Instance.Parent", 21)["unchanged_writes"].as_u64(), Some(1), "Parent set to the parent it has");
    assert_eq!(row("set", "Instance.Parent", 22)["unchanged_writes"].as_u64(), Some(0));
    assert!(summary.contains("unchanged writes") && summary.lines().any(|line| line.contains("Instance.Parent") && line.contains("boundary:21")), "{summary}");
    assert_eq!(repeats(&row("get", "Instance.Parent", 32)), (0, 0, 0), "after Destroy: another parent, a new value\n{summary}");
    let child = report["by_site_count"].as_array().unwrap().iter().find(|row| row["kind"] == "get" && row["line"] == 36).cloned().unwrap_or_else(|| panic!("no child read at line 36\n{summary}"));
    assert_eq!(repeats(&child), (0, 0, 0), "a child added: the child list is new: {child}");
    let renamed = report["by_site_count"].as_array().unwrap().iter().find(|row| row["kind"] == "get" && row["line"] == 49).cloned().unwrap_or_else(|| panic!("no child read at line 49\n{summary}"));
    assert_eq!(repeats(&renamed), (0, 0, 0), "two children's names swapped: the name reads another part: {renamed}");
}

/// A host time profile of `source` run once, then `frames` frames with `between` run before each (the engine's own work)
fn time_profile(name: &str, frames: u32, source: &str, between: impl FnMut()) -> (String, Json) {
    time_profile_with(name, frames, source, false, between)
}

/// [`time_profile`], on the test's own clock where `manual` (luau_profile::manual_ticks): the clock moves only when `between`
/// calls luau_profile::advance_ticks or the script calls `tick(n)`, so every booking is exact
fn time_profile_with(name: &str, frames: u32, source: &str, manual: bool, mut between: impl FnMut()) -> (String, Json) {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::TIME, temp_dir(name)) };
    crate::luau_profile::manual_ticks(manual);
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    run_with(&lua, &engine, name, source, |env| {
        let tick = lua
            .create_function(|_, ticks: u64| {
                crate::luau_profile::advance_ticks(ticks);
                Ok(())
            })
            .unwrap();
        env.raw_set("tick", tick).unwrap();
    });
    for _ in 0..frames {
        between();
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let summary = session
        .finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: frames as u64, seconds: frames as f64 / 60.0, fps: 60.0, ..Default::default() })
        .unwrap();
    crate::luau_profile::manual_ticks(false);
    let json: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// A function row by its exact name (an engine step's frame: `(ree engine step: recorder)`)
fn named<'a>(json: &'a Json, name: &str) -> Option<&'a Json> {
    json["functions"].as_array().unwrap().iter().find(|f| f["name"] == name)
}

/// A work profile of `source` with `outer` (a host function: it calls the Luau function it is given, an error it raises caught,
/// then charges a millisecond of the engine's work) and `pair` (two host functions no global holds) in its environment
fn host_function_profile(name: &str, source: &str) -> (String, Json) {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::WORK, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    let work = engine.state.borrow().sched.work.clone();
    run_with(&lua, &engine, name, source, |env| {
        let outer = lua
            .create_function(move |_, inner: LuaFunction| {
                let _ = inner.call::<()>(());
                work.set(work.get() + 1e-3);
                Ok(())
            })
            .unwrap();
        env.raw_set("outer", outer).unwrap();
        let pair = lua
            .create_function(|lua, ()| {
                let first = lua.create_function(|_, ()| Ok(1))?;
                let second = lua.create_function(|_, ()| Ok(2))?;
                Ok((first, second))
            })
            .unwrap();
        env.raw_set("pair", pair).unwrap();
    });
    profiler::collect(&lua);
    let summary = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: 0, ..Default::default() }).unwrap();
    let json: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    (summary, json)
}

/// What a C function does after a Luau function it called is unwound by an error (or its thread yielded) is its own: the
/// 10 ms `outer` charges once its argument raised go to `outer`'s row, not to `error`'s, the C function the unwound frame
/// reached last
#[test]
fn a_c_functions_work_after_an_error_inside_it_is_its_own() {
    let _lock = lock();
    let source = "local M = {}\nfunction M.inner()\n\terror(\"no\")\nend\nfunction M.run()\n\tfor i = 1, 10 do\n\t\touter(M.inner)\n\tend\nend\nM.run()\n";
    let (summary, json) = host_function_profile("unwound", source);
    let own = |name: &str| c_function(&json, name)["self"]["work_ms"].as_f64().unwrap();
    assert!(own("outer") >= 10.0, "outer's own 10 ms: {}\n{summary}", own("outer"));
    assert!(own("error") < 1.0, "error's: {}\n{summary}", own("error"));
}

/// C functions no global holds and with no debug name read apart, by the call site that reached them
#[test]
fn unnamed_c_functions_are_named_by_their_call_site() {
    let _lock = lock();
    let source = "local first, second = pair()\nlocal M = {}\nfunction M.run()\n\tlocal t = 0\n\tfor i = 1, 10 do\n\t\tt += first()\n\t\tt += second()\n\tend\n\treturn t\nend\nM.run()\n";
    let (summary, json) = host_function_profile("unnamed", source);
    let names: Vec<&str> = json["functions"].as_array().unwrap().iter().filter(|f| f["kind"] == "C").filter_map(|f| f["name"].as_str()).collect();
    assert!(names.contains(&"? called at unnamed:6") && names.contains(&"? called at unnamed:7"), "{names:?}\n{summary}");
    assert!(!names.contains(&"?"), "{names:?}");
}

/// A cache profile of `source` (chunk `=name`) in a playing client, its environment made unsafe after its load when `unsafe_env`
/// (as another script's getfenv would make it): a setup frame, then `frames` frames in a phase of their own, `steady`, which the
/// report covers (cold tracking begins with the first frame, as in a client, where a table's first touch, a C function's first
/// call and a function's first run are charged: the first handler can run before it). Its `clock()` reads the work clock, as a
/// run's Context.WorkSeconds does, for the script to measure itself while its `measuring` global is set (the steady frames):
/// profile.json and its `moved` global
fn frame_charge_profile(name: &str, source: &str, unsafe_env: bool, frames: u32) -> (Json, f64) {
    let (json, moved, _) = frame_charge_profile_with(name, source, unsafe_env, frames, None);
    (json, moved)
}

/// [`frame_charge_profile`], with a second measure the script keeps (a global's name) read too
fn frame_charge_profile_with(name: &str, source: &str, unsafe_env: bool, frames: u32, second: Option<&str>) -> (Json, f64, Option<f64>) {
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::CACHES, temp_dir(name)) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    let script = {
        let mut state = engine.state.borrow_mut();
        let class = engine.reflection.class_id("ModuleScript").unwrap();
        crate::instance::create_named(&mut state, &engine.reflection, class, "Test")
    };
    let env = engine_lua::modules::environment(&lua, &engine, script).expect("environment");
    let clock_engine = engine.clone();
    env.raw_set("clock", lua.create_function(move |_, ()| Ok(clock_engine.state.borrow().sched.work_total())).unwrap()).unwrap();
    let function = lua.load(source).set_name(format!("={name}")).set_environment(env.clone()).into_function().expect("compile");
    if unsafe_env {
        unsafe { lua.exec_raw::<()>(&env, |state| mlua::ffi::lua_setsafeenv(state, -1, 0)).unwrap() };
    }
    let call = engine_lua::globals::call(&lua, &engine, function, LuaMultiValue::new()).expect("call");
    if let CallOutcome::Failed(message, _) = call.outcome().unwrap() {
        panic!("test script failed: {message}");
    }
    scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    crate::luau_profile::set_phase("steady");
    env.raw_set("measuring", true).unwrap();
    for _ in 0..frames {
        scheduler::step(&lua, &engine, Some(1.0 / 60.0)).unwrap();
    }
    profiler::collect(&lua);
    let all = frames as u64 + 1;
    let _ = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), frames: all, seconds: all as f64 / 60.0, fps: 60.0, ..Default::default() });
    let json: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    assert_eq!(json["luau_caches"]["scope"], serde_json::json!(["steady"]), "the report covers the steady frames");
    let moved: f64 = env.raw_get("moved").expect("the script's measure");
    let second = second.map(|global| env.raw_get::<f64>(global).expect("the script's second measure"));
    (json, moved, second)
}

/// The work clock's charges and the uncharged prices never overlap, inside frames (where a table's first touch, a C function's
/// first call and a function's first run in the frame are charged): for each kind of outcome against hits in the same
/// instructions, a Heartbeat handler reads the work clock around the loop, and what it moved is exactly what the profile's
/// charged figures moved (the sites' charge past their dispatch; a declined builtin call's, less what the fast path would charge
/// the same calls: its fallback's instructions, the C function it reached with its cold first call, the engine's own Luau a
/// Vector3.new reached), with an `__index` function's own instructions in its row; the uncharged figures (a reprediction, an
/// absent key, an insert with room, a metatable walk) are work beyond the clock's. So the profile's priced figures plus the
/// clock's work are the clock's work plus only the uncharged figures. A site that did more than hit is apart by call-tree node,
/// and its nodes add up to it exactly.
#[test]
fn the_work_clocks_charges_and_the_uncharged_prices_never_overlap() {
    let _lock = lock();
    const FRAMES: u32 = 4;
    // (setups: a pool of fresh tables for each frame's inserts, two tables that hold the same key at different slots)
    let pool = "local function pool(proto)\n\tlocal p = table.create(2000)\n\tfor i = 1, 2000 do\n\t\tp[i] = table.clone(proto)\n\tend\n\treturn p\nend\n";
    let pair = "local function build(order)\n\tlocal t = {}\n\tfor _, key in order do\n\t\tt[key] = 1\n\tend\n\treturn t\nend\nlocal function node(t)\n\tlocal index = 0\n\tfor key in next, t do\n\t\tindex += 1\n\t\tif key == \"x\" then\n\t\t\treturn index\n\t\tend\n\tend\n\treturn 0\nend\nlocal first, second\nfor k = 1, 64 do\n\tlocal key = \"k\" .. k\n\tfirst, second = build({\"x\", key}), build({key, \"x\"})\n\tif node(first) ~= node(second) then\n\t\tbreak\n\tend\nend\n";
    let index = "setmetatable({}, {__index = function(t, key)\n\treturn 1\nend})";
    enum Kind {
        /// The clock charges it: the profile's charged figure is all of it, none priced again
        Charged,
        /// The clock does not: its price is work beyond the clock's
        Uncharged,
    }
    struct Case {
        name: &'static str,
        /// Runs of the loop's body a frame
        n: u32,
        body: &'static str,
        /// The loop's arguments, made each frame before the clock is read: the outcome's run, the hits'
        outcome: &'static str,
        hit: &'static str,
        setup: &'static str,
        kind: Kind,
        unsafe_env: [bool; 2],
        /// The loop runs every frame (1) or every other one (2: each run a function's first in a frame whose frame before ran none)
        every: u32,
    }
    let case = |name, n, body, outcome, hit, setup, kind| Case { name, n, body, outcome, hit, setup, kind, unsafe_env: [false, false], every: 1 };
    let cases = [
        case("repredicted", 2000, "local t = if i % 2 == 0 then a else b\n\t\ts = t.x", "first, second", "first, first", pair, Kind::Uncharged),
        case("absent", 2000, "s = a.y", "{x = 1}", "{x = 1, y = 1}", "", Kind::Uncharged),
        case("inserted", 2000, "a[i].x = i", "pool({a = 1, b = 1, c = 1})", "pool({x = 0, a = 1, b = 1, c = 1})", pool, Kind::Uncharged),
        case("metatable", 2000, "s = a.x", "setmetatable({}, {__index = {x = 1}})", "{x = 1}", "", Kind::Uncharged),
        case("rehashed", 2000, "a[i].x = i", "pool({a = 1, b = 1, c = 1, d = 1})", "pool({x = 0, a = 1, b = 1, c = 1})", pool, Kind::Charged),
        // (a table the frame did not make: its first touch in the frame is charged, at the instruction that touched it)
        case("cold first touch", 2000, "s = a.x", "kept", "{x = 1}", "local kept = {x = 1}\n", Kind::Charged),
        case("declined math.abs", 2000, "s = math.abs(a)", "\"3\"", "3", "", Kind::Charged),
        // (once a frame: the C function's first call in the frame is most of it)
        case("declined math.ceil once a frame", 1, "s = math.ceil(a)", "\"3\"", "3", "", Kind::Charged),
        // (the engine's own Luau constructs it: what it runs is charged to the script's call)
        case("declined Vector3.new", 2000, "s = Vector3.new(a, 2, i)", "\"1\"", "1", "", Kind::Charged),
        case("declined Vector3.new given nils", 2000, "s = Vector3.new(a, b)", "nil, nil", "0, 0", "", Kind::Charged),
        Case { unsafe_env: [true, false], ..case("looked-up import", 2000, "s = game", "nil", "nil", "", Kind::Charged) },
        // (its own instructions are charged in its row; its call and its first run in a frame at the site, every other frame)
        Case { every: 2, ..case("__index function", 2000, "s = a.x", index, "{x = 1}", "", Kind::Uncharged) },
    ];
    let source = |case: &Case, args: &str| {
        format!(
            "local clock = clock\n{}local M = {{}}\nfunction M.loop(n, a, b)\n\tlocal s\n\tfor i = 1, n do\n\t\t{}\n\tend\n\treturn s\nend\nlocal total, frame = 0, 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n\tframe += 1\n\tif frame % {} ~= 0 then\n\t\treturn\n\tend\n\tlocal a, b = {}\n\tlocal before = clock()\n\tlocal r = M.loop({}, a, b)\n\tlocal after = clock()\n\tif measuring then\n\t\ttotal += after - before\n\t\tmoved = total\n\tend\n\tresult = r\nend)\n",
            case.setup, case.body, case.every, args, case.n
        )
    };
    // the loop's figures, ms: its total work, its sites' charged figures, its builtin calls' decline charge, the uncharged figures,
    // the outcomes within the noise; and the row of a function defined on the source's line `index_line` (an __index function)
    struct Figures {
        work: f64,
        charged: f64,
        /// Of `charged`: the first touches in a frame of the tables the sites reach, and the rest
        first_touch: f64,
        outcome: f64,
        declined: f64,
        extra: f64,
        noisy: u64,
        body: f64,
    }
    let figures = |json: &Json, index_line: Option<usize>| -> Figures {
        let id = function(json, "charges", "loop")["id"].clone();
        let sites: Vec<&Json> = json["luau_caches"]["sites"].as_array().unwrap().iter().filter(|site| site["function"] == id).collect();
        let sum = |field: &str| sites.iter().map(|site| site[field].as_f64().unwrap()).sum::<f64>() / 1e3;
        // (the split of each site's charge adds up to it exactly)
        for site in &sites {
            let (charged, first_touch, outcome) = (site["charged_work_us"].as_f64().unwrap(), site["first_touch_us"].as_f64().unwrap(), site["outcome_charged_us"].as_f64().unwrap());
            assert!((charged - first_touch - outcome).abs() < 1e-9 && first_touch >= 0.0 && outcome >= 0.0, "charged = first touch + outcome: {site}");
            let (charged, first_touch, outcome) = (site["charged_work_us_per_frame"].as_f64().unwrap(), site["first_touch_us_per_frame"].as_f64().unwrap(), site["outcome_charged_us_per_frame"].as_f64().unwrap());
            assert!((charged - first_touch - outcome).abs() < 1e-9, "and a frame's: {site}");
            // (a site that did more than hit is apart by call-tree node: the nodes add up to it exactly)
            if let Some(nodes) = site["nodes"].as_array() {
                for field in ["charged_work_us", "first_touch_us", "outcome_charged_us", "extra_work_us", "within_noise", "presized_us"] {
                    if site[field].is_null() {
                        continue;
                    }
                    let total: f64 = nodes.iter().map(|node| node[field].as_f64().unwrap()).sum();
                    assert!((total - site[field].as_f64().unwrap()).abs() < 1e-9, "{field}: the nodes add up to the site: {site}");
                }
                for (outcome, count) in site["outcomes"].as_object().unwrap() {
                    let total: u64 = nodes.iter().map(|node| node["outcomes"][outcome].as_u64().unwrap()).sum();
                    assert_eq!(Some(total), count.as_u64(), "{outcome}: the nodes add up to the site: {site}");
                }
            } else {
                assert!(site["charged_work_us"].as_f64() == Some(0.0) && site["outcomes"].as_object().unwrap().iter().all(|(outcome, n)| outcome == "hit" || n == 0), "only a site that only hit is not apart by node: {site}");
            }
        }
        let builtin_sites = json["builtins"].as_array().unwrap().iter().flat_map(|row| row["sites"].as_array().unwrap());
        // (a declined call's charge less what the fast path would charge the same calls: what the clock moves against hits)
        let declined = builtin_sites.filter(|site| site["function"] == id).map(|site| site["decline_charged_us"].as_f64().unwrap() - site["fast_equivalent_us"].as_f64().unwrap()).sum::<f64>() / 1e3;
        let body = index_line.map_or(0.0, |line| {
            let row = json["functions"].as_array().unwrap().iter().find(|f| f["module"] == "charges" && f["line"].as_u64() == Some(line as u64));
            row.unwrap_or_else(|| panic!("no function on line {line}: {:#}", json["functions"]))["phases"]["steady"]["total_work_ms"].as_f64().unwrap()
        });
        Figures {
            // (the steady frames' work: the run's holds the setup frame's)
            work: function(json, "charges", "loop")["phases"]["steady"]["total_work_ms"].as_f64().unwrap(),
            charged: sum("charged_work_us"),
            first_touch: sum("first_touch_us"),
            outcome: sum("outcome_charged_us"),
            declined,
            extra: sum("extra_work_us"),
            noisy: sites.iter().map(|site| site["within_noise"].as_u64().unwrap()).sum(),
            body,
        }
    };
    let index_line = |source: &str| source.lines().position(|line| line.contains("__index = function")).map(|at| at + 1);
    let ns = |ms: f64, per: f64| ms * 1e6 / per;
    // (profile.json gives a site's figures to a tenth of a nanosecond: a few of them summed, within a nanosecond)
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6 + 1e-9 * a.abs().max(b.abs());
    for case in &cases {
        let (source_with, source_without) = (source(case, case.outcome), source(case, case.hit));
        let ((with, moved_with), (without, moved_without)) =
            (frame_charge_profile("charges", &source_with, case.unsafe_env[0], FRAMES), frame_charge_profile("charges", &source_without, case.unsafe_env[1], FRAMES));
        // (the steady frames the loop ran in)
        let runs = (FRAMES / case.every) as f64;
        let outcomes = case.n as f64 * runs;
        // (the clock read around the loop in seconds, as ms; the profile's figures in ms)
        let moved = (moved_with - moved_without) * 1e3;
        let (w, h) = (figures(&with, index_line(&source_with)), figures(&without, index_line(&source_without)));
        let charged = (w.charged + w.declined) - (h.charged + h.declined);
        let body = w.body - h.body;
        println!(
            "{}: the clock moved {:.3} ns an outcome ({:.3} ns a run of the loop); the profile: the loop's work {:.3} ns, charged {:.3} ns (first touches {:.3} ns, the outcomes' own {:.3} ns, declined calls {:.3} ns), the __index function's row {:.3} ns, uncharged {:.3} ns ({} within the noise)",
            case.name,
            ns(moved, outcomes),
            ns(moved, runs),
            ns(w.work - h.work, outcomes),
            ns(charged, outcomes),
            ns(w.first_touch - h.first_touch, outcomes),
            ns(w.outcome - h.outcome, outcomes),
            ns(w.declined - h.declined, outcomes),
            ns(body, outcomes),
            ns(w.extra, outcomes),
            w.noisy
        );
        assert!(close(w.work - h.work, moved), "{}: the profile's work at the loop is the clock's: {} ms against {moved} ms", case.name, w.work - h.work);
        assert!(close(charged + body, moved), "{}: every charge the clock made is in a charged figure (or the __index function's row): {charged} + {body} ms against {moved} ms", case.name);
        assert!(h.extra.abs() < 1e-12, "{}: hits are priced nothing over hits: {} ms", case.name, h.extra);
        match case.kind {
            Kind::Charged => {
                assert!(moved > 0.0, "{}: the clock charges it: {moved} ms", case.name);
                assert_eq!(w.extra, 0.0, "{}: none of it priced again", case.name);
            }
            Kind::Uncharged => assert!(w.extra > 0.0 || w.noisy as f64 == outcomes, "{}: its price is work beyond the clock's ({} ms), or within the noise ({} of them)", case.name, w.extra, w.noisy),
        }
        match case.name {
            "cold first touch" => {
                assert!(close(ns(moved, runs), crate::cost::LUAU_COLD_TABLE), "a kept table's first touch a frame: {} ns a frame", ns(moved, runs));
                // (all of it the table's first touch, none the outcome's: a hit)
                assert!((w.first_touch - h.first_touch - moved).abs() < 1e-6 && (w.outcome - h.outcome).abs() < 1e-9, "the first touch split out: {} and {} ms of {moved} ms", w.first_touch - h.first_touch, w.outcome - h.outcome);
            }
            "repredicted" => assert!((w.first_touch - h.first_touch - moved).abs() < 1e-6, "the second kept table's first touch a frame, split out: {} ms of {moved} ms", w.first_touch - h.first_touch),
            "rehashed" => assert!(w.first_touch == 0.0 && (w.outcome - moved).abs() < 1e-6, "tables the frame made: no first touch, the rehashes all the outcome's: {} / {} ms of {moved} ms", w.first_touch, w.outcome),
            "declined math.ceil once a frame" => {
                // (the C function's row holds its work at the call and its first call in the frame)
                let row = with["functions"].as_array().unwrap().iter().find(|f| f["kind"] == "C" && f["name"].as_str().is_some_and(|name| name.contains("ceil")));
                let row = row.unwrap_or_else(|| panic!("no math.ceil row: {:#}", with["functions"]))["phases"]["steady"]["total_work_ms"].as_f64().unwrap();
                println!("  the C function's row: {:.3} ns a frame", ns(row, runs));
                assert!(ns(row, runs) >= crate::cost::LUAU_COLD_CFUNC, "the C function's row holds its cold first call: {} ns a frame", ns(row, runs));
                assert!(ns(w.declined - h.declined, runs) >= crate::cost::LUAU_COLD_CFUNC, "and the decline's charge holds the row");
            }
            "declined Vector3.new" | "declined Vector3.new given nils" => {
                // (the constructor's first run for a script in the frame among it)
                assert!(ns(moved, runs) > crate::cost::LUAU_COLD_HOSTFUNC, "{}: {} ns a frame", case.name, ns(moved, runs));
            }
            "looked-up import" => {
                // (each lookup at cost::LUAU_SLOW_IMPORT; once a frame the environment's first touch, split out as the table the
                // instruction reaches, and that of the shared globals its __index leads to, the lookup's own)
                let (cold, slow) = (crate::cost::LUAU_COLD_TABLE, crate::cost::LUAU_SLOW_IMPORT);
                assert!((moved * 1e6 - (outcomes * slow + 2.0 * runs * cold)).abs() < 1.0, "each at cost::LUAU_SLOW_IMPORT, two first touches a frame: {} ns", moved * 1e6);
                assert!(((w.first_touch - h.first_touch) * 1e6 - runs * cold).abs() < 1.0, "the environment's first touch split out: {} ns", (w.first_touch - h.first_touch) * 1e6);
            }
            "__index function" => {
                assert!(body > 0.0, "the function's own instructions are in its row: {body} ms");
                // (its first run in each frame it ran in, whose frame before did not run it)
                assert!(ns(charged, runs) >= crate::cost::LUAU_RARE_PROTO, "the site's charge holds the function's first run in each: {} ns a run", ns(charged, runs));
                let price = crate::profiler::prices::outcome(3, vm::METAFUNCTION, 1.0).unwrap();
                let full = price.ns + crate::cost::INTERPRETED_OPS[crate::profiler::prices::OP_LOADN] + crate::cost::INTERPRETED_OPS[crate::profiler::prices::OP_RETURN];
                assert!(price.ns < full && (price.within_noise && w.noisy as f64 == outcomes || (w.extra - outcomes * price.ns / 1e6).abs() < 1e-6), "the call's price leaves the body out: {} ms", w.extra);
            }
            _ => {}
        }
    }
}

/// A site whose inserts rehash says what building its tables presized saves, inside frames: its outcome_charged_us less its
/// presized_us (the rehashes' charge less what allocating the tables' storage at its final size up front charges beyond the
/// storage they were built with) is the work clock's difference between a loop that builds each table and grows it at the site
/// and one that builds it at its final size; a table's first touch in the frame, which presizing still pays, is split out of the
/// charge and adds back to it exactly
#[test]
fn a_rehash_sites_presized_figure_is_what_presizing_saves() {
    let _lock = lock();
    const FRAMES: u32 = 4;
    const N: f64 = 2000.0;
    // (a template's constant values fill the table: each constructor is one DUPTABLE, of 4 slots or of 8)
    let source = |constructor: &str| {
        format!("local clock = clock\nlocal M = {{}}\nfunction M.loop(n)\n\tlocal s\n\tfor i = 1, n do\n\t\tlocal t = {constructor}\n\t\tt.x = i\n\t\ts = t\n\tend\n\treturn s\nend\nlocal total = 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n\tlocal before = clock()\n\tlocal r = M.loop(2000)\n\tlocal after = clock()\n\tif measuring then\n\t\ttotal += after - before\n\t\tmoved = total\n\tend\n\tresult = r\nend)\n")
    };
    let ((growing, growing_clock), (presized, presized_clock)) = (
        frame_charge_profile("presized", &source("{a = 1, b = 1, c = 1, d = 1}"), false, FRAMES),
        frame_charge_profile("presized", &source("{a = 1, b = 1, c = 1, d = 1, x = 0}"), false, FRAMES),
    );
    // the loop's `t.x = i` (line 7)
    let site = |json: &Json| -> Json {
        let id = function(json, "presized", "loop")["id"].clone();
        let found = json["luau_caches"]["sites"].as_array().unwrap().iter().find(|site| site["function"] == id && site["line"] == 7 && site["op"] == "SETTABLEKS");
        found.unwrap_or_else(|| panic!("no t.x site: {:#}", json["luau_caches"]["sites"])).clone()
    };
    let (grows, built) = (site(&growing), site(&presized));
    let tables = N * FRAMES as f64;
    assert_eq!(grows["outcomes"]["rehashed"].as_f64(), Some(tables), "each table rehashes at the site: {grows}");
    assert_eq!(built["outcomes"]["hit"].as_f64(), Some(tables), "a presized table is a hit there: {built}");
    assert!(built["presized_us"].is_null(), "no rehash, no presized figure: {built}");
    let us = |site: &Json, field: &str| site[field].as_f64().unwrap_or_else(|| panic!("{field}: {site}"));
    for site in [&grows, &built] {
        assert!((us(site, "charged_work_us") - us(site, "first_touch_us") - us(site, "outcome_charged_us")).abs() < 1e-9, "the split adds up: {site}");
    }
    // (the clock read around each loop, in seconds, as µs)
    let saves = (growing_clock - presized_clock) * 1e6;
    let (outcome, presize) = (us(&grows, "outcome_charged_us"), us(&grows, "presized_us"));
    println!(
        "a rehash site, a table: charged {:.3} ns (first touch {:.3}, the outcome's own {:.3}), presized {:.3} ns; outcome - presized {:.3} ns, the clock's saving {:.3} ns",
        us(&grows, "charged_work_us") * 1e3 / tables,
        us(&grows, "first_touch_us") * 1e3 / tables,
        outcome * 1e3 / tables,
        presize * 1e3 / tables,
        (outcome - presize) * 1e3 / tables,
        saves * 1e3 / tables
    );
    assert!(presize > 0.0 && presize < outcome, "presizing costs something, less than the rehashes: {presize} of {outcome} µs");
    assert!((outcome - presize - saves).abs() < 1e-3, "outcome_charged_us - presized_us is the clock's saving: {outcome} - {presize} µs against {saves} µs");
    // (a slot holds 32 bytes: the 4 more an 8-slot table holds, at the clock's price for a byte of storage)
    assert!((presize * 1e3 / tables - 4.0 * 32.0 * crate::cost::LUAU_STORED).abs() < 1e-6, "{} ns a table", presize * 1e3 / tables);
}

/// What a declined builtin call would cost on the fast path (fast_equivalent_us: its fast call and the builtin's own counted
/// work there, a new string's interning and the objects it allocates, which both paths pay) is what fixing it keeps paying:
/// inside frames, a loop whose call declines against the same loop written to take the fast path, `decline_charged_us -
/// fast_equivalent_us` (the declining loop's, less the fixed one's own few declines) is the clock's difference between the two
/// loops less what the instructions the fixed form adds charge (the line that gives the call its argument, its calls' work
/// with it). string.sub to the end of a string made each frame (a new string every call: interned and allocated either way),
/// math.floor of a number given as a string, string.byte asked for two results of a range of three, and table.insert of a value
/// at the end of a table made each frame, given its position (the fast path takes none: a growing table's rehashes, keys and
/// storage, are paid either way)
#[test]
fn a_declined_builtins_fast_equivalent_is_what_a_fix_still_pays() {
    let _lock = lock();
    const FRAMES: u32 = 4;
    struct Case {
        name: &'static str,
        builtin: &'static str,
        n: u32,
        /// The line that gives the call its argument, and the call: the declining form's and the fixed one's
        setup: [&'static str; 2],
        call: [&'static str; 2],
        /// The loop's arguments: the declining form's, the fixed one's
        args: [&'static str; 2],
    }
    let cases = [
        // (-1 given as an argument: a constant would be loaded on the call's own line, which the fixed form's is not)
        Case { name: "string.sub to the end", builtin: "string.sub", n: 600, setup: ["local j = b", "local j = #a"], call: ["s = string.sub(a, i, j)"; 2], args: ["text, -1", "text, 0"] },
        Case { name: "math.floor of a string", builtin: "math.floor", n: 2000, setup: ["local x = a", "local x = tonumber(a)"], call: ["s = math.floor(x)"; 2], args: ["\"3.7\", 0", "\"3.7\", 0"] },
        Case { name: "string.byte of two results", builtin: "string.byte", n: 600, setup: ["local j = i + b", "local j = i + b"], call: ["local p, q = string.byte(a, i, j)\n\t\ts = p + q"; 2], args: ["text, 2", "text, 1"] },
        // (a fresh table each frame, grown one value a call: the fast path pays its rehashes' keys and storage, as the call does)
        Case { name: "table.insert into a growing table", builtin: "table.insert", n: 1100, setup: ["local j = i", "local j = i"], call: ["table.insert(a, i, j)", "table.insert(a, j)"], args: ["{}, 0", "{}, 0"] },
    ];
    // (the loop runs in the steady frames only: the rows of the run, the lines', are theirs)
    let source = |case: &Case, fixed: usize| {
        format!(
            "local clock = clock\nlocal M = {{}}\nfunction M.loop(n, a, b)\n\tlocal s\n\tfor i = 1, n do\n\t\t{}\n\t\t{}\n\tend\n\treturn s\nend\nlocal total, frame = 0, 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n\tframe += 1\n\tif not measuring then\n\t\treturn\n\tend\n\tlocal text = string.rep(\"abcdefghijklmnopqrstuvwxyz\", 25) .. frame\n\tlocal before = clock()\n\tlocal r = M.loop({}, {})\n\tlocal after = clock()\n\ttotal += after - before\n\tmoved = total\n\tresult = r\nend)\n",
            case.setup[fixed], case.call[fixed], case.n, case.args[fixed]
        )
    };
    for case in &cases {
        let ((declining, clock_declining), (fixed, clock_fixed)) =
            (frame_charge_profile("fastpath", &source(case, 0), false, FRAMES), frame_charge_profile("fastpath", &source(case, 1), false, FRAMES));
        // (the builtin's sites in the loop: their decline charge and fast-path equivalent, ms, and their declines; the per-node
        // records add up to them)
        let figures = |json: &Json| -> (f64, f64, u64) {
            let id = function(json, "fastpath", "loop")["id"].clone();
            let mut sums = (0.0, 0.0, 0u64);
            for row in json["builtins"].as_array().unwrap().iter().filter(|row| row["name"] == case.builtin) {
                for site in row["sites"].as_array().unwrap().iter().filter(|site| site["function"] == id) {
                    sums.0 += site["decline_charged_us"].as_f64().unwrap() / 1e3;
                    sums.1 += site["fast_equivalent_us"].as_f64().unwrap() / 1e3;
                    sums.2 += site["declines"].as_object().unwrap().values().map(|n| n.as_u64().unwrap()).sum::<u64>();
                    if let Some(nodes) = site["nodes"].as_array() {
                        for field in ["decline_charged_us", "fast_equivalent_us", "calls", "fast"] {
                            let total: f64 = nodes.iter().map(|node| node[field].as_f64().unwrap()).sum();
                            assert!((total - site[field].as_f64().unwrap()).abs() < 1e-9, "{field}: the nodes add up to the site: {site}");
                        }
                    }
                }
            }
            sums
        };
        let ((charged, fast, declines), (charged_fixed, fast_fixed, declines_fixed)) = (figures(&declining), figures(&fixed));
        // (what the fixed form's instructions charge more: the line before the call, its calls' work with it, and the line that
        // calls the loop, where a function's first run in a frame charges its code's cold first touch, an added instruction's too)
        let line_work = |json: &Json, source: &str| {
            let id = function(json, "fastpath", "loop")["id"].clone();
            let ids: Vec<Json> = json["functions"].as_array().unwrap().iter().filter(|f| f["module"] == "fastpath").map(|f| f["id"].clone()).collect();
            let calling = source.lines().position(|line| line.contains("local r = M.loop(")).unwrap() as u64 + 1;
            let lines = json["lines"].as_array().unwrap();
            let before_call = lines.iter().find(|l| l["function"] == id && l["line"] == 6).map_or(0.0, |l| l["self_work_ms"].as_f64().unwrap());
            let calls_loop: f64 = lines.iter().filter(|l| ids.contains(&l["function"]) && l["line"].as_u64() == Some(calling)).map(|l| l["self_work_ms"].as_f64().unwrap()).sum();
            before_call + calls_loop
        };
        let added = line_work(&fixed, &source(case, 1)) - line_work(&declining, &source(case, 0));
        let moved = (clock_declining - clock_fixed) * 1e3;
        let calls = (case.n * FRAMES) as f64;
        println!(
            "{}: {} of {} calls declined; a decline charged {:.3} ns, on the fast path {:.3} ns, so a fix saves {:.3} ns a call; the clock's difference {:.3} ns a call and the fixed form's added line {:.3} ns ({} of the fixed loop's calls declined)",
            case.name,
            declines,
            calls,
            charged * 1e6 / declines.max(1) as f64,
            fast * 1e6 / declines.max(1) as f64,
            (charged - fast) * 1e6 / declines.max(1) as f64,
            moved * 1e6 / calls,
            added * 1e6 / calls,
            declines_fixed
        );
        assert_eq!(declines as f64, calls, "{}: every call of the declining form declines", case.name);
        // (the fast path is no free lunch: its dispatch at least; it can cost the clock more than the call, whose C function's
        // allocations count with its call)
        assert!(fast > 0.0, "{}: the fast path would charge something: {fast} ms", case.name);
        let saving = (charged - fast) - (charged_fixed - fast_fixed);
        let lines = |json: &Json| -> String {
            let loop_row = function(json, "fastpath", "loop");
            let id = loop_row["id"].clone();
            let c_rows: Vec<String> = json["functions"].as_array().unwrap().iter().filter(|f| f["kind"] == "C").map(|f| format!("{} {}", f["name"], f["phases"]["steady"]["total_work_ms"])).collect();
            format!(
                "loop total {} self {}; C rows {}; work outside instructions {}; lines {}",
                loop_row["phases"]["steady"]["total_work_ms"],
                loop_row["phases"]["steady"]["self_work_ms"],
                c_rows.join(", "),
                json["work"]["outside_instructions_ms"],
                json["lines"].as_array().unwrap().iter().filter(|l| l["function"] == id).map(|l| format!("{}: {} {} ms", l["line"], l["ops"], l["self_work_ms"])).collect::<Vec<_>>().join("; ")
            )
        };
        assert!(
            (saving - (moved + added)).abs() < 1e-6,
            "{}: decline_charged_us - fast_equivalent_us is the clock's saving less the fixed form's added line: {saving} ms against {moved} + {added} ms
declining: {}
fixed: {}",
            case.name,
            lines(&declining),
            lines(&fixed)
        );
    }
}

/// A site's records apart by call-tree node add up to its totals, and split its charge by where it ran: a comparator whose
/// math.abs calls decline (numbers given as strings), called by table.sort and from a plain loop, has its decline charge in two
/// nodes, each (less what the fast path would charge) what the clock charged there: the clock read around the sort and around
/// the loop, against the same code on numbers, whose calls take the fast path. Inside frames
#[test]
fn a_comparators_declines_split_between_the_sort_and_the_loop() {
    let _lock = lock();
    const FRAMES: u32 = 4;
    let source = |value: &str| {
        format!(
            "local clock = clock\nlocal M = {{}}\nfunction M.less(a, b)\n\treturn math.abs(a) < math.abs(b)\nend\nfunction M.loop(xs, n)\n\tlocal s = 0\n\tfor i = 1, n - 1 do\n\t\tif M.less(xs[i], xs[i + 1]) then\n\t\t\ts += 1\n\t\tend\n\tend\n\treturn s\nend\nlocal function values(n, seed)\n\tlocal out = table.create(n)\n\tfor i = 1, n do\n\t\tlocal v = (i * 7919 + seed) % 1000\n\t\tout[i] = {value}\n\tend\n\treturn out\nend\nlocal sorted, looped = 0, 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n\tif not measuring then\n\t\treturn\n\tend\n\tlocal items, xs = values(300, 1), values(600, 7)\n\tlocal t0 = clock()\n\ttable.sort(items, M.less)\n\tlocal t1 = clock()\n\tlocal r = M.loop(xs, 600)\n\tlocal t2 = clock()\n\tsorted += t1 - t0\n\tlooped += t2 - t1\n\tmoved = sorted\n\tmovedLoop = looped\n\tresult = r\nend)\n"
        )
    };
    let ((strings, sort_strings, loop_strings), (numbers, sort_numbers, loop_numbers)) =
        (frame_charge_profile_two("comparator", &source("tostring(v)"), FRAMES), frame_charge_profile_two("comparator", &source("v"), FRAMES));
    // the math.abs sites in M.less: per node, by the function of the node's parent, decline charged less the fast path's (ms)
    let per_node = |json: &Json| -> std::collections::BTreeMap<i64, f64> {
        let less = function(json, "comparator", "less")["id"].clone();
        let tree = json["tree"].as_array().unwrap();
        let mut out = std::collections::BTreeMap::new();
        for row in json["builtins"].as_array().unwrap().iter().filter(|row| row["name"] == "math.abs") {
            for site in row["sites"].as_array().unwrap().iter().filter(|site| site["function"] == less) {
                let Some(nodes) = site["nodes"].as_array() else { continue };
                for field in ["decline_charged_us", "fast_equivalent_us", "calls"] {
                    let total: f64 = nodes.iter().map(|node| node[field].as_f64().unwrap()).sum();
                    assert!((total - site[field].as_f64().unwrap()).abs() < 1e-9, "{field}: the nodes add up to the site: {site}");
                }
                for node in nodes {
                    let parent = tree[node["node"].as_u64().unwrap() as usize][0].as_i64().unwrap();
                    let parent_function = tree[parent as usize][1].as_i64().unwrap();
                    *out.entry(parent_function).or_default() += (node["decline_charged_us"].as_f64().unwrap() - node["fast_equivalent_us"].as_f64().unwrap()) / 1e3;
                }
            }
        }
        out
    };
    let (split, fast_split) = (per_node(&strings), per_node(&numbers));
    assert!(fast_split.values().all(|ms| ms.abs() < 1e-12), "numbers take the fast path: {fast_split:?}");
    let loop_id = function(&strings, "comparator", "loop")["id"].as_i64().unwrap();
    let sort_id = c_function(&strings, "table.sort")["id"].as_i64().unwrap();
    assert_eq!(split.len(), 2, "two contexts, the sort and the loop: {split:?}");
    let in_sort = *split.get(&sort_id).unwrap_or_else(|| panic!("a node under table.sort: {split:?}"));
    let in_loop = *split.get(&loop_id).unwrap_or_else(|| panic!("a node under the loop: {split:?}"));
    let (moved_sort, moved_loop) = ((sort_strings - sort_numbers) * 1e3, (loop_strings - loop_numbers) * 1e3);
    println!("a comparator's declines: under table.sort {:.3} us (the clock's {:.3} us), from the loop {:.3} us (the clock's {:.3} us)", in_sort * 1e3, moved_sort * 1e3, in_loop * 1e3, moved_loop * 1e3);
    assert!((in_sort - moved_sort).abs() < 1e-6, "the sort's share is the clock's there: {in_sort} ms against {moved_sort} ms");
    assert!((in_loop - moved_loop).abs() < 1e-6, "the loop's share is the clock's there: {in_loop} ms against {moved_loop} ms");
}

/// [`frame_charge_profile`] for a script that measures two parts of a frame: profile.json, its `moved` and its `movedLoop`
fn frame_charge_profile_two(name: &str, source: &str, frames: u32) -> (Json, f64, f64) {
    let (json, moved, second) = frame_charge_profile_with(name, source, false, frames, Some("movedLoop"));
    (json, moved, second.unwrap_or(f64::NAN))
}

/// The opcodes the committed prices are based on are the ones vm::OPCODES names
#[test]
fn the_prices_base_opcodes_are_named() {
    use crate::profiler::{prices, vm};
    for (op, name) in [(prices::OP_LOADN, "LOADN"), (prices::OP_GETGLOBAL, "GETGLOBAL"), (prices::OP_GETIMPORT, "GETIMPORT"), (prices::OP_GETTABLEKS, "GETTABLEKS"), (prices::OP_RETURN, "RETURN")] {
        assert_eq!(vm::opcode_name(op as u8), name);
    }
    assert_eq!(prices::BASE_OPS.map(|op| vm::opcode_name(op as u8)), ["GETGLOBAL", "GETIMPORT", "GETTABLEKS", "GETTABLEKS"]);
}

/// The function row named `name` in `module`, if the profile has one (a host time profile lists only what a sample found)
fn function_if<'a>(json: &'a Json, module: &str, name: &str) -> Option<&'a Json> {
    json["functions"].as_array().unwrap().iter().find(|f| f["module"] == module && f["name"].as_str().is_some_and(|n| n == name || n.ends_with(&format!(".{name}"))))
}

/// Host time booked at an Instance API call leaves the tick's safepoint work to the next safepoint: the hardware counters'
/// reading at a safepoint keeps its own tick (it used to share host time's, and skipped a tick an API call had booked). The
/// call to `stamps` is the safepoint after the Name write
#[test]
fn a_booking_at_an_api_call_leaves_the_safepoint_its_tick() {
    let _lock = lock();
    let source = "local part = Instance.new(\"Part\")\ntick(1)\npart.Name = \"Moved\"\nseen = {stamps()}\n";
    let engine = Engine::new(Options { mode: "play".into(), ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::TIME, temp_dir("stamps")) };
    crate::luau_profile::manual_ticks(true);
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    let seen: std::rc::Rc<std::cell::Cell<(u64, u64)>> = std::rc::Rc::default();
    let kept = seen.clone();
    run_with(&lua, &engine, "stamps", source, |env| {
        let tick = lua
            .create_function(|_, ticks: u64| {
                crate::luau_profile::advance_ticks(ticks);
                Ok(())
            })
            .unwrap();
        env.raw_set("tick", tick).unwrap();
        env.raw_set(
            "stamps",
            lua.create_function(move |_, ()| {
                kept.set((crate::profiler::LAST_SAFEPOINT.with(std::cell::Cell::get), crate::luau_profile::seen()));
                Ok(())
            })
            .unwrap(),
        )
        .unwrap();
    });
    profiler::collect(&lua);
    let _ = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() });
    crate::luau_profile::manual_ticks(false);
    let _ = std::fs::remove_dir_all(&config.out);
    let (safepoint, time) = seen.get();
    assert_eq!(time, 2, "host time booked the tick at the Name write");
    assert_eq!(safepoint, 2, "the safepoint after it still ran for that tick");
}

/// Host time the engine spends between frames is its own: a step it marks (the recorder's sampling) is booked to the step's
/// frame, the rest of what runs outside the interpreter to `(ree engine: outside scripts)`, and the first handler after them keeps
/// its own cost (it used to take all of it, on its first loop edge), an error it catches each frame or not (the VM unwinds the
/// interpreter's frames for it without running their destructors: the catch tells the profile). On the test's own clock: the
/// recorder's step moves it 6 ticks a frame, the engine 4 more outside any step, the handler 1 (its `tick(1)`), so each booking
/// is exact
#[test]
fn host_time_between_frames_is_the_engines() {
    let _lock = lock();
    let source = "local RunService = game:GetService(\"RunService\")\nlocal M = {}\nfunction M.fail()\n\terror(\"no\")\nend\nfunction M.handler()\n\tlocal x = 0\n\tfor i = 1, 200 do\n\t\tx += i % 7\n\tend\n\ttick(1)\n\tpcall(M.fail)\n\tfor i = 1, 200 do\n\t\tx += i % 7\n\tend\n\treturn x\nend\nRunService.Heartbeat:Connect(M.handler)\n";
    let (summary, json) = time_profile_with("between", 10, source, true, || {
        crate::luau_profile::engine_step("recorder", || crate::luau_profile::advance_ticks(6));
        crate::luau_profile::advance_ticks(4);
    });
    // (wall time: a tick is a millisecond of the wall clock; the CPU weight is the thread's cycles, which the test's clock does not move)
    let time_of = |row: Option<&Json>| row.map_or(0.0, |row| row["total"]["wall_ms"].as_f64().unwrap());
    let recorder = time_of(named(&json, "(ree engine step: recorder)"));
    let outside = time_of(named(&json, "(ree engine: outside scripts)"));
    let handler = time_of(function_if(&json, "between", "handler"));
    assert_eq!(recorder, 60.0, "the recorder's 6 ms a frame\n{summary}");
    assert_eq!(outside, 40.0, "the engine's other 4 ms a frame outside scripts\n{summary}");
    assert_eq!(handler, 10.0, "the handler keeps its own 1 ms a frame\n{summary}");
    assert_eq!(json["time"]["wall_ms"].as_f64(), Some(110.0), "every tick booked once: {}", json["time"]);
}

/// A sample is weighted by the run thread's CPU time, with the wall clock's beside it: a step that waits (the thread not running,
/// as when another process preempts it) reads its wall time, not CPU time
#[test]
fn host_time_is_weighted_by_the_threads_cpu_time() {
    let _lock = lock();
    let (summary, json) = time_profile("cpu-weight", 3, "local x = 0\n", || crate::luau_profile::engine_step("waiting", || std::thread::sleep(std::time::Duration::from_millis(25))));
    let waiting = named(&json, "(ree engine step: waiting)").unwrap_or_else(|| panic!("{summary}"));
    let (cpu, wall) = (waiting["total"]["time_ms"].as_f64().unwrap(), waiting["total"]["wall_ms"].as_f64().unwrap());
    if cfg!(windows) {
        assert_eq!(json["time"]["weight"], "thread CPU", "{}", json["time"]);
        assert!(wall >= 50.0 && cpu < 0.2 * wall, "a sleeping step: {cpu} ms of CPU in {wall} ms of wall time\n{summary}");
    }
}

/// An Instance API call's time is booked to its own line: 3,000 CFrame writes a frame leave their time on the write's line, not
/// on the loop's edge that comes after them
#[test]
fn host_time_of_an_instance_call_is_its_lines() {
    let _lock = lock();
    let source = "local RunService = game:GetService(\"RunService\")\nlocal part = Instance.new(\"Part\")\npart.Parent = workspace\nlocal M = {}\nfunction M.writes()\n\tfor i = 1, 3000 do\n\t\tpart.CFrame = CFrame.new(i, 0, 0)\n\tend\nend\nRunService.Heartbeat:Connect(M.writes)\n";
    let (summary, json) = time_profile("writes", 30, source, || {});
    let writes = function(&json, "writes", "writes")["id"].clone();
    let line = |number: i64| json["lines"].as_array().unwrap().iter().find(|l| l["function"] == writes && l["line"] == number).map_or(0.0, |l| l["time_ms"].as_f64().unwrap());
    let (edge, write) = (line(6), line(7));
    assert!(write > 3.0 * edge && write > 5.0, "the writes' line {write} ms, the loop's edge {edge} ms\n{summary}");
}

/// A function -O2 inlined into its caller runs with no frame of its own: a sample stands for the code since the safepoint before
/// it, and an inlined copy's share of that code (by the work clock's prices of its instructions) is its function's estimated
/// inlined time, a leaf's (no call, no loop: no safepoint of its own) too; the caller's self time holds them. A sample right after
/// a call that reached a C function is not split (it holds the call's own time: a C function has no safepoint), so a leaf right
/// after `string.rep` gets none; one right after a call to a Luau function (whose own safepoints sampled it) gets its share.
/// On the test's clock, a tick every 7 safepoints: samples land on calls, returns and loop edges in turn, the same every run
#[test]
fn host_time_on_an_inlined_functions_lines_is_its() {
    let _lock = lock();
    let source = "--!optimize 2\nlocal M = {}\nfunction M.other(x)\n\treturn x\nend\nlocal function leaf(x)\n\treturn x * 3 + 1\nend\nlocal function viaCall(x)\n\treturn M.other(x) * 2\nend\nlocal function late(x)\n\treturn x * 5 + 2\nend\nlocal function afterLua(x)\n\treturn x * 7 + 3\nend\nfunction M.run(n)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tt += leaf(i) + viaCall(i)\n\tend\n\treturn t\nend\nfunction M.afterCall(n)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal s = string.rep(\"x\", 64)\n\t\tt += late(#s)\n\tend\n\treturn t\nend\nfunction M.afterLuaCall(n)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = M.other(i)\n\t\tt += afterLua(y)\n\tend\n\treturn t\nend\nresult = M.run(3000) + M.afterCall(3000) + M.afterLuaCall(3000)\n";
    crate::luau_profile::ticks_every(Some(7));
    let (summary, json) = time_profile_with("inlined", 0, source, true, || {});
    crate::luau_profile::ticks_every(None);
    let (leaf, via, late, caller) = (function(&json, "inlined", "leaf"), function(&json, "inlined", "viaCall"), function(&json, "inlined", "late"), function(&json, "inlined", "run"));
    let inlined = |row: &Json| row["inlined"]["estimated_wall_ms"].as_f64().unwrap_or(0.0);
    assert!(leaf["inlined"]["into"].as_array().is_some_and(|into| into.iter().any(|row| row["function"] == caller["id"])), "{leaf}");
    let own = caller["self"]["wall_ms"].as_f64().unwrap();
    assert!(inlined(leaf) > 0.0, "the leaf's share of the samples: {}\n{summary}", inlined(leaf));
    assert!(inlined(via) > 0.0, "the copy with a call's share: {}\n{summary}", inlined(via));
    assert!(inlined(leaf) + inlined(via) < own, "the copies' shares are the caller's: {} + {} of {own}", inlined(leaf), inlined(via));
    assert_eq!(inlined(late), 0.0, "a leaf after a C call: the samples there hold the call's time, not split\n{summary}");
    assert!(late["inlined"]["into"].as_array().is_some_and(|into| !into.is_empty()), "late is inlined: {late}");
    let after_lua = function(&json, "inlined", "afterLua");
    assert!(after_lua["inlined"]["into"].as_array().is_some_and(|into| !into.is_empty()), "afterLua is inlined: {after_lua}");
    assert!(inlined(after_lua) > 0.0, "a leaf right after a call to a Luau function gets its share: {}\n{summary}", inlined(after_lua));
    println!("inlined host time: leaf {:.3} ms, viaCall {:.3} ms, late (after a C call) {:.3} ms, afterLua (after a Luau call) {:.3} ms", inlined(leaf), inlined(via), inlined(late), inlined(after_lua));
    assert!(summary.contains("inlined functions' host time, an estimate"), "{summary}");
}

/// A call site that reaches a C function at one time and a Luau function at another: each sample after it is split or not by
/// whether a C function ran since the safepoint before it (the VM's count of C calls, read at each safepoint), not by the site's
/// history. Inside frames: the first frame's handler runs the loop once with math.abs, the later ones 3,000 times a frame with
/// a Luau function, and the leaf after the call gets its share; a loop that always calls math.abs leaves its leaf none. And a
/// builtin call its fast path served is no C call: a loop of math.abs calls after one call of math.abs before it gives its leaf
/// its share as the same loop after a Luau call does. On the test's clock, a tick every 7 safepoints
#[test]
fn host_time_after_a_mixed_call_site_follows_each_call() {
    let _lock = lock();
    let source = "--!optimize 2\nlocal RunService = game:GetService(\"RunService\")\nlocal M = {}\nfunction M.other(x)\n\treturn x\nend\nlocal function mixedLeaf(x)\n\treturn x * 7 + 3\nend\nlocal function cLeaf(x)\n\treturn x * 5 + 2\nend\nfunction M.mixed(n, f)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = f(i)\n\t\tt += mixedLeaf(y)\n\tend\n\treturn t\nend\nfunction M.always(n, f)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = f(i)\n\t\tt += cLeaf(y)\n\tend\n\treturn t\nend\nlocal function servedLeafC(x)\n\treturn x * 3 + 7\nend\nlocal function servedLeafLua(x)\n\treturn x * 3 + 7\nend\nfunction M.servedAfterC(n, f)\n\tf(1)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = math.abs(i)\n\t\tt += servedLeafC(y)\n\tend\n\treturn t\nend\nfunction M.servedAfterLua(n, f)\n\tf(1)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = math.abs(i)\n\t\tt += servedLeafLua(y)\n\tend\n\treturn t\nend\nlocal frame = 0\nRunService.Heartbeat:Connect(function()\n\tframe += 1\n\tif frame == 1 then\n\t\tresult = M.mixed(1, math.abs)\n\telse\n\t\tresult = M.mixed(3000, M.other)\n\tend\n\tresult = M.always(3000, math.abs)\n\tresult = M.servedAfterC(3000, math.abs) + M.servedAfterLua(3000, M.other)\nend)\n";
    crate::luau_profile::ticks_every(Some(7));
    let (summary, json) = time_profile_with("mixed", 5, source, true, || {});
    crate::luau_profile::ticks_every(None);
    let inlined = |row: &Json| row["inlined"]["estimated_wall_ms"].as_f64().unwrap_or(0.0);
    let (mixed_leaf, c_leaf) = (function(&json, "mixed", "mixedLeaf"), function(&json, "mixed", "cLeaf"));
    for leaf in [mixed_leaf, c_leaf] {
        assert!(leaf["inlined"]["into"].as_array().is_some_and(|into| !into.is_empty()), "inlined: {leaf}");
    }
    let own = |name: &str| function(&json, "mixed", name)["self"]["wall_ms"].as_f64().unwrap_or(0.0);
    println!(
        "inlined host time: the leaf after a call site that reached math.abs once and a Luau function after {:.3} ms (its loop's self {:.3} ms); after one that always reaches math.abs {:.3} ms (its loop's self {:.3} ms)",
        inlined(mixed_leaf),
        own("mixed"),
        inlined(c_leaf),
        own("always")
    );
    assert!(inlined(mixed_leaf) > 0.0, "a leaf after the Luau calls gets its share, though the site once reached a C function\n{summary}");
    assert_eq!(inlined(c_leaf), 0.0, "a leaf after a C call: the samples hold the call's time, not split\n{summary}");
    // (math.abs's calls in the loop take the fast path: whether the call before the loop reached a C function or a Luau one, the
    // samples in the loop are split alike)
    let (after_c, after_lua) = (inlined(function(&json, "mixed", "servedLeafC")), inlined(function(&json, "mixed", "servedLeafLua")));
    println!("a leaf after builtin calls its fast path served: {after_c:.3} ms when the call before the loop reached math.abs, {after_lua:.3} ms when it reached a Luau function");
    assert!(after_c > 0.0 && (after_c - after_lua).abs() <= 0.25 * after_lua, "served builtin calls are no C calls: {after_c} ms against {after_lua} ms\n{summary}");
}

/// A profile of a C function a loop calls that runs on after an interrupt from inside it: string.gsub on a 64 KiB subject with
/// pattern "^." (its matcher interrupts from inside C at the match, then the subject is copied), `replacement` its third
/// argument, a leaf -O2 inlined after each call; and `extra`'s functions after the loop. A setup frame, then five steady frames
/// that run it 100 times each, on the test's clock (a tick every 7 safepoints, the matcher's interrupts among them)
fn gsub_loop_profile(name: &str, replacement: &str, extra: &str, extra_call: &str) -> (String, Json) {
    let source = format!(
        "--!optimize 2\nlocal RunService = game:GetService(\"RunService\")\nlocal M = {{}}\nlocal function gsubLeaf(x)\n\treturn x * 7 + 3\nend\nlocal function replace()\n\treturn \"x\"\nend\nfunction M.gsubLoop(n, f, s)\n\tlocal t = 0\n\tfor i = 1, n do\n\t\tlocal y = f(s, \"^.\", {replacement})\n\t\tt += gsubLeaf(#y)\n\tend\n\treturn t\nend\n{extra}local text = string.rep(\"abcdefgh\", 8192)\nlocal frame = 0\nRunService.Heartbeat:Connect(function()\n\tframe += 1\n\tif frame == 1 then\n\t\treturn\n\tend\n\tresult = M.gsubLoop(100, string.gsub, text){extra_call}\nend)\n"
    );
    crate::luau_profile::ticks_every(Some(7));
    let profile = time_profile_with(name, 6, &source, true, || {});
    crate::luau_profile::ticks_every(None);
    profile
}

/// A C function's time after an interrupt from inside it is its own: string.gsub's matcher interrupts from inside C (the
/// interrupt runs at the match, while gsub still runs), and the rest of gsub's work (the copy of the subject) lands on the
/// next safepoint of the frame that called it, whose sample holds the call's time, so the leaf inlined after the call gets
/// none. An interrupt from inside a C function leaves the count of C calls to the calling frame's next safepoint (inside
/// frames, on the test's clock)
#[test]
fn host_time_after_a_c_function_that_interrupts_from_inside_is_its() {
    let _lock = lock();
    let (summary, json) = gsub_loop_profile("interrupting", "\"x\"", "", "");
    let leaf = function(&json, "interrupting", "gsubLeaf");
    assert!(leaf["inlined"]["into"].as_array().is_some_and(|into| !into.is_empty()), "inlined: {leaf}");
    let inlined = leaf["inlined"]["estimated_wall_ms"].as_f64().unwrap_or(0.0);
    let own = function(&json, "interrupting", "gsubLoop")["self"]["wall_ms"].as_f64().unwrap_or(0.0);
    println!("inlined host time after string.gsub with a literal replacement: the leaf {inlined:.3} ms (its loop's self {own:.3} ms)");
    assert!(own > 0.0, "the loop's samples hold gsub's time\n{summary}");
    assert_eq!(inlined, 0.0, "the samples after string.gsub hold its time, though its matcher interrupted from inside it\n{summary}");
}

/// A C function's time after a Luau function it called back returned into it is its own: string.gsub with a function for a
/// replacement calls it at the match (a Luau callback, its own entry into the interpreter, with its own safepoints), and the
/// rest of gsub's work lands on the next safepoint of the frame that called it, so the leaf inlined after the call gets none.
/// A callback's own samples are its own: a loop pcall runs (called through a local: a C call that calls back) gives its
/// inlined leaf its share as any loop does (inside frames, on the test's clock)
#[test]
fn host_time_after_a_c_function_that_calls_back_into_luau_is_its() {
    let _lock = lock();
    let under_pcall = "local function pcallLeaf(x)\n\treturn x * 7 + 3\nend\nfunction M.underPcall(n, p)\n\tlocal t = 0\n\tp(function()\n\t\tfor i = 1, n do\n\t\t\tt += pcallLeaf(i)\n\t\tend\n\tend)\n\treturn t\nend\n";
    let (summary, json) = gsub_loop_profile("callingback", "replace", under_pcall, " + M.underPcall(3000, pcall)");
    let inlined = |name: &str| {
        let leaf = function(&json, "callingback", name);
        assert!(leaf["inlined"]["into"].as_array().is_some_and(|into| !into.is_empty()), "inlined: {leaf}");
        leaf["inlined"]["estimated_wall_ms"].as_f64().unwrap_or(0.0)
    };
    let (after_gsub, in_callback) = (inlined("gsubLeaf"), inlined("pcallLeaf"));
    let own = function(&json, "callingback", "gsubLoop")["self"]["wall_ms"].as_f64().unwrap_or(0.0);
    let replace = function(&json, "callingback", "replace")["self"]["wall_ms"].as_f64().unwrap_or(0.0);
    println!(
        "inlined host time after string.gsub with a function for a replacement: the leaf {after_gsub:.3} ms (its loop's self {own:.3} ms, the replacement's own {replace:.3} ms); a leaf in a loop pcall runs {in_callback:.3} ms"
    );
    assert!(replace > 0.0, "gsub called the replacement back, whose own safepoints were sampled\n{summary}");
    assert_eq!(after_gsub, 0.0, "the samples after string.gsub hold its time, though a Luau function it called back returned into it\n{summary}");
    assert!(in_callback > 0.0, "a callback's own samples are split as any code's\n{summary}");
}

/// The timeline's instance and thread columns follow the program, not the collector: 200 parts made in the Workspace and
/// destroyed each frame keep the DataModel's count where it was (they are destroyed and kept only until the VM collects them),
/// and the threads made a frame are as many each frame (the collector frees finished ones when its cycle comes)
#[test]
fn timeline_instances_and_threads_follow_the_program() {
    let _lock = lock();
    let source = "local RunService = game:GetService(\"RunService\")\nlocal kept = {}\nRunService.Heartbeat:Connect(function()\n\tlocal parts = table.create(200)\n\tfor i = 1, 200 do\n\t\tlocal part = Instance.new(\"Part\")\n\t\tpart.Parent = workspace\n\t\tparts[i] = part\n\tend\n\tfor _, part in parts do\n\t\tpart:Destroy()\n\tend\n\tfor i = 1, 3 do\n\t\ttask.spawn(function() end)\n\tend\n\tkept[#kept + 1] = Instance.new(\"Folder\")\nend)\n";
    let (summary, json) = profile("census", Kinds::TIMELINE.with(Kinds::HEAP), 12, source);
    let timeline = &json["timeline"];
    let columns: Vec<&str> = timeline["columns"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    let column = |name: &str| columns.iter().position(|c| *c == name).unwrap_or_else(|| panic!("no {name}: {columns:?}"));
    let rows = timeline["rows"].as_array().unwrap();
    let values = |name: &str| -> Vec<u64> { rows.iter().map(|row| row[column(name)].as_u64().unwrap()).collect() };
    let inside = values("instances_in_datamodel");
    assert!(inside.iter().all(|n| *n == inside[0]), "the DataModel's instances stay: {inside:?}\n{summary}");
    let outside = values("instances_outside");
    assert_eq!(outside[11] - outside[0], 11, "a Folder kept a frame, outside the DataModel: {outside:?}");
    let destroyed = values("instances_destroyed");
    // (the destroyed parts wait for the collector, which takes them a step at a time: that column follows it)
    assert!(destroyed.iter().any(|n| *n >= 200), "destroyed parts until the collector takes them: {destroyed:?}");
    let made = values("threads_made");
    assert!(made.iter().all(|n| *n == made[0]) && made[0] >= 3, "as many threads made each frame: {made:?}");
    assert!(!summary.contains("over the run"), "{summary}");
    assert!(summary.contains("threads reachable at the heap snapshots"), "{summary}");
    // (a heap snapshot needs no host time: the heap kind slows the run, so the frame's host time is not taken)
    assert!(rows.iter().all(|row| row[column("host_ms")].is_null()), "a run the heap kind slows takes no host time a frame");
}

/// A module's short name is its last segment, qualified where two modules would share one: the what-if runner's harness and a
/// user's file both called WhatIf read apart
#[test]
fn short_names_are_qualified_where_two_modules_share_one() {
    let shorts = profiler::report::short_names(["ReplicatedStorage.RURFixtures.WhatIf", "ReplicatedStorage.WhatIf", "ReplicatedStorage.Scene.Mount", "rur.record"].into_iter());
    assert_eq!(shorts["ReplicatedStorage.RURFixtures.WhatIf"], "RURFixtures.WhatIf");
    assert_eq!(shorts["ReplicatedStorage.WhatIf"], "ReplicatedStorage.WhatIf");
    assert_eq!(shorts["ReplicatedStorage.Scene.Mount"], "Mount");
    assert_eq!(shorts["rur.record"], "record");
}

/// The harness of a run a tool of ree's made (the what-if runner sets REE_PROFILE_TOOL_HARNESS) is ree's own code (`tool`), as the
/// engine's chunks that drive a harness are; a user's harness and the game's code are not
#[test]
fn a_tools_harness_is_rees_own_code() {
    let _lock = lock();
    let engine = Engine::new(Options::default()).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    engine_lua::modules::interpret_all(&lua, &engine);
    let config = Config { print: false, ..Config::new(Kinds::WORK.with(Kinds::CACHES), temp_dir("tool")) };
    let session = profiler::Session::begin(config.clone());
    crate::luau_profile::install(&lua);
    // (a builtin call with its string an argument, which the compiler cannot fold; `game` an import read each turn)
    let spin = "local M = {}\nfunction M.spin(n, s)\n\tlocal x = 0\n\tfor i = 1, n do\n\t\tlocal a, b = string.byte(s, 1, 2)\n\t\tlocal g = game\n\t\tx += i + a + (if g then 1 else 0)\n\tend\n\treturn x\nend\nresult = M.spin(1000, \"xy\")\n";
    run(&lua, &engine, "ReplicatedStorage.RURFixtures.WhatIf", spin);
    run(&lua, &engine, "ReplicatedStorage.WhatIf", spin);
    run(&lua, &engine, "rur.record", spin);
    profiler::collect(&lua);
    // (the variable the what-if runner sets on its runs, read when the profile is written)
    unsafe { std::env::set_var(profiler::report::TOOL_HARNESS_ENV, "1") };
    let written = session.finish(profiler::Run { command: "test".into(), status: "ok".into(), ..Default::default() });
    unsafe { std::env::remove_var(profiler::report::TOOL_HARNESS_ENV) };
    let summary = written.unwrap();
    let json: Json = serde_json::from_str(&std::fs::read_to_string(config.out.join("profile.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&config.out);
    let spin_of = |module: &str| function(&json, module, "spin");
    assert_eq!((spin_of("ReplicatedStorage.RURFixtures.WhatIf")["tool"].as_bool(), spin_of("ReplicatedStorage.RURFixtures.WhatIf")["short"].as_str()), (Some(true), Some("RURFixtures.WhatIf")), "{summary}");
    assert_eq!((spin_of("ReplicatedStorage.WhatIf")["tool"].as_bool(), spin_of("ReplicatedStorage.WhatIf")["short"].as_str()), (Some(false), Some("ReplicatedStorage.WhatIf")));
    assert_eq!(spin_of("rur.record")["tool"].as_bool(), Some(true));
    // ree's own code's builtin calls and lookups are apart from the user's: the user's table and its advice hold the user's
    // module's 1,000 calls, not the tools' 2,000
    let byte = |rows: &Json| rows.as_array().unwrap().iter().find(|row| row["name"] == "string.byte").map_or(0, |row| row["calls"].as_u64().unwrap());
    assert_eq!((byte(&json["builtins"]), byte(&json["builtins_tool"])), (1000, 2000), "{summary}");
    let imports = |rows: &Json| rows.as_array().unwrap().iter().find(|row| row["kind"] == "import").map_or(0, |row| row["lookups"].as_u64().unwrap());
    assert_eq!((imports(&json["luau_caches"]["by_kind"]), imports(&json["luau_caches"]["tool"]["by_kind"])), (1000, 2000), "{:#}", json["luau_caches"]["by_kind"]);
    assert!(summary.contains("ree's own code's builtin calls: 2,000 calls"), "{summary}");
}

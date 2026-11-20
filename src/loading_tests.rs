//! `loadstring`, `debug.loadmodule` and the services Roblox keeps to itself, through the script API (lua/loading.rs,
//! services::ROBLOX_ONLY_SERVICES); the lines are what Studio 0.747 printed for tools/parity/probes/loadstring.luau,
//! loadmodule.luau and core_services.luau.

use std::rc::Rc;

use mlua::prelude::*;

use super::{
    Engine, Options, RunContext,
    lua::{self as engine_lua, globals::CallOutcome},
    scheduler,
};

/// Runs `source` as a script of a fresh engine with `options`, then its clock for a second: the lines it printed.
fn run(options: Options, source: &str) -> Vec<String> {
    let engine = Engine::new(options).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    script(&lua, &engine, source)
}

fn script(lua: &Lua, engine: &Rc<Engine>, source: &str) -> Vec<String> {
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
    let now = engine.state.borrow().sched.now;
    scheduler::run_until(lua, engine, now + 1.0).unwrap();
    let entries = &engine.state.borrow().output.entries;
    let errors: Vec<String> = entries.iter().filter(|e| e.kind == "error").map(|e| e.message.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    entries.iter().filter(|e| e.kind == "print").map(|e| e.message.to_string()).collect()
}

fn server(load_string: bool) -> Options {
    Options { mode: "play".into(), run_context: RunContext::Server, local_player_name: None, load_string, ..Options::default() }
}

fn check(lines: &[String], wanted: &[&str]) {
    assert_eq!(lines.iter().map(String::as_str).collect::<Vec<_>>(), wanted, "{lines:#?}");
}

const MODULES: &str = r##"
    local function Make(Name, Source, Class)
        local Module = Instance.new(Class or "ModuleScript")
        Module.Name = Name
        Module.Source = Source
        Module.Parent = game:GetService("ReplicatedStorage")
        return Module
    end
"##;

#[test]
fn loadstring_is_refused_on_a_client_and_where_the_server_has_it_off() {
    let source = r##"
        print(type(loadstring), pcall(loadstring, "return 1"))
        print(pcall(loadstring))
    "##;
    let refused = ["function false loadstring() is not available", "false loadstring() is not available"];
    check(&run(Options::default(), source), &refused);
    check(&run(Options { mode: "play".into(), load_string: true, ..Options::default() }, source), &refused);
    check(&run(server(false), source), &refused);
    let enabled = run(server(true), source);
    assert!(enabled[0].starts_with("function true function: "), "{enabled:?}");
    assert_eq!(enabled[1], "false missing argument #1 to 'loadstring' (string expected)");
}

#[test]
fn loadstring_compiles_a_chunk_in_the_callers_environment_without_running_it() {
    let lines = run(
        server(true),
        r##"
        local Chunk = loadstring("Ran = (Ran or 0) + 1 return Ran, script, ...")
        print("loaded", type(Chunk), Ran)
        print("called", Chunk("a") == 1, select(2, Chunk()) == script, select(3, Chunk("b")), Ran, rawget(getfenv(1), "Ran"))
        print("env", getfenv(Chunk) == getfenv(1), pcall(loadstring, "return 1") and select(2, pcall(function() return getfenv(loadstring("return 1")) == getfenv(1) end)))
        print("twins", loadstring("return 1") == loadstring("return 1"))
        local Fenced = loadstring("return Marker, type(print), script")
        print("setfenv", setfenv(Fenced, { Marker = "replaced", type = type }) == Fenced, Fenced())
        print("caller", Marker, type(print))
        setfenv(Fenced, setmetatable({ Marker = "layered" }, { __index = getfenv(1) }))
        print("layered", Fenced())
        print("yields", loadstring("task.wait() return 'waited'")())
        "##,
    );
    check(
        &lines,
        &[
            "loaded function nil",
            "called true true b 3 3",
            "env true true",
            "twins false",
            "setfenv true replaced nil nil",
            "caller nil function",
            "layered layered function Test",
            "yields waited",
        ],
    );
}

#[test]
fn loadstring_names_its_chunk_and_answers_a_syntax_error_as_a_message() {
    let lines = run(
        server(true),
        r##"
        local function Fail(...) return select(2, pcall(loadstring(...))) end
        print(Fail("error('boom')"))
        print(Fail("error('boom')", "Name"))
        print(Fail("error('boom')", "=Name"))
        print(Fail("error('boom')", "@Name"))
        print(Fail("local A = 1\nerror('boom')"))
        print(debug.info(loadstring("return 1", "=Name"), "s"), debug.info(loadstring(""), "s"), debug.info(loadstring("\0\1\2"), "s"))
        print(loadstring("local = 1"))
        print(loadstring("local = 1", "=Name"))
        print(select("#", loadstring("local = 1")), select("#", loadstring("return 1")))
        print(loadstring(12))
        print(pcall(loadstring, nil))
        print(pcall(loadstring, {}))
        print(pcall(loadstring, "return 1", {}))
        "##,
    );
    check(
        &lines,
        &[
            "[string \"error('boom')\"]:1: boom",
            "[string \"Name\"]:1: boom",
            "Name:1: boom",
            "Name:1: boom",
            "[string \"local A = 1...\"]:2: boom",
            "Name [string \"\"] [string \"\"]",
            "nil [string \"local = 1\"]:1: Expected identifier when parsing variable name, got '='",
            "nil Name:1: Expected identifier when parsing variable name, got '='",
            "2 1",
            "nil [string \"12\"]:1: Expected identifier when parsing expression, got '12'",
            "false invalid argument #1 to 'loadstring' (string expected, got nil)",
            "false invalid argument #1 to 'loadstring' (string expected, got table)",
            "false invalid argument #2 to 'loadstring' (string expected, got table)",
        ],
    );
}

#[test]
fn loadmodule_is_not_enabled_unless_the_run_turns_it_on() {
    let lines = run(
        Options::default(),
        r##"
        print(type(debug.loadmodule), pcall(debug.loadmodule, Instance.new("ModuleScript")))
        print(pcall(debug.loadmodule))
        "##,
    );
    check(&lines, &["function false debug.loadmodule is not enabled.", "false debug.loadmodule is not enabled."]);
}

#[test]
fn loadmodule_compiles_a_modules_source_without_running_it() {
    let source = format!(
        "{MODULES}{}",
        r##"
        local Counter = Make("Counter", "_G.Loads = (_G.Loads or 0) + 1 return { Count = _G.Loads, Script = script, Arguments = select('#', ...) }")
        local Load, Problem, Cleanup = debug.loadmodule(Counter)
        print("loaded", type(Load), Problem, type(Cleanup), select("#", debug.loadmodule(Counter)), _G.Loads)
        print("info", debug.info(Load, "s"), debug.info(Load, "l"))
        local First, Second = Load(), Load("a", "b")
        print("calls", First.Count, Second.Count, First ~= Second, First.Script == Counter, Second.Arguments)
        print("require", require(Counter).Count, require(Counter).Count, Load().Count)
        print("cleanup", select("#", Cleanup()), Load().Count)
        Counter.Source = "return 'second'"
        Counter.Name = "Renamed"
        local Again = debug.loadmodule(Counter)
        print("source", Again(), debug.info(Again, "s"), debug.info(Load, "s"))
        local Loose = Instance.new("ModuleScript")
        print("fresh", debug.info(debug.loadmodule(Loose), "s"), select("#", debug.loadmodule(Loose)()))
        print("fails", pcall(debug.loadmodule(Make("Fail", "local A = 1\nerror('boom')"))))
        print("yields", debug.loadmodule(Make("Yields", "task.wait() return 'waited'"))())
        "##
    );
    let lines = run(Options { load_module: true, ..Options::default() }, &source);
    check(
        &lines,
        &[
            "loaded function nil function 3 nil",
            "info ReplicatedStorage.Counter 1",
            "calls 1 2 true true 2",
            "require 3 3 4",
            "cleanup 0 5",
            "source second ReplicatedStorage.Renamed ReplicatedStorage.Counter",
            "fresh ModuleScript 0",
            "fails false ReplicatedStorage.Fail:2: boom",
            "yields waited",
        ],
    );
}

#[test]
fn a_loaded_modules_environment_is_its_own_and_setfenv_replaces_it() {
    let source = format!(
        "{MODULES}{}",
        r##"
        local Reader = Make("Reader", "return { getfenv(1), script, _G, shared, game }")
        local Load = debug.loadmodule(Reader)
        local Env, Script, Globals, Shared, Game = table.unpack(Load())
        print("env", Env == getfenv(Load), Env ~= getfenv(1), Script == Reader, rawget(Env, "script") == Reader, Globals == _G, Shared == shared, Game == game)
        local Twin = debug.loadmodule(Reader)
        print("twin", Twin ~= Load, getfenv(Twin) ~= Env, require(Reader)[1] ~= Env)
        print("requires", Env.require(Make("Dependency", "return 'dependency ' .. script.Name")))
        local Fenced = debug.loadmodule(Make("Fenced", "return Marker, type(print), script"))
        local Default = getfenv(Fenced)
        print("setfenv", setfenv(Fenced, { Marker = "replaced", type = type }) == Fenced, Fenced())
        local Layered = setmetatable({ Marker = "layered", script = "mocked", print = false }, { __index = Default })
        setfenv(Fenced, Layered)
        print("layered", getfenv(Fenced) == Layered, Fenced())
        print("other", debug.loadmodule(Make("Fenced", "return Marker, type(print), script.Name"))())
        "##
    );
    let lines = run(Options { load_module: true, ..Options::default() }, &source);
    check(
        &lines,
        &[
            "env true true true true true true true",
            "twin true true true",
            "requires dependency Dependency",
            "setfenv true replaced nil nil",
            "layered true layered boolean mocked",
            "other nil function Fenced",
        ],
    );
}

#[test]
fn loadmodule_answers_a_syntax_error_as_a_message_and_refuses_what_is_no_module() {
    let source = format!(
        "{MODULES}{}",
        r##"
        print(debug.loadmodule(Make("Syntax", "local = 1")))
        print(select("#", debug.loadmodule(Make("Lines", "local A = 1\nlocal B = (\nreturn"))), (select(2, debug.loadmodule(Make("Lines", "local A = 1\nlocal B = (\nreturn")))))
        for _, Case in { { Make("AScript", "return 1", "Script") }, { Make("ALocal", "return 1", "LocalScript") }, { game }, {}, { "return 1" }, { 12 }, { Make("Extra", "return 1"), "more" } } do
            print(pcall(debug.loadmodule, table.unpack(Case)))
        end
        "##
    );
    let lines = run(Options { load_module: true, ..Options::default() }, &source);
    let refused = "false Attempted to call loadmodule with invalid argument(s).";
    check(
        &lines,
        &[
            "nil ReplicatedStorage.Syntax:1: Expected identifier when parsing variable name, got '='",
            "2 ReplicatedStorage.Lines:3: Expected identifier when parsing expression, got 'return'",
            refused,
            refused,
            refused,
            refused,
            refused,
            refused,
            refused,
        ],
    );
}

#[test]
fn services_roblox_keeps_to_itself_answer_nil() {
    let lines = run(
        Options::default(),
        r##"
        for _, Name in { "CoreScriptSyncService", "CorePackages", "RobloxReplicatedStorage", "PluginGuiService", "RobloxPluginGuiService", "AppStorageService", "CSGDictionaryService" } do
            assert(game:GetService(Name) == nil and game:FindService(Name) == nil, Name)
        end
        print(pcall(function() local Service = game:GetService("CoreScriptSyncService") local _ = Service.Name return Service end))
        print(game:GetService("CoreGui"), game:GetService("ScriptContext").Name, game:FindService("ServerStorage"))
        print(pcall(game.FindService, game, "NotAService"))
        "##,
    );
    check(
        &lines,
        &[
            "false Test:5: attempt to index nil with 'Name'",
            "CoreGui Script Context nil",
            "false 'NotAService' is not a valid Service name",
        ],
    );
}

#[test]
fn getfenv_of_the_thread_and_of_an_engine_function_is_the_calling_scripts_environment() {
    let lines = run(
        Options::default(),
        r##"
        local Env = getfenv(1)
        print("script", getfenv(0) == Env, getfenv() == Env, getfenv(print) == Env, getfenv(0).script == script, type(getfenv(0).warn))
        local function Inner() return getfenv(1), getfenv(2), getfenv(0) end
        local Own = setmetatable({}, { __index = Env })
        setfenv(Inner, Own)
        local One, Two, Zero = Inner()
        print("levels", One == Own, Two == Env, Zero == Own, getfenv(Inner) == Own)
        task.spawn(function() print("spawned", getfenv(0) == Env) end)
        local Event = Instance.new("BindableEvent")
        Event.Event:Connect(function() print("handler", getfenv(0) == Env) end)
        Event:Fire()
        print("pcall", select(2, pcall(getfenv, 0)) == Env, coroutine.wrap(function() return getfenv(0) == Env end)())
        print("invalid", select(2, pcall(getfenv, 50)), select(2, pcall(getfenv, "x")))
        "##,
    );
    check(
        &lines,
        &[
            "script true true true true function",
            "levels true true true true",
            "spawned true",
            "pcall true true",
            "invalid invalid argument #1 to 'getfenv' (invalid level) invalid argument #1 to 'getfenv' (number expected, got string)",
            "handler true",
        ],
    );
}

#[test]
fn loadmodule_gives_a_json_module_a_function_that_decodes_it() {
    let folder = std::env::temp_dir().join(format!("ree-loadmodule-json-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Data.json"), r##"{"Name": "Sample", "Values": [1, 2, 3]}"##).unwrap();
    let engine = Engine::new(Options { load_module: true, ..Options::default() }).expect("engine");
    let lua = Lua::new();
    engine_lua::start(&lua, &engine).expect("start");
    super::project::mount_at(&engine, &super::project::Mount::parse(&format!("ReplicatedStorage/Mounted={}", folder.to_string_lossy()), false).unwrap()).unwrap();
    let lines = script(
        &lua,
        &engine,
        r##"
        local Module = game:GetService("ReplicatedStorage"):FindFirstChild("Data", true)
        local Load, Problem, Cleanup = debug.loadmodule(Module)
        local First, Second = Load(), Load()
        print(type(Load), Problem, type(Cleanup), First.Name, #First.Values, First ~= Second, debug.info(Load, "s") == Module:GetFullName())
        print(setfenv(Load, setmetatable({}, { __index = getfenv(Load) })) == Load, Load().Values[3], require(Module).Name)
        "##,
    );
    let _ = std::fs::remove_dir_all(&folder);
    check(&lines, &["function nil function Sample 3 true true", "true 3 Sample"]);
}

#[test]
fn an_edit_run_context_is_both_sides() {
    let source = r##"local RunService = game:GetService("RunService") print(RunService:IsClient(), RunService:IsServer(), RunService:IsRunning(), game:GetService("Players").LocalPlayer, #game:GetService("Players"):GetPlayers())"##;
    // (the command line's edit run context has no local player, as Studio's edit DataModel has none)
    check(&run(Options { run_context: RunContext::Edit, local_player_name: None, ..Options::default() }, source), &["true true false nil 0"]);
    check(&run(Options::default(), source), &["true false false Player 1"]);
}

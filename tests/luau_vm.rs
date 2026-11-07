//! Patched Luau VM behaviour (patches/luau/, applied to luau0-src via [patch.crates-io]).
//!
//! Ported from the Lune-hosted runtime/tests/table_find.luau and runtime/tests/native_builtins.luau.
//! The Luau sources live in tests/luau/ and run through mlua directly, so they exercise the same VM
//! build the engine links.

use mlua::{chunk::Compiler, Function, Lua, LuaSerdeExt, Result, Table, Value};

const TABLE_FIND: &str = include_str!("luau/table_find.luau");
const NATIVE_BUILTINS: &str = include_str!("luau/native_builtins.luau");

fn codegen_supported() -> bool {
    unsafe { mlua::ffi::luau_codegen_supported() != 0 }
}

/// table.find fast path: holes, start index bounds, identity, __eq, and a frozen 191232-case hash
/// recorded on the unpatched VM. Runs once interpreted and once with JIT (native codegen) enabled
/// when the CPU supports it.
#[test]
fn table_find_fast_path() -> Result<()> {
    for jit in [false, true] {
        let lua = Lua::new();
        lua.enable_jit(jit);
        lua.load(TABLE_FIND).set_name("table_find").exec()?;
    }
    Ok(())
}

/// Lune's `Luau.load` with `environment` and `injectGlobals = false`: a fresh environment table
/// carrying the custom environment's metatable and raw fields. JIT is off because the environment
/// changed (as in Lune).
fn fresh_environment(lua: &Lua, custom: &Table) -> Result<Table> {
    let environment = lua.create_table()?;
    environment.set_metatable(custom.metatable())?;
    for pair in custom.pairs::<Value, Value>() {
        let (key, value) = pair?;
        environment.raw_set(key, value)?;
    }
    Ok(environment)
}

fn debug_name(options: &Table) -> Result<String> {
    Ok(options.get::<Option<String>>("debugName")?.unwrap_or_else(|| "luau.load(...)".into()))
}

/// Global names reachable from `table` and its `__index` chain (tools/native-host/lune.patch).
fn collect_names(table: &Table, names: &mut std::collections::BTreeSet<String>, depth: usize) -> Result<()> {
    if depth > 16 {
        return Err(mlua::Error::runtime("environment chain too deep"));
    }
    for pair in table.pairs::<Value, Value>() {
        if let (Value::String(key), _) = pair? {
            names.insert(key.to_str()?.to_string());
        }
    }
    if let Some(meta) = table.metatable() {
        match meta.raw_get::<Value>("__index")? {
            Value::Nil => (),
            Value::Table(parent) => collect_names(&parent, names, depth + 1)?,
            _ => return Err(mlua::Error::runtime("native environment requires table __index")),
        }
    }
    Ok(())
}

/// Installs the host globals native_builtins.luau used from Lune: Luau.compile / Luau.load /
/// Luau.loadNative (mirroring load_native in tools/native-host/lune.patch), Serde.encode('json')
/// and Process.env.RUR_REQUIRE_NATIVE.
fn install_host(lua: &Lua) -> Result<()> {
    let luau = lua.create_table()?;
    luau.set(
        "compile",
        lua.create_function(|lua, (source, options): (mlua::String, Option<Table>)| {
            let level = match &options {
                Some(options) => options.get::<Option<u8>>("optimizationLevel")?.unwrap_or(1),
                None => 1,
            };
            let bytes = Compiler::new().set_optimization_level(level).compile(source.as_bytes())?;
            lua.create_string(bytes)
        })?,
    )?;
    luau.set(
        "load",
        lua.create_function(|lua, (bytecode, options): (mlua::String, Table)| -> Result<Function> {
            let custom: Table = options.get("environment")?;
            assert!(!options.get::<Option<bool>>("injectGlobals")?.unwrap_or(true), "harness supports injectGlobals=false only");
            let environment = fresh_environment(lua, &custom)?;
            lua.enable_jit(false);
            lua.load(bytecode.as_bytes().to_vec())
                .set_name(debug_name(&options)?)
                .set_environment(environment)
                .into_function()
        })?,
    )?;
    if codegen_supported() {
        luau.set(
            "loadNative",
            lua.create_function(|lua, (source, options): (mlua::String, Table)| -> Result<Function> {
                if options.get::<Option<bool>>("injectGlobals")?.unwrap_or(true) {
                    return Err(mlua::Error::runtime("native load requires injectGlobals=false"));
                }
                let custom: Table = options.get("environment")?;
                let environment = fresh_environment(lua, &custom)?;
                let mut names = std::collections::BTreeSet::new();
                collect_names(&environment, &mut names, 0)?;
                collect_names(&lua.globals(), &mut names, 0)?;
                let bytes = Compiler::new()
                    .set_optimization_level(2)
                    .set_mutable_globals(names)
                    .compile(source.as_bytes())?;
                environment.set_safeenv(true);
                let previous = lua.globals();
                unsafe {
                    lua.exec_raw::<()>((environment.clone(),), |state| {
                        mlua::ffi::lua_replace(state, mlua::ffi::LUA_GLOBALSINDEX);
                    })?;
                }
                lua.enable_jit(true);
                let loaded = lua
                    .load(bytes)
                    .set_name(debug_name(&options)?)
                    .set_environment(environment)
                    .into_function();
                unsafe {
                    lua.exec_raw::<()>((previous,), |state| {
                        mlua::ffi::lua_replace(state, mlua::ffi::LUA_GLOBALSINDEX);
                    })?;
                }
                lua.enable_jit(false);
                loaded
            })?,
        )?;
    }
    lua.globals().set("Luau", luau)?;

    let serde = lua.create_table()?;
    serde.set(
        "encode",
        lua.create_function(|lua, (format, value): (String, Value)| {
            assert_eq!(format, "json", "harness supports json only");
            let json: serde_json::Value = lua.from_value(value)?;
            Ok(serde_json::to_string(&json).map_err(mlua::Error::external)?)
        })?,
    )?;
    lua.globals().set("Serde", serde)?;

    let env = lua.create_table()?;
    if let Ok(value) = std::env::var("RUR_REQUIRE_NATIVE") {
        env.set("RUR_REQUIRE_NATIVE", value)?;
    }
    let process = lua.create_table()?;
    process.set("env", env)?;
    lua.globals().set("Process", process)?;
    Ok(())
}

/// Guarded builtin fast paths (math.floor, string.byte, table.insert) under replaced globals,
/// getfenv/setfenv, captured aliases, argument-order mutation, arity/coercion, exact errors and
/// allocation stress. Every check asserts its expected value in the O0 interpreter; when native
/// codegen is supported, the same checks run on O2 native code (safeenv, JIT) and the full JSON log
/// of values and error messages must match the interpreter's byte for byte. Without codegen support
/// (Luau.loadNative is nil) only the interpreted run and its assertions execute.
#[test]
fn native_builtins_guarded_fast_paths() -> Result<()> {
    let lua = Lua::new();
    install_host(&lua)?;
    if !codegen_supported() {
        eprintln!("luau_vm: native codegen unsupported on this build/CPU; interpreter-only comparison");
    }
    lua.load(NATIVE_BUILTINS).set_name("native_builtins").exec()
}

/// The interpreter runs a Luau iterator of a generic for as one of its frames (LUA_CALLINFO_FORGLOOP), so the iterator can
/// yield its coroutine as on Roblox; the stock VM raised "attempt to yield across metamethod/C-call boundary". Native code
/// keeps the stock C call. `const Name = value` parses as a local, as Roblox's Luau does.
#[test]
fn an_interpreted_iterator_yields_and_const_declares_a_local() -> Result<()> {
    let lua = Lua::new();
    lua.enable_jit(false);
    let result: String = lua
        .load(
            r#"
            const Limit = 3
            local function Count(State, Index)
                coroutine.yield("pause")
                if Index < State then
                    return Index + 1, Index * 2
                end
                return nil
            end
            local Thread = coroutine.wrap(function()
                local Seen = {}
                for Index, Double in Count, Limit, 0 do
                    table.insert(Seen, `{Index}/{Double}`)
                end
                return table.concat(Seen, " ")
            end)
            local Pauses = 0
            local Value = Thread()
            while Value == "pause" do
                Pauses += 1
                Value = Thread()
            end
            local const = 2
            return `{Value} {Pauses} {const}`
            "#,
        )
        .set_name("iterator_yield")
        .eval()?;
    assert_eq!(result, "1/0 2/2 3/4 4 2");
    Ok(())
}

/// `math.isnan`, `math.isinf` and `math.isfinite`, Luau's number classifiers (0.701; upstream's own since the 0.746 bump): the
/// results, the strings `luaL_checknumber` takes, the errors, one result, and a constant argument folded by the compiler, with the
/// same answers interpreted, natively compiled and through a plain C call (a table's field, never a fast call).
#[test]
fn math_number_classifiers_answer_as_luau_does() -> Result<()> {
    for jit in [false, true] {
        let lua = Lua::new();
        lua.enable_jit(jit && codegen_supported());
        let result: String = lua
            .load(
                r##"
                local K = { Zero = 0, One = 1, Huge = math.huge }
                local Field = { isnan = math.isnan, isinf = math.isinf, isfinite = math.isfinite }
                local Out = {}
                for _, Value in { K.Zero, -K.Zero, K.One, 1e308 * K.One, 5e-324 * K.One, K.Huge, -K.Huge, K.Zero / K.Zero, -(K.Zero / K.Zero), 1e308 * 10 * K.One } do
                    local Fast = `{math.isnan(Value)} {math.isinf(Value)} {math.isfinite(Value)}`
                    assert(Fast == `{Field.isnan(Value)} {Field.isinf(Value)} {Field.isfinite(Value)}`, "a C call answers as the fast call")
                    table.insert(Out, Fast)
                end
                for _, Text in { "1", " 1 ", "0x10", "1e400", "inf", "nan" } do
                    table.insert(Out, `{math.isnan(Text)} {math.isinf(Text)} {math.isfinite(Text)}`)
                end
                assert(math.isnan(0 / 0) and not math.isnan(0) and math.isinf(math.huge) and not math.isinf(-4) and math.isfinite(42) and not math.isfinite(-math.huge))
                assert(select("#", math.isnan(1)) == 1 and select("#", math.isfinite(1, 2, 3)) == 1)
                local function Error(Function, ...)
                    local Ok, Message = pcall(Function, ...)
                    assert(not Ok)
                    return Message
                end
                table.insert(Out, Error(math.isnan, nil))
                table.insert(Out, Error(math.isinf))
                table.insert(Out, Error(math.isfinite, "abc"))
                table.insert(Out, Error(math.isnan, {}))
                return table.concat(Out, "|")
                "##,
            )
            .set_name("classifiers")
            .eval()?;
        assert_eq!(
            result,
            "false false true|false false true|false false true|false false true|false false true|false true false|false true false|\
             true false false|true false false|false true false|\
             false false true|false false true|false false true|false true false|false true false|true false false|\
             invalid argument #1 to 'isnan' (number expected, got nil)|missing argument #1 to 'isinf' (number expected)|\
             invalid argument #1 to 'isfinite' (number expected, got string)|invalid argument #1 to 'isnan' (number expected, got table)",
            "jit {jit}"
        );
    }
    Ok(())
}

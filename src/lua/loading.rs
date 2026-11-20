//! `loadstring` and `debug.loadmodule`: Luau a script compiles while it runs. Measured in Studio 0.747 (a headless play
//! session's client and server, each capture taken twice: tools/parity/probes/loadstring.luau and loadmodule.luau).
//!
//! `loadstring(source, chunkname?)`:
//! - A client has none, and a server only where ServerScriptService.LoadStringEnabled is on: the call raises
//!   `loadstring() is not available`, whatever its arguments. (A server whose LoadStringEnabled is off was not measured:
//!   Roblox's documentation gives it the same answer.)
//! - The chunk's name is the second argument, else the source itself, as Luau reads a chunk name: `=Name` and `@Name`
//!   are `Name`, anything else `[string "<its first line>"]`.
//! - The function runs in the calling script's environment (`getfenv(f)` is `getfenv(1)` of the caller, a global it
//!   assigns is the script's), and `setfenv` replaces that for this function alone.
//! - A source that does not compile answers `nil, message`, the message under the chunk's name.
//! - A number is taken as its text; no argument and other types raise Luau's argument errors.
//! - The text ends at its first NUL byte, as a C string does.
//!
//! `debug.loadmodule(module)`:
//! - It raises `debug.loadmodule is not enabled.` unless FFlagEnableLoadModule is on, which a live client's and a plain
//!   Studio's is not ([`crate::Options::load_module`]).
//! - Anything but one ModuleScript raises `Attempted to call loadmodule with invalid argument(s).` (a Script, a
//!   LocalScript and a second argument included).
//! - It answers `function, nil, cleanup`: the module's current Source compiled as `require` compiles it (named by the
//!   module's full name when it is loaded, `script` the module) and not run. Each load is a function and an environment
//!   of its own, each call runs the source again, and `require` of the same module neither sees nor changes them.
//! - A source that does not compile answers `nil, message`: two values.
//!
//! Not measured: what the cleanup function releases (it answers nothing, and the function runs after it).

use std::rc::Rc;

use mlua::prelude::*;

use super::{Args, Ret, modules, raise_plain, values};
use crate::{Engine, dom::flags, scheduler, value::Value};

/// Whether this run's scripts have `loadstring`: a server's, with ServerScriptService.LoadStringEnabled on in the place
/// or by [`crate::Options::load_string`].
fn load_string_enabled(engine: &Engine) -> bool {
    if !engine.options.server() {
        return false;
    }
    if engine.options.load_string {
        return true;
    }
    let Some(service) = crate::services::find_service(engine, engine.world().game, "ServerScriptService") else { return false };
    let Some(property) = engine.reflection.name_id("LoadStringEnabled") else { return false };
    matches!(engine.state.borrow().dom.raw_get(service, property), Some(Value::Bool(true)))
}

/// The environment of the script that called the engine function now running: the nearest Luau function up the stack
/// that is not the engine's own (the prelude's wrapper around this function). Roblox gives a loaded chunk the calling
/// thread's globals, which there are the script's environment (as the VM's getfenv answers them:
/// patches/luau/18-script-thread-globals.patch).
fn caller_environment(lua: &Lua, engine: &Engine) -> Option<LuaTable> {
    caller_function(lua, engine)?.environment()
}

fn caller_function(lua: &Lua, engine: &Engine) -> Option<LuaFunction> {
    let mut level = 1;
    loop {
        let function = lua.inspect_stack(level, |debug| debug.function())?;
        if is_script(engine, &function) {
            return Some(function);
        }
        level += 1;
    }
}

/// Whether `function` is a script's Luau: not a C function, and not the engine's own Luau, which runs in the VM's globals
/// (the prelude) or in the table scripts' environments read through (services.luau, server.luau).
fn is_script(engine: &Engine, function: &LuaFunction) -> bool {
    let Some(env) = function.environment() else { return false };
    let env = env.to_pointer();
    let prelude = engine.prelude().wrap.environment().map(|env| env.to_pointer());
    let shared = engine.lua.borrow().shared.as_ref().map(|shared| shared.to_pointer());
    Some(env) != prelude && Some(env) != shared
}

/// An argument Luau's `luaL_checklstring` takes: a string, or a number as its text.
fn text(lua: &Lua, value: &LuaValue) -> Option<Vec<u8>> {
    match value {
        LuaValue::String(text) => Some(text.as_bytes().to_vec()),
        LuaValue::Integer(_) | LuaValue::Number(_) => lua.coerce_string(value.clone()).ok().flatten().map(|text| text.as_bytes().to_vec()),
        _ => None,
    }
}

fn until_nul(mut bytes: Vec<u8>) -> Vec<u8> {
    if let Some(end) = bytes.iter().position(|&byte| byte == 0) {
        bytes.truncate(end);
    }
    bytes
}

pub fn loadstring(lua: &Lua, engine: &Rc<Engine>, args: LuaMultiValue) -> Ret {
    if !load_string_enabled(engine) {
        return raise_plain("loadstring() is not available");
    }
    let count = args.len();
    let args = Args::new(args);
    if count == 0 {
        return raise_plain("missing argument #1 to 'loadstring' (string expected)");
    }
    let Some(source) = text(lua, args.get(0)) else {
        return raise_plain(format!("invalid argument #1 to 'loadstring' (string expected, got {})", args.get(0).type_name()));
    };
    let source = until_nul(source);
    let name = match args.get(1) {
        LuaValue::Nil => source.clone(),
        value => match text(lua, value) {
            Some(name) => until_nul(name),
            None => return raise_plain(format!("invalid argument #2 to 'loadstring' (string expected, got {})", value.type_name())),
        },
    };
    let env = match caller_environment(lua, engine) {
        Some(env) => env,
        None => lua.globals(),
    };
    // (text whatever its first byte is: a chunk of bytecode is not loaded, Luau's lexer refuses it as text)
    let chunk = lua
        .load(source.as_slice())
        .set_mode(mlua::chunk::ChunkMode::Text)
        .set_compiler(modules::compiler())
        .set_name(String::from_utf8_lossy(&name))
        .set_environment(env);
    match modules::load(lua, engine, chunk) {
        Ok(function) => Ok(values([LuaValue::Function(function)])),
        Err(error) => Ok(values([LuaValue::Nil, LuaValue::String(lua.create_string(scheduler::error_message(&error))?)])),
    }
}

pub fn loadmodule(lua: &Lua, engine: &Rc<Engine>, args: LuaMultiValue) -> Ret {
    if !engine.options.load_module {
        return raise_plain("debug.loadmodule is not enabled.");
    }
    let count = args.len();
    let args = Args::new(args);
    let module = args.instance(0).filter(|&module| {
        count == 1 && Some(engine.state.borrow().dom.at(module).class) == engine.class_id("ModuleScript")
    });
    let Some(module) = module else {
        return raise_plain("Attempted to call loadmodule with invalid argument(s).");
    };
    let (studio_only, json, path) = {
        let state = engine.state.borrow();
        let instance = state.dom.at(module);
        (instance.has(flags::STUDIO_ONLY), instance.has(flags::JSON_MODULE), instance.source_path().cloned())
    };
    if let Some(path) = &path {
        modules::note_required(engine, path);
    }
    // A JSON module (Rojo `.json`, which Studio holds as a ModuleScript returning the table): each call decodes the file
    // again, as running that module's source would build the table again
    if let (true, Some(path)) = (json, &path) {
        let name = engine.state.borrow().dom.full_name(&engine.reflection, module);
        if let Err(message) = modules::decode_json(lua, engine, path) {
            return Ok(values([LuaValue::Nil, LuaValue::String(lua.create_string(format!("{name}: invalid JSON module ({message})"))?)]));
        }
        let (e, path) = (engine.clone(), path.clone());
        let decode = lua.create_function(move |lua, ()| modules::decode_json(lua, &e, &path).map_err(LuaError::runtime))?;
        let env = modules::environment(lua, engine, module)?;
        let chunk = lua
            .load("local Decode = ...
return function()
	return Decode()
end")
            .set_compiler(modules::compiler())
            .set_name(format!("={name}"))
            .set_environment(env);
        let function: LuaFunction = modules::load(lua, engine, chunk)?.call(decode)?;
        let cleanup = lua.create_function(|_, _: LuaMultiValue| Ok(()))?;
        return Ok(values([LuaValue::Function(function), LuaValue::Nil, LuaValue::Function(cleanup)]));
    }
    let source = match modules::source_of(engine, module) {
        Some(source) => source,
        // (a module the project has no source for: an empty function would pass for its code)
        None if studio_only => {
            let name = engine.state.borrow().dom.full_name(&engine.reflection, module);
            let message = format!("Module {name} exists only in Studio (its source is not in the project)");
            return Ok(values([LuaValue::Nil, LuaValue::String(lua.create_string(message)?)]));
        }
        None => Vec::new(),
    };
    match modules::compile(lua, engine, module, &source) {
        Ok(function) => {
            let cleanup = lua.create_function(|_, _: LuaMultiValue| Ok(()))?;
            Ok(values([LuaValue::Function(function), LuaValue::Nil, LuaValue::Function(cleanup)]))
        }
        Err(error) => Ok(values([LuaValue::Nil, LuaValue::String(lua.create_string(scheduler::error_message(&error))?)])),
    }
}

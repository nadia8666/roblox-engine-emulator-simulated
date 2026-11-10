//! ModuleScript loading (port of `runtime/engine/modules.luau`): instance requires, Roblox
//! require-by-string (./, ../, @self, @game, @<Service>), per-script environments, Roblox-style
//! caching and cyclic-require errors. The chunk itself runs in the prelude's `Require` (so a module
//! may yield while loading); these are its Rust halves.

use std::rc::Rc;

use mlua::{chunk::Compiler, prelude::*};
use rustc_hash::FxHashMap;

use super::{instance::defer_thread, instance::materialize, instance_value, raw::instance_of_ud};
use crate::{Engine, InstanceId, K, NameId, dom::flags, value::Value};

#[derive(PartialEq, Eq)]
enum Status {
    Loading,
    Done,
    Error,
}

struct Record {
    status: Status,
    result: Option<LuaValue>,
    waiting: Vec<LuaThread>,
    owner: Option<LuaThread>,
}

/// The VM a script runs in: None for the main VM, else the key of its Actor ([`vm_key`]). An Actor runs its scripts in a VM of its own in
/// Studio: its own `_G` and its own copy of every module it requires.
pub type Vm = Option<u64>;

/// What the prelude passes for an Actor's VM: its instance id as a number.
pub fn vm_key(actor: InstanceId) -> u64 {
    (u64::from(actor.generation) << 32) | u64::from(actor.index)
}

#[derive(Default)]
pub struct ModuleRecords {
    records: FxHashMap<(Vm, InstanceId), Record>,
    /// The scripts under Actors that started (actors.rs)
    started: rustc_hash::FxHashSet<InstanceId>,
    /// The parallel phases running (actors.rs)
    pub parallel: super::actors::Parallel,
    /// Source files of the modules required so far (`--dependencies`: what a run read)
    files: std::collections::BTreeSet<String>,
    /// Every source file read so far (required modules', JSON modules', Source reads)
    read: rustc_hash::FxHashSet<String>,
    /// Sources (and bytecode) read ahead on other threads ([`Prefetch`])
    prefetch: Option<std::sync::Arc<Prefetch>>,
    /// Bytecode of a module's prefetched source, until its chunk is loaded
    bytecode: FxHashMap<InstanceId, Vec<u8>>,
    /// Deferred native compilation ([`defer_codegen`]): module chunks loaded as bytecode only,
    /// compiled to native code later by [`compile_deferred`]
    deferred: Option<std::collections::VecDeque<LuaFunction>>,
    /// Module chunks load as bytecode only and stay interpreted ([`interpret_modules`])
    interpreted: bool,
    /// Every chunk of the VM runs in the interpreter, the engine's own included ([`interpret_all`]):
    /// loading a module leaves native code generation off
    interpret_all: bool,
}

impl ModuleRecords {
    /// Whether an Actor's script has started (actors.rs).
    pub fn started(&self, script: InstanceId) -> bool {
        self.started.contains(&script)
    }

    pub fn mark_started(&mut self, script: InstanceId) {
        self.started.insert(script);
    }

    /// A script got another source: the prefetched bytecode of the old one is stale.
    pub fn forget_bytecode(&mut self, script: InstanceId) {
        self.bytecode.remove(&script);
    }
}

/// Module sources read ahead: the native app's engine worker starts one with the files the story
/// required the last time it was mounted (see `runtime.rs`), so their reads and their compilation
/// to bytecode (`luau_compile`, the same compiler options, no Lua state involved) run on other
/// threads while the engine starts and mounts. A require takes a file's bytes from here when they
/// are ready (else it reads the file as before); the bytecode is used only for the unpatched
/// source it was compiled from and a successful compile, so a chunk loads exactly as if it had
/// been compiled on the spot (compile errors take the ordinary path).
pub struct Prefetch {
    ready: std::sync::Mutex<std::collections::HashMap<String, (Vec<u8>, Option<Vec<u8>>)>>,
}

impl Prefetch {
    pub fn start(paths: Vec<String>) -> std::sync::Arc<Prefetch> {
        let prefetch = std::sync::Arc::new(Prefetch { ready: std::sync::Mutex::new(std::collections::HashMap::new()) });
        let shared = prefetch.clone();
        let _ = std::thread::Builder::new().name("rur-prefetch".into()).spawn(move || {
            use rayon::prelude::*;
            let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(4).thread_name(|_| "rur-prefetch".into()).build() else { return };
            pool.install(|| {
                paths.par_iter().for_each(|path| {
                    let Ok(source) = std::fs::read(path) else { return };
                    let lua_source = path.ends_with(".lua") || path.ends_with(".luau");
                    let bytecode = if lua_source { compiler().compile(&source).ok() } else { None };
                    if let Ok(mut ready) = shared.ready.lock() {
                        ready.insert(path.clone(), (source, bytecode));
                    }
                });
            });
        });
        prefetch
    }

    fn take(&self, path: &str) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        self.ready.lock().ok()?.remove(path)
    }
}

/// Uses `prefetch` for this engine's requires.
pub fn use_prefetch(engine: &Engine, prefetch: std::sync::Arc<Prefetch>) {
    engine.modules.borrow_mut().prefetch = Some(prefetch);
}

/// A file's bytes: prefetched when they are ready, from the process's compiled sources when it
/// keeps them ([`reuse_compiled_modules`]), else read now.
fn read_file(engine: &Engine, path: &str) -> std::io::Result<(Vec<u8>, Option<Vec<u8>>)> {
    let prefetch = engine.modules.borrow().prefetch.clone();
    if let Some(taken) = prefetch.and_then(|prefetch| prefetch.take(path)) {
        return Ok(taken);
    }
    if let Some(compiled) = COMPILED.get() {
        let listed = engine.state.borrow().project.stamps.get(path).copied();
        return compiled.read(path, listed);
    }
    std::fs::read(path).map(|bytes| (bytes, None))
}

/// Module sources and their bytecode kept by a process that runs many VMs (the render service,
/// serve.rs): a file whose size and modification time are unchanged is neither read nor compiled
/// again. The bytecode is the compile a require makes (as [`Prefetch`]'s), so a chunk loads exactly
/// as if it had been compiled on the spot.
struct CompiledSources {
    entries: std::sync::Mutex<std::collections::HashMap<String, CompiledSource>>,
}

/// (stamp, source and bytecode, whether a lasting thread made it: see [`CompiledSources::read`])
type CompiledSource = (crate::files::Stamp, std::sync::Arc<(Vec<u8>, Option<Vec<u8>>)>, bool);

thread_local! {
    /// A thread that ends with its job ([`precompile`]'s): what it keeps sits in a heap that outlives
    /// it, so a lasting thread copies it to its own on first use
    static TRANSIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A kept source and bytecode, copied to this thread's long-lived heap (crate::alloc).
fn kept(source: &[u8], bytecode: Option<&[u8]>) -> std::sync::Arc<(Vec<u8>, Option<Vec<u8>>)> {
    crate::alloc::long_lived(|| std::sync::Arc::new((source.to_vec(), bytecode.map(<[u8]>::to_vec))))
}

static COMPILED: std::sync::OnceLock<CompiledSources> = std::sync::OnceLock::new();

impl CompiledSources {
    /// `path`'s source and bytecode; `listed` is its stamp when the caller knows it already (the
    /// project listing's).
    fn read(&self, path: &str, listed: Option<crate::files::Stamp>) -> std::io::Result<(Vec<u8>, Option<Vec<u8>>)> {
        let stamp = match listed {
            Some(stamp) => stamp,
            None => crate::files::stamp(std::path::Path::new(path)).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?,
        };
        let lasting = !TRANSIENT.with(std::cell::Cell::get);
        let known = self.entries.lock().ok().and_then(|entries| entries.get(path).cloned());
        if let Some((known_stamp, entry, home)) = known {
            if known_stamp == stamp {
                crate::profiler::caches::hit(crate::profiler::caches::Cache::Bytecode);
                if lasting && !home {
                    let copy = kept(&entry.0, entry.1.as_deref());
                    if let Ok(mut entries) = self.entries.lock() {
                        crate::alloc::long_lived(|| entries.insert(path.to_string(), (stamp, copy, true)));
                    }
                }
                return Ok((entry.0.clone(), entry.1.clone()));
            }
        }
        crate::profiler::caches::miss(crate::profiler::caches::Cache::Bytecode);
        let source = std::fs::read(path)?;
        let lua_source = path.ends_with(".lua") || path.ends_with(".luau");
        let bytecode = if lua_source { compiler().compile(&source).ok() } else { None };
        // (kept across runs: copied to the long-lived heap, away from the compiler's garbage)
        let entry = kept(&source, bytecode.as_deref());
        if let Ok(mut entries) = self.entries.lock() {
            crate::alloc::long_lived(|| entries.insert(path.to_string(), (stamp, entry.clone(), lasting)));
        }
        Ok((source, bytecode))
    }
}

/// For a process that runs many VMs one after another (the render service): module sources and
/// bytecode are kept between VMs ([`CompiledSources`]), and engines created afterwards load chunks
/// without compiling them to native code (`luau_codegen_compile` costs a one-shot story more than
/// it gains; the interpreter gives the same results).
pub fn reuse_compiled_modules() {
    let _ = COMPILED.set(CompiledSources { entries: std::sync::Mutex::new(Default::default()) });
}

/// Reads and compiles `paths` into the process's compiled sources on other threads (those not
/// kept yet), while the caller goes on: what a run is expected to require.
pub fn precompile(paths: Vec<String>) {
    let Some(compiled) = COMPILED.get() else { return };
    let missing: Vec<String> = {
        let Ok(entries) = compiled.entries.lock() else { return };
        paths.into_iter().filter(|path| !entries.contains_key(path)).collect()
    };
    if missing.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("ree-precompile".into()).spawn(move || {
        use rayon::prelude::*;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).thread_name(|_| "ree-precompile".into()).start_handler(|_| TRANSIENT.with(|transient| transient.set(true)));
        let Ok(pool) = pool.build() else { return };
        pool.install(|| missing.par_iter().for_each(|path| drop(compiled.read(path, None))));
    });
}

/// [`precompile`] on the thread pool, returning when every file is read and compiled (`ree serve
/// --warm`): how many were read now.
pub fn precompile_now(paths: &[String]) -> usize {
    use rayon::prelude::*;
    let Some(compiled) = COMPILED.get() else { return 0 };
    let missing: Vec<&String> = {
        let Ok(entries) = compiled.entries.lock() else { return 0 };
        paths.iter().filter(|path| !entries.contains_key(*path)).collect()
    };
    missing.par_iter().filter(|path| compiled.read(path, None).is_ok()).count()
}

/// Drops the kept sources and bytecode of module files that changed or are gone (a changed file is
/// read and compiled again on its next require; its old entry would be kept for nothing).
pub fn forget_stale_modules() {
    let Some(compiled) = COMPILED.get() else { return };
    let Ok(mut entries) = compiled.entries.lock() else { return };
    let before = entries.len();
    entries.retain(|path, (stamp, _, _)| crate::files::stamp(std::path::Path::new(path)) == Some(*stamp));
    crate::profiler::caches::evicted(crate::profiler::caches::Cache::Bytecode, (before - entries.len()) as u64);
}

/// Whether [`reuse_compiled_modules`] is on.
pub fn reusing_compiled_modules() -> bool {
    COMPILED.get().is_some()
}

/// Defers native compilation of module chunks (the native app's engine worker, which compiles
/// them while it waits for commands): loading a chunk then skips `luau_codegen_compile`, the
/// largest part of a story's first mount, and its functions run in the interpreter until
/// [`compile_deferred`] compiles the chunk (native and interpreted execution behave the same;
/// functions entered later run natively).
pub fn defer_codegen(engine: &Engine) {
    engine.modules.borrow_mut().deferred.get_or_insert_with(Default::default);
}

/// Loads this engine's module chunks without native code generation, for good (`ree record` /
/// `ree run`: compiling a game's hundreds of modules to native code cost a recording ~1.2 s of its
/// ~1.5 s module loading and its frames ran no faster; the interpreter gives the same results).
pub fn interpret_modules(engine: &Engine) {
    engine.modules.borrow_mut().interpreted = true;
}

/// Loads every chunk of this VM, from now on, without native code generation: the engine's prelude,
/// services and Vector3 as well as module chunks (a render service makes a VM per render: compiling
/// those to native code at every VM start was most of the start; the interpreter gives the same
/// results). Call before the project mounts.
pub fn interpret_all(lua: &Lua, engine: &Engine) {
    {
        let mut modules = engine.modules.borrow_mut();
        modules.interpreted = true;
        modules.interpret_all = true;
    }
    crate::scheduler::start_cold_tracking(lua, engine);
    #[cfg(feature = "jit")]
    lua.enable_jit(false);
    #[cfg(not(feature = "jit"))]
    let _ = lua;
}

/// Whether [`interpret_all`] is on for this engine's VM
pub fn interprets_all(engine: &Engine) -> bool {
    engine.modules.borrow().interpret_all
}

/// Whether this engine's module chunks stay interpreted ([`interpret_modules`] or [`interpret_all`]): their Luau is what the work
/// clock counts and charges (a natively compiled chunk's instructions are not counted)
pub fn interprets_scripts(engine: &Engine) -> bool {
    engine.modules.borrow().interpreted
}

/// Compiles the oldest deferred chunk to native code; returns whether more are waiting.
pub fn compile_deferred(lua: &Lua, engine: &Engine) -> bool {
    let next = engine.modules.borrow_mut().deferred.as_mut().and_then(|queue| queue.pop_front());
    // (without feature `jit` there is no Luau native code generation: the VM interprets)
    #[cfg(not(feature = "jit"))]
    let _ = (lua, next);
    #[cfg(feature = "jit")]
    if let Some(function) = next {
        if unsafe { mlua::ffi::luau_codegen_supported() } != 0 {
            let _ = unsafe { lua.exec_raw::<()>(&function, |state| mlua::ffi::luau_codegen_compile(state, -1)) };
        }
    }
    engine.modules.borrow().deferred.as_ref().is_some_and(|queue| !queue.is_empty())
}

/// Every source file this engine has read so far (modules required, JSON modules, Source reads).
pub fn read_files(engine: &Engine) -> rustc_hash::FxHashSet<String> {
    engine.modules.borrow().read.clone()
}

/// Notes that the engine read source file `path` ([`read_files`]; one that is no project listing's
/// is noted for a prepared run as well: crate::files::note).
fn note_read(engine: &Engine, path: &str) {
    if !engine.state.borrow().project.stamps.contains_key(path) {
        crate::files::note(std::path::Path::new(path));
    }
    engine.modules.borrow_mut().read.insert(path.to_string());
}

/// The source files of every module required so far, sorted
pub fn required_files(engine: &Engine) -> Vec<String> {
    engine.modules.borrow().files.iter().cloned().collect()
}

/// Roblox compiles scripts at optimization level 2 (a UI library detects O2 through debug.info).
/// Scripts see the engine's `typeof` (Vector3 for native vectors, registered table types), so
/// the compiler must not fold or fast-call the builtin.
/// Vector3.new compiles as the VM's vector constructor (a fast call), as Roblox compiles it.
pub fn compiler() -> Compiler {
    // (typeof is a builtin, as in Roblox, whose client runs a script's `typeof(x)` as a fast call: the VM's builtin names ree's
    // values as ree's typeof does, raw.rs typeof_function; measured in tools/cost-parity's env rows. The compiler leaves the
    // typeof of a constant vector to the VM, which names it Vector3, rather than folding it to "vector": Luau has done so
    // since LuauCompileTypeofFold's behaviour became its only one)
    Compiler::new().set_optimization_level(2).set_vector_ctor("Vector3.new").set_vector_type("Vector3")
}

/// The engine's own Luau (the prelude, services, vector3.luau), whose `Vector3` is Lune's.
pub fn engine_compiler() -> Compiler {
    Compiler::new().set_optimization_level(2).set_disabled_builtins(["typeof"])
}

/// Determinism patch for vide's `values()` (modules.luau DeterministicSource): its output order
/// followed table iteration over address-keyed scopes; scopes are put in creation order.
pub fn deterministic_source<'a>(chunk_name: &str, source: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
    if !chunk_name.ends_with(".vide.values") {
        return source.into();
    }
    let find = |haystack: &[u8], needle: &[u8], from: usize| -> Option<usize> {
        haystack.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
    };
    let looped = b"for _, scope in scopes do";
    let create = b"scopes[v] = {";
    let (Some(loop_at), Some(create_at)) = (find(source, looped, 0), find(source, create, 0)) else {
        return source.into();
    };
    if find(source, looped, loop_at + 1).is_some() || find(source, create, create_at + 1).is_some() {
        return source.into();
    }
    let sorted: &[u8] = b"local __rur_sorted = {} for _, __rur_scope in scopes do table.insert(__rur_sorted, __rur_scope) end table.sort(__rur_sorted, function(a, b) return a.__rur_order < b.__rur_order end) for _, scope in __rur_sorted do";
    let mut patched = Vec::with_capacity(source.len() + 400);
    patched.extend_from_slice(&source[..loop_at]);
    patched.extend_from_slice(sorted);
    patched.extend_from_slice(&source[loop_at + looped.len()..]);
    let create_at = find(&patched, create, 0).unwrap();
    let mut result = Vec::with_capacity(patched.len() + 200);
    result.extend_from_slice(b"local __rur_order_count = 0 local function __rur_next_order() __rur_order_count += 1 return __rur_order_count end ");
    result.extend_from_slice(&patched[..create_at]);
    result.extend_from_slice(b"scopes[v] = { __rur_order = __rur_next_order(),");
    result.extend_from_slice(&patched[create_at + create.len()..]);
    result.into()
}

/// The environment a script runs in (globals.luau Environment).
pub fn environment(lua: &Lua, engine: &Rc<Engine>, script: InstanceId) -> LuaResult<LuaTable> {
    environment_in(lua, engine, script, None)
}

/// [`environment`] for a script of the VM `vm`: its `_G` and its `require` are that VM's.
pub fn environment_in(lua: &Lua, engine: &Rc<Engine>, script: InstanceId, vm: Vm) -> LuaResult<LuaTable> {
    let (shared, make) = {
        let refs = engine.lua.borrow();
        let prelude = refs.prelude.clone().expect("prelude");
        (refs.shared.clone().expect("shared globals"), prelude.exports.get::<LuaFunction>("Environment")?)
    };
    let env: LuaTable = make.call((shared, instance_value(lua, engine, script)?, vm))?;
    // An interpreted script's environment is safe, as Roblox makes every script's: builtins take the VM's
    // fast calls and ipairs / pairs loops run inline (getfenv / setfenv end it, as there). Native code would
    // inline them with its own arithmetic (vector.magnitude in doubles), which Studio's interpreted scripts
    // do not, so a VM that compiles modules natively keeps the environment unsafe.
    if engine.modules.borrow().interpreted || cfg!(not(feature = "jit")) {
        unsafe { lua.exec_raw::<()>(&env, |state| mlua::ffi::lua_setsafeenv(state, -1, 1))? };
    }
    Ok(env)
}

/// Modules.Compile: the chunk of a script's source in its own environment, named by its full name.
pub fn compile(lua: &Lua, engine: &Rc<Engine>, script: InstanceId, source: &[u8]) -> LuaResult<LuaFunction> {
    compile_in(lua, engine, script, source, None)
}

/// [`compile`] for a script of the VM `vm`.
pub fn compile_in(lua: &Lua, engine: &Rc<Engine>, script: InstanceId, source: &[u8], vm: Vm) -> LuaResult<LuaFunction> {
    let env = environment_in(lua, engine, script, vm)?;
    let name = engine.state.borrow().dom.full_name(&engine.reflection, script);
    // (a profile's annotated listing reads the chunk's source: profiler.rs)
    if crate::profiler::active() {
        let path = engine.state.borrow().dom.at(script).source_path().map(|path| path.to_string());
        crate::profiler::source_loaded(&name, path.as_deref(), &std::rc::Rc::from(source));
    }
    let source = deterministic_source(&name, source);
    let bytecode = engine.modules.borrow_mut().bytecode.remove(&script);
    let chunk = match (bytecode, &source) {
        // (the prefetcher's compile of this very source: loads as the text would)
        (Some(bytecode), std::borrow::Cow::Borrowed(_)) => lua.load(bytecode).set_mode(mlua::chunk::ChunkMode::Binary),
        _ => lua.load(&*source).set_compiler(compiler()),
    };
    // A script's chunk is its full name as Roblox names it: error positions, tracebacks and debug.info(f, "s") read
    // `ReplicatedStorage.Module:3:`, not Luau's `[string "ReplicatedStorage.Module"]:3:` for an unmarked name
    // (measured in a playing client, Studio 0.744: tools/parity/probes/chunknames.luau).
    let chunk = chunk.set_name(format!("={name}")).set_environment(env);
    let (deferred, interpreted, interpret_all) = {
        let modules = engine.modules.borrow();
        (modules.deferred.is_some(), modules.interpreted, modules.interpret_all)
    };
    if !deferred && !interpreted {
        return crate::timing::time("modules: compile + load", || chunk.into_function());
    }
    #[cfg(feature = "jit")]
    lua.enable_jit(false);
    let function = crate::timing::time("modules: compile + load", || chunk.into_function());
    #[cfg(feature = "jit")]
    if !interpret_all {
        lua.enable_jit(true);
    }
    #[cfg(not(feature = "jit"))]
    let _ = interpret_all;
    if interpreted {
        return function;
    }
    if let (Ok(function), Some(queue)) = (&function, engine.modules.borrow_mut().deferred.as_mut()) {
        queue.push_back(function.clone());
    }
    function
}

/// Gives `module` another source before anything requires it (a stand-in for a module a run
/// cannot have, e.g. one that needs the whole client); an error once it was required.
pub fn replace_source(engine: &Engine, module: InstanceId, source: &[u8]) -> Result<(), String> {
    if engine.modules.borrow().records.keys().any(|&(_, required)| required == module) {
        return Err(format!("{} was already required", engine.state.borrow().dom.full_name(&engine.reflection, module)));
    }
    engine.modules.borrow_mut().bytecode.remove(&module);
    let mut state = engine.state.borrow_mut();
    let instance = state.dom.at_mut(module);
    instance.set_source(Some(Rc::from(source)));
    instance.set_source_path(None);
    Ok(())
}

/// A script's source text: its file under the mounted roots (read once), else its Source property.
pub fn source_of(engine: &Engine, module: InstanceId) -> Option<Vec<u8>> {
    let (source, path) = {
        let state = engine.state.borrow();
        let instance = state.dom.at(module);
        (instance.source().cloned(), instance.source_path().cloned())
    };
    if let Some(source) = source {
        return Some(source.to_vec());
    }
    if let Some(path) = path {
        note_read(engine, &path);
        if let Ok((bytes, bytecode)) = crate::timing::time("modules: read sources", || read_file(engine, &path)) {
            engine.state.borrow_mut().dom.at_mut(module).set_source(Some(Rc::from(bytes.as_slice())));
            if let Some(bytecode) = bytecode {
                engine.modules.borrow_mut().bytecode.insert(module, bytecode);
            }
            return Some(bytes);
        }
    }
    match engine.state.borrow().dom.raw_get(module, K::Source as NameId) {
        Some(Value::String(source)) => Some(source.as_bytes().to_vec()),
        _ => None,
    }
}

fn error_result(lua: &Lua, message: String, level: i64) -> LuaResult<LuaMultiValue> {
    Ok(LuaMultiValue::from_iter([
        LuaValue::Integer(1),
        LuaValue::String(lua.create_string(message)?),
        LuaValue::Integer(level as _),
    ]))
}

/// Resolves a require-by-string path from the calling script.
fn resolve_path(engine: &Rc<Engine>, path: &[u8], caller: InstanceId) -> Result<InstanceId, String> {
    let text = String::from_utf8_lossy(path).into_owned();
    let segments: Vec<&str> = text.split('/').collect();
    let first = segments[0];
    let parent_of = |id: InstanceId| engine.state.borrow().dom.at(id).parent;
    let (mut current, start) = if first == "." {
        (parent_of(caller), 1)
    } else if first == ".." {
        (parent_of(caller).and_then(parent_of), 1)
    } else if first == "@self" {
        (Some(caller), 1)
    } else if let Some(alias) = first.strip_prefix('@') {
        (resolve_alias(engine, alias), 1)
    } else {
        return Err("require path must start with a valid prefix: ./, ../, or @".into());
    };
    for segment in &segments[start..] {
        let Some(node) = current else { break };
        if *segment == ".." {
            current = parent_of(node);
        } else if *segment != "." && !segment.is_empty() {
            materialize(engine, node);
            let state = engine.state.borrow();
            let mut found = state.dom.find_child(node, segment.as_bytes());
            if found.is_none() {
                let lower = segment.to_ascii_lowercase();
                let stripped = if lower.ends_with(".luau") {
                    &segment[..segment.len() - 5]
                } else if lower.ends_with(".lua") {
                    &segment[..segment.len() - 4]
                } else {
                    segment
                };
                found = state.dom.find_child(node, stripped.as_bytes());
            }
            current = found;
        }
    }
    current.ok_or_else(|| {
        format!(
            "Could not resolve require path '{text}' from {}",
            engine.state.borrow().dom.full_name(&engine.reflection, caller)
        )
    })
}

/// Modules.ResolveAlias: @game, or a service of the DataModel.
fn resolve_alias(engine: &Rc<Engine>, alias: &str) -> Option<InstanceId> {
    let game = engine.world().game;
    if alias == "game" {
        return Some(game);
    }
    crate::services::get_service(engine, game, alias).ok()
}

fn thread_key(thread: &LuaThread) -> usize {
    thread.to_pointer() as usize
}

/// RequireBegin(Target, Caller): see prelude.luau Require.
pub fn require_begin(lua: &Lua, engine: &Rc<Engine>, (target, caller, vm): (LuaValue, LuaValue, Vm)) -> LuaResult<LuaMultiValue> {
    let caller_id = match &caller {
        LuaValue::UserData(ud) => instance_of_ud(ud),
        _ => None,
    };
    let module = match &target {
        LuaValue::String(path) => {
            let Some(caller) = caller_id else {
                return error_result(lua, "require by string needs a calling script".into(), 0);
            };
            match resolve_path(engine, &path.as_bytes(), caller) {
                Ok(module) => module,
                Err(message) => return error_result(lua, message, 0),
            }
        }
        LuaValue::Number(_) | LuaValue::Integer(_) => {
            let text = lua.coerce_string(target.clone())?.map(|s| s.to_string_lossy()).unwrap_or_default();
            // (a playing client's text, measured, Studio 0.744: tools/parity/probes/errors.luau; a server downloads the asset,
            // which ree cannot offline)
            if !engine.options.server() {
                return error_result(lua, format!("require(assetId) cannot be called from a client.  assetId = {text}"), 0);
            }
            return error_result(lua, format!("require(assetId) is not supported offline (asset {text})"), 0);
        }
        LuaValue::UserData(ud) if instance_of_ud(ud).is_some() => instance_of_ud(ud).unwrap(),
        _ => return error_result(lua, "Attempted to call require with invalid argument(s).".into(), 0),
    };
    let module_script = engine.class_id("ModuleScript");
    if Some(engine.state.borrow().dom.at(module).class) != module_script {
        return error_result(lua, "Attempted to call require with invalid argument(s).".into(), 0);
    }

    let current = lua.current_thread();
    {
        let mut refs = engine.modules.borrow_mut();
        if let Some(record) = refs.records.get_mut(&(vm, module)) {
            match record.status {
                Status::Done => {
                    crate::profiler::caches::hit(crate::profiler::caches::Cache::Requires);
                    return Ok(LuaMultiValue::from_iter([LuaValue::Integer(0), record.result.clone().unwrap_or(LuaValue::Nil)]));
                }
                Status::Error => {
                    return error_result(lua, "Requested module experienced an error while loading".into(), 0);
                }
                Status::Loading => {
                    if record.owner.as_ref().map(thread_key) == Some(thread_key(&current)) {
                        return error_result(lua, "Requested module was required recursively".into(), 0);
                    }
                    record.waiting.push(current);
                    return Ok(LuaMultiValue::from_iter([LuaValue::Integer(2)]));
                }
            }
        }
    }

    crate::profiler::caches::miss(crate::profiler::caches::Cache::Requires);
    // JSON modules (Rojo `.json`) evaluate to the decoded value
    let (json, path, studio_only) = {
        let state = engine.state.borrow();
        let instance = state.dom.at(module);
        (instance.has(flags::JSON_MODULE), instance.source_path().cloned(), instance.has(flags::STUDIO_ONLY))
    };
    if let Some(path) = &path {
        engine.modules.borrow_mut().files.insert(path.to_string());
    }
    if let (true, Some(path)) = (json, &path) {
        note_read(engine, path);
        let decoded = read_file(engine, path)
            .map(|(bytes, _)| bytes)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| error.to_string()))
            .and_then(|value| {
                lua.to_value_with(
                    &value,
                    LuaSerializeOptions::new()
                        .set_array_metatable(false)
                        .serialize_none_to_null(false)
                        .serialize_unit_to_null(false),
                )
                .map_err(|error| error.to_string())
            });
        let mut refs = engine.modules.borrow_mut();
        return match decoded {
            Ok(value) => {
                refs.records.insert((vm, module), Record { status: Status::Done, result: Some(value.clone()), waiting: Vec::new(), owner: None });
                Ok(LuaMultiValue::from_iter([LuaValue::Integer(0), value]))
            }
            Err(message) => {
                refs.records.insert((vm, module), Record { status: Status::Error, result: None, waiting: Vec::new(), owner: None });
                drop(refs);
                let name = engine.state.borrow().dom.full_name(&engine.reflection, module);
                error_result(lua, format!("{name}: invalid JSON module ({message})"), 0)
            }
        };
    }

    let Some(source) = source_of(engine, module) else {
        let name = engine.state.borrow().dom.full_name(&engine.reflection, module);
        let message = if studio_only {
            format!("Module {name} exists only in Studio (its source is not in the project)")
        } else {
            format!("Module {name} has no source")
        };
        return error_result(lua, message, 0);
    };

    engine.modules.borrow_mut().records.insert(
        (vm, module),
        Record { status: Status::Loading, result: None, waiting: Vec::new(), owner: Some(current) },
    );
    match compile_in(lua, engine, module, &source, vm) {
        Ok(chunk) => Ok(LuaMultiValue::from_iter([
            LuaValue::Integer(3),
            LuaValue::Function(chunk),
            instance_value(lua, engine, module)?,
        ])),
        Err(error) => {
            // A compile error fails the module like a runtime error (pcall(Modules.Compile))
            let message = super::super::scheduler::error_message(&error);
            finish(engine, vm, module, Err(()));
            Ok(LuaMultiValue::from_iter([LuaValue::Integer(1), LuaValue::String(lua.create_string(message)?), LuaValue::Integer(0)]))
        }
    }
}

fn finish(engine: &Rc<Engine>, vm: Vm, module: InstanceId, result: Result<LuaValue, ()>) {
    let waiting = {
        let mut refs = engine.modules.borrow_mut();
        let Some(record) = refs.records.get_mut(&(vm, module)) else { return };
        match result {
            Ok(value) => {
                record.status = Status::Done;
                record.result = Some(value);
            }
            Err(()) => record.status = Status::Error,
        }
        record.owner = None;
        std::mem::take(&mut record.waiting)
    };
    for thread in waiting {
        defer_thread(engine, thread, Vec::new());
    }
}

/// RequireFinish(Module, pcall results): records the outcome and wakes waiting requirers.
pub fn require_finish(lua: &Lua, engine: &Rc<Engine>, (module, results, vm): (LuaValue, LuaTable, Vm)) -> LuaResult<LuaMultiValue> {
    let LuaValue::UserData(ud) = module else { return Err(LuaError::runtime("RequireFinish: module expected")) };
    let module = instance_of_ud(&ud).ok_or_else(|| LuaError::runtime("RequireFinish: module expected"))?;
    let count: i64 = results.get("n")?;
    let ok: bool = results.get(1)?;
    if !ok {
        let failure: LuaValue = results.get(2)?;
        finish(engine, vm, module, Err(()));
        return Ok(LuaMultiValue::from_iter([LuaValue::Boolean(false), failure, LuaValue::Integer(0)]));
    }
    if count - 1 != 1 {
        finish(engine, vm, module, Err(()));
        return Ok(LuaMultiValue::from_iter([
            LuaValue::Boolean(false),
            LuaValue::String(lua.create_string("Module code did not return exactly one value")?),
            LuaValue::Integer(0),
        ]));
    }
    let value: LuaValue = results.get(2)?;
    finish(engine, vm, module, Ok(value.clone()));
    Ok(LuaMultiValue::from_iter([LuaValue::Boolean(true), value]))
}

/// Modules.Require from the engine (no calling script): runs on the current thread.
pub fn require(lua: &Lua, engine: &Rc<Engine>, module: InstanceId) -> LuaResult<LuaValue> {
    let prelude = engine.prelude();
    let require: LuaFunction = prelude.exports.get("Require")?;
    require.call((instance_value(lua, engine, module)?, LuaValue::Nil, LuaValue::Nil))
}

/// Each module's result, for the modules that returned (diagnostics: `lua::holders`).
impl ModuleRecords {
    pub fn results(&self) -> Vec<(InstanceId, LuaValue)> {
        self.records.iter().filter(|((vm, _), _)| vm.is_none()).filter_map(|(&(_, module), record)| Some((module, record.result.clone()?))).collect()
    }
}

/// Modules.Reset
pub fn reset(engine: &Engine, module: InstanceId) {
    engine.modules.borrow_mut().records.retain(|&(_, required), _| required != module);
}

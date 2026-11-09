//! An emulated Roblox engine that runs a game's own Luau outside Roblox: instances with
//! reflection-backed properties and defaults, Roblox's datatypes, deferred signals, a virtual-time
//! scheduler with RunService's frame phases, services, `require` over a Rojo project, the 3D world
//! scripts see (CFrame views, spatial queries, emits and sound plays) and a recorder that writes
//! what a script does to the Workspace as a scene and a playback ror draws. One [`Engine`] per
//! mounted project, with the game's Luau running in an embedded, patched Luau VM (mlua).
//!
//! State that never runs Luau code lives in [`State`] behind one `RefCell`; operations that run
//! scripts (resuming threads, immediate signal handlers, requires) borrow it only between script
//! calls. Luau-side caches (instance userdata, method functions) live in [`lua::LuaRefs`].
//!
//! A host adds what it needs on top through [`Layer`]s (behaviours, hooks and frame phases
//! installed at Engine.Start) and [`Extension`]s (per-engine state of its own): roblox-ui-renderer
//! adds GUI layout, text measurement, input and the scene export this way. `docs/engine.md` is the
//! contract; docs/api.md the API in brief.

pub mod alloc;
pub mod animation;
pub mod boundary;
pub mod census;
pub mod cost;
pub mod derived;
pub mod dom;
pub mod emulator;
pub mod files;
pub mod images;
pub mod instance;
pub mod launch;
pub mod lua;
pub mod luau_profile;
pub mod meshes;
pub mod lune_json;
pub mod microprofile;
pub mod mount;
pub mod output;
pub mod profile;
pub mod profiler;
pub mod project;
pub mod record;
pub mod reflection;
pub mod scheduler;
#[cfg(feature = "cli")]
pub mod serve;
pub mod services;
pub mod signal;
pub mod text_measure;
pub mod timing;
pub mod trace;
pub mod value;
pub mod watchdog;
pub mod world;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod scheduler_tests;
#[cfg(test)]
mod server_tests;
#[cfg(test)]
mod luau_profile_tests;
#[cfg(test)]
mod profiler_tests;
#[cfg(test)]
mod profiler_prices_tests;
#[cfg(test)]
mod profiler_vm_tests;

use std::{
    any::Any,
    cell::{Cell, OnceCell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use rustc_hash::FxHashMap;

pub use dom::{Dom, InstanceId};
pub use emulator::Emulator;
pub use lune_roblox;
pub use mlua;
pub use reflection::{ClassId, K, NameId, Reflection};
pub use value::{Str, Value};

/// The engine's data, compiled in: Studio's ReflectionService dump (`data/reflection.json`,
/// tools/dump-reflection.luau writes it) and the Enum.Font faces (`data/font-enums.json`).
pub mod data {
    pub const REFLECTION: &[u8] = include_bytes!("../data/reflection.json");
    pub const FONT_ENUMS: &[u8] = include_bytes!("../data/font-enums.json");
}

/// Engine.Start options (runtime/engine/init.luau DefaultOptions).
#[derive(Clone, Debug)]
pub struct Options {
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub camera_width: Option<f64>,
    pub camera_height: Option<f64>,
    pub pixel_scale: f64,
    pub clips_descendants_supports_rotation: bool,
    /// "edit" (UI Labs previews) or a running game
    pub mode: String,
    /// Whose side scripts run on: a playing client (the default, with its local player) or a Studio
    /// Play-mode server (`--run-context server`: no local player, the server-only services)
    pub run_context: RunContext,
    /// RunService:IsStudio(): true for a Studio session (the default), false for a live game's
    /// server or client (`--studio false`), whose scripts then take their live branches
    pub studio: bool,
    pub topbar_inset: f64,
    pub device: String,
    pub place_id: f64,
    pub game_id: f64,
    pub creator_id: f64,
    pub place_name: String,
    pub local_player_name: Option<String>,
    pub local_player_user_id: f64,
    pub local_player_display_name: Option<String>,
    pub verbose: bool,
    pub offline: bool,
    pub loopback: bool,
    /// The place's Workspace.StreamingTargetRadius when it streams (StreamingEnabled), as its place
    /// file holds it (the project's rur.json `streamingTargetRadius`, or --streaming)
    pub streaming_target_radius: Option<f64>,
    /// The host's checkout: its state dir ([`state_dir`]) holds the caches (manifest bundles, Studio
    /// snapshots), and the default project sits beside it (launch.rs). Hosts set their own.
    pub repo: PathBuf,
    /// The project's folder, which content naming a relative local path is read from
    /// ([`Options::local_file`]): launch.rs gives `--project`, else the folder holding the first root,
    /// else the current directory. None: the current directory.
    pub project: Option<PathBuf>,
    /// The places whose clients pay more than ree's default prices (a Text write's localization lookup, a match place's
    /// geometry writes): cost.rs's list, plus `REE_LOCALIZED_PLACES` / `REE_MATCH_PLACES` on a command line (launch.rs)
    pub place_prices: cost::PlacePrices,
    /// A sub-frame timing check's second run (`ree record --timing-check`, record/run.rs): the in-frame clock moves, on twice
    /// the modelled work and never frozen (a frozen run's wall clock stays pinned), so a recording that changes with it hinges
    /// on when within a frame a script reads os.clock
    pub clock_moved: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            viewport_width: 1920.0,
            viewport_height: 1080.0,
            camera_width: None,
            camera_height: None,
            pixel_scale: 1.0,
            clips_descendants_supports_rotation: true,
            mode: "edit".into(),
            run_context: RunContext::Client,
            studio: true,
            topbar_inset: 0.0,
            device: "desktop".into(),
            place_id: 0.0,
            game_id: 0.0,
            creator_id: 0.0,
            place_name: "Place".into(),
            local_player_name: Some("Player".into()),
            local_player_user_id: 1.0,
            local_player_display_name: None,
            verbose: false,
            offline: false,
            loopback: false,
            streaming_target_radius: None,
            repo: default_repo(),
            project: None,
            place_prices: cost::PlacePrices::default(),
            clock_moved: false,
        }
    }
}

impl Options {
    pub fn edit_mode(&self) -> bool {
        self.mode == "edit"
    }

    pub fn server(&self) -> bool {
        self.run_context == RunContext::Server
    }

    pub fn server_services(&self) -> bool {
        self.server() || self.loopback
    }

    /// The file content `uri` names on disk, by ror's rules (its ror-assets uri.rs): `file://<path>`
    /// (`file:///C:/x` too, `%xx` decoded), or text that is no asset id or URI and is a path
    /// (`output/x.mesh`, `C:/x.mesh`, `./x.png`) or a file the project holds. A relative path is the
    /// project's ([`Options::project`]): a game's asset id modules can hold an asset's repo-relative file
    /// until it is uploaded. None for an asset id or any other URI.
    pub fn local_file(&self, uri: &str) -> Option<PathBuf> {
        let text = uri.trim();
        let file_uri = text.get(..7).is_some_and(|head| head.eq_ignore_ascii_case("file://"));
        let path = if file_uri {
            let decoded = percent_decode(&text[7..]);
            let bytes = decoded.as_bytes();
            let path = if bytes.len() > 2 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' { &decoded[1..] } else { &decoded[..] };
            if path.is_empty() {
                return None;
            }
            PathBuf::from(path)
        } else {
            if text.is_empty() || text.contains("://") || text.bytes().all(|b| b.is_ascii_digit()) || text.to_ascii_lowercase().contains("roblox.com/") {
                return None;
            }
            PathBuf::from(text)
        };
        let path = if path.is_absolute() { path } else { self.project.clone().or_else(|| std::env::current_dir().ok()).unwrap_or_default().join(path) };
        // (one spelling: `C:\x\output\y.mesh`, not `C:\x\output/y.mesh`, in what a failure says)
        let path = std::path::absolute(&path).unwrap_or(path);
        (file_uri || looks_like_path(text) || files::is_file(&path)).then_some(path)
    }
}

/// A drive or UNC path, one starting `./` or `../`, or any holding a separator (ror-assets'
/// looks_like_path; URIs are told apart before)
fn looks_like_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
    drive || text.starts_with("\\\\") || text.starts_with("./") || text.starts_with(".\\") || text.starts_with("../") || text.starts_with("..\\") || text.contains(['/', '\\'])
}

/// `%xx` escapes decoded (a file URI's), other bytes as they are
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A script error as a playing client's LogService.MessageOut gets it: the message, then the stack between `Stack Begin`
/// and `Stack End`, a line per frame of the script's own code (`Script '<chunk>', Line <n> - function <name>`, or no
/// function for an anonymous one), each indented four spaces. Measured in a playing client (Studio 0.744,
/// tools/parity/probes/logservice_text.luau): `task.spawn(error, "x")` has no frame between the two. `trace` is the
/// Luau traceback the scheduler took (`<chunk>:<line>[ function <name>]` a line); the engine's own Luau (`rur.*`) is
/// Roblox's C++ and has no frame. (ree compiles scripts with inlining, so a function inlined into its caller has no frame
/// of its own; Studio's playing client keeps one per call.)
fn client_error_text(message: &str, trace: &str) -> String {
    let mut text = format!("{message}\n    Stack Begin");
    for frame in stack_frames(trace) {
        text.push_str("\n    ");
        text.push_str(&frame);
    }
    text.push_str("\n    Stack End");
    text
}

/// The frames of a Luau traceback (`<chunk>:<line>[ function <name>]` a line) as Roblox's stack text names them, the script's
/// own code only: `Script '<chunk>', Line <n>`, with ` - function <name>` for a named function ([`client_error_text`]).
pub(crate) fn stack_frames(trace: &str) -> Vec<String> {
    let mut frames = Vec::new();
    for line in trace.lines() {
        let (place, function) = match line.split_once(" function ") {
            Some((place, function)) => (place, Some(function)),
            None => (line, None),
        };
        let Some((chunk, number)) = place.rsplit_once(':') else { continue };
        if chunk.starts_with("rur.") || chunk.starts_with('[') || number.parse::<u32>().is_err() {
            continue;
        }
        frames.push(match function {
            Some(function) => format!("Script '{chunk}', Line {number} - function {function}"),
            None => format!("Script '{chunk}', Line {number}"),
        });
    }
    frames
}

/// An output line LogService.MessageOut has yet to get ([`scheduler::deliver_log`]): its kind (print, info, warn, error), its
/// text, and the converted context a LogService method gave (MessageOut's third argument; a line without one gets an empty table).
pub struct LogLine {
    pub kind: &'static str,
    pub text: String,
    pub context: Option<Value>,
}

/// The side a run's scripts are on ([`Options::run_context`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunContext {
    #[default]
    Client,
    Server,
}

impl RunContext {
    pub fn parse(text: &str) -> Result<RunContext, String> {
        match text {
            "client" => Ok(RunContext::Client),
            "server" => Ok(RunContext::Server),
            other => Err(format!("--run-context must be client or server, not {other}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RunContext::Client => "client",
            RunContext::Server => "server",
        }
    }
}

/// Runs before `ree serve` exits on its own (its process exit skips the program's own ending: a
/// `--features dhat-heap` build writes its heap profile here)
pub static BEFORE_EXIT: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// This crate's checkout (where `ree` runs from): a service's starter's, else the executable's; hosts pass their own [`Options::repo`].
pub fn default_repo() -> PathBuf {
    if let Some(repo) = STARTER_REPO.get() {
        return repo.clone();
    }
    DEFAULT_REPO.get_or_init(|| std::env::current_exe().ok().and_then(|exe| checkout_of(&exe)).unwrap_or_else(|| PathBuf::from("."))).clone()
}

/// The checkout an executable belongs to: the nearest folder above it holding a `Cargo.lock`, without a verbatim prefix.
pub fn checkout_of(exe: &Path) -> Option<PathBuf> {
    let dir = exe.ancestors().skip(1).find(|dir| dir.join("Cargo.lock").is_file())?;
    let text = dir.to_string_lossy();
    Some(match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => dir.to_path_buf(),
    })
}

/// Sets [`default_repo`] for the rest of this process, even after something read it (a service: its starter's checkout).
pub fn set_default_repo(repo: PathBuf) {
    let _ = STARTER_REPO.set(repo);
}

static STARTER_REPO: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static DEFAULT_REPO: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The state dir: RUR_STATE_DIR, else RUR_CACHE_DIR, else `<repo>/.cache`. Caches live here, so a
/// checkout can be used without writing into it. (The variables keep the names they have in
/// roblox-ui-renderer, which shares this engine's caches.)
pub fn state_dir(repo: &Path) -> PathBuf {
    ["RUR_STATE_DIR", "RUR_CACHE_DIR"]
        .iter()
        .find_map(|name| std::env::var_os(name).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join(".cache"))
}

/// `relative` in the state dir, else the checkout's own `.cache` copy when only that one exists
/// (read-only data: Studio snapshots).
pub fn shared_state_path(repo: &Path, relative: &str) -> PathBuf {
    let own = state_dir(repo).join(relative);
    files::note(&own);
    files::note(&repo.join(".cache").join(relative));
    if own.exists() {
        return own;
    }
    let fallback = repo.join(".cache").join(relative);
    if fallback.exists() { fallback } else { own }
}

/// An error a script sees as a string. `level` 1 prefixes the position of the script line that
/// called the engine (Luau `error(msg, 2)` from an engine function); 0 adds no position.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    pub message: String,
    pub level: u8,
}

impl ScriptError {
    pub fn at_caller(message: impl Into<String>) -> Self {
        Self { message: message.into(), level: 1 }
    }

    pub fn plain(message: impl Into<String>) -> Self {
        Self { message: message.into(), level: 0 }
    }
}

pub type ScriptResult<T> = Result<T, ScriptError>;

/// Well-known instances of the DataModel.
#[derive(Clone, Copy, Debug)]
pub struct World {
    pub game: InstanceId,
    pub workspace: InstanceId,
    pub camera: InstanceId,
    pub local_player: Option<InstanceId>,
}

/// Engine state that never runs Luau code.
pub struct State {
    pub dom: Dom,
    pub signals: signal::Signals,
    pub sched: scheduler::Scheduler,
    pub output: output::Output,
    pub project: project::ProjectState,
    /// Engine-wide instance signals by (instance, event) are in the dom; these are services'
    pub collection: services::CollectionState,
    /// Rigid assemblies: the joint index and pending joint work (world::joints)
    pub joints: world::joints::Joints,
    /// Free fall: the unanchored parts in the Workspace and their velocities (world::gravity)
    pub gravity: world::gravity::Gravity,
}

/// Rust-side hooks (the Luau runtime's InstanceModule.Hooks that engine subsystems install).
#[derive(Default)]
pub struct Hooks {
    /// A property of an instance changed (layout invalidation; after signals are queued)
    pub property_changed: Vec<Box<dyn Fn(&mut State, InstanceId, NameId)>>,
    /// A write (instance::raw_set) stored the value the property already held: no Changed fires and
    /// `property_changed` does not run (the recorder keeps a run's writes to Lighting, whose values
    /// before the run stand in for a place's it does not have)
    pub property_unchanged: Vec<Box<dyn Fn(&mut State, InstanceId, NameId)>>,
    /// A script's SetAttribute changed an attribute (set, changed or removed): (instance, attribute name)
    pub attribute_changed: Vec<Box<dyn Fn(&mut State, InstanceId, &str)>>,
    /// An instance's parent changed: (instance, old parent, new parent)
    pub hierarchy_changed:
        Vec<Box<dyn Fn(&mut State, InstanceId, Option<InstanceId>, Option<InstanceId>)>>,
    /// An unreachable instance is about to be freed (lua::raw::collect): its id becomes stale
    /// and its slot index is reused, so caches keyed by id or slot index drop it here
    pub instance_freed: Vec<Box<dyn Fn(&mut State, InstanceId)>>,
    /// Derived property views (engine::derived)
    pub derived: derived::DerivedViews,
    /// What the 3D world did that no property records (ParticleEmitter:Emit / Clear, Trail:Clear,
    /// Sound plays and stops), reported by engine::world after the call (engine::record observes it)
    pub world_events: Vec<Box<dyn Fn(&mut State, &world::WorldEvent)>>,
}

/// What a host adds to every engine it starts (roblox-ui-renderer: GUI layout, TextService's text
/// measurement, input). [`lua::start`] installs each layer once the core's behaviours, derived
/// views and world are in and before the DataModel is built, so a layer registers behaviours
/// ([`Engine::behaviors`]), property / hierarchy hooks ([`Engine::hooks`]) and frame phases
/// (`state.sched.add_hook`) exactly as the core does, and may keep its state in an
/// [`Extension`].
pub trait Layer {
    fn install(&self, engine: &Rc<Engine>);
}

/// Per-engine state a layer keeps ([`Engine::extension_or_insert_with`]), one value per type.
pub trait Extension: Any {
    /// [`Engine::teardown`]: drop whatever refers back to the engine (closures, Lua values, its
    /// `Rc`), as the engine's own parts are dropped there
    fn teardown(&self) {}
}

/// How many extension types one engine holds.
const EXTENSIONS: usize = 6;

pub struct Engine {
    pub reflection: Arc<Reflection>,
    pub options: Options,
    pub state: RefCell<State>,
    pub lua: RefCell<lua::LuaRefs>,
    pub behaviors: RefCell<services::Behaviors>,
    pub hooks: RefCell<Hooks>,
    /// ModuleScript records (require caching / cycles)
    pub modules: RefCell<lua::modules::ModuleRecords>,
    /// The host's layers, installed by Engine.Start in this order
    layers: Vec<Rc<dyn Layer>>,
    /// The host's per-engine state, by type (set once, dropped by [`Engine::teardown`]'s turn)
    extensions: [OnceCell<Box<dyn Extension>>; EXTENSIONS],
    /// Where manifest bundles come from (project::model_assets); the asset worker unless set
    #[cfg(feature = "bundles")]
    bundles: RefCell<Option<Box<dyn project::model_assets::BundleSource>>>,
    member_cache: RefCell<FxHashMap<(ClassId, NameId), instance::Member>>,
    world: Cell<Option<World>>,
}

impl Engine {
    /// Creates the engine state (no DataModel yet: see `lua::start`), with no layers.
    pub fn new(options: Options) -> Result<Rc<Engine>, String> {
        Self::with_layers(options, Vec::new())
    }

    /// Creates the engine state with the host's `layers` (installed by `lua::start`).
    pub fn with_layers(options: Options, layers: Vec<Rc<dyn Layer>>) -> Result<Rc<Engine>, String> {
        let reflection = Reflection::shared()?;
        let mut output = output::Output::default();
        output.verbose = options.verbose;
        let sched = scheduler::Scheduler::new(&options);
        Ok(Rc::new(Engine {
            reflection,
            state: RefCell::new(State {
                dom: Dom::new(),
                signals: Default::default(),
                sched,
                output,
                project: Default::default(),
                collection: Default::default(),
                joints: Default::default(),
                gravity: Default::default(),
            }),
            options,
            lua: RefCell::new(lua::LuaRefs::default()),
            behaviors: RefCell::new(services::Behaviors::default()),
            hooks: RefCell::new(Hooks::default()),
            modules: RefCell::new(Default::default()),
            layers,
            extensions: Default::default(),
            #[cfg(feature = "bundles")]
            bundles: RefCell::new(None),
            member_cache: RefCell::new(FxHashMap::default()),
            world: Cell::new(None),
        }))
    }

    pub fn world(&self) -> World {
        self.world.get().expect("engine not started")
    }

    /// The host's layers, in installation order.
    pub fn layers(&self) -> &[Rc<dyn Layer>] {
        &self.layers
    }

    /// The extension of type `T`, if a layer made one.
    pub fn extension<T: Extension>(&self) -> Option<&T> {
        self.extensions.iter().filter_map(OnceCell::get).find_map(|extension| (extension.as_ref() as &dyn Any).downcast_ref::<T>())
    }

    /// The extension of type `T`, made with `make` on first use.
    pub fn extension_or_insert_with<T: Extension>(&self, make: impl FnOnce() -> T) -> &T {
        if let Some(found) = self.extension::<T>() {
            return found;
        }
        let slot = self.extensions.iter().find(|slot| slot.get().is_none()).expect("an engine holds at most 6 extension types");
        let stored = slot.get_or_init(|| Box::new(make()));
        (stored.as_ref() as &dyn Any).downcast_ref::<T>().expect("the extension just stored")
    }

    /// Where `rur.json` `modelAssets` / `--assets` bundles load from in this engine (default: the
    /// asset worker, project::model_assets::AssetWorker). Set before the project mounts.
    #[cfg(feature = "bundles")]
    pub fn set_bundle_source(&self, source: Box<dyn project::model_assets::BundleSource>) {
        *self.bundles.borrow_mut() = Some(source);
    }

    #[cfg(feature = "bundles")]
    pub(crate) fn take_bundle_source(&self) -> Option<Box<dyn project::model_assets::BundleSource>> {
        self.bundles.borrow_mut().take()
    }

    /// Breaks the references the engine's parts hold back to it (Rust hooks and behaviours whose
    /// closures capture the engine, the Instance API, the layers' extensions), once its Luau VM is
    /// gone, so that dropping the last handle frees it. A process that runs many VMs (a render
    /// service, an app's story switches) would keep every one otherwise. The engine is not usable
    /// afterwards. Each part is moved out before it is dropped, so a Drop that reaches back into the
    /// engine finds it unborrowed.
    pub fn teardown(&self) {
        drop(std::mem::take(&mut *self.lua.borrow_mut()));
        drop(std::mem::take(&mut *self.hooks.borrow_mut()));
        drop(std::mem::take(&mut *self.behaviors.borrow_mut()));
        drop(std::mem::take(&mut *self.modules.borrow_mut()));
        for extension in self.extensions.iter().filter_map(OnceCell::get) {
            extension.teardown();
        }
        drop(std::mem::take(&mut *self.member_cache.borrow_mut()));
        let signals = std::mem::take(&mut self.state.borrow_mut().signals);
        drop(signals);
        let collection = std::mem::take(&mut self.state.borrow_mut().collection);
        drop(collection);
        let sched = std::mem::replace(&mut self.state.borrow_mut().sched, scheduler::Scheduler::new(&self.options));
        drop(sched);
        let dom = std::mem::replace(&mut self.state.borrow_mut().dom, Dom::new());
        drop(dom);
    }

    pub(crate) fn set_world(&self, world: World) {
        self.world.set(Some(world));
        self.state.borrow().sched.touched.borrow_mut().workspace = Some(world.workspace);
    }

    /// Output.Add with the virtual clock.
    pub fn output(&self, kind: &'static str, message: impl Into<String>, trace: Option<String>) {
        self.output_with_context(kind, message, trace, None);
    }

    /// Output.Add with the virtual clock, and the line for LogService.MessageOut, with `context` (a LogService method's converted
    /// context table) as its third argument. Kinds: print, info (MessageInfo: LogService:Info), warn, error.
    pub fn output_with_context(&self, kind: &'static str, message: impl Into<String>, trace: Option<String>, context: Option<Value>) {
        let mut state = self.state.borrow_mut();
        let now = state.sched.now;
        let message = message.into();
        let server = self.options.server();
        if server && kind == "error" {
            self.fire_script_error(&mut state, &message, trace.as_deref());
        }
        if !self.options.edit_mode() && !state.sched.delivering_log {
            // LogService.MessageOut gets the line later in the frame, on a playing client and on a server alike
            // (scheduler::deliver_log; measured in a server game script, Studio 0.744: print, LogService:Info, :Warn and :Error's
            // lines of one step arrive after that frame's Heartbeat handlers and the deferred work beside them). An error, and
            // an engine warning that carries the script's stack, has its stack lines in the text, on a client and on a server
            // alike (measured in a server's game script too: tools/parity/probes/logservice.luau and logservice_text.luau run
            // with run.mjs --server).
            let text = match &trace {
                Some(trace) if kind == "error" || kind == "warn" => client_error_text(&message, trace),
                _ => message.clone(),
            };
            state.sched.log_queue.push(LogLine { kind, text, context });
        }
        state.output.add(kind, message, trace, now);
    }

    /// A LogService error table no script caught (LogService:Error, or :Log with MessageError): its method logged the message
    /// when it was called, so the thread's end logs no error line of its own but the stack, each line apart as MessageInfo:
    /// "Stack Begin", a `Script '<chunk>', Line <n>` line per frame, "Stack End". Measured in a playing client (Studio 0.744):
    /// so for a spawned thread, a deferred one, an event handler and a caught table thrown again; ScriptContext.Error gets the
    /// error's text as for any error (fired where ree fires it: a server's).
    pub fn uncaught_log_error(&self, message: &str, trace: Option<String>) {
        self.output("info", "Stack Begin", None);
        for frame in trace.as_deref().map(stack_frames).unwrap_or_default() {
            self.output("info", frame, None);
        }
        self.output("info", "Stack End", None);
        if self.options.server() {
            self.fire_script_error(&mut self.state.borrow_mut(), message, trace.as_deref());
        }
    }

    /// A server's ScriptContext.Error (message, stackTrace, script) for a script error, as Roblox's documentation describes it
    /// (deferred handlers; the script argument is nil: the engine does not track which script a thread belongs to). Only when
    /// the service was made and the event connected.
    fn fire_script_error(&self, state: &mut State, message: &str, trace: Option<&str>) {
        let Some(world) = self.world.get() else { return };
        let reflection = &self.reflection;
        let found = state.dom.at(world.game).children.iter().copied().find(|&child| &*reflection.class(state.dom.at(child).class).name == "ScriptContext");
        if let (Some(context), Some(event)) = (found, reflection.name_id("Error")) {
            instance::fire_event(state, context, event, vec![Value::String(Str::from(message)), Value::String(Str::from(trace.unwrap_or_default())), Value::Nil]);
        }
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.output("warn", message, None);
    }

    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.reflection.class_id(name)
    }
}

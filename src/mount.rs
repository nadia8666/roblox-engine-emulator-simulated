//! Mounting a project (what `runtime/main.luau` mounted before a story): Engine.Start, Studio
//! snapshots, the Rojo roots (or a Rojo project file's tree), data roots, explicit mounts, package
//! stand-ins, the local player joining, the place's Studio hierarchy and the manifest bundles.
//!
//! [`mount_project`] does all of it. A host that adds its own setup between the player joining and
//! the Studio hierarchy (roblox-ui-renderer loads its story host there and sets
//! [`crate::project::ProjectState::client_gui_mount`]) calls [`begin_project`], does its part, then
//! [`Mounting::finish`]. RUR_ENGINE_TIMINGS=1 prints phase timings; RUR_ENGINE_DUMP=<file> writes
//! every instance after the mount.

use std::{path::PathBuf, rc::Rc, time::Instant};

use mlua::prelude::*;

use super::{
    Engine,
    lua::{self as engine_lua, globals::join_local_player},
    project, services,
};

/// Where the project comes from.
#[derive(Clone, Debug, Default)]
pub struct ProjectSpec {
    pub roots: Vec<String>,
    /// Studio exports (<Service>.rbxm) loaded into their services
    pub snapshots: Vec<String>,
    pub snapshot_dir: Option<String>,
    /// Explicit mounts after the roots (`--mount` / `--instance`, Rojo project files)
    pub mounts: Vec<project::Mount>,
    /// Where and which manifest bundles mount instead of rur.json `modelAssets`' choice
    pub model_assets: project::ModelAssetsOverride,
    /// A Rojo project file's tree (`--rojo`, crate::rojo), mounted where the roots mount: after the snapshots, before the
    /// data roots, the Studio names and the explicit mounts. It comes without roots; its folder stands for the place's
    /// root ([`ProjectSpec::place_roots`]).
    pub rojo: Option<crate::rojo::Tree>,
}

impl ProjectSpec {
    /// The folders the place is looked up by ([`place_roots`])
    pub fn place_roots(&self) -> Vec<String> {
        place_roots(&self.roots, self.rojo.as_ref())
    }
}

/// The folders a place is looked up by, as a root's folder names it (the Studio hierarchy snapshot and the Studio-only
/// instances it serves, the place id, rur.json's settings, the data roots, the Studio name map, the package stand-ins
/// and the manifest bundles): the roots, or, for a Rojo project file mounted without them, the project file's folder,
/// which stands for the place's root as the root folder holding a place's project file does
/// ([`project::PLACES_FILE`]'s `projectFile`). The layout mount (`<root>/<Service>/...`) is the roots' alone.
pub fn place_roots(roots: &[String], rojo: Option<&crate::rojo::Tree>) -> Vec<String> {
    match rojo {
        Some(tree) if roots.is_empty() => vec![tree.folder()],
        _ => roots.to_vec(),
    }
}

/// The mounted project.
#[derive(Debug)]
pub struct Project {
    /// Studio hierarchy snapshot in use (a scene's diagnostics.studioOnly.hierarchy)
    pub hierarchy: Option<String>,
    /// Seconds from the start to the end of the project mount
    pub mount_time: f64,
    pub started: Instant,
}

/// A project mount between [`begin_project`] and [`Mounting::finish`].
pub struct Mounting {
    started: Instant,
    phase_start: Instant,
    timings: bool,
}

impl Mounting {
    /// Ends a timed phase (RUR_ENGINE_TIMINGS=1 prints it with the instances created so far).
    pub fn phase(&mut self, engine: &Engine, name: &str) {
        if self.timings {
            let created = engine.state.borrow().dom.created();
            crate::output::diagnostic(&format!("[engine] {name}: {:.1} ms ({created} instances)", self.phase_start.elapsed().as_secs_f64() * 1000.0));
        }
        if crate::timing::enabled() {
            crate::timing::add(format!("mount: {name}"), self.phase_start.elapsed().as_secs_f64());
        }
        self.phase_start = Instant::now();
    }

    /// The rest of the mount: the place's Studio hierarchy and the manifest bundles.
    pub fn finish(mut self, engine: &Rc<Engine>, spec: &ProjectSpec) -> Result<Project, String> {
        let place_roots = spec.place_roots();
        let hierarchy = project::use_studio_hierarchy(engine, &place_roots);
        if let Some(path) = &hierarchy {
            project::mount_place_content(engine, path);
        }
        #[cfg(feature = "bundles")]
        project::model_assets::mount(engine, &place_roots, &spec.model_assets);
        self.phase(engine, "studio hierarchy");
        if let Ok(path) = std::env::var("RUR_ENGINE_DUMP") {
            let state = engine.state.borrow();
            let mut lines = Vec::new();
            for index in 0..state.dom.slot_count() {
                if let Some(id) = state.dom.id_at_slot(index as u32) {
                    let instance = state.dom.at(id);
                    lines.push(format!("{} {} {}", instance.serial, engine.reflection.class(instance.class).name, state.dom.full_name(&engine.reflection, id)));
                }
            }
            let _ = std::fs::write(path, lines.join("\n"));
        }
        Ok(Project { hierarchy, mount_time: self.started.elapsed().as_secs_f64(), started: self.started })
    }
}

fn lua_error(error: LuaError) -> String {
    error.to_string()
}

/// Engine.Start and the project mount up to the local player joining: snapshots, roots (or a Rojo
/// project file's tree), data roots, mounts, package stand-ins, JoinLocalPlayer. `started` is when the
/// run began (timings count from it).
pub fn begin_project(lua: &Lua, engine: &Rc<Engine>, spec: &ProjectSpec, started: Instant) -> Result<Mounting, String> {
    let mut mounting = Mounting { started, phase_start: started, timings: std::env::var("RUR_ENGINE_TIMINGS").as_deref() == Ok("1") };
    mounting.phase(engine, "engine (reflection)");
    let place_roots = spec.place_roots();
    // (the roots listed on other threads during Engine.Start)
    let mut prescan = project::Prescan::start_for(&spec.roots, &place_roots, &engine.reflection);
    engine_lua::start(lua, engine).map_err(lua_error)?;
    // Scheduler.MainThread (Engine.Start runs on the main thread)
    engine.state.borrow_mut().sched.set_main_thread(&lua.current_thread());
    mounting.phase(engine, "start");

    // Snapshots (Studio exports: <dir>/<Service>.rbxm)
    let game = engine.world().game;
    let service_of = |path: &str| -> Option<String> {
        let name = PathBuf::from(path).file_name()?.to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let stem = if lower.ends_with(".rbxmx") {
            &name[..name.len() - 6]
        } else if lower.ends_with(".rbxm") {
            &name[..name.len() - 5]
        } else {
            return None;
        };
        Some(stem.to_string())
    };
    for path in &spec.snapshots {
        let Some(service_name) = service_of(path) else { continue };
        match services::get_service(engine, game, &service_name) {
            Ok(service) => {
                if let Err(error) = project::load_snapshot(engine, std::path::Path::new(path), service) {
                    engine.warn(format!("snapshot {path}: {error}"));
                }
            }
            Err(_) => engine.warn(format!("snapshot {path}: {service_name} is not a service")),
        }
    }
    if let Some(dir) = &spec.snapshot_dir {
        crate::files::note_listing(std::path::Path::new(dir));
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == "PhysicsService.collisiongroups" {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        crate::world::query::set_collision_group_data(engine, bytes);
                    }
                    continue;
                }
                let Some(service_name) = service_of(&name) else { continue };
                if let Ok(service) = services::get_service(engine, game, &service_name) {
                    let path = format!("{dir}/{name}");
                    if let Err(error) = project::load_snapshot(engine, std::path::Path::new(&path), service) {
                        engine.warn(format!("snapshot {path}: {error}"));
                    }
                }
            }
        }
    }

    mounting.phase(engine, "snapshots");
    for root in &spec.roots {
        prescan.mount(engine, root);
        mounting.phase(engine, &format!("mount {root}"));
    }
    // A Rojo project file's tree, where the roots mount; a `$path` it maps that doesn't exist is reported (an optional
    // one is not)
    if let Some(tree) = &spec.rojo {
        for missing in &tree.missing {
            engine.warn(format!("{missing}; not mounted"));
        }
        for mount in &tree.mounts {
            project::mount_at(engine, mount)?;
        }
        mounting.phase(engine, &format!("mount {}", tree.file.display()));
    }
    prescan.mount_data(engine, &place_roots);
    mounting.phase(engine, "data roots");
    if engine.options.server() {
        project::apply_studio_names(engine, &place_roots);
        mounting.phase(engine, "studio names");
    }
    for mount in &spec.mounts {
        project::mount_at(engine, mount)?;
    }
    if !spec.mounts.is_empty() {
        mounting.phase(engine, "mounts");
    }
    project::mount_stand_in_packages(engine, &place_roots);
    join_local_player(engine);
    mounting.phase(engine, "stand-ins, player");
    Ok(mounting)
}

/// Engine.Start plus the whole project mount.
pub fn mount_project(lua: &Lua, engine: &Rc<Engine>, spec: &ProjectSpec, started: Instant) -> Result<Project, String> {
    begin_project(lua, engine, spec, started)?.finish(engine, spec)
}

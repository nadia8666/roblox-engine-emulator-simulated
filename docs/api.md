# Rust API (`roblox_engine_emulator`)

Guide to embedding `roblox_engine_emulator` in a Rust host, extending it with custom layers (such as a GUI engine), and depending on the crate from Cargo. For the underlying behavioral contract, see [engine.md](engine.md).

## Minimal example

```rust
use roblox_engine_emulator::{Emulator, Engine, Options, mount::ProjectSpec};

let engine = Engine::new(Options { mode: "play".into(), repo: my_checkout, ..Options::default() })?;
let emulator = Emulator::start(engine, &ProjectSpec { roots: vec![shared_root, game_root], ..Default::default() })?;
let call = emulator.run("Probe", "print(workspace.CurrentCamera.FieldOfView) task.wait(1)")?;
emulator.run_until(2.0)?; // or emulator.step(1.0 / 60.0) frame by frame
for line in emulator.output() {
    println!("{} {}", line.kind, line.message);
}
```

## Core building blocks

- **`Engine` (`src/lib.rs`):** Single-threaded (`!Send`), one per mounted project. Construct with `Engine::new(options)` or `Engine::with_layers(options, layers)`. Exposes `state` (DOM, signals, scheduler, output, project state), `reflection`, `behaviors`, and `hooks`. Call `engine.teardown()` after dropping the Lua VM to break reference cycles (`Emulator` manages both automatically). `Options` configures viewport, pixel scale, mode, place, local player, `repo` (host checkout for state caches), and work-clock device pricing.
- **Mounting (`src/mount.rs`):** `mount_project(lua, engine, &spec, started)` runs `lua::start` plus snapshots, Rojo roots, data roots, explicit mounts, package stand-ins, local player join, Studio hierarchy, and manifest bundles. Use `begin_project` + `Mounting::finish` to inject custom setup right before player join. `launch::parse`, `Args::accept`, and `prepare` translate CLI engine options into `Options`, roots, and mounts. `ProjectSpec::rojo` (a `rojo::Tree`, read by `prepare` from `--rojo`) mounts a Rojo project file's tree in the roots' place.
- **Rojo project files (`src/rojo.rs`):** `rojo::mounts(file, at, &mut warnings)` reads a `*.project.json` into its mounts (`--mount` / `--instance` pairs in file order), `rojo::mappings` lists every `$path` with whether it exists, and `project_file_in`, `missing_path`, `wally_advice` and `several_projects` help a host find and explain projects; `rojo::Tree::read` gives the engine's mounts for a run.
- **Executing Luau (`src/lua/`):**
  - `lua::globals::call(lua, engine, function, args)` runs a Luau function on a scheduler thread (may yield; inspect `Call::outcome`).
  - `lua::modules::environment` creates script environments; `lua::modules::require` loads a `ModuleScript`; `replace_source` swaps module source code.
  - `lua::instance_value` / `lua::instance_id` convert between Rust instance IDs and Luau values.
- **Virtual clock & scheduler (`src/scheduler.rs`):** `scheduler::step(lua, engine, Some(dt))`, `run_until(lua, engine, t)`, `step_display_duration` (display refresh cadence), `flush_deferred`, and `use_recorded_clock` (replaying a recorded Studio clock).
- **3D world (`src/world/`):** World CFrame views for parts and attachments, spatial queries (`world::query`: `solids`, `cast_ray`, `cast_box`, and `WorldRoot` methods), `Camera` projections, and `Hooks::world_events` (particle emits, clears, sound plays via `world::WorldEvent`).
- **3D recording (`src/record/`):** `Recorder::install(engine, fps)` → `recorder.sample(engine, frame)` after each frame → `recorder.write(engine, out, extra)`. High-level CLI driver: `record::run::record(&args, &host)` (or `record::run::execute` + `record::run::release` to retain/defer freeing the emulator).
- **Layers and extensions:** Implement `Layer::install(&self, engine: &Rc<Engine>)`, which executes during `Engine.Start` after core behaviors/world setup and before `DataModel` construction. Layers register class behaviors (`engine.behaviors.borrow_mut().add_method` / `add_getter` / `add_setter`), property/hierarchy hooks (`engine.hooks`), and scheduler phase hooks (`state.sched.add_hook(Phase::Render, ...)`). Store per-engine state in an `Extension` (`engine.extension_or_insert_with(...)`, `engine.extension::<T>()`) whose `teardown` releases back-references to the engine. See `roblox-ui-renderer`'s `native/src/engine/mod.rs` for a full GUI layer.
- **Bundles (`src/project/model_assets.rs`):** Call `engine.set_bundle_source(Box<dyn BundleSource>)` to replace the default `AssetWorker`. `cached` reports pre-cached files for eager decoding; `shape` chooses between `BundleShape::Uploaded` (single root replaces placeholder) and the default `BundleShape::Members` (root's children mounted in a folder at the bundle path).
- **Reusable model plans (`src/project/`):** `ModelPlan::new(&reflection, &dom, roots)` compiles a decoded model into compact `Send + Sync` flat arrays. Instantiate eagerly with `plan.instantiate(engine, parent)`, or lazily on script access with `plan.instantiate_lazy`, `instantiate_lazy_as`, or `instantiate_lazy_members` ([engine.md](engine.md)). Multi-engine hosts can share compiled bytecode (`lua::modules::reuse_compiled_modules`) and decoded bundles (`project::model_assets::keep_bundles`).
- **Profiling (`src/profiler/`):** Build a `profiler::Config` (`from_args` or `new`) and call `profiler::Session::begin(config)` before engine startup, hook the VM with `luau_profile::install(&lua)`, mark phases with `luau_profile::set_phase`, collect before VM drop via `profiler::collect(&lua)`, and finish with `session.finish(Run { ... })`. Hosts can register custom caches via `profiler::caches::named` + `named_hit` / `named_miss` / `named_evicted`.
- **Reusing `ree` CLI commands:** Supply a `launch::Host` (name, checkout, engine factory) to `record::run::cli(argv, &host, true)`—this is how `rur-native record` invokes `ree`'s recorder with `rur`'s GUI layer.

## Depending on the crate from Cargo

```toml
[dependencies]
roblox-engine-emulator = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<commit>" }

# Cargo only applies [patch.crates-io] from the workspace root, so dependents must repeat
# the patched Luau VM and mlua-sys pins at the exact same commit:
[patch.crates-io]
luau0-src = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<the same commit>" }
mlua-sys = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<the same commit>" }
```

Pin exact versions of shared crates (`mlua`, `rbx-dom`, `serde_json`) to match `Cargo.toml` here.
When developing against a local checkout of both repos, add a path patch in an untracked `.cargo/config.toml`:
`[patch."https://github.com/Anime-Vanguards/roblox-engine-emulator"] roblox-engine-emulator = { path = "..." }`.

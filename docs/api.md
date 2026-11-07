# Rust API

The crate `roblox_engine_emulator`: running the engine from Rust, extending it with a layer, and depending on it.
[engine.md](engine.md) is the contract these types follow.

## Overview

```rust
use roblox_engine_emulator::{Emulator, Engine, Options, mount::ProjectSpec};

let engine = Engine::new(Options { mode: "play".into(), repo: my_checkout, ..Options::default() })?;
let emulator = Emulator::start(engine, &ProjectSpec { roots: vec![shared_root, game_root], ..Default::default() })?;
let call = emulator.run("Probe", "print(workspace.CurrentCamera.FieldOfView) task.wait(1)")?;
emulator.run_until(2.0)?;              // or emulator.step(1.0 / 60.0) frame by frame
for line in emulator.output() { println!("{} {}", line.kind, line.message) }
```

- **Engine** (`src/lib.rs`): one per mounted project, single-threaded (`!Send`). `Engine::new(options)`
  or `Engine::with_layers(options, layers)`; `engine.state` (the dom, signals, scheduler, output,
  project state), `engine.reflection`, `engine.behaviors`, `engine.hooks`; `engine.teardown()` frees
  it after its VM is dropped ([`Emulator`] does both). `Options` are Engine.Start's: viewport, pixel
  scale, mode, place, local player, `repo` (the host's checkout: caches go to its state dir), the
  place prices of the work clock.
- **Mounting** (`src/mount.rs`): `mount_project(lua, engine, &spec, started)` = Engine.Start (`lua::start`)
  plus snapshots, Rojo roots, data roots, mounts, package stand-ins, the player joining, the place's
  Studio hierarchy and manifest bundles. `begin_project` / `Mounting::finish` split it at the player
  joining for a host that adds its own setup there. `launch::parse` / `Args::accept` / `prepare` turn
  command-line engine options into `Options`, roots and mounts.
- **Running Luau** (`src/lua`): `lua::globals::call(lua, engine, function, args)` runs a function on a
  scheduler thread (it may yield; `Call::outcome` tells how it ended so far); `lua::modules::environment`
  gives a script environment, `lua::modules::require` requires a ModuleScript, `replace_source` stands
  a module in; `lua::instance_value` / `lua::instance_id` convert between instances and script values.
- **The clock** (`src/scheduler.rs`): `scheduler::step(lua, engine, Some(dt))`, `run_until(lua, engine, t)`,
  `step_display_duration` (a display's cadence), `flush_deferred`, `use_recorded_clock` (a Studio clock).
- **The 3D world** (`src/world`): CFrame views on parts and attachments, `world::query` (`solids`, `cast_ray`, `cast_box`) and the
  WorldRoot queries, the Camera's projections; `Hooks::world_events` reports emits, clears and sound
  plays (`world::WorldEvent`).
- **Recording** (`src/record`): `Recorder::install(engine, fps)`, `recorder.sample(engine, frame)` after
  each frame, `recorder.write(engine, out, extra)`; `record::run::record(&args, &host)` is `ree record`
  (`record::run::execute` returns the emulator too, for a caller that frees it later or never:
  `record::run::release`).
- **Layers and extensions**: a `Layer` (`fn install(&self, engine: &Rc<Engine>)`) runs during
  Engine.Start, after the core's behaviours, derived views and world and before the DataModel is built:
  it registers behaviours (`engine.behaviors.borrow_mut().add_method(class, name, services::method(..))`,
  `add_getter`, `add_setter`), property and hierarchy hooks (`engine.hooks`) and frame phases
  (`state.sched.add_hook(Phase::Render, ..)`) exactly as the core does. Its per-engine state is an
  `Extension` (`engine.extension_or_insert_with(MyState::default)`, `engine.extension::<MyState>()`),
  whose `teardown` drops what refers back to the engine. rur's GUI layer is the worked example
  (roblox-ui-renderer `native/src/engine/mod.rs`).
- **Bundles**: `engine.set_bundle_source(Box<dyn project::model_assets::BundleSource>)` replaces the
  default asset worker (`project::model_assets::AssetWorker`) as the source of manifest bundles (its
  `cached` says which files it holds without fetching: those are decoded ahead; its `shape` what they
  hold: `BundleShape::Uploaded`, the asset as uploaded, whose one instance the placeholder at the
  bundle's path becomes as the game installs it, or the default `Members`, the children of that
  instance, mounted in a folder at the path).
- **Models**: `project::ModelPlan::new(&reflection, &dom, roots)` converts a decoded model once (a plan is
  a few flat arrays, `Send + Sync`); `plan.instantiate(engine, parent)` makes its instances in any
  engine, as `project::convert_instances` does, and `plan.instantiate_lazy(&mut state, &reflection,
  parent)` (how bundles import) makes its roots and the rest as scripts look at them (docs/engine.md,
  "Project mount"); `instantiate_lazy_as(.., target)` makes a plan's one root as an instance that stood in
  its place (a bundle's placeholder), and `instantiate_lazy_members(.., parent)` that root's children. A process running many engines keeps compiled modules
  (`lua::modules::reuse_compiled_modules`) and converted bundles (`project::model_assets::keep_bundles`).
- **Profiling** (`src/profiler`): `profiler::Config::from_args(args, default_dir)` (or `Config::new(kinds, dir)`)
  and `profiler::Session::begin(config)` before the engine starts; `luau_profile::install(&lua)` hooks the VM
  (it calls `profiler::install`), `scheduler::step` feeds the timeline and `luau_profile::set_phase` marks
  phases; `profiler::collect(&lua)` reads the VM's profile before the VM goes, and `session.finish(Run {..})`
  writes the files and returns the summary. A host counts its own caches with `profiler::caches::named(name,
  about)` and `named_hit` / `named_miss` / `named_evicted`.
- **A program running `ree`'s commands** passes a `launch::Host` (its name, checkout and engine
  constructor): `record::run::cli(argv, &host, true)` is how rur's `rur-native record` runs the recorder
  with its own engines.

## Use from another crate

```toml
[dependencies]
roblox-engine-emulator = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<commit>" }

# The patched Luau VM and the mlua-sys that reads its bytecode: only the workspace root's [patch] applies, so the
# dependent crate repeats them
[patch.crates-io]
luau0-src = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<the same commit>" }
mlua-sys = { git = "https://github.com/Anime-Vanguards/roblox-engine-emulator", rev = "<the same commit>" }
```

Pin exact versions of the crates you share with the engine (mlua, rbx-dom, serde_json) as Cargo.toml
here does. To build against a local checkout while changing both, patch the git source with a path (an
untracked `.cargo/config.toml` of the dependent:
`[patch."https://github.com/Anime-Vanguards/roblox-engine-emulator"] roblox-engine-emulator = { path = "..." }`).

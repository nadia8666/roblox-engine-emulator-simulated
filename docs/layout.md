# Source layout

Where things are in the source tree.

```
src/lib.rs             Engine, Options, State, Hooks, Layer, Extension, compiled-in data
src/alloc.rs           the long-lived heap of what a warm service keeps (`ree`'s allocator: src/main.rs)
src/emulator.rs        Emulator: an engine, its VM and its project
src/launch.rs          command-line engine options -> Options, roots, mounts; Host
src/mount.rs           the project mount
src/dom.rs reflection.rs value.rs signal.rs instance.rs derived.rs output.rs
src/scheduler.rs       virtual clock, task library, frame phases, recorded Studio clocks
src/cost.rs            the work clock's prices
src/services/          per-class behaviours (DataModel, RunService, TweenService, queries, ...)
src/project.rs project/   Rojo mounting, models and snapshots, Studio-only placeholders, bundles,
                       model plans and lazy imports (project/plan.rs), their files (plan/codec.rs),
                       Rojo's JSON models (project/json_model.rs)
src/serve/             `ree serve` and its client (`--service`)
src/timing.rs files.rs `--profile`'s phases; file stamps for the caches
src/watchdog.rs        Roblox's script timeout, the wall-clock cap, the exit when the parent is gone
build.rs               the conversion digest: the key of the bundle plan files (`<bundle>.plan`)
src/lua/               the Luau-facing API: Instance userdata, datatypes, globals, require, stubs
src/world/ src/record/ the 3D world and the recorder
src/animation/         AnimationTracks: the Animator's step, clips (KeyframeSequences) and where they load from
src/meshes.rs          EditableMesh: meshes scripts build or load, their MeshParts' FileMesh snapshots
src/profile.rs microprofile.rs luau_profile.rs   simulation profile, MicroProfiler scopes, Luau sampler
src/profiler/          the profiling suite (`--profile <kinds>`): the VM's instruction profile, memory, caches,
                       the timeline, the CPU, the report's files and `ree profile diff`; hw/ the hardware
                       counters' client (the counter service's wire format, readings, the counters by name and their
                       derived figures, the readings around sampled calls and how counts are booked)
src/trace.rs                                     Context.TraceWrites: Luau call sites of Parent / property writes
src/census.rs                                    Context.Census: live threads, connections, timers, instances by their Luau line
data/                  reflection.json (Studio's dump; tools/dump-reflection.luau), font-enums.json, text metrics
vendor/ patches/ tools/vendor.mjs   the patched Luau VM and Lune datatypes
tools/luau-cost/       the work clock's Luau calibration (run in ree and in Studio, fitted by fit.py)
tools/hw-counters/     the counter service `--profile hw` reads through (a crate of its own) and setup.ps1, its install
profile-view/          the profile viewer, `ree profile view|render` (a crate and binary of its own, reading profile
                       folders): the terminal UI, its views and issue detectors, and the PNG, SVG and text renderer;
                       fonts/ the bundled JetBrains Mono
tools/what-if/         variant files for `ree profile try`, with what each shows
tools/profile-demo/    a harness written slowly on purpose, to try the profiler and its viewer on (README "Profiling")
tests/                 the patched VM's tests and the process guards
docs/engine.md         the contract: architecture, rules and formats
```

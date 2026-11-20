# Source layout

Directory and module overview for `roblox-engine-emulator`.

```text
src/
  lib.rs               Engine, Options, State, Hooks, Layer, Extension, compiled-in data
  alloc.rs             Long-lived mimalloc heap for warm-service shared state (allocator in main.rs)
  emulator.rs          High-level Emulator wrapper owning Engine, Lua VM, and mounted project
  launch.rs            CLI engine option parsing -> Options, roots, mounts; Host trait
  mount.rs             Project mount orchestration
  rojo.rs              Rojo project files (`--rojo`): a project's tree read into mounts, shared with roblox-ui-renderer
                       (end-to-end tests in rojo_tests.rs)
  dom.rs               Instance arena/DOM
  reflection.rs        Studio reflection database lookup & property metadata
  value.rs             Roblox property Value representation
  signal.rs            Deferred signal queues and connections
  instance.rs          Instance methods and lifecycle
  derived.rs           Derived property views
  output.rs            Structured output & diagnostics log
  scheduler.rs         Virtual clock, task library, RunService frame phases, recorded Studio clocks
  cost.rs              Work-clock instruction and engine-call prices
  services/            Per-class Roblox service and instance behaviors
  project.rs, project/ Rojo mounting, models, snapshots, Studio placeholders, bundles,
                       ModelPlan & lazy instantiation (plan.rs, plan/codec.rs), JSON models (json_model.rs)
  loopback/            `--run-context loopback`: paired server + client replication and remotes, their C-API copier, lazily
                       paired Studio-only instances (tests in loopback_tests.rs; tests/loopback_alloc.rs, tests/loopback_bench.rs
                       and tools/bench/loopback/ hold its allocation guard and bench)
  serve/               `ree serve` daemon and `--service` client
  timing.rs, files.rs  `--profile` phase timers and cache file stamps
  watchdog.rs          Script timeout, wall-clock cap, and parent-death watchdog
  lua/                 Luau-facing API: Instance userdata, datatypes, globals, require, Luau stubs
  world/               3D world transforms, spatial queries, Camera projections
  record/              3D scene & timeline recorder (`ree record`)
  animation/           AnimationTracks, Animator stepping, KeyframeSequence & CurveAnimation loaders
  meshes.rs            EditableMesh and FileMesh snapshot export
  profile.rs           Simulation profiler (plus microprofile.rs, luau_profile.rs)
  profiler/            Profiling suite (`--profile <kinds>`), HTML/JSON reports, `ree profile diff`,
                       and `hw/` hardware-counter service client
  trace.rs             Context.TraceWrites (Luau call sites of Parent/property writes)
  census.rs            Context.Census (live threads, connections, timers, instances by source line)

build.rs               Computes conversion digest keying cached `<bundle>.plan` files
data/                  Compiled-in Studio reflection dump (reflection.json; tools/dump-reflection.luau,
                       tools/reflection-diff.mjs), font enums, text metrics, and the canonical normalization table
                       utf8.nfcnormalize / nfdnormalize read (unicode-normalization.rs; tools/unicode-normalization.mjs)
vendor/, patches/      Vendored patched Luau VM, mlua-sys, and Lune datatypes (rebuilt by tools/vendor.mjs)
profile-view/          Separate workspace crate & binary (`ree-profile-view`) for terminal UI and
                       PNG/SVG/text profile rendering (bundles JetBrains Mono under fonts/)
tools/
  cost-parity/         Work-clock parity benchmark against real Roblox clients, fits, reference sessions
  luau-cost/           Historical Studio-playtest work-clock fit
  hw-counters/         Separate Windows service crate (`ree-hw-counters`) and setup.ps1 for `--profile hw`
  what-if/             Variant benchmark scripts for `ree profile try`
  profile-demo/        Intentionally slow demo harness for trying the profiler (see README)
tests/                 Patched-VM tests (`luau_vm.rs`), parity runner (`parity.rs`), process guards
docs/engine.md         Public engine contract: architecture, rules, and formats
```

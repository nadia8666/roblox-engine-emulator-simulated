# The engine: architecture and contract

This is the contract code builds against: what each part of the engine does, the rules it follows and
the formats it reads and writes. Change it in the same commit when an interface or a rule changes. The
README and its documentation index ([building.md](building.md), [cli.md](cli.md), [api.md](api.md)) cover
building, the command line and the API; this file covers how the engine works inside.


The engine began as the native rewrite of a Luau emulator inside
[roblox-ui-renderer](https://github.com/Anime-Vanguards/roblox-ui-renderer) (rur) and was split out of
it in October 2026. rur still builds its UI previews on it and adds the GUI side (layout, text, input,
scene export) as a layer; rur's own docs are the contract of that side.

## Why

Roblox runs its engine in C++. Running a game's own UI and 3D code offline needs an engine that scripts
cannot tell apart from Roblox's. The first emulator was written in Luau; on its benchmarks the Luau
thread spent about 97% of its time in the emulator and 3% in game code, so the engine became native
(Rust), with the game's Luau still running in a Luau VM (mlua) and seeing Instances as userdata, as in
Roblox.

## Principle: parity first, then speed

Everything a script can see follows Roblox Studio as measured, else Roblox's documentation, and the code
says which. A comment that states a rule cites the measurement (the Studio version or date, the scene,
the numbers) or the documentation page. Nothing is guessed silently: a member that is not emulated
reports itself in the run's Output (kind `missing`) instead of returning a made-up value.

Speed comes second and never changes output: hot paths read properties by interned id, allocate nothing
per frame for whole scenes, and keep the Instance API's common operations in C metamethods ("Instance
API").

## Shape

```
src/lib.rs           Engine, Options, State, Hooks, Layer, Extension; data (the compiled-in dump and fonts)
src/emulator.rs      Emulator: an engine, its Luau VM and its project, dropped in the right order
src/launch.rs        command-line engine options (Args, accept, parse, prepare) and Host
src/mount.rs         the project mount: begin_project / Mounting::finish, mount_project
src/dom.rs           arena of instances: generational InstanceId, class, name, parent, ordered children,
                     properties (their own sorted vector, or a plan node's, read through its lazy import
                     until written), attributes, tags, flags, change versions for invalidation
src/reflection.rs    classes, properties, events, methods, defaults: data/reflection.json (Studio's dump)
                     merged over rbx_reflection_database
src/value.rs         property values: rbx-dom's Variant plus engine-only kinds (Font, TweenInfo, ...)
src/signal.rs        RBXScriptSignal / Connection, Changed / GetPropertyChangedSignal, ChildAdded, ...;
                     deferred signal behaviour as measured
src/scheduler.rs     virtual clock, task library, Heartbeat / RenderStepped / Stepped, frame phases,
                     recorded Studio clocks
src/cost.rs          the work clock's prices: what script work costs in Studio
src/instance.rs derived.rs output.rs lune_json.rs profile.rs microprofile.rs luau_profile.rs
src/services/        per-class behaviours (mod.rs; tween.rs TweenService, query.rs QueryDescendants,
                     encoding.rs EncodingService, reflection_service.rs ReflectionService)
src/project.rs project/   Rojo mounting, models and snapshots, Studio-only placeholders, asset manifest
                     bundles (model_assets.rs), model plans (plan.rs, plan/codec.rs, plan/store.rs), Rojo
                     JSON models (json_model.rs)
src/lua/             the Luau-facing API: Instance userdata (raw.rs), datatypes, globals, require, Actors,
                     Luau stubs (services.luau, server.luau, prelude.luau, vector3.luau)
src/world/ src/record/   the 3D world and the recorder
src/animation/       AnimationTracks: the Animator step, clips, keyframe loading
src/meshes.rs images.rs  EditableMesh and EditableImage
src/serve/           `ree serve` (a warm process running record / run jobs) and its client (`--service`)
src/watchdog.rs      the script timeout, the wall-clock cap, the exit when the parent is gone
src/trace.rs census.rs lua/holders.rs   Context.TraceWrites, Context.Census, Context.Holders
src/timing.rs files.rs alloc.rs   `--profile` timings, file stamps for caches, the long-lived heap
src/main.rs          `ree`
build.rs             the conversion digest (REE_CONVERSION_DIGEST): the key of `<bundle>.plan` files
vendor/ patches/ tools/vendor.mjs   the patched Luau VM and Lune's datatypes
```

## Verification

- `cargo test --release`: the engine's unit tests (script-API tests in src/tests.rs, the scheduler,
  datatypes, the world, the recorder, model bundles, the Emulator) and the patched VM's
  (tests/luau_vm.rs), plus the process guards (tests/process_guards.rs, which run the built `ree`).
  Tests build their own scenes in temporary directories; none needs Roblox Studio, a game checkout, a
  GPU or the network.
- `node tools/vendor.mjs --check`: vendor/ is exactly the pinned archives with patches/ applied (the VM's patches are a
  topic series, patches/luau/; docs/building.md "Updating Luau").
- The VM runs with the Luau flag values a live Roblox client serves (data/luau-flags.json, refreshed by
  `node tools/luau-flags.mjs`; patches/luau/14-client-flags.patch sets them before the first VM starts or chunk
  compiles). Roblox ships Luau changes behind those flags, so upstream's defaults would run another Luau.
  `REE_LUAU_FLAGS=off` keeps upstream's defaults; `REE_LUAU_FLAGS=FFlagName=true,FIntName=3` changes some on top.
  (mlua's `luau-jit` feature, the `jit` build's, turns LuauEmitCallFeedback and LuauPromoteProto on itself, as the
  client has them: `off` leaves those two on.)
- Recordings are deterministic: the same harness and project give byte-identical `scene.rbxm`,
  `playback.json`, `events.json`, `camera.json` and summary files. Two runs of one scene make the same
  writes: tables keyed by Instances, tables, functions or threads are visited in an order fixed by the
  objects' creation, not their addresses (patches/luau/04-deterministic-hash.patch, `REE_HASH_SEED`).
- Downstream: roblox-ui-renderer runs its fixtures, stories and Studio comparison replays through this
  engine and compares their output with recordings made in Studio. A change to script-visible
  behaviour is checked there too; an intended change of output is re-recorded only after a comparison
  shows it is right in Studio.

## Layers and extensions

The core never calls a GUI: hosts add one. `Engine::with_layers(options, layers)` keeps the host's
`Layer`s; `lua::start` (Engine.Start) installs the core's service behaviours, derived views, world and
static GUI behaviours (`services::install_gui_behaviors`), then each layer's `install` in order, then
builds the DataModel, so instance numbering, behaviour lookup and frame-phase hook order are the same
with or without a layer.

- A layer registers what the core registers: behaviours (`engine.behaviors`: `add_method`,
  `add_getter`, `add_setter`, with `services::method` / `services::getter` wrapping closures; looked up
  through the superclass chain, the last registration of a (class, name) wins), property, hierarchy and
  instance-freed hooks (`engine.hooks`), and frame phases (`state.sched.add_hook(Phase::Render, ..)`).
- Its state is an `Extension` (`engine.extension_or_insert_with(T::default)` returns `&T`; a few types
  per engine). `Engine::teardown` calls each extension's `teardown` where the engine drops its own
  parts, so state that captures the engine (closures, Lua values) is released.
- roblox-ui-renderer's GUI layer is the worked example: TextService's measurements, the layout
  (Absolute* / TextBounds / AbsoluteContentSize / AbsoluteCanvasSize, CanvasPosition clamping, the
  Render-phase pass) and input (UserInputService state, GuiState, scrolling, TextBox editing, drag).
  Without it those members keep the core's static answers (GetMouseLocation (-1000, -1000), GuiState
  Idle), TextService:GetTextSize answers from Studio's measured advances (src/text_measure.rs), and the
  rest is reported as not emulated.

## Project mount (`src/mount.rs`, `src/project.rs`)

`mount_project(lua, engine, &ProjectSpec { roots, snapshots, snapshot_dir, mounts, model_assets },
started)` runs Engine.Start, then mounts, in order: Studio snapshots (`<Service>.rbxm` into their
services), the Rojo roots, the data roots, explicit mounts, package stand-ins (lua/packages: a stand-in
for the UI Labs package's data shapes), the local player joining, the place's Studio hierarchy
(Studio-only placeholders) and the manifest bundles. The roots and data roots are listed on the thread
pool while Engine.Start runs (`project::Prescan`). `begin_project` stops after the player joins and
`Mounting::finish` does the rest, so a host can add its own setup in between (rur loads its story host
there).

- **Rojo roots**: `<root>/<Service>/...` with `init.luau`, `*.server.luau`, `*.client.luau`,
  `init.meta.json`, JSON modules and `*.model.json` models. A directory holding `default.project.json`
  mounts as that project's tree (its `$path`, `$className` and child nodes; Wally packages ship one
  each), ahead of any init script, named by the project's `name` (else the directory's), as Rojo nests
  it; a directory mounted itself (by `--mount`, `Context.Mount` or a `$path`) keeps that name rather
  than the project's. A project file can come from a third-party package, so a `$path` stays inside the
  folder holding the file: it is looked up only in that folder's listing, and one that is absolute (a
  leading `/` or `\`, a UNC path or a drive such as `C:`) or has a `..` segment is refused with a
  warning. On a tree's root, a `$path` that is refused or names nothing leaves the directory mounted as
  it is, as does a project file ree can't read (not JSON, or no `tree` object); on a child node, the
  node is left out. Each is reported, except an absent `{ "optional": ... }` target. A project inside
  more than 32 others, or past the 10000th of one mount, is left an empty Folder, with a warning. A
  root's folder is the project folder that the conventions below are read from. A link to a
  directory (a symlink or a Windows junction) mounts what it leads to, unless it leads back to a
  directory on the way down to it: that directory would hold itself without end, so the link is left
  out with a warning (place content's folders, below, are walked the same way).
- **Project settings**: `rur.json` at the project folder, keyed by the root folder's name (the first
  root with a value wins): `placeIds`, `modelAssets` (`mount`, `prefixes`), `clientGui`,
  `streamingTargetRadius`. The name is kept from rur, which wrote these files first.
- **Studio hierarchy snapshot**: the project folder's `studio-places.json` lists its places,
  `{"environments": [{"name": "<Place>", "projectFile": "<root>/default.project.json",
  "studioPlaceId": <id>}]}`; a root that is the folder of a place's project file belongs to that place,
  and `studio-snapshots/<Place>.json` (`{"nodes": [{"name", "className", "parent"}]}`, a node's parent an
  index into the list, -1 for the DataModel) is its hierarchy as Studio holds it. Instances the snapshot
  lists that no root or model provides become Studio-only placeholders, made when a script first looks
  up or lists children, so `WaitForChild` and `FindFirstChild` see what Studio would. A root's directory
  that names no class of its own (no init script, no `init.meta.json` className) takes the class the
  snapshot gives its path when that is not a Folder, a script or a service: a directory that holds only
  the scripts of a Part the place keeps is that Part (Studio-only: its properties are the class's
  defaults unless place content stores it, and then the stored instance keeps the directory's scripts).
- **Place content**: a snapshot lists non-script content by name and class only. Files at
  `<repo>/data/place-content/<Place>/<parent path>/*.rbxm` (Studio's own serialization of those
  instances) replace the placeholders of the same names under that parent with the stored instances,
  and the snapshot adds nothing below them.
- **Data roots**: the project folder's `data-roots.json`, `{"paths": ["<path>", ...]}`, names folders
  that checkouts at `<project>/external/<Name>/src` hold; each one found mounts as a root.
- **Studio names**: the project folder's `studio-name-map.json`, `{"files": {"<root>/<Service>/.../<file>":
  {"studioPathParts": [...]}}}`, gives the Studio names of files whose names a file system cannot hold
  (a colon, a slash, a trailing dot). A server mount renames each mounted instance on such a path whose
  name differs; a client mount keeps the file names.
- **Local files**: content naming a relative local path (`output/x.mesh`, `file://<path>`) is read from
  the project's folder (`Options::project`: `--project`, else the folder holding the first root, else
  the current directory), by the same rules ror uses for what names a file (`Options::local_file`).
  Studio reads no local files; this lets a project use meshes and images before they are uploaded.

Model files (snapshots, models, bundles) convert through `project::ModelPlan` (project/plan.rs): the
structure and property values once, kept as a few flat arrays (nodes, properties, distinct values, the
text of names and strings, attributes, tags, unmodeled values) without shared pointers, so a plan is
`Send + Sync`; `instantiate` makes the instances (creation order, names, properties, attributes, tags,
sources). `instantiate_lazy` makes a plan's roots and the rest as scripts look at it: each made
instance's children are made when a script first finds or lists them, a lazily made instance reads its
properties through the plan until a write gives it its own, and before a script moves or destroys one,
or calls a method that may look below it (`plan::before_method`), the rest of its import region is
made, so events, methods, joints and the recorder see the whole tree. Everything script-visible is as
if the import were made at once: serials (`GetDebugId`), child order, references and `GetTagged`.

Rojo's JSON models (`*.model.json`, project/json_model.rs) mount as Rojo 7 builds them: one instance
named after the file, `className`, `name`, `properties`, `attributes`, `children` (lowercase or Rojo's
older capitalized keys), property values explicit (`{"Vector3": [1, 2, 3]}`) or implicit (resolved by
the property's type in rbx-dom's database), round-tripped through rbx_binary as `rojo build` writes
them. A file Rojo would reject is reported as a warning and not mounted.

## Asset bundles (`src/project/model_assets.rs`, feature `bundles`)

A project can name model assets in an asset manifest: the project folder's `asset-manifest.luau`, a
Luau module returning a table with one bundle a line,

```lua
return {
	["Models/Props"] = { AssetId = 1234567890, Version = 3 },
	["Models/Props/_Loose"] = { AssetId = 2345678901, Version = 1 },
	["Models/Props/Tall Lamp"] = { AliasOf = "Models/Props/Lamp" },
}
```

`rur.json` `modelAssets` (or `ree record --assets-at <Service.Path> --assets <prefix>,...`) chooses
where they mount and which (`*`: every one). Only the folders are mounted at startup, with a
placeholder where each bundle goes. A bundle imports when a script finds, tests or touches its
placeholder or looks inside it: the placeholder becomes the bundle's one instance in place (its class,
properties, attributes, tags and children; its name stays the path's last segment), so a script that
holds it holds the instance. A `_Loose` bundle's members go into its parent folder. A bundle file
without exactly one instance at its top mounts its instances in a folder at its path, with a warning.
An `AliasOf` line (an alias of an alias followed) is a copy of the Model at the path it names, made the first
time a script asks for the alias's path by name (FindFirstChild, WaitForChild or an index, its folders too):
the bundle holding the model imports, and the copy, named as the alias's last segment, goes into the alias's
folders (a bundle's own folders when the path runs through one). A listing makes no alias, and an alias of
anything but a Model, or of a path no bundle holds, is not made, with a warning.

Bundles come from a `BundleSource` (`catalog`, `bundle`, `cached`, `shape`), which a host sets with
`engine.set_bundle_source`. The default is `AssetWorker`: a child process (`REE_ASSET_WORKER` names the
executable) that answers one JSON request a line on its stdin with one JSON reply a line on its stdout:

- `{"command": "catalog", "project": "<project folder>", "manifest": "<manifest file>"}` ->
  `{"bundles": ["Models/Props", ...]}`
- `{"command": "asset", "uri": "rbxassetid://<id>?version=<v>", "out": "<file>"}` -> `{"file": "<path>"}`
  (the asset as uploaded, written to `out`)
- a reply with `"error": "<text>"` is a failure; ree passes the worker nothing but the requests and
  `--offline`, and the worker handles any authentication the downloads need itself.

Bundle files are kept in the state dir as `model-bundles/asset-<AssetId>-v<Version>.rbxm`, named by
what they hold, so a file that exists is current and one file serves every project that pins that
version. Each file's converted plan is written beside it (`<file>.plan`, keyed by build.rs's conversion
digest and the file's stamp) and mapped in place where the system allows (Windows), so concurrent runs
share its pages. The bundles a run imported (`recent-*.txt`) are decoded ahead on other threads at the
next mount of the same scene. Tests use their own `BundleSource`, so none needs a worker.

## Engine API (`src`)

One `Rc<Engine>` per mounted project, owned next to its `mlua::Lua` (drop the Lua first, then
`engine.teardown()`; `Emulator` does both). Everything is single-threaded; `Engine` is `!Send`.

```
lib.rs          Engine { reflection: Arc<Reflection>, options: Options, state: RefCell<State>,
                         hooks: RefCell<Hooks>, behaviors, lua: RefCell<LuaRefs>, modules, layers,
                         extensions }
                State { dom: Dom, signals, sched: Scheduler, output, project, collection }
                engine.world() -> World { game, workspace, camera, local_player }
reflection.rs   Reflection (compiled in, shared per process: Reflection::shared()), K (pre-interned names)
value.rs        Value, Str, FontValue, ContentValue; JSON / rbx-dom default decoding
instance.rs     script semantics without Luau calls: create, set_parent (event order), raw_set /
                raw_set_silent / notify_changed, clone, attributes, tags, fire
derived.rs      derived property views (Font of FontFace, Transparency, *Content, a part's BrickColor of Color, ...)
                and the Changed events a part fires beside a write whatever their values, as a playing client
                fires them (Position, AssemblyCenterOfMass, Orientation, Rotation beside CFrame; AssemblyMass and
                Mass beside Size; BrickColor before Color; ...)
scheduler.rs    Scheduler data and the Luau-running steps (flush_deferred, resume, step, run_until)
```

- **Reading instances**: `let state = engine.state.borrow();` then `state.dom.at(id)` (class, serial,
  name, parent, children, props, attributes, tags, flags, versions). Properties by interned id, never by
  string: `state.dom.raw_get(id, K::Size as NameId)` (the stored value, else the class default), then
  `Value::as_udim2()`, `as_vector2()`, `as_color3()`, `as_number()`, `as_bool()`, `as_enum()`,
  `as_instance()`, `as_str()`. Names every hot path reads are pre-interned in reflection.rs
  `known_names!`; other names go through `reflection.name_id("X")` once at setup. Resolve classes and
  enum items once (`reflection.class_id`, `reflection.enum_item`) and compare ids.
- **Tree**: `dom.children_of(id)`, `dom.parent_of(id)`, `dom.descendants(id)` (GetDescendants order).
  Studio-only placeholders and the children of lazily imported instances exist once materialized
  (`project::materialize`, `project::materialize_lazy_subtree`); engine code that hands a script a child
  it found calls `lua::instance::reached` (or `find_child_where` for class scans) so a bundle's
  placeholder becomes its instance first.
- **Writing computed values**: `instance::raw_set(&mut state, &reflection, &hooks, id, name, value)` fires
  Changed / GetPropertyChangedSignal (deferred); `raw_set_silent` stores without events. Both bump
  `Instance.version`.
- **Invalidation**: `engine.hooks.borrow_mut().property_changed` and `hierarchy_changed` (instance, old
  parent, new parent) run after a change's signals are queued; `property_unchanged` runs for a `raw_set`
  that stored the value the property already held (no signals). `Instance.version` changes with every own
  property, attribute or tag change, `child_version` with its child list, `Dom::structure_version` with
  any parenting. A hook receives `&mut State` and must not borrow `engine.state`.
- **Frame phases**: `state.sched.add_hook(Phase::Render | PreRender | Tween | RenderStep | Heartbeat |
  Simulation, ..)`. Hooks run Luau, so they must not hold a `state` borrow across script calls.
- **Behaviours**: `add_method(class, name, Rc::new(|lua, engine, id, args| -> Ret))`, `add_getter`,
  `add_setter` (call `engine.clear_member_cache()` after changing them). Rust functions return
  `Ok(values([..]))`, `Err(Flow::Error(ScriptError::at_caller(msg)))` (the script sees `msg` at its own
  line, like Luau's `error(msg, 2)`), `ScriptError::plain` (no position) or `Err(Flow::Yield)` after
  registering the thread's resumption. Luau stubs register with `R.Method(className, name, function)` in
  lua/services.luau.
- **Values to and from Luau**: `lua::convert::to_lua`, `from_lua`, `coerce` (assignment rules and
  errors); `lua::instance_value(lua, engine, id)` (the one userdata of an instance) and
  `lua::instance_id(&LuaValue)`.
- **Assignment** (`convert::coerce`, measured in a playing client): a number property takes any value, a
  string as Luau reads a number and anything else as 0; `int` properties truncate (the minimum when out
  of range or not finite), `int64` ones round half away from zero (0 when out of range); a boolean
  property takes Lua truth; a string property takes strings and numbers. A Vector3 or CFrame property keeps
  the script value's float32s as they are (a client reads back `CFrame.Angles(0.001, 0, 0)` exactly). A part's
  CFrame, a joint's C0 / C1 and a BulkMoveTo keep the rotation while every entry of RᵀR - I is within 1e-5, and
  otherwise store it as `CFrame:Orthonormalize()` gives it (identity for a zero column); a model's WorldPivot keeps
  any rotation (`world::frame::stored_rotation`, measured in a playing client).
- **Models as a client receives them** (`project::received`). A playing client gets models, snapshots and bundles
  from the server, so ree loads every stored rotation as the client reads it after the server's load and the
  network. This was measured in a playing client on 2,347 rotations of the game's bundles: 2,328 come out to the
  bit, 18 bones within 5e-7, and one pivot whose quaternion had near-equal components 1.7e-4 off.
  - **The server's load.** It stores a part's CFrame as a script's write does (orthonormalized past 1e-5), and
    orthonormalizes a joint's C0 / C1 and an attachment's or bone's CFrame. It keeps a PivotOffset or a model's
    pivot as stored.
  - **The network.** A rotation that is not axis-aligned then crosses as a 15-bit quaternion: the largest
    component is dropped, and the other three, times √2, cross in steps of 1/16383. So the client's rotation is
    orthonormal to float precision, and within about 1e-4 of the server's.
  - **What crosses exactly.** Positions and axis-aligned rotations. A `Clone` also copies a rotation exactly.
- **Scripts' chunk names**: a script or module compiles as a chunk named by its full name, as Roblox
  names it: error positions, tracebacks and `debug.info(f, "s")` read `ReplicatedStorage.Module:3:`
  (measured in a playing client; tools/parity/probes/chunknames.luau). Roblox keeps one compiled chunk per
  source text, so a module whose source equals an earlier one's carries that module's name; ree does not.
- **The Luau library**: the VM is Luau 0.746 (`vendor/luau0-src`, the release a live client runs) with the topic
  series in patches/luau/ and the client's flag values. Its `math.isnan`, `math.isinf` and `math.isfinite` (Luau's
  since 0.701) are builtins (fast calls, a constant argument folded by the compiler) that take a number or a string Luau reads as one (`"1"`, `"inf"`,
  `"nan"`, `"1e400"`) and raise `invalid argument #1 to 'isnan' (number expected, got nil)` for anything
  else, as a playing client does (tools/parity/probes/math_classify.luau).
- **CFrame:FuzzyEq(other, eps = 1e-5)** (measured in a playing client, Studio 0.744: about 25,000 pairs at its
  thresholds; tools/parity/probes/cframe_fuzzyeq.luau): single precision, eps taken as a float and its size
  used. Position component by component: self's `a` and the other's `b` are equal, or `|a - b| <= |eps| *
  (|a| + 1)` (the tolerance grows with self's component only; `|eps|` once `|a| + 1` is infinite). Rotation:
  `sqrt(sum of (Ra^T Rb - I)^2) / sqrt(2) <= |eps|`, the relative rotation's column dot products and the sum
  taken in order without a fused multiply-add (the angle between them for small turns; for matrices that are
  not rotations, their distance: a stretched axis passes up to `sqrt(2) * eps`). An eps over 0.1 in size raises
  `The eps value provided to FuzzyEq should be a small positive value <= 0.1` (NaN does not, and compares
  false); a numeric string is an eps. The argument errors are the client's: `invalid argument #1 to 'FuzzyEq'
  (CFrame expected, got number)`, `invalid argument #2 (CFrame expected, got nil)` and `#3 (number expected,
  ...)` at the calling line (an eps of nil given is an error), `Invalid number of arguments: 1, CFrame.FuzzyEq
  expects 2 or 3.` without one.
- **Lifetime**: instances a script can reach keep their userdata identity; unreachable parentless
  subtrees are freed by `lua::raw::collect` (run from the render step). Rust code that holds an
  `InstanceId` outside the tree across frames keeps it reachable with `state.dom.pin(id)` / `unpin(id)`,
  queued work (scheduler job and timer arguments are scanned) or an instance property. Caches keyed by
  id drop entries in `hooks.instance_freed` (or check the generation: `dom.get(id)` of a freed id is
  `None`).

## Instance API (raw userdata, `src/lua/raw.rs`)

Instances are tagged Luau userdata (a 16-byte payload: slot index, generation and a pointer to the API
table) whose metatable holds C functions: `__index`, `__newindex`, `__namecall` (with a continuation),
`__tostring`, `__type`, the locked `__metatable` and `__iter`.

- Keys resolve through Luau string atoms: a string maps to its member `NameId` once, and members
  resolve once per (class, name) into a dispatch entry.
- Fast paths without mlua or Luau: plain property reads (primitives, strings, instances, enum items,
  datatypes through a per-(instance, property, version) cache), Name / Parent / ClassName, children by
  name, events, plain property writes, the base Instance methods (FindFirstChild, IsA, GetChildren,
  GetDescendants, FindFirstChildOfClass / WhichIsA, FindFirstAncestor*, IsDescendantOf / IsAncestorOf,
  GetPropertyChangedSignal, Get/SetAttribute, GetFullName, WaitForChild hits, HasTag),
  BindableEvent:Fire and `Instance.new`.
- Everything else calls mlua functions from C. Errors are raised after every Rust value is dropped,
  as Roblox raises them (measured in a playing client, tools/parity/probes/errors.luau): the engine's
  own member errors carry no script position (`NotAMember is not a valid member of Part "Part"`,
  `Unable to assign property ClassName. Property is read only`, `'X' is not a valid Service name`,
  require's errors); errors Luau's own checks would raise keep the calling line's (`invalid argument
  #3 (Instance expected, got number)` for a bad Parent, `attempt to call a nil value`).
- What a key is: a property scripts can read (Studio's dump permits Read; NotScriptable ones such as
  PVInstance.Origin permit nothing), else a behaviour's getter, method, event or callback, otherwise
  the child of that name (so `part.Origin` finds a child named Origin, as in Roblox). Callbacks a game
  script may assign (BindableFunction.OnInvoke, RemoteFunction.OnClientInvoke / OnServerInvoke,
  MarketplaceService.ProcessReceipt, ...) are kept; a read raises Studio's message.
- Member names: a key that names no member and no child, and starts with a lower-case ASCII letter, reads
  the member it spells with that letter upper-cased (`part.anchored`, `part.touched`,
  `part.getPropertyChangedSignal`: the same value, signal and function as the exact name; a callback's
  read error names the callback), and a method call that names no method calls the method so spelled
  (`part:getPropertyChangedSignal("Position")`). Only the first letter (`HttpService:jSONEncode` answers,
  `jsonEncode` does not), only towards upper case, members only (children answer to their exact names),
  and the API dump's own lower-case names (`findFirstChild`, `getService`, `brickColor`, `childAdded`) are
  members of their own, found first. Each is the member it stands for (the same name with the first
  letter upper-cased, or Players' `players` / `playerFromCharacter`): a property alias reads and writes
  that member's value and its Changed names it (`BrickColor`); a method alias does its work through a
  function of its own; `childAdded` and a value object's `changed` are signals of their own that fire with
  ChildAdded and Changed; `game.lighting` is the Lighting service. A write takes the exact name only (`part.anchored = true` raises `anchored is
  not a valid member of Part "Part"`). A method call finds methods only: a property, event, callback or
  child by either spelling raises the same not-a-member error. Measured in a playing client and a server
  (every method, event and readable property of the API dump answers so on reads, none on writes;
  tools/parity/probes/camelcase.luau). Datatypes, enums, libraries and globals have no such rule: each
  datatype answers the lower-case names it lists and nothing else (Vector3 and Vector2 `x` / `y` / `z`,
  `magnitude`, `unit`, `lerp`; CFrame `p`, `x` / `y` / `z`, `lookVector` / `rightVector` / `upVector`,
  `components`, `inverse`, `lerp`, `toEulerAnglesXYZ` / `YXZ`, `toEulerAngles`, `toOrientation`, `toAxisAngle`,
  `toObjectSpace` / `toWorldSpace` and the point and vector forms; Color3 `r` / `g` / `b`, `lerp`; UDim2
  `lerp`; Ray `unit`; Vector3int16 / Vector2int16 `x` / `y` / `z`; BrickColor `name`, `number`, `r` / `g` / `b`
  (not `R` / `G` / `B`); RBXScriptSignal `connect`, `wait`, `connectParallel` (not `once`); RBXScriptConnection
  `connected`, `disconnect`), enums and libraries answer exact names (`BrickColor.New` is the library's own).
- What holds an instance is what holds it in Roblox (measured in Studio 0.740): a connected handler that
  references the instance keeps it; Destroy (which disconnects the instance and its descendants,
  deferred) or Disconnect releases it; a destroyed tree nothing references is freed whole.
- Destroy queues each instance's Destroying handlers like any deferred event, just before that
  instance's removal events and after its parent's (measured in a playing client): they run after
  Destroy returned and see the instance unparented and its Parent locked.
- Signals and connections reuse slots; a connection id is (slot, generation), so a stale
  RBXScriptConnection value or queued handler never reaches the slot's next connection.
- BindableEvent:Fire and BindableFunction:Invoke copy what they pass as Roblox does (measured in a
  playing client, tools/parity/probes/signals.luau; `src/lua/bindable.rs`): tables once when sent and
  again for each handler, waiting thread or caller, without metatables; a table with `#t > 0` as the
  array 1..`#t`, any other as a dictionary with string keys (`1.5` -> `"1"`, an Instance key
  `<Instance> (Name)`); boolean and thread keys and cyclic tables are errors; threads arrive as nil and
  buffers as copies.

## Scheduler API (`src/scheduler.rs`)

A playing client's frame (`--mode play`, measured in a real client): wait / delay / spawn threads resume
(then a full flush of deferred work), PreAnimation, Stepped / PreSimulation, PostSimulation, every due
task.wait / task.delay thread in the order they were scheduled (then one pass over the deferred queue:
what that work defers waits), Heartbeat, then the render step (BindToRenderStep callbacks, RenderStepped /
PreRender, the tween step, then one resumption point: the tween step's events first, then what the
callbacks and handlers deferred, a full flush). A RenderStepped or PreRender handler reads a tween's value
from before the frame's step, and a tween played there takes its first step that frame. Edit mode keeps
Studio's edit order (a flush after each resumption; the tween step right after the callbacks).

`scheduler::step(lua, engine, Some(dt))`, `run_until(lua, engine, t)`, `step_display_duration(d)` /
`step_display(n)` (a display's cadence: edit-mode Heartbeats keep their own 240 Hz deadlines),
`use_recorded_clock(engine, Some(&json))` (`RUR_CLOCK`: a Studio session's recorded Heartbeats, render
dts, clocks and seed), `flush_deferred`, `resume`. `--mode play` runs one Heartbeat and one render step
per frame with Stepped firing; `--mode edit` runs the edit-mode clock.

Deferred work follows Roblox's Deferred signal behaviour (measured in a playing client): a chain of it,
events fired from deferred handlers and threads, and task.defer calls from deferred threads alike, runs one
level deeper at each step and stops at 80; a refused handler is logged as `Maximum event re-entrancy depth
exceeded for <class>.<event> when calling <function> on line <n> in <chunk>`, a refused task.defer as
`Maximum re-entrancy depth (80) exceeded calling task.defer`.

### Script timeout (`src/watchdog.rs`)

A resumption the scheduler makes (a thread, a handler, a harness; nested ones count with the outermost)
that runs the limit without yielding errors with Roblox's `Script timeout: exhausted allowed execution
time` at its next safepoint in a script's own code, and at every one after until it yields or ends, so a
`pcall` cannot keep it going. The limit is 10 s by default, as Studio's Script Timeout Length (text and
default from Roblox's documentation, not measured), counted in seconds of the work clock, so it fires
under a frozen clock and the same at any machine load; 6 times the limit in host time fires it too
(work the clock misses: natively compiled code). A watchdog thread installs the VM's interrupt only once
a resumption has run a whole 0.1 s tick, so runs whose scripts yield pay one atomic increment per
resumption. Roblox goes on after a timeout; ree stops a run at the third (exit 126). `ree` also caps a
run's wall-clock time (600 s plus 20 s per `--seconds`, exit 124) and exits when the process that started
it is gone (exit 125).

### Held values (`src/lua/refs.rs`)

Every Lua value Rust holds for later (a queued, delayed or waiting thread, a deferred job's argument, a
timer's thread) takes a slot of mlua's reference thread, which Luau caps at 1,000,000. Roblox has no such
count, so a loop that queues work without yielding runs there until its script timeout; here the slots
would run out first. So task.spawn, task.defer, task.delay, spawn, delay, Debris:AddItem and
BindableEvent:Fire raise `<call>: too many threads and values held for scripts (...)` at the caller once
900,000 slots are held, and task.wait, wait, Signal:Wait and WaitForChild at 950,000. The offending script
errors with its traceback, what it queued runs as the scheduler reaches it, and the other scripts go on.
What still fills the stack (one call with a million arguments) errors in the resumed thread instead of
ending the run.

### Work clock (`src/cost.rs`)

`os.clock` (and `tick`, `DateTime`, `elapsedTime`, `GetServerTimeNow`) reads the frame's virtual time
plus the script work charged since the frame began. Every Instance read (by what the value is; a member
reached by its lower-case first letter 40 ns more, `LOWER_FIRST`), write and method call, Instance.new, Clone (per instance), a subtree entering or leaving the DataModel (per instance),
Destroy, the lists methods return (per element), a find's walk (per child or node looked at), a ray's path
and a spatial query's results (per part), signals and threads, and EditableMesh's own work are charged what
they cost in a client, as measured with os.clock (cost.rs gives each price with its measurement). A script's
own Luau is charged too, from the VM's work counters (patches/luau/02-work-counters.patch `lua_workcounters`: instructions
by opcode, a `--!native` module's apart, objects and table storage allocated, keys moved as tables grow, C
calls and calls into the engine's own Luau, imports looked up, and the library's own loops: numbers made
strings, new strings, sorting, table.create / clone / concat, string.split and string.format). Each
instruction is priced by its kind, interpreted or native (`INTERPRETED_OPS`, `NATIVE_OPS`). The engine's own
Luau (prelude, services, vector3.luau) stands in for Studio's C++ and is not charged. Roblox's datatypes are
priced by operation where a client pays more than the instructions that reach them (lune-roblox
`datatype_price`, cframe.rs `price`): a Vector2, UDim or UDim2's arithmetic and `==`, a member that makes a new
value (a UDim2's X, a keypoint's Value, a Vector2's Unit), a sequence's Keypoints (a new table of new keypoints
on every read), their constructors and CFrame's methods. A Vector2's X / Y and the deprecated lowercase names
are plain member reads (an instruction).

The first touch of an instance in a frame (a read, a write or a method call) costs more than the next ones:
45 ns for a part, 270 ns for a GUI object or UI component, which a client lays out and draws every frame
(`COLD_TOUCH`, `COLD_TOUCH_GUI`), and a write's dirtying on top (`cold_write_extra`). The first use of each
member in a frame runs the engine's own code for it cold: a property's first write 1.4 us more, its first read
0.5, a method's first call 0.8 (`FIRST_WRITE`, `FIRST_READ`, `FIRST_CALL`, `Touched::first_use`; a client's
first GUI write of a frame 1.65 us over the same write later in it, a part's 0.9-2.35, a method's 0.6-0.95).
Those 45 ns are what instances made together cost, touched in the order they were made: an instance touched by
itself (not made right before or after the one touched cold before it, and not touched the frame before) costs
0.32 us more (`COLD_TOUCH_ALONE`; a client's parts cloned a frame apart between other work, touched once each,
0.29 us more at the median than parts made together in one loop).

An effect's engine work runs each engine path's code cold at its first use in a frame (`Path`, `PATH_FIRST`):
a clone 25 us and each class the frame has not cloned yet (`first_cloned`: a part 55 us in all), a destroy 10 and
its classes (`first_destroyed`), entering the Workspace 13.5 (26 with a part), a PivotTo 16, a part's geometry
write 9.5, BulkMoveTo 28, Emit 4.4, QueryDescendants 6.6, GetDescendants 3.6, an emitter's Enabled 2.4. After
that a cold instance adds what it adds in a client: a clone of a template the frame has not touched
(`clone_cold`, a part 1.8 us), a destroy of an instance in the Workspace since an earlier frame (`destroy_live`,
a part 4.4), a move of a part neither moved nor touched the frame before (`PIVOT_COLD_PART`, `BULK_COLD_PART`,
1.2-1.25 us, and each emitter or attachment under it), an emit from or a switch of an emitter the frame has not
touched (1.45, 0.7), a query from a root it has not touched (4.6, once a call: a client walks a large static tree
for less a node than a fresh model). A script's first call in a frame of a C function costs 2.5 us more, of a
datatype's operation in the engine's Luau 0.9, and the first run of a function whose code did not run the frame
before 0.7 (the VM's work counters `LUA_WORK_COLDCFUNCS`, `LUA_WORK_COLDHOSTFUNCS` and
`LUA_WORK_RAREPROTOS`, priced at `LUAU_COLD_CFUNC` and the rest; a C function the engine's own code calls, one
dispatched through `__namecall` and a C closure made in the frame are not counted). A render-step binding costs
1.55 us to dispatch (`RENDER_STEP_DISPATCH`), and a thread handed to task.spawn what a function's new thread
does. Each was measured in clients of a game's match place as a path's first use in a frame less a second use
on another cold copy, less what the member's first use already costs (cost.rs gives each); headless clients
read the same within 20 %.

A client runs its engine work between two frames' scripts, so Luau that runs once a frame over data it made
earlier finds that out of the caches too. The VM marks each table, Luau closure and function a script touches
in a frame (a bit of the object's header that nothing else uses), counts the first touch in a frame of one
made before the frame (`lua_coldtracking`, `lua_coldframe`; the work counters' `coldtables`, `coldclosures`,
`coldprotos` and `coldcode`), and the clock charges each 200 ns a table, 30 a closure, 5 a function and 0.85 an
instruction of its code (`LUAU_COLD_TABLE` and below). A table made in the frame is not cold, and a walk run
twice in a frame pays once. The lists of marked objects live outside the VM's heap, and a collected object's
page goes back to the allocator a frame later: collectgarbage("count"), gcinfo and the collector's pace are
the same with the marks counted and without. A host turns the counting on where it charges scripts' Luau
(an interpreting VM: `ree record`, `ree run`, `ree serve`, a host that calls `interpret_modules` or
`interpret_all`). A client pays 14 ns a record walked once a frame when the records were made together in one
loop, 50-70 when other allocations came between them, 140-210 when they were made over many frames between
instances and other allocations: the VM does not see where an object lies, and the price is the made-over-time
end, where a game's per-frame data is.

The prices are measured with the parity bench (`tools/cost-parity`, README there): about 400 workloads that
run unchanged in ree and in a real client, engine calls by family, the language loops interpreted and as a
`--!native` module, and a module whose environment getfenv made unsafe; it prints each workload's ree /
engine ratio and keeps a third of the workloads, by a hash of their names, out of every fit. Its rows that
measure what a client has done before (a heap of a game's size makes new strings 2-3x dearer) are taken in
clients that run the whole corpus. A client runs the corpus's groups of families as jobs of one session,
and each job takes its scene out of the DataModel again (`Corpus.Cleanup`): the scenes left behind had made
the later groups' spatial queries meet three copies of their geometry. A memory row keeps every round's
objects alive until the row ends and reads its median round.

What the prices compose to on real per-frame code is a second question, measured on a game's own modules
timed the same way on both sides: a GUI scroll and gradient step, an effect system's step lists and twelve
attacks' casts (each after a warm-up cast, against a client's script time for the same cast: its profiler's
self time over idle, the median of three casts). The engine paths' prices above were fitted on three of the
attacks; nine attacks, a virtualized card grid on its frame scheduler and a server's enemy movement loop are
held out of every fit. Without any cold costs the work clock read 0.15-0.53x a client, with the first touches
alone the casts 0.22-0.67x. With all of them a GUI scroll reads 0.82x, a gradient step 0.78x, the step lists
0.85-1.17x, the server loop 1.03-1.08x, the card grid 0.63-0.66x, and the casts 0.73-1.10x fitted and
0.51-0.81x held out (most 0.6-0.75x): a held-out cast's engine calls read 0.5-0.9x a client and its own Luau
0.3-1.0x (most 0.5-0.7x). What is still missing is named in every profile's summary (`cost::FIDELITY`): the
rest of a cast's cold cost (code and data a client finds cold after frames unused), where data lies, what a
layout leaves warm, and the cold of each node a large walk visits. Size a cut in a client.

Within one instruction kind the clock charges an average, so it cannot tell some ways of writing the same thing
apart where the interpreter does (`ree profile try`, tools/what-if, host time against the clock, 9 interleaved rounds):
a lookup by a number in a table's array part and by a string in its hash part cost the same on the clock and 1.5-1.8x
apart in host time (10,000 keys, read 40,000 times); a numeric `for i = 1, #t` over an array reads 1.04x a generalized
loop on the clock and 1.3-1.7x in host time. Growing a list costs more on the clock than in this machine's interpreter
(appending 10,000 values to `{}` against `table.create(n)`: 5.5-5.8x on the clock, 1.4-1.9x in host time): the clock's
prices for the keys a rehash moves and the bytes it stores are a client's.

A script's environment is safe, as Roblox makes every script's: its imports (`game`, `Vector3.new`,
`math.floor`) are resolved when it loads and read as constants, builtins (`math.*`, `type`, `typeof`, ...)
take the VM's fast calls, and getfenv / setfenv end both for that environment, as there (the bench: two
imports a loop step 4.6 ns in a module, 21.4 after getfenv; `typeof(x)` 4.7 and 25.9). The VM's builtin
`typeof` names ree's own values as ree's `typeof` does (`lua_settypeofnames`: Instances, the registered
proxies and tables, vectors as Vector3), and the compiler leaves a constant vector's typeof to it (Luau's
`LuauCompileTypeofFold`).

So a loop that slices its work by an os.clock budget per Heartbeat takes as many frames as in Studio, and
a frozen clock (`REE_FROZEN_CLOCK=1`: the clock follows the frames only) makes runs of code that differs
in speed write the same values; a client's os.clock is real time, so a script that times itself on it within a
frame can draw differently there when its real work moves (`ree record --timing-check` says when a recording
hinges on that: docs/cli.md). The clock never runs backwards and never falls behind the frame's time.
A recorded clock (`RUR_CLOCK`) takes no charges; the busy-loop allowance still ends a loop that only reads
the clock. A frozen clock counts those reads within one resumption only, so every thread that reads it in a
frame, however many there are, reads the frame's time, as a frame-locked benchmark clock gives them.

The same script is charged the same in every run. Luau's garbage collector paces its cycles by how long
they took, and on the wall clock it stepped at other points in each run on a loaded machine: an allocating
builtin's fast path gives way to its C function while a step is due, a string is found alive or made
again, and what the VM counts moved with it, so a budgeted loop could take other frames. The collector
paces itself by the scripts' own work on this clock instead (patches/luau/03-gc-clock.patch `lua_setgcclock`,
cost.rs `install_gc_clock`), so a run collects at the same points however fast the host runs, one-shot
or through `ree serve`.

A client looks a Text write up for translation when AutoLocalize is on and the text has a letter in it,
whatever the place holds: about 33 us for a one-letter text, 0.43 us a letter more (an edit DataModel and a
server pay nothing). A place can carry prices of its own (`cost::PlacePrices`, `Options::place_prices`):
`REE_LOCALIZED_PLACES` (`<place id>[=<us for a one-letter text>],...`) gives a place its own lookup, and
`REE_MATCH_PLACES` (`<place id>,...`) scales a part's geometry write in the workspace by 1.55, as Studio
playtest clients of a game's match place read it. The engine lists no places by default; an embedding host
sets `Options::place_prices`.

The prices are a desktop client's. `--device phone` (or `REE_DEVICE=phone`) emulates a touch device and
scales the script work the clock charges by 2.77, a sim-to-phone calibration for scripts (range
2.13-4.16): a conversion of the desktop prices, not a measurement on a phone.

## Profiling (`src/profiler`, `src/luau_profile.rs`, `src/boundary.rs`)

`--profile <kinds>` (docs/cli.md "Profiles and caches") or a host's `profiler::Session` runs the profilers a
profile asks for; `profiler::report` writes its files and `ree profile diff` compares two; the profile viewer
(profile-view/, docs/cli.md "The profile viewer") reads them and nothing else. What each one measures:

- **The VM's instruction profile** (patches/luau/12-profiling.patch, `lprofile.cpp`, `lua_profilestart`). While a profile
  steps, `luau_execute` runs its single-step variant on every thread of the VM, and the dispatch calls the
  profile before each instruction. It counts the instruction; it books to the previous instruction what moved
  since it: the VM's work counters (`lua_workcounters`), priced slot by slot with `cost::luau`'s own prices (the
  profile checks the model is a weighted sum of the counts), and the engine's charges (the Instance API's,
  event dispatch); a C function's own work is its calling line's self work too (`c_work_ms` beside the line's own
  instructions'), so the lines' self work adds up to the work clock, less what is charged outside any instruction; and
  it keeps a calling-context
  tree from the frames each thread shows (call counts, inclusive figures, callers and callees). A C function
  a CALL reaches gets a node of its own at that call site, named by where the globals hold it (a library's
  table, the caller's environment read first) or by the method a NAMECALL looked up, and what is charged
  from the call to its return, past the CALL's own dispatch, is its own (a sort's comparisons); a Luau
  function a C function calls counts a call each time C enters it (`luaP_enter` marks the frame there as
  another call's). A C function no global holds, with no method and no debug name, is named by the call site that
  reached it (`? called at Module:12`), so two of them read apart. The interpreter's entries are kept with where their
  `luau_execute` frame sits on the C stack: what runs once one is left, until the next step, is the C function's it came
  from (a `pcall`'s, a `coroutine.resume`'s). ree builds the VM without C++ unwind semantics, so an error that unwinds
  the interpreter skips `luaP_Execute`'s destructor: the protected call's catch (`luaD_rawrunprotected`, `luaP_unwound`)
  drops the entries deeper than itself, so the nesting stays right after an error and the outer C function's work after
  it is its own, not the C function the unwound frames reached last (an `error`'s). The engine's own Luau books to the
  script instruction that called it; what is charged while no instruction runs (an event's dispatch, a thread's
  resumption) is booked as outside any instruction. The instructions with an inline cache record what the interpreter did, read before the
  instruction runs with the lookups it is about to make (string keys only: an object key would be given a
  hash id and move later tables' order): the cached slot served it; the key was found elsewhere in the table
  (a method: in its first `__index` table), so the slot is repatched; the key is absent and nothing else was
  looked at; a new key was inserted (rehashed: the table grew to fit it, told by the rehash hook before the
  next instruction); `__index` / `__newindex` tables walked (how many) or a function called; an unsafe
  environment (an import looked up each time; an import not resolved at load is absent); a value that is not
  a table, with the type whose metamethod served it; a store into a frozen table. GETGLOBAL / SETGLOBAL,
  GETTABLEKS / SETTABLEKS, NAMECALL and GETIMPORT carry caches; Luau's loader turns GETTABLEKS, SETTABLEKS
  and NAMECALL with a member name's key into GETUDATAKS, SETUDATAKS and NAMECALLUDATA, which fall back to the
  table access at their first run when no direct access serves the value (the engine registers none): the
  profile records them as the access they ran as, its copy of the code included. A builtin call (the
  FASTCALL family) records whether its fast path served it and, when not, the first of the fast path's
  conditions that failed (lbuiltins.cpp's, per builtin): the argument count, an argument's type, the results
  asked for, a collector step owed (string.sub, string.char, tostring and table.create cannot run the
  collector), an unsafe environment, else the values (out of its range). A cache or builtin site keeps a record for
  each call-tree node and phase it ran in (`node`, `phase`), with what the clock charged the instruction past its dispatch
  there (`charged`), so the report's tables cover the reported phases and a site can be told apart by where it ran (a
  comparator called by `table.sort` and from a loop); the cache totals at the marks (`lua_profilecachetotals`,
  with each kind's metatable walks by depth) leave ree's tools' chunks out (`lua_profiletoolchunks`: `rur.`, `ree.`, the
  what-if runner's harness). What each outcome costs is below ("What an outcome costs"). With host time profiled (`lua_profilechunks`), a loaded chunk's functions are
  registered at its load, so a function that only ever runs as inlined copies is named and its copies known without
  stepping; stepping registers a chunk's functions when it first runs one, and no other kind needs them. Host time reads
  the VM's count of the C calls scripts make (its work counters' `ccalls`) at each safepoint, so a sample after a C
  function, whose time no safepoint of its caller samples, is told from one after a Luau call or a builtin call its fast
  path served: nothing in the interpreter keeps it. The count is kept for each of the interpreter's entries open
  (`lua_profileentries`): an interrupt from inside a C function (string.gsub's matcher sends one at each match) and the
  safepoints of a Luau function a C function calls back consume none of the calling frame's count, so the rest of the C
  function's time stays on the calling frame's next safepoint, after it returned; a callback's own samples are split as
  any code's. The counters past an opcode range's last opcode (the cold first calls and rare functions' `LUA_WORK_COLDCFUNCS`,
  `LUA_WORK_COLDHOSTFUNCS`, `LUA_WORK_RAREPROTOS`) are booked when they move, with the instruction or C function that moved
  them. `Context.ProfilePhase` starts a phase: the frames hang from a node of the phase's. Natively compiled code keeps running natively:
  a function's entries into its native code and the work booked while it ran are its own, and its
  instructions are not counted (the summary names such functions); a call from native code into natively
  compiled code is not seen, so the callee's work stays with its caller. Luau's `-O2` (modules compile at
  it; `--!optimize 2` in a chunk asks for it) inlines small local functions: their bodies are compiled into
  the callers, which run them in their own frames, so their instructions and work count on the caller's
  function, on the inlined function's own lines. The profile finds those copies: when it first sees a chunk
  it registers all of its functions, and a run of a caller's instructions on another function's lines (one
  enclosing the caller aside, and the instructions creating a closure) is a copy of that function, the same
  instructions on the same lines again right after it the next copy, and a copy of a function inlined into
  the copied one sits inside its copy. Each time a frame steps into a copy from outside it counts a call of
  the inlined function (a call made inside the copy returns into it; a loop inside it stays in it). The
  summary lists inlined functions with these calls, their other calls, and the instructions and work on
  their lines in each caller; the callers keep those figures as the VM ran them. Two copies of one function
  in a row whose code differs (a constant argument folded differently) count as one.
- **Allocations** (`profiler::memory`): the VM's `onallocate` callback sees every allocation; `lua_allocblock`
  gives a collectable object's block (its type is read at the next callback, once the VM set it; an Instance's
  userdata names its class) and what other memory is for (a table's array and hash parts, a thread's stack
  and frames, the string table, a loaded function's arrays). A sampled object is marked in its header
  (`lua_profilemark`), and the collector calls the profile when it frees one, so the run's live objects are
  known by the stack that allocated them. What `lua_allocblock` reports is the calling thread's allocation:
  VMs allocating on other threads at the same time never show in it.
- **Heap snapshots**: `lua_enumheap` walks every object and every reference the collector follows (those
  `luaC_enumheap` gives, and what it leaves out: a prototype's source, debug name, upvalue and local names, a
  thread's namecall and open upvalues, a weak table's strings, which the collector never clears), from the
  collector's roots: the main thread, the registry, the basic types' metatables and the fixed names. What the
  walk reaches is what a full collection would keep: the reachable heap, which every heap figure reports. The
  rest, with the objects a sweep has found dead and not freed yet (given by type and size alone: a dead closure
  can hold a prototype already freed), is the garbage not swept yet, counted apart: how much there is depends
  on where the collector's cycle stood, not on what the run holds. A snapshot collects nothing. From a root
  over those roots it finds each object's dominator (Cooper, Harvey and Kennedy's algorithm), so an object's
  retained size is what would be freed with it, and a breadth-first holder path (a path through the
  registry's array part is a reference the engine itself holds: a handler, a thread it will resume).
  Each reachable table's shape is read during the walk (`lua_profiletable`: its array part's slots and those holding a
  value, its hash part's nodes and those holding one; `lua_profiletablebytes` the bytes of a table, a slot and a node),
  and its nesting level comes from the references between tables (a metatable's aside): one more than the deepest table
  it holds, the tables of a cycle sharing theirs (strongly connected components, levelled in reverse topological order).
  The tables the profile followed are grouped by the stack that allocated them (`memory::Tables`).
- **Table growth**: a table grows only by rehashing (ltable.cpp `rehash`, when a new key finds no free slot), which
  moves every key it holds into its new parts; the VM's work counters count those keys for a script's own instruction
  (`rehashed`, which the work clock prices). With a profile, `rehash` also calls the profile's hook
  (`lua_setprofilerehash`: the table, its parts' sizes before and after, the keys, whether a script's instruction made
  it), behind the same thread-local check as the rest of the profile; the allocation profile books each rehash to the
  stack that made it and to the stack that made the table.
- **Marks**: each `Context.ProfilePhase` mark, and the run's end, take the counts so far (the work clock, host time, the
  run thread's cycles, instructions, the VM's work counters, the allocation profile's totals and rehashes, the collector,
  the inline caches' outcomes summed by kind and the builtin calls served and declined: `lua_profilecachetotals`), so a
  phase's figures are differences of marks. `ree profile try` (docs/cli.md "What-if comparisons") reads its phases this
  way.
- **What an outcome costs** (profiler/prices.rs), apart by whether the work clock charges it. Charged: a rehash (the keys it
  moves, the new storage), an import looked up each time, a value another type's own metamethod serves (the engine's price):
  the VM books what the host's prices charged an instruction with an inline cache past its dispatch to its site, per phase
  (`lua_ProfileCacheSite.charged`: whatever the clock charged it there, a table's cold first touch in the frame among it),
  and the report gives it as the site's `charged_work_us`, part of its function's work. Of it, `firsttouch` is the first
  touch in the frame of the table the instruction reaches (its operand's, the environment's for a global: the cold touch
  tells the profile, `luaP_coldtouch`), which any change to the instruction still pays (`first_touch_us`; the rest is
  `outcome_charged_us`), and an insert that rehashed adds to `presized` the storage the rehash grew the table by at the
  clock's price for storage: what a presized constructor would have charged more in its place (`presized_us`). A
  declined builtin call is charged too: the VM books to its site what the clock charged it (`lua_ProfileBuiltinSite.charged`:
  its fast call's dispatch, the instructions from the fast call to its CALL, and the function that CALL reached, a C
  function's node with its cold first call in the frame or the engine's own Luau, which books to the script's CALL), the
  report's `decline_charged_us`, and what the fast path would charge the same calls (`fastequivalent`: the fast call's
  dispatch and the work a builtin's fast path counts, a new string and a number made a string as the call counted them, and
  the objects and table storage the call allocated and the keys its tables' rehashes moved, which the counters leave to a C
  function's call: `luaP_uncounted` counts them for the profile), `fast_equivalent_us`: a fix saves the difference. Uncharged: a
  reprediction, an absent key, an insert into a table with room, `__index` tables walked, an `__index` function's call (its
  own instructions are charged where they run): no counter sees them, so each is priced at a committed ratio to its base
  opcode's hit, measured on ree's interpreter (profiler_prices_tests.rs's calibrate_prices, run by hand in sessions at
  different times, each 7 processes of 21 interleaved rounds, gathered in a sessions file; the median of every process's
  median, the least and greatest of them the spread), times the work clock's price for the base opcode: `extra_work_us`,
  work beyond the clock's. A price whose spread reaches zero is within the noise: shown, never summed. No run measures
  anything.
- **The collector**: the VM's interrupt runs at each collector step's start and end (steps, cycles, bytes
  freed, host time by the state a step began in). **The timeline**: `scheduler::step` takes a row a frame: the live
  instances by where they are (in the DataModel's tree, alive outside it, destroyed and kept until the VM collects the
  userdata that holds them: only that last follows the collector) and the threads the VM made in the frame (the reachable
  ones are the heap snapshots'); its host time and cycles only in a run no kind slows. **ree's caches**: counters at each
  cache (`profiler::caches`, process-wide); a member name's atom misses where the VM looks a new string up by its bytes,
  and the instances a userdata was made for, the modules run and the datatype property reads are the game's facts.
  **The CPU**: from the run's start (the VM's install), the run thread's cycles (QueryThreadCycleTime: timestamp-counter
  ticks while it ran) in milliseconds at the counter's rate, by phase from the marks and by frame, the process's CPU
  time, page faults, the working set and private bytes at the start and their growth; a run a kind slows takes these in a
  pass of its own (`profiler::cpu_pass`: the command again with `--profile cpu`).
- **Hardware counters** (`hw`, `profiler::hw`, tools/hw-counters). User mode cannot run `rdpmc`, and a kernel trace
  session needs the system's profiling right, so a service installed once (docs/cli.md "Hardware counters") runs a
  session that records the CPU's counters at every context switch, for the asking process's threads only, and hands
  back each thread's slices: from being switched in to being switched out on one CPU, with what each counter counted
  (`hw::protocol`, shared with the service). The run thread's readings make it switch out (`hw::client::Reader`), so
  the slices split exactly at them (`hw::readings::split`: a slice is the window of the first reading whose switch is
  at or after its end, the last such reading's when a switch was not seen). Readings come at the profile's start and
  end, when the VM is installed, at each frame's start and end, at phase marks, around the engine's own steps, around
  the Luau calls the VM samples, and where a Luau thread holding a sampled call resumes or stops (the VM's `preresume`
  and `postresume` callbacks). The call hook (`lua_setprofilecalls`, patches/luau/17-profile-calls.patch) runs as a
  sampled call's first instruction is about to run and as it returns or an error unwinds it to a protected call; each
  call is drawn on its own with a chance of one in `every` (`lua_setprofilecallevery`), and a call inside a sampled
  call of its own function on the same thread is sampled with it (up to 64 from each recursion's outermost sampled
  call), so a recursion is read as a whole; a thread an error ends, or one reset, leaves its sampled calls, and a freed
  one gives them up (each thread's open sampled calls are tracked). The
  profile sets `every` from the function's mean call (each measured call weighed by the calls it stood for) against a
  threshold between 20 us and 1 ms that keeps call readings near 10,000 a second (frame, engine-step and thread readings
  cannot raise it), after reading a function's first 4 calls; under 10 us, none
  (stopping only under 7.5 us), with one call read for its length after 64, 128, ... 65,536 calls. A stretch between
  readings goes, as the reading that opened it left things, to the innermost open sampled call of the innermost
  running thread that holds one (else the engine step, else the profile's own frames), booked to its stack (its
  callers at their call sites, itself without a line) weighted by its `every`; at its end `every - 1` times its net
  comes off what it ran in (still, when that call ended first), so each function's own counts are an unbiased estimate
  and every booking adds up to the run's counts; a line's figures are the calls made from it. Calls read for their
  length alone, and a function's first calls when its fourth found it too short (decided then), are their callers'. A reading's own
  counts (the median of back-to-back readings) come out of every stretch between readings, and the run thread and the
  reader's helper are pinned to one CPU while the service counts. The counters' names, the AMD events they read and
  the figures derived from them (the share of cycles stalled on loads, a miss's latency) are `hw::counters`, shared
  with the service and the profile viewer, which also maps the names profiles used before. What ran before the VM and
  outside the sampled calls gets frames of their own. `REE_HW_COUNTERS=mock` takes the thread's cycle time at each
  reading instead (tests). When the service reaches its limit of two million slices, ree books only the run up to the
  last complete reading; `counters.cut_short` gives the host seconds captured, and the summary and the viewer's header
  mark the profile CUT SHORT. Each reading makes two slices, so 10,000 readings a second allow about 100 s, and fewer
  with extra frame and thread readings. A thin wrapper between a caller and a heavy callee (such as a `pcall`) can have
  a noisy self estimate, with the opposite error in its caller; measured, it was up to 11 % of the callee's
  instructions. Read the callee's own figure for its work.
- **Host time** (`time`, luau_profile.rs, `profiler::time`): a 1 ms ticker; where it moved, what ran since the last
  booking is booked to the Luau stack at a safepoint (a call, a return, a loop edge), to the Luau that ran last when the
  interpreter is left, to the engine's step open when it is entered (`lua_setprofilevm`: the Animator, joints, physics,
  the recorder's sampling), else to `(ree engine: outside scripts)`, and to an Instance API call on its own line (its C
  metamethod's entry and return). Each booking point compares the ticker with the tick last booked first (an atomic load
  each), before any thread-local work. A sample's weight is the run thread's CPU time since the one before (its cycles at
  the counter's rate, measured at the start), with the wall clock's beside it; the innermost Luau line of a sample inside
  a copy `-O2` inlined is that function's time too.

Off, a profiler costs a thread-local read where it hooks in (the interpreter: one per entry; a table's rehash: one, and one
more where C or the host rehashed it; an object or table storage C or the host allocates: one; a table's counted first
touch in a frame: one; a chunk's load and an error a protected call catches: one), a thread-local store where the VM
allocates table storage, and a bit test where the collector frees an object. A read costs about a quarter of a
nanosecond: at most 0.6 % of a loop of the library's allocating calls (string.split passes nine a call), 1.4 % of one
that does nothing but touch tables first in a frame (docs/engine.md). A profile's
hooks are written straight into the VM's callbacks, allocating nothing on its heap (a protected call to set them made a
closure there, and the collector's later cycles then began elsewhere: what a script read of memory moved). The hardware
counters' call hook costs a Luau call nothing while off: `luau_execute` runs a copy of the interpreter that asks only
while a hook is set, as it runs one for single-stepping, compiled in a translation unit of its own
(lvmexecute_profiled.cpp, so the copies build side by side; a call from C or a metamethod's fast path loads
a flag in the global state, a return tests a frame flag it loads anyway; measured, 50 million calls of a table's
function took the same cycles as with the VM before the hook, +0.0 % the median of 15 interleaved pairs); their other
readings sit behind the frame hook's read and the engine steps' check. On, nothing a
script sees changes: the profile allocates nothing on the VM's heap and leaves the VM's counts as they are,
so the work clock, os.clock and a recording are the same (a sim recorded with every kind on, a frozen clock
and a pinned epoch is byte for byte the unprofiled recording). Stepping makes the interpreter about five
times slower (allocation stacks about twice; the timeline a tenth or two; host time sampling about a fifth on a
loop-heavy run, where every loop edge calls the interrupt).

What the memory figures are, beside Roblox's:

- `gcinfo()` and `collectgarbage("count")` read the VM's whole heap in KB, as on Roblox (the profile's
  `gcinfo` column and the timeline's heap), garbage not swept yet included; a snapshot's reachable heap leaves
  that garbage out. ree's `collectgarbage("count")` returns whole KB (gcinfo's), where Roblox's has a fraction.
- The VM holds the engine's own Luau too (its prelude, services and datatype tables), which Roblox keeps in
  C++: the `start` snapshot is that baseline. Compare a run's growth, or the run's own live objects, not
  totals. Instances live in the engine on both; each has a Luau userdata (48 bytes here).
- `Stats:GetMemoryUsageMbForTag` is not emulated. The Developer Console's Luau heap view splits memory by
  memory category (one per script); the profile splits it by the stack that allocated it, by type and by
  Instance class, and gives the biggest objects with their holders, as its snapshot view does.
- Imports: the engine loads a chunk before it gives it its environment, so imports are never resolved at
  load and every GETIMPORT looks its global up (the cache profile reports them as misses); Roblox resolves
  them at load. The work clock charges a GETIMPORT as one instruction either way.

## World and recorder (`src/world`, `src/record`)

The 3D side of the engine and the recorder behind `ree record`. They use the dom, reflection, values,
signals, the scheduler, the Lua bindings and the project mount, and nothing of a GUI layer.

```
world/mod.rs     install(engine), terrain, WorldEvent + report, world_cframe, hung_from
world/frame.rs   CFrame arithmetic (glam, f32), Orientation (YXZ) and Rotation (XYZ) Euler forms
world/query.rs   spatial queries: solids, cast_ray, cast_box, overlaps, the WorldRoot methods
world/camera.rs  WorldToViewportPoint / WorldToScreenPoint / ViewportPointToRay / ScreenPointToRay
world/joints.rs  rigid assemblies, the Motor6D.Transform batch
world/gravity.rs free flight: gravity, a script's velocity and spin, impulses, mass
record/mod.rs    Recorder::install / sample / write: what is recorded and how
record/encode.rs values as playback JSON and as rbx-dom variants
record/run.rs    `ree record` / `ree run`: the project, the harness module and Context, the frame loop
record/prepare.rs runs prepared ahead by `ree serve`
```

- **Views**: BasePart and Attachment Position / Orientation / Rotation (and Attachment Axis /
  SecondaryAxis) read and write CFrame. Attachment World* properties read and write the world CFrame of
  what the attachment hangs from (measured in Studio): its nearest ancestor that is an attachment, a part
  or a model, past Folders, Configurations and Accessories; a model's frame is its pivot, a WorldModel's
  or the Workspace's the identity. An attachment under a bone follows the bone's
  TransformedWorldCFrame.
- **Rigid assemblies** (Roblox's documentation; the root measured in a playing client): Weld, Snap, ManualWeld,
  Motor, Motor6D (Part0 * C0 * Transform = Part1 * C1) and WeldConstraint carry the parts they join. Each assembly
  hangs from its root, which `BasePart.AssemblyRootPart` reads (nil outside a WorldRoot): an anchored part, else a
  part that is not Massless, then the higher RootPriority, then the larger largest face (its area, not rounded)
  times 20 for a Seat or VehicleSeat, 10 for a part named HumanoidRootPart and 5 for one named Torso, then the
  later made part. A
  part's CFrame write moves its whole assembly rigidly, the root's rotation stored as a part's CFrame write stores
  it, so a rig whose parts are written one by one (a BulkMoveTo of all of them) keeps its shape; Position / Orientation / Rotation move the part
  alone and rewrite its joints. Model:PivotTo and ScaleTo write their parts as one batch. After PivotTo a
  model whose pivot is its own (no PrimaryPart) has the target as its pivot exactly, every call, as in a client;
  composing the move onto the old pivot compounded float error until a model pivoted every frame lost its rotation.
  Motor6D.Transform applies after PreSimulation (edit mode: at each Heartbeat's start).
- **BulkMoveTo** (measured in a playing client): `WorldRoot:BulkMoveTo` checks its lists before any part moves, with a
  client's messages: lists of different lengths ("Length of part list (2) and length of CFrame list (1) do not match."),
  a part list entry that is no BasePart, a frame list entry that is no CFrame, and arguments that are no arrays. A nil in
  the part list leaves its frame unused, so the lists still pair by index. Destroyed parts, parts with no parent and
  parts outside the Workspace move.
- **Free flight** (measured in playing clients): in play mode an assembly in the Workspace with no
  anchored part, nothing that can collide, no Humanoid and no constraint flies from its first simulation
  step under Workspace.Gravity, at the velocity and with the spin a script gave any of its parts
  (AssemblyLinearVelocity or Velocity, AssemblyAngularVelocity or RotVelocity; ApplyImpulse,
  ApplyImpulseAtPosition and ApplyAngularImpulse act at its next step). Each step updates the velocity,
  then moves by it: four steps of 1/240 s on an assembly's first simulated frame, one of 1/60 s a frame
  after. Spin turns it about its centre of mass as a free rigid body (box inertia), decaying 5.2% a
  second. Mass is volume times density (CustomPhysicalProperties', else the Material's; a Ball's volume a
  sphere's); Massless parts weigh nothing; Mass, AssemblyMass, AssemblyCenterOfMass and GetMass() read it.
  A place that streams simulates only near its replication foci (the character, Player.ReplicationFocus,
  Player:AddReplicationFocus parts): an assembly whose bounding sphere starts about a focus simulates while
  its centre distance plus radius stays within StreamingTargetRadius cut to a whole 64 studs (32 less for
  an added focus), then stands still. Not modelled: collisions, FallenPartsDestroyHeight, a mesh's own
  volume and inertia (a MeshPart's box), and the extra steps a client takes when a frame runs long.
- **Queries**: Block parts are boxes, Ball parts spheres, Cylinder parts cylinders along X, anything else
  its oriented bounding box (no mesh geometry); Terrain is never hit. Rays starting inside a part do not
  hit it; CanQuery off is skipped, RespectCanCollide skips CanCollide off; collision groups are not
  modelled. FilterDescendantsInstances and the newer ExcludeInstances / IncludeInstances lists follow
  Roblox's documentation (devforum.roblox.com/t/4563575); a part must pass both.
- **World events** (`Hooks::world_events`): `ParticleEmitter:Emit` (with where its emitter was at the
  call), `Clear`, `Trail:Clear`, sound plays and stops. Particles, trails and beams are not simulated:
  a renderer draws them from these events and the recorded switches.
- **What is recorded**: the Workspace's descendants (code, sounds, values, animators, Humanoids, Terrain,
  Cameras and Clouds skipped with their subtrees), the CurrentCamera's and Terrain's children as roots (Roblox
  draws what a client parents there), Lighting's own properties, the Terrain's Clouds' and the skies, atmospheres and post
  effects a run parents to Lighting or the camera, SurfaceGuis and BillboardGuis in the PlayerGui under
  their adornees, and the PlayerGui's ScreenGuis in the scene's `PlayerGui`. Each instance gets one copy
  in `scene.rbxm` as it appeared, hidden before and after. Lighting's values before the run stand in for the
  place's, which the renderer's stage draws: every write the run makes to a tracked Lighting property plays
  (one before frame 0 at frame 0, one of the value the engine held too), and a keyed tween holds
  `{"initial": true}` (the stage's value) until the run first changes the property. The Terrain's Clouds (a
  Clouds in the Workspace's Terrain, whatever its name or who made it; Roblox draws no other) play the same way on
  the target `Workspace.Terrain.Clouds`, their path in a place: Color, Cover, Density and Enabled. `playback.json` holds `set` actions, keyed
  tweens for properties that change frame after frame, motion tracks, emits and clears, reparenting and
  references written later; `events.json` holds prints, warnings, errors and sounds with their times.
  Values are rounded to 1e-5, and keys a straight line reproduces are dropped: a motion key's position within 0.002 studs, its rotation within 2e-4.
  A Studio-only placeholder is the place's own: it has a class and a name and no values, and it is made
  when a script first lists its parent, so drawing it drew default effects and parts, and a run that
  looked at more of the place recorded more of it than one that did not. One that draws (an effect, a
  part, a highlight, a sky) is never drawn, in the Workspace or in Lighting; a placeholder folder or model
  is drawn once something the run made is under it, as that appears.
- **EditableMesh and EditableImage** (src/meshes.rs, src/images.rs, measured in Studio): ids carry their
  kind in the high bits; AddTriangle gives each corner a new automatic normal, a white opaque colour
  and a (0, 0) uv; CreateEditableMeshAsync of a mesh asset (feature `meshes`, decoded by ror's ror-mesh)
  makes one vertex per file vertex with its bones and bind CFrames. A MeshPart bound to an EditableMesh
  points its MeshId at a FileMesh snapshot in the state dir, rewritten when the mesh changes. An image
  assigned through `Content.fromObject` shows as a PNG snapshot of its pixels.
- **Context helpers**: `Context.TraceWrites` names the Luau line behind each Parent or property write
  (src/trace.rs), `Context.Census` the line behind every live thread, connection, timer and instance
  (src/census.rs), `Context.Holders` what keeps a tree outside the DataModel alive (src/lua/holders.rs).

## Animation (`src/animation`)

AnimationTracks run as Roblox's documentation describes them (AnimationTrack, Animator, Motor6D,
KeyframeSequence pages); how a track behaves before its asset arrives is measured in Studio.

- Loading is the host's choice: `animation::enable(&engine, Loading { directories, download })`.
  `ree record` / `ree run` enable it with `--animations` directories (`<id>.rbxm` / `.rbxmx`), the
  download cache (`<state dir>/animations`) and, with feature `bundles`, the asset worker's `asset`
  command. An engine without it loads only RegisterKeyframeSequence's ids. An id that loads nowhere warns
  once per engine and its tracks have Length 0.
- LoadAnimation on an Animator, Humanoid or AnimationController makes an AnimationTrack; Play, Stop,
  AdjustWeight and AdjustSpeed fade and play as documented; KeyframeReached, GetMarkerReachedSignal,
  DidLoop, Stopped and Ended fire in time order.
- The Animator step runs in the play-mode simulation phase right after PreAnimation and before Stepped.
  Per joint, priorities from the highest (Action4 ... Action, Movement, Idle, Core) each take their
  tracks' summed weight of what is left, blended by weight; what is left stays at rest. A Pose moves the
  Motor6D whose Part0 / Part1 names are its parent Pose's and its own, else the Bone of its name.
- Fetching (measured in Studio playtests): an asset's first request in a session arrives 0.45 s later;
  tracks and preloads of it share the fetch, and one fetched before loads at once. Until it arrives a
  track has Length 0 and never poses. ContentProvider:PreloadAsync waits for the Animations it is given.
- Not emulated: CurveAnimations, the default `Animate` script, animation graphs, RootMotion.

## Server run context (`--run-context server`, `src/lua/server.luau`)

A server is a Studio Play-mode server as Roblox's RunService documentation tabulates it, and as a server's game
script measured it (`node tools/parity/run.mjs --server`): `IsServer`, `IsStudio` and `IsRunning` true; `IsClient`
and `IsRunMode` false; `IsEdit` is refused to game scripts, as on a client. It needs `--mode play` and refuses
`--player`.

- No local player: Players starts empty and a harness adds players with `Instance.new("Player")`.
  Nothing renders: connecting to RenderStepped raises `RenderStepped event can only be used from local scripts`;
  PreRender connects and BindToRenderStep binds, and neither runs. Tweens step after the task.wait resumptions and
  before the Heartbeat handlers (a client steps them in its render step).
- In-memory DataStoreService (GetAsync, SetAsync, UpdateAsync, IncrementAsync, RemoveAsync, versions,
  keys, OrderedDataStore pages), MemoryStoreService (sorted maps, hash maps, queues) and MessagingService
  (this server's own subscribers), all empty at the start of every run. Limits and error codes follow
  Roblox's documentation where it gives them.
- Remotes: FireClient / FireAllClients reach no client; InvokeClient waits until its player leaves.
- EncodingService (Zstd compression and Base64) in a server; a client keeps it unemulated.
- LogService.MessageOut gets a server's lines as a playing client's ("Output and LogService" below), and
  ScriptContext.Error fires for a server's own errors.
- `--studio false` makes `RunService:IsStudio()` false, as on a live server, so a game takes its live
  branches.

A print of exactly `@done` ends `ree run` / `ree record` at the end of that frame.

## Output and LogService

Every line a run outputs has a kind: `print`, `info` (LogService:Info, and Log with MessageInfo), `warn` or
`error`. LogService.MessageOut gets each line after the frame's Heartbeat handlers and the deferred work they
leave, on a playing client and on a server alike, with three arguments: the text, its MessageType and a context
table (a copy of the one a LogService method was given, else an empty table). A script error, a playing client's
and a server's alike, carries its stack lines (`Stack Begin`, `Script '<name>', Line <n> - function <f>`, `Stack End`); a line a
MessageOut handler logs reaches no handler. Measured in a playing client and in a server game script.

LogService's Output, Info, Warn, Error and Log log a line of their MessageType with the message's `{key}`
placeholders filled from the context (`{{` and `}}` write one brace; a key the context lacks keeps its
placeholder; a value is written as `tostring` writes it). The context MessageOut and an error carry is a copy:
with string keys only, a function becomes `"<function>"`, an Instance its full name and a nested table a copy;
with any other key, every value but strings, numbers and booleans becomes its `tostring` text. Error logs its
line, then throws a table with `message`, `template`, `context` and `stack` whose `tostring` is the message; one
no script catches logs its stack as MessageInfo lines and no second error line. A message that is neither a
string nor a number logs a warning with the script's stack first. These follow Roblox's documentation and
measurements in a playing client.

## Known limitations

- ImageLabel and ImageButton report `IsLoaded` true at once; Sounds report `IsLoaded` true and a
  `TimeLength` of 1; `DataModel:IsLoaded()` is true. No asset is fetched for them.
- Physics beyond free flight (collisions, constraints, body movers), particles and the default character
  scripts are not simulated.
- Script HTTP requests are disabled: no script reaches the network.
- SecurityCapabilities are not emulated. A ReflectionService entry's `Permits` is a frozen empty table
  that reports itself (Output `missing`) when a script reads or iterates it, and a filter's `Security`
  would do the same. GetPropertiesOfClass, GetMethodsOfClass and GetEventsOfClass need fields the dump
  does not record (a reference type's InstanceType, a parameter's DefaultValue, member display data) and
  are not emulated.
- A member a datatype does not have raises another message than Roblox's `<key> is not a valid member of
  <Type>` (some types quote the key), and reads nil on CFrame, Color3, UDim2, Vector2, Ray and the raycast
  and overlap params, where Roblox raises (tools/parity/known.json, probe camelcase).
- The engine is developed and tested on Windows. Other systems build and run, but some fallbacks are
  weak: off Windows a `ree serve` record of a dead process is not detected (`alive()` always answers
  true), and memory is read from `/proc/meminfo` (Linux only).

## Data files

- `data/reflection.json`: Studio's API dump (ReflectionService: classes, enums, members, permissions,
  serialization) plus the Instance.new defaults of every creatable class, written by
  tools/dump-reflection.luau in Studio and compiled in. It is merged over rbx_reflection_database. It is
  Roblox's public API metadata, as Roblox publishes in its own API dump; it holds no engine code.
  ReflectionService:GetClasses and GetClass answer from it as a game script gets them (measured in a
  playing client and a server game script): the dump's classes in name order, each call a new frozen list
  of new frozen entries (`Name`, `Superclass`, `Serialized` when true, `Subclasses` when there are any,
  `Display`, `Permits`); a class only rbx_reflection_database describes is nil there, though it is named in
  its superclass's `Subclasses`. The filter's `IsA` and `ExcludeDisplay` work as measured, with Roblox's
  errors for a filter that is not a dictionary or a field of the wrong type.
- `data/text-advances.json`: per-glyph advance fits for each Enum.Font face, measured in Studio (no font
  files are included).
- `data/fredoka-sizes.json`: FredokaOne advances and kerning at integer sizes 4-100, from Studio.
- `data/font-enums.json`: the Enum.Font to `rbxasset://fonts/families/*.json` mapping.

## Environment

None of these carry credentials; `ree serve` keys a service by an unkeyed 64-bit hash of the `RUR_*`,
`REE_*` and `ROR_*` variables (and `LOCALAPPDATA`), never their values. The `RUR_*` names are kept from
rur, whose tools set them.

- `RUR_STATE_DIR` / `RUR_CACHE_DIR` (caches; default `<repo>/.cache`), `RUR_VERBOSE`,
  `RUR_ENGINE_TIMINGS`, `RUR_ENGINE_DUMP`, `RUR_EPOCH`, `RUR_CLOCK`, `RUR_CAMERA`, `RUR_PLACE_ID`,
  `RUR_PLAYER`, `RUR_PLAYER_ID`, `RUR_PLAYER_DISPLAY`, `RUR_ROTATION_CLIPPING`, `RUR_PROFILE_CALLBACKS`,
  `RUR_LUAU_PROFILE`, `RUR_LUAU_PROFILE_CLOCK`, `RUR_LUAU_PROFILE_UNIT`, `RUR_LUAU_ALLOC_PROFILE`,
  `RUR_LUAU_ALLOC_STEP`, `RUR_BOUNDARY_PROFILE`, `RUR_CHARGE_TALLY`.
- `REE_SCRIPT_TIMEOUT`, `REE_MAX_SCRIPT_TIMEOUTS`, `REE_MAX_WALL_SECONDS`, `REE_PARENT_WATCH`,
  `REE_SERVE_WORKERS`, `REE_FROZEN_CLOCK`, `REE_TIMING_CHECK`, `REE_RANDOM_SEED`, `REE_WORK_COUNTERS`, `REE_HASH_SEED`,
  `REE_HASH_ADDRESS`, `REE_LOCALIZED_PLACES`, `REE_MATCH_PLACES`, `REE_ASSET_WORKER`, `REE_PROFILE`,
  `REE_PROFILE_OUT`, `REE_PROFILE_COUNTERS`, `REE_HW_COUNTERS`, `REE_LUAU_FLAGS`, `REE_VIEW_COLOR` (the profile viewer's colours:
  `truecolor` or `256`), `REE_VIEW_BLESS` (its snapshot tests write their snapshots).
- Others: `RAYON_NUM_THREADS`, `MIMALLOC_EAGER_COMMIT`, `MIMALLOC_PURGE_DELAY`.

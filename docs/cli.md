# Command line

`ree record`, `ree run` and `ree serve`: harnesses, options, outputs, the warm service and the environment.
The [README](../README.md) has a first run; [engine.md](engine.md) is the contract behind it.

`ree --version` prints the engine version.

A [release install](building.md#release-binaries) runs the same offline harnesses as a source build. Name the harness
and any roots, mounts, snapshots or animation directories it needs: it reads no checkout data or `.env` from the
current folder or the executable's folder. Its project is the current folder, and its caches go to the per-user `ree`
cache folder (`RUR_STATE_DIR`, else `RUR_CACHE_DIR`, moves it). `--repo <dir>` names input data only: it never selects
host mode or puts caches in `<dir>/.cache`. Every job keeps the installed binary's standalone defaults for project,
state and asset workers, and `--service`, services started by hand and `serve --warm` share them and one service key.
`serve --state-dir <dir>` overrides state for that service, its engines and its workers.
Environment paths, including `RUR_CLOCK` and `RUR_ENGINE_DUMP`, are resolved from the client's current folder before a
standalone service starts; its key and launch use those same paths.
When no flag names the output, a run's profile goes in `<state dir>/ree-profile`, its boundary profile in
`<state dir>/ree-boundary.json` and `profile try` in `<state dir>/what-if/<file name>`, and each command prints the path.
`profile diff` and `profile report` also put their default files in `<state dir>/ree-profile`, even when their inputs
are elsewhere. `profile diff --out <dir>` uses the named output directory.
A path you name is used as given, and a recording's profile stays in the recording's output folder. A run that spools
its script output to a file does so in `<state dir>/output` and deletes the file when it closes.

`--version` and `--help` need no cache, and `--help` or `-h` after `run` or `record` prints the same usage and exits 0.
A command that writes files resolves state first and fails
with an error asking for `RUR_STATE_DIR` when neither an override nor a usable per-user cache exists, also with
`--repo`. A binary in its own checkout and library hosts keep their existing defaults.

A release archive holds ree as its only executable, so `profile view` and `profile render` report that they need the
separate profile viewer from a source build. `profile report`, `profile diff`, `profile try` and profiling during a run
work. Manifest bundles need `REE_ASSET_WORKER`; an installed ree does not look for a worker beside a checkout.

## Commands

```sh
ree record --harness path/to/Harness.luau --out out/scene --roots "game/shared;game/src" \
    --seconds 3 --fps 60 --context '{"Scenario":"demo"}' --assets-at ReplicatedStorage.Bundles --assets Models \
    [--animations <dir>]                             # pre-fetched KeyframeSequences, <id>.rbxm
    [--timing-check]                                 # record again with the in-frame clock moved (What a recording holds)
ror video out/scene/scene.rbxm --playback out/scene/playback.json --duration 3
ree run --harness path/to/Probe.luau --seconds 1     # no recording: the status and every output line as JSON
ree run --harness path/to/Server.luau --run-context server --seconds 60   # a Play-mode server; `@done` ends it early
ree record --service ...                             # the same, run by the warm service (started when missing)
ree record ... --profile                             # where the time went, on stderr
ree run ... --profile boundary [--profile-out f.json]  # Luau-to-engine crossings per call site, priced (src/boundary.rs)
ree record ... --profile work,alloc,heap [--profile-out dir]  # the profiling suite: summary, JSON, flame graphs, a report page
ree profile diff before/profile after/profile        # two profiles compared
ree profile try tools/what-if/nested-keys.luau       # variants of the same code side by side ("What-if comparisons")
ree profile view before/profile after/profile        # explore one in the terminal ("The profile viewer")
ree profile render out/scene/profile --view all --out shots   # every view as a PNG, for screenshots
ree serve --status                                   # the running services; --stop asks them to exit
ree serve --warm --roots "game/shared;game/src" --assets-at ReplicatedStorage.Bundles --assets Models
                                                     # start the service and read in what a first recording reads
```

## Harnesses

- The harness is a ModuleScript (mounted as `ReplicatedStorage.RURFixtures.<name>`) returning
  `function(Context)` or `{ Run = function(Context) }`. Its own modules are its children: a
  directory named after it beside the file (`Harness.luau` and `Harness/`), or the folder of a
  `Harness/init.luau`, mounts below it as a project directory does, so `require(script.Server)`
  works. It requires the game's modules through
  services, builds what a server would replicate (tagged parts with attributes, a fight folder, a
  floor), connects `RunService` as the game's loops do, and may yield. `Context` has `Seconds`, `Fps`,
  the `--context` object's fields and `StandIn(Module, Source)`, which gives a ModuleScript another
  source before anything requires it (a stand-in for a module the run cannot have; a harness should
  say why in a comment). `Context.Mount(Path, File)` mounts a model file or directory (an absolute
  path) at an instance path as `--mount Path=File` does, from the harness, and returns the instance
  there. `Context.TraceWrites(Properties?, Depth?, Root?)` returns `Drain()`, the Luau call sites
  (`Chunk:line`, innermost `Depth` frames) of every Parent write and every write of the listed
  properties since its last call (a listed `"*"` watches every property and SetAttribute, Kind
  `"attribute"`), for censuses that must name the script behind a Deferred event
  (docs/engine.md). `Context.Census(Depth?)` returns `Take(Collect?)`, a JSON census of what a run
  holds now by the Luau line behind it: live threads by the line that made them (with where each is
  suspended and what will resume it), connections by the line that connected them, handler runs and
  threads made since the last Take, pending task.wait / task.delay / deferred resumptions, and live
  instances by class and top-level parent; printed every N virtual seconds of a long run, it shows
  what accumulates (src/census.rs). `Context.Holders(Filters, Examples?, Requeue?)` names what keeps
  the trees outside the DataModel whose root starts with a filter alive: a property of a live
  instance, queued work, a pin, or the shortest Luau way from a module result, a handler, a thread,
  the globals or the registry (src/lua/holders.rs). `Context.Checkpoint()` marks where a run's setup ends (the game's modules
  required, the world built: what every run with the same arguments does alike) and what is read
  afresh each run begins (the module being worked on); it does nothing to the run, and a service
  prepares the next run up to it (`--service` below). Only its first call, on the harness's own
  thread before the harness first yields, counts. `Context.FrameCheckpoint()` is the same for a setup
  that runs frames (a match server's boot, in a `Prepare`): called on the harness's own thread after
  it first yielded, the harness waits there and the run resumes it before the next frame, in every
  run alike; a service prepares the next `ree run` up to there for the jobs with the same `--fps`
  (and `--frame-times`). `Context.ReseedRandoms(Seed)` restarts every Random alive (one a setup made
  before the run's seed was known included) from Seed and the order it was made in, and returns how
  many it restarted. `Context.SetRandomSeed(Seed)` reseeds math.random and the next unseeded
  Random.new() from then on. `Context.WorkSeconds()` is the script work charged so far in seconds of
  Studio's time, the same however fast the host is. `Context.FireEvent(Instance, Event, ...)` fires an
  engine event the emulator does not raise itself to the connections made to it (a Humanoid's
  `Running` while the harness walks a character, which nothing simulates here), with nil, boolean,
  number, string and instance arguments. A server run's `Context.SetJoinData(Player,
  JoinData)` gives a fake player, before it joins, the join data a teleport would have carried
  (`TeleportData`, `Members`, ...), which `Player:GetJoinData()` then returns as a fresh copy.
- A harness may split its setup instead: `return { Prepare = function(Context) ... end, Run =
  function(Context) ... end }`. `Prepare` gets a Context with `StandIn`, `Mount` and `Checkpoint` but
  without the `--context` fields, `Seconds` and `Fps`, so it can only do what every run of the harness
  does alike (stand-ins, the game's modules required, the world every scenario shares); where it
  returns is the checkpoint. Then the same Context gets those fields and `Run` goes on with it,
  mounting what the run varies (a character's bundle passed in the context, say) with `Context.Mount`. A
  service's prepared run of such a harness serves every job whose other arguments are the same:
  every character, item and scenario. A `Prepare` that yields prepares nothing.
- Frame 0 is sampled once the harness has started; then the clock runs `--fps` frames a second for
  `--seconds` (`--mode play`, the default: `RunService:IsRunning()` is true, one Heartbeat and one
  render step per frame, `Stepped` fires; `--mode edit` runs the edit-mode clock). A script that prints a
  line that is exactly `@done` ends the run at the end of that frame: the summary's `frames` are the
  frames that ran, and it says `"done": true`.

## Run context

- `--run-context server` runs the scripts as a Studio Play-mode server (docs/engine.md "Server run
  context"): `RunService:IsServer()` true and `IsClient()` false, no local player (Players starts empty;
  a harness parents `Instance.new("Player")`s to it), nothing renders, in-memory DataStoreService,
  MemoryStoreService and MessagingService that start empty every run, and EncodingService (Zstd,
  Base64); `FireClient` / `FireAllClients`
  reach no client and `InvokeClient` waits until its player leaves. It needs `--mode play`. The default,
  `client`, is a playing client with its local player, as before. `--studio false` makes
  `RunService:IsStudio()` false, as on a live game's server or client, so the game takes its live
  branches (TeleportData from `Player:GetJoinData()`, live costs and cooldowns); the default is true.
  It is part of a prepared run's key.
- `--device desktop|touch|gamepad|phone` (or `REE_DEVICE`) is the device the client emulates (UserInputService's inputs;
  desktop by default). `phone` is a touch device whose work clock charges a phone's prices: the desktop prices times a
  sim-to-phone calibration of script time, 2.77 (docs/engine.md "Work clock"): a conversion, not a measurement.

## Engine options

- Options take `--key value` or `--key=value` (the form for a value below zero). Engine options
  (src/launch.rs): `--roots "<dir>;<dir>"` (the project's Rojo roots; without them there is no project,
  only the harness and its mounts), `--project <dir>` (the project's folder) and `--place <name>`;
  `--mount Service/Name=<dir|file>` and `--instance Service/Name=<ClassName>` (repeatable),
  `--snapshot-dir <dir>` / `--snapshot <Service.rbxm>` (Studio exports; in a checkout, its `data/place-content/<Place>/<parent path>/*.rbxm` replaces a hierarchy snapshot's name-and-class placeholders with the stored instances, no flag), `--run-context client|server`, `--player <name>`,
  `--place-id <n>`, `--streaming <radius>` (the place streams: Workspace.StreamingEnabled and its
  StreamingTargetRadius, as rur.json's `streamingTargetRadius` sets them per root), `--viewport WxH`,
  `--offline`, `--verbose`. Content naming a local file (`file://<path>`, or a path such as
  `output/x.mesh`, as a game's asset id modules can name a mesh or image until it is uploaded:
  CreateMeshPartAsync, CreateEditableMeshAsync, CreateEditableImageAsync) reads a relative path from the
  project's folder: `--project`, else the folder holding the first root, else the current directory (a
  run of mounts alone; through `--service` its client names that as `--project`). `--assets-at
  <Service.Path>` and `--assets <prefix>,...` choose which asset manifest bundles mount where (over the
  project's rur.json `modelAssets`; `*` is every bundle; docs/engine.md "Asset bundles").
  Bundles come from an asset worker, the executable `REE_ASSET_WORKER` names, which downloads
  the assets the manifest pins (docs/engine.md "Asset bundles").

## Limits and guards

- A run cannot outlive its job (src/watchdog.rs). Roblox's script execution timeout: a resumption that runs 10 s of the
  work clock without yielding errors with `Script timeout: exhausted allowed execution time` at its next safepoint in
  script code, and at every one after until it yields or ends, so a `pcall` cannot keep it going. The work clock counts the
  VM's instructions, calls and allocations and the engine calls at Studio's prices, so the timeout fires under
  `REE_FROZEN_CLOCK=1` too, and on a busy machine as on an idle one; 60 s of host time in one resumption fires it as well. `--script-timeout <s>` /
  `REE_SCRIPT_TIMEOUT` (0: off). Each one writes `ree: script timeout <n> at <stack>` to stderr; the third stops the run:
  the frame loop ends it, stdout has the summary (status `error`, the failure names the stack) and ree exits 126
  (`--max-script-timeouts <n>` / `REE_MAX_SCRIPT_TIMEOUTS`, 0: never). A wall-clock cap, 600 s plus 20 s per `--seconds`,
  exits 124 (`--max-wall-seconds <s>` / `REE_MAX_WALL_SECONDS`, 0: off), and ree exits 125 when the process that started
  it is gone, at once when it is gone already (`--no-parent-watch` / `REE_PARENT_WATCH=0`). The `--service` client keeps
  the cap and the parent watch itself; the service's job has the timeout and returns its 126, and lives no longer than
  its client: a job whose client left while it waited (it exited, was ended, or gave up at its cap) is not run, and a
  running one stops before its next frame once its client's connection has closed (exit 125, no files written).
- What a script exhausts is that script's error and the run goes on: past 900,000 threads and values held for scripts
  (mlua's reference stack holds a million) task.spawn / defer / delay and the calls like them raise
  `<call>: too many threads and values held for scripts (...)` at the caller, and waits past 950,000 (docs/engine.md
  "Held values"); HttpService:JSONEncode refuses a cyclic table and nesting past 300 levels, as Roblox does; the command runs on a 64 MiB
  stack, as the service's workers do.

## What a recording holds

- `record` writes `scene.rbxm` (every instance that was ever in the world, as it was when it appeared,
  hidden until then and after it leaves), `playback.json` (ror's playback: appearances and removals,
  `set` actions and keyed tweens, motion tracks, `Emit(n)` / `Clear()` (each emit with where its
  emitter was at the call), moves as `parent`, instance references written after an instance appeared
  as `set` actions naming the referenced instance's path, Lighting),
  `events.json` (prints, warnings, errors, sound plays and stops with their times), `camera.json` and
  `summary.json`, which is also printed on stdout; a `ree: record ...` line goes to stderr.
- `--timing-check` (or `REE_TIMING_CHECK=1`) records the harness again under faster and slower clients and
  compares each recording with the first. A script that times itself on `os.clock` within a frame (a visual
  clock's waits, an elapsed-time sum, a work budget) draws, sounds or works differently in a client whenever
  its real work moves, which ree's modelled work and a frozen clock do not follow. Each probe runs the in-frame
  clock on a multiple of the modelled work (never frozen; a frozen run's wall clock stays pinned) across the
  range a client may lie in: the work clock's current calibration reads 0.55-1.34x a client's script time, so a
  desktop client's is 0.75-1.82x the modelled work, and a phone's script time is 2.13-4.16x a desktop client's. A
  desktop run probes 0.75x (the fastest desktop client), 1.82x (the slowest) and 7.56x (the slowest phone); a
  `--device phone` run, whose clock carries the phone's 2.77x already, probes 0.57x and 2.73x of it (the fastest and
  slowest phone);
  `REE_TIMING_PROBES=a,b` names others. Each probe is compared with the first recording in playback.json's lists,
  camera.json, events.json (output lines and sounds) and the work each frame did (the summary's `frame_work_ms`,
  recorded for timing checks): a threshold on os.clock flips under whichever probe crosses it, slower or faster.
  If a probe differs, an A/B whose recordings read identical has not shown the look holds there. The verdict is
  `timing` in the summary and `timing.json` (`hinges`, the earliest difference with its time, list, what and
  probe, each probe's verdict and differing counts per list, and `range`: the factors tested and their basis), and
  the status line ends `hinges on sub-frame timing under <factor>x (<client>) from <t> s (<list>: <what>); no
  change at ...` or `sub-frame timing: no change (tested <low>x to <high>x the modelled clock: ...)`. The
  probes' files are removed. A warm service's prepared run is not checked.

## Animation

- AnimationTracks play (docs/engine.md "Animation"): `Animator:LoadAnimation` (and the Humanoid's and
  AnimationController's) loads the KeyframeSequence its AnimationId names, and each play-mode frame
  the Animator writes the blended pose into the rig's Motor6D / Bone Transforms. Keyframes come from
  `--animations` directories (`<id>.rbxm` / `.rbxmx`), the download cache (`<state dir>/animations`)
  or the asset worker (its `asset` command); an id
  none of them has is warned about once and its tracks have Length 0. As in Studio, an asset's first
  request in a run arrives 0.45 s later (its tracks keep Length 0 and no pose until then; a later one,
  or one ContentProvider:PreloadAsync waited for, loads at once).
  The asset may hold a KeyframeSequence or a CurveAnimation; both play.
  `KeyframeSequenceProvider:GetKeyframeSequenceAsync` returns a copy of the same KeyframeSequence (it
  raises for an id none of them has; of a CurveAnimation it returns the KeyframeSequence Studio makes
  of one) and `AnimationClipProvider:GetAnimationClipAsync` the clip as it is. The default `Animate`
  script is not emulated.

## What is not simulated

- Nothing else is simulated: particles, trails and beams are what ror draws from the emits and switches;
  physics does not run beyond free fall; spatial queries see Block parts as boxes, Ball parts as
  spheres, Cylinder parts as cylinders and anything else as its oriented bounding box. Rigid joints
  (Weld, Motor6D with its Transform, WeldConstraint, ...) do carry the parts they join, as Roblox's
  kinematics do: moving a rig's HumanoidRootPart moves its body (docs/engine.md, "World and recorder").

## The warm service

- `--service` runs the command in `ree serve`, a warm process of this build and environment that
  keeps the reflection database, the project's module sources and bytecode and the converted
  manifest bundles between recordings (each recording still gets its own engine, VM and
  DataModel; ~0.25 GB private while idle). It runs several recordings at once, so callers sharing it
  do not queue: `REE_SERVE_WORKERS`, else one per 4 hardware threads and per 4 GB of memory, 1 to 4
  (`ree serve --workers <n>` when run by hand); a recording does not start beside others while less
  than 2 GB of memory is free. The client prints what the command prints and exits
  with its code. A lone command runs in its client (a warm service saved a sim nothing measurable,
  and starting one costs ~0.4 s); a command within 10 s of the last one of this build and environment
  (a batch, `@align` passes, parallel lanes) starts the service (detached, from a copy of the binary in `<state dir>/bin`, so cargo can rebuild
  `ree` meanwhile; a newer build replaces it), and the commands after it use it while it runs. It runs in its state dir, not the folder it was started
  from (a process holds its folder: a worktree could not be removed), and exits 20 s after its last
  job (`--idle`; health requests and runs prepared ahead are not jobs; a job whose client has gone stops, so it holds
  no service up: /health counts it `abandoned`). `ree serve --help` lists its options; one it does not know is refused. A service is
  found by its build and by the environment variables a run reads (`RUR_*`, `REE_*`, `ROR_*`), so a
  client with another environment gets its own. Without a reachable service
  the client runs the command itself, as it does for a Luau profile (`RUR_LUAU_PROFILE`). A record
  whose process is gone (a crash, a reboot) is skipped without connecting to it. After a recording
  (or a `ree run`) whose harness called `Context.Checkpoint()`, the worker runs the same command again
  in a fresh engine up to that call and holds it there (src/record/prepare.rs): the next job of that
  command with the same arguments (`--out` and `--profile` aside; for a harness with `Prepare`,
  `--context`, `--seconds` and `--fps` aside too; `--run-context` is one of them, so a client's and a
  server's never share one) goes on from it when everything the run read before the
  call reads the same (the files and listings it read, the project's directories, the wall clock's
  hour), checked while it goes on; else it starts over. It records the same bytes as a recording
  made from the start (a 3 s sim: ~290 -> ~35 ms in the service; with the harness's `Prepare`, for
  every character and scenario). A run is prepared only when jobs like it repeat
  (another started within 10 s, or one waits) and its job read nothing a prepared run cannot check
  before the checkpoint (a mesh, an image: the preparation would fail alike); a preparation that fails
  is not tried again until the next job like it. A worker holds one
  such run (~0.2 GB); one for other arguments is dropped after the next job's reply. An idle worker
  holding nothing prepares the most recent recording too while fewer than two workers hold it, so
  the next job like it finds one ready while the other is prepared again. A job waits for a worker
  still preparing its run only when that is expected ready within 0.2 s; otherwise a free worker
  records it from the start (and then prepares one too). The service listens on 127.0.0.1 only and
  needs an `X-REE: 1` header and a local Host; it has no authentication, so it is a tool for a
  single-user machine (SECURITY.md).
- `ree serve --warm [record's engine options]` starts the service of this build and environment when
  it is missing (run it with the environment the recordings use) and reads in what
  a first recording of the project reads: the last module lists (`record/*.txt`) of the harnesses run
  against this project (their files in its roots or its `external` data roots) read and compiled, the
  plans of the bundles recent recordings of that mount imported, the project scan, the Studio hierarchy
  and Engine.Start; a list whose checkout is gone (its first file's folder is) is deleted on the way.
  It records nothing, writes only caches in the state dir,
  prints a JSON summary (`{"warm":true,"modules":...,"modulesRead":...,"bundles":...,"bundlesRead":...,"seconds":...}`:
  what is ready, and what of it was read now) and exits 0 once
  the service is ready (1 when it could not reach one); the service then idles out as usual. A build
  without it exits 2 with `ree serve: unknown option --warm`.

## Profiles and caches

- `--profile` alone prints where the command's time went on stderr: the mount phases, the harness start,
  module loads, bundle imports, the frames and the files (`prepared: used`, or why a prepared run was
  stale, when a service's worker held one). `--profile boundary` alone is the boundary profiler of before
  (Luau-to-engine crossings per call site, priced; `--profile-out <file.json>`, its folder made when missing).
- `--profile <kinds> [--profile-out <dir>]` profiles the run (docs/engine.md "Profiling"); kinds are
  comma-separated, and a word after `--profile` that names no kind (a misspelt one) stops the command before
  it starts, exit 2, with the kinds listed:
  - `work` (also `instructions`, `lines`): every instruction the VM runs is counted on its own line, and
    what the work clock charges (the instruction's own cost, the engine calls it makes, what it allocates)
    is booked to the instruction that caused it. Per function and per line, self and total: instructions,
    work in milliseconds of Studio time, calls, by opcode; callers and callees; the hottest functions'
    source annotated. Exact, and the same every run of the same code. A C function a call reaches is a
    function of its own at that call site (`table.sort`, `string.byte`, `Instance:GetAttribute`: named by
    where the globals hold it, or by the method a method call looked up), with its calls and what was charged
    between the call and its return, whether or not it calls Luau; a Luau function a C function calls (a sort's
    comparison function) counts a call each time it is entered. ree compiles modules at `-O2`, which
    inlines small local functions into their callers: such a function has no frame of its own there, so its
    calls are counted where its inlined copies run, listed under "inlined functions" (docs/engine.md
    "Profiling").
  - `caches`: what each of Luau's inline caches did per call site, as the interpreter did it: the cached slot
    served it (hit), the key was at another slot (repredicted), the key is absent (a full lookup for nil), a
    new key was inserted (and whether the table rehashed to fit it), metatables were walked (how many, a table
    or a function at the end), the environment is unsafe, or the value is not a table (and which type's
    metamethod served it). The VM keeps a site's counts for each phase it ran in, and the tables cover the reported
    phases (`--profile-phase`). What the outcomes cost is apart by whether the work clock charges them
    (`luau_caches.prices.kinds`, each outcome `charged` or not). Charged (a rehash's keys and storage, an import looked up
    each time, an Instance member's engine price) is the clock's own charge at the site, as the VM booked it
    (`charged_work_us`, which holds whatever else the clock charged the instruction past its dispatch, a table's cold first
    touch in the frame among it): part of its function's work, never added to it. It splits exactly into `first_touch_us`,
    the first touch in a frame of the table the site reaches (its operand's, the environment's for a global), which any change
    to the site still pays, and `outcome_charged_us`, the rest. At a site whose inserts rehashed, `presized_us` is what
    allocating those tables' storage at the size each rehash made, up front, would have charged beyond the storage they were
    built with (a presized constructor's or `table.create`'s cost in their place, at the clock's price for storage), so
    `outcome_charged_us - presized_us` is what building them presized saves (each with a `_per_frame` figure, and summed by
    kind). A site that did more than hit (a charge, another outcome) lists its records apart by the call-tree node it ran in
    too (`nodes`: each with `node`, its index in `tree`, its outcomes and the same figures, which add up exactly to the
    site's; the per-frame ones are those over `luau_caches.frames`), so a function called from two places, a comparator under
    `table.sort` and from a loop, has its charge in each; a site that only hit has none. Uncharged (a reprediction, an absent key, an insert
    into a table with room, a metatable walk, an `__index` function's call) is priced at committed prices
    (profiler/prices.rs, the same in every profile): a ratio measured on ree's interpreter (a module's safe environment,
    values passed as arguments, interleaved rounds, in several processes in each of several sessions at different times:
    the median of every process's median, the least and greatest of them the spread; the CPU, the sessions and their dates
    are its provenance) times the work clock's price for the base opcode (a global read's, a keyed read's for a table key and
    a method's lookup). A site's `extra_work_us` is those, work beyond the clock's; a price whose spread reaches zero (the
    outcome might cost nothing over a hit) is within the noise, shown and left out of every sum (`within_noise` counts the
    outcomes). The builtin calls the compiler made fast calls of (`builtins`): per builtin, the
    calls its fast path served and the ones it declined, by reason (the argument count, a type, the results asked for, a
    collector step owed, an unsafe environment, values out of its range), with what the work clock charged the declines
    (`decline_charged_us`, and each site's: what the VM booked for a declined call, its fast call's dispatch, its fallback's
    instructions and the function their CALL reached, a C function's work with its cold first call in the frame or the
    engine's own Luau, as Vector3.new's constructor; part of the function's work), what the fast path would charge the same
    calls (`fast_equivalent_us`: its dispatch and the builtin's own counted work there, a new string's interning, the
    objects and table storage it allocates and the keys its tables' rehashes move, which a fix still pays, so
    `decline_charged_us - fast_equivalent_us` is what it saves), a site that declined apart by call-tree node (`nodes`, adding up exactly to it), and advice where an equivalent
    call takes the fast path. ree's own tool code (`rur.record`, a harness
    one of ree's tools ran) is counted apart (`luau_caches.tool`, `builtins_tool`), out of the tables and the advice, and
    out of the marks' cache totals. ree's own caches (member atoms and dispatch, datatype values, Instance userdata, require results,
    module bytecode, spatial query solids, bundle plans: hits, misses and evictions) are listed under
    `emulator`: they say how fast ree runs, not what the game costs; what three of them say of the game is
    listed as its facts (`game`: the instances its scripts touched, the modules that ran, the datatype property
    reads). These are software caches, counted exactly by the patched VM (patches/luau/12-profiling.patch).
    They are not the CPU's caches, and no CPU counter is involved: the CPU's cache misses are the `hw` kind's
    (Hardware counters, below).
  - `alloc`: bytes and counts allocated, by type (table, its storage, function, string, userdata by
    Instance class or datatype, thread and its stack, buffer, function prototypes) and by the Luau stack
    that allocated them. Every allocation is counted by type; stacks are taken for every allocation unless
    `--profile-alloc-step <bytes>` samples them (weighted by the inverse of the chance). And the tables that rehashed to
    grow (a table grows only by rehashing, which moves every key it holds into its new parts): by the stack that made
    them rehash and the stack that made the table, with the keys moved and the parts' bytes before and after (profile.json
    `alloc.rehash`, the summary's "table rehashes").
  - `heap`: a snapshot of what the VM holds at the start, at every `Context.ProfilePhase` mark and at the
    end. Its figures are the reachable heap, what scripts and the engine still hold (what a full
    collection would keep): by type and Instance class, the biggest objects by what they keep alive (their
    dominated size) with the path that holds them, the run's own reachable objects by the stack that
    allocated them. The garbage the collector has not swept yet (what that collection would free) is
    counted apart, by type, and is in none of those figures: how much there is depends on where the
    collector's cycle stood when the snapshot was taken. A snapshot collects nothing. Its reachable tables by the stack
    that made them (`tables` in each snapshot): how many, their bytes (a table with its array and hash parts, and the
    parts apart), their array and hash parts as distributions (slots, those holding a value, least, median, greatest, a
    power-of-two histogram), how many levels of tables each is with the tables it holds, and the rehashes that grew them;
    the summary lists the biggest sites. And the collector's work (steps, cycles, bytes freed, host time).
  - `timeline`: a row a frame: work, host time and the run thread's cycles, instructions, the heap,
    allocations, the collector, the live instances in the DataModel, alive outside it and destroyed (kept until
    the VM collects them), connections and the threads made. A kind that slows the run (stepping, the
    allocation and heap hooks, host time, the boundary) leaves the frame's host time and cycles out: `cpu` takes
    them in a pass of its own.
    A phase mark inside a frame closes the row so far; its parts share the frame number and add up to one frame.
    With work, `game_ms` and `harness_ms` split each row's code work. `timeline.function_stats` stores
    `[function id, total work us, frames active, largest frame us, frame number]` over the reported phases.
    `timeline.node_frames` stores `[row index, node id, work us, ...]` only on whole frames reaching half the
    recorded budget. `site_frames` keeps those frames' net site savings and `replacement_frames` their shared
    known shared replacement work. Small events keep their summaries. When a replacement's cold work is known,
    `function_contexts` stores compact inclusive activity ranges
    `[function id, node id, first frame, last frame, ...]`. A combined fix pays cold work only on the callers' frames
    left running. No per-frame work history is stored for these ranges. The report page omits frame evidence.
  - `cpu`: from the run's start (its VM's) to the profile's end: the run thread's CPU time (its cycles at the
    timestamp counter's rate) by phase and by frame, the process's CPU time and page faults, the working set
    and private bytes at the run's start and their growth, the most working set a frame's end saw. Nothing is
    estimated. Asked with a kind that slows the run (`all` is), the command runs again first, as a process of
    its own with `cpu` alone, into `<profile>/cpu-pass`, which keeps only its `profile.json` (its recording, if any, and
    its own report go): the CPU figures and the timeline's host time a frame come from there. Each run records its script
    timeouts and those host time fired alone (the timeout's host-time backstop, which the profiling's slowdown makes
    likelier); when the pass went another way (other timeouts, frames or status), the summary and `cpu.pass_differs` say
    so. `ree profile try`'s profiled runs leave `cpu` out (its timed runs take host time and cycles). The CPU's hardware
    counters are `hw`'s.
  - `hw`: the CPU's hardware counters on the run thread ("Hardware counters" below): instructions, cycles
    and IPC, the cycles stalled waiting for load data, the L1 data cache's misses outstanding and how long they
    took, branch mispredictions; for the run, each thread, each phase and frame, and each Luau function,
    calling context and call site, from readings around sampled calls.
  - `phases`: the wall-time phases of plain `--profile`, as a tree: a phase timed inside another is its child
    (the recorder's first walk inside the harness's start), each with its own time beside its total, the
    command's own the time no phase covers; the own times add up to the command's (`phase_tree` in
    profile.json).
  - `time`: host time, sampled each millisecond the ticker moved and booked to what ran since the sample
    before: the Luau stack at a call, return or loop edge; the Luau that ran last when the interpreter is left;
    the engine's step that was open when it is entered (the recorder, the animator, joints, physics), else
    `(ree engine: outside scripts)`; an Instance API call on its own line. A sample is weighted by the run
    thread's CPU time (wall time beside it), so time the thread was not running is not booked. A line's time lands on
    its next safepoint: a line with no call, return or loop edge of its own has its time on the line of the one after it.
    An inlined function's host time is an estimate (`inlined.estimated_time_ms`): a sample stands for the instructions
    since the safepoint before it, and a copy `-O2` inlined gets its share of them by the work clock's prices of their
    opcodes, a leaf's with no safepoint of its own too; a sample right after a call that reached a C function is not split,
    as it holds the call's own time, while one after a call to a Luau function, which that function's own safepoints
    sampled, is: each sample says whether a C function ran since its frame's safepoint before it (the VM's count of the C
    calls scripts make, read at each safepoint while host time is sampled, kept for each entry into the interpreter: an
    interrupt from inside a C function, as string.gsub's matcher sends at a match, and the safepoints of a Luau function it
    calls back leave the count to the calling frame's next safepoint, after the call returned), so a call site that reaches
    both is split after its Luau calls, and a builtin call its fast path served is no C call. About 1.2 times slower on a
    loop-heavy run.
    `boundary`: the boundary profiler, its report in the summary and its data in `boundary.json` in the
    profile's folder: a repeat read is a value read again in the frame while it was unchanged, split by where
    a cache would go (the same call: a local; a later call: read it once in the caller; another function), and
    a write that stored what the property held is an unchanged write.
  - `all`: `work`, `caches`, `alloc`, `heap`, `timeline`, `cpu` and `phases`. `time`, `hw` and `boundary`
    are asked for apart: counting every instruction slows the interpreter several times over, so host time
    and the CPU's counters are read in runs of their own (`cpu` takes its own pass). The cpu pass is the command run a
    second time, so `all` takes longer in wall time than its kinds without `cpu` (about 1.3 times on a recording): the price
    of CPU figures a slowed run cannot give.
- A profile writes, in `--profile-out` (default `<out>/profile` for `record`, `<state dir>/ree-profile` for a standalone
  `run`, `ree-profile` in the current folder for a host or checkout build):
  `summary.txt` (also printed on stderr), `profile.json` (everything), folded stacks for flame graph tools
  (`work.folded` in microseconds, `instructions.folded`, `time.folded`, `alloc.folded` and
  `alloc.counts.folded`, `heap.folded`, `counters.<counter>.folded`), `profile.speedscope.json`
  (speedscope.app) and `report.html`, one self-contained page (hot paths with callers, callees and
  annotated source, a call tree, memory, the timeline, caches, the CPU and its counters). `--profile-top
  <n>` sets the summary's rows; `--profile-phase <a,b|all>` the phases the summary's function tables and
  the boundary profiler report: by default `steady` when the run marked one (`Context.ProfilePhase`), else
  every phase. The summary's tables are the game's code: the harness's own (`--harness` modules are
  mounted under `ReplicatedStorage.RURFixtures`; the engine's `rur.record`, which calls the harness, is
  counted with it) and what it called are set apart, with their total on the
  summary's `reported below:` line and their top functions in a table of their own. Lines are counted for
  the whole run, not per phase. In profile.json each function has `harness` and its figures per phase
  (`phases`, with `harness_self_work_ms` and `harness_cold_work_ms`, the parts done for the harness's code), and `scope` names the
  reported phases with each side's self work. A function's `tool` marks ree's own code (the engine's chunks
  that drive a harness, and the harness of a run `ree profile try` made), never the game's nor a user's
  harness; `short` is its module's last segment, qualified where two modules would share one. A C function's
  `call_sites` give its callers and the lines that called it; a tree node's last element is the line that
  called it.
  The work clock's figures read in the units the run's clock ran on (`work.units`: a `--device phone` run's are the
  desktop prices times 2.77), and the summary's headline names the device and the reference client. Each function's
  self work splits into kinds (`self.cold_work_ms`: the frame's cold and first-use charges; `collector_ms`: the
  collector's share of what it allocated; `dispatch_ms`: the engine's delivery of its handlers and resumption of its
  threads; `by_provenance_ms`: priced from a client, from a Studio-era measurement or by a borrowed price), and
  `saving_ms` is what cutting it saves: its hot work and only the cold charges for objects nothing else touched in
  their frame (a shared object's charge moves to its next user). A function's and a line's totals count its
  outermost call on a path only, so recursion adds nothing twice. profile.json's `cold` lists the cold and first-use
  charges by object (a table by the line that made it, a C or engine function, an instance's class, a member, an
  engine path) with who paid them and in how many frames other code shared them; `handlers` the event handlers by
  the line that connected them (connections, runs, dispatch and whole work); `resumptions` parked threads' resumptions
  by the line that waited; `clock_reads` the lines that read os.clock: `reads`, `frames`, `runs` of their function
  that read it (a call, or what is left of one after its thread yielded), `iterations` (two reads by one run) and
  `iteration_us` (the clock's advance between them, callees included, in the clock's units); a line that reads it
  more than once a run is an os.clock budget loop (`budget_loop`), which the summary lists with its work an
  iteration: its total is set by its budget, so compare that, and profile on the work clock, not a frozen one. A read
  after a yield starts a run (across a yield the clock's advance is whatever else ran), so a loop that waits each
  iteration is none; `unmeasured_fires` the events of kinds no bench measured.
  `fidelity` is the profile's own band for what the summary reports (`weighted`: the reported phases' game code): each
  component's error on rows no fit saw (cost.rs `BENCH_BANDS`; `components.<name>` `{share, band, rows, basis}`)
  weighted by that code's work in each, the Studio-era and borrowed shares at their bands; `band` folds a phone's
  factor's spread in (`clock_band` is the band before it, `device.ratio_band` what the spread multiplies it by).
  `work.collector` gives the collector's price an object (`object_ns`, desktop ns) and the objects it charged; the
  work's kinds are in `work.cold_work_ms`, `work.collector_ms`, `work.dispatch_ms` and `work.by_provenance_ms`;
  `cold.total_ms` is the run's cold and first-use charges and `cold.objects_ms` the part its objects carry (the rest
  is functions' own code, closures and first runs, in their cold column). `work.prices` gives the work
  clock's price of one count, in ns at desktop prices (a phone run's are these times `work.units.scale`): `object_ns` (an
  object a script's instruction made), `stored_byte_ns` (a byte of its table storage), `rehashed_key_ns` (a key its
  table's growth moved), `c_call_ns`, `sort_compare_ns`, `sort_call_ns`, `allocated_byte_ns` (a byte any script work
  allocated, its library calls' too), `hash_rehashed_key_ns` (a key a growth put back into a hash part),
  `large_block_ns` (an allocation over 1 KB), `library_object_ns` (an object a library call made) and
  `collector_object_ns` (the collector's charge an object, which `collector_ms` holds and the others leave out).
  `work.unpriced` gives the bytes the clock counts and prices at nothing: `buffer_fresh_bytes` (new buffers past Luau's
  own pages, over 1 KB) and `buffer_big_bytes` (buffer copies, fills and string writes of over 64 KB a call); the
  summary prints them on an `unpriced:` line when a run has any, as the clock reads that work low.
  The summary's `engine operations` line counts what the reported phases' scripts asked the engine to do, whatever the
  work clock prices it at (the engine's side of a cast, which the work clock reads lower than a client pays): instances
  cloned (each of a copy), made with Instance.new, destroyed (each of the subtree) and taken into the Workspace; property
  writes, those that changed a value, and among them the moves and resizes of a part in the Workspace and the switches of
  an emitter, beam or trail; pivots of what is in the Workspace, parts bulk-moved, emits and descendant queries
  (`scope.engine` in profile.json: `cloned`, `created`, `destroyed`, `entered`, `writes`, `changed`, `geometry`,
  `toggles`, `pivots`, `bulk_moved`, `emits`, `queries`). They are counted at the engine, so they are every script's,
  the harness's too, and what the engine's own Luau does inside a call it prices whole (a PivotTo's moves) is the call's.
  profile.json's `marks` (each `Context.ProfilePhase` mark) and `end` (the run's end) carry the counts so far, so a
  phase's figures are the difference of two marks': the work clock, host time and the run thread's cycles, instructions,
  the VM's work counters (`vm`: objects, their bytes, table storage, keys rehashed, cold first touches, large buffers'
  unpriced bytes), the allocation
  profile's bytes and objects, rehashes (all, and a script's own: `script_rehashes`, `script_rehash_keys`), the
  collector's steps, cycles, bytes freed and host time, the inline caches' outcomes by kind, and the engine operations
  (`engine`).
- `ree profile diff <before> <after> [--out <dir>]` compares two profile directories: the headline
  figures (the reachable heap at the end, and apart from it the garbage not swept yet), the functions
  whose work, calls, instructions or allocations moved, the lines of modules whose source is the same in
  both, caches, memory by type and the timeline; it writes `diff.txt` (also printed), `diff.json` and
  `diff.html`. When both profiles report the same phases (or both marked `steady`), the headline leads
  with the game's code and the harness's in those phases, and the function tables cover them, the
  harness's functions in a table of their own: a harness's work moves with what it scans, not with the
  game. The headline's `engine:` rows compare the engine operations in the same phases (the whole run when
  the profiles share none): instances made, instances destroyed, property writes, parts moved (by a write
  or BulkMoveTo), pivots and emits. Profiles from before the split are compared whole. Functions pair by module and name;
  an anonymous one, whose key carries its line, pairs across an edit that moved it by its compiled code and its text
  (profile.json `code_hash`, `source_hash`: the same code hashes the same wherever it lies; then, between those, in order),
  and shows the move (`moved from :15 to :18`). A function is called priced differently, not changed, only when the
  profiles show it: it ran the same path both times (the same calls, instructions and count of each opcode) and its cold
  and first-use charges (profile.json `cold_work_ms`, the work clock's exact split, which counts only the cold
  instructions that ran) account for the change in its work, to within 0.01 % of the rest of it (its warm part) and
  2 us. Where the diff covers some phases, a row leaves out `harness_cold_work_ms` as it leaves out
  `harness_self_work_ms`. The work clock is deterministic, so a move of who pays the cold charges leaves nothing
  else, and 1 % of a 50 ms function would hide 0.5 ms of real growth. Cold first touches and first runs in a frame go
  to whichever code touches first. The same instructions prove nothing on their own: a handler that clones a model of 200 parts runs the instructions one of
  10 parts does while the engine's charges for what it clones grow twentyfold, so a profile without the split tags no
  function, keeps those with the same calls and instructions among the changes, and says why in a note. Those tagged are
  listed apart, after the functions whose path or charges changed, with a line totalling each side's self work and each
  one's change in cold charges. A function or line is listed at all when its work moved by more than 1 ns (two sums of
  the same charges in another order part in their last bits) or its calls, instructions or allocations changed, the
  rule the viewer's Diff uses too. A measured figure (host time, the CPU's counters) has its change marked `~`: one run
  each gives it no verdict; host time is left out when either profile stepped every instruction (`work`, `caches`), which
  slows the interpreter several times over. The note
  that the runs ran other harnesses compares the harnesses' contents (profile.json `run.harness_hash`), so two copies of
  one harness in other folders are the same. profile.json carries a `version` (2), which keeps every row's shape, so an older reader still parses it: a `tree` node is `[parent, function,
  calls, self work in us, self instructions, label]`, and its calling line is in `tree_lines`, one a node (-1 for none);
  two profiles of different versions are compared without what changed meaning between them (a function's and
  a line's self work, the game's and the harness's code: a C function's work is its own row since version 2, not its
  caller's; a function's calls and instructions: version 1 counted a module's main chunk at none; host time, the wall
  clock's before; Luau's inline caches, builtin calls and ree's own caches), and the diff says so (`diff.json` gives those
  figures as null, the text as `-`). The viewer refuses a newer version and says on its Overview when it reads an
  older one, and its Diff leaves the same figures out across versions; the page says when its profile's version is not
  the one it reads. `ree profile report <dir>` prints a profile's summary again and rewrites its page from its
  profile.json. `ree profile view` explores one in the terminal and `ree profile render`
  draws its screens to files ("The profile viewer").
- With no profile the engine pays nothing measurable (a thread-local read where each profiler hooks in),
  and a profile changes nothing a script sees: the work clock, os.clock and a recording are the same with
  and without it (it allocates nothing on the VM's heap). A profiled command runs in its own process,
  never through `--service`. `REE_PROFILE=<kinds>` (and `REE_PROFILE_OUT=<dir>`, `REE_PROFILE_COUNTERS=<a,b>`)
  asks for a profile where a command line cannot.
- Caches in the state dir: `animations/` (downloaded animation assets, `<id>.rbxm`; delete a file to
  fetch it again), `model-bundles/` (bundle files as uploaded, `asset-<AssetId>-v<Version>.rbxm`, named by
  the asset version they hold and shared by every project that pins it, and the manifest's catalog: the
  worker starts only for a bundle the manifest pins anew; beside each, its
  converted plan, `<file>.plan`, valid while the conversion is the same (build.rs digests its code and
  data: a rebuild that changes neither keeps it): a run maps it and reads it in place (Windows; the
  pages are the file's, shared by every run reading it) instead of decoding and converting the
  bundle again),
  `record/<harness and project>.txt` (the module files a harness's last run read: the next run reads
  and compiles them on other threads), `model-bundles/recent-*.txt` (the bundles the last run of a
  mount imported, and those of the last run of each scene, a harness with its place and context: the
  next run of the scene decodes them ahead the same way; its first run decodes the mount's), `services/`, `bin/` and `serve.log` (the service). All are guesses that only
  save time: a stale one is noticed and the file is read as usual.

## Hardware counters

`--profile hw` reads what the CPU counted while the run thread ran, and how much of that went to waiting for memory:
per run, thread, phase and frame, and per Luau function, calling context and call site. The counters measure ree's
own process running the game's Luau: a guide to where memory costs and why (which function waits, how long a miss
takes), not what a client's frame costs, which the work clock estimates (`work`).

A process cannot read the CPU's counters on Windows (`rdpmc` faults in user mode), and the kernel trace session that
can record them needs the system's profiling right, which an account's ordinary processes never hold (User Account
Control takes it out of their token). So a small Windows service reads them on a profile's behalf (tools/hw-counters):

- **Install it once**, in two steps, so that no program the account can change runs elevated. First, from an
  ordinary PowerShell: `powershell -NoProfile -ExecutionPolicy Bypass -File <checkout>\tools\hw-counters\setup.ps1`.
  It builds the service (a Rust program with no dependencies; cargo on PATH) in `%LOCALAPPDATA%\ree-hw-counters-build`
  and prints the build's SHA-256, the script's, how to open an elevated PowerShell that runs no profile
  (`Start-Process powershell.exe -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass'`: right-click, Run as
  administrator, would run the account's PowerShell profile as administrator), and the command to run there:
  `setup.ps1 -Binary <exe> -Sha256 <hash>`, behind a check of the script's SHA-256 that reads the script once, checks
  those bytes and runs those bytes (a script block, told the script's path), so nothing written to the file after the
  check runs. It warns when PSReadLine sits in the account's own module folder, from which an interactive PowerShell
  loads it, `-NoProfile` too. That second step builds nothing
  and runs no program from the account's folders, only Windows' own (PowerShell's modules, imported from PowerShell's
  folder before any of their commands runs, so a script of the same name on PATH cannot stand in; sc.exe by its full
  path) and the installed copy. It gives the Program Files folder `ree-hw-counters` (the folder from Windows, not from
  an environment variable; a link there is refused) an ACL of its own (Administrators and SYSTEM full control, Users
  read and run, nothing inherited, owned by Administrators), reads the build into memory and checks its hash (a build
  changed after it was hashed is refused before anything is written), writes those bytes there as a file owned by
  Administrators with the same ACL, checks the file, removes everything else in the folder (an older program, its
  counters' profile, which the service writes again), only then registers it, and runs the installed copy's probe
  (below), its lines shown as they come. A service the Services console holds open stays marked for deletion: the
  step stops and says to close it (or to end a ree run starting it again). The script itself comes from the checkout,
  which the account can change: run it from a checkout you have read; its hash check keeps it from changing between
  the two steps, not before step 1. The command it prints to remove the service later is guarded the same way.
  When the hash check refuses, the script cannot be read or the setup fails, `$LASTEXITCODE` is left nonzero and the
  shell stays open, so the message can be read. If the old service does not stop, it is neither deleted nor replaced;
  the protected folder and the staged build may already have been prepared.
  `-User <name>` installs it for another account, `-Uninstall` removes it. A service installed before a change to
  tools/hw-counters keeps its own counter list: run setup.ps1 again after one (until then a profile that names a
  counter it lacks says so, and says to run setup.ps1 again).
- **What it grants.** The service runs as LocalSystem holding two privileges only: the system's profiling right and
  folder traversal. The one exception: when the installed copy's probe fails for want of a right and counts only with
  LocalSystem's own privileges, setup.ps1 leaves the service those and says so (when it fails either way, the two come
  back). It starts on demand and stops itself after a minute with no client. The account it was installed
  for may start it and read its status, nothing else. Its pipe opens only for that account (and SYSTEM), from this
  machine, one connection a process; the client process must run as that account and gets counts for its own threads
  only, never another process's, and at most two million slices of them (about 180 MB in the service: past them its
  results say how many more it dropped). Each reading makes two slices, and frame, engine-step and thread readings
  count toward the limit too, so how long a capture lasts depends on the workload. Measured on a Ryzen 9 9950X: heavy
  frames made about 7,000 slices a second (a limit of about 280 s), ordinary runs 31,000-87,000 (23-64 s), and a run
  whose threads idle in `task.wait()` loops about 640,000 (about 3 s). When the limit is reached, ree books only the
  run up to the last complete reading. The
  summary and the profile viewer's header mark the hardware-counter profile **CUT SHORT**, with the host time captured,
  and the rest of the run has no counter figures: count a shorter run when that mark appears.
  While any client counts (each 30 minutes at most) it runs one kernel trace session of context switches
  that carry the counters' values. Only one session at a time may hold the CPU's counters, so up to eight clients
  share it: a ree profile, roblox-sim's frame logs, two arms of an A/B side by side. The first client starts it with
  its counters, later ones count those or some of them (one that asks for others is told the service is busy, with
  what it counts), each gets only its own process's threads, and the last one out stops it. A client that leaves while
  others count waits a little over a second for the trace's last buffers. On AMD Family 1Ah processors it registers
  the AMD events below with Windows until the next restart, through Windows Performance Recorder (`wpr.exe`, part of
  Windows), each once. No driver is installed and no protection is turned off.

### What the counters count

Zen 5 counts the L1 data cache's misses only per line: its miss buffer (MAB) allocates one entry a line, and its fills
count a line once, however many loads missed it while it was on its way, and a line a prefetch was already fetching
not at all. So no counter there says how often a load missed, and a count of lines is no measure of what the misses
cost. What Zen 5 does count is the cost: the cycles nothing retired because the oldest op waited for load data, and
the misses outstanding each cycle. Those are the default set. The events are AMD's, as its Processor Programming
Reference for Family 1Ah and Linux perf's Zen 5 tables (`tools/perf/pmu-events/arch/x86/amdzen5`) give them:

| counter | counts | AMD event (event, unit mask) |
|---|---|---|
| `instructions` | instructions retired | the CPU's built-in source |
| `cycles` | core clock cycles while the thread ran | the CPU's built-in source |
| `load-stall-cycles` | cycles nothing retired because the oldest op waited for load data: nearly all of a miss's wait, part of an L1 hit's in a chain of dependent loads (measured below) | `ex_no_retire.load_not_complete` (0xD6, 0xA2) |
| `op-stall-cycles` | cycles nothing retired while the oldest op had not completed, as AMD counts it: every long wait (load stalls among them), short ones only in part (measured below) | `ex_no_retire.not_complete` (0xD6, 0x02) |
| `no-retire-cycles` | cycles nothing retired, for any reason | `ex_no_retire.all` (0xD6, 0x1B) |
| `miss-buffer-occupancy` | L1 data cache misses outstanding, summed every cycle (it counts more than one a cycle: up to about 20 when misses overlap) | `ls_alloc_mab_count` (0x5F) |
| `miss-buffer-allocs` | miss-buffer entries allocated: one a line the L1 data cache fetched, for loads and stores, the hardware prefetcher and page walks | `ls_mab_alloc.all_allocations` (0x41, 0x0F) |
| `miss-buffer-allocs-ls` | of those, the load-store pipes' (page walks' requests are among them) | `ls_mab_alloc.load_store_allocations` (0x41, 0x07) |
| `miss-buffer-allocs-prefetch` | of those, the hardware prefetcher's | `ls_mab_alloc.hardware_prefetcher_allocations` (0x41, 0x08) |
| `demand-fills` | lines filled into the L1 data cache for demand loads and stores, from any source: one a line; a load that missed a line already outstanding is not counted again, and a line a prefetch brought is not counted | `ls_dmnd_fills_from_sys.all` (0x43, 0xFF) |
| `demand-fills-beyond-l2` | of those, lines from beyond the core's L2 | `ls_dmnd_fills_from_sys`, every source but `local_l2` (0x43, 0xFE) |
| `demand-fills-beyond-l3` | of those, lines from beyond the core complex's L3 (another complex's cache, memory) | `ls_dmnd_fills_from_sys` `near_cache`, `dram_io_near`, `far_cache`, `dram_io_far`, `alternate_memories` (0x43, 0xDC) |
| `ls-dispatched` | load and store ops dispatched to the load-store unit (some later discarded) | `ls_dispatch.all` (0x29, 0x07) |
| `branches`, `branch-misses` | branches retired, and those mispredicted | the CPU's built-in sources |

What the stall counters count, measured with Linux perf's raw events on the same CPU (a Ryzen 9 9950X, WSL2), for
loops of dependent ops, a cycle of each op: a chain of multiplies (3.0 cycles each) counted 0.90 to 0.96 of each
multiply under `op-stall-cycles`, 1.5 under `ex_no_retire.other` and none under `load-stall-cycles`; a chain of loads
that hit the L1 (4.0 cycles each) 1.2 under both `op-stall-cycles` and `load-stall-cycles` and 1.9 under `other`; a
chase through 256 MB (690 to 760 cycles a load) 99 % of its cycles under both, nearly none under `other`. So the stall
counters hold the long waits whole and the short ones in part: `op-stall-cycles` is not every cycle an op spent
executing, and `memory share of op stalls` is Linux perf's split of what it does hold.

The fills were called `l1d-misses`, `l2-misses` and `llc-misses`, and `ls-dispatched` `l1d-accesses`: a profile.json
with the old names still opens in the viewer and in `ree profile diff`, `--profile-counters` still takes them, and
their Windows profile sources keep their first names (`ReeL1dMisses` is the demand fills), so a service installed
before the rename still counts them. Any other name is taken for a Windows profile source as `ree-hw-counters sources`
(or `wpr -pmcsources`) lists it.

The figures that measure what memory cost (the profile derives them wherever its counts allow, for the run, each
phase, frame, function and call site):

- **load stall share**: `load-stall-cycles` over `cycles`, the share of the run thread's cycles spent waiting for load
  data. The measure of impact: it rises with every miss that held up retirement, whether its line was counted as a fill
  or not.
- **load stall cycles per 1000 instructions**: the same cycles against the work done.
- **L1D miss latency**: `miss-buffer-occupancy` over `miss-buffer-allocs`, the cycles a miss stayed outstanding on
  average: about 12 to 16 from the L2, 50 to 70 from the L3, hundreds from memory. It says where misses were served
  from, not what they cost (fewer misses from farther away read a higher latency and may cost less), so the viewer and
  `ree profile diff` do not color it better or worse. The allocations include the
  prefetcher's and page walks' entries, whose lines the occupancy holds too (with load and store allocations alone a
  stream the prefetcher feeds read 8,300 cycles); page walks' entries are short, so a run that walks pages on most
  misses (a random walk through more memory than the TLB covers) reads a lower latency than its misses' own.
- **L1D misses outstanding per cycle**: `miss-buffer-occupancy` over `cycles`, averaged over every cycle (cycles with
  none outstanding count as zero): how busy the miss buffer was, not how many misses overlapped while any was
  outstanding, which would need a count of the cycles with one or more. Above one, misses overlapped; a high stall
  share with a low figure is a chain of dependent loads (each waits for the one before), with a high one a stream.
- Event rates beside them: miss buffers, demand fills and branch misses per 1000 instructions, the fills' sources as
  shares, IPC, and `memory share of op stalls` (`load-stall-cycles` over `op-stall-cycles`, Linux perf's
  `backend_bound_by_memory` split).

The default set is `instructions,cycles,load-stall-cycles,miss-buffer-occupancy,miss-buffer-allocs,branch-misses`
(the CPU counts six at once): IPC needs the first two; the stall cycles are the impact; occupancy and allocations give
a miss's latency, which says where the misses were served from, and how busy the miss buffer was;
and branch mispredictions are the other common way an interpreter loses cycles, which none of the others shows. The
fills are left out: a miss's latency says on average where misses were served from, and the fills undercount (a line
once, prefetched lines never). `--profile-counters a,b,...` chooses others: `demand-fills,demand-fills-beyond-l2,
demand-fills-beyond-l3` say where lines came from line by line, `ls-dispatched` beside `demand-fills` how few lines
the loads and stores needed fetched.

### How the counts reach functions

- **Readings.** The kernel records the counters at every context switch, so the service hands back each of the
  process's threads' stretches on a CPU with what was counted in each. The run thread takes a reading by switching out
  (it signals a helper thread and waits for its answer): at the profile's start and end, when the VM starts, at each
  frame's start and end, at each phase mark, around the engine's own steps (Animator, joints, physics), around the
  Luau calls it samples, and where a Luau thread that holds a sampled call resumes or stops. The counts between two
  readings are exact. A reading's own counts (the median of 64
  back-to-back readings at the start) are taken out of every stretch between two readings, and the summary says what
  share of the run's cycles the readings took. For the profile's length the run thread is pinned to its CPU and the
  reader's helper with it, so a reading switches between them on one CPU that never idles and the run thread keeps its
  caches (`REE_HW_PIN=0` leaves both free).
- **Calls.** The VM calls the profile's hook as a sampled Luau call's first instruction is about to run and as it
  returns (or an error unwinds it to a `pcall` or a coroutine's handler), and the profile takes a reading at each:
  everything counted between them ran inside that call, and what was counted while it was the innermost call read
  around is its own (its callees' apart). Each call is drawn on its own: every call of a function whose calls take at
  least the threshold (100 us at first), one in `threshold / its mean call` of a shorter one. A call inside a sampled
  call of its own function on the same thread is read too (up to 64 a sampled call, then drawn again), so a recursion
  is read as a whole: drawn level by level, the corrections multiply, and a function recursing four deep at one in six
  read 12 % off in a run of 3,000 calls (simulated), against 2 % read whole. The 64 are counted from each recursion's
  outermost sampled call on its thread, whatever other threads hold. A function's first 4 calls are all read, to learn
  its length; one whose calls take under 10 us on average is then not sampled (its counts are its callers', those
  first calls' too: decided when the fourth is measured, so a function long at first and short later keeps its long
  calls), but one of its calls is read for its length after 64 calls, then 128, up to 65,536, and it is sampled again
  once its calls grow; a sampled function stops only below 7.5 us, so one near the line does not flip.
  The mean weighs each measured call by the calls it stood for (a recursion's inner calls read with their outer one do
  not pull it down). The threshold rises when **call readings** come faster than 10,000 a second and falls when they
  come slower, staying between 20 us and 1 ms. Frame, engine-step and thread readings do not raise it, because sampling
  fewer calls cannot reduce them (they can still make the profiler expensive).
  A sampled call's own counts stand for the calls of its function that were
  not read around (times its `every`), and come off the call they ran in, which held those calls' counts: so each
  function's own counts are an unbiased estimate of what was counted while it ran (exact when every call is read
  around), its total and a call site's too, and the functions' figures add up to the run's exactly. An estimate can
  fall below zero (sampling noise, where what came off a function for its callees' unsampled calls outweighed what its
  sampled calls read): the summary lists those, the folded files leave them out and profile.json says how much.
  A thin wrapper between a caller and a heavy callee (a `pcall`, a sort comparator, a `coroutine.wrap` closure) does
  almost no work of its own, but its **self** estimate can be far off: measured values ran from -2,785 M to +1,755 M
  instructions, up to 11 % of the callee's, and its caller's figure is off by the opposite amount. The callee's own
  figure stayed within 1-2 %. Read the callee's figure for the work, and do not take the wrapper's self estimate,
  positive or negative, as work done or work saved.
- **Threads.** The VM's resume callbacks say where a Luau thread resumes and stops; one that holds an open sampled
  call takes a reading there. A stretch goes to the innermost open sampled call of the innermost running thread that
  holds one, else to the engine step running, else to the profile's own frames; a thread that holds none switches
  without a reading, since what it runs is booked where its resumer's is (a coroutine resumed from a function inside
  that function, a thread the engine resumes outside every call). A thread an error ends, or one that is reset (a
  coroutine closed, a thread reused), gives its sampled calls up there, as an unwound call leaves, and a thread the
  collector frees gives them up silently: no function keeps a call open for a thread that is gone. A sampled call that
  outlives the call it began in (a coroutine resumed later from elsewhere) still passes its correction to where it
  began, so the bookings still add up.
- **The limit, measured.** A reading's switch costs the code after it part of its caches and branch history, which a
  sampled call pays at its start and its caller after its return, and which the estimate does not take out: a short
  sampled function reads high and its callers low. On a Zen 5 CPU (Ryzen 9 9950X): a 1.2 us call (75,000 cycles for
  1,000 iterations unread, so 6,000 for 80) read around took 9,000 to 12,600 cycles and fetched 44 to 100 lines again;
  a 14 us call read around took 1 to 9 % more cycles for its instructions than the same loop run inline between its
  calls (700 to 5,800 cycles a call, four runs of 20 rounds of 300 calls and 300 inline passes); calls of 300,000
  cycles paid the same few thousand, about 1 %. Hence the 10 us floor. A
  reading took 1.6-1.8 us pinned (2.2-3.7 us unpinned; 23-25 us with the helper on another core, whose idle CPU was
  slow to wake); the harness's readings took 0.24-0.29 % of the run thread's cycles, and its phases took as long as
  with no readings at all, within their spread from run to run (`--profile cpu`: long calls 42.5-43.5 ms against
  42.9-45.0, short calls 97-108 against 95-102).
- **Lines.** A line's own code is its function's: the readings come microseconds apart, lines run in nanoseconds, so a
  stretch between readings spans many lines and no reading can tell them apart. A line's figures are what the calls
  made from it cost (the summary's "top call sites", profile.json's `lines`, with `"calls": true`; the viewer's
  Source view).
- **What else falls to the caller.** A call Luau's `-O2` inlined has no frame to read around (call a function through
  a table field to keep it apart), nor does a call into native code (`--!native`) or into a C function (a builtin, an
  engine method): their counts are the calling function's. A coroutine resumed from Luau counts as a stack of its own
  (its calls' stacks start at its body).
- **The script watchdog.** A resumption that runs longer than 0.1 s gets ree's script-timeout interrupt at every
  safepoint until it yields (watchdog.rs), whose work counts with the code it interrupts: measured, a loop retired up to
  20 % more instructions in the runs it armed. `--script-timeout 0` (or `REE_SCRIPT_TIMEOUT=0`) turns it off for a
  measurement; the numbers on this page were taken so.
- The counters count in user and kernel mode (the interrupts the thread's CPU took meanwhile included), as Windows
  Performance Analyzer's counter tables do. The process's other threads are totalled per thread.

### Where it shows

- **The summary**: the counts and every figure; a `memory:` line (the stall share, the stall cycles per 1000
  instructions, a miss's latency, the misses outstanding); the readings' own cost, the calls read (and those booked
  to their callers) and the readings at thread switches; by phase, thread and frame (with the frames' stall shares);
  the top functions by their own stall cycles (with each one's stall share and IPC), then those estimated below zero
  and what the folded files left out; and the top call sites.
- **profile.json** `counters`: `names`, `about`, `amd` (each event's AMD name), `attribution` (`method` `calls`,
  `version` 2: each call drawn on its own and the threads' switches read, a profile without one drew one call of each
  run and read no switch; the calls read and `calls_booked_to_callers`, `switch_readings`, the threshold, the readings'
  share of the run's cycles, `folded_left_out` a counter), `cut_short` (null, or the host seconds captured before the
  slice limit), `run` and `phases` with `rates` (the derived figures), `frames` (a row a frame: the counts, then
  `load stall share`, `load stall cycles per 1000 instructions`, `L1D miss latency`, `IPC`),
  `functions` (`self`, `total`, `self_rates`, `total_rates`), `lines` (call sites: `total`, `total_rates`), and a
  folded file a counter (`counters.<name>.folded`, each stack a sampled call's: below zero left out).
- **The report** (report.html): the stall share among the tiles, and the CPU tab's counter tables with the stall share
  of each phase, function and call site, and the frames' stall share and miss latency charted.
- **The profile viewer**: the stall share and a miss's latency among the Overview's cards; every counter in the metric
  switch (`m`), the stall cycles among them, which the flame graph, Calls and Source then show (a line: its calls); the
  Timeline's series of each counter and of the derived figures a frame; the Diff's headline; and an Issues entry,
  "Waits on memory", for a function holding at least 5 % of the run's stall cycles with at least 30 % of its own cycles
  stalled (from the counters only).
- **`ree profile diff`**: the counts both profiles read, the stall share, stall cycles per 1000 instructions, a miss's
  latency and IPC, and the functions whose stall cycles moved most (with their stall shares). When the two profiles'
  counters reached functions in other ways (1 ms samples, the first readings around calls, or these), it says so: the
  run's counts compare, the functions' only roughly. The viewer's Compare says the same.
- **`ree profile try`**: `hw_load_stall_cycles`, `hw_load_stall_share`, `hw_miss_latency`, `hw_miss_buffers` and the
  others below from its counter rounds.
- With `work`, `caches`, `alloc` or `heap` in the same run the counts include those profilers' own work (the profile
  warns).

### Checking it

- **The probe**: `ree-hw-counters probe` (`%ProgramFiles%\ree-hw-counters\ree-hw-counters.exe probe`; setup.ps1 runs
  it) counts kernels whose counts are known, through the service: a loop of four instructions and a branch an
  iteration, random branches half of which mispredict, loads that hit the L1 data cache, chases through random cycles
  of cache lines that fit the L1, the L2 and the L3 and fit none, arithmetic with no loads, and streams of lines read
  eight loads a line, in an order the prefetcher cannot follow and in order. Each counter must count what its name
  says (exit 0; 48 checks on a Zen 5 CPU, 47 when the L3 chase's speed is between the L3's and memory's and it is not
  checked): the arithmetic stalls on no loads; the memory chase (each step a load waiting on the one before) is at
  least 85 % stalled on loads, and its miss latency (occupancy over allocations) is 60 to 105 % of a step's cycles (its
  lines visited in 2 MB groups, so a step pays the cache's miss and not a page walk too); the random stream keeps more
  than one miss outstanding a cycle. The L3 chase's fills from beyond the L3 are read against the fastest of three
  tries' own speed (on a busy machine other programs push the buffer out of the shared L3). Then the readings
  themselves: a function chasing pointers through memory (each call going on where the last stopped, so every load
  misses to memory) called between calls of one doing arithmetic, 800 calls each, every call read around and then each
  drawn with a chance of one in four: the chase must hold at least 90 % of the two functions' stall cycles, the
  arithmetic at most 5 % of its own cycles, and one in four (times four) must read the chase's stall cycles within 20 %
  of every call's.
- **The undercount, as the probe measures it.** Eight loads to each line of a stream: through the service on a Zen 5
  CPU (Ryzen 9 9950X), lines in a random order read 9.30 loads and stores dispatched a line and 1.018 demand fills a
  line (a count of misses per load would read 8); in order, 9.44 dispatched and 0.037 fills a line, the prefetcher
  having fetched the lines first. The cycles they cost (Linux perf on the same CPU): in a random order 1.002 fills and
  65 cycles stalled on loads a line (87 % of the loop's cycles), in order 0.010 fills and 8.2 stalled cycles a line
  (53 %): the stalls go on where the fills read nothing. The probe prints both for the machine it runs on.
- **Attribution in Luau**: tools/hw-counters/attribution.luau calls a function chasing a random cycle through 64 MB
  of array slots between calls of one doing multiply-adds, first in calls of 100 us and more (every call read around),
  then of 15 to 20 us (one in several). `tools/hw-counters/attribution.ps1`, from an ordinary (not elevated) shell,
  runs it through this checkout's ree (with `--script-timeout 0`) and checks that the chases hold at least 90 % of the
  four functions' stall cycles (setup.ps1 prints the command; it never runs ree, which the account can rebuild, from
  its elevated shell). It runs in Windows PowerShell 5.1 (ree's output goes to a file, never through PowerShell's
  error stream) and removes its profile folder whatever happens.
- **Sampling under frame and thread load**: tools/hw-counters/sampling.luau plants a known number of identical loop
  iterations in 60 simulated seconds at 60 fps. Run it with `--script-timeout 0 --profile hw --profile-counters
  cycles,instructions`. The harness's context fields choose the variant: `Steps` is the iterations in each call,
  `Background` adds that many idle `task.wait` loops, `Recursive` runs four coroutines that each recurse four calls
  deep, and `Bridge` calls through a thin `pcall` wrapper. At the end it prints `MEASURE` with the number of calls and
  the steps in each. The reference function runs a million iterations, so the callee's own instructions should match
  the reference's own instructions times `calls * steps / 1000000`.
- **The install itself**: `powershell -NoProfile -ExecutionPolicy Bypass -File tools\hw-counters\setup-test.ps1`,
  from an ordinary shell, checks what setup.ps1's second step does that needs no elevation, in temporary folders: a
  build with its own hash is staged with the protected ACL, a changed one is refused before anything is written, a
  malformed hash is refused, the folder's ACL comes out as Administrators and SYSTEM full control, Users read and run,
  nothing inherited, with an ordinary account unable to write into it, an owner other than Administrators is caught, a
  folder that is a link elsewhere is refused, scripts on PATH named like the cmdlets setup.ps1 uses run in their place
  in a fresh Windows PowerShell but not after setup.ps1's import, setup.ps1 is ASCII without a BOM (so the hash of its
  text as it runs is its file's), a quote in a printed path is doubled, PSReadLine in the account's module folder is
  found, and the printed step-2 command, for a copy in a folder whose name holds a quote, runs the bytes it checked and
  leaves the shell running, reports failure when the copy changed or cannot be read, and keeps a nonzero status after
  a mocked install exception or missing Cargo (23 checks). The PATH checks require the child shell to exit successfully.
  Measured through the service with the fill counters (before the stall counters were installed there), three runs:
  the chases held 99.2-100.0 % of the four functions' fills from beyond the L3 (`chaseLong`, every call read around,
  315,000-320,000 for its 320,000 steps; `chaseShort`, one call in several, 612,000-615,000 for its 600,000).
- **The booking, measured.** The review's harnesses, through the service, three runs each (cycles, `--script-timeout
  0`): a function recursing four deep, 6,000 calls of `f(3, 700)`, kept 99.6-103.1 % of its own and its caller's
  cycles, the caller -3.1 to +0.4 % (instructions: -1.9 to -0.8 %); a coroutine's body running an 8,000-iteration loop
  between yields, beside the same loop in a function, 600 rounds: the body 49.6-49.8 % of the three functions' cycles,
  the function 49.7-49.8 %, the code resuming it 0.4-0.7 % (1,199 readings where it resumed and yielded); two threads
  the engine resumes each frame, one running a 30,000-iteration loop inline and one calling a function with it: the
  inline loop 48.2-50.1 %, the called one 49.8-51.6 %, the calling thread's own 0.0-0.2 %; a function whose first call
  is short and the next 1,000 long: 99.3-99.7 % its own (its caller 0.3-0.7 %); one whose first ten calls are short:
  92.0-93.8 % (its 57 calls before its first re-measure are its caller's); a tiny function called 200,000 times: none
  of its own, every count its caller's, 22 calls read. Two 600,000-iteration calls of a function, then 20,000 tiny
  ones: the function keeps 100.1-100.2 % of the two long calls' instructions (eight runs; when the run's end decided,
  it kept none). The recursion after a coroutine died by an error inside `f`: the caller -2.4 to +0.4 % of `f` and
  its caller (instructions, five runs) and 6,267-6,562 calls read, as without it (-1.7 to +0.6 %, 6,233-6,452); after
  one closed while suspended inside `f`: -1.7 to +0.1 %, 5,522-6,295 calls read.
- The probe is the client's: a build's own (`cargo build --release --manifest-path tools/hw-counters/Cargo.toml`)
  checks the installed service; a change to the service itself, its counter list included, takes effect when setup.ps1
  runs again.
- Without the service (not installed, counting other counters for another client, the CPU's counters held by another
  tool) the run goes on and the profile says why. `REE_HW_COUNTERS=mock` counts without it (the run thread's cycle
  count at each reading and fixed shares of it: for tests, never a measurement); `REE_HW_COUNTERS=console` reads from
  `ree-hw-counters serve` run in a console (a pipe of its own; it counts only with the profiling right, so from an
  elevated shell). Windows only.

## What-if comparisons

`ree profile try <file.luau>` measures other ways to write the same thing side by side: every variant at every input
size runs in a fresh engine under the profiler, and one folder holds the profiles and an index of what they show
(`compare.json`), which agents and the profile viewer read. tools/what-if/ has examples (nested keys either way, an
array of records or a record of arrays, string or number keys, closures or methods, appending, a table per frame or one
reused, ways to iterate, Vector3 or numbers) and what they show.

- **The variant file** is a Luau module:

  ```lua
  return {
      title = "t[k][v] or t[v][k]",                  -- optional: the file's name otherwise
      about = "two keys, many values each",          -- optional, one line
      sizes = { 10, 100, 1000, 10000 },              -- the n every variant runs at (default { 1 })
      frames = 1,                                    -- calls of run, one a frame (default 1)
      per_frame = 1,                                 -- optional: how many times a frame the game runs this code (default 1)
      setup = function(n) return data end,           -- the input; measured as the setup phase, not the run
      variants = {
          ["t[k][v]"] = function(data) return result end,
          ["t[v][k]"] = { setup = function(n) return layout end, run = function(layout) return result end },
      },
      check = function(a, b, n) return a == b end,   -- optional: the baseline's result against another's
  }
  ```

  A variant is a function, run with the file's `setup` (which then defaults to returning n), or `{ setup = ..., run = ... }`:
  its own setup builds its own layout of the input (which the heap figures then show) and `run` uses it. A dictionary of
  variants runs in name order, a list of `{ name = ..., setup = ..., run = ... }` in its own; the first is the baseline,
  which the others are measured against. `check(a, b, n)` gets the baseline's result and another variant's at the same size
  and returns true, or false and a message; `check = true` compares them for deep equality. Results are checked in an
  engine of their own, never in a profiled run; without a check, by their digests (a hash of the result's contents, keys
  sorted). A variant whose result differs, either way, computes something else: it wins nothing, the summary and the
  viewer say so, and the command exits 1. `per_frame` is how often the game runs the code in a frame: a gap in a ms figure
  is material from 0.1 % of a 60 fps frame at that rate (16.7 us a call at the default 1), and 1 ns a call at least.
- **Versions of a module**: `versions = { before = "old/Handler.luau", after = "../src/Handler.luau" }` (or a list of
  `{ name = ..., file = ... }`; paths from the variant file's folder) makes each version of one module a variant, with
  `setup = function(n, Module)` and `run = function(data, Module)` driving it. With `module =
  "ReplicatedStorage/Shared/Handler"` each version stands in for that ModuleScript of the project before anything requires
  it (`Context.StandIn`), so it requires its siblings as the project's own does; without `module` each version is mounted on
  its own. Project options (`--roots`, `--project`, `--place`, `--mount`, and the other engine options above) pass through
  to every run; without them the runs have no project.
- **What runs.** For each variant and size, one profiled run: `ree run` of a generated harness with `--profile
  work,caches,alloc,heap,timeline,phases`, written to `runs/<variant>/<n>/`, an ordinary profile folder (`ree profile
  report` and `diff` read it). The harness
  marks three phases, each with a heap snapshot at its mark: `setup` (the variant's setup, in a frame of its own), `run`
  (`frames` calls of run, one a frame, from the second frame after setup's, so that what it touches of setup's data is a
  first touch in its frame) and `after` (the result still held). Every mark carries the run's counts so far (profile.json
  `marks`), so a phase's figures are the difference of two marks'. Host time comes from runs of its own whose only
  profile is the marks' clock (`--profile cpu`): `--rounds` rounds (default 9), each running every variant and size once in
  a fresh process, in a new order each round, with the baseline run twice (an A/A pair: how far two runs of the same code
  differ). Such a run calls run for about `--time-ms` (default 10) ms of warm work, at least three times, on inputs it
  prepares in groups of about 1 MB (what a CPU's L2 cache holds); each group's calls run back to back and only they are
  timed, by the run thread's cycles and the host's clock. A pair still undecided after those rounds (its gap looks larger
  than the noise, or it is a round or two short of the sign test below) gets 6 rounds more at a time, of its two variants
  and its size's A/A pair, until it is decided or `--max-rounds` (default three times `--rounds`) is reached. With the
  hardware counter service installed, three more rounds of such runs read `--profile hw` alone (a new order each round;
  the first round's profiles in `runs/<variant>/<n>/hw/`), and each counter's figure is the median of its rounds. One more
  engine runs every variant at every size for the check. The profiled runs take `--jobs` (default 4) processes at a time;
  the timed ones run one at a time.
- **Options**: `--out <dir>` (default `<state dir>/what-if/<file name>` for a standalone binary, `what-if/<file name>` here
  for a host or checkout build; a folder holding anything a comparison does not write
  is refused), `--sizes a,b`, `--variants a,b` (some of them, the first the baseline), `--frames <n>`, `--rounds <n>`,
  `--max-rounds <n>` (`--rounds` adds none), `--time-ms <ms>`, `--jobs <n>`, `--no-time`, `--no-hw`, `--no-check`
  (neither the check nor the digests), `--keep-timed` (keep the timed runs' profile folders in `timed/`),
  `--script-timeout <s>`, and engine options. Exit 0 when every run finished and every result held, 1 otherwise
  (compare.json is written either way), 2 for a command line it cannot read.
- **Which numbers are exact.** The work clock (with and without its cold and first-use charges), instructions, allocations
  (bytes and count), the reachable heap, the collector's steps and bytes freed (it paces itself by the scripts' work),
  table shapes, rehashes, cold first touches and inline cache counts are the same in every run of the same code: one
  profiled run is enough, and any difference between variants is theirs. Host time and hardware counters are measured.
  Host time's figure is each variant's fastest round, since whatever else runs on the machine only ever slows a round. Its
  verdicts pair the variants' runs round by round (each round runs every arm once, in an order of its own, so a round's
  two runs met the same machine): first, a run over 1.15x what its arm's level times its round's lead one to expect is
  left out (the two levels fitted by median polish on the runs' logarithms: each arm's median of what is left, then each
  round's, in turn; the machine slowed such a run alone, while a stretch where it slowed every run raises its rounds'
  level and stays, as a pair's ratio cancels it). A
  variant is then clearly slower than another when it was slower in enough rounds for a sign test at 2 % (8 of 9; 0.5 %
  once rounds were added for the pair, as a pair looked at again had more chances to cross by luck), the 95 % interval
  of its shift (the Hodges-Lehmann estimate of the rounds' log ratios, from the exact signed-rank distribution) leaves
  out none, the shift is past both 2 % and the noise two runs of the same code show at that many rounds (the 99th
  percentile of the shift over 2,000 draws of the A/A pairs' rounds, every size's pooled; at a size where the A/A pair
  itself parted beyond the noise, past the far end of its interval too), and its fastest run is slower too, by a third of
  the shift at least and 2 % (whatever slows the machine moves a variant's typical runs, not its fastest: a gap in the code
  moves both). The summary gives the ratio with its interval and the rounds won. Measured on one shared desktop under
  heavy load from other work (8-10 other agents' builds and sweeps), at the default 9 rounds (adding 6 at a time, up to
  27, for an undecided pair) with the A/A noise at 9 rounds 3.5-21.5 %: three identical variants were called apart in
  none of 240 host-time verdicts (10 comparisons), and planted gaps of 3 %, 10 % and 30 % (a loop run 1.03, 1.10 and 1.30
  times as often, at n = 10,000 and 30,000) were found in 10, 19 and 20 of 20 comparisons, never the wrong way, their
  ratios read 1.026-1.039, 1.093-1.128 and 1.264-1.376 (the 10 % gap missed was in a comparison where two runs of the
  same code parted by up to 11.6 %). A review's batch on the same machine at another time found the same gaps in 2, 13
  and 20 of 20, also never the wrong way: so 2-10, 13-19 and 20 of 20 over the two. How much a machine's load moves
  runs decides the smallest gap found; a quieter machine finds smaller gaps. On a counter (the median of its rounds: a core's own cycles, which such
  slowing hardly moves), only where it won every round, by 5 % or more. The collector's host time comes from the profiled
  run (its profilers slow it) and is a guide, not a measure.
- **The summary** (`compare.txt`, also printed): the check (and how: the file's function, deep equality or digests); a
  table of each variant at each n by the main figures; the setup's cost; how each figure grows with n: the exponent at
  large n (a fit to a + b * n^k with a fixed cost a call apart, read as the growing part's slope between the two largest
  sizes: a constant overhead no longer bends it) with each segment's own slope; at the largest n the best variant on each
  figure where it is clear of every other (else "no clear winner among" it and those no clear gap parts from it) with
  every other variant's figure over it (host time with its interval and the rounds it won), the figures where
  they are about equal, within noise or too small to matter, and the floor that made them so;
  the tables each variant's run left, by the line that made them (count, array and hash parts, levels, bytes each, the
  rehashes that grew them); then notes (where the work clock's model bears on the comparison: cold and first-use charges
  that are a large share of the work, a gap host time sees and the work clock does not; the collector; failed runs), what is exact and
  what measured, and the fidelity line.
- **Files**: `compare.json` (below), `compare.txt`, `runs/<variant>/<n>/`, `diff/<variant>/` (`ree profile diff` of the
  baseline's profile and the variant's at the largest n) and `harness/` (the generated harness and each run's context).

### compare.json

```jsonc
{
  "format": "ree-what-if", "version": 2,
  "title": "t[k][v] or t[v][k]", "about": "...", "file": "<the variant file>", "out": "<this folder>",
  "command": "<the command line>",
  "baseline": "t[k][v]",
  "variants": [{"name": "t[k][v]", "slug": "t-k-v", "kind": "function", "source": "ReplicatedStorage.WhatIf.tkv:12", "file": null}],
  "sizes": [10, 100, 1000, 10000],
  "frames": 1,
  "per_frame": 1,
  "module": null,
  "fidelity": "fidelity: ...",
  "fidelity_profile": null,
  "floor": {"ms": 0.0167, "per_frame": 1, "frame_ms": 16.67, "frame_share": 0.001, "ns": 1, "host": 0.02},
  "metrics": [{"key": "work_ms", "label": "work clock", "unit": "ms", "better": "lower", "exact": true, "per_call": true, "about": "..."}],
  "runs": [{
    "variant": "t[k][v]", "size": 10, "status": "ok", "error": null,
    "profile": "runs/t-k-v/10", "hw_profile": null,
    "digest": "9f3c0a21", "check": {"ok": true, "message": null},
    "setup": {"work_ms": 0.004, "insns": 120, "host_ms": null, "...": 0},
    "run": {"work_ms": 0.002, "warm_ms": 0.0011, "host_ms": 0.0011, "...": 0},
    "host": {"ms": [0.0011, 0.0012], "cycles": [5200, 5310], "rounds": [0, 2], "reps": 2500, "group": 133},
    "hw": {"hw_instructions": [4100, 4102, 4100], "...": [], "rounds": [0, 1, 2]},
    "heap": {"before": 1052672, "after_setup": 1060864, "after_run": 1061888},
    "tables": {"count": 3, "bytes": 1312, "setup": {"count": 1, "bytes": 560}, "run": {"count": 2, "bytes": 752},
               "sizes": {"table": 48, "slot": 16, "node": 32}, "sites": [{"...": "a site, below"}]},
    "hot": [{"key": "...", "name": "build", "module": "...", "line": 14, "work_ms": 0.0018, "insns": 98, "calls": 1}]
  }],
  "scaling": [{"variant": "t[k][v]", "metric": "work_ms", "exponent": 1.0, "last": 0.97,
               "fit": {"k": 1.0, "fixed": 0.0011, "coefficient": 3e-7, "error": 0.004, "fitted": true},
               "segments": [[10, 100, 0.42], [100, 1000, 0.85], [1000, 10000, 0.97]],
               "points": [[10, 0.002], [100, 0.019]]}],
  "functions": [{"variant": "t[k][v]", "key": "...", "name": "build", "module": "...", "line": 14,
                 "exponent": 1.98, "fit": {"...": 0}, "segments": [], "last": 1.97, "points": [[10, 0.0004], [100, 0.04]]}],
  "winners": [{"metric": "host_ms", "size": 10000, "winner": "t[k][v]", "value": 3.3, "runner_up": "t[v][k]",
               "ratio": 1.77, "interval": [1.71, 1.82], "vs_baseline": null, "clear": true, "material": true,
               "rounds": {"won": 9, "of": 9, "p": 0.002, "alpha": 0.02, "noise": 0.031, "added": false, "aa_bar": null, "faster": false, "lost": 0},
               "ranking": [{"variant": "t[k][v]", "value": 3.3, "ratio": 1.0, "value_ratio": 1.0, "clear": false, "material": null, "differs": false, "rounds": null},
                           {"variant": "t[v][k]", "value": 5.8, "ratio": 1.77, "interval": [1.71, 1.82], "value_ratio": 1.76, "clear": true, "material": true,
                            "gap": 2.5, "differs": false, "rounds": {"won": 9, "of": 9, "...": 0}}],
               "baseline": [{"variant": "t[v][k]", "ratio": 1.77, "interval": [1.71, 1.82], "clear": true, "material": true, "differs": false, "rounds": {"...": 0}}]}],
  "aa": [{"size": 10, "n": 9, "wins": 4, "shift": 1.004, "interval": [0.98, 1.03], "separated": false, "noise": 0.031,
          "ratio": 1.02, "min_ratio": 1.01, "spread": 0.03, "first_ms": [0.0011, 0.0012], "second_ms": [0.0012, 0.0011]}],
  "noise": {"pool": 36, "quantile": 0.99, "slowed": 1.15, "floor": 0.02, "at_rounds": 9, "level": 0.031, "alpha": 0.02, "later_alpha": 0.005},
  "host": {"rounds": 9, "max_rounds": 27, "ran": 15, "time_ms": 10, "reps": [[10, 2500]], "groups": [[10, 133]],
           "added": [{"after": 9, "rounds": 6, "pairs": [{"size": 10000, "a": "t[k][v]", "b": "t[v][k]"}]}]},
  "hw": {"read": true, "why": null, "rounds": 3},
  "check": {"ran": true, "by": "function", "ok": true, "failures": []},
  "diffs": [{"variant": "t[v][k]", "size": 10000, "dir": "diff/t-v-k"}],
  "notes": ["..."],
  "seconds": 18.4
}
```

- `variants[].slug` is the variant's folder name; `kind` is `function` or `module` (a version); `source` is where it is
  defined (a function: its chunk and line; a version: the file as the variant file names it) and `file` a version's
  file. `metrics` lists every figure a phase can carry, in the order a table shows them: `label`, `unit` (`ms`, `count`,
  `bytes`, `ratio`), `better` (`lower`, `higher`, or `none` for a figure that names no winner), whether it is `exact`
  (above) and whether a run's figure is `per_call`.
- `runs[]`: one per variant and size. `status` is `ok`, `error` (the variant raised: `error` says in which phase and
  where) or `failed` (the run did not finish). `profile` and `hw_profile` are folders relative to this one. `digest` is a
  hash of the result's contents (keys sorted), equal for equal results; `check` is null when nothing was checked. `setup`
  and `run` hold the figures of their phase by metric key, null where not measured; run's are per call (the phase's
  totals divided by `frames`) where the metric says `per_call`:
  - `work_ms` the work clock (ms of a client's time, docs/engine.md "Work clock") and `warm_ms` the same without any
    cold or first-use charge (code run for the first time in the frame, and first touches of tables, functions and engine
    objects: what a call costs once a loop has run it; a client's figure lies between the two); `insns` instructions; `alloc_bytes` and `allocs` what the VM allocated (bytes of
    objects, table parts and stacks; collectable objects); `heap_bytes` the reachable heap at the phase's end beyond its
    start (what the phase still holds); `gc_steps`, `gc_freed` (bytes) and `gc_ms` the collector's work (the work clock charges it in `work_ms`; `gc_ms`
    is its host time); `cache_hits`,
    `cache_lookups` (those a table serves) and `cache_hit_rate` Luau's inline caches, and `cache_uncharged_ms` what their
    outcomes the work clock does not charge cost over hits at the committed prices (a reprediction, an absent key, an insert
    with room, a metatable walk by its depth), beyond `work_ms`, which holds what the clock charges (a rehash, a looked-up
    import); prices within the noise are left out; `cold_tables` the tables
    the frame touched first that were made before it (the whole phase's, the harness's included) and `cold_ms` every cold
    and first-use charge of the variant's code, its tables' first touches among them; `rehashes` and `rehash_keys` the
    rehashes a script's own instructions made tables do to grow, and the keys they moved; `host_ms` (the fastest round),
    `host_ms_median` and `host_cycles` (the fastest round, the run thread's reference cycles) from the timed runs; `hw_instructions`, `hw_cycles`,
    `hw_ipc`, `hw_load_stall_cycles`, `hw_load_stall_share`, `hw_miss_latency`, `hw_miss_buffers`, `hw_demand_fills`,
    `hw_demand_fills_beyond_l2`, `hw_demand_fills_beyond_l3` and `hw_branch_misses` from the hw run (each from the
    counters it read).
  - `host`: the timed runs' figures a call, one per round whose run finished (`rounds` names those rounds: a failed run
    leaves its round out), the calls a timed run made (`reps`) and how many it prepared and ran at a time (`group`).
    `hw`: each counter's figure a call, one per round of the counters' runs that finished, and `rounds` (empty when they
    were not read).
  - `heap`: the reachable heap (bytes) at the setup mark, the run mark and the after mark.
  - `tables`: the tables setup and run allocated that are still reachable after the run (the result held), by
    allocation site, largest first, with the totals of each phase's and the bytes of a table, an array slot and a hash
    node. A site: `{"site": "tkv:14 < tkv:30", "frames": [{"module": "ReplicatedStorage.WhatIf.tkv", "name": "build",
    "line": 14, "path": "<file>"}], "phase": "run", "count": 10000, "bytes": 1760000, "header_bytes": 480000,
    "array_bytes": 0, "hash_bytes": 1280000, "array": {"slots": 0, "used": 0, "min": 0, "median": 0, "max": 0, "hist": [[0,
    10000]]}, "hash": {"slots": 40000, "used": 40000, "min": 4, "median": 4, "max": 4, "hist": [[4, 10000]]}, "levels":
    {"min": 1, "median": 1, "max": 1, "hist": [[1, 10000]]}, "rehashes": 30000, "rehash_keys": 60000}`. `frames` are the
    site's Luau frames, innermost first, without the harness's; `bytes` is each table with its array and hash parts as the
    VM counts them (gcinfo); a part's `slots` is its size summed over the site's tables and `used` the slots holding a
    value; `hist` is `[[upper, tables]]` over power-of-two buckets (a part of 3 slots counts under 4); `levels` is how many
    levels of tables a table is with those it holds (1: it holds no table; tables in a cycle share a level; a metatable is
    not held); `rehashes` and `rehash_keys` count the rehashes of the site's tables over the run (any line's) and the keys
    they moved.
  - `hot`: the functions with the most work in the run phase (its part of the call tree), the harness's left out.
- `scaling`: per variant and metric, how the figure grows with n (sizes with a figure above zero): `exponent` the
  exponent at large n, `fit` the fit of the figure to `a + b * n^k` by least squares on the relative error (`fixed` a,
  the cost a call that does not grow, `coefficient` b, `k`, `error` the largest relative miss at a size; with two sizes
  no fixed cost is fitted and `fitted` is false), `segments` each pair of neighbouring sizes with the figure's own log-log
  slope between them, `last` the raw slope between the two largest, and the points. `exponent` is the growing part's
  slope (the figure less the fixed cost) between the two largest sizes: a linear walk with a fixed cost reads 1.00 where
  a log-log fit without one read 0.68. The viewer's scaling column and its issues read it. `functions`: the same for the
  hot functions' work, matched across sizes by their key. (Version 1 had `exponent` from a log-log fit without a fixed
  cost and its `r2`.)
- `winners`: per metric with a `better` and size, the variant with the best figure among those whose result holds (tied
  leaders lead in the variants' order, the next at ratio 1, so a tie names no winner and every other variant keeps its
  verdict; host time's leader is the one its paired rounds put ahead, so the order agrees with the paired ratios), the
  runner-up, `ratio` and `interval` the runner-up's entry's, `clear` whether the best is clear of every other variant
  whose result holds (the summary names no winner otherwise: "no clear winner among" the best and those within reach of
  it), and `vs_baseline`; `ranking` every variant from the
  best (those whose result differs last, `differs` true, with no verdict), each with its figure over the best's
  (`ratio`: for host time the paired shift's, with its 95 % `interval`; `value_ratio` the figures' own ratio) and whether
  that gap is `clear`: an exact figure by 1 % or more, host time by the rule above (`rounds`: won of paired, the sign
  test's `p` and the level it had to clear, the A/A `noise` at that many rounds, whether rounds were `added` at the size,
  `aa_bar` the gap the size's A/A pair parted by when it did), a hardware counter when it won every round by 5 % or more
  (`rounds` too), another measured figure by 10 %. `material` is false for a gap too small to matter, judged by what a
  frame pays as a ms figure is: in a ms figure under `floor.ms` a call (0.1 % of a 60 fps frame over `per_frame`, 1 ns
  at least); in host cycles and the counters' CPU cycles likewise, as the time they take at the clock they ran at (host
  cycles at `floor.cycles_per_ms`, measured from the timed rounds; core cycles at the counter runs' own clock); in a
  count a call, times `per_frame`, under a thousandth of a call's instructions or 10; in bytes so, under 1 KB (the heap
  a phase holds, no figure a call, as it is); a gap that is not material is never clear. `baseline`: each other variant against the baseline, either way (its `ratio` above 1 is worse), with the same
  fields: the viewer colours a verdict only where it is clear. `aa`: per size, the A/A pair's paired shift (the second
  run over the first) with its interval and rounds, whether it `separated` (parted beyond the noise: a verdict there then
  has to pass its interval's far end), the noise level at its rounds, the median ratio round by round, `min_ratio` the
  ratio of their fastest rounds, `spread` the relative interquartile range of the round-by-round ratios, and both runs'
  figures in the rounds where both finished. `noise`: the pool of A/A log ratios the verdicts read and its level at the
  comparison's rounds. `host`: the timing's settings, the rounds run and those added, and the calls per size.
  `check.ok` is there only when something was checked (`ran`), and `check.by` says how (`function`, `equality`,
  `digest`); fields with no value are null, but `about`, a frame's `path` and the samples' lists hold no nulls.
- `notes`: what to keep in mind reading this comparison: where the work clock's model bears on it (a large share of
  cold first touches, a gap host time sees and the clock does not), rounds the machine slowed (twice a variant's fastest
  or more), the runs left out as slowed, the rounds added for undecided pairs, an A/A pair that parted, the collector's
  work, failed runs and checks, why the hardware counters were not read. `fidelity_profile`: the baseline's profile's
  `fidelity` object at the largest size (how far its figures may lie from a client's), null when the profile has none.

## The profile viewer

`ree profile view <dir> [<dir2>]` opens a profile in the terminal: one profile, a before and after pair, or a what-if
folder (`compare.json`). `ree profile render ...` draws the same screens headless to a PNG, an SVG or text. Both only
read profile folders. They run `ree-profile-view`, a binary of its own beside `ree` that `cargo build --release` builds
with it (profile-view/, on ratatui: the engine's crate has no terminal UI). Each view is panels with rounded borders,
tables with their header raised and every other row shaded, charts and gauges; the panel that takes the keys has its
border in the accent.

<img src="media/profile-view-source.webp" width="100%" alt="The Source view of tools/profile-demo/leaderboard.luau: each line's work, instructions and allocations beside the code on a heat scale, the sort's comparator the hottest line, and the selected line's opcodes below">

- **What it opens**: a profile folder (it holds profile.json), the folder of a run made with `--profile` (profile/ in
  it), or a what-if folder. Two folders are a pair, before and after, and open on Diff. A what-if folder opens on
  Compare, with the baseline's run at the largest size open in the other views.
- **Views**, a key each:
  - `1` Overview: the run's totals (the mount's work apart from a frame's), the work clock's fidelity line, the run
    frame by frame, the issues and the hottest functions in the phases the profile reports (its `scope`: as the
    summary's tables, `--profile-phase`, by default steady when the run marked one, else every phase). The game's code
    leads, and the harness's own code (under ReplicatedStorage.RURFixtures, and the engine's `rur.record` that calls it,
    with the C functions, the engine's Luau and mlua's helpers they called) comes in a panel of its own, with a card for
    the two sides' work; a run whose code is all the harness's (under 1% on the game's side: code measured by
    `--harness` alone) is shown whole. A line under the fidelity line counts what those phases' scripts asked the
    engine to do (instances cloned, made, destroyed and taken into the Workspace, property writes and the moves among
    them, pivots, bulk moves, emits, queries: profile.json's marks, from ree 64fb6e6), every script's, the harness's
    too: the engine's side of a cast, which the work clock reads lower than a client pays.
  - `2` Flame: the call tree as an icicle, the run at the top; each node as wide as its total and coloured by its own
    share. It opens on the run's heaviest phase; down follows the heaviest callee, enter zooms into a node, esc comes
    back out.
  - `3` Calls: a function between its callers and its callees, what it cost under each, and its hottest lines.
  - `4` Source: a module's lines with their heat, instructions, allocations and Luau cache misses, and the selected
    line's opcodes.
  - `5` Allocations: the sites (the Luau line that allocated), their bytes, objects, object size, bytes a frame in the
    steady state and types, and each site's stacks.
  - `6` Heap: the reachable heap at each snapshot, the biggest holders and what they keep alive, the heap by type, the
    run's own objects still reachable by allocating stack, and the collector.
  - `7` Timeline: a column a frame, by phase; pick a frame to see each of its figures beside its phase's median frame,
    and each figure frame by frame.
  - `8` Caches: Luau's inline caches by kind and by call site, and ree's own.
  - `9` Issues: what the profile's numbers show to fix (below).
  - `d` Diff, of a pair: the headline figures, then the functions, lines and allocation sites that moved. When both
    profiles report the same phases, or both marked steady, it leads with the game's code and the harness's in them,
    and the functions' work covers those phases, the harness's functions last and greyed, as `ree profile diff`. The
    engine's counts in the same phases follow (instances made and destroyed, property writes, parts moved, pivots,
    emits), in ree profile diff's groups. A measured figure (host time, a CPU counter) is marked `~` in no colour: one
    run each gives its change no verdict (and host time, the run's and a frame's, is left out when a profile stepped
    every instruction, which slows the interpreter several times over). Functions pair as `ree profile diff` pairs them, an anonymous one
    across a move; their table shows calls and instructions before and after (`not counted` where a profile did not step
    every instruction, as a `time` profile does not) and a path: `moved`, `gone` and `new` for a function one profile
    did not run (not one host time's samples missed), or `price only` for one that ran the same calls, instructions and
    opcodes and whose cold and first-use charges account for its change, as `ree profile diff` tags it (a profile
    without that split tags none, nor does scripts only, which folds C functions into their callers and says so),
    marked ≈ with its bar grey. A row moved by `ree profile diff`'s rule. The tables of a measured metric (host time, a
    CPU counter) mark each change `~` in no colour, as the headline does.
  - `c` Compare, of a what-if folder: each variant at each size in the comparison's metric, how it grows with n (a
    log-log plot, a line a variant) and each variant over the baseline by n (on a log scale: a ratio of 2 and one of 0.5
    lie as far from 1), the best of each size, each variant's figure at the selected size as bars, every figure of a
    variant over the baseline's (worse to the right, on a log scale; a figure that names no winner, as `better` "none",
    as it is, in no colour), and the variant's own table shapes (its run's, the setup's input apart). A winner that is
    clear and material is marked ◆, one within noise or too small to matter ≈, and a variant whose result differs from
    the baseline's ✗ (it wins nothing; the header names it, and the issues list it). Against the baseline, ▲ worse and
    ▼ better in colour only where compare.json's verdict calls the gap clear and material; any other gap shows its
    ratio in no colour with why it names nothing (noise, too small, about equal). The scaling column is the exponent at
    large n. Enter opens the run in the other views, `D` diffs it against the baseline's.
- **Keys**: arrows or hjkl move, enter goes in, esc or backspace comes back out; `m` and `M` the metric (Studio work,
  host time, instructions, allocated bytes, allocations, Luau cache misses, each hardware counter the profile read);
  `s` sort; `f` scripts only (C functions and the engine's own Luau fold into the script that called them); `/` search,
  `n` and `N` the next and previous match; `y` copy the selected name or path (through the terminal); `x` the other
  side of a pair; `t` light or dark; `?` help; `q` quit.
- **Colour**: one heat scale for a figure's share of the run, through several hues as inferno's (purple at 0.1 %,
  crimson, orange, gold at 30 % and more on the dark theme; the same turned round on the light one, so more is always
  further from the page; its lightness steps evenly, so the order reads in grey and under colour blindness), grey
  under 0.1 %, its legend along the bottom of the panels that use it; red ▲ and blue ▼ for more and less than before;
  a colour a variant or phase, in the data-viz palette's fixed order; an issue's severity as an icon and a word.
  24-bit colour where the terminal shows it, else each colour's nearest of the 256 every terminal has
  (`REE_VIEW_COLOR=truecolor|256` decides). Screenshots are always 24-bit.
- **Issues** come from measured patterns only. Each issue shows its numbers, its price, advice, and a link (enter) to the
  view and line that show it.

  *Price.* Every issue names a set of call-tree nodes or charged sites. Its standalone price is what removing the
  advised work saves, minus the replacement's cost, divided by the reported phases' frames. It is always a mean rate,
  including an event seen once: "19 ms once in this run: 31.7 us a frame over 600 frames". Spreading work between
  frames saves no work. The words steady, recurring and once say how often a function worked. They describe an
  issue and never change its price.

  *Precision.* A price is exact, an estimate or a bound, and the issue says which.
  - **Exact:** the work the clock charged at identified sites, less a known replacement's cost. Only exact prices are
    clock savings.
  - **Estimate:** work the clock did not charge (`extra_work_us`), shown with the calibration's low and high spread. An
    estimate never enters the charged totals or the strongest-saving claim.
  - **At most:** when the setup instructions of a source rewrite are unknown, the saving is somewhere between zero and
    the price, which is then an upper bound.
  - **Part:** a removal's recorded subtotal (a sort, a repeat read, an unchanged write) leaves out instructions the edit
    also changes. Every advised source edit uses the whole scoped code work as the complete edit's upper bound by
    default, including the caller's instructions and argument evaluation in other functions.

  None of these claims to be the complete saving of a source edit.

  *Floor and the closing row.* A fix is listed when its mean saving reaches the absolute floor, 41.7 us a frame
  (0.25% of a 60 fps frame), or its own largest frame reaches 2.5% of the recorded frame budget. The cutoff only
  decides what is shown. One closing row holds every millisecond outside the listed fixes' union.

  *Overlap.* Each issue keeps its standalone saving. Totals intersect the actual node and site sets, so a declined
  builtin inside a comparator overlaps removing the sort, while the comparator's work outside the sort stays separate.
  `counted_ms` is the additional union work each row covers. Every function whose work the fix saves is named. A frame
  hitch names its contributors and shares, and books no work itself.

  *Severity.* An issue is as severe as the worse of its own mean price and its own work in frames over budget. The budget
  is `1000 / fps` ms from the profile. A mean price of a tenth of the budget is critical, a fortieth serious, a
  two-hundredth warning. A hitch contribution over budget is warning, over by a quarter of the budget serious, over
  by a whole budget critical. Another function's hitch cannot promote a small contributor. Rank by severity, then mean
  price. "Nothing elsewhere can save as much" belongs only to the largest exact saving across all candidates, including
  those below the display floor. Another issue's upper bound counts against it, and an at-most price never gets it.

  *Caches and builtins.* A cache fix keeps paying `first_touch_us`. Its charged saving is `outcome_charged_us`, less
  `presized_us` for rehash outcomes. Presizing advice retains the writes; the price removes the rehash work.
  Profiles without that breakdown say **not priced: needs a newer profile**.
  `extra_work_us` is uncharged work beyond the measured clock; `luau_caches.prices.kinds` identifies charged outcomes.
  Each calibration outcome also carries `low_ns` and `high_ns`. They are the spread of the estimate, not clock charges.
  Outcomes within noise are shown and never summed.

  A builtin fix saves its site's `decline_charged_us`, less the fast path's cost for those same calls
  (`fast_equivalent_us`) and less the added instructions of the advised rewrite. For `math.abs` given a numeric string,
  pass an existing numeric value directly: that price includes no added conversion.

  For `string.sub`, `string.byte`, `table.insert` and `table.unpack` the rewrite adds code, and its cost is counted
  from the opcodes alone. `fix_instructions_us` is a nominal warm cost; `fix_cold_us` is unknown without the replacement's
  executed instructions. Only two rewrites keep a site-only bound, because they keep every original operand:
  `string.byte(s)` to
  `string.byte(s, 1)` and `string.sub(s, i)` to `string.sub(s, i, #s)`, each with no other advised reason to fix. Every
  other builtin rewrite and every cache edit states its site part separately and bounds the complete edit by the whole
  scoped code work.

  An early `unsafe_env` rejection hides the checks after it. The advice is therefore the combination of a safe
  environment and the types, values, argument count and result count the fast path accepts. Removing the environment
  access alone is not priced as reaching the fast path. The cache lookups in a declined call's fallback are the same
  work as the decline that contains them, so both issues name that work in the same way and the totals count it once.

  A source edit's added cold work is unknown without the replacement's executed instructions in each frame, so
  `fix_cold_us` is null for the argument rewrites above. Added code words and prototype activity do not establish that
  cost: fallback words can stay dead, and a builtin can skip frames its function runs in. The operand-preserving
  rewrites still give site-only upper bounds by subtracting none of the unknown setup, including cold setup. When the
  site part of an edit with a whole-code bound is zero or negative, the issue says the recorded site part saves nothing
  on this clock and keeps the bound, since the complete edit may still
  save work outside the site. Only the two operand-preserving rewrites can say the complete fix saves nothing on this
  clock.
  Sites keep their exact context amounts under `nodes`. Without the replacement's cost or the exact call context, an
  issue is not priced. A sort's whole subtree is priced only with the advice to remove those calls when ordering is
  unnecessary. Older profiles still open and say what pricing evidence they lack.

  `timeline.issue_stats` holds each issue's active frames, its peak and its largest own work in an over-budget frame,
  written from every frame. Listing and severity therefore do not depend on which frames the timeline kept. Priced site
  tables are not capped.

  *What is checked.* Functions and their top lines, sorts every frame (a comparator's runs included; a sort with no
  comparator needs the C call rows), cache misses (absent and inserted outcomes too), builtin declines, a property read
  again in a frame and writes that change nothing (priced at what keeping the value saves: the first read of a frame
  keeps its cold charge, and repeats in the same call and in later calls are both priced), allocation and table growth,
  collector spikes, a heap that keeps growing, connections, instances or threads that keep growing, and, in a
  comparison, cost that grows faster than n at large n (with a fixed cost a call apart) and a variant whose result
  differs (by its check, or by digest without one). An issue the profile cannot price says
  why (**not priced: ...**) and ranks after the priced ones; its severity is its own check's measure (bytes, counts,
  steps). A check that cannot run on the profile (a kind missing, too few frames) is listed as not checked, with why.
  The issues cover the game's code: what the harness's own code does is left out unless the run's code is all the
  harness's. A price in ns (`work.prices`, boundary.json's costs) is a desktop one: the viewer multiplies it by the
  run's `work.units.scale`, as the profile's own ms already are. A run of `ree profile try` is not read frame by frame:
  its variant runs once, in one frame.
- `ree profile render <dir> [<dir2>] --out <file|dir|-> [--view <name>|all|<a,b>] [--metric <m>] [--select
  <function|file:line>] [--keys "<keys>"] [--size <cols>x<rows>] [--theme dark|light] [--scale <n>] [--format
  png|svg|txt]` draws exactly what the terminal shows at that size (default 160x48), the same on every machine:
  `--out x.png` (the cells rasterized with the bundled JetBrains Mono, 9x20 pixels a cell at `--scale 1`), `.svg` or
  `.txt`. A folder takes a file a view (`--view all`: every view the input has, `<folder>/<view>.png`), and `-` prints
  text. `--select` picks a function by its name, or a line as `Module:line` or `file.luau:line`, before anything is
  drawn; `--keys` then plays keys as typed (`"2 down enter"`: the Flame, down its heaviest callee, zoomed in; named
  keys enter, esc, backspace, up, down, left, right, pgup, pgdn, home, end, tab, backtab, space; `/text` searches). It
  prints each file it wrote, a PNG with its Markdown image line.
- **For agents**: `ree profile render <dir> --view issues --out -` is the quickest read of what to fix; `--view all
  --out <folder>` gives a screenshot of every view in one command, and the printed Markdown line shows a PNG to the
  user. To show one finding, `--select` its function or line and render the view its issue links to (Calls for a
  function, Source for a line, Allocations for a site, Timeline for a frame).

## Environment

Environment (the variables keep the names they had in rur, whose tools set them; `REE_PROFILE`, `REE_PROFILE_OUT` and
`REE_PROFILE_COUNTERS` are the profile's, above, and `REE_HW_COUNTERS` its counters' source; the guards' `REE_SCRIPT_TIMEOUT`,
`REE_MAX_SCRIPT_TIMEOUTS`, `REE_MAX_WALL_SECONDS` and `REE_PARENT_WATCH` are above; docs/engine.md "Environment"
lists them all): `RUR_STATE_DIR`
(caches; default the checkout's `.cache`, or the standalone per-user `ree` cache), `RUR_VERBOSE=1` (script output on stderr), `RUR_ENGINE_TIMINGS=1` (mount
phases), `RUR_ENGINE_DUMP=<file>` (every instance after the mount), `RUR_EPOCH` (the wall clock's start),
`RUR_CLOCK` (a recorded Studio clock), `RUR_CAMERA`, `RUR_PLACE_ID`, `RUR_PLAYER`, `RUR_PLAYER_ID`,
`RUR_PLAYER_DISPLAY`, `RUR_ROTATION_CLIPPING`, `RUR_PROFILE_CALLBACKS`, `RUR_LUAU_PROFILE=<file>` (the
Luau VM's sampled stacks, folded; `RUR_LUAU_PROFILE_CLOCK=work` samples the work clock's charges
instead of host time, `RUR_LUAU_PROFILE_UNIT=us` writes microseconds), `RUR_LUAU_ALLOC_PROFILE=<file>` (bytes the VM allocates by Luau stack,
folded, with the allocation counts in `<file>.counts`; a native function's allocations end the stack in it;
`RUR_LUAU_ALLOC_STEP=<bytes>` sets the sampling step, default 1024, 1 records every allocation; a harness
names the phase of both profiles with `Context.ProfilePhase(Name)`; see src/luau_profile.rs),
`REE_LOCALIZED_PLACES` / `REE_MATCH_PLACES` (places whose clients pay a localization lookup of their own on Text
writes, or more for part geometry writes: docs/engine.md "Work clock").

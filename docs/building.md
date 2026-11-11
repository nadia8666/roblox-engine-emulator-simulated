# Building

How to build ree, the cargo features and the allocator. The [README](../README.md) has the short version.

## Release binaries

[Releases](https://github.com/Anime-Vanguards/roblox-engine-emulator/releases) provide a flat zip per platform, named
`ree-<version>-<os>-<arch>.zip`. Extract `ree.exe` on Windows or `ree` elsewhere and put it on PATH.

### Platforms

| Platform | Built on |
| --- | --- |
| Windows x86_64 | `windows-latest` |
| Linux x86_64 | `ubuntu-22.04`, the oldest runner GitHub offers, for a low glibc floor |
| macOS arm64 | `macos-15` |

The maintainers develop and test on Windows. The release workflow builds each platform on its runner and smoke-tests
the archive on a fresh runner of the same platform: `--version`, an offline harness, a recording and the service,
with the caches redirected and a check of where files were written. A release is published only when every platform
builds and passes its smoke test.

### What a release needs

The Luau VM, the API data and the stand-ins are compiled in. Offline harnesses and recordings need no Rust, C++
compiler, Roblox Studio, GPU or network. Drawing a recording is ror's job and has its own requirements. Asset
downloads keep their network requirements, and manifest bundles need `REE_ASSET_WORKER` set to an executable that
implements the protocol in [engine.md](engine.md#asset-bundles-srcprojectmodel_assetsrs-feature-bundles). The Windows
hardware-counter service is separate and not in the archive; profiling works without it.

### Which commands work

`run`, `record`, `serve`, profiling during a run, `profile report`, `profile diff` and `profile try` work with an
installed binary and the inputs you name. The terminal profile viewer and `profile render` need the separate
`ree-profile-view` binary from a source build of the workspace. An installed ree says so and does not look beside
itself for a viewer, because the archive holds exactly one executable (the only file toolchain managers install).
Examples and tools in the repository need a checkout.

### Where state goes

An installed ree reads no implicit checkout data, `.env`, snapshots or place content from the current folder. A worker
named by `REE_ASSET_WORKER` is looked up as
[engine.md](engine.md#asset-bundles-srcprojectmodel_assetsrs-feature-bundles) describes; on Windows a bare name is also
looked for beside the client's executable. The project is `--project`, else the folder holding the first `--roots`
entry, else the client's current folder. Caches go to:

- Windows: `%LOCALAPPDATA%\ree`.
- Linux: `$XDG_CACHE_HOME/ree`, else `~/.cache/ree` (a relative `XDG_CACHE_HOME` is ignored).
- macOS: `~/Library/Caches/ree`.

Set `RUR_STATE_DIR` (else `RUR_CACHE_DIR`) to move them; the names are shared with rur. `ree serve --state-dir <dir>`
takes priority for that service, its engines and its workers. A service keeps its starter's standalone mode and state,
although it runs a copy of ree from the cache. Clients, services started by hand and `serve --warm` use the same
service key and resolved environment paths, including the asset worker, recorded clock and engine dump. Standalone
services are keyed by the full environment their workers receive, including custom variables. Known paths and the
PATH-style lists (PATH and NODE_PATH) are resolved in the client; any other relative value is read from the worker's
state directory, both directly and through a service (docs/engine.md, "Asset bundles").

An installed binary is standalone for every job, so `--repo <dir>` names input data only: the project, places and data
read from that folder. It never puts caches in `<repo>/.cache`, selects host worker rules, or adds implicit project
roots or snapshots. `--project`, roots and mounts still select what is mounted, and relative local content comes from
the client's project folder.

The cache folder is looked up only when a command needs state. `--version` and `--help` work without it.
A run, service, warm-up or profile command that writes files with no usable cache fails with an error that asks for
`RUR_STATE_DIR`, also with `--repo`.
A run or profile command looks up state first, before it starts a profiler or writes a file, so a missing cache stops it
with nothing written. A profile, boundary profile or comparison that no flag named goes under state, and the command
prints where it went. `profile diff` and `profile report` write their default files in `<state dir>/ree-profile`;
naming an input never names its output folder. A path you name for output is used as given, including a recording's
output folder and `profile diff --out`.

A ree built inside a checkout finds the nearest folder above it with a `Cargo.lock` and a `Cargo.toml` naming the
`roblox-engine-emulator` package. It uses that checkout's `.cache`, with the same environment overrides and `--repo`.
An unrelated Cargo project above an installed binary does not count. A library host that embeds ree keeps its own
defaults: the nearest `Cargo.lock` above its executable, else `.`, with `<repo>/.cache` as the cache. Only the `ree`
binary applies the stricter checkout test above, which it does at startup before it reads any path; a service copy
uses its starter's choice.

### How a release is built

[release.yml](../.github/workflows/release.yml) builds from the public tag on GitHub's runners, with Rust 1.90.0, a
C++ toolchain, `node tools/vendor.mjs --check` and `cargo build --release --locked --bin ree --target <triple>` with the
default features. No build cache is used.

Windows sets two things:

- `RUSTFLAGS="-C target-feature=+crt-static"` links the C/C++ runtime into the executable, so it needs no Visual C++
  redistributable.
- `CXXFLAGS_x86_64_pc_windows_msvc="-DMI_WIN_INIT_USE_FLS=1"` makes mimalloc v2 start through the runtime's own
  startup path. Without it, mimalloc's early thread-local-storage hook runs before the static runtime has started and
  the program crashes at startup. The pinned mimalloc crate compiles a C++ wrapper on MSVC, so this is a CXXFLAGS
  setting. Use `-D`: Git Bash converts a `/D` flag in the environment into a path before the compiler sees it.

The Windows build runs `--version` before packaging, so a startup failure fails the build job itself.

Each platform then gets a fresh runner, which extracts the archive outside any checkout, runs `--version`, an offline
harness, a recording and the service with redirected user caches, and checks that nothing was written outside the
cache, the output and the temp folders.

Pushes to main that touch release files, and manual runs, only upload workflow artifacts. Only a tag push publishes,
after checking that the tag matches every release manifest and its commit is on main.

### Verifying a release

Download `SHA256SUMS` from the same release and compare the archive's SHA-256 (`sha256sum -c SHA256SUMS` with the
archives present, or `Get-FileHash <zip> -Algorithm SHA256` on Windows). Then verify its provenance:

```sh
gh attestation verify <zip> -R Anime-Vanguards/roblox-engine-emulator
```

### Licenses and source

The archive holds the project notices, the GPL-3.0 text, the license texts of every crate compiled in, and `SOURCE.md`.
The distributed binary is GPL-3.0-or-later; the source stays GPL-2.0-or-later.

The Corresponding Source is:

- the public tag and its exact commit, and GitHub's source archive of that tag;
- the crates.io archives pinned in `Cargo.lock` with their SHA-256, and the git dependencies at the commits pinned there;
- the native sources, which are vendored in the repository (the vendor check compares them with the pinned upstream
  archives).

`SOURCE.md` and the release notes link these and give the toolchain and the build command. The
[release scripts](../tools/release/README.md) describe their self-checks and packaging rules.

## Requirements

- Rust 1.90 or later (edition 2024).
- A C++ toolchain for the Luau VM (on Windows, the MSVC Build Tools). `.cargo/config.toml` links the
  Windows target with rust-lld, which comes with Rust.
- Git, for the `ror-mesh` dependency (feature `meshes`), fetched from the public
  [roblox-object-renderer](https://github.com/Anime-Vanguards/roblox-object-renderer) repository.
- Node.js 20 or later, to re-vendor the VM (`tools/vendor.mjs`) and for the release scripts, which `cargo test` also
  runs through the standalone smoke test (`tests/standalone.rs`).

## Build

```sh
cargo build --release        # target/release/ree(.exe)
cargo test --release         # the engine's tests and the patched VM's
```

Nothing else is fetched: the patched Luau VM (`vendor/luau0-src`), the mlua-sys that binds it
(`vendor/mlua-sys`) and Lune's Roblox datatypes (`vendor/lune-roblox`, `vendor/lune-utils`) are vendored.
They are the pinned upstream archives with the topic series in `patches/luau/` and `patches/lune.patch`
applied, and mlua-sys with the VM's bytecode version range: `node tools/vendor.mjs` rebuilds
`vendor/` after a patch changes, `node tools/vendor.mjs --check` verifies it ([Updating
Luau](#updating-luau) moves the VM to a newer release). `--release` is the everyday build:
incremental, linked with rust-lld (`.cargo/config.toml`), and with cargo's ThinLTO across the crate's own
codegen units (`lto = false`), which keeps the engine's cross-module inlining. Measured 2026-10-10:
`lto = "off"` rebuilt a one-line edit in 7 s instead of 12 s, but property reads and writes ran 5-25%
slower, so the profile keeps it. Float results are the same either way.

Features: `jit` (Luau native code generation), `bundles` (asset manifest bundles through an asset
worker process), `meshes` (EditableMesh from mesh assets, decoded by ror's `ror-mesh`) and `cli` (the
`ree` binary), all on by default; a WebAssembly host turns off `jit` and `bundles`. `dhat-heap` builds a
`ree` that writes a heap profile of its command (or of `ree serve` when it exits) to `dhat-heap.json`,
for DHAT's viewer: `cargo build --release --features dhat-heap --target-dir target-dhat`.

`ree` allocates with mimalloc: v2, which hands a finished run's pages back where v3 kept them
committed, committing a thread's segments page by page (`MIMALLOC_EAGER_COMMIT=1` restores whole
segments). What a warm service keeps across runs (converted bundles, compiled modules, the Studio
hierarchy) is copied to a heap of its own while `alloc::long_lived` runs, so the pages a run used are
freed whole when it ends (src/alloc.rs).

## Updating Luau

The VM is a release of upstream [Luau](https://github.com/luau-lang/luau) plus a series of topic patches.
`patches/luau/upstream.json` pins the release: its source archive (SHA-256 checked) and the
[luau0-src](https://github.com/mlua-rs/luau-src-rs) crate whose build logic compiles it for mlua. The
patches in `patches/luau/` apply in file-name order, one topic each; every patch starts with a header
saying what it does and why upstream lacks it. A backport also names the first upstream release that has
it with no flag (`# Upstream: 0.735`), and a bump to that release or later leaves it out.

```sh
node tools/vendor.mjs try latest        # does the series apply to the newest release? changes nothing
node tools/vendor.mjs bump client       # move to the release a live Roblox client runs (or latest, a tag, a commit)
node tools/vendor.mjs bump --continue   # go on after resolving a topic's conflicts in .cache/vendor/bump
node tools/vendor.mjs edit              # change a topic in the middle of the series; then edit --finish
```

`bump` downloads the target's source and pins it by hash, rebases each topic onto it with git's
three-way merge, leaves out the backports the target already has, and rewrites the series and
`vendor/`, then checks it. It stops at a topic that conflicts and names the files and lines: resolve them
in `.cache/vendor/bump`, `git add` them and run `bump --continue` (`bump --abort` drops the bump).
`--crate <version>` moves to another luau0-src crate, which a release with a new source layout needs;
`--verify` builds, tests and runs the parity probes at the end. `edit` puts the series in
`.cache/vendor/edit` as one commit per topic on the pinned release: commit a change there with
`git commit --fixup=:/<topic>` (or as a new topic, `git commit -m NN-<topic>`), and `edit --finish` folds
it into its topic, rebases the later topics over it and rewrites the patches.

A merge without conflicts does not prove a topic still covers what it should: code the release added can
need it too. After a bump:

1. Read upstream's changes to the files each topic touches. New call opcodes, allocation paths and
   table lookups often need the topic's counters, hooks or hashing too, and a new path that frees
   objects needs the work an object's free does.
2. If the C API that mlua binds moved (a type tag, a callback's signature), move `mlua` and `mlua-sys` to
   the versions made for it, pinned exactly in Cargo.toml (and mlua-sys's archive in `tools/vendor.mjs`,
   which vendors it with the release's bytecode version range), and update the engine's declarations of
   the series' own functions.
3. `node tools/luau-flags.mjs`: the values a live client serves for the release's Luau flags
   (`data/luau-flags.json`, which the VM applies when it starts); it rebuilds `vendor/`. Run it again
   whenever Roblox changes them (it prints what changed since the last run).
4. `cargo build --release && cargo test --release`, and `node tools/vendor.mjs --check`.
5. `node tools/parity/run.mjs --diff`: script-visible behaviour against a real client holds or improves.
6. `tools/cost-parity`: instruction counts and prices against a client; refit what the release moved,
   keeping the held-out rows apart.
7. A crate that depends on this one moves its pins (the engine, `luau0-src` and `mlua-sys`) and runs its
   own checks.
8. THIRD_PARTY_NOTICES.md names the new release and crate.

A daily workflow (`.github/workflows/luau-upstream.yml`) runs `node tools/luau-release.mjs`, which keeps
one issue for the update. When upstream has tagged a release newer than the pinned one, or a live Roblox
client runs one that upstream's master already has, it opens "Update Luau to 0.NNN" with what `try` says
about the series (the topics that conflict, and where) and the steps above as a checklist. A release a
client runs before upstream has published it is only logged: there is nothing to build yet. A later
release moves the same issue, and the issue closes once the pin has reached the release. `node tools/luau-release.mjs --dry-run` prints what it
would do and changes nothing.

# Building

How to build ree, the cargo features and the allocator. The [README](../README.md) has the short version.

## Requirements

- Rust 1.90 or later (edition 2024).
- A C++ toolchain for the Luau VM (on Windows, the MSVC Build Tools). `.cargo/config.toml` links the
  Windows target with rust-lld, which comes with Rust.
- Git, for the `ror-mesh` dependency (feature `meshes`), fetched from the public
  [roblox-object-renderer](https://github.com/Anime-Vanguards/roblox-object-renderer) repository.
- Node.js 20 or later, only to re-vendor the VM (`tools/vendor.mjs`).

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

A weekly workflow (`.github/workflows/luau-upstream.yml`) runs `try latest` and fails when the series no
longer applies to the newest release.

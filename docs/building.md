# Building and releases

How to install and verify prebuilt `ree` binaries, build from source, configure Cargo features and mimalloc, and update the vendored Luau VM.

## Release binaries

[GitHub Releases](https://github.com/Anime-Vanguards/roblox-engine-emulator/releases) ship one flat zip per platform named `ree-<version>-<os>-<arch>.zip`. Extract `ree.exe` (Windows) or `ree` (Linux/macOS) onto your `PATH`.

Between releases, the nightly [canary](https://github.com/Anime-Vanguards/roblox-engine-emulator/releases/tag/canary) prerelease publishes `ree-canary-<os>-<arch>.zip` with stable download URLs and `SHA256SUMS`. Canary builds are untested as releases and may break.

### Platforms

| Platform | GitHub runner |
|---|---|
| Windows `x86_64` | `windows-latest` |
| Linux `x86_64` | `ubuntu-22.04` (chosen for a low glibc baseline) |
| macOS `arm64` | `macos-15` |

Maintainers develop and test on Windows. CI builds and smoke-tests every archive on a clean runner (`--version`, offline harness, recording, and `ree serve` with redirected caches and filesystem write checks) and publishes a release only when all three platforms pass.

### Runtime requirements and supported commands

- **Zero runtime dependencies for offline runs:** The patched Luau VM, Studio API reflection dump, and stubs are compiled in. Offline harnesses and recordings need no Rust, C++, Roblox Studio, GPU, or network. (Rendering a recording requires `ror`; optional manifest bundles require `REE_ASSET_WORKER` implementing the protocol in [engine.md](engine.md#asset-bundles-srcprojectmodel_assetsrs-feature-bundles); Windows hardware counters require the separate `ree-hw-counters` service.)
- **Included CLI commands:** `run`, `record`, `serve`, `--profile`, `profile report`, `profile diff`, and `profile try`.
- **Source-build-only commands:** `profile view` and `profile render` require the separate `ree-profile-view` binary from a workspace source build (release archives contain only one executable so tool managers like Rokit can install cleanly). Repository examples and `tools/` scripts also require a checkout.

### State paths and standalone behavior

Installed release binaries run strictly in standalone mode—never implicitly loading `.env`, checkout snapshots, or local place content:

- **Default cache directory:**
  - Windows: `%LOCALAPPDATA%\ree`
  - Linux: `$XDG_CACHE_HOME/ree` (relative values ignored), else `~/.cache/ree`
  - macOS: `~/Library/Caches/ree`
- **Overrides:** Set `RUR_STATE_DIR` (or legacy `RUR_CACHE_DIR`) to relocate caches; `ree serve --state-dir <dir>` overrides for that service and its workers.
- **Project & `--repo`:** The project resolves from `--project`, else the parent of the first `--roots` entry, else the current directory. In standalone mode, `--repo <dir>` supplies input data only (never placing caches in `<repo>/.cache` or adding implicit mounts). Relative `PATH`/`NODE_PATH` entries and known paths resolve in the client; other relative env values resolve against the worker's state directory. On Windows, a bare `REE_ASSET_WORKER` name is also searched next to the `ree` executable.
- **Service isolation:** Standalone services inherit their launcher's standalone mode and state directory and key on the full worker environment.
- **Cache lookup & output paths:** `--version` and `--help` work without a cache folder. Commands that need state validate the cache before writing anything (asking for `RUR_STATE_DIR` if unavailable). Unnamed profile, boundary, and what-if outputs write under `<state dir>` (default `<state dir>/ree-profile` for `profile diff` and `profile report`) and print their paths; explicit `--out` paths are used verbatim.
- **Checkout builds vs. embedded hosts:** A `ree` binary compiled inside a checkout detects the nearest ancestor containing `Cargo.lock` + `Cargo.toml` for `roblox-engine-emulator` and uses `<checkout>/.cache` (ignoring unrelated ancestor Cargo workspaces). Library hosts embedding the crate keep the standard `<repo>/.cache` default.

### How releases are built

`.github/workflows/release.yml` builds from public tags using Rust `1.90.0`, `node tools/vendor.mjs --check`, default features, and `cargo build --release --locked --bin ree --target <triple>` (no build cache).

Windows sets two build flags:
- `RUSTFLAGS="-C target-feature=+crt-static"` — statically links the C/C++ runtime so no VC++ redistributable is required.
- `CXXFLAGS_x86_64_pc_windows_msvc="-DMI_WIN_INIT_USE_FLS=1"` — makes `mimalloc` v2 initialize via FLS rather than an early TLS hook that runs before static CRT initialization (use `-D`, not `/D`, so Git Bash does not rewrite it as a path).

Windows verifies `--version` immediately after compiling. Main-branch pushes and manual runs perform dry runs uploading artifacts; tag pushes publish the passing main-branch dry-run zips (or build and test from scratch if none exist). Nightly runs build `main` if no passing run exists for HEAD, and the `canary` tag publishes the latest passing nightly zips.

### Verifying a release

Verify archive checksums against `SHA256SUMS` (`sha256sum -c SHA256SUMS` or `Get-FileHash <zip> -Algorithm SHA256`), then check GitHub build provenance:

```sh
gh attestation verify <zip> -R Anime-Vanguards/roblox-engine-emulator
```

Each archive includes GPL-3.0-or-later license terms, notices, crate license texts, and `SOURCE.md` linking the exact commit, pinned crates, vendored native sources, and rebuild commands (see `tools/release/README.md`).

## Requirements

- **Rust 1.90+** (edition 2024).
- **C++ toolchain** for compiling the Luau VM (Visual Studio MSVC Build Tools on Windows; `.cargo/config.toml` links Windows via `rust-lld`).
- **Git** (for the `ror-mesh` dependency pulled from `roblox-object-renderer` under the `meshes` feature).
- **Node.js 20+** (only needed for `tools/vendor.mjs`, parity scripts, and release/standalone smoke tests).

## Building from source

```sh
cargo build --release        # Builds target/release/ree(.exe)
cargo test --release         # Runs engine, VM, and process-guard tests
```

The patched Luau VM (`vendor/luau0-src`), `vendor/mlua-sys`, and Lune's Roblox datatypes (`vendor/lune-roblox`, `vendor/lune-utils`) are vendored in-tree from pinned upstream archives plus `patches/`. Never edit `vendor/` directly—edit `patches/` and run `node tools/vendor.mjs` (verify with `node tools/vendor.mjs --check`).

**Release profile & Cargo features:**
- `--release` uses `rust-lld` and Cargo's default ThinLTO across crate codegen units (`lto = false`) for cross-module inlining (~7 s vs. ~12 s incremental rebuilds, 5–25% faster property access; identical floating-point results).
- Default features: `jit` (Luau native codegen), `bundles` (asset-worker bundle loading), `meshes` (`EditableMesh` via `ror-mesh`), and `cli` (`ree` binary). WebAssembly targets disable `jit` and `bundles`.
- Optional `dhat-heap` feature: writes `dhat-heap.json` on exit (`cargo build --release --features dhat-heap --target-dir target-dhat`).
- **Allocator:** Uses `mimalloc` v2 (commits segments page by page unless `MIMALLOC_EAGER_COMMIT=1`, and returns pages to the OS after each run). Warm-service shared state (bundles, bytecode, Studio hierarchy) allocates on a dedicated long-lived heap (`src/alloc.rs`, `alloc::long_lived`) so per-run arenas free cleanly.

## Updating Luau

The embedded VM tracks upstream [Luau](https://github.com/luau-lang/luau) pinned in `patches/luau/upstream.json` (source archive SHA-256 + `luau0-src` build crate) plus ordered topic patches under `patches/luau/`. Each patch starts with a header explaining why it exists; backports include `# Upstream: 0.NNN` so bumps to that release drop them automatically.

```sh
node tools/vendor.mjs try latest        # Dry-run patch series against newest upstream Luau
node tools/vendor.mjs bump client       # Rebase series onto live Roblox client release (or latest / tag / commit)
node tools/vendor.mjs bump --continue   # Resume after resolving conflicts in .cache/vendor/bump (or --abort)
node tools/vendor.mjs edit              # Edit a mid-series topic in .cache/vendor/edit; finish with edit --finish
```

Useful `bump` flags: `--crate <version>` (switch `luau0-src` crate version if upstream file layout changed) and `--verify` (build, test, and run parity probes afterward).

**Post-bump checklist** (clean rebases don't guarantee new upstream code paths are covered by topic hooks):
1. Audit upstream diffs touching any file patched by a topic (new call opcodes, allocations, table lookups, or free paths often need corresponding counters/hooks).
2. If Luau's C API or type tags changed (e.g., Luau 0.747 removing the integer type tag), update `mlua` and `mlua-sys` pins in `Cargo.toml` / `tools/vendor.mjs` (or temporarily bridge with `patches/mlua-sys.patch`).
3. Refresh live Roblox client Luau FFlags (`data/luau-flags.json`) and re-vendor: `node tools/luau-flags.mjs`.
4. Verify build, tests, and vendor integrity: `cargo build --release && cargo test --release && node tools/vendor.mjs --check`.
5. Run differential parity probes: `node tools/parity/run.mjs --diff`.
6. Check work-clock calibration in `tools/cost-parity/` and refit shifted prices without overfitting held-out rows.
7. Update downstream pins (`roblox-engine-emulator`, `luau0-src`, `mlua-sys`) in dependent crates (`rur`) and run their checks.
8. Update version entries in `THIRD_PARTY_NOTICES.md`.

A daily GitHub Actions workflow (`.github/workflows/luau-upstream.yml` → `node tools/luau-release.mjs`) tracks upstream Luau and live Roblox client releases, keeping one open tracking issue with conflict diagnostics and this checklist (test locally with `node tools/luau-release.mjs --dry-run`).

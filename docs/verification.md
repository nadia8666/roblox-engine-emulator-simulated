# Verification and test suites

How `ree`'s correctness, Roblox parity, and determinism are verified.

- **Unit, VM, and process-guard tests (`cargo test --release`):** Covers core engine unit tests, script API probes, 3D world and recorder behavior, Studio-measured rules, patched Luau VM tests (`tests/luau_vm.rs`), and CLI watchdog/process guards (`tests/process_guards.rs`). Requires ~2 GB free RAM and needs no Roblox Studio, GPU, game checkout, or network.
- **Downstream UI regression gate:** `roblox-ui-renderer`'s golden suite runs its fixtures and Studio comparison replays through `ree` and verifies output stays identical to Studio recordings.
- **Differential parity probes (`tools/parity/`, [README](../tools/parity/README.md)):** Runs identical Luau probes (scheduler, signals, instances, standard libraries, datatypes) in `ree` and in a real Roblox client. `node tools/parity/run.mjs` prints pass/diff tables; `cargo test --release --test parity` enforces that every previously matching line stays matching while tracking remaining known gaps in `known.json` (plus `--server` for server scripts, `--loopback` for paired client+server sessions, and `--rur <checkout>` for probes requiring GUI layout).
- **Deterministic recordings:** Running `ree record` twice on the same harness and project produces byte-identical `scene.rbxm`, `playback.json`, `events.json`, `camera.json`, and `summary.json`.

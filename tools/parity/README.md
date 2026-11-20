# Parity probes (`tools/parity`)

Differential tests comparing `ree` directly against a real Roblox client or server. Each script in `probes/` runs unmodified on both engines and prints canonical output lines; `expected/` stores the real engine's output, and the runner diffs them.

## Commands

```sh
node tools/parity/run.mjs                                # Run all client probes and print pass/diff table
node tools/parity/run.mjs task_order --diff              # Show line-by-line diff (- real engine, + ree)
node tools/parity/run.mjs --write-known                  # Update known.json with current diffs (preserves reasons)
node tools/parity/run.mjs ui_signals --rur <rur-dir>     # Run probes requiring GUI layout via rur
node tools/parity/run.mjs --server                       # Run server-side probes (expected/server, known-server.json)
node tools/parity/run.mjs --loopback                     # Run paired client+server probes (expected/loopback, known-loopback.json)

# Re-capture expected outputs after a Roblox update
REE_PARITY_CAPTURE="<command>" node tools/parity/run.mjs --capture [probe ...]
```

`cargo test --release --test parity` enforces the exact same checks across client, server, and loopback sets: any line that previously matched must keep matching, any remaining discrepancy must be recorded in `known*.json`, and newly matching entries are flagged so you can remove them from `known*.json`.

- **`--server`:** Runs eligible probes with `--run-context server` (skipping device/renderer probes).
- **`--loopback`:** Runs paired probes (`probes/loopback/`) with `--run-context loopback`, testing server+client interaction in one session.

## Paired loopback probes

Each `probes/loopback/<name>.luau` file contains shared code at the top, followed by `--- server` and `--- client` sections. Each side executes the file with the other section blanked out (preserving line numbers in `@line:` errors) after `prelude.luau` and `loopback.luau`:
- Server code boots before the player joins; client code runs once client scripts start.
- `P.player()` returns the local player on the client, or waits for the first player to join on the server.
- `P.meet(Tag)` synchronizes both sides at a barrier and yields to a scheduler resumption point. Frame counts are tracked per side and never compared across processes.
- At completion, the server forwards its collected lines to the client, which outputs `server: <line>` followed by `client: <line>`.
- Always capture loopback probes 3 times and keep only lines identical across all 3 runs—cross-process frame phase jitter makes single-send frame counts noisy.

## Authoring a probe

- **`prelude.luau` helpers:** `P.num` / `P.f32` (portable number formatting), `P.err` (strips chunk names and normalizes line numbers to `@12:`), `P.try` (prints call outcome), `P.rng` (isolated RNG so `Random` / `math.random` can be tested), `P.sync` (aligns to a scheduler resumption point). `print` escapes invalid UTF-8 and control bytes.
- **Avoid nondeterminism:** Never print table addresses, wall timings, or raw frame-time sums. Count frames instead of relying on exact boundary durations (use `0.04` or `0.06` s rather than `0.05` s, which falls on 3 x 60 Hz frames). Headless clients tick `PreRender` independently of simulation steps, so don't rely on `PreRender`-relative ordering.
- **Directive comments:**
  - `-- parity: identity=4` — run at script identity 4 (needed to write `ModuleScript.Source`).
  - `context=edit` — run `ree` in its edit run context (`--mode edit --run-context edit`, a plugin's code) for a probe captured at identity 4, whose Plugin capability a game script lacks (`settings()`). Such a probe has no server capture.
  - `flag=<name>` — enable an engine flag (e.g., `flag=FFlagEnableLoadModule` passes `--load-module` to `ree`). Server runs automatically pass `--load-string` to match server captures.
  - `-- parity: device=desktop rendered` — capture on a rendered 1920x1080 desktop client (58 px top-bar inset). Plain `ree` lacks GUI layout, so these lines sit in `known.json` and must match 100% when run with `--rur <checkout>` (`RUR_NATIVE_ENGINE` overrides the `rur-native` binary).
- Capture new probes twice and verify identical output before checking them in.

## Capture harness contract

`expected/*.txt` headers record the Roblox engine version and date. `--capture` executes `<REE_PARITY_CAPTURE> <bodies.json> <outputs.json>`:
- Input: `[{"name", "body", "server"?}]`.
- Output: `{"engine", "date", "probes": {"<name>": {"ok", "lines", "error", "engine"?}}}`.
- Run client bodies at identity 2 (unless overridden) in a play client with `Workspace.SignalBehavior = Deferred`; run `"server": true` bodies as a server `Script` in `ServerScriptService` at identity 2.


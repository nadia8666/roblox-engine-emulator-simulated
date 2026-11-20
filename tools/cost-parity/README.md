# Work-clock parity benchmark (`tools/cost-parity`)

`os.clock` in `ree` is a deterministic work clock priced to match a real Roblox client ([docs/engine.md](../../docs/engine.md), `src/cost.rs`). This benchmark suite runs ~400 identical workloads in both `ree` and a real Roblox client to measure and calibrate per-operation pricing.

## Files

- **`Corpus.luau`:** Engine-call benchmarks grouped by family (reads, writes, methods, finds, attributes, `Instance.new`, `Clone`, reparenting, `Destroy`, signals, threads, tweens, datatypes, library calls, GC, memory, plus `libsize` and `json` scaling across 10–100,000 elements and 1 KB–1 MB). Uses median of 9 rounds; memory rows keep objects alive across rounds to isolate GC and internal bridge-table growth spikes.
- **`Language.luau`:** Pure Luau microbenchmarks loaded three ways: standard interpreted, `--!native`, and after `getfenv()` (unsafe environment disabling fastcall/import optimizations, running `M.Unsafe` rows).
- **`parity.py`:** Runs either engine, compares results, and prints error tables.
- **`fit.py`:** Computes per-row residuals (`residuals`) and fits pricing models (`luau`, `luau --native`, `library`, `sizes`) strictly on non-held-out rows.
- **`reference/`:** Captured client sessions backing `src/cost.rs` prices and `BENCH_BANDS`.

One-third of workloads are deterministically held out by name hash (`parity.held_out`) so prices cannot overfit; held-out error is reported separately (`parity.py compare --fit-rows` lists fit-eligible rows).

## Commands

```sh
python -I tools/cost-parity/parity.py ree --ree target/release/ree.exe --out ree.json
python -I tools/cost-parity/parity.py ree --device phone --out ree-phone.json
python -I tools/cost-parity/parity.py job > job.luau
python -I tools/cost-parity/parity.py compare ree.json engine.json [engine2.json ...]
python -I tools/cost-parity/parity.py compare ree.json tools/cost-parity/reference/*.json --fastest

python -I tools/cost-parity/fit.py residuals ree.json engine.json --family method
python -I tools/cost-parity/fit.py luau ree.json engine.json [--native]
python -I tools/cost-parity/fit.py sizes ree.json tools/cost-parity/reference/*.json --fastest
```

Pass `--fastest` to compare against each row's minimum across sessions (slower sessions reflect background scheduling or GC phase noise).

**Running on a real Roblox client:** Execute `job.luau` in any playtest client with identity 4 (needed to write `ModuleScript.Source` and `require` it) and save the returned JSON. Always run the full corpus per session (realistic heap sizes shift string allocation costs 2–3x), keep `Corpus.Cleanup` active between family groups so leftover parts don't pollute spatial queries, and average/take the fastest across multiple sessions because allocating datatype rows vary 2–3x with GC phase.


## Reference sessions and accuracy

- **Studio 0.744 baseline (`2026-11-08`):** Rendered client running a match place (`cost::REFERENCE_CLIENT`): `client-full-*` (full corpus), `client-sizes-*` (`libsize`, `json`, `gc`, `task`, `method`, `lib`), `client-libsize-*`, and `client-buffers-1` (256 KB / 4 MB buffer rows). After the size fit, median held-out error dropped from **25.2%** (`0.06–1.37x` p10–p90) to **15.2%** (`0.55–1.34x`, 173 priced rows; **16.5%** across all 180 rows including 7 zero-priced `LUAU_BUFFER_FRESH_BYTE` rows). Check bands with `bands.py <ree.json> --priced`.
- **Studio 0.747 update (`2026-11-14`):** `client-747-full-*` (3 full-corpus sessions). Matches 0.744 closely (`0.99x` median, `0.77–1.08x` p10–p90) except Luau 0.747's faster `string.split` (5-part split `245 → 71 ns`, fitted via `libsize.string.split.*` 10–10,000 parts). Against all kept sessions, median held-out error is **16.5%** (`0.60–1.38x`, 175 rows; down from 17.6%).

## Reading comparison tables

`compare` prints `ree` commit/device and client version/place, then per-row times (`ns`, or `bytes` for `mem` rows), `ree / engine` ratio, and cross-session spread. Summaries break down fit rows, held-out rows, and all rows (median/mean/worst absolute error, share within 25% and 50%, and median error per family). `meta` validates matching engine version, signal behavior, localization tables, compiler settings, and identical spatial query hit counts on both sides.

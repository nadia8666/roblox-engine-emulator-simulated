# What-if variant benchmarks (`ree profile try`)

Sample variant files for `ree profile try` (see [docs/cli.md](../../docs/cli.md)). Each file implements the same task several ways and measures them in fresh engines across input sizes `n = 10, 100, 1,000, 10,000`.

```sh
cargo build --release
target/release/ree profile try tools/what-if/nested-keys.luau
target/release/ree profile try tools/what-if/append.luau --sizes 1000 --no-hw
```

Each run prints per-call metrics, scaling behavior with `n`, relative ratios against the winning variant, and retained table layouts by allocation site. All data saves to `compare.json`, and each `runs/<variant>/<n>/` folder is a normal profile readable with `ree profile view`, `report`, or `diff`.

## Sample benchmarks (at `n = 10,000`)

Work clock, allocations, heap bytes, and table shapes are deterministic across runs. Host wall-clock times reflect the fastest of 9 interleaved rounds on a desktop machine (modern runs pair rounds against A/A noise and only declare a clear win above noise with >= 8/9 round wins and fastest-run agreement). The work clock includes cold first-touch penalties for tables allocated before the frame (`warm` excludes them).

| Script | Variants compared | Key findings (`n = 10,000`) |
|---|---|---|
| `nested-keys.luau` | `t[k][v]` vs. `t[v][k]` (4 kinds x `n` items) | `t[k][v]` uses 4 arrays (1.00 MB, 6 allocs) vs. 10,000 hash tables (1.93 MB, 10,002 allocs, 65x more GC steps). `t[k][v]` is 1.8x faster in host time (9/9) and 1.06x on work clock (1.23x warm). |
| `records-or-arrays.luau` | Array of structs vs. struct of arrays (`x, y, vx, vy, hp`) | 5 arrays take 782 KB vs. 3.05 MB for 10,000 5-field hash tables. Update pass uses identical instructions, but struct-of-arrays is 1.18x faster warm, 10x on full work clock (cold touches), and 1.06–1.12x in host time. |
| `string-or-number-keys.luau` | Integer keys vs. cached string keys vs. formatted string keys | Integer array takes 156 KB (16 B/slot) vs. 512 KB hash (32 B/node). 40,000 lookups: strings take 1.5–1.8x host time (9/9); building `"item" .. id` per lookup costs 4.8x work clock and 5.6–8.5x host time (zero new memory due to string interning). |
| `closures-or-methods.luau` | Metatable `__index` methods vs. per-instance closures | Per object: 1 table (80 B) vs. 1 table + 2 closures + 1 upvalue (4 allocs). Methods use 2.75x less memory, 3x fewer GC steps, 1.51x less work clock, and 1.24x less host time. |
| `append.luau` | `table.insert`, `t[#t+1]`, manual counter, `table.create(n)` + index | Growing `{}` triggers 15 doublings (16,398 rehashed keys, 256 KB) vs. 0 rehashes and 156 KB for `table.create(n)`. Host time vs. preallocated: counter `1.4x`, `table.insert` `1.6x`, `t[#t+1]` `1.8–1.9x` (`table.insert` slightly beats `t[#t+1]`). Work clock puts growing variants at 5.5–5.8x. |
| `reuse-table.luau` | New `{}`/frame vs. `table.create(n)`/frame vs. `table.clear` reuse (30 frames) | Reusing one table allocates 4 KB/frame vs. 128–156 KB, triggers zero GC steps, and avoids rehashes after frame 1 (`table.clear` keeps capacity). New `{}` costs 2.1x warm work clock. |
| `iteration.luau` | `ipairs`, numeric `for i = 1, #t`, generalized `for _, v in t`, `pairs` | `ipairs`, generalized, and `pairs` execute 2 instructions/step vs. 3 for numeric `for` (1.3–1.7x slower host time, 14/15 rounds). Work clock is within 7% because `ipairs`/`pairs` pay ~2.5 us cold C-call overhead. |
| `vector-math.luau` | Native `Vector3` vs. 3 parallel number arrays | Zero allocations either way. `Vector3` runs 3.2x fewer instructions, 2.85x less work clock, 2.9x less host time (9/9), and uses 1/3 the memory (one 16 B slot vs. three). |

## Alignment with Luau's performance guide

Every ranking matches [luau.org/performance](https://luau.org/performance): prefer array parts, preallocate with `table.create` and known indices, prefer `table.insert` over `t[#t+1]`, prefer generalized iteration/`ipairs`/`pairs` over `for i = 1, #t`, put methods on `__index` metatables rather than per-object closures, use native vectors, and reuse tables in hot loops.

## Where a live Roblox client may differ

- **Cold first touches:** The work clock charges 200 ns per pre-frame table touched in a frame (typical for scattered game-heap objects), whereas tightly packed setup loops pay ~14 ns on a client. Expect real client numbers between full work clock and `warm`.
- **Per-instruction averages:** The work clock assigns one average price per opcode kind, so array vs. hash lookup differences show up more clearly in host time than in work clock (flagged in summary notes).
- **Table growth:** Rehash prices match real client measurements and penalize growing lists more heavily than desktop host time (5.5–5.8x vs. 1.4–1.9x), though rankings match.
- **`--!native` codegen:** Benchmarks run interpreted like `ree` scripts; `--!native` speeds up numeric loops significantly on real clients and can shift relative gaps.

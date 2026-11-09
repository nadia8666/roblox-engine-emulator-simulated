# What-if comparisons

Variant files for `ree profile try` (docs/cli.md "What-if comparisons"): the same work written in different ways,
measured side by side in fresh engines, each at n = 10, 100, 1,000 and 10,000.

```sh
cargo build --release
target/release/ree profile try tools/what-if/nested-keys.luau          # writes what-if/nested-keys/ and prints the summary
target/release/ree profile try tools/what-if/append.luau --sizes 1000 --no-hw   # one size, no hardware counters
```

The summary prints the figures per call, how each grows with n, the best variant on each figure with every other
variant's figure over it, and the tables each variant's run left behind by the line that made them. `compare.json` holds
it all, and each `runs/<variant>/<n>/` is an ordinary profile (`ree profile report`, `ree profile diff`).

## What they show

Figures at n = 10,000. The work clock, allocations, heap and table shapes are exact: the same every run. Host time
here is the Luau VM in ree on one desktop machine, under load from other work: each variant's fastest of 9 interleaved
rounds (other work only ever slows a round); "won 9 of 9" is how often the faster variant was faster round by round.
These figures were read before host time's verdicts paired rounds against the A/A noise (docs/cli.md "Which numbers are
exact"); a comparison run now prints each host-time gap with its interval, and calls one clear only past that noise, by
8 of 9 rounds or more, and with the fastest runs agreeing. The work clock prices each instruction kind
at a client's measured average, and charges a cold first touch in a frame of every table made before the frame
(`warm` leaves those out).

| File | Variants | What differs |
| --- | --- | --- |
| `nested-keys.luau` | `t[k][v]`, `t[v][k]` (4 kinds by n items) | `t[k][v]` holds its index in 4 arrays of 16,384 slots (256 KB each, 1.00 MB in all); `t[v][k]` in 10,000 tables of 4 hash nodes (176 B each, 1.93 MB). Building it: 6 allocations against 10,002, 65 times fewer collector steps; host time 1.8x (won 9 of 9); work clock 1.06x (warm 1.23x: the input's 40,000 records are cold in both). |
| `records-or-arrays.luau` | array of records, record of arrays (x, y, vx, vy, hp) | The input: 3.05 MB as 10,000 records of 5 fields (hash parts of 8 nodes), 782 KB as five arrays. One update pass: the same instructions; warm work clock 1.18x for the arrays; the work clock 10x, almost all of it the records' cold first touches. Host time 1.06-1.12x for the arrays. |
| `string-or-number-keys.luau` | number keys, string keys held, a string built per lookup | 10,000 values: an array part of 16 B a slot (156 KB) against a hash part of 32 B a node (512 KB). 40,000 lookups: the same work clock for number and string keys, but host time 1.5-1.8x for strings (won 9 of 9); building `"item" .. id` per lookup 4.8x on the clock, 5.6-8.5x in host time (the strings exist already, so no new memory: Luau interns them). |
| `closures-or-methods.luau` | methods (a metatable with `__index`), closures (a table of two closures over a local) | Per object: one table of 1 node (80 B) against a table of 2 nodes, two closures and an upvalue (4 allocations): 2.75x the memory, 3x the collector steps, 1.51x the work clock, 1.24x the host time. |
| `append.luau` | `table.insert`, `t[#t+1]`, a counted index, `table.create(n)` and an index | Growing from `{}` rehashes 16,398 keys over 15 doublings and ends with 16,384 slots (256 KB); `table.create(n)` rehashes nothing and holds 156 KB. Host time against the preallocated list: counted index 1.4x, `table.insert` 1.6x, `t[#t+1]` 1.8-1.9x (each won 8 or 9 of 9); `table.insert` is a little faster than `t[#t+1]`. The work clock puts the growing three at 5.5-5.8x. |
| `reuse-table.luau` | a new `{}` a frame, a new `table.create(n)` a frame, one table reused with `table.clear` (30 frames) | Reused: 4 KB allocated a frame against 128-156 KB, no collector steps, and after its first frame no rehashes (`table.clear` keeps the parts' sizes); `table.create(n)` avoids the rehashes but allocates the most. Warm work clock 2.1x for a new `{}` a frame; host time within a few percent here. |
| `iteration.luau` | `ipairs`, numeric `for i = 1, #t`, `for _, v in t`, `pairs` | `ipairs`, generalized iteration and `pairs` take the same instructions (2 a step); the numeric for takes 3 a step and 1.3-1.7x the host time (slower in 14 of 15 rounds of a 15-round run). On the clock the loops are within 7 %: `ipairs` and `pairs` pay their C function's cold first call in a frame (about 2.5 us), so the numeric for reads 1.04x generalized iteration and they 1.07x. |
| `vector-math.luau` | `Vector3` values, three numbers in arrays | No allocation either way (a Vector3 is a value). Vector3: 3.2x fewer instructions, 2.85x less work clock, 2.9x less host time (won 9 of 9), and a third of the memory (16 B a vector in an array slot against three 16 B slots). |

## Against what is known of Luau

The rankings agree with Luau's performance guide (luau.org/performance): a table's hybrid array part is its fast,
compact part; "it's recommended to use `table.create`" and "it can be more efficient to use a known index for insertion"
when filling a list; `table.insert` is the recommended way to append; "the performance of iteration using generalized
iteration, `pairs` and `ipairs` is comparable" while "iterating through array-like tables using `for i=1,#t` tends to be
slightly slower"; an object is best a table whose metatable's `__index` is a table; native vectors give "significantly
smaller GC pressure and significantly faster execution"; and it is best "to avoid allocating memory in tight loops".

## Where a client may differ

- **Cold first touches.** The work clock charges every table made before the frame 200 ns on its first touch in a frame,
  as for data made over many frames among other allocations; a client pays about 14 ns for data made together in one
  loop, as a setup makes it. Where these dominate (nested keys, records, the per-frame list), a client's figure lies
  between the work clock and its warm figure; the summary says so in its notes.
- **What an instruction finds.** The clock prices an instruction kind at one average, so a lookup in an array part and in
  a hash part, or a numeric for and a generalized loop, cost it nearly the same where the interpreter's host time
  differs; the summary notes a gap that host time sees and the clock does not.
- **Growth.** The clock's prices for the keys a rehash moves and the bytes it stores are a client's, and put growing a
  list well above this machine's interpreter (5.5-5.8x against 1.4-1.9x); the ranking is the same.
- **Native code.** These run interpreted, as ree runs scripts; a `--!native` module in a client runs its numeric loops
  several times faster, and the gaps between variants can narrow or widen.

# The parity bench: ree's work clock against a real client

`os.clock` in ree is a work clock (docs/engine.md "Work clock", src/cost.rs): every engine call and every kind of
Luau work is charged a price. This bench measures how far those prices are from a real client's: about 400
workloads run unchanged in ree and in a Roblox client, and each one's time per operation is compared.

- `Corpus.luau`: the engine-call workloads by family (reads, writes, methods, finds, attributes, Instance.new,
  Clone, reparenting, Destroy, signals, threads, tweens, datatypes, library calls, garbage, memory; `libsize` and
  `json`: library and JSON calls at three to five sizes of their input, 10 to 100,000 elements, 1 KB to 1 MB), a scene
  they share, and the runner (median of 9 rounds; a round's own measure where the work runs later, such as a
  signal's handlers). A memory row keeps every round's objects alive until the row ends and reads its median
  round: the collector then has none of the row's own garbage to free while a round builds, and a round in
  which the engine grew a table of its own (its cache of the Instance references scripts hold doubles: one
  round of nine read 572 bytes a Folder where the others read 48) does not decide the figure.
- `Language.luau`: pure Luau loops. The job loads it three times: as is, with `--!native` (a client compiles
  that module to machine code) and after a `getfenv()` call (whose environment is then unsafe: imports are
  looked up again and builtins become C calls), which runs only the rows `M.Unsafe` names.
- `parity.py`: runs either side, compares, prints the table.
- `fit.py`: the prices that would make rows exact (`residuals`), and fits of the Luau prices by kind of
  instruction (`luau`, `luau --native`), of the library's (`library`) and of the work that grows with a call's
  input (`sizes`: elements scanned and moved, string and buffer bytes, pattern steps, allocated bytes, objects and
  large blocks, JSON text and values), each on the rows that are not held out.
- `reference/`: the client sessions the prices were fitted on and the bands measured with (below).

A third of the workloads, chosen by a hash of their names (`parity.held_out`), is held out: no price may be
fitted on it, and the table reports it apart. `parity.py compare --fit-rows` lists the rows a price may use.

## Running

```sh
python -I tools/cost-parity/parity.py ree --ree target/release/ree.exe --out ree.json     # ree's side (REE_WORK_COUNTERS=1)
python -I tools/cost-parity/parity.py ree --device phone --out ree-phone.json              # with the phone price profile
python -I tools/cost-parity/parity.py job > job.luau                                       # the engine side's job text
python -I tools/cost-parity/parity.py compare ree.json engine.json [engine2.json ...]      # the table
python -I tools/cost-parity/fit.py residuals ree.json engine.json --family method
python -I tools/cost-parity/fit.py luau ree.json engine.json [--native]
python -I tools/cost-parity/fit.py sizes ree.json tools/cost-parity/reference/*.json --fastest
python -I tools/cost-parity/parity.py compare ree.json tools/cost-parity/reference/*.json --fastest    # against the kept sessions
```

`--fastest` compares each row with its fastest session: a session's slower readings of a row are its own noise
(another job's slot, the collector's phase), not the client's price.

The engine side is any client that can set `ModuleScript.Source` and require it (identity 4: a plugin-level
command bar in a playtest client, or a job), running the job text and returning its value as JSON. Run the
whole corpus in each session: what a client has done before moves some prices (a heap of a game's size makes
new strings 2-3x dearer), and the reference numbers come from sessions that ran everything. A client that runs
the groups of families as jobs of one session keeps what each job left in the DataModel, so each job takes its
scene out again (`Corpus.Cleanup`); without it the later groups' spatial queries met every earlier group's
parts (three stacks of 24 instead of one), and Meta.queries showed it (a box of the stack found 288). Several
engine files give the noise: each row's spread between sessions is printed beside its error. The rows that
make a new value (a datatype's operation that returns one) vary 2-3x between sessions with the collector's
phase: take the reference from several sessions.


## The reference sessions

`reference/` keeps the client sessions behind the prices and src/cost.rs's `BENCH_BANDS` (2026-11-08): a rendered
roblox-sim client (Studio 0.744's engine) of a copy of a game's match place on one desktop PC, `cost::REFERENCE_CLIENT`.
`client-full-*` ran the whole corpus, `client-sizes-*` the `libsize`, `json`, `gc`, `task`, `method` and `lib`
families, `client-libsize-*` the `libsize` rows added then, and `client-buffers-1` two sessions of the `libsize` family
kept only for the buffer rows they added (256 KB and 4 MB: sessions of one row differ by up to 50 % there, so a later
session's faster rows would move every row's fastest). ree's side is not kept: `parity.py ree` makes it from the commit
at hand. Against these, the work clock of the size fit reads the held-out rows it prices 15.2 % off at the median (173
rows in ns; 0.55-1.34x from the 10th to the 90th percentile), where the clock before it reads 25.2 % (0.06-1.37x). The
seven held-out large buffer rows it leaves out count bytes it prices at nothing (src/cost.rs `LUAU_BUFFER_FRESH_BYTE`);
with them, the 180 rows read 16.5 % off (0.52-1.34x) against 27.8 % (0.03-1.37x). A band is each row's ree / client,
the client its fastest kept session, over the held-out rows in ns, from the 10th to the 90th percentile (`bands.py <ree side
json> --priced`).

## Reading the table

`compare`'s header names ree's commit and device and each engine run's version and place. Each row gives the
engine's and ree's time per operation (ns, or bytes for the `mem` rows), ree / engine, and the spread between
engine sessions; then the fit rows, the held-out rows and all rows: the median, mean and worst absolute error
and how many rows are within 25 % and 50 %, and the median error by family. A price change lists the rows it
moves, the error on the fit rows and on the held-out rows apart.

The `meta` of each side's output records what the corpus found: the engine's version, signal behaviour, the
place's localization tables, the compiler's choices (a constant vector's `typeof`), and what the spatial
rows' queries return on each side (both sides must find the same parts for a per-part price to compare).

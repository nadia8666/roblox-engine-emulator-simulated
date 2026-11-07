//! Profiles built for the tests: a frame loop whose numbers are chosen so each issue detector has one clear case
//! (a sort every frame, allocation in the steady state, tables grown a step at a time, a collector spike, a frame
//! spike, connections that keep growing, a growing heap, an inline cache that misses, a property read again in a
//! frame), written as ree writes a profile directory: profile.json, the folded stacks and boundary.json.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const MODULE: &str = "ReplicatedStorage.Fixture.Main";
/// The harness's module, where ree mounts a run's --harness (its code is not the game's)
pub const HARNESS_MODULE: &str = "ReplicatedStorage.RURFixtures.Fixture";

/// A temporary folder of the test's own, removed when dropped
pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new(name: &str) -> Temp {
        // (its name always the same length, so a screen that shows a path cuts it at the same place on any machine)
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let unique = ree_profile_view::model::fxhash(format!("{}-{stamp}", std::process::id()).as_bytes());
        let dir = std::env::temp_dir().join(format!("ree-profile-view-{name}-{unique:016x}"));
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fixture's source (40 lines): the lines the figures name hold the code they describe
pub fn source() -> String {
    let mut lines: Vec<String> = (1..=40).map(|n| format!("-- line {n}")).collect();
    let mut set = |n: usize, text: &str| lines[n - 1] = text.to_string();
    set(1, "local Items = {}");
    set(5, "local cache = { size = 0 }");
    set(10, "local function update()");
    set(11, "\tlocal records = rank(Items)");
    set(12, "\tlayout(records)");
    set(13, "end");
    set(20, "local function rank(items)");
    set(21, "\tfor _, item in items do records[#records + 1] = { item = item, score = item.Score } end");
    set(22, "\ttable.sort(records, function(a, b) return a.score > b.score end)");
    set(23, "\treturn records");
    set(30, "local function layout(records)");
    set(31, "\tlocal width = item.Size.X + item.Size.X * 0.5");
    set(32, "\tlocal buckets = {} for _, r in records do buckets[r.key] = true end");
    set(33, "\titem.Position = position");
    set(34, "end");
    lines.join("\n") + "\n"
}

/// What a fixture profile varies
#[derive(Clone, Copy)]
pub struct Shape {
    /// Sorts in the steady state's 100 frames (100: every frame)
    pub sorts: u64,
    /// Records `rank` allocates a frame
    pub records: u64,
    /// A harness function in the steady state: `watch` (Fixture:5), scanning with a C function, calling rank once in ten
    /// frames, allocating, missing its caches and reading members again
    pub harness: bool,
}

impl Shape {
    pub const BEFORE: Shape = Shape { sorts: 100, records: 200, harness: false };
    pub const AFTER: Shape = Shape { sorts: 1, records: 0, harness: false };
    pub const HARNESS: Shape = Shape { sorts: 100, records: 200, harness: true };
}

fn us(value: f64) -> f64 {
    value / 1000.0
}

/// Writes a profile directory; returns it
pub fn write_profile(dir: &Path, shape: Shape) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let source_path = dir.join("Main.luau");
    let text = source();
    std::fs::write(&source_path, &text).unwrap();
    let hash = format!("{:016x}", ree_profile_view::model::fxhash(text.as_bytes()));
    let sort_share = shape.sorts as f64 / 100.0;
    // the calling-context tree: [parent, function, calls, self work in us, self instructions, label]
    let compare_calls = 500 * shape.sorts;
    let mut tree = vec![
        json!([-1, -1, 0, 0.0, 0, "startup"]),
        json!([0, 0, 1, 500.0, 400, null]),
        json!([0, -1, 0, 0.0, 0, "frames"]),
        json!([2, 1, 20, 200.0, 1000, null]),
        json!([3, 2, 20, 600.0, 4000, null]),
        json!([4, -2, 20, 40.0, 0, "sort [C]"]),
        json!([5, 3, 10000, 800.0, 60000, null]),
        json!([3, 4, 20, 400.0, 2000, null]),
        json!([0, -1, 0, 0.0, 0, "steady"]),
        json!([8, 1, 100, 1000.0, 5000, null]),
        json!([9, 2, 100, 3000.0 * (0.25 + 0.75 * sort_share), 20000, null]),
        json!([10, -2, shape.sorts, 2.0 * shape.sorts as f64, 0, "sort [C]"]),
        json!([11, 3, compare_calls, 40.0 * shape.sorts as f64, 3000 * shape.sorts, null]),
        json!([9, 4, 100, 2000.0, 10000, null]),
        json!([13, -3, 30000, 300.0, 0, "__index [C]"]),
        json!([9, 5, 100, 100.0, 800, null]),
    ];
    if shape.harness {
        // the harness's watcher under the steady phase, its C scan (the harness's side) and a call into the game's rank
        // (the game's side: a script function is its own module's)
        tree.push(json!([8, 6, 100, 1500.0, 6000, null]));
        tree.push(json!([16, -4, 100, 2500.0, 0, "GetDescendants [C]"]));
        tree.push(json!([16, 2, 10, 30.0, 200, null]));
    }
    // per function: (self work us, total work us, self insns, total insns, calls)
    let node = |i: usize| -> (i64, i64, u64, f64, f64) {
        let n = tree[i].as_array().unwrap();
        (n[0].as_i64().unwrap(), n[1].as_i64().unwrap(), n[2].as_u64().unwrap(), n[3].as_f64().unwrap(), n[4].as_f64().unwrap())
    };
    let count = tree.len();
    let mut subtree: Vec<(f64, f64)> = (0..count).map(|i| (node(i).3, node(i).4)).collect();
    for i in (0..count).rev() {
        let parent = node(i).0;
        if parent >= 0 {
            let (w, n) = subtree[i];
            subtree[parent as usize].0 += w;
            subtree[parent as usize].1 += n;
        }
    }
    let mut figures: std::collections::BTreeMap<i64, (f64, f64, f64, f64, u64)> = std::collections::BTreeMap::new();
    for i in 0..count {
        let (_, function, calls, work, insns) = node(i);
        if function == -1 {
            continue;
        }
        let entry = figures.entry(function).or_default();
        entry.0 += work;
        entry.1 += subtree[i].0;
        entry.2 += insns;
        entry.3 += subtree[i].1;
        entry.4 += calls;
    }
    // per (function, phase): [self work, total work, calls, self insns, self work on the harness's side]; a node's phase
    // is its phase root's, its side its nearest script function's
    let harness_function = |id: i64| id == 6;
    let script_function = |id: i64| matches!(id, 0..=4 | 6);
    let mut phase_of: Vec<String> = vec![String::new(); count];
    let mut side_of: Vec<bool> = vec![false; count];
    for i in 0..count {
        let n = tree[i].as_array().unwrap();
        let (parent, function) = (node(i).0, node(i).1);
        phase_of[i] = if function == -1 { n[5].as_str().unwrap().to_string() } else { phase_of[parent as usize].clone() };
        side_of[i] = if script_function(function) { harness_function(function) } else { parent >= 0 && side_of[parent as usize] };
    }
    let mut by_phase: std::collections::BTreeMap<i64, std::collections::BTreeMap<String, [f64; 5]>> = std::collections::BTreeMap::new();
    for i in 0..count {
        let (_, function, calls, work, insns) = node(i);
        if function == -1 {
            continue;
        }
        let f = by_phase.entry(function).or_default().entry(phase_of[i].clone()).or_default();
        f[0] += work;
        f[1] += subtree[i].0;
        f[2] += calls as f64;
        f[3] += insns;
        if side_of[i] {
            f[4] += work;
        }
    }
    let (mut game_steady, mut harness_steady) = (0.0, 0.0);
    for i in 0..count {
        if node(i).1 != -1 && phase_of[i] == "steady" {
            if side_of[i] {
                harness_steady += node(i).3;
            } else {
                game_steady += node(i).3;
            }
        }
    }
    // allocations, the stacks innermost first: rank's records (a table and its storage each, 64 + 48 bytes), layout's
    // buckets (a table a frame grown 6 more times, 64 bytes a step), the chunk's cache, and the engine's thread stacks
    let records_steady = shape.records * 100;
    let records_frames = shape.records * 20;
    let records = records_steady + records_frames;
    let mut alloc_stacks = vec![
        json!({"frames": [[2, 21], [1, 10]], "type": "table", "bytes": (records * 64) as f64, "count": records as f64}),
        json!({"frames": [[2, 21], [1, 10]], "type": "table storage", "bytes": (records * 48) as f64, "count": records as f64}),
        json!({"frames": [[4, 32], [1, 10]], "type": "table storage", "bytes": 840.0 * 64.0, "count": 840.0}),
        json!({"frames": [[4, 32], [1, 10]], "type": "table", "bytes": 120.0 * 64.0, "count": 120.0}),
        json!({"frames": [[0, 5]], "type": "table", "bytes": 640.0, "count": 10.0}),
        json!({"frames": [], "type": "thread stack", "bytes": 3072.0, "count": 3.0}),
    ];
    if shape.harness {
        // the watcher's tables, regrown six times each: 28 KB a frame
        alloc_stacks.push(json!({"frames": [[6, 7]], "type": "table", "bytes": 6400.0, "count": 100.0}));
        alloc_stacks.push(json!({"frames": [[6, 7]], "type": "table storage", "bytes": 2_867_200.0, "count": 700.0}));
    }
    let rank_bytes = (records * 112) as f64;
    let alloc_total = rank_bytes + 61440.0 + 640.0 + 3072.0;
    let alloc_objects = (records * 2) as f64 + 960.0 + 10.0 + 3.0;
    let by_type = vec![
        json!({"type": "table", "group": "table", "bytes": (records * 64) as f64 + 7680.0 + 640.0, "count": records as f64 + 130.0}),
        json!({"type": "table storage", "group": "table", "bytes": (records * 48) as f64 + 53760.0, "count": records as f64 + 840.0}),
        json!({"type": "thread stack", "group": "thread", "bytes": 3072.0, "count": 3.0}),
    ];
    let alloc_of = |id: i64| -> (f64, f64) {
        match id {
            2 => (rank_bytes, (records * 2) as f64),
            4 => (61440.0, 960.0),
            0 => (640.0, 10.0),
            _ => (0.0, 0.0),
        }
    };
    let total_alloc_of = |id: i64| -> (f64, f64) {
        match id {
            1 => (rank_bytes + 61440.0, (records * 2) as f64 + 960.0),
            other => alloc_of(other),
        }
    };
    let names: std::collections::BTreeMap<i64, (&str, Option<i64>, &str)> = [
        (0, ("", Some(1), "Luau")),
        (1, ("update", Some(10), "Luau")),
        (2, ("rank", Some(20), "Luau")),
        (3, ("", Some(22), "Luau")),
        (4, ("layout", Some(30), "Luau")),
        (5, ("helper", Some(3), "engine Luau")),
        (6, ("watch", Some(5), "harness")),
        (-2, ("sort [C]", None, "C")),
        (-3, ("__index [C]", None, "C")),
        (-4, ("GetDescendants [C]", None, "C")),
    ]
    .into_iter()
    .collect();
    let functions: Vec<Value> = figures
        .iter()
        .map(|(&id, &(work, total, insns, total_insns, calls))| {
            let (name, line, kind) = names[&id];
            let module = match kind {
                "Luau" => Some(MODULE),
                "harness" => Some(HARNESS_MODULE),
                "engine Luau" => Some("rur.prelude"),
                _ => None,
            };
            let kind = if kind == "harness" { "Luau" } else { kind };
            let phases: serde_json::Map<String, Value> = by_phase
                .get(&id)
                .into_iter()
                .flatten()
                .map(|(phase, f)| (phase.clone(), json!({"self_work_ms": us(f[0]), "total_work_ms": us(f[1]), "calls": f[2] as u64, "insns": f[3] as u64, "harness_self_work_ms": us(f[4])})))
                .collect();
            let key = match (module, name) {
                (Some(m), "") => format!("{m}:anonymous@{}", line.unwrap()),
                (Some(m), n) => format!("{m}:{n}"),
                (None, n) => n.to_string(),
            };
            let (alloc, allocs) = alloc_of(id);
            let (total_alloc, total_allocs) = total_alloc_of(id);
            json!({
                "id": id, "key": key, "name": name, "module": module, "line": line, "last_line": line.map(|l| l + 4), "kind": kind,
                "native": false, "native_entries": 0, "native_work_ms": 0.0, "calls": calls, "harness": harness_function(id), "phases": phases,
                "self": {"insns": insns, "work_ms": us(work), "time_ms": 0.0, "alloc_bytes": alloc, "allocs": allocs},
                "total": {"insns": total_insns, "work_ms": us(total), "time_ms": 0.0, "alloc_bytes": total_alloc, "allocs": total_allocs},
            })
        })
        .collect();
    let line = |function: i64, line: i64, insns: f64, work_us: f64, ops: Value| json!({"function": function, "line": line, "insns": insns, "work_ms": us(work_us), "total_work_ms": us(work_us), "total_insns": insns, "time_ms": 0.0, "total_time_ms": 0.0, "alloc_bytes": 0.0, "allocs": 0.0, "ops": ops});
    let rank_work = 600.0 + 3000.0 * (0.25 + 0.75 * sort_share);
    let lines = vec![
        line(2, 21, 20000.0, rank_work * 0.8, json!({"NEWTABLE": 12000, "SETTABLEKS": 8000})),
        line(2, 22, 4000.0, rank_work * 0.2, json!({"CALL": 120, "DUPCLOSURE": 120})),
        line(3, 22, 60000.0 + 3000.0 * shape.sorts as f64, 800.0 + 40.0 * shape.sorts as f64, json!({"GETTABLEKS": 30000, "JUMPIFLT": 15000})),
        line(4, 31, 8000.0, 1700.0, json!({"GETTABLEKS": 6000, "ADD": 2000})),
        line(4, 32, 4000.0, 700.0, json!({"NEWTABLE": 120, "SETTABLE": 3000})),
        line(1, 11, 4000.0, 900.0, json!({"CALL": 120})),
        line(1, 12, 2000.0, 300.0, json!({"CALL": 120})),
        line(0, 5, 400.0, 500.0, json!({"NEWTABLE": 1})),
    ];
    let total_work: f64 = figures.values().map(|f| f.0).sum();
    // the timeline: 20 frames of phase frames, 100 of steady; a collector spike at frame 90, a work spike at frame 60,
    // and connections growing a frame through the steady state
    let rows: Vec<Value> = (1..=120u64)
        .map(|frame| {
            let phase = if frame <= 20 { 1 } else { 2 };
            let work = if frame == 60 { 2.0 } else if phase == 1 { 0.102 } else { 0.106 };
            let gc = if frame == 90 { 3.0 } else { 0.05 };
            let connections = if phase == 2 { 10 + (frame - 21) } else { 10 };
            let (allocated, allocations) = if phase == 2 { (shape.records as f64 * 112.0 + 512.0, shape.records as f64 * 2.0 + 8.0) } else { (shape.records as f64 * 112.0 + 512.0, shape.records as f64 * 2.0 + 8.0) };
            json!([frame, frame as f64 / 60.0, phase, 0.4, work, 3000, 1_000_000 + frame * 1000, allocated, allocations, if frame == 90 { 400 } else { 2 }, gc, if frame == 90 { 900_000 } else { 1000 }, 500, connections, 3, 1_000_000])
        })
        .collect();
    let snapshot = |label: &str, frame: u64, reachable: f64, by_type: Value, biggest: Value| {
        json!({
            "label": label, "frame": frame, "seconds": frame as f64 / 60.0, "gcinfo_kb": reachable / 1024.0 + 100.0, "total_bytes": reachable + 102400.0,
            "objects": 5000, "object_bytes": reachable + 51200.0,
            "reachable": {"count": 4000, "bytes": reachable},
            "unswept": {"count": 1000, "bytes": 51200.0, "by_type": [{"type": "table", "count": 1000, "bytes": 51200.0}]},
            "by_type": by_type,
            "allocated_since": {"count": 600.0, "bytes": 61440.0},
            "biggest": biggest,
            "by_stack": [{"frames": [[4, 32], [1, 10]], "type": "table", "count": 120.0, "bytes": 61440.0}],
        })
    };
    let heap_types = json!([{"type": "table", "count": 3000, "bytes": 2_000_000.0}, {"type": "function prototype", "count": 200, "bytes": 600_000.0}, {"type": "string", "count": 800, "bytes": 300_000.0}]);
    let holders = json!([
        {"type": "table", "name": "", "size": 4096.0, "retained": 1_800_000.0, "path": "registry > Items", "stack": [[0, 5]]},
        {"type": "thread", "name": "", "size": 6768.0, "retained": 220_000.0, "path": "(engine reference)", "stack": null},
    ]);
    // what the scripts asked the engine to do, so far at each mark and at the end (ree 64fb6e6): the mount makes the
    // grid, the frames add a little, and the steady state clones, destroys and writes a frame's worth each frame (a
    // tenth of it after the change, and half the writes)
    let names = ["cloned", "created", "destroyed", "entered", "writes", "changed", "geometry", "toggles", "pivots", "bulk_moved", "emits", "queries"];
    let mount = [40.0, 12.0, 0.0, 52.0, 300.0, 260.0, 0.0, 0.0, 0.0, 0.0, 0.0, 3.0];
    let frames_ops = [0.0, 0.0, 0.0, 0.0, 40.0, 40.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let busy = shape.records > 0;
    let steady_ops = if busy { [100.0, 0.0, 100.0, 100.0, 3000.0, 2400.0, 600.0, 20.0, 10.0, 0.0, 50.0, 200.0] } else { [10.0, 0.0, 10.0, 10.0, 1200.0, 1150.0, 300.0, 20.0, 10.0, 0.0, 50.0, 200.0] };
    let engine_at = |mark: usize| -> Value {
        let mut map = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            let so_far = [0.0, mount[index], mount[index] + frames_ops[index], mount[index] + frames_ops[index] + steady_ops[index]][mark];
            map.insert(name.to_string(), json!(so_far));
        }
        Value::Object(map)
    };
    let steady_engine: Value = Value::Object(names.iter().zip(steady_ops).map(|(name, n)| (name.to_string(), json!(n))).collect());
    let mut cache_sites = vec![json!({"function": 4, "line": 31, "pc": 7, "op": "GETTABLEKS", "hits": 1000, "misses": 5000, "slow": 0, "other": 0}), json!({"function": 2, "line": 21, "pc": 3, "op": "GETTABLEKS", "hits": 89000, "misses": 0, "slow": 0, "other": 2000})];
    if shape.harness {
        // (the watcher's lookups miss every time: the harness's, not an issue of the game's)
        cache_sites.push(json!({"function": 6, "line": 6, "pc": 2, "op": "GETTABLEKS", "hits": 0, "misses": 9000, "slow": 0, "other": 0}));
    }
    let profile = json!({
        "version": 1, "tool": "ree",
        "kinds": ["work", "caches", "alloc", "heap", "timeline", "cpu", "phases"],
        "run": {"command": "run", "status": "ok", "harness": "C:/fixture/Fixture.luau", "frames": 120, "seconds": 2.0, "fps": 60.0, "elapsed": 0.5},
        "phases": ["startup", "frames", "steady"],
        "marks": [{"phase": "startup", "frame": 0, "seconds": 0.0, "work_ms": 0.0, "engine": engine_at(0)}, {"phase": "frames", "frame": 0, "seconds": 0.0, "work_ms": 0.5, "engine": engine_at(1)}, {"phase": "steady", "frame": 20, "seconds": 0.333, "work_ms": 2.54, "engine": engine_at(2)}],
        "end": {"phase": "end", "frame": 120, "seconds": 2.0, "work_ms": us(total_work) + 0.06, "engine": engine_at(3)},
        "warnings": [],
        "functions": functions,
        "lines": lines,
        "edges": [],
        "tree": tree,
        "work": {"total_ms": us(total_work) + 0.06, "outside_instructions_ms": 0.06, "per_second_ms": (us(total_work) + 0.06) / 2.0, "per_frame_ms": (us(total_work) + 0.06) / 120.0,
            "fidelity": "fidelity: a fixture's numbers, chosen for the tests", "script_insns": 40000, "engine_insns": 800, "native_functions": [], "opcodes": [{"op": "GETTABLEKS", "count": 36000, "work_ms": 1.0}]},
        "luau_caches": {"by_kind": [{"kind": "table key slot", "hits": 90000, "misses": 5000, "slow": 0, "other": 2000}, {"kind": "import", "hits": 400, "misses": 0, "slow": 4, "other": 0}],
            "sites": cache_sites},
        "alloc": {"bytes": alloc_total, "objects": alloc_objects, "step": 1, "by_type": by_type, "by_stack": alloc_stacks},
        "heap": {"snapshots": [snapshot("start", 0, 512_000.0, heap_types.clone(), holders.clone()), snapshot("steady", 20, 1_048_576.0, heap_types.clone(), holders.clone()), snapshot("end", 120, 3_670_016.0, heap_types, holders)],
            "gc": {"cycles": 3, "steps": 640, "freed": 2_400_000, "ms": 9.5, "by_state": [{"state": "propagate", "steps": 400, "ms": 6.0}, {"state": "sweep", "steps": 240, "ms": 3.5}]}},
        "ree_caches": [{"name": "member atoms", "about": "member names to their atoms", "hits": 120000, "misses": 12, "evictions": 0}, {"name": "unused", "about": "", "hits": 0, "misses": 0, "evictions": 0}],
        "timeline": {"columns": ["frame", "seconds", "phase", "host_ms", "work_ms", "instructions", "heap_bytes", "allocated_bytes", "allocations", "gc_steps", "gc_ms", "gc_freed", "instances", "connections", "threads", "cycles"], "rows": rows},
        "cpu": {"thread_cycles": 1_000_000_000u64},
        "listings": {
            "1": {"path": source_path.to_string_lossy(), "lines": [[10, "local function update()"]]},
            "2": {"path": source_path.to_string_lossy(), "lines": [[20, "local function rank(items)"]]},
        },
        "source_hashes": {MODULE: hash},
        "scope": {"phases": ["steady"], "game_self_ms": us(game_steady), "harness_self_ms": us(harness_steady), "harness_folder": "ReplicatedStorage.RURFixtures", "engine": steady_engine},
        "summary": "ree profile: run ok (a test fixture)\n",
        "files": ["summary.txt", "profile.json"],
    });
    std::fs::write(dir.join("profile.json"), serde_json::to_string(&profile).unwrap()).unwrap();
    std::fs::write(dir.join("summary.txt"), "ree profile: run ok (a test fixture)\n").unwrap();
    // the folded allocations by phase: what profile.json's stacks hold, split by phase
    let frame = |name: &str, line: i64| format!("{name} {MODULE}:{line}");
    let update = frame("update", 10);
    let rank = frame("rank", 20);
    let layout = frame("layout", 30);
    let chunk = frame("anonymous", 1);
    let bytes = format!(
        "steady;{update};{rank} @21 {}\nframes;{update};{rank} @21 {}\nsteady;{update};{layout} @32 51200\nframes;{update};{layout} @32 10240\nstartup;{chunk} @5 640\nsteady 3072\n",
        records_steady * 112,
        records_frames * 112
    );
    let counts = format!("steady;{update};{rank} @21 {}\nframes;{update};{rank} @21 {}\nsteady;{update};{layout} @32 800\nframes;{update};{layout} @32 160\nstartup;{chunk} @5 10\nsteady 3\n", records_steady * 2, records_frames * 2);
    let watch = format!("watch {HARNESS_MODULE}:5");
    let (bytes, counts) = if shape.harness { (format!("{bytes}steady;{watch} @7 2873600\n"), format!("{counts}steady;{watch} @7 800\n")) } else { (bytes, counts) };
    std::fs::write(dir.join("alloc.folded"), bytes).unwrap();
    std::fs::write(dir.join("alloc.counts.folded"), counts).unwrap();
    // the boundary profile: Part.Size read again 200 times a frame at Main:31, Position written unchanged at Main:33
    let mut boundary = json!({
        "version": 1, "phase": "steady", "emulated_seconds": 1.6667, "frames": 100, "crossings": 33000, "crossings_per_s": 19800.0, "cost_ms_per_s": 4.0,
        "by_site_count": [{"script": MODULE, "function": "layout", "line": 33, "kind": "write", "member": "Part.Position", "count": 3000, "per_s": 1800.0, "cost_ms_per_s": 1.0, "unchanged_writes": 3000, "repeat_reads": 0, "repeat_reads_per_s": 0.0}],
        "by_site_cost": [],
        "by_member": [{"member": "Part.Size", "kind": "read", "count": 30000, "per_s": 18000.0, "cost_ms_per_s": 3.0, "call_sites": 1, "reads_per_frame": 300.0, "distinct_per_frame": 100.0, "same_function_repeats_per_frame": 200.0, "cross_function_repeats_per_frame": 0.0}],
        "repeat_reads": [{"script": MODULE, "function": "layout", "line": 31, "kind": "read", "member": "Part.Size", "count": 30000, "per_s": 15000.0, "cost_ms_per_s": 3.0, "unchanged_writes": 0, "repeat_reads": 20000, "repeat_reads_per_s": 12000.0}],
    });
    if shape.harness {
        // (the watcher reads each part's Name again: the harness's)
        boundary["repeat_reads"].as_array_mut().unwrap().push(json!({"script": HARNESS_MODULE, "function": "watch", "line": 6, "kind": "read", "member": "Instance.Name", "count": 60000, "per_s": 36000.0, "cost_ms_per_s": 6.0, "unchanged_writes": 0, "repeat_reads": 50000, "repeat_reads_per_s": 30000.0}));
    }
    std::fs::write(dir.join("boundary.json"), serde_json::to_string(&boundary).unwrap()).unwrap();
    dir.to_path_buf()
}

/// A what-if folder: t[k][v] against t[v][k] at three sizes, with the two largest runs' profiles
pub fn write_comparison(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    write_profile(&dir.join("runs/t-k-v/1000"), Shape::BEFORE);
    write_profile(&dir.join("runs/t-v-k/1000"), Shape::AFTER);
    let run = |variant: &str, slug: &str, n: f64, work: f64, alloc: f64, host: f64, hit: f64, tables: Value| {
        json!({
            "variant": variant, "size": n, "status": "ok", "error": null,
            "profile": if n == 1000.0 { json!(format!("runs/{slug}/1000")) } else { Value::Null }, "hw_profile": null,
            "digest": "9f3c0a21", "check": {"ok": true, "message": null},
            "setup": {"work_ms": work / 2.0},
            "run": {"work_ms": work, "insns": work * 10000.0, "alloc_bytes": alloc, "allocs": alloc / 64.0, "heap_bytes": alloc, "host_ms": host, "cache_hit_rate": hit, "rehashes": n / 10.0, "gc_ms": host / 4.0},
            "host": {"ms": [host, host * 1.02], "cycles": [5200.0, 5310.0], "reps": 2500},
            "heap": {"before": 1_052_672.0, "after_setup": 1_060_864.0 + alloc, "after_run": 1_061_888.0 + alloc},
            "tables": tables,
            "hot": [{"key": format!("{slug}:build"), "name": "build", "module": slug, "line": 14, "work_ms": work * 0.8, "insns": work * 8000.0}],
        })
    };
    let kv_tables = |n: f64| json!({"count": n + 2.0, "bytes": n * 120.0 + 640.0, "setup": {"count": 1.0, "bytes": 640.0}, "run": {"count": n + 1.0, "bytes": n * 120.0}, "sites": [{"site": "input:5", "frames": [{"module": "input", "name": "setup", "line": 5, "path": "input.luau"}], "phase": "setup", "count": 1.0, "bytes": 640.0, "header_bytes": 56.0, "array_bytes": 584.0, "hash_bytes": 0.0, "array": {"slots": 64, "used": 64, "min": 64, "median": 64, "max": 64, "hist": [[64, 1]]}, "hash": {"slots": 0, "used": 0, "min": 0, "median": 0, "max": 0, "hist": [[0, 1]]}, "levels": {"min": 1, "median": 1, "max": 1, "hist": [[1, 1]]}, "rehashes": 0.0, "rehash_keys": 0.0}, {"site": "tkv:14 < tkv:30", "frames": [{"module": "tkv", "name": "build", "line": 14, "path": "tkv.luau"}], "phase": "run", "count": n, "bytes": n * 112.0, "header_bytes": n * 56.0, "array_bytes": 0.0, "hash_bytes": n * 56.0, "array": {"slots": 0, "used": 0, "min": 0, "median": 0, "max": 0, "hist": [[0, n]]}, "hash": {"slots": n * 2.0, "used": n * 2.0, "min": 2, "median": 2, "max": 2, "hist": [[2, n]]}, "levels": {"min": 1, "max": 1, "hist": [[1, n]]}, "rehashes": n, "rehash_keys": n * 3.0}]});
    let vk_tables = |n: f64| json!({"count": 3.0, "bytes": n * 40.0, "sites": [{"site": "tvk:20 < tvk:30", "frames": [{"module": "tvk", "name": "build", "line": 20, "path": "tvk.luau"}], "phase": "run", "count": 2.0, "bytes": n * 32.0, "header_bytes": 112.0, "array_bytes": 0.0, "hash_bytes": n * 32.0, "array": {"slots": 0, "used": 0, "min": 0, "median": 0, "max": 0, "hist": [[0, 2]]}, "hash": {"slots": n * 2.0, "used": n * 2.0, "min": n, "median": n, "max": n, "hist": [[n, 2]]}, "levels": {"min": 1, "max": 1, "hist": [[1, 2]]}, "rehashes": 22.0, "rehash_keys": n * 4.0}]});
    let mut runs = Vec::new();
    for n in [10.0, 100.0, 1000.0] {
        runs.push(run("t[k][v]", "t-k-v", n, 0.0002 * n, 120.0 * n, 0.00011 * n, 0.95, kv_tables(n)));
        runs.push(run("t[v][k]", "t-v-k", n, 0.0001 * n * (n / 10.0).log10().max(1.0), 40.0 * n, 0.00009 * n, 0.99, vk_tables(n)));
    }
    let compare = json!({
        "format": "ree-what-if", "version": 1,
        "title": "t[k][v] or t[v][k]", "about": "two keys, many values each", "file": "nested-keys.luau", "out": ".",
        "command": "ree profile try nested-keys.luau",
        "baseline": "t[k][v]",
        "variants": [{"name": "t[k][v]", "slug": "t-k-v", "kind": "function", "source": "nested-keys.luau:12"}, {"name": "t[v][k]", "slug": "t-v-k", "kind": "function", "source": "nested-keys.luau:20"}],
        "sizes": [10, 100, 1000],
        "frames": 1,
        "fidelity": "fidelity: a fixture",
        "metrics": [
            {"key": "work_ms", "label": "work clock", "unit": "ms", "better": "lower", "exact": true, "about": "the work clock a call took: the same every run"},
            {"key": "insns", "label": "instructions", "unit": "count", "better": "lower", "exact": true, "about": ""},
            {"key": "alloc_bytes", "label": "allocated", "unit": "bytes", "better": "lower", "exact": true, "about": ""},
            {"key": "heap_bytes", "label": "heap kept", "unit": "bytes", "better": "lower", "exact": true, "about": ""},
            {"key": "host_ms", "label": "host time", "unit": "ms", "better": "lower", "exact": false, "about": "the fastest of the timed rounds"},
            {"key": "cache_hit_rate", "label": "cache hit rate", "unit": "ratio", "better": "higher", "exact": true, "about": ""},
            {"key": "rehashes", "label": "rehashes", "unit": "count", "better": "lower", "exact": true, "about": ""},
            {"key": "hw_cycles", "label": "cycles", "unit": "count", "better": "lower", "exact": false, "about": ""},
            {"key": "gc_ms", "label": "collector host time", "unit": "ms", "better": "none", "exact": false, "per_call": true, "about": ""}
        ],
        "runs": runs,
        "scaling": [
            {"variant": "t[k][v]", "metric": "work_ms", "exponent": 1.0, "r2": 1.0, "last": 1.0, "points": [[10, 0.002], [100, 0.02], [1000, 0.2]]},
            {"variant": "t[v][k]", "metric": "work_ms", "exponent": 1.15, "r2": 0.99, "last": 1.18, "points": [[10, 0.001], [100, 0.01], [1000, 0.2]]}
        ],
        "functions": [{"variant": "t[v][k]", "key": "tvk:sortAll", "name": "sortAll", "module": "tvk", "line": 40, "exponent": 1.9, "points": [[10, 0.0001], [100, 0.008], [1000, 0.6]]}],
        "winners": [
            {"metric": "work_ms", "size": 10, "winner": "t[v][k]", "value": 0.001, "runner_up": "t[k][v]", "ratio": 2.0, "vs_baseline": 2.0, "clear": true},
            {"metric": "work_ms", "size": 100, "winner": "t[v][k]", "value": 0.01, "runner_up": "t[k][v]", "ratio": 2.0, "vs_baseline": 2.0, "clear": true},
            {"metric": "work_ms", "size": 1000, "winner": "t[k][v]", "value": 0.2, "runner_up": "t[v][k]", "ratio": 1.0, "vs_baseline": 1.0, "clear": false},
            {"metric": "alloc_bytes", "size": 1000, "winner": "t[v][k]", "value": 40000.0, "runner_up": "t[k][v]", "ratio": 3.0, "vs_baseline": 3.0, "clear": true},
            {"metric": "rehashes", "size": 10, "winner": "t[k][v]", "value": 1.0, "runner_up": "t[v][k]", "ratio": 1.0, "vs_baseline": 1.0, "clear": true, "material": false}
        ],
        "aa": [{"size": 1000, "ratio": 1.01, "spread": 0.03}],
        "hw": {"read": false, "why": "the counter service is not installed"},
        "check": {"ran": true, "ok": true, "failures": []},
        "diffs": [{"variant": "t[v][k]", "size": 1000, "dir": "diff/t-v-k"}],
        "notes": ["a fixture: the numbers are made up"]
    });
    std::fs::write(dir.join("compare.json"), serde_json::to_string(&compare).unwrap()).unwrap();
    dir.to_path_buf()
}

/// The profile at `dir` opened as the viewer opens it
pub fn open(paths: &[&Path]) -> ree_profile_view::app::App {
    let paths: Vec<PathBuf> = paths.iter().map(|p| p.to_path_buf()).collect();
    ree_profile_view::cli::open(&paths, ree_profile_view::theme::ThemeKind::Dark).unwrap()
}

/// The screen as text after a key script
pub fn screen(app: &mut ree_profile_view::app::App, keys: &str, cols: u16, rows: u16) -> String {
    ree_profile_view::cli::play(app, keys, cols, rows).unwrap();
    ree_profile_view::export::text(&ree_profile_view::cli::screen(app, cols, rows))
}

/// Compares a screen with its snapshot in tests/snapshots (REE_VIEW_BLESS=1 writes them); the folders of the
/// fixture's temporary files are replaced by `<tmp>`
pub fn snapshot(name: &str, text: &str, temp: &Path) {
    let temp_text = temp.to_string_lossy().to_string();
    let mut text = text.replace(&temp_text, "<tmp>");
    // (the folder's own name too: the Source view shows a path's last parts)
    for alt in [temp_text.replace('\\', "/"), temp.file_name().unwrap().to_string_lossy().to_string()] {
        text = text.replace(&alt, "<tmp>");
    }
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("snapshots").join(format!("{name}.txt"));
    // (REE_VIEW_BLESS=1 writes beside the tests; a folder writes there instead, for a build in another tree)
    if let Some(bless) = std::env::var_os("REE_VIEW_BLESS") {
        let file = if bless == "1" { file } else { PathBuf::from(bless).join(format!("{name}.txt")) };
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&file).unwrap_or_else(|_| panic!("no snapshot {}: run the tests with REE_VIEW_BLESS=1 and read it", file.display()));
    let expected = expected.replace("\r\n", "\n");
    if expected != text {
        let mut report = String::new();
        for (index, (want, got)) in expected.lines().zip(text.lines()).enumerate() {
            if want != got {
                report.push_str(&format!("line {}:\n  want: {want}\n  got:  {got}\n", index + 1));
            }
        }
        if expected.lines().count() != text.lines().count() {
            report.push_str(&format!("lines: want {}, got {}\n", expected.lines().count(), text.lines().count()));
        }
        panic!("snapshot {name} differs (REE_VIEW_BLESS=1 rewrites it):\n{report}\n--- got ---\n{text}");
    }
}

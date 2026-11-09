//! `ree-hw-counters probe`: checks that each counter counts what its name says, through the service, on kernels whose
//! counts are known: a loop of four instructions and one branch an iteration; random branches, half of which mispredict,
//! beside the same loop always taken; loads that hit the L1 data cache; chases through a random cycle of cache lines in
//! buffers that fit the L1, the L2, the L3, and none (each step a load that waits on the one before); arithmetic with no
//! loads; and streams of lines that miss, read several loads a line, in an order the prefetcher cannot follow and in
//! one it can. Then the readings themselves: calls of a function that waits on memory between calls of one that
//! computes, with a reading around each call (and around one call in four), must put the stall cycles on the first.
//! The thread is pinned to one CPU, and each kernel runs between two readings, so its counts are exactly its own.

use std::arch::asm;

use crate::{client, counters, ffi, protocol, readings, register};

/// 4 instructions and a branch an iteration
fn spin(iterations: u64) {
    unsafe {
        asm!("2:", "add {a}, 1", "add {b}, 1", "dec {n}", "jnz 2b", n = inout(reg) iterations => _, a = inout(reg) 0u64 => _, b = inout(reg) 0u64 => _, options(nomem, nostack));
    }
}

/// A xorshift step and a branch on `mask` of it an iteration (mask 1: taken at random; 0: always taken), and the
/// loop's own branch
fn branches(iterations: u64, mask: u64) {
    unsafe {
        asm!(
            "2:",
            "mov {t}, {x}", "shl {t}, 13", "xor {x}, {t}",
            "mov {t}, {x}", "shr {t}, 7", "xor {x}, {t}",
            "mov {t}, {x}", "shl {t}, 17", "xor {x}, {t}",
            "test {x}, {m}", "jz 3f", "add {c}, 1",
            "3:", "dec {n}", "jnz 2b",
            n = inout(reg) iterations => _, x = inout(reg) 0x9E37_79B9_7F4A_7C15u64 => _, t = out(reg) _, c = inout(reg) 0u64 => _, m = in(reg) mask,
            options(nomem, nostack)
        );
    }
}

/// Four loads from one cache line an iteration
fn loads(iterations: u64, line: *const u64) {
    unsafe {
        asm!(
            "2:", "mov {t}, qword ptr [{p}]", "mov {t}, qword ptr [{p} + 8]", "mov {t}, qword ptr [{p} + 16]", "mov {t}, qword ptr [{p} + 24]", "dec {n}", "jnz 2b",
            n = inout(reg) iterations => _, p = in(reg) line, t = out(reg) _, options(nostack, readonly)
        );
    }
}

/// `steps` dependent loads along a cycle of pointers
fn chase(start: *const u64, steps: u64) {
    unsafe {
        asm!("2:", "mov {p}, qword ptr [{p}]", "dec {n}", "jnz 2b", p = inout(reg) start => _, n = inout(reg) steps => _, options(nostack, readonly));
    }
}

/// `steps` dependent loads along a cycle from `start`, as [`chase`]; where it stopped, for the next call to go on from
fn chase_on(start: *const u64, steps: u64) -> *const u64 {
    let mut at = start;
    unsafe {
        asm!("2:", "mov {p}, qword ptr [{p}]", "dec {n}", "jnz 2b", p = inout(reg) at, n = inout(reg) steps => _, options(nostack, readonly));
    }
    at
}

/// A multiply, an add and an exclusive or an iteration, each on the one before: no loads
fn arith(iterations: u64) {
    unsafe {
        asm!("2:", "imul {a}, {b}", "add {b}, 7", "xor {a}, {b}", "dec {n}", "jnz 2b", n = inout(reg) iterations => _, a = inout(reg) 1u64 => _, b = inout(reg) 3u64 => _, options(nomem, nostack));
    }
}

/// The lines `order` names (line indices into `base`), eight loads each: the line's address comes from `order`, never
/// from a load of the stream, so many lines are in flight at once
fn stream8(base: *const u64, order: &[u32]) {
    unsafe {
        asm!(
            "2:",
            "mov {i:e}, dword ptr [{o}]", "shl {i}, 6", "add {i}, {b}",
            "mov {t}, qword ptr [{i}]", "mov {t}, qword ptr [{i} + 8]", "mov {t}, qword ptr [{i} + 16]", "mov {t}, qword ptr [{i} + 24]",
            "mov {t}, qword ptr [{i} + 32]", "mov {t}, qword ptr [{i} + 40]", "mov {t}, qword ptr [{i} + 48]", "mov {t}, qword ptr [{i} + 56]",
            "add {o}, 4", "dec {n}", "jnz 2b",
            o = inout(reg) order.as_ptr() => _, n = inout(reg) order.len() as u64 => _, b = in(reg) base, i = out(reg) _, t = out(reg) _, options(nostack, readonly)
        );
    }
}

/// A random generator for the buffers' orders (xorshift64)
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Shuffles `items` (Fisher and Yates)
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let other = (self.next() % (index as u64 + 1)) as usize;
            items.swap(index, other);
        }
    }
}

/// The lines of a buffer of `lines` lines, each `group` lines' worth in a random order before the next group's, the
/// groups in a random order: within a group of 2 MB a step's page stays in the TLB, so a step pays the cache's miss and
/// not a page walk as well, while every line is still a stranger to the caches
fn grouped_order(lines: usize, group: usize, random: &mut Random) -> Vec<u32> {
    let group = group.clamp(1, lines);
    let mut groups: Vec<usize> = (0..lines.div_ceil(group)).collect();
    random.shuffle(&mut groups);
    let mut order = Vec::with_capacity(lines);
    for first in groups {
        let mut members: Vec<u32> = (first * group..((first + 1) * group).min(lines)).map(|line| line as u32).collect();
        random.shuffle(&mut members);
        order.extend(members);
    }
    order
}

/// A buffer of `bytes` whose cache lines point at each other in one cycle, in the order of [`grouped_order`]
fn cycle(bytes: usize, group_bytes: usize) -> Vec<u64> {
    let lines = (bytes / 64).max(2);
    let mut random = Random(0x2545_F491_4F6C_DD1D);
    let order = grouped_order(lines, (group_bytes / 64).max(1), &mut random);
    let mut buffer = vec![0u64; lines * 8];
    let base = buffer.as_mut_ptr();
    for position in 0..lines {
        let (from, to) = (order[position] as usize, order[(position + 1) % lines] as usize);
        unsafe { *base.add(from * 8) = base.add(to * 8) as u64 };
    }
    buffer
}

/// A check of a kernel's counts
#[derive(Clone, Copy, Debug)]
enum Check {
    /// A counter a unit, in [low, high]
    Per(&'static str, f64, f64),
    /// One counter over another, in [low, high] (labelled)
    Ratio(&'static str, &'static str, &'static str, f64, f64),
    /// A miss's latency (the miss buffer's occupancy over its allocations) over the cycles of a unit, in [low, high]
    LatencyOfUnit(f64, f64),
}

struct Kernel<'a> {
    name: String,
    /// What the per-unit figures count (an iteration, a load, a step, a line)
    unit: &'static str,
    units: u64,
    run: Box<dyn Fn() + 'a>,
    checks: Vec<Check>,
}

/// What the L3 chase's last-level fills must read, from where its steps were served: its own speed beside the memory
/// chase's. At L3 speed few steps fill from beyond the L3; at memory speed nearly every one does (on a busy machine
/// other programs' data pushes the buffer out of the shared L3); in between nothing is checked. `attempts` are (cycles,
/// fills from beyond the L3) a step, and the fastest decides: Some(passed) or None, and what to print.
fn l3_verdict(attempts: &[(f64, f64)], memory_cycles: f64) -> (Option<bool>, String) {
    let Some(&(cycles, misses)) = attempts.iter().min_by(|a, b| a.0.total_cmp(&b.0)) else { return (None, "not run".into()) };
    let share = cycles / memory_cycles.max(1.0);
    let speed = format!("{cycles:.0} cycles a step, memory {memory_cycles:.0}");
    if share <= 0.3 {
        let passed = misses <= 0.15;
        (Some(passed), format!("{}: L3 speed ({speed}), expected 0..0.15", if passed { "ok" } else { "FAILED" }))
    } else if share >= 0.6 {
        let passed = misses >= 0.6;
        (Some(passed), format!("{}: memory speed ({speed}: the L3 was busy), expected 0.6..1.1", if passed { "ok" } else { "FAILED" }))
    } else {
        (None, format!("not checked: between L3 and memory speed ({speed})"))
    }
}

/// Counts `kernels` with `counters`: each kernel's counts, its reading's own counts taken out
fn measure(endpoint: client::Endpoint, counters: &[&str], kernels: &[Kernel]) -> Result<Vec<readings::Counts>, String> {
    let reader = client::Reader::new()?;
    let mut service = client::Client::connect(endpoint)?;
    let sources: Vec<String> = counters.iter().map(|name| protocol::source_of(name).to_string()).collect();
    let started = service.start(&sources, &[client::current_thread_id()])?;
    for note in &started.notes {
        println!("  note: {note}");
    }
    let mut at = Vec::with_capacity(kernels.len() * 2 + 16);
    // (back-to-back readings: a reading's own counts, taken out of each kernel's)
    for _ in 0..16 {
        at.push(reader.read());
    }
    for kernel in kernels {
        // (once to warm the caches and the branch predictor up to the kernel, then counted)
        (kernel.run)();
        at.push(reader.read());
        (kernel.run)();
        at.push(reader.read());
    }
    drop(reader);
    let results = service.stop()?;
    for note in &results.notes {
        println!("  note: {note}");
    }
    if results.lost_events > 0 || results.lost_buffers > 0 {
        println!("  warning: the session lost {} events and {} buffers", results.lost_events, results.lost_buffers);
    }
    let thread = results.threads.iter().find(|thread| thread.registered).ok_or("the service sent no slices of the probe's thread")?;
    let frequency = started.qpc_frequency.max(1) as i64;
    let split = readings::split(&at, &thread.slices, frequency / 1000);
    if split.matched < at.len() {
        println!("  warning: {} of {} readings found no switch of their own", at.len() - split.matched, at.len());
    }
    // (switches without values matter only where they cost a reading its switch, which the line above says)
    if results.without_counters > 0 {
        println!("  note: {} context switches came without counter values", results.without_counters);
    }
    let own = median_windows(&split.windows[1..16]);
    Ok((0..kernels.len()).map(|index| less(&split.windows[16 + 2 * index + 1], &own)).collect())
}

/// The counter-by-counter median of some windows
fn median_windows(windows: &[readings::Counts]) -> readings::Counts {
    let mut median = [0u64; protocol::MAX_COUNTERS];
    for (counter, value) in median.iter_mut().enumerate() {
        let mut values: Vec<u64> = windows.iter().map(|counts| counts[counter]).collect();
        values.sort_unstable();
        *value = values.get(values.len() / 2).copied().unwrap_or(0);
    }
    median
}

fn less(counts: &readings::Counts, own: &readings::Counts) -> readings::Counts {
    std::array::from_fn(|counter| counts[counter].saturating_sub(own[counter]))
}

/// A check's value from a kernel's counts a unit (None when a counter it needs was not counted)
fn value_of(check: &Check, per: &dyn Fn(&str) -> Option<f64>) -> Option<f64> {
    match *check {
        Check::Per(counter, _, _) => per(counter),
        Check::Ratio(part, whole, _, _, _) => Some(per(part)? / per(whole)?.max(f64::MIN_POSITIVE)),
        Check::LatencyOfUnit(_, _) => Some(per("miss-buffer-occupancy")? / per("miss-buffer-allocs")?.max(f64::MIN_POSITIVE) / per("cycles")?.max(f64::MIN_POSITIVE)),
    }
}

fn bounds(check: &Check) -> (f64, f64) {
    match *check {
        Check::Per(_, low, high) | Check::Ratio(_, _, _, low, high) | Check::LatencyOfUnit(low, high) => (low, high),
    }
}

fn label(check: &Check) -> String {
    match *check {
        Check::Per(counter, _, _) => counter.to_string(),
        Check::Ratio(_, _, label, _, _) => label.to_string(),
        Check::LatencyOfUnit(_, _) => "a miss's latency (occupancy / allocations) over a step's cycles".to_string(),
    }
}

/// The probe's tally
#[derive(Default)]
struct Tally {
    checked: usize,
    failed: usize,
}

impl Tally {
    fn check(&mut self, passed: bool) -> &'static str {
        self.checked += 1;
        self.failed += usize::from(!passed);
        if passed { "ok" } else { "FAILED" }
    }
}

/// Prints a group's kernels, their counts a unit, the figures derived from them and their checks
fn report(counters: &[&str], kernels: &[Kernel], counts: &[readings::Counts], tally: &mut Tally) {
    let names: Vec<String> = counters.iter().map(|name| name.to_string()).collect();
    for (kernel, counts) in kernels.iter().zip(counts) {
        let per = |name: &str| counters.iter().position(|counter| *counter == name).map(|index| counts[index] as f64 / kernel.units as f64);
        println!("  {} ({} {}s)", kernel.name, kernel.units, kernel.unit);
        for (index, counter) in counters.iter().enumerate() {
            let value = counts[index] as f64 / kernel.units as f64;
            let verdict = kernel
                .checks
                .iter()
                .find(|check| matches!(check, Check::Per(name, _, _) if name == counter))
                .map(|check| {
                    let (low, high) = bounds(check);
                    format!("{}, expected {low}..{high}", tally.check((low..=high).contains(&value)))
                })
                .unwrap_or_default();
            println!("    {counter:<24} {:>16} total {value:>12.4} a {}  {verdict}", counts[index], kernel.unit);
        }
        let figures: Vec<counters::Figure> = counters::figures(&names, &counts[..names.len()].iter().map(|count| *count as f64).collect::<Vec<_>>());
        let shown: Vec<String> = figures.iter().filter(|figure| figure.key == "IPC" || figure.cost || figure.key == counters::MISSES_OUTSTANDING).map(|figure| format!("{} {}", figure.key, figure.text())).collect();
        if !shown.is_empty() {
            println!("    = {}", shown.join(", "));
        }
        for check in kernel.checks.iter().filter(|check| !matches!(check, Check::Per(..))) {
            let (low, high) = bounds(check);
            match value_of(check, &per) {
                Some(value) => println!("    {:<60} {value:>10.4}  {}, expected {low}..{high}", label(check), tally.check((low..=high).contains(&value))),
                None => println!("    {:<60} not counted", label(check)),
            }
        }
    }
}

/// Calls of a function that waits on memory (`chase`) between calls of one that computes (`arith`), with readings
/// around one call in `every` of each (a random one of each run of `every` calls, as the VM samples Luau calls): what
/// each function counted while its sampled calls ran, times `every`, and what ran outside them, its estimate (the
/// counts outside less `every - 1` times the sampled calls'), with the readings' own counts taken out
struct Attributed {
    chase: readings::Counts,
    arith: readings::Counts,
    outside: [f64; protocol::MAX_COUNTERS],
    calls: usize,
}

fn attribute(endpoint: client::Endpoint, counters: &[&str], chase_call: &dyn Fn(), arith_call: &dyn Fn(), calls: usize, every: usize) -> Result<Attributed, String> {
    let reader = client::Reader::new()?;
    let mut service = client::Client::connect(endpoint)?;
    let sources: Vec<String> = counters.iter().map(|name| protocol::source_of(name).to_string()).collect();
    let started = service.start(&sources, &[client::current_thread_id()])?;
    let mut at = Vec::with_capacity(4 * calls + 32);
    for _ in 0..16 {
        at.push(reader.read());
    }
    chase_call();
    arith_call();
    at.push(reader.read());
    let first = at.len() - 1;
    // which reading pairs bracket which function's call (each call drawn on its own, a chance of one in `every`, as the
    // engine's VM draws them)
    let mut brackets: Vec<(usize, bool)> = Vec::new();
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    for _ in 0..calls {
        for (function, is_chase) in [(chase_call, true), (arith_call, false)] {
            if (random.next() >> 11).is_multiple_of(every as u64) {
                at.push(reader.read());
                function();
                at.push(reader.read());
                brackets.push((at.len() - 1, is_chase));
            } else {
                function();
            }
        }
    }
    at.push(reader.read());
    let last = at.len() - 1;
    drop(reader);
    let results = service.stop()?;
    let thread = results.threads.iter().find(|thread| thread.registered).ok_or("the service sent no slices of the probe's thread")?;
    let split = readings::split(&at, &thread.slices, started.qpc_frequency.max(1) as i64 / 1000);
    let own = median_windows(&split.windows[1..16]);
    let mut attributed = Attributed { chase: [0; protocol::MAX_COUNTERS], arith: [0; protocol::MAX_COUNTERS], outside: [0.0; protocol::MAX_COUNTERS], calls: brackets.len() };
    let mut inside = vec![false; at.len()];
    for &(leave, is_chase) in &brackets {
        inside[leave] = true;
        readings::add(if is_chase { &mut attributed.chase } else { &mut attributed.arith }, &less(&split.windows[leave], &own));
    }
    for (index, window) in split.windows.iter().enumerate().take(last + 1).skip(first + 1) {
        if !inside[index] {
            for counter in 0..protocol::MAX_COUNTERS {
                attributed.outside[counter] += window[counter] as f64 - own[counter] as f64;
            }
        }
    }
    let scale = every as u64;
    for counter in 0..protocol::MAX_COUNTERS {
        attributed.outside[counter] -= (every as f64 - 1.0) * (attributed.chase[counter] + attributed.arith[counter]) as f64;
        attributed.chase[counter] *= scale;
        attributed.arith[counter] *= scale;
    }
    Ok(attributed)
}

/// Runs the kernels and prints what each counter counted: 0 when every check passed, 1 when one failed, 2 when nothing
/// could be counted
pub fn run(endpoint: client::Endpoint) -> i32 {
    let cpu = unsafe { ffi::GetCurrentProcessorNumber() };
    if cpu < usize::BITS {
        unsafe { ffi::SetThreadAffinityMask(ffi::GetCurrentThread(), 1usize << cpu) };
    }
    let (vendor, family) = register::cpu();
    let custom = register::custom_counters_fit();
    println!("ree-hw-counters probe: {vendor} family {family:#x}, pinned to CPU {cpu}; ree's AMD counters: {}", if custom { "yes" } else { "not defined for this CPU" });
    let line = cycle(64, 64);
    let small = cycle(16 << 10, 16 << 10);
    let l2 = cycle(512 << 10, 512 << 10);
    let l3 = cycle(8 << 20, 8 << 20);
    // (fully random for the fills by source, as the probe always ran it; in 2 MB groups for a miss's latency, where page
    // walks' own miss-buffer entries would dilute it)
    let memory = cycle(256 << 20, 256 << 20);
    let grouped = cycle(256 << 20, 2 << 20);
    // a 256 MB stream of lines, in 2 MB groups of random order and in order
    let stream_lines = (256usize << 20) / 64;
    let stream: Vec<u64> = vec![1u64; stream_lines * 8];
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let random_order = grouped_order(stream_lines, (2 << 20) / 64, &mut random);
    let in_order: Vec<u32> = (0..stream_lines as u32).collect();
    let chase_kernel = |name: &str, buffer: &Vec<u64>, steps: u64, checks: Vec<Check>| {
        let start = buffer.as_ptr() as usize;
        Kernel { name: name.to_string(), unit: "step", units: steps, run: Box::new(move || chase(start as *const u64, steps)), checks }
    };
    let stream_base = stream.as_ptr() as usize;
    fn stream_kernel<'a>(name: &str, base: usize, order: &'a [u32], checks: Vec<Check>) -> Kernel<'a> {
        // (2M lines: 128 MB of the stream, far more than the L3)
        let order: &'a [u32] = &order[..order.len().min(1 << 21)];
        Kernel { name: name.to_string(), unit: "line", units: order.len() as u64, run: Box::new(move || stream8(base as *const u64, order)), checks }
    }
    let line_start = line.as_ptr() as usize;
    let mut first: Vec<&str> = vec!["instructions", "cycles", "branches", "branch-misses"];
    let mut loads_checks = vec![];
    if custom {
        first.extend(["ls-dispatched", "demand-fills"]);
        loads_checks = vec![Check::Per("ls-dispatched", 3.5, 5.0), Check::Per("demand-fills", 0.0, 0.01)];
    }
    let mut first_kernels = vec![
        Kernel { name: "spin".into(), unit: "iteration", units: 50_000_000, run: Box::new(|| spin(50_000_000)), checks: vec![Check::Per("instructions", 3.98, 4.04), Check::Per("branches", 0.99, 1.02), Check::Per("branch-misses", 0.0, 0.001)] },
        Kernel { name: "random branches".into(), unit: "iteration", units: 20_000_000, run: Box::new(|| branches(20_000_000, 1)), checks: vec![Check::Per("branches", 1.98, 2.05), Check::Per("branch-misses", 0.35, 0.65)] },
        Kernel { name: "steady branches".into(), unit: "iteration", units: 20_000_000, run: Box::new(|| branches(20_000_000, 0)), checks: vec![Check::Per("branches", 1.98, 2.05), Check::Per("branch-misses", 0.0, 0.01)] },
        Kernel { name: "L1 loads".into(), unit: "iteration", units: 25_000_000, run: Box::new(move || loads(25_000_000, line_start as *const u64)), checks: loads_checks },
    ];
    if custom {
        // (the loads a line, against the fills a line: one a line however many loads missed it, none for a prefetched line)
        first_kernels.push(stream_kernel(STREAM_RANDOM, stream_base, &random_order, vec![Check::Per("ls-dispatched", 8.5, 10.5), Check::Per("demand-fills", 0.85, 1.2)]));
        first_kernels.push(stream_kernel(STREAM_IN_ORDER, stream_base, &in_order, vec![Check::Per("ls-dispatched", 8.5, 10.5), Check::Per("demand-fills", 0.0, 0.25)]));
    }
    let mut groups: Vec<(&str, Vec<&str>, Vec<Kernel>)> = vec![("the built-in counters, the loads dispatched and the demand fills", first, first_kernels)];
    if custom {
        groups.push((
            "the demand fills by source (one a line)",
            vec!["instructions", "cycles", "demand-fills", "demand-fills-beyond-l2", "demand-fills-beyond-l3", "branch-misses"],
            vec![
                chase_kernel("chase 16 KB (fits L1)", &small, 4_000_000, vec![Check::Per("demand-fills", 0.0, 0.05)]),
                chase_kernel("chase 512 KB (fits L2)", &l2, 4_000_000, vec![Check::Per("demand-fills", 0.7, 1.1), Check::Per("demand-fills-beyond-l2", 0.0, 0.1)]),
                // (three tries: the fills from beyond the L3 are checked on the fastest, against its speed: see l3_verdict)
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![Check::Per("demand-fills-beyond-l2", 0.7, 1.1)]),
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![Check::Per("demand-fills-beyond-l2", 0.7, 1.1)]),
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![Check::Per("demand-fills-beyond-l2", 0.7, 1.1)]),
                chase_kernel(MEMORY_CHASE, &memory, 2_000_000, vec![Check::Per("demand-fills-beyond-l3", 0.6, 1.1)]),
            ],
        ));
        groups.push((
            "what memory costs: cycles stalled on loads, misses outstanding",
            vec!["instructions", "cycles", "load-stall-cycles", "miss-buffer-occupancy", "miss-buffer-allocs", "demand-fills"],
            vec![
                Kernel { name: "arithmetic, no loads".into(), unit: "iteration", units: 50_000_000, run: Box::new(|| arith(50_000_000)), checks: vec![Check::Ratio("load-stall-cycles", "cycles", "load stall share", 0.0, 0.05), Check::Per("miss-buffer-allocs", 0.0, 0.001)] },
                chase_kernel("chase 16 KB (fits L1)", &small, 4_000_000, vec![Check::Per("miss-buffer-allocs", 0.0, 0.05)]),
                chase_kernel("chase 512 KB (fits L2)", &l2, 4_000_000, vec![Check::Per("miss-buffer-allocs", 0.9, 1.1), Check::Ratio("miss-buffer-occupancy", "miss-buffer-allocs", "a miss's latency in cycles (from the L2)", 8.0, 30.0)]),
                chase_kernel(MEMORY_CHASE_GROUPED, &grouped, 2_000_000, vec![Check::Ratio("load-stall-cycles", "cycles", "load stall share", 0.85, 1.0), Check::Per("demand-fills", 0.9, 1.1), Check::LatencyOfUnit(0.6, 1.05), Check::Ratio("cycles", "demand-fills", "cycles a step (memory: hundreds)", 150.0, 5000.0)]),
                stream_kernel(STREAM_RANDOM, stream_base, &random_order, vec![Check::Per("demand-fills", 0.85, 1.2), Check::Per("load-stall-cycles", 10.0, 10_000.0), Check::Ratio("miss-buffer-occupancy", "cycles", "misses outstanding a cycle, over all cycles (above one: they overlap)", 1.5, 64.0)]),
                stream_kernel(STREAM_IN_ORDER, stream_base, &in_order, vec![Check::Per("demand-fills", 0.0, 0.25), Check::Ratio("load-stall-cycles", "cycles", "load stall share", 0.15, 1.0)]),
            ],
        ));
        groups.push((
            "the other stall and miss-buffer counters",
            vec!["cycles", "op-stall-cycles", "no-retire-cycles", "miss-buffer-allocs-ls", "miss-buffer-allocs-prefetch", "ls-dispatched"],
            vec![
                chase_kernel(MEMORY_CHASE_GROUPED, &grouped, 2_000_000, vec![Check::Ratio("op-stall-cycles", "cycles", "op stall share", 0.85, 1.0), Check::Ratio("no-retire-cycles", "cycles", "no-retire share", 0.85, 1.0), Check::Per("miss-buffer-allocs-ls", 0.9, 1.5)]),
                stream_kernel(STREAM_IN_ORDER, stream_base, &in_order, vec![Check::Per("miss-buffer-allocs-prefetch", 0.5, 1.5), Check::Per("miss-buffer-allocs-ls", 0.0, 0.25), Check::Per("ls-dispatched", 8.5, 10.5)]),
            ],
        ));
    }
    let mut tally = Tally::default();
    // each stream's loads, fills, cycles and stalls a line, from whichever groups counted them
    let mut undercount: Vec<(String, [Option<f64>; 4])> = vec![(STREAM_RANDOM.to_string(), [None; 4]), (STREAM_IN_ORDER.to_string(), [None; 4])];
    for (title, counters_wanted, kernels) in &groups {
        println!("\n{title}: {}", counters_wanted.join(", "));
        let counts = match measure(endpoint, counters_wanted, kernels) {
            Ok(counts) => counts,
            Err(error) => {
                println!("  could not count: {error}");
                if counters_wanted.contains(&"instructions") && counters_wanted.contains(&"branches") {
                    return 2;
                }
                // (a service installed before these counters: the others are still checked)
                tally.checked += 1;
                tally.failed += 1;
                continue;
            }
        };
        report(counters_wanted, kernels, &counts, &mut tally);
        // the L3 chase's fills from beyond the L3, against its own speed
        let at = |name: &str| counters_wanted.iter().position(|counter| *counter == name);
        if let (Some(cycles), Some(misses)) = (at("cycles"), at("demand-fills-beyond-l3")) {
            let per = |kernel: &Kernel, counts: &readings::Counts, index: usize| counts[index] as f64 / kernel.units as f64;
            let attempts: Vec<(f64, f64)> = kernels.iter().zip(&counts).filter(|(kernel, _)| kernel.name == L3_ATTEMPT).map(|(kernel, counts)| (per(kernel, counts, cycles), per(kernel, counts, misses))).collect();
            let memory_speed = kernels.iter().zip(&counts).find(|(kernel, _)| kernel.name == MEMORY_CHASE).map(|(kernel, counts)| per(kernel, counts, cycles));
            if let (false, Some(memory_speed)) = (attempts.is_empty(), memory_speed) {
                let (passed, text) = l3_verdict(&attempts, memory_speed);
                let fastest = attempts.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map_or(0.0, |attempt| attempt.1);
                println!("  {L3_ATTEMPT}, the fastest of {}: demand-fills-beyond-l3 {fastest:.4} a step  {text}", attempts.len());
                if let Some(passed) = passed {
                    tally.check(passed);
                }
            }
        }
        // the undercount: loads a line against fills a line, and the cycles stalled
        for (kernel, counts) in kernels.iter().zip(&counts) {
            let Some((_, figures)) = undercount.iter_mut().find(|(name, _)| *name == kernel.name) else { continue };
            for (slot, counter) in ["ls-dispatched", "demand-fills", "cycles", "load-stall-cycles"].iter().enumerate() {
                if let Some(index) = at(counter) {
                    figures[slot].get_or_insert(counts[index] as f64 / kernel.units as f64);
                }
            }
        }
    }
    if undercount.iter().any(|(_, figures)| figures[1].is_some()) {
        println!("\nthe undercount (8 loads to each line, each an L1 miss while the line is in flight: a count of misses per load would read 8 a line):");
        for (name, [loads, fills, cycles, stalls]) in &undercount {
            let shown = |value: &Option<f64>, digits: usize| value.map_or("-".to_string(), |value| format!("{value:.digits$}"));
            let share = match (stalls, cycles) {
                (Some(stalls), Some(cycles)) if *cycles > 0.0 => format!(" ({:.1} % of its cycles)", stalls / cycles * 100.0),
                _ => String::new(),
            };
            println!("  {name}: {} loads and stores dispatched a line, {} demand fills a line, {} cycles a line, {} of them stalled on loads{share}", shown(loads, 2), shown(fills, 3), shown(cycles, 1), shown(stalls, 1));
        }
    }
    if custom {
        attribution_checks(endpoint, &grouped, &mut tally);
    }
    println!("\n{} of {} checks passed", tally.checked - tally.failed, tally.checked);
    if tally.failed > 0 { 1 } else { 0 }
}

/// The readings around calls: a function chasing pointers through memory between calls of one doing arithmetic, every
/// call read around, then each call drawn with a chance of one in four: the stall cycles must land on the chase, the
/// estimate must match. The chase goes on where its last call stopped, so every load misses to memory (from the same
/// start each call would chase the same thousand lines, out of the L2 after the first)
fn attribution_checks(endpoint: client::Endpoint, memory: &[u64], tally: &mut Tally) {
    let counters_wanted = ["instructions", "cycles", "load-stall-cycles", "miss-buffer-occupancy", "miss-buffer-allocs", "demand-fills"];
    println!("\nthe readings around calls: chase (1,000 dependent loads through 256 MB, each call going on where the last stopped) between arith (100,000 multiply-adds), 800 calls each: {}", counters_wanted.join(", "));
    let at = std::cell::Cell::new(memory.as_ptr() as usize);
    let chase_call = || at.set(chase_on(at.get() as *const u64, 1_000) as usize);
    let arith_call = || arith(100_000);
    let index = |name: &str| counters_wanted.iter().position(|counter| *counter == name).unwrap();
    let (cycles, stalls) = (index("cycles"), index("load-stall-cycles"));
    let mut every_call: Option<Attributed> = None;
    for every in [1usize, 4] {
        let attributed = match attribute(endpoint, &counters_wanted, &chase_call, &arith_call, 800, every) {
            Ok(attributed) => attributed,
            Err(error) => {
                println!("  could not count: {error}");
                tally.check(false);
                return;
            }
        };
        let share = |counts: &readings::Counts| counts[stalls] as f64 / (counts[cycles] as f64).max(1.0);
        let both = (attributed.chase[stalls] + attributed.arith[stalls]) as f64;
        let of_both = attributed.chase[stalls] as f64 / both.max(1.0);
        println!(
            "  {} ({} calls read around): chase {} cycles, {} stalled on loads ({:.1} %); arith {} cycles, {} stalled ({:.1} %); outside the calls {:.0} cycles, {:.0} stalled",
            if every == 1 { "every call" } else { "each call drawn, one in four" },
            attributed.calls,
            attributed.chase[cycles],
            attributed.chase[stalls],
            share(&attributed.chase) * 100.0,
            attributed.arith[cycles],
            attributed.arith[stalls],
            share(&attributed.arith) * 100.0,
            attributed.outside[cycles],
            attributed.outside[stalls]
        );
        println!("    the chase's share of the two functions' stall cycles {:.4}  {}, expected 0.9..1", of_both, tally.check(of_both >= 0.9));
        println!("    the chase's load stall share {:.4}  {}, expected 0.8..1", share(&attributed.chase), tally.check(share(&attributed.chase) >= 0.8));
        println!("    arith's load stall share {:.4}  {}, expected 0..0.05", share(&attributed.arith), tally.check(share(&attributed.arith) <= 0.05));
        match &every_call {
            None => every_call = Some(attributed),
            Some(full) => {
                let ratio = attributed.chase[stalls] as f64 / (full.chase[stalls] as f64).max(1.0);
                println!("    one call in four times four over every call, the chase's stall cycles {ratio:.4}  {}, expected 0.8..1.25", tally.check((0.8..=1.25).contains(&ratio)));
            }
        }
    }
}

const L3_ATTEMPT: &str = "chase 8 MB (fits L3)";
const MEMORY_CHASE: &str = "chase 256 MB (memory)";
const MEMORY_CHASE_GROUPED: &str = "chase 256 MB in 2 MB groups (memory, few page walks)";
const STREAM_RANDOM: &str = "8 loads to each line of 256 MB, lines in a random order (2 MB groups)";
const STREAM_IN_ORDER: &str = "8 loads to each line of 256 MB, lines in order (the prefetcher fetches ahead)";

#[cfg(test)]
mod tests {
    use super::*;

    /// The L3 check reads the fills against the chase's own speed: on a busy machine (8 MB at 549 cycles a step, memory
    /// 741: the buffer was pushed out of the L3) nearly every step must fill from beyond the L3, as they did; on a quiet
    /// one few may; a count that disagrees with the speed fails; in between, and with no attempt, nothing is checked; the
    /// fastest try decides
    #[test]
    fn the_l3_check_reads_the_misses_against_the_chases_speed() {
        assert_eq!(l3_verdict(&[(549.0, 0.94)], 741.0).0, Some(true));
        assert_eq!(l3_verdict(&[(549.0, 0.88)], 741.0).0, Some(true));
        assert_eq!(l3_verdict(&[(55.0, 0.02)], 741.0).0, Some(true));
        assert_eq!(l3_verdict(&[(55.0, 0.9)], 741.0).0, Some(false), "L3 speed with misses: the counter is wrong");
        assert_eq!(l3_verdict(&[(600.0, 0.05)], 741.0).0, Some(false), "memory speed without misses: the counter is wrong");
        assert_eq!(l3_verdict(&[(300.0, 0.5)], 741.0).0, None);
        assert_eq!(l3_verdict(&[], 741.0).0, None);
        let (passed, text) = l3_verdict(&[(549.0, 0.94), (60.0, 0.03), (300.0, 0.4)], 741.0);
        assert_eq!(passed, Some(true));
        assert!(text.contains("L3 speed") && text.contains("60 cycles"), "{text}");
    }

    /// The grouped order visits every line once, each group's lines together
    #[test]
    fn the_grouped_order_visits_every_line_once_a_group_at_a_time() {
        let mut random = Random(7);
        // (a last group shorter than the rest: every line still once)
        let mut seen = grouped_order(1000, 64, &mut random);
        seen.sort_unstable();
        assert_eq!(seen, (0..1000).collect::<Vec<u32>>());
        let order = grouped_order(1024, 64, &mut random);
        for chunk in order.chunks(64) {
            let group = chunk[0] / 64;
            assert!(chunk.iter().all(|line| line / 64 == group), "{chunk:?}");
        }
        assert_ne!(order[..64].to_vec(), (0..64).collect::<Vec<u32>>(), "the lines in a random order");
        // the cycle through a buffer is one cycle over every line
        let buffer = cycle(64 * 300, 64 * 32);
        let base = buffer.as_ptr() as u64;
        let mut at = base;
        let mut visited = std::collections::HashSet::new();
        for _ in 0..300 {
            assert!(visited.insert(at));
            at = buffer[((at - base) / 8) as usize];
        }
        assert_eq!(at, base, "back to the start after every line");
    }

    /// The checks read their figures from the counts a unit
    #[test]
    fn checks_read_their_figures() {
        let per = |name: &str| match name {
            "cycles" => Some(400.0),
            "miss-buffer-occupancy" => Some(360.0),
            "miss-buffer-allocs" => Some(1.0),
            "load-stall-cycles" => Some(396.0),
            _ => None,
        };
        assert_eq!(value_of(&Check::LatencyOfUnit(0.6, 1.05), &per), Some(0.9));
        assert_eq!(value_of(&Check::Ratio("load-stall-cycles", "cycles", "share", 0.85, 1.0), &per), Some(0.99));
        assert_eq!(value_of(&Check::Per("demand-fills", 0.9, 1.1), &per), None);
    }
}

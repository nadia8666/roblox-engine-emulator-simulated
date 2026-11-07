//! `ree-hw-counters probe`: checks that each counter counts what its name says, through the service, on kernels whose
//! counts are known: a loop of four instructions and one branch an iteration; random branches, half of which mispredict,
//! beside the same loop always taken; loads that hit the L1 data cache; and chases through a random cycle of cache lines
//! in buffers that fit the L1, the L2, the L3, and none (each step a load that waits on the one before). The thread is
//! pinned to one CPU, and each kernel runs between two readings, so its counts are exactly its own.

use std::arch::asm;

use crate::{client, ffi, protocol, readings, register};

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

/// A buffer of `bytes` whose cache lines point at each other in one random cycle (Sattolo's shuffle)
fn cycle(bytes: usize) -> Vec<u64> {
    let lines = (bytes / 64).max(2);
    let mut buffer = vec![0u64; lines * 8];
    let mut order: Vec<usize> = (0..lines).collect();
    let mut random = 0x2545_F491_4F6C_DD1Du64;
    for index in (1..lines).rev() {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        order.swap(index, (random % index as u64) as usize);
    }
    let base = buffer.as_mut_ptr();
    for position in 0..lines {
        let (from, to) = (order[position], order[(position + 1) % lines]);
        unsafe { *base.add(from * 8) = base.add(to * 8) as u64 };
    }
    buffer
}

struct Kernel<'a> {
    name: String,
    /// What the per-unit figures count (an iteration, a load, a step)
    unit: &'static str,
    units: u64,
    run: Box<dyn Fn() + 'a>,
    /// (counter, lowest, highest per unit)
    checks: Vec<(&'static str, f64, f64)>,
}

/// What the L3 chase's misses must read, from where its steps were served: its own speed beside the memory chase's. At
/// L3 speed few steps miss the last-level cache; at memory speed nearly every one does (on a busy machine other
/// programs' data pushes the buffer out of the shared L3); in between nothing is checked. `attempts` are (cycles,
/// llc-misses) a step, and the fastest decides: Some(passed) or None, and what to print.
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

/// Counts `kernels` with `counters`: each kernel's counts
fn measure(endpoint: client::Endpoint, counters: &[&str], kernels: &[Kernel]) -> Result<Vec<readings::Counts>, String> {
    let reader = client::Reader::new()?;
    let mut service = client::Client::connect(endpoint)?;
    let sources: Vec<String> = counters.iter().map(|name| protocol::source_of(name).to_string()).collect();
    let started = service.start(&sources, &[client::current_thread_id()])?;
    for note in &started.notes {
        println!("  note: {note}");
    }
    let mut at = Vec::with_capacity(kernels.len() * 2);
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
    Ok((0..kernels.len()).map(|index| split.windows[2 * index + 1]).collect())
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
    println!("ree-hw-counters probe: {vendor} family {family:#x}, pinned to CPU {cpu}; custom cache counters: {}", if custom { "yes" } else { "not defined for this CPU" });
    let line = cycle(64);
    let small = cycle(16 << 10);
    let l2 = cycle(512 << 10);
    let l3 = cycle(8 << 20);
    let memory = cycle(256 << 20);
    let chase_kernel = |name: &str, buffer: &Vec<u64>, steps: u64, checks: Vec<(&'static str, f64, f64)>| {
        let start = buffer.as_ptr() as usize;
        Kernel { name: name.to_string(), unit: "step", units: steps, run: Box::new(move || chase(start as *const u64, steps)), checks }
    };
    let mut first: Vec<&str> = vec!["instructions", "cycles", "branches", "branch-misses"];
    if custom {
        first.extend(["l1d-accesses", "l1d-misses"]);
    }
    let mut loads_checks = vec![];
    if custom {
        loads_checks = vec![("l1d-accesses", 3.5, 5.0), ("l1d-misses", 0.0, 0.01)];
    }
    let line_start = line.as_ptr() as usize;
    let groups: Vec<(Vec<&str>, Vec<Kernel>)> = vec![
        (
            first,
            vec![
                Kernel { name: "spin".into(), unit: "iteration", units: 50_000_000, run: Box::new(|| spin(50_000_000)), checks: vec![("instructions", 3.98, 4.04), ("branches", 0.99, 1.02), ("branch-misses", 0.0, 0.001)] },
                Kernel { name: "random branches".into(), unit: "iteration", units: 20_000_000, run: Box::new(|| branches(20_000_000, 1)), checks: vec![("branches", 1.98, 2.05), ("branch-misses", 0.35, 0.65)] },
                Kernel { name: "steady branches".into(), unit: "iteration", units: 20_000_000, run: Box::new(|| branches(20_000_000, 0)), checks: vec![("branches", 1.98, 2.05), ("branch-misses", 0.0, 0.01)] },
                Kernel { name: "L1 loads".into(), unit: "iteration", units: 25_000_000, run: Box::new(move || loads(25_000_000, line_start as *const u64)), checks: loads_checks },
            ],
        ),
    ];
    let mut groups = groups;
    if custom {
        groups.push((
            vec!["instructions", "cycles", "l1d-misses", "l2-misses", "llc-misses", "branch-misses"],
            vec![
                chase_kernel("chase 16 KB (fits L1)", &small, 4_000_000, vec![("l1d-misses", 0.0, 0.05)]),
                chase_kernel("chase 512 KB (fits L2)", &l2, 4_000_000, vec![("l1d-misses", 0.7, 1.1), ("l2-misses", 0.0, 0.1)]),
                // (three tries: the last-level misses are checked on the fastest, against its speed: see l3_verdict)
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![("l2-misses", 0.7, 1.1)]),
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![("l2-misses", 0.7, 1.1)]),
                chase_kernel(L3_ATTEMPT, &l3, 2_000_000, vec![("l2-misses", 0.7, 1.1)]),
                chase_kernel(MEMORY_CHASE, &memory, 2_000_000, vec![("llc-misses", 0.6, 1.1)]),
            ],
        ));
    }
    let (mut checked, mut failed) = (0, 0);
    for (counters, kernels) in &groups {
        println!("\ncounters: {}", counters.join(", "));
        let counts = match measure(endpoint, counters, kernels) {
            Ok(counts) => counts,
            Err(error) => {
                println!("  could not count: {error}");
                return 2;
            }
        };
        for (kernel, counts) in kernels.iter().zip(&counts) {
            let per = |name: &str| counters.iter().position(|counter| *counter == name).map(|index| counts[index] as f64 / kernel.units as f64);
            let ipc = match (per("instructions"), per("cycles")) {
                (Some(instructions), Some(cycles)) if cycles > 0.0 => format!(", IPC {:.2}", instructions / cycles),
                _ => String::new(),
            };
            println!("  {} ({} {}s{ipc})", kernel.name, kernel.units, kernel.unit);
            for (index, counter) in counters.iter().enumerate() {
                let value = counts[index] as f64 / kernel.units as f64;
                let check = kernel.checks.iter().find(|check| check.0 == *counter);
                let verdict = match check {
                    Some(&(_, low, high)) => {
                        checked += 1;
                        if (low..=high).contains(&value) {
                            format!("ok, expected {low}..{high}")
                        } else {
                            failed += 1;
                            format!("FAILED, expected {low}..{high}")
                        }
                    }
                    None => String::new(),
                };
                println!("    {counter:<14} {:>16} total {value:>10.4} a {}  {verdict}", counts[index], kernel.unit);
            }
        }
        // the L3 chase's last-level misses, against its own speed
        let at = |name: &str| counters.iter().position(|counter| *counter == name);
        if let (Some(cycles), Some(misses)) = (at("cycles"), at("llc-misses")) {
            let per = |kernel: &Kernel, counts: &readings::Counts, index: usize| counts[index] as f64 / kernel.units as f64;
            let attempts: Vec<(f64, f64)> = kernels.iter().zip(&counts).filter(|(kernel, _)| kernel.name == L3_ATTEMPT).map(|(kernel, counts)| (per(kernel, counts, cycles), per(kernel, counts, misses))).collect();
            let memory = kernels.iter().zip(&counts).find(|(kernel, _)| kernel.name == MEMORY_CHASE).map(|(kernel, counts)| per(kernel, counts, cycles));
            if let (false, Some(memory)) = (attempts.is_empty(), memory) {
                let (passed, text) = l3_verdict(&attempts, memory);
                let fastest = attempts.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map_or(0.0, |attempt| attempt.1);
                println!("  {L3_ATTEMPT}, the fastest of {}: llc-misses {fastest:.4} a step  {text}", attempts.len());
                if let Some(passed) = passed {
                    checked += 1;
                    failed += usize::from(!passed);
                }
            }
        }
    }
    println!("\n{} of {checked} checks passed", checked - failed);
    if failed > 0 { 1 } else { 0 }
}

const L3_ATTEMPT: &str = "chase 8 MB (fits L3)";
const MEMORY_CHASE: &str = "chase 256 MB (memory)";

#[cfg(test)]
mod tests {
    use super::l3_verdict;

    /// The L3 check reads the misses against the chase's own speed: on a busy machine (8 MB at 549 cycles a step, memory
    /// 741: the buffer was pushed out of the L3) nearly every step must miss, as they did; on a quiet one few may; a count
    /// that disagrees with the speed fails; in between, and with no attempt, nothing is checked; the fastest try decides
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
}

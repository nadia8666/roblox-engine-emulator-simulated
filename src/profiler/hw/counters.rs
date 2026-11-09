//! The counters a hardware-counter profile can read, by name, and the figures derived from their counts. Shared by the
//! engine (`--profile hw`, [`super`]), the counter service (tools/hw-counters registers the AMD events below) and the
//! profile viewer (profile-view reads profiles with these names, older names included), so it uses nothing but std.
//!
//! The question the counters answer is how much the code waited on memory. Zen 5 has no counter of L1 data cache misses
//! per access: its miss buffer (MAB) and its fills count a line once, however many loads missed it, and a line a
//! prefetch was already fetching not at all. What it does count is the cost: the cycles nothing retired because the
//! oldest op waited for load data, and how many misses were outstanding each cycle. Those, with the cycles and
//! instructions, are the default set; the fills by source stay, under names that say they count lines.

/// An AMD event the CPU's built-in list lacks, which the service registers with Windows (the event select and unit mask
/// of AMD's Processor Programming Reference for Family 1Ah, as Linux perf's amdzen5 tables give them)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Custom {
    pub event: u16,
    pub unit: u8,
}

/// A counter a profile names: the Windows profile source it reads, what it counts, and the vendor's own name for it
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub name: &'static str,
    pub source: &'static str,
    pub about: &'static str,
    /// AMD's (and Linux perf's) name of the event, for the custom ones
    pub amd: &'static str,
    pub custom: Option<Custom>,
}

/// The counters known by name. Any other name is taken for a Windows profile source as `wpr -pmcsources` lists it.
///
/// The custom counters' Windows profile sources keep the names the first service registered them under (`ReeL1dMisses`
/// is the demand fills), so a service installed before the rename still counts them.
pub const KNOWN: [Known; 15] = [
    Known { name: "instructions", source: "InstructionRetired", about: "instructions retired", amd: "", custom: None },
    Known { name: "cycles", source: "TotalCycles", about: "core clock cycles while the thread ran (not halted)", amd: "", custom: None },
    Known {
        name: "load-stall-cycles",
        source: "ReeLoadStallCycles",
        about: "cycles nothing retired because the oldest op waited for load data (measured on a Ryzen 9 9950X: a chain of L1 hits counted 1.2 of each load's 4 cycles, a chase to memory 99 % of its wait)",
        amd: "ex_no_retire.load_not_complete",
        custom: Some(Custom { event: 0xD6, unit: 0xA2 }),
    },
    Known {
        name: "op-stall-cycles",
        source: "ReeOpStallCycles",
        about: "cycles nothing retired while the oldest op had not completed, as AMD counts it: every long wait, load stalls included, but short waits only in part (measured on a Ryzen 9 9950X: a chain of dependent multiplies counted 0.9 of each multiply's 3 cycles here and 1.5 under ex_no_retire.other; a chain of L1 hits 1.2 of each load's 4 cycles here and 1.9 under other; a chase to memory 99 % of its wait here)",
        amd: "ex_no_retire.not_complete",
        custom: Some(Custom { event: 0xD6, unit: 0x02 }),
    },
    Known { name: "no-retire-cycles", source: "ReeNoRetireCycles", about: "cycles nothing retired, for any reason", amd: "ex_no_retire.all", custom: Some(Custom { event: 0xD6, unit: 0x1B }) },
    Known {
        name: "miss-buffer-occupancy",
        source: "ReeMabOccupancy",
        about: "L1 data cache misses outstanding, summed every cycle: the miss buffer's entries in use (load and store misses, the hardware prefetcher's, page walks')",
        amd: "ls_alloc_mab_count",
        custom: Some(Custom { event: 0x5F, unit: 0x00 }),
    },
    Known {
        name: "miss-buffer-allocs",
        source: "ReeMabAllocs",
        about: "miss buffer entries allocated: one a line the L1 data cache had to fetch (loads and stores that miss a line already outstanding take none), the hardware prefetcher's and page walks' included",
        amd: "ls_mab_alloc.all_allocations",
        custom: Some(Custom { event: 0x41, unit: 0x0F }),
    },
    Known {
        name: "miss-buffer-allocs-ls",
        source: "ReeMabAllocsLs",
        about: "of those, the entries the load-store pipes allocated for loads and stores (page walks' requests are among them)",
        amd: "ls_mab_alloc.load_store_allocations",
        custom: Some(Custom { event: 0x41, unit: 0x07 }),
    },
    Known {
        name: "demand-fills",
        source: "ReeL1dMisses",
        about: "lines filled into the L1 data cache for demand loads and stores, from any source: one a line, so loads that miss a line already outstanding are not counted again, and a line a prefetch brought is not counted",
        amd: "ls_dmnd_fills_from_sys.all",
        custom: Some(Custom { event: 0x43, unit: 0xFF }),
    },
    Known {
        name: "demand-fills-beyond-l2",
        source: "ReeL2Misses",
        about: "of those, lines from beyond the core's L2 (the L3, another core's cache, memory)",
        amd: "ls_dmnd_fills_from_sys, every source but local_l2",
        custom: Some(Custom { event: 0x43, unit: 0xFE }),
    },
    Known {
        name: "demand-fills-beyond-l3",
        source: "ReeLlcMisses",
        about: "of those, lines from beyond the core complex's L3 (another complex's cache, memory)",
        amd: "ls_dmnd_fills_from_sys near_cache, dram_io_near, far_cache, dram_io_far, alternate_memories",
        custom: Some(Custom { event: 0x43, unit: 0xDC }),
    },
    Known {
        name: "ls-dispatched",
        source: "ReeL1dAccesses",
        about: "load and store ops dispatched to the load-store unit (some of them later discarded)",
        amd: "ls_dispatch.all",
        custom: Some(Custom { event: 0x29, unit: 0x07 }),
    },
    Known { name: "branches", source: "BranchInstructions", about: "branches retired", amd: "", custom: None },
    Known { name: "branch-misses", source: "BranchMispredictions", about: "branches retired that were mispredicted", amd: "", custom: None },
    Known {
        name: "miss-buffer-allocs-prefetch",
        source: "ReeMabAllocsPrefetch",
        about: "miss buffer entries the hardware prefetcher allocated (lines fetched ahead of the loads that use them)",
        amd: "ls_mab_alloc.hardware_prefetcher_allocations",
        custom: Some(Custom { event: 0x41, unit: 0x08 }),
    },
];

/// The counters a profile reads when it names none (the CPU counts six at once): instructions and cycles; the cycles
/// lost waiting for load data, the measure of how much the code waits on memory; the misses outstanding and allocated,
/// whose ratio is how long a miss takes (so where it was served from); and branch mispredictions, the other common way
/// an interpreter loses cycles, which none of the others shows
pub const DEFAULT_COUNTERS: [&str; 6] = ["instructions", "cycles", "load-stall-cycles", "miss-buffer-occupancy", "miss-buffer-allocs", "branch-misses"];

/// Names profiles used before the counters were named for what they count, and the names they have now: a profile.json
/// with them still opens, and `--profile-counters` still takes them
pub const OLD_NAMES: [(&str, &str); 4] = [("l1d-misses", "demand-fills"), ("l2-misses", "demand-fills-beyond-l2"), ("llc-misses", "demand-fills-beyond-l3"), ("l1d-accesses", "ls-dispatched")];

/// A counter's name now (an old name's new one; any other name as it is)
pub fn canonical(name: &str) -> &str {
    OLD_NAMES.iter().find(|(old, _)| *old == name).map_or(name, |(_, new)| new)
}

pub fn known(name: &str) -> Option<&'static Known> {
    let name = canonical(name);
    KNOWN.iter().find(|known| known.name == name)
}

/// The Windows profile source a counter's name reads
pub fn source_of(name: &str) -> &str {
    known(name).map_or(name, |known| known.source)
}

/// The name a profile shows for a Windows profile source (the known name, else the source's own)
pub fn name_of(source: &str) -> &str {
    KNOWN.iter().find(|known| known.source == source).map_or(source, |known| known.name)
}

/// How a derived figure prints
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// A part of a whole: `42.0 %`
    Share,
    /// A ratio: `1.23`
    Ratio,
    /// Cycles: `412`
    Cycles,
    /// A count per thousand instructions: `3.21`
    PerThousand,
}

/// A figure derived from a set of counts
#[derive(Clone, Debug, PartialEq)]
pub struct Figure {
    /// profile.json's key (`rates`) and the label it prints with
    pub key: &'static str,
    pub value: f64,
    pub shown: Shown,
    /// What it is, one line
    pub about: &'static str,
    /// Whether it measures what the code paid (cycles lost), rather than a count of events
    pub cost: bool,
}

impl Figure {
    pub fn text(&self) -> String {
        match self.shown {
            Shown::Share => format!("{:.1} %", self.value * 100.0),
            Shown::Ratio => format!("{:.2}", self.value),
            Shown::Cycles => format!("{:.0}", self.value),
            Shown::PerThousand => format!("{:.2}", self.value),
        }
    }
}

/// profile.json's keys of the derived figures that measure what memory cost the code
pub const LOAD_STALL_SHARE: &str = "load stall share";
pub const STALL_PER_THOUSAND: &str = "load stall cycles per 1000 instructions";
pub const MISS_LATENCY: &str = "L1D miss latency";
pub const MISSES_OUTSTANDING: &str = "L1D misses outstanding per cycle";

/// The figures a set of counts allows (named by `names`, old names too): IPC; what memory cost (the share of cycles
/// stalled on loads, those cycles per thousand instructions) and where the misses went (how long a miss took, how many
/// were outstanding on average); the stalls
/// apart (all, an op not finished, the memory share of those); and the event rates (miss buffers, fills, branch misses
/// per thousand instructions, the fills' sources). A figure needs both its counts, and none is given over a zero.
pub fn figures(names: &[String], counts: &[f64]) -> Vec<Figure> {
    let get = |name: &str| names.iter().position(|known| canonical(known) == name).and_then(|index| counts.get(index).copied());
    let mut out = Vec::new();
    let mut ratio = |key: &'static str, part: Option<f64>, whole: Option<f64>, scale: f64, shown: Shown, about: &'static str, cost: bool| {
        if let (Some(part), Some(whole)) = (part, whole)
            && whole > 0.0
        {
            out.push(Figure { key, value: part / whole * scale, shown, about, cost });
        }
    };
    let (instructions, cycles) = (get("instructions"), get("cycles"));
    let (stalls, occupancy, allocs) = (get("load-stall-cycles"), get("miss-buffer-occupancy"), get("miss-buffer-allocs"));
    ratio("IPC", instructions, cycles, 1.0, Shown::Ratio, "instructions retired a cycle", false);
    ratio(LOAD_STALL_SHARE, stalls, cycles, 1.0, Shown::Share, "the share of cycles nothing retired because the oldest op waited for load data", true);
    ratio(STALL_PER_THOUSAND, stalls, instructions, 1000.0, Shown::PerThousand, "cycles stalled on loads per thousand instructions retired", true);
    // (where the misses were served from, not what they cost: the stall cycles are the cost)
    ratio(MISS_LATENCY, occupancy, allocs, 1.0, Shown::Cycles, "cycles an L1 data cache miss stayed outstanding on average (misses outstanding summed a cycle, over the misses allocated): about 14 from the L2, 50 to 70 from the L3, hundreds from memory", false);
    ratio(MISSES_OUTSTANDING, occupancy, cycles, 1.0, Shown::Ratio, "L1 data cache misses outstanding, averaged over every cycle (cycles with none count as zero): how busy the miss buffer was, not how many misses overlapped while any was outstanding", false);
    ratio("no-retire share", get("no-retire-cycles"), cycles, 1.0, Shown::Share, "the share of cycles nothing retired, for any reason", true);
    ratio("op stall share", get("op-stall-cycles"), cycles, 1.0, Shown::Share, "the share of cycles nothing retired while the oldest op had not completed, as AMD counts it (long waits all, short ones in part: op-stall-cycles)", true);
    ratio("memory share of op stalls", stalls, get("op-stall-cycles"), 1.0, Shown::Share, "of the cycles the oldest op had not completed, those it waited for load data (Linux perf's backend_bound_memory split)", true);
    ratio("miss buffers per 1000 instructions", allocs, instructions, 1000.0, Shown::PerThousand, "lines the L1 data cache fetched (prefetches and page walks included) per thousand instructions", false);
    ratio("load-store miss buffers per 1000 instructions", get("miss-buffer-allocs-ls"), instructions, 1000.0, Shown::PerThousand, "lines loads and stores missed per thousand instructions (a line once, however many missed it)", false);
    ratio("prefetch share of miss buffers", get("miss-buffer-allocs-prefetch"), allocs, 1.0, Shown::Share, "the share of miss buffer entries the hardware prefetcher allocated", false);
    ratio("demand fills per 1000 instructions", get("demand-fills"), instructions, 1000.0, Shown::PerThousand, "lines filled for demand loads and stores per thousand instructions (a line once; prefetched lines not at all)", false);
    ratio("demand fills per load or store", get("demand-fills"), get("ls-dispatched"), 1.0, Shown::Share, "demand fills over load and store ops dispatched: how few lines the loads and stores needed fetched, not how many of them missed", false);
    ratio("demand fills beyond L2, share", get("demand-fills-beyond-l2"), get("demand-fills"), 1.0, Shown::Share, "the share of demand fills that came from beyond the core's L2", false);
    ratio("demand fills beyond L3, share", get("demand-fills-beyond-l3"), get("demand-fills-beyond-l2"), 1.0, Shown::Share, "the share of fills from beyond the L2 that came from beyond the core complex's L3", false);
    ratio("branch misses per 1000 instructions", get("branch-misses"), instructions, 1000.0, Shown::PerThousand, "mispredicted branches per thousand instructions", false);
    ratio("branch miss rate", get("branch-misses"), get("branches"), 1.0, Shown::Share, "the share of branches mispredicted", false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// The fill counters are named for what they count, the old names still read them, and every default is known
    #[test]
    fn names_say_what_they_count_and_old_names_still_read() {
        assert_eq!(canonical("l1d-misses"), "demand-fills");
        assert_eq!(canonical("llc-misses"), "demand-fills-beyond-l3");
        assert_eq!(canonical("cycles"), "cycles");
        assert_eq!(source_of("l2-misses"), "ReeL2Misses", "an old name reads the source the service registered");
        assert_eq!(source_of("demand-fills-beyond-l2"), "ReeL2Misses");
        assert_eq!(name_of("ReeL1dMisses"), "demand-fills");
        assert_eq!(source_of("IcacheMisses"), "IcacheMisses", "any other name is a profile source of its own");
        assert!(DEFAULT_COUNTERS.iter().all(|name| known(name).is_some()));
        assert!(DEFAULT_COUNTERS.contains(&"load-stall-cycles"));
        // (no name a reader could take for a per-access miss count)
        assert!(KNOWN.iter().all(|known| !known.name.ends_with("-misses") || known.name == "branch-misses"));
        // every custom source is registered once, with a distinct event
        for (index, a) in KNOWN.iter().enumerate() {
            for b in &KNOWN[index + 1..] {
                assert_ne!(a.name, b.name);
                assert_ne!(a.source, b.source);
                assert!(a.custom.is_none() || a.custom != b.custom, "{} and {}", a.name, b.name);
            }
        }
    }

    /// The events are AMD's, as Linux perf's amdzen5 tables give them (execution.json, load-store.json)
    #[test]
    fn the_events_are_amds() {
        let event = |name: &str| known(name).and_then(|known| known.custom).map(|custom| (custom.event, custom.unit));
        assert_eq!(event("load-stall-cycles"), Some((0xD6, 0xA2)));
        assert_eq!(event("op-stall-cycles"), Some((0xD6, 0x02)));
        assert_eq!(event("no-retire-cycles"), Some((0xD6, 0x1B)));
        assert_eq!(event("miss-buffer-occupancy"), Some((0x5F, 0x00)));
        assert_eq!(event("miss-buffer-allocs"), Some((0x41, 0x0F)));
        assert_eq!(event("miss-buffer-allocs-ls"), Some((0x41, 0x07)));
        assert_eq!(event("miss-buffer-allocs-prefetch"), Some((0x41, 0x08)));
        assert_eq!(event("demand-fills"), Some((0x43, 0xFF)));
        assert_eq!(event("demand-fills-beyond-l2"), Some((0x43, 0xFE)));
        assert_eq!(event("demand-fills-beyond-l3"), Some((0x43, 0xDC)));
        assert_eq!(event("ls-dispatched"), Some((0x29, 0x07)));
    }

    /// The cost figures: the share of cycles stalled on loads, those cycles per thousand instructions; a miss's latency
    /// and the misses outstanding (not costs); each needs both its counts and says nothing over a zero
    #[test]
    fn the_cost_figures_come_from_their_counts() {
        let names = named(&DEFAULT_COUNTERS);
        // a memory-bound stretch: 2,000 instructions in 10,000 cycles, 6,000 of them stalled on loads, 20 misses each
        // outstanding 400 cycles (8,000 summed), 30 branch misses
        let counts = [2000.0, 10000.0, 6000.0, 8000.0, 20.0, 30.0];
        let all = figures(&names, &counts);
        let get = |key: &str| all.iter().find(|figure| figure.key == key).map(|figure| figure.value);
        assert_eq!(get("IPC"), Some(0.2));
        assert_eq!(get(LOAD_STALL_SHARE), Some(0.6));
        assert_eq!(get(STALL_PER_THOUSAND), Some(3000.0));
        assert_eq!(get(MISS_LATENCY), Some(400.0));
        assert_eq!(get(MISSES_OUTSTANDING), Some(0.8));
        let cost = |key: &str| all.iter().find(|figure| figure.key == key).map(|figure| figure.cost);
        assert_eq!((cost(LOAD_STALL_SHARE), cost(STALL_PER_THOUSAND)), (Some(true), Some(true)), "the stall cycles are what memory cost");
        assert_eq!((cost(MISS_LATENCY), cost(MISSES_OUTSTANDING)), (Some(false), Some(false)), "a miss's latency says where it was served from, not what it cost");
        assert_eq!(get("branch misses per 1000 instructions"), Some(15.0));
        assert_eq!(get("miss buffers per 1000 instructions"), Some(10.0));
        assert!(all.iter().find(|figure| figure.key == LOAD_STALL_SHARE).is_some_and(|figure| figure.cost && figure.text() == "60.0 %"));
        assert_eq!(get("branch miss rate"), None, "no branches counted");
        assert!(figures(&names, &[0.0; 6]).is_empty(), "nothing over a zero");
    }

    /// A profile from before the rename still gets its figures: the fills by their old names
    #[test]
    fn old_profiles_get_the_fill_figures() {
        let names = named(&["instructions", "cycles", "l1d-misses", "l2-misses", "llc-misses", "branch-misses"]);
        let all = figures(&names, &[3000.0, 1000.0, 30.0, 3.0, 1.0, 6.0]);
        let get = |key: &str| all.iter().find(|figure| figure.key == key).map(|figure| figure.value);
        assert_eq!(get("IPC"), Some(3.0));
        assert_eq!(get("demand fills per 1000 instructions"), Some(10.0));
        assert_eq!(get("demand fills beyond L2, share"), Some(0.1));
        assert_eq!(get(LOAD_STALL_SHARE), None, "no stall counts in an old profile");
    }
}

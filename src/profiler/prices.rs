//! What an inline cache's outcomes cost over a hit (`--profile caches`), apart by whether the work clock charges them.
//!
//! - Charged: the work clock counts the outcome's work and charges it at the instruction, as part of its function's work: a
//!   rehash (its keys and the new storage), an import looked up each time, a value another type's metamethod serves (the
//!   engine's price). A profile reports that charge as the site's charged work (`charged_work_us`, the VM's booking at the
//!   instruction past its dispatch), never as work on top. So is a declined builtin call (the report reads its charge off the
//!   fallback's instructions and the C function it reached).
//! - Uncharged: the interpreter does more than a hit, and no counter sees it: a key found at another slot (repredicted), an
//!   absent key, a new key a table had room for, `__index` tables walked, an `__index` function's call (its own instructions
//!   are charged where they run). Those are priced here: a ratio measured on ree's interpreter (profiler_prices_tests.rs's
//!   calibrate_prices, run by hand: medians across several processes, each the median of interleaved rounds) times the work
//!   clock's price for the base opcode (cost::INTERPRETED_OPS: a global read's for a global, a keyed read's for a table key and
//!   a method's lookup, as the work clock puts a method call's lookup into its CALL). A profile reports them as work beyond the
//!   clock's (`extra_work_us`).
//!
//! A price whose spread (the least and greatest of every session's processes) reaches zero is within the noise: shown so, and left
//! out of every sum.

use serde_json::{Value as Json, json};

use super::vm;

/// The opcodes the prices are based on (Luau's Bytecode.h numbers; profiler_vm_tests checks them against vm::OPCODES)
pub const OP_LOADN: usize = 4;
pub const OP_GETGLOBAL: usize = 7;
pub const OP_GETIMPORT: usize = 12;
pub const OP_GETTABLEKS: usize = 15;
pub const OP_RETURN: usize = 22;

/// A measured ratio: an outcome's extra over its hit, as a multiple of its base's own cost (the median of every calibration
/// session's processes' medians, and the least and greatest of them)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ratio {
    pub median: f64,
    pub low: f64,
    pub high: f64,
}

const fn r(median: f64, low: f64, high: f64) -> Ratio {
    Ratio { median, low, high }
}

/// Where and how the ratios were measured
pub const PROVENANCE: [(&str, &str); 6] = [
    ("cpu", "AMD Ryzen 9 9950X 16-Core Processor"),
    ("date", "2026-11-08"),
    ("sessions", "4 at different times (12:47, 12:55, 13:35 and 14:10 local time), each 7 processes one after another; the machine ran other work meanwhile, at 30-60 % of its CPU"),
    ("rounds", "21 in each process, each the fastest of 3 runs of about 2 ms"),
    (
        "method",
        "ree's interpreter (no native code), a module's environment (safe), the engine's compiler settings, values passed as arguments; in each process every round times each outcome against its hit and the base it divides by, in a rotated order, each the fastest of 3 runs, and keeps the rounds' median; the price is the median of every session's processes' medians, its spread the least and greatest of them",
    ),
    ("bases", "global read 2.174 ns (2.059-3.008); key read 2.645 ns (2.565-3.514); key store 2.813 ns (2.680-3.329)"),
];

/// The calibration's cases, as calibrate_prices names them: an outcome's ratio to its base (a slot read's or store's own cost,
/// a global read's)
const MEASURED: &[(&str, Ratio)] = &[
    ("table key repredicted", r(1.045, 0.909, 1.178)),
    ("table key absent", r(0.872, 0.741, 1.013)),
    ("table key inserted", r(1.523, 1.087, 2.392)),
    ("table key metatable_function", r(9.409, 8.987, 17.987)),
    ("global slot metatable 1", r(5.233, 4.768, 5.736)),
    ("global slot absent", r(2.296, 1.359, 2.469)),
    ("method slot repredicted", r(2.786, 2.360, 3.151)),
    ("method slot metatable_function", r(9.250, 7.851, 9.963)),
    ("table key metatable 1", r(4.060, 3.880, 4.520)),
    ("table key metatable 2", r(6.262, 5.966, 6.658)),
    ("method slot metatable 2", r(5.439, 4.966, 6.001)),
    ("table key metatable 3", r(8.314, 8.024, 8.694)),
    ("method slot metatable 3", r(7.752, 6.554, 8.537)),
    ("table key metatable 4", r(10.512, 9.655, 11.215)),
    ("method slot metatable 4", r(9.661, 9.124, 10.247)),
];

/// The base opcode each cache kind is priced by, in vm::CACHE_KINDS' order: GETGLOBAL, GETIMPORT, GETTABLEKS for a method's
/// lookup, GETTABLEKS
pub const BASE_OPS: [usize; 4] = [OP_GETGLOBAL, OP_GETIMPORT, OP_GETTABLEKS, OP_GETTABLEKS];

/// How an outcome is priced
#[derive(Clone, Copy, Debug)]
pub enum Pricing {
    /// Uncharged: a ratio the calibration measured for this kind's outcome (its case's name)
    Measured(&'static str),
    /// Uncharged: the same work measured on another kind (the table key's case)
    Borrowed(&'static str),
    /// Uncharged: an `__index` function's call, less what the work clock charges the calibration's function's own
    /// instructions (`return 1`: a LOADN and a RETURN), which it charges where they run
    CallOnly(&'static str),
    /// Charged by the work clock at the instruction, and why
    Charged(&'static str),
    /// Not priced, and why
    Unpriced(&'static str),
}

fn measured(name: &str) -> Option<Ratio> {
    MEASURED.iter().find(|(case, _)| *case == name).map(|(_, ratio)| *ratio)
}

const REHASH: Pricing = Pricing::Charged("a rehash: the work clock charges the keys it moves and the new storage, at the instruction");
const LOOKED_UP: Pricing = Pricing::Charged("an import looked up each time: the work clock charges it (cost::LUAU_SLOW_IMPORT), at the instruction");
const ENGINE: Pricing = Pricing::Charged("the value's own metamethod serves it (an Instance's or a datatype's member): the engine's price, which the work clock charges at the instruction");
const ERROR: Pricing = Pricing::Unpriced("the access raises an error");
const NONE: Pricing = Pricing::Unpriced("not an outcome of this kind");

/// How kind `kind`'s (vm::CACHE_KINDS index) outcome `outcome` is priced; a metatable walk by its depth ([`metatable_pricing`])
pub fn pricing(kind: usize, outcome: usize) -> Pricing {
    match (kind, outcome) {
        (_, vm::HIT) => Pricing::Unpriced("the fast path: the base"),
        (_, vm::NOT_TABLE) => ENGINE,
        (_, vm::READONLY) => ERROR,
        (_, vm::REHASHED) => REHASH,
        // a global read: by GETGLOBAL's price
        (0, vm::REPREDICTED) => Pricing::Borrowed("table key repredicted"),
        (0, vm::ABSENT) => Pricing::Measured("global slot absent"),
        (0, vm::INSERTED) => Pricing::Borrowed("table key inserted"),
        (0, vm::METAFUNCTION) => Pricing::CallOnly("table key metatable_function"),
        (0, vm::UNSAFE_ENV) => NONE,
        // an import: resolved at load, else looked up each time
        (1, vm::ABSENT | vm::UNSAFE_ENV) => LOOKED_UP,
        (1, _) => NONE,
        // a method's lookup: by GETTABLEKS' price
        (2, vm::REPREDICTED) => Pricing::Measured("method slot repredicted"),
        (2, vm::METAFUNCTION) => Pricing::CallOnly("method slot metatable_function"),
        (2, vm::ABSENT) => ERROR,
        (2, _) => NONE,
        // a table key
        (3, vm::REPREDICTED) => Pricing::Measured("table key repredicted"),
        (3, vm::ABSENT) => Pricing::Measured("table key absent"),
        (3, vm::INSERTED) => Pricing::Measured("table key inserted"),
        (3, vm::METAFUNCTION) => Pricing::CallOnly("table key metatable_function"),
        (3, _) => NONE,
        _ => NONE,
    }
}

/// How a metatable walk of kind `kind` that consulted `depth` __index tables (1 to vm::CACHE_DEPTHS, the last "or more") is priced
pub fn metatable_pricing(kind: usize, depth: usize) -> Pricing {
    let depth = depth.clamp(1, vm::CACHE_DEPTHS);
    match kind {
        // (a global's: its environment's __index, the shared globals, measured; deeper chains are a table key's)
        0 if depth == 1 => Pricing::Measured("global slot metatable 1"),
        0 => Pricing::Borrowed(DEPTH_CASES[depth - 1]),
        1 => NONE,
        // (a method found in the first __index table with the object's own node for it chained is a table key's walk)
        2 if depth == 1 => Pricing::Borrowed(DEPTH_CASES[0]),
        2 => Pricing::Measured(METHOD_DEPTH_CASES[depth - 1]),
        _ => Pricing::Measured(DEPTH_CASES[depth - 1]),
    }
}

const DEPTH_CASES: [&str; vm::CACHE_DEPTHS] = ["table key metatable 1", "table key metatable 2", "table key metatable 3", "table key metatable 4"];
const METHOD_DEPTH_CASES: [&str; vm::CACHE_DEPTHS] = ["", "method slot metatable 2", "method slot metatable 3", "method slot metatable 4"];

/// Whether the work clock charges an outcome (its work is in the site's charged work and its function's)
pub fn charged(kind: usize, outcome: usize) -> bool {
    matches!(pricing(kind, outcome), Pricing::Charged(_))
}

/// A price in the work clock's nanoseconds: the median and the spread's ends, and whether the spread reaches zero (within the
/// noise: shown, never summed)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub ns: f64,
    pub low_ns: f64,
    pub high_ns: f64,
    pub within_noise: bool,
}

impl Price {
    fn of(ratio: Ratio, base_ns: f64, less_ns: f64) -> Price {
        let (ns, low_ns, high_ns) = (ratio.median * base_ns - less_ns, ratio.low * base_ns - less_ns, ratio.high * base_ns - less_ns);
        // (within the noise where the interval reaches zero: the outcome might cost nothing over a hit)
        Price { ns, low_ns, high_ns, within_noise: low_ns <= 0.0 }
    }

    fn scaled(self, scale: f64) -> Price {
        Price { ns: self.ns * scale, low_ns: self.low_ns * scale, high_ns: self.high_ns * scale, within_noise: self.within_noise }
    }
}

/// An uncharged pricing's price on kind `kind`'s base, in the work clock's nanoseconds times `scale` (the run's device scale;
/// 1 for the desktop prices the work clock's totals are in); None for a charged or unpriced one
fn price_of(kind: usize, pricing: Pricing, scale: f64) -> Option<Price> {
    let base = crate::cost::INTERPRETED_OPS[BASE_OPS[kind]];
    let price = match pricing {
        Pricing::Measured(name) | Pricing::Borrowed(name) => Price::of(measured(name)?, base, 0.0),
        Pricing::CallOnly(name) => Price::of(measured(name)?, base, crate::cost::INTERPRETED_OPS[OP_LOADN] + crate::cost::INTERPRETED_OPS[OP_RETURN]),
        Pricing::Charged(_) | Pricing::Unpriced(_) => return None,
    };
    Some(price.scaled(scale))
}

/// What kind `kind`'s outcome `outcome` costs over a hit beyond the work clock's charge (None: charged, or not priced; a
/// metatable walk: [`metatable`])
pub fn outcome(kind: usize, outcome: usize, scale: f64) -> Option<Price> {
    price_of(kind, pricing(kind, outcome), scale)
}

/// What kind `kind`'s metatable walk of `depth` __index tables costs over a hit
pub fn metatable(kind: usize, depth: usize, scale: f64) -> Option<Price> {
    price_of(kind, metatable_pricing(kind, depth), scale)
}

/// A site's outcomes priced: the uncharged ones' work beyond the clock's, in microseconds (the prices clear of the noise), the
/// count of outcomes whose prices are within the noise (left out of it), the count the clock charges (in the site's charged
/// work), and the count with no price (an error)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extra {
    pub uncharged_us: f64,
    pub within_noise: u64,
    pub charged: u64,
    pub unpriced: u64,
}

/// What a site's outcomes (and its metatable walks by depth) cost over hits beyond the work clock's charge
pub fn site_extra(kind: usize, outcomes: &[u64; vm::CACHE_OUTCOMES], depths: &[u64; vm::CACHE_DEPTHS], scale: f64) -> Extra {
    let mut extra = Extra::default();
    let mut add = |price: Option<Price>, pricing: Pricing, count: u64| {
        if count == 0 {
            return;
        }
        match (price, pricing) {
            (_, Pricing::Charged(_)) => extra.charged += count,
            (Some(price), _) if price.within_noise => extra.within_noise += count,
            (Some(price), _) => extra.uncharged_us += count as f64 * price.ns / 1e3,
            (None, _) => extra.unpriced += count,
        }
    };
    for (what, &count) in outcomes.iter().enumerate() {
        if count == 0 || what == vm::HIT {
            continue;
        }
        if what == vm::METATABLE {
            let walks: u64 = depths.iter().sum();
            for (depth, &walked) in depths.iter().enumerate() {
                add(metatable(kind, depth + 1, scale), metatable_pricing(kind, depth + 1), walked);
            }
            // (walks that ended where the site recorded no depth: the shallowest)
            add(metatable(kind, 1, scale), metatable_pricing(kind, 1), count.saturating_sub(walks));
            continue;
        }
        add(outcome(kind, what, scale), pricing(kind, what), count);
    }
    extra
}

fn price_json(price: Option<Price>, pricing: Pricing) -> Json {
    let (charged, source, about) = match pricing {
        Pricing::Measured(name) => (false, "measured", format!("calibrated: {name}")),
        Pricing::Borrowed(name) => (false, "borrowed", format!("the same work measured on a table key: {name}")),
        Pricing::CallOnly(name) => (false, "measured", format!("calibrated: {name}, less what the work clock charges the __index function's own instructions where they run")),
        Pricing::Charged(why) => (true, "the work clock's", why.to_string()),
        Pricing::Unpriced(why) => (false, "unpriced", why.to_string()),
    };
    match price {
        Some(price) => json!({"charged": charged, "ns": round3(price.ns), "low_ns": round3(price.low_ns), "high_ns": round3(price.high_ns), "within_noise": price.within_noise, "source": source, "about": about}),
        None => json!({"charged": charged, "ns": null, "source": source, "about": about}),
    }
}

fn round3(value: f64) -> f64 {
    (value * 1e3).round() / 1e3
}

/// profile.json `luau_caches.prices`: every outcome by kind, charged or not, the uncharged ones' prices and the metatable walks by
/// depth, in the work clock's nanoseconds times `scale`, with the table's provenance
pub fn json(scale: f64) -> Json {
    let kinds: serde_json::Map<String, Json> = vm::CACHE_KINDS
        .iter()
        .enumerate()
        .map(|(kind, name)| {
            let outcomes: serde_json::Map<String, Json> = vm::CACHE_OUTCOME_NAMES
                .iter()
                .enumerate()
                .filter(|(what, _)| *what != vm::HIT && *what != vm::METATABLE)
                .map(|(what, outcome_name)| (outcome_name.to_string(), price_json(outcome(kind, what, scale), pricing(kind, what))))
                .collect();
            let depths: serde_json::Map<String, Json> = (1..=vm::CACHE_DEPTHS)
                .map(|depth| (if depth == vm::CACHE_DEPTHS { format!("{depth}+") } else { depth.to_string() }, price_json(metatable(kind, depth, scale), metatable_pricing(kind, depth))))
                .collect();
            (name.to_string(), json!({"base": vm::opcode_name(BASE_OPS[kind] as u8), "base_ns": crate::cost::INTERPRETED_OPS[BASE_OPS[kind]] * scale, "outcomes": outcomes, "metatable_depths": depths}))
        })
        .collect();
    json!({
        "clock": "work",
        "units": "work clock ns",
        "scale": scale,
        "about": "each outcome over a hit: charged ones are the work clock's own charge at the site (charged_work_us, part of its function's work); uncharged ones are a ratio measured on ree's interpreter times the work clock's price for the base opcode (extra_work_us, beyond the clock's work); a price within the noise is left out of every sum",
        "provenance": PROVENANCE.iter().map(|(key, value)| (key.to_string(), json!(value))).collect::<serde_json::Map<_, _>>(),
        "kinds": kinds,
    })
}

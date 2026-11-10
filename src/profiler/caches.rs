//! ree's own caches, counted while a profile asks for them (`--profile caches`): hits, misses and evictions of each
//! cache the engine keeps between script calls. Off, a count site pays one relaxed load. The counts are the process's
//! (a profile runs in its own process): work the engine does on other threads (bundles decoded ahead, modules compiled
//! ahead) counts too. A host counts its own caches (rur's layout) through [`named`].
//!
//! The caches measure how fast ree runs, not what the game costs (the summary lists them under "emulator"); three of them
//! say something a client does too, and [`facts`] gives those as the game's: the instances its scripts touched (a userdata
//! made for each, as a client makes one), the modules that ran, and the datatype property reads (a client makes a value for
//! each read; ree hands back the one it made while the property is unchanged).

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

static ON: AtomicBool = AtomicBool::new(false);

/// The instances a userdata was made for, by (index, generation): the instances the scripts touched
static TOUCHED: Mutex<Option<rustc_hash::FxHashSet<u64>>> = Mutex::new(None);

/// A cache of the engine's
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    /// A string's member id, kept on the Luau string (its atom): found on the string (hit) or looked up by its bytes (miss)
    Atoms,
    /// The Instance API's dispatch per (class, member): found (hit) or worked out (miss); evictions: the table cleared
    Members,
    /// The engine's member resolution per (class, name)
    Resolve,
    /// A datatype property's converted value per (instance, property) while the instance is unchanged
    Values,
    /// An instance's userdata: found (hit) or made (miss); evictions: collected by the VM
    Wrappers,
    /// require: the module's result kept (hit) or the module run (miss)
    Requires,
    /// A module file's source and bytecode kept in the process (a service's, or read ahead): kept (hit) or read and
    /// compiled (miss); evictions: dropped as stale
    Bytecode,
    /// Spatial queries: the solids of a filter kept while the world is unchanged (hit) or gathered again (miss)
    Queries,
    /// A manifest bundle's converted plan: read from its plan file (hit) or converted from the bundle (miss)
    BundlePlans,
}

impl Cache {
    pub const ALL: [Cache; 9] = [Cache::Atoms, Cache::Members, Cache::Resolve, Cache::Values, Cache::Wrappers, Cache::Requires, Cache::Bytecode, Cache::Queries, Cache::BundlePlans];

    pub fn name(self) -> &'static str {
        match self {
            Cache::Atoms => "member atoms",
            Cache::Members => "member dispatch",
            Cache::Resolve => "member resolution",
            Cache::Values => "datatype values",
            Cache::Wrappers => "instance userdata",
            Cache::Requires => "require results",
            Cache::Bytecode => "module bytecode",
            Cache::Queries => "query solids",
            Cache::BundlePlans => "bundle plans",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Cache::Atoms => "a property or method name's id kept on the Luau string (hit: read off the string; miss: looked up by its bytes, once for each new string the VM makes)",
            Cache::Members => "the Instance API's fast path per class and member (miss: worked out from the reflection)",
            Cache::Resolve => "the engine's member resolution per class and name",
            Cache::Values => "a UDim2, Color3, ... property's converted value, kept while the instance is unchanged",
            Cache::Wrappers => "an instance's Luau userdata (miss: a new one made; eviction: one collected)",
            Cache::Requires => "a ModuleScript's result (miss: the module ran)",
            Cache::Bytecode => "a module file's source and bytecode kept by the process (miss: read and compiled)",
            Cache::Queries => "a raycast or overlap filter's solids, kept while the world is unchanged",
            Cache::BundlePlans => "a manifest bundle's converted plan (miss: converted from the bundle file)",
        }
    }
}

/// Hits, misses and evictions
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// The engine's caches and up to this many of a host's
const SLOTS: usize = 32;

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ROW: [AtomicU64; 3] = [ZERO; 3];
static COUNTS: [[AtomicU64; 3]; SLOTS] = [ROW; SLOTS];
/// A host's own caches after the engine's: (name, what it is)
static NAMED: Mutex<Vec<(&'static str, &'static str)>> = Mutex::new(Vec::new());

/// Whether caches are counted
#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub(crate) fn set_on(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

fn add(index: usize, field: usize, count: u64) {
    if let Some(slot) = COUNTS.get(index) {
        slot[field].fetch_add(count, Ordering::Relaxed);
    }
}

#[inline]
pub fn hit(cache: Cache) {
    if on() {
        add(cache as usize, 0, 1);
    }
}

#[inline]
pub fn miss(cache: Cache) {
    if on() {
        add(cache as usize, 1, 1);
    }
}

#[inline]
pub fn evicted(cache: Cache, count: u64) {
    if on() && count > 0 {
        add(cache as usize, 2, count);
    }
}

/// A host's own cache (a layer's: rur's layout), its id for [`named_hit`] and the others; made once per name. Past the
/// slots there are, a cache shares the last one.
pub fn named(name: &'static str, about: &'static str) -> usize {
    let mut named = NAMED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let index = match named.iter().position(|(known, _)| *known == name) {
        Some(index) => index,
        None => {
            named.push((name, about));
            named.len() - 1
        }
    };
    (Cache::ALL.len() + index).min(SLOTS - 1)
}

#[inline]
pub fn named_hit(id: usize) {
    if on() {
        add(id, 0, 1);
    }
}

#[inline]
pub fn named_miss(id: usize) {
    if on() {
        add(id, 1, 1);
    }
}

#[inline]
pub fn named_evicted(id: usize, count: u64) {
    if on() && count > 0 {
        add(id, 2, count);
    }
}

fn counts(index: usize) -> Counts {
    COUNTS.get(index).map_or_else(Counts::default, |slot| Counts { hits: slot[0].load(Ordering::Relaxed), misses: slot[1].load(Ordering::Relaxed), evictions: slot[2].load(Ordering::Relaxed) })
}

/// Every cache's counts (name, what it is, counts), the engine's then the host's.
pub fn snapshot() -> Vec<(&'static str, &'static str, Counts)> {
    let mut rows: Vec<(&'static str, &'static str, Counts)> = Cache::ALL.iter().map(|cache| (cache.name(), cache.about(), counts(*cache as usize))).collect();
    let named = NAMED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    for (index, (name, about)) in named.into_iter().enumerate() {
        rows.push((name, about, counts((Cache::ALL.len() + index).min(SLOTS - 1))));
    }
    rows
}

/// A userdata was made for the instance (index, generation): one the scripts touched
#[inline]
pub fn instance_touched(index: u32, generation: u32) {
    if on() {
        let mut touched = TOUCHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        touched.get_or_insert_with(Default::default).insert(((generation as u64) << 32) | index as u64);
    }
}

/// What the engine's caches say of the game itself: what a client does too
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Instances the scripts touched: a userdata was made for each (a client makes one for an instance a script first reaches)
    pub instances_touched: u64,
    /// The userdata made (an instance's again once the VM collected its first) and collected
    pub userdata_made: u64,
    pub userdata_collected: u64,
    /// ModuleScripts that ran (a require that ran the module), and requires served by the result kept
    pub modules_run: u64,
    pub requires_kept: u64,
    /// Datatype property reads (UDim2, Color3, ...): a client makes a new value for each; ree handed back the value it made
    /// while the property was unchanged for `datatype_reads_reused` of them
    pub datatype_reads: u64,
    pub datatype_reads_reused: u64,
}

/// The game's facts the caches counted (the counts since the profile began)
pub fn facts() -> Facts {
    let (wrappers, requires, values) = (counts(Cache::Wrappers as usize), counts(Cache::Requires as usize), counts(Cache::Values as usize));
    let touched = TOUCHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().map_or(0, |set| set.len() as u64);
    Facts {
        instances_touched: touched,
        userdata_made: wrappers.misses,
        userdata_collected: wrappers.evictions,
        modules_run: requires.misses,
        requires_kept: requires.hits,
        datatype_reads: values.hits + values.misses,
        datatype_reads_reused: values.hits,
    }
}

/// Clears the counts (a profile's start).
pub(crate) fn reset() {
    for slot in &COUNTS {
        for field in slot {
            field.store(0, Ordering::Relaxed);
        }
    }
    *TOUCHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

//! The figure every view ranks, sizes and colours by; `m` cycles through the ones the profile has.

use crate::model::{LineFig, Profile};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Metric {
    /// The work clock: what each instruction costs in Studio, priced (`work`)
    Work,
    /// Host time sampled every millisecond (`time`)
    Time,
    /// Instructions the VM ran (`work`)
    Insns,
    /// Bytes allocated (`alloc`)
    AllocBytes,
    /// Objects allocated (`alloc`)
    AllocCount,
    /// Luau inline cache lookups that missed or took the slow path (`caches`)
    CacheMisses,
    /// A hardware counter, by its index in the profile's counters (`hw`)
    Counter(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Ms,
    Count,
    Bytes,
}

impl Metric {
    /// The metrics a profile has, in the order `m` cycles them
    pub fn available(p: &Profile) -> Vec<Metric> {
        let mut out = Vec::new();
        let work = p.raw.work.is_some() || !p.raw.tree.is_empty();
        if work {
            out.push(Metric::Work);
        }
        if p.raw.time.is_some() || p.folded.contains_key("time") {
            out.push(Metric::Time);
        }
        if work {
            out.push(Metric::Insns);
        }
        if p.raw.alloc.is_some() {
            out.push(Metric::AllocBytes);
            out.push(Metric::AllocCount);
        }
        if p.raw.luau_caches.as_ref().is_some_and(|c| !c.by_kind.is_empty() || !c.sites.is_empty()) {
            out.push(Metric::CacheMisses);
        }
        if let Some(counters) = p.raw.counters.as_ref().filter(|c| c.error.is_none()) {
            for index in 0..counters.names.len().min(255) {
                out.push(Metric::Counter(index as u8));
            }
        }
        out
    }

    /// The metric's name in headers and legends
    pub fn label(self, p: &Profile) -> String {
        match self {
            Metric::Work => "Studio work".into(),
            Metric::Time => "host time".into(),
            Metric::Insns => "instructions".into(),
            Metric::AllocBytes => "allocated bytes".into(),
            Metric::AllocCount => "allocations".into(),
            Metric::CacheMisses => "Luau cache misses".into(),
            Metric::Counter(index) => p.raw.counters.as_ref().and_then(|c| c.names.get(index as usize)).cloned().unwrap_or_else(|| format!("counter {index}")),
        }
    }

    /// The metric's name in a table's column header
    pub fn short_label(self, p: &Profile) -> String {
        match self {
            Metric::Work => "work".into(),
            Metric::Time => "host".into(),
            Metric::Insns => "insns".into(),
            Metric::AllocBytes => "bytes".into(),
            Metric::AllocCount => "objects".into(),
            Metric::CacheMisses => "misses".into(),
            Metric::Counter(_) => self.label(p),
        }
    }

    /// What the metric is, one line (the help and the header's note)
    pub fn about(self) -> &'static str {
        match self {
            Metric::Work => "each instruction priced at what a Studio client spends on it; the same every run",
            Metric::Time => "this machine's time, sampled every millisecond; varies run to run",
            Metric::Insns => "instructions the Luau VM ran",
            Metric::AllocBytes => "bytes the VM allocated",
            Metric::AllocCount => "objects the VM allocated",
            Metric::CacheMisses => "inline-cache lookups that missed or took the slow path (no stacks: the flame shows work)",
            Metric::Counter(_) => "a CPU counter on the run thread; varies run to run",
        }
    }

    /// The name `--metric` takes
    pub fn cli_name(self, p: &Profile) -> String {
        match self {
            Metric::Work => "work".into(),
            Metric::Time => "time".into(),
            Metric::Insns => "insns".into(),
            Metric::AllocBytes => "alloc".into(),
            Metric::AllocCount => "allocs".into(),
            Metric::CacheMisses => "misses".into(),
            Metric::Counter(_) => self.label(p),
        }
    }

    /// `--metric <name>`: work, time, insns (instructions), alloc (bytes), allocs (count), misses (cache), or a
    /// hardware counter's name
    pub fn parse(name: &str, p: &Profile) -> Option<Metric> {
        let name = name.trim().to_ascii_lowercase();
        let named = match name.as_str() {
            "work" | "studio" => Some(Metric::Work),
            "time" | "host" => Some(Metric::Time),
            "insns" | "instructions" => Some(Metric::Insns),
            "alloc" | "bytes" | "allocated" => Some(Metric::AllocBytes),
            "allocs" | "count" | "allocations" => Some(Metric::AllocCount),
            "misses" | "cache" | "caches" => Some(Metric::CacheMisses),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
        let counter = name.strip_prefix("hw:").unwrap_or(&name);
        p.raw.counters.as_ref()?.names.iter().position(|n| n.eq_ignore_ascii_case(counter)).map(|index| Metric::Counter(index as u8))
    }

    pub fn unit(self) -> Unit {
        match self {
            Metric::Work | Metric::Time => Unit::Ms,
            Metric::AllocBytes => Unit::Bytes,
            _ => Unit::Count,
        }
    }

    /// Whether the figures are the same every run of the same code
    pub fn exact(self) -> bool {
        !matches!(self, Metric::Time | Metric::Counter(_))
    }

    /// The folded file whose stacks size this metric's flame graph (None: the call tree in profile.json; work and
    /// instructions come from it, with call counts)
    pub fn folded(self, p: &Profile) -> Option<String> {
        match self {
            Metric::Time => Some("time".into()),
            Metric::AllocBytes => Some("alloc".into()),
            Metric::AllocCount => Some("alloc.counts".into()),
            Metric::Counter(index) => {
                let name = p.raw.counters.as_ref()?.names.get(index as usize)?;
                Some(format!("counters.{}", name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>()))
            }
            _ => None,
        }
    }

    /// Whether the metric has stacks (a flame graph, callers and callees of its own); cache misses have none and
    /// borrow work's
    pub fn has_stacks(self, p: &Profile) -> bool {
        match self {
            Metric::Work | Metric::Insns => !p.raw.tree.is_empty(),
            Metric::CacheMisses => false,
            _ => self.folded(p).is_some_and(|name| p.folded.contains_key(&name)),
        }
    }

    /// The metric whose stacks the flame graph and the callers use for this one
    pub fn stacks(self, p: &Profile) -> Metric {
        if self.has_stacks(p) { self } else { Metric::Work }
    }

    /// The run's total
    pub fn total(self, p: &Profile) -> f64 {
        match self {
            Metric::Work => p.raw.work.as_ref().map(|w| w.total_ms).filter(|t| *t > 0.0).unwrap_or_else(|| p.funcs.iter().map(|f| f.own.work_ms).sum()),
            Metric::Time => p.raw.time.as_ref().map(|t| t.sampled_ms).unwrap_or_else(|| p.folded.get("time").map_or(0.0, |f| f.total)),
            Metric::Insns => p.raw.work.as_ref().map(|w| w.script_insns + w.engine_insns).filter(|t| *t > 0.0).unwrap_or_else(|| p.funcs.iter().map(|f| f.own.insns).sum()),
            Metric::AllocBytes => p.raw.alloc.as_ref().map_or(0.0, |a| a.bytes),
            Metric::AllocCount => p.raw.alloc.as_ref().map_or(0.0, |a| a.objects),
            Metric::CacheMisses => p.raw.luau_caches.as_ref().map_or(0.0, |c| c.by_kind.iter().map(|k| k.misses + k.slow).sum()),
            Metric::Counter(index) => p.raw.counters.as_ref().and_then(|c| c.names.get(index as usize).and_then(|name| c.run.counts.get(name))).copied().unwrap_or(0.0),
        }
    }

    /// A function's own figure, as the profile counted it
    pub fn func_self(self, p: &Profile, func: usize) -> f64 {
        let f = &p.funcs[func];
        match self {
            Metric::Work => f.own.work_ms,
            Metric::Time => f.own.time_ms,
            Metric::Insns => f.own.insns,
            Metric::AllocBytes => f.own.alloc_bytes,
            Metric::AllocCount => f.own.allocs,
            Metric::CacheMisses => p.miss_funcs.get(&func).map_or(0.0, |m| m.0),
            Metric::Counter(index) => p.counter_funcs.get(&func).and_then(|(own, _)| own.get(index as usize)).copied().unwrap_or(0.0),
        }
    }

    /// A function's total, its callees' included, as the profile counted it
    pub fn func_total(self, p: &Profile, func: usize) -> f64 {
        let f = &p.funcs[func];
        match self {
            Metric::Work => f.total.work_ms,
            Metric::Time => f.total.time_ms,
            Metric::Insns => f.total.insns,
            Metric::AllocBytes => f.total.alloc_bytes,
            Metric::AllocCount => f.total.allocs,
            Metric::CacheMisses => self.func_self(p, func),
            Metric::Counter(index) => p.counter_funcs.get(&func).and_then(|(_, total)| total.get(index as usize)).copied().unwrap_or(0.0),
        }
    }

    /// A line's own figure
    pub fn line_self(self, fig: &LineFig) -> f64 {
        match self {
            Metric::Work => fig.work,
            Metric::Time => fig.time,
            Metric::Insns => fig.insns,
            Metric::AllocBytes => fig.alloc,
            Metric::AllocCount => fig.allocs,
            Metric::CacheMisses => fig.misses,
            Metric::Counter(index) => fig.counters.get(index as usize).copied().unwrap_or(0.0),
        }
    }

    /// A line's total: its own and what the calls it makes cost (work, instructions and host time count them apart)
    pub fn line_total(self, fig: &LineFig) -> f64 {
        match self {
            Metric::Work => fig.total_work.max(fig.work),
            Metric::Time => fig.total_time.max(fig.time),
            Metric::Insns => fig.total_insns.max(fig.insns),
            _ => self.line_self(fig),
        }
    }

    /// The timeline column that holds this metric a frame (`counters.frames` for a counter)
    pub fn timeline_column(self, p: &Profile) -> Option<String> {
        match self {
            Metric::Work => Some("work_ms".into()),
            Metric::Time => Some("host_ms".into()),
            Metric::Insns => Some("instructions".into()),
            Metric::AllocBytes => Some("allocated_bytes".into()),
            Metric::AllocCount => Some("allocations".into()),
            Metric::CacheMisses => None,
            Metric::Counter(index) => p.raw.counters.as_ref().and_then(|c| c.names.get(index as usize)).cloned(),
        }
    }
}

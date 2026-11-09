//! A what-if comparison (`ree profile try`): compare.json, its variants' figures at each input size, how each scales
//! with n, the winners and the table shapes. The format is docs/cli.md "compare.json"; each run is an ordinary profile
//! folder the other views open.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    fmt,
    issues::{Issue, Severity, Target},
};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VariantInfo {
    pub name: String,
    pub slug: String,
    pub kind: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MetricInfo {
    pub key: String,
    pub label: String,
    pub unit: String,
    pub better: String,
    pub exact: bool,
    pub about: String,
    /// A call's figure (else the run's, as the heap it keeps)
    pub per_call: bool,
}

impl MetricInfo {
    /// Whether less is better (a cost); not for a figure that is better higher, nor one that names no winner
    pub fn lower_is_better(&self) -> bool {
        self.better == "lower" || self.better.is_empty()
    }

    /// Whether the figure names a better side: `better` "none" (a median beside the best round, the collector's
    /// host time, cache hits) only describes
    pub fn ranked(&self) -> bool {
        self.better != "none"
    }

    pub fn show(&self, value: f64) -> String {
        match self.unit.as_str() {
            "ms" => fmt::ms(value),
            "bytes" => fmt::bytes(value),
            "ratio" => {
                if self.key.contains("rate") {
                    format!("{:.1}%", value * 100.0)
                } else {
                    format!("{value:.2}")
                }
            }
            _ => fmt::compact(value),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Check {
    pub ok: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HostSamples {
    pub ms: Vec<f64>,
    pub cycles: Vec<f64>,
    pub reps: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HeapMarks {
    pub before: f64,
    pub after_setup: f64,
    pub after_run: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Part {
    pub slots: f64,
    pub used: f64,
    pub min: f64,
    pub median: f64,
    pub max: f64,
    pub hist: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Levels {
    pub min: f64,
    pub median: f64,
    pub max: f64,
    pub hist: Vec<(f64, f64)>,
}

/// A count of tables and their bytes
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Totals {
    pub count: f64,
    pub bytes: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TableFrame {
    pub module: String,
    pub name: String,
    pub line: i64,
    pub path: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TableSite {
    pub site: String,
    pub frames: Vec<TableFrame>,
    pub phase: String,
    pub count: f64,
    pub bytes: f64,
    pub header_bytes: f64,
    pub array_bytes: f64,
    pub hash_bytes: f64,
    pub array: Part,
    pub hash: Part,
    pub levels: Levels,
    pub rehashes: f64,
    pub rehash_keys: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Tables {
    pub count: f64,
    pub bytes: f64,
    pub sites: Vec<TableSite>,
    /// The setup's tables (the input, the same for every variant) and the run's (the variant's own layout)
    pub setup: Option<Totals>,
    pub run: Option<Totals>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Hot {
    pub key: String,
    pub name: String,
    pub module: String,
    pub line: i64,
    pub work_ms: f64,
    pub insns: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RunInfo {
    pub variant: String,
    pub size: f64,
    pub status: String,
    pub error: Option<String>,
    pub profile: Option<String>,
    pub hw_profile: Option<String>,
    pub digest: Option<String>,
    pub check: Option<Check>,
    pub setup: BTreeMap<String, Option<f64>>,
    pub run: BTreeMap<String, Option<f64>>,
    pub host: Option<HostSamples>,
    pub heap: Option<HeapMarks>,
    pub tables: Option<Tables>,
    pub hot: Vec<Hot>,
}

impl RunInfo {
    /// A run-phase figure (a call's), None when not measured
    pub fn value(&self, key: &str) -> Option<f64> {
        self.run.get(key).copied().flatten()
    }
}

/// A fit of a figure to a + b * n^k: a fixed cost a call apart (compare.json version 2)
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Fit {
    pub k: f64,
    pub fixed: f64,
    pub coefficient: f64,
    /// The fit's largest relative error at a size
    pub error: f64,
    pub fitted: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Scaling {
    pub variant: String,
    pub metric: String,
    /// Version 2: the exponent at large n (the growing part's slope between the two largest sizes, the fixed cost apart);
    /// version 1: a log-log fit with no fixed cost, which a constant overhead bends low
    pub exponent: Option<f64>,
    /// Version 1's fit quality
    pub r2: Option<f64>,
    /// The raw slope between the two largest sizes
    pub last: Option<f64>,
    pub fit: Option<Fit>,
    /// Each pair of neighbouring sizes: (n, next n, slope)
    pub segments: Vec<(f64, f64, f64)>,
    pub points: Vec<(f64, f64)>,
}

impl Scaling {
    /// The exponent to read: version 2's large-n one; for version 1, the slope between the two largest sizes (its fit,
    /// without a fixed cost, read a linear walk as n^0.68)
    pub fn large_n(&self) -> Option<f64> {
        if self.fit.is_some() { self.exponent } else { self.last.or(self.exponent) }
    }

    /// Whether the exponent rests on a fit good enough to read (version 2: within 25 % at every size; version 1: r² 0.9)
    pub fn trusted(&self) -> bool {
        match &self.fit {
            Some(fit) => !fit.fitted || fit.error <= 0.25,
            None => self.r2.unwrap_or(1.0) >= 0.9,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FunctionScaling {
    pub variant: String,
    pub key: String,
    pub name: String,
    pub module: String,
    pub line: i64,
    pub exponent: Option<f64>,
    pub last: Option<f64>,
    pub fit: Option<Fit>,
    pub points: Vec<(f64, f64)>,
}

impl FunctionScaling {
    pub fn large_n(&self) -> Option<f64> {
        if self.fit.is_some() { self.exponent } else { self.last.or(self.exponent) }
    }
}

/// The rounds a measured verdict rests on: won of paired, the sign test's chance, the A/A noise at that many rounds
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rounds {
    pub won: Option<u64>,
    pub of: Option<u64>,
    pub p: Option<f64>,
    pub noise: Option<f64>,
    /// Rounds were added at the size for pairs still undecided
    pub added: bool,
    /// The size's A/A pair parted beyond the noise: the gap (a ratio less 1) a verdict there had to pass
    pub aa_bar: Option<f64>,
}

/// A variant against the best (ranking) or the baseline: its figure over the other's (above 1 worse, or more for a figure
/// that names no better side), whether the gap is clear of noise and material
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Verdict {
    pub variant: String,
    pub value: Option<f64>,
    pub ratio: Option<f64>,
    /// A measured figure's 95 % interval of the ratio (host time's paired shift)
    pub interval: Option<(f64, f64)>,
    pub clear: bool,
    pub material: Option<bool>,
    pub rounds: Option<Rounds>,
    /// Its result differs from the baseline's: no verdict
    pub differs: bool,
    /// The comparison weighed no verdict here (a figure that names no better side, or where nothing won): the ratio alone
    #[serde(skip)]
    pub unweighed: bool,
}

impl Verdict {
    /// A gap the comparison names: clear of noise, material, and between two variants that compute the same thing
    pub fn significant(&self) -> bool {
        self.clear && self.material != Some(false) && !self.differs
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Winner {
    pub metric: String,
    pub size: f64,
    pub winner: String,
    pub value: Option<f64>,
    pub runner_up: Option<String>,
    pub ratio: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub vs_baseline: Option<f64>,
    pub clear: bool,
    /// False when the gap is too small to matter (a ms gap under the floor, a count under 0.1 % of the run's instructions)
    pub material: Option<bool>,
    pub ranking: Vec<Verdict>,
    /// Version 2: each variant against the baseline, either way
    pub baseline: Vec<Verdict>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AaPair {
    pub size: f64,
    pub ratio: f64,
    pub spread: f64,
    /// Version 2: the paired shift of the second run over the first, its interval, whether the pair parted beyond the noise
    pub shift: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub separated: bool,
    pub noise: Option<f64>,
}

/// The material floors a comparison applied (version 2)
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Floor {
    /// A ms figure's gap a call from which it matters
    pub ms: f64,
    pub per_frame: f64,
    pub host: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HwInfo {
    pub read: bool,
    pub why: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CheckAll {
    pub ran: bool,
    pub ok: bool,
    /// How results were checked (version 2): `function`, `equality` or `digest` (no check function in the file)
    pub by: Option<String>,
    pub failures: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DiffRef {
    pub variant: String,
    pub size: f64,
    pub dir: String,
}

/// compare.json
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Compare {
    #[serde(skip)]
    pub dir: PathBuf,
    pub format: String,
    pub version: u32,
    pub title: String,
    pub about: String,
    pub file: String,
    pub command: String,
    pub baseline: String,
    pub variants: Vec<VariantInfo>,
    pub sizes: Vec<f64>,
    pub frames: f64,
    pub fidelity: String,
    pub metrics: Vec<MetricInfo>,
    pub runs: Vec<RunInfo>,
    pub scaling: Vec<Scaling>,
    pub functions: Vec<FunctionScaling>,
    pub winners: Vec<Winner>,
    pub aa: Vec<AaPair>,
    pub hw: Option<HwInfo>,
    pub check: Option<CheckAll>,
    pub diffs: Vec<DiffRef>,
    pub notes: Vec<String>,
    pub floor: Option<Floor>,
    pub per_frame: Option<f64>,
}

impl Compare {
    /// Reads a comparison folder (or its compare.json)
    pub fn load(path: &Path) -> Result<Compare, String> {
        let (dir, file) = if path.is_file() { (path.parent().unwrap_or(Path::new(".")).to_path_buf(), path.to_path_buf()) } else { (path.to_path_buf(), path.join("compare.json")) };
        let text = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        let mut compare: Compare = serde_json::from_slice(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        compare.dir = dir;
        if compare.format != "ree-what-if" && compare.format != "" {
            return Err(format!("{}: not a what-if comparison (format {:?})", file.display(), compare.format));
        }
        if compare.metrics.is_empty() {
            // (a comparison without its metric list: the figures its runs carry, in a fixed order)
            let mut keys: Vec<String> = compare.runs.iter().flat_map(|run| run.run.keys().cloned()).collect();
            keys.sort();
            keys.dedup();
            compare.metrics = keys.into_iter().map(|key| MetricInfo { label: key.replace('_', " "), unit: if key.ends_with("_ms") { "ms".into() } else if key.contains("bytes") { "bytes".into() } else { "count".into() }, better: "lower".into(), exact: !key.starts_with("host") && !key.starts_with("hw_"), per_call: key != "heap_bytes", key, about: String::new() }).collect();
        }
        Ok(compare)
    }

    pub fn is_comparison(path: &Path) -> bool {
        if path.is_file() {
            return path.file_name().is_some_and(|name| name == "compare.json");
        }
        path.join("compare.json").is_file() && !path.join("profile.json").is_file()
    }

    pub fn metric(&self, key: &str) -> Option<&MetricInfo> {
        self.metrics.iter().find(|m| m.key == key)
    }

    /// The metrics some run measured, in the file's order
    pub fn measured(&self) -> Vec<&MetricInfo> {
        self.metrics.iter().filter(|m| self.runs.iter().any(|run| run.value(&m.key).is_some())).collect()
    }

    pub fn run(&self, variant: &str, size: f64) -> Option<&RunInfo> {
        self.runs.iter().find(|run| run.variant == variant && run.size == size)
    }

    pub fn scaling(&self, variant: &str, metric: &str) -> Option<&Scaling> {
        self.scaling.iter().find(|s| s.variant == variant && s.metric == metric)
    }

    pub fn winner(&self, metric: &str, size: f64) -> Option<&Winner> {
        self.winners.iter().find(|w| w.metric == metric && w.size == size)
    }

    /// Whether a run's result differs from the baseline's at its size: its check failed, or (a comparison from before
    /// results were checked by digest) no check ran and its digest is not the baseline's
    pub fn differs(&self, run: &RunInfo) -> bool {
        if run.check.as_ref().is_some_and(|check| !check.ok) {
            return true;
        }
        let checked = self.check.as_ref().is_some_and(|check| check.ran);
        if checked || run.variant == self.baseline {
            return false;
        }
        let base = self.run(&self.baseline, run.size).and_then(|base| base.digest.as_ref());
        matches!((base, &run.digest), (Some(base), Some(digest)) if base != digest)
    }

    /// The runs whose results differ from the baseline's
    pub fn differing(&self) -> Vec<&RunInfo> {
        self.runs.iter().filter(|run| self.differs(run)).collect()
    }

    /// A variant against the baseline in a metric at a size: compare.json's verdict (version 2), else, for an exact
    /// figure, a gap of 1 % or more (version 1 gave no verdict against the baseline; a measured figure then has none).
    /// Where version 2 weighed none (a figure that names no better side, as the median round, the collector's host time
    /// and the cache hit count; or, from a ree before tied leaders kept the others' verdicts, a size where the best figure
    /// tied) the ratio stands alone, `unweighed`
    pub fn against_baseline(&self, metric: &MetricInfo, size: f64, variant: &str) -> Option<Verdict> {
        if let Some(found) = self.winner(&metric.key, size).and_then(|winner| winner.baseline.iter().find(|entry| entry.variant == variant)) {
            return Some(found.clone());
        }
        let (base, run) = (self.run(&self.baseline, size)?, self.run(variant, size)?);
        let (b, v) = (base.value(&metric.key)?, run.value(&metric.key)?);
        if b <= 0.0 || v <= 0.0 {
            return None;
        }
        let ratio = if metric.lower_is_better() || !metric.ranked() { v / b } else { b / v };
        // (version 2 gave a verdict for every gap it weighed: a tie reads as the same figure, any other gap as its ratio
        // with no verdict)
        if self.version >= 2 {
            let tie = (ratio - 1.0).abs() < 0.0005;
            return Some(Verdict { variant: variant.to_string(), value: Some(v), ratio: Some(if tie { 1.0 } else { ratio }), differs: self.differs(run), unweighed: !tie, ..Verdict::default() });
        }
        Some(Verdict { variant: variant.to_string(), value: Some(v), ratio: Some(ratio), clear: metric.exact && (ratio >= 1.01 || ratio <= 1.0 / 1.01), material: None, differs: self.differs(run), ..Verdict::default() })
    }

    /// A run's profile folder
    pub fn profile_dir(&self, run: &RunInfo) -> Option<PathBuf> {
        run.profile.as_ref().map(|relative| self.dir.join(relative)).filter(|dir| dir.join("profile.json").is_file())
    }

    /// What the comparison shows as issues: code whose cost grows faster than n (read at large n, where a fixed cost a
    /// call no longer bends it), results that differ from the baseline's, runs that failed
    pub fn issues(&self) -> Vec<Issue> {
        let mut out = Vec::new();
        let curve = |points: &[(f64, f64)]| points.iter().map(|(n, v)| format!("n={} {}", fmt::compact(*n), fmt::ms(*v))).collect::<Vec<_>>().join(", ");
        for scaling in self.scaling.iter().filter(|s| s.metric == "work_ms") {
            let Some(exponent) = scaling.large_n() else { continue };
            if exponent < 1.3 || !scaling.trusted() || scaling.points.len() < 3 {
                continue;
            }
            let segments = scaling.segments.iter().map(|(from, to, slope)| format!("{}-{} {slope:.2}", fmt::compact(*from), fmt::compact(*to))).collect::<Vec<_>>().join(", ");
            let fixed = scaling.fit.as_ref().filter(|fit| fit.fitted && fit.fixed > 0.0).map(|fit| format!(", a fixed {} a call apart", fmt::ms(fit.fixed))).unwrap_or_default();
            out.push(issue(
                if exponent >= 1.8 { Severity::Serious } else { Severity::Warning },
                "scaling",
                format!("{}: work grows as n^{exponent:.2}", scaling.variant),
                vec![format!("at large n: exponent {exponent:.2}{fixed}; each segment {}", if segments.is_empty() { "-".into() } else { segments }), curve(&scaling.points)],
                "Something is repeated per element",
                Target::Variant(scaling.variant.clone()),
                exponent,
            ));
        }
        for function in &self.functions {
            let Some(exponent) = function.large_n() else { continue };
            if exponent < 1.5 || function.points.len() < 3 {
                continue;
            }
            out.push(issue(
                if exponent >= 1.8 { Severity::Serious } else { Severity::Warning },
                "scaling",
                format!("{} in {} grows as n^{exponent:.2}", function.name, function.variant),
                vec![format!("{}:{}: work {}", function.module, function.line, curve(&function.points))],
                "It does per-element work for every element",
                Target::Variant(function.variant.clone()),
                exponent,
            ));
        }
        let by_digest = self.check.as_ref().is_none_or(|check| !check.ran || check.by.as_deref() == Some("digest"));
        for run in self.differing() {
            let message = run.check.as_ref().and_then(|check| check.message.clone()).unwrap_or_else(|| {
                format!("its result's digest is {}, the baseline's {}", run.digest.as_deref().unwrap_or("?"), self.run(&self.baseline, run.size).and_then(|base| base.digest.as_deref()).unwrap_or("?"))
            });
            out.push(issue(
                Severity::Critical,
                "result differs",
                format!("{} at n={}: a different result from {}{}", run.variant, fmt::compact(run.size), self.baseline, if by_digest { " (by digest)" } else { "" }),
                vec![message],
                if by_digest { "It does not compute the same thing (or computes it in another form): it wins nothing. Add check = function(a, b, n) to the file to compare results your way" } else { "It does not compute the same thing, so it is no faster version of it: it wins nothing" },
                Target::Variant(run.variant.clone()),
                1.0,
            ));
        }
        for run in self.runs.iter().filter(|run| run.status != "ok" && !run.status.is_empty()) {
            out.push(issue(Severity::Serious, "failed run", format!("{} at n={}: {}", run.variant, fmt::compact(run.size), run.status), vec![run.error.clone().unwrap_or_else(|| "no message".into())], "", Target::Variant(run.variant.clone()), 0.5));
        }
        out
    }
}

/// An issue of the comparison's (one place builds them, so the issue type's other fields keep their defaults)
fn issue(severity: Severity, kind: &'static str, title: String, evidence: Vec<String>, advice: &str, target: Target, weight: f64) -> Issue {
    Issue { severity, kind, title, evidence, advice: advice.to_string(), target, weight, ..Default::default() }
}

impl Compare {
    /// The variants' digests at a size disagree with the baseline's where no check ran: a header line's worth
    pub fn differ_line(&self) -> Option<String> {
        let differing = self.differing();
        if differing.is_empty() {
            return None;
        }
        let mut by_variant: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for run in &differing {
            by_variant.entry(run.variant.as_str()).or_default().push(fmt::grouped(run.size));
        }
        let how = match self.check.as_ref().and_then(|check| check.by.as_deref()) {
            Some("digest") | None => "by digest: no check function",
            Some("equality") => "by deep equality",
            _ => "by the file's check",
        };
        Some(format!("results differ from {}'s ({how}): {}", self.baseline, by_variant.iter().map(|(variant, sizes)| format!("{variant} at n = {}", sizes.join(", "))).collect::<Vec<_>>().join("; ")))
    }
}

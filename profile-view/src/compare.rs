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

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Scaling {
    pub variant: String,
    pub metric: String,
    pub exponent: Option<f64>,
    pub r2: Option<f64>,
    pub last: Option<f64>,
    pub points: Vec<(f64, f64)>,
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
    pub points: Vec<(f64, f64)>,
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
    pub vs_baseline: Option<f64>,
    pub clear: bool,
    /// False when the figure is a count too small beside the run's instructions to matter
    pub material: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AaPair {
    pub size: f64,
    pub ratio: f64,
    pub spread: f64,
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

    /// A run's profile folder
    pub fn profile_dir(&self, run: &RunInfo) -> Option<PathBuf> {
        run.profile.as_ref().map(|relative| self.dir.join(relative)).filter(|dir| dir.join("profile.json").is_file())
    }

    /// What the comparison shows as issues: code whose cost grows faster than n, results that disagree, runs that failed
    pub fn issues(&self) -> Vec<Issue> {
        let mut out = Vec::new();
        let lo = self.sizes.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = self.sizes.iter().copied().fold(0.0, f64::max);
        for scaling in self.scaling.iter().filter(|s| s.metric == "work_ms") {
            let (Some(exponent), r2) = (scaling.exponent, scaling.r2.unwrap_or(1.0)) else { continue };
            if exponent < 1.3 || r2 < 0.9 || scaling.points.len() < 3 {
                continue;
            }
            out.push(Issue {
                severity: if exponent >= 1.8 { Severity::Serious } else { Severity::Warning },
                kind: "scaling",
                title: format!("{}: work grows as n^{exponent:.2}", scaling.variant),
                evidence: vec![
                    format!("n {} to {}: exponent {exponent:.2} (r² {r2:.3}){}", fmt::compact(lo), fmt::compact(hi), scaling.last.map(|last| format!(", {last:.2} between the two largest")).unwrap_or_default()),
                    scaling.points.iter().map(|(n, v)| format!("n={} {}", fmt::compact(*n), fmt::ms(*v))).collect::<Vec<_>>().join(", "),
                ],
                advice: "Something is repeated per element".into(),
                target: Target::Variant(scaling.variant.clone()),
                weight: exponent,
            });
        }
        for function in &self.functions {
            let Some(exponent) = function.exponent else { continue };
            if exponent < 1.5 || function.points.len() < 3 {
                continue;
            }
            out.push(Issue {
                severity: if exponent >= 1.8 { Severity::Serious } else { Severity::Warning },
                kind: "scaling",
                title: format!("{} in {} grows as n^{exponent:.2}", function.name, function.variant),
                evidence: vec![format!("{}:{}: work {}", function.module, function.line, function.points.iter().map(|(n, v)| format!("n={} {}", fmt::compact(*n), fmt::ms(*v))).collect::<Vec<_>>().join(", "))],
                advice: "It does per-element work for every element".into(),
                target: Target::Variant(function.variant.clone()),
                weight: exponent,
            });
        }
        for run in self.runs.iter().filter(|run| run.check.as_ref().is_some_and(|c| !c.ok)) {
            out.push(Issue {
                severity: Severity::Critical,
                kind: "check",
                title: format!("{} at n={}: a different result from {}", run.variant, fmt::compact(run.size), self.baseline),
                evidence: vec![run.check.as_ref().and_then(|c| c.message.clone()).unwrap_or_else(|| "check() returned false".into())],
                advice: "It does not compute the same thing, so it is no faster version of it".into(),
                target: Target::Variant(run.variant.clone()),
                weight: 1.0,
            });
        }
        for run in self.runs.iter().filter(|run| run.status != "ok" && !run.status.is_empty()) {
            out.push(Issue {
                severity: Severity::Serious,
                kind: "failed run",
                title: format!("{} at n={}: {}", run.variant, fmt::compact(run.size), run.status),
                evidence: vec![run.error.clone().unwrap_or_else(|| "no message".into())],
                advice: String::new(),
                target: Target::Variant(run.variant.clone()),
                weight: 0.5,
            });
        }
        out
    }
}

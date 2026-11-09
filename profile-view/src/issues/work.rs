//! The work a fix removes, named by call-tree node and by charged site, so two fixes' savings can be added without
//! counting shared work twice. How much work a function did in all says nothing about which of it two fixes share,
//! so only node and site identity decides what overlaps.
use std::collections::{BTreeMap, BTreeSet};

/// One charged site a fix changes, in the call-tree context that ran it
#[derive(Clone, Debug, Default)]
pub struct SiteWork {
    /// The script call-tree context that executed the site.
    pub node: usize,
    /// The site's identity: the same site in the same context is the same work in every fix
    pub key: String,
    /// Several compiled sites can belong to the same source edit. Empty means the site itself.
    pub fix: String,
    /// Saving after per-call replacement work, ms over all reported frames. Shared replacement work is kept below.
    pub saving: f64,
}

impl SiteWork {
    /// What this site's edit is called: the source edit it belongs to, or the site itself
    pub fn fix_key(&self) -> &str {
        if self.fix.is_empty() { &self.key } else { &self.fix }
    }
}

/// The work a fix removes
#[derive(Clone, Debug, Default)]
pub struct WorkSet {
    /// Call-tree nodes the fix removes whole: all of each node's own work is saved
    pub nodes: BTreeSet<usize>,
    /// Charged sites the fix changes: each saves its charged work less its replacement's
    pub sites: Vec<SiteWork>,
    /// Shared replacement work for an edited site, paid once while any of its contexts still need the fix.
    pub replacements: BTreeMap<String, f64>,
    /// Active frames per prototype context, only for fixes that add cold code.
    pub replacement_contexts: BTreeMap<String, BTreeMap<usize, BTreeSet<i64>>>,
}

impl WorkSet {
    /// Adds the other fix's work to this one. Work both name is kept once: a node once, a site once at its larger saving,
    /// a replacement once.
    pub fn union(&mut self, other: &Self) {
        self.nodes.extend(&other.nodes);
        for (fix, contexts) in &other.replacement_contexts {
            self.replacement_contexts.entry(fix.clone()).or_insert_with(|| contexts.clone());
        }
        for (fix, cost) in &other.replacements {
            self.replacements.entry(fix.clone()).or_insert(*cost);
        }
        for site in &other.sites {
            let known = self.sites.iter_mut().find(|known| known.node == site.node && known.key == site.key);
            match known {
                Some(known) => known.saving = known.saving.max(site.saving),
                None => self.sites.push(site.clone()),
            }
        }
    }

    /// The work the fix saves in all (ms over the reported frames): its nodes' own work, and its sites' savings less the
    /// replacement work each edit still needs. A site inside a node the fix removes is already saved with that node.
    pub fn total(&self, node_work: &[f64]) -> f64 {
        let removed_nodes: f64 = self.nodes.iter().map(|node| node_work.get(*node).copied().unwrap_or(0.0)).sum();
        let mut site_savings_by_edit: BTreeMap<&str, f64> = BTreeMap::new();
        for site in &self.sites {
            if !self.nodes.contains(&site.node) {
                *site_savings_by_edit.entry(site.fix_key()).or_default() += site.saving;
            }
        }
        let changed_sites: f64 = site_savings_by_edit
            .iter()
            .map(|(edit, saving)| (saving - self.replacement_still_paid(edit)).max(0.0))
            .sum();
        removed_nodes + changed_sites
    }

    /// The replacement work an edit still pays (ms): its shared replacement cost, scaled to the share of its active
    /// frames that are in contexts this fix does not remove. Once every context is removed it pays nothing.
    fn replacement_still_paid(&self, edit: &str) -> f64 {
        let cost = self.replacements.get(edit).copied().unwrap_or(0.0);
        let Some(contexts) = self.replacement_contexts.get(edit) else { return cost };
        let mut frames_in_any_context: BTreeSet<i64> = BTreeSet::new();
        let mut frames_in_kept_contexts: BTreeSet<i64> = BTreeSet::new();
        for (node, frames) in contexts {
            frames_in_any_context.extend(frames);
            if !self.nodes.contains(node) {
                frames_in_kept_contexts.extend(frames);
            }
        }
        if frames_in_any_context.is_empty() {
            return cost;
        }
        cost * (frames_in_kept_contexts.len() as f64 / frames_in_any_context.len() as f64)
    }
}

/// The work the profile's call tree measured in the reported phases, by node and by the script function that owns it
pub struct Work {
    /// Each call-tree node's own work, ms over the reported frames (0 for nodes outside the report or the issues' code)
    pub amounts: Vec<f64>,
    /// Each node's owning script function: the nearest one on its path to the root
    pub owner: Vec<Option<usize>>,
    /// Each function's own work as a set: every node it owns
    pub functions: BTreeMap<usize, WorkSet>,
    /// The nodes where each function itself ran (not those of the C functions and engine code it called)
    pub contexts: BTreeMap<usize, Vec<usize>>,
}

impl Work {
    /// The function that owns a node, if it has one
    pub fn owner_of(&self, node: usize) -> Option<usize> {
        self.owner.get(node).copied().flatten()
    }

    /// All the work a function did (ms over the reported frames): the most a fix in it could save
    pub fn function_total(&self, func: usize) -> f64 {
        self.functions.get(&func).map_or(0.0, |set| set.total(&self.amounts))
    }
}

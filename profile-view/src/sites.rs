//! Allocation sites: the Luau line that allocated (the innermost script frame of the stack; a C function's or the engine's
//! own Luau's allocation is the script line's that called it), with what it allocated over the run, by phase and by type.
//!
//! Two of the profile's views are joined, each exact for what it holds: the folded stacks (`alloc.folded`,
//! `alloc.counts.folded`) hold every stack by phase but name only the innermost frame's line, and profile.json's
//! `alloc.by_stack` holds the heaviest stacks (up to 500) with every frame's line and the type, over the whole run.
//! A folded stack the JSON also holds takes its line from there; one it does not, whose innermost frame is a C
//! function, is the Luau caller's with no line (-1).

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;

use crate::model::{Kind, Profile};

/// The site of allocations no Luau frame made (the engine's)
pub const ENGINE: u32 = u32::MAX;

#[derive(Clone, Debug, Default)]
pub struct AllocSite {
    /// The function (ENGINE when no Luau frame was on the stack)
    pub func: u32,
    /// The line, -1 when the stacks kept none
    pub line: i64,
    pub bytes: f64,
    pub count: f64,
    /// By phase index: (bytes, count)
    pub phases: BTreeMap<u32, (f64, f64)>,
    /// By type over the run, from the stacks profile.json kept: (type, bytes, count), largest first
    pub types: Vec<(String, f64, f64)>,
    /// The `alloc.by_stack` rows of this site, heaviest first
    pub stacks: Vec<usize>,
}

impl AllocSite {
    /// (bytes, count) a frame in a phase
    pub fn per_frame(&self, phase: u32, frames: u64) -> (f64, f64) {
        let (bytes, count) = self.phases.get(&phase).copied().unwrap_or_default();
        if frames == 0 { (0.0, 0.0) } else { (bytes / frames as f64, count / frames as f64) }
    }

    /// The type's (bytes, count) at this site
    pub fn of_type(&self, ty: &str) -> (f64, f64) {
        self.types.iter().find(|row| row.0 == ty).map_or((0.0, 0.0), |row| (row.1, row.2))
    }
}

/// A frame a site can be: a script's Luau line. (The engine's own Luau, its require among it, is passed through like a
/// C function: what it allocates is the line's that called into it.)
fn luau(p: &Profile, func: u32) -> bool {
    p.funcs.get(func as usize).is_some_and(|f| f.kind == Kind::Script && !f.module.is_empty())
}

/// Every allocation site of a profile, heaviest first
pub fn alloc_sites(p: &Profile) -> Vec<AllocSite> {
    let Some(alloc) = &p.raw.alloc else { return Vec::new() };
    // the JSON's stacks, by the key a folded row has: (frames outermost first, the innermost frame's line)
    let mut by_key: FxHashMap<(Vec<u32>, Option<i64>), Vec<usize>> = FxHashMap::default();
    let mut site_of_stack: Vec<Option<(u32, i64)>> = Vec::with_capacity(alloc.by_stack.len());
    for (index, stack) in alloc.by_stack.iter().enumerate() {
        let Some(frames) = &stack.frames else {
            site_of_stack.push(None);
            continue;
        };
        let funcs: Option<Vec<u32>> = frames.iter().rev().map(|(id, _)| p.func_of_id(*id).map(|f| f as u32)).collect();
        let innermost_line = frames.first().map(|frame| frame.1).filter(|line| *line >= 0);
        let site = frames.iter().find_map(|(id, line)| p.func_of_id(*id).map(|f| f as u32).filter(|&f| luau(p, f)).map(|f| (f, *line))).unwrap_or((ENGINE, -1));
        site_of_stack.push(Some(site));
        if let Some(funcs) = funcs {
            by_key.entry((funcs, innermost_line)).or_default().push(index);
        }
    }
    let mut sites: BTreeMap<(u32, i64), AllocSite> = BTreeMap::new();
    let folded_site = |frames: &[u32], line: Option<i64>| -> (u32, i64) {
        if let Some(stacks) = by_key.get(&(frames.to_vec(), line)) {
            if let Some(Some(site)) = stacks.first().map(|&index| site_of_stack[index]) {
                return site;
            }
        }
        match frames.iter().rposition(|&f| luau(p, f)) {
            Some(at) => (frames[at], if at + 1 == frames.len() { line.unwrap_or(-1) } else { -1 }),
            None => (ENGINE, -1),
        }
    };
    let bytes = p.folded.get("alloc");
    let counts = p.folded.get("alloc.counts");
    if bytes.is_some() || counts.is_some() {
        for (folded, is_bytes) in [(bytes, true), (counts, false)] {
            for row in folded.map(|f| f.rows.as_slice()).unwrap_or(&[]) {
                let key = folded_site(&row.frames, row.line);
                let site = sites.entry(key).or_insert_with(|| AllocSite { func: key.0, line: key.1, ..AllocSite::default() });
                let phase = site.phases.entry(row.phase.unwrap_or(0)).or_default();
                if is_bytes {
                    site.bytes += row.weight;
                    phase.0 += row.weight;
                } else {
                    site.count += row.weight;
                    phase.1 += row.weight;
                }
            }
        }
    } else {
        // no folded stacks: the JSON's, with no phases
        for (index, stack) in alloc.by_stack.iter().enumerate() {
            let key = site_of_stack[index].unwrap_or((ENGINE, -1));
            let site = sites.entry(key).or_insert_with(|| AllocSite { func: key.0, line: key.1, ..AllocSite::default() });
            site.bytes += stack.bytes;
            site.count += stack.count;
        }
    }
    // types and stacks from the JSON
    for (index, stack) in alloc.by_stack.iter().enumerate() {
        let key = site_of_stack[index].unwrap_or((ENGINE, -1));
        let site = sites.entry(key).or_insert_with(|| AllocSite { func: key.0, line: key.1, ..AllocSite::default() });
        match site.types.iter_mut().find(|row| row.0 == stack.ty) {
            Some(row) => {
                row.1 += stack.bytes;
                row.2 += stack.count;
            }
            None => site.types.push((stack.ty.clone(), stack.bytes, stack.count)),
        }
        site.stacks.push(index);
    }
    let mut out: Vec<AllocSite> = sites.into_values().collect();
    for site in &mut out {
        site.types.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        site.stacks.sort_by(|&a, &b| alloc.by_stack[b].bytes.total_cmp(&alloc.by_stack[a].bytes).then(a.cmp(&b)));
    }
    out.sort_by(|a, b| b.bytes.total_cmp(&a.bytes).then(b.count.total_cmp(&a.count)).then((a.func, a.line).cmp(&(b.func, b.line))));
    out
}

/// A site's name: `rank  Demo:24`, `rank  Demo (line not kept)`, `(engine)`
pub fn site_label(p: &Profile, site: &AllocSite) -> String {
    if site.func == ENGINE {
        return "(engine)".into();
    }
    let func = &p.funcs[site.func as usize];
    if site.line >= 0 { format!("{}  {}:{}", func.name, func.short, site.line) } else { format!("{}  {} (no line)", func.name, func.short) }
}

//! Caches: Luau's inline caches by kind and by call site (hit, repredicted, slow path), and ree's own caches.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::kit::{self, Item, col, col_fill, col_r};
use crate::{
    app::{App, CacheSort, Focus, View},
    fmt,
    keys::Key,
    model::Kind,
    theme::{Mark, Theme},
};

/// The call sites in the order shown (indexes into `luau_caches.sites`)
pub fn order(app: &App) -> Vec<usize> {
    let p = &app.profiles[app.shown];
    let Some(caches) = &p.raw.luau_caches else { return Vec::new() };
    let mut order: Vec<usize> = (0..caches.sites.len())
        .filter(|&i| {
            let site = &caches.sites[i];
            let kind = p.func_of_id(site.function).map(|f| p.funcs[f].kind);
            // (the miss rankings leave out sites that never missed; the rate's, sites with under 100 lookups)
            let ranked = match app.caches.sort {
                CacheSort::Misses => site.missed() > 0.0,
                CacheSort::Rate => site.missed() > 0.0 && site.runs() >= 100.0,
                CacheSort::Runs => true,
            };
            ranked && kind != Some(Kind::Engine) && (!app.scripts_only || kind == Some(Kind::Script))
        })
        .collect();
    let key = |i: usize| -> f64 {
        let site = &caches.sites[i];
        match app.caches.sort {
            CacheSort::Misses => site.missed(),
            CacheSort::Rate => {
                if site.runs() > 0.0 {
                    site.missed() / site.runs()
                } else {
                    0.0
                }
            }
            CacheSort::Runs => site.runs(),
        }
    };
    order.sort_by(|&a, &b| key(b).total_cmp(&key(a)).then(a.cmp(&b)));
    order
}

fn site_label(app: &App, index: usize) -> String {
    let p = &app.profiles[app.shown];
    let Some(site) = p.raw.luau_caches.as_ref().and_then(|c| c.sites.get(index)) else { return String::new() };
    match p.func_of_id(site.function) {
        Some(func) => {
            let f = &p.funcs[func];
            format!("{}  {}:{}", f.name, f.short, site.line)
        }
        None => format!("function {} line {}", site.function, site.line),
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let order = order(app);
    let mut list = app.caches.list;
    let p = &app.profiles[app.shown];
    let Some(caches) = &p.raw.luau_caches else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Caches", "", false));
        kit::empty(buf, inner, theme, "no cache counts in this profile (--profile caches)");
        return;
    };
    let ree: Vec<&crate::model::ReeCache> = p.raw.ree_caches.iter().flatten().filter(|c| c.hits + c.misses + c.evictions > 0.0).collect();
    let kinds_height = caches.by_kind.len() as u16 + 3;
    let ree_height = if ree.is_empty() { 0 } else { (ree.len() as u16 + 3).min(area.height / 3) };
    let parts = kit::rows(area, &[kinds_height, area.height.saturating_sub(kinds_height + ree_height), ree_height]);
    // Luau's inline caches by kind: each one's hit rate as a gauge
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, "Luau inline caches", "miss: predicted wrong · slow: a metatable or an unsafe environment · other: not a table", false));
    let cols = [col("cache", 18), col_fill("hit rate", 2), col_r("lookups", 11), col_r("misses", 10), col_r("slow", 10), col_r("other", 10)];
    let gauge_col = kit::widths(&cols, inner.width)[1];
    let rows = caches
        .by_kind
        .iter()
        .map(|kind| {
            let table = kind.hits + kind.misses + kind.slow;
            let rate = if table > 0.0 { kind.hits / table } else { 0.0 };
            let filled = ((rate * (gauge_col.saturating_sub(7)) as f64).round() as usize).min(gauge_col.saturating_sub(7) as usize);
            let rate_cell = if table > 0.0 {
                Item::Spans(Line::from(vec![
                    Span::styled(format!("{:>6} ", format!("{:.1}%", rate * 100.0)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
                    Span::styled("━".repeat(filled), Style::new().fg(theme.slot(0))),
                    Span::styled("━".repeat(gauge_col.saturating_sub(7) as usize - filled), Style::new().fg(theme.faint)),
                ]))
            } else {
                Item::text("-", Style::new().fg(theme.muted))
            };
            vec![
                Item::text(kind.kind.clone(), Style::new().fg(theme.ink)),
                rate_cell,
                Item::text(fmt::grouped(kind.hits + kind.misses + kind.slow + kind.other), Style::new().fg(theme.ink2)),
                Item::text(fmt::grouped(kind.misses), Style::new().fg(theme.ink2)),
                Item::text(fmt::grouped(kind.slow), Style::new().fg(theme.ink2)),
                Item::text(fmt::grouped(kind.other), Style::new().fg(theme.ink2)),
            ]
        })
        .collect();
    kit::table(buf, inner, theme, &cols, rows, None, 0);
    // the call sites
    let sort = match app.caches.sort {
        CacheSort::Misses => "by misses",
        CacheSort::Rate => "by miss rate, 100 lookups or more",
        CacheSort::Runs => "by lookups",
    };
    let all_missed: f64 = caches.sites.iter().map(|s| s.missed()).sum();
    let rows_shown = parts[1].height.saturating_sub(3) as usize;
    list.fit(order.len(), rows_shown);
    let scroll = if order.len() > rows_shown { format!("{}-{} of {}", fmt::grouped((list.offset + 1) as f64), fmt::grouped((list.offset + rows_shown).min(order.len()) as f64), fmt::grouped(order.len() as f64)) } else { String::new() };
    let block = kit::bottom(kit::note_right(kit::panel(theme, "Call sites", &format!("{} · {sort}", fmt::grouped(order.len() as f64)), true), theme, &scroll), super::legend(app, theme, area.width));
    let inner = kit::draw_panel(buf, parts[1], block);
    if order.is_empty() {
        kit::empty(buf, inner, theme, if app.caches.sort == CacheSort::Runs { "none" } else { "no misses (s: by lookups)" });
    } else {
        let cols = [col("op", 11), col_fill("site", 3), col_r("lookups", 11), col_r("hits", 11), col_r("misses", 10), col_r("slow", 10), col_r("missed", 7), col_fill("", 2)];
        let bar_width = kit::widths(&cols, inner.width)[7];
        let max = order.first().map_or(0.0, |&i| caches.sites[i].missed());
        let rows = order
            .iter()
            .map(|&index| {
                let site = &caches.sites[index];
                let missed = site.missed();
                let share = if all_missed > 0.0 { missed / all_missed } else { 0.0 };
                let style = theme.row(share);
                vec![
                    Item::text(site.op.clone(), Style::new().fg(theme.ink2)),
                    Item::label(site_label(app, index), style),
                    Item::text(fmt::grouped(site.runs()), Style::new().fg(theme.ink2)),
                    Item::text(fmt::grouped(site.hits), Style::new().fg(theme.ink2)),
                    Item::text(fmt::grouped(site.misses), style),
                    Item::text(fmt::grouped(site.slow), style),
                    Item::text(fmt::share(missed, site.runs()), style),
                    Item::Spans(kit::bar(if max > 0.0 { missed / max } else { 0.0 }, bar_width, theme.heat_of(share))),
                ]
            })
            .collect();
        let grid = kit::table(buf, inner, theme, &cols, rows, Some(list.selected), list.offset);
        kit::mark(buf, grid.column(7), Mark::Bar);
        list.offset = grid.offset;
    }
    app.caches.list = list;
    if ree_height == 0 {
        return;
    }
    // ree's own caches
    let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, "ree's caches", "the emulator's own", false));
    let cols = [col("cache", 20), col_r("hit rate", 9), col_r("lookups", 12), col_r("misses", 10), col_r("evicted", 10), col_fill("what it keeps", 1)];
    let rows = ree
        .iter()
        .map(|cache| {
            let lookups = cache.hits + cache.misses;
            vec![
                Item::text(cache.name.clone(), Style::new().fg(theme.ink)),
                Item::text(fmt::share(cache.hits, lookups), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
                Item::text(fmt::grouped(lookups), Style::new().fg(theme.ink2)),
                Item::text(fmt::grouped(cache.misses), Style::new().fg(theme.ink2)),
                Item::text(fmt::grouped(cache.evictions), Style::new().fg(theme.ink2)),
                Item::text(cache.about.clone(), Style::new().fg(theme.muted)),
            ]
        })
        .collect();
    kit::table(buf, inner, theme, &cols, rows, None, 0);
}

pub fn key(app: &mut App, key: Key) {
    let order = order(app);
    if app.caches.list.key(key, order.len(), app.page) {
        return;
    }
    match key {
        Key::Char('s') => {
            app.caches.sort = match app.caches.sort {
                CacheSort::Misses => CacheSort::Rate,
                CacheSort::Rate => CacheSort::Runs,
                CacheSort::Runs => CacheSort::Misses,
            };
            app.caches.list.selected = 0;
        }
        Key::Enter => {
            let p = &app.profiles[app.shown];
            let Some(site) = order.get(app.caches.list.selected).and_then(|&i| p.raw.luau_caches.as_ref().map(|c| c.sites[i].clone())) else { return };
            if let Some(func) = p.func_of_id(site.function) {
                app.focus = Some(Focus { func: func as u32, line: Some(site.line) });
                app.source.module = None;
                app.source.line = site.line;
                app.set_view(View::Source);
            }
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let order = order(app);
    let p = &app.profiles[app.shown];
    let site = order.get(app.caches.list.selected).and_then(|&i| p.raw.luau_caches.as_ref().map(|c| &c.sites[i]))?;
    let func = p.func_of_id(site.function)?;
    super::calls::place_text(app, func as u32, Some(site.line))
}

pub fn find(app: &mut App, query: &str, forward: bool, from_start: bool) -> bool {
    let order = order(app);
    match super::next_match(order.len(), app.caches.list.selected, forward, from_start, |i| site_label(app, order[i]).to_lowercase().contains(query)) {
        Some(found) => {
            app.caches.list.selected = found;
            true
        }
        None => false,
    }
}

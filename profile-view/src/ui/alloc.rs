//! Allocations: what the run allocated by site (the Luau line), with each site's share, object size, rate a frame in
//! the steady state and types; the run's totals by type; the selected site's stacks.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::kit::{self, Item, col_fill, col_r};
use crate::{
    app::{AllocSort, App, Focus, View},
    fmt,
    keys::Key,
    metric::Metric,
    model::Kind,
    sites::{AllocSite, ENGINE, site_label},
    theme::{Mark, Theme},
};

/// The sites in the order shown
pub fn order(app: &App, sites: &[AllocSite]) -> Vec<usize> {
    let p = &app.profiles[app.shown];
    let steady = p.steady_phase();
    let mut order: Vec<usize> = (0..sites.len()).filter(|&i| !app.scripts_only || (sites[i].func != ENGINE && p.funcs[sites[i].func as usize].kind == Kind::Script)).collect();
    let key = |site: &AllocSite| -> f64 {
        match app.alloc.sort {
            AllocSort::Bytes => {
                if app.metric == Metric::AllocCount {
                    site.count
                } else {
                    site.bytes
                }
            }
            AllocSort::Count => site.count,
            AllocSort::SteadyBytes => steady.map_or(0.0, |(phase, frames)| site.per_frame(phase, frames).0),
            AllocSort::Size => {
                if site.count > 0.0 {
                    site.bytes / site.count
                } else {
                    0.0
                }
            }
        }
    };
    order.sort_by(|&a, &b| key(&sites[b]).total_cmp(&key(&sites[a])).then(a.cmp(&b)));
    order
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let sites = app.sites();
    let order = order(app, &sites);
    let mut list = app.alloc.list;
    let p = &app.profiles[app.shown];
    let Some(alloc) = &p.raw.alloc else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Allocations", "", false));
        kit::empty(buf, inner, theme, "no allocations in this profile (--profile alloc)");
        return;
    };
    let steady = p.steady_phase();
    let by_count = app.metric == Metric::AllocCount;
    let stacks_height = 6u16;
    let parts = kit::rows(area, &[5, area.height.saturating_sub(5 + stacks_height), stacks_height]);
    // the totals: a frame's in the steady state, and the shares by type as gauges
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, "Allocated", &format!("{} · {} objects over the run", fmt::bytes(alloc.bytes), fmt::grouped(alloc.objects)), false));
    let mut line = Vec::new();
    if let (Some((phase, frames)), Some(timeline)) = (steady, &p.raw.timeline) {
        let phases = timeline.values("phase").unwrap_or_default();
        let bytes = timeline.values("allocated_bytes").unwrap_or_default();
        let counts = timeline.values("allocations").unwrap_or_default();
        let rows: Vec<usize> = (0..phases.len()).filter(|&i| phases[i] as u32 == phase).collect();
        if !rows.is_empty() {
            let mean = |values: &[f64]| rows.iter().map(|&i| values.get(i).copied().unwrap_or(0.0)).sum::<f64>() / rows.len() as f64;
            let of = p.phase_words(phase).map(|words| format!(" of {words}")).unwrap_or_default();
            line.push(Span::styled(format!("a frame{of}  "), Style::new().fg(theme.muted)));
            line.push(Span::styled(fmt::bytes(mean(&bytes)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)));
            line.push(Span::styled(format!(", {} objects", fmt::grouped(mean(&counts))), Style::new().fg(theme.ink)));
            line.push(Span::styled(format!("  (mean of {} frames)", fmt::grouped(frames as f64)), Style::new().fg(theme.muted)));
        }
    }
    if alloc.step > 1.0 {
        line.push(Span::styled(format!("   stacks sampled every {} bytes", fmt::grouped(alloc.step)), Style::new().fg(theme.serious)));
    }
    Paragraph::new(Line::from(line)).render(Rect { height: 1, ..inner }, buf);
    let mut groups: Vec<(String, f64, f64)> = Vec::new();
    for row in &alloc.by_type {
        let group = if row.group.is_empty() { row.ty.clone() } else { row.group.clone() };
        match groups.iter_mut().find(|g| g.0 == group) {
            Some(g) => {
                g.1 += row.bytes;
                g.2 += row.count;
            }
            None => groups.push((group, row.bytes, row.count)),
        }
    }
    groups.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let per_row = 3usize;
    let gauge_width = inner.width / per_row as u16;
    for (index, (name, bytes, _)) in groups.iter().take(per_row * 2).enumerate() {
        let x = inner.x + (index % per_row) as u16 * gauge_width;
        let y = inner.y + 1 + (index / per_row) as u16;
        if y >= inner.bottom() {
            break;
        }
        let label = Line::from(vec![Span::styled(format!("{:<22}", fmt::fit(name, 22)), Style::new().fg(theme.ink2)), Span::styled(format!("{:>6}", fmt::share(*bytes, alloc.bytes)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD))]);
        kit::gauge(buf, Rect { x, y, width: gauge_width.saturating_sub(2), height: 1 }, theme, if alloc.bytes > 0.0 { bytes / alloc.bytes } else { 0.0 }, label, theme.slot(0));
    }
    // the sites
    let sort = match app.alloc.sort {
        AllocSort::Bytes => {
            if by_count {
                "objects"
            } else {
                "bytes"
            }
        }
        AllocSort::Count => "objects",
        AllocSort::SteadyBytes => "bytes a frame",
        AllocSort::Size => "object size",
    };
    let block = kit::panel(theme, "Sites", &format!("{} · by {sort}", fmt::grouped(order.len() as f64)), true);
    let inner_rows = parts[1].height.saturating_sub(3) as usize;
    list.fit(order.len(), inner_rows);
    let scroll = if order.len() > inner_rows { format!("{}-{} of {}", fmt::grouped((list.offset + 1) as f64), fmt::grouped((list.offset + inner_rows).min(order.len()) as f64), fmt::grouped(order.len() as f64)) } else { String::new() };
    let block = kit::bottom(kit::note_right(block, theme, &scroll), super::legend(app, theme, area.width));
    let inner = kit::draw_panel(buf, parts[1], block);
    let cols = [
        col_fill("site", 3),
        col_r("bytes", 10),
        col_r("objects", 8),
        col_r("size", 9),
        // (the steady state's, which the panel above names; none without one)
        if steady.is_some() { col_r("a frame: bytes, objects", 23) } else { col_r("", 0) },
        col_fill("types", 3),
        col_fill("", 2),
    ];
    let bar_width = kit::widths(&cols, inner.width)[6];
    let total = if by_count { alloc.objects } else { alloc.bytes };
    let max = order.first().map_or(0.0, |&i| if by_count { sites[i].count } else { sites[i].bytes });
    let rows = order
        .iter()
        .map(|&index| {
            let site = &sites[index];
            let value = if by_count { site.count } else { site.bytes };
            let share = if total > 0.0 { value / total } else { 0.0 };
            let style = theme.row(share);
            let frame = steady.map(|(phase, frames)| site.per_frame(phase, frames)).filter(|(_, count)| *count > 0.0).map(|(bytes, count)| format!("{}, {}", fmt::bytes(bytes), fmt::compact(count))).unwrap_or_default();
            let types: Vec<String> = site.types.iter().take(2).map(|(ty, bytes, _)| format!("{ty} {}", fmt::share(*bytes, site.bytes.max(1.0)))).collect();
            vec![
                Item::label(site_label(p, site), style),
                Item::text(fmt::bytes(site.bytes), style),
                Item::text(fmt::compact(site.count), style),
                Item::text(if site.count > 0.0 { fmt::bytes(site.bytes / site.count) } else { "-".into() }, Style::new().fg(theme.ink2)),
                Item::text(frame, style),
                Item::text(types.join(", "), Style::new().fg(theme.ink2)),
                Item::Spans(kit::bar(if max > 0.0 { value / max } else { 0.0 }, bar_width, theme.heat_of(share))),
            ]
        })
        .collect();
    let grid = kit::table(buf, inner, theme, &cols, rows, Some(list.selected), list.offset);
    kit::mark(buf, grid.column(6), Mark::Bar);
    list.offset = grid.offset;
    app.alloc.list = list;
    // the selected site's stacks
    let Some(site) = order.get(list.selected).map(|&i| &sites[i]) else { return };
    let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, "Stacks", &site_label(p, site), false));
    if site.stacks.is_empty() {
        kit::empty(buf, inner, theme, "not among the 500 stacks the profile kept");
        return;
    }
    let lines: Vec<Line> = site
        .stacks
        .iter()
        .take(inner.height as usize)
        .map(|&stack_index| {
            let stack = &alloc.by_stack[stack_index];
            let frames: Vec<String> = stack.frames.iter().flatten().filter_map(|(id, line)| {
                let func = p.func_of_id(*id)?;
                let f = &p.funcs[func];
                Some(if f.module.is_empty() { f.name.clone() } else { format!("{}:{line}", f.short) })
            }).take(8).collect();
            let mut spans = vec![
                Span::styled(format!("{:>10} ", fmt::bytes(stack.bytes)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{:>8} ", fmt::compact(stack.count)), Style::new().fg(theme.ink2)),
                Span::styled(format!("{:<16} ", fmt::fit(&stack.ty, 16)), Style::new().fg(theme.ink2)),
            ];
            for (index, frame) in frames.iter().enumerate() {
                if index > 0 {
                    spans.push(Span::styled(" ‹ ", Style::new().fg(theme.faint)));
                }
                spans.push(Span::styled(frame.clone(), Style::new().fg(if index == 0 { theme.ink } else { theme.ink2 })));
            }
            Line::from(spans)
        })
        .collect();
    Paragraph::new(lines).render(inner, buf);
}

pub fn key(app: &mut App, key: Key) {
    let sites = app.sites();
    let order = order(app, &sites);
    if app.alloc.list.key(key, order.len(), app.page) {
        if let Some(site) = order.get(app.alloc.list.selected).map(|&i| &sites[i]) {
            if site.func != ENGINE {
                app.focus = Some(Focus { func: site.func, line: (site.line >= 0).then_some(site.line) });
            }
        }
        return;
    }
    match key {
        Key::Char('s') => {
            app.alloc.sort = match app.alloc.sort {
                AllocSort::Bytes => AllocSort::Count,
                AllocSort::Count => AllocSort::SteadyBytes,
                AllocSort::SteadyBytes => AllocSort::Size,
                AllocSort::Size => AllocSort::Bytes,
            };
            app.alloc.list.selected = 0;
        }
        Key::Enter => {
            if let Some(site) = order.get(app.alloc.list.selected).map(|&i| &sites[i]) {
                if site.func != ENGINE {
                    let (func, line) = (site.func, site.line);
                    app.focus = Some(Focus { func, line: (line >= 0).then_some(line) });
                    app.source.module = None;
                    app.source.line = if line >= 0 { line } else { app.profile().funcs[func as usize].line };
                    app.set_view(View::Source);
                }
            }
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let sites = app.sites();
    let order = order(app, &sites);
    let site = order.get(app.alloc.list.selected).map(|&i| &sites[i])?;
    if site.func == ENGINE {
        return None;
    }
    super::calls::place_text(app, site.func, (site.line >= 0).then_some(site.line))
}

pub fn find(app: &mut App, query: &str, forward: bool, from_start: bool) -> bool {
    let sites = app.sites();
    let order = order(app, &sites);
    let p = &app.profiles[app.shown];
    match super::next_match(order.len(), app.alloc.list.selected, forward, from_start, |i| site_label(p, &sites[order[i]]).to_lowercase().contains(query)) {
        Some(found) => {
            app.alloc.list.selected = found;
            true
        }
        None => false,
    }
}

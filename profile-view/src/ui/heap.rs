//! Heap: the reachable heap at each snapshot (the start, every phase mark, the end), what holds the most of it, its
//! types, the run's own reachable objects by the stack that allocated them, and the collector's work frame by frame.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Tabs, Widget},
};

use super::kit::{self, Item, col, col_fill, col_r};
use crate::{
    app::{App, Focus, HeapPane, View},
    fmt,
    keys::Key,
    model::{Profile, Snapshot},
    theme::{Mark, Theme},
};

fn snapshot_index(app: &App) -> Option<usize> {
    let snapshots = &app.profile().raw.heap.as_ref()?.snapshots;
    if snapshots.is_empty() {
        return None;
    }
    Some(app.heap.snapshot.unwrap_or(snapshots.len() - 1).min(snapshots.len() - 1))
}

/// A stack's frames as `Module:line < Module:line`
fn stack_text(p: &Profile, frames: &[(i64, i64)]) -> String {
    let parts: Vec<String> = frames.iter().filter_map(|(id, line)| {
        let func = p.func_of_id(*id)?;
        let f = &p.funcs[func];
        Some(if f.module.is_empty() || *line < 0 { if f.module.is_empty() { f.name.clone() } else { f.short.clone() } } else { format!("{}:{line}", f.short) })
    }).take(6).collect();
    if parts.is_empty() { "(engine)".into() } else { parts.join(" < ") }
}

fn pane_len(snapshot: &Snapshot, pane: HeapPane) -> usize {
    match pane {
        HeapPane::Holders => snapshot.biggest.len(),
        HeapPane::Types => snapshot.by_type.len(),
        HeapPane::Stacks => snapshot.by_stack.len(),
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(index) = snapshot_index(app) else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Heap", "", false));
        kit::empty(buf, inner, theme, "no heap snapshots in this profile (--profile heap)");
        return;
    };
    let mut list = app.heap.list;
    let pane = app.heap.pane;
    let p = &app.profiles[app.shown];
    let heap = p.raw.heap.as_ref().unwrap();
    let snapshot = &heap.snapshots[index];
    let (reachable, objects, unswept) = snapshot.split();
    let collector_height = 5u16;
    let summary_height = 5u16;
    let body = area.height.saturating_sub(summary_height + collector_height);
    let parts = kit::rows(area, &[summary_height, body / 2, body - body / 2, collector_height]);
    // the snapshots as tabs, and the selected one's heap as shares
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, "Heap", &format!("{} snapshots · ←→", heap.snapshots.len()), false));
    let mut previous: Option<f64> = None;
    let titles: Vec<Line> = heap
        .snapshots
        .iter()
        .map(|s| {
            let (bytes, _, _) = s.split();
            let delta = previous.map(|before| format!(" {}", fmt::signed(crate::metric::Unit::Bytes, bytes - before))).unwrap_or_default();
            previous = Some(bytes);
            Line::from(format!(" {} · frame {} · {}{delta} ", s.label, s.frame, fmt::bytes(bytes)))
        })
        .collect();
    Tabs::new(titles)
        .select(index)
        .style(Style::new().fg(theme.ink2).bg(theme.panel))
        .highlight_style(Style::new().fg(theme.accent_ink).bg(theme.accent).add_modifier(Modifier::BOLD))
        .divider(Span::styled(" → ", Style::new().fg(theme.muted)))
        .padding("", "")
        .render(Rect { height: 1, ..inner }, buf);
    if inner.height >= 3 {
        let half = inner.width / 2;
        let heap_total = reachable + unswept;
        let label = Line::from(vec![Span::styled("reachable ", Style::new().fg(theme.muted)), Span::styled(format!("{} ", fmt::bytes(reachable)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)), Span::styled(format!("{} objects · unswept {} ", fmt::compact(objects), fmt::bytes(unswept)), Style::new().fg(theme.ink2))]);
        kit::gauge(buf, Rect { y: inner.y + 2, width: half.saturating_sub(2), height: 1, ..inner }, theme, if heap_total > 0.0 { reachable / heap_total } else { 0.0 }, label, theme.slot(0));
        let own = snapshot.allocated_since.bytes;
        let label = Line::from(vec![Span::styled("the run's own ", Style::new().fg(theme.muted)), Span::styled(format!("{} ", fmt::bytes(own)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)), Span::styled(format!("{} of reachable ", fmt::share(own, reachable)), Style::new().fg(theme.ink2))]);
        kit::gauge(buf, Rect { x: inner.x + half, y: inner.y + 2, width: inner.width - half, height: 1 }, theme, if reachable > 0.0 { own / reachable } else { 0.0 }, label, theme.slot(1));
    }
    // what holds the most, and the heap by type
    let (left, right) = kit::split(parts[1], parts[1].width * 3 / 5);
    let inner = kit::draw_panel(buf, left, kit::panel(theme, "Biggest holders", "what each keeps alive", pane == HeapPane::Holders));
    if pane == HeapPane::Holders {
        list.fit(pane_len(snapshot, pane), kit::rows_shown(inner));
    }
    let cols = [col_r("keeps", 10), col_r("own", 9), col_fill("object", 2), col_fill("held by", 3)];
    let rows = snapshot
        .biggest
        .iter()
        .map(|held| {
            let share = if reachable > 0.0 { held.retained / reachable } else { 0.0 };
            let style = theme.row(share);
            let name = if held.name.is_empty() { held.ty.clone() } else { format!("{} \"{}\"", held.ty, held.name) };
            vec![Item::text(fmt::bytes(held.retained), style), Item::text(fmt::bytes(held.size), Style::new().fg(theme.ink2)), Item::text(name, style), Item::text(held.path.clone(), Style::new().fg(theme.ink2))]
        })
        .collect();
    let (selected, offset) = if pane == HeapPane::Holders { (Some(list.selected), list.offset) } else { (None, 0) };
    let grid = kit::table(buf, inner, theme, &cols, rows, selected, offset);
    if pane == HeapPane::Holders {
        list.offset = grid.offset;
    }
    let inner = kit::draw_panel(buf, right, kit::panel(theme, "By type", &format!("{} reachable", fmt::bytes(reachable)), pane == HeapPane::Types));
    if pane == HeapPane::Types {
        list.fit(pane_len(snapshot, pane), kit::rows_shown(inner));
    }
    let cols = [col_fill("type", 3), col_r("bytes", 9), col_r("count", 7), col_fill("", 2)];
    let bar_width = kit::widths(&cols, inner.width)[3];
    let max = snapshot.by_type.first().map_or(0.0, |t| t.bytes);
    let rows = snapshot
        .by_type
        .iter()
        .map(|ty| {
            let share = if reachable > 0.0 { ty.bytes / reachable } else { 0.0 };
            vec![Item::text(ty.ty.clone(), theme.row(share)), Item::text(fmt::bytes(ty.bytes), Style::new().fg(theme.ink2)), Item::text(fmt::compact(ty.count), Style::new().fg(theme.ink2)), Item::Spans(kit::bar(if max > 0.0 { ty.bytes / max } else { 0.0 }, bar_width, theme.heat_of(share)))]
        })
        .collect();
    let (selected, offset) = if pane == HeapPane::Types { (Some(list.selected), list.offset) } else { (None, 0) };
    let grid = kit::table(buf, inner, theme, &cols, rows, selected, offset);
    kit::mark(buf, grid.column(3), Mark::Bar);
    if pane == HeapPane::Types {
        list.offset = grid.offset;
    }
    // the run's own reachable objects, by the stack that allocated them
    let note = format!("{}, {} objects, by stack", fmt::bytes(snapshot.allocated_since.bytes), fmt::grouped(snapshot.allocated_since.count));
    let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, "Allocated by the run, still reachable", &note, pane == HeapPane::Stacks));
    if snapshot.by_stack.is_empty() {
        kit::empty(buf, inner, theme, "not followed (needs the alloc kind with heap)");
    } else {
        if pane == HeapPane::Stacks {
            list.fit(pane_len(snapshot, pane), kit::rows_shown(inner));
        }
        let cols = [col_r("bytes", 10), col_r("objects", 9), col("type", 18), col_fill("stack, innermost first", 1)];
        let rows = snapshot
            .by_stack
            .iter()
            .map(|row| {
                let share = if reachable > 0.0 { row.bytes / reachable } else { 0.0 };
                vec![Item::text(fmt::bytes(row.bytes), theme.row(share)), Item::text(fmt::compact(row.count), Style::new().fg(theme.ink2)), Item::text(row.ty.clone(), Style::new().fg(theme.ink2)), Item::Spans(stack_line(p, row.frames.as_deref().unwrap_or(&[]), theme))]
            })
            .collect();
        let (selected, offset) = if pane == HeapPane::Stacks { (Some(list.selected), list.offset) } else { (None, 0) };
        let grid = kit::table(buf, inner, theme, &cols, rows, selected, offset);
        if pane == HeapPane::Stacks {
            list.offset = grid.offset;
        }
    }
    app.heap.list = list;
    // the collector
    let gc = &heap.gc;
    let inner = kit::draw_panel(buf, parts[3], kit::panel(theme, "Collector", &format!("{} cycles · {} steps · {} freed · {} of host time", fmt::grouped(gc.cycles), fmt::grouped(gc.steps), fmt::bytes(gc.freed), fmt::ms(gc.ms)), false));
    let mut spans: Vec<Span> = Vec::new();
    for state in gc.by_state.iter().filter(|s| s.steps > 0.0) {
        if !spans.is_empty() {
            spans.push(Span::styled("  ·  ", Style::new().fg(theme.faint)));
        }
        spans.push(Span::styled(format!("{} ", state.state), Style::new().fg(theme.muted)));
        spans.push(Span::styled(fmt::ms(state.ms), Style::new().fg(theme.ink)));
        spans.push(Span::styled(format!(" ({} steps)", fmt::grouped(state.steps)), Style::new().fg(theme.ink2)));
    }
    Paragraph::new(Line::from(spans)).render(Rect { height: 1, ..inner }, buf);
    if let Some(timeline) = &p.raw.timeline {
        // (by steps, the same every run of the same code, as the collector spikes are found; its milliseconds are host
        // time)
        let series = timeline.values("gc_steps").map(|v| (v, "steps a frame ", true)).or_else(|| timeline.values("gc_ms").map(|v| (v, "time a frame ", false)));
        if let (Some((per_frame, label, steps)), true) = (series, inner.height >= 3) {
            let y = inner.y + 2;
            let label_width = label.chars().count() as u16;
            let width = inner.width.saturating_sub(label_width + 20) as usize;
            let values = kit::bucket_max(&per_frame, width);
            let max = values.iter().copied().fold(0.0, f64::max);
            let top = if steps { format!(" max {} steps", fmt::grouped(max)) } else { format!(" max {}", fmt::ms(max)) };
            let mut line = vec![Span::styled(label, Style::new().fg(theme.muted))];
            line.extend(kit::spark(&values, max, |_| theme.slot(0)).spans);
            line.push(Span::styled(top, Style::new().fg(theme.muted)));
            Paragraph::new(Line::from(line)).render(Rect { y, height: 1, ..inner }, buf);
            kit::mark(buf, Rect { x: inner.x + label_width, y, width: values.len() as u16, height: 1 }, Mark::Column);
        }
    }
}

/// A stack's frames, innermost first: `Module:line ‹ Module:line`, the innermost in ink
fn stack_line(p: &Profile, frames: &[(i64, i64)], theme: &Theme) -> Line<'static> {
    let text = stack_text(p, frames);
    let mut spans = Vec::new();
    for (index, part) in text.split(" < ").enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ‹ ", Style::new().fg(theme.faint)));
        }
        spans.push(Span::styled(part.to_string(), Style::new().fg(if index == 0 { theme.ink } else { theme.ink2 })));
    }
    Line::from(spans)
}

pub fn key(app: &mut App, key: Key) {
    let Some(index) = snapshot_index(app) else { return };
    let count = app.profile().raw.heap.as_ref().map_or(0, |h| h.snapshots.len());
    match key {
        Key::Left => {
            app.heap.snapshot = Some(index.saturating_sub(1));
            app.heap.list = Default::default();
        }
        Key::Right => {
            app.heap.snapshot = Some((index + 1).min(count.saturating_sub(1)));
            app.heap.list = Default::default();
        }
        Key::Tab | Key::Char(' ') => {
            app.heap.pane = match app.heap.pane {
                HeapPane::Holders => HeapPane::Types,
                HeapPane::Types => HeapPane::Stacks,
                HeapPane::Stacks => HeapPane::Holders,
            };
            app.heap.list = Default::default();
        }
        Key::Enter => {
            // a stack's innermost Luau line
            if app.heap.pane == HeapPane::Stacks {
                let p = &app.profiles[app.shown];
                let snapshot = &p.raw.heap.as_ref().unwrap().snapshots[index];
                if let Some(frames) = snapshot.by_stack.get(app.heap.list.selected).and_then(|row| row.frames.clone()) {
                    if let Some((func, line)) = frames.iter().find_map(|(id, line)| p.func_of_id(*id).filter(|f| !p.funcs[*f].module.is_empty()).map(|f| (f as u32, *line))) {
                        app.focus = Some(Focus { func, line: Some(line) });
                        app.source.module = None;
                        app.source.line = line;
                        app.set_view(View::Source);
                    }
                }
            }
        }
        _ => {
            let p = &app.profiles[app.shown];
            let snapshot = &p.raw.heap.as_ref().unwrap().snapshots[index];
            let len = pane_len(snapshot, app.heap.pane);
            let page = app.page;
            app.heap.list.key(key, len, page);
        }
    }
}

pub fn copy(app: &App) -> Option<String> {
    let index = snapshot_index(app)?;
    let p = &app.profiles[app.shown];
    let snapshot = &p.raw.heap.as_ref()?.snapshots[index];
    match app.heap.pane {
        HeapPane::Holders => snapshot.biggest.get(app.heap.list.selected).map(|h| h.path.clone()),
        HeapPane::Types => snapshot.by_type.get(app.heap.list.selected).map(|t| t.ty.clone()),
        HeapPane::Stacks => snapshot.by_stack.get(app.heap.list.selected).map(|row| stack_text(p, row.frames.as_deref().unwrap_or(&[]))),
    }
}

//! Timeline: a column a frame (bucketed to the screen's width, each column the worst of its frames) in the metric's
//! per-frame figure, coloured by phase; pick a frame to see all of its figures beside its phase's median.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{
    kit::{self, Item, col, col_fill, col_r},
    overview::unit_of,
};
use crate::{
    app::App,
    fmt,
    keys::Key,
    model::Profile,
    theme::Theme,
};

/// The series a timeline can show: the timeline's columns, then the hardware counters' a frame
pub fn series(p: &Profile) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(timeline) = &p.raw.timeline {
        for column in &timeline.columns {
            if !matches!(column.as_str(), "frame" | "seconds" | "phase") {
                out.push(column.clone());
            }
        }
    }
    if let Some(counters) = p.raw.counters.as_ref().filter(|c| c.error.is_none()) {
        for name in &counters.names {
            out.push(format!("hw:{name}"));
        }
    }
    out
}

/// A series' values, row by row, with each row's frame and phase
pub fn values(p: &Profile, series: &str) -> Option<(Vec<f64>, Vec<f64>, Vec<u32>)> {
    if let Some(name) = series.strip_prefix("hw:") {
        let frames = &p.raw.counters.as_ref()?.frames;
        let values = frames.values(name)?;
        let numbers = frames.values("frame").unwrap_or_default();
        // (the counters' rows name their phase)
        let phase_index = frames.index("phase");
        let phases = frames.rows.iter().map(|row| phase_index.and_then(|i| row.get(i)).and_then(|v| v.as_str().map(|name| p.raw.phases.iter().position(|ph| ph == name).unwrap_or(0) as u32).or_else(|| v.as_u64().map(|n| n as u32))).unwrap_or(0)).collect();
        return Some((values, numbers, phases));
    }
    let timeline = p.raw.timeline.as_ref()?;
    let values = timeline.values(series)?;
    Some((values, timeline.values("frame").unwrap_or_default(), timeline.values("phase").unwrap_or_default().into_iter().map(|v| v as u32).collect()))
}

/// The series shown: one picked with [ ], else the metric's, else work
pub fn current(app: &App) -> Option<String> {
    let p = &app.profiles[app.shown];
    let all = series(p);
    if let Some(column) = app.timeline.column.as_ref().filter(|c| all.contains(c)) {
        return Some(column.clone());
    }
    let metric = app.metric.timeline_column(p).map(|c| if matches!(app.metric, crate::metric::Metric::Counter(_)) { format!("hw:{c}") } else { c });
    metric.filter(|c| all.contains(c)).or_else(|| all.iter().find(|c| *c == "work_ms").cloned()).or_else(|| all.first().cloned())
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(name) = current(app) else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Timeline", "", false));
        kit::empty(buf, inner, theme, "no timeline in this profile (--profile timeline)");
        return;
    };
    let p = &app.profiles[app.shown];
    let Some((all, frames, phases)) = values(p, &name) else { return };
    if all.is_empty() {
        return;
    }
    let (from, to) = app.timeline.range.map(|(a, b)| (a.min(all.len() - 1), b.min(all.len()))).unwrap_or((0, all.len()));
    let shown = &all[from..to];
    let selected = app.timeline.frame.unwrap_or_else(|| (0..all.len()).max_by(|&a, &b| all[a].total_cmp(&all[b]).then(b.cmp(&a))).unwrap_or(0)).min(all.len() - 1);
    let unit = unit_of(&name);
    let frame_at = |row: usize| frames.get(row).copied().unwrap_or(row as f64);
    let range = if app.timeline.range.is_some() { format!("frames {}-{} of {}", frame_at(from), frame_at(to - 1), fmt::grouped(all.len() as f64)) } else { format!("{} frames", fmt::grouped(all.len() as f64)) };
    let series_count = series(p).len();
    let chart_height = (area.height / 2).clamp(9, 20);
    let table_height = (series_count as u16 + 4).min(area.height.saturating_sub(chart_height));
    let parts = kit::rows(area, &[area.height.saturating_sub(table_height), table_height]);
    let block = kit::note_right(
        kit::panel(theme, &format!("{} a frame", super::overview::series_label(&name)), &range, true),
        theme,
        &format!("▲ frame {}: {}", frame_at(selected), fmt::short(unit, all[selected])),
    );
    let inner = kit::draw_panel(buf, parts[0], block);
    if inner.height < 5 || inner.width < 20 {
        return;
    }
    // the chart: an axis of 10 columns at the left, the columns a frame, then the frame numbers and the phases
    let gutter = 11u16;
    let chart = Rect { x: inner.x + gutter, y: inner.y, width: inner.width - gutter, height: inner.height - 3 };
    let order = super::overview::phase_order(p);
    let axis_of = kit::FrameAxis::new(shown.len(), chart.width as usize);
    let column_of = |row: usize| axis_of.span(row.saturating_sub(from)).0;
    let picked = (selected >= from && selected < to).then(|| axis_of.span(selected - from));
    let max = axis_of.values(shown).iter().copied().fold(0.0, f64::max);
    super::overview::frame_columns(buf, chart, &axis_of, shown, |column| {
        if picked.is_some_and(|(a, b)| column >= a && column < b) {
            theme.accent
        } else {
            super::overview::phase_color(&order, axis_of.row(column).and_then(|row| phases.get(from + row).copied()).unwrap_or(0), theme)
        }
    }, theme);
    let axis = Style::new().fg(theme.faint);
    let label = Style::new().fg(theme.muted);
    for y in chart.top()..chart.bottom() {
        buf[(chart.x - 1, y)].set_symbol("│").set_style(axis);
    }
    let marks = [(chart.top(), max), (chart.top() + chart.height / 2, max / 2.0), (chart.bottom() - 1, 0.0)];
    for (y, value) in marks {
        let text = fmt::short(unit, value);
        Paragraph::new(Line::from(Span::styled(text, label))).alignment(ratatui::layout::Alignment::Right).render(Rect { x: inner.x, y, width: gutter - 2, height: 1 }, buf);
        buf[(chart.x - 1, y)].set_symbol("┤").set_style(axis);
    }
    let base = chart.bottom();
    buf[(chart.x - 1, base)].set_symbol("└").set_style(axis);
    for x in chart.left()..chart.right() {
        buf[(x, base)].set_symbol("─").set_style(axis);
    }
    // (the first frame's number, then round steps, about one every 20 columns; the cursor under the frame picked)
    let y = base + 1;
    let (first, last_frame) = (frame_at(from), frame_at(to - 1));
    let step = nice_step((last_frame - first).max(1.0) / (chart.width as f64 / 20.0).max(1.0));
    let mut ticks = vec![first];
    let mut tick = (first / step).floor() * step + step;
    while tick <= last_frame {
        ticks.push(tick);
        tick += step;
    }
    let row_of = |tick: f64| if frames.len() >= to { from + frames[from..to].iter().position(|&f| f >= tick).unwrap_or(to - from - 1) } else { (tick.max(0.0) as usize).clamp(from, to - 1) };
    let cursor = picked.map(|(a, b)| chart.x + ((a + b.max(a + 1) - 1) / 2) as u16);
    if let Some(x) = cursor {
        buf[(x, y)].set_symbol("▲").set_style(Style::new().fg(theme.accent).add_modifier(Modifier::BOLD));
    }
    let mut last_end = chart.x;
    for tick in ticks {
        let text = format!("{tick}");
        let width = text.chars().count() as u16;
        let x = (chart.x + column_of(row_of(tick)) as u16).max(last_end);
        if x + width <= chart.right() && !cursor.is_some_and(|c| c >= x && c <= x + width) {
            buf[(x.min(chart.right() - 1), base)].set_symbol("┴").set_style(axis);
            Paragraph::new(Line::from(Span::styled(text, label))).render(Rect { x, y, width, height: 1 }, buf);
            last_end = x + width + 1;
        }
    }
    let mut last: Option<u32> = None;
    let mut phase_end = chart.x;
    for column in 0..axis_of.columns() {
        let Some(phase) = axis_of.row(column).and_then(|row| phases.get(from + row).copied()) else { continue };
        if last != Some(phase) {
            let x = (chart.x + column as u16).max(phase_end);
            let line = Line::from(vec![Span::styled("■ ", Style::new().fg(super::overview::phase_color(&order, phase, theme))), Span::styled(p.phase_name(phase).to_string(), Style::new().fg(theme.ink2))]);
            let width = line.width() as u16;
            Paragraph::new(line).render(Rect { x, y: y + 1, width: chart.right().saturating_sub(x), height: 1 }, buf);
            phase_end = x + width + 2;
            last = Some(phase);
        }
    }
    // the selected frame beside its phase's median frame, each figure frame by frame
    let phase = phases.get(selected).copied().unwrap_or(0);
    let same_phase: Vec<usize> = (0..all.len()).filter(|&i| phases.get(i).copied().unwrap_or(0) == phase).collect();
    let note = match p.phase_words(phase).filter(|_| order.len() > 1) {
        Some(words) => format!("in {words} · beside its median frame"),
        None => "beside the median frame".to_string(),
    };
    let inner = kit::draw_panel(buf, parts[1], kit::panel(theme, &format!("Frame {}", frame_at(selected)), &note, false));
    let cols = [col("figure", 16), col_r("frame", 11), col("", 1), col_r("median", 11), col_r("ratio", 8), col_fill("frame by frame", 1)];
    let line_width = kit::widths(&cols, inner.width)[5] as usize;
    let mut rows: Vec<Vec<Item>> = Vec::new();
    for column in series(p) {
        let Some((values, _, _)) = values(p, &column) else { continue };
        let Some(&value) = values.get(selected) else { continue };
        let typical = median(&same_phase.iter().filter_map(|&i| values.get(i).copied()).collect::<Vec<_>>());
        let unit = unit_of(&column);
        let ratio = if typical > 0.0 { value / typical } else if value > 0.0 { f64::INFINITY } else { 1.0 };
        let (mark, color) = if ratio >= 2.0 && value > 0.0 { ("▲", theme.more) } else if ratio <= 0.5 && typical > 0.0 { ("▼", theme.less) } else { (" ", theme.muted) };
        let ratio_text = if ratio.is_finite() { format!("{ratio:.1}x") } else { "new".into() };
        let shown_values = &values[from.min(values.len())..to.min(values.len())];
        let line_axis = kit::FrameAxis::new(shown_values.len(), line_width);
        let buckets = line_axis.values(shown_values);
        let bright = (selected >= from && selected < to).then(|| line_axis.span(selected - from));
        let top = buckets.iter().copied().fold(0.0, f64::max);
        rows.push(vec![
            Item::text(super::overview::series_label(&column), Style::new().fg(theme.ink2)),
            Item::text(fmt::short(unit, value), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
            Item::Spans(Line::from(Span::styled(mark, Style::new().fg(color).add_modifier(Modifier::BOLD)))),
            Item::text(fmt::short(unit, typical), Style::new().fg(theme.ink2)),
            Item::text(ratio_text, Style::new().fg(if mark == " " { theme.muted } else { color })),
            Item::Spans(kit::spark(&buckets, top, |b| if bright.is_some_and(|(from, to)| b >= from && b < to) { theme.accent } else { theme.slot(0) })),
        ]);
    }
    kit::table(buf, inner, theme, &cols, rows, None, 0);
}

/// A round step of at least `raw`: 1, 2, 5, 10, 20, 50, ...
fn nice_step(raw: f64) -> f64 {
    let power = 10f64.powf(raw.max(1.0).log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * power).find(|step| *step >= raw).unwrap_or(10.0 * power)
}

pub fn key(app: &mut App, key: Key) {
    let Some(name) = current(app) else { return };
    let p = &app.profiles[app.shown];
    let Some((all, _, _)) = values(p, &name) else { return };
    if all.is_empty() {
        return;
    }
    let len = all.len();
    let selected = app.timeline.frame.unwrap_or_else(|| (0..len).max_by(|&a, &b| all[a].total_cmp(&all[b]).then(b.cmp(&a))).unwrap_or(0));
    let series = series(p);
    match key {
        Key::Left => app.timeline.frame = Some(selected.saturating_sub(1)),
        Key::Right => app.timeline.frame = Some((selected + 1).min(len - 1)),
        Key::PageUp | Key::Up => app.timeline.frame = Some(selected.saturating_sub(10)),
        Key::PageDown | Key::Down => app.timeline.frame = Some((selected + 10).min(len - 1)),
        Key::Home => app.timeline.frame = Some(0),
        Key::End => app.timeline.frame = Some(len - 1),
        Key::Enter => {
            // zoom to the frames around the selected one, a column each
            let window = (app.page * 2).max(60).min(len);
            let from = selected.saturating_sub(window / 2).min(len - window);
            app.timeline.range = Some((from, from + window));
        }
        Key::Esc | Key::Backspace => app.timeline.range = None,
        Key::Char(']') | Key::Char('[') => {
            if let Some(at) = series.iter().position(|s| *s == name) {
                let next = if key == Key::Char(']') { (at + 1) % series.len() } else { (at + series.len() - 1) % series.len() };
                app.timeline.column = Some(series[next].clone());
                app.status = format!("series: {}", series[next].replace('_', " "));
            }
        }
        Key::Char('s') => {
            // the worst frame of the series
            app.timeline.frame = (0..len).max_by(|&a, &b| all[a].total_cmp(&all[b]).then(b.cmp(&a)));
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let name = current(app)?;
    let (all, frames, _) = values(app.profile(), &name)?;
    let selected = app.timeline.frame.unwrap_or(0).min(all.len().saturating_sub(1));
    Some(format!("frame {}", frames.get(selected).copied().unwrap_or(0.0)))
}

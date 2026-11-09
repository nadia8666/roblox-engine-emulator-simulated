//! Compare: a what-if comparison's variants side by side (`ree profile try`): each variant's figure at each input size
//! in the chosen metric, how it grows with n, the winner of each size, and the selected run's figures, table shapes and
//! hot functions. Enter opens a run's own profile in the other views; D diffs it against the baseline's.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Bar, BarChart, Chart, Dataset, GraphType, LegendPosition, Paragraph, Widget, Wrap},
};

use super::kit::{self, Item, col, col_fill, col_r};
use crate::{
    app::{App, View},
    compare::{Compare, MetricInfo, RunInfo},
    fmt,
    keys::Key,
    model::Profile,
    theme::{Mark, Theme},
};

/// The metric shown: the one picked with m, else the work clock, else the first measured
pub fn metric(app: &App) -> Option<MetricInfo> {
    let compare = app.compare.as_ref()?;
    let measured = compare.measured();
    let picked = app.compare_view.metric.as_ref().and_then(|key| measured.iter().find(|m| &m.key == key));
    picked.or_else(|| measured.iter().find(|m| m.key == "work_ms")).or(measured.first()).map(|m| (*m).clone())
}

/// `m` / `M` in this view: the comparison's metrics
pub fn cycle_metric(app: &mut App, forward: bool) {
    let Some(compare) = app.compare.as_ref() else { return };
    let keys: Vec<String> = compare.measured().iter().map(|m| m.key.clone()).collect();
    if keys.is_empty() {
        return;
    }
    let current = metric(app).map(|m| m.key).unwrap_or_default();
    let at = keys.iter().position(|k| *k == current).unwrap_or(0);
    let next = if forward { (at + 1) % keys.len() } else { (at + keys.len() - 1) % keys.len() };
    app.compare_view.metric = Some(keys[next].clone());
    if let Some(m) = metric(app) {
        app.status = format!("metric: {}{}", m.label, if m.exact { " (exact)" } else { " (measured)" });
    }
}

fn size_label(n: f64) -> String {
    format!("n={}", si(n))
}

/// The ratio of a value to the baseline's, oriented so that above 1 is worse
fn worse_ratio(m: &MetricInfo, value: f64, base: f64) -> Option<f64> {
    if base <= 0.0 || value <= 0.0 || !m.ranked() {
        return None;
    }
    Some(if m.lower_is_better() { value / base } else { base / value })
}

/// A variant's figure over the baseline's: oriented so above 1 is worse when the figure names a better side, else as
/// it is (above 1 is more)
fn ratio_of(m: &MetricInfo, value: f64, base: f64) -> Option<f64> {
    if base <= 0.0 || value <= 0.0 {
        return None;
    }
    if m.ranked() { worse_ratio(m, value, base) } else { Some(value / base) }
}

fn selected(app: &App, compare: &Compare) -> (usize, usize) {
    let variant = app.compare_view.list.selected.min(compare.variants.len().saturating_sub(1));
    let last = compare.sizes.len().saturating_sub(1);
    let size = app.compare_view.size.unwrap_or(last).min(last);
    (variant, size)
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(compare) = app.compare.as_ref() else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Compare", "", false));
        kit::empty(buf, inner, theme, "no comparison: open a folder ree profile try wrote (it holds compare.json)");
        return;
    };
    let Some(m) = metric(app) else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, &compare.title, "", false));
        kit::empty(buf, inner, theme, "the comparison measured nothing");
        return;
    };
    let (sel_variant, sel_size) = selected(app, compare);
    let variants_height = (compare.variants.len() as u16 + 5).min(area.height / 3);
    let parts = kit::rows(area, &[variants_height, 0]);
    // the variants: a row each, a column a size
    let what = format!("{}{}{}{}", m.label, if m.per_call { " a call" } else { "" }, if m.exact { " · exact" } else { " · measured" }, if m.ranked() { "" } else { " · names no winner" });
    let legend = Line::from(Span::styled(" ◆ best, clear of noise · ≈ best within noise, or too small to matter · ✗ a different result · ▲▼ only where the gap is clear · underlined: the run below ", Style::new().fg(theme.muted)));
    let block = kit::bottom(kit::note_right(kit::panel(theme, &compare.title, &compare.about, true), theme, &what), legend);
    let inner = kit::draw_panel(buf, parts[0], block);
    let sizes: Vec<String> = compare.sizes.iter().map(|n| fmt::grouped(*n)).collect();
    let calls = if compare.frames > 1.0 { format!("{} calls each", fmt::grouped(compare.frames)) } else { "one call each".to_string() };
    // (the results: held, or which variants compute something else, which blocks their every verdict)
    let check = match (compare.differ_line(), &compare.check) {
        (Some(line), _) => vec![Span::styled("   ✗ ", Style::new().fg(theme.critical).add_modifier(Modifier::BOLD)), Span::styled(line, Style::new().fg(theme.critical))],
        (None, Some(check)) if check.ran && check.ok => vec![
            Span::styled("   ✓ ", Style::new().fg(theme.good).add_modifier(Modifier::BOLD)),
            Span::styled(if check.by.as_deref() == Some("digest") { "every result has the baseline's digest" } else { "every result holds against the baseline's" }, Style::new().fg(theme.ink2)),
        ],
        _ => Vec::new(),
    };
    let mut first = vec![
        Span::styled("baseline ", Style::new().fg(theme.muted)),
        Span::styled(compare.baseline.clone(), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
        Span::styled("   n ", Style::new().fg(theme.muted)),
        Span::styled(sizes.join(" · "), Style::new().fg(theme.ink2)),
        Span::styled(format!("   {calls}"), Style::new().fg(theme.muted)),
    ];
    first.extend(check);
    Paragraph::new(Line::from(first)).render(Rect { height: 1, ..inner }, buf);
    // (what the figure is, in the comparison's own words: which round a measured figure takes, what it leaves out)
    if !m.about.is_empty() && inner.height > 1 {
        let about = Line::from(vec![Span::styled("◇ ", Style::new().fg(theme.accent)), Span::styled(fmt::fit(&format!("{}: {}", m.label, m.about), inner.width.saturating_sub(2) as usize), Style::new().fg(theme.muted))]);
        Paragraph::new(about).render(Rect { y: inner.y + 1, height: 1, ..inner }, buf);
    }
    let largest = compare.sizes.iter().copied().fold(0.0, f64::max);
    let mut cols = vec![col("variant", 26)];
    for n in &compare.sizes {
        cols.push(col_r(&size_label(*n), 12));
    }
    cols.push(col_r("scaling", 8));
    cols.push(col_fill(&format!("  n={} vs baseline", si(largest)), 1));
    let table_area = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
    let mut list = app.compare_view.list;
    list.fit(compare.variants.len(), kit::rows_shown(table_area));
    let rows = compare
        .variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let mut cells = vec![Item::Spans(Line::from(vec![
                Span::styled("■ ", Style::new().fg(theme.slot(index))),
                Span::styled(variant.name.clone(), Style::new().fg(theme.ink)),
                Span::styled(if variant.name == compare.baseline { "  baseline" } else { "" }, Style::new().fg(theme.muted)),
            ]))];
            for n in &compare.sizes {
                let run = compare.run(&variant.name, *n);
                let shown = match (run, run.and_then(|r| r.value(&m.key))) {
                    (Some(r), _) if r.status != "ok" && !r.status.is_empty() => r.status.clone(),
                    (_, Some(v)) => m.show(v),
                    _ => "-".into(),
                };
                // (a variant whose result differs wins nothing: ✗, never ◆)
                let failed = run.is_some_and(|r| compare.differs(r));
                let winner = compare.winner(&m.key, *n).filter(|w| w.winner == variant.name);
                let (mark, mark_color) = match winner {
                    _ if failed => (" ✗", theme.critical),
                    Some(w) if w.clear && w.material != Some(false) => (" ◆", theme.good),
                    Some(_) => (" ≈", theme.muted),
                    None => ("  ", theme.muted),
                };
                cells.push(Item::Spans(Line::from(vec![Span::styled(shown, Style::new().fg(if failed { theme.critical } else { theme.ink })), Span::styled(mark, Style::new().fg(mark_color))])));
            }
            // (the exponent at large n: a fixed cost a call no longer bends it)
            let scaling = compare.scaling(&variant.name, &m.key).and_then(|s| s.large_n());
            cells.push(match scaling {
                Some(e) => Item::text(format!("n^{e:.2}"), Style::new().fg(if e >= 1.3 && m.lower_is_better() { theme.more } else { theme.ink2 })),
                None => Item::empty(),
            });
            // (against the baseline: a verdict's colour only where the gap is clear of noise and material)
            cells.push(match (variant.name == compare.baseline, compare.against_baseline(&m, largest, &variant.name)) {
                (false, Some(verdict)) => verdict_cell(&m, &verdict, theme),
                _ => Item::empty(),
            });
            cells
        })
        .collect();
    let grid = kit::table(buf, table_area, theme, &cols, rows, Some(list.selected), list.offset);
    list.offset = grid.offset;
    app.compare_view.list = list;
    // (the run below: the selected size's cell underlined in the accent)
    if sel_variant >= grid.offset && sel_variant < grid.offset + grid.shown {
        let rect = Rect { y: grid.top + (sel_variant - grid.offset) as u16, height: 1, ..grid.column(1 + sel_size) };
        buf.set_style(rect, Style::new().fg(theme.accent).add_modifier(Modifier::UNDERLINED | Modifier::BOLD));
    }
    let compare = app.compare.as_ref().unwrap();
    // below: the plot by n, and the selected size's figures
    let below = parts[1];
    if below.height < 8 {
        return;
    }
    let (left, right) = kit::split(below, below.width * 11 / 20);
    if compare.variants.len() >= 2 && left.height >= 20 {
        let parts = kit::rows(left, &[left.height * 3 / 5, 0]);
        plot(buf, parts[0], compare, &m, sel_size, theme);
        plot_ratio(buf, parts[1], compare, &m, sel_size, theme);
    } else {
        plot(buf, left, compare, &m, sel_size, theme);
    }
    draw_detail(buf, right, compare, &m, sel_variant, sel_size, theme);
}

/// A variant's figure against the baseline's, worse to the right in red
fn ratio_cell(ratio: f64, theme: &Theme) -> Item {
    let (arrow, word, color) = if ratio > 1.0005 {
        ("▲", "worse", theme.more)
    } else if ratio < 0.9995 {
        ("▼", "better", theme.less)
    } else {
        ("=", "same", theme.muted)
    };
    let times = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
    Item::Spans(Line::from(vec![
        Span::styled(format!("{} ", times_text(times)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{arrow} {word}"), Style::new().fg(color)),
    ]))
}

/// A variant against the baseline: ▲ worse / ▼ better in colour only for a gap clear of noise and material; otherwise its
/// ratio in no colour with why it names nothing (within noise, too small, about equal), and ✗ for a different result
fn verdict_cell(m: &MetricInfo, verdict: &crate::compare::Verdict, theme: &Theme) -> Item {
    if verdict.differs {
        return Item::Spans(Line::from(Span::styled("✗ a different result", Style::new().fg(theme.critical))));
    }
    let Some(ratio) = verdict.ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.0) else { return Item::empty() };
    if !m.ranked() || verdict.unweighed {
        return neutral_cell(ratio, m.exact, theme);
    }
    if verdict.significant() {
        return ratio_cell(ratio, theme);
    }
    // (the same figure: nothing to weigh)
    if (ratio - 1.0).abs() < 0.0005 {
        return Item::Spans(Line::from(vec![Span::styled("1.00x ", Style::new().fg(theme.ink2)), Span::styled("= same", Style::new().fg(theme.muted))]));
    }
    let times = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
    let why = if verdict.material == Some(false) {
        "too small"
    } else if verdict.rounds.as_ref().is_some_and(|rounds| rounds.aa_bar.is_some()) {
        "within A/A"
    } else if m.exact {
        "about equal"
    } else {
        "noise"
    };
    Item::Spans(Line::from(vec![Span::styled(format!("{} ", times_text(times)), Style::new().fg(theme.ink2)), Span::styled(format!("≈ {why}"), Style::new().fg(theme.muted))]))
}

/// A diverging bar in no colour: the size of a gap that names nothing
fn quiet_bar(fraction: f64, width: u16, theme: &Theme) -> Line<'static> {
    let line = kit::diverging(fraction, width, theme);
    Line::from(line.spans.into_iter().map(|span| if span.content.contains('█') { Span::styled(span.content, Style::new().fg(theme.faint)) } else { span }).collect::<Vec<_>>())
}

/// A figure that names no winner over the baseline's: more or less, in no colour, `~` before a measured one's (one run
/// each of host time or a counter says nothing of how far two runs of the same code part)
fn neutral_cell(ratio: f64, exact: bool, theme: &Theme) -> Item {
    let word = if ratio > 1.0005 { "more" } else if ratio < 0.9995 { "less" } else { "same" };
    let times = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
    let mark = if exact { "" } else { "~" };
    Item::Spans(Line::from(vec![Span::styled(format!("{mark}{} ", times_text(times)), Style::new().fg(theme.ink2)), Span::styled(word, Style::new().fg(theme.muted))]))
}

/// `1.06x`, `27.1x`, `1667x`; below a tenth as a fraction, `1/1667x`
fn times_text(times: f64) -> String {
    if times >= 100.0 {
        format!("{times:.0}x")
    } else if times >= 10.0 {
        format!("{times:.1}x")
    } else if times >= 0.1 || times <= 0.0 {
        format!("{times:.2}x")
    } else {
        format!("1/{}x", times_text(1.0 / times).trim_end_matches('x'))
    }
}

/// The figure by n on log scales: a line a variant in its colour, its measured points, the selected size marked
fn plot(buf: &mut Buffer, area: Rect, compare: &Compare, m: &MetricInfo, sel_size: usize, theme: &Theme) {
    let block = kit::panel(theme, &format!("{} by n", m.label), "log-log · a line a variant", false);
    let inner = kit::draw_panel(buf, area, block);
    if compare.sizes.len() < 2 {
        kit::empty(buf, inner, theme, "one input size: nothing to scale");
        return;
    }
    let lines: Vec<Vec<(f64, f64)>> = compare
        .variants
        .iter()
        .map(|variant| compare.sizes.iter().filter_map(|n| compare.run(&variant.name, *n).and_then(|r| r.value(&m.key)).filter(|v| *v > 0.0 && *n > 0.0).map(|v| (n.log10(), v.log10()))).collect())
        .collect();
    let ys: Vec<f64> = lines.iter().flatten().map(|p| p.1).collect();
    if ys.is_empty() {
        kit::empty(buf, inner, theme, "nothing above zero to plot on log scales");
        return;
    }
    // (decades for the bounds, so the axis labels, which sit evenly along it, fall on them)
    let (ymin, ymax) = ys.iter().fold((f64::MAX, f64::MIN), |(lo, hi), y| (lo.min(*y), hi.max(*y)));
    let (y0, mut y1) = (ymin.floor(), ymax.ceil());
    if y1 <= y0 {
        y1 = y0 + 1.0;
    }
    let xs: Vec<f64> = compare.sizes.iter().filter(|n| **n > 0.0).map(|n| n.log10()).collect();
    let (x0, mut x1) = (xs.iter().copied().fold(f64::MAX, f64::min).floor(), xs.iter().copied().fold(f64::MIN, f64::max).ceil());
    if x1 <= x0 {
        x1 = x0 + 1.0;
    }
    let label = Style::new().fg(theme.muted);
    let x_labels: Vec<Span> = (x0 as i32..=x1 as i32).map(|e| Span::styled(si(10f64.powi(e)), label)).collect();
    let y_labels: Vec<Span> = (y0 as i32..=y1 as i32).map(|e| Span::styled(tick_label(m, 10f64.powi(e)), label)).collect();
    // the selected size: a faint upright line
    let selected_x = compare.sizes.get(sel_size).filter(|n| **n > 0.0).map(|n| n.log10());
    let cursor: Vec<(f64, f64)> = selected_x.map(|x| vec![(x, y0), (x, y1)]).unwrap_or_default();
    let mut datasets = vec![Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(theme.faint)).data(&cursor)];
    let names: Vec<String> = compare
        .variants
        .iter()
        .map(|variant| {
            let exponent = compare.scaling(&variant.name, &m.key).and_then(|s| s.large_n()).map(|e| format!("  n^{e:.2}")).unwrap_or_default();
            format!("{}{exponent}", variant.name)
        })
        .collect();
    for (index, points) in lines.iter().enumerate() {
        let color = theme.slot(index);
        datasets.push(Dataset::default().name(Line::from(vec![Span::raw("■ "), Span::styled(names[index].clone(), Style::new().fg(theme.ink))])).marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(color)).data(points));
        datasets.push(Dataset::default().marker(Marker::Dot).graph_type(GraphType::Scatter).style(Style::new().fg(color)).data(points));
    }
    let axis = Style::new().fg(theme.faint);
    Chart::new(datasets)
        .style(Style::new().bg(theme.panel).fg(theme.ink2))
        .x_axis(Axis::default().title(Line::from(Span::styled("n", label))).bounds([x0, x1]).labels(x_labels).style(axis))
        .y_axis(Axis::default().bounds([y0, y1]).labels(y_labels).style(axis))
        .legend_position(Some(LegendPosition::TopLeft))
        .hidden_legend_constraints((Constraint::Ratio(1, 1), Constraint::Ratio(1, 1)))
        .render(inner, buf);
}

/// Each variant's figure over the baseline's by n (above 1 is worse) on a log scale, the baseline flat at 1: a ratio of 2
/// and one of 0.5 lie as far from it, and a ratio of 1,000 does not squash the rest (a linear axis padded round 1667x
/// labelled its bottom -459x)
fn plot_ratio(buf: &mut Buffer, area: Rect, compare: &Compare, m: &MetricInfo, sel_size: usize, theme: &Theme) {
    let block = kit::panel(theme, &format!("over {}", compare.baseline), if m.ranked() { "by n · log scale · above 1 is worse" } else { "by n · log scale · above 1 is more" }, false);
    let inner = kit::draw_panel(buf, area, block);
    let lines: Vec<(usize, Vec<(f64, f64)>)> = compare
        .variants
        .iter()
        .enumerate()
        .filter(|(_, v)| v.name != compare.baseline)
        .map(|(index, variant)| {
            let points = compare
                .sizes
                .iter()
                .filter(|n| **n > 0.0)
                .filter_map(|n| {
                    let value = compare.run(&variant.name, *n).and_then(|r| r.value(&m.key))?;
                    let base = compare.run(&compare.baseline, *n).and_then(|r| r.value(&m.key))?;
                    // (the natural log of the ratio: the axis is logarithmic)
                    ratio_of(m, value, base).filter(|ratio| *ratio > 0.0 && ratio.is_finite()).map(|ratio| (n.log10(), ratio.ln()))
                })
                .collect();
            (index, points)
        })
        .collect();
    let ys: Vec<f64> = lines.iter().flat_map(|(_, points)| points.iter().map(|p| p.1)).collect();
    if ys.is_empty() {
        kit::empty(buf, inner, theme, "no figure the baseline also has");
        return;
    }
    // (1x, ln 0, always in view; the labels sit evenly along the axis, so each is the ratio at its height)
    let (lo, hi) = ys.iter().fold((0.0f64, 0.0f64), |(lo, hi), y| (lo.min(*y), hi.max(*y)));
    let pad = ((hi - lo) * 0.15).max(0.02f64.ln_1p());
    let (y0, y1) = (lo - pad, hi + pad);
    let xs: Vec<f64> = compare.sizes.iter().filter(|n| **n > 0.0).map(|n| n.log10()).collect();
    let (x0, mut x1) = (xs.iter().copied().fold(f64::MAX, f64::min).floor(), xs.iter().copied().fold(f64::MIN, f64::max).ceil());
    if x1 <= x0 {
        x1 = x0 + 1.0;
    }
    let label = Style::new().fg(theme.muted);
    let x_labels: Vec<Span> = (x0 as i32..=x1 as i32).map(|e| Span::styled(si(10f64.powi(e)), label)).collect();
    let y_labels: Vec<Span> = [y0, (y0 + y1) / 2.0, y1].iter().map(|v| Span::styled(times_text(v.exp()), label)).collect();
    let baseline = vec![(x0, 0.0), (x1, 0.0)];
    let selected_x = compare.sizes.get(sel_size).filter(|n| **n > 0.0).map(|n| n.log10());
    let cursor: Vec<(f64, f64)> = selected_x.map(|x| vec![(x, y0), (x, y1)]).unwrap_or_default();
    let mut datasets = vec![
        Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(theme.faint)).data(&cursor),
        Dataset::default().name(Line::from(vec![Span::raw("■ "), Span::styled(format!("{} = 1", compare.baseline), Style::new().fg(theme.ink))])).marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(theme.slot(0))).data(&baseline),
    ];
    for (index, points) in &lines {
        let color = theme.slot(*index);
        datasets.push(Dataset::default().name(Line::from(vec![Span::raw("■ "), Span::styled(compare.variants[*index].name.clone(), Style::new().fg(theme.ink))])).marker(Marker::Braille).graph_type(GraphType::Line).style(Style::new().fg(color)).data(points));
        datasets.push(Dataset::default().marker(Marker::Dot).graph_type(GraphType::Scatter).style(Style::new().fg(color)).data(points));
    }
    let axis = Style::new().fg(theme.faint);
    Chart::new(datasets)
        .style(Style::new().bg(theme.panel).fg(theme.ink2))
        .x_axis(Axis::default().title(Line::from(Span::styled("n", label))).bounds([x0, x1]).labels(x_labels).style(axis))
        .y_axis(Axis::default().bounds([y0, y1]).labels(y_labels).style(axis))
        .legend_position(Some(LegendPosition::TopLeft))
        .hidden_legend_constraints((Constraint::Ratio(1, 1), Constraint::Ratio(1, 1)))
        .render(inner, buf);
}

/// A count in its shortest round form: `10`, `100`, `1K`, `10K`, `1.5M`
fn si(value: f64) -> String {
    let (scaled, suffix) = if value >= 1e9 {
        (value / 1e9, "G")
    } else if value >= 1e6 {
        (value / 1e6, "M")
    } else if value >= 1e3 {
        (value / 1e3, "K")
    } else {
        (value, "")
    };
    format!("{}{suffix}", trim(scaled))
}

/// A number without trailing zeros: `10`, `2.5`
fn trim(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// An axis tick in the metric's unit, round: `10 µs`, `1 ms`, `100 kB`, `1K`
fn tick_label(m: &MetricInfo, value: f64) -> String {
    match m.unit.as_str() {
        "ms" if value >= 1000.0 => format!("{} s", trim(value / 1000.0)),
        "ms" if value >= 1.0 => format!("{} ms", trim(value)),
        "ms" if value >= 0.001 => format!("{} µs", trim(value * 1000.0)),
        "ms" => format!("{} ns", trim(value * 1e6)),
        "bytes" if value >= 1e6 => format!("{} MB", trim(value / 1e6)),
        "bytes" if value >= 1e3 => format!("{} kB", trim(value / 1e3)),
        "bytes" => format!("{} B", trim(value)),
        "ratio" => format!("{}%", trim(value * 100.0)),
        _ => si(value),
    }
}

fn draw_detail(buf: &mut Buffer, area: Rect, compare: &Compare, m: &MetricInfo, variant: usize, size: usize, theme: &Theme) {
    let (Some(info), Some(&n)) = (compare.variants.get(variant), compare.sizes.get(size)) else { return };
    let bars_height = (compare.variants.len() as u16 * 2 + 2).min(area.height / 3);
    let tables_height = if compare.run(&info.name, n).and_then(|r| r.tables.as_ref()).is_some_and(|t| !t.sites.is_empty()) { 7 } else { 0 };
    let notes_height = if compare.notes.is_empty() { 0 } else { (area.height / 5).clamp(4, 8) };
    let parts = kit::rows(area, &[bars_height, area.height.saturating_sub(bars_height + tables_height + notes_height), tables_height, notes_height]);
    // each variant's figure at the size, as bars in its colour
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, &format!("{} at n = {}", m.label, fmt::grouped(n)), "←→ size", false));
    let values: Vec<Option<f64>> = compare.variants.iter().map(|v| compare.run(&v.name, n).and_then(|r| r.value(&m.key))).collect();
    let max = values.iter().flatten().copied().fold(0.0, f64::max);
    let bars: Vec<Bar> = compare
        .variants
        .iter()
        .enumerate()
        .map(|(index, v)| {
            let color = theme.slot(index);
            let value = values[index].unwrap_or(0.0);
            let scaled = if max > 0.0 { ((value / max) * 1000.0).round() as u64 } else { 0 };
            Bar::default()
                .value(scaled)
                .label(Line::from(Span::styled(v.name.clone(), Style::new().fg(theme.ink2))))
                .text_value(values[index].map_or("-".into(), |v| format!(" {} ", m.show(v))))
                .style(Style::new().fg(color))
                .value_style(Style::new().fg(theme.ink_on(color)).bg(color).add_modifier(Modifier::BOLD))
        })
        .collect();
    BarChart::horizontal(bars).bar_width(1).bar_gap(1).max(1000).style(Style::new().bg(theme.panel)).render(inner, buf);
    // every figure of a variant over the baseline's at the size (the selected variant, or the first other when the
    // baseline is selected), the largest differences first
    let other = if info.name == compare.baseline { compare.variants.iter().find(|v| v.name != compare.baseline) } else { Some(info) };
    if let (Some(other), Some(base)) = (other, compare.run(&compare.baseline, n)) {
        let title = format!("{} vs {}", other.name, compare.baseline);
        let inner = kit::draw_panel(buf, parts[1], kit::panel(theme, &title, &format!("each figure at n = {}", fmt::grouped(n)), false));
        let run = compare.run(&other.name, n);
        // (each figure's verdict against the baseline: coloured only where the gap is clear and material)
        let mut rows: Vec<(String, f64, bool, Option<crate::compare::Verdict>, MetricInfo)> = compare
            .measured()
            .into_iter()
            .filter_map(|metric| {
                let (Some(value), Some(base)) = (run.and_then(|r| r.value(&metric.key)), base.value(&metric.key)) else { return None };
                let verdict = compare.against_baseline(metric, n, &other.name);
                let ratio = verdict.as_ref().and_then(|verdict| verdict.ratio).or_else(|| ratio_of(metric, value, base))?;
                Some((metric.label.clone(), ratio, metric.ranked(), verdict, metric.clone()))
            })
            .collect();
        // (significant gaps first, then the other ranked ones, then those that name no better side; largest first)
        let rank = |row: &(String, f64, bool, Option<crate::compare::Verdict>, MetricInfo)| (row.2, row.3.as_ref().is_some_and(|verdict| verdict.significant()));
        rows.sort_by(|a, b| rank(b).1.cmp(&rank(a).1).then(rank(b).0.cmp(&rank(a).0)).then(b.1.ln().abs().total_cmp(&a.1.ln().abs())).then(a.0.cmp(&b.0)));
        let cols = [col_fill("figure", 2), col_r("", 20), col_fill("", 3)];
        let bar_width = kit::widths(&cols, inner.width)[2];
        // (the bars on a log scale: the largest ratio shown fills an arm)
        let widest = rows.iter().filter(|row| row.2).map(|row| row.1.ln().abs()).fold(0.0, f64::max).max(2f64.ln());
        let items = rows
            .iter()
            .take(kit::rows_shown(inner))
            .map(|(label, ratio, ranked, verdict, metric)| {
                let significant = verdict.as_ref().is_some_and(|verdict| verdict.significant());
                match (ranked, verdict) {
                    (true, Some(verdict)) => vec![
                        Item::text(label.clone(), Style::new().fg(if significant { theme.ink2 } else { theme.muted })),
                        verdict_cell(metric, verdict, theme),
                        Item::Spans(if significant { kit::diverging(ratio.ln() / widest, bar_width, theme) } else { quiet_bar(ratio.ln() / widest, bar_width, theme) }),
                    ],
                    // (no verdict to read: its ratio, no colour)
                    (true, None) => vec![Item::text(label.clone(), Style::new().fg(theme.muted)), neutral_cell(*ratio, metric.exact, theme), Item::Spans(quiet_bar(ratio.ln() / widest, bar_width, theme))],
                    // (no better side: no bar)
                    (false, _) => vec![Item::text(label.clone(), Style::new().fg(theme.muted)), neutral_cell(*ratio, metric.exact, theme), Item::empty()],
                }
            })
            .collect();
        let grid = kit::table(buf, inner, theme, &cols, items, None, 0);
        kit::mark(buf, grid.column(2), Mark::Bar);
    } else {
        let inner = kit::draw_panel(buf, parts[1], kit::panel(theme, "vs baseline", "", false));
        kit::empty(buf, inner, theme, "one variant: nothing to set beside it");
    }
    // the tables the run's result keeps
    if tables_height > 0 {
        if let Some(tables) = compare.run(&info.name, n).and_then(|r| r.tables.as_ref()) {
            // (a site's phase says whose it is: the run's are the variant's own layout, the setup's the input every
            // variant shares; a comparison without phases lists them all)
            let own: Vec<&crate::compare::TableSite> = tables.sites.iter().filter(|site| site.phase != "setup").collect();
            let (count, bytes) = tables.run.map_or((tables.count, tables.bytes), |run| (run.count, run.bytes));
            let note = if tables.run.is_some() { format!("its own: {} · {}", fmt::grouped(count), fmt::bytes(bytes)) } else { format!("{} · {}", fmt::grouped(count), fmt::bytes(bytes)) };
            let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, &format!("Tables kept · {}", info.name), &note, false));
            let input = tables.setup.filter(|setup| setup.count > 0.0);
            let room = (inner.height as usize).saturating_sub(usize::from(input.is_some()));
            let mut lines = Vec::new();
            for site in own.iter().take(room) {
                let each = if site.count > 0.0 { site.bytes / site.count } else { 0.0 };
                let mut spans = vec![
                    Span::styled(format!("{:>7} × ", fmt::grouped(site.count)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
                    Span::styled(shape_text(site), Style::new().fg(theme.ink)),
                    Span::styled(format!("  {} each  ", fmt::bytes(each)), Style::new().fg(theme.ink2)),
                    Span::styled(site.site.clone(), Style::new().fg(theme.muted)),
                ];
                if site.rehashes > 0.0 {
                    spans.push(Span::styled(format!("  grew {}×", fmt::grouped(site.rehashes)), Style::new().fg(theme.serious)));
                }
                lines.push(Line::from(spans));
            }
            if let Some(setup) = input {
                lines.push(Line::from(Span::styled(format!("{:>7}   the setup's input (all variants'): {} tables, {}", "", fmt::grouped(setup.count), fmt::bytes(setup.bytes)), Style::new().fg(theme.muted))));
            }
            Paragraph::new(lines).render(inner, buf);
        }
    }
    if notes_height > 0 {
        let inner = kit::draw_panel(buf, parts[3], kit::panel(theme, "Notes", "", false));
        let lines: Vec<Line> = compare.notes.iter().map(|note| Line::from(vec![Span::styled("◇ ", Style::new().fg(theme.accent)), Span::styled(note.clone(), Style::new().fg(theme.ink2))])).collect();
        Paragraph::new(lines).wrap(Wrap { trim: true }).render(inner, buf);
    }
}

/// `{hash 2, array 0} slots, 1 level`: a site's typical table
pub fn shape_text(site: &crate::compare::TableSite) -> String {
    let part = |name: &str, p: &crate::compare::Part| -> Option<String> {
        if p.max <= 0.0 {
            return None;
        }
        Some(if p.min == p.max { format!("{name} {}", fmt::grouped(p.max)) } else { format!("{name} {}-{} (median {})", fmt::grouped(p.min), fmt::grouped(p.max), fmt::grouped(p.median)) })
    };
    let parts: Vec<String> = [part("hash", &site.hash), part("array", &site.array)].into_iter().flatten().collect();
    let slots = if parts.is_empty() { "empty".to_string() } else { format!("{{{}}} slots", parts.join(", ")) };
    let levels = if site.levels.max > 1.0 { format!(", {} levels deep", fmt::grouped(site.levels.max)) } else { String::new() };
    format!("{slots}{levels}")
}

fn open_profile(compare: &Compare, run: &RunInfo) -> Result<Profile, String> {
    let dir = compare.profile_dir(run).ok_or_else(|| format!("{} at n={}: no profile folder", run.variant, fmt::compact(run.size)))?;
    let mut profile = Profile::load(&dir)?;
    profile.label = Some(format!("{} n={}", run.variant, fmt::compact(run.size)));
    Ok(profile)
}

pub fn key(app: &mut App, key: Key) {
    let Some(compare) = app.compare.as_ref() else { return };
    let variants = compare.variants.len();
    let sizes = compare.sizes.len();
    if app.compare_view.list.key(key, variants, app.page) {
        return;
    }
    let (variant, size) = selected(app, compare);
    match key {
        Key::Left => app.compare_view.size = Some(size.saturating_sub(1)),
        Key::Right => app.compare_view.size = Some((size + 1).min(sizes.saturating_sub(1))),
        Key::Enter => {
            let compare = app.compare.as_ref().unwrap();
            let Some(run) = compare.variants.get(variant).and_then(|v| compare.sizes.get(size).and_then(|n| compare.run(&v.name, *n))) else { return };
            match open_profile(compare, run) {
                Ok(profile) => {
                    let label = format!("{} at n={}", run.variant, fmt::compact(run.size));
                    app.open_profiles(vec![profile]);
                    app.set_view(View::Overview);
                    app.status = format!("{label} · c returns to Compare");
                }
                Err(error) => app.status = error,
            }
        }
        Key::Char('D') => {
            let compare = app.compare.as_ref().unwrap();
            let Some(&n) = compare.sizes.get(size) else { return };
            let Some(info) = compare.variants.get(variant) else { return };
            if info.name == compare.baseline {
                app.status = "the baseline: pick another variant to diff against it".into();
                return;
            }
            let (Some(before), Some(after)) = (compare.run(&compare.baseline, n), compare.run(&info.name, n)) else { return };
            match (open_profile(compare, before), open_profile(compare, after)) {
                (Ok(b), Ok(a)) => {
                    let label = format!("{} → {} at n={}", compare.baseline, info.name, fmt::compact(n));
                    app.open_profiles(vec![b, a]);
                    app.set_view(View::Diff);
                    app.status = format!("{label} · c returns to Compare");
                }
                (Err(error), _) | (_, Err(error)) => app.status = error,
            }
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let compare = app.compare.as_ref()?;
    let (variant, size) = selected(app, compare);
    let run = compare.variants.get(variant).and_then(|v| compare.sizes.get(size).and_then(|n| compare.run(&v.name, *n)))?;
    compare.profile_dir(run).map(|dir| dir.display().to_string())
}

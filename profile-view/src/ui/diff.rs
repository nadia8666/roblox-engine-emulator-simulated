//! Diff: two profiles, before and after. The headline figures, then the functions, lines or allocation sites that
//! moved (tab switches), each with its before, after and change: red ▲ for more, blue ▼ for less, the numbers in ink.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Tabs, Widget},
};

use super::kit::{self, Item, col, col_fill, col_r};
use crate::{
    app::{App, DiffSort, DiffTab, Focus, View},
    diff::{self, Pair},
    fmt,
    keys::Key,
    metric::{Metric, Unit},
    theme::{Mark, Theme},
};

fn rows(app: &App) -> (Vec<Pair>, Unit, String) {
    let p = &app.profiles[app.shown];
    let metric = app.metric;
    let (mut rows, unit, what) = match app.diff.tab {
        DiffTab::Functions => (diff::functions(app, metric), metric.unit(), format!("own {}", metric.label(p))),
        DiffTab::Lines => (diff::lines(app, metric), metric.unit(), format!("own {}", metric.label(p))),
        DiffTab::Sites => {
            let by_count = metric == Metric::AllocCount;
            (diff::sites(app, metric), if by_count { Unit::Count } else { Unit::Bytes }, if by_count { "objects allocated".into() } else { "bytes allocated".into() })
        }
    };
    match app.diff.sort {
        DiffSort::Change => rows.sort_by(|a, b| b.delta().abs().total_cmp(&a.delta().abs()).then(a.label.cmp(&b.label))),
        DiffSort::Increase => rows.sort_by(|a, b| b.delta().total_cmp(&a.delta()).then(a.label.cmp(&b.label))),
        DiffSort::Decrease => rows.sort_by(|a, b| a.delta().total_cmp(&b.delta()).then(a.label.cmp(&b.label))),
        DiffSort::Name => rows.sort_by(|a, b| a.label.cmp(&b.label)),
    }
    // (the harness's own code after the game's, each in the chosen order)
    rows.sort_by_key(|row| row.harness);
    (rows, unit, what)
}

/// The ▲ / ▼ mark of a change in a cost, and its colour
fn mark(delta: f64, theme: &Theme) -> (&'static str, Color) {
    if delta > 0.0 {
        ("▲", theme.more)
    } else if delta < 0.0 {
        ("▼", theme.less)
    } else {
        (" ", theme.muted)
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    if app.profiles.len() < 2 {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Diff", "", false));
        kit::empty(buf, inner, theme, "Diff needs two profiles: ree profile view <before> <after>");
        return;
    }
    let (figures, notes) = diff::headline(&app.profiles[0], &app.profiles[1]);
    let (before, after) = (app.profiles[0].name(), app.profiles[1].name());
    let names = if before.eq_ignore_ascii_case("before") && after.eq_ignore_ascii_case("after") { String::new() } else { format!("{before} → {after}") };
    let two = area.width >= 150;
    let per_column = (figures.len() as u16).div_ceil(if two { 2 } else { 1 });
    let headline_height = per_column + notes.len() as u16 + 3;
    let parts = kit::rows(area, &[headline_height, 0]);
    // the headline, two columns of figures when the screen is wide
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, "Before → after", &names, false));
    let mut lines: Vec<Line> = notes.iter().map(|note| Line::from(vec![Span::styled("◇ ", Style::new().fg(theme.accent)), Span::styled(note.clone(), Style::new().fg(theme.ink2))])).collect();
    lines.push(Line::default());
    Paragraph::new(lines).render(inner, buf);
    let column_width = if two { inner.width / 2 } else { inner.width };
    for (index, figure) in figures.iter().enumerate() {
        let col = index as u16 / per_column;
        let row = index as u16 % per_column;
        let x = inner.x + col * column_width;
        let y = inner.y + notes.len() as u16 + 1 + row;
        if y >= inner.bottom() {
            continue;
        }
        let delta = figure.after - figure.before;
        let (m, color) = if figure.cost { mark(delta, theme) } else { (" ", theme.muted) };
        let line = Line::from(vec![
            Span::styled(format!("{:<34}", fmt::fit(&figure.name, 34)), Style::new().fg(theme.ink2)),
            Span::styled(format!("{:>11}", fmt::short(figure.unit, figure.before)), Style::new().fg(theme.ink2)),
            Span::styled(" → ", Style::new().fg(theme.muted)),
            Span::styled(format!("{:<11}", fmt::short(figure.unit, figure.after)), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {m} "), Style::new().fg(color)),
            Span::styled(format!("{:>9}", fmt::change(figure.before, figure.after)), Style::new().fg(if figure.cost && delta != 0.0 { color } else { theme.muted }).add_modifier(if figure.exact { Modifier::BOLD } else { Modifier::empty() })),
        ]);
        Paragraph::new(line).render(Rect { x, y, width: column_width.saturating_sub(2), height: 1 }, buf);
    }
    // the functions, lines or sites that moved, a tab each
    let (rows, unit, what) = rows(app);
    let sort = match app.diff.sort {
        DiffSort::Change => "the largest change",
        DiffSort::Increase => "what grew most",
        DiffSort::Decrease => "what shrank most",
        DiffSort::Name => "name",
    };
    let block = kit::panel(theme, "Moved", &format!("{} · {what} · by {sort}", fmt::grouped(rows.len() as f64)), true);
    let inner = kit::draw_panel(buf, parts[1], block);
    let tabs = [(DiffTab::Functions, "Functions"), (DiffTab::Lines, "Lines"), (DiffTab::Sites, "Allocation sites")];
    let selected = tabs.iter().position(|(tab, _)| *tab == app.diff.tab);
    Tabs::new(tabs.iter().map(|(_, name)| Line::from(format!(" {name} "))))
        .select(selected)
        .style(Style::new().fg(theme.ink2).bg(theme.panel))
        .highlight_style(Style::new().fg(theme.accent_ink).bg(theme.accent).add_modifier(Modifier::BOLD))
        .divider(Span::raw(" "))
        .padding("", "")
        .render(Rect { height: 1, ..inner }, buf);
    if app.diff.tab == DiffTab::Lines {
        let x = inner.x + 46;
        Paragraph::new(Line::from(Span::styled("where the source is the same", Style::new().fg(theme.muted)))).render(Rect { x, height: 1, width: inner.right().saturating_sub(x), ..inner }, buf);
    }
    let table = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
    if rows.is_empty() {
        kit::empty(buf, table, theme, "nothing moved");
        app.diff.list = Default::default();
        return;
    }
    let show_calls = app.diff.tab == DiffTab::Functions;
    let mut cols = vec![col_fill("", 3), col_r("before", 11), col_r("after", 11), col("", 1), col_r("change", 11), col_r("", 8)];
    if show_calls {
        cols.push(col_r("calls", 17));
    }
    cols.push(col_fill("", 2));
    let bar_col = cols.len() - 1;
    let bar_width = kit::widths(&cols, table.width)[bar_col];
    let mut list = app.diff.list;
    list.fit(rows.len(), kit::rows_shown(table));
    let max = rows.iter().map(|r| r.delta().abs()).fold(0.0, f64::max);
    let items = rows
        .iter()
        .map(|row| {
            let delta = row.delta();
            let (m, color) = mark(delta, theme);
            let mut cells = vec![
                Item::label(row.label.clone(), Style::new().fg(if row.harness { theme.muted } else { theme.ink })),
                Item::text(fmt::short(unit, row.before), Style::new().fg(theme.ink2)),
                Item::text(fmt::short(unit, row.after), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
                Item::Spans(Line::from(Span::styled(m, Style::new().fg(color)))),
                Item::text(fmt::signed(unit, delta), Style::new().fg(theme.ink)),
                Item::text(fmt::change(row.before, row.after), Style::new().fg(color)),
            ];
            if show_calls {
                cells.push(match row.calls {
                    Some((before, after)) => Item::text(format!("{} → {}", fmt::compact(before as f64), fmt::compact(after as f64)), Style::new().fg(theme.ink2)),
                    None => Item::empty(),
                });
            }
            cells.push(Item::Spans(kit::diverging(if max > 0.0 { delta / max } else { 0.0 }, bar_width, theme)));
            cells
        })
        .collect();
    let grid = kit::table(buf, table, theme, &cols, items, Some(list.selected), list.offset);
    kit::mark(buf, grid.column(bar_col), Mark::Bar);
    list.offset = grid.offset;
    app.diff.list = list;
}

pub fn key(app: &mut App, key: Key) {
    if app.profiles.len() < 2 {
        return;
    }
    let (rows, _, _) = rows(app);
    if app.diff.list.key(key, rows.len(), app.page) {
        return;
    }
    match key {
        Key::Tab | Key::Right => {
            app.diff.tab = match app.diff.tab {
                DiffTab::Functions => DiffTab::Lines,
                DiffTab::Lines => DiffTab::Sites,
                DiffTab::Sites => DiffTab::Functions,
            };
            app.diff.list = Default::default();
        }
        Key::BackTab | Key::Left => {
            app.diff.tab = match app.diff.tab {
                DiffTab::Functions => DiffTab::Sites,
                DiffTab::Lines => DiffTab::Functions,
                DiffTab::Sites => DiffTab::Lines,
            };
            app.diff.list = Default::default();
        }
        Key::Char('s') => {
            app.diff.sort = match app.diff.sort {
                DiffSort::Change => DiffSort::Increase,
                DiffSort::Increase => DiffSort::Decrease,
                DiffSort::Decrease => DiffSort::Name,
                DiffSort::Name => DiffSort::Change,
            };
            app.diff.list.selected = 0;
        }
        Key::Enter => {
            // the after profile's function or line
            let Some(row) = rows.get(app.diff.list.selected) else { return };
            let Some(func) = row.func else { return };
            if app.shown != 1 {
                app.swap();
            }
            app.focus = Some(Focus { func, line: row.line });
            match (app.diff.tab, row.line) {
                (DiffTab::Functions, _) | (_, None) => {
                    app.calls.center = Some(func);
                    app.set_view(View::Calls);
                }
                (_, Some(line)) => {
                    app.source.module = None;
                    app.source.line = line;
                    app.set_view(View::Source);
                }
            }
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let (rows, unit, _) = rows(app);
    let row = rows.get(app.diff.list.selected)?;
    Some(format!("{}: {} -> {} ({})", row.label, fmt::short(unit, row.before), fmt::short(unit, row.after), fmt::change(row.before, row.after)))
}

pub fn find(app: &mut App, query: &str, forward: bool, from_start: bool) -> bool {
    let (rows, _, _) = rows(app);
    match super::next_match(rows.len(), app.diff.list.selected, forward, from_start, |i| rows[i].label.to_lowercase().contains(query)) {
        Some(found) => {
            app.diff.list.selected = found;
            true
        }
        None => false,
    }
}

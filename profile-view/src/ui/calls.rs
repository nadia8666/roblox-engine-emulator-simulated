//! Calls: a function between its callers (above) and its callees (below), with what each cost under it in the metric
//! and how often it called; its hottest lines at the bottom. Enter re-centres on the selected caller or callee, esc
//! goes back.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{
    kit::{self, Item, col, col_fill, col_r},
    overview::{hot, ranked},
};
use crate::{
    app::{App, Focus},
    fmt,
    keys::Key,
    metric::Metric,
    theme::{Mark, Theme},
    tree::{EdgeStat, Frame},
};

#[derive(Clone, Copy, Debug)]
enum Row {
    Caller(Frame, EdgeStat),
    Callee(u32, EdgeStat),
}

/// The function in the middle: the one the views follow (picked here or in another view), else the hottest
pub fn center(app: &App) -> Option<u32> {
    app.focus.map(|f| f.func).or(app.calls.center).or_else(|| hot(app).first().map(|r| r.func))
}

fn rows(app: &App, func: u32) -> (Vec<Row>, usize) {
    let tree = app.tree();
    let mut callers: Vec<Row> = tree.callers(func).into_iter().map(|(frame, stat)| Row::Caller(frame, stat)).collect();
    let mut callees: Vec<Row> = tree.callees(func).into_iter().map(|(callee, stat)| Row::Callee(callee, stat)).collect();
    if app.calls.by_calls {
        let calls = |row: &Row| match row {
            Row::Caller(_, stat) | Row::Callee(_, stat) => stat.calls,
        };
        callers.sort_by_key(|row| std::cmp::Reverse(calls(row)));
        callees.sort_by_key(|row| std::cmp::Reverse(calls(row)));
    }
    let split = callers.len();
    callers.extend(callees);
    (callers, split)
}

fn row_name(app: &App, row: &Row) -> String {
    let p = &app.profiles[app.shown];
    match row {
        Row::Caller(Frame::Func(func), _) | Row::Callee(func, _) => p.funcs[*func as usize].label(),
        Row::Caller(Frame::Phase(phase), _) => format!("(the engine, in {})", p.phase_name(*phase)),
        Row::Caller(Frame::Root, _) => "(the engine)".into(),
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(func) = center(app) else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Calls", "", false));
        kit::empty(buf, inner, theme, "no functions");
        return;
    };
    let tree = app.tree();
    let (rows, split) = rows(app, func);
    let mut list = app.calls.list;
    let p = &app.profiles[app.shown];
    let metric = tree.metric;
    let unit = metric.unit();
    let run_total = tree.total();
    let stat = tree.funcs.get(&func).copied().unwrap_or_default();
    let f = &p.funcs[func as usize];
    // (a line's work in the profile's reported phases when the profile counts lines per phase, a newer ree's; else over
    // the run, which the panel says)
    let scope = p.scope();
    let per_phase = app.metric == Metric::Work && p.lines_of(func as usize).any(|(_, fig)| !fig.phases.is_empty());
    let line_value = |fig: &crate::model::LineFig| if per_phase { fig.phases.iter().filter(|(phase, _)| scope.covers(**phase)).map(|(_, work)| *work).sum() } else { app.metric.line_self(fig) };
    let mut lines: Vec<(i64, f64, &crate::model::LineFig)> = p.lines_of(func as usize).map(|(line, fig)| (line, line_value(fig), fig)).filter(|(_, v, _)| *v > 0.0).collect();
    lines.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // each panel's rows: what it needs, the lines taking what is left (a panel is its rows, a header and two borders)
    let (callers_len, callees_len) = (split, rows.len() - split);
    let card_height = 4u16;
    let room = area.height.saturating_sub(card_height + 9) as usize;
    let callers_rows = callers_len.max(1).min((room / 3).max(1));
    let callees_rows = callees_len.max(1).min((room.saturating_sub(callers_rows) / 2).max(1));
    let lines_rows = room.saturating_sub(callers_rows + callees_rows).min(lines.len());
    let parts = kit::rows(area, &[callers_rows as u16 + 3, card_height, callees_rows as u16 + 3, if lines.is_empty() { 0 } else { lines_rows as u16 + 3 }]);
    list.fit(rows.len(), rows.len().max(1));
    let sel = list.selected;
    let table_rows = |slice: &[Row], whole: f64, width: u16| -> Vec<Vec<Item>> {
        slice
            .iter()
            .map(|row| {
                let stat = match row {
                    Row::Caller(_, stat) | Row::Callee(_, stat) => stat,
                };
                let share = if run_total > 0.0 { stat.total / run_total } else { 0.0 };
                let style = theme.row(share);
                vec![
                    Item::label(row_name(app, row), style),
                    Item::text(if tree.has_calls { fmt::grouped(stat.calls as f64) } else { "-".into() }, style),
                    Item::text(fmt::short(unit, stat.total), style),
                    Item::text(fmt::share(stat.total, whole), style),
                    Item::Spans(kit::bar(if whole > 0.0 { stat.total / whole } else { 0.0 }, width, theme.heat_of(share))),
                ]
            })
            .collect()
    };
    let cols = [col_fill("function", 3), col_r("calls", 10), col_r(&metric.label(p), 12), col_r("share", 7), col_fill("", 2)];
    // the callers: what this function cost under each
    let inner = kit::draw_panel(buf, parts[0], kit::panel(theme, "Callers", &fmt::grouped(callers_len as f64), sel < split));
    if callers_len == 0 {
        kit::empty(buf, inner, theme, "none: the engine calls it");
    } else {
        let width = kit::widths(&cols, inner.width)[4];
        let offset = if sel < split { sel.saturating_sub(callers_rows.saturating_sub(1)) } else { 0 };
        let grid = kit::table(buf, inner, theme, &cols, table_rows(&rows[..split], stat.total, width), (sel < split).then_some(sel), offset);
        kit::mark(buf, grid.column(4), Mark::Bar);
    }
    // the function itself
    let share = if run_total > 0.0 { stat.own / run_total } else { 0.0 };
    let mut title = vec![Span::raw(" "), Span::styled("■ ", Style::new().fg(theme.node_of(share))), Span::styled(f.label(), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD))];
    if f.harness {
        title.push(Span::styled("  the harness's code", Style::new().fg(theme.muted)));
    }
    title.push(Span::raw(" "));
    let title = Line::from(title);
    let inner = kit::draw_panel(buf, parts[1], kit::bottom(kit::panel_line(theme, title, false), super::legend(app, theme, area.width)));
    let figure = |label: &str, value: f64| vec![Span::styled(format!("{label} "), Style::new().fg(theme.muted)), Span::styled(fmt::value(unit, value), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)), Span::styled(format!(" {}", fmt::share(value, run_total)), Style::new().fg(theme.ink2))];
    let mut spans = figure("own", stat.own);
    spans.push(Span::raw("     "));
    spans.extend(figure("total", stat.total));
    // (a function C calls, a sort's comparator, counts one call per C call in a profile without invocation counts: its
    // runs are not known, and a figure each would be a C call's)
    let from_c = tree.callers(func).iter().any(|(caller, _)| matches!(caller, Frame::Func(c) if p.funcs[*c as usize].kind == crate::model::Kind::C));
    let invocations = p.raw.work.as_ref().is_some_and(|w| w.c_invocations);
    if tree.has_calls && stat.calls > 0 && from_c && !invocations {
        spans.push(Span::styled(format!("     {} calls, counted once a C call (needs invocation counts)", fmt::grouped(stat.calls as f64)), Style::new().fg(theme.muted)));
    } else if tree.has_calls && stat.calls > 0 {
        spans.push(Span::styled(format!("     {} calls, {} each", fmt::grouped(stat.calls as f64), fmt::short(unit, stat.total / stat.calls as f64)), Style::new().fg(theme.ink2)));
    }
    Paragraph::new(Line::from(spans)).render(Rect { height: 1, ..inner }, buf);
    if inner.height >= 2 && stat.total > 0.0 {
        kit::gauge(buf, Rect { y: inner.y + 1, height: 1, ..inner }, theme, stat.own / stat.total, Line::from(Span::styled(format!("its own {} ", fmt::share(stat.own, stat.total)), Style::new().fg(theme.ink2))), theme.heat_of(share));
    }
    // the callees: what each cost under this function
    let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, "Callees", &fmt::grouped(callees_len as f64), sel >= split));
    if callees_len == 0 {
        kit::empty(buf, inner, theme, "none");
    } else {
        let width = kit::widths(&cols, inner.width)[4];
        let offset = if sel >= split { (sel - split).saturating_sub(callees_rows.saturating_sub(1)) } else { 0 };
        let grid = kit::table(buf, inner, theme, &cols, table_rows(&rows[split..], stat.total, width), (sel >= split).then(|| sel - split), offset);
        kit::mark(buf, grid.column(4), Mark::Bar);
    }
    // its hottest lines, with their code and their commonest instructions
    if !lines.is_empty() && parts[3].height >= 4 {
        let over = if per_phase { scope.label.clone() } else if app.metric == Metric::Work && scope.phases.is_some() { "the whole run (lines are not counted per phase)".into() } else { String::new() };
        let note = if over.is_empty() { format!("own {}", app.metric.label(p)) } else { format!("own {} · {over}", app.metric.label(p)) };
        let inner = kit::draw_panel(buf, parts[3], kit::panel(theme, "Hottest lines", &note, false));
        // (a share of the reported phases' work when the lines are theirs)
        let line_total = if per_phase { p.scope_work().map_or(0.0, |(game, harness)| game + harness) } else { Metric::total(app.metric, p) };
        let max = lines.first().map_or(0.0, |l| l.1);
        let source = p.source(&f.module);
        let cols = [col_r("line", 6), col_r("own", 10), col_r("share", 6), col("", 14), col_fill("code", 3), col_fill("instructions", 2)];
        let rows = lines
            .iter()
            .take(kit::rows_shown(inner))
            .map(|(line, value, fig)| {
                let share = if line_total > 0.0 { value / line_total } else { 0.0 };
                let style = theme.row(share);
                let mut ops: Vec<(&String, &u64)> = fig.ops.iter().collect();
                ops.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                let ops: Vec<String> = ops.iter().take(3).map(|(op, n)| format!("{op} {}", fmt::compact(**n as f64))).collect();
                let code = source.as_ref().and_then(|s| s.lines.get(line)).map(|code| code.trim().to_string()).unwrap_or_default();
                vec![
                    Item::text(line.to_string(), Style::new().fg(theme.ink2)),
                    Item::text(fmt::short(app.metric.unit(), *value), style),
                    Item::text(fmt::share(*value, line_total), style),
                    Item::Spans(kit::bar(if max > 0.0 { value / max } else { 0.0 }, 14, theme.heat_of(share))),
                    Item::Spans(super::source::highlight(&code, theme)),
                    Item::text(ops.join(" · "), Style::new().fg(theme.muted)),
                ]
            })
            .collect();
        let grid = kit::table(buf, inner, theme, &cols, rows, None, 0);
        kit::mark(buf, grid.column(3), Mark::Bar);
    }
    app.calls.list = list;
}

pub fn key(app: &mut App, key: Key) {
    let Some(func) = center(app) else { return };
    let (rows, _) = rows(app, func);
    if app.calls.list.key(key, rows.len(), app.page) {
        return;
    }
    match key {
        Key::Enter | Key::Right => {
            let target = match rows.get(app.calls.list.selected) {
                Some(Row::Caller(Frame::Func(f), _)) | Some(Row::Callee(f, _)) => Some(*f),
                _ => None,
            };
            if let Some(target) = target {
                app.calls.history.push(func);
                app.calls.center = Some(target);
                app.calls.list.selected = 0;
                app.focus = Some(Focus { func: target, line: None });
            }
        }
        Key::Esc | Key::Backspace | Key::Left => {
            if let Some(previous) = app.calls.history.pop() {
                app.calls.center = Some(previous);
                app.calls.list.selected = 0;
                app.focus = Some(Focus { func: previous, line: None });
            }
        }
        Key::Char('s') => {
            app.calls.by_calls = !app.calls.by_calls;
            app.status = if app.calls.by_calls { "by calls".into() } else { "by cost".into() };
        }
        _ => {}
    }
}

/// `path:line` when the profile names the module's file, else `Module:line`
pub fn place_text(app: &App, func: u32, line: Option<i64>) -> Option<String> {
    let p = &app.profiles[app.shown];
    let f = p.funcs.get(func as usize)?;
    if f.module.is_empty() {
        return Some(f.name.clone());
    }
    let line = line.unwrap_or(f.line);
    Some(match p.module_path(&f.module) {
        Some(path) => format!("{path}:{line}"),
        None => format!("{}:{line}", f.module),
    })
}

pub fn copy(app: &App) -> Option<String> {
    let func = center(app)?;
    let (rows, _) = rows(app, func);
    match rows.get(app.calls.list.selected) {
        Some(Row::Caller(Frame::Func(f), _)) | Some(Row::Callee(f, _)) => place_text(app, *f, None),
        _ => place_text(app, func, None),
    }
}

pub fn find(app: &mut App, query: &str) -> bool {
    // (a search re-centres on the hottest function whose name matches)
    let p = &app.profiles[app.shown];
    let hit = ranked(app).into_iter().find(|r| p.funcs[r.func as usize].label().to_lowercase().contains(query)).map(|r| r.func);
    if let Some(func) = hit {
        if let Some(current) = center(app) {
            app.calls.history.push(current);
        }
        app.calls.center = Some(func);
        app.calls.list.selected = 0;
        app.focus = Some(Focus { func, line: None });
        return true;
    }
    false
}
